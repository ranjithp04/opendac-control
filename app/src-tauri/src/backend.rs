//! The device side of the app: one long-lived `g6-core` [`Session`] plus the
//! app's own state (Direct Mode flag, presets, active preset, launch at
//! login), turned into [`AppState`] snapshots.
//!
//! This module knows nothing about Tauri: events leave through a [`Sink`]
//! (Tauri's is in `lib.rs`), so everything here is tested against
//! `g6-core`'s fake device. All methods block (device calls wait for the
//! device thread); the Tauri commands run them with `spawn_blocking`.
//!
//! Requirements from `app/API.md` section 5 implemented here:
//! - every session event burst produces one `state-changed` snapshot
//!   ([`run_forwarder`]);
//! - a write returns a snapshot that already holds the confirmed value
//!   ([`Pending`] overlays it until the device's echo reaches the session);
//! - Direct Mode follows the headphone jack;
//! - `set_sbx(true)` with Scout on switches Scout off first.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use g6_core::direct::{self, DirectMode};
use g6_core::events::{Change, Event};
use g6_core::features::{self, Feature};
use g6_core::preset::{self, PresetStore};
use g6_core::protocol::{MaskTarget, MicBoost};
use g6_core::session::Session;
use g6_core::state::DeviceState;
use g6_core::store::{self, Paths};
use serde::{Deserialize, Serialize};

use crate::api::{
    ApiError, ApiResult, AppState, Connection, ConnectionChanged, DacFilter, DolbyMode, EffectName,
    ErrorCode, Extras, Output, OutputChanged, SmartVolumeMode,
};
use crate::eq_curves::{self, EqCurve, EqCurveStore};

/// Where the backend's events go (the Tauri app emits them to the UI).
pub trait Sink: Send + Sync + 'static {
    fn state_changed(&self, state: &AppState);
    fn connection_changed(&self, payload: &ConnectionChanged);
    fn output_changed(&self, payload: &OutputChanged);
    fn device_error(&self, error: &ApiError);
}

/// Events closer together than this are one burst (one `state-changed`).
const COALESCE: Duration = Duration::from_millis(30);
/// A burst is cut after this long even if events keep coming.
const MAX_BURST: Duration = Duration::from_millis(200);
/// A written value is overlaid at most this long when no echo arrives.
const PENDING_TTL: Duration = Duration::from_secs(2);

/// App settings file in the config dir.
const SETTINGS_FILE: &str = "app_settings.json";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Settings {
    #[serde(default)]
    active_preset: Option<String>,
}

fn feat(name: &str) -> &'static Feature {
    features::by_name(name)
        .unwrap_or_else(|| panic!("feature '{name}' is not in the g6-core table"))
}

fn on_off(on: bool) -> f32 {
    if on {
        1.0
    } else {
        0.0
    }
}

// ---------------------------------------------------------------------------
// Pending writes
// ---------------------------------------------------------------------------

/// Confirmed writes whose echo has not reached the session state yet.
///
/// `Session::state()` is updated from the device's echo event, which can
/// arrive after the write call returned. Until then every snapshot gets the
/// written value overlaid. An entry ends when the session reports any value
/// for the same setting (the echo, or a later change), when the device
/// reconnects or switches output, or after [`PENDING_TTL`].
#[derive(Debug, Default)]
struct Pending(Vec<(Change, Instant)>);

fn same_setting(a: &Change, b: &Change) -> bool {
    match (a, b) {
        (Change::Feature { feature: x, .. }, Change::Feature { feature: y, .. }) => {
            x.family == y.family && x.id == y.id
        }
        (Change::Mask(_), Change::Mask(_))
        | (Change::DacFilter(_), Change::DacFilter(_))
        | (Change::MicBoost(_), Change::MicBoost(_)) => true,
        _ => false,
    }
}

impl Pending {
    fn push(&mut self, change: Change, now: Instant) {
        self.0.retain(|(c, _)| !same_setting(c, &change));
        self.0.push((change, now));
    }

    /// The session reported `ev`.
    fn observe(&mut self, ev: &Event) {
        match ev {
            Event::Changed(c) => self.0.retain(|(p, _)| !same_setting(p, c)),
            Event::Connected | Event::Disconnected | Event::OutputSwitched(_) => self.0.clear(),
        }
    }

    fn clear(&mut self) {
        self.0.clear();
    }

    /// Overlay the live entries onto `state` (dropping expired ones).
    fn apply(&mut self, state: &mut DeviceState, now: Instant) {
        self.0
            .retain(|(_, at)| now.saturating_duration_since(*at) < PENDING_TTL);
        for (c, _) in &self.0 {
            state.apply(&Event::Changed(*c));
        }
    }
}

// ---------------------------------------------------------------------------
// Backend
// ---------------------------------------------------------------------------

struct Inner {
    revision: u64,
    firmware: Option<String>,
    direct_mode: bool,
    presets: Vec<String>,
    active_preset: Option<String>,
    eq_curves: Vec<EqCurve>,
    launch_at_login: bool,
    pending: Pending,
}

/// The app's device backend. Shared as `Arc<Backend>` (Tauri managed state).
pub struct Backend {
    session: Session,
    paths: Option<Paths>,
    sink: Arc<dyn Sink>,
    inner: Mutex<Inner>,
    /// Serialises device operations (multi-step ones must not interleave:
    /// SBX after Scout off, Direct Mode, presets, Direct Mode following the
    /// jack).
    ops: Mutex<()>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn no_paths() -> ApiError {
    ApiError::new(
        ErrorCode::Storage,
        "cannot find the config directory (no home directory)",
    )
}

impl Backend {
    /// Wrap a started session. `paths` must be the session's paths (presets,
    /// Direct Mode file, app settings); `None` disables everything stored.
    /// Call [`run_forwarder`] with the session's receiver afterwards.
    pub fn new(
        session: Session,
        paths: Option<Paths>,
        launch_at_login: bool,
        sink: Arc<dyn Sink>,
    ) -> Arc<Backend> {
        let direct_mode = paths.as_ref().is_some_and(|p| {
            DirectMode::new(p.direct_file())
                .is_active()
                .unwrap_or_else(|e| {
                    log::warn!("cannot read the Direct Mode file: {e}");
                    false
                })
        });
        let presets = paths
            .as_ref()
            .map(|p| {
                PresetStore::from_paths(p).list().unwrap_or_else(|e| {
                    log::warn!("cannot list presets: {e}");
                    Vec::new()
                })
            })
            .unwrap_or_default();
        let settings = paths
            .as_ref()
            .and_then(|p| match store::read_json::<Settings>(&settings_file(p)) {
                Ok(s) => s,
                Err(e) => {
                    log::warn!("ignoring app settings: {e}");
                    None
                }
            })
            .unwrap_or_default();
        let eq_curves = paths
            .as_ref()
            .map(|p| {
                EqCurveStore::in_dir(&p.config_dir)
                    .list()
                    .unwrap_or_else(|e| {
                        log::warn!("ignoring saved EQ curves: {e}");
                        Vec::new()
                    })
            })
            .unwrap_or_default();
        Arc::new(Backend {
            session,
            paths,
            sink,
            inner: Mutex::new(Inner {
                revision: 0,
                firmware: None,
                direct_mode,
                presets,
                active_preset: settings.active_preset,
                eq_curves,
                launch_at_login,
                pending: Pending::default(),
            }),
            ops: Mutex::new(()),
        })
    }

    // --- Snapshots -----------------------------------------------------------

    /// Session state with the pending writes overlaid.
    fn device_state(&self, inner: &mut Inner) -> DeviceState {
        let mut s = self.session.state();
        inner.pending.apply(&mut s, Instant::now());
        s
    }

