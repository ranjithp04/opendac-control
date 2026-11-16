//! Feature table: rounding of float noise, range rejection, EQ band names.
//! Includes the end-to-end check that invalid writes never reach the device.

mod common;

use common::*;
use g6_core::events::Error;
use g6_core::features::*;
use g6_core::protocol::{decode, encode, Message, ProtocolError};
use g6_core::transport::fake::FakeDevice;

/// Ideal (noise free) values to test a feature with.
fn ideal_values(f: &Feature) -> Vec<f32> {
    match f.kind {
        Kind::Bool => vec![0.0, 1.0],
        Kind::Choice(list) => list.iter().map(|(v, _)| *v as f32).collect(),
        Kind::Level => {
            let mut v = vec![0.0, 0.01, 0.25, 0.9, f.max];
            v.retain(|x| *x <= f.max);
            v
        }
        Kind::Db => vec![f.min, -1.5, -0.5, 0.0, 0.5, 1.5, f.max],
        Kind::Distance => vec![f.min, 10.5, 150.0, 150.5, f.max],
    }
}

/// Device-style noise: a few ulps either way and a ~1.8e-7 absolute offset
/// (`0x34400000`, the plan's example).
fn noisy(v: f32) -> Vec<f32> {
    let bits = v.to_bits();
    let mut out = vec![
        f32::from_bits(bits.wrapping_add(1)),
        f32::from_bits(bits.wrapping_add(2)),
        f32::from_bits(bits.wrapping_sub(1)),
        f32::from_bits(bits.wrapping_sub(2)),
        v + f32::from_bits(0x3440_0000),
        v - f32::from_bits(0x3440_0000),
    ];
    if v == 0.0 {
        // wrapping_sub on +0.0 is garbage; use small signed values instead.
        out = vec![
            f32::from_bits(0x3440_0000),
            -f32::from_bits(0x3440_0000),
            -0.0,
            1.0e-9,
            -1.0e-9,
        ];
    }
    out
}

#[test]
fn rounding_turns_device_noise_back_into_the_ideal_value() {
    for f in FEATURES {
        for ideal in ideal_values(f) {
            for n in noisy(ideal) {
                assert_eq!(
                    f.round(n),
                    ideal,
                    "{}: {n:e} should round to {ideal}",
                    f.name
                );
            }
            assert_eq!(f.round(ideal), ideal, "{}: rounding is idempotent", f.name);
        }
    }
}

#[test]
fn rounding_never_produces_negative_zero() {
    for f in FEATURES {
        for v in [-0.0f32, -1.0e-7, 1.0e-7, f32::from_bits(0x3440_0000)] {
            let r = f.round(v);
            assert_eq!(r, 0.0, "{}", f.name);
            assert!(r.is_sign_positive(), "{}: got -0", f.name);
        }
    }
}

#[test]
fn noise_survives_the_wire_and_known_cleans_it() {
    // Encode a noisy value into a `5a 11` reply, decode, round via known().
    for f in FEATURES {
        for ideal in ideal_values(f) {
            for n in noisy(ideal) {
                let b = n.to_le_bytes();
                let frame =
                    encode(0x11, &[0x01, 0x00, f.family, f.id, b[0], b[1], b[2], b[3]]).unwrap();
                let Message::Features(v) = decode(&frame) else {
                    panic!("{}", f.name)
                };
                let (got, val) = v[0].known().unwrap();
                assert_eq!(got.name, f.name);
                assert_eq!(val, ideal, "{} noisy {n:e}", f.name);
            }
        }
    }
}

#[test]
fn level_rounding_is_to_hundredths() {
    let lvl = by_name("bass_level").unwrap();
    for (raw, want) in [
        (0.004, 0.0),
        (0.006, 0.01),
        (0.994, 0.99),
        (0.996, 1.0),
        (0.3333, 0.33),
        (0.6666, 0.67),
    ] {
        assert_eq!(lvl.round(raw), want, "{raw}");
    }
}

