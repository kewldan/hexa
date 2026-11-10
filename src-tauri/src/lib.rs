//! Hexa desktop shell: a thin Tauri layer over `hexa-core`.

// Tauri commands receive their arguments by value.
#![allow(clippy::needless_pass_by_value)]

mod clipboard;
mod commands;
mod desktop;
mod elevate;
mod error;
mod recent;
mod recovery;
mod script;
mod state;
mod watch;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::state::AppState;

static WINDOWS: AtomicU32 = AtomicU32::new(0);

/// Creates an editor window; `query` is appended to the page URL.
pub(crate) fn create_window(app: &AppHandle, query: &str) -> tauri::Result<()> {
    let n = WINDOWS.fetch_add(1, Ordering::Relaxed);
    let label = if n == 0 {
        "main".to_owned()
    } else {
        format!("w{n}")
    };
    let builder = WebviewWindowBuilder::new(
        app,
        label,
        WebviewUrl::App(format!("index.html{query}").into()),
    )
    .title("Hexa")
    .inner_size(1280.0, 820.0)
    .min_inner_size(640.0, 420.0)
    .disable_drag_drop_handler()
    .visible(true);
    #[cfg(target_os = "macos")]
    let builder = builder
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .hidden_title(true)
        .traffic_light_position(tauri::LogicalPosition::new(20.0, 26.0));
    builder.build()?;
    Ok(())
}

fn file_args(args: impl IntoIterator<Item = String>, cwd: Option<&str>) -> Vec<PathBuf> {
    args.into_iter()
        .skip(1)
        .filter(|a| !a.starts_with('-'))
        .map(|a| {
            let p = PathBuf::from(&a);
            match cwd {
                Some(dir) if p.is_relative() => PathBuf::from(dir).join(p),
                _ => p,
            }
        })
        .filter(|p| p.is_file())
        .collect()
}

