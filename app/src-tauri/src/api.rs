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
use g6_core::{direct, events, preset, store};
use serde::{Deserialize, Serialize};

pub use crate::eq_curves::EqCurve;

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
    /// Pre-amp in dB (-6..=6, steps of 0.5). Per output, not owned by Scout,
    /// untouched by Direct Mode.
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
    /// Mic EQ on/off. The 8 band gains are not exposed.
    pub eq_enabled: bool,
    /// Acoustic echo cancellation.
    pub aec: bool,
    /// Recording Smart Volume.
    pub smart_volume: bool,
}

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
    /// Preset names, sorted.
    pub presets: Vec<String>,
    /// Last preset applied or saved (the "Preset: X" footer); persisted.
    pub active_preset: Option<String>,
    /// User-saved EQ curves in saved order (the built-in curves are in the UI).
    pub eq_curves: Vec<EqCurve>,
    pub launch_at_login: bool,
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
    pub launch_at_login: bool,
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
                aec: on("mic_aec"),
                smart_volume: on("mic_smartvol"),
            },
            dac_filter: dev.dac_filter.map(DacFilter::from),
            dolby_decoder: choice("decoder_mode").and_then(DolbyMode::from_core_name),
            presets: extras.presets,
            active_preset: extras.active_preset,
            eq_curves: extras.eq_curves,
            launch_at_login: extras.launch_at_login,
        }
    }
}

// ---------------------------------------------------------------------------
// Event payloads
// ---------------------------------------------------------------------------

/// Payload of `connection-changed`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionChanged {
    pub connection: Connection,
    pub firmware: Option<String>,
}

/// Payload of `output-changed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputChanged {
    pub output: Output,
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
        set(&mut s, "eq", 1.0);
        set(&mut s, "eq_1k", -1.5);
        set(&mut s, "eq_16k", 12.0);
        set(&mut s, "eq_preamp", -2.5);
        set(&mut s, "surround", 1.0);
        set(&mut s, "surround_level", 0.12);
        set(&mut s, "surround_distance", 150.5);
        set(&mut s, "smartvol_level", 0.74);
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
        assert_eq!(a.mic.noise_reduction, 0.5, "device 0.25 is UI 0.5");
        assert_eq!(a.dac_filter, Some(DacFilter::Nos));
        assert_eq!(a.dolby_decoder, Some(DolbyMode::Night));
    }

    #[test]
    fn default_snapshot_is_disconnected_and_neutral() {
        let a = AppState::default();
        assert_eq!(a.connection, Connection::Disconnected);
        assert_eq!(a.output, None);
        assert_eq!(a.eq.bands, [0.0; 10]);
        assert_eq!(a.eq.preamp, 0.0);
        assert!(a.eq_curves.is_empty());
        assert_eq!(a.effects.surround.distance, 10.0);
        assert_eq!(a.mic.boost, None);
        assert_eq!(a.dac_filter, None);
        assert_eq!(a.dolby_decoder, None);
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
                "connection",
                "dacFilter",
                "directMode",
                "dolbyDecoder",
                "effects",
                "eq",
                "eqCurves",
                "firmware",
                "launchAtLogin",
                "mic",
                "output",
                "presets",
                "revision",
                "sbx",
                "scout"
            ]
        );
        assert_eq!(v["connection"], "disconnected");
        assert_eq!(v["eq"]["bands"].as_array().unwrap().len(), 10);
        assert_eq!(v["eq"]["preamp"], 0.0);
        assert_eq!(
            v["mic"],
            json!({
                "boost": null,
                "voiceClarity": false,
                "noiseReduction": 0.0,
                "eqEnabled": false,
                "aec": false,
                "smartVolume": false
            })
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
