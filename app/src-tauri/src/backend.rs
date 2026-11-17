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
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use g6_core::direct::{self, DirectMode};
use g6_core::events::{Change, Event};
use g6_core::features::{self, Feature};
use g6_core::preset::{self, Preset, PresetStore};
use g6_core::protocol::{self as core, MaskTarget, MicBoost};
use g6_core::session::Session;
use g6_core::state::DeviceState;
use g6_core::store::{self, Paths};
use serde::{Deserialize, Serialize};

use crate::api::{
    ApiError, ApiResult, AppState, AutoSwitch, Connection, ConnectionChanged, DacFilter,
    DeviceInfo, DolbyMode, EffectName, ErrorCode, Extras, Hotkeys, Mixer, MixerScope, Output,
    OutputChanged, SmartVolumeMode, MIC_EQ_BANDS,
};
use crate::eq_curves::{self, EqCurve, EqCurveStore};
use crate::hotkeys::{HotkeyAction, HotkeySettings};
use crate::mic_eq_curves::{self, MicEqCurve, MicEqCurveStore};
use crate::mixer::{MixerBackend, MixerError, Unsupported};
use crate::tray_menu::TrayAction;

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
    #[serde(default)]
    auto_switch: AutoSwitch,
    #[serde(default)]
    hotkeys: HotkeySettings,
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
/// written value overlaid. An entry ends when the session reports the
/// last written value (its echo) or a value no write of this entry set (a
/// change made elsewhere), when the device reconnects or switches output, or
/// after [`PENDING_TTL`]. The late echo of an older write to the same setting
/// (a slider drag, two curves in a row) does not end it, also when the older
/// value was written again later (A, B, A): an echo is matched to the oldest
/// outstanding write of that value.
#[derive(Debug, Default)]
struct Pending(Vec<PendingWrite>);

#[derive(Debug)]
struct PendingWrite {
    /// Writes to this setting whose echo has not arrived, oldest first; the
    /// last one is what the overlay shows. No two neighbours are equal.
    outstanding: Vec<Change>,
    at: Instant,
}

impl PendingWrite {
    fn newest(&self) -> Change {
        *self.outstanding.last().expect("an entry has a write")
    }
}

fn same_setting(a: &Change, b: &Change) -> bool {
    match (a, b) {
        (Change::Feature { feature: x, .. }, Change::Feature { feature: y, .. }) => {
            x.family == y.family && x.id == y.id
        }
        (Change::Mask(_), Change::Mask(_))
        | (Change::DacFilter(_), Change::DacFilter(_))
        | (Change::MicBoost(_), Change::MicBoost(_))
        | (Change::LightingEnabled(_), Change::LightingEnabled(_))
        | (Change::LightingColor(_), Change::LightingColor(_)) => true,
        _ => false,
    }
}

impl Pending {
    fn push(&mut self, change: Change, now: Instant) {
        if let Some(w) = self
            .0
            .iter_mut()
            .find(|w| same_setting(&w.newest(), &change))
        {
            if w.newest() != change {
                w.outstanding.push(change);
            }
            w.at = now;
            return;
        }
        self.0.push(PendingWrite {
            outstanding: vec![change],
            at: now,
        });
    }

    /// The session reported `ev`.
    fn observe(&mut self, ev: &Event) {
        match ev {
            Event::Changed(c) => self.0.retain_mut(|w| {
                if !same_setting(&w.newest(), c) {
                    return true;
                }
                // The echo of the oldest outstanding write of that value
                // settles it and every write before it; a value none of the
                // writes set is a change made elsewhere and ends the entry.
                match w.outstanding.iter().position(|o| o == c) {
                    Some(i) => {
                        w.outstanding.drain(..=i);
                        !w.outstanding.is_empty()
                    }
                    None => false,
                }
            }),
            Event::Connected | Event::Disconnected | Event::OutputSwitched(_) => self.0.clear(),
        }
    }

    fn clear(&mut self) {
        self.0.clear();
    }

    /// Overlay the live entries onto `state` (dropping expired ones).
    fn apply(&mut self, state: &mut DeviceState, now: Instant) {
        self.0
            .retain(|w| now.saturating_duration_since(w.at) < PENDING_TTL);
        for w in &self.0 {
            state.apply(&Event::Changed(w.newest()));
        }
    }
}

// ---------------------------------------------------------------------------
// Backend
// ---------------------------------------------------------------------------

