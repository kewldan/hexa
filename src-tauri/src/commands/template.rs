//! Structure templates on documents (STRC-01…23, SRCH-11, DIFF-10, NAV-12).
//!
//! Each application of a template gets a key; its evaluated tree stays in
//! [`TplState`] so the UI can page through children, map offsets to fields
//! and edit values without re-sending the whole tree. Lazily materialised
//! array elements get keys of their own.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use hexa_core::{Cancel, Tree};
use hexa_template::query::{self, ExportFormat, ExportOptions, FieldChange};
use hexa_template::result::{Color, Node, NodeHit};
use hexa_template::{
    Dialect, EvalOptions, ParseOptions, Program, TemplateResult, library, parse_with,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::State;

use super::doc::{EditResult, mutate};
use crate::error::{CmdError, CmdResult};
use crate::state::AppState;

fn terr(e: impl std::fmt::Display) -> CmdError {
    CmdError::new("template", e.to_string())
}

#[derive(Debug)]
struct Applied {
    doc: u64,
    name: String,
    source: TplSource,
    opts: EvalOptions,
    program: Arc<Program>,
    result: Arc<TemplateResult>,
    /// For lazily expanded elements: the parent key.
    parent: Option<u64>,
}

#[derive(Debug, Default)]
pub struct TplState {
    next: AtomicU64,
    applied: Mutex<HashMap<u64, Applied>>,
}

impl TplState {
    fn insert(&self, a: Applied) -> u64 {
        let key = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        self.applied.lock().insert(key, a);
        key
    }

    fn get(&self, key: u64) -> CmdResult<(u64, Arc<Program>, Arc<TemplateResult>)> {
        let map = self.applied.lock();
        let a = map
            .get(&key)
            .ok_or_else(|| CmdError::invalid("template result is gone"))?;
        Ok((a.doc, a.program.clone(), a.result.clone()))
    }
}

/// Where a template comes from.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TplSource {
    Library {
        id: String,
    },
    Text {
        text: String,
        dialect: Option<String>,
    },
    File {
        path: PathBuf,
    },
}

fn dialect_of(name: Option<&str>, path: Option<&std::path::Path>) -> Dialect {
    let ext = path
        .and_then(|p| p.extension())
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match name
        .map(str::to_ascii_lowercase)
        .as_deref()
        .or(ext.as_deref())
    {
        Some("bt") => Dialect::Bt,
        Some("c" | "h" | "hpp") => Dialect::C,
        Some("ksy") => Dialect::Ksy,
        _ => Dialect::Hexpat,
    }
}

fn compile(source: &TplSource) -> CmdResult<(String, Program)> {
    let (name, text, dialect, base) = match source {
        TplSource::Library { id } => {
            let e = library::get(id).ok_or_else(|| CmdError::invalid("unknown template"))?;
            return Ok((e.name.to_owned(), library::program(id).map_err(terr)?));
        }
        TplSource::Text { text, dialect } => (
            "Шаблон".to_owned(),
            text.clone(),
            dialect_of(dialect.as_deref(), None),
            None,
        ),
        TplSource::File { path } => (
            path.file_name()
                .map_or_else(|| "template".into(), |s| s.to_string_lossy().into_owned()),
            std::fs::read_to_string(path)?,
            dialect_of(None, Some(path)),
            path.parent().map(std::path::Path::to_path_buf),
        ),
    };
    if dialect == Dialect::Ksy {
        let p = hexa_template::import::ksy::from_ksy(&text).map_err(terr)?;
        return Ok((name, p));
    }
    // Includes resolve next to the template file.
    let resolver = move |inc: &str| -> Option<String> {
        base.as_ref()
            .and_then(|b| std::fs::read_to_string(b.join(inc)).ok())
    };
    let (program, diags) = parse_with(
        &text,
        &ParseOptions {
            dialect,
            resolver: Some(&resolver),
            ..ParseOptions::default()
        },
    );
    if let Some(d) = diags
        .iter()
        .find(|d| d.severity == hexa_template::Severity::Error)
    {
        return Err(CmdError::new(
            "template",
            format!("{}:{}: {}", d.line, d.col, d.message),
        ));
    }
    Ok((name, program))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryItem {
    pub id: String,
    pub name: String,
    pub description: String,
    pub extensions: Vec<String>,
}

#[tauri::command]
pub fn tpl_library() -> Vec<LibraryItem> {
    library::list()
        .into_iter()
        .map(|i| LibraryItem {
            id: i.id,
            name: i.name,
            description: i.description,
            extensions: i.extensions,
        })
        .collect()
}

/// Library templates that fit the document, best first (STRC-09).
#[tauri::command]
pub fn tpl_detect(state: State<'_, AppState>, id: u64) -> CmdResult<Vec<String>> {
    let entry = state.get(id)?;
    let tree = entry.view.read().tree.clone();
    let head = tree.read_vec(0, 64 << 10)?;
    let name = entry.doc.lock().name().to_owned();
    Ok(library::detect(&head, Some(&name))
        .into_iter()
        .map(str::to_owned)
        .collect())
}

/// A node as the tree view needs it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UiNode {
    #[serde(flatten)]
    pub node: Node,
    /// Number of children (elements for arrays).
    pub child_count: u64,
}

