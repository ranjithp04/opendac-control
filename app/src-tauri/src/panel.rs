//! App window and menu bar icon: a normal 380x720 window with a Dock icon,
//! plus a tray / menu bar icon as a second way in. Closing the window hides
//! it; the app keeps running (live sync, re-apply after a replug) until Quit.

use tauri::image::Image;
use tauri::menu::MenuBuilder;
#[cfg(target_os = "macos")]
use tauri::menu::{Menu, MenuItemBuilder, PredefinedMenuItem, SubmenuBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Window, WindowEvent};

/// Label of the app window (tauri.conf.json).
pub const WINDOW_LABEL: &str = "main";
const TRAY_ID: &str = "main";
const MENU_QUIT: &str = "quit";
const MENU_SHOW: &str = "show";

/// Set up the app menu (macOS) and the tray icon. Called from `setup`.
pub fn init(app: &mut tauri::App) -> tauri::Result<()> {
    #[cfg(target_os = "macos")]
    app.set_menu(app_menu(app.handle())?)?;

    let menu = MenuBuilder::new(app)
        .text(MENU_SHOW, "Show OpenDAC Control")
        .separator()
        .text(MENU_QUIT, "Quit OpenDAC Control")
        .build()?;
    // Template image: macOS tints it for the light / dark menu bar.
    let icon = Image::from_bytes(include_bytes!("../icons/tray.png"))?;
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .icon_as_template(true)
        .tooltip("OpenDAC Control")
        .menu(&menu)
        // Left click shows the window, right click opens the menu.
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| on_menu(app, event.id().as_ref()))
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

/// macOS app menu: Quit really quits (closing the window does not), and the
/// Edit menu makes copy / paste work in text fields.
#[cfg(target_os = "macos")]
fn app_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let quit = MenuItemBuilder::with_id(MENU_QUIT, "Quit OpenDAC Control")
        .accelerator("CmdOrCtrl+Q")
        .build(app)?;
    let app_sub = SubmenuBuilder::new(app, "OpenDAC Control")
        .item(&PredefinedMenuItem::about(app, None, None)?)
        .separator()
        .item(&PredefinedMenuItem::hide(app, None)?)
        .item(&PredefinedMenuItem::hide_others(app, None)?)
        .separator()
        .item(&quit)
        .build()?;
    let edit = SubmenuBuilder::new(app, "Edit")
        .undo()
        .redo()
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
        .build()?;
    let window = SubmenuBuilder::new(app, "Window")
        .minimize()
        .close_window()
        .build()?;
    let menu = MenuBuilder::new(app)
        .items(&[&app_sub, &edit, &window])
        .build()?;
    app.on_menu_event(|app, event| on_menu(app, event.id().as_ref()));
    Ok(menu)
}

fn on_menu(app: &AppHandle, id: &str) {
    match id {
        MENU_QUIT => app.exit(0),
        MENU_SHOW => show(app),
        _ => {}
    }
}

/// Window events: closing hides the window, the app keeps running.
pub fn on_window_event(window: &Window, event: &WindowEvent) {
    if window.label() != WINDOW_LABEL {
        return;
    }
    if let WindowEvent::CloseRequested { api, .. } = event {
        api.prevent_close();
        let _ = window.hide();
    }
}

/// Show and focus the window, then tell the UI.
pub fn show(app: &AppHandle) {
    let Some(window) = app.get_webview_window(WINDOW_LABEL) else {
        return;
    };
    let _ = window.unminimize();
    let _ = window.show();
    let _ = window.set_focus();
    let _ = window.emit("panel-shown", ());
}

/// Hide the window (the `hide_panel` command).
pub fn hide(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(WINDOW_LABEL) {
        let _ = w.hide();
    }
}
