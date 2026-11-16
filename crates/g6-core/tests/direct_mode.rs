//! Direct Mode save / restore against the fake device: what gets written,
//! in which order, and what is left alone. Public API only.

mod common;

use common::*;
use g6_core::direct::{self, DirectMode};
use g6_core::protocol::{Mask, Output};
use g6_core::transport::fake::FakeDevice;

/// User state: EQ on, effects on, SBX on, Voice Clarity on.
fn user_setup(fake: &FakeDevice) {
    set(fake, "eq", 1.0);
    set(fake, "eq_1k", 3.0);
    set(fake, "eq_preamp", -2.5);
    set(fake, "surround", 1.0);
    set(fake, "surround_level", 0.6);
    set(fake, "dialog", 1.0);
    set(fake, "smartvol", 1.0);
    set(fake, "crystalizer", 1.0);
    set(fake, "bass", 1.0);
    set(fake, "mic_clarity", 1.0);
    set(fake, "mic_eq", 1.0);
    set(fake, "decoder_mode", 3.0);
    fake.press_mask(Mask::SBX | Mask::VOICE_CLARITY);
}

fn direct_in(t: &TempDir) -> DirectMode {
    DirectMode::new(t.path().join("direct_mode.json"))
}

/// Compact text for a write frame, for order assertions.
fn describe(f: &[u8]) -> String {
    match (f[1], f[3]) {
        (0x26, 0x07) => format!(
            "mask:{}={}",
            match f[4] {
                1 => "sbx",
                2 => "scout",
                other => panic!("unexpected mask target {other:02x}"),
            },
            f[6]
        ),
        (0x12, 0x01) => {
            let name = g6_core::features::lookup(f[4], f[5]).map_or("?", |x| x.name);
            let v = f32::from_le_bytes([f[6], f[7], f[8], f[9]]);
            format!("{name}={v}")
        }
        _ => format!("other:{:02x?}", &f[..8]),
    }
}

fn written_since(fake: &FakeDevice, n: usize) -> Vec<String> {
    fake.written()[n..]
        .iter()
        .filter(|f| is_write(f))
        .map(|f| describe(f))
        .collect()
}

#[test]
fn enable_then_disable_gives_back_every_setting() {
    let fake = FakeDevice::new();
    user_setup(&fake);
    let before = snapshot(&fake);
    let mask_before = fake.mask();
    let (dev, _rx) = start(&fake);
    let t = TempDir::new();
    let d = direct_in(&t);

    d.enable(&dev).unwrap();
    // Only EQ on/off and the SBX bit changed; everything else is as it was.
    let during = snapshot(&fake);
    for ((name, was), (_, now)) in before.iter().zip(&during) {
        if *name == "eq" {
            assert_eq!(*now, 0.0);
        } else {
            assert_eq!(was, now, "{name} changed by enable");
        }
    }
    assert_eq!(fake.mask(), Mask::VOICE_CLARITY);

    d.disable(&dev).unwrap();
    assert_eq!(snapshot(&fake), before);
    assert_eq!(fake.mask(), mask_before);
    assert!(!d.path().exists());
}

#[test]
fn enable_with_scout_on_writes_scout_off_first_and_eq_last() {
    let fake = FakeDevice::new();
    user_setup(&fake);
    fake.press_mask(Mask::SBX | Mask::SCOUT | Mask::VOICE_CLARITY);
    let (dev, _rx) = start(&fake);
    let t = TempDir::new();
    let n = fake.written().len();
    direct_in(&t).enable(&dev).unwrap();
    // The fake's Scout turns SBX off by itself, so no separate SBX write is
    // needed once Scout is off; what matters is that Scout goes first and
    // the EQ last.
    let w = written_since(&fake, n);
    assert_eq!(w.first().map(String::as_str), Some("mask:scout=0"), "{w:?}");
    assert_eq!(w.last().map(String::as_str), Some("eq=0"), "{w:?}");
    assert!(w.len() <= 3, "{w:?}");
    assert_eq!(fake.mask(), Mask::VOICE_CLARITY);
}

#[test]
fn enable_with_sbx_and_eq_on_writes_exactly_sbx_off_then_eq_off() {
    let fake = FakeDevice::new();
    user_setup(&fake);
    let (dev, _rx) = start(&fake);
    let t = TempDir::new();
    let n = fake.written().len();
    direct_in(&t).enable(&dev).unwrap();
    assert_eq!(written_since(&fake, n), ["mask:sbx=0", "eq=0"]);
}

