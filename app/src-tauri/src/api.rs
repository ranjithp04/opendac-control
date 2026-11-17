//! Types of the UI <-> Rust contract. The contract itself is `app/API.md`;
//! these types are mirrored by hand in `app/ui/src/lib/api.ts` — change both
//! together.
//!
//! Everything is serialised as JSON with camelCase field names. Enum values
//! are the lower-case strings the TypeScript side uses.

use std::fmt;

use g6_core::features::{self, Feature};
use g6_core::protocol as core;
use g6_core::state::DeviceState;
use g6_core::{direct, eq_import, events, preset, store};
use serde::{Deserialize, Serialize};

pub use crate::eq_curves::EqCurve;
use crate::hotkeys::{HotkeyAction, HotkeySettings};
pub use crate::mic_eq_curves::MicEqCurve;

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// Active output. The UI labels the edited profile "Headphones" / "Speakers".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Output {
    Headphones,
    Speakers,
}

impl From<core::Output> for Output {
    fn from(o: core::Output) -> Self {
        match o {
            core::Output::Headphones => Output::Headphones,
            core::Output::Speakers => Output::Speakers,
        }
    }
}

impl From<Output> for core::Output {
    fn from(o: Output) -> Self {
        match o {
            Output::Headphones => core::Output::Headphones,
            Output::Speakers => core::Output::Speakers,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Connection {
    Connected,
    Disconnected,
}

/// DAC filter; the strings are `g6_core::protocol::DacFilter::name`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DacFilter {
    FastMin,
    SlowMin,
    Nos,
    FastLin,
    SlowLin,
}

impl From<core::DacFilter> for DacFilter {
    fn from(f: core::DacFilter) -> Self {
        match f {
            core::DacFilter::FastMin => DacFilter::FastMin,
            core::DacFilter::SlowMin => DacFilter::SlowMin,
            core::DacFilter::Nos => DacFilter::Nos,
            core::DacFilter::FastLin => DacFilter::FastLin,
            core::DacFilter::SlowLin => DacFilter::SlowLin,
        }
    }
}

impl From<DacFilter> for core::DacFilter {
    fn from(f: DacFilter) -> Self {
        match f {
            DacFilter::FastMin => core::DacFilter::FastMin,
            DacFilter::SlowMin => core::DacFilter::SlowMin,
            DacFilter::Nos => core::DacFilter::Nos,
            DacFilter::FastLin => core::DacFilter::FastLin,
            DacFilter::SlowLin => core::DacFilter::SlowLin,
        }
    }
}

/// Dolby Digital decoder mode (feature `decoder_mode`, optical input).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DolbyMode {
    Full,
    Normal,
    Night,
}

impl DolbyMode {
    /// The `choice_name` of the `decoder_mode` feature.
    pub fn core_name(self) -> &'static str {
        match self {
            DolbyMode::Full => "full",
            DolbyMode::Normal => "normal",
            DolbyMode::Night => "night",
        }
    }

    pub fn from_core_name(s: &str) -> Option<Self> {
        match s {
            "full" => Some(DolbyMode::Full),
            "normal" => Some(DolbyMode::Normal),
            "night" => Some(DolbyMode::Night),
            _ => None,
        }
    }
}

/// Smart Volume mode (feature `smartvol_mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SmartVolumeMode {
    Normal,
    Loud,
    Night,
}

impl SmartVolumeMode {
    /// The `choice_name` of the `smartvol_mode` feature.
    pub fn core_name(self) -> &'static str {
        match self {
            SmartVolumeMode::Normal => "normal",
            SmartVolumeMode::Loud => "loud",
            SmartVolumeMode::Night => "night",
        }
    }

    pub fn from_core_name(s: &str) -> Option<Self> {
        match s {
            "normal" => Some(SmartVolumeMode::Normal),
            "loud" => Some(SmartVolumeMode::Loud),
            "night" => Some(SmartVolumeMode::Night),
            _ => None,
        }
    }
}

/// SBX effects with an on/off and a level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EffectName {
    Surround,
    Crystalizer,
    Bass,
    Dialog,
    SmartVolume,
}

impl EffectName {
    /// Names of the on/off and level features in the g6-core table.
    pub fn features(self) -> (&'static str, &'static str) {
        match self {
            EffectName::Surround => ("surround", "surround_level"),
            EffectName::Crystalizer => ("crystalizer", "crystalizer_level"),
            EffectName::Bass => ("bass", "bass_level"),
            EffectName::Dialog => ("dialog", "dialog_level"),
            EffectName::SmartVolume => ("smartvol", "smartvol_level"),
        }
    }
}

