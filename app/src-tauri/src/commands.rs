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

use tauri::State;
use tauri_plugin_autostart::ManagerExt;

use crate::api::{
    ApiError, ApiResult, AppState, DacFilter, DolbyMode, EffectName, EqCurve, ErrorCode,
    SmartVolumeMode,
};
use crate::backend::Backend;

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

/// EQ pre-amp, `db` -6..=6 in 0.5 steps. Allowed while Scout is on.
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
pub async fn set_dolby_decoder(backend: Shared<'_>, mode: DolbyMode) -> ApiResult<AppState> {
    blocking(&backend, move |b| b.set_dolby_decoder(mode)).await
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
