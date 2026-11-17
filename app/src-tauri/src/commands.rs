//! Tauri commands: the UI -> Rust half of `app/API.md`.
//!
//! Each command is a thin wrapper around [`Backend`]. Device calls block on
//! the device thread, so every command that may touch the device or the disk
//! runs on Tauri's blocking pool (`spawn_blocking`), never on the main
//! thread.
//!
//! JS side: `invoke('set_eq_band', { band: 3, gain: 1.5 })`. Tauri turns the
//! snake_case Rust argument names into camelCase keys.

use std::sync::Arc;

use g6_core::store;
use tauri::State;
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_dialog::DialogExt;

use crate::api::{
    ApiError, ApiResult, AppState, DacFilter, DeviceInfo, DolbyMode, EffectName, EqCurve, EqImport,
    ErrorCode, MicEqCurve, MixerScope, Output, SmartVolumeMode,
};
use crate::backend::Backend;
use crate::hotkeys::HotkeySettings;

type Shared<'a> = State<'a, Arc<Backend>>;

/// Run `f` on the blocking pool.
async fn blocking<T, F>(backend: &Shared<'_>, f: F) -> ApiResult<T>
where
    T: Send + 'static,
    F: FnOnce(&Backend) -> ApiResult<T> + Send + 'static,
{
    let b = Arc::clone(backend);
    tauri::async_runtime::spawn_blocking(move || f(&b))
        .await
        .map_err(|e| ApiError::new(ErrorCode::Internal, format!("command task failed: {e}")))?
}

// --- State -----------------------------------------------------------------

/// Current snapshot. Never touches the device. Works while disconnected
/// (last known settings).
#[tauri::command]
pub async fn get_state(backend: Shared<'_>) -> ApiResult<AppState> {
    blocking(&backend, |b| Ok(b.snapshot())).await
}

/// Re-read the active output's settings from the device.
#[tauri::command]
pub async fn refresh_state(backend: Shared<'_>) -> ApiResult<AppState> {
    blocking(&backend, |b| b.refresh_state()).await
}

/// Firmware, serial, DSP version and hardware id. Cached per connection; a
/// field the device did not answer is `null`.
#[tauri::command]
pub async fn get_device_info(backend: Shared<'_>) -> ApiResult<DeviceInfo> {
    blocking(&backend, |b| Ok(b.device_info())).await
}

// --- EQ, SBX, Scout ----------------------------------------------------------

#[tauri::command]
pub async fn set_eq_enabled(backend: Shared<'_>, enabled: bool) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.set_eq_enabled(enabled)).await
}

/// `band` 0..=9 (31 Hz .. 16 kHz), `gain` -12..=12 dB in 0.5 steps.
#[tauri::command]
pub async fn set_eq_band(backend: Shared<'_>, band: u8, gain: f32) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.set_eq_band(band, gain)).await
}

/// All 10 bands at once. `gains.len()` must be 10.
#[tauri::command]
pub async fn set_eq_bands(backend: Shared<'_>, gains: Vec<f32>) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.set_eq_bands(&gains)).await
}

/// EQ pre-amp, `db` -6..=6 in 0.5 steps. Allowed while Scout is on (the G6
/// shows 0 dB then and restores the old value when Scout turns off).
#[tauri::command]
pub async fn set_eq_preamp(backend: Shared<'_>, db: f32) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.set_eq_preamp(db)).await
}

/// Write all 10 bands and the pre-amp of an EQ curve and turn EQ on.
#[tauri::command]
pub async fn apply_eq_curve(
    backend: Shared<'_>,
    bands: Vec<f32>,
    preamp: f32,
) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.apply_eq_curve(&bands, preamp)).await
}

// --- User EQ curves --------------------------------------------------------

#[tauri::command]
pub async fn list_eq_curves(backend: Shared<'_>) -> ApiResult<Vec<EqCurve>> {
    blocking(&backend, |b| b.list_eq_curves()).await
}

/// Save the active output's current bands and pre-amp under `name`.
#[tauri::command]
pub async fn save_eq_curve(backend: Shared<'_>, name: String) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.save_eq_curve(&name)).await
}

#[tauri::command]
pub async fn delete_eq_curve(backend: Shared<'_>, name: String) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.delete_eq_curve(&name)).await
}

// --- Mic EQ bands and curves -----------------------------------------------

/// `band` 0..=7 (mic EQ bands 1..=8), `gain` -12..=12 dB in 0.5 steps.
#[tauri::command]
pub async fn set_mic_eq_band(backend: Shared<'_>, band: u8, gain: f32) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.set_mic_eq_band(band, gain)).await
}

/// Write all 8 mic EQ bands of a curve or preset and turn mic EQ on.
#[tauri::command]
pub async fn apply_mic_eq_curve(backend: Shared<'_>, bands: Vec<f32>) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.apply_mic_eq_curve(&bands)).await
}