// ---------------------------------------------------------------------------
// Snapshot
// ---------------------------------------------------------------------------

/// Equalizer of the active output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Eq {
    pub enabled: bool,
    /// Gain in dB for 31, 62, 125, 250, 500, 1k, 2k, 4k, 8k, 16k Hz
    /// (-12..=12, steps of 0.5).
    pub bands: [f32; 10],
    /// Pre-amp in dB (-6..=6, steps of 0.5). Per output, untouched by Direct
    /// Mode. The G6 sets it to 0 while Scout is on and restores it after; it
    /// is not locked by Scout.
    pub preamp: f32,
}

/// An effect with an on/off and a level (UI scale 0..=1).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Effect {
    pub enabled: bool,
    pub level: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Surround {
    pub enabled: bool,
    pub level: f32,
    /// 10..=300, steps of 0.5.
    pub distance: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SmartVolume {
    pub enabled: bool,
    pub level: f32,
    pub mode: SmartVolumeMode,
}

/// SBX effects of the active output.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Effects {
    pub surround: Surround,
    pub crystalizer: Effect,
    pub bass: Effect,
    pub dialog: Effect,
    pub smart_volume: SmartVolume,
}

/// Mic settings. Everything except `boost` is stored per output by the
/// device (g6-core), so these are the active output's values.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Mic {
    /// Mic boost in dB: 0, 10, 20 or 30 (shared by both outputs).
    pub boost: Option<u8>,
    pub voice_clarity: bool,
    /// Voice Clarity noise reduction level, UI scale 0..=1 (device 0..=0.5).
    pub noise_reduction: f32,
    /// Mic EQ on/off.
    pub eq_enabled: bool,
    /// Mic EQ band gains in dB, bands 1..=8 (`mic_eq_1`..`mic_eq_8`; the
    /// frequencies are unknown). Unknown bands read 0.
    pub eq_bands: [f32; 8],
    /// Acoustic echo cancellation.
    pub aec: bool,
    /// Recording Smart Volume.
    pub smart_volume: bool,
}

/// Feature names of the mic EQ bands 1..=8, in band order.
pub const MIC_EQ_BANDS: [&str; 8] = [
    "mic_eq_1", "mic_eq_2", "mic_eq_3", "mic_eq_4", "mic_eq_5", "mic_eq_6", "mic_eq_7", "mic_eq_8",
];

/// LED lighting. `color` is lower-case `rrggbb` without `#`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lighting {
    pub enabled: bool,
    pub color: String,
}

/// Colour shown while the LED colour is unknown.
pub const DEFAULT_LIGHTING_COLOR: &str = "ffffff";

/// Full snapshot of everything the panels show. Sent by `get_state`, returned
/// by every write command and carried by the `state-changed` event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppState {
    /// Increases with every snapshot the backend produces; the UI drops
    /// snapshots older than the one it shows.
    pub revision: u64,
    pub connection: Connection,
    /// Firmware string, e.g. `2.1.250903.1324`; `None` until read.
    pub firmware: Option<String>,
    /// Active output (follows the headphone jack); `None` until first read.
    pub output: Option<Output>,
    /// Our Direct Mode is on (EQ, SBX, Scout off and read-only).
    pub direct_mode: bool,
    pub sbx: bool,
    pub scout: bool,
    pub eq: Eq,
    pub effects: Effects,
    pub mic: Mic,
    pub dac_filter: Option<DacFilter>,
    pub dolby_decoder: Option<DolbyMode>,
    pub lighting: Lighting,
    /// Preset names, sorted.
    pub presets: Vec<String>,
    /// Last preset applied or saved (the "Preset: X" footer); persisted.
    pub active_preset: Option<String>,
    /// User-saved EQ curves in saved order (the built-in curves are in the UI).
    pub eq_curves: Vec<EqCurve>,
    /// User-saved mic EQ curves in saved order (the built-in presets are in the UI).
    pub mic_eq_curves: Vec<MicEqCurve>,
    /// Profile to apply when the output switches to headphones / speakers.
    pub auto_switch: AutoSwitch,
    pub launch_at_login: bool,
    /// Global hotkeys: settings and whether each one could be registered.
    pub hotkeys: Hotkeys,
    /// The G6's USB audio controls (macOS and Windows); `None` where the OS offers
    /// none or the G6 audio device is not there.
    pub mixer: Option<Mixer>,
}

