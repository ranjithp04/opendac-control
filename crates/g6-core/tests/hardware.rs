//! Hardware integration tests. They need a Creative Sound BlasterX G6
//! (041e:3256) on USB and are therefore `#[ignore]`d:
//!
//! ```text
//! cargo test -p g6-core --test hardware -- --ignored --test-threads=1
//! ```
//!
//! Every test that changes something follows the same pattern: read the
//! original value, write, read back, then restore the original. The restore
//! runs in a `Drop` guard or after `catch_unwind`, so a failed assertion does
//! not leave the device changed. Only interface 4 is opened (by
//! `HidConnector`); only commands from the project plan §7.2 are used. Tests share one
//! device and serialise on a lock, so `--test-threads=1` is optional but
//! harmless.

mod common;

use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::sync::mpsc::Receiver;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use common::TempDir;
use g6_core::direct::DirectMode;
use g6_core::events::{Config, Device, Event};
use g6_core::features::{self, Feature, Kind, FEATURES};
use g6_core::preset::{self, Preset, PresetProfile};
use g6_core::protocol::{DacFilter, Mask, MaskTarget, MicBoost};

static DEVICE_LOCK: Mutex<()> = Mutex::new(());

/// An open device plus the lock that keeps other hardware tests away.
struct Rig {
    dev: Device,
    /// Kept so the device thread always has a receiver.
    _events: Receiver<Event>,
    _lock: MutexGuard<'static, ()>,
}

fn open() -> Rig {
    let lock = DEVICE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (dev, events) = Device::open_hid(Config::default());
    // Connect retries with backoff; the first reads after plug-in can fail.
    loop {
        match events.recv_timeout(Duration::from_secs(10)) {
            Ok(Event::Connected) => break,
            Ok(_) => continue,
            Err(_) => {
                panic!("no G6 (041e:3256, interface 4) answered within 10 s; is it plugged in?")
            }
        }
    }
    Rig {
        dev,
        _events: events,
        _lock: lock,
    }
}

fn feat(name: &str) -> &'static Feature {
    features::by_name(name).unwrap_or_else(|| panic!("no feature {name}"))
}

/// Every table feature, read from the device, by name.
fn read_all(dev: &Device) -> Vec<(&'static str, f32)> {
    FEATURES
        .iter()
        .map(|f| {
            let v = dev
                .read_feature(f)
                .unwrap_or_else(|e| panic!("read {}: {e}", f.name));
            (f.name, v)
        })
        .collect()
}

/// Restores a feature value when dropped, whatever happened in between.
struct FeatureGuard<'a> {
    dev: &'a Device,
    feature: &'static Feature,
    original: f32,
}

impl<'a> FeatureGuard<'a> {
    fn new(dev: &'a Device, name: &str) -> Self {
        let feature = feat(name);
        let original = dev
            .read_feature(feature)
            .unwrap_or_else(|e| panic!("read original {name}: {e}"));
        FeatureGuard {
            dev,
            feature,
            original,
        }
    }
}

impl Drop for FeatureGuard<'_> {
    fn drop(&mut self) {
        if self.feature.exposed {
            if let Err(e) = self.dev.write_feature(self.feature, self.original) {
                eprintln!("RESTORE FAILED for {}: {e}", self.feature.name);
            }
        }
    }
}

/// A valid value different from `original`.
fn other_value(f: &Feature, original: f32) -> f32 {
    match f.kind {
        Kind::Bool => 1.0 - f.round(original),
        Kind::Choice(list) => list
            .iter()
            .map(|(v, _)| *v as f32)
            .find(|v| *v != f.round(original))
            .expect("choice with one value"),
        Kind::Level | Kind::Db | Kind::Distance => {
            let up = f.round(original + 10.0 * f.step);
            if up <= f.max {
                up
            } else {
                f.round(original - 10.0 * f.step)
            }
        }
    }
}

