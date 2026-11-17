//! DeviceState model with per-output (headphones / speakers) profiles.
//!
//! The G6 keeps separate playback settings (family `0x96`: EQ, SBX effects)
//! for headphones and speakers; the active one follows the headphone jack.
//! [`DeviceState`] holds both profiles, the settings that do not depend on
//! the output (mic, decoder, DAC filter, mic boost, LED, SBX / Scout mask) and is
//! kept current by feeding it [`Event`]s or by [`DeviceState::refresh`].
//!
//! Only the active output's profile can be read from the device; the other
//! profile keeps its last known values (from the last time it was active, or
//! from the file the [`crate::session::Session`] persists).

use std::collections::BTreeMap;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::events::{Change, Device, Error, Event};
use crate::features::{self, Feature, FEATURES};
use crate::protocol::{rgb_from_hex, rgb_to_hex, DacFilter, Mask, MicBoost, Output, Rgb};

/// Stable name of an output (`headphones` / `speakers`), used in JSON.
pub fn output_name(o: Output) -> &'static str {
    match o {
        Output::Headphones => "headphones",
        Output::Speakers => "speakers",
    }
}

/// Inverse of [`output_name`].
pub fn output_from_name(s: &str) -> Option<Output> {
    match s {
        "headphones" => Some(Output::Headphones),
        "speakers" => Some(Output::Speakers),
        _ => None,
    }
}

/// Feature values keyed by feature name (device-side values, rounded).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Profile(pub BTreeMap<String, f32>);

impl Profile {
    pub fn get(&self, feature: &Feature) -> Option<f32> {
        self.0.get(feature.name).copied()
    }

    pub fn get_by_name(&self, name: &str) -> Option<f32> {
        self.0.get(name).copied()
    }

    /// Store a value; returns whether it changed.
    pub fn set(&mut self, feature: &Feature, value: f32) -> bool {
        self.0.insert(feature.name.to_string(), value) != Some(value)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// EQ band gains in band order (31 Hz … 16 kHz).
    pub fn eq_bands(&self) -> [Option<f32>; 10] {
        std::array::from_fn(|i| features::eq_band(i).and_then(|f| self.get(f)))
    }
}

/// Serde adapters for the protocol enums stored in [`DeviceState`] (the
/// protocol layer itself has no serde dependency).
macro_rules! opt_serde {
    ($m:ident, $t:ty, $repr:ty, $to:expr, $from:expr) => {
        mod $m {
            use super::*;
            pub fn serialize<S: Serializer>(v: &Option<$t>, s: S) -> Result<S::Ok, S::Error> {
                let to: fn($t) -> $repr = $to;
                v.map(to).serialize(s)
            }
            pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<$t>, D::Error> {
                let from: fn(&$repr) -> Option<$t> = $from;
                match Option::<$repr>::deserialize(d)? {
                    None => Ok(None),
                    Some(r) => from(&r)
                        .map(Some)
                        .ok_or_else(|| D::Error::custom(format!("invalid value {r:?}"))),
                }
            }
        }
    };
}

opt_serde!(
    output_serde,
    Output,
    String,
    |o| output_name(o).to_string(),
    |s| output_from_name(s)
);
opt_serde!(mask_serde, Mask, u8, |m| m.0, |b| Some(Mask(*b)));
opt_serde!(
    filter_serde,
    DacFilter,
    String,
    |f| f.name().to_string(),
    |s| DacFilter::from_name(s)
);
opt_serde!(
    boost_serde,
    MicBoost,
    u8,
    |b| b.db(),
    |d| MicBoost::from_db(*d)
);
opt_serde!(color_serde, Rgb, String, |c| rgb_to_hex(c), |s| {
    rgb_from_hex(s)
});

/// Full snapshot of the G6.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceState {
    /// Device is currently connected.
    pub connected: bool,
    /// Active output; `None` until first read.
    #[serde(with = "output_serde")]
    pub output: Option<Output>,
    /// Global SBX / Scout / Voice Clarity mask.
    #[serde(with = "mask_serde")]
    pub mask: Option<Mask>,
    /// Per-output settings ([`features::per_output`]: playback and mic) while
    /// on headphones.
    pub headphones: Profile,
    /// Per-output settings while on speakers.
    pub speakers: Profile,
    /// Settings shared by both outputs: decoder (`0x97`).
    pub common: Profile,
    #[serde(with = "filter_serde")]
    pub dac_filter: Option<DacFilter>,
    #[serde(with = "boost_serde")]
    pub mic_boost: Option<MicBoost>,
    /// LED on / off.
    pub lighting_enabled: Option<bool>,
    /// LED color (`"rrggbb"` in JSON).
    #[serde(with = "color_serde")]
    pub lighting_color: Option<Rgb>,
}

