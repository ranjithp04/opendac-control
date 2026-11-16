//! Feature table (the project plan §7.3): family / id → f32 value.
//!
//! This table is an allow-list. Only features listed here may be read or
//! written; values are validated and rounded before they are sent.

use std::fmt;

/// Feature family `0x96` — playback.
pub const FAMILY_PLAYBACK: u8 = 0x96;
/// Feature family `0x95` — mic.
pub const FAMILY_MIC: u8 = 0x95;
/// Feature family `0x97` — decoder.
pub const FAMILY_DECODER: u8 = 0x97;

/// Whether the device keeps a separate value per output (headphones /
/// speakers). Playback and mic features are per output: mic EQ was verified
/// to differ between outputs on hardware; the other mic features are kept per
/// output too, which is harmless since each output switch dumps them.
pub fn per_output(feature: &Feature) -> bool {
    feature.family == FAMILY_PLAYBACK || feature.family == FAMILY_MIC
}

/// Value type of a feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `0.0` / `1.0`.
    Bool,
    /// Continuous level, rounded to 0.01.
    Level,
    /// Gain in dB, rounded to 0.5.
    Db,
    /// Surround distance, rounded to 0.5.
    Distance,
    /// Integer choice from a fixed list of `(value, name)`.
    Choice(&'static [(u8, &'static str)]),
}

/// One entry of the feature table.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Feature {
    pub family: u8,
    pub id: u8,
    /// Stable machine name (used by CLI / presets).
    pub name: &'static str,
    pub kind: Kind,
    /// Device-side minimum.
    pub min: f32,
    /// Device-side maximum.
    pub max: f32,
    /// Rounding step for values.
    pub step: f32,
    /// Factor from device value to UI value (e.g. mic NR level device 0–0.5, UI ×2).
    pub ui_scale: f32,
    /// `false` = readable but must not be exposed or written. Every feature in
    /// the table is currently exposed; the flag is kept as a guard for any
    /// feature that has to stay read-only.
    pub exposed: bool,
}

/// Validation error for a feature value.
#[derive(Debug, Clone, PartialEq)]
pub enum FeatureError {
    /// `(family, id)` is not in the allow-list.
    Unknown { family: u8, id: u8 },
    /// Feature exists but is not exposed for writing.
    NotExposed(&'static str),
    /// Value is NaN or infinite.
    NotFinite(&'static str),
    /// Value is outside the allowed range.
    OutOfRange {
        name: &'static str,
        value: f32,
        min: f32,
        max: f32,
    },
    /// Value is not one of the allowed choices.
    InvalidChoice { name: &'static str, value: f32 },
}

impl fmt::Display for FeatureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FeatureError::Unknown { family, id } => {
                write!(
                    f,
                    "feature {family:02x}:{id:02x} is not in the feature table"
                )
            }
            FeatureError::NotExposed(n) => write!(f, "feature '{n}' is not exposed"),
            FeatureError::NotFinite(n) => write!(f, "feature '{n}': value is not a finite number"),
            FeatureError::OutOfRange {
                name,
                value,
                min,
                max,
            } => write!(f, "feature '{name}': {value} is outside {min}..={max}"),
            FeatureError::InvalidChoice { name, value } => {
                write!(f, "feature '{name}': {value} is not a valid choice")
            }
        }
    }
}

impl std::error::Error for FeatureError {}

const SMART_VOLUME_MODES: &[(u8, &str)] = &[(0, "normal"), (1, "loud"), (2, "night")];
const DECODER_MODES: &[(u8, &str)] = &[(1, "full"), (2, "normal"), (3, "night")];

const fn f(
    family: u8,
    id: u8,
    name: &'static str,
    kind: Kind,
    min: f32,
    max: f32,
    step: f32,
) -> Feature {
    Feature {
        family,
        id,
        name,
        kind,
        min,
        max,
        step,
        ui_scale: 1.0,
        exposed: true,
    }
}

const fn boolean(family: u8, id: u8, name: &'static str) -> Feature {
    f(family, id, name, Kind::Bool, 0.0, 1.0, 1.0)
}

const fn level(family: u8, id: u8, name: &'static str) -> Feature {
    f(family, id, name, Kind::Level, 0.0, 1.0, 0.01)
}

const fn db(family: u8, id: u8, name: &'static str, limit: f32) -> Feature {
    f(family, id, name, Kind::Db, -limit, limit, 0.5)
}

const fn choice(
    family: u8,
    id: u8,
    name: &'static str,
    choices: &'static [(u8, &'static str)],
) -> Feature {
    let mut min = choices[0].0;
    let mut max = choices[0].0;
    let mut i = 1;
    while i < choices.len() {
        if choices[i].0 < min {
            min = choices[i].0;
        }
        if choices[i].0 > max {
            max = choices[i].0;
        }
        i += 1;
    }
    f(
        family,
        id,
        name,
        Kind::Choice(choices),
        min as f32,
        max as f32,
        1.0,
    )
}