/// One global hotkey as the UI shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HotkeyBinding {
    /// Canonical accelerator, e.g. `Ctrl+Alt+Cmd+D`.
    pub accelerator: String,
    /// Why registering it failed (taken by another app, not accepted by the
    /// OS); `None` when it is registered or the hotkeys are off.
    pub error: Option<String>,
}

/// Global hotkeys (opt-in). Registration failures are reported here, never fatal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hotkeys {
    pub enabled: bool,
    pub toggle_direct_mode: HotkeyBinding,
    pub next_profile: HotkeyBinding,
    pub toggle_sbx: HotkeyBinding,
}

impl Hotkeys {
    /// `errors` is indexed like [`HotkeyAction::index`].
    pub fn new(settings: &HotkeySettings, errors: &[Option<String>; 3]) -> Hotkeys {
        let binding = |a: HotkeyAction| HotkeyBinding {
            accelerator: settings.accelerator(a).to_string(),
            error: errors[a.index()].clone(),
        };
        Hotkeys {
            enabled: settings.enabled,
            toggle_direct_mode: binding(HotkeyAction::ToggleDirectMode),
            next_profile: binding(HotkeyAction::NextProfile),
            toggle_sbx: binding(HotkeyAction::ToggleSbx),
        }
    }
}

impl Default for Hotkeys {
    fn default() -> Self {
        Hotkeys::new(&HotkeySettings::default(), &Default::default())
    }
}

/// The parts of [`AppState`] that do not come from the device state.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Extras {
    pub revision: u64,
    pub firmware: Option<String>,
    pub direct_mode: bool,
    pub presets: Vec<String>,
    pub active_preset: Option<String>,
    pub eq_curves: Vec<EqCurve>,
    pub mic_eq_curves: Vec<MicEqCurve>,
    pub auto_switch: AutoSwitch,
    pub launch_at_login: bool,
    pub hotkeys: Hotkeys,
    pub mixer: Option<Mixer>,
}

fn feat(name: &str) -> &'static Feature {
    features::by_name(name)
        .unwrap_or_else(|| panic!("feature '{name}' is not in the g6-core table"))
}

fn round2(v: f32) -> f32 {
    (v * 100.0).round() / 100.0
}

impl Default for AppState {
    /// Disconnected, nothing known: what the UI gets before the first sync.
    fn default() -> Self {
        AppState::from_device(&DeviceState::new(), Extras::default())
    }
}

impl AppState {
    /// Snapshot of `dev` (g6-core's state; per-output values are those of the
    /// active output) plus the app-side `extras`. Unknown values take a
    /// neutral default (off, flat, 0, distance at its minimum); they are only
    /// meaningful once `connection` is `connected` or last-known settings
    /// were loaded.
    pub fn from_device(dev: &DeviceState, extras: Extras) -> AppState {
        let raw = |name: &str| dev.feature(feat(name));
        let on = |name: &str| raw(name).is_some_and(|v| v >= 0.5);
        // Device value -> UI value (applies the Feature scale, e.g. mic NR x2).
        let ui = |name: &str| {
            let f = feat(name);
            raw(name).map_or(0.0, |v| round2(f.to_ui(v)).clamp(0.0, 1.0))
        };
        let choice = |name: &str| raw(name).and_then(|v| feat(name).choice_name(v));
        let effect = |e: EffectName| {
            let (toggle, level) = e.features();
            Effect {
                enabled: on(toggle),
                level: ui(level),
            }
        };

        let bands: [f32; 10] = match dev.active_profile() {
            Some(p) => p.eq_bands().map(|b| b.unwrap_or(0.0)),
            None => [0.0; 10],
        };
        let surround = effect(EffectName::Surround);
        let smart = effect(EffectName::SmartVolume);

        AppState {
            revision: extras.revision,
            connection: if dev.connected {
                Connection::Connected
            } else {
                Connection::Disconnected
            },
            firmware: extras.firmware,
            output: dev.output.map(Output::from),
            direct_mode: extras.direct_mode,
            sbx: dev.mask.is_some_and(|m| m.sbx()),
            scout: dev.mask.is_some_and(|m| m.scout()),
            eq: Eq {
                enabled: on("eq"),
                bands,
                preamp: raw("eq_preamp").unwrap_or(0.0),
            },
            effects: Effects {
                surround: Surround {
                    enabled: surround.enabled,
                    level: surround.level,
                    distance: raw("surround_distance").unwrap_or(feat("surround_distance").min),
                },
                crystalizer: effect(EffectName::Crystalizer),
                bass: effect(EffectName::Bass),
                dialog: effect(EffectName::Dialog),
                smart_volume: SmartVolume {
                    enabled: smart.enabled,
                    level: smart.level,
                    mode: choice("smartvol_mode")
                        .and_then(SmartVolumeMode::from_core_name)
                        .unwrap_or(SmartVolumeMode::Normal),
                },
            },
            mic: Mic {
                boost: dev.mic_boost.map(|b| b.db()),
                voice_clarity: on("mic_clarity"),
                noise_reduction: ui("mic_nr_level"),
                eq_enabled: on("mic_eq"),
                eq_bands: MIC_EQ_BANDS.map(|n| raw(n).unwrap_or(0.0)),
                aec: on("mic_aec"),
                smart_volume: on("mic_smartvol"),
            },
            dac_filter: dev.dac_filter.map(DacFilter::from),
            dolby_decoder: choice("decoder_mode").and_then(DolbyMode::from_core_name),
            lighting: Lighting {
                enabled: dev.lighting_enabled.unwrap_or(false),
                color: dev
                    .lighting_color
                    .map_or_else(|| DEFAULT_LIGHTING_COLOR.to_string(), core::rgb_to_hex),
            },
            presets: extras.presets,
            active_preset: extras.active_preset,
            eq_curves: extras.eq_curves,
            mic_eq_curves: extras.mic_eq_curves,
            auto_switch: extras.auto_switch,
            launch_at_login: extras.launch_at_login,
            hotkeys: extras.hotkeys,
            mixer: extras.mixer,
        }
    }
}

