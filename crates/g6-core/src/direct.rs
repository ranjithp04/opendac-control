//! Our Direct Mode: save state, disable SBX / Scout / EQ, restore on exit.
//!
//! Enabling saves {SBX / Scout mask, EQ enable, effect toggles} to disk
//! **before** anything on the device is changed, so a crash or a failed
//! write never loses the user's settings; then it switches Scout, SBX and EQ
//! off. Disabling restores what was saved and deletes the file. The mic is
//! never touched (the Voice Clarity bit of the mask is left alone), and
//! neither is the EQ pre-amp or any EQ band: only the EQ on/off switch.
//!
//! Creative's hardware Direct Mode (`0x39`, `docs/protocol-g6.md` §2.2a) is an
//! extra step: switched on last when enabling (and verified by reading it
//! back), and off first when disabling. It changes only the audio path, so a
//! failing step when enabling is logged and the software Direct Mode carries
//! on. When disabling, a hardware mode that is still on after the step is a
//! failed step: the saved state is kept so `disable` can be retried.
//!
//! Disabling switches Scout off before restoring anything (the Scout button
//! still works while Direct Mode is on), and turns it back on last if it was
//! on before Direct Mode. The same holds for a restore still pending for an
//! output after Direct Mode is off: with Scout on, it is switched off, the
//! restore written, and Scout switched on again.
//!
//! Playback settings are per output. The saved state therefore has one entry
//! per output that was active while Direct Mode was on, and the mask is saved
//! once. If the headphone jack switches the output while Direct Mode is on,
//! call [`DirectMode::enable`] again: it is idempotent, it adds an entry for
//! the new output without overwriting existing ones, and switches EQ off
//! there too. [`DirectMode::disable`] restores the mask and the active
//! output's entry; entries of other outputs stay on disk (`active: false`)
//! until [`DirectMode::disable`] runs while that output is active.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::events::{self, Device};
use crate::features::{self, Feature};
use crate::protocol::{MaskTarget, Output};
use crate::state::{output_from_name, output_name};
use crate::store::{self, StoreError};

/// Effect on/off toggles that are saved (and restored) with the EQ enable.
pub const EFFECT_TOGGLES: [&str; 5] = ["surround", "dialog", "smartvol", "crystalizer", "bass"];

const EQ: &str = "eq";
const FORMAT_VERSION: u32 = 1;

/// Mask bits as they were before Direct Mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedMask {
    pub sbx: bool,
    pub scout: bool,
}

/// One output's settings as they were before Direct Mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedOutput {
    pub eq: bool,
    /// Effect name (see [`EFFECT_TOGGLES`]) → on.
    pub effects: BTreeMap<String, bool>,
}

/// The file written to disk before the device is touched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedState {
    pub version: u32,
    /// Direct Mode is on (`false` = only restores for other outputs pending).
    pub active: bool,
    /// Mask before Direct Mode; `None` when not active.
    pub mask: Option<SavedMask>,
    /// Keyed by output name (`headphones` / `speakers`).
    pub outputs: BTreeMap<String, SavedOutput>,
    /// Direct Mode is off, but a restore switched Scout off and it still has
    /// to go back on (the step failed; `disable` finishes it). Absent in
    /// older files and when `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub scout_back: bool,
}

/// Direct Mode errors.
#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    Device(events::Error),
    Store(StoreError),
    /// Nothing saved: Direct Mode is not on.
    NotActive,
    /// Some steps failed; the saved state is kept so `disable` can be retried.
    Incomplete(Vec<String>),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Device(e) => e.fmt(f),
            Error::Store(e) => e.fmt(f),
            Error::NotActive => write!(f, "direct mode is not on"),
            Error::Incomplete(v) => write!(f, "direct mode incomplete: {}", v.join("; ")),
        }
    }
}

impl std::error::Error for Error {}

impl From<events::Error> for Error {
    fn from(e: events::Error) -> Self {
        Error::Device(e)
    }
}

impl From<StoreError> for Error {
    fn from(e: StoreError) -> Self {
        Error::Store(e)
    }
}

/// Handle to Direct Mode's saved-state file.
#[derive(Debug, Clone)]
pub struct DirectMode {
    path: PathBuf,
}

fn feat(name: &str) -> &'static Feature {
    features::by_name(name).expect("direct mode feature is in the table")
}

fn on(v: f32) -> bool {
    v >= 0.5
}

/// EQ enable and effect toggles of the active output.
fn read_output_settings(dev: &Device) -> Result<SavedOutput, events::Error> {
    let eq = on(dev.read_feature(feat(EQ))?);
    let mut effects = BTreeMap::new();
    for name in EFFECT_TOGGLES {
        effects.insert(name.to_string(), on(dev.read_feature(feat(name))?));
    }
    Ok(SavedOutput { eq, effects })
}

