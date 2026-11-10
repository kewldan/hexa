//! Statistics, visualisations, strings, embedded files and text firmware /
//! dump formats (ANL-03…17, IO-01…04, SRCH-10).

use std::path::PathBuf;

use hexa_analysis::bitmap::{BitmapImage, BitmapOptions};
use hexa_analysis::classify::Region;
use hexa_analysis::dump::{DumpFormat, DumpTextFormat};
use hexa_analysis::embedded::{Embedded, EmbeddedOptions};
use hexa_analysis::firmware::{ExportOptions, FirmwareFormat, LineError};
use hexa_analysis::plot::{PlotOptions, PlotSeries};
use hexa_analysis::stats::{EntropySeries, Histogram};
use hexa_analysis::strings::{StringsOptions, StringsResult};
use hexa_analysis::visual::{Digraph, HilbertMap};
use hexa_core::piece::Piece;
use hexa_core::task::no_progress;
use hexa_core::{Cancel, Document, Tree};
use serde::Serialize;
use tauri::ipc::Response;
use tauri::{AppHandle, State};

use super::doc::opened_doc;
use crate::error::{CmdError, CmdResult};
use crate::state::{AppState, DocInfo};

fn tree_of(state: &AppState, id: u64) -> CmdResult<Tree> {
    Ok(state.get(id)?.view.read().tree.clone())
}