#[test]
fn db_and_distance_rounding_is_to_half_steps() {
    let eq = by_name("eq_1k").unwrap();
    for (raw, want) in [
        (0.24, 0.0),
        (0.26, 0.5),
        (0.74, 0.5),
        (0.76, 1.0),
        (-0.76, -1.0),
        (-11.9, -12.0),
    ] {
        assert_eq!(eq.round(raw), want, "{raw}");
    }
    let d = by_name("surround_distance").unwrap();
    for (raw, want) in [(10.2, 10.0), (10.3, 10.5), (299.8, 300.0), (150.74, 150.5)] {
        assert_eq!(d.round(raw), want, "{raw}");
    }
}

// ---------------------------------------------------------------------------
// Range rejection
// ---------------------------------------------------------------------------

#[test]
fn range_limits_match_plan_7_3() {
    let expect: &[(&str, f32, f32)] = &[
        ("surround", 0.0, 1.0),
        ("surround_level", 0.0, 1.0),
        ("surround_distance", 10.0, 300.0),
        ("dialog_level", 0.0, 1.0),
        ("smartvol_level", 0.0, 1.0),
        ("smartvol_mode", 0.0, 2.0),
        ("crystalizer_level", 0.0, 1.0),
        ("bass_level", 0.0, 1.0),
        ("eq", 0.0, 1.0),
        ("eq_preamp", -6.0, 6.0),
        ("eq_31", -12.0, 12.0),
        ("eq_16k", -12.0, 12.0),
        ("mic_nr_level", 0.0, 0.5),
        ("mic_eq_1", -12.0, 12.0),
        ("mic_eq_8", -12.0, 12.0),
        ("decoder_mode", 1.0, 3.0),
    ];
    for (name, lo, hi) in expect {
        let f = by_name(name).unwrap();
        assert_eq!((f.min, f.max), (*lo, *hi), "{name}");
    }
    // Steps: 0.5 dB for EQ bands, pre-amp and distance.
    for name in [
        "eq_31",
        "eq_16k",
        "eq_preamp",
        "surround_distance",
        "mic_eq_4",
    ] {
        assert_eq!(by_name(name).unwrap().step, 0.5, "{name}");
    }
}

#[test]
fn every_exposed_feature_rejects_values_outside_its_range() {
    for f in FEATURES.iter().filter(|f| f.exposed) {
        if matches!(f.kind, Kind::Choice(_)) {
            continue; // covered below
        }
        // One full step beyond either end is out of range even after rounding.
        for v in [
            f.max + f.step,
            f.min - f.step,
            f.max * 10.0 + 100.0,
            -1000.0,
        ] {
            if v >= f.min && v <= f.max {
                continue;
            }
            assert!(
                matches!(f.validate_write(v), Err(FeatureError::OutOfRange { .. })),
                "{}: {v} must be rejected",
                f.name
            );
        }
        // The ends themselves are fine.
        assert_eq!(f.validate_write(f.min), Ok(f.min), "{}", f.name);
        assert_eq!(f.validate_write(f.max), Ok(f.max), "{}", f.name);
    }
}

#[test]
fn non_finite_values_are_rejected_for_every_feature() {
    for f in FEATURES {
        for v in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(
                f.validate(v),
                Err(FeatureError::NotFinite(f.name)),
                "{}",
                f.name
            );
            assert!(f.validate_write(v).is_err(), "{}", f.name);
        }
    }
}

#[test]
fn enum_features_accept_only_listed_values() {
    for name in ["smartvol_mode", "decoder_mode"] {
        let f = by_name(name).unwrap();
        let Kind::Choice(list) = f.kind else { panic!() };
        for n in -3..=6 {
            let listed = list.iter().any(|(v, _)| *v as i32 == n);
            let r = f.validate_write(n as f32);
            assert_eq!(r.is_ok(), listed, "{name}={n}: {r:?}");
        }
    }
    // smart volume: 0 normal, 1 loud, 2 night; decoder: 1 full, 2 normal, 3 night.
    let sv = by_name("smartvol_mode").unwrap();
    assert_eq!(
        [0.0, 1.0, 2.0].map(|v| sv.choice_name(v).unwrap()),
        ["normal", "loud", "night"]
    );
    let dm = by_name("decoder_mode").unwrap();
    assert_eq!(
        [1.0, 2.0, 3.0].map(|v| dm.choice_name(v).unwrap()),
        ["full", "normal", "night"]
    );
    assert_eq!(dm.choice_name(0.0), None);
    assert_eq!(dm.choice_value("bogus"), None);
    assert_eq!(by_name("eq").unwrap().choice_name(1.0), None);
}

