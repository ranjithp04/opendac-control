//! Executes an [`Action`] against a [`Session`].

use std::fmt;
use std::io::{self, Write};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use g6_core::direct::{self, DirectMode};
use g6_core::events::{self, Change, Device, Event, Written};
use g6_core::preset::{self, PresetStore};
use g6_core::session::Session;
use g6_core::state::DeviceState;
use g6_core::store::Paths;

use crate::action::{Action, Op, PresetAction, UsageError};
use crate::format;

/// Why a command failed.
#[derive(Debug)]
pub enum CliError {
    /// Bad input (exit status 2).
    Usage(String),
    /// Device, file or protocol failure (exit status 1).
    Failure(String),
    /// Stdout was closed (e.g. `g6 watch | head`); exit quietly.
    BrokenPipe,
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CliError::Usage(m) | CliError::Failure(m) => f.write_str(m),
            CliError::BrokenPipe => f.write_str("output closed"),
        }
    }
}

impl From<UsageError> for CliError {
    fn from(e: UsageError) -> Self {
        CliError::Usage(e.0)
    }
}

fn fail(what: &str, e: impl fmt::Display) -> CliError {
    CliError::Failure(format!("{what}: {e}"))
}

fn dev<T>(what: &str, r: Result<T, events::Error>) -> Result<T, CliError> {
    r.map_err(|e| fail(what, e))
}

fn say(out: &mut dyn Write, line: impl fmt::Display) -> Result<(), CliError> {
    let r = writeln!(out, "{line}").and_then(|()| out.flush());
    match r {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Err(CliError::BrokenPipe),
        Err(e) => Err(fail("writing output", e)),
    }
}

/// Starts a session on demand.
pub type Starter = Box<dyn FnMut() -> (Session, Receiver<Event>)>;

/// Everything a command needs. The device is only opened by commands that
/// need it.
pub struct Env {
    start: Starter,
    paths: Option<Paths>,
    connect_timeout: Duration,
    /// Stop `watch` after this long (tests); `None` = run until killed.
    pub watch_limit: Option<Duration>,
    live: Option<(Session, Receiver<Event>)>,
}

impl Env {
    pub fn new(start: Starter, paths: Option<Paths>, connect_timeout: Duration) -> Env {
        Env {
            start,
            paths,
            connect_timeout,
            watch_limit: None,
            live: None,
        }
    }

    /// The session, started on first use. Fails when the G6 does not answer.
    fn session(&mut self) -> Result<&Session, CliError> {
        let (session, _) = self.live.get_or_insert_with(|| (self.start)());
        if !session.wait_connected(self.connect_timeout) {
            if session.device().is_connected() {
                return Err(CliError::Failure(format!(
                    "G6 is connected but its settings could not be read within {}s (run with -v for details)",
                    self.connect_timeout.as_secs()
                )));
            }
            return Err(CliError::Failure(format!(
                "G6 not found: no answer from USB device 041e:3256 (HID interface 4) within {}s; is it plugged in?",
                self.connect_timeout.as_secs()
            )));
        }
        Ok(session)
    }

    fn paths(&self) -> Result<&Paths, CliError> {
        self.paths.as_ref().ok_or_else(|| {
            CliError::Failure("cannot find a config directory (no home directory)".into())
        })
    }

    fn presets(&self) -> Result<PresetStore, CliError> {
        Ok(PresetStore::from_paths(self.paths()?))
    }

    fn direct_mode(&self) -> Result<DirectMode, CliError> {
        Ok(DirectMode::new(self.paths()?.direct_file()))
    }
}

/// Save the new state after a successful change. The change itself worked,
/// so a failure here is only a warning.
fn sync(session: &Session) {
    if let Err(e) = session.refresh() {
        eprintln!("g6: warning: could not re-read the device state: {e}");
    }
}

pub fn run(action: Action, env: &mut Env, out: &mut dyn Write) -> Result<(), CliError> {
    match action {
        Action::Info => info(env, out),
        Action::Status { json } => status(env, out, json),
        Action::Watch => watch(env, out),
        Action::Apply(ops) => apply(env, out, &ops),
        Action::Direct(on) => direct_mode(env, out, on),
        Action::Preset(p) => preset_cmd(env, out, p),
    }
}