fn open_files(app: &AppHandle, files: Vec<PathBuf>) {
    if files.is_empty() {
        return;
    }
    let paths: Vec<String> = files
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    if let Some(w) = app
        .get_webview_window("main")
        .or_else(|| app.webview_windows().into_values().next())
    {
        let _ = w.set_focus();
        let _ = w.emit("hexa://open-files", paths);
    } else {
        app.state::<AppState>().initial_files.lock().extend(files);
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, argv, cwd| {
            let files = file_args(argv, Some(&cwd));
            if files.is_empty() {
                let _ = create_window(app, "");
            } else {
                open_files(app, files);
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::default())
        .manage(watch::Watcher::default())
        .manage(script::ScriptState::default())
        .manage(commands::sources::SourceState::default())
        .manage(commands::memory::MemState::default())
        .setup(|app| {
            let handle = app.handle();
            let state = app.state::<AppState>();
            *state.config_dir.write() = desktop::portable_dir().unwrap_or_else(|| {
                app.path()
                    .app_config_dir()
                    .unwrap_or_else(|_| std::env::temp_dir().join("hexa"))
            });
            desktop::install(handle)?;
            *state.initial_files.lock() = file_args(std::env::args(), None);
            watch::init(handle);
            recovery::start(handle);
            create_window(handle, "")?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::doc::doc_open,
            commands::doc::doc_new,
            commands::doc::doc_attach,
            commands::doc::doc_release,
            commands::doc::doc_info,
            commands::doc::doc_read,
            commands::doc::doc_changes,
            commands::doc::doc_type,
            commands::doc::doc_edit,
            commands::doc::doc_undo,
            commands::doc::doc_redo,
            commands::doc::doc_history,
            commands::doc::doc_history_goto,
            commands::doc::doc_history_revert,
            commands::doc::doc_set_mode,
            commands::doc::doc_set_fixed,
            commands::doc::doc_save,
            commands::doc::doc_save_as,
            commands::doc::doc_save_copy,
            commands::doc::doc_save_elevated,
            commands::doc::doc_reload,
            commands::doc::doc_keep_mine,
            commands::doc::doc_set_watch_mode,
            commands::doc::doc_props,
            commands::doc::journal_pending,
            commands::doc::journal_rollback,
            commands::doc::journal_discard,
            commands::doc::inspect,
            commands::doc::inspector_set,
            commands::doc::marks_list,
            commands::doc::marks_add,
            commands::doc::marks_update,
            commands::doc::marks_remove,
            commands::doc::marks_set,
            commands::doc::inspect_ext,
            commands::doc::replace_plan,
            commands::doc::replace_apply,
            commands::doc::find_runs,
            commands::compare::diff_run,
            commands::compare::diff_export,
            commands::compare::diff_apply,
            commands::compare::merge3,
            commands::compare::patch_inspect,
            commands::compare::patch_create,
            commands::compare::patch_apply,
            commands::transform::xfrm_preview,
            commands::transform::xfrm_apply,
            commands::transform::xfrm_to_new,
            commands::transform::xfrm_invert,
            commands::transform::xfrm_write_back,
            commands::transform::xfrm_graph,
            commands::transform::xfrm_xor_guess,
            commands::analysis::anl_overview,
            commands::analysis::anl_digraph,
            commands::analysis::anl_hilbert,
            commands::analysis::anl_strings,
            commands::analysis::anl_embedded,
            commands::analysis::anl_plot,
            commands::analysis::anl_bitmap,
            commands::analysis::anl_image,
            commands::analysis::anl_fuzzy_compare,
            commands::analysis::fw_import,
            commands::analysis::fw_export,
            commands::analysis::dump_export,
            commands::analysis::dump_import,
            commands::re::re_info,
            commands::re::re_arches,
            commands::re::re_disasm,
            commands::re::re_assemble,
            commands::re::re_patch_plan,
            commands::re::re_xrefs,
            commands::re::re_pointers,
            commands::re::re_fix_checksum,
            commands::re::re_patches_export,
            commands::re::re_patches_check,
            commands::re::re_patches_apply,
            commands::re::re_patches_port,
            commands::re::re_tools,
            commands::re::re_tool_prepare,
            commands::re::re_tool_spawn,
            desktop::app_info,
            desktop::app_print,
            commands::files::files_search,
            commands::files::file_split,
            commands::files::files_concat,
            commands::tools::job_cancel,
            commands::tools::search_next,
            commands::tools::search_all,
            commands::tools::hash_algorithms,
            commands::tools::crc_presets,
            commands::tools::hash_run,
            commands::tools::hash_quick,
            commands::tools::encodings,
            commands::tools::encoding_table,
            commands::tools::encode_text,
            commands::tools::decode_bytes,
            commands::tools::parse_hex,
            commands::tools::clip_copy,
            commands::tools::copy_as,
            commands::tools::clip_info,
            commands::tools::clip_paste,
            commands::tools::clip_paste_auto,
            commands::tools::clip_parse,
            commands::app::settings_get,
            commands::app::settings_set,
            commands::app::recent_list,
            commands::app::recent_pin,
            commands::app::recent_remove,
            commands::app::app_initial_files,
            commands::app::window_new,
            commands::app::workspace_save,
            commands::app::workspace_load,
            commands::app::read_text_file,
            commands::app::write_text_file,
            commands::app::recovery_list,
            commands::app::recovery_restore,
            commands::app::recovery_discard,
            commands::script::script_run,
            commands::script::script_repl_eval,
            commands::script::script_complete,
            commands::script::script_repl_reset,
            commands::script::script_cancel,
            commands::script::script_prompt_reply,
            commands::script::script_set_context,
            commands::script::macro_save,
            commands::script::macro_load,
            commands::script::macro_to_lua,
            commands::script::plugins_list,
            commands::script::plugins_reload,
            commands::script::plugins_open_dir,
            commands::script::plugins_run,
            commands::script::catalog_read,
            commands::script::catalog_install,
            commands::script::api_server_start,
            commands::script::api_server_stop,
            commands::script::api_server_set_port,
            commands::script::api_server_status,
            commands::script::api_server_regenerate_token,
            commands::script::predicate_search,
            commands::sources::src_info,
            commands::sources::src_commit_preview,
            commands::sources::src_commit,
            commands::sources::src_rollback,
            commands::sources::disk_list,
            commands::sources::disk_open,
            commands::sources::image_open,
            commands::sources::raid_open,
            commands::sources::disk_layout,
            commands::sources::fs_list,
            commands::sources::fs_deleted,
            commands::sources::fs_extents,
            commands::sources::fs_map_build,
            commands::sources::fs_owner_at,
            commands::sources::fs_open_file,
            commands::sources::fs_extract,
            commands::sources::disk_carve,
            commands::sources::disk_clone,
            commands::sources::disk_wipe,
            commands::sources::file_shred,
            commands::sources::file_streams,
            commands::sources::file_stream_open,
            commands::sources::file_stream_save,
            commands::sources::file_stream_remove,
            commands::sources::archive_list,
            commands::sources::archive_open_entry,
            commands::sources::archive_repack,
            commands::sources::net_open_http,
            commands::sources::net_open_s3,
            commands::sources::net_open_sftp,
            commands::sources::net_capture_start,
            commands::sources::net_capture_status,
            commands::sources::net_capture_control,
            commands::memory::proc_list,
            commands::memory::proc_privileges,
            commands::memory::proc_open,
            commands::memory::gdb_connect,
            commands::memory::mem_map,
            commands::memory::scan_first,
            commands::memory::scan_next,
            commands::memory::scan_page,
            commands::memory::scan_reset,
            commands::memory::mem_write_value,
            commands::memory::mem_freeze,
            commands::memory::mem_frozen,
            commands::memory::mem_freeze_control,
            commands::memory::ptr_scan,
            commands::memory::ptr_rescan,
            commands::memory::ptr_results,
            commands::memory::mem_dump,
        ])
        .build(tauri::generate_context!())
        .expect("failed to build the Tauri application");

    app.run(|handle, event| {
        // macOS "Open with" / Finder double click (FILE-01).
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        if let tauri::RunEvent::Opened { urls } = &event {
            let files = urls.iter().filter_map(|u| u.to_file_path().ok()).collect();
            open_files(handle, files);
        }
        let _ = (handle, event);
    });
}