// ---------------------------------------------------------------------------
// Event payloads
// ---------------------------------------------------------------------------

/// Result of `get_device_info`: identity of the connected G6. Fields the
/// device did not answer are `None` (read again on the next call). The serial
/// is sensitive: shown in the UI only, never logged.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    /// Firmware string, e.g. `2.1.250903.1324`.
    pub firmware: Option<String>,
    /// Serial number, 8 bytes as 16 lowercase hex digits.
    pub serial: Option<String>,
    /// DSP version bytes as lowercase hex, e.g. `30011000`.
    pub dsp_version: Option<String>,
    /// Hardware id bytes as lowercase hex, e.g. `97000000`.
    pub hardware_id: Option<String>,
}

/// Payload of `connection-changed`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionChanged {
    pub connection: Connection,
    pub firmware: Option<String>,
}

/// Auto-switch rules: the profile to apply when the output becomes
/// headphones / speakers (`None`: do nothing). Stored in `app_settings.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoSwitch {
    #[serde(default)]
    pub headphones: Option<String>,
    #[serde(default)]
    pub speakers: Option<String>,
}

impl AutoSwitch {
    pub fn rule(&self, output: Output) -> Option<&str> {
        match output {
            Output::Headphones => self.headphones.as_deref(),
            Output::Speakers => self.speakers.as_deref(),
        }
    }

    pub fn set(&mut self, output: Output, profile: Option<String>) {
        match output {
            Output::Headphones => self.headphones = profile,
            Output::Speakers => self.speakers = profile,
        }
    }

    /// Run `f` on each rule's profile name; `f` returns the new value. True
    /// if a rule changed.
    pub fn map(&mut self, f: impl Fn(&str) -> Option<String>) -> bool {
        let mut changed = false;
        for rule in [&mut self.headphones, &mut self.speakers] {
            if let Some(name) = rule.as_deref() {
                let new = f(name);
                if new.as_deref() != Some(name) {
                    *rule = new;
                    changed = true;
                }
            }
        }
        changed
    }
}

/// Payload of `output-changed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputChanged {
    pub output: Output,
}

// ---------------------------------------------------------------------------
// Mixer
// ---------------------------------------------------------------------------

/// Which set of USB audio controls a mixer command addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MixerScope {
    /// Speaker volume / mute (CoreAudio output scope).
    Playback,
    /// Monitoring, i.e. what is passed from the inputs to the output (play-through scope).
    Monitoring,
    /// Recording level / mute of the selected input (input scope).
    Recording,
}

/// Volume and mute of one scope. Both channels are always written together.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MixerLevel {
    /// Scalar 0..1 (mean of the channels; CoreAudio's volume curve, not dB).
    pub volume: f32,
    /// What the driver says `volume` is in dB (mean of the channels).
    pub volume_db: Option<f32>,
    /// `None` when the device offers no mute for this scope.
    pub mute: Option<bool>,
}

