//! JSON presets per output, stored in the OS config dir.
//!
//! A preset is `{ "headphones": {...}, "speakers": {...} }`. Each profile
//! maps feature names (see [`crate::features`]) to device-side values and
//! may carry `"sbx"` / `"scout"` booleans for the global mask, e.g.
//!
//! ```json
//! { "headphones": { "sbx": true, "eq": 1, "eq_31": 3, "surround": 1 } }
//! ```
//!
//! Only playback features (family `0x96`, the per-output ones, including the
//! EQ pre-amp `eq_preamp`) are allowed. [`apply`] writes the profile of the
//! **active** output; mic, decoder, DAC filter and mic boost are not part of presets.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::events::{self, Device};
use crate::features::{self, FAMILY_PLAYBACK, FEATURES};
use crate::protocol::{MaskTarget, Output};
use crate::state::{output_name, DeviceState};
use crate::store::{self, Paths, StoreError};

/// Settings for one output.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PresetProfile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sbx: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scout: Option<bool>,
    /// Feature name → device-side value.
    #[serde(flatten)]
    pub features: std::collections::BTreeMap<String, f32>,
}

/// A preset: one optional profile per output.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headphones: Option<PresetProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speakers: Option<PresetProfile>,
}

/// Preset errors.
#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// Content not allowed (unknown / non-playback / non-exposed feature,
    /// value out of range).
    Invalid(String),
    /// Preset name is empty, too long or has unsafe characters.
    BadName(String),
    NotFound(String),
    /// The preset has no profile for the active output.
    NoProfile(Output),
    Store(StoreError),
    Device(events::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Invalid(m) => write!(f, "invalid preset: {m}"),
            Error::BadName(n) => write!(
                f,
                "invalid preset name '{n}': use 1-64 letters, digits, space, '-', '_' or '.', not starting with '.'"
            ),
            Error::NotFound(n) => write!(f, "preset '{n}' not found"),
            Error::NoProfile(o) => write!(f, "preset has no profile for {}", output_name(*o)),
            Error::Store(e) => e.fmt(f),
            Error::Device(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for Error {}

impl From<StoreError> for Error {
    fn from(e: StoreError) -> Self {
        Error::Store(e)
    }
}

impl From<events::Error> for Error {
    fn from(e: events::Error) -> Self {
        Error::Device(e)
    }
}

impl Preset {
    pub fn profile(&self, output: Output) -> Option<&PresetProfile> {
        match output {
            Output::Headphones => self.headphones.as_ref(),
            Output::Speakers => self.speakers.as_ref(),
        }
    }

    /// Check every entry against the feature table.
    pub fn validate(&self) -> Result<(), Error> {
        for output in [Output::Headphones, Output::Speakers] {
            let Some(p) = self.profile(output) else {
                continue;
            };
            let o = output_name(output);
            for (name, value) in &p.features {
                let f = features::by_name(name)
                    .ok_or_else(|| Error::Invalid(format!("{o}: unknown feature '{name}'")))?;
                if f.family != FAMILY_PLAYBACK {
                    return Err(Error::Invalid(format!(
                        "{o}: '{name}' is not a per-output feature"
                    )));
                }
                f.validate_write(*value)
                    .map_err(|e| Error::Invalid(format!("{o}: {e}")))?;
            }
        }
        Ok(())
    }

    /// Snapshot of `state`: both profiles that are known, each with the
    /// exposed playback features; the SBX / Scout mask goes into the active
    /// output's profile (the mask is global, the inactive profile has none).
    /// While Scout is on, the active profile leaves out the features Scout
    /// sets itself ([`features::scout_owned`]): their values are Scout's.
    pub fn capture(state: &DeviceState) -> Preset {
        let profile = |output: Output| {
            let src = state.profile(output);
            if src.is_empty() {
                return None;
            }
            let scout = state.output == Some(output) && state.mask.is_some_and(|m| m.scout());
            let mut p = PresetProfile::default();
            for f in FEATURES.iter().filter(|f| {
                f.family == FAMILY_PLAYBACK && f.exposed && !(scout && features::scout_owned(f))
            }) {
                if let Some(v) = src.get(f) {
                    p.features.insert(f.name.to_string(), v);
                }
            }
            if state.output == Some(output) {
                if let Some(m) = state.mask {
                    p.sbx = Some(m.sbx());
                    p.scout = Some(m.scout());
                }
            }
            Some(p)
        };
        Preset {
            headphones: profile(Output::Headphones),
            speakers: profile(Output::Speakers),
        }
    }
}

/// What [`apply`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub output: Output,
    /// Number of device writes issued (features + mask bits).
    pub writes: usize,
}

/// Apply the profile for the active output. The whole preset is validated
/// first; nothing is sent if it is invalid. Order: Scout off, SBX, features
/// in table order, Scout on last (turning Scout on makes the device apply
/// its own EQ curve). With `scout: true` the features Scout sets itself
/// ([`features::scout_owned`]: EQ, Smart Volume) are not written, so the
/// device keeps the user's EQ to restore when Scout is turned off. A device
/// error stops the apply part-way; the writes done so far stay applied.
pub fn apply(dev: &Device, preset: &Preset) -> Result<Applied, Error> {
    preset.validate()?;
    let output = dev.read_output()?;
    let profile = preset.profile(output).ok_or(Error::NoProfile(output))?;
    let mut writes = 0;

    let mask = dev.read_mask()?;
    if profile.scout == Some(false) && mask.scout() {
        dev.set_mask(MaskTarget::Scout, false)?;
        writes += 1;
    }
    if let Some(want) = profile.sbx {
        if dev.read_mask()?.sbx() != want {
            dev.set_mask(MaskTarget::Sbx, want)?;
            writes += 1;
        }
    }
    // Scout-owned features are skipped whenever Scout will be on afterwards,
    // also for a profile that says nothing about Scout (the inactive output's
    // profile from `Preset::capture`) while the device has it on.
    let scout = profile.scout.unwrap_or(mask.scout());
    for f in FEATURES
        .iter()
        .filter(|f| !(scout && features::scout_owned(f)))
    {
        if let Some(&v) = profile.features.get(f.name) {
            dev.write_feature(f, v)?;
            writes += 1;
        }
    }
    if profile.scout == Some(true) && !dev.read_mask()?.scout() {
        dev.set_mask(MaskTarget::Scout, true)?;
        writes += 1;
    }
    Ok(Applied { output, writes })
}

/// Device names Windows refuses as file names (with any extension). Refused
/// on every platform so profiles stay portable.
fn is_reserved_name(name: &str) -> bool {
    let base = name.split('.').next().unwrap_or(name).trim_end();
    let base = base.to_ascii_uppercase();
    matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|p| {
            base.strip_prefix(p)
                .is_some_and(|n| matches!(n.as_bytes(), [b'1'..=b'9']))
        })
}