struct Inner {
    revision: u64,
    firmware: Option<String>,
    /// `get_device_info` cache for the current connection (reset on connect).
    device_info: DeviceInfo,
    /// Counts connects, so a read that started before one is not cached.
    connection_epoch: u64,
    direct_mode: bool,
    presets: Vec<String>,
    active_preset: Option<String>,
    auto_switch: AutoSwitch,
    /// Output the last handled event left the device on (`None` until the
    /// first connect has been seen). Rules fire only when it changes.
    last_output: Option<Output>,
    /// Output a manual switch (`set_output`) is moving to, set before the
    /// write and cleared after the resync: a burst that ends there is not a
    /// jack switch, however late the forwarder handles it.
    manual_output: Option<Output>,
    eq_curves: Vec<EqCurve>,
    mic_eq_curves: Vec<MicEqCurve>,
    launch_at_login: bool,
    hotkeys: HotkeySettings,
    /// Registration failure per hotkey ([`HotkeyAction::index`]); set by the shell.
    hotkey_errors: [Option<String>; 3],
    /// The G6's USB audio controls as last read (`None`: not supported / not found).
    mixer: Option<Mixer>,
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
    /// The OS audio controls (not HID, so not behind `ops`).
    mixer: Arc<dyn MixerBackend>,
    /// Serialises mixer reads and writes, so the cached state is never older
    /// than a write that has returned.
    mixer_ops: Mutex<()>,
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
        Backend::with_mixer(session, paths, launch_at_login, Arc::new(Unsupported), sink)
    }

    /// [`new`](Self::new) with a mixer (the platform's, see
    /// [`mixer::platform`](crate::mixer::platform)). Starts the thread that
    /// turns the mixer's change hints into snapshots.
    pub fn with_mixer(
        session: Session,
        paths: Option<Paths>,
        launch_at_login: bool,
        mixer: Arc<dyn MixerBackend>,
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
        let mic_eq_curves = paths
            .as_ref()
            .map(|p| {
                MicEqCurveStore::in_dir(&p.config_dir)
                    .list()
                    .unwrap_or_else(|e| {
                        log::warn!("ignoring saved mic EQ curves: {e}");
                        Vec::new()
                    })
            })
            .unwrap_or_default();
        let backend = Arc::new(Backend {
            session,
            paths,
            sink,
            inner: Mutex::new(Inner {
                revision: 0,
                firmware: None,
                device_info: DeviceInfo::default(),
                connection_epoch: 0,
                direct_mode,
                presets,
                active_preset: settings.active_preset,
                auto_switch: settings.auto_switch,
                last_output: None,
                manual_output: None,
                eq_curves,
                mic_eq_curves,
                launch_at_login,
                hotkeys: settings.hotkeys.sanitized(),
                hotkey_errors: Default::default(),
                mixer: None,
                pending: Pending::default(),
            }),
            ops: Mutex::new(()),
            mixer,
            mixer_ops: Mutex::new(()),
        });
        // Subscribe before the first read, so no change slips in between.
        let (tx, rx) = mpsc::channel::<()>();
        let tx = Mutex::new(tx);
        backend.mixer.subscribe(Box::new(move || {
            let _ = lock(&tx).send(());
        }));
        backend.sync_mixer();
        let weak = Arc::downgrade(&backend);
        std::thread::Builder::new()
            .name("g6-mixer".into())
            .spawn(move || run_mixer_watcher(weak, rx))
            .expect("spawn the mixer watcher thread");
        backend
    }

    // --- Device info ---------------------------------------------------------

    /// Firmware, serial, DSP version and hardware id of the connected G6.
    /// They do not change, so each is read once per connection and cached; a
    /// field the device did not answer is `None` and is tried again on the
    /// next call. Never fails. While disconnected it returns what is cached.
    /// The serial is never logged.
    pub fn device_info(&self) -> DeviceInfo {
        let (mut info, epoch) = {
            let inner = lock(&self.inner);
            let mut info = inner.device_info.clone();
            if info.firmware.is_none() {
                info.firmware = inner.firmware.clone();
            }
            (info, inner.connection_epoch)
        };
        if !self.session.state().connected {
            return info;
        }
        let d = self.session.device();
        if info.firmware.is_none() {
            info.firmware = soft("firmware version", d.firmware())
                .map(|f| f.trim_end_matches('\0').trim().to_string());
        }
        if info.serial.is_none() {
            info.serial = soft("serial", d.serial()).map(|b| hex(&b));
        }
        if info.dsp_version.is_none() {
            info.dsp_version = soft("DSP version", d.dsp_version()).map(|b| hex(&b));
        }
        if info.hardware_id.is_none() {
            info.hardware_id = soft("hardware id", d.hw_id()).map(|b| hex(&b));
        }
        let mut inner = lock(&self.inner);
        if inner.connection_epoch == epoch {
            inner.device_info = info.clone();
        }
        info
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
                auto_switch: inner.auto_switch.clone(),
                eq_curves: inner.eq_curves.clone(),
                mic_eq_curves: inner.mic_eq_curves.clone(),
                launch_at_login: inner.launch_at_login,
                hotkeys: Hotkeys::new(&inner.hotkeys, &inner.hotkey_errors),
                mixer: inner.mixer.clone(),
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

    /// EQ pre-amp (dB). Allowed while Scout is on (not Scout-owned), but the G6
    /// sets it to 0 dB with Scout on and restores it after, so the snapshot
    /// follows the device; writing it during Scout is not verified on hardware.
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
        self.set_sbx_locked(Some(enabled))
    }

    /// [`Self::set_sbx`], `None` inverting the current state (the hotkey:
    /// read under `ops`, so two quick presses are two toggles). The caller
    /// holds `ops`.
    fn set_sbx_locked(&self, enabled: Option<bool>) -> ApiResult<AppState> {
        self.check_direct_mode_off()?;
        let s = self.connected_state()?;
        let enabled = enabled.unwrap_or_else(|| !s.mask.is_some_and(|m| m.sbx()));
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

    /// One mic EQ band, `band` 0..=7 (mic EQ bands 1..=8), dB.
    pub fn set_mic_eq_band(&self, band: u8, gain: f32) -> ApiResult<AppState> {
        let name = MIC_EQ_BANDS.get(usize::from(band)).ok_or_else(|| {
            ApiError::new(
                ErrorCode::InvalidValue,
                format!("mic EQ band {band} does not exist (0..7)"),
            )
        })?;
        self.plain_write(feat(name), gain)
    }

    /// Write all 8 mic EQ bands of a curve or preset, then turn mic EQ on.
    /// Everything is validated before the first write; if a write fails the
    /// device is re-read. Mic settings are outside Direct Mode and Scout, so
    /// neither blocks this.
    pub fn apply_mic_eq_curve(&self, bands: &[f32]) -> ApiResult<AppState> {
        if bands.len() != MIC_EQ_BANDS.len() {
            return Err(ApiError::new(
                ErrorCode::InvalidValue,
                format!("expected 8 mic EQ gains, got {}", bands.len()),
            ));
        }
        let writes: Vec<(&'static Feature, f32)> = MIC_EQ_BANDS
            .iter()
            .zip(bands)
            .map(|(n, g)| (feat(n), *g))
            .chain([(feat("mic_eq"), 1.0)])
            .collect();
        for (f, v) in &writes {
            f.validate_write(*v)
                .map_err(|e| ApiError::from(g6_core::events::Error::from(e)))?;
        }
        let _op = lock(&self.ops);
        self.connected_state()?;
        if let Err(e) = writes.iter().try_for_each(|(f, v)| self.write(f, *v)) {
            self.resync();
            return Err(e);
        }
        Ok(self.snapshot())
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

    pub fn set_lighting_enabled(&self, enabled: bool) -> ApiResult<AppState> {
        let _op = lock(&self.ops);
        self.connected_state()?;
        let w = self.session.device().set_lighting_enabled(enabled)?;
        self.pend(Change::LightingEnabled(w.value));
        Ok(self.snapshot())
    }

    /// `color` is `rrggbb` (a leading `#` is tolerated).
    pub fn set_lighting_color(&self, color: &str) -> ApiResult<AppState> {
        let rgb = core::rgb_from_hex(color).ok_or_else(|| {
            ApiError::new(
                ErrorCode::InvalidValue,
                format!("lighting color must be rrggbb hex, not {color:?}"),
            )
        })?;
        let _op = lock(&self.ops);
        self.connected_state()?;
        let w = self.session.device().set_lighting_color(rgb)?;
        self.pend(Change::LightingColor(w.value));
        Ok(self.snapshot())
    }

    /// Manual output override; the jack still decides on the next plug / unplug.
    /// The device answers with the new output and a dump of its profile; the
    /// session applies those as events, so re-read before the snapshot (the
    /// per-output values must belong to the new output).
    pub fn set_output(&self, output: Output) -> ApiResult<AppState> {
        let _op = lock(&self.ops);
        self.connected_state()?;
        // A manual switch is not a jack switch: auto-switch rules do not fire.
        // The forwarder does not take `ops`, so it can handle the device's
        // push while this runs: say what is coming before the write.
        lock(&self.inner).manual_output = Some(output);
        let written = self.session.device().set_output(output.into());
        if written.is_ok() {
            self.resync();
        }
        {
            let mut inner = lock(&self.inner);
            inner.manual_output = None;
            // The confirmed write, not the session state (which may not
            // have applied the switch yet if the resync failed).
            if let Ok(w) = &written {
                inner.last_output = Some(Output::from(w.value));
            }
        }
        written?;
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
        self.set_direct_mode_locked(&dm, enabled)
    }

    /// The hotkey: the state to invert is read under `ops`, so two quick
    /// presses are two toggles.
    fn toggle_direct_mode(&self) -> ApiResult<AppState> {
        let dm = self.direct()?;
        let _op = lock(&self.ops);
        let on = dm.is_active()?;
        self.set_direct_mode_locked(&dm, !on)
    }

    /// [`Self::set_direct_mode`]; the caller holds `ops`.
    fn set_direct_mode_locked(&self, dm: &DirectMode, enabled: bool) -> ApiResult<AppState> {
        if !enabled && dm.saved()?.is_none() {
            // Not on and nothing waiting to be restored: nothing to do. The
            // cached flag may still have been stale (file removed elsewhere):
            // then tell the other listeners too.
            let before = lock(&self.inner).direct_mode;
            self.reload_direct_flag(dm);
            if lock(&self.inner).direct_mode != before {
                return Ok(self.announce());
            }
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
        self.reload_direct_flag(dm);
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
    /// Decided under `ops` from the file, not the cached flag: a
    /// `set_direct_mode` that ran while this waited for `ops` must not be
    /// undone.
    fn follow_direct_mode(&self) {
        let Ok(dm) = self.direct() else { return };
        let _op = lock(&self.ops);
        let active = match dm.is_active() {
            Ok(a) => a,
            Err(e) => {
                log::warn!("cannot read the Direct Mode file: {e}");
                return;
            }
        };
        if active {
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
        let output = match self.session.device().read_output() {
            Ok(o) => o,
            Err(e) => {
                log::warn!("cannot read the active output: {e}");
                return;
            }
        };
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
                auto_switch: inner.auto_switch.clone(),
                hotkeys: inner.hotkeys.clone(),
            },
        )?;
        Ok(())
    }

    /// Re-list presets into the cache; `active` replaces the active preset.
    /// Auto-switch rules naming a profile that no longer exists are cleared.
    fn update_presets(&self, store: &PresetStore, active: Option<Option<String>>) -> ApiResult<()> {
        let list = store.list()?;
        let mut inner = lock(&self.inner);
        let mut dirty = inner
            .auto_switch
            .map(|n| list.iter().any(|p| p == n).then(|| n.to_string()));
        inner.presets = list;
        if let Some(a) = active {
            if inner.active_preset != a {
                inner.active_preset = a;
                dirty = true;
            }
        }
        if dirty {
            self.save_settings(&inner)?;
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
        let _op = lock(&self.ops);
        self.apply_preset_locked(name)?;
        Ok(self.announce())
    }

    /// The apply of [`Self::apply_preset`], also used by auto-switch. The
    /// caller holds `ops`.
    fn apply_preset_locked(&self, name: &str) -> ApiResult<()> {
        let store = self.presets_store()?;
        let p = store.load(name)?;
        self.check_direct_mode_off()?;
        self.connected_state()?;
        let r = preset::apply(self.session.device(), &p);
        // Some writes may have happened even on error.
        self.resync();
        r?;
        self.update_presets(&store, Some(Some(name.to_string())))
    }

    /// The profile file for export: the stored preset as pretty JSON (the
    /// same validation as a load). Works while disconnected.
    pub fn export_preset_json(&self, name: &str) -> ApiResult<String> {
        Ok(self.presets_store()?.load(name)?.to_json()?)
    }

    /// Save a profile file (`text`, JSON) under `name`. Everything is checked
    /// first (feature allow-list, ranges, name; an existing name is not
    /// replaced); nothing reaches the device, the profile is not applied and
    /// `activePreset` does not change. Works while disconnected.
    pub fn import_preset(&self, text: &str, name: &str) -> ApiResult<AppState> {
        let store = self.presets_store()?;
        let preset = Preset::from_json(text)?;
        if store.list()?.iter().any(|n| n.eq_ignore_ascii_case(name)) {
            return Err(ApiError::new(
                ErrorCode::PresetInvalidName,
                format!("A profile named \"{name}\" already exists."),
            ));
        }
        store.save(name, &preset)?;
        self.update_presets(&store, None)?;
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
        let active = {
            let mut inner = lock(&self.inner);
            // The rules follow the rename too (saved below or by update_presets).
            let renamed = inner
                .auto_switch
                .map(|n| Some(if n == from { to } else { n }.to_string()));
            if renamed {
                self.save_settings(&inner)?;
            }
            inner.active_preset.clone()
        };
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

    // --- Auto-switch ---------------------------------------------------------------

    /// Set (or clear, with `None`) the profile applied when the output becomes
    /// `output`. Only a setting: works while disconnected and in Direct Mode.
    pub fn set_auto_switch(&self, output: Output, profile: Option<String>) -> ApiResult<AppState> {
        if let Some(name) = &profile {
            self.presets_store()?.load(name)?;
        }
        {
            let mut inner = lock(&self.inner);
            inner.auto_switch.set(output, profile);
            self.save_settings(&inner)?;
        }
        Ok(self.announce())
    }

    /// The jack switched to `output`: apply its rule's profile with the same
    /// code as `apply_preset`. Returns whether it tried (the state may have
    /// changed). Skipped while Direct Mode is on. A rule whose profile is gone
    /// is cleared. Runs on the forwarder thread, so the events the apply
    /// causes queue up behind it (and are not output switches).
    fn auto_switch(&self, output: Output) -> bool {
        let Some(name) = lock(&self.inner)
            .auto_switch
            .rule(output)
            .map(str::to_string)
        else {
            return false;
        };
        let _op = lock(&self.ops);
        if lock(&self.inner).direct_mode {
            log::info!("auto-switch: Direct Mode is on, not applying \"{name}\"");
            return false;
        }
        // Another switch happened meanwhile: its own event decides.
        if self.session.state().output.map(Output::from) != Some(output) {
            log::info!("auto-switch: the output changed again, not applying \"{name}\"");
            return false;
        }
        let Err(mut e) = self.apply_preset_locked(&name) else {
            return true;
        };
        log::warn!("auto-switch: could not apply \"{name}\": {}", e.message);
        match e.code {
            ErrorCode::NotConnected => return false,
            ErrorCode::PresetNotFound => {
                let mut inner = lock(&self.inner);
                if inner.auto_switch.rule(output) == Some(name.as_str()) {
                    inner.auto_switch.set(output, None);
                    if let Err(e) = self.save_settings(&inner) {
                        log::warn!("cannot save app settings: {}", e.message);
                    }
                }
            }
            _ => {}
        }
        e.message = format!("Auto-switch could not apply \"{name}\": {}", e.message);
        self.sink.device_error(&e);
        true
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

    // --- User mic EQ curves ----------------------------------------------------------

    fn mic_curves_store(&self) -> ApiResult<MicEqCurveStore> {
        Ok(MicEqCurveStore::in_dir(
            &self.paths.as_ref().ok_or_else(no_paths)?.config_dir,
        ))
    }

    /// Saved mic EQ curves, re-read from disk (also refreshes the snapshot's list).
    pub fn list_mic_eq_curves(&self) -> ApiResult<Vec<MicEqCurve>> {
        let list = self.mic_curves_store()?.list()?;
        let changed = {
            let mut inner = lock(&self.inner);
            let changed = inner.mic_eq_curves != list;
            inner.mic_eq_curves = list.clone();
            changed
        };
        if changed {
            self.announce();
        }
        Ok(list)
    }

    /// Save the active output's current mic EQ bands under `name` (replaces a
    /// curve with the same name). Allowed in Direct Mode and Scout.
    pub fn save_mic_eq_curve(&self, name: &str) -> ApiResult<AppState> {
        let name = mic_eq_curves::validate_name(name)?;
        let store = self.mic_curves_store()?;
        let _op = lock(&self.ops);
        let state = self.connected_state()?;
        let mut bands = [0.0; 8];
        for (slot, n) in bands.iter_mut().zip(MIC_EQ_BANDS) {
            *slot = state.feature(feat(n)).ok_or_else(|| {
                ApiError::new(ErrorCode::NotConnected, "mic EQ values not read yet")
            })?;
        }
        store.save(&name, bands).map_err(ApiError::from)?;
        self.reload_mic_eq_curves(&store)?;
        Ok(self.announce())
    }

    /// Works while disconnected.
    pub fn delete_mic_eq_curve(&self, name: &str) -> ApiResult<AppState> {
        let store = self.mic_curves_store()?;
        let _op = lock(&self.ops);
        let r = store.delete(name);
        self.reload_mic_eq_curves(&store)?;
        r?;
        Ok(self.announce())
    }

    fn reload_mic_eq_curves(&self, store: &MicEqCurveStore) -> ApiResult<()> {
        let list = store.list()?;
        lock(&self.inner).mic_eq_curves = list;
        Ok(())
    }

    // --- App -------------------------------------------------------------------------

    /// Record the launch-at-login state (the autostart plugin is driven by
    /// the Tauri command).
    pub fn set_launch_at_login_flag(&self, enabled: bool) -> AppState {
        lock(&self.inner).launch_at_login = enabled;
        self.announce()
    }

    // --- Mixer -----------------------------------------------------------------------

    /// Re-read the mixer into the cache; true if it changed. Never writes.
    fn sync_mixer(&self) -> bool {
        let _g = lock(&self.mixer_ops);
        self.sync_mixer_locked()
    }

    fn sync_mixer_locked(&self) -> bool {
        let now = self.mixer.read();
        let mut inner = lock(&self.inner);
        if inner.mixer == now {
            return false;
        }
        inner.mixer = now;
        true
    }

    /// The OS said something may have changed (knob, another app, replug).
    fn refresh_mixer(&self) {
        if self.sync_mixer() {
            self.announce();
        }
    }

    /// Check against a fresh read, write, read back, announce a change.
    fn mixer_write(
        &self,
        check: impl FnOnce(&Mixer) -> ApiResult<()>,
        write: impl FnOnce(&dyn MixerBackend) -> Result<(), MixerError>,
    ) -> ApiResult<AppState> {
        let guard = lock(&self.mixer_ops);
        let now = self.mixer.read().ok_or_else(|| {
            ApiError::new(
                ErrorCode::MixerUnavailable,
                "the G6 audio device is not available",
            )
        })?;
        check(&now)?;
        let written = write(&*self.mixer);
        // Also after a failed write: it may have been partly applied.
        let changed = self.sync_mixer_locked();
        drop(guard);
        let snapshot = if changed {
            self.announce()
        } else {
            self.snapshot()
        };
        written?;
        Ok(snapshot)
    }

    /// `volume` 0..=1 (both channels). `invalidValue` outside; `mixerUnavailable` without such a control.
    pub fn set_mixer_volume(&self, scope: MixerScope, volume: f32) -> ApiResult<AppState> {
        if !volume.is_finite() || !(0.0..=1.0).contains(&volume) {
            return Err(ApiError::new(
                ErrorCode::InvalidValue,
                format!("volume {volume} is outside 0..1"),
            ));
        }
        self.mixer_write(
            |m| need_level(m, scope).map(|_| ()),
            |mx| mx.set_volume(scope, volume),
        )
    }

    pub fn set_mixer_mute(&self, scope: MixerScope, mute: bool) -> ApiResult<AppState> {
        self.mixer_write(
            |m| {
                need_level(m, scope)?
                    .mute
                    .map(|_| ())
                    .ok_or_else(|| ApiError::new(ErrorCode::MixerUnavailable, no_control(scope)))
            },
            |mx| mx.set_mute(scope, mute),
        )
    }

    /// `id` must be one of `mixer.recordingSource.options`.
    pub fn set_mixer_source(&self, id: u32) -> ApiResult<AppState> {
        self.mixer_write(
            |m| {
                let s = m.recording_source.as_ref().ok_or_else(|| {
                    ApiError::new(ErrorCode::MixerUnavailable, "no recording source selector")
                })?;
                if s.options.iter().any(|o| o.id == id) {
                    Ok(())
                } else {
                    Err(ApiError::new(
                        ErrorCode::InvalidValue,
                        format!("unknown recording source {id}"),
                    ))
                }
            },
            |mx| mx.set_source(id),
        )
    }

    // --- Tray menu and hotkeys ---------------------------------------------------------

    /// A device-related tray menu item was clicked. The same methods as the
    /// commands; a failure is a `device-error` (the window may be hidden, so
    /// it is logged too) and the snapshot is announced either way, so the menu
    /// shows what really happened.
    pub fn run_tray_action(&self, action: TrayAction) {
        let r = match action {
            TrayAction::DirectMode(on) => self.set_direct_mode(on),
            TrayAction::Sbx(on) => self.set_sbx(on),
            TrayAction::Scout(on) => self.set_scout(on),
            TrayAction::Output(o) => self.set_output(o),
            TrayAction::Lighting(on) => self.set_lighting_enabled(on),
            TrayAction::Profile(name) => self.apply_preset(&name),
        };
        self.finish_shell_action(r);
    }

    /// A global hotkey was pressed.
    pub fn run_hotkey(&self, action: HotkeyAction) {
        let r = match action {
            HotkeyAction::ToggleDirectMode => self.toggle_direct_mode(),
            HotkeyAction::NextProfile => self.apply_next_preset(),
            HotkeyAction::ToggleSbx => {
                let _op = lock(&self.ops);
                self.set_sbx_locked(None)
            }
        };
        self.finish_shell_action(r);
    }

    fn finish_shell_action(&self, r: ApiResult<AppState>) {
        match r {
            Ok(s) => self.sink.state_changed(&s),
            Err(e) => {
                log::warn!("menu / hotkey action failed: {}", e.message);
                self.sink.device_error(&e);
                self.announce();
            }
        }
    }

    /// Apply the profile after the active one (alphabetical, wrapping; the
    /// first one when none is active). A profile without a part for the
    /// active output is skipped, so repeated presses do not stick on it. The
    /// active profile is read under `ops`, so two quick presses move two on.
    pub fn apply_next_preset(&self) -> ApiResult<AppState> {
        let _op = lock(&self.ops);
        let (list, active) = {
            let inner = lock(&self.inner);
            (inner.presets.clone(), inner.active_preset.clone())
        };
        let mut last = ApiError::new(ErrorCode::PresetNotFound, "There are no profiles.");
        for name in next_presets(&list, active.as_deref()) {
            match self.apply_preset_locked(name) {
                Ok(()) => return Ok(self.announce()),
                Err(e) if e.code == ErrorCode::PresetNoProfile => last = e,
                Err(e) => return Err(e),
            }
        }
        Err(last)
    }

    /// The hotkey settings in use (validated).
    pub fn hotkey_settings(&self) -> HotkeySettings {
        lock(&self.inner).hotkeys.clone()
    }

    /// Validate and store new hotkey settings (an empty accelerator means the
    /// default). Registration failures of the previous settings are
    /// forgotten; the shell registers the new ones and reports with
    /// [`Self::set_hotkey_status`]. Rejects `invalidValue` (nothing stored) or
    /// `storage`.
    pub fn set_hotkeys(&self, new: HotkeySettings) -> ApiResult<HotkeySettings> {
        let valid = new
            .validated()
            .map_err(|m| ApiError::new(ErrorCode::InvalidValue, m))?;
        let mut inner = lock(&self.inner);
        let old = std::mem::replace(&mut inner.hotkeys, valid.clone());
        inner.hotkey_errors = Default::default();
        if let Err(e) = self.save_settings(&inner) {
            inner.hotkeys = old;
            return Err(e);
        }
        Ok(valid)
    }

    /// The shell registered the hotkeys: `errors` has the failure per
    /// hotkey ([`HotkeyAction::index`]). Announces the snapshot.
    pub fn set_hotkey_status(&self, errors: [Option<String>; 3]) -> AppState {
        lock(&self.inner).hotkey_errors = errors;
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
        // Auto-switch: a connect only records the output (no rule fires at
        // startup); a switch fires when it ends on another output than the
        // one known before the burst.
        // The session state, not the burst, says where we ended up.
        let state = self.session.state();
        let connected = state.connected;
        // Read, compute and write `last_output` under one lock, together with
        // the manual switch in flight (`set_output` changes both under one
        // lock): a `set_output` finishing meanwhile is never overwritten.
        let (from, to, manual) = {
            let mut inner = lock(&self.inner);
            let mut from = inner.last_output;
            let mut to = from;
            for ev in burst {
                match ev {
                    Event::Connected => {
                        connection = Some(Connection::Connected);
                        from = state.output.map(Output::from);
                        to = from;
                    }
                    Event::Disconnected => connection = Some(Connection::Disconnected),
                    Event::OutputSwitched(o) => {
                        output = Some(Output::from(*o));
                        to = output;
                    }
                    Event::Changed(_) => {}
                }
            }
            inner.last_output = to;
            (from, to, inner.manual_output)
        };
        let switched_to = if connected && from.is_some() && to != from && to != manual {
            to
        } else {
            None
        };
        if connection.is_some() {
            connection = Some(if connected {
                Connection::Connected
            } else {
                Connection::Disconnected
            });
        }
        if connection == Some(Connection::Connected) {
            {
                let mut inner = lock(&self.inner);
                inner.device_info = DeviceInfo::default();
                inner.connection_epoch += 1;
            }
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
        if switched_to.is_some_and(|o| self.auto_switch(o)) {
            self.announce();
        }
    }
}

fn no_control(scope: MixerScope) -> String {
    format!("the G6 offers no {scope:?} control of that kind")
}

fn need_level(m: &Mixer, scope: MixerScope) -> ApiResult<&crate::api::MixerLevel> {
    m.level(scope)
        .ok_or_else(|| ApiError::new(ErrorCode::MixerUnavailable, no_control(scope)))
}

/// Turns the mixer's change hints into re-reads (hints closer than
/// [`COALESCE`] are one, as with device events). Ends when the mixer drops
/// its `Notify`, or at the first hint after the backend is gone. The
/// CoreAudio mixer never drops it (CoreAudio keeps the pointer), so there
/// this thread lives until such a hint, at worst for the process.
fn run_mixer_watcher(backend: Weak<Backend>, hints: Receiver<()>) {
    while hints.recv().is_ok() {
        let start = Instant::now();
        while start.elapsed() < MAX_BURST && hints.recv_timeout(COALESCE).is_ok() {}
        let Some(b) = backend.upgrade() else { return };
        b.refresh_mixer();
    }
}

/// A device read that may fail without failing the caller: logs the error
/// (never a value) and gives `None`.
fn soft<T>(what: &str, r: Result<T, g6_core::events::Error>) -> Option<T> {
    r.map_err(|e| log::warn!("cannot read the {what}: {e}"))
        .ok()
}

/// Profiles to try for "next profile", in order: those after `active`,
/// wrapping round, then `active` itself (only reached when it is the sole
/// one, or all others failed). From the first one when `active` is `None` or
/// no longer listed.
fn next_presets<'a>(list: &'a [String], active: Option<&str>) -> Vec<&'a str> {
    let start = active
        .and_then(|a| list.iter().position(|n| n == a))
        .map_or(0, |i| i + 1);
    (0..list.len())
        .map(|k| list[(start + k) % list.len()].as_str())
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
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
    use g6_core::transport::fake::{FakeDevice, FAKE_FIRMWARE, FAKE_SERIAL, SCOUT_CURVE};
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
    #[allow(clippy::large_enum_variant)]
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

    /// A session that reads and writes no files (never the user's real
    /// `last_known.json`, which `SessionConfig::default()` would use).
    fn no_storage() -> SessionConfig {
        SessionConfig {
            paths: None,
            reapply_on_first_connect: false,
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

    /// Wait until the forwarder has seen the echo of every write made so far
    /// (no write is still overlaid on the snapshot), so the session state
    /// itself holds the written values.
    fn settle(r: &Rig) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !lock(&r.backend.inner).pending.0.is_empty() {
            assert!(Instant::now() < deadline, "write echoes did not arrive");
            std::thread::sleep(Duration::from_millis(5));
        }
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

    #[test]
    fn a_late_echo_of_an_older_write_keeps_the_newer_overlay() {
        let now = Instant::now();
        let overlaid = |p: &mut Pending| {
            let mut s = connected_state();
            p.apply(&mut s, now);
            s.feature(feat("eq_1k"))
        };
        let mut p = Pending::default();
        // A slider drag: 0 dB, then 6 dB, before either echo was handled.
        p.push(feature_change("eq_1k", 0.0), now);
        p.push(feature_change("eq_1k", 6.0), now);
        p.observe(&Event::Changed(feature_change("eq_1k", 0.0)));
        assert_eq!(
            overlaid(&mut p),
            Some(6.0),
            "the 0 dB echo is the older write's"
        );
        p.observe(&Event::Changed(feature_change("eq_1k", 6.0)));
        assert_eq!(overlaid(&mut p), None, "the 6 dB echo ends it");

        // A value no write set is a change made elsewhere: it ends the entry.
        p.push(Change::Mask(Mask(0x01)), now);
        p.push(Change::Mask(Mask(0x00)), now);
        p.observe(&Event::Changed(Change::Mask(Mask(0x01))));
        assert_eq!(p.0.len(), 1);
        p.observe(&Event::Changed(Change::Mask(Mask(0x02))));
        assert!(p.0.is_empty());
    }

    #[test]
    fn echoes_of_a_b_a_writes_keep_the_overlay_until_the_last() {
        let now = Instant::now();
        let overlaid = |p: &mut Pending| {
            let mut s = connected_state();
            p.apply(&mut s, now);
            s.feature(feat("eq_1k"))
        };
        let mut p = Pending::default();
        p.push(feature_change("eq_1k", 0.0), now);
        p.push(feature_change("eq_1k", 6.0), now);
        p.push(feature_change("eq_1k", 0.0), now);
        // First 0 dB echo: the 6 dB and the last 0 dB write are outstanding.
        p.observe(&Event::Changed(feature_change("eq_1k", 0.0)));
        assert_eq!(overlaid(&mut p), Some(0.0));
        p.observe(&Event::Changed(feature_change("eq_1k", 6.0)));
        assert_eq!(overlaid(&mut p), Some(0.0), "the last write is outstanding");
        p.observe(&Event::Changed(feature_change("eq_1k", 0.0)));
        assert_eq!(overlaid(&mut p), None);
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

    /// Frames of command `c` the fake received so far.
    fn sent(fake: &FakeDevice, c: u8) -> usize {
        fake.written().iter().filter(|f| f[1] == c).count()
    }

    fn wait_connected_event(r: &Rig) {
        r.rec.wait(
            "connection-changed",
            |o| matches!(o, Out::Conn(c) if c.connection == Connection::Connected),
        );
    }

    #[test]
    fn device_info_is_read_once_per_connection() {
        let r = rig();
        wait_connected_event(&r);
        let info = r.backend.device_info();
        assert_eq!(
            info,
            DeviceInfo {
                firmware: Some(FAKE_FIRMWARE.into()),
                serial: Some(hex(&FAKE_SERIAL)),
                dsp_version: Some("30011000".into()),
                hardware_id: Some("97000000".into()),
            }
        );
        assert_eq!(info.serial.as_deref(), Some("ef67740000000000"));
        assert_eq!(r.backend.device_info(), info);
        assert_eq!(sent(&r.fake, core::cmd::SERIAL), 1, "second call is cached");
        assert_eq!(sent(&r.fake, core::cmd::DSP_VERSION), 1);
        assert_eq!(sent(&r.fake, core::cmd::HW_ID), 1);

        // A new connection reads again.
        r.rec.take();
        r.fake.unplug();
        r.rec
            .wait_state("disconnected", |s| s.connection == Connection::Disconnected);
        assert_eq!(
            r.backend.device_info(),
            info,
            "cache is served while disconnected"
        );
        assert_eq!(
            sent(&r.fake, core::cmd::SERIAL),
            1,
            "no device access while disconnected"
        );
        r.fake.replug();
        wait_connected_event(&r);
        assert_eq!(r.backend.device_info(), info);
        assert_eq!(sent(&r.fake, core::cmd::SERIAL), 2);
    }

    #[test]
    fn device_info_tolerates_a_failed_read_and_retries_it() {
        let r = rig();
        wait_connected_event(&r);
        r.fake.drop_replies(core::cmd::SERIAL, 1);
        let info = r.backend.device_info();
        assert_eq!(info.serial, None);
        assert_eq!(info.firmware.as_deref(), Some(FAKE_FIRMWARE));
        assert_eq!(info.dsp_version.as_deref(), Some("30011000"));
        assert_eq!(info.hardware_id.as_deref(), Some("97000000"));
        let info = r.backend.device_info();
        assert_eq!(info.serial, Some(hex(&FAKE_SERIAL)));
        assert_eq!(sent(&r.fake, core::cmd::SERIAL), 2);
        assert_eq!(
            sent(&r.fake, core::cmd::DSP_VERSION),
            1,
            "good fields stay cached"
        );
    }

    #[test]
    fn device_info_before_any_connection_is_empty() {
        let fake = FakeDevice::new();
        fake.fail_next_connects(1000);
        let (session, _events) = Session::start(Box::new(fake.connector()), fast(), no_storage());
        let b = Backend::new(session, None, false, Arc::new(Recorder::default()));
        assert_eq!(b.device_info(), DeviceInfo::default());
        assert_eq!(sent(&fake, core::cmd::SERIAL), 0);
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
    fn lighting_writes_are_read_back_and_validated() {
        let r = rig();
        let s = r.backend.set_lighting_color("ff8000").unwrap();
        assert_eq!(s.lighting.color, "ff8000");
        let s = r.backend.set_lighting_enabled(false).unwrap();
        assert!(!s.lighting.enabled);
        assert_eq!(s.lighting.color, "ff8000");
        let s = r.backend.set_lighting_enabled(true).unwrap();
        assert!(s.lighting.enabled);
        // The session learns the values and later snapshots agree.
        r.rec.wait_state("lighting", |s| {
            s.lighting.enabled && s.lighting.color == "ff8000"
        });
        let st = r.backend.session.state();
        assert_eq!(st.lighting_color, Some([255, 128, 0]));
        assert_eq!(st.lighting_enabled, Some(true));

        let before = r.fake.written().len();
        for bad in ["", "ff80", "gg0000", "ff80000", "red"] {
            assert_eq!(
                r.backend.set_lighting_color(bad).unwrap_err().code,
                ErrorCode::InvalidValue,
                "{bad:?}"
            );
        }
        assert_eq!(r.fake.written().len(), before, "nothing was sent");
    }

    #[test]
    fn set_output_switches_the_output_and_its_profile() {
        let r = rig();
        let s = r.backend.set_output(Output::Speakers).unwrap();
        assert_eq!(s.output, Some(Output::Speakers));
        assert_eq!(r.fake.output(), core::Output::Speakers);
        r.rec
            .wait_state("speakers", |s| s.output == Some(Output::Speakers));
        let s = r.backend.set_output(Output::Headphones).unwrap();
        assert_eq!(s.output, Some(Output::Headphones));
        assert_eq!(r.fake.output(), core::Output::Headphones);
    }

    #[test]
    fn lighting_and_output_writes_need_a_connection() {
        let r = rig();
        r.fake.unplug();
        r.rec
            .wait_state("lost", |s| s.connection == Connection::Disconnected);
        let code = |res: ApiResult<AppState>| res.unwrap_err().code;
        assert_eq!(
            code(r.backend.set_lighting_enabled(true)),
            ErrorCode::NotConnected
        );
        assert_eq!(
            code(r.backend.set_lighting_color("ffffff")),
            ErrorCode::NotConnected
        );
        assert_eq!(
            code(r.backend.set_output(Output::Speakers)),
            ErrorCode::NotConnected
        );
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
    fn preamp_follows_the_device_through_scout_and_stays_writable() {
        let r = rig();
        let s = r.backend.set_eq_preamp(-2.5).unwrap();
        assert_eq!(s.eq.preamp, -2.5);
        assert_eq!(fake_feature(&r.fake, "eq_preamp"), -2.5);
        r.rec.wait_state("echo", |s| s.eq.preamp == -2.5);

        // The device sets the pre-amp to 0 dB when Scout turns on and gives the
        // old value back when it turns off; the snapshot follows the device.
        r.backend.set_scout(true).unwrap();
        r.rec
            .wait_state("scout pre-amp", |s| s.scout && s.eq.preamp == 0.0);
        assert_eq!(fake_feature(&r.fake, "eq_preamp"), 0.0);

        // Not locked while Scout is on. (The fake accepts the write; that the
        // G6 does is not verified.)
        let s = r.backend.set_eq_preamp(3.0).unwrap();
        assert_eq!(s.eq.preamp, 3.0);
        r.rec.wait_state("echo", |s| s.eq.preamp == 3.0);

        let before = r.fake.written().len();
        let code = |res: ApiResult<AppState>| res.unwrap_err().code;
        assert_eq!(code(r.backend.set_eq_preamp(6.5)), ErrorCode::InvalidValue);
        assert_eq!(code(r.backend.set_eq_preamp(-6.5)), ErrorCode::InvalidValue);
        assert_eq!(r.fake.written().len(), before, "nothing was sent");
    }

    #[test]
    fn preamp_before_scout_is_restored_when_scout_turns_off() {
        let r = rig();
        r.backend.set_eq_preamp(-5.5).unwrap();
        r.rec.wait_state("echo", |s| s.eq.preamp == -5.5);
        r.backend.set_scout(true).unwrap();
        r.rec
            .wait_state("scout pre-amp", |s| s.scout && s.eq.preamp == 0.0);
        r.backend.set_scout(false).unwrap();
        let s = r
            .rec
            .wait_state("restored", |s| !s.scout && s.eq.preamp == -5.5);
        assert_eq!(s.eq.preamp, -5.5);
        assert_eq!(fake_feature(&r.fake, "eq_preamp"), -5.5);
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
        settle(&r);
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

    const MIC_PRESET_1: [f32; 8] = [-3.0, -4.0, 0.0, 2.0, 3.0, -3.0, 4.0, 5.0];

    #[test]
    fn apply_mic_eq_curve_writes_8_bands_turns_mic_eq_on_and_ignores_direct_mode_and_scout() {
        let r = rig();
        r.backend.set_direct_mode(true).unwrap();
        let s = r.backend.apply_mic_eq_curve(&MIC_PRESET_1).unwrap();
        assert_eq!(s.mic.eq_bands, MIC_PRESET_1);
        assert!(s.mic.eq_enabled);
        for (n, g) in MIC_EQ_BANDS.iter().zip(MIC_PRESET_1) {
            assert_eq!(fake_feature(&r.fake, n), g, "{n}");
        }
        assert_eq!(fake_feature(&r.fake, "mic_eq"), 1.0);
        r.rec.wait_state("echo", |s| s.mic.eq_bands == MIC_PRESET_1);
        // The playback EQ was left alone.
        assert_eq!(r.backend.snapshot().eq.bands, [0.0; 10]);

        r.backend.set_direct_mode(false).unwrap();
        r.backend.set_scout(true).unwrap();
        assert!(r.backend.apply_mic_eq_curve(&[0.0; 8]).is_ok());
    }

    #[test]
    fn apply_mic_eq_curve_validates_before_writing() {
        let r = rig();
        let before = r.fake.written().len();
        let code = |res: ApiResult<AppState>| res.unwrap_err().code;
        assert_eq!(
            code(r.backend.apply_mic_eq_curve(&[0.0; 10])),
            ErrorCode::InvalidValue
        );
        let mut bad = [0.0; 8];
        bad[7] = 12.5;
        assert_eq!(
            code(r.backend.apply_mic_eq_curve(&bad)),
            ErrorCode::InvalidValue
        );
        bad[7] = f32::NAN;
        assert_eq!(
            code(r.backend.apply_mic_eq_curve(&bad)),
            ErrorCode::InvalidValue
        );
        assert_eq!(
            code(r.backend.set_mic_eq_band(8, 0.0)),
            ErrorCode::InvalidValue
        );
        assert_eq!(r.fake.written().len(), before, "nothing was sent");

        let s = r.backend.set_mic_eq_band(7, -2.5).unwrap();
        assert_eq!(s.mic.eq_bands[7], -2.5);
        assert_eq!(fake_feature(&r.fake, "mic_eq_8"), -2.5);
    }

    #[test]
    fn mic_eq_curves_save_list_delete_and_persist() {
        let dir = TempDir::new();
        let root = dir.0.clone();
        let r = rig_with(FakeDevice::new(), dir);
        assert!(r.backend.snapshot().mic_eq_curves.is_empty());

        r.backend.apply_mic_eq_curve(&MIC_PRESET_1).unwrap();
        let s = r.backend.save_mic_eq_curve("  Podcast ").unwrap();
        assert_eq!(
            s.mic_eq_curves,
            [MicEqCurve {
                name: "Podcast".into(),
                bands: MIC_PRESET_1
            }]
        );
        assert!(root.join("config").join(mic_eq_curves::FILE).exists());
        assert!(
            !root.join("config").join(eq_curves::FILE).exists(),
            "separate from the playback curves"
        );
        r.rec
            .wait_state("curve announced", |s| s.mic_eq_curves.len() == 1);
        assert_eq!(r.backend.list_mic_eq_curves().unwrap(), s.mic_eq_curves);

        assert_eq!(
            code_of(r.backend.save_mic_eq_curve("preset 3")),
            ErrorCode::InvalidValue
        );
        assert_eq!(
            code_of(r.backend.delete_mic_eq_curve("Nope")),
            ErrorCode::CurveNotFound
        );
        assert_eq!(r.backend.snapshot().mic_eq_curves.len(), 1);

        // Delete works while disconnected.
        r.fake.unplug();
        r.rec
            .wait_state("disconnected", |s| s.connection == Connection::Disconnected);
        assert!(r
            .backend
            .delete_mic_eq_curve("podcast")
            .unwrap()
            .mic_eq_curves
            .is_empty());
        assert_eq!(
            code_of(r.backend.save_mic_eq_curve("A")),
            ErrorCode::NotConnected
        );
    }

    fn code_of(res: ApiResult<AppState>) -> ErrorCode {
        res.unwrap_err().code
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
    fn direct_mode_noop_disable_announces_a_stale_flag() {
        let r = rig();
        r.backend.set_direct_mode(true).unwrap();
        // The Direct Mode file disappears behind the backend's back.
        std::fs::remove_file(Paths::under(&r._dir.0).direct_file()).unwrap();
        r.rec.take();
        let s = r.backend.set_direct_mode(false).unwrap();
        assert!(!s.direct_mode);
        r.rec
            .wait_state("directMode false announced", |s| !s.direct_mode);
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

    /// The forwarder decides from the Direct Mode file under `ops`, never from
    /// a flag it read before a `set_direct_mode` that ran meanwhile (here the
    /// stale flag is set by hand).
    #[test]
    fn following_the_jack_never_undoes_a_direct_mode_toggle() {
        let r = rig();
        let dm = DirectMode::new(Paths::under(&r._dir.0).direct_file());
        // Turned off, but the forwarder still believes it is on.
        r.backend.set_eq_enabled(true).unwrap();
        r.backend.set_direct_mode(true).unwrap();
        r.backend.set_direct_mode(false).unwrap();
        assert_eq!(fake_feature(&r.fake, "eq"), 1.0);
        lock(&r.backend.inner).direct_mode = true;
        r.backend.follow_direct_mode();
        assert!(!dm.is_active().unwrap(), "not switched on again");
        assert_eq!(fake_feature(&r.fake, "eq"), 1.0);

        // Turned on, but the forwarder still believes it is off.
        r.backend.set_direct_mode(true).unwrap();
        assert_eq!(fake_feature(&r.fake, "eq"), 0.0);
        lock(&r.backend.inner).direct_mode = false;
        r.backend.follow_direct_mode();
        assert!(dm.is_active().unwrap(), "not switched off again");
        assert_eq!(fake_feature(&r.fake, "eq"), 0.0);
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
    fn profile_export_and_import() {
        let r = rig();
        r.backend.set_eq_band(2, 1.5).unwrap();
        r.backend.save_preset("Music").unwrap();
        let json = r.backend.export_preset_json("Music").unwrap();
        let file: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(file["headphones"]["eq_125"], 1.5);
        assert_eq!(
            r.backend.export_preset_json("Nope").unwrap_err().code,
            ErrorCode::PresetNotFound
        );

        // Import saves a new profile without applying it or making it active.
        let before = r.fake.written().len();
        r.backend.set_eq_band(2, -3.0).unwrap();
        let writes = r.fake.written().len();
        assert!(writes > before);
        let s = r.backend.import_preset(&json, "Copy").unwrap();
        assert_eq!(s.presets, ["Copy", "Music"]);
        assert_eq!(s.active_preset.as_deref(), Some("Music"));
        assert_eq!(r.fake.written().len(), writes, "nothing sent to the device");
        assert_eq!(s.eq.bands[2], -3.0);
        r.backend.apply_preset("Copy").unwrap();
        assert_eq!(fake_feature(&r.fake, "eq_125"), 1.5);
    }

    #[test]
    fn profile_import_validates_before_saving_anything() {
        let r = rig();
        r.backend.save_preset("Music").unwrap();
        let code = |text: &str, name: &str| r.backend.import_preset(text, name).unwrap_err().code;
        assert_eq!(
            code(r#"{"headphones":{"eq_31":99}}"#, "Bad"),
            ErrorCode::InvalidValue
        );
        assert_eq!(
            code(r#"{"headphones":{"mic_aec":1}}"#, "Bad"),
            ErrorCode::InvalidValue
        );
        assert_eq!(code("[]", "Bad"), ErrorCode::InvalidValue);
        let ok = r#"{"headphones":{"bass":1}}"#;
        assert_eq!(code(ok, "../x"), ErrorCode::PresetInvalidName);
        assert_eq!(code(ok, "music"), ErrorCode::PresetInvalidName, "exists");
        assert_eq!(r.backend.list_presets().unwrap(), ["Music"]);
        let on_disk = std::fs::read_dir(PresetStore::from_paths(&Paths::under(&r._dir.0)).dir())
            .unwrap()
            .count();
        assert_eq!(on_disk, 1, "no file left behind");
    }

    #[test]
    fn profile_import_works_while_disconnected() {
        let r = rig();
        r.fake.unplug();
        r.rec
            .wait_state("disconnected", |s| s.connection == Connection::Disconnected);
        let s = r
            .backend
            .import_preset(r#"{"speakers":{"bass":1}}"#, "Offline")
            .unwrap();
        assert_eq!(s.presets, ["Offline"]);
        assert!(r
            .backend
            .export_preset_json("Offline")
            .unwrap()
            .contains("\"speakers\""));
    }

    // --- Auto-switch -----------------------------------------------------------------

    /// A profile for each output that turns Bass on (the fake starts with it off).
    fn bass_preset(dir: &TempDir, name: &str, headphones: bool, speakers: bool) {
        let profile = || preset::PresetProfile {
            features: [("bass".to_string(), 1.0)].into(),
            ..Default::default()
        };
        let p = preset::Preset {
            headphones: headphones.then(profile),
            speakers: speakers.then(profile),
        };
        PresetStore::from_paths(&Paths::under(&dir.0))
            .save(name, &p)
            .unwrap();
    }

    fn write_rules(dir: &TempDir, rules: AutoSwitch) {
        let p = settings_file(&Paths::under(&dir.0));
        store::write_json(
            &p,
            &Settings {
                active_preset: None,
                auto_switch: rules,
                ..Settings::default()
            },
        )
        .unwrap();
    }

    fn rules(h: Option<&str>, s: Option<&str>) -> AutoSwitch {
        AutoSwitch {
            headphones: h.map(Into::into),
            speakers: s.map(Into::into),
        }
    }

    /// Wait until the jack switch has been handled (its `output-changed` and
    /// the state after it).
    fn wait_switched(r: &Rig, o: Output) {
        r.rec.wait("output-changed", |e| {
            *e == Out::Output(OutputChanged { output: o })
        });
        r.rec.wait_state("output", |s| s.output == Some(o));
    }

    #[test]
    fn auto_switch_applies_the_rule_on_a_jack_switch() {
        let dir = TempDir::new();
        bass_preset(&dir, "Spk", false, true);
        bass_preset(&dir, "Hp", true, false);
        let r = rig_with(FakeDevice::new(), dir);
        let s = r
            .backend
            .set_auto_switch(Output::Speakers, Some("Spk".into()))
            .unwrap();
        assert_eq!(s.auto_switch, rules(None, Some("Spk")));
        let s = r
            .backend
            .set_auto_switch(Output::Headphones, Some("Hp".into()))
            .unwrap();
        assert_eq!(s.auto_switch, rules(Some("Hp"), Some("Spk")));
        assert_eq!(s.active_preset, None, "setting a rule applies nothing");

        r.rec.take();
        r.fake.switch_output(core::Output::Speakers);
        r.rec
            .wait_state("Spk applied", |s| s.active_preset.as_deref() == Some("Spk"));
        assert_eq!(fake_feature(&r.fake, "bass"), 1.0);
        assert!(r.backend.snapshot().effects.bass.enabled);

        // Back to headphones: its rule applies the headphones profile.
        r.fake.switch_output(core::Output::Headphones);
        r.rec
            .wait_state("Hp applied", |s| s.active_preset.as_deref() == Some("Hp"));
        assert_eq!(fake_feature(&r.fake, "bass"), 1.0);
    }

    #[test]
    fn auto_switch_does_not_fire_at_startup_or_for_the_same_output() {
        let dir = TempDir::new();
        bass_preset(&dir, "Hp", true, false);
        bass_preset(&dir, "Spk", false, true);
        write_rules(&dir, rules(Some("Hp"), Some("Spk")));
        // The fake starts on headphones, which has a rule.
        let r = rig_with(FakeDevice::new(), dir);
        std::thread::sleep(Duration::from_millis(150));
        let s = r.backend.snapshot();
        assert_eq!(s.output, Some(Output::Headphones));
        assert_eq!(s.active_preset, None);
        assert_eq!(fake_feature(&r.fake, "bass"), 0.0);

        // A push that repeats the current output is not a switch.
        r.rec.take();
        r.fake.switch_output(core::Output::Headphones);
        wait_switched(&r, Output::Headphones);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(r.backend.snapshot().active_preset, None);

        // A real switch fires once; repeating it later does not apply again.
        r.fake.switch_output(core::Output::Speakers);
        r.rec
            .wait_state("Spk applied", |s| s.active_preset.as_deref() == Some("Spk"));
        r.backend.save_preset("Other").unwrap();
        r.rec.take();
        r.fake.switch_output(core::Output::Speakers);
        wait_switched(&r, Output::Speakers);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(r.backend.snapshot().active_preset.as_deref(), Some("Other"));
    }

    #[test]
    fn auto_switch_does_not_fire_for_a_manual_output_switch() {
        let dir = TempDir::new();
        bass_preset(&dir, "Spk", false, true);
        let r = rig_with(FakeDevice::new(), dir);
        r.backend
            .set_auto_switch(Output::Speakers, Some("Spk".into()))
            .unwrap();
        let s = r.backend.set_output(Output::Speakers).unwrap();
        assert_eq!(s.output, Some(Output::Speakers));
        std::thread::sleep(Duration::from_millis(150));
        let s = r.backend.snapshot();
        assert_eq!(s.active_preset, None, "no profile was applied");
        // The jack switching away and back is a real switch again.
        r.rec.take();
        r.fake.switch_output(core::Output::Headphones);
        wait_switched(&r, Output::Headphones);
        r.fake.switch_output(core::Output::Speakers);
        r.rec
            .wait_state("Spk applied", |s| s.active_preset.as_deref() == Some("Spk"));
    }

    #[test]
    fn auto_switch_does_not_fire_for_a_manual_switch_during_a_slow_resync() {
        let dir = TempDir::new();
        bass_preset(&dir, "Spk", false, true);
        let r = rig_with(FakeDevice::new(), dir);
        r.backend
            .set_auto_switch(Output::Speakers, Some("Spk".into()))
            .unwrap();
        // A busy link: the re-read after the switch (~70 reads) takes far
        // longer than the forwarder's coalescing window, so it handles the
        // device's push while `set_output` is still running.
        r.fake.set_write_delay(Duration::from_millis(5));
        let s = r.backend.set_output(Output::Speakers).unwrap();
        r.fake.set_write_delay(Duration::ZERO);
        assert_eq!(s.output, Some(Output::Speakers));
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            r.backend.snapshot().active_preset,
            None,
            "no profile was applied"
        );
        assert_eq!(fake_feature(&r.fake, "bass"), 0.0);
    }

    #[test]
    fn a_burst_without_an_output_event_keeps_the_known_output() {
        let r = rig();
        r.backend.set_output(Output::Speakers).unwrap();
        assert_eq!(
            lock(&r.backend.inner).last_output,
            Some(Output::Speakers),
            "set_output records the output it wrote"
        );
        wait_switched(&r, Output::Speakers);
        // Unrelated echoes do not touch it.
        lock(&r.backend.inner).last_output = Some(Output::Headphones);
        r.backend
            .handle_burst(&[Event::Changed(feature_change("eq_1k", 1.0))]);
        assert_eq!(lock(&r.backend.inner).last_output, Some(Output::Headphones));
    }

    #[test]
    fn auto_switch_is_skipped_in_direct_mode() {
        let dir = TempDir::new();
        bass_preset(&dir, "Spk", false, true);
        let r = rig_with(FakeDevice::new(), dir);
        r.backend
            .set_auto_switch(Output::Speakers, Some("Spk".into()))
            .unwrap();
        r.backend.set_direct_mode(true).unwrap();
        r.rec.take();
        r.fake.switch_output(core::Output::Speakers);
        wait_switched(&r, Output::Speakers);
        std::thread::sleep(Duration::from_millis(100));
        let s = r.backend.snapshot();
        assert!(s.direct_mode);
        assert_eq!(s.active_preset, None);
        assert_eq!(fake_feature(&r.fake, "bass"), 0.0);
        assert!(!r.rec.take().iter().any(|o| matches!(o, Out::Error(_))));
    }

    #[test]
    fn auto_switch_rules_follow_rename_and_clear_on_delete() {
        let r = rig();
        r.backend.save_preset("Music").unwrap();
        r.backend.save_preset("Games").unwrap();
        let code = |res: ApiResult<AppState>| res.unwrap_err().code;
        assert_eq!(
            code(
                r.backend
                    .set_auto_switch(Output::Speakers, Some("Nope".into()))
            ),
            ErrorCode::PresetNotFound
        );
        assert_eq!(
            code(
                r.backend
                    .set_auto_switch(Output::Speakers, Some("../x".into()))
            ),
            ErrorCode::PresetInvalidName
        );
        r.backend
            .set_auto_switch(Output::Headphones, Some("Music".into()))
            .unwrap();
        r.backend
            .set_auto_switch(Output::Speakers, Some("Games".into()))
            .unwrap();

        let s = r.backend.rename_preset("Music", "Tunes").unwrap();
        assert_eq!(s.auto_switch, rules(Some("Tunes"), Some("Games")));
        let s = r.backend.delete_preset("Games").unwrap();
        assert_eq!(s.auto_switch, rules(Some("Tunes"), None));
        let s = r.backend.set_auto_switch(Output::Headphones, None).unwrap();
        assert_eq!(s.auto_switch, AutoSwitch::default());

        // Saved: a restart sees the same rules.
        r.backend
            .set_auto_switch(Output::Speakers, Some("Tunes".into()))
            .unwrap();
        let Rig { backend, _dir, .. } = r;
        let weak = Arc::downgrade(&backend);
        drop(backend);
        wait_dropped(&weak);
        let r = rig_with(FakeDevice::new(), _dir);
        assert_eq!(r.backend.snapshot().auto_switch, rules(None, Some("Tunes")));
    }

    #[test]
    fn old_settings_files_without_rules_load() {
        let dir = TempDir::new();
        let p = settings_file(&Paths::under(&dir.0));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, r#"{"activePreset":"Old"}"#).unwrap();
        let r = rig_with(FakeDevice::new(), dir);
        let s = r.backend.snapshot();
        assert_eq!(s.active_preset.as_deref(), Some("Old"));
        assert_eq!(s.auto_switch, AutoSwitch::default());
    }

    #[test]
    fn auto_switch_to_a_missing_or_unusable_profile_reports_and_clears_it() {
        let dir = TempDir::new();
        // "Hp" only has a headphones profile, so it cannot apply to speakers.
        bass_preset(&dir, "Hp", true, false);
        write_rules(&dir, rules(None, Some("Ghost")));
        let r = rig_with(FakeDevice::new(), dir);
        r.rec.take();
        r.fake.switch_output(core::Output::Speakers);
        let Out::Error(e) = r.rec.wait("error", |o| matches!(o, Out::Error(_))) else {
            unreachable!()
        };
        assert_eq!(e.code, ErrorCode::PresetNotFound);
        r.rec
            .wait_state("rule cleared", |s| s.auto_switch == AutoSwitch::default());

        r.backend
            .set_auto_switch(Output::Speakers, Some("Hp".into()))
            .unwrap();
        r.rec.take();
        r.fake.switch_output(core::Output::Headphones);
        wait_switched(&r, Output::Headphones);
        r.fake.switch_output(core::Output::Speakers);
        let Out::Error(e) = r.rec.wait("error", |o| matches!(o, Out::Error(_))) else {
            unreachable!()
        };
        assert_eq!(e.code, ErrorCode::PresetNoProfile);
        assert_eq!(r.backend.snapshot().auto_switch, rules(None, Some("Hp")));
    }

    // --- Tray menu actions and hotkeys -------------------------------------------------

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn next_presets_cycles_after_the_active_one() {
        let l = names(&["A", "B", "C"]);
        assert_eq!(next_presets(&l, Some("A")), ["B", "C", "A"]);
        assert_eq!(next_presets(&l, Some("C")), ["A", "B", "C"]);
        assert_eq!(next_presets(&l, None), ["A", "B", "C"]);
        assert_eq!(next_presets(&l, Some("Gone")), ["A", "B", "C"]);
        assert_eq!(next_presets(&names(&["Solo"]), Some("Solo")), ["Solo"]);
        assert!(next_presets(&[], None).is_empty());
    }

    fn rig_with_presets(list: &[(&str, bool, bool)]) -> Rig {
        let dir = TempDir::new();
        for (name, hp, sp) in list {
            bass_preset(&dir, name, *hp, *sp);
        }
        rig_with(FakeDevice::new(), dir)
    }

    #[test]
    fn next_profile_cycles_and_wraps() {
        let r = rig_with_presets(&[("A", true, true), ("B", true, true), ("C", true, true)]);
        let seen: Vec<_> = (0..4)
            .map(|_| {
                r.backend
                    .apply_next_preset()
                    .unwrap()
                    .active_preset
                    .unwrap()
            })
            .collect();
        assert_eq!(seen, ["A", "B", "C", "A"]);
    }

    #[test]
    fn next_profile_skips_profiles_without_a_part_for_this_output() {
        // The fake starts on headphones; "B" is speakers-only.
        let r = rig_with_presets(&[("A", true, true), ("B", false, true), ("C", true, false)]);
        r.backend.apply_preset("A").unwrap();
        assert_eq!(
            r.backend
                .apply_next_preset()
                .unwrap()
                .active_preset
                .as_deref(),
            Some("C")
        );
        assert_eq!(
            r.backend
                .apply_next_preset()
                .unwrap()
                .active_preset
                .as_deref(),
            Some("A")
        );
        // Only profiles for the other output: the error names the problem.
        let only_sp = rig_with_presets(&[("S", false, true)]);
        assert_eq!(
            code_of(only_sp.backend.apply_next_preset()),
            ErrorCode::PresetNoProfile
        );
    }

    #[test]
    fn next_profile_reports_no_profiles_and_direct_mode() {
        let none = rig();
        assert_eq!(
            code_of(none.backend.apply_next_preset()),
            ErrorCode::PresetNotFound
        );
        let r = rig_with_presets(&[("A", true, true), ("B", true, true)]);
        r.backend.set_direct_mode(true).unwrap();
        assert_eq!(
            code_of(r.backend.apply_next_preset()),
            ErrorCode::DirectModeActive
        );
    }

    #[test]
    fn hotkeys_are_off_by_default_and_stored_in_the_settings_file() {
        let r = rig();
        let h = r.backend.snapshot().hotkeys;
        assert!(!h.enabled);
        assert_eq!(h.toggle_sbx.error, None);
        assert_eq!(
            h.toggle_direct_mode.accelerator,
            HotkeyAction::ToggleDirectMode.default_accelerator()
        );

        let stored = r
            .backend
            .set_hotkeys(HotkeySettings {
                enabled: true,
                toggle_direct_mode: "alt+ctrl+x".into(),
                next_profile: String::new(),
                toggle_sbx: "Ctrl+Alt+Y".into(),
            })
            .unwrap();
        assert_eq!(stored.toggle_direct_mode, "Ctrl+Alt+X");
        assert_eq!(r.backend.hotkey_settings(), stored);
        let file: Settings = store::read_json(&settings_file(&Paths::under(&r._dir.0)))
            .unwrap()
            .unwrap();
        assert_eq!(file.hotkeys, stored);
        // The other settings in the file are kept.
        assert_eq!(file.auto_switch, AutoSwitch::default());
    }

    #[test]
    fn invalid_hotkeys_are_rejected_and_nothing_changes() {
        let r = rig();
        let before = r.backend.hotkey_settings();
        for bad in [
            HotkeySettings {
                toggle_sbx: "S".into(),
                ..before.clone()
            },
            HotkeySettings {
                toggle_sbx: "Ctrl+Alt+Nope".into(),
                ..before.clone()
            },
            HotkeySettings {
                toggle_direct_mode: before.toggle_sbx.clone(),
                ..before.clone()
            },
        ] {
            assert_eq!(
                r.backend.set_hotkeys(bad).unwrap_err().code,
                ErrorCode::InvalidValue
            );
        }
        assert_eq!(r.backend.hotkey_settings(), before);
        assert!(!settings_file(&Paths::under(&r._dir.0)).exists());
    }

    #[test]
    fn hotkey_registration_errors_show_in_the_snapshot_until_the_settings_change() {
        let r = rig();
        let s = r
            .backend
            .set_hotkey_status([None, Some("taken".into()), None]);
        assert_eq!(s.hotkeys.next_profile.error.as_deref(), Some("taken"));
        assert_eq!(s.hotkeys.toggle_sbx.error, None);
        assert_eq!(r.backend.snapshot().hotkeys, s.hotkeys);
        // Announced to the UI.
        r.rec.wait_state("registration error", |s| {
            s.hotkeys.next_profile.error.is_some()
        });
        r.backend.set_hotkeys(r.backend.hotkey_settings()).unwrap();
        assert_eq!(r.backend.snapshot().hotkeys.next_profile.error, None);
    }

    #[test]
    fn hotkey_settings_of_older_or_broken_files_load() {
        let dir = TempDir::new();
        let p = settings_file(&Paths::under(&dir.0));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(
            &p,
            r#"{"hotkeys":{"enabled":true,"toggleSbx":"Ctrl+Alt+K","nextProfile":"oops"}}"#,
        )
        .unwrap();
        let r = rig_with(FakeDevice::new(), dir);
        let h = r.backend.hotkey_settings();
        // A broken accelerator falls back to its default; the valid one and
        // `enabled` are kept.
        assert!(h.enabled);
        assert_eq!(h.toggle_sbx, "Ctrl+Alt+K");
        assert_eq!(
            h.next_profile,
            HotkeyAction::NextProfile.default_accelerator()
        );
        let old = TempDir::new();
        let p = settings_file(&Paths::under(&old.0));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, r#"{"activePreset":"Old"}"#).unwrap();
        let r = rig_with(FakeDevice::new(), old);
        assert_eq!(r.backend.hotkey_settings(), HotkeySettings::default());
    }

    #[test]
    fn hotkeys_toggle_sbx_and_direct_mode() {
        let r = rig();
        r.backend.set_sbx(false).unwrap();
        r.backend.run_hotkey(HotkeyAction::ToggleSbx);
        assert!(r.backend.snapshot().sbx);
        r.backend.run_hotkey(HotkeyAction::ToggleSbx);
        assert!(!r.backend.snapshot().sbx);

        r.backend.run_hotkey(HotkeyAction::ToggleDirectMode);
        assert!(r.backend.snapshot().direct_mode);
        // SBX belongs to Direct Mode now: the press is reported, not applied.
        r.rec.take();
        r.backend.run_hotkey(HotkeyAction::ToggleSbx);
        let out = r.rec.take();
        assert!(out
            .iter()
            .any(|o| matches!(o, Out::Error(e) if e.code == ErrorCode::DirectModeActive)));
        r.backend.run_hotkey(HotkeyAction::ToggleDirectMode);
        assert!(!r.backend.snapshot().direct_mode);
    }

    #[test]
    fn hotkey_next_profile_applies_one() {
        let r = rig_with_presets(&[("A", true, true), ("B", true, true)]);
        r.backend.run_hotkey(HotkeyAction::NextProfile);
        assert_eq!(r.backend.snapshot().active_preset.as_deref(), Some("A"));
        assert_eq!(fake_feature(&r.fake, "bass"), 1.0);
    }

    /// Press `action` twice while another operation holds `ops` (each press
    /// on its own thread, as the shell does).
    fn press_twice_while_busy(r: &Rig, action: HotkeyAction) {
        let busy = lock(&r.backend.ops);
        std::thread::scope(|sc| {
            for _ in 0..2 {
                sc.spawn(|| r.backend.run_hotkey(action));
            }
            // Both presses are waiting by now.
            std::thread::sleep(Duration::from_millis(100));
            drop(busy);
        });
    }

    #[test]
    fn two_quick_hotkey_presses_are_two_presses() {
        let r = rig_with_presets(&[("A", true, true), ("B", true, true), ("C", true, true)]);
        r.backend.apply_preset("A").unwrap();
        press_twice_while_busy(&r, HotkeyAction::NextProfile);
        assert_eq!(r.backend.snapshot().active_preset.as_deref(), Some("C"));

        r.backend.set_sbx(false).unwrap();
        press_twice_while_busy(&r, HotkeyAction::ToggleSbx);
        assert!(!r.backend.snapshot().sbx, "on, then off again");

        press_twice_while_busy(&r, HotkeyAction::ToggleDirectMode);
        assert!(!r.backend.snapshot().direct_mode, "on, then off again");
        assert!(!r.rec.take().iter().any(|o| matches!(o, Out::Error(_))));
    }

    #[test]
    fn tray_actions_use_the_same_paths_as_the_commands() {
        let r = rig_with_presets(&[("Bassy", true, true)]);
        r.backend.run_tray_action(TrayAction::Sbx(false));
        assert!(!r.backend.snapshot().sbx);
        r.backend.run_tray_action(TrayAction::Sbx(true));
        // A click delivered twice does the same thing twice.
        r.backend.run_tray_action(TrayAction::Sbx(true));
        assert!(r.backend.snapshot().sbx);
        r.backend.run_tray_action(TrayAction::Scout(true));
        let s = r.backend.snapshot();
        assert!(s.scout && !s.sbx);

        r.backend
            .run_tray_action(TrayAction::Output(Output::Speakers));
        assert_eq!(r.backend.snapshot().output, Some(Output::Speakers));
        r.backend.run_tray_action(TrayAction::Lighting(false));
        assert!(!r.backend.snapshot().lighting.enabled);
        r.backend.run_tray_action(TrayAction::Lighting(true));
        assert!(r.backend.snapshot().lighting.enabled);

        r.backend
            .run_tray_action(TrayAction::Profile("Bassy".into()));
        assert_eq!(r.backend.snapshot().active_preset.as_deref(), Some("Bassy"));
        assert_eq!(fake_feature(&r.fake, "bass"), 1.0);

        r.backend.run_tray_action(TrayAction::DirectMode(true));
        assert!(r.backend.snapshot().direct_mode);
        r.backend.run_tray_action(TrayAction::DirectMode(false));
        assert!(!r.backend.snapshot().direct_mode);
    }

    #[test]
    fn a_failed_tray_action_is_reported_and_announced() {
        let r = rig();
        r.rec.take();
        r.backend
            .run_tray_action(TrayAction::Profile("Nope".into()));
        let out = r.rec.take();
        assert!(out
            .iter()
            .any(|o| matches!(o, Out::Error(e) if e.code == ErrorCode::PresetNotFound)));
        assert!(
            out.iter().any(|o| matches!(o, Out::State(_))),
            "the snapshot is announced so the menu shows the real state"
        );
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

    // --- Mixer ---------------------------------------------------------------------

    use crate::api::{MixerLevel, MixerSources};
    use crate::mixer::FakeMixer;

    /// A backend over a never-connecting G6 and the given mixer.
    fn mixer_rig(mixer: Arc<FakeMixer>) -> (Arc<Backend>, Arc<Recorder>) {
        let fake = FakeDevice::new();
        fake.fail_next_connects(1000);
        let (session, events) = Session::start(Box::new(fake.connector()), fast(), no_storage());
        // The session thread needs a live receiver; it lives as long as the test process.
        std::mem::forget(events);
        let rec = Arc::new(Recorder::default());
        let b = Backend::with_mixer(session, None, false, mixer, rec.clone());
        (b, rec)
    }

    fn level_of(s: &AppState, scope: MixerScope) -> MixerLevel {
        *s.mixer.as_ref().unwrap().level(scope).unwrap()
    }

    fn source_of(s: &AppState) -> MixerSources {
        s.mixer.as_ref().unwrap().recording_source.clone().unwrap()
    }

    #[test]
    fn mixer_is_in_the_snapshot_and_null_where_unsupported() {
        let (b, _rec) = mixer_rig(Arc::new(FakeMixer::g6()));
        let s = b.snapshot();
        assert_eq!(level_of(&s, MixerScope::Playback).volume, 0.5);
        assert_eq!(level_of(&s, MixerScope::Monitoring).mute, Some(false));
        let src = source_of(&s);
        assert_eq!(src.current, 1);
        assert_eq!(src.options.len(), 4);
        assert_eq!(src.options[3].name, "What U Hear");

        // Backend::new has no mixer; a missing G6 audio device is the same.
        let fake = FakeDevice::new();
        fake.fail_next_connects(1000);
        let (session, _events) = Session::start(Box::new(fake.connector()), fast(), no_storage());
        let plain = Backend::new(session, None, false, Arc::new(Recorder::default()));
        assert_eq!(plain.snapshot().mixer, None);
        assert_eq!(
            plain
                .set_mixer_volume(MixerScope::Playback, 0.5)
                .unwrap_err()
                .code,
            ErrorCode::MixerUnavailable
        );
        let (none, _rec) = mixer_rig(Arc::new(FakeMixer::with(None)));
        assert_eq!(none.snapshot().mixer, None);
        assert_eq!(
            none.set_mixer_mute(MixerScope::Playback, true)
                .unwrap_err()
                .code,
            ErrorCode::MixerUnavailable
        );
    }

    #[test]
    fn mixer_writes_are_read_back_into_the_snapshot() {
        let mixer = Arc::new(FakeMixer::g6());
        let (b, _rec) = mixer_rig(mixer.clone());
        let before = b.snapshot();
        let s = b.set_mixer_volume(MixerScope::Recording, 0.25).unwrap();
        assert_eq!(level_of(&s, MixerScope::Recording).volume, 0.25);
        assert_eq!(
            level_of(&s, MixerScope::Playback).volume,
            0.5,
            "others stay"
        );
        assert!(s.revision > before.revision);
        let s = b.set_mixer_mute(MixerScope::Monitoring, true).unwrap();
        assert_eq!(level_of(&s, MixerScope::Monitoring).mute, Some(true));
        let s = b.set_mixer_source(3).unwrap();
        assert_eq!(source_of(&s).current, 3);
        assert_eq!(b.snapshot().mixer, s.mixer);
        assert_eq!(mixer.writes(), 3);
    }

    #[test]
    fn mixer_values_are_validated_before_the_os_is_touched() {
        let mixer = Arc::new(FakeMixer::g6());
        let (b, _rec) = mixer_rig(mixer.clone());
        for v in [f32::NAN, f32::INFINITY, -0.1, 1.01] {
            assert_eq!(
                b.set_mixer_volume(MixerScope::Playback, v)
                    .unwrap_err()
                    .code,
                ErrorCode::InvalidValue,
                "{v}"
            );
        }
        assert_eq!(
            b.set_mixer_source(99).unwrap_err().code,
            ErrorCode::InvalidValue
        );
        b.set_mixer_volume(MixerScope::Playback, 0.0).unwrap();
        b.set_mixer_volume(MixerScope::Playback, 1.0).unwrap();
        assert_eq!(mixer.writes(), 2, "only the two valid calls got through");

        // A scope or mute the device does not offer.
        mixer.change(|m| {
            let m = m.as_mut().unwrap();
            m.monitoring = None;
            m.recording.as_mut().unwrap().mute = None;
            m.recording_source = None;
        });
        let writes = mixer.writes();
        for r in [
            b.set_mixer_volume(MixerScope::Monitoring, 0.5),
            b.set_mixer_mute(MixerScope::Monitoring, true),
            b.set_mixer_mute(MixerScope::Recording, true),
            b.set_mixer_source(1),
        ] {
            assert_eq!(r.unwrap_err().code, ErrorCode::MixerUnavailable);
        }
        assert_eq!(mixer.writes(), writes);
    }

    #[test]
    fn mixer_changes_made_elsewhere_reach_the_ui_and_unchanged_ones_do_not() {
        let mixer = Arc::new(FakeMixer::g6());
        let (b, rec) = mixer_rig(mixer.clone());
        let before = b.snapshot().revision;
        // The volume knob (or another app) moves the playback volume.
        mixer.change(|m| {
            let l = m.as_mut().unwrap().playback.as_mut().unwrap();
            l.volume = 0.9;
        });
        let s = rec.wait_state("mixer change", |s| {
            s.mixer
                .as_ref()
                .is_some_and(|m| m.playback.is_some_and(|l| l.volume == 0.9))
        });
        assert!(s.revision > before);
        // A hint without a change announces nothing.
        rec.take();
        mixer.change(|_| {});
        std::thread::sleep(Duration::from_millis(150));
        assert!(!rec.take().iter().any(|o| matches!(o, Out::State(_))));
        // The G6 audio device goes away and comes back.
        mixer.change(|m| *m = None);
        rec.wait_state("mixer gone", |s| s.mixer.is_none());
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
        assert_eq!(
            b.export_preset_json("x").unwrap_err().code,
            ErrorCode::Storage
        );
        assert_eq!(
            b.import_preset(r#"{"headphones":{"bass":1}}"#, "x")
                .unwrap_err()
                .code,
            ErrorCode::Storage
        );
        assert_eq!(b.list_eq_curves().unwrap_err().code, ErrorCode::Storage);
        assert_eq!(b.save_eq_curve("A").unwrap_err().code, ErrorCode::Storage);
        assert_eq!(
            b.set_direct_mode(true).unwrap_err().code,
            ErrorCode::Storage
        );
        b.set_eq_enabled(true).unwrap();
    }
}
