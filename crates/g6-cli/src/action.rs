//! Turns parsed arguments into an [`Action`]: every value is checked against
//! the feature table here, before any device is opened.

use std::fmt;

use g6_core::features::{self, parse_eq_band, Feature, Kind, EQ_BANDS};
use g6_core::protocol::{DacFilter, MaskTarget, MicBoost};

use crate::cli::{
    Command, EffectArgs, EqCommand, MicCommand, PresetCommand, Setting, SmartvolArgs, SurroundArgs,
};

/// Bad user input.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageError(pub String);

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UsageError {}

fn usage<T>(msg: impl Into<String>) -> Result<T, UsageError> {
    Err(UsageError(msg.into()))
}

/// One validated device write.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Op {
    /// Feature write; the value is device-side and already validated.
    Feature(&'static Feature, f32),
    Mask(MaskTarget, bool),
    DacFilter(DacFilter),
    MicBoost(MicBoost),
}

#[derive(Debug, Clone, PartialEq)]
pub enum PresetAction {
    Save(String),
    Load(String),
    List,
    Delete(String),
}

/// What a command line asks for.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Info,
    Status {
        json: bool,
    },
    Watch,
    /// Device writes, in order.
    Apply(Vec<Op>),
    Direct(bool),
    Preset(PresetAction),
}

pub fn build(command: Command) -> Result<Action, UsageError> {
    Ok(match command {
        Command::Info => Action::Info,
        Command::Status(a) => Action::Status { json: a.json },
        Command::Watch => Action::Watch,
        Command::Eq(EqCommand::On) => Action::Apply(vec![feature_op("eq", 1.0)?]),
        Command::Eq(EqCommand::Off) => Action::Apply(vec![feature_op("eq", 0.0)?]),
        Command::Eq(EqCommand::Set { bands }) => Action::Apply(eq_set(&bands)?),
        Command::Sbx(t) => Action::Apply(vec![Op::Mask(MaskTarget::Sbx, t.state.is_on())]),
        Command::Scout(t) => Action::Apply(vec![Op::Mask(MaskTarget::Scout, t.state.is_on())]),
        Command::Surround(a) => Action::Apply(surround(&a)?),
        Command::Crystalizer(a) => Action::Apply(effect("crystalizer", &a)?),
        Command::Bass(a) => Action::Apply(effect("bass", &a)?),
        Command::Dialog(a) => Action::Apply(effect("dialog", &a)?),
        Command::Smartvol(a) => Action::Apply(smartvol(&a)?),
        Command::Dolby(a) => Action::Apply(vec![choice_op("decoder_mode", &a.mode)?]),
        Command::Filter(a) => {
            let Some(filter) = DacFilter::from_name(&a.filter) else {
                return usage(format!("unknown filter '{}'", a.filter));
            };
            Action::Apply(vec![Op::DacFilter(filter)])
        }
        Command::Mic(m) => Action::Apply(vec![mic(m)?]),
        Command::Direct(t) => Action::Direct(t.state.is_on()),
        Command::Preset(p) => Action::Preset(match p {
            PresetCommand::Save { name } => PresetAction::Save(name),
            PresetCommand::Load { name } => PresetAction::Load(name),
            PresetCommand::List => PresetAction::List,
            PresetCommand::Delete { name } => PresetAction::Delete(name),
        }),
    })
}

fn feature(name: &str) -> Result<&'static Feature, UsageError> {
    match features::by_name(name) {
        Some(f) => Ok(f),
        None => usage(format!(
            "internal error: feature '{name}' is not in the table"
        )),
    }
}

/// Write of a device-side on/off value to a Bool feature.
fn feature_op(name: &str, device_value: f32) -> Result<Op, UsageError> {
    let f = feature(name)?;
    match f.validate_write(device_value) {
        Ok(v) => Ok(Op::Feature(f, v)),
        Err(e) => usage(e.to_string()),
    }
}

