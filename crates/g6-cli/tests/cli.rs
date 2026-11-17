//! Runs the real `g6` binary for everything that must work, or fail, without
//! a device: help, usage errors (exit 2) and the preset commands that only
//! touch files. HOME and the XDG directories point at a temp dir, so nothing
//! of the user's is read or written.
//!
//! No test here may open the G6: each of them either fails argument
//! validation first or never needs the device.

use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

struct Home(PathBuf);

impl Home {
    fn new() -> Home {
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "g6-cli-it-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&p).unwrap();
        Home(p)
    }

    /// Where `g6` keeps presets with this HOME (`directories` rules).
    fn presets_dir(&self) -> PathBuf {
        if cfg!(target_os = "macos") {
            self.0
                .join("Library/Application Support/opendac-control/presets")
        } else {
            self.0.join("config/opendac-control/presets")
        }
    }

    fn g6(&self, args: &[&str]) -> Out {
        let out = Command::new(env!("CARGO_BIN_EXE_g6"))
            .args(args)
            .env("HOME", &self.0)
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("XDG_DATA_HOME", self.0.join("data"))
            .env_remove("RUST_LOG")
            .output()
            .expect("run g6");
        Out(out)
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Out(Output);

impl Out {
    fn code(&self) -> i32 {
        self.0.status.code().expect("g6 was killed by a signal")
    }
    fn stdout(&self) -> String {
        String::from_utf8_lossy(&self.0.stdout).into_owned()
    }
    fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.0.stderr).into_owned()
    }
}

fn g6(args: &[&str]) -> Out {
    Home::new().g6(args)
}

const SUBCOMMANDS: &[&str] = &[
    "info",
    "status",
    "watch",
    "serve",
    "eq",
    "sbx",
    "scout",
    "surround",
    "crystalizer",
    "bass",
    "dialog",
    "smartvol",
    "dolby",
    "filter",
    "mic",
    "direct",
    "light",
    "output",
    "preset",
];

#[test]
fn version_and_help_exit_zero() {
    let v = g6(&["--version"]);
    assert_eq!(v.code(), 0);
    assert!(v.stdout().starts_with("g6 "), "{}", v.stdout());

    for flag in ["--help", "-h"] {
        let h = g6(&[flag]);
        assert_eq!(h.code(), 0, "{flag}");
        let text = h.stdout();
        for sub in SUBCOMMANDS {
            assert!(
                text.lines().any(|l| l.trim_start().starts_with(sub)),
                "`{sub}` missing from --help:\n{text}"
            );
        }
        assert!(text.contains("--connect-timeout"));
    }
}

#[test]
fn every_subcommand_has_help() {
    let paths: &[&[&str]] = &[
        &["info"],
        &["status"],
        &["watch"],
        &["serve"],
        &["eq"],
        &["eq", "on"],
        &["eq", "off"],
        &["eq", "set"],
        &["eq", "preamp"],
        &["sbx"],
        &["scout"],
        &["surround"],
        &["crystalizer"],
        &["bass"],
        &["dialog"],
        &["smartvol"],
        &["dolby"],
        &["filter"],
        &["mic"],
        &["mic", "boost"],
        &["mic", "clarity"],
        &["mic", "nr"],
        &["mic", "eq"],
        &["mic", "aec"],
        &["direct"],
        &["light"],
        &["light", "on"],
        &["light", "off"],
        &["light", "color"],
        &["output"],
        &["preset"],
        &["preset", "save"],
        &["preset", "load"],
        &["preset", "list"],
        &["preset", "delete"],
    ];
    let home = Home::new();
    for p in paths {
        let mut args = p.to_vec();
        args.push("--help");
        let o = home.g6(&args);
        assert_eq!(o.code(), 0, "g6 {}: {}", args.join(" "), o.stderr());
        assert!(o.stdout().contains("Usage"), "g6 {}", args.join(" "));
    }
}