/// read original -> write other -> read back -> restore -> read back.
fn feature_roundtrip(dev: &Device, name: &str) {
    let guard = FeatureGuard::new(dev, name);
    let f = guard.feature;
    let original = f.round(guard.original);
    assert!(
        f.validate(original).is_ok(),
        "{name}: device value {original} is outside the table range"
    );
    let new = other_value(f, original);
    assert_ne!(new, original, "{name}: test value must differ");

    let written = dev
        .write_feature(f, new)
        .unwrap_or_else(|e| panic!("write {name}={new}: {e}"));
    assert_eq!(written.value, new, "{name}: confirmed value");
    assert_eq!(dev.read_feature(f).unwrap(), new, "{name}: read back");

    dev.write_feature(f, original)
        .unwrap_or_else(|e| panic!("restore {name}={original}: {e}"));
    assert_eq!(dev.read_feature(f).unwrap(), original, "{name}: restored");
    drop(guard);
}

// ---------------------------------------------------------------------------
// Read-only
// ---------------------------------------------------------------------------

#[test]
#[ignore = "needs a G6 on USB"]
fn hw_identity_firmware_and_ids() {
    let rig = open();
    let id = rig.dev.identity().unwrap();
    assert_eq!(id.len(), 4, "identity payload {id:02x?}");
    let fw = rig.dev.firmware().unwrap();
    assert!(!fw.is_empty() && fw.contains('.'), "firmware {fw:?}");
    assert!(!fw.contains('\0'), "NUL must be stripped");
    assert!(!rig.dev.hw_id().unwrap().is_empty());
    assert!(!rig.dev.dsp_version().unwrap().is_empty());
}

#[test]
#[ignore = "needs a G6 on USB"]
fn hw_every_feature_reads_and_is_in_range_after_rounding() {
    let rig = open();
    for f in FEATURES {
        let v = rig
            .dev
            .read_feature(f)
            .unwrap_or_else(|e| panic!("read {}: {e}", f.name));
        // The events layer already rounds what it reads.
        assert_eq!(f.round(v), v, "{}: not rounded", f.name);
        assert!(
            f.validate(v).is_ok(),
            "{}: device reports {v}, outside {}..={}",
            f.name,
            f.min,
            f.max
        );
    }
}

#[test]
#[ignore = "needs a G6 on USB"]
fn hw_mask_output_filter_and_boost_read() {
    let rig = open();
    rig.dev.read_mask().unwrap();
    rig.dev.read_output().unwrap();
    rig.dev.read_dac_filter().unwrap();
    rig.dev.read_mic_boost().unwrap();
}

#[test]
#[ignore = "needs a G6 on USB"]
fn hw_reading_twice_gives_the_same_state() {
    let rig = open();
    assert_eq!(read_all(&rig.dev), read_all(&rig.dev));
}

#[test]
#[ignore = "needs a G6 on USB"]
fn hw_reconnect_after_dropping_the_device() {
    let first = open();
    let fw = first.dev.firmware().unwrap();
    drop(first); // releases the lock too
    let second = open();
    assert_eq!(second.dev.firmware().unwrap(), fw);
}

// ---------------------------------------------------------------------------
// Write, read back, restore
// ---------------------------------------------------------------------------

#[test]
#[ignore = "needs a G6 on USB; briefly changes and restores EQ bands"]
fn hw_eq_bands_roundtrip() {
    let rig = open();
    for name in ["eq_31", "eq_1k", "eq_16k"] {
        feature_roundtrip(&rig.dev, name);
    }
}

#[test]
#[ignore = "needs a G6 on USB; briefly changes and restores EQ on/off"]
fn hw_eq_enable_roundtrip() {
    let rig = open();
    feature_roundtrip(&rig.dev, "eq");
}

#[test]
#[ignore = "needs a G6 on USB; briefly changes and restores effect settings"]
fn hw_effect_levels_and_toggles_roundtrip() {
    let rig = open();
    for name in [
        "surround_level",
        "surround_distance",
        "dialog_level",
        "smartvol_level",
        "smartvol_mode",
        "crystalizer_level",
        "bass_level",
        "surround",
        "dialog",
        "crystalizer",
        "bass",
    ] {
        feature_roundtrip(&rig.dev, name);
    }
}

#[test]
#[ignore = "needs a G6 on USB; briefly changes and restores mic settings"]
fn hw_mic_features_roundtrip() {
    let rig = open();
    for name in ["mic_aec", "mic_clarity", "mic_nr_level", "mic_eq"] {
        feature_roundtrip(&rig.dev, name);
    }
}

#[test]
#[ignore = "needs a G6 on USB; briefly changes and restores the decoder mode"]
fn hw_decoder_mode_roundtrip() {
    let rig = open();
    feature_roundtrip(&rig.dev, "decoder_mode");
}