#[test]
fn bool_features_reject_anything_but_zero_or_one() {
    for f in FEATURES.iter().filter(|f| f.kind == Kind::Bool) {
        assert_eq!(f.validate_write(0.0), Ok(0.0));
        assert_eq!(f.validate_write(1.0), Ok(1.0));
        for v in [2.0, -1.0, 5.0, 255.0] {
            assert!(f.validate_write(v).is_err(), "{}={v}", f.name);
        }
    }
}

#[test]
fn eq_preamp_is_readable_but_not_writable() {
    let p = by_name("eq_preamp").unwrap();
    assert!(!p.exposed);
    assert_eq!(p.validate(-2.5), Ok(-2.5));
    assert_eq!(
        p.validate_write(-2.5),
        Err(FeatureError::NotExposed("eq_preamp"))
    );
    assert_eq!(FEATURES.iter().filter(|f| !f.exposed).count(), 1);
}

#[test]
fn mic_noise_reduction_level_is_scaled_for_the_ui() {
    let nr = by_name("mic_nr_level").unwrap();
    assert_eq!(nr.ui_scale, 2.0);
    for (device, ui) in [(0.0, 0.0), (0.25, 0.5), (0.5, 1.0)] {
        assert_eq!(nr.to_ui(device), ui);
        assert_eq!(nr.from_ui(ui), device);
    }
    // Every other feature is 1:1.
    for f in FEATURES.iter().filter(|f| f.name != "mic_nr_level") {
        assert_eq!(f.ui_scale, 1.0, "{}", f.name);
    }
}

// ---------------------------------------------------------------------------
// EQ band names
// ---------------------------------------------------------------------------

#[test]
fn eq_band_names_map_to_ids_0b_to_14() {
    let names = [
        "31", "62", "125", "250", "500", "1k", "2k", "4k", "8k", "16k",
    ];
    assert_eq!(EQ_BANDS, names);
    assert_eq!(EQ_FIRST_ID, 0x0b);
    for (i, n) in names.iter().enumerate() {
        for spelling in [
            n.to_string(),
            n.to_uppercase(),
            format!("{n}Hz"),
            format!("{n}hz"),
            format!(" {n} "),
        ] {
            assert_eq!(parse_eq_band(&spelling), Some(i), "{spelling:?}");
        }
        let f = eq_band(i).unwrap();
        assert_eq!((f.family, f.id), (0x96, 0x0b + i as u8));
    }
    for (spelling, i) in [
        ("1000", 5),
        ("2000", 6),
        ("4000", 7),
        ("8000", 8),
        ("16000", 9),
    ] {
        assert_eq!(parse_eq_band(spelling), Some(i), "{spelling}");
    }
    for bad in [
        "", "0", "30", "32", "63", "100", "3k", "32k", "1kk", "k", "1 k", "-1k", "1.5k", "1khzz",
        "eq_1k",
    ] {
        assert_eq!(parse_eq_band(bad), None, "{bad:?}");
    }
    assert!(eq_band(10).is_none());
}

// ---------------------------------------------------------------------------
// Device level: invalid writes never reach the wire
// ---------------------------------------------------------------------------

#[test]
fn invalid_writes_never_reach_the_device() {
    let fake = FakeDevice::new();
    let (dev, _rx) = start(&fake);
    let before = fake.written().len();
    for f in FEATURES {
        let bad: Vec<f32> = if f.exposed {
            vec![f.max + 100.0, f.min - 100.0, f32::NAN, f32::INFINITY]
        } else {
            vec![0.0] // not exposed: rejected whatever the value
        };
        for v in bad {
            let r = dev.write_feature(f, v);
            assert!(
                matches!(r, Err(Error::Protocol(ProtocolError::Feature(_)))),
                "{}={v}: {r:?}",
                f.name
            );
        }
    }
    assert_eq!(fake.written().len(), before, "a rejected value was sent");
}