#[test]
fn choice_lists_appear_in_help() {
    let h = g6(&["filter", "--help"]).stdout();
    for f in ["fast-min", "slow-min", "nos", "fast-lin", "slow-lin"] {
        assert!(h.contains(f), "{f} missing:\n{h}");
    }
    let h = g6(&["dolby", "--help"]).stdout();
    for m in ["full", "normal", "night"] {
        assert!(h.contains(m), "{m} missing:\n{h}");
    }
    let h = g6(&["smartvol", "--help"]).stdout();
    for m in ["normal", "loud", "night"] {
        assert!(h.contains(m), "{m} missing:\n{h}");
    }
    let h = g6(&["mic", "boost", "--help"]).stdout();
    for m in ["0", "10", "20", "30"] {
        assert!(h.contains(m), "{m} missing:\n{h}");
    }
}

#[test]
fn no_arguments_prints_help_and_exits_2() {
    let o = g6(&[]);
    assert_eq!(o.code(), 2);
    assert!(o.stderr().contains("Usage"), "{}", o.stderr());
}

/// Each of these is bad input, found before the device is opened: exit 2,
/// a message on stderr, nothing on stdout, no mention of a missing G6.
#[test]
fn bad_input_exits_2_without_touching_the_device() {
    let cases: &[(&[&str], &str)] = &[
        // Range and step checks against the feature table.
        (&["eq", "set", "1k=13"], "outside the range -12..12"),
        (&["eq", "set", "1k=-12.5"], "outside"),
        (&["eq", "set", "1k=0.3"], "multiple of 0.5"),
        (&["eq", "set", "3k=1"], "unknown EQ band"),
        (&["eq", "set", "1k"], "BAND=DB"),
        (&["eq", "set", "1k=loud"], "not a number"),
        (&["eq", "set", "1k=1", "1000=2"], "twice"),
        (&["eq", "preamp", "6.5"], "outside the range -6..6"),
        (&["eq", "preamp", "-7"], "outside the range -6..6"),
        (&["eq", "preamp", "1.2"], "multiple of 0.5"),
        (&["eq", "preamp", "loud"], "invalid value"),
        (&["eq", "preamp"], "required"),
        (&["surround", "2"], "outside the range"),
        (&["bass", "-0.1"], "outside the range"),
        (&["surround"], "on, off"),
        (
            &["surround", "--distance", "9.5"],
            "outside the range 10..300",
        ),
        (&["surround", "--distance", "300.5"], "outside"),
        (&["surround", "--distance", "10.2"], "multiple of 0.5"),
        (&["crystalizer", "1.5"], "outside the range"),
        (&["bass", "loud"], "expected on, off"),
        (&["dialog", "nan"], "expected on, off"),
        (&["smartvol"], "on, off"),
        (&["mic", "nr", "1.5"], "outside the range 0..1"),
        (&["mic", "nr", "-0.5"], "outside the range 0..1"),
        // Choices.
        (&["mic", "boost", "15"], "invalid value"),
        (&["filter", "sharp"], "invalid value"),
        (&["dolby", "loud"], "invalid value"),
        (&["smartvol", "--mode", "quiet"], "invalid value"),
        (&["sbx", "maybe"], "invalid value"),
        (&["scout", "1"], "invalid value"),
        (&["direct", "toggle"], "invalid value"),
        (&["light", "dim"], "unrecognized subcommand"),
        (&["light", "color"], "required"),
        (&["light", "color", "red"], "six hex digits"),
        (&["light", "color", "ff80"], "six hex digits"),
        (&["light", "color", "gg0000"], "six hex digits"),
        (&["light", "color", "ff8000ff"], "six hex digits"),
        (&["output", "tv"], "invalid value"),
        (&["output"], "required"),
        // Shape of the command line.
        (&["frobnicate"], "unrecognized subcommand"),
        (&["eq", "set"], "required"),
        (&["preset", "save"], "required"),
        (&["info", "--bogus"], "unexpected argument"),
        (&["watch", "--bogus"], "unexpected argument"),
        (&["serve", "--port", "70000"], "70000"),
        (&["serve", "--port", "http"], "http"),
        (&["serve", "--host", "0.0.0.0"], "unexpected argument"),
        (&["--connect-timeout", "0", "info"], "0"),
        (&["--connect-timeout", "61", "info"], "61"),
        // Preset names are checked before the device is opened.
        (&["preset", "save", "../evil"], "invalid preset name"),
        (&["preset", "save", ".hidden"], "invalid preset name"),
        (&["preset", "load", "a/b"], "invalid preset name"),
        (&["preset", "delete", "../evil"], "invalid preset name"),
    ];
    let home = Home::new();
    for (args, needle) in cases {
        let o = home.g6(args);
        let err = o.stderr();
        assert_eq!(o.code(), 2, "g6 {}: stderr: {err}", args.join(" "));
        assert!(
            err.contains(needle),
            "g6 {}: expected `{needle}` in stderr:\n{err}",
            args.join(" ")
        );
        assert!(
            o.stdout().is_empty(),
            "g6 {}: {}",
            args.join(" "),
            o.stdout()
        );
        assert!(
            !err.contains("G6 not found"),
            "g6 {} tried the device",
            args.join(" ")
        );
    }
}

