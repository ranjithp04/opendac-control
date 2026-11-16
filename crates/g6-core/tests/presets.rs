//! Presets: JSON shape and round trip, the on-disk store, and apply against
//! the fake device. Public API only.

mod common;

use std::collections::BTreeMap;

use common::*;
use g6_core::features::{self, FAMILY_PLAYBACK, FEATURES};
use g6_core::preset::{self, Preset, PresetProfile, PresetStore};
use g6_core::protocol::{Mask, Output};
use g6_core::session::{Session, SessionConfig};
use g6_core::store::Paths;
use g6_core::transport::fake::FakeDevice;

fn profile(pairs: &[(&str, f32)]) -> PresetProfile {
    PresetProfile {
        features: pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        ..Default::default()
    }
}

/// A preset using every exposed playback feature, on both outputs, with
/// different valid values per output.
fn full_preset() -> Preset {
    let mut h = PresetProfile {
        sbx: Some(true),
        scout: Some(false),
        features: BTreeMap::new(),
    };
    let mut s = PresetProfile::default();
    for (i, f) in FEATURES
        .iter()
        .filter(|f| f.family == FAMILY_PLAYBACK && f.exposed)
        .enumerate()
    {
        let (hv, sv) = match f.kind {
            features::Kind::Bool => ((i % 2) as f32, ((i + 1) % 2) as f32),
            features::Kind::Choice(list) => (
                list[i % list.len()].0 as f32,
                list[(i + 1) % list.len()].0 as f32,
            ),
            features::Kind::Level => (0.3, 0.6),
            features::Kind::Db => (-1.5, 2.5),
            features::Kind::Distance => (120.5, 20.0),
        };
        h.features.insert(f.name.to_string(), hv);
        s.features.insert(f.name.to_string(), sv);
    }
    Preset {
        headphones: Some(h),
        speakers: Some(s),
    }
}

// ---------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------

#[test]
fn json_round_trip_is_lossless() {
    let p = full_preset();
    p.validate().unwrap();
    let json = serde_json::to_string_pretty(&p).unwrap();
    let back: Preset = serde_json::from_str(&json).unwrap();
    assert_eq!(back, p);
    // And once more from the re-serialized text: stable.
    assert_eq!(serde_json::to_string_pretty(&back).unwrap(), json);
}

#[test]
fn json_shape_is_flat_per_output() {
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
        serde_json::json!({ "headphones": { "sbx": true, "eq": 1.0, "eq_31": 3.0 } })
    );
}

#[test]
fn hand_written_json_loads() {
    let p: Preset = serde_json::from_str(
        r#"{
            "headphones": { "sbx": true, "eq": 1, "eq_1k": -2.5, "surround_level": 0.9 },
            "speakers":   { "scout": false, "bass": 1 }
        }"#,
    )
    .unwrap();
    p.validate().unwrap();
    let h = p.profile(Output::Headphones).unwrap();
    assert_eq!(h.sbx, Some(true));
    assert_eq!(h.scout, None);
    assert_eq!(h.features["eq"], 1.0);
    assert_eq!(h.features["eq_1k"], -2.5);
    assert_eq!(h.features["surround_level"], 0.9);
    let s = p.profile(Output::Speakers).unwrap();
    assert_eq!(s.scout, Some(false));
    assert_eq!(s.features.len(), 1);
}