/// Validate a value as the user sees it (UI scale) and build the write.
fn ui_op(name: &str, label: &str, ui: f32) -> Result<Op, UsageError> {
    let f = feature(name)?;
    let (lo, hi) = (f.to_ui(f.min), f.to_ui(f.max));
    if !ui.is_finite() || ui < lo || ui > hi {
        return usage(format!("{label}: {ui} is outside the range {lo}..{hi}"));
    }
    if matches!(f.kind, Kind::Db | Kind::Distance) {
        let steps = ui / f.step;
        if (steps - steps.round()).abs() > 1e-3 {
            return usage(format!("{label}: {ui} is not a multiple of {}", f.step));
        }
    }
    match f.validate_write(f.from_ui(ui)) {
        Ok(v) => Ok(Op::Feature(f, v)),
        Err(e) => usage(format!("{label}: {e}")),
    }
}

fn choice_op(name: &str, choice: &str) -> Result<Op, UsageError> {
    let f = feature(name)?;
    match f.choice_value(choice) {
        Some(v) => Ok(Op::Feature(f, v)),
        None => usage(format!("unknown value '{choice}' for {name}")),
    }
}

fn eq_set(pairs: &[String]) -> Result<Vec<Op>, UsageError> {
    let mut seen = [false; EQ_BANDS.len()];
    let mut ops = Vec::new();
    for pair in pairs {
        let Some((band, gain)) = pair.split_once('=') else {
            return usage(format!("'{pair}': expected BAND=DB, e.g. 1k=-1"));
        };
        let Some(idx) = parse_eq_band(band) else {
            return usage(format!(
                "'{pair}': unknown EQ band '{band}' (bands: {})",
                EQ_BANDS.join(" ")
            ));
        };
        let Ok(db) = gain.trim().parse::<f32>() else {
            return usage(format!("'{pair}': '{gain}' is not a number of dB"));
        };
        if std::mem::replace(&mut seen[idx], true) {
            return usage(format!("EQ band {} given twice", EQ_BANDS[idx]));
        }
        let name = features::eq_band(idx).map_or("eq", |f| f.name);
        ops.push(ui_op(name, &format!("EQ {}", EQ_BANDS[idx]), db)?);
    }
    Ok(ops)
}

/// `on` / `off` / level for a feature pair `NAME` + `NAME_level`.
fn toggle_or_level(name: &str, setting: Setting) -> Result<Vec<Op>, UsageError> {
    Ok(match setting {
        Setting::On => vec![feature_op(name, 1.0)?],
        Setting::Off => vec![feature_op(name, 0.0)?],
        Setting::Level(v) => vec![
            ui_op(&format!("{name}_level"), &format!("{name} level"), v)?,
            feature_op(name, 1.0)?,
        ],
    })
}

fn effect(name: &str, a: &EffectArgs) -> Result<Vec<Op>, UsageError> {
    toggle_or_level(name, a.value)
}

fn surround(a: &SurroundArgs) -> Result<Vec<Op>, UsageError> {
    if a.value.is_none() && a.distance.is_none() {
        return usage("surround: give on, off, a level 0..1 and/or --distance 10..300");
    }
    let mut ops = Vec::new();
    if let Some(d) = a.distance {
        ops.push(ui_op("surround_distance", "surround distance", d)?);
    }
    if let Some(v) = a.value {
        ops.extend(toggle_or_level("surround", v)?);
    }
    Ok(ops)
}

fn smartvol(a: &SmartvolArgs) -> Result<Vec<Op>, UsageError> {
    if a.value.is_none() && a.mode.is_none() {
        return usage("smartvol: give on, off, a level 0..1 and/or --mode normal|loud|night");
    }
    let mut ops = Vec::new();
    if let Some(m) = &a.mode {
        ops.push(choice_op("smartvol_mode", m)?);
    }
    if let Some(v) = a.value {
        ops.extend(toggle_or_level("smartvol", v)?);
    }
    Ok(ops)
}