    /// A new snapshot (next revision). Built under the lock, so a higher
    /// revision never holds an older session state.
    pub fn snapshot(&self) -> AppState {
        let mut inner = lock(&self.inner);
        let dev = self.device_state(&mut inner);
        inner.revision += 1;
        AppState::from_device(
            &dev,
            Extras {
                revision: inner.revision,
                firmware: inner.firmware.clone(),
                direct_mode: inner.direct_mode,
                presets: inner.presets.clone(),
                active_preset: inner.active_preset.clone(),
                eq_curves: inner.eq_curves.clone(),
                launch_at_login: inner.launch_at_login,
            },
        )
    }

    /// Snapshot, also sent as `state-changed` (for changes no device event
    /// announces: presets, Direct Mode flag, launch at login).
    fn announce(&self) -> AppState {
        let s = self.snapshot();
        self.sink.state_changed(&s);
        s
    }

    // --- Guards ----------------------------------------------------------------

    /// Live (overlaid) device state; `notConnected` unless synced.
    fn connected_state(&self) -> ApiResult<DeviceState> {
        let s = self.device_state(&mut lock(&self.inner));
        if s.connected {
            Ok(s)
        } else {
            Err(ApiError::new(ErrorCode::NotConnected, "G6 not connected"))
        }
    }

    fn check_direct_mode_off(&self) -> ApiResult<()> {
        if lock(&self.inner).direct_mode {
            Err(ApiError::direct_mode_active())
        } else {
            Ok(())
        }
    }

    fn check_scout_off(state: &DeviceState) -> ApiResult<()> {
        if state.mask.is_some_and(|m| m.scout()) {
            Err(ApiError::scout_active())
        } else {
            Ok(())
        }
    }

    fn pend(&self, change: Change) {
        lock(&self.inner).pending.push(change, Instant::now());
    }

    /// Re-read the device after a multi-step change; the change itself
    /// succeeded, so a failure here is only logged.
    fn resync(&self) {
        match self.session.refresh() {
            Ok(_) => lock(&self.inner).pending.clear(),
            Err(e) => log::warn!("could not re-read the device: {e}"),
        }
    }

    // --- Writes ----------------------------------------------------------------

    /// Write a feature (device value) and overlay the confirmed value.
    fn write(&self, f: &'static Feature, value: f32) -> ApiResult<()> {
        let w = self.session.device().write_feature(f, value)?;
        self.pend(Change::Feature {
            feature: f,
            value: w.value,
        });
        Ok(())
    }

    fn set_mask(&self, target: MaskTarget, on: bool) -> ApiResult<()> {
        let w = self.session.device().set_mask(target, on)?;
        self.pend(Change::Mask(w.value));
        Ok(())
    }

    /// Playback write: rejected while Direct Mode is on, and for features
    /// Scout owns while Scout is on.
    fn playback_write(&self, f: &'static Feature, value: f32) -> ApiResult<AppState> {
        let _op = lock(&self.ops);
        self.check_direct_mode_off()?;
        let s = self.connected_state()?;
        if features::scout_owned(f) {
            Self::check_scout_off(&s)?;
        }
        self.write(f, value)?;
        Ok(self.snapshot())
    }

    /// Write that is allowed during Direct Mode and Scout (mic, decoder).
    fn plain_write(&self, f: &'static Feature, value: f32) -> ApiResult<AppState> {
        let _op = lock(&self.ops);
        self.connected_state()?;
        self.write(f, value)?;
        Ok(self.snapshot())
    }

    pub fn refresh_state(&self) -> ApiResult<AppState> {
        let _op = lock(&self.ops);
        self.connected_state()?;
        self.session.refresh()?;
        lock(&self.inner).pending.clear();
        Ok(self.snapshot())
    }

    pub fn set_eq_enabled(&self, enabled: bool) -> ApiResult<AppState> {
        self.playback_write(feat("eq"), on_off(enabled))
    }

    pub fn set_eq_band(&self, band: u8, gain: f32) -> ApiResult<AppState> {
        let f = features::eq_band(band.into()).ok_or_else(|| {
            ApiError::new(
                ErrorCode::InvalidValue,
                format!("EQ band {band} does not exist (0..9)"),
            )
        })?;
        self.playback_write(f, gain)
    }

    /// All 10 bands. Every value is validated before the first write.
    pub fn set_eq_bands(&self, gains: &[f32]) -> ApiResult<AppState> {
        if gains.len() != features::EQ_BANDS.len() {
            return Err(ApiError::new(
                ErrorCode::InvalidValue,
                format!("expected 10 EQ gains, got {}", gains.len()),
            ));
        }
        let bands: Vec<&'static Feature> = (0..gains.len())
            .map(|i| features::eq_band(i).expect("10 bands"))
            .collect();
        for (f, g) in bands.iter().zip(gains) {
            f.validate_write(*g)
                .map_err(|e| ApiError::from(g6_core::events::Error::from(e)))?;
        }
        let _op = lock(&self.ops);
        self.check_direct_mode_off()?;
        Self::check_scout_off(&self.connected_state()?)?;
        for (f, g) in bands.iter().zip(gains) {
            self.write(f, *g)?;
        }
        Ok(self.snapshot())
    }

    /// EQ pre-amp (dB). Not owned by Scout, so allowed while Scout is on.
    pub fn set_eq_preamp(&self, db: f32) -> ApiResult<AppState> {
        self.playback_write(feat("eq_preamp"), db)
    }

    /// Write a whole EQ curve (10 bands, then the pre-amp) and turn EQ on.
    /// Everything is validated before the first write; if a write fails the
    /// device is re-read, since some of the writes may have happened.
    pub fn apply_eq_curve(&self, bands: &[f32], preamp: f32) -> ApiResult<AppState> {
        let writes = Self::curve_writes(bands, preamp)?;
        let _op = lock(&self.ops);
        self.check_direct_mode_off()?;
        Self::check_scout_off(&self.connected_state()?)?;
        let r = writes
            .iter()
            .chain(&[(feat("eq"), 1.0)])
            .try_for_each(|(f, v)| self.write(f, *v));
        if let Err(e) = r {
            self.resync();
            return Err(e);
        }
        Ok(self.snapshot())
    }

