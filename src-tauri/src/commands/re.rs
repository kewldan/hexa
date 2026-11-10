//! Executables, disassembly, assembly, patches and cross-references
//! (RE-01…09, ANL-12…14, SRCH-12).

use hexa_core::Tree;
use hexa_core::document::{self, ChangeKind};
use hexa_re::arch::{Arch, ArchInfo};
use hexa_re::detect::Finding;
use hexa_re::disasm::{DisasmOptions, Instruction};
use hexa_re::exe::ExecutableInfo;
use hexa_re::patch::{InstructionPatch, Patch, PatchList, PatchState, PortOptions, PortReport};
use hexa_re::tools::{ExternalTool, ToolInvocation};
use hexa_re::xref::{PointerOptions, PointerRef, XrefReport};
use serde::Serialize;
use tauri::State;

use super::doc::{EditResult, mutate};
use crate::error::{CmdError, CmdResult};
use crate::state::AppState;

/// Largest file read whole for cross-references and checksums.
const MAX_WHOLE: u64 = 512 << 20;

impl From<hexa_re::ReError> for CmdError {
    fn from(e: hexa_re::ReError) -> Self {
        Self::new("re", e.to_string())
    }
}

fn tree_of(state: &AppState, id: u64) -> CmdResult<Tree> {
    Ok(state.get(id)?.view.read().tree.clone())
}

fn whole(tree: &Tree) -> CmdResult<Vec<u8>> {
    if tree.len() > MAX_WHOLE {
        return Err(CmdError::invalid("file is too large for this operation"));
    }
    Ok(tree.read_vec(0, tree.len() as usize)?)
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> CmdResult<T> + Send + 'static,
) -> CmdResult<T> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| CmdError::new("internal", e.to_string()))?
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExeOverview {
    pub info: Option<ExecutableInfo>,
    pub error: Option<String>,
    pub findings: Vec<Finding>,
}

/// Headers, sections, imports … and compiler/packer detection (RE-01, RE-07).
#[tauri::command]
pub async fn re_info(state: State<'_, AppState>, id: u64) -> CmdResult<ExeOverview> {
    let tree = tree_of(&state, id)?;
    blocking(move || {
        let (info, error) = match hexa_re::exe::parse_tree(&tree) {
            Ok(i) => (Some(i), None),
            Err(e) => (None, Some(e.to_string())),
        };
        let findings = hexa_re::detect::detect_tree(&tree).unwrap_or_default();
        Ok(ExeOverview {
            info,
            error,
            findings,
        })
    })
    .await
}

#[tauri::command]
pub fn re_arches() -> Vec<ArchInfo> {
    Arch::catalog()
}

/// Disassembles `len` bytes at a file offset shown at `address` (ANL-12, ANL-13).
#[tauri::command]
pub fn re_disasm(
    state: State<'_, AppState>,
    id: u64,
    offset: u64,
    len: u64,
    address: u64,
    arch: Arch,
    max: Option<usize>,
) -> CmdResult<Vec<Instruction>> {
    let tree = tree_of(&state, id)?;
    let len = len.min(tree.len().saturating_sub(offset)).min(1 << 20);
    let opts = DisasmOptions {
        max_instructions: max.unwrap_or(400),
        ..DisasmOptions::default()
    };
    Ok(hexa_re::disasm::disassemble_tree(
        arch,
        &tree,
        offset,
        len as usize,
        address,
        &opts,
    )?)
}

/// Assembles one or more instructions (ANL-14).
#[tauri::command]
pub fn re_assemble(arch: Arch, source: String, address: u64) -> CmdResult<Vec<u8>> {
    Ok(hexa_re::asm::assemble(arch, &source, address)?)
}

/// Plans replacing the instruction at `offset` with `source` (RE-03).
#[tauri::command]
pub fn re_patch_plan(
    state: State<'_, AppState>,
    id: u64,
    offset: u64,
    address: u64,
    arch: Arch,
    source: String,
) -> CmdResult<InstructionPatch> {
    let tree = tree_of(&state, id)?;
    Ok(hexa_re::patch::plan_instruction_patch_tree(
        arch, &tree, offset, address, &source,
    )?)
}

/// Code and data references to a virtual address (RE-04, SRCH-12).
#[tauri::command]
pub async fn re_xrefs(
    state: State<'_, AppState>,
    id: u64,
    va: u64,
    max: Option<usize>,
) -> CmdResult<XrefReport> {
    let tree = tree_of(&state, id)?;
    blocking(move || {
        let data = whole(&tree)?;
        let info = hexa_re::exe::parse(&data)?;
        Ok(hexa_re::xref::find_xrefs(
            &data,
            &info,
            va,
            max.unwrap_or(2000),
        )?)
    })
    .await
}