/// One entry of the recording source selector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MixerSource {
    /// CoreAudio data source id (`kAudioDevicePropertyDataSource`).
    pub id: u32,
    pub name: String,
}

/// The recording source selector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MixerSources {
    pub current: u32,
    pub options: Vec<MixerSource>,
}

/// The G6's USB Audio Class controls as the OS exposes them (not HID, not
/// part of `g6-core`). A scope the OS does not offer is `None`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Mixer {
    pub playback: Option<MixerLevel>,
    pub monitoring: Option<MixerLevel>,
    pub recording: Option<MixerLevel>,
    pub recording_source: Option<MixerSources>,
}

impl Mixer {
    pub fn level(&self, scope: MixerScope) -> Option<&MixerLevel> {
        match scope {
            MixerScope::Playback => self.playback.as_ref(),
            MixerScope::Monitoring => self.monitoring.as_ref(),
            MixerScope::Recording => self.recording.as_ref(),
        }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorCode {
    NotConnected,
    DirectModeActive,
    ScoutActive,
    InvalidValue,
    DeviceTimeout,
    DeviceRejected,
    DeviceMismatch,
    Incomplete,
    PresetNotFound,
    PresetInvalidName,
    PresetNoProfile,
    CurveNotFound,
    Storage,
    Autostart,
    /// The mixer control is not offered (no mixer on this OS, G6 audio device not found,
    /// control missing or read-only).
    MixerUnavailable,
    Internal,
    /// No longer returned (all commands are implemented); kept because the
    /// TypeScript mirror still lists it.
    NotImplemented,
}

/// What a rejected command promise carries (and the `device-error` payload).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
}

pub type ApiResult<T> = Result<T, ApiError>;

impl ApiError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        ApiError {
            code,
            message: message.into(),
        }
    }

    pub fn scout_active() -> Self {
        ApiError::new(
            ErrorCode::ScoutActive,
            "Scout Mode is on: EQ and Smart Volume are set by the device",
        )
    }

    pub fn direct_mode_active() -> Self {
        ApiError::new(
            ErrorCode::DirectModeActive,
            "Direct Mode is on: EQ, SBX and Scout are read-only",
        )
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

impl From<events::Error> for ApiError {
    fn from(e: events::Error) -> Self {
        use events::Error as E;
        let code = match &e {
            E::NotConnected | E::Transport(_) => ErrorCode::NotConnected,
            E::Timeout { .. } => ErrorCode::DeviceTimeout,
            E::Rejected { .. } => ErrorCode::DeviceRejected,
            E::Mismatch { .. } => ErrorCode::DeviceMismatch,
            E::Protocol(core::ProtocolError::Feature(_)) => ErrorCode::InvalidValue,
            E::Protocol(_) | E::Shutdown => ErrorCode::Internal,
        };
        ApiError::new(code, e.to_string())
    }
}

impl From<store::StoreError> for ApiError {
    fn from(e: store::StoreError) -> Self {
        ApiError::new(ErrorCode::Storage, e.to_string())
    }
}

/// Result of `import_eq`: an AutoEq / Equalizer APO text mapped onto the
/// 10 bands and the pre-amp (dB, 0.5 steps, bands within +-12, pre-amp +-6),
/// plus what was clamped or approximated. Nothing is written to the device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EqImport {
    pub bands: [f32; 10],
    pub preamp: f32,
    /// Human-readable notes (clamped values, ignored lines, approximation).
    pub notes: Vec<String>,
}

impl EqImport {
    /// Parse `text` (no device access). A text without any EQ rejects `invalidValue`.
    pub fn parse(text: &str) -> ApiResult<Self> {
        let r = eq_import::parse_eq_text(text)?;
        Ok(EqImport {
            bands: r.bands,
            preamp: r.preamp,
            notes: r.notes,
        })
    }
}

impl From<eq_import::Error> for ApiError {
    fn from(e: eq_import::Error) -> Self {
        ApiError::new(ErrorCode::InvalidValue, e.to_string())
    }
}

impl From<preset::Error> for ApiError {
    fn from(e: preset::Error) -> Self {
        use preset::Error as E;
        match e {
            E::Device(d) => d.into(),
            E::Store(s) => s.into(),
            other => {
                let code = match &other {
                    E::Invalid(_) => ErrorCode::InvalidValue,
                    E::BadName(_) => ErrorCode::PresetInvalidName,
                    E::NotFound(_) => ErrorCode::PresetNotFound,
                    E::NoProfile(_) => ErrorCode::PresetNoProfile,
                    E::Device(_) | E::Store(_) => unreachable!("handled above"),
                };
                ApiError::new(code, other.to_string())
            }
        }
    }
}