#[test]
#[ignore = "needs a G6 on USB; the pre-amp must be refused and stay unchanged"]
fn hw_preamp_is_refused_and_unchanged() {
    let rig = open();
    let f = feat("eq_preamp");
    let before = rig.dev.read_feature(f).unwrap();
    assert!(rig.dev.write_feature(f, before + 0.5).is_err());
    assert_eq!(rig.dev.read_feature(f).unwrap(), before);
}

#[test]
#[ignore = "needs a G6 on USB; out-of-range values must be refused and change nothing"]
fn hw_out_of_range_writes_are_refused_and_change_nothing() {
    let rig = open();
    let before = read_all(&rig.dev);
    assert!(rig.dev.write_feature(feat("eq_1k"), 12.5).is_err());
    assert!(rig
        .dev
        .write_feature(feat("surround_distance"), 5.0)
        .is_err());
    assert!(rig.dev.write_feature(feat("decoder_mode"), 0.0).is_err());
    assert_eq!(read_all(&rig.dev), before);
}

/// Restores the SBX / Scout bits when dropped.
struct MaskGuard<'a> {
    dev: &'a Device,
    original: Mask,
}

impl Drop for MaskGuard<'_> {
    fn drop(&mut self) {
        // Scout first: turning it off lets the device restore its own state.
        for (target, want) in [
            (MaskTarget::Scout, self.original.scout()),
            (MaskTarget::Sbx, self.original.sbx()),
        ] {
            let Ok(now) = self.dev.read_mask() else {
                return;
            };
            let have = match target {
                MaskTarget::Sbx => now.sbx(),
                MaskTarget::Scout => now.scout(),
            };
            if have != want {
                if let Err(e) = self.dev.set_mask(target, want) {
                    eprintln!("RESTORE FAILED for {target:?}: {e}");
                }
            }
        }
    }
}

#[test]
#[ignore = "needs a G6 on USB; documented, not yet verified: SBX write. Briefly toggles SBX"]
fn hw_sbx_roundtrip() {
    let rig = open();
    let original = rig.dev.read_mask().unwrap();
    let _guard = MaskGuard {
        dev: &rig.dev,
        original,
    };
    let want = !original.sbx();
    let w = rig.dev.set_mask(MaskTarget::Sbx, want).unwrap();
    assert_eq!(w.value.sbx(), want);
    let now = rig.dev.read_mask().unwrap();
    assert_eq!(now.sbx(), want, "read back");
    // The mic bit is not ours to touch.
    assert_eq!(now.voice_clarity(), original.voice_clarity());
}

#[test]
#[ignore = "needs a G6 on USB; documented, not yet verified: Scout write. Briefly toggles Scout"]
fn hw_scout_roundtrip_leaves_playback_settings_as_they_were() {
    let rig = open();
    let original = rig.dev.read_mask().unwrap();
    let before = read_all(&rig.dev);
    {
        let _guard = MaskGuard {
            dev: &rig.dev,
            original,
        };
        let want = !original.scout();
        let w = rig.dev.set_mask(MaskTarget::Scout, want).unwrap();
        assert_eq!(w.value.scout(), want);
        assert_eq!(rig.dev.read_mask().unwrap().scout(), want, "read back");
    }
    assert_eq!(rig.dev.read_mask().unwrap(), original, "mask restored");
    // the project plan §6: Scout writes its curve into the EQ registers and the device
    // restores them itself when Scout goes off.
    assert_eq!(read_all(&rig.dev), before, "settings restored");
}

#[test]
#[ignore = "needs a G6 on USB; documented, not yet verified: DAC filter write"]
fn hw_dac_filter_roundtrip() {
    let rig = open();
    let original = rig.dev.read_dac_filter().unwrap();
    let new = DacFilter::ALL.into_iter().find(|f| *f != original).unwrap();
    let result = catch_unwind(AssertUnwindSafe(|| {
        let w = rig.dev.set_dac_filter(new).unwrap();
        assert_eq!(w.value, new);
        assert_eq!(rig.dev.read_dac_filter().unwrap(), new, "read back");
    }));
    rig.dev.set_dac_filter(original).unwrap();
    assert_eq!(rig.dev.read_dac_filter().unwrap(), original, "restored");
    if let Err(p) = result {
        resume_unwind(p);
    }
}

