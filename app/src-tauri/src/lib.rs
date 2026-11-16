//! OpenDAC Control menu bar app (Tauri 2). All device access goes through
//! `g6-core`; this crate never opens HID devices itself.
//!
//! The UI <-> Rust contract is `app/API.md`; the types are in [`api`].

pub mod api;
pub mod backend;
pub mod commands;
pub mod eq_curves;
pub mod panel;

use std::sync::Arc;

use g6_core::events::Config;
use g6_core::session::{Session, SessionConfig};
use g6_core::store::Paths;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};

use crate::api::{ApiError, AppState, ConnectionChanged, OutputChanged};
use crate::backend::{Backend, Sink};

/// Sends the backend's events to the UI (names from `app/API.md` section 3).
struct TauriSink(AppHandle);

impl TauriSink {
    fn emit<S: serde::Serialize + Clone>(&self, event: &str, payload: S) {
        if let Err(e) = self.0.emit(event, payload) {
            log::warn!("could not emit {event}: {e}");
        }
    }
}

impl Sink for TauriSink {
    fn state_changed(&self, state: &AppState) {
        self.emit("state-changed", state);
    }
    fn connection_changed(&self, payload: &ConnectionChanged) {
        self.emit("connection-changed", payload);
    }
    fn output_changed(&self, payload: &OutputChanged) {
        self.emit("output-changed", payload);
    }
    fn device_error(&self, error: &ApiError) {
        self.emit("device-error", error);
    }
}

/// Start the one long-lived device session (restore the last
/// settings on connect) and its event forwarder; manage the backend.
fn start_backend(app: &tauri::App) {
    let paths = Paths::system();
    if paths.is_none() {
        log::warn!("no home directory: presets, Direct Mode and last settings are not stored");
    }
    let (session, events) = Session::open_hid(
        Config::default(),
        SessionConfig {
            paths: paths.clone(),
            reapply_on_first_connect: true,
        },
    );
    let launch_at_login = app.autolaunch().is_enabled().unwrap_or_else(|e| {
        log::warn!("cannot read the launch at login state: {e}");
        false
    });
    let backend = Backend::new(
        session,
        paths,
        launch_at_login,
        Arc::new(TauriSink(app.handle().clone())),
    );
    let weak = Arc::downgrade(&backend);
    std::thread::Builder::new()
        .name("g6-events".into())
        .spawn(move || backend::run_forwarder(weak, events))
        .expect("spawn the event forwarder thread");
    app.manage(backend);
}

pub fn run() {
    let app = tauri::Builder::default()
        // Must be the first plugin. A second launch just shows the window.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            panel::show(app);
        }))
        // Launch at login is off until the user turns it on (set_launch_at_login).
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .on_window_event(panel::on_window_event)
        .setup(|app| {
            panel::init(app)?;
            start_backend(app);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_state,
            commands::refresh_state,
            commands::set_eq_enabled,
            commands::set_eq_band,
            commands::set_eq_bands,
            commands::set_eq_preamp,
            commands::apply_eq_curve,
            commands::list_eq_curves,
            commands::save_eq_curve,
            commands::delete_eq_curve,
            commands::set_sbx,
            commands::set_scout,
            commands::set_effect_enabled,
            commands::set_effect_level,
            commands::set_surround_distance,
            commands::set_smart_volume_mode,
            commands::set_mic_boost,
            commands::set_voice_clarity,
            commands::set_noise_reduction,
            commands::set_mic_eq_enabled,
            commands::set_mic_aec,
            commands::set_mic_smart_volume,
            commands::set_dac_filter,
            commands::set_dolby_decoder,
            commands::set_direct_mode,
            commands::list_presets,
            commands::save_preset,
            commands::apply_preset,
            commands::rename_preset,
            commands::delete_preset,
            commands::set_launch_at_login,
            commands::hide_panel,
            commands::set_app_theme,
        ])
        .build(tauri::generate_context!())
        .expect("error while building the OpenDAC Control app");

    app.run(|app, event| match event {
        // Keeps running with the window closed; only Quit exits.
        tauri::RunEvent::ExitRequested { api, code, .. } if code.is_none() => {
            api.prevent_exit();
        }
        // Dock icon clicked while the window is hidden.
        #[cfg(target_os = "macos")]
        tauri::RunEvent::Reopen { .. } => panel::show(app),
        _ => {}
    });
}