impl From<direct::Error> for ApiError {
    fn from(e: direct::Error) -> Self {
        use direct::Error as E;
        match e {
            E::Device(d) => d.into(),
            E::Store(s) => s.into(),
            E::Incomplete(_) => ApiError::new(ErrorCode::Incomplete, e.to_string()),
            E::NotActive => ApiError::new(ErrorCode::Internal, e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use g6_core::events::{Change, Event};
    use g6_core::protocol::{Mask, MicBoost};
    use serde_json::json;

    fn set(s: &mut DeviceState, name: &str, v: f32) {
        s.set_feature(feat(name), v);
    }

    fn state() -> DeviceState {
        let mut s = DeviceState::new();
        s.apply(&Event::Connected);
        s.apply(&Event::OutputSwitched(core::Output::Speakers));
        s.apply(&Event::Changed(Change::Mask(Mask(0x01))));
        s.apply(&Event::Changed(Change::DacFilter(core::DacFilter::Nos)));
        s.apply(&Event::Changed(Change::MicBoost(MicBoost::Db20)));
        s.apply(&Event::Changed(Change::LightingEnabled(true)));
        s.apply(&Event::Changed(Change::LightingColor([255, 128, 0])));
        set(&mut s, "eq", 1.0);
        set(&mut s, "eq_1k", -1.5);
        set(&mut s, "eq_16k", 12.0);
        set(&mut s, "eq_preamp", -2.5);
        set(&mut s, "surround", 1.0);
        set(&mut s, "surround_level", 0.12);
        set(&mut s, "surround_distance", 150.5);
        set(&mut s, "smartvol_level", 0.74);
        set(&mut s, "mic_eq_3", -2.5);
        set(&mut s, "smartvol_mode", 2.0);
        set(&mut s, "mic_nr_level", 0.25);
        set(&mut s, "mic_clarity", 1.0);
        set(&mut s, "decoder_mode", 3.0);
        s
    }

    #[test]
    fn snapshot_maps_device_state() {
        let a = AppState::from_device(
            &state(),
            Extras {
                revision: 7,
                firmware: Some("2.1.250903.1324".into()),
                presets: vec!["Default".into()],
                active_preset: Some("Default".into()),
                ..Extras::default()
            },
        );
        assert_eq!(a.revision, 7);
        assert_eq!(a.connection, Connection::Connected);
        assert_eq!(a.output, Some(Output::Speakers));
        assert!(a.sbx && !a.scout);
        assert!(a.eq.enabled);
        assert_eq!(a.eq.bands[5], -1.5);
        assert_eq!(a.eq.bands[9], 12.0);
        assert_eq!(a.eq.bands[0], 0.0, "unknown band is flat");
        assert_eq!(a.eq.preamp, -2.5);
        assert!(a.effects.surround.enabled);
        assert_eq!(a.effects.surround.level, 0.12);
        assert_eq!(a.effects.surround.distance, 150.5);
        assert!(!a.effects.bass.enabled);
        assert_eq!(a.effects.smart_volume.level, 0.74);
        assert_eq!(a.effects.smart_volume.mode, SmartVolumeMode::Night);
        assert_eq!(a.mic.boost, Some(20));
        assert!(a.mic.voice_clarity);
        assert_eq!(a.mic.eq_bands[2], -2.5);
        assert_eq!(a.mic.eq_bands[0], 0.0, "unknown band is flat");
        assert_eq!(a.mic.noise_reduction, 0.5, "device 0.25 is UI 0.5");
        assert_eq!(a.dac_filter, Some(DacFilter::Nos));
        assert_eq!(a.dolby_decoder, Some(DolbyMode::Night));
        assert_eq!(
            a.lighting,
            Lighting {
                enabled: true,
                color: "ff8000".into()
            }
        );
    }

    #[test]
    fn default_snapshot_is_disconnected_and_neutral() {
        let a = AppState::default();
        assert_eq!(a.connection, Connection::Disconnected);
        assert_eq!(a.output, None);
        assert_eq!(a.eq.bands, [0.0; 10]);
        assert_eq!(a.eq.preamp, 0.0);
        assert!(a.eq_curves.is_empty());
        assert!(a.mic_eq_curves.is_empty());
        assert_eq!(a.auto_switch, AutoSwitch::default());
        assert_eq!(a.mic.eq_bands, [0.0; 8]);
        assert_eq!(a.effects.surround.distance, 10.0);
        assert_eq!(a.mic.boost, None);
        assert_eq!(a.dac_filter, None);
        assert_eq!(a.dolby_decoder, None);
        assert_eq!(
            a.lighting,
            Lighting {
                enabled: false,
                color: "ffffff".into()
            }
        );
    }

    #[test]
    fn auto_switch_rules_wire_shape_and_map() {
        let mut a = AutoSwitch::default();
        assert_eq!(
            serde_json::to_value(&a).unwrap(),
            json!({ "headphones": null, "speakers": null })
        );
        // Old settings files have no such keys.
        assert_eq!(serde_json::from_str::<AutoSwitch>("{}").unwrap(), a);
        a.set(Output::Speakers, Some("Games".into()));
        assert_eq!(a.rule(Output::Speakers), Some("Games"));
        assert_eq!(a.rule(Output::Headphones), None);
        assert!(!a.map(|n| Some(n.to_string())));
        assert!(a.map(|_| None));
        assert_eq!(a, AutoSwitch::default());
    }

    #[test]
    fn mixer_wire_shape() {
        let m = Mixer {
            playback: Some(MixerLevel {
                volume: 0.5,
                volume_db: Some(-16.0),
                mute: Some(false),
            }),
            monitoring: None,
            recording: Some(MixerLevel {
                volume: 0.75,
                volume_db: None,
                mute: None,
            }),
            recording_source: Some(MixerSources {
                current: 2,
                options: vec![MixerSource {
                    id: 2,
                    name: "External Mic".into(),
                }],
            }),
        };
        assert_eq!(
            serde_json::to_value(&m).unwrap(),
            json!({
                "playback": { "volume": 0.5, "volumeDb": -16.0, "mute": false },
                "monitoring": null,
                "recording": { "volume": 0.75, "volumeDb": null, "mute": null },
                "recordingSource": {
                    "current": 2,
                    "options": [{ "id": 2, "name": "External Mic" }]
                }
            })
        );
        assert_eq!(AppState::default().mixer, None);
        for (scope, name) in [
            (MixerScope::Playback, "playback"),
            (MixerScope::Monitoring, "monitoring"),
            (MixerScope::Recording, "recording"),
        ] {
            assert_eq!(serde_json::to_value(scope).unwrap(), name);
            assert_eq!(
                serde_json::from_str::<MixerScope>(&format!("\"{name}\"")).unwrap(),
                scope
            );
            assert_eq!(m.level(scope).is_some(), scope != MixerScope::Monitoring);
        }
        let e = ApiError::from(crate::mixer::MixerError::Unavailable("x".into()));
        assert_eq!(serde_json::to_value(e).unwrap()["code"], "mixerUnavailable");
        let e = ApiError::from(crate::mixer::MixerError::Failed("x".into()));
        assert_eq!(e.code, ErrorCode::Internal);
    }

    #[test]
    fn snapshot_json_shape_matches_api_md() {
        let v = serde_json::to_value(AppState::default()).unwrap();
        let obj = v.as_object().unwrap();
        let mut keys: Vec<_> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "activePreset",
                "autoSwitch",
                "connection",
                "dacFilter",
                "directMode",
                "dolbyDecoder",
                "effects",
                "eq",
                "eqCurves",
                "firmware",
                "hotkeys",
                "launchAtLogin",
                "lighting",
                "mic",
                "micEqCurves",
                "mixer",
                "output",
                "presets",
                "revision",
                "sbx",
                "scout"
            ]
        );
        assert_eq!(v["connection"], "disconnected");
        let h = v["hotkeys"].as_object().unwrap();
        let mut hkeys: Vec<_> = h.keys().map(String::as_str).collect();
        hkeys.sort_unstable();
        assert_eq!(
            hkeys,
            ["enabled", "nextProfile", "toggleDirectMode", "toggleSbx"]
        );
        assert_eq!(v["hotkeys"]["enabled"], false);
        assert_eq!(v["hotkeys"]["toggleSbx"]["error"], serde_json::Value::Null);
        assert!(v["hotkeys"]["toggleSbx"]["accelerator"].is_string());
        assert_eq!(v["eq"]["bands"].as_array().unwrap().len(), 10);
        assert_eq!(v["eq"]["preamp"], 0.0);
        assert_eq!(
            v["mic"],
            json!({
                "boost": null,
                "voiceClarity": false,
                "noiseReduction": 0.0,
                "eqEnabled": false,
                "eqBands": [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
                "aec": false,
                "smartVolume": false
            })
        );
        assert_eq!(
            v["lighting"],
            json!({ "enabled": false, "color": "ffffff" })
        );
        assert_eq!(
            v["effects"]["smartVolume"],
            json!({ "enabled": false, "level": 0.0, "mode": "normal" })
        );
        assert_eq!(
            v["effects"]["surround"],
            json!({ "enabled": false, "level": 0.0, "distance": 10.0 })
        );
    }

    #[test]
    fn device_info_json_shape_matches_api_md() {
        let v = serde_json::to_value(DeviceInfo::default()).unwrap();
        assert_eq!(
            v,
            json!({
                "firmware": null,
                "serial": null,
                "dspVersion": null,
                "hardwareId": null
            })
        );
    }

    #[test]
    fn enum_strings() {
        let s = |v: &dyn ErasedSerialize| v.json();
        assert_eq!(s(&DacFilter::FastLin), "\"fast-lin\"");
        assert_eq!(s(&EffectName::SmartVolume), "\"smartVolume\"");
        assert_eq!(s(&Output::Headphones), "\"headphones\"");
        assert_eq!(s(&ErrorCode::DirectModeActive), "\"directModeActive\"");
        // DacFilter strings are the g6-core names.
        for f in core::DacFilter::ALL {
            assert_eq!(s(&DacFilter::from(f)), format!("\"{}\"", f.name()));
            assert_eq!(core::DacFilter::from(DacFilter::from(f)), f);
        }
    }

    #[test]
    fn eq_import_json_keys_and_errors() {
        let r = EqImport::parse("Preamp: -6.4 dB\nFilter 1: ON PK Fc 1000 Hz Gain -3.0 dB Q 1.0")
            .unwrap();
        let v: serde_json::Value = serde_json::to_value(&r).unwrap();
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        keys.sort();
        assert_eq!(keys, ["bands", "notes", "preamp"]);
        assert_eq!(v["bands"].as_array().unwrap().len(), 10);
        assert_eq!(v["preamp"], -6.0);
        assert!(v["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n.as_str().unwrap().starts_with("Pre-amp: -6.4 dB clamped")));
        let e = EqImport::parse("not an eq").unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidValue);
        assert!(e.message.contains("no EQ found"), "{}", e.message);
    }

    trait ErasedSerialize {
        fn json(&self) -> String;
    }
    impl<T: Serialize> ErasedSerialize for T {
        fn json(&self) -> String {
            serde_json::to_string(self).unwrap()
        }
    }

    #[test]
    fn effect_names_are_in_the_feature_table() {
        for e in [
            EffectName::Surround,
            EffectName::Crystalizer,
            EffectName::Bass,
            EffectName::Dialog,
            EffectName::SmartVolume,
        ] {
            let (t, l) = e.features();
            assert_eq!(feat(t).kind, features::Kind::Bool);
            assert_eq!(feat(l).kind, features::Kind::Level);
        }
    }

    #[test]
    fn error_mapping() {
        let code = |e: events::Error| ApiError::from(e).code;
        assert_eq!(code(events::Error::NotConnected), ErrorCode::NotConnected);
        assert_eq!(
            code(events::Error::Timeout { cmd: 0x11 }),
            ErrorCode::DeviceTimeout
        );
        assert_eq!(
            code(events::Error::Rejected {
                cmd: 0x12,
                status: 0x80
            }),
            ErrorCode::DeviceRejected
        );
        let out_of_range = features::FeatureError::OutOfRange {
            name: "eq_1k",
            value: 13.0,
            min: -12.0,
            max: 12.0,
        };
        assert_eq!(code(out_of_range.into()), ErrorCode::InvalidValue);
        assert_eq!(
            ApiError::from(preset::Error::NotFound("x".into())).code,
            ErrorCode::PresetNotFound
        );
        assert_eq!(
            ApiError::from(preset::Error::BadName("..".into())).code,
            ErrorCode::PresetInvalidName
        );
        assert_eq!(
            ApiError::from(preset::Error::Device(events::Error::NotConnected)).code,
            ErrorCode::NotConnected
        );
        assert_eq!(
            ApiError::from(direct::Error::Incomplete(vec!["x".into()])).code,
            ErrorCode::Incomplete
        );
    }
}