impl DeviceState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Playback profile of `output`.
    pub fn profile(&self, output: Output) -> &Profile {
        match output {
            Output::Headphones => &self.headphones,
            Output::Speakers => &self.speakers,
        }
    }

    pub fn profile_mut(&mut self, output: Output) -> &mut Profile {
        match output {
            Output::Headphones => &mut self.headphones,
            Output::Speakers => &mut self.speakers,
        }
    }

    /// Profile of the active output, if known.
    pub fn active_profile(&self) -> Option<&Profile> {
        self.output.map(|o| self.profile(o))
    }

    /// Current value of a feature: per-output features come from the active
    /// profile, the others from [`DeviceState::common`].
    pub fn feature(&self, feature: &Feature) -> Option<f32> {
        if features::per_output(feature) {
            self.active_profile()?.get(feature)
        } else {
            self.common.get(feature)
        }
    }

    /// Store a feature value (see [`DeviceState::feature`] for where it
    /// goes). Returns whether the state changed; a per-output value is
    /// dropped while the active output is unknown.
    pub fn set_feature(&mut self, feature: &Feature, value: f32) -> bool {
        if features::per_output(feature) {
            match self.output {
                Some(o) => self.profile_mut(o).set(feature, value),
                None => {
                    log::debug!(
                        "{}: per-output value before output known; ignored",
                        feature.name
                    );
                    false
                }
            }
        } else {
            self.common.set(feature, value)
        }
    }

    /// The active output is about to become `new`. Left while Scout is on,
    /// the old output's EQ / Smart Volume / pre-amp are Scout's, not the
    /// user's: forget them so they are never captured into a profile or
    /// re-applied as the user's. Call before `output` and `mask` are updated.
    fn leave_output(&mut self, new: Output) {
        if let Some(old) = self.output.filter(|old| *old != new) {
            if self.mask.is_some_and(|m| m.scout()) {
                self.profile_mut(old)
                    .0
                    .retain(|k, _| !features::by_name(k).is_some_and(features::scout_resets));
            }
        }
    }

    /// Update from a device event. Returns whether anything changed.
    pub fn apply(&mut self, event: &Event) -> bool {
        match *event {
            Event::Connected => replace(&mut self.connected, true),
            Event::Disconnected => replace(&mut self.connected, false),
            Event::OutputSwitched(o) => {
                self.leave_output(o);
                replace(&mut self.output, Some(o))
            }
            Event::Changed(Change::Feature { feature, value }) => self.set_feature(feature, value),
            Event::Changed(Change::Mask(m)) => replace(&mut self.mask, Some(m)),
            Event::Changed(Change::DacFilter(x)) => replace(&mut self.dac_filter, Some(x)),
            Event::Changed(Change::MicBoost(x)) => replace(&mut self.mic_boost, Some(x)),
            Event::Changed(Change::LightingEnabled(x)) => {
                replace(&mut self.lighting_enabled, Some(x))
            }
            Event::Changed(Change::LightingColor(x)) => replace(&mut self.lighting_color, Some(x)),
        }
    }

    /// Read the whole state from the device: output, mask, DAC filter, mic
    /// boost, LED and every feature in the table. Only the active output's
    /// profile is read, the other keeps its last known values. A mask, DAC
    /// filter, mic boost, LED or feature read that does not answer is skipped (and
    /// logged; the field keeps its value); an output read that does not
    /// answer and a lost device are errors.
    pub fn refresh(&mut self, dev: &Device) -> Result<(), Error> {
        /// `Ok(None)` on a timeout (logged), the error otherwise.
        fn soft<T>(what: &str, r: Result<T, Error>) -> Result<Option<T>, Error> {
            match r {
                Ok(v) => Ok(Some(v)),
                Err(Error::Timeout { .. }) => {
                    log::warn!("no reply reading {what}");
                    Ok(None)
                }
                Err(Error::Rejected { status, .. }) => {
                    log::warn!("{what} read rejected (status {status:#04x})");
                    Ok(None)
                }
                Err(e) => Err(e),
            }
        }
        // The jack may switch mid-read; retry so a profile never mixes outputs.
        // Reads go to a scratch copy that replaces `self` only once the output
        // is confirmed unchanged, so a switch never leaves values of one
        // output in the other's profile.
        for attempt in 0..3 {
            let mut read = self.clone();
            let output = dev.read_output()?;
            // An output change found here (e.g. after a reconnect) has no
            // `OutputSwitched` event to forget the old output's Scout values.
            read.leave_output(output);
            read.output = Some(output);
            if let Some(m) = soft("mask", dev.read_mask())? {
                read.mask = Some(m);
            }
            if let Some(x) = soft("dac filter", dev.read_dac_filter())? {
                read.dac_filter = Some(x);
            }
            if let Some(x) = soft("mic boost", dev.read_mic_boost())? {
                read.mic_boost = Some(x);
            }
            if let Some(x) = soft("lighting", dev.read_lighting_enabled())? {
                read.lighting_enabled = Some(x);
            }
            if let Some(x) = soft("lighting color", dev.read_lighting_color())? {
                read.lighting_color = Some(x);
            }
            for f in FEATURES {
                if let Some(v) = soft(f.name, dev.read_feature(f))? {
                    read.set_feature(f, v);
                }
            }
            if dev.read_output()? == output {
                *self = read;
                return Ok(());
            }
            log::debug!("output switched during refresh (attempt {attempt}); retrying");
        }
        log::warn!("output kept switching during refresh; state not updated");
        Ok(())
    }
}