    /// The feature writes of a curve, validated: 10 bands, then the pre-amp.
    fn curve_writes(bands: &[f32], preamp: f32) -> ApiResult<Vec<(&'static Feature, f32)>> {
        if bands.len() != features::EQ_BANDS.len() {
            return Err(ApiError::new(
                ErrorCode::InvalidValue,
                format!("expected 10 EQ gains, got {}", bands.len()),
            ));
        }
        let mut writes: Vec<(&'static Feature, f32)> = bands
            .iter()
            .enumerate()
            .map(|(i, g)| (features::eq_band(i).expect("10 bands"), *g))
            .collect();
        writes.push((feat("eq_preamp"), preamp));
        for (f, v) in &writes {
            f.validate_write(*v)
                .map_err(|e| ApiError::from(g6_core::events::Error::from(e)))?;
        }
        Ok(writes)
    }

    pub fn set_sbx(&self, enabled: bool) -> ApiResult<AppState> {
        let _op = lock(&self.ops);
        self.check_direct_mode_off()?;
        let s = self.connected_state()?;
        if enabled && s.mask.is_some_and(|m| m.scout()) {
            // As in the mockup: SBX on ends Scout Mode.
            self.set_mask(MaskTarget::Scout, false)?;
        }
        self.set_mask(MaskTarget::Sbx, enabled)?;
        Ok(self.snapshot())
    }

    pub fn set_scout(&self, enabled: bool) -> ApiResult<AppState> {
        let _op = lock(&self.ops);
        self.check_direct_mode_off()?;
        self.connected_state()?;
        self.set_mask(MaskTarget::Scout, enabled)?;
        Ok(self.snapshot())
    }

    pub fn set_effect_enabled(&self, effect: EffectName, enabled: bool) -> ApiResult<AppState> {
        self.playback_write(feat(effect.features().0), on_off(enabled))
    }

    pub fn set_effect_level(&self, effect: EffectName, level: f32) -> ApiResult<AppState> {
        let f = feat(effect.features().1);
        self.playback_write(f, f.from_ui(level))
    }

    pub fn set_surround_distance(&self, distance: f32) -> ApiResult<AppState> {
        self.playback_write(feat("surround_distance"), distance)
    }

    pub fn set_smart_volume_mode(&self, mode: SmartVolumeMode) -> ApiResult<AppState> {
        let f = feat("smartvol_mode");
        let v = f
            .choice_value(mode.core_name())
            .expect("smartvol_mode choices match SmartVolumeMode");
        self.playback_write(f, v)
    }

    pub fn set_mic_boost(&self, db: u8) -> ApiResult<AppState> {
        let boost = MicBoost::ALL
            .into_iter()
            .find(|b| b.db() == db)
            .ok_or_else(|| {
                ApiError::new(
                    ErrorCode::InvalidValue,
                    format!("mic boost must be 0, 10, 20 or 30 dB, not {db}"),
                )
            })?;
        let _op = lock(&self.ops);
        self.connected_state()?;
        let w = self.session.device().set_mic_boost(boost)?;
        self.pend(Change::MicBoost(w.value));
        Ok(self.snapshot())
    }

    pub fn set_voice_clarity(&self, enabled: bool) -> ApiResult<AppState> {
        self.plain_write(feat("mic_clarity"), on_off(enabled))
    }

    pub fn set_noise_reduction(&self, level: f32) -> ApiResult<AppState> {
        let f = feat("mic_nr_level");
        self.plain_write(f, f.from_ui(level))
    }

    pub fn set_mic_eq_enabled(&self, enabled: bool) -> ApiResult<AppState> {
        self.plain_write(feat("mic_eq"), on_off(enabled))
    }

    pub fn set_mic_aec(&self, enabled: bool) -> ApiResult<AppState> {
        self.plain_write(feat("mic_aec"), on_off(enabled))
    }

    pub fn set_mic_smart_volume(&self, enabled: bool) -> ApiResult<AppState> {
        self.plain_write(feat("mic_smartvol"), on_off(enabled))
    }

    pub fn set_dac_filter(&self, filter: DacFilter) -> ApiResult<AppState> {
        let _op = lock(&self.ops);
        self.connected_state()?;
        let w = self.session.device().set_dac_filter(filter.into())?;
        self.pend(Change::DacFilter(w.value));
        Ok(self.snapshot())
    }

    pub fn set_dolby_decoder(&self, mode: DolbyMode) -> ApiResult<AppState> {
        let f = feat("decoder_mode");
        let v = f
            .choice_value(mode.core_name())
            .expect("decoder_mode choices match DolbyMode");
        self.plain_write(f, v)
    }

    // --- Direct Mode -------------------------------------------------------------

    fn direct(&self) -> ApiResult<DirectMode> {
        let p = self.paths.as_ref().ok_or_else(no_paths)?;
        Ok(DirectMode::new(p.direct_file()))
    }

    /// Update the cached Direct Mode flag from the file.
    fn reload_direct_flag(&self, dm: &DirectMode) {
        match dm.is_active() {
            Ok(active) => lock(&self.inner).direct_mode = active,
            Err(e) => log::warn!("cannot read the Direct Mode file: {e}"),
        }
    }

    pub fn set_direct_mode(&self, enabled: bool) -> ApiResult<AppState> {
        let dm = self.direct()?;
        let _op = lock(&self.ops);
        if !enabled && dm.saved()?.is_none() {
            // Not on and nothing waiting to be restored: nothing to do.
            self.reload_direct_flag(&dm);
            return Ok(self.snapshot());
        }
        self.connected_state()?;
        let r = if enabled {
            dm.enable(self.session.device())
        } else {
            match dm.disable(self.session.device()) {
                Err(direct::Error::NotActive) => Ok(()),
                other => other,
            }
        };
        // Whatever happened, the file says whether Direct Mode is on now.
        self.reload_direct_flag(&dm);
        self.resync();
        let s = self.announce();
        r?;
        Ok(s)
    }

    /// The device (re)connected or switched output. While Direct Mode is on,
    /// extend it to the active output (idempotent). Once it is off, finish
    /// the restore still pending for the active output (g6-core keeps the
    /// other outputs' entries on disk until `disable` runs while that output
    /// is active).
    fn follow_direct_mode(&self) {
        let Ok(dm) = self.direct() else { return };
        if lock(&self.inner).direct_mode {
            let _op = lock(&self.ops);
            if let Err(e) = dm.enable(self.session.device()) {
                log::warn!("Direct Mode could not be extended to the active output: {e}");
                let mut err = ApiError::from(e);
                err.message = format!(
                    "Direct Mode could not be applied to the active output: {}",
                    err.message
                );
                self.sink.device_error(&err);
            }
            self.reload_direct_flag(&dm);
            return;
        }
        match dm.pending_outputs() {
            Ok(p) if p.is_empty() => return,
            Ok(_) => {}
            Err(e) => {
                log::warn!("cannot read the Direct Mode file: {e}");
                return;
            }
        }
        let _op = lock(&self.ops);
        let output = match self.session.device().read_output() {
            Ok(o) => o,
            Err(e) => {
                log::warn!("cannot read the active output: {e}");
                return;
            }
        };
        // Re-check under the lock: a command may have changed the file.
        if !dm.pending_outputs().is_ok_and(|p| p.contains(&output)) {
            return;
        }
        match dm.disable(self.session.device()) {
            Ok(()) | Err(direct::Error::NotActive) => {}
            Err(e) => {
                log::warn!("Direct Mode settings could not be restored on this output: {e}");
                let mut err = ApiError::from(e);
                err.message = format!(
                    "Direct Mode settings could not be restored on the active output: {}",
                    err.message
                );
                self.sink.device_error(&err);
            }
        }
        self.reload_direct_flag(&dm);
        self.resync();
    }

    // --- Presets -------------------------------------------------------------------

    fn presets_store(&self) -> ApiResult<PresetStore> {
        Ok(PresetStore::from_paths(
            self.paths.as_ref().ok_or_else(no_paths)?,
        ))
    }

    fn save_settings(&self, inner: &Inner) -> ApiResult<()> {
        let p = self.paths.as_ref().ok_or_else(no_paths)?;
        store::write_json(
            &settings_file(p),
            &Settings {
                active_preset: inner.active_preset.clone(),
            },
        )?;
        Ok(())
    }

    /// Re-list presets into the cache; `active` replaces the active preset.
    fn update_presets(&self, store: &PresetStore, active: Option<Option<String>>) -> ApiResult<()> {
        let list = store.list()?;
        let mut inner = lock(&self.inner);
        inner.presets = list;
        if let Some(a) = active {
            if inner.active_preset != a {
                inner.active_preset = a;
                self.save_settings(&inner)?;
            }
        }
        Ok(())
    }

    pub fn list_presets(&self) -> ApiResult<Vec<String>> {
        let store = self.presets_store()?;
        let list = store.list()?;
        let changed = {
            let mut inner = lock(&self.inner);
            let changed = inner.presets != list;
            inner.presets = list.clone();
            changed
        };
        if changed {
            self.announce();
        }
        Ok(list)
    }

    pub fn save_preset(&self, name: &str) -> ApiResult<AppState> {
        let store = self.presets_store()?;
        let _op = lock(&self.ops);
        self.check_direct_mode_off()?;
        let state = self.connected_state()?;
        store.save_current(name, &state)?;
        self.update_presets(&store, Some(Some(name.to_string())))?;
        Ok(self.announce())
    }

    pub fn apply_preset(&self, name: &str) -> ApiResult<AppState> {
        let store = self.presets_store()?;
        let p = store.load(name)?;
        let _op = lock(&self.ops);
        self.check_direct_mode_off()?;
        self.connected_state()?;
        let r = preset::apply(self.session.device(), &p);
        // Some writes may have happened even on error.
        self.resync();
        r?;
        self.update_presets(&store, Some(Some(name.to_string())))?;
        Ok(self.announce())
    }

    /// Rename a profile; the active profile follows the rename. Works while
    /// disconnected (file operation only).
    pub fn rename_preset(&self, from: &str, to: &str) -> ApiResult<AppState> {
        let store = self.presets_store()?;
        let p = store.load(from)?;
        if to == from {
            return Ok(self.announce());
        }
        // Only the case changes: on a case-insensitive file system the old
        // file *is* the new one, so it is removed first.
        let case_only = to.eq_ignore_ascii_case(from);
        if !case_only && store.list()?.iter().any(|n| n.eq_ignore_ascii_case(to)) {
            return Err(ApiError::new(
                ErrorCode::PresetInvalidName,
                format!("A profile named \"{to}\" already exists."),
            ));
        }
        if case_only {
            store.delete(from)?;
            store.save(to, &p)?;
        } else {
            store.save(to, &p)?;
            store.delete(from)?;
        }
        let active = lock(&self.inner).active_preset.clone();
        let follow = (active.as_deref() == Some(from)).then(|| Some(to.to_string()));
        self.update_presets(&store, follow)?;
        Ok(self.announce())
    }

    pub fn delete_preset(&self, name: &str) -> ApiResult<AppState> {
        let store = self.presets_store()?;
        let r = store.delete(name);
        let active = lock(&self.inner).active_preset.clone();
        let clear = r.is_ok() && active.as_deref() == Some(name);
        self.update_presets(&store, clear.then_some(None))?;
        r?;
        Ok(self.announce())
    }

    // --- User EQ curves ------------------------------------------------------------

    fn curves_store(&self) -> ApiResult<EqCurveStore> {
        Ok(EqCurveStore::in_dir(
            &self.paths.as_ref().ok_or_else(no_paths)?.config_dir,
        ))
    }

    /// Saved curves, re-read from disk (also refreshes the snapshot's list).
    pub fn list_eq_curves(&self) -> ApiResult<Vec<EqCurve>> {
        let list = self.curves_store()?.list()?;
        let changed = {
            let mut inner = lock(&self.inner);
            let changed = inner.eq_curves != list;
            inner.eq_curves = list.clone();
            changed
        };
        if changed {
            self.announce();
        }
        Ok(list)
    }

    /// Save the active output's current bands and pre-amp under `name`
    /// (replaces a curve with the same name). Rejected while Scout (the bands
    /// are the device's) or Direct Mode is on.
    pub fn save_eq_curve(&self, name: &str) -> ApiResult<AppState> {
        let name = eq_curves::validate_name(name)?;
        let store = self.curves_store()?;
        let _op = lock(&self.ops);
        self.check_direct_mode_off()?;
        let state = self.connected_state()?;
        Self::check_scout_off(&state)?;
        let unread = || ApiError::new(ErrorCode::NotConnected, "EQ values not read yet");
        let mut bands = [0.0; 10];
        for (slot, b) in bands
            .iter_mut()
            .zip(state.active_profile().ok_or_else(unread)?.eq_bands())
        {
            *slot = b.ok_or_else(unread)?;
        }
        let preamp = state.feature(feat("eq_preamp")).ok_or_else(unread)?;
        store.save(&name, bands, preamp).map_err(ApiError::from)?;
        self.reload_eq_curves(&store)?;
        Ok(self.announce())
    }

    /// Works while disconnected.
    pub fn delete_eq_curve(&self, name: &str) -> ApiResult<AppState> {
        let store = self.curves_store()?;
        let _op = lock(&self.ops);
        let r = store.delete(name);
        self.reload_eq_curves(&store)?;
        r?;
        Ok(self.announce())
    }

    fn reload_eq_curves(&self, store: &EqCurveStore) -> ApiResult<()> {
        let list = store.list()?;
        lock(&self.inner).eq_curves = list;
        Ok(())
    }

    // --- App -------------------------------------------------------------------------

    /// Record the launch-at-login state (the autostart plugin is driven by
    /// the Tauri command).
    pub fn set_launch_at_login_flag(&self, enabled: bool) -> AppState {
        lock(&self.inner).launch_at_login = enabled;
        self.announce()
    }

    // --- Events ------------------------------------------------------------------------

    /// Handle one burst of session events: bookkeeping, side effects, then
    /// `connection-changed` / `output-changed` and one `state-changed`.
    fn handle_burst(&self, burst: &[Event]) {
        for ev in burst {
            lock(&self.inner).pending.observe(ev);
        }
        let mut connection = None;
        let mut output = None;
        for ev in burst {
            match ev {
                Event::Connected => connection = Some(Connection::Connected),
                Event::Disconnected => connection = Some(Connection::Disconnected),
                Event::OutputSwitched(o) => output = Some(Output::from(*o)),
                Event::Changed(_) => {}
            }
        }
        // The session state, not the burst, says where we ended up.
        let connected = self.session.state().connected;
        if connection.is_some() {
            connection = Some(if connected {
                Connection::Connected
            } else {
                Connection::Disconnected
            });
        }
        if connection == Some(Connection::Connected) {
            match self.session.device().firmware() {
                Ok(fw) => {
                    lock(&self.inner).firmware = Some(fw.trim_end_matches('\0').trim().to_string())
                }
                Err(e) => log::warn!("cannot read the firmware version: {e}"),
            }
        }
        if connected && (connection == Some(Connection::Connected) || output.is_some()) {
            self.follow_direct_mode();
        }
        if let Some(c) = connection {
            let firmware = lock(&self.inner).firmware.clone();
            self.sink.connection_changed(&ConnectionChanged {
                connection: c,
                firmware,
            });
        }
        if let Some(o) = output {
            self.sink.output_changed(&OutputChanged { output: o });
        }
        self.announce();
    }
}

fn settings_file(p: &Paths) -> PathBuf {
    p.config_dir.join(SETTINGS_FILE)
}

/// Forward session events to the sink until the session stops. Runs on its
/// own thread; holds only a weak reference so dropping the backend stops
/// the session (and with it this loop).
pub fn run_forwarder(backend: Weak<Backend>, events: Receiver<Event>) {
    while let Ok(first) = events.recv() {
        let mut burst = vec![first];
        let start = Instant::now();
        while start.elapsed() < MAX_BURST {
            match events.recv_timeout(COALESCE) {
                Ok(e) => burst.push(e),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        let Some(b) = backend.upgrade() else { return };
        b.handle_burst(&burst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use g6_core::events::Config;
    use g6_core::protocol::{self as core, Mask};
    use g6_core::session::SessionConfig;
    use g6_core::transport::fake::{FakeDevice, FAKE_FIRMWARE, SCOUT_CURVE};
    use g6_core::transport::Backoff;
    use std::sync::atomic::{AtomicU32, Ordering};

    // --- Helpers ---------------------------------------------------------------

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> TempDir {
            static N: AtomicU32 = AtomicU32::new(0);
            let p = std::env::temp_dir().join(format!(
                "opendac-app-test-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Debug, Clone, PartialEq)]
    enum Out {
        State(AppState),
        Conn(ConnectionChanged),
        Output(OutputChanged),
        Error(ApiError),
    }

    #[derive(Default)]
    struct Recorder(Mutex<Vec<Out>>);

    impl Sink for Recorder {
        fn state_changed(&self, s: &AppState) {
            lock(&self.0).push(Out::State(s.clone()));
        }
        fn connection_changed(&self, c: &ConnectionChanged) {
            lock(&self.0).push(Out::Conn(c.clone()));
        }
        fn output_changed(&self, o: &OutputChanged) {
            lock(&self.0).push(Out::Output(*o));
        }
        fn device_error(&self, e: &ApiError) {
            lock(&self.0).push(Out::Error(e.clone()));
        }
    }

    impl Recorder {
        fn take(&self) -> Vec<Out> {
            std::mem::take(&mut *lock(&self.0))
        }

        /// Wait until a recorded output matches.
        fn wait(&self, what: &str, pred: impl Fn(&Out) -> bool) -> Out {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(o) = lock(&self.0).iter().find(|o| pred(o)) {
                    return o.clone();
                }
                assert!(Instant::now() < deadline, "timed out waiting for {what}");
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        fn wait_state(&self, what: &str, pred: impl Fn(&AppState) -> bool) -> AppState {
            match self.wait(what, |o| matches!(o, Out::State(s) if pred(s))) {
                Out::State(s) => s,
                _ => unreachable!(),
            }
        }
    }

    fn fast() -> Config {
        Config {
            reply_timeout: Duration::from_millis(300),
            ack_timeout: Duration::from_millis(60),
            poll_interval: Duration::from_millis(5),
            probe_timeout: Duration::from_millis(100),
            backoff: Backoff {
                initial: Duration::from_millis(2),
                max: Duration::from_millis(10),
            },
        }
    }

    struct Rig {
        fake: FakeDevice,
        rec: Arc<Recorder>,
        backend: Arc<Backend>,
        _dir: TempDir,
    }

    fn fake_feature(fake: &FakeDevice, name: &str) -> f32 {
        let f = feat(name);
        fake.feature(f.family, f.id).unwrap()
    }

    fn set_fake(fake: &FakeDevice, name: &str, v: f32) {
        let f = feat(name);
        fake.set_feature(f.family, f.id, v);
    }

    fn rig_with(fake: FakeDevice, dir: TempDir) -> Rig {
        let paths = Paths::under(&dir.0);
        let (session, events) = Session::start(
            Box::new(fake.connector()),
            fast(),
            SessionConfig {
                paths: Some(paths.clone()),
                reapply_on_first_connect: true,
            },
        );
        let rec = Arc::new(Recorder::default());
        let backend = Backend::new(session, Some(paths), false, rec.clone());
        let weak = Arc::downgrade(&backend);
        std::thread::spawn(move || run_forwarder(weak, events));
        rec.wait_state("connected", |s| s.connection == Connection::Connected);
        Rig {
            fake,
            rec,
            backend,
            _dir: dir,
        }
    }

    fn rig() -> Rig {
        rig_with(FakeDevice::new(), TempDir::new())
    }

    // --- Pending (pure) ------------------------------------------------------------

    fn connected_state() -> DeviceState {
        let mut s = DeviceState::new();
        s.apply(&Event::Connected);
        s.apply(&Event::OutputSwitched(core::Output::Headphones));
        s
    }

    fn feature_change(name: &str, value: f32) -> Change {
        Change::Feature {
            feature: feat(name),
            value,
        }
    }

    #[test]
    fn pending_overlays_until_the_same_setting_is_reported() {
        let now = Instant::now();
        let mut p = Pending::default();
        p.push(feature_change("eq_1k", 3.0), now);
        p.push(Change::Mask(Mask(0x01)), now);

        let mut s = connected_state();
        p.apply(&mut s, now);
        assert_eq!(s.feature(feat("eq_1k")), Some(3.0));
        assert_eq!(s.mask, Some(Mask(0x01)));

        // Another feature's echo does not end the eq_1k entry.
        p.observe(&Event::Changed(feature_change("eq_2k", 1.0)));
        // The echo of eq_1k does (even with another value: the device spoke).
        p.observe(&Event::Changed(feature_change("eq_1k", 2.5)));
        let mut s = connected_state();
        p.apply(&mut s, now);
        assert_eq!(s.feature(feat("eq_1k")), None);
        assert_eq!(s.mask, Some(Mask(0x01)), "mask entry still pending");
    }

    #[test]
    fn pending_replaces_same_setting_expires_and_clears_on_lifecycle() {
        let now = Instant::now();
        let mut p = Pending::default();
        p.push(feature_change("surround_level", 0.2), now);
        p.push(feature_change("surround_level", 0.4), now);
        assert_eq!(p.0.len(), 1);
        let mut s = connected_state();
        p.apply(&mut s, now);
        assert_eq!(s.feature(feat("surround_level")), Some(0.4));

        let mut s = connected_state();
        p.apply(&mut s, now + PENDING_TTL);
        assert_eq!(s.feature(feat("surround_level")), None, "expired");

        p.push(Change::DacFilter(core::DacFilter::Nos), now);
        p.observe(&Event::OutputSwitched(core::Output::Speakers));
        assert!(p.0.is_empty());
    }

    // --- Against the fake device -----------------------------------------------------

    #[test]
    fn startup_snapshot_and_connection_events() {
        let r = rig();
        let conn = r
            .rec
            .wait("connection-changed", |o| matches!(o, Out::Conn(_)));
        assert_eq!(
            conn,
            Out::Conn(ConnectionChanged {
                connection: Connection::Connected,
                firmware: Some(FAKE_FIRMWARE.into()),
            })
        );
        let s = r.backend.snapshot();
        assert_eq!(s.connection, Connection::Connected);
        assert_eq!(s.output, Some(Output::Headphones));
        assert_eq!(s.firmware.as_deref(), Some(FAKE_FIRMWARE));
        assert_eq!(s.mic.boost, Some(30));
        assert!(!s.direct_mode);
    }

    #[test]
    fn revisions_increase() {
        let r = rig();
        let a = r.backend.snapshot();
        let b = r.backend.set_eq_enabled(true).unwrap();
        let c = r.backend.snapshot();
        assert!(a.revision < b.revision && b.revision < c.revision);
    }

    #[test]
    fn write_returns_the_confirmed_value() {
        let r = rig();
        let s = r.backend.set_eq_band(5, 2.3).unwrap();
        assert_eq!(s.eq.bands[5], 2.5, "rounded by g6-core, read back");
        assert_eq!(fake_feature(&r.fake, "eq_1k"), 2.5);
        let s = r.backend.set_effect_level(EffectName::Bass, 0.42).unwrap();
        assert_eq!(s.effects.bass.level, 0.42);
        let s = r.backend.set_noise_reduction(0.8).unwrap();
        assert_eq!(s.mic.noise_reduction, 0.8);
        assert_eq!(
            fake_feature(&r.fake, "mic_nr_level"),
            0.4,
            "UI 0..1 -> device 0..0.5"
        );
        let s = r.backend.set_mic_boost(10).unwrap();
        assert_eq!(s.mic.boost, Some(10));
        let s = r.backend.set_dac_filter(DacFilter::SlowLin).unwrap();
        assert_eq!(s.dac_filter, Some(DacFilter::SlowLin));
        let s = r
            .backend
            .set_smart_volume_mode(SmartVolumeMode::Loud)
            .unwrap();
        assert_eq!(s.effects.smart_volume.mode, SmartVolumeMode::Loud);
        let s = r.backend.set_dolby_decoder(DolbyMode::Night).unwrap();
        assert_eq!(s.dolby_decoder, Some(DolbyMode::Night));
        // The echo reaches the session and a later snapshot agrees.
        r.rec.wait_state("echo", |s| s.eq.bands[5] == 2.5);
    }

    #[test]
    fn dac_filter_and_mic_boost_writes_outlive_the_pending_overlay() {
        // The device does not push an echo for these two; the session must
        // still learn the new values (otherwise the UI reverts once the
        // overlay expires and the next replug re-applies the old value).
        let dir = TempDir::new();
        let file = Paths::under(&dir.0).last_known_file();
        let r = rig_with(FakeDevice::new(), dir);
        r.backend.set_dac_filter(DacFilter::SlowLin).unwrap();
        r.backend.set_mic_boost(10).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let st = r.backend.session.state();
            let on_disk = store::read_json::<DeviceState>(&file).unwrap_or(None);
            if st.dac_filter == Some(core::DacFilter::SlowLin)
                && st.mic_boost == Some(MicBoost::Db10)
                && on_disk.is_some_and(|d| {
                    d.dac_filter == Some(core::DacFilter::SlowLin)
                        && d.mic_boost == Some(MicBoost::Db10)
                })
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "session state / file not updated"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn invalid_values_are_rejected_before_sending() {
        let r = rig();
        let before = r.fake.written().len();
        let code = |res: ApiResult<AppState>| res.unwrap_err().code;
        assert_eq!(
            code(r.backend.set_eq_band(10, 0.0)),
            ErrorCode::InvalidValue
        );
        assert_eq!(
            code(r.backend.set_eq_band(0, 13.0)),
            ErrorCode::InvalidValue
        );
        assert_eq!(code(r.backend.set_mic_boost(15)), ErrorCode::InvalidValue);
        assert_eq!(
            code(r.backend.set_eq_bands(&[0.0; 9])),
            ErrorCode::InvalidValue
        );
        let mut gains = [0.0; 10];
        gains[9] = f32::NAN;
        assert_eq!(
            code(r.backend.set_eq_bands(&gains)),
            ErrorCode::InvalidValue
        );
        assert_eq!(
            code(r.backend.set_effect_level(EffectName::Dialog, 1.5)),
            ErrorCode::InvalidValue
        );
        assert_eq!(r.fake.written().len(), before, "nothing was sent");
    }

    #[test]
    fn set_eq_bands_writes_all_ten() {
        let r = rig();
        let gains = [1.0, 2.0, 3.0, 4.0, 5.0, -1.0, -2.0, -3.0, -4.0, 0.5];
        let s = r.backend.set_eq_bands(&gains).unwrap();
        assert_eq!(s.eq.bands, gains);
        for (i, g) in gains.iter().enumerate() {
            let f = features::eq_band(i).unwrap();
            assert_eq!(r.fake.feature(f.family, f.id), Some(*g));
        }
    }

    #[test]
    fn preamp_is_written_per_snapshot_and_stays_writable_in_scout() {
        let r = rig();
        let s = r.backend.set_eq_preamp(-2.5).unwrap();
        assert_eq!(s.eq.preamp, -2.5);
        assert_eq!(fake_feature(&r.fake, "eq_preamp"), -2.5);
        r.rec.wait_state("echo", |s| s.eq.preamp == -2.5);

        // Not Scout-owned.
        r.backend.set_scout(true).unwrap();
        let s = r.backend.set_eq_preamp(3.0).unwrap();
        assert_eq!(s.eq.preamp, 3.0);

        let before = r.fake.written().len();
        let code = |res: ApiResult<AppState>| res.unwrap_err().code;
        assert_eq!(code(r.backend.set_eq_preamp(6.5)), ErrorCode::InvalidValue);
        assert_eq!(code(r.backend.set_eq_preamp(-6.5)), ErrorCode::InvalidValue);
        assert_eq!(r.fake.written().len(), before, "nothing was sent");
    }

    #[test]
    fn preamp_is_locked_and_untouched_by_direct_mode() {
        let r = rig();
        r.backend.set_eq_preamp(-4.0).unwrap();
        let s = r.backend.set_direct_mode(true).unwrap();
        assert_eq!(s.eq.preamp, -4.0);
        assert_eq!(fake_feature(&r.fake, "eq_preamp"), -4.0);
        assert_eq!(
            r.backend.set_eq_preamp(0.0).unwrap_err().code,
            ErrorCode::DirectModeActive
        );
        let s = r.backend.set_direct_mode(false).unwrap();
        assert_eq!(s.eq.preamp, -4.0);
    }

    const BASS_BOOST: [f32; 10] = [6.0, 5.0, 4.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];

    #[test]
    fn apply_eq_curve_writes_bands_and_preamp_and_turns_eq_on() {
        let r = rig();
        assert!(!r.backend.snapshot().eq.enabled);
        let s = r.backend.apply_eq_curve(&BASS_BOOST, -6.0).unwrap();
        assert_eq!(s.eq.bands, BASS_BOOST);
        assert_eq!(s.eq.preamp, -6.0);
        assert!(s.eq.enabled);
        for (i, g) in BASS_BOOST.iter().enumerate() {
            let f = features::eq_band(i).unwrap();
            assert_eq!(r.fake.feature(f.family, f.id), Some(*g));
        }
        assert_eq!(fake_feature(&r.fake, "eq_preamp"), -6.0);
        assert_eq!(fake_feature(&r.fake, "eq"), 1.0);
        r.rec
            .wait_state("echo", |s| s.eq.bands == BASS_BOOST && s.eq.preamp == -6.0);
    }

    #[test]
    fn apply_eq_curve_validates_before_writing_and_respects_scout_and_direct_mode() {
        let r = rig();
        let before = r.fake.written().len();
        let code = |res: ApiResult<AppState>| res.unwrap_err().code;
        assert_eq!(
            code(r.backend.apply_eq_curve(&[0.0; 9], 0.0)),
            ErrorCode::InvalidValue
        );
        assert_eq!(
            code(r.backend.apply_eq_curve(&[0.0; 10], 6.5)),
            ErrorCode::InvalidValue
        );
        let mut bad = [0.0; 10];
        bad[9] = 12.5;
        assert_eq!(
            code(r.backend.apply_eq_curve(&bad, 0.0)),
            ErrorCode::InvalidValue
        );
        bad[9] = f32::NAN;
        assert_eq!(
            code(r.backend.apply_eq_curve(&bad, 0.0)),
            ErrorCode::InvalidValue
        );
        assert_eq!(r.fake.written().len(), before, "nothing was sent");

        r.backend.set_scout(true).unwrap();
        let before = r.fake.written().len();
        assert_eq!(
            code(r.backend.apply_eq_curve(&BASS_BOOST, -6.0)),
            ErrorCode::ScoutActive
        );
        assert_eq!(r.fake.written().len(), before, "nothing was sent");
        r.backend.set_scout(false).unwrap();

        r.backend.set_direct_mode(true).unwrap();
        assert_eq!(
            code(r.backend.apply_eq_curve(&BASS_BOOST, -6.0)),
            ErrorCode::DirectModeActive
        );
    }

    #[test]
    fn eq_curves_save_list_apply_delete_and_persist() {
        let dir = TempDir::new();
        let root = dir.0.clone();
        let r = rig_with(FakeDevice::new(), dir);
        assert!(r.backend.snapshot().eq_curves.is_empty());

        r.backend.apply_eq_curve(&BASS_BOOST, -6.0).unwrap();
        let s = r.backend.save_eq_curve("  Mine ").unwrap();
        assert_eq!(
            s.eq_curves,
            [EqCurve {
                name: "Mine".into(),
                bands: BASS_BOOST,
                preamp: -6.0
            }]
        );
        assert!(root.join("config").join(eq_curves::FILE).exists());
        // The new list also went out as a state-changed.
        r.rec
            .wait_state("curve announced", |s| s.eq_curves.len() == 1);
        assert_eq!(r.backend.list_eq_curves().unwrap(), s.eq_curves);

        // Applying a saved curve is apply_eq_curve with its values.
        r.backend.apply_eq_curve(&[0.0; 10], 0.0).unwrap();
        let c = &s.eq_curves[0];
        let applied = r.backend.apply_eq_curve(&c.bands, c.preamp).unwrap();
        assert_eq!(applied.eq.bands, BASS_BOOST);
        assert_eq!(applied.eq.preamp, -6.0);

        let code = |res: ApiResult<AppState>| res.unwrap_err().code;
        assert_eq!(
            code(r.backend.save_eq_curve("flat")),
            ErrorCode::InvalidValue,
            "built-in names are reserved"
        );
        assert_eq!(code(r.backend.save_eq_curve("  ")), ErrorCode::InvalidValue);
        assert_eq!(
            code(
                r.backend
                    .save_eq_curve(&"n".repeat(eq_curves::MAX_NAME_LEN + 1))
            ),
            ErrorCode::InvalidValue
        );
        assert_eq!(
            code(r.backend.delete_eq_curve("Nope")),
            ErrorCode::CurveNotFound
        );
        assert_eq!(r.backend.snapshot().eq_curves.len(), 1);

        // Survives a restart.
        let Rig { backend, _dir, .. } = r;
        let weak = Arc::downgrade(&backend);
        drop(backend);
        wait_dropped(&weak);
        let r = rig_with(FakeDevice::new(), _dir);
        assert_eq!(r.backend.snapshot().eq_curves.len(), 1);

        // Delete works while disconnected.
        r.fake.unplug();
        r.rec
            .wait_state("disconnected", |s| s.connection == Connection::Disconnected);
        assert_eq!(
            code(r.backend.save_eq_curve("Other")),
            ErrorCode::NotConnected
        );
        assert!(r
            .backend
            .delete_eq_curve("mine")
            .unwrap()
            .eq_curves
            .is_empty());
    }

    #[test]
    fn saving_a_curve_is_rejected_in_scout_and_direct_mode() {
        let r = rig();
        let code = |res: ApiResult<AppState>| res.unwrap_err().code;
        r.backend.set_scout(true).unwrap();
        assert_eq!(code(r.backend.save_eq_curve("A")), ErrorCode::ScoutActive);
        r.backend.set_scout(false).unwrap();
        r.backend.set_direct_mode(true).unwrap();
        assert_eq!(
            code(r.backend.save_eq_curve("A")),
            ErrorCode::DirectModeActive
        );
        assert!(r.backend.snapshot().eq_curves.is_empty());
    }

    #[test]
    fn scout_owns_eq_and_smart_volume() {
        let r = rig();
        let s = r.backend.set_scout(true).unwrap();
        assert!(s.scout);
        let code = |res: ApiResult<AppState>| res.unwrap_err().code;
        assert_eq!(code(r.backend.set_eq_band(0, 1.0)), ErrorCode::ScoutActive);
        assert_eq!(
            code(r.backend.set_eq_enabled(false)),
            ErrorCode::ScoutActive
        );
        assert_eq!(
            code(r.backend.set_eq_bands(&[0.0; 10])),
            ErrorCode::ScoutActive
        );
        assert_eq!(
            code(r.backend.set_effect_level(EffectName::SmartVolume, 0.5)),
            ErrorCode::ScoutActive
        );
        // Other effects stay writable.
        r.backend
            .set_effect_enabled(EffectName::Surround, true)
            .unwrap();
        // The session re-reads what Scout set.
        r.rec
            .wait_state("scout curve", |s| s.scout && s.eq.bands == SCOUT_CURVE);
    }

    #[test]
    fn sbx_on_switches_scout_off_first() {
        let r = rig();
        r.backend.set_scout(true).unwrap();
        let s = r.backend.set_sbx(true).unwrap();
        assert!(s.sbx && !s.scout);
        assert!(!Mask(r.fake.mask()).scout());
        assert!(Mask(r.fake.mask()).sbx());
    }

    #[test]
    fn hardware_buttons_and_jack_reach_the_ui() {
        let r = rig();
        r.rec.take();
        r.fake.press_mask(Mask::SBX);
        r.rec.wait_state("sbx button", |s| s.sbx);
        r.fake.switch_output(core::Output::Speakers);
        r.rec.wait("output-changed", |o| {
            *o == Out::Output(OutputChanged {
                output: Output::Speakers,
            })
        });
        r.rec
            .wait_state("speakers", |s| s.output == Some(Output::Speakers));
        // output-changed comes before the state-changed of its burst.
        let outs = r.rec.take();
        let oc = outs
            .iter()
            .position(|o| matches!(o, Out::Output(_)))
            .unwrap();
        assert!(outs[oc + 1..].iter().any(|o| matches!(o, Out::State(_))));
    }

    #[test]
    fn disconnect_and_reconnect() {
        let r = rig();
        r.rec.take();
        r.fake.unplug();
        r.rec.wait(
            "disconnected",
            |o| matches!(o, Out::Conn(c) if c.connection == Connection::Disconnected),
        );
        let s = r.backend.snapshot();
        assert_eq!(s.connection, Connection::Disconnected);
        assert_eq!(
            r.backend.set_eq_enabled(true).unwrap_err().code,
            ErrorCode::NotConnected
        );
        r.fake.replug();
        r.rec.wait(
            "reconnected",
            |o| matches!(o, Out::Conn(c) if c.connection == Connection::Connected),
        );
        r.rec
            .wait_state("connected", |s| s.connection == Connection::Connected);
    }

    #[test]
    fn direct_mode_blocks_playback_and_restores() {
        let r = rig();
        r.backend.set_sbx(true).unwrap();
        r.backend.set_eq_enabled(true).unwrap();
        let s = r.backend.set_direct_mode(true).unwrap();
        assert!(s.direct_mode);
        assert!(!s.sbx && !s.eq.enabled);
        assert_eq!(fake_feature(&r.fake, "eq"), 0.0);

        let code = |res: ApiResult<AppState>| res.unwrap_err().code;
        assert_eq!(
            code(r.backend.set_eq_band(0, 1.0)),
            ErrorCode::DirectModeActive
        );
        assert_eq!(code(r.backend.set_sbx(true)), ErrorCode::DirectModeActive);
        assert_eq!(code(r.backend.set_scout(true)), ErrorCode::DirectModeActive);
        assert_eq!(
            code(r.backend.set_effect_enabled(EffectName::Bass, true)),
            ErrorCode::DirectModeActive
        );
        assert_eq!(
            code(r.backend.save_preset("x")),
            ErrorCode::DirectModeActive
        );
        // Mic and device settings stay usable.
        r.backend.set_mic_aec(true).unwrap();
        r.backend.set_dac_filter(DacFilter::Nos).unwrap();

        let s = r.backend.set_direct_mode(false).unwrap();
        assert!(!s.direct_mode);
        assert!(s.sbx && s.eq.enabled, "restored");
        // Disabling again is a no-op that resolves.
        assert!(!r.backend.set_direct_mode(false).unwrap().direct_mode);
    }

    #[test]
    fn direct_mode_follows_the_jack() {
        let r = rig();
        r.backend.set_eq_enabled(true).unwrap();
        r.backend.set_direct_mode(true).unwrap();
        // The fake keeps one set of values: make the speakers' EQ look on.
        set_fake(&r.fake, "eq", 1.0);
        r.fake.switch_output(core::Output::Speakers);
        // The backend switches EQ off on the new output.
        let deadline = Instant::now() + Duration::from_secs(5);
        while fake_feature(&r.fake, "eq") != 0.0 {
            assert!(Instant::now() < deadline, "EQ was not switched off");
            std::thread::sleep(Duration::from_millis(5));
        }
        r.rec.wait_state("eq off on speakers", |s| {
            s.output == Some(Output::Speakers) && s.direct_mode && !s.eq.enabled
        });
        let saved = DirectMode::new(Paths::under(&r._dir.0).direct_file())
            .saved()
            .unwrap()
            .unwrap();
        assert!(saved.outputs.contains_key("speakers"));
    }

    #[test]
    fn pending_restore_finishes_when_the_jack_switches_back() {
        let r = rig();
        let dm = DirectMode::new(Paths::under(&r._dir.0).direct_file());
        // Headphones: EQ and Bass on.
        set_fake(&r.fake, "eq", 1.0);
        set_fake(&r.fake, "bass", 1.0);
        set_fake(&r.fake, "surround", 0.0);
        r.backend.set_direct_mode(true).unwrap();
        // Speakers (the fake keeps one set of values): EQ and Surround on.
        set_fake(&r.fake, "eq", 1.0);
        set_fake(&r.fake, "bass", 0.0);
        set_fake(&r.fake, "surround", 1.0);
        r.fake.switch_output(core::Output::Speakers);
        r.rec.wait_state("speakers in direct mode", |s| {
            s.output == Some(Output::Speakers) && s.direct_mode && !s.eq.enabled
        });
        assert_eq!(dm.pending_outputs().unwrap().len(), 2);
        // Back to headphones, then Direct Mode off there.
        set_fake(&r.fake, "eq", 0.0);
        set_fake(&r.fake, "bass", 1.0);
        set_fake(&r.fake, "surround", 0.0);
        r.rec.take();
        r.fake.switch_output(core::Output::Headphones);
        r.rec
            .wait_state("headphones", |s| s.output == Some(Output::Headphones));
        let s = r.backend.set_direct_mode(false).unwrap();
        assert!(!s.direct_mode);
        assert_eq!(dm.pending_outputs().unwrap(), [core::Output::Speakers]);

        // Speakers again, still showing Direct Mode's state: the restore is
        // finished without a command.
        set_fake(&r.fake, "eq", 0.0);
        set_fake(&r.fake, "bass", 1.0);
        set_fake(&r.fake, "surround", 0.0);
        r.rec.take();
        r.fake.switch_output(core::Output::Speakers);
        let s = r.rec.wait_state("speakers restored", |s| {
            s.output == Some(Output::Speakers) && s.eq.enabled
        });
        assert!(!s.direct_mode);
        assert_eq!(fake_feature(&r.fake, "eq"), 1.0);
        assert_eq!(fake_feature(&r.fake, "surround"), 1.0);
        assert_eq!(fake_feature(&r.fake, "bass"), 0.0);
        assert!(!dm.path().exists(), "nothing left to restore");
        assert!(dm.pending_outputs().unwrap().is_empty());
    }

    #[test]
    fn presets_save_apply_delete() {
        let r = rig();
        r.backend.set_eq_band(0, 4.0).unwrap();
        r.backend
            .set_effect_enabled(EffectName::Bass, true)
            .unwrap();
        let s = r.backend.save_preset("Music").unwrap();
        assert_eq!(s.presets, ["Music"]);
        assert_eq!(s.active_preset.as_deref(), Some("Music"));
        assert_eq!(r.backend.list_presets().unwrap(), ["Music"]);

        r.backend.set_eq_band(0, -4.0).unwrap();
        r.backend
            .set_effect_enabled(EffectName::Bass, false)
            .unwrap();
        let s = r.backend.apply_preset("Music").unwrap();
        assert_eq!(s.eq.bands[0], 4.0);
        assert!(s.effects.bass.enabled);

        let code = |res: ApiResult<AppState>| res.unwrap_err().code;
        assert_eq!(
            code(r.backend.apply_preset("Nope")),
            ErrorCode::PresetNotFound
        );
        assert_eq!(
            code(r.backend.save_preset("../x")),
            ErrorCode::PresetInvalidName
        );
        assert_eq!(
            code(r.backend.delete_preset("Nope")),
            ErrorCode::PresetNotFound
        );

        let s = r.backend.delete_preset("Music").unwrap();
        assert!(s.presets.is_empty());
        assert_eq!(
            s.active_preset, None,
            "deleting the active preset clears it"
        );
    }

    #[test]
    fn applying_a_profile_without_scout_info_keeps_scouts_eq() {
        // Profiles captured for the inactive output carry no sbx / scout
        // entry; applied while Scout is on they must not write EQ into it.
        let dir = TempDir::new();
        let store = PresetStore::from_paths(&Paths::under(&dir.0));
        let profile = preset::PresetProfile {
            features: [("eq_62".to_string(), -4.0), ("bass".to_string(), 1.0)].into(),
            ..Default::default()
        };
        let p = preset::Preset {
            headphones: Some(profile),
            speakers: None,
        };
        store.save("Hand", &p).unwrap();
        let r = rig_with(FakeDevice::new(), dir);
        r.backend.set_scout(true).unwrap();
        let s = r.backend.apply_preset("Hand").unwrap();
        assert!(s.scout, "Scout stays on");
        assert_eq!(fake_feature(&r.fake, "bass"), 1.0);
        assert_eq!(fake_feature(&r.fake, "eq_62"), SCOUT_CURVE[1]);
        assert_eq!(s.eq.bands, SCOUT_CURVE);
    }

    #[test]
    fn presets_rename() {
        let r = rig();
        r.backend.save_preset("Music").unwrap();
        r.backend.save_preset("Games").unwrap();
        let code = |res: ApiResult<AppState>| res.unwrap_err().code;
        assert_eq!(
            code(r.backend.rename_preset("Nope", "X")),
            ErrorCode::PresetNotFound
        );
        assert_eq!(
            code(r.backend.rename_preset("Music", "games")),
            ErrorCode::PresetInvalidName,
            "an existing name (ignoring case) is refused"
        );
        assert_eq!(
            code(r.backend.rename_preset("Music", "../x")),
            ErrorCode::PresetInvalidName
        );

        // "Games" is active (saved last): the active profile follows the rename.
        let s = r.backend.rename_preset("Games", "Gaming").unwrap();
        assert_eq!(s.presets, ["Gaming", "Music"]);
        assert_eq!(s.active_preset.as_deref(), Some("Gaming"));
        let s = r.backend.rename_preset("Music", "music").unwrap();
        assert!(s.presets.contains(&"music".to_string()), "{:?}", s.presets);
        assert_eq!(s.presets.len(), 2);
        assert_eq!(s.active_preset.as_deref(), Some("Gaming"));
    }

    #[test]
    fn active_preset_is_persisted() {
        let dir = TempDir::new();
        let root = dir.0.clone();
        let r = rig_with(FakeDevice::new(), dir);
        r.backend.save_preset("Games").unwrap();
        let Rig { backend, _dir, .. } = r;
        let weak = Arc::downgrade(&backend);
        drop(backend);
        wait_dropped(&weak);

        let r = rig_with(FakeDevice::new(), _dir);
        let s = r.backend.snapshot();
        assert_eq!(s.active_preset.as_deref(), Some("Games"));
        assert_eq!(s.presets, ["Games"]);
        assert!(root.join("config").join(SETTINGS_FILE).exists());
    }

    #[test]
    fn presets_work_while_disconnected() {
        let r = rig();
        r.backend.save_preset("A").unwrap();
        r.fake.unplug();
        r.rec
            .wait_state("disconnected", |s| s.connection == Connection::Disconnected);
        assert_eq!(r.backend.list_presets().unwrap(), ["A"]);
        assert_eq!(
            r.backend.apply_preset("A").unwrap_err().code,
            ErrorCode::NotConnected
        );
        assert!(r.backend.delete_preset("A").unwrap().presets.is_empty());
    }

    #[test]
    fn launch_at_login_flag_is_in_the_snapshot() {
        let r = rig();
        r.rec.take();
        let s = r.backend.set_launch_at_login_flag(true);
        assert!(s.launch_at_login);
        assert!(r
            .rec
            .take()
            .iter()
            .any(|o| matches!(o, Out::State(e) if e.launch_at_login)));
    }

    #[test]
    fn dropping_the_backend_stops_the_session() {
        let r = rig();
        let weak = Arc::downgrade(&r.backend);
        drop(r.backend);
        wait_dropped(&weak);
    }

    /// The forwarder may hold the backend for the length of one burst.
    fn wait_dropped(weak: &Weak<Backend>) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while weak.strong_count() > 0 {
            assert!(Instant::now() < deadline, "backend not dropped");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn no_paths_means_storage_errors_not_panics() {
        let fake = FakeDevice::new();
        let (session, events) = Session::start(
            Box::new(fake.connector()),
            fast(),
            SessionConfig {
                paths: None,
                reapply_on_first_connect: true,
            },
        );
        let rec = Arc::new(Recorder::default());
        let b = Backend::new(session, None, false, rec.clone());
        std::thread::spawn({
            let w = Arc::downgrade(&b);
            move || run_forwarder(w, events)
        });
        rec.wait_state("connected", |s| s.connection == Connection::Connected);
        assert_eq!(b.list_presets().unwrap_err().code, ErrorCode::Storage);
        assert_eq!(b.list_eq_curves().unwrap_err().code, ErrorCode::Storage);
        assert_eq!(b.save_eq_curve("A").unwrap_err().code, ErrorCode::Storage);
        assert_eq!(
            b.set_direct_mode(true).unwrap_err().code,
            ErrorCode::Storage
        );
        b.set_eq_enabled(true).unwrap();
    }
}