const P: u8 = FAMILY_PLAYBACK;
const M: u8 = FAMILY_MIC;
const D: u8 = FAMILY_DECODER;

/// Every feature in the project plan §7.3.
pub static FEATURES: &[Feature] = &[
    // Family 0x96 — playback
    boolean(P, 0x00, "surround"),
    level(P, 0x01, "surround_level"),
    f(
        P,
        0x17,
        "surround_distance",
        Kind::Distance,
        10.0,
        300.0,
        0.5,
    ),
    boolean(P, 0x02, "dialog"),
    level(P, 0x03, "dialog_level"),
    boolean(P, 0x04, "smartvol"),
    level(P, 0x05, "smartvol_level"),
    choice(P, 0x06, "smartvol_mode", SMART_VOLUME_MODES),
    boolean(P, 0x07, "crystalizer"),
    level(P, 0x08, "crystalizer_level"),
    boolean(P, 0x18, "bass"),
    level(P, 0x19, "bass_level"),
    boolean(P, 0x09, "eq"),
    db(P, 0x0a, "eq_preamp", 6.0),
    db(P, 0x0b, "eq_31", 12.0),
    db(P, 0x0c, "eq_62", 12.0),
    db(P, 0x0d, "eq_125", 12.0),
    db(P, 0x0e, "eq_250", 12.0),
    db(P, 0x0f, "eq_500", 12.0),
    db(P, 0x10, "eq_1k", 12.0),
    db(P, 0x11, "eq_2k", 12.0),
    db(P, 0x12, "eq_4k", 12.0),
    db(P, 0x13, "eq_8k", 12.0),
    db(P, 0x14, "eq_16k", 12.0),
    // Family 0x95 — mic
    boolean(M, 0x00, "mic_aec"),
    boolean(M, 0x04, "mic_clarity"),
    Feature {
        ui_scale: 2.0,
        ..f(M, 0x05, "mic_nr_level", Kind::Level, 0.0, 0.5, 0.01)
    },
    boolean(M, 0x13, "mic_eq"),
    db(M, 0x14, "mic_eq_1", 12.0),
    db(M, 0x15, "mic_eq_2", 12.0),
    db(M, 0x16, "mic_eq_3", 12.0),
    db(M, 0x17, "mic_eq_4", 12.0),
    db(M, 0x18, "mic_eq_5", 12.0),
    db(M, 0x19, "mic_eq_6", 12.0),
    db(M, 0x1a, "mic_eq_7", 12.0),
    db(M, 0x1b, "mic_eq_8", 12.0),
    // ASSUMPTION: the project plan does not give a type for recording Smart Volume; treated as bool.
    boolean(M, 0x2c, "mic_smartvol"),
    // Family 0x97
    choice(D, 0x02, "decoder_mode", DECODER_MODES),
];

/// EQ band names in band order (feature ids `0x96:0b`–`0x96:14`).
pub const EQ_BANDS: [&str; 10] = [
    "31", "62", "125", "250", "500", "1k", "2k", "4k", "8k", "16k",
];

/// First EQ band feature id.
pub const EQ_FIRST_ID: u8 = 0x0b;

/// Look up a feature by family and id.
pub fn lookup(family: u8, id: u8) -> Option<&'static Feature> {
    FEATURES.iter().find(|f| f.family == family && f.id == id)
}

/// Look up a feature by its machine name.
pub fn by_name(name: &str) -> Option<&'static Feature> {
    FEATURES.iter().find(|f| f.name == name)
}

/// Playback features Scout Mode sets itself when it is turned on (the project plan
/// §6: EQ on with the Scout curve in the band registers, Smart Volume on at
/// 0.9). While Scout is on their values are Scout's, not the user's; writing
/// them right before turning Scout on would make the device remember the
/// Scout curve as the "previous" EQ.
pub fn scout_owned(feature: &Feature) -> bool {
    feature.family == FAMILY_PLAYBACK
        && (matches!(feature.name, "eq" | "smartvol" | "smartvol_level")
            || (EQ_FIRST_ID..EQ_FIRST_ID + EQ_BANDS.len() as u8).contains(&feature.id))
}

/// Parse an EQ band name (`31`, `62`, `125`, `250`, `500`, `1k`, `2k`, `4k`,
/// `8k`, `16k`; case-insensitive, optional `hz` suffix, `1000` == `1k`) into a
/// band index `0..10`.
pub fn parse_eq_band(s: &str) -> Option<usize> {
    let lower = s.trim().to_ascii_lowercase();
    let base = lower.strip_suffix("hz").unwrap_or(&lower);
    let base = match base {
        "1000" => "1k",
        "2000" => "2k",
        "4000" => "4k",
        "8000" => "8k",
        "16000" => "16k",
        other => other,
    };
    EQ_BANDS.iter().position(|b| *b == base)
}