#[test]
fn non_numeric_values_are_a_parse_error() {
    assert!(serde_json::from_str::<Preset>(r#"{"headphones":{"eq":"on"}}"#).is_err());
    assert!(serde_json::from_str::<Preset>(r#"{"headphones":{"sbx":1}}"#).is_err());
}

#[test]
fn validation_rejects_everything_outside_the_playback_table() {
    let bad: &[(&str, f32)] = &[
        ("eq_31", 12.5),     // out of range
        ("eq_31", f32::NAN), // not finite (cannot come from JSON, but the API takes f32)
        ("eq_preamp", 6.5),  // out of range (+-6)
        ("eq_preamp", -7.0), // out of range (+-6)
        ("mic_aec", 1.0),    // not per-output
        ("mic_nr_level", 0.2),
        ("decoder_mode", 2.0),
        ("no_such_feature", 1.0),
        ("smartvol_mode", 3.0), // not a choice
        ("surround_distance", 5.0),
    ];
    for (name, v) in bad {
        let p = Preset {
            headphones: Some(profile(&[(*name, *v)])),
            speakers: None,
        };
        assert!(
            matches!(p.validate(), Err(preset::Error::Invalid(_))),
            "{name}={v} must be invalid"
        );
        // The speakers profile is checked too.
        let p = Preset {
            headphones: None,
            speakers: Some(profile(&[(*name, *v)])),
        };
        assert!(p.validate().is_err(), "speakers {name}={v}");
    }
    assert!(Preset::default().validate().is_ok());
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

#[test]
fn store_round_trip_and_listing() {
    let t = TempDir::new();
    let store = PresetStore::from_paths(&Paths::under(t.path()));
    assert_eq!(store.list().unwrap(), Vec::<String>::new());

    let p = full_preset();
    store.save("movies", &p).unwrap();
    store.save("Night mode_2.1", &Preset::default()).unwrap();
    assert_eq!(store.load("movies").unwrap(), p);
    assert_eq!(store.list().unwrap(), ["Night mode_2.1", "movies"]);

    // Overwrite.
    let q = Preset {
        speakers: Some(profile(&[("bass", 1.0)])),
        ..Default::default()
    };
    store.save("movies", &q).unwrap();
    assert_eq!(store.load("movies").unwrap(), q);

    store.delete("movies").unwrap();
    assert!(matches!(
        store.load("movies"),
        Err(preset::Error::NotFound(_))
    ));
    assert!(matches!(
        store.delete("movies"),
        Err(preset::Error::NotFound(_))
    ));
    assert_eq!(store.list().unwrap(), ["Night mode_2.1"]);
}

#[test]
fn store_writes_one_pretty_json_file_per_preset_in_the_presets_dir() {
    let t = TempDir::new();
    let paths = Paths::under(t.path());
    let store = PresetStore::from_paths(&paths);
    store.save("a", &full_preset()).unwrap();
    let file = paths.presets_dir().join("a.json");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(serde_json::from_str::<Preset>(&text).is_ok());
    // No stray temp files from the atomic write.
    let names: Vec<_> = std::fs::read_dir(paths.presets_dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["a.json"]);
}

#[test]
fn unsafe_names_are_refused_everywhere() {
    let t = TempDir::new();
    let store = PresetStore::new(t.path());
    let long = "x".repeat(65);
    for bad in [
        "",
        ".hidden",
        "../evil",
        "a/b",
        "a\\b",
        "x\0y",
        "tab\tname",
        "ü",
        long.as_str(),
    ] {
        assert!(
            matches!(
                store.save(bad, &Preset::default()),
                Err(preset::Error::BadName(_))
            ),
            "save {bad:?}"
        );
        assert!(
            matches!(store.load(bad), Err(preset::Error::BadName(_))),
            "load {bad:?}"
        );
        assert!(
            matches!(store.delete(bad), Err(preset::Error::BadName(_))),
            "delete {bad:?}"
        );
    }
    assert!(store.save(&"x".repeat(64), &Preset::default()).is_ok());
    // Nothing escaped the directory.
    assert!(!t.path().parent().unwrap().join("evil.json").exists());
}

#[test]
fn an_invalid_preset_is_not_saved() {
    let t = TempDir::new();
    let store = PresetStore::new(t.path());
    let bad = Preset {
        headphones: Some(profile(&[("eq_preamp", 6.5)])),
        speakers: None,
    };
    assert!(matches!(
        store.save("bad", &bad),
        Err(preset::Error::Invalid(_))
    ));
    assert_eq!(store.list().unwrap(), Vec::<String>::new());
}

#[test]
fn hand_edited_files_are_checked_on_load_and_garbage_is_reported() {
    let t = TempDir::new();
    let store = PresetStore::new(t.path());
    std::fs::write(
        t.path().join("preamp.json"),
        r#"{"headphones":{"eq_preamp":7}}"#,
    )
    .unwrap();
    assert!(matches!(
        store.load("preamp"),
        Err(preset::Error::Invalid(_))
    ));
    std::fs::write(t.path().join("junk.json"), "not json {").unwrap();
    assert!(matches!(store.load("junk"), Err(preset::Error::Store(_))));
    // Non-json files and bad stems are not listed.
    std::fs::write(t.path().join("notes.txt"), "x").unwrap();
    std::fs::write(t.path().join(".hidden.json"), "{}").unwrap();
    assert_eq!(store.list().unwrap(), ["junk", "preamp"]);
}

// ---------------------------------------------------------------------------
// Apply and capture against the fake
// ---------------------------------------------------------------------------

#[test]
fn save_current_then_load_restores_the_device() {
    let t = TempDir::new();
    let paths = Paths::under(t.path());
    let fake = FakeDevice::new();
    set(&fake, "eq", 1.0);
    set(&fake, "eq_62", 2.0);
    set(&fake, "eq_16k", -3.5);
    set(&fake, "eq_preamp", -2.5);
    set(&fake, "surround", 1.0);
    set(&fake, "surround_level", 0.8);
    set(&fake, "surround_distance", 175.5);
    set(&fake, "smartvol_mode", 2.0);
    set(&fake, "bass_level", 0.35);
    fake.press_mask(Mask::SBX | Mask::VOICE_CLARITY);

    let (session, _rx) = Session::start(
        Box::new(fake.connector()),
        fast(),
        SessionConfig {
            paths: Some(paths.clone()),
            reapply_on_first_connect: false,
        },
    );
    assert!(session.wait_connected(std::time::Duration::from_secs(3)));
    let state = session.refresh().unwrap();

    let store = PresetStore::from_paths(&paths);
    let saved = store.save_current("mine", &state).unwrap();
    let h = saved.headphones.as_ref().unwrap();
    assert_eq!(h.features["eq_16k"], -3.5);
    assert_eq!(h.features["surround_distance"], 175.5);
    assert_eq!(h.sbx, Some(true));
    assert_eq!(h.features["eq_preamp"], -2.5);
    assert!(h
        .features
        .keys()
        .all(|k| features::by_name(k).unwrap().family == FAMILY_PLAYBACK));

    let before = snapshot(&fake);
    // Mess everything up, then load the preset.
    set(&fake, "eq", 0.0);
    set(&fake, "eq_62", -9.0);
    set(&fake, "surround_distance", 20.0);
    set(&fake, "smartvol_mode", 0.0);
    fake.press_mask(Mask::VOICE_CLARITY);
    let writes_before = writes(&fake).len();

    let loaded = store.load("mine").unwrap();
    let applied = preset::apply(session.device(), &loaded).unwrap();
    assert_eq!(applied.output, Output::Headphones);
    assert_eq!(snapshot(&fake), before);
    assert_eq!(fake.mask(), Mask::SBX | Mask::VOICE_CLARITY);

    // Only playback features were written (never the mic family), and the
    // pre-amp (96:0a) is among them: it was changed back from the mess below.
    for w in &writes(&fake)[writes_before..] {
        if w[1] == 0x12 {
            assert_eq!(w[4], FAMILY_PLAYBACK, "{w:02x?}");
        }
    }
    assert_eq!(get(&fake, "eq_preamp"), -2.5);
}

#[test]
fn the_pre_amp_is_saved_and_applied_per_output() {
    let fake = FakeDevice::new(); // headphones
    let (dev, _rx) = start(&fake);
    let p = Preset {
        headphones: Some(profile(&[("eq_preamp", -3.0)])),
        speakers: Some(profile(&[("eq_preamp", 2.5)])),
    };
    p.validate().unwrap();
    let json = serde_json::to_string(&p).unwrap();
    assert_eq!(serde_json::from_str::<Preset>(&json).unwrap(), p);

    preset::apply(&dev, &p).unwrap();
    assert_eq!(get(&fake, "eq_preamp"), -3.0);
    fake.switch_output(Output::Speakers);
    let (dev, _rx) = start(&fake);
    preset::apply(&dev, &p).unwrap();
    assert_eq!(get(&fake, "eq_preamp"), 2.5);
}

#[test]
fn apply_write_order_is_scout_off_sbx_features_scout_on() {
    let fake = FakeDevice::new();
    fake.press_mask(Mask::SCOUT);
    let (dev, _rx) = start(&fake);
    let before = fake.written().len();
    let p = Preset {
        headphones: Some(PresetProfile {
            sbx: Some(true),
            scout: Some(false),
            ..profile(&[("eq_1k", 1.0), ("eq", 1.0)])
        }),
        speakers: None,
    };
    let r = preset::apply(&dev, &p).unwrap();
    assert_eq!(r.writes, 4);
    let w: Vec<_> = fake.written()[before..]
        .iter()
        .filter(|f| is_write(f))
        .map(|f| f[..12].to_vec())
        .collect();
    assert_eq!(w.len(), 4);
    assert_eq!(&w[0][..8], &hex("5a 26 05 07 02 00 00 00")[..]); // scout off
    assert_eq!(&w[1][..8], &hex("5a 26 05 07 01 00 01 00")[..]); // sbx on
                                                                 // Features in table order: eq (09) before eq_1k (10).
    assert_eq!(&w[2][1..6], &hex("12 07 01 96 09")[..]);
    assert_eq!(&w[3][1..6], &hex("12 07 01 96 10")[..]);

    // And the other direction: Scout on goes last.
    let before = fake.written().len();
    let p = Preset {
        headphones: Some(PresetProfile {
            scout: Some(true),
            ..profile(&[("bass", 1.0)])
        }),
        speakers: None,
    };
    preset::apply(&dev, &p).unwrap();
    let w: Vec<_> = fake.written()[before..]
        .iter()
        .filter(|f| is_write(f))
        .map(|f| f.to_vec())
        .collect();
    assert_eq!(w.len(), 2);
    assert_eq!(w[0][1], 0x12);
    assert_eq!(&w[1][..8], &hex("5a 26 05 07 02 00 01 00")[..]);
}

#[test]
fn apply_uses_the_profile_of_the_active_output_only() {
    let p = Preset {
        headphones: Some(profile(&[("bass_level", 0.25)])),
        speakers: Some(profile(&[("bass_level", 0.75)])),
    };
    for (out, want) in [(Output::Headphones, 0.25), (Output::Speakers, 0.75)] {
        let fake = FakeDevice::new();
        fake.switch_output(out);
        let (dev, _rx) = start(&fake);
        let r = preset::apply(&dev, &p).unwrap();
        assert_eq!(r.output, out);
        assert_eq!(get(&fake, "bass_level"), want);
    }
}

#[test]
fn apply_does_nothing_when_the_preset_is_invalid_or_has_no_profile() {
    let fake = FakeDevice::new();
    let (dev, _rx) = start(&fake);
    let before = fake.written().len();

    let invalid = Preset {
        headphones: Some(profile(&[("bass", 1.0), ("eq_31", 13.0)])),
        speakers: None,
    };
    assert!(matches!(
        preset::apply(&dev, &invalid),
        Err(preset::Error::Invalid(_))
    ));
    // Invalid content is caught before even the output is read.
    assert_eq!(fake.written().len(), before);

    let speakers_only = Preset {
        speakers: Some(profile(&[("bass", 1.0)])),
        ..Default::default()
    };
    assert_eq!(
        preset::apply(&dev, &speakers_only),
        Err(preset::Error::NoProfile(Output::Headphones))
    );
    assert!(writes(&fake).is_empty());
    assert_eq!(get(&fake, "bass"), 0.0);
}
