//! Text and JSON rendering of device state and events.

use std::fmt::Write as _;

use g6_core::events::{Change, Event};
use g6_core::features::{self, Feature, Kind, EQ_BANDS, EQ_FIRST_ID, FAMILY_PLAYBACK, FEATURES};
use g6_core::protocol::{rgb_to_hex, Mask, MicBoost, Output};
use g6_core::state::{output_name, DeviceState, Profile};
use serde_json::{json, Value};

/// A feature value as the user sees it (`on`, `0.50`, `-1 dB`, `night`).
/// `device` is the device-side value.
pub fn feature_display(f: &Feature, device: f32) -> String {
    match f.kind {
        Kind::Bool => if device >= 0.5 { "on" } else { "off" }.to_string(),
        Kind::Level => format!("{:.2}", f.to_ui(device)),
        Kind::Db => format!("{} dB", f.to_ui(device)),
        Kind::Distance => format!("{}", f.to_ui(device)),
        Kind::Choice(_) => match f.choice_name(device) {
            Some(n) => n.to_string(),
            None => format!("{device}"),
        },
    }
}

pub fn on_off(b: bool) -> &'static str {
    if b {
        "on"
    } else {
        "off"
    }
}

pub fn boost_display(b: MicBoost) -> String {
    format!("{} dB", b.db())
}

pub fn mask_display(m: Mask) -> String {
    format!(
        "sbx {}, scout {}, voice clarity {}",
        on_off(m.sbx()),
        on_off(m.scout()),
        on_off(m.voice_clarity())
    )
}

pub fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_eq_band(f: &Feature) -> bool {
    f.family == FAMILY_PLAYBACK
        && f.id >= EQ_FIRST_ID
        && usize::from(f.id - EQ_FIRST_ID) < EQ_BANDS.len()
}

/// One event as a line (without timestamp).
pub fn event_line(e: &Event) -> String {
    match e {
        Event::Connected => "connected".to_string(),
        Event::Disconnected => "disconnected (reconnecting)".to_string(),
        Event::OutputSwitched(o) => format!("output: {}", output_name(*o)),
        Event::Changed(Change::Feature { feature, value }) => {
            format!("{}: {}", feature.name, feature_display(feature, *value))
        }
        Event::Changed(Change::Mask(m)) => format!("mask: {}", mask_display(*m)),
        Event::Changed(Change::DacFilter(x)) => format!("filter: {}", x.name()),
        Event::Changed(Change::MicBoost(x)) => format!("mic boost: {}", boost_display(*x)),
        Event::Changed(Change::LightingEnabled(x)) => format!("light: {}", on_off(*x)),
        Event::Changed(Change::LightingColor(x)) => format!("light color: {}", rgb_to_hex(*x)),
    }
}

/// One event as a JSON object for `watch --json` and the API's event stream.
/// `ts` is seconds since the Unix epoch. Keys per `event`:
/// `connected` / `disconnected` (none), `output` (`output`), `feature`
/// (`name`, `value`, `text`), `mask` (`sbx`, `scout`, `voice_clarity`),
/// `filter` (`filter`), `mic_boost` (`db`), `light` (`on`), `light_color`
/// (`color`). Feature values use the same scale as `status --json`.
pub fn event_json(e: &Event, ts: f64) -> Value {
    let ts = (ts * 1000.0).round() / 1000.0;
    match e {
        Event::Connected => json!({"ts": ts, "event": "connected"}),
        Event::Disconnected => json!({"ts": ts, "event": "disconnected"}),
        Event::OutputSwitched(o) => json!({"ts": ts, "event": "output", "output": output_name(*o)}),
        Event::Changed(Change::Feature { feature, value }) => json!({
            "ts": ts,
            "event": "feature",
            "name": feature.name,
            "value": round_ui(f64::from(*value), feature.ui_scale),
            "text": feature_display(feature, *value),
        }),
        Event::Changed(Change::Mask(m)) => json!({
            "ts": ts,
            "event": "mask",
            "sbx": m.sbx(),
            "scout": m.scout(),
            "voice_clarity": m.voice_clarity(),
        }),
        Event::Changed(Change::DacFilter(x)) => {
            json!({"ts": ts, "event": "filter", "filter": x.name()})
        }
        Event::Changed(Change::MicBoost(x)) => {
            json!({"ts": ts, "event": "mic_boost", "db": x.db()})
        }
        Event::Changed(Change::LightingEnabled(x)) => json!({"ts": ts, "event": "light", "on": x}),
        Event::Changed(Change::LightingColor(x)) => {
            json!({"ts": ts, "event": "light_color", "color": rgb_to_hex(*x)})
        }
    }
}

/// Seconds since the Unix epoch (0 if the clock is before it).
pub fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// A device value on the UI scale, with the f32 -> f64 noise rounded away
/// (every step is a multiple of 0.01).
fn round_ui(device: f64, ui_scale: f32) -> f64 {
    (device * f64::from(ui_scale) * 100.0).round() / 100.0
}