/// Stored pointers with a given value in any width and byte order (SRCH-12).
#[tauri::command]
pub async fn re_pointers(
    state: State<'_, AppState>,
    id: u64,
    value: u64,
    opts: PointerOptions,
) -> CmdResult<Vec<PointerRef>> {
    let tree = tree_of(&state, id)?;
    blocking(move || Ok(hexa_re::xref::find_pointers_tree(&tree, value, &opts)?)).await
}

/// Recomputes the PE checksum field and writes it (RE-06).
#[tauri::command]
pub fn re_fix_checksum(state: State<'_, AppState>, id: u64) -> CmdResult<EditResult> {
    let tree = tree_of(&state, id)?;
    let mut data = whole(&tree)?;
    let info = hexa_re::exe::parse(&data)?;
    let field = info
        .checksum
        .ok_or_else(|| CmdError::invalid("this format has no checksum field"))?;
    let value = hexa_re::checksum::fix_pe_checksum(&mut data)?;
    mutate(&state, id, |d| {
        d.overwrite(field.offset, &value.to_le_bytes()).map(Some)
    })
}

/// The edits of a document as a patch list (RE-05); same-size edits only.
#[tauri::command]
pub fn re_patches_export(state: State<'_, AppState>, id: u64) -> CmdResult<PatchList> {
    let entry = state.get(id)?;
    let v = entry.view.read();
    if v.tree.len() != v.base.len() {
        return Err(CmdError::invalid(
            "the file size changed: a patch list needs overwrite-only edits",
        ));
    }
    let mut patches = Vec::new();
    for c in document::changes_in(&v.tree, &v.base, 0, v.tree.len()) {
        if c.kind != ChangeKind::Modified || c.end <= c.start {
            continue;
        }
        let len = (c.end - c.start) as usize;
        let mut original = vec![0; len];
        v.base.read_at(c.start, &mut original)?;
        let replacement = v.tree.read_vec(c.start, len)?;
        if original != replacement {
            patches.push(Patch {
                offset: c.start,
                original,
                replacement,
                comment: String::new(),
            });
        }
    }
    let doc = entry.doc.lock();
    Ok(PatchList {
        version: 1,
        target: Some(doc.name().to_owned()),
        target_size: Some(v.tree.len()),
        patches,
    })
}

/// State of every patch against the document.
#[tauri::command]
pub fn re_patches_check(
    state: State<'_, AppState>,
    id: u64,
    list: PatchList,
) -> CmdResult<Vec<PatchState>> {
    Ok(list.states_tree(&tree_of(&state, id)?)?)
}

/// Applies (or with `revert` undoes) a patch list as one undoable step.
#[tauri::command]
pub fn re_patches_apply(
    state: State<'_, AppState>,
    id: u64,
    list: PatchList,
    revert: bool,
) -> CmdResult<EditResult> {
    list.validate()?;
    mutate(&state, id, |d| {
        d.transaction(
            if revert {
                "Revert patches"
            } else {
                "Apply patches"
            },
            |d| {
                let mut last = None;
                for p in &list.patches {
                    let bytes = if revert { &p.original } else { &p.replacement };
                    last = Some(d.overwrite(p.offset, bytes)?);
                }
                Ok(last)
            },
        )
    })
}

/// Finds where patches made for `old` belong in `new` (RE-05).
#[tauri::command]
pub async fn re_patches_port(
    state: State<'_, AppState>,
    old: u64,
    new: u64,
    list: PatchList,
) -> CmdResult<PortReport> {
    let (to, tn) = (tree_of(&state, old)?, tree_of(&state, new)?);
    blocking(move || {
        Ok(hexa_re::patch::port_patches_tree(
            &to,
            &tn,
            &list,
            &PortOptions::default(),
        )?)
    })
    .await
}

#[tauri::command]
pub fn re_tools() -> Vec<ExternalTool> {
    hexa_re::tools::known_tools()
}

/// Writes the selection to a temporary file and builds the command or URL
/// that opens it in an external tool (RE-09).
#[tauri::command]
pub fn re_tool_prepare(
    state: State<'_, AppState>,
    id: u64,
    start: u64,
    end: u64,
    tool: String,
    tool_dir: Option<String>,
) -> CmdResult<ToolInvocation> {
    let tool = hexa_re::tools::known_tools()
        .into_iter()
        .find(|t| t.id == tool)
        .ok_or_else(|| CmdError::invalid("unknown tool"))?;
    if end - start > MAX_WHOLE {
        return Err(CmdError::invalid("selection is too large"));
    }
    let bytes = tree_of(&state, id)?.read_vec(start, (end - start) as usize)?;
    let file = hexa_re::tools::write_selection_temp(&bytes, &tool.extension)?;
    Ok(hexa_re::tools::render_invocation(
        &tool,
        &file,
        &bytes,
        tool_dir.as_deref().unwrap_or(""),
    ))
}

/// Starts a prepared command-line tool (the user picked it explicitly).
#[tauri::command]
pub fn re_tool_spawn(program: String, args: Vec<String>) -> CmdResult<()> {
    std::process::Command::new(program).args(args).spawn()?;
    Ok(())
}