#[test]
fn the_eq_preamp_is_not_an_eq_band() {
    // It has its own command: `g6 eq preamp <dB>`.
    let o = g6(&["eq", "set", "preamp=1"]);
    assert_eq!(o.code(), 2);
    assert!(o.stderr().contains("unknown EQ band"), "{}", o.stderr());
}

// ---------------------------------------------------------------------------
// Preset commands that only need files
// ---------------------------------------------------------------------------

#[test]
fn preset_list_load_and_delete_work_on_files_without_a_device() {
    if cfg!(windows) {
        return; // M1 is macOS; the directory layout differs on Windows.
    }
    let home = Home::new();
    let dir = home.presets_dir();

    // Empty: exit 0, hint on stderr, nothing on stdout.
    let o = home.g6(&["preset", "list"]);
    assert_eq!(o.code(), 0, "{}", o.stderr());
    assert_eq!(o.stdout(), "");
    assert!(o.stderr().contains("no presets"), "{}", o.stderr());

    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("movies.json"),
        r#"{"headphones":{"sbx":true,"eq":1,"eq_1k":-2.5}}"#,
    )
    .unwrap();
    std::fs::write(dir.join("music.json"), r#"{"speakers":{"bass":1}}"#).unwrap();
    // Not a preset.
    std::fs::write(dir.join("notes.txt"), "x").unwrap();

    let o = home.g6(&["preset", "list"]);
    assert_eq!(o.code(), 0);
    assert_eq!(o.stdout(), "movies\nmusic\n");

    // Delete: one goes, one stays.
    let o = home.g6(&["preset", "delete", "movies"]);
    assert_eq!(o.code(), 0, "{}", o.stderr());
    assert!(o.stdout().contains("deleted preset 'movies'"));
    assert!(!dir.join("movies.json").exists());
    assert_eq!(home.g6(&["preset", "list"]).stdout(), "music\n");

    // Missing preset: failure (1), found before the device is opened.
    for args in [["preset", "delete", "movies"], ["preset", "load", "movies"]] {
        let o = home.g6(&args);
        assert_eq!(o.code(), 1, "g6 {}", args.join(" "));
        assert!(o.stderr().contains("not found"), "{}", o.stderr());
        assert!(!o.stderr().contains("G6 not found"), "{}", o.stderr());
    }
}

#[test]
fn loading_an_invalid_or_broken_preset_fails_before_the_device() {
    if cfg!(windows) {
        return;
    }
    let home = Home::new();
    let dir = home.presets_dir();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("preamp.json"), r#"{"headphones":{"eq_preamp":7}}"#).unwrap();
    std::fs::write(dir.join("range.json"), r#"{"headphones":{"eq_31":99}}"#).unwrap();
    std::fs::write(dir.join("mic.json"), r#"{"headphones":{"mic_aec":1}}"#).unwrap();
    std::fs::write(dir.join("junk.json"), "not json").unwrap();
    for name in ["preamp", "range", "mic", "junk"] {
        let o = home.g6(&["preset", "load", name]);
        assert_eq!(o.code(), 1, "{name}: {}", o.stderr());
        assert!(
            !o.stderr().contains("G6 not found"),
            "{name}: {}",
            o.stderr()
        );
        assert!(o.stdout().is_empty(), "{name}");
    }
}