/// Runs `f` on a blocking thread as a cancellable job.
async fn job<T: Send + 'static>(
    state: &AppState,
    f: impl FnOnce(&Cancel) -> hexa_core::Result<T> + Send + 'static,
) -> CmdResult<T> {
    let (job, cancel) = state.start_job();
    let r = tauri::async_runtime::spawn_blocking(move || f(&cancel))
        .await
        .map_err(|e| CmdError::new("internal", e.to_string()));
    state.end_job(job);
    Ok(r??)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Overview {
    pub histogram: Histogram,
    pub entropy: EntropySeries,
    pub regions: Vec<Region>,
}

/// Histogram, entropy curve and region classification (ANL-07…09).
#[tauri::command]
pub async fn anl_overview(
    state: State<'_, AppState>,
    id: u64,
    start: u64,
    end: u64,
    points: Option<u64>,
) -> CmdResult<Overview> {
    let tree = tree_of(&state, id)?;
    job(&state, move |c| {
        let histogram = hexa_analysis::stats::histogram(&tree, start, end, c, &no_progress)?;
        let entropy = hexa_analysis::stats::entropy_minimap(
            &tree,
            start,
            end,
            points.unwrap_or(512),
            c,
            &no_progress,
        )?;
        // Blocks scale with the range so huge files stay quick.
        let block = ((end - start) / 4096).next_power_of_two().max(4096);
        let regions = hexa_analysis::classify::classify(&tree, start, end, block, c, &no_progress)?;
        Ok(Overview {
            histogram,
            entropy,
            regions,
        })
    })
    .await
}

#[tauri::command]
pub async fn anl_digraph(
    state: State<'_, AppState>,
    id: u64,
    start: u64,
    end: u64,
    size: Option<u32>,
) -> CmdResult<Digraph> {
    let tree = tree_of(&state, id)?;
    job(&state, move |c| {
        hexa_analysis::visual::digraph(&tree, start, end, size.unwrap_or(256), c, &no_progress)
    })
    .await
}

#[tauri::command]
pub async fn anl_hilbert(
    state: State<'_, AppState>,
    id: u64,
    start: u64,
    end: u64,
    order: Option<u32>,
) -> CmdResult<HilbertMap> {
    let tree = tree_of(&state, id)?;
    job(&state, move |c| {
        hexa_analysis::visual::hilbert_map(&tree, start, end, order.unwrap_or(8), c, &no_progress)
    })
    .await
}

/// String extraction (ANL-11).
#[tauri::command]
pub async fn anl_strings(
    state: State<'_, AppState>,
    id: u64,
    start: u64,
    end: u64,
    opts: StringsOptions,
) -> CmdResult<StringsResult> {
    let tree = tree_of(&state, id)?;
    job(&state, move |c| {
        hexa_analysis::strings::extract_strings(&tree, start, end, &opts, c, &no_progress)
    })
    .await
}

/// Files embedded in the data (ANL-16, SRCH-10).
#[tauri::command]
pub async fn anl_embedded(
    state: State<'_, AppState>,
    id: u64,
    start: u64,
    end: u64,
) -> CmdResult<Vec<Embedded>> {
    let tree = tree_of(&state, id)?;
    job(&state, move |c| {
        hexa_analysis::embedded::find_embedded(
            &tree,
            start,
            end,
            &EmbeddedOptions::default(),
            c,
            &no_progress,
        )
    })
    .await
}

/// Numeric plot (ANL-17).
#[tauri::command]
pub async fn anl_plot(
    state: State<'_, AppState>,
    id: u64,
    start: u64,
    end: u64,
    opts: PlotOptions,
) -> CmdResult<PlotSeries> {
    let tree = tree_of(&state, id)?;
    job(&state, move |c| {
        hexa_analysis::plot::plot_series(&tree, start, end, &opts, c, &no_progress)
    })
    .await
}

fn rgba_response(width: u32, height: u32, rgba: Vec<u8>) -> Response {
    let mut out = Vec::with_capacity(8 + rgba.len());
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.extend_from_slice(&rgba);
    Response::new(out)
}

/// Raw bitmap view (ANL-15): `[width u32le, height u32le, rgba…]`.
#[tauri::command]
pub async fn anl_bitmap(
    state: State<'_, AppState>,
    id: u64,
    opts: BitmapOptions,
) -> CmdResult<Response> {
    let tree = tree_of(&state, id)?;
    let img: BitmapImage = job(&state, move |c| {
        hexa_analysis::bitmap::decode_bitmap(&tree, &opts, c)
    })
    .await?;
    Ok(rgba_response(img.width, img.height, img.rgba))
}

/// Decodes an image file inside the data (ANL-16), same layout as `anl_bitmap`.
#[tauri::command]
pub async fn anl_image(
    state: State<'_, AppState>,
    id: u64,
    start: u64,
    end: u64,
) -> CmdResult<Response> {
    let tree = tree_of(&state, id)?;
    let img = job(&state, move |_| {
        hexa_analysis::preview::decode_image_range(&tree, start, end)
    })
    .await?;
    Ok(rgba_response(img.width, img.height, img.rgba))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FuzzyCompare {
    pub ssdeep: [String; 2],
    pub tlsh: [Option<String>; 2],
    /// ssdeep similarity 0–100.
    pub score: Option<u32>,
    /// TLSH distance (0 = identical, > 200 = unrelated).
    pub distance: Option<u32>,
}

/// Similarity of two documents (ANL-04).
#[tauri::command]
pub async fn anl_fuzzy_compare(
    state: State<'_, AppState>,
    a: u64,
    b: u64,
) -> CmdResult<FuzzyCompare> {
    let (ta, tb) = (tree_of(&state, a)?, tree_of(&state, b)?);
    job(&state, move |c| {
        let ids = ["ssdeep".to_owned(), "tlsh".to_owned()];
        let ha = hexa_analysis::hash::hash_range(&ta, 0, ta.len(), &ids, None, c, &no_progress)?;
        let hb = hexa_analysis::hash::hash_range(&tb, 0, tb.len(), &ids, None, c, &no_progress)?;
        let get = |v: &[hexa_core::hash::HashResult], id: &str| {
            v.iter()
                .find(|h| h.id == id)
                .map(|h| h.value.clone())
                .filter(|s| !s.is_empty())
        };
        let (sa, sb) = (
            get(&ha, "ssdeep").unwrap_or_default(),
            get(&hb, "ssdeep").unwrap_or_default(),
        );
        let (ta, tb) = (get(&ha, "tlsh"), get(&hb, "tlsh"));
        let score = hexa_analysis::fuzzy::ssdeep_compare(&sa, &sb).ok();
        let distance = match (&ta, &tb) {
            (Some(x), Some(y)) => hexa_analysis::fuzzy::tlsh_diff(x, y, true).ok(),
            _ => None,
        };
        Ok(FuzzyCompare {
            ssdeep: [sa, sb],
            tlsh: [ta, tb],
            score,
            distance,
        })
    })
    .await
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Imported {
    pub info: DocInfo,
    pub base: u64,
    pub segments: usize,
    pub errors: Vec<LineError>,
    pub warnings: Vec<LineError>,
    pub format: String,
}

fn bytes_tree(data: Vec<u8>) -> Tree {
    if data.is_empty() {
        Tree::new()
    } else {
        Tree::from_piece(Piece::bytes(data))
    }
}

/// Opens Intel HEX, S-Record, TI-TXT or Tektronix files as a flat image
/// (IO-01, IO-02); gaps are filled with `fill`.
#[tauri::command]
pub fn fw_import(
    app: AppHandle,
    state: State<'_, AppState>,
    path: PathBuf,
    format: Option<FirmwareFormat>,
    fill: Option<u8>,
) -> CmdResult<Imported> {
    let text = std::fs::read_to_string(&path)?;
    let img = hexa_analysis::firmware::import(&text, format)?;
    let flat = hexa_analysis::firmware::flatten(
        &img.segments,
        fill.unwrap_or(0xFF),
        None,
        None,
        hexa_analysis::firmware::MAX_FLAT_LEN,
    )?;
    let name = path
        .file_name()
        .map_or_else(|| "firmware".into(), |s| s.to_string_lossy().into_owned());
    let info = opened_doc(
        &app,
        &state,
        Document::from_tree(name, bytes_tree(flat.data)),
    );
    Ok(Imported {
        info,
        base: flat.address,
        segments: img.segments.len(),
        errors: img.errors,
        warnings: Vec::new(),
        format: format!("{:?}", img.format),
    })
}

/// Exports a range as a text firmware file at `base` (IO-01).
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn fw_export(
    state: State<'_, AppState>,
    id: u64,
    start: u64,
    end: u64,
    base: u64,
    format: FirmwareFormat,
    opts: Option<ExportOptions>,
    path: PathBuf,
) -> CmdResult<()> {
    let tree = tree_of(&state, id)?;
    if end - start > hexa_analysis::firmware::MAX_FLAT_LEN {
        return Err(CmdError::invalid("range is too large for a firmware file"));
    }
    let data = tree.read_vec(start, (end - start) as usize)?;
    let text =
        hexa_analysis::firmware::export_bytes(&data, base, format, &opts.unwrap_or_default())?;
    std::fs::write(path, text)?;
    Ok(())
}

/// Writes a text dump of a range: xxd, hexdump -C, HTML, RTF, Markdown,
/// CSV or a C header (IO-03).
#[tauri::command]
pub async fn dump_export(
    state: State<'_, AppState>,
    id: u64,
    start: u64,
    end: u64,
    format: DumpFormat,
    path: PathBuf,
) -> CmdResult<()> {
    let tree = tree_of(&state, id)?;
    job(&state, move |c| {
        let file = std::fs::File::create(&path)?;
        let mut w = std::io::BufWriter::new(file);
        hexa_analysis::dump::dump_range(&tree, start, end, &format, &mut w, c, &no_progress)?;
        std::io::Write::flush(&mut w)?;
        Ok(())
    })
    .await
}

/// Opens an xxd / hexdump -C / od / plain hex text dump (IO-04).
#[tauri::command]
pub fn dump_import(
    app: AppHandle,
    state: State<'_, AppState>,
    path: PathBuf,
    format: Option<DumpTextFormat>,
) -> CmdResult<Imported> {
    let text = std::fs::read_to_string(&path)?;
    let imp = hexa_analysis::dump::import_dump(&text, format)?;
    let flat = hexa_analysis::firmware::flatten(
        &imp.segments,
        0,
        None,
        None,
        hexa_analysis::firmware::MAX_FLAT_LEN,
    )?;
    let name = path
        .file_stem()
        .map_or_else(|| "dump".into(), |s| s.to_string_lossy().into_owned());
    let info = opened_doc(
        &app,
        &state,
        Document::from_tree(name, bytes_tree(flat.data)),
    );
    Ok(Imported {
        info,
        base: flat.address,
        segments: imp.segments.len(),
        errors: imp.errors,
        warnings: imp.warnings,
        format: format!("{:?}", imp.format),
    })
}