/// Errors that mean the device is gone: no point trying further steps.
fn fatal(e: &events::Error) -> bool {
    matches!(
        e,
        events::Error::NotConnected | events::Error::Transport(_) | events::Error::Shutdown
    )
}

/// Switch the hardware Direct Mode, best effort: a step the device refuses
/// is logged, not an error. A lost device is still one.
fn hw_direct(dev: &Device, on: bool) -> Result<(), Error> {
    match dev.set_hw_direct(on) {
        Ok(_) => Ok(()),
        Err(e) if fatal(&e) => Err(Error::Device(e)),
        Err(e) => {
            log::warn!(
                "hardware direct mode {}: {e}",
                if on { "on" } else { "off" }
            );
            Ok(())
        }
    }
}

/// Collects step failures; stops early when the device is lost.
struct Steps(Vec<String>);

impl Steps {
    fn run<T>(&mut self, what: &str, r: Result<T, events::Error>) -> Result<Option<T>, Error> {
        match r {
            Ok(v) => Ok(Some(v)),
            Err(e) if fatal(&e) => Err(Error::Device(e)),
            Err(e) => {
                self.0.push(format!("{what}: {e}"));
                Ok(None)
            }
        }
    }

    fn finish(self) -> Result<(), Error> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(Error::Incomplete(self.0))
        }
    }
}

impl DirectMode {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        DirectMode { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The saved state, if any.
    pub fn saved(&self) -> Result<Option<SavedState>, StoreError> {
        store::read_json(&self.path)
    }

    /// Direct Mode is on.
    pub fn is_active(&self) -> Result<bool, StoreError> {
        Ok(self.saved()?.is_some_and(|s| s.active))
    }

    /// Turn Direct Mode on (idempotent; see the module docs).
    ///
    /// With Scout on, the EQ enable and Smart Volume the device shows are
    /// Scout's, not the user's. The entry saved first (so the file is on
    /// disk before any write) holds them; after Scout is switched off and
    /// the device has restored the user's settings they are read again and
    /// the saved entry is replaced.
    pub fn enable(&self, dev: &Device) -> Result<(), Error> {
        // 1. Read what we are about to change.
        let output = dev.read_output()?;
        let mask = dev.read_mask()?;
        let current = read_output_settings(dev)?;
        let key = output_name(output).to_string();

        // 2. Persist BEFORE the first device write. Existing entries win:
        //    they hold the user's settings, the device now shows Direct Mode's.
        let mut saved = self.saved()?.unwrap_or(SavedState {
            version: FORMAT_VERSION,
            active: false,
            mask: None,
            outputs: BTreeMap::new(),
            scout_back: false,
        });
        let first_save = !saved.active;
        if first_save {
            saved.active = true;
            saved.mask = Some(SavedMask {
                sbx: mask.sbx(),
                // Off only because a restore switched it off: it is the user's.
                scout: mask.scout() || saved.scout_back,
            });
            saved.scout_back = false;
        }
        let new_entry = !saved.outputs.contains_key(&key);
        saved.outputs.entry(key.clone()).or_insert(current);
        store::write_json(&self.path, &saved)?;

        // 3. Switch off. Scout first: turning it off makes the device restore
        //    its previous EQ / SBX state, so EQ is read again afterwards.
        let mut steps = Steps(Vec::new());
        let mut sbx = mask.sbx();
        if mask.scout() {
            if let Some(w) = steps.run("scout off", dev.set_mask(MaskTarget::Scout, false))? {
                sbx = w.value.sbx();
                // With Scout on the mask shows SBX off; the device restores
                // the real SBX state when Scout goes off. Save that too, or
                // a later disable would leave SBX off once Scout is switched
                // off again.
                if first_save && sbx {
                    if let Some(m) = saved.mask.as_mut() {
                        m.sbx = true;
                    }
                    store::write_json(&self.path, &saved)?;
                }
                if new_entry {
                    if let Some(user) =
                        steps.run("read after scout off", read_output_settings(dev))?
                    {
                        saved.outputs.insert(key, user);
                        store::write_json(&self.path, &saved)?;
                    }
                }
            }
        }
        if sbx {
            steps.run("sbx off", dev.set_mask(MaskTarget::Sbx, false))?;
        }
        if let Some(v) = steps.run("read eq", dev.read_feature(feat(EQ)))? {
            if on(v) {
                steps.run("eq off", dev.write_feature(feat(EQ), 0.0))?;
            }
        }
        hw_direct(dev, true)?;
        steps.finish()
    }