/// Preset name must be a safe file name stem.
fn check_name(name: &str) -> Result<(), Error> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && !is_reserved_name(name)
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'));
    if ok {
        Ok(())
    } else {
        Err(Error::BadName(name.to_string()))
    }
}

/// Directory of `NAME.json` presets.
#[derive(Debug, Clone)]
pub struct PresetStore {
    dir: PathBuf,
}

impl PresetStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        PresetStore { dir: dir.into() }
    }

    pub fn from_paths(paths: &Paths) -> Self {
        Self::new(paths.presets_dir())
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn file(&self, name: &str) -> Result<PathBuf, Error> {
        check_name(name)?;
        Ok(self.dir.join(format!("{name}.json")))
    }

    /// Save (overwriting) after validating.
    pub fn save(&self, name: &str, preset: &Preset) -> Result<(), Error> {
        let path = self.file(name)?;
        preset.validate()?;
        store::write_json(&path, preset)?;
        Ok(())
    }

    pub fn load(&self, name: &str) -> Result<Preset, Error> {
        let path = self.file(name)?;
        let preset: Preset =
            store::read_json(&path)?.ok_or_else(|| Error::NotFound(name.to_string()))?;
        preset.validate()?;
        Ok(preset)
    }

    /// Preset names, sorted.
    pub fn list(&self) -> Result<Vec<String>, Error> {
        let rd = match std::fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(Error::Store(StoreError::Io {
                    path: self.dir.clone(),
                    message: e.to_string(),
                }))
            }
        };
        let mut names: Vec<String> = rd
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let p = e.path();
                if p.extension()? != "json" {
                    return None;
                }
                let stem = p.file_stem()?.to_str()?.to_string();
                check_name(&stem).ok()?;
                Some(stem)
            })
            .collect();
        names.sort();
        Ok(names)
    }

    pub fn delete(&self, name: &str) -> Result<(), Error> {
        let path = self.file(name)?;
        if !path.exists() {
            return Err(Error::NotFound(name.to_string()));
        }
        store::remove_file(&path)?;
        Ok(())
    }

    /// Capture `state` and save it as `name`.
    pub fn save_current(&self, name: &str, state: &DeviceState) -> Result<Preset, Error> {
        let preset = Preset::capture(state);
        self.save(name, &preset)?;
        Ok(preset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Mask;
    use crate::state::tests::start;
    use crate::store::testutil::TempDir;
    use crate::transport::fake::FakeDevice;

    fn profile(pairs: &[(&str, f32)]) -> PresetProfile {
        PresetProfile {
            features: pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
            ..Default::default()
        }
    }

    fn get(fake: &FakeDevice, name: &str) -> f32 {
        let f = features::by_name(name).unwrap();
        fake.feature(f.family, f.id).unwrap()
    }

    #[test]
    fn json_shape() {
        let p = Preset {
            headphones: Some(PresetProfile {
                sbx: Some(true),
                ..profile(&[("eq", 1.0), ("eq_31", 3.0)])
            }),
            speakers: None,
        };
        let v: serde_json::Value = serde_json::to_value(&p).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"headphones": {"sbx": true, "eq": 1.0, "eq_31": 3.0}})
        );
        let back: Preset = serde_json::from_value(v).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn validation_uses_the_feature_table() {
        let bad = |pairs: &[(&str, f32)]| {
            Preset {
                headphones: Some(profile(pairs)),
                speakers: None,
            }
            .validate()
        };
        assert!(bad(&[("eq_1k", 12.0)]).is_ok());
        assert!(matches!(bad(&[("eq_1k", 12.5)]), Err(Error::Invalid(_))));
        assert!(matches!(bad(&[("nope", 1.0)]), Err(Error::Invalid(_))));
        assert!(bad(&[("eq_preamp", -6.0)]).is_ok());
        assert!(matches!(bad(&[("eq_preamp", 6.5)]), Err(Error::Invalid(_))));
        assert!(matches!(bad(&[("mic_aec", 1.0)]), Err(Error::Invalid(_))));
        assert!(matches!(
            bad(&[("surround_distance", 5.0)]),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn names_are_restricted_to_safe_stems() {
        for bad in ["", ".hidden", "../x", "a/b", "a\\b", &"x".repeat(65), "né"] {
            assert!(matches!(check_name(bad), Err(Error::BadName(_))), "{bad}");
        }
        for ok in [
            "Night",
            "movie night",
            "a-b_c.1",
            "console",
            "COM0",
            "COM10",
        ] {
            assert!(check_name(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn windows_reserved_names_are_refused_on_every_platform() {
        for bad in [
            "CON", "con", "Prn", "AUX", "nul", "COM1", "com9", "LPT1", "lpt9", "NUL.txt", "con.1",
            "CON ",
        ] {
            assert!(matches!(check_name(bad), Err(Error::BadName(_))), "{bad}");
        }
        let t = TempDir::new();
        let s = PresetStore::new(t.path());
        assert!(matches!(
            s.save("CON", &Preset::default()),
            Err(Error::BadName(_))
        ));
        assert!(matches!(s.load("com1"), Err(Error::BadName(_))));
    }

    #[test]
    fn store_save_load_list_delete() {
        let t = TempDir::new();
        let s = PresetStore::new(t.path().join("presets"));
        assert!(s.list().unwrap().is_empty());
        let p = Preset {
            headphones: Some(profile(&[("bass", 1.0)])),
            speakers: Some(profile(&[("bass", 0.0)])),
        };
        s.save("b", &p).unwrap();
        s.save("a", &p).unwrap();
        std::fs::write(s.dir().join("notes.txt"), "x").unwrap();
        assert_eq!(s.list().unwrap(), vec!["a", "b"]);
        assert_eq!(s.load("a").unwrap(), p);
        s.delete("a").unwrap();
        assert_eq!(s.list().unwrap(), vec!["b"]);
        assert_eq!(s.load("a"), Err(Error::NotFound("a".into())));
        assert_eq!(s.delete("a"), Err(Error::NotFound("a".into())));
        assert!(matches!(s.save("../evil", &p), Err(Error::BadName(_))));
        // Invalid content is refused on save and on load.
        let bad = Preset {
            headphones: Some(profile(&[("eq_31", 99.0)])),
            speakers: None,
        };
        assert!(matches!(s.save("bad", &bad), Err(Error::Invalid(_))));
        std::fs::write(
            s.dir().join("bad.json"),
            r#"{"headphones":{"eq_preamp":7}}"#,
        )
        .unwrap();
        assert!(matches!(s.load("bad"), Err(Error::Invalid(_))));
    }

    #[test]
    fn apply_writes_the_active_output_profile() {
        let fake = FakeDevice::new(); // headphones
        let (dev, _rx) = start(&fake);
        let p = Preset {
            headphones: Some(PresetProfile {
                sbx: Some(true),
                ..profile(&[
                    ("eq", 1.0),
                    ("eq_1k", -2.5),
                    ("surround", 1.0),
                    ("surround_level", 0.4),
                ])
            }),
            speakers: Some(profile(&[("bass", 1.0)])),
        };
        let r = apply(&dev, &p).unwrap();
        assert_eq!(r.output, Output::Headphones);
        assert_eq!(r.writes, 5);
        assert_eq!(get(&fake, "eq"), 1.0);
        assert_eq!(get(&fake, "eq_1k"), -2.5);
        assert_eq!(get(&fake, "surround_level"), 0.4);
        assert_eq!(get(&fake, "bass"), 0.0, "speakers profile not applied");
        assert!(fake.mask() & Mask::SBX != 0);
    }

    #[test]
    fn apply_picks_speakers_when_active() {
        let fake = FakeDevice::new();
        fake.switch_output(Output::Speakers);
        let (dev, _rx) = start(&fake);
        let p = Preset {
            headphones: Some(profile(&[("bass", 0.0)])),
            speakers: Some(profile(&[("bass", 1.0)])),
        };
        apply(&dev, &p).unwrap();
        assert_eq!(get(&fake, "bass"), 1.0);
    }

    #[test]
    fn apply_without_profile_for_output_or_with_invalid_content_sends_nothing() {
        let fake = FakeDevice::new();
        let (dev, _rx) = start(&fake);
        let only_speakers = Preset {
            headphones: None,
            speakers: Some(profile(&[("bass", 1.0)])),
        };
        assert_eq!(
            apply(&dev, &only_speakers),
            Err(Error::NoProfile(Output::Headphones))
        );
        let invalid = Preset {
            headphones: Some(profile(&[("bass", 1.0), ("eq_31", 50.0)])),
            speakers: None,
        };
        let before = fake.written().len();
        assert!(matches!(apply(&dev, &invalid), Err(Error::Invalid(_))));
        assert_eq!(fake.written().len(), before, "nothing sent");
    }

    #[test]
    fn scout_is_switched_off_before_and_on_after_features() {
        let fake = FakeDevice::new();
        fake.press_mask(Mask::SCOUT);
        let (dev, _rx) = start(&fake);
        let off = Preset {
            headphones: Some(PresetProfile {
                scout: Some(false),
                ..profile(&[("eq_31", 1.0)])
            }),
            speakers: None,
        };
        apply(&dev, &off).unwrap();
        assert_eq!(fake.mask() & Mask::SCOUT, 0);
        assert_eq!(get(&fake, "eq_31"), 1.0);

        let on = Preset {
            headphones: Some(PresetProfile {
                scout: Some(true),
                ..profile(&[("eq_31", 2.0)])
            }),
            speakers: None,
        };
        apply(&dev, &on).unwrap();
        assert_eq!(fake.mask() & Mask::SCOUT, Mask::SCOUT);
    }

    #[test]
    fn scout_on_preset_does_not_write_what_scout_sets_itself() {
        let fake = FakeDevice::new();
        let (dev, _rx) = start(&fake);
        // The user's own EQ, Scout off.
        dev.write_feature(features::by_name("eq_1k").unwrap(), 5.0)
            .unwrap();
        let curve = crate::transport::fake::SCOUT_CURVE;
        let mut p = profile(&[("eq", 1.0), ("smartvol", 1.0), ("bass", 1.0)]);
        for (i, v) in curve.iter().enumerate() {
            p.features
                .insert(features::eq_band(i).unwrap().name.to_string(), *v);
        }
        p.scout = Some(true);
        let r = apply(
            &dev,
            &Preset {
                headphones: Some(p),
                speakers: None,
            },
        )
        .unwrap();
        assert_eq!(r.writes, 2, "bass + scout on only");
        assert_eq!(fake.mask() & Mask::SCOUT, Mask::SCOUT);
        // Scout off brings the user's EQ back, not Scout's curve.
        dev.set_mask(MaskTarget::Scout, false).unwrap();
        assert_eq!(get(&fake, "eq_1k"), 5.0);
        assert_eq!(get(&fake, "eq"), 0.0);
        assert_eq!(get(&fake, "smartvol"), 0.0);
    }

    #[test]
    fn capture_with_scout_on_leaves_out_what_scout_sets() {
        let fake = FakeDevice::new();
        let (dev, _rx) = start(&fake);
        dev.write_feature(features::by_name("bass").unwrap(), 1.0)
            .unwrap();
        dev.set_mask(MaskTarget::Scout, true).unwrap();
        let mut state = DeviceState::new();
        state.refresh(&dev).unwrap();
        let h = Preset::capture(&state).headphones.unwrap();
        assert_eq!(h.scout, Some(true));
        assert_eq!(h.features.get("bass"), Some(&1.0));
        for name in ["eq", "eq_31", "eq_16k", "smartvol", "smartvol_level"] {
            assert!(!h.features.contains_key(name), "{name} captured");
        }
    }

    #[test]
    fn capture_then_apply_roundtrip() {
        let fake = FakeDevice::new();
        let (dev, _rx) = start(&fake);
        let setup = Preset {
            headphones: Some(PresetProfile {
                sbx: Some(true),
                ..profile(&[
                    ("eq", 1.0),
                    ("eq_250", 1.5),
                    ("crystalizer", 1.0),
                    ("crystalizer_level", 0.7),
                ])
            }),
            speakers: None,
        };
        apply(&dev, &setup).unwrap();

        let mut state = DeviceState::new();
        state.refresh(&dev).unwrap();
        let captured = Preset::capture(&state);
        captured.validate().unwrap();
        let h = captured.headphones.as_ref().unwrap();
        assert_eq!(h.sbx, Some(true));
        assert_eq!(h.scout, Some(false));
        assert_eq!(h.features["eq_250"], 1.5);
        assert!(
            h.features.contains_key("eq_preamp"),
            "pre-amp is saved with the profile"
        );
        assert!(!h.features.contains_key("mic_aec"));
        assert!(
            captured.speakers.is_none(),
            "unknown profile is not invented"
        );

        // Change things, then load the capture back.
        let f = features::by_name("eq_250").unwrap();
        dev.write_feature(f, -4.0).unwrap();
        dev.set_mask(MaskTarget::Sbx, false).unwrap();
        apply(&dev, &captured).unwrap();
        assert_eq!(get(&fake, "eq_250"), 1.5);
        assert!(fake.mask() & Mask::SBX != 0);

        // Persisted via the store.
        let t = TempDir::new();
        let s = PresetStore::from_paths(&Paths::under(t.path()));
        s.save_current("mine", &state).unwrap();
        assert_eq!(s.load("mine").unwrap(), captured);
    }
}