#[test]
fn disable_restores_effects_and_eq_first_then_sbx_then_scout_last() {
    let fake = FakeDevice::new();
    user_setup(&fake);
    fake.press_mask(Mask::SCOUT | Mask::VOICE_CLARITY);
    let (dev, _rx) = start(&fake);
    let t = TempDir::new();
    let d = direct_in(&t);
    d.enable(&dev).unwrap();
    // The state file knows Scout was on.
    assert!(d.saved().unwrap().unwrap().mask.unwrap().scout);

    // Pretend the device switched effects off meanwhile.
    for name in ["surround", "dialog", "smartvol", "crystalizer", "bass"] {
        set(&fake, name, 0.0);
    }
    let n = fake.written().len();
    d.disable(&dev).unwrap();
    let w = written_since(&fake, n);
    let pos = |s: &str| {
        w.iter()
            .position(|x| x == s)
            .unwrap_or_else(|| panic!("{s} not in {w:?}"))
    };
    let last_feature = [
        "surround=1",
        "dialog=1",
        "smartvol=1",
        "crystalizer=1",
        "bass=1",
        "eq=1",
    ]
    .iter()
    .map(|s| pos(s))
    .max()
    .unwrap();
    assert!(last_feature < pos("mask:scout=1"), "{w:?}");
    assert_eq!(
        w.last().map(String::as_str),
        Some("mask:scout=1"),
        "Scout goes last: {w:?}"
    );
    // Features in the documented order: surround, dialog, smartvol, crystalizer, bass, then EQ.
    let order: Vec<usize> = [
        "surround=1",
        "dialog=1",
        "smartvol=1",
        "crystalizer=1",
        "bass=1",
        "eq=1",
    ]
    .iter()
    .map(|s| pos(s))
    .collect();
    assert!(order.windows(2).all(|p| p[0] < p[1]), "{w:?}");
    assert_eq!(fake.mask() & Mask::SCOUT, Mask::SCOUT);
}

#[test]
fn only_effect_toggles_eq_and_the_two_mask_bits_are_ever_written() {
    let fake = FakeDevice::new();
    user_setup(&fake);
    fake.press_mask(Mask::SBX | Mask::SCOUT | Mask::VOICE_CLARITY);
    let (dev, _rx) = start(&fake);
    let t = TempDir::new();
    let d = direct_in(&t);
    let n = fake.written().len();
    d.enable(&dev).unwrap();
    for name in [
        "surround",
        "dialog",
        "smartvol",
        "crystalizer",
        "bass",
        "eq",
    ] {
        set(&fake, name, 0.0);
    }
    d.disable(&dev).unwrap();

    let allowed = [
        "surround",
        "dialog",
        "smartvol",
        "crystalizer",
        "bass",
        "eq",
    ];
    for f in fake.written()[n..].iter().filter(|f| is_write(f)) {
        match f[1] {
            0x12 => {
                assert_eq!(f[4], 0x96, "non-playback family written: {f:02x?}");
                let name = g6_core::features::lookup(f[4], f[5]).unwrap().name;
                assert!(allowed.contains(&name), "{name} written");
            }
            0x26 => assert!(f[4] == 1 || f[4] == 2, "bad mask target {f:02x?}"),
            other => panic!("unexpected write command {other:02x}"),
        }
    }
    // Untouched on the device.
    assert_eq!(get(&fake, "mic_clarity"), 1.0);
    assert_eq!(get(&fake, "mic_eq"), 1.0);
    assert_eq!(get(&fake, "eq_preamp"), -2.5);
    assert_eq!(get(&fake, "decoder_mode"), 3.0);
    assert_eq!(fake.mask() & Mask::VOICE_CLARITY, Mask::VOICE_CLARITY);
}

#[test]
fn everything_already_off_writes_nothing_and_restores_nothing() {
    let fake = FakeDevice::new();
    let (dev, _rx) = start(&fake);
    let t = TempDir::new();
    let d = direct_in(&t);
    d.enable(&dev).unwrap();
    assert!(writes(&fake).is_empty());
    assert!(d.is_active().unwrap());
    d.disable(&dev).unwrap();
    assert!(writes(&fake).is_empty());
    assert!(!d.path().exists());
}

