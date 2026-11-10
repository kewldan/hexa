//! Data transformations of a selection (XFRM-01…12).

use std::collections::BTreeMap;

use hexa_core::Document;
use hexa_core::Tree;
use hexa_core::piece::Piece;
use hexa_transform::{Graph, KeySizeScore, Preview, Recipe, TransformError, XorKeyCandidate};
use serde::Serialize;
use tauri::{AppHandle, State};

use super::doc::{EditResult, mutate, opened_doc};
use crate::error::{CmdError, CmdResult};
use crate::state::{AppState, DocInfo};

/// Largest selection a transformation reads into memory.
const MAX_INPUT: u64 = 256 << 20;

impl From<TransformError> for CmdError {
    fn from(e: TransformError) -> Self {
        Self::new("transform", e.to_string())
    }
}

fn read_range(state: &AppState, id: u64, start: u64, end: u64, max: u64) -> CmdResult<Vec<u8>> {
    let entry = state.get(id)?;
    let tree = entry.view.read().tree.clone();
    let end = end.min(tree.len());
    if end < start {
        return Err(CmdError::invalid("range out of bounds"));
    }
    if end - start > max {
        return Err(CmdError::invalid(format!(
            "selection is larger than {} MiB",
            max >> 20
        )));
    }
    let mut buf = vec![0; (end - start) as usize];
    tree.read(start, &mut buf)?;
    Ok(buf)
}

/// Live preview of a recipe on a selection (XFRM-12).
#[tauri::command]
pub async fn xfrm_preview(
    state: State<'_, AppState>,
    id: u64,
    start: u64,
    end: u64,
    recipe: Recipe,
    max_out: Option<usize>,
) -> CmdResult<Preview> {
    let data = read_range(state.inner(), id, start, end, MAX_INPUT)?;
    let max_out = max_out.unwrap_or(4096);
    tauri::async_runtime::spawn_blocking(move || hexa_transform::preview(&recipe, &data, max_out))
        .await
        .map_err(|e| CmdError::new("internal", e.to_string()))?
        .map_err(Into::into)
}

/// Replaces the selection with the recipe's output (XFRM-10).
#[tauri::command]
pub fn xfrm_apply(
    state: State<'_, AppState>,
    id: u64,
    start: u64,
    end: u64,
    recipe: Recipe,
) -> CmdResult<EditResult> {
    let label = recipe.name.clone().unwrap_or_else(|| {
        let names: Vec<&str> = recipe
            .steps
            .iter()
            .map(hexa_transform::Step::name)
            .collect();
        format!("Transform: {}", names.join(" → "))
    });
    mutate(&state, id, |d| {
        d.transform(start, end, &label, |buf| {
            *buf = hexa_transform::apply(&recipe, buf)
                .map_err(|e| hexa_core::Error::Invalid(e.to_string()))?;
            Ok(())
        })
        .map(Some)
    })
}

/// Opens the recipe's output in a new buffer.
#[tauri::command]
pub fn xfrm_to_new(
    app: AppHandle,
    state: State<'_, AppState>,
    id: u64,
    start: u64,
    end: u64,
    recipe: Recipe,
) -> CmdResult<DocInfo> {
    let data = read_range(&state, id, start, end, MAX_INPUT)?;
    let out = hexa_transform::apply(&recipe, &data)?;
    let tree = if out.is_empty() {
        Tree::new()
    } else {
        Tree::from_piece(Piece::bytes(out))
    };
    Ok(opened_doc(
        &app,
        &state,
        Document::from_tree("Transformed", tree),
    ))
}

/// The recipe that undoes `recipe` (XFRM-09).
#[tauri::command]
pub fn xfrm_invert(recipe: Recipe) -> CmdResult<Recipe> {
    Ok(recipe.invert()?)
}

/// Writes edited output back through an invertible recipe (XFRM-09, CORE-03).
#[tauri::command]
pub fn xfrm_write_back(
    state: State<'_, AppState>,
    id: u64,
    start: u64,
    end: u64,
    recipe: Recipe,
    output: Vec<u8>,
) -> CmdResult<EditResult> {
    let original = hexa_transform::reverse_apply(&recipe.steps, &output)?;
    mutate(&state, id, |d| d.replace(start, end, &original).map(Some))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphOutput {
    pub name: String,
    pub preview: Vec<u8>,
    pub len: usize,
}

/// Evaluates a node graph on the selection (XFRM-11).
#[tauri::command]
pub fn xfrm_graph(
    state: State<'_, AppState>,
    id: u64,
    start: u64,
    end: u64,
    graph: Graph,
) -> CmdResult<Vec<GraphOutput>> {
    let data = read_range(&state, id, start, end, MAX_INPUT)?;
    let outs: BTreeMap<String, Vec<u8>> = hexa_transform::evaluate(&graph, &data)?;
    Ok(outs
        .into_iter()
        .map(|(name, v)| GraphOutput {
            name,
            len: v.len(),
            preview: v[..v.len().min(4096)].to_vec(),
        })
        .collect())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct XorGuess {
    pub single: Vec<XorKeyCandidate>,
    pub repeating: Vec<XorKeyCandidate>,
    pub sizes: Vec<KeySizeScore>,
}

/// Guesses XOR keys of the selection (XFRM-03).
#[tauri::command]
pub async fn xfrm_xor_guess(
    state: State<'_, AppState>,
    id: u64,
    start: u64,
    end: u64,
    max_key_len: Option<usize>,
) -> CmdResult<XorGuess> {
    let end = end.min(start + (1 << 20));
    let data = read_range(state.inner(), id, start, end, MAX_INPUT)?;
    let max_key = max_key_len.unwrap_or(32).clamp(1, 40);
    tauri::async_runtime::spawn_blocking(move || XorGuess {
        single: hexa_transform::guess_single_byte_xor(&data, 8),
        repeating: hexa_transform::guess_repeating_xor(&data, max_key, 8),
        sizes: hexa_transform::estimate_key_sizes(&data, max_key),
    })
    .await
    .map_err(|e| CmdError::new("internal", e.to_string()))
}