fn replace<T: PartialEq>(slot: &mut T, new: T) -> bool {
    if *slot == new {
        false
    } else {
        *slot = new;
        true
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::events::Config;
    use crate::features::by_name;
    use crate::transport::fake::FakeDevice;
    use crate::transport::Backoff;
    use std::sync::mpsc::Receiver;
    use std::time::Duration;

    pub(crate) fn fast() -> Config {
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

    pub(crate) fn start(dev: &FakeDevice) -> (Device, Receiver<Event>) {
        let (d, rx) = Device::spawn(Box::new(dev.connector()), fast());
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)),
            Ok(Event::Connected)
        );
        (d, rx)
    }

    fn feat(name: &str) -> &'static Feature {
        by_name(name).unwrap()
    }

    fn changed(name: &str, value: f32) -> Event {
        Event::Changed(Change::Feature {
            feature: feat(name),
            value,
        })
    }

    #[test]
    fn events_update_active_profile_only() {
        let mut s = DeviceState::new();
        assert!(s.apply(&Event::OutputSwitched(Output::Headphones)));
        assert!(s.apply(&changed("surround", 1.0)));
        assert!(!s.apply(&changed("surround", 1.0)), "no change reported");
        assert!(s.apply(&Event::OutputSwitched(Output::Speakers)));
        assert_eq!(s.feature(feat("surround")), None);
        assert!(s.apply(&changed("eq_1k", -1.5)));
        assert_eq!(s.speakers.get(feat("eq_1k")), Some(-1.5));
        assert_eq!(s.headphones.get(feat("surround")), Some(1.0));
        assert_eq!(s.headphones.get(feat("eq_1k")), None);
        // Switching back exposes the headphones profile again.
        s.apply(&Event::OutputSwitched(Output::Headphones));
        assert_eq!(s.feature(feat("surround")), Some(1.0));
    }

    #[test]
    fn leaving_an_output_under_scout_forgets_its_scout_values() {
        let mut s = DeviceState::new();
        s.apply(&Event::OutputSwitched(Output::Headphones));
        s.apply(&Event::Changed(Change::Mask(Mask(Mask::SCOUT))));
        // Read under Scout: Scout's values.
        s.apply(&changed("eq", 1.0));
        s.apply(&changed("eq_31", 2.0));
        s.apply(&changed("smartvol_level", 0.9));
        s.apply(&changed("eq_preamp", 0.0));
        s.apply(&changed("bass", 1.0));
        s.apply(&Event::OutputSwitched(Output::Speakers));
        for name in ["eq", "eq_31", "smartvol_level", "eq_preamp"] {
            assert_eq!(s.headphones.get(feat(name)), None, "{name} kept");
        }
        assert_eq!(s.headphones.get(feat("bass")), Some(1.0));
        // Neither a profile nor a re-apply picks them up.
        let p = crate::preset::Preset::capture(&s);
        let h = p.headphones.unwrap();
        assert!(!h.features.contains_key("eq_31"));
        assert_eq!(h.features.get("bass"), Some(&1.0));

        // Without Scout the inactive profile is kept whole.
        s.apply(&Event::Changed(Change::Mask(Mask(0))));
        s.apply(&changed("eq_31", -1.0));
        s.apply(&Event::OutputSwitched(Output::Headphones));
        assert_eq!(s.speakers.get(feat("eq_31")), Some(-1.0));
    }

    #[test]
    fn decoder_is_shared() {
        let mut s = DeviceState::new();
        s.apply(&Event::OutputSwitched(Output::Headphones));
        s.apply(&changed("decoder_mode", 2.0));
        s.apply(&Event::OutputSwitched(Output::Speakers));
        assert_eq!(s.feature(feat("decoder_mode")), Some(2.0));
        assert!(s.headphones.get(feat("decoder_mode")).is_none());
    }

    #[test]
    fn playback_value_before_output_is_dropped() {
        let mut s = DeviceState::new();
        assert!(!s.apply(&changed("bass", 1.0)));
        assert!(s.headphones.is_empty() && s.speakers.is_empty());
    }

    #[test]
    fn mask_filter_boost_connection_events() {
        let mut s = DeviceState::new();
        assert!(s.apply(&Event::Connected));
        assert!(s.apply(&Event::Changed(Change::Mask(Mask(0x05)))));
        assert!(s.apply(&Event::Changed(Change::DacFilter(DacFilter::Nos))));
        assert!(s.apply(&Event::Changed(Change::MicBoost(MicBoost::Db10))));
        assert!(s.apply(&Event::Changed(Change::LightingEnabled(false))));
        assert!(!s.apply(&Event::Changed(Change::LightingEnabled(false))));
        assert!(s.apply(&Event::Changed(Change::LightingColor([1, 2, 3]))));
        assert_eq!(s.lighting_enabled, Some(false));
        assert_eq!(s.lighting_color, Some([1, 2, 3]));
        assert!(s.mask.unwrap().sbx() && !s.mask.unwrap().scout());
        assert_eq!(s.dac_filter, Some(DacFilter::Nos));
        assert_eq!(s.mic_boost, Some(MicBoost::Db10));
        assert!(s.apply(&Event::Disconnected));
        assert!(!s.connected);
        assert_eq!(s.mask, Some(Mask(0x05)), "values survive a disconnect");
    }

    #[test]
    fn refresh_reads_everything_from_the_device() {
        let fake = FakeDevice::new();
        fake.switch_output(Output::Speakers);
        fake.set_feature(0x96, 0x00, 1.0);
        fake.set_feature(0x96, 0x10, -1.5);
        fake.set_feature(0x95, 0x05, 0.25);
        fake.press_mask(0x01);
        let (dev, _rx) = start(&fake);
        let mut s = DeviceState::new();
        s.refresh(&dev).unwrap();
        assert_eq!(s.output, Some(Output::Speakers));
        assert_eq!(s.mask, Some(Mask(0x01)));
        assert_eq!(s.dac_filter, Some(DacFilter::FastMin));
        assert_eq!(s.mic_boost, Some(MicBoost::Db30));
        assert_eq!(s.lighting_enabled, Some(true));
        assert_eq!(s.lighting_color, Some([255, 0, 0]));
        assert_eq!(s.speakers.get(feat("surround")), Some(1.0));
        assert_eq!(s.speakers.eq_bands()[5], Some(-1.5));
        assert_eq!(s.speakers.get(feat("mic_nr_level")), Some(0.25));
        assert!(s.headphones.is_empty(), "inactive profile is not read");
        // Every table entry is present in exactly one place.
        let n = s.speakers.0.len() + s.common.0.len();
        assert_eq!(n, FEATURES.len());
    }

    #[test]
    fn mic_settings_are_kept_per_output() {
        let mut s = DeviceState::new();
        s.apply(&Event::OutputSwitched(Output::Headphones));
        s.set_feature(feat("mic_eq"), 1.0);
        s.set_feature(feat("mic_eq_1"), -3.0);
        s.apply(&Event::OutputSwitched(Output::Speakers));
        assert_eq!(s.feature(feat("mic_eq")), None);
        s.set_feature(feat("mic_eq"), 0.0);
        s.set_feature(feat("decoder_mode"), 2.0);
        assert_eq!(s.headphones.get(feat("mic_eq")), Some(1.0));
        assert_eq!(s.headphones.get(feat("mic_eq_1")), Some(-3.0));
        assert_eq!(s.speakers.get(feat("mic_eq")), Some(0.0));
        // The decoder mode stays shared.
        assert_eq!(s.common.get(feat("decoder_mode")), Some(2.0));
        s.apply(&Event::OutputSwitched(Output::Headphones));
        assert_eq!(s.feature(feat("mic_eq")), Some(1.0));
        assert_eq!(s.feature(feat("decoder_mode")), Some(2.0));
    }

    #[test]
    fn refresh_finding_a_new_output_under_scout_forgets_the_old_scout_values() {
        let fake = FakeDevice::new();
        fake.switch_output(Output::Speakers);
        let (dev, _rx) = start(&fake);
        // Last known: headphones with Scout on (Scout's values), as after a
        // reconnect where the jack switched meanwhile.
        let mut s = DeviceState::new();
        s.apply(&Event::OutputSwitched(Output::Headphones));
        s.apply(&Event::Changed(Change::Mask(Mask(Mask::SCOUT))));
        for (name, v) in [
            ("eq", 1.0),
            ("eq_31", 2.0),
            ("eq_preamp", 0.0),
            ("bass", 1.0),
        ] {
            s.apply(&changed(name, v));
        }
        s.refresh(&dev).unwrap();
        assert_eq!(s.output, Some(Output::Speakers));
        for name in ["eq", "eq_31", "eq_preamp"] {
            assert_eq!(s.headphones.get(feat(name)), None, "{name} kept");
        }
        assert_eq!(s.headphones.get(feat("bass")), Some(1.0));

        // Without Scout the old output's profile is kept whole.
        let mut s = DeviceState::new();
        s.apply(&Event::OutputSwitched(Output::Headphones));
        s.apply(&changed("eq_31", 2.0));
        s.refresh(&dev).unwrap();
        assert_eq!(s.headphones.get(feat("eq_31")), Some(2.0));
    }

    #[test]
    fn refresh_keeps_inactive_profile() {
        let fake = FakeDevice::new();
        let (dev, _rx) = start(&fake);
        let mut s = DeviceState::new();
        s.speakers.set(feat("bass"), 1.0);
        s.refresh(&dev).unwrap(); // device is on headphones
        assert_eq!(s.output, Some(Output::Headphones));
        assert_eq!(s.speakers.get(feat("bass")), Some(1.0));
        assert!(!s.headphones.is_empty());
    }

    #[test]
    fn refresh_does_not_mix_outputs_when_the_jack_switches_mid_read() {
        let fake = FakeDevice::new();
        let (dev, _rx) = start(&fake);
        // The mic boost read stalls for the reply timeout; the jack switches
        // meanwhile, before any feature is read.
        fake.drop_replies(crate::protocol::cmd::MIC_BOOST, 1);
        let f2 = fake.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            f2.set_feature(0x96, 0x10, 5.0);
            f2.switch_output(Output::Speakers);
        });
        let mut s = DeviceState::new();
        s.refresh(&dev).unwrap();
        t.join().unwrap();
        assert_eq!(s.output, Some(Output::Speakers));
        assert_eq!(s.speakers.get(feat("eq_1k")), Some(5.0));
        assert!(
            s.headphones.is_empty(),
            "values read after the switch must not land in the old output's profile: {:?}",
            s.headphones
        );
    }

    #[test]
    fn refresh_fails_when_disconnected() {
        let fake = FakeDevice::new();
        let (dev, _rx) = start(&fake);
        fake.unplug();
        let mut s = DeviceState::new();
        assert!(s.refresh(&dev).is_err());
    }

    #[test]
    fn json_roundtrip() {
        let mut s = DeviceState::new();
        s.apply(&Event::OutputSwitched(Output::Speakers));
        s.apply(&changed("eq_62", 2.5));
        s.apply(&changed("surround_level", 0.35));
        s.apply(&Event::Changed(Change::Mask(Mask(3))));
        s.apply(&Event::Changed(Change::DacFilter(DacFilter::SlowLin)));
        s.apply(&Event::Changed(Change::MicBoost(MicBoost::Db20)));
        s.apply(&Event::Changed(Change::LightingEnabled(true)));
        s.apply(&Event::Changed(Change::LightingColor([0, 255, 0])));
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"output\":\"speakers\""));
        assert!(json.contains("\"dac_filter\":\"slow-lin\""));
        assert!(json.contains("\"lighting_enabled\":true"));
        assert!(json.contains("\"lighting_color\":\"00ff00\""));
        let back: DeviceState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
        // Old / partial files still load.
        let partial: DeviceState = serde_json::from_str("{\"output\":\"headphones\"}").unwrap();
        assert_eq!(partial.output, Some(Output::Headphones));
        assert!(serde_json::from_str::<DeviceState>("{\"output\":\"bogus\"}").is_err());
        // A file saved before the LED was tracked has neither field.
        let old: DeviceState =
            serde_json::from_str("{\"output\":\"headphones\",\"dac_filter\":\"nos\"}").unwrap();
        assert_eq!((old.lighting_enabled, old.lighting_color), (None, None));
        assert!(serde_json::from_str::<DeviceState>("{\"lighting_color\":\"red\"}").is_err());
    }
}