#[tauri::command]
pub async fn list_mic_eq_curves(backend: Shared<'_>) -> ApiResult<Vec<MicEqCurve>> {
    blocking(&backend, |b| b.list_mic_eq_curves()).await
}

/// Save the active output's current mic EQ bands under `name`.
#[tauri::command]
pub async fn save_mic_eq_curve(backend: Shared<'_>, name: String) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.save_mic_eq_curve(&name)).await
}

#[tauri::command]
pub async fn delete_mic_eq_curve(backend: Shared<'_>, name: String) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.delete_mic_eq_curve(&name)).await
}

#[tauri::command]
pub async fn set_sbx(backend: Shared<'_>, enabled: bool) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.set_sbx(enabled)).await
}

#[tauri::command]
pub async fn set_scout(backend: Shared<'_>, enabled: bool) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.set_scout(enabled)).await
}

// --- Effects ---------------------------------------------------------------

#[tauri::command]
pub async fn set_effect_enabled(
    backend: Shared<'_>,
    effect: EffectName,
    enabled: bool,
) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.set_effect_enabled(effect, enabled)).await
}

/// `level` 0..=1 (UI scale).
#[tauri::command]
pub async fn set_effect_level(
    backend: Shared<'_>,
    effect: EffectName,
    level: f32,
) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.set_effect_level(effect, level)).await
}

/// `distance` 10..=300 in 0.5 steps.
#[tauri::command]
pub async fn set_surround_distance(backend: Shared<'_>, distance: f32) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.set_surround_distance(distance)).await
}

#[tauri::command]
pub async fn set_smart_volume_mode(
    backend: Shared<'_>,
    mode: SmartVolumeMode,
) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.set_smart_volume_mode(mode)).await
}

// --- Mic -------------------------------------------------------------------

/// `db` is one of 0, 10, 20, 30.
#[tauri::command]
pub async fn set_mic_boost(backend: Shared<'_>, db: u8) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.set_mic_boost(db)).await
}

#[tauri::command]
pub async fn set_voice_clarity(backend: Shared<'_>, enabled: bool) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.set_voice_clarity(enabled)).await
}

/// `level` 0..=1 (UI scale; the device range is 0..=0.5).
#[tauri::command]
pub async fn set_noise_reduction(backend: Shared<'_>, level: f32) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.set_noise_reduction(level)).await
}

#[tauri::command]
pub async fn set_mic_eq_enabled(backend: Shared<'_>, enabled: bool) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.set_mic_eq_enabled(enabled)).await
}

#[tauri::command]
pub async fn set_mic_aec(backend: Shared<'_>, enabled: bool) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.set_mic_aec(enabled)).await
}

#[tauri::command]
pub async fn set_mic_smart_volume(backend: Shared<'_>, enabled: bool) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.set_mic_smart_volume(enabled)).await
}

// --- Device ----------------------------------------------------------------

#[tauri::command]
pub async fn set_dac_filter(backend: Shared<'_>, filter: DacFilter) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.set_dac_filter(filter)).await
}

#[tauri::command]
pub async fn set_lighting_enabled(backend: Shared<'_>, enabled: bool) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.set_lighting_enabled(enabled)).await
}

#[tauri::command]
pub async fn set_lighting_color(backend: Shared<'_>, color: String) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.set_lighting_color(&color)).await
}

#[tauri::command]
pub async fn set_output(backend: Shared<'_>, output: Output) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.set_output(output)).await
}

#[tauri::command]
pub async fn set_dolby_decoder(backend: Shared<'_>, mode: DolbyMode) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.set_dolby_decoder(mode)).await
}

// --- Mixer (USB audio controls, macOS and Windows) ---------------------------------

/// `volume` 0..=1, both channels. Rejects `mixerUnavailable` where the OS offers no such control.
#[tauri::command]
pub async fn set_mixer_volume(
    backend: Shared<'_>,
    scope: MixerScope,
    volume: f32,
) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.set_mixer_volume(scope, volume)).await
}

#[tauri::command]
pub async fn set_mixer_mute(
    backend: Shared<'_>,
    scope: MixerScope,
    mute: bool,
) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.set_mixer_mute(scope, mute)).await
}

/// Select the recording source; `id` is one of `mixer.recordingSource.options`.
#[tauri::command]
pub async fn set_mixer_source(backend: Shared<'_>, id: u32) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.set_mixer_source(id)).await
}

#[tauri::command]
pub async fn set_direct_mode(backend: Shared<'_>, enabled: bool) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.set_direct_mode(enabled)).await
}

// --- Presets ---------------------------------------------------------------

#[tauri::command]
pub async fn list_presets(backend: Shared<'_>) -> ApiResult<Vec<String>> {
    blocking(&backend, |b| b.list_presets()).await
}

/// Save the current settings under `name` (overwrites).
#[tauri::command]
pub async fn save_preset(backend: Shared<'_>, name: String) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.save_preset(&name)).await
}

#[tauri::command]
pub async fn apply_preset(backend: Shared<'_>, name: String) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.apply_preset(&name)).await
}