#[test]
fn the_eq_pre_amp_and_bands_are_not_saved_and_not_changed() {
    let fake = FakeDevice::new();
    user_setup(&fake); // pre-amp -2.5, eq_1k 3.0
    let (dev, _rx) = start(&fake);
    let t = TempDir::new();
    let d = direct_in(&t);

    d.enable(&dev).unwrap();
    assert_eq!(get(&fake, "eq_preamp"), -2.5);
    assert_eq!(get(&fake, "eq_1k"), 3.0);
    let text = std::fs::read_to_string(d.path()).unwrap();
    assert!(!text.contains("preamp"), "{text}");
    assert!(!text.contains("eq_1k"), "{text}");

    d.disable(&dev).unwrap();
    assert_eq!(get(&fake, "eq_preamp"), -2.5);
    assert_eq!(get(&fake, "eq_1k"), 3.0);
    assert_eq!(get(&fake, "eq"), 1.0);
}

#[test]
fn saved_file_records_what_was_on_before() {
    let fake = FakeDevice::new();
    user_setup(&fake);
    set(&fake, "dialog", 0.0);
    let (dev, _rx) = start(&fake);
    let t = TempDir::new();
    let d = direct_in(&t);
    d.enable(&dev).unwrap();

    let text = std::fs::read_to_string(d.path()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["active"], true);
    assert_eq!(v["mask"]["sbx"], true);
    assert_eq!(v["mask"]["scout"], false);
    let h = &v["outputs"]["headphones"];
    assert_eq!(h["eq"], true);
    assert_eq!(h["effects"]["surround"], true);
    assert_eq!(h["effects"]["dialog"], false);
    assert_eq!(h["effects"]["bass"], true);
    // Voice Clarity is not part of what is saved.
    assert!(v["mask"].get("voice_clarity").is_none());
}

#[test]
fn speakers_are_saved_under_their_own_key() {
    let fake = FakeDevice::new();
    fake.switch_output(Output::Speakers);
    set(&fake, "eq", 1.0);
    let (dev, _rx) = start(&fake);
    let t = TempDir::new();
    let d = direct_in(&t);
    d.enable(&dev).unwrap();
    let s = d.saved().unwrap().unwrap();
    assert_eq!(s.outputs.keys().collect::<Vec<_>>(), ["speakers"]);
    assert_eq!(d.pending_outputs().unwrap(), [Output::Speakers]);
    d.disable(&dev).unwrap();
    assert_eq!(get(&fake, "eq"), 1.0);
}

#[test]
fn state_survives_a_restart_of_the_program_and_of_the_connection() {
    let fake = FakeDevice::new();
    user_setup(&fake);
    let before = snapshot(&fake);
    let t = TempDir::new();
    {
        let (dev, _rx) = start(&fake);
        direct_in(&t).enable(&dev).unwrap();
        // Program exits here (the Device is dropped), Direct Mode stays on.
    }
    assert_eq!(get(&fake, "eq"), 0.0);
    let (dev, _rx) = start(&fake);
    let d = direct_in(&t);
    assert!(d.is_active().unwrap());
    d.disable(&dev).unwrap();
    assert_eq!(snapshot(&fake), before);
}

#[test]
fn disable_when_not_active_is_an_error_and_writes_nothing() {
    let fake = FakeDevice::new();
    user_setup(&fake);
    let (dev, _rx) = start(&fake);
    let t = TempDir::new();
    assert_eq!(direct_in(&t).disable(&dev), Err(direct::Error::NotActive));
    assert!(writes(&fake).is_empty());
}

#[test]
fn a_corrupt_state_file_stops_enable_before_the_device_is_touched() {
    let fake = FakeDevice::new();
    user_setup(&fake);
    let (dev, _rx) = start(&fake);
    let t = TempDir::new();
    let d = direct_in(&t);
    std::fs::write(d.path(), "{ not json").unwrap();
    assert!(matches!(d.enable(&dev), Err(direct::Error::Store(_))));
    assert!(writes(&fake).is_empty());
    // The broken file is left for the user to look at.
    assert_eq!(std::fs::read_to_string(d.path()).unwrap(), "{ not json");
}

#[test]
fn a_write_that_the_device_rejects_keeps_the_saved_state() {
    let fake = FakeDevice::new();
    user_setup(&fake);
    let before = snapshot(&fake);
    let (dev, _rx) = start(&fake);
    let t = TempDir::new();
    let d = direct_in(&t);
    fake.reject_next_write(0x81);
    assert!(matches!(d.enable(&dev), Err(direct::Error::Incomplete(_))));
    assert!(d.is_active().unwrap(), "saved before the failed write");
    // Retrying disable puts everything back.
    d.disable(&dev).unwrap();
    assert_eq!(snapshot(&fake), before);
    assert_eq!(fake.mask(), Mask::SBX | Mask::VOICE_CLARITY);
}