fn info(env: &mut Env, out: &mut dyn Write) -> Result<(), CliError> {
    let d = env.session()?.device();
    let firmware = dev("reading firmware", d.firmware())?;
    say(
        out,
        format_args!("{:<14}{}", "firmware", firmware.trim_end_matches('\0')),
    )?;
    say(
        out,
        format_args!(
            "{:<14}{}",
            "hardware id",
            format::hex(&dev("reading hardware id", d.hw_id())?)
        ),
    )?;
    say(
        out,
        format_args!(
            "{:<14}{}",
            "identity",
            format::hex(&dev("reading identity", d.identity())?)
        ),
    )?;
    say(
        out,
        format_args!(
            "{:<14}{}",
            "dsp version",
            format::hex(&dev("reading DSP version", d.dsp_version())?)
        ),
    )
}

fn status(env: &mut Env, out: &mut dyn Write, json: bool) -> Result<(), CliError> {
    let state = dev("reading state", env.session()?.refresh())?;
    let direct = env
        .direct_mode()
        .ok()
        .and_then(|d| d.is_active().ok())
        .unwrap_or(false);
    if json {
        say(
            out,
            format::status_json(&state, direct).map_err(|e| fail("encoding JSON", e))?,
        )
    } else {
        write!(out, "{}", format::status_text(&state, direct))
            .and_then(|()| out.flush())
            .map_err(|e| match e.kind() {
                io::ErrorKind::BrokenPipe => CliError::BrokenPipe,
                _ => fail("writing output", e),
            })
    }
}

fn watch(env: &mut Env, out: &mut dyn Write) -> Result<(), CliError> {
    // Not through `session()`: watching also covers the time before the
    // device is plugged in; the session reconnects on its own.
    let (session, events) = (env.start)();
    eprintln!("g6: watching device events (Ctrl-C to stop)");
    let begin = Instant::now();
    let deadline = env.watch_limit.map(|d| begin + d);
    // What was last shown: only changes are printed (the device repeats
    // whole dumps), and the EQ pre-amp is never shown.
    let mut shown = DeviceState::new();
    loop {
        let wait = deadline.map_or(Duration::from_secs(3600), |d| {
            d.saturating_duration_since(Instant::now())
        });
        match events.recv_timeout(wait) {
            Ok(Event::Changed(Change::Feature { feature, .. })) if !feature.exposed => {}
            Ok(e) if e != Event::Connected && !shown.apply(&e) => {}
            Ok(e) => {
                if e == Event::Connected {
                    shown = session.state();
                }
                say(
                    out,
                    format_args!(
                        "[{:>7.1}s] {}",
                        begin.elapsed().as_secs_f32(),
                        format::event_line(&e)
                    ),
                )?
            }
            Err(RecvTimeoutError::Timeout) if deadline.is_none() => {}
            Err(_) => break,
        }
    }
    drop(session);
    Ok(())
}

fn exec_op(d: &Device, op: &Op) -> Result<String, CliError> {
    match op {
        Op::Feature(f, v) => {
            let w = dev(f.name, d.write_feature(f, *v))?;
            Ok(format!(
                "{}: {}",
                f.name,
                format::feature_display(f, w.value)
            ))
        }
        Op::Mask(target, on) => {
            let Written { value, .. } = dev("mask", d.set_mask(*target, *on))?;
            Ok(match target {
                g6_core::protocol::MaskTarget::Sbx => {
                    format!("sbx: {}", format::on_off(value.sbx()))
                }
                g6_core::protocol::MaskTarget::Scout => {
                    format!("scout: {}", format::on_off(value.scout()))
                }
            })
        }
        Op::DacFilter(f) => {
            let w = dev("dac filter", d.set_dac_filter(*f))?;
            Ok(format!("filter: {}", w.value.name()))
        }
        Op::MicBoost(b) => {
            let w = dev("mic boost", d.set_mic_boost(*b))?;
            Ok(format!("mic boost: {}", format::boost_display(w.value)))
        }
    }
}

fn apply(env: &mut Env, out: &mut dyn Write, ops: &[Op]) -> Result<(), CliError> {
    let session = env.session()?;
    for op in ops {
        let line = exec_op(session.device(), op)?;
        say(out, line)?;
    }
    sync(session);
    Ok(())
}

fn direct_mode(env: &mut Env, out: &mut dyn Write, on: bool) -> Result<(), CliError> {
    let direct = env.direct_mode()?;
    let session = env.session()?;
    let r: Result<(), direct::Error> = if on {
        direct.enable(session.device())
    } else {
        direct.disable(session.device())
    };
    r.map_err(|e| fail(if on { "direct on" } else { "direct off" }, e))?;
    sync(session);
    say(out, format_args!("direct mode: {}", format::on_off(on)))
}

