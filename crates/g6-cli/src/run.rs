//! Executes an [`Action`] against a [`Session`].

use std::fmt;
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use g6_core::direct::{self, DirectMode};
use g6_core::eq_import;
use g6_core::events::{self, Device, Event, Written};
use g6_core::features::{self, EQ_BANDS};
use g6_core::preset::{self, Preset, PresetStore};
use g6_core::protocol::rgb_to_hex;
use g6_core::session::Session;
use g6_core::state::DeviceState;
use g6_core::store::Paths;

use crate::action::{self, Action, Op, PresetAction, UsageError};
use crate::format;
use crate::serve;

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

pub(crate) fn say(out: &mut dyn Write, line: impl fmt::Display) -> Result<(), CliError> {
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

    /// A fresh session that is not waited for: `watch` and `serve` also run
    /// before the G6 is plugged in; the session reconnects on its own.
    pub(crate) fn start_session(&mut self) -> (Session, Receiver<Event>) {
        (self.start)()
    }

    pub(crate) fn paths(&self) -> Result<&Paths, CliError> {
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
pub(crate) fn sync(session: &Session) {
    if let Err(e) = session.refresh() {
        eprintln!("g6: warning: could not re-read the device state: {e}");
    }
}

pub fn run(action: Action, env: &mut Env, out: &mut dyn Write) -> Result<(), CliError> {
    match action {
        Action::Info => info(env, out),
        Action::Status { json } => status(env, out, json),
        Action::Watch { json } => watch(env, out, json),
        Action::Serve { port } => serve::run(env, out, port),
        Action::Apply(ops) => apply(env, out, &ops),
        Action::Direct(on) => direct_mode(env, out, on),
        Action::Preset(p) => preset_cmd(env, out, p),
        Action::EqImport(file) => eq_import_cmd(env, out, &file),
    }
}

fn info(env: &mut Env, out: &mut dyn Write) -> Result<(), CliError> {
    let d = env.session()?.device();
    let firmware = dev("reading firmware", d.firmware())?;
    say(
        out,
        format_args!("{:<14}{}", "firmware", firmware.trim_end_matches('\0')),
    )?;
    // Not every firmware answers the serial query: the rest of `info` is still shown.
    let serial = d.serial().map_or_else(
        |e| {
            eprintln!("g6: warning: could not read the serial number: {e}");
            "unavailable".to_string()
        },
        |b| format::hex(&b),
    );
    say(out, format_args!("{:<14}{serial}", "serial"))?;
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

/// Filters out events that repeat what was already shown (the device
/// repeats whole dumps).
pub(crate) struct Novelty {
    shown: DeviceState,
}

impl Novelty {
    pub(crate) fn new() -> Novelty {
        Novelty {
            shown: DeviceState::new(),
        }
    }

    /// Whether `e` is worth showing.
    pub(crate) fn check(&mut self, e: &Event, session: &Session) -> bool {
        if *e == Event::Connected {
            self.shown = session.state();
            return true;
        }
        self.shown.apply(e)
    }
}

fn watch(env: &mut Env, out: &mut dyn Write, json: bool) -> Result<(), CliError> {
    // Not through `session()`: watching also covers the time before the
    // device is plugged in; the session reconnects on its own.
    let (session, events) = env.start_session();
    eprintln!("g6: watching device events (Ctrl-C to stop)");
    let begin = Instant::now();
    let deadline = env.watch_limit.map(|d| begin + d);
    let mut novelty = Novelty::new();
    loop {
        let wait = deadline.map_or(Duration::from_secs(3600), |d| {
            d.saturating_duration_since(Instant::now())
        });
        match events.recv_timeout(wait) {
            Ok(e) if !novelty.check(&e, &session) => {}
            Ok(e) if json => say(out, format::event_json(&e, format::unix_now()))?,
            Ok(e) => say(
                out,
                format_args!(
                    "[{:>7.1}s] {}",
                    begin.elapsed().as_secs_f32(),
                    format::event_line(&e)
                ),
            )?,
            Err(RecvTimeoutError::Timeout) if deadline.is_none() => {}
            Err(_) => break,
        }
    }
    drop(session);
    Ok(())
}

pub(crate) fn exec_op(d: &Device, op: &Op) -> Result<String, CliError> {
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
        Op::LightEnabled(on) => {
            let w = dev("light", d.set_lighting_enabled(*on))?;
            Ok(format!("light: {}", format::on_off(w.value)))
        }
        Op::LightColor(rgb) => {
            let w = dev("light color", d.set_lighting_color(*rgb))?;
            Ok(format!("light color: {}", rgb_to_hex(w.value)))
        }
        Op::Output(o) => {
            let w = dev("output", d.set_output(*o))?;
            Ok(format!("output: {}", g6_core::state::output_name(w.value)))
        }
    }
}

/// Playback settings (EQ, SBX, Scout, effects, presets) are read-only while
/// Direct Mode is on, as in the app and `g6 serve`. Without a config
/// directory there is no Direct Mode file, so it is off.
fn check_direct_mode_off(env: &Env) -> Result<(), CliError> {
    let Ok(paths) = env.paths() else {
        return Ok(());
    };
    match DirectMode::new(paths.direct_file()).is_active() {
        Ok(false) => Ok(()),
        Ok(true) => Err(CliError::Failure(
            "Direct Mode is on: EQ, SBX and Scout are read-only (run `g6 direct off` first)".into(),
        )),
        Err(e) => Err(fail("reading the Direct Mode state", e)),
    }
}

/// While Scout Mode is on, the EQ bands, the EQ on / off switch and Smart
/// Volume are Scout's (the pre-amp is not locked), as in the app and
/// `g6 serve`. Needs the live mask, so it runs once the device is open.
fn check_scout_off(device: &Device, ops: &[Op]) -> Result<(), CliError> {
    let scout_owned = ops
        .iter()
        .any(|op| matches!(op, Op::Feature(f, _) if features::scout_owned(f)));
    if scout_owned && dev("reading mask", device.read_mask())?.scout() {
        return Err(CliError::Failure(
            "Scout Mode is on: EQ and Smart Volume are set by the device".into(),
        ));
    }
    Ok(())
}

fn apply(env: &mut Env, out: &mut dyn Write, ops: &[Op]) -> Result<(), CliError> {
    let playback = ops.iter().any(|op| match op {
        Op::Feature(f, _) => f.family == features::FAMILY_PLAYBACK,
        Op::Mask(..) => true,
        _ => false,
    });
    if playback {
        check_direct_mode_off(env)?;
    }
    let session = env.session()?;
    check_scout_off(session.device(), ops)?;
    // A closed stdout (`| head -1`) must not leave the change half applied:
    // every op is written, then the first output error is returned.
    let mut printed = Ok(());
    for op in ops {
        let line = exec_op(session.device(), op)?;
        if printed.is_ok() {
            printed = say(out, line);
        }
    }
    sync(session);
    printed
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

/// A whole text file of at most `limit` bytes (UTF-8; a leading byte order
/// mark is dropped). Only regular files: a FIFO or `/dev/zero` would block
/// or never end.
fn read_text(file: &Path, limit: usize) -> Result<String, CliError> {
    let reading = |e: io::Error| fail(&format!("reading {}", file.display()), e);
    if !std::fs::metadata(file).map_err(reading)?.is_file() {
        return Err(CliError::Usage(format!(
            "{}: not a regular file",
            file.display()
        )));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(file)
        .and_then(|f| f.take(limit as u64 + 1).read_to_end(&mut bytes))
        .map_err(reading)?;
    if bytes.len() > limit {
        return Err(CliError::Usage(format!(
            "{}: larger than {} KiB",
            file.display(),
            limit / 1024
        )));
    }
    let text = String::from_utf8(bytes)
        .map_err(|e| reading(io::Error::new(io::ErrorKind::InvalidData, e)))?;
    Ok(match text.strip_prefix('\u{feff}') {
        Some(rest) => rest.to_string(),
        None => text,
    })
}

/// `eq import FILE`: the file is parsed and every value checked before the
/// device is opened. Like `eq set`, it leaves the EQ on / off switch alone.
fn eq_import_cmd(env: &mut Env, out: &mut dyn Write, file: &Path) -> Result<(), CliError> {
    let text = read_text(file, eq_import::MAX_INPUT_BYTES)?;
    let eq = eq_import::parse_eq_text(&text)
        .map_err(|e| CliError::Usage(format!("{}: {e}", file.display())))?;
    for note in &eq.notes {
        eprintln!("g6: {note}");
    }
    let mut ops = Vec::new();
    for (i, gain) in eq.bands.iter().enumerate() {
        let name = features::eq_band(i).map_or("eq", |f| f.name);
        ops.push(action::ui_op(name, &format!("EQ {}", EQ_BANDS[i]), *gain)?);
    }
    ops.push(action::ui_op("eq_preamp", "EQ pre-amp", eq.preamp)?);
    apply(env, out, &ops)
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
            match store.delete(&name) {
                Err(e @ preset::Error::BadName(_)) => return Err(CliError::Usage(e.to_string())),
                other => other.map_err(|e| fail("deleting preset", e))?,
            }
            say(out, format_args!("deleted preset '{name}'"))
        }
        PresetAction::Export { name, file } => {
            let preset = match store.load(&name) {
                Err(e @ preset::Error::BadName(_)) => return Err(CliError::Usage(e.to_string())),
                other => other.map_err(|e| fail("loading preset", e))?,
            };
            let json = preset.to_json().map_err(|e| fail("exporting preset", e))?;
            g6_core::store::write_atomic(&file, json.as_bytes()).map_err(|e| fail("writing", e))?;
            say(
                out,
                format_args!("exported preset '{name}' to {}", file.display()),
            )
        }
        PresetAction::Import { file, name } => {
            let text = read_text(&file, preset::MAX_FILE_BYTES)?;
            let preset = Preset::from_json(&text)
                .map_err(|e| CliError::Usage(format!("{}: {e}", file.display())))?;
            let explicit = name.is_some();
            let name = name.unwrap_or_else(|| {
                file.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_string()
            });
            let hint = if explicit {
                ""
            } else {
                " (taken from the file name; pass --name to choose another)"
            };
            let taken = store
                .list()
                .map_err(|e| fail("listing presets", e))?
                .iter()
                .any(|n| n.eq_ignore_ascii_case(&name));
            if taken {
                return Err(CliError::Usage(format!(
                    "preset '{name}' already exists{hint}; delete it first or use --name"
                )));
            }
            match store.save(&name, &preset) {
                Err(e @ preset::Error::BadName(_)) => {
                    return Err(CliError::Usage(format!("{e}{hint}")))
                }
                other => other.map_err(|e| fail("saving preset", e))?,
            }
            say(out, format_args!("imported preset '{name}'"))
        }
        PresetAction::Save(name) => {
            // Reject a bad name before touching the device.
            if let Err(e @ preset::Error::BadName(_)) = store.load(&name) {
                return Err(CliError::Usage(e.to_string()));
            }
            // Direct Mode's off-state is not the user's: never save it.
            check_direct_mode_off(env)?;
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
            check_direct_mode_off(env)?;
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
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};

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
        assert!(
            out.contains("serial        ef 67 74 00 00 00 00 00"),
            "{out}"
        );
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
        assert_eq!(
            rig.exec(&["eq", "preamp", "-2.5"]).unwrap(),
            "eq_preamp: -2.5 dB\n"
        );
        assert_eq!(raw(&rig.fake, "eq_preamp"), -2.5);
        assert!(rig.exec(&["eq", "preamp", "6.5"]).is_err());
        assert_eq!(raw(&rig.fake, "eq_preamp"), -2.5);
        let text = rig.exec(&["status"]).unwrap();
        assert!(text.contains("eq pre-amp        -2.5 dB"), "{text}");
        let json = rig.exec(&["status", "--json"]).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["headphones"]["eq_preamp"], -2.5);
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
        assert_eq!(v["headphones"]["eq_preamp"], 0.0);
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
    fn light_commands_write_the_device_and_show_in_status() {
        let rig = Rig::new();
        assert_eq!(rig.exec(&["light", "off"]).unwrap(), "light: off\n");
        assert!(!rig.fake.lighting_enabled());
        // A color does not switch the LED on.
        assert_eq!(
            rig.exec(&["light", "color", "#00FF00"]).unwrap(),
            "light color: 00ff00\n"
        );
        assert_eq!(rig.fake.lighting_color(), [0, 255, 0]);
        assert!(!rig.fake.lighting_enabled());
        let text = rig.exec(&["status"]).unwrap();
        assert!(text.contains("light               off"), "{text}");
        assert!(text.contains("light color         00ff00"), "{text}");
        let v: serde_json::Value =
            serde_json::from_str(&rig.exec(&["status", "--json"]).unwrap()).unwrap();
        assert_eq!(v["lighting_enabled"], false);
        assert_eq!(v["lighting_color"], "00ff00");
        assert_eq!(rig.exec(&["light", "on"]).unwrap(), "light: on\n");
        assert!(rig.fake.lighting_enabled());
    }

    #[test]
    fn output_command_switches_the_active_output() {
        let rig = Rig::new();
        assert_eq!(
            rig.exec(&["output", "speakers"]).unwrap(),
            "output: speakers\n"
        );
        assert_eq!(rig.fake.output(), Output::Speakers);
        let json = rig.exec(&["status", "--json"]).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["output"], "speakers");
        assert_eq!(
            rig.exec(&["output", "headphones"]).unwrap(),
            "output: headphones\n"
        );
        assert_eq!(rig.fake.output(), Output::Headphones);
    }

    #[test]
    fn direct_mode_also_switches_the_hardware_direct_mode() {
        let rig = Rig::new();
        rig.exec(&["direct", "on"]).unwrap();
        assert!(rig.fake.hw_direct());
        rig.exec(&["direct", "off"]).unwrap();
        assert!(!rig.fake.hw_direct());
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
    fn preset_export_and_import_round_trip_through_a_file() {
        let rig = Rig::new();
        rig.exec(&["eq", "set", "62=2"]).unwrap();
        rig.exec(&["preset", "save", "movies"]).unwrap();
        let file = rig._tmp.0.join("movies.json");
        let file_arg = file.to_str().unwrap();
        let out = rig.exec(&["preset", "export", "movies", file_arg]).unwrap();
        assert!(out.starts_with("exported preset 'movies' to "), "{out}");
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(json["headphones"]["eq_62"], 2.0);

        // The name defaults to the file stem, but never replaces a preset.
        let e = rig.exec(&["preset", "import", file_arg]).unwrap_err();
        assert!(
            matches!(&e, CliError::Usage(m) if m.contains("already exists") && m.contains("file name")),
            "{e}"
        );
        assert_eq!(
            rig.exec(&["preset", "import", file_arg, "--name", "copy"])
                .unwrap(),
            "imported preset 'copy'\n"
        );
        rig.exec(&["preset", "delete", "movies"]).unwrap();
        assert_eq!(
            rig.exec(&["preset", "import", file_arg]).unwrap(),
            "imported preset 'movies'\n"
        );
        assert_eq!(rig.exec(&["preset", "list"]).unwrap(), "copy\nmovies\n");
        rig.exec(&["eq", "set", "62=-4"]).unwrap();
        rig.exec(&["preset", "load", "copy"]).unwrap();
        assert_eq!(raw(&rig.fake, "eq_62"), 2.0);
    }

    #[test]
    fn preset_import_refuses_bad_files_and_names_without_a_device() {
        let rig = Rig::new();
        rig.fake.unplug();
        let dir = &rig._tmp.0;
        let write = |name: &str, text: &str| {
            let p = dir.join(name);
            std::fs::write(&p, text).unwrap();
            p.to_str().unwrap().to_string()
        };
        // Out-of-range value, a mic feature, not JSON.
        for (n, text, what) in [
            ("range.json", r#"{"headphones":{"eq_31":99}}"#, "eq_31"),
            ("mic.json", r#"{"headphones":{"mic_aec":1}}"#, "mic_aec"),
            ("junk.json", "hello", "not a profile file"),
        ] {
            let f = write(n, text);
            let e = rig.exec(&["preset", "import", &f]).unwrap_err();
            assert!(matches!(&e, CliError::Usage(m) if m.contains(what)), "{e}");
        }
        assert_eq!(rig.exec(&["preset", "list"]).unwrap(), "", "nothing saved");
        // A file name that is not a valid preset name needs --name.
        let f = write("caf\u{e9}.json", r#"{"headphones":{"bass":1}}"#);
        let e = rig.exec(&["preset", "import", &f]).unwrap_err();
        assert!(
            matches!(&e, CliError::Usage(m) if m.contains("pass --name")),
            "{e}"
        );
        rig.exec(&["preset", "import", &f, "--name", "cafe"])
            .unwrap();
        // Missing input file and unknown preset are file failures / not found.
        let e = rig
            .exec(&["preset", "import", "/no/such/file.json"])
            .unwrap_err();
        assert!(
            matches!(&e, CliError::Failure(m) if m.contains("reading")),
            "{e}"
        );
        let out = dir.join("x.json");
        let e = rig
            .exec(&["preset", "export", "nope", out.to_str().unwrap()])
            .unwrap_err();
        assert!(
            matches!(&e, CliError::Failure(m) if m.contains("not found")),
            "{e}"
        );
        assert!(!out.exists());
    }

    #[test]
    fn eq_import_writes_bands_and_preamp_after_parsing() {
        let rig = Rig::new();
        let file = rig._tmp.0.join("autoeq.txt");
        std::fs::write(
            &file,
            "Preamp: -3.0 dB\nFilter 1: ON PK Fc 1000 Hz Gain -3.0 dB Q 1.0\nFilter 2: ON PK Fc 16000 Hz Gain 15 dB Q 2.0\n",
        )
        .unwrap();
        rig.exec(&["eq", "import", file.to_str().unwrap()]).unwrap();
        assert_eq!(raw(&rig.fake, "eq_1k"), -3.0);
        assert_eq!(raw(&rig.fake, "eq_16k"), 12.0, "clamped");
        assert_eq!(raw(&rig.fake, "eq_preamp"), -3.0);
        assert_eq!(raw(&rig.fake, "eq"), 0.0, "does not switch the EQ on");

        // Garbage never reaches the device.
        rig.fake.unplug();
        let bad = rig._tmp.0.join("bad.txt");
        std::fs::write(&bad, "nothing here").unwrap();
        let e = rig
            .exec(&["eq", "import", bad.to_str().unwrap()])
            .unwrap_err();
        assert!(
            matches!(&e, CliError::Usage(m) if m.contains("no EQ found")),
            "{e}"
        );
        let e = rig
            .exec(&["eq", "import", "/no/such/file.txt"])
            .unwrap_err();
        assert!(
            matches!(&e, CliError::Failure(m) if m.contains("reading")),
            "{e}"
        );
    }

    #[test]
    fn bad_preset_name_is_a_usage_error_without_a_device() {
        let rig = Rig::new();
        rig.fake.unplug();
        for args in [
            ["preset", "save", "../x"],
            ["preset", "load", "../x"],
            ["preset", "delete", "../x"],
        ] {
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

    /// Takes `lines` lines, then reports a closed pipe (`| head -1`).
    struct ClosesAfter {
        lines: usize,
        got: Vec<u8>,
    }

    impl Write for ClosesAfter {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            if self.got.iter().filter(|&&c| c == b'\n').count() >= self.lines {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            self.got.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_closed_stdout_does_not_cut_a_change_short() {
        let rig = Rig::new();
        let file = rig._tmp.0.join("autoeq.txt");
        std::fs::write(
            &file,
            "Preamp: -3.0 dB\nFilter 1: ON PK Fc 31 Hz Gain 2.0 dB Q 1.0\nFilter 2: ON PK Fc 16000 Hz Gain -4 dB Q 2.0\n",
        )
        .unwrap();
        // The last write of each: the band after two more, the pre-amp
        // after all ten bands.
        for (args, last, value) in [
            (vec!["eq", "set", "31=3", "1k=-1", "16k=0.5"], "eq_16k", 0.5),
            (
                vec!["eq", "import", file.to_str().unwrap()],
                "eq_preamp",
                -3.0,
            ),
        ] {
            let mut argv = vec!["g6"];
            argv.extend_from_slice(&args);
            let action = action::build(Cli::try_parse_from(argv).unwrap().command).unwrap();
            let mut out = ClosesAfter {
                lines: 1,
                got: Vec::new(),
            };
            let e = run(action, &mut rig.env(), &mut out).unwrap_err();
            assert!(matches!(e, CliError::BrokenPipe), "{e}");
            assert_eq!(out.got.iter().filter(|&&c| c == b'\n').count(), 1);
            assert_eq!(raw(&rig.fake, last), value, "{args:?}");
        }
    }

    #[test]
    fn playback_commands_are_refused_while_direct_mode_is_on() {
        let rig = Rig::new();
        rig.exec(&["eq", "set", "62=2"]).unwrap();
        rig.exec(&["preset", "save", "movies"]).unwrap();
        let file = rig._tmp.0.join("autoeq.txt");
        std::fs::write(&file, "Preamp: -3.0 dB\n").unwrap();
        rig.exec(&["direct", "on"]).unwrap();
        let writes = rig.fake.written().len();
        for args in [
            &["eq", "on"][..],
            &["eq", "set", "1k=2"],
            &["eq", "preamp", "-1"],
            &["sbx", "on"],
            &["scout", "on"],
            &["bass", "on"],
            &["preset", "save", "direct"],
            &["preset", "load", "movies"],
            &["eq", "import", file.to_str().unwrap()],
        ] {
            let e = rig.exec(args).unwrap_err();
            assert!(
                matches!(&e, CliError::Failure(m) if m.starts_with("Direct Mode is on")),
                "{args:?}: {e}"
            );
        }
        assert_eq!(
            rig.fake.written().len(),
            writes,
            "nothing reached the device"
        );
        assert_eq!(rig.exec(&["preset", "list"]).unwrap(), "movies\n");
        // Mic, decoder, filter, light and output still work.
        for args in [
            &["mic", "nr", "1"][..],
            &["dolby", "full"],
            &["filter", "nos"],
            &["light", "off"],
            &["output", "speakers"],
            &["output", "headphones"],
        ] {
            rig.exec(args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
        }
        rig.exec(&["direct", "off"]).unwrap();
        assert_eq!(rig.exec(&["eq", "on"]).unwrap(), "eq: on\n");
    }

    #[test]
    fn scout_owned_settings_are_refused_while_scout_is_on() {
        let rig = Rig::new();
        rig.exec(&["eq", "set", "62=2"]).unwrap();
        let file = rig._tmp.0.join("autoeq.txt");
        std::fs::write(
            &file,
            "Preamp: -3.0 dB\nFilter 1: ON PK Fc 1000 Hz Gain -3.0 dB Q 1.0\n",
        )
        .unwrap();
        rig.exec(&["scout", "on"]).unwrap();
        // Reads are not counted: the device is opened to read the mask.
        let writes = |rig: &Rig| {
            rig.fake
                .written()
                .iter()
                .filter(|f| {
                    f[1] == g6_core::protocol::cmd::FEATURE_WRITE
                        || (f[1] == g6_core::protocol::cmd::MASK && f[3] == 0x07)
                })
                .count()
        };
        let before = writes(&rig);
        for args in [
            &["eq", "on"][..],
            &["eq", "off"],
            &["eq", "set", "1k=2"],
            &["smartvol", "0.5"],
            &["smartvol", "off"],
            &["eq", "import", file.to_str().unwrap()],
        ] {
            let e = rig.exec(args).unwrap_err();
            assert!(
                matches!(&e, CliError::Failure(m)
                    if m == "Scout Mode is on: EQ and Smart Volume are set by the device"),
                "{args:?}: {e}"
            );
        }
        assert_eq!(writes(&rig), before, "nothing reached the device");
        // The pre-amp, other effects, SBX and Scout itself are not locked.
        for args in [
            &["eq", "preamp", "-1"][..],
            &["bass", "on"],
            &["surround", "0.4"],
            &["scout", "off"],
        ] {
            rig.exec(args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
        }
        assert_eq!(rig.exec(&["eq", "on"]).unwrap(), "eq: on\n");
    }

    #[test]
    fn imported_files_are_size_checked_before_they_are_read_whole() {
        let rig = Rig::new();
        rig.fake.unplug();
        let dir = &rig._tmp.0;
        let big = dir.join("big.json");
        std::fs::write(&big, " ".repeat(preset::MAX_FILE_BYTES + 1)).unwrap();
        let e = rig
            .exec(&["preset", "import", big.to_str().unwrap()])
            .unwrap_err();
        assert!(
            matches!(&e, CliError::Usage(m) if m.contains("larger than 256 KiB")),
            "{e}"
        );
        let big = dir.join("big.txt");
        std::fs::write(&big, " ".repeat(eq_import::MAX_INPUT_BYTES + 1)).unwrap();
        let e = rig
            .exec(&["eq", "import", big.to_str().unwrap()])
            .unwrap_err();
        assert!(
            matches!(&e, CliError::Usage(m) if m.contains("larger than 1024 KiB")),
            "{e}"
        );
        // Not a regular file (a directory here; also FIFOs and devices).
        for args in [["preset", "import"], ["eq", "import"]] {
            let mut args = args.to_vec();
            args.push(dir.to_str().unwrap());
            let e = rig.exec(&args).unwrap_err();
            assert!(
                matches!(&e, CliError::Usage(m) if m.contains("not a regular file")),
                "{e}"
            );
        }
        #[cfg(unix)]
        {
            let e = rig.exec(&["eq", "import", "/dev/zero"]).unwrap_err();
            assert!(
                matches!(&e, CliError::Usage(m) if m.contains("not a regular file")),
                "{e}"
            );
        }
        assert_eq!(rig.exec(&["preset", "list"]).unwrap(), "");
    }

    #[test]
    fn preset_export_replaces_a_file_whole() {
        let rig = Rig::new();
        rig.exec(&["eq", "set", "62=2"]).unwrap();
        rig.exec(&["preset", "save", "movies"]).unwrap();
        let dir = rig._tmp.0.join("out");
        let file = dir.join("movies.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&file, "x".repeat(100_000)).unwrap();
        rig.exec(&["preset", "export", "movies", file.to_str().unwrap()])
            .unwrap();
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(json["headphones"]["eq_62"], 2.0);
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["movies.json"], "no temp file left behind");
    }

    /// Output the test thread can read while the command runs.
    #[derive(Clone, Default)]
    struct SharedOut(Arc<Mutex<Vec<u8>>>);

    impl SharedOut {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    impl Write for SharedOut {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// Run `g6 watch ARGS`. `inject` runs once `connected` is printed, i.e.
    /// after the connect sync, so its pushes are not part of the synced
    /// state; the watch ends once `done` holds for the output (plus a moment
    /// for repeats to show up), or after a generous limit.
    fn watch_with(
        rig: &Rig,
        args: &[&str],
        inject: impl FnOnce(&FakeDevice) + Send + 'static,
        done: impl Fn(&str) -> bool + Send + 'static,
    ) -> String {
        let stop = Arc::new(AtomicBool::new(false));
        let (fake, paths) = (rig.fake.clone(), rig.paths.clone());
        let session_paths = paths.clone();
        let stop_events = stop.clone();
        let mut env = Env::new(
            Box::new(move || {
                let (session, events) = g6_core::session::Session::start(
                    Box::new(fake.connector()),
                    fast(),
                    SessionConfig {
                        paths: Some(session_paths.clone()),
                        reapply_on_first_connect: false,
                    },
                );
                // Forward the events until told to stop: then `watch` sees
                // the channel close and returns.
                let (tx, rx) = std::sync::mpsc::channel();
                let stop = stop_events.clone();
                std::thread::spawn(move || {
                    while !stop.load(Ordering::SeqCst) {
                        match events.recv_timeout(Duration::from_millis(10)) {
                            Ok(e) if tx.send(e).is_err() => break,
                            Ok(_) | Err(RecvTimeoutError::Timeout) => {}
                            Err(RecvTimeoutError::Disconnected) => break,
                        }
                    }
                });
                (session, rx)
            }),
            Some(paths),
            Duration::from_secs(2),
        );
        env.watch_limit = Some(Duration::from_secs(30));
        let out = SharedOut::default();
        let t = {
            let (out, fake) = (out.clone(), rig.fake.clone());
            std::thread::spawn(move || {
                let wait_for = |what: &dyn Fn(&str) -> bool| {
                    let deadline = Instant::now() + Duration::from_secs(20);
                    while !what(&out.text()) && Instant::now() < deadline {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                };
                wait_for(&|t: &str| t.contains("connected"));
                inject(&fake);
                wait_for(&done);
                std::thread::sleep(Duration::from_millis(300));
                stop.store(true, Ordering::SeqCst);
            })
        };
        let mut argv = vec!["g6"];
        argv.extend_from_slice(args);
        let action = action::build(Cli::try_parse_from(argv).unwrap().command).unwrap();
        run(action, &mut env, &mut out.clone()).unwrap();
        t.join().unwrap();
        out.text()
    }

    #[test]
    fn watch_streams_events() {
        let rig = Rig::new();
        let out = watch_with(
            &rig,
            &["watch"],
            |fake| {
                fake.press_mask(0x01);
                fake.switch_output(Output::Speakers);
            },
            |out| out.contains("output: speakers"),
        );
        assert!(out.contains("connected"), "{out}");
        assert!(out.contains("mask: sbx on"), "{out}");
        assert!(out.contains("output: speakers"), "{out}");
    }

    #[test]
    fn watch_json_prints_one_object_per_event() {
        let rig = Rig::new();
        let out = watch_with(
            &rig,
            &["watch", "--json"],
            |fake| {
                fake.press_mask(0x01);
                fake.switch_output(Output::Speakers);
                fake.switch_output(Output::Speakers);
                fake.push_raw(&[
                    0x5a, 0x11, 0x08, 0x01, 0x00, 0x96, 0x0a, 0x00, 0x00, 0x20, 0xc0,
                ]);
            },
            |out| out.contains("\"eq_preamp\"") && out.contains("\"output\":\"speakers\""),
        );
        let events: Vec<serde_json::Value> = out
            .lines()
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
            .collect();
        for e in &events {
            assert!(e["ts"].as_f64().unwrap() > 1.0e9, "{e}");
        }
        let of = |kind: &str| events.iter().filter(|e| e["event"] == kind).count();
        assert_eq!(of("connected"), 1, "{out}");
        assert_eq!(of("mask"), 1, "{out}");
        assert_eq!(of("output"), 1, "{out}");
        let mask = events.iter().find(|e| e["event"] == "mask").unwrap();
        assert_eq!(
            (mask["sbx"].as_bool(), mask["scout"].as_bool()),
            (Some(true), Some(false))
        );
        let output = events.iter().find(|e| e["event"] == "output").unwrap();
        assert_eq!(output["output"], "speakers");
        let preamp = events
            .iter()
            .find(|e| e["event"] == "feature" && e["name"] == "eq_preamp" && e["value"] == -2.5)
            .unwrap_or_else(|| panic!("{out}"));
        assert_eq!(
            (&preamp["value"], &preamp["text"]),
            (&serde_json::json!(-2.5), &serde_json::json!("-2.5 dB"))
        );
        assert!(!out.contains("[ "), "no text lines in --json mode: {out}");
    }

    #[test]
    fn watch_prints_only_changes_and_shows_the_preamp() {
        let rig = Rig::new();
        let out = watch_with(
            &rig,
            &["watch"],
            |fake| {
                fake.switch_output(Output::Speakers);
                fake.switch_output(Output::Speakers);
                // A pushed pre-amp change (-2.5 dB), repeated: shown once.
                fake.push_raw(&[
                    0x5a, 0x11, 0x08, 0x01, 0x00, 0x96, 0x0a, 0x00, 0x00, 0x20, 0xc0,
                ]);
                fake.push_raw(&[
                    0x5a, 0x11, 0x08, 0x01, 0x00, 0x96, 0x0a, 0x00, 0x00, 0x20, 0xc0,
                ]);
            },
            |out| out.contains("eq_preamp: -2.5 dB") && out.contains("output: speakers"),
        );
        assert_eq!(out.matches("output: speakers").count(), 1, "{out}");
        assert!(out.matches("eq_31:").count() <= 1, "{out}");
        assert_eq!(out.matches("eq_preamp: -2.5 dB").count(), 1, "{out}");
    }
}