fn ui(n: Node) -> UiNode {
    let child_count = if n.is_array {
        n.len
    } else {
        n.children.len() as u64
    };
    UiNode {
        node: n,
        child_count,
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HighlightOut {
    pub start: u64,
    pub end: u64,
    pub color: String,
    pub node: u32,
}

const PALETTE: [&str; 6] = [
    "#2F6BF0", "#1F9D74", "#E8890C", "#7C4DFF", "#D6336C", "#0EA5E9",
];

fn color_css(c: &Color) -> String {
    match c {
        Color::Palette(i) => PALETTE[usize::from(*i) % PALETTE.len()].to_owned(),
        Color::Rgb(s) => format!("#{}", s.trim_start_matches('#')),
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedOut {
    pub key: u64,
    pub name: String,
    pub roots: Vec<UiNode>,
    pub errors: Vec<hexa_template::result::EvalMessage>,
    pub log: Vec<String>,
    pub truncated: bool,
    pub node_count: usize,
    pub highlights: Vec<HighlightOut>,
}

fn out(key: u64, name: &str, r: &TemplateResult) -> AppliedOut {
    let mut highlights: Vec<HighlightOut> = hexa_template::highlights(r)
        .into_iter()
        .map(|(start, size, c, node)| HighlightOut {
            start,
            end: start + size,
            color: color_css(&c),
            node,
        })
        .collect();
    highlights.truncate(200_000);
    AppliedOut {
        key,
        name: name.to_owned(),
        roots: r
            .roots
            .iter()
            .filter_map(|&id| r.node(id).cloned())
            .map(ui)
            .collect(),
        errors: r.errors.clone(),
        log: r.log.iter().take(1000).cloned().collect(),
        truncated: r.truncated,
        node_count: r.nodes.len(),
        highlights,
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ApplyArgs {
    pub offset: u64,
    pub start: Option<u64>,
    pub end: Option<u64>,
    pub root_type: Option<String>,
}

fn eval(program: &Program, tree: &Tree, opts: &EvalOptions) -> CmdResult<TemplateResult> {
    hexa_template::evaluate(program, tree, opts, &Cancel::new()).map_err(terr)
}

/// Applies a template at an offset or to a selection (STRC-08).
#[tauri::command]
pub async fn tpl_apply(
    state: State<'_, AppState>,
    tpl: State<'_, TplState>,
    id: u64,
    source: TplSource,
    args: Option<ApplyArgs>,
) -> CmdResult<AppliedOut> {
    let args = args.unwrap_or_default();
    let tree = state.get(id)?.view.read().tree.clone();
    let opts = EvalOptions {
        base_offset: args.offset,
        range: match (args.start, args.end) {
            (Some(s), Some(e)) => Some(s..e),
            _ => None,
        },
        root_type: args.root_type,
        ..EvalOptions::default()
    };
    let src = source.clone();
    let (name, program, result) = tauri::async_runtime::spawn_blocking(move || {
        let (name, program) = compile(&src)?;
        let result = eval(&program, &tree, &opts).map(|r| (r, opts))?;
        Ok::<_, CmdError>((name, program, result))
    })
    .await
    .map_err(|e| CmdError::new("internal", e.to_string()))??;
    let (result, opts) = result;
    let result = Arc::new(result);
    let key = tpl.insert(Applied {
        doc: id,
        name: name.clone(),
        source,
        opts,
        program: Arc::new(program),
        result: result.clone(),
        parent: None,
    });
    Ok(out(key, &name, &result))
}

/// Re-evaluates after the data changed (STRC-11); the key stays the same.
#[tauri::command]
pub async fn tpl_reapply(
    state: State<'_, AppState>,
    tpl: State<'_, TplState>,
    key: u64,
) -> CmdResult<AppliedOut> {
    let (doc, program, opts, name) = {
        let map = tpl.applied.lock();
        let a = map
            .get(&key)
            .ok_or_else(|| CmdError::invalid("template result is gone"))?;
        (a.doc, a.program.clone(), a.opts.clone(), a.name.clone())
    };
    let tree = state.get(doc)?.view.read().tree.clone();
    let result = tauri::async_runtime::spawn_blocking(move || eval(&program, &tree, &opts))
        .await
        .map_err(|e| CmdError::new("internal", e.to_string()))??;
    let result = Arc::new(result);
    {
        let mut map = tpl.applied.lock();
        // Expanded elements of the old tree are stale now.
        map.retain(|_, a| a.parent != Some(key));
        if let Some(a) = map.get_mut(&key) {
            a.result = result.clone();
        }
    }
    Ok(out(key, &name, &result))
}

#[tauri::command]
pub fn tpl_remove(tpl: State<'_, TplState>, key: u64) {
    tpl.applied
        .lock()
        .retain(|&k, a| k != key && a.parent != Some(key));
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildrenOut {
    /// Key the returned nodes belong to (a new one for an expanded element).
    pub key: u64,
    pub nodes: Vec<UiNode>,
}

/// Children of a node, paged for big arrays (STRC-10, STRC-12).
#[tauri::command]
pub fn tpl_children(
    tpl: State<'_, TplState>,
    key: u64,
    node: u32,
    start: u64,
    end: u64,
) -> CmdResult<Vec<UiNode>> {
    let (_, _, r) = tpl.get(key)?;
    Ok(hexa_template::children(&r, node, start..end)
        .into_iter()
        .map(ui)
        .collect())
}

/// Materialises one lazy struct element so its fields can be browsed.
#[tauri::command]
pub fn tpl_expand(
    tpl: State<'_, TplState>,
    key: u64,
    array: u32,
    index: u64,
) -> CmdResult<ChildrenOut> {
    let (doc, program, r) = tpl.get(key)?;
    let sub = hexa_template::expand_element(&r, array, index)
        .ok_or_else(|| CmdError::invalid("element has no fields"))?;
    let nodes = sub
        .roots
        .iter()
        .filter_map(|&id| sub.node(id).cloned())
        .flat_map(|root| {
            root.children
                .iter()
                .filter_map(|&c| sub.node(c).cloned())
                .collect::<Vec<_>>()
        })
        .map(ui)
        .collect();
    let (name, source, opts) = {
        let map = tpl.applied.lock();
        let a = map
            .get(&key)
            .ok_or_else(|| CmdError::invalid("template result is gone"))?;
        (a.name.clone(), a.source.clone(), a.opts.clone())
    };
    let sub_key = tpl.insert(Applied {
        doc,
        name,
        source,
        opts,
        program,
        result: Arc::new(sub),
        parent: Some(key),
    });
    Ok(ChildrenOut {
        key: sub_key,
        nodes,
    })
}

/// The field under an offset as a path of node ids (STRC-13, NAV-12).
#[tauri::command]
pub fn tpl_node_at(tpl: State<'_, TplState>, key: u64, offset: u64) -> CmdResult<Option<NodeHit>> {
    let (_, _, r) = tpl.get(key)?;
    Ok(hexa_template::node_at(&r, offset))
}

/// Byte ranges of the ancestors of the field under `offset`, innermost
/// first, for growing and shrinking the selection (NAV-12).
#[tauri::command]
pub fn tpl_ranges_at(
    tpl: State<'_, TplState>,
    key: u64,
    offset: u64,
) -> CmdResult<Vec<(u64, u64)>> {
    let (_, _, r) = tpl.get(key)?;
    let Some(hit) = hexa_template::node_at(&r, offset) else {
        return Ok(Vec::new());
    };
    let mut ranges: Vec<(u64, u64)> = hit
        .path
        .iter()
        .rev()
        .filter_map(|&id| r.node(id))
        .filter(|n| n.size > 0)
        .map(|n| (n.offset, n.offset + n.size))
        .collect();
    ranges.dedup();
    Ok(ranges)
}

/// Writes a new value into a field (STRC-14).
#[tauri::command]
pub fn tpl_set_value(
    state: State<'_, AppState>,
    tpl: State<'_, TplState>,
    key: u64,
    node: u32,
    elem: Option<u64>,
    text: String,
) -> CmdResult<EditResult> {
    let (doc, _, r) = tpl.get(key)?;
    let target = match elem {
        Some(i) => hexa_template::children(&r, node, i..i + 1)
            .into_iter()
            .next(),
        None => r.node(node).cloned(),
    }
    .ok_or_else(|| CmdError::invalid("no such field"))?;
    let bytes = query::encode_value(&r, &target, &text).map_err(terr)?;
    mutate(&state, doc, |d| {
        d.overwrite(target.offset, &bytes).map(Some)
    })
}

/// Node ids matching a filter, plus their ancestors (STRC-15).
#[tauri::command]
pub fn tpl_filter(tpl: State<'_, TplState>, key: u64, query: String) -> CmdResult<Vec<u32>> {
    let (_, _, r) = tpl.get(key)?;
    let mut ids = query::filter(&r, &query);
    let extra: Vec<u32> = ids.iter().flat_map(|&i| query::ancestors(&r, i)).collect();
    ids.extend(extra);
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

/// JSON, CSV or XML export of the tree (STRC-15, IO-07).
#[tauri::command]
pub fn tpl_export(
    tpl: State<'_, TplState>,
    key: u64,
    format: ExportFormat,
    path: PathBuf,
) -> CmdResult<()> {
    let (_, _, r) = tpl.get(key)?;
    std::fs::write(path, query::export(&r, format, &ExportOptions::default()))?;
    Ok(())
}

/// Field-level comparison of two applied templates (DIFF-10).
#[tauri::command]
pub fn tpl_diff(tpl: State<'_, TplState>, a: u64, b: u64) -> CmdResult<Vec<FieldChange>> {
    let (_, _, ra) = tpl.get(a)?;
    let (_, _, rb) = tpl.get(b)?;
    Ok(query::diff(&ra, &rb))
}

/// Every offset where `type_name` parses without errors (SRCH-11).
#[tauri::command]
pub async fn tpl_scan(
    state: State<'_, AppState>,
    id: u64,
    source: TplSource,
    type_name: String,
    start: u64,
    end: u64,
    step: Option<u64>,
) -> CmdResult<Vec<u64>> {
    let state = state.inner();
    let tree = state.get(id)?.view.read().tree.clone();
    let (job, cancel) = state.start_job();
    let r = tauri::async_runtime::spawn_blocking(move || {
        let (_, program) = compile(&source)?;
        hexa_template::scan::scan_valid(
            &tree,
            &program,
            &type_name,
            start..end.min(tree.len()),
            step.unwrap_or(1).max(1),
            &cancel,
        )
        .map_err(terr)
    })
    .await
    .map_err(|e| CmdError::new("internal", e.to_string()))?;
    state.end_job(job);
    r
}

/// Editor support: diagnostics, completions and hover (STRC-17).
#[tauri::command]
pub fn tpl_diagnostics(text: String, dialect: Option<String>) -> Vec<hexa_template::Diagnostic> {
    hexa_template::editor::diagnostics_with(
        &text,
        &ParseOptions {
            dialect: dialect_of(dialect.as_deref(), None),
            ..ParseOptions::default()
        },
    )
}

#[tauri::command]
pub fn tpl_completions(
    text: String,
    line: u32,
    col: u32,
) -> Vec<hexa_template::editor::Completion> {
    hexa_template::editor::completions(&text, line, col)
}

#[tauri::command]
pub fn tpl_library_source(id: String) -> CmdResult<String> {
    library::get(&id)
        .map(|e| e.source.to_owned())
        .ok_or_else(|| CmdError::invalid("unknown template"))
}

/// Type guesses for the bytes at an offset (STRC-20).
#[tauri::command]
pub fn tpl_suggest(
    state: State<'_, AppState>,
    id: u64,
    offset: u64,
) -> CmdResult<Vec<hexa_template::suggest::Suggestion>> {
    let tree = state.get(id)?.view.read().tree.clone();
    let bytes = tree.read_vec(offset, 64)?;
    Ok(hexa_template::suggest::suggest(&bytes, tree.len() - offset))
}

/// Exports a template as Kaitai, a C header or a parser (STRC-21, STRC-22).
#[tauri::command]
pub fn tpl_generate(source: TplSource, target: String, root: Option<String>) -> CmdResult<String> {
    use hexa_template::codegen::Language;
    let (_, program) = compile(&source)?;
    let root = root.unwrap_or_default();
    let lang = match target.as_str() {
        "ksy" => return hexa_template::export::to_ksy(&program, &root).map_err(terr),
        "c" => return hexa_template::export::to_c_header(&program).map_err(terr),
        "python" => Language::Python,
        "go" => Language::Go,
        "typescript" => Language::TypeScript,
        "rust" => Language::Rust,
        _ => return Err(CmdError::invalid("unknown target")),
    };
    hexa_template::codegen::generate(&program, &root, lang).map_err(terr)
}

#[cfg(test)]
mod tests {
    use hexa_core::piece::Piece;

    use super::*;

    #[test]
    fn applies_text_templates_and_reports_errors() {
        let src = TplSource::Text {
            text: "struct H { u32 magic; u16 n; };\nH h @ 0;".into(),
            dialect: None,
        };
        let (_, program) = compile(&src).unwrap();
        let tree = Tree::from_piece(Piece::bytes(vec![1, 2, 3, 4, 5, 6, 7]));
        let r = eval(&program, &tree, &EvalOptions::default()).unwrap();
        let o = out(1, "t", &r);
        assert_eq!(o.roots.len(), 1);
        assert_eq!(o.roots[0].child_count, 2);
        let spans: Vec<(u64, u64)> = o.highlights.iter().map(|h| (h.start, h.end)).collect();
        assert_eq!(spans, [(0, 4), (4, 6)]);
        let bad = TplSource::Text {
            text: "struct {".into(),
            dialect: None,
        };
        assert!(compile(&bad).is_err());
    }

    #[test]
    fn library_templates_compile() {
        for item in tpl_library() {
            compile(&TplSource::Library { id: item.id.clone() })
                .unwrap_or_else(|e| panic!("{}: {}", item.id, e.message));
        }
    }
}
