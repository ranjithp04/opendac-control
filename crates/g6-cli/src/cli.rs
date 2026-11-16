//! Command-line grammar (clap). Parsing only: range checks against the
//! feature table happen in [`crate::action`].

use std::str::FromStr;

use clap::builder::PossibleValuesParser;
use clap::{Args, Parser, Subcommand, ValueEnum};
use g6_core::features;
use g6_core::protocol::DacFilter;

#[derive(Debug, Parser)]
#[command(
    name = "g6",
    version,
    about = "Control a Creative Sound BlasterX G6 over USB",
    arg_required_else_help = true
)]
pub struct Cli {
    /// More logging on stderr (-v warnings, -vv debug).
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    /// Seconds to wait for the G6 to answer before giving up.
    #[arg(
        long,
        value_name = "SECS",
        default_value_t = 3,
        global = true,
        value_parser = clap::value_parser!(u64).range(1..=60)
    )]
    pub connect_timeout: u64,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show firmware version and hardware ids.
    Info,
    /// Show the full device state.
    Status(StatusArgs),
    /// Stream changes pushed by the device (buttons, jack switch, replug). Ctrl-C to stop.
    Watch,
    /// Equalizer: switch on/off, set band gains or the pre-amp.
    #[command(subcommand)]
    Eq(EqCommand),
    /// SBX on or off.
    Sbx(ToggleArg),
    /// Scout mode on or off.
    Scout(ToggleArg),
    /// Surround: on, off, or a level 0..1 (a level also switches it on).
    Surround(SurroundArgs),
    /// Crystalizer: on, off, or a level 0..1 (a level also switches it on).
    Crystalizer(EffectArgs),
    /// Bass: on, off, or a level 0..1 (a level also switches it on).
    Bass(EffectArgs),
    /// Dialog Plus: on, off, or a level 0..1 (a level also switches it on).
    Dialog(EffectArgs),
    /// Smart Volume: on, off, a level 0..1, and/or --mode.
    Smartvol(SmartvolArgs),
    /// Dolby decoder mode (optical input).
    Dolby(DolbyArgs),
    /// DAC digital filter.
    Filter(FilterArgs),
    /// Microphone settings.
    #[command(subcommand)]
    Mic(MicCommand),
    /// Direct Mode: SBX, Scout and EQ off (restored when switched off).
    Direct(ToggleArg),
    /// Save, load, list or delete presets.
    #[command(subcommand)]
    Preset(PresetCommand),
}

#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Print machine-readable JSON. Playback and mic values are the raw
    /// device values (on/off = 1/0), except mic_nr_level, which is 0..1.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Toggle {
    On,
    Off,
}

impl Toggle {
    pub fn is_on(self) -> bool {
        self == Toggle::On
    }
}

#[derive(Debug, Args)]
pub struct ToggleArg {
    /// on or off.
    #[arg(value_enum)]
    pub state: Toggle,
}

/// `on`, `off`, or a level.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Setting {
    On,
    Off,
    Level(f32),
}

impl FromStr for Setting {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "on" => Ok(Setting::On),
            "off" => Ok(Setting::Off),
            other => match other.parse::<f32>() {
                Ok(v) if v.is_finite() => Ok(Setting::Level(v)),
                _ => Err("expected on, off or a number from 0 to 1".to_string()),
            },
        }
    }
}

#[derive(Debug, Args)]
pub struct EffectArgs {
    /// on, off or a level 0..1.
    #[arg(value_name = "on|off|0..1", allow_hyphen_values = true)]
    pub value: Setting,
}

#[derive(Debug, Args)]
pub struct SurroundArgs {
    /// on, off or a level 0..1.
    #[arg(value_name = "on|off|0..1")]
    pub value: Option<Setting>,
    /// Surround distance, 10..300 in steps of 0.5.
    #[arg(long, value_name = "10..300")]
    pub distance: Option<f32>,
}

#[derive(Debug, Args)]
pub struct SmartvolArgs {
    /// on, off or a level 0..1.
    #[arg(value_name = "on|off|0..1")]
    pub value: Option<Setting>,
    /// Smart Volume mode.
    #[arg(long, value_parser = choices("smartvol_mode"))]
    pub mode: Option<String>,
}

#[derive(Debug, Args)]
pub struct DolbyArgs {
    /// Decoder mode.
    #[arg(value_parser = choices("decoder_mode"))]
    pub mode: String,
}

#[derive(Debug, Args)]
pub struct FilterArgs {
    /// Filter name.
    #[arg(value_parser = PossibleValuesParser::new(DacFilter::ALL.map(DacFilter::name)))]
    pub filter: String,
}