fn mic(cmd: MicCommand) -> Result<Op, UsageError> {
    Ok(match cmd {
        MicCommand::Boost { db } => match db.parse::<u8>().ok().and_then(MicBoost::from_db) {
            Some(b) => Op::MicBoost(b),
            None => return usage(format!("mic boost must be 0, 10, 20 or 30 dB, not '{db}'")),
        },
        MicCommand::Clarity(t) => feature_op("mic_clarity", f32::from(t.state.is_on()))?,
        MicCommand::Nr { level } => ui_op("mic_nr_level", "mic noise reduction level", level)?,
        MicCommand::Eq(t) => feature_op("mic_eq", f32::from(t.state.is_on()))?,
        MicCommand::Aec(t) => feature_op("mic_aec", f32::from(t.state.is_on()))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;
    use clap::Parser;

    fn parse(args: &[&str]) -> Result<Action, String> {
        let mut argv = vec!["g6"];
        argv.extend_from_slice(args);
        let cli = Cli::try_parse_from(argv).map_err(|e| e.to_string())?;
        build(cli.command).map_err(|e| e.to_string())
    }

    fn ops(args: &[&str]) -> Vec<(String, f32)> {
        match parse(args).unwrap() {
            Action::Apply(ops) => ops
                .iter()
                .map(|op| match op {
                    Op::Feature(f, v) => (f.name.to_string(), *v),
                    other => (format!("{other:?}"), 0.0),
                })
                .collect(),
            a => panic!("not Apply: {a:?}"),
        }
    }

    fn err(args: &[&str]) -> String {
        parse(args).unwrap_err()
    }

    #[test]
    fn eq_on_off() {
        assert_eq!(ops(&["eq", "on"]), [("eq".into(), 1.0)]);
        assert_eq!(ops(&["eq", "off"]), [("eq".into(), 0.0)]);
    }

    #[test]
    fn eq_set_bands() {
        assert_eq!(
            ops(&["eq", "set", "31=3", "1k=-1", "16kHz=0.5", "250=-12"]),
            [
                ("eq_31".into(), 3.0),
                ("eq_1k".into(), -1.0),
                ("eq_16k".into(), 0.5),
                ("eq_250".into(), -12.0),
            ]
        );
    }

    #[test]
    fn eq_set_rejects_bad_input() {
        assert!(err(&["eq", "set", "1k=13"]).contains("outside the range -12..12"));
        assert!(err(&["eq", "set", "1k=0.3"]).contains("not a multiple of 0.5"));
        assert!(err(&["eq", "set", "3k=1"]).contains("unknown EQ band"));
        assert!(err(&["eq", "set", "1k"]).contains("expected BAND=DB"));
        assert!(err(&["eq", "set", "1k=abc"]).contains("not a number"));
        assert!(err(&["eq", "set", "1k=1", "1000=2"]).contains("twice"));
        assert!(err(&["eq", "set", "1k=NaN"]).contains("outside"));
        assert!(err(&["eq", "set"]).contains("required"));
    }

    #[test]
    fn effects_toggle_and_level() {
        assert_eq!(ops(&["bass", "on"]), [("bass".into(), 1.0)]);
        assert_eq!(ops(&["dialog", "OFF"]), [("dialog".into(), 0.0)]);
        assert_eq!(
            ops(&["crystalizer", "0.5"]),
            [
                ("crystalizer_level".into(), 0.5),
                ("crystalizer".into(), 1.0)
            ]
        );
        assert!(err(&["bass", "1.5"]).contains("outside the range 0..1"));
        assert!(err(&["bass", "loud"]).contains("expected on, off"));
        assert!(err(&["bass", "NaN"]).contains("expected on, off"));
    }

    #[test]
    fn surround_level_and_distance() {
        assert_eq!(
            ops(&["surround", "0.7", "--distance", "150.5"]),
            [
                ("surround_distance".into(), 150.5),
                ("surround_level".into(), 0.7),
                ("surround".into(), 1.0)
            ]
        );
        assert_eq!(
            ops(&["surround", "--distance", "10"]),
            [("surround_distance".into(), 10.0)]
        );
        assert!(err(&["surround"]).contains("give on, off"));
        assert!(err(&["surround", "--distance", "5"]).contains("outside the range 10..300"));
        assert!(err(&["surround", "--distance", "301"]).contains("outside"));
        assert!(err(&["surround", "--distance", "10.2"]).contains("multiple of 0.5"));
    }

    #[test]
    fn smartvol_mode() {
        assert_eq!(
            ops(&["smartvol", "--mode", "night"]),
            [("smartvol_mode".into(), 2.0)]
        );
        assert_eq!(
            ops(&["smartvol", "on", "--mode", "loud"]),
            [("smartvol_mode".into(), 1.0), ("smartvol".into(), 1.0)]
        );
        assert!(err(&["smartvol"]).contains("give on, off"));
        assert!(err(&["smartvol", "--mode", "quiet"]).contains("invalid value"));
    }

    #[test]
    fn dolby_and_filter() {
        assert_eq!(ops(&["dolby", "full"]), [("decoder_mode".into(), 1.0)]);
        assert_eq!(ops(&["dolby", "night"]), [("decoder_mode".into(), 3.0)]);
        assert!(err(&["dolby", "loud"]).contains("invalid value"));
        assert_eq!(
            parse(&["filter", "slow-lin"]).unwrap(),
            Action::Apply(vec![Op::DacFilter(DacFilter::SlowLin)])
        );
        assert!(err(&["filter", "sharp"]).contains("invalid value"));
    }

    #[test]
    fn sbx_scout_direct() {
        assert_eq!(
            parse(&["sbx", "on"]).unwrap(),
            Action::Apply(vec![Op::Mask(MaskTarget::Sbx, true)])
        );
        assert_eq!(
            parse(&["scout", "off"]).unwrap(),
            Action::Apply(vec![Op::Mask(MaskTarget::Scout, false)])
        );
        assert_eq!(parse(&["direct", "on"]).unwrap(), Action::Direct(true));
        assert!(err(&["sbx", "maybe"]).contains("invalid value"));
    }

    #[test]
    fn mic_commands() {
        assert_eq!(
            parse(&["mic", "boost", "20"]).unwrap(),
            Action::Apply(vec![Op::MicBoost(MicBoost::Db20)])
        );
        assert!(err(&["mic", "boost", "15"]).contains("invalid value"));
        assert_eq!(
            ops(&["mic", "clarity", "on"]),
            [("mic_clarity".into(), 1.0)]
        );
        assert_eq!(ops(&["mic", "aec", "off"]), [("mic_aec".into(), 0.0)]);
        assert_eq!(ops(&["mic", "eq", "on"]), [("mic_eq".into(), 1.0)]);
        // UI 0..1 maps to device 0..0.5.
        assert_eq!(ops(&["mic", "nr", "1"]), [("mic_nr_level".into(), 0.5)]);
        assert_eq!(ops(&["mic", "nr", "0.5"]), [("mic_nr_level".into(), 0.25)]);
        assert!(err(&["mic", "nr", "1.5"]).contains("outside the range 0..1"));
    }

    #[test]
    fn simple_actions() {
        assert_eq!(parse(&["info"]).unwrap(), Action::Info);
        assert_eq!(parse(&["status"]).unwrap(), Action::Status { json: false });
        assert_eq!(
            parse(&["status", "--json"]).unwrap(),
            Action::Status { json: true }
        );
        assert_eq!(parse(&["watch"]).unwrap(), Action::Watch);
        assert_eq!(
            parse(&["preset", "save", "movies"]).unwrap(),
            Action::Preset(PresetAction::Save("movies".into()))
        );
        assert_eq!(
            parse(&["preset", "list"]).unwrap(),
            Action::Preset(PresetAction::List)
        );
        assert!(err(&["preset", "save"]).contains("required"));
        assert!(err(&["frobnicate"]).contains("unrecognized subcommand"));
    }

    #[test]
    fn pre_amp_is_never_written() {
        // No command produces an op for the non-exposed pre-amp.
        for args in [&["eq", "set", "31=1"][..], &["eq", "on"]] {
            for (name, _) in ops(args) {
                assert_ne!(name, "eq_preamp");
            }
        }
    }
}