/// Human-readable status.
pub fn status_text(state: &DeviceState, direct_mode: bool) -> String {
    let mut s = String::new();
    let mut row = |k: &str, v: String| {
        let _ = writeln!(s, "{k:<20}{v}");
    };
    row(
        "connected",
        if state.connected { "yes" } else { "no" }.into(),
    );
    row(
        "output",
        state
            .output
            .map_or("unknown".into(), |o| output_name(o).into()),
    );
    match state.mask {
        Some(m) => {
            row("sbx", on_off(m.sbx()).into());
            row("scout", on_off(m.scout()).into());
        }
        None => {
            row("sbx", "unknown".into());
            row("scout", "unknown".into());
        }
    }
    row("direct mode", on_off(direct_mode).into());
    row(
        "dac filter",
        state
            .dac_filter
            .map_or("unknown".into(), |f| f.name().into()),
    );
    row(
        "mic boost",
        state.mic_boost.map_or("unknown".into(), boost_display),
    );
    row(
        "light",
        state
            .lighting_enabled
            .map_or("unknown".into(), |on| on_off(on).into()),
    );
    row(
        "light color",
        state.lighting_color.map_or("unknown".into(), rgb_to_hex),
    );
    for f in FEATURES.iter().filter(|f| !features::per_output(f)) {
        if let Some(v) = state.common.get(f) {
            row(f.name, feature_display(f, v));
        }
    }
    for output in [Output::Headphones, Output::Speakers] {
        let tag = if state.output == Some(output) {
            "active"
        } else {
            "last known"
        };
        let _ = writeln!(s, "\n[{}] ({tag})", output_name(output));
        write_profile(&mut s, state.profile(output));
    }
    s
}

fn write_profile(s: &mut String, p: &Profile) {
    if p.is_empty() {
        s.push_str("  (no data)\n");
        return;
    }
    for f in FEATURES
        .iter()
        .filter(|f| f.family == FAMILY_PLAYBACK && f.exposed && !is_eq_band(f))
    {
        if let Some(v) = p.get(f) {
            let label = if f.name == "eq_preamp" {
                "eq pre-amp"
            } else {
                f.name
            };
            let _ = writeln!(s, "  {:<18}{}", label, feature_display(f, v));
        }
    }
    let bands: Vec<String> = p
        .eq_bands()
        .iter()
        .enumerate()
        .filter_map(|(i, v)| v.map(|v| format!("{}={}", EQ_BANDS[i], v)))
        .collect();
    if !bands.is_empty() {
        let _ = writeln!(s, "  {:<18}{}", "eq bands (dB)", bands.join(" "));
    }
    for f in FEATURES
        .iter()
        .filter(|f| f.family != FAMILY_PLAYBACK && features::per_output(f) && f.exposed)
    {
        if let Some(v) = p.get(f) {
            let _ = writeln!(s, "  {:<18}{}", f.name, feature_display(f, v));
        }
    }
}