#[derive(Debug, Subcommand)]
pub enum EqCommand {
    /// Switch the equalizer on.
    On,
    /// Switch the equalizer off.
    Off,
    /// Set band gains in dB, e.g. `eq set 31=3 62=2 1k=-1`. Bands: 31 62 125
    /// 250 500 1k 2k 4k 8k 16k. Range -12..12 in steps of 0.5. Does not change
    /// whether the EQ is on.
    Set {
        /// BAND=DB pairs.
        #[arg(required = true, value_name = "BAND=DB")]
        bands: Vec<String>,
    },
    /// Set the EQ pre-amp in dB. Range -6..6 in steps of 0.5. Does not change
    /// whether the EQ is on.
    Preamp {
        #[arg(value_name = "-6..6", allow_hyphen_values = true)]
        db: f32,
    },
}

#[derive(Debug, Subcommand)]
pub enum MicCommand {
    /// Mic boost in dB.
    Boost {
        /// 0, 10, 20 or 30.
        #[arg(value_parser = ["0", "10", "20", "30"])]
        db: String,
    },
    /// Voice Clarity noise reduction on or off.
    Clarity(ToggleArg),
    /// Voice Clarity noise reduction level 0..1.
    Nr {
        #[arg(value_name = "0..1", allow_hyphen_values = true)]
        level: f32,
    },
    /// Mic equalizer on or off.
    Eq(ToggleArg),
    /// Acoustic echo cancellation on or off.
    Aec(ToggleArg),
}

#[derive(Debug, Subcommand)]
pub enum PresetCommand {
    /// Save the current settings under NAME.
    Save { name: String },
    /// Apply preset NAME to the active output.
    Load { name: String },
    /// List saved presets.
    List,
    /// Delete preset NAME.
    Delete { name: String },
}

