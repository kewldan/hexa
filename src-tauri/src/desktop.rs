//! Desktop integration: portable mode (UI-12) and the native macOS menu
//! (UI-14). Menu items carry command ids from the UI's command registry and
//! are executed by the frontend through the `hexa://menu` event.

use std::path::PathBuf;

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Emitter, Manager, Runtime};

/// Settings live next to the executable when a `portable` marker file or a
/// `hexa-data` folder exists there (UI-12).
pub fn portable_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let mut dir = exe.parent()?.to_path_buf();
    // Inside a macOS bundle the marker sits next to Hexa.app.
    if cfg!(target_os = "macos") && dir.ends_with("Contents/MacOS") {
        dir = dir.parent()?.parent()?.parent()?.to_path_buf();
    }
    let data = dir.join("hexa-data");
    if data.is_dir() || dir.join("portable").is_file() {
        std::fs::create_dir_all(&data).ok()?;
        Some(data)
    } else {
        None
    }
}

/// `(command id, title)`; an empty id is a separator.
type Items = &'static [(&'static str, &'static str)];

const FILE: Items = &[
    ("file.new", "Новый файл…"),
    ("file.open", "Открыть…"),
    ("file.save", "Сохранить"),
    ("file.saveAs", "Сохранить как…"),
    ("", ""),
    ("file.importFirmware", "Импорт прошивки…"),
    ("file.importDump", "Импорт дампа…"),
    ("file.exportAs", "Экспорт…"),
    ("file.print", "Печать…"),
    ("", ""),
    ("file.props", "Свойства файла"),
    ("file.close", "Закрыть вкладку"),
];
const EDIT: Items = &[
    ("edit.fill", "Заполнить…"),
    ("edit.insertBytes", "Вставить байты…"),
    ("edit.clipHistory", "История буфера обмена…"),
    ("edit.copyAs", "Копировать как…"),
];
const SEARCH: Items = &[
    ("search.find", "Найти…"),
    ("search.replace", "Заменить…"),
    ("search.inFiles", "Найти в файлах…"),
    ("nav.goto", "Перейти к смещению…"),
];
const VIEW: Items = &[
    ("view.palette", "Палитра команд"),
    ("", ""),
    ("view.split2", "Разделить на 2 панели"),
    ("view.unsplit", "Убрать разделение"),
    ("view.sideBySide", "Открыть рядом…"),
    ("", ""),
    ("view.foldRepeats", "Свернуть повторы"),
    ("view.unfoldAll", "Развернуть всё"),
];
const TOOLS: Items = &[
    ("diff.compare", "Сравнить с…"),
    ("diff.nway", "Сравнить несколько файлов…"),
    ("diff.merge3", "Трёхстороннее слияние…"),
    ("patch.create", "Создать патч…"),
    ("patch.apply", "Применить патч…"),
    ("", ""),
    ("tools.calc", "Калькулятор"),
    ("file.split", "Разделить файл на части…"),
    ("file.concat", "Склеить файлы…"),
];
const HELP: Items = &[
    ("help.commands", "Справка по командам"),
    ("help.update", "Проверить обновления"),
    ("help.about", "О программе"),
];

fn submenu<R: Runtime>(app: &AppHandle<R>, title: &str, items: Items) -> tauri::Result<Submenu<R>> {
    let menu = Submenu::new(app, title, true)?;
    for (id, text) in items {
        if id.is_empty() {
            menu.append(&PredefinedMenuItem::separator(app)?)?;
        } else {
            menu.append(&MenuItem::with_id(app, *id, *text, true, None::<&str>)?)?;
        }
    }
    Ok(menu)
}

/// The application menu: native app/edit/window items plus Hexa commands.
pub fn menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let app_menu = Submenu::with_items(
        app,
        "Hexa",
        true,
        &[
            &PredefinedMenuItem::about(app, Some("О Hexa"), None)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "app.settings", "Настройки…", true, Some("CmdOrCtrl+,"))?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::services(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::hide(app, None)?,
            &PredefinedMenuItem::hide_others(app, None)?,
            &PredefinedMenuItem::show_all(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::quit(app, None)?,
        ],
    )?;
    let edit = submenu(app, "Правка", EDIT)?;
    // Text fields need the native clipboard items on macOS.
    edit.prepend_items(&[
        &PredefinedMenuItem::undo(app, None)?,
        &PredefinedMenuItem::redo(app, None)?,
        &PredefinedMenuItem::separator(app)?,
        &PredefinedMenuItem::cut(app, None)?,
        &PredefinedMenuItem::copy(app, None)?,
        &PredefinedMenuItem::paste(app, None)?,
        &PredefinedMenuItem::select_all(app, None)?,
        &PredefinedMenuItem::separator(app)?,
    ])?;
    let window = Submenu::with_items(
        app,
        "Окно",
        true,
        &[
            &MenuItem::with_id(app, "window.new", "Новое окно", true, None::<&str>)?,
            &PredefinedMenuItem::minimize(app, None)?,
            &PredefinedMenuItem::maximize(app, None)?,
            &PredefinedMenuItem::fullscreen(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::close_window(app, None)?,
        ],
    )?;
    Menu::with_items(
        app,
        &[
            &app_menu,
            &submenu(app, "Файл", FILE)?,
            &edit,
            &submenu(app, "Поиск", SEARCH)?,
            &submenu(app, "Вид", VIEW)?,
            &submenu(app, "Инструменты", TOOLS)?,
            &window,
            &submenu(app, "Справка", HELP)?,
        ],
    )
}

/// Installs the native menu (macOS only: elsewhere the in-window UI is the menu).
pub fn install<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    if !cfg!(target_os = "macos") {
        return Ok(());
    }
    app.set_menu(menu(app)?)?;
    app.on_menu_event(|app, event| {
        let id = event.id().0.clone();
        // Send to the focused window; fall back to every window.
        let focused = app
            .webview_windows()
            .into_values()
            .find(|w| w.is_focused().unwrap_or(false));
        let _ = match focused {
            Some(w) => w.emit("hexa://menu", id),
            None => app.emit("hexa://menu", id),
        };
    });
    Ok(())
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub version: &'static str,
    pub portable: bool,
    pub config_dir: String,
}

#[tauri::command]
pub fn app_info(state: tauri::State<'_, crate::state::AppState>) -> AppInfo {
    AppInfo {
        version: env!("CARGO_PKG_VERSION"),
        portable: portable_dir().is_some(),
        config_dir: state.config_dir.read().to_string_lossy().into_owned(),
    }
}

/// Prints the window; the UI shows only the print sheet under `@media print` (IO-06).
#[tauri::command]
pub fn app_print(window: tauri::WebviewWindow) -> crate::error::CmdResult<()> {
    window.print()?;
    Ok(())
}