/// Feature for EQ band index `0..10`.
pub fn eq_band(index: usize) -> Option<&'static Feature> {
    if index < EQ_BANDS.len() {
        lookup(FAMILY_PLAYBACK, EQ_FIRST_ID + index as u8)
    } else {
        None
    }
}

fn round_to(value: f32, step: f32) -> f32 {
    let r = (value / step).round() * step;
    // Avoid "-0".
    if r == 0.0 {
        0.0
    } else {
        r
    }
}

impl Feature {
    /// Round a value to this feature's step (bools to 0/1, levels to 0.01,
    /// dB / distance to 0.5, choices to integers). Used both for values
    /// read from the device (float noise) and values about to be written.
    pub fn round(&self, value: f32) -> f32 {
        let r = round_to(value, self.step);
        if self.step < 1.0 {
            // Clean up binary noise from multiplying by e.g. 0.01.
            let decimals = 100.0;
            (r * decimals).round() / decimals
        } else {
            r
        }
    }

    /// Validate and round a device-side value for writing.
    pub fn validate(&self, value: f32) -> Result<f32, FeatureError> {
        if !value.is_finite() {
            return Err(FeatureError::NotFinite(self.name));
        }
        let v = self.round(value);
        if let Kind::Choice(choices) = self.kind {
            return if choices.iter().any(|(c, _)| *c as f32 == v) {
                Ok(v)
            } else {
                Err(FeatureError::InvalidChoice {
                    name: self.name,
                    value,
                })
            };
        }
        if v < self.min || v > self.max {
            return Err(FeatureError::OutOfRange {
                name: self.name,
                value,
                min: self.min,
                max: self.max,
            });
        }
        Ok(v)
    }

    /// Validate a value for writing; also rejects features that are not exposed.
    pub fn validate_write(&self, value: f32) -> Result<f32, FeatureError> {
        if !self.exposed {
            return Err(FeatureError::NotExposed(self.name));
        }
        self.validate(value)
    }

    /// Convert a device value to the UI value.
    pub fn to_ui(&self, device: f32) -> f32 {
        device * self.ui_scale
    }

    /// Convert a UI value to the device value.
    pub fn from_ui(&self, ui: f32) -> f32 {
        ui / self.ui_scale
    }