fn preset_cmd(env: &mut Env, out: &mut dyn Write, p: PresetAction) -> Result<(), CliError> {
    let store = env.presets()?;
    match p {
        PresetAction::List => {
            let names = store.list().map_err(|e| fail("listing presets", e))?;
            if names.is_empty() {
                eprintln!("g6: no presets saved");
            }
            for n in names {
                say(out, n)?;
            }
            Ok(())
        }
        PresetAction::Delete(name) => {
            store
                .delete(&name)
                .map_err(|e| fail("deleting preset", e))?;
            say(out, format_args!("deleted preset '{name}'"))
        }
        PresetAction::Save(name) => {
            // Reject a bad name before touching the device.
            if let Err(e @ preset::Error::BadName(_)) = store.load(&name) {
                return Err(CliError::Usage(e.to_string()));
            }
            let state = dev("reading state", env.session()?.refresh())?;
            store
                .save_current(&name, &state)
                .map_err(|e| fail("saving preset", e))?;
            say(out, format_args!("saved preset '{name}'"))
        }
        PresetAction::Load(name) => {
            let preset = match store.load(&name) {
                Err(e @ preset::Error::BadName(_)) => return Err(CliError::Usage(e.to_string())),
                other => other.map_err(|e| fail("loading preset", e))?,
            };
            let session = env.session()?;
            let applied = preset::apply(session.device(), &preset)
                .map_err(|e| fail(&format!("applying preset '{name}'"), e))?;
            sync(session);
            say(
                out,
                format_args!(
                    "applied preset '{name}' to {} ({} writes)",
                    g6_core::state::output_name(applied.output),
                    applied.writes
                ),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action;
    use crate::cli::Cli;
    use clap::Parser;
    use g6_core::events::Config;
    use g6_core::features::by_name;
    use g6_core::protocol::{DacFilter, Mask, MicBoost, Output};
    use g6_core::session::SessionConfig;
    use g6_core::transport::fake::FakeDevice;
    use g6_core::transport::Backoff;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static N: AtomicU32 = AtomicU32::new(0);
            let p = std::env::temp_dir().join(format!(
                "g6-cli-test-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            std::fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn fast() -> Config {
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

    struct Rig {
        fake: FakeDevice,
        paths: Paths,
        _tmp: TempDir,
    }

    impl Rig {
        fn new() -> Rig {
            let tmp = TempDir::new();
            Rig {
                fake: FakeDevice::new(),
                paths: Paths::under(&tmp.0),
                _tmp: tmp,
            }
        }

        fn env(&self) -> Env {
            let fake = self.fake.clone();
            let paths = self.paths.clone();
            let session_paths = paths.clone();
            Env::new(
                Box::new(move || {
                    g6_core::session::Session::start(
                        Box::new(fake.connector()),
                        fast(),
                        SessionConfig {
                            paths: Some(session_paths.clone()),
                            reapply_on_first_connect: false,
                        },
                    )
                }),
                Some(paths),
                Duration::from_secs(2),
            )
        }

        /// Run one command line in a fresh process-like env.
        fn exec(&self, args: &[&str]) -> Result<String, CliError> {
            self.exec_env(&mut self.env(), args)
        }

        fn exec_env(&self, env: &mut Env, args: &[&str]) -> Result<String, CliError> {
            let mut argv = vec!["g6"];
            argv.extend_from_slice(args);
            let cli = Cli::try_parse_from(argv).expect("clap parse");
            let action = action::build(cli.command)?;
            let mut out = Vec::new();
            run(action, env, &mut out)?;
            Ok(String::from_utf8(out).unwrap())
        }
    }

    fn raw(fake: &FakeDevice, name: &str) -> f32 {
        let f = by_name(name).unwrap();
        fake.feature(f.family, f.id).unwrap()
    }

    #[test]
    fn info_prints_firmware() {
        let rig = Rig::new();
        let out = rig.exec(&["info"]).unwrap();
        assert!(out.contains("firmware"), "{out}");
        assert!(out.contains("2.1.250903.1324"), "{out}");
        assert!(out.contains("hardware id"), "{out}");
    }

    #[test]
    fn eq_commands_write_device() {
        let rig = Rig::new();
        let out = rig
            .exec(&["eq", "set", "31=3", "1k=-1", "16k=0.5"])
            .unwrap();
        assert_eq!(out, "eq_31: 3 dB\neq_1k: -1 dB\neq_16k: 0.5 dB\n");
        assert_eq!(raw(&rig.fake, "eq_31"), 3.0);
        assert_eq!(raw(&rig.fake, "eq_1k"), -1.0);
        assert_eq!(rig.exec(&["eq", "on"]).unwrap(), "eq: on\n");
        assert_eq!(raw(&rig.fake, "eq"), 1.0);
        assert_eq!(rig.exec(&["eq", "off"]).unwrap(), "eq: off\n");
        assert_eq!(raw(&rig.fake, "eq"), 0.0);
    }

    #[test]
    fn effects_mask_filter_mic_and_decoder() {
        let rig = Rig::new();
        rig.exec(&["surround", "0.75", "--distance", "120"])
            .unwrap();
        assert_eq!(raw(&rig.fake, "surround_level"), 0.75);
        assert_eq!(raw(&rig.fake, "surround"), 1.0);
        assert_eq!(raw(&rig.fake, "surround_distance"), 120.0);
        rig.exec(&["smartvol", "--mode", "night"]).unwrap();
        assert_eq!(raw(&rig.fake, "smartvol_mode"), 2.0);
        rig.exec(&["dolby", "full"]).unwrap();
        assert_eq!(raw(&rig.fake, "decoder_mode"), 1.0);
        rig.exec(&["mic", "nr", "1"]).unwrap();
        assert_eq!(raw(&rig.fake, "mic_nr_level"), 0.5);
        assert_eq!(rig.exec(&["sbx", "on"]).unwrap(), "sbx: on\n");
        assert!(Mask(rig.fake.mask()).sbx());
        assert_eq!(rig.exec(&["scout", "on"]).unwrap(), "scout: on\n");
        assert!(Mask(rig.fake.mask()).scout());
        assert_eq!(rig.exec(&["filter", "nos"]).unwrap(), "filter: nos\n");
        assert_eq!(rig.fake.dac_filter(), DacFilter::Nos);
        assert_eq!(
            rig.exec(&["mic", "boost", "10"]).unwrap(),
            "mic boost: 10 dB\n"
        );
        assert_eq!(rig.fake.mic_boost(), MicBoost::Db10);
    }

    #[test]
    fn status_text_and_json() {
        let rig = Rig::new();
        rig.exec(&["bass", "0.3"]).unwrap();
        let text = rig.exec(&["status"]).unwrap();
        assert!(text.contains("[headphones] (active)"), "{text}");
        assert!(text.contains("bass_level"), "{text}");
        let json = rig.exec(&["status", "--json"]).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["output"], "headphones");
        assert_eq!(v["connected"], true);
        assert_eq!(v["direct_mode"], false);
        assert_eq!(v["dac_filter"], "fast-min");
        assert_eq!(v["mic_boost"], 30);
        assert_eq!(v["headphones"]["bass_level"], 0.3);
        assert!(v["headphones"].get("eq_preamp").is_none());
    }

    #[test]
    fn direct_mode_round_trip() {
        let rig = Rig::new();
        rig.exec(&["sbx", "on"]).unwrap();
        rig.exec(&["eq", "on"]).unwrap();
        rig.exec(&["crystalizer", "on"]).unwrap();
        assert_eq!(rig.exec(&["direct", "on"]).unwrap(), "direct mode: on\n");
        assert!(!Mask(rig.fake.mask()).sbx());
        assert_eq!(raw(&rig.fake, "eq"), 0.0);
        let v: serde_json::Value =
            serde_json::from_str(&rig.exec(&["status", "--json"]).unwrap()).unwrap();
        assert_eq!(v["direct_mode"], true);
        assert_eq!(rig.exec(&["direct", "off"]).unwrap(), "direct mode: off\n");
        assert!(Mask(rig.fake.mask()).sbx());
        assert_eq!(raw(&rig.fake, "eq"), 1.0);
        assert_eq!(raw(&rig.fake, "crystalizer"), 1.0);
        // Off again without being on is an error.
        let e = rig.exec(&["direct", "off"]).unwrap_err();
        assert!(matches!(e, CliError::Failure(m) if m.contains("not on")));
    }

    #[test]
    fn preset_save_list_load_delete() {
        let rig = Rig::new();
        rig.exec(&["eq", "set", "62=2"]).unwrap();
        rig.exec(&["surround", "0.4"]).unwrap();
        assert_eq!(
            rig.exec(&["preset", "save", "movies"]).unwrap(),
            "saved preset 'movies'\n"
        );
        assert_eq!(rig.exec(&["preset", "list"]).unwrap(), "movies\n");
        rig.exec(&["eq", "set", "62=-4"]).unwrap();
        rig.exec(&["surround", "off"]).unwrap();
        let out = rig.exec(&["preset", "load", "movies"]).unwrap();
        assert!(
            out.starts_with("applied preset 'movies' to headphones"),
            "{out}"
        );
        assert_eq!(raw(&rig.fake, "eq_62"), 2.0);
        assert_eq!(raw(&rig.fake, "surround"), 1.0);
        assert_eq!(raw(&rig.fake, "surround_level"), 0.4);
        assert_eq!(
            rig.exec(&["preset", "delete", "movies"]).unwrap(),
            "deleted preset 'movies'\n"
        );
        assert_eq!(rig.exec(&["preset", "list"]).unwrap(), "");
        let e = rig.exec(&["preset", "load", "movies"]).unwrap_err();
        assert!(matches!(e, CliError::Failure(m) if m.contains("not found")));
        let e = rig.exec(&["preset", "delete", "movies"]).unwrap_err();
        assert!(matches!(e, CliError::Failure(m) if m.contains("not found")));
    }

    #[test]
    fn bad_preset_name_is_a_usage_error_without_a_device() {
        let rig = Rig::new();
        rig.fake.unplug();
        for args in [["preset", "save", "../x"], ["preset", "load", "../x"]] {
            let e = rig.exec(&args).unwrap_err();
            assert!(matches!(e, CliError::Usage(m) if m.contains("invalid preset name")));
        }
    }

    #[test]
    fn missing_device_fails_with_clear_message() {
        let rig = Rig::new();
        rig.fake.unplug();
        let mut env = rig.env();
        env.connect_timeout = Duration::from_millis(300);
        let e = rig.exec_env(&mut env, &["eq", "on"]).unwrap_err();
        assert!(
            matches!(&e, CliError::Failure(m) if m.contains("G6 not found")),
            "{e}"
        );
        // Commands that need no device still work.
        assert_eq!(rig.exec_env(&mut env, &["preset", "list"]).unwrap(), "");
    }

    #[test]
    fn rejected_write_is_a_failure() {
        let rig = Rig::new();
        rig.fake.reject_next_write(0x81);
        let e = rig.exec(&["bass", "on"]).unwrap_err();
        assert!(
            matches!(&e, CliError::Failure(m) if m.contains("bass") && m.contains("rejected")),
            "{e}"
        );
    }

    #[test]
    fn unconfirmed_write_is_a_failure() {
        let rig = Rig::new();
        rig.fake.set_ignore_writes(true);
        let e = rig.exec(&["bass", "on"]).unwrap_err();
        assert!(
            matches!(&e, CliError::Failure(m) if m.contains("not applied")),
            "{e}"
        );
    }

    #[test]
    fn watch_streams_events() {
        let rig = Rig::new();
        let mut env = rig.env();
        env.watch_limit = Some(Duration::from_millis(3000));
        let fake = rig.fake.clone();
        let t = std::thread::spawn(move || {
            // After the connect sync (~0.8 s with the fake): earlier events are
            // part of the synced state and not printed again.
            std::thread::sleep(Duration::from_millis(1500));
            fake.press_mask(0x01);
            fake.switch_output(Output::Speakers);
        });
        let out = rig.exec_env(&mut env, &["watch"]).unwrap();
        t.join().unwrap();
        assert!(out.contains("connected"), "{out}");
        assert!(out.contains("mask: sbx on"), "{out}");
        assert!(out.contains("output: speakers"), "{out}");
    }

    #[test]
    fn watch_prints_only_changes_and_hides_the_preamp() {
        let rig = Rig::new();
        let mut env = rig.env();
        env.watch_limit = Some(Duration::from_millis(3000));
        let fake = rig.fake.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1500));
            fake.switch_output(Output::Speakers);
            fake.switch_output(Output::Speakers);
        });
        let out = rig.exec_env(&mut env, &["watch"]).unwrap();
        t.join().unwrap();
        assert_eq!(out.matches("output: speakers").count(), 1, "{out}");
        assert!(out.matches("eq_31:").count() <= 1, "{out}");
        assert!(!out.contains("eq_preamp"), "{out}");
    }
}