    /// Turn Direct Mode off: restore the mask and the active output's
    /// settings. On any failed step the saved state is kept.
    pub fn disable(&self, dev: &Device) -> Result<(), Error> {
        let mut saved = self.saved()?.ok_or(Error::NotActive)?;
        let output = dev.read_output()?;
        let key = output_name(output);
        let mut steps = Steps(Vec::new());

        // Hardware Direct Mode first; it pushes a dump of the (still direct)
        // profile, which the restore below then overwrites. Unlike enable, a
        // failure here is a failed step: forgetting the saved state would
        // leave the device in hardware Direct Mode with nothing to retry.
        let mut hw_done = true;
        if saved.active {
            if let Err(e) = dev.set_hw_direct(false) {
                if fatal(&e) {
                    return Err(Error::Device(e));
                }
                // A refused write is harmless if the mode is off anyway.
                if !matches!(dev.read_hw_direct(), Ok(false)) {
                    steps.run("hardware direct mode off", Err::<(), _>(e))?;
                    hw_done = false;
                }
            }
        }

        // Scout may have been turned on meanwhile (its button is not blocked):
        // switch it off first, or the device would undo the restore below
        // when Scout goes off. If that fails, restore nothing. Direct Mode
        // already off (only this output's restore pending), Scout on is the
        // user's choice: it goes back on after the restore. Without an entry
        // for this output there is nothing to restore and Scout is left alone.
        let mut scout_off = true;
        // A failed "scout on" of an earlier attempt is finished here too.
        let mut scout_back = saved.scout_back && !saved.active;
        let has_entry = saved.outputs.contains_key(key);
        if (saved.active || has_entry) && dev.read_mask()?.scout() {
            if !saved.active {
                // On disk before Scout goes off, so a failure further on
                // leaves a retry able to switch it on again.
                saved.scout_back = true;
                store::write_json(&self.path, &saved)?;
            }
            scout_off = steps
                .run("scout off", dev.set_mask(MaskTarget::Scout, false))?
                .is_some();
            scout_back = scout_off && !saved.active;
        }

        // Effects and EQ enable first, then SBX, then Scout last: turning
        // Scout on makes the device apply its own curve / Smart Volume.
        let mut entry_done = scout_off;
        if let Some(entry) = saved.outputs.get(key).filter(|_| scout_off) {
            let mut all_ok = true;
            let mut restore = |name: &str, want: bool| -> Result<(), Error> {
                let f = feat(name);
                let cur = dev.read_feature(f);
                let r = match cur {
                    Ok(v) if on(v) == want => return Ok(()),
                    Ok(_) => dev
                        .write_feature(f, if want { 1.0 } else { 0.0 })
                        .map(|_| ()),
                    Err(e) => Err(e),
                };
                let ok = r.is_ok();
                steps.run(name, r)?;
                all_ok &= ok;
                Ok(())
            };
            for name in EFFECT_TOGGLES {
                if let Some(&want) = entry.effects.get(name) {
                    restore(name, want)?;
                }
            }
            restore(EQ, entry.eq)?;
            entry_done = all_ok;
        }

        let mut mask_done = scout_off && hw_done;
        if saved.active && scout_off {
            if let Some(m) = saved.mask {
                let before = steps.0.len();
                for (target, want) in [(MaskTarget::Sbx, m.sbx), (MaskTarget::Scout, m.scout)] {
                    let cur = dev.read_mask()?;
                    let have = match target {
                        MaskTarget::Sbx => cur.sbx(),
                        MaskTarget::Scout => cur.scout(),
                    };
                    if have != want {
                        steps.run(&format!("{target:?}"), dev.set_mask(target, want))?;
                    }
                }
                mask_done &= steps.0.len() == before;
            }
        }
        let mut scout_on_done = true;
        if scout_back {
            let r = match dev.read_mask() {
                Ok(m) if m.scout() => Ok(()),
                Ok(_) => dev.set_mask(MaskTarget::Scout, true).map(|_| ()),
                Err(e) => Err(e),
            };
            scout_on_done = steps.run("scout on", r)?.is_some();
        }
        saved.scout_back = scout_back && !scout_on_done;

        // Forget only what was restored; the entry stays until Scout is back
        // on, so the output still counts as pending.
        if entry_done && scout_on_done {
            saved.outputs.remove(key);
        }
        if mask_done && saved.active {
            saved.active = false;
            saved.mask = None;
        }
        if saved.outputs.is_empty() && !saved.active && !saved.scout_back {
            store::remove_file(&self.path)?;
        } else {
            store::write_json(&self.path, &saved)?;
        }
        steps.finish()
    }