    /// Name of a choice value, for `Kind::Choice` features.
    pub fn choice_name(&self, value: f32) -> Option<&'static str> {
        match self.kind {
            Kind::Choice(choices) => {
                let v = value.round();
                choices
                    .iter()
                    .find(|(c, _)| *c as f32 == v)
                    .map(|(_, n)| *n)
            }
            _ => None,
        }
    }

    /// Value of a named choice, for `Kind::Choice` features.
    pub fn choice_value(&self, name: &str) -> Option<f32> {
        match self.kind {
            Kind::Choice(choices) => choices
                .iter()
                .find(|(_, n)| n.eq_ignore_ascii_case(name))
                .map(|(c, _)| *c as f32),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_unique_ids_and_names() {
        for (i, a) in FEATURES.iter().enumerate() {
            for b in &FEATURES[i + 1..] {
                assert!(!(a.family == b.family && a.id == b.id), "dup id {}", a.name);
                assert_ne!(a.name, b.name);
            }
        }
    }

    #[test]
    fn table_covers_plan_7_3() {
        let ids: &[(u8, &[u8])] = &[
            (
                0x96,
                &[
                    0x00, 0x01, 0x17, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x18, 0x19, 0x09,
                    0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14,
                ],
            ),
            (
                0x95,
                &[
                    0x00, 0x04, 0x05, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x2c,
                ],
            ),
            (0x97, &[0x02]),
        ];
        let mut n = 0;
        for (fam, list) in ids {
            for id in *list {
                assert!(lookup(*fam, *id).is_some(), "{fam:02x}:{id:02x}");
                n += 1;
            }
        }
        assert_eq!(n, FEATURES.len());
    }

    #[test]
    fn undocumented_ids_not_in_table() {
        for id in 0x70..=0x72 {
            assert!(lookup(0x96, id).is_none());
        }
        assert!(lookup(0x95, 0x0a).is_none());
        assert!(lookup(0x95, 0x2d).is_none());
    }

    #[test]
    fn preamp_is_exposed_and_validated() {
        let p = by_name("eq_preamp").unwrap();
        assert_eq!((p.family, p.id), (0x96, 0x0a));
        assert!(p.exposed);
        assert_eq!(p.validate_write(-2.5), Ok(-2.5));
        assert_eq!(p.validate_write(6.0), Ok(6.0));
        assert_eq!(p.validate_write(-6.0), Ok(-6.0));
        assert!(matches!(
            p.validate_write(6.5),
            Err(FeatureError::OutOfRange { .. })
        ));
        assert!(matches!(
            p.validate_write(-6.5),
            Err(FeatureError::OutOfRange { .. })
        ));
        assert!(!scout_owned(p));
        assert!(FEATURES.iter().all(|f| f.exposed));
    }

    #[test]
    fn scout_owned_features() {
        let owned: Vec<_> = FEATURES
            .iter()
            .filter(|f| scout_owned(f))
            .map(|f| f.name)
            .collect();
        assert_eq!(
            owned,
            [
                "smartvol",
                "smartvol_level",
                "eq",
                "eq_31",
                "eq_62",
                "eq_125",
                "eq_250",
                "eq_500",
                "eq_1k",
                "eq_2k",
                "eq_4k",
                "eq_8k",
                "eq_16k"
            ]
        );
    }

    #[test]
    fn eq_band_parsing() {
        let names = [
            "31", "62", "125", "250", "500", "1k", "2k", "4k", "8k", "16k",
        ];
        for (i, n) in names.iter().enumerate() {
            assert_eq!(parse_eq_band(n), Some(i));
            let f = eq_band(i).unwrap();
            assert_eq!(f.id, 0x0b + i as u8);
            assert_eq!(f.name, format!("eq_{n}"));
        }
        assert_eq!(parse_eq_band("1K"), Some(5));
        assert_eq!(parse_eq_band("16kHz"), Some(9));
        assert_eq!(parse_eq_band("1000"), Some(5));
        assert_eq!(parse_eq_band("31hz"), Some(0));
        assert_eq!(parse_eq_band("32"), None);
        assert_eq!(parse_eq_band(""), None);
        assert_eq!(parse_eq_band("k"), None);
        assert!(eq_band(10).is_none());
    }

    #[test]
    fn rounding() {
        let eq = by_name("eq_31").unwrap();
        // 0xbf7ffffd ≈ -0.99999982 from a captured dump.
        assert_eq!(eq.round(f32::from_bits(0xbf7f_fffd)), -1.0);
        // 0x34400000 ≈ 1.8e-7 → 0.
        assert_eq!(eq.round(f32::from_bits(0x3440_0000)), 0.0);
        assert_eq!(eq.round(1.3), 1.5);
        assert_eq!(eq.round(-0.1), 0.0);
        assert!(eq.round(-0.1).is_sign_positive());

        let lvl = by_name("surround_level").unwrap();
        assert_eq!(lvl.round(0.899_999_9), 0.9);
        assert_eq!(lvl.round(0.123_4), 0.12);

        let b = by_name("eq").unwrap();
        assert_eq!(b.round(0.999_9), 1.0);
        assert_eq!(b.round(f32::from_bits(0x3440_0000)), 0.0);
    }

    #[test]
    fn validation() {
        let eq = by_name("eq_1k").unwrap();
        assert_eq!(eq.validate(12.0), Ok(12.0));
        assert_eq!(eq.validate(-12.2), Ok(-12.0));
        assert!(matches!(
            eq.validate(12.5),
            Err(FeatureError::OutOfRange { .. })
        ));
        assert!(matches!(
            eq.validate(f32::NAN),
            Err(FeatureError::NotFinite(_))
        ));

        let dist = by_name("surround_distance").unwrap();
        assert_eq!(dist.validate(10.0), Ok(10.0));
        assert_eq!(dist.validate(123.3), Ok(123.5));
        assert!(dist.validate(9.5).is_err());
        assert!(dist.validate(300.5).is_err());

        let b = by_name("surround").unwrap();
        assert_eq!(b.validate(1.0), Ok(1.0));
        assert!(b.validate(2.0).is_err());
        assert!(b.validate(-1.0).is_err());

        let mode = by_name("smartvol_mode").unwrap();
        assert_eq!(mode.validate(2.0), Ok(2.0));
        assert!(matches!(
            mode.validate(3.0),
            Err(FeatureError::InvalidChoice { .. })
        ));
        assert_eq!(mode.choice_value("Night"), Some(2.0));
        assert_eq!(mode.choice_name(1.0), Some("loud"));

        let dec = by_name("decoder_mode").unwrap();
        assert_eq!((dec.min, dec.max), (1.0, 3.0));
        assert!(dec.validate(0.0).is_err());
        assert_eq!(dec.choice_value("full"), Some(1.0));

        let nr = by_name("mic_nr_level").unwrap();
        assert_eq!(nr.validate(0.5), Ok(0.5));
        assert!(nr.validate(0.6).is_err());
        assert_eq!(nr.from_ui(1.0), 0.5);
        assert_eq!(nr.to_ui(0.25), 0.5);
    }
}