#[tauri::command]
pub async fn rename_preset(backend: Shared<'_>, from: String, to: String) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.rename_preset(&from, &to)).await
}

#[tauri::command]
pub async fn delete_preset(backend: Shared<'_>, name: String) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.delete_preset(&name)).await
}

/// Export profile `name` as a JSON file chosen in the native Save dialog.
/// Resolves with the path written, or `null` when the dialog was cancelled.
/// The path comes from the dialog, never from the UI. The profile is
/// validated before the dialog opens; no device access.
#[tauri::command]
pub async fn export_preset(
    app: tauri::AppHandle,
    backend: Shared<'_>,
    name: String,
) -> ApiResult<Option<String>> {
    blocking(&backend, move |b| {
        let json = b.export_preset_json(&name)?;
        // Blocks this pool thread until the user answers; the plugin runs the
        // dialog on the main thread.
        let picked = app
            .dialog()
            .file()
            .set_title("Export profile")
            .set_file_name(format!("{name}.json"))
            .add_filter("Profile (JSON)", &["json"])
            .blocking_save_file();
        let Some(picked) = picked else {
            return Ok(None);
        };
        let path = picked
            .into_path()
            .map_err(|e| ApiError::new(ErrorCode::Storage, e.to_string()))?;
        store::write_atomic(&path, json.as_bytes())?;
        Ok(Some(path.display().to_string()))
    })
    .await
}

/// Save the profile file `text` (read by the UI) under `name`. Validated
/// first; not applied.
#[tauri::command]
pub async fn import_preset(backend: Shared<'_>, text: String, name: String) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.import_preset(&text, &name)).await
}

/// Map an AutoEq / Equalizer APO text onto the 10 bands and pre-amp. Pure:
/// nothing is written (the UI applies the result with `apply_eq_curve`).
#[tauri::command]
pub async fn import_eq(backend: Shared<'_>, text: String) -> ApiResult<EqImport> {
    blocking(&backend, move |_| EqImport::parse(&text)).await
}

/// Profile to apply when the output becomes `output` (`null` clears the rule).
#[tauri::command]
pub async fn set_auto_switch(
    backend: Shared<'_>,
    output: Output,
    profile: Option<String>,
) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.set_auto_switch(output, profile)).await
}

// --- App -------------------------------------------------------------------

/// Turn launch at login on or off (macOS Launch Agent via the autostart
/// plugin). The snapshot's `launchAtLogin` is the plugin's `is_enabled()`
/// afterwards.
#[tauri::command]
pub async fn set_launch_at_login(
    app: tauri::AppHandle,
    backend: Shared<'_>,
    enabled: bool,
) -> ApiResult<AppState> {
    blocking(&backend, move |b| {
        let autostart = app.autolaunch();
        let r = if enabled {
            autostart.enable()
        } else {
            autostart.disable()
        };
        let now = autostart.is_enabled().unwrap_or(enabled && r.is_ok());
        let state = b.set_launch_at_login_flag(now);
        r.map_err(|e| ApiError::new(ErrorCode::Autostart, e.to_string()))?;
        Ok(state)
    })
    .await
}

/// Turn the global hotkeys on or off and set their accelerators (an empty
/// string means that action's default). Validated and stored first (rejects
/// `invalidValue`, `storage`), then registered with the OS; a shortcut that
/// cannot be registered is reported in the snapshot's `hotkeys`, not as an error.
#[tauri::command]
pub async fn set_hotkeys(
    app: tauri::AppHandle,
    backend: Shared<'_>,
    enabled: bool,
    toggle_direct_mode: String,
    next_profile: String,
    toggle_sbx: String,
) -> ApiResult<AppState> {
    blocking(&backend, move |b| {
        b.set_hotkeys(HotkeySettings {
            enabled,
            toggle_direct_mode,
            next_profile,
            toggle_sbx,
        })?;
        Ok(crate::shortcuts::apply(&app, b))
    })
    .await
}

/// Light or dark window chrome (title bar, traffic lights, window background)
/// to match the UI theme. App-wide on macOS: `AppHandle::set_theme` also
/// refreshes every window, unlike the per-window setter.
#[tauri::command]
pub async fn set_app_theme(app: tauri::AppHandle, theme: String) -> ApiResult<()> {
    use tauri::{window::Color, Manager, Theme};
    let light = theme == "light";
    app.set_theme(Some(if light { Theme::Light } else { Theme::Dark }));
    if let Some(w) = app.get_webview_window(crate::panel::WINDOW_LABEL) {
        let c = if light { 255 } else { 0 };
        if let Err(e) = w.set_background_color(Some(Color(c, c, c, 255))) {
            log::warn!("cannot set the window background: {e}");
        }
    }
    Ok(())
}

/// Hide the panel (Escape key).
#[tauri::command]
pub async fn hide_panel(app: tauri::AppHandle) -> ApiResult<()> {
    crate::panel::hide(&app);
    Ok(())
}