/// Status as JSON. Raw device values, except `mic_nr_level` which is shown
/// 0..1 (the EQ pre-amp is `eq_preamp`, in dB). `direct_mode` is added.
pub fn status_json(state: &DeviceState, direct_mode: bool) -> Result<String, serde_json::Error> {
    let mut v = serde_json::to_value(state)?;
    if let Some(obj) = v.as_object_mut() {
        for key in ["headphones", "speakers", "common"] {
            let Some(profile) = obj.get_mut(key).and_then(Value::as_object_mut) else {
                continue;
            };
            for f in FEATURES.iter().filter(|f| !f.exposed) {
                profile.remove(f.name);
            }
            // Values are f32 on the device. Mic NR is shown 0..1.
            for (name, value) in profile.iter_mut() {
                if let Some(dev) = value.as_f64() {
                    let scale = features::by_name(name).map_or(1.0, |f| f.ui_scale);
                    *value = Value::from(round_ui(dev, scale));
                }
            }
        }
        obj.insert("direct_mode".into(), Value::Bool(direct_mode));
    }
    serde_json::to_string_pretty(&v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use g6_core::features::by_name;
    use g6_core::protocol::DacFilter;

    fn f(name: &str) -> &'static Feature {
        by_name(name).unwrap()
    }

    #[test]
    fn displays_values() {
        assert_eq!(feature_display(f("surround"), 1.0), "on");
        assert_eq!(feature_display(f("surround"), 0.0), "off");
        assert_eq!(feature_display(f("bass_level"), 0.5), "0.50");
        assert_eq!(feature_display(f("eq_1k"), -1.0), "-1 dB");
        assert_eq!(feature_display(f("eq_1k"), 0.5), "0.5 dB");
        assert_eq!(feature_display(f("surround_distance"), 150.5), "150.5");
        assert_eq!(feature_display(f("decoder_mode"), 3.0), "night");
        assert_eq!(feature_display(f("smartvol_mode"), 1.0), "loud");
        // mic NR: device 0.25 is UI 0.50
        assert_eq!(feature_display(f("mic_nr_level"), 0.25), "0.50");
    }

    #[test]
    fn event_lines() {
        assert_eq!(event_line(&Event::Connected), "connected");
        assert_eq!(
            event_line(&Event::OutputSwitched(Output::Speakers)),
            "output: speakers"
        );
        assert_eq!(
            event_line(&Event::Changed(Change::Mask(Mask(0x05)))),
            "mask: sbx on, scout off, voice clarity on"
        );
        assert_eq!(
            event_line(&Event::Changed(Change::LightingEnabled(false))),
            "light: off"
        );
        assert_eq!(
            event_line(&Event::Changed(Change::LightingColor([255, 128, 0]))),
            "light color: ff8000"
        );
    }

    #[test]
    fn event_json_has_stable_keys_for_every_event() {
        let ts = 12.3456;
        let j = |e: Event| event_json(&e, ts);
        assert_eq!(
            j(Event::Connected),
            json!({"ts": 12.346, "event": "connected"})
        );
        assert_eq!(
            j(Event::Disconnected),
            json!({"ts": 12.346, "event": "disconnected"})
        );
        assert_eq!(
            j(Event::OutputSwitched(Output::Speakers)),
            json!({"ts": 12.346, "event": "output", "output": "speakers"})
        );
        assert_eq!(
            j(Event::Changed(Change::Feature {
                feature: f("eq_1k"),
                value: -1.5
            })),
            json!({"ts": 12.346, "event": "feature", "name": "eq_1k", "value": -1.5, "text": "-1.5 dB"})
        );
        // Mic NR: device 0.25 is shown 0.5, as in `status --json`.
        assert_eq!(
            j(Event::Changed(Change::Feature {
                feature: f("mic_nr_level"),
                value: 0.25
            })),
            json!({"ts": 12.346, "event": "feature", "name": "mic_nr_level", "value": 0.5, "text": "0.50"})
        );
        assert_eq!(
            j(Event::Changed(Change::Feature {
                feature: f("decoder_mode"),
                value: 3.0
            })),
            json!({"ts": 12.346, "event": "feature", "name": "decoder_mode", "value": 3.0, "text": "night"})
        );
        assert_eq!(
            j(Event::Changed(Change::Mask(Mask(0x05)))),
            json!({"ts": 12.346, "event": "mask", "sbx": true, "scout": false, "voice_clarity": true})
        );
        assert_eq!(
            j(Event::Changed(Change::DacFilter(DacFilter::Nos))),
            json!({"ts": 12.346, "event": "filter", "filter": "nos"})
        );
        assert_eq!(
            j(Event::Changed(Change::MicBoost(MicBoost::Db20))),
            json!({"ts": 12.346, "event": "mic_boost", "db": 20})
        );
        assert_eq!(
            j(Event::Changed(Change::LightingEnabled(false))),
            json!({"ts": 12.346, "event": "light", "on": false})
        );
        assert_eq!(
            j(Event::Changed(Change::LightingColor([255, 128, 0]))),
            json!({"ts": 12.346, "event": "light_color", "color": "ff8000"})
        );
    }

    #[test]
    fn status_shows_the_light() {
        let mut st = DeviceState::new();
        let t = status_text(&st, false);
        assert!(t.contains("light               unknown"), "{t}");
        assert!(t.contains("light color         unknown"), "{t}");
        st.lighting_enabled = Some(true);
        st.lighting_color = Some([0, 255, 0]);
        let t = status_text(&st, false);
        assert!(t.contains("light               on"), "{t}");
        assert!(t.contains("light color         00ff00"), "{t}");
        let v: Value = serde_json::from_str(&status_json(&st, false).unwrap()).unwrap();
        assert_eq!(v["lighting_enabled"], true);
        assert_eq!(v["lighting_color"], "00ff00");
    }

    #[test]
    fn json_has_preamp_and_scales_nr() {
        let mut st = DeviceState::new();
        st.output = Some(Output::Headphones);
        st.set_feature(f("eq_preamp"), -2.5);
        st.set_feature(f("surround"), 1.0);
        st.set_feature(f("mic_nr_level"), 0.25);
        let v: Value = serde_json::from_str(&status_json(&st, true).unwrap()).unwrap();
        assert_eq!(v["direct_mode"], true);
        assert_eq!(v["output"], "headphones");
        assert_eq!(v["headphones"]["surround"], 1.0);
        assert_eq!(v["headphones"]["eq_preamp"], -2.5);
        assert_eq!(v["headphones"]["mic_nr_level"], 0.5);
    }

    #[test]
    fn text_marks_active_output_and_shows_preamp() {
        let mut st = DeviceState::new();
        st.output = Some(Output::Speakers);
        st.set_feature(f("eq_preamp"), -2.5);
        st.set_feature(f("crystalizer"), 1.0);
        st.set_feature(f("eq_31"), -1.0);
        let t = status_text(&st, false);
        assert!(t.contains("[speakers] (active)"));
        assert!(t.contains("[headphones] (last known)"));
        assert!(t.contains("crystalizer       on"));
        assert!(t.contains("31=-1"));
        assert!(t.contains("eq pre-amp        -2.5 dB"), "{t}");
        assert!(!t.contains("eq_preamp"), "{t}");
    }
}
