//! App window and menu bar icon: a normal 1280x720 window with a Dock icon,
//! plus a tray / menu bar icon as a second way in. Closing the window hides
//! it; the app keeps running (live sync, re-apply after a replug) until Quit.
//!
//! The tray menu (right click) is built from the latest snapshot
//! (`tray_menu.rs`) and replaced when what it shows changes ([`sync_tray`]).

use std::sync::{Arc, Mutex};

use tauri::image::Image;
#[cfg(target_os = "macos")]
use tauri::menu::PredefinedMenuItem;
use tauri::menu::{CheckMenuItemBuilder, Menu, MenuBuilder, MenuItemBuilder, SubmenuBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Window, WindowEvent};

use crate::api::AppState;
use crate::backend::Backend;
use crate::tray_menu::{self, MenuEntry, TrayAction};

/// Label of the app window (tauri.conf.json).
pub const WINDOW_LABEL: &str = "main";
const TRAY_ID: &str = "main";
const MENU_QUIT: &str = tray_menu::QUIT;
const MENU_SHOW: &str = tray_menu::SHOW;

/// Revision of the snapshot the tray menu was built from, and the menu (to
/// rebuild it only when it changes).
struct TrayMenuCache(Mutex<(u64, Vec<MenuEntry>)>);

/// Set up the app menu (macOS) and the tray icon. Called from `setup`.
pub fn init(app: &mut tauri::App) -> tauri::Result<()> {
    #[cfg(target_os = "macos")]
    app.set_menu(app_menu(app.handle())?)?;
    // One handler for every menu: Tauri also calls the app-level handler for
    // tray menu clicks, so a second one would run each action twice.
    app.handle()
        .on_menu_event(|app, event| on_menu(app, event.id().as_ref()));

    let entries = tray_menu::build(&AppState::default());
    let menu = build_menu(app.handle(), &entries)?;
    app.manage(TrayMenuCache(Mutex::new((0, entries))));
    // Template image: macOS tints it for the light / dark menu bar.
    let icon = Image::from_bytes(include_bytes!("../icons/tray.png"))?;
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .icon_as_template(true)
        .tooltip("OpenDAC Control")
        .menu(&menu)
        // Left click shows the window, right click opens the menu.
        .show_menu_on_left_click(false)
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
    Ok(menu)
}

fn on_menu(app: &AppHandle, id: &str) {
    match id {
        MENU_QUIT => app.exit(0),
        MENU_SHOW => show(app),
        _ => {
            let Some(action) = TrayAction::from_id(id) else {
                return;
            };
            let Some(backend) = app.try_state::<Arc<Backend>>() else {
                return;
            };
            let backend = Arc::clone(&backend);
            // A native check item flips itself when clicked. Forget the cached menu so the next
            // snapshot rebuilds it, even when the action failed and the model did not change.
            if let Some(cache) = app.try_state::<TrayMenuCache>() {
                cache.0.lock().unwrap_or_else(|p| p.into_inner()).1.clear();
            }
            // Device calls block: never on the main thread.
            tauri::async_runtime::spawn_blocking(move || backend.run_tray_action(action));
        }
    }
}

/// Native menu for the model.
fn build_menu(app: &AppHandle, entries: &[MenuEntry]) -> tauri::Result<Menu<tauri::Wry>> {
    let mut builder = MenuBuilder::new(app);
    for entry in entries {
        match entry {
            MenuEntry::Separator => builder = builder.separator(),
            MenuEntry::Item {
                id,
                label,
                enabled,
                checked: Some(c),
            } => {
                let item = CheckMenuItemBuilder::with_id(id, label)
                    .checked(*c)
                    .enabled(*enabled)
                    .build(app)?;
                builder = builder.item(&item);
            }
            MenuEntry::Item {
                id,
                label,
                enabled,
                checked: None,
            } => {
                let item = MenuItemBuilder::with_id(id, label)
                    .enabled(*enabled)
                    .build(app)?;
                builder = builder.item(&item);
            }
            MenuEntry::Submenu {
                label,
                enabled,
                items,
            } => {
                let mut sub = SubmenuBuilder::new(app, label).enabled(*enabled);
                for entry in items {
                    sub = match entry {
                        MenuEntry::Item {
                            id,
                            label,
                            enabled,
                            checked,
                        } => match checked {
                            Some(c) => sub.item(
                                &CheckMenuItemBuilder::with_id(id, label)
                                    .checked(*c)
                                    .enabled(*enabled)
                                    .build(app)?,
                            ),
                            None => sub.item(
                                &MenuItemBuilder::with_id(id, label)
                                    .enabled(*enabled)
                                    .build(app)?,
                            ),
                        },
                        // Submenus nest one level at most (see `tray_menu::build`).
                        MenuEntry::Separator | MenuEntry::Submenu { .. } => sub.separator(),
                    };
                }
                builder = builder.item(&sub.build()?);
            }
        }
    }
    builder.build()
}

/// Show `state` in the tray menu. Called with every snapshot; the menu is only
/// replaced when what it shows differs from the current one (snapshots come
/// every ~30 ms while a slider is dragged). Menus are built on the main thread.
pub fn sync_tray(app: &AppHandle, state: &AppState) {
    let entries = tray_menu::build(state);
    let Some(cache) = app.try_state::<TrayMenuCache>() else {
        return;
    };
    // Queued under the lock, so two racing snapshots keep their order; an older snapshot that
    // arrives late is ignored.
    let mut current = cache.0.lock().unwrap_or_else(|p| p.into_inner());
    if state.revision < current.0 || current.1 == entries {
        return;
    }
    *current = (state.revision, entries.clone());
    let handle = app.clone();
    let queued = app.run_on_main_thread(move || {
        let Some(tray) = handle.tray_by_id(TRAY_ID) else {
            return;
        };
        match build_menu(&handle, &entries) {
            Ok(menu) => {
                if let Err(e) = tray.set_menu(Some(menu)) {
                    log::warn!("could not update the tray menu: {e}");
                }
            }
            Err(e) => log::warn!("could not build the tray menu: {e}"),
        }
    });
    if let Err(e) = queued {
        log::warn!("could not update the tray menu: {e}");
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
