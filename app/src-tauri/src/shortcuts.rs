//! Registers the global hotkeys with the OS (`tauri-plugin-global-shortcut`).
//! The settings, accelerator validation and the actions are in `hotkeys.rs`
//! and `backend.rs`; this is only the OS side. Registration is done from Rust,
//! never from the webview, so the plugin needs no capability entry.
//!
//! The plugin does each (un)registration on the main thread and waits for it,
//! so [`apply`] must not run on the main thread.

use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

use crate::backend::Backend;
use crate::hotkeys::HotkeyAction;

/// One (un)registration pass at a time: the startup thread and `set_hotkeys`
/// would otherwise interleave their `unregister` / `on_shortcut` calls and
/// report shortcuts as "already registered". Holds the accelerators the last
/// pass registered: they are unregistered one by one, never with the
/// plugin's `unregister_all`, which keeps the plugin's lock while it waits
/// for the main thread, where a hotkey press needs that lock (deadlock).
static APPLY: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Register the stored hotkeys at startup, if the user turned them on. Does
/// nothing (and touches no OS API) while they are off. Runs on its own thread.
pub fn apply_at_startup(app: &AppHandle) {
    let Some(backend) = app.try_state::<Arc<Backend>>() else {
        return;
    };
    if !backend.hotkey_settings().enabled {
        return;
    }
    let app = app.clone();
    let spawned = std::thread::Builder::new()
        .name("g6-hotkeys".into())
        .spawn(move || {
            if let Some(b) = app.try_state::<Arc<Backend>>() {
                apply(&app, &b);
            }
        });
    if let Err(e) = spawned {
        log::warn!("could not start the hotkey registration: {e}");
    }
}

/// Replace the registered shortcuts by the backend's current settings, and
/// tell the backend which ones failed (shown in the UI). Never fatal. Not for
/// the main thread.
pub fn apply(app: &AppHandle, backend: &Backend) -> crate::api::AppState {
    let mut registered = APPLY.lock().unwrap_or_else(|p| p.into_inner());
    // Read under the lock, so the last pass registers the newest settings.
    let settings = backend.hotkey_settings();
    let shortcuts = app.global_shortcut();
    // One that fails stays listed, so the next pass tries again.
    registered.retain(
        |accelerator| match shortcuts.unregister(accelerator.as_str()) {
            Ok(()) => false,
            Err(e) => {
                log::warn!("could not unregister {accelerator}: {e}");
                true
            }
        },
    );
    let mut errors: [Option<String>; 3] = Default::default();
    if settings.enabled {
        for action in HotkeyAction::ALL {
            let accelerator = settings.accelerator(action);
            let result = shortcuts.on_shortcut(accelerator, move |app, _shortcut, event| {
                // Pressed only: Released would run the action a second time.
                if event.state != ShortcutState::Pressed {
                    return;
                }
                if let Some(b) = app.try_state::<Arc<Backend>>() {
                    let b = Arc::clone(&b);
                    // Device calls block: never on the main thread.
                    tauri::async_runtime::spawn_blocking(move || b.run_hotkey(action));
                }
            });
            if result.is_ok() && !registered.iter().any(|a| a == accelerator) {
                registered.push(accelerator.to_string());
            }
            if let Err(e) = result {
                log::warn!(
                    "could not register {accelerator} for {}: {e}",
                    action.label()
                );
                errors[action.index()] = Some(format!(
                    "{accelerator} could not be registered (another app may be using it): {e}"
                ));
            }
        }
    }
    backend.set_hotkey_status(errors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotkeys::{normalize, HotkeySettings};
    use std::str::FromStr;
    use tauri_plugin_global_shortcut::Shortcut;

    /// What `hotkeys::normalize` accepts must be what the plugin parses, or
    /// a valid setting would fail to register.
    #[test]
    fn canonical_accelerators_parse_with_the_plugin() {
        let d = HotkeySettings::default();
        for a in HotkeyAction::ALL {
            Shortcut::from_str(d.accelerator(a)).unwrap();
        }
        let mut keys: Vec<String> = ('A'..='Z').map(String::from).collect();
        keys.extend((0..=9).map(|n| n.to_string()));
        keys.extend((1..=24).map(|n| format!("F{n}")));
        keys.extend(
            [
                "Space",
                "Tab",
                "Enter",
                "Backspace",
                "Delete",
                "Insert",
                "Home",
                "End",
                "PageUp",
                "PageDown",
                "ArrowUp",
                "ArrowDown",
                "ArrowLeft",
                "ArrowRight",
                "Minus",
                "Equal",
                "Comma",
                "Period",
                "Slash",
                "Semicolon",
                "Quote",
                "Backquote",
                "Backslash",
                "BracketLeft",
                "BracketRight",
            ]
            .map(String::from),
        );
        for k in keys {
            let text = normalize(&format!("shift+cmd+alt+ctrl+{k}")).unwrap();
            Shortcut::from_str(&text).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(normalize(&text).unwrap(), text);
        }
    }

    #[test]
    fn equal_canonical_accelerators_are_the_same_shortcut() {
        let a = Shortcut::from_str(&normalize("alt+ctrl+x").unwrap()).unwrap();
        let b = Shortcut::from_str(&normalize("Ctrl+Option+X").unwrap()).unwrap();
        assert_eq!(a.id(), b.id());
    }
}