#[test]
#[ignore = "needs a G6 on USB; documented, not yet verified: mic boost write"]
fn hw_mic_boost_roundtrip() {
    let rig = open();
    let original = rig.dev.read_mic_boost().unwrap();
    let new = MicBoost::ALL.into_iter().find(|b| *b != original).unwrap();
    let result = catch_unwind(AssertUnwindSafe(|| {
        let w = rig.dev.set_mic_boost(new).unwrap();
        assert_eq!(w.value, new);
        assert_eq!(rig.dev.read_mic_boost().unwrap(), new, "read back");
    }));
    rig.dev.set_mic_boost(original).unwrap();
    assert_eq!(rig.dev.read_mic_boost().unwrap(), original, "restored");
    if let Err(p) = result {
        resume_unwind(p);
    }
}

// ---------------------------------------------------------------------------
// Presets and Direct Mode
// ---------------------------------------------------------------------------

#[test]
#[ignore = "needs a G6 on USB; applies a preset and puts the previous values back"]
fn hw_preset_apply_and_restore() {
    let rig = open();
    let dev = &rig.dev;
    let before = read_all(dev);

    // "What it is now" (for the restore) and a changed set of values.
    let names = ["eq_31", "eq_1k", "bass_level", "surround_level"];
    let current: Vec<(String, f32)> = names
        .iter()
        .map(|n| (n.to_string(), dev.read_feature(feat(n)).unwrap()))
        .collect();
    let changed: Vec<(String, f32)> = current
        .iter()
        .map(|(n, v)| (n.clone(), other_value(feat(n), *v)))
        .collect();
    let mk = |pairs: &[(String, f32)]| {
        let p = PresetProfile {
            features: pairs.iter().cloned().collect(),
            ..Default::default()
        };
        // Same profile for both outputs: whichever is active gets applied.
        Preset {
            headphones: Some(p.clone()),
            speakers: Some(p),
        }
    };
    let restore = mk(&current);
    let change = mk(&changed);

    let result = catch_unwind(AssertUnwindSafe(|| {
        preset::apply(dev, &change).unwrap();
        for (n, v) in &changed {
            assert_eq!(dev.read_feature(feat(n)).unwrap(), *v, "{n} after apply");
        }
    }));
    preset::apply(dev, &restore).unwrap();
    assert_eq!(read_all(dev), before, "restored");
    if let Err(p) = result {
        resume_unwind(p);
    }
}

#[test]
#[ignore = "needs a G6 on USB; turns SBX, Scout and EQ off and restores them"]
fn hw_direct_mode_enable_disable_restores_everything() {
    let rig = open();
    let dev = &rig.dev;
    let features_before = read_all(dev);
    let mask_before = dev.read_mask().unwrap();
    let t = TempDir::new();
    let d = DirectMode::new(t.path().join("direct_mode.json"));

    let result = catch_unwind(AssertUnwindSafe(|| {
        d.enable(dev).unwrap();
        let mask = dev.read_mask().unwrap();
        assert!(!mask.sbx() && !mask.scout(), "SBX/Scout off: {mask:?}");
        assert_eq!(
            mask.voice_clarity(),
            mask_before.voice_clarity(),
            "Voice Clarity is not ours"
        );
        assert_eq!(dev.read_feature(feat("eq")).unwrap(), 0.0, "EQ off");
        assert!(d.is_active().unwrap());
    }));
    // Always put the user's state back, even if an assertion failed.
    if d.is_active().unwrap_or(false) {
        d.disable(dev).unwrap();
    }
    assert!(!d.path().exists(), "saved state removed after restore");
    assert_eq!(dev.read_mask().unwrap(), mask_before, "mask restored");
    assert_eq!(read_all(dev), features_before, "settings restored");
    if let Err(p) = result {
        resume_unwind(p);
    }
}

#[test]
#[ignore = "needs a G6 on USB; two Direct Mode cycles keep the state stable"]
fn hw_direct_mode_twice_is_stable() {
    let rig = open();
    let dev = &rig.dev;
    let before = (read_all(dev), dev.read_mask().unwrap());
    let t = TempDir::new();
    let d = DirectMode::new(t.path().join("direct_mode.json"));
    for _ in 0..2 {
        d.enable(dev).unwrap();
        d.disable(dev).unwrap();
        assert_eq!((read_all(dev), dev.read_mask().unwrap()), before);
    }
}