    /// Outputs whose settings are still waiting to be restored.
    pub fn pending_outputs(&self) -> Result<Vec<Output>, StoreError> {
        Ok(self
            .saved()?
            .map(|s| {
                s.outputs
                    .keys()
                    .filter_map(|k| output_from_name(k))
                    .collect()
            })
            .unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Mask;
    use crate::state::tests::start;
    use crate::store::testutil::TempDir;
    use crate::transport::fake::FakeDevice;

    fn set(fake: &FakeDevice, name: &str, v: f32) {
        let f = feat(name);
        fake.set_feature(f.family, f.id, v);
    }

    fn get(fake: &FakeDevice, name: &str) -> f32 {
        let f = feat(name);
        fake.feature(f.family, f.id).unwrap()
    }

    fn user_setup(fake: &FakeDevice) {
        set(fake, EQ, 1.0);
        set(fake, "surround", 1.0);
        set(fake, "surround_level", 0.6);
        set(fake, "bass", 1.0);
        set(fake, "eq_1k", 3.0);
        fake.press_mask(Mask::SBX | Mask::VOICE_CLARITY);
    }

    #[test]
    fn enable_disables_sbx_and_eq_and_disable_restores() {
        let fake = FakeDevice::new();
        user_setup(&fake);
        let (dev, _rx) = start(&fake);
        let t = TempDir::new();
        let d = DirectMode::new(t.path().join("direct.json"));
        assert!(!d.is_active().unwrap());

        d.enable(&dev).unwrap();
        assert!(d.is_active().unwrap());
        assert_eq!(
            fake.mask(),
            Mask::VOICE_CLARITY,
            "SBX off, mic bit untouched"
        );
        assert_eq!(get(&fake, EQ), 0.0);
        // Effects keep their toggles; EQ band gains are untouched.
        assert_eq!(get(&fake, "surround"), 1.0);
        assert_eq!(get(&fake, "eq_1k"), 3.0);

        d.disable(&dev).unwrap();
        assert!(!d.is_active().unwrap());
        assert!(!d.path().exists());
        assert_eq!(fake.mask(), Mask::SBX | Mask::VOICE_CLARITY);
        assert_eq!(get(&fake, EQ), 1.0);
        assert_eq!(get(&fake, "surround"), 1.0);
        assert_eq!(get(&fake, "surround_level"), 0.6);
        assert_eq!(get(&fake, "bass"), 1.0);
    }

    /// Index of the first / last frame with `command` written so far.
    fn frame_positions(fake: &FakeDevice, command: u8) -> Vec<usize> {
        fake.written()
            .iter()
            .enumerate()
            .filter(|(_, f)| f[1] == command)
            .map(|(i, _)| i)
            .collect()
    }

    #[test]
    fn hardware_direct_mode_follows_the_software_one() {
        let fake = FakeDevice::new();
        user_setup(&fake);
        let (dev, _rx) = start(&fake);
        let t = TempDir::new();
        let d = DirectMode::new(t.path().join("d.json"));

        d.enable(&dev).unwrap();
        assert!(fake.hw_direct(), "on after enable");
        // Switched on after SBX and EQ went off (the last device writes).
        let hw = frame_positions(&fake, crate::protocol::cmd::HW_DIRECT);
        let other = frame_positions(&fake, crate::protocol::cmd::FEATURE_WRITE);
        assert!(hw[0] > *other.last().unwrap(), "hw step is last");
        let before_disable = fake.written().len();

        d.disable(&dev).unwrap();
        assert!(!fake.hw_direct(), "off after disable");
        // Switched off before the saved state is restored.
        let written = fake.written();
        let first_restore = written[before_disable..]
            .iter()
            .position(|f| f[1] == crate::protocol::cmd::FEATURE_WRITE)
            .unwrap();
        let hw_off = written[before_disable..]
            .iter()
            .position(|f| f[1] == crate::protocol::cmd::HW_DIRECT)
            .unwrap();
        assert!(hw_off < first_restore, "hw step is first");
        assert_eq!(get(&fake, EQ), 1.0);
        assert_eq!(fake.mask(), Mask::SBX | Mask::VOICE_CLARITY);
    }

    #[test]
    fn a_failing_hardware_step_does_not_break_software_direct_mode() {
        let fake = FakeDevice::new();
        user_setup(&fake);
        let (dev, _rx) = start(&fake);
        let t = TempDir::new();
        let d = DirectMode::new(t.path().join("d.json"));
        // The device never answers the 0x39 write and its read-back.
        fake.drop_replies(crate::protocol::cmd::HW_DIRECT, 2);
        d.enable(&dev).unwrap();
        assert!(d.is_active().unwrap());
        assert!(!fake.hw_direct());
        assert_eq!(get(&fake, EQ), 0.0);
        d.disable(&dev).unwrap();
        assert_eq!(get(&fake, EQ), 1.0);
        assert!(!d.path().exists());

        // Rejected (nothing else to switch off, so the 0x39 write is the first).
        let fake = FakeDevice::new();
        let (dev, _rx) = start(&fake);
        fake.reject_next_write(0x83);
        d.enable(&dev).unwrap();
        assert!(d.is_active().unwrap());
        assert!(!fake.hw_direct());
        fake.reject_next_write(0x83);
        d.disable(&dev).unwrap();
        assert!(!d.path().exists());
    }

    #[test]
    fn hardware_mode_left_on_by_disable_keeps_the_saved_state() {
        let fake = FakeDevice::new();
        user_setup(&fake);
        let (dev, _rx) = start(&fake);
        let t = TempDir::new();
        let d = DirectMode::new(t.path().join("d.json"));
        d.enable(&dev).unwrap();
        assert!(fake.hw_direct());
        // The 0x39 off write is refused: the device stays in hardware Direct Mode.
        fake.reject_next_write(0x83);
        assert!(matches!(d.disable(&dev), Err(Error::Incomplete(_))));
        assert!(fake.hw_direct());
        assert!(d.is_active().unwrap(), "kept for a retry");
        // The rest was restored meanwhile.
        assert_eq!(get(&fake, EQ), 1.0);
        assert_eq!(fake.mask(), Mask::SBX | Mask::VOICE_CLARITY);

        d.disable(&dev).unwrap();
        assert!(!fake.hw_direct());
        assert!(!d.path().exists());
    }

    #[test]
    fn scout_pressed_while_active_does_not_undo_the_restore() {
        let fake = FakeDevice::new();
        user_setup(&fake); // EQ on, SBX on, Scout off
        let (dev, _rx) = start(&fake);
        let t = TempDir::new();
        let d = DirectMode::new(t.path().join("d.json"));
        d.enable(&dev).unwrap();
        // The Scout button: the device remembers Direct Mode's EQ off / SBX off.
        fake.press_scout(true);
        d.disable(&dev).unwrap();
        assert_eq!(fake.mask() & Mask::SCOUT, 0, "Scout was off before");
        assert_eq!(fake.mask() & Mask::SBX, Mask::SBX);
        assert_eq!(get(&fake, EQ), 1.0, "the user's EQ enable");
        assert_eq!(get(&fake, "smartvol"), 0.0);
        assert!(!d.path().exists());
    }

    #[test]
    fn scout_pressed_while_active_with_scout_saved_on() {
        let fake = FakeDevice::new();
        set(&fake, EQ, 1.0);
        let (dev, _rx) = start(&fake);
        dev.set_mask(MaskTarget::Scout, true).unwrap();
        let t = TempDir::new();
        let d = DirectMode::new(t.path().join("d.json"));
        d.enable(&dev).unwrap(); // Scout off, the user's EQ on, then EQ off
        fake.press_scout(true);
        d.disable(&dev).unwrap();
        assert_eq!(fake.mask() & Mask::SCOUT, Mask::SCOUT);
        // Turning Scout off later brings back the user's EQ, not Direct Mode's.
        dev.set_mask(MaskTarget::Scout, false).unwrap();
        assert_eq!(get(&fake, EQ), 1.0);
    }

    #[test]
    fn saved_state_is_on_disk_before_the_first_device_write() {
        let fake = FakeDevice::new();
        user_setup(&fake);
        let (dev, _rx) = start(&fake);
        let t = TempDir::new();
        let d = DirectMode::new(t.path().join("direct.json"));
        // Make the first device write fail: the file must already exist.
        fake.reject_next_write(0x81);
        let r = d.enable(&dev);
        assert!(matches!(r, Err(Error::Incomplete(_))), "{r:?}");
        let s = d
            .saved()
            .unwrap()
            .expect("saved before changing the device");
        assert!(s.active);
        assert_eq!(
            s.mask,
            Some(SavedMask {
                sbx: true,
                scout: false
            })
        );
        let h = &s.outputs["headphones"];
        assert!(h.eq);
        assert!(h.effects["surround"]);
        assert!(!h.effects["dialog"]);
    }

    #[test]
    fn unwritable_state_file_aborts_before_touching_the_device() {
        let fake = FakeDevice::new();
        user_setup(&fake);
        let (dev, _rx) = start(&fake);
        let t = TempDir::new();
        // Parent "directory" is a regular file, so the save must fail.
        let blocker = t.path().join("blocker");
        std::fs::write(&blocker, "x").unwrap();
        let d = DirectMode::new(blocker.join("direct.json"));
        let writes_before = fake.written().len();
        assert!(matches!(d.enable(&dev), Err(Error::Store(_))));
        let new_writes: Vec<_> = fake.written()[writes_before..]
            .iter()
            .filter(|f| {
                f[1] == crate::protocol::cmd::FEATURE_WRITE
                    || (f[1] == crate::protocol::cmd::MASK && f[3] == 0x07)
            })
            .cloned()
            .collect();
        assert!(new_writes.is_empty(), "device was written: {new_writes:?}");
        assert_eq!(fake.mask(), Mask::SBX | Mask::VOICE_CLARITY);
        assert_eq!(get(&fake, EQ), 1.0);
    }

    #[test]
    fn mic_family_is_never_written() {
        let fake = FakeDevice::new();
        user_setup(&fake);
        set(&fake, "mic_clarity", 1.0);
        set(&fake, "mic_eq", 1.0);
        let (dev, _rx) = start(&fake);
        let t = TempDir::new();
        let d = DirectMode::new(t.path().join("d.json"));
        d.enable(&dev).unwrap();
        d.disable(&dev).unwrap();
        for f in fake.written() {
            if f[1] == crate::protocol::cmd::FEATURE_WRITE {
                assert_ne!(f[4], crate::features::FAMILY_MIC, "mic written: {f:02x?}");
            }
        }
        assert_eq!(get(&fake, "mic_clarity"), 1.0);
    }

    #[test]
    fn scout_on_is_turned_off_and_restored() {
        let fake = FakeDevice::new();
        set(&fake, EQ, 1.0);
        fake.press_mask(Mask::SCOUT);
        let (dev, _rx) = start(&fake);
        let t = TempDir::new();
        let d = DirectMode::new(t.path().join("d.json"));
        d.enable(&dev).unwrap();
        assert_eq!(fake.mask() & (Mask::SBX | Mask::SCOUT), 0);
        assert_eq!(get(&fake, EQ), 0.0);
        d.disable(&dev).unwrap();
        assert_eq!(fake.mask() & Mask::SCOUT, Mask::SCOUT);
        assert_eq!(get(&fake, EQ), 1.0);
    }

    #[test]
    fn pre_amp_is_never_written_and_survives_scout() {
        let fake = FakeDevice::new();
        set(&fake, "eq_preamp", -5.5);
        let (dev, _rx) = start(&fake);
        // Scout on by a write: the device sets the pre-amp to 0 dB.
        dev.set_mask(MaskTarget::Scout, true).unwrap();
        assert_eq!(get(&fake, "eq_preamp"), 0.0);
        let t = TempDir::new();
        let d = DirectMode::new(t.path().join("d.json"));
        let before = fake.written().len();
        d.enable(&dev).unwrap();
        assert_eq!(get(&fake, "eq_preamp"), -5.5, "restored by Scout off");
        d.disable(&dev).unwrap();
        assert_eq!(get(&fake, "eq_preamp"), 0.0, "Scout is on again");
        let id = crate::features::by_name("eq_preamp").unwrap().id;
        let wrote_preamp = fake.written()[before..].iter().any(|f| {
            f[1] == crate::protocol::cmd::FEATURE_WRITE
                && f[4] == crate::features::FAMILY_PLAYBACK
                && f[5] == id
        });
        assert!(!wrote_preamp, "Direct Mode wrote the pre-amp");
        dev.set_mask(MaskTarget::Scout, false).unwrap();
        assert_eq!(get(&fake, "eq_preamp"), -5.5);
    }

    #[test]
    fn scout_settings_are_not_saved_as_the_users() {
        let fake = FakeDevice::new();
        // The user's settings: EQ off, Smart Volume off, own EQ curve.
        set(&fake, "eq_1k", 5.0);
        let (dev, _rx) = start(&fake);
        // Scout on: the device switches EQ + Smart Volume on itself.
        dev.set_mask(MaskTarget::Scout, true).unwrap();
        assert_eq!(get(&fake, EQ), 1.0);
        assert_eq!(get(&fake, "smartvol"), 1.0);

        let t = TempDir::new();
        let d = DirectMode::new(t.path().join("d.json"));
        d.enable(&dev).unwrap();
        let s = d.saved().unwrap().unwrap();
        let h = &s.outputs["headphones"];
        assert!(!h.eq, "Scout's EQ enable saved as the user's");
        assert!(!h.effects["smartvol"], "Scout's Smart Volume saved");
        assert_eq!(
            s.mask,
            Some(SavedMask {
                sbx: false,
                scout: true
            })
        );

        d.disable(&dev).unwrap();
        assert_eq!(fake.mask() & Mask::SCOUT, Mask::SCOUT);
        // Turning Scout off later brings the user's settings back.
        dev.set_mask(MaskTarget::Scout, false).unwrap();
        assert_eq!(get(&fake, EQ), 0.0);
        assert_eq!(get(&fake, "smartvol"), 0.0);
        assert_eq!(get(&fake, "eq_1k"), 5.0);
    }

    #[test]
    fn enable_is_idempotent_and_keeps_the_first_snapshot() {
        let fake = FakeDevice::new();
        user_setup(&fake);
        let (dev, _rx) = start(&fake);
        let t = TempDir::new();
        let d = DirectMode::new(t.path().join("d.json"));
        d.enable(&dev).unwrap();
        let first = d.saved().unwrap().unwrap();
        d.enable(&dev).unwrap(); // device now shows Direct Mode's state
        assert_eq!(d.saved().unwrap().unwrap(), first);
        d.disable(&dev).unwrap();
        assert_eq!(get(&fake, EQ), 1.0);
        assert!(fake.mask() & Mask::SBX != 0);
    }

    #[test]
    fn output_switch_while_active_is_handled_per_output() {
        let fake = FakeDevice::new();
        user_setup(&fake); // headphones: EQ on, SBX on
        let (dev, _rx) = start(&fake);
        let t = TempDir::new();
        let d = DirectMode::new(t.path().join("d.json"));
        d.enable(&dev).unwrap();

        // Jack switch to speakers, whose EQ is on.
        fake.switch_output(Output::Speakers);
        set(&fake, EQ, 1.0);
        d.enable(&dev).unwrap();
        assert_eq!(get(&fake, EQ), 0.0);
        assert_eq!(d.pending_outputs().unwrap().len(), 2);

        // Disable on speakers: speakers + mask restored, headphones pending.
        d.disable(&dev).unwrap();
        assert_eq!(get(&fake, EQ), 1.0);
        assert!(!d.is_active().unwrap());
        assert_eq!(d.pending_outputs().unwrap(), vec![Output::Headphones]);
        assert!(d.path().exists());

        // Back on headphones: restore the rest.
        fake.switch_output(Output::Headphones);
        d.disable(&dev).unwrap();
        assert!(!d.path().exists());
        assert_eq!(get(&fake, EQ), 1.0);
    }

    #[test]
    fn pending_restore_is_not_written_under_scout() {
        let fake = FakeDevice::new();
        user_setup(&fake); // headphones: EQ on, SBX on
        let (dev, _rx) = start(&fake);
        let t = TempDir::new();
        let d = DirectMode::new(t.path().join("d.json"));
        d.enable(&dev).unwrap();
        fake.switch_output(Output::Speakers);
        set(&fake, EQ, 1.0);
        d.enable(&dev).unwrap();
        d.disable(&dev).unwrap(); // speakers restored, headphones pending
        assert_eq!(d.pending_outputs().unwrap(), vec![Output::Headphones]);
        assert!(!d.is_active().unwrap());

        // Back on headphones, where the user has Scout on.
        fake.switch_output(Output::Headphones);
        set(&fake, EQ, 0.0); // still Direct Mode's EQ off (the fake is not per output)
        dev.set_mask(MaskTarget::Scout, true).unwrap();
        d.disable(&dev).unwrap();
        assert!(!d.path().exists(), "restore done");
        assert_eq!(fake.mask() & Mask::SCOUT, Mask::SCOUT, "Scout stays on");
        // Turning Scout off brings back the restored EQ, not Direct Mode's.
        dev.set_mask(MaskTarget::Scout, false).unwrap();
        assert_eq!(get(&fake, EQ), 1.0);
        assert_eq!(get(&fake, "smartvol"), 0.0);
    }

    #[test]
    fn failed_scout_on_after_a_pending_restore_can_be_finished() {
        let fake = FakeDevice::new();
        user_setup(&fake);
        let (dev, _rx) = start(&fake);
        let t = TempDir::new();
        let d = DirectMode::new(t.path().join("d.json"));
        d.enable(&dev).unwrap();
        fake.switch_output(Output::Speakers);
        set(&fake, EQ, 1.0);
        d.enable(&dev).unwrap();
        d.disable(&dev).unwrap(); // headphones pending
        fake.switch_output(Output::Headphones);
        dev.set_mask(MaskTarget::Scout, true).unwrap();

        // Scout off goes through, the restore needs no write, the "scout on" is refused.
        fake.reject_write_after(1, 0x81);
        assert!(matches!(d.disable(&dev), Err(Error::Incomplete(_))));
        assert_eq!(fake.mask() & Mask::SCOUT, 0, "left off");
        assert!(d.saved().unwrap().unwrap().scout_back);
        assert_eq!(d.pending_outputs().unwrap(), vec![Output::Headphones]);

        // The retry switches Scout back on and clears the file.
        d.disable(&dev).unwrap();
        assert_eq!(fake.mask() & Mask::SCOUT, Mask::SCOUT);
        assert!(!d.path().exists());
        dev.set_mask(MaskTarget::Scout, false).unwrap();
        assert_eq!(get(&fake, EQ), 1.0);
    }

    #[test]
    fn scout_back_flag_is_optional_in_the_file() {
        let old = r#"{"version":1,"active":false,"mask":null,"outputs":{}}"#;
        let s: SavedState = serde_json::from_str(old).unwrap();
        assert!(!s.scout_back);
        assert_eq!(serde_json::to_string(&s).unwrap(), old.replace(" ", ""));
    }

    #[test]
    fn enabling_after_a_failed_scout_on_keeps_scout_in_the_saved_mask() {
        let fake = FakeDevice::new();
        user_setup(&fake);
        let (dev, _rx) = start(&fake);
        let t = TempDir::new();
        let d = DirectMode::new(t.path().join("d.json"));
        d.enable(&dev).unwrap();
        fake.switch_output(Output::Speakers);
        set(&fake, EQ, 1.0);
        d.enable(&dev).unwrap();
        d.disable(&dev).unwrap();
        fake.switch_output(Output::Headphones);
        dev.set_mask(MaskTarget::Scout, true).unwrap();
        fake.reject_write_after(1, 0x81);
        assert!(d.disable(&dev).is_err());
        // Direct Mode on again while Scout is (wrongly) off: Scout is still the user's.
        d.enable(&dev).unwrap();
        let s = d.saved().unwrap().unwrap();
        assert!(s.mask.unwrap().scout && !s.scout_back);
        d.disable(&dev).unwrap();
        assert_eq!(fake.mask() & Mask::SCOUT, Mask::SCOUT);
    }

    #[test]
    fn pending_restore_for_another_output_leaves_scout_alone() {
        let fake = FakeDevice::new();
        user_setup(&fake);
        let (dev, _rx) = start(&fake);
        let t = TempDir::new();
        let d = DirectMode::new(t.path().join("d.json"));
        d.enable(&dev).unwrap();
        fake.switch_output(Output::Speakers);
        d.enable(&dev).unwrap();
        d.disable(&dev).unwrap(); // headphones pending
                                  // Scout on, on the output with nothing pending: no Scout writes.
        dev.set_mask(MaskTarget::Scout, true).unwrap();
        let before = fake.written().len();
        d.disable(&dev).unwrap();
        assert!(d.path().exists(), "headphones still pending");
        let scout_writes = fake.written()[before..]
            .iter()
            .filter(|f| f[1] == crate::protocol::cmd::MASK && f[3] == 0x07)
            .count();
        assert_eq!(scout_writes, 0);
        assert_eq!(fake.mask() & Mask::SCOUT, Mask::SCOUT);
    }

    #[test]
    fn disable_without_saved_state_is_an_error() {
        let fake = FakeDevice::new();
        let (dev, _rx) = start(&fake);
        let t = TempDir::new();
        let d = DirectMode::new(t.path().join("d.json"));
        assert_eq!(d.disable(&dev), Err(Error::NotActive));
    }

    #[test]
    fn failed_restore_keeps_the_saved_state() {
        let fake = FakeDevice::new();
        user_setup(&fake);
        let (dev, _rx) = start(&fake);
        let t = TempDir::new();
        let d = DirectMode::new(t.path().join("d.json"));
        d.enable(&dev).unwrap();
        fake.set_ignore_writes(true); // writes ACK but are not applied
        assert!(d.disable(&dev).is_err());
        assert!(d.is_active().unwrap(), "state kept for a retry");
        fake.set_ignore_writes(false);
        d.disable(&dev).unwrap();
        assert!(!d.path().exists());
        assert_eq!(get(&fake, EQ), 1.0);
        assert!(fake.mask() & Mask::SBX != 0);
    }

    #[test]
    fn survives_a_crash_between_enable_and_disable() {
        // "Crash" = a new DirectMode handle on the same file.
        let fake = FakeDevice::new();
        user_setup(&fake);
        let (dev, _rx) = start(&fake);
        let t = TempDir::new();
        let path = t.path().join("d.json");
        DirectMode::new(&path).enable(&dev).unwrap();
        let after_restart = DirectMode::new(&path);
        assert!(after_restart.is_active().unwrap());
        after_restart.disable(&dev).unwrap();
        assert_eq!(get(&fake, EQ), 1.0);
    }
}