/// Allowed names of a `Kind::Choice` feature, straight from the feature table.
fn choices(feature: &str) -> PossibleValuesParser {
    let names: Vec<&'static str> = match features::by_name(feature).map(|f| f.kind) {
        Some(features::Kind::Choice(list)) => list.iter().map(|(_, n)| *n).collect(),
        _ => Vec::new(),
    };
    PossibleValuesParser::new(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;
    use clap::CommandFactory;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        let mut argv = vec!["g6"];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv)
    }

    fn kind(args: &[&str]) -> ErrorKind {
        parse(args).unwrap_err().kind()
    }

    #[test]
    fn grammar_is_consistent() {
        Cli::command().debug_assert();
    }

    /// Every documented `g6` command line (README), with concrete values.
    #[test]
    fn every_plan_command_line_parses() {
        let lines: &[&[&str]] = &[
            &["info"],
            &["status"],
            &["status", "--json"],
            &["watch"],
            &["eq", "on"],
            &["eq", "off"],
            &["eq", "set", "31=3", "62=2", "1k=-1"],
            &["sbx", "on"],
            &["sbx", "off"],
            &["scout", "on"],
            &["scout", "off"],
            &["surround", "on"],
            &["surround", "off"],
            &["surround", "0.5"],
            &["surround", "0.5", "--distance", "120"],
            &["surround", "--distance", "300"],
            &["crystalizer", "on"],
            &["crystalizer", "0.25"],
            &["bass", "off"],
            &["bass", "1"],
            &["dialog", "on"],
            &["dialog", "0"],
            &["smartvol", "on"],
            &["smartvol", "0.9"],
            &["smartvol", "on", "--mode", "normal"],
            &["smartvol", "--mode", "loud"],
            &["smartvol", "--mode", "night"],
            &["dolby", "full"],
            &["dolby", "normal"],
            &["dolby", "night"],
            &["filter", "fast-min"],
            &["filter", "slow-min"],
            &["filter", "nos"],
            &["filter", "fast-lin"],
            &["filter", "slow-lin"],
            &["mic", "boost", "0"],
            &["mic", "boost", "10"],
            &["mic", "boost", "20"],
            &["mic", "boost", "30"],
            &["mic", "clarity", "on"],
            &["mic", "nr", "0.5"],
            &["mic", "eq", "off"],
            &["mic", "aec", "on"],
            &["direct", "on"],
            &["direct", "off"],
            &["preset", "save", "movies"],
            &["preset", "load", "movies"],
            &["preset", "list"],
            &["preset", "delete", "movies"],
        ];
        for line in lines {
            parse(line).unwrap_or_else(|e| panic!("`g6 {}` failed: {e}", line.join(" ")));
        }
    }

    #[test]
    fn commands_map_to_the_right_variants() {
        assert!(matches!(parse(&["info"]).unwrap().command, Command::Info));
        assert!(matches!(
            parse(&["status", "--json"]).unwrap().command,
            Command::Status(StatusArgs { json: true })
        ));
        assert!(matches!(
            parse(&["status"]).unwrap().command,
            Command::Status(StatusArgs { json: false })
        ));
        assert!(matches!(parse(&["watch"]).unwrap().command, Command::Watch));
        assert!(matches!(
            parse(&["eq", "on"]).unwrap().command,
            Command::Eq(EqCommand::On)
        ));
        let Command::Eq(EqCommand::Set { bands }) = parse(&["eq", "set", "31=3", "62=2", "1k=-1"])
            .unwrap()
            .command
        else {
            panic!("not eq set")
        };
        assert_eq!(bands, ["31=3", "62=2", "1k=-1"]);
        assert!(matches!(
            parse(&["sbx", "on"]).unwrap().command,
            Command::Sbx(ToggleArg { state: Toggle::On })
        ));
        assert!(matches!(
            parse(&["scout", "off"]).unwrap().command,
            Command::Scout(ToggleArg { state: Toggle::Off })
        ));
        assert!(matches!(
            parse(&["direct", "on"]).unwrap().command,
            Command::Direct(ToggleArg { state: Toggle::On })
        ));
        let Command::Smartvol(a) = parse(&["smartvol", "0.9", "--mode", "night"])
            .unwrap()
            .command
        else {
            panic!()
        };
        assert_eq!(a.value, Some(Setting::Level(0.9)));
        assert_eq!(a.mode.as_deref(), Some("night"));
        let Command::Surround(a) = parse(&["surround", "--distance", "150.5"]).unwrap().command
        else {
            panic!()
        };
        assert_eq!((a.value, a.distance), (None, Some(150.5)));
        let Command::Preset(PresetCommand::Save { name }) =
            parse(&["preset", "save", "my preset"]).unwrap().command
        else {
            panic!()
        };
        assert_eq!(name, "my preset");
    }

    #[test]
    fn global_options() {
        let c = parse(&["info"]).unwrap();
        assert_eq!((c.verbose, c.connect_timeout), (0, 3));
        assert_eq!(parse(&["-v", "info"]).unwrap().verbose, 1);
        assert_eq!(parse(&["-vv", "info"]).unwrap().verbose, 2);
        assert_eq!(parse(&["info", "-v", "-v"]).unwrap().verbose, 2);
        // Global options work before and after the subcommand, also nested.
        assert_eq!(
            parse(&["--connect-timeout", "10", "info"])
                .unwrap()
                .connect_timeout,
            10
        );
        assert_eq!(
            parse(&["eq", "on", "--connect-timeout", "7"])
                .unwrap()
                .connect_timeout,
            7
        );
        assert_eq!(
            parse(&["mic", "boost", "20", "--connect-timeout=5"])
                .unwrap()
                .connect_timeout,
            5
        );
        // 1..=60 seconds.
        assert_eq!(
            parse(&["--connect-timeout", "1", "info"])
                .unwrap()
                .connect_timeout,
            1
        );
        assert_eq!(
            parse(&["--connect-timeout", "60", "info"])
                .unwrap()
                .connect_timeout,
            60
        );
        for bad in ["0", "61", "abc", "1.5", ""] {
            assert_eq!(
                kind(&["--connect-timeout", bad, "info"]),
                ErrorKind::ValueValidation,
                "{bad:?}"
            );
        }
    }

    #[test]
    fn negative_connect_timeout_is_rejected() {
        // "-1" is read as an unknown short option, not as a value.
        assert!(parse(&["--connect-timeout", "-1", "info"]).is_err());
        assert!(parse(&["--connect-timeout=-1", "info"]).is_err());
    }

    #[test]
    fn help_version_and_no_arguments() {
        assert_eq!(kind(&["--help"]), ErrorKind::DisplayHelp);
        assert_eq!(kind(&["-h"]), ErrorKind::DisplayHelp);
        assert_eq!(kind(&["--version"]), ErrorKind::DisplayVersion);
        // No subcommand: show help (arg_required_else_help).
        assert_eq!(
            kind(&[]),
            ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        );
        for sub in ["eq", "mic", "preset"] {
            assert_eq!(
                kind(&[sub]),
                ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand,
                "{sub}"
            );
        }
    }

    #[test]
    fn unknown_and_misspelled_input_is_rejected() {
        assert_eq!(kind(&["frobnicate"]), ErrorKind::InvalidSubcommand);
        assert_eq!(kind(&["info", "--bogus"]), ErrorKind::UnknownArgument);
        assert_eq!(kind(&["status", "--yaml"]), ErrorKind::UnknownArgument);
        assert_eq!(kind(&["info", "extra"]), ErrorKind::UnknownArgument);
        assert_eq!(kind(&["eq", "toggle"]), ErrorKind::InvalidSubcommand);
        assert_eq!(kind(&["mic", "gain", "1"]), ErrorKind::InvalidSubcommand);
        assert_eq!(
            kind(&["preset", "rename", "a", "b"]),
            ErrorKind::InvalidSubcommand
        );
    }

    #[test]
    fn missing_values_are_rejected() {
        for args in [
            &["sbx"][..],
            &["scout"],
            &["direct"],
            &["bass"],
            &["crystalizer"],
            &["dialog"],
            &["dolby"],
            &["filter"],
            &["eq", "set"],
            &["mic", "boost"],
            &["mic", "nr"],
            &["mic", "clarity"],
            &["preset", "save"],
            &["preset", "load"],
            &["preset", "delete"],
        ] {
            assert_eq!(
                kind(args),
                ErrorKind::MissingRequiredArgument,
                "g6 {}",
                args.join(" ")
            );
        }
    }

    #[test]
    fn options_without_a_value_are_rejected() {
        assert!(parse(&["surround", "--distance"]).is_err());
        assert!(parse(&["smartvol", "--mode"]).is_err());
        assert!(parse(&["--connect-timeout"]).is_err());
    }

    #[test]
    fn choice_values_come_from_the_tables() {
        for ok in ["on", "off"] {
            assert!(parse(&["sbx", ok]).is_ok());
        }
        for bad in ["yes", "1", "true", ""] {
            assert_eq!(kind(&["sbx", bad]), ErrorKind::InvalidValue, "sbx {bad:?}");
        }
        for f in DacFilter::ALL {
            assert!(parse(&["filter", f.name()]).is_ok(), "{}", f.name());
        }
        for bad in ["fast", "nos-lin", "FAST-MIN", "0", "1"] {
            assert_eq!(
                kind(&["filter", bad]),
                ErrorKind::InvalidValue,
                "filter {bad}"
            );
        }
        for ok in ["full", "normal", "night"] {
            assert!(parse(&["dolby", ok]).is_ok());
        }
        assert_eq!(kind(&["dolby", "loud"]), ErrorKind::InvalidValue);
        for ok in ["normal", "loud", "night"] {
            assert!(parse(&["smartvol", "--mode", ok]).is_ok());
        }
        assert_eq!(
            kind(&["smartvol", "--mode", "quiet"]),
            ErrorKind::InvalidValue
        );
        for ok in ["0", "10", "20", "30"] {
            assert!(parse(&["mic", "boost", ok]).is_ok());
        }
        for bad in ["5", "40", "-10", "20dB"] {
            assert!(parse(&["mic", "boost", bad]).is_err(), "boost {bad}");
        }
    }

    #[test]
    fn negative_numbers_are_values_not_options() {
        // Range checks happen later (action.rs); parsing must keep the sign.
        let Command::Bass(a) = parse(&["bass", "-0.5"]).unwrap().command else {
            panic!()
        };
        assert_eq!(a.value, Setting::Level(-0.5));
        let Command::Mic(MicCommand::Nr { level }) = parse(&["mic", "nr", "-1"]).unwrap().command
        else {
            panic!()
        };
        assert_eq!(level, -1.0);
        let Command::Eq(EqCommand::Set { bands }) =
            parse(&["eq", "set", "16k=-12"]).unwrap().command
        else {
            panic!()
        };
        assert_eq!(bands, ["16k=-12"]);
    }

    #[test]
    fn setting_parses_on_off_and_numbers() {
        assert_eq!("on".parse(), Ok(Setting::On));
        assert_eq!("ON".parse(), Ok(Setting::On));
        assert_eq!("Off".parse(), Ok(Setting::Off));
        assert_eq!("0.5".parse(), Ok(Setting::Level(0.5)));
        assert_eq!("1".parse(), Ok(Setting::Level(1.0)));
        assert_eq!("-2".parse(), Ok(Setting::Level(-2.0)));
        assert_eq!(".5".parse(), Ok(Setting::Level(0.5)));
        for bad in [
            "", "high", "nan", "NaN", "inf", "-inf", "1,5", "0..1", "on off",
        ] {
            assert!(bad.parse::<Setting>().is_err(), "{bad:?}");
        }
    }
}
