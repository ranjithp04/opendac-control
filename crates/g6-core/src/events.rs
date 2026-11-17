//! Reader thread owning the device; reply matching and push events.
//!
//! One background thread owns the [`Transport`]. [`Device`] methods send
//! commands to it over a channel and block for the answer. Replies are
//! matched by command code (plus family / id for feature reads, `0x11`);
//! every other incoming frame is a push and becomes an [`Event`].
//!
//! Writes follow this sequence: send → wait for ACK
//! `5a 02 0a CMD STATUS` → read back → confirm. A non-zero status is an
//! error. A missing ACK is reported as [`Ack::Missing`] (the first write
//! after open is known not to ACK, §5 #3) and the read-back decides.
//!
//! When the device goes away the thread emits [`Event::Disconnected`] and
//! keeps reconnecting with backoff; [`Event::Connected`] follows a
//! successful reconnect.

use std::fmt;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::features::{Feature, FeatureError};
use crate::protocol::{
    self, cmd, DacFilter, Frame, Mask, MaskTarget, Message, MicBoost, Output, ProtocolError, Rgb,
};
use crate::transport::{self, Backoff, Connector, HidConnector, Transport, TransportError};

/// Something the device reported without being asked.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event {
    /// Device opened (first connect or reconnect). Re-read / re-apply state.
    Connected,
    /// A setting changed (push, hardware button, or a write's echo).
    Changed(Change),
    /// Headphone jack switched the active output (`5a 2c` push).
    OutputSwitched(Output),
    /// Device lost; the thread keeps trying to reconnect.
    Disconnected,
}

/// A changed setting.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Change {
    /// Known feature with its rounded value.
    Feature {
        feature: &'static Feature,
        value: f32,
    },
    Mask(Mask),
    DacFilter(DacFilter),
    MicBoost(MicBoost),
    /// LED on / off.
    LightingEnabled(bool),
    /// LED color.
    LightingColor(Rgb),
}

/// Outcome of the ACK step of a write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ack {
    /// `5a 02 0a CMD 00`.
    Ok,
    /// No ACK within the timeout. `first_write` = first write after open
    /// (expected, `docs/protocol-g6.md` §3 item 3). The write was confirmed by read-back.
    Missing { first_write: bool },
}

/// A confirmed write: the value read back and how the ACK went.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Written<T> {
    pub value: T,
    pub ack: Ack,
}

/// Errors from [`Device`] calls.
#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// The device is not connected (reconnect in progress).
    NotConnected,
    /// No reply to command `cmd` within the timeout.
    Timeout { cmd: u8 },
    /// ACK with a non-zero status (`80` / `81` / `83`).
    Rejected { cmd: u8, status: u8 },
    /// Read-back after a write did not match what was sent.
    Mismatch { expected: String, read_back: String },
    /// Refused before sending (not in the allow-list, out of range, …).
    Protocol(ProtocolError),
    /// Transport failed; the device is treated as disconnected.
    Transport(TransportError),
    /// The device thread has stopped.
    Shutdown,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotConnected => write!(f, "G6 not connected"),
            Error::Timeout { cmd } => write!(f, "no reply to command {cmd:#04x}"),
            Error::Rejected { cmd, status } => {
                write!(f, "command {cmd:#04x} rejected (status {status:#04x})")
            }
            Error::Mismatch {
                expected,
                read_back,
            } => write!(
                f,
                "write not applied: sent {expected}, read back {read_back}"
            ),
            Error::Protocol(e) => e.fmt(f),
            Error::Transport(e) => e.fmt(f),
            Error::Shutdown => write!(f, "device thread stopped"),
        }
    }
}

impl std::error::Error for Error {}

impl From<ProtocolError> for Error {
    fn from(e: ProtocolError) -> Self {
        Error::Protocol(e)
    }
}

impl From<FeatureError> for Error {
    fn from(e: FeatureError) -> Self {
        Error::Protocol(ProtocolError::Feature(e))
    }
}

/// Timing for the device thread.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// Wait for a query reply / read-back.
    pub reply_timeout: Duration,
    /// Wait for a write ACK.
    pub ack_timeout: Duration,
    /// Read timeout while idle (also the command latency bound).
    pub poll_interval: Duration,
    /// Wait for the identity reply when connecting.
    pub probe_timeout: Duration,
    /// Reconnect backoff.
    pub backoff: Backoff,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            reply_timeout: Duration::from_millis(500),
            ack_timeout: Duration::from_millis(300),
            poll_interval: Duration::from_millis(20),
            probe_timeout: Duration::from_millis(500),
            backoff: Backoff::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Commands to the device thread
// ---------------------------------------------------------------------------

/// What reply a command waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// A decoded reply with this command code (not an ACK / unknown frame).
    Cmd(u8),
    /// A single-entry `5a 11` frame for this family / id (the read reply,
    /// `docs/protocol-g6.md` §2.2). Multi-entry frames are pushes: the
    /// Scout-on push carries a wrong "EQ off" and must not answer a read.
    Feature(u8, u8),
    /// The LED on / off reply. `5a 3a` carries two kinds of reply, so the
    /// command code alone cannot tell a pushed colour from the status asked for.
    LightingEnabled,
    /// The LED colour reply (see [`Expect::LightingEnabled`]).
    LightingColor,
}

impl Expect {
    fn cmd(self) -> u8 {
        match self {
            Expect::Cmd(c) => c,
            Expect::Feature(..) => cmd::FEATURE_READ,
            Expect::LightingEnabled | Expect::LightingColor => cmd::LIGHTING,
        }
    }

    fn matches(self, msg: &Message) -> bool {
        match (self, msg) {
            (_, Message::Ack { .. } | Message::Unknown(_)) => false,
            (Expect::Feature(fam, id), Message::Features(v)) => {
                matches!(v.as_slice(), [e] if e.family == fam && e.id == id)
            }
            (Expect::Feature(..), _) => false,
            (Expect::LightingEnabled, m) => matches!(m, Message::LightingEnabled(_)),
            (Expect::LightingColor, m) => matches!(m, Message::LightingColor(_)),
            (Expect::Cmd(c), m) => m.cmd() == Some(c),
        }
    }
}

type Reply<T> = Sender<Result<T, Error>>;

enum Cmd {
    Query {
        frame: Frame,
        expect: Expect,
        reply: Reply<Message>,
    },
    Write {
        /// Sent in order, each waiting for its own ACK, before the read-back.
        frames: Vec<Frame>,
        readback: Frame,
        expect: Expect,
        reply: Reply<(Message, Ack)>,
    },
    IsConnected(Sender<bool>),
    Shutdown,
}

// ---------------------------------------------------------------------------
// Public handle
// ---------------------------------------------------------------------------

/// Handle to the device thread. Dropping it stops the thread.
pub struct Device {
    tx: Sender<Cmd>,
    thread: Option<JoinHandle<()>>,
}

impl Device {
    /// Start the device thread on the real G6 (hidapi, interface 4 only).
    pub fn open_hid(config: Config) -> (Device, Receiver<Event>) {
        Self::spawn(Box::new(HidConnector::new()), config)
    }

    /// Start the device thread with any connector (e.g. a fake device).
    pub fn spawn(connector: Box<dyn Connector>, config: Config) -> (Device, Receiver<Event>) {
        let (tx, cmds) = mpsc::channel();
        let (events, rx) = mpsc::channel();
        let worker = Worker {
            connector,
            config,
            events,
            cmds,
            link: None,
        };
        let thread = thread::Builder::new()
            .name("g6-device".into())
            .spawn(move || worker.run())
            .expect("spawn g6-device thread");
        (
            Device {
                tx,
                thread: Some(thread),
            },
            rx,
        )
    }

    fn call<T>(&self, make: impl FnOnce(Reply<T>) -> Cmd) -> Result<T, Error> {
        let (tx, rx) = mpsc::channel();
        self.tx.send(make(tx)).map_err(|_| Error::Shutdown)?;
        rx.recv().map_err(|_| Error::Shutdown)?
    }

    fn query(&self, frame: Frame, expect: Expect) -> Result<Message, Error> {
        self.call(|reply| Cmd::Query {
            frame,
            expect,
            reply,
        })
    }

    fn write(
        &self,
        frame: Frame,
        readback: Frame,
        expect: Expect,
    ) -> Result<(Message, Ack), Error> {
        self.write_all(vec![frame], readback, expect)
    }

    fn write_all(
        &self,
        frames: Vec<Frame>,
        readback: Frame,
        expect: Expect,
    ) -> Result<(Message, Ack), Error> {
        self.call(|reply| Cmd::Write {
            frames,
            readback,
            expect,
            reply,
        })
    }

    /// Whether the thread currently has the device open.
    pub fn is_connected(&self) -> bool {
        let (tx, rx) = mpsc::channel();
        self.tx.send(Cmd::IsConnected(tx)).is_ok() && rx.recv().unwrap_or(false)
    }

    /// Identity bytes (`5a 05` reply payload).
    pub fn identity(&self) -> Result<Vec<u8>, Error> {
        match self.query(protocol::identity(), Expect::Cmd(cmd::IDENTITY))? {
            Message::Identity(b) => Ok(b),
            m => Err(unexpected(cmd::IDENTITY, &m)),
        }
    }

    /// Firmware version string.
    pub fn firmware(&self) -> Result<String, Error> {
        match self.query(protocol::firmware(), Expect::Cmd(cmd::FIRMWARE))? {
            Message::Firmware(s) => Ok(s),
            m => Err(unexpected(cmd::FIRMWARE, &m)),
        }
    }

    /// Serial number bytes (8). Treat as sensitive: do not log it.
    pub fn serial(&self) -> Result<[u8; 8], Error> {
        match self.query(protocol::serial(), Expect::Cmd(cmd::SERIAL))? {
            Message::Serial(b) => Ok(b),
            // Not `unexpected()`: that logs the message, which may hold the serial.
            _ => {
                log::warn!("unexpected reply to the serial query");
                Err(Error::Timeout { cmd: cmd::SERIAL })
            }
        }
    }

    /// Hardware id bytes.
    pub fn hw_id(&self) -> Result<Vec<u8>, Error> {
        match self.query(protocol::hw_id(), Expect::Cmd(cmd::HW_ID))? {
            Message::HwId(b) => Ok(b),
            m => Err(unexpected(cmd::HW_ID, &m)),
        }
    }

    /// DSP version bytes.
    pub fn dsp_version(&self) -> Result<Vec<u8>, Error> {
        match self.query(protocol::dsp_version(), Expect::Cmd(cmd::DSP_VERSION))? {
            Message::DspVersion(b) => Ok(b),
            m => Err(unexpected(cmd::DSP_VERSION, &m)),
        }
    }

    /// Read a feature; returns the rounded device-side value.
    pub fn read_feature(&self, feature: &'static Feature) -> Result<f32, Error> {
        let frame = protocol::feature_read(feature.family, feature.id)?;
        let msg = self.query(frame, Expect::Feature(feature.family, feature.id))?;
        feature_value(feature, &msg)
    }

    /// Write a feature (validated and rounded first), confirm by read-back.
    pub fn write_feature(
        &self,
        feature: &'static Feature,
        value: f32,
    ) -> Result<Written<f32>, Error> {
        let (frame, sent) = protocol::feature_write_for(feature, value)?;
        let readback = protocol::feature_read(feature.family, feature.id)?;
        let (msg, ack) =
            self.write(frame, readback, Expect::Feature(feature.family, feature.id))?;
        let got = feature_value(feature, &msg)?;
        if (got - sent).abs() > 1e-4 {
            return Err(Error::Mismatch {
                expected: format!("{} = {sent}", feature.name),
                read_back: format!("{got}"),
            });
        }
        Ok(Written { value: got, ack })
    }

    /// Global mask (SBX / Scout / Voice Clarity).
    pub fn read_mask(&self) -> Result<Mask, Error> {
        match self.query(protocol::mask_read(), Expect::Cmd(cmd::MASK))? {
            Message::Mask(m) => Ok(m),
            m => Err(unexpected(cmd::MASK, &m)),
        }
    }

    /// Turn SBX or Scout on/off; confirmed by reading the mask back.
    pub fn set_mask(&self, target: MaskTarget, on: bool) -> Result<Written<Mask>, Error> {
        let (msg, ack) = self.write(
            protocol::mask_write(target, on),
            protocol::mask_read(),
            Expect::Cmd(cmd::MASK),
        )?;
        let Message::Mask(m) = msg else {
            return Err(unexpected(cmd::MASK, &msg));
        };
        let bit = m.0 & (target as u8) != 0;
        if bit != on {
            return Err(Error::Mismatch {
                expected: format!("{target:?} = {on}"),
                read_back: format!("mask {:#04x}", m.0),
            });
        }
        Ok(Written { value: m, ack })
    }

    /// Active output.
    pub fn read_output(&self) -> Result<Output, Error> {
        match self.query(protocol::output_read(), Expect::Cmd(cmd::OUTPUT))? {
            Message::Output(o) => Ok(o),
            m => Err(unexpected(cmd::OUTPUT, &m)),
        }
    }

    pub fn read_dac_filter(&self) -> Result<DacFilter, Error> {
        match self.query(protocol::dac_filter_read(), Expect::Cmd(cmd::DAC_FILTER))? {
            Message::DacFilter(x) => Ok(x),
            m => Err(unexpected(cmd::DAC_FILTER, &m)),
        }
    }

    pub fn set_dac_filter(&self, filter: DacFilter) -> Result<Written<DacFilter>, Error> {
        let (msg, ack) = self.write(
            protocol::dac_filter_write(filter),
            protocol::dac_filter_read(),
            Expect::Cmd(cmd::DAC_FILTER),
        )?;
        match msg {
            Message::DacFilter(x) if x == filter => Ok(Written { value: x, ack }),
            Message::DacFilter(x) => Err(Error::Mismatch {
                expected: filter.name().into(),
                read_back: x.name().into(),
            }),
            m => Err(unexpected(cmd::DAC_FILTER, &m)),
        }
    }

    /// Switch the output by hand (`5a 2c 05 00 MODE 00 00 00`); confirmed by
    /// reading the output back.
    ///
    /// A manual override only: the headphone jack decides the output, so
    /// the session never re-applies it after a reconnect. The device answers
    /// with the new output, a dump of its profile and the ACK in the middle
    /// of the dump; all of it reaches the caller of [`Device::spawn`] as
    /// events ([`Event::OutputSwitched`], [`Change`]).
    pub fn set_output(&self, output: Output) -> Result<Written<Output>, Error> {
        let (msg, ack) = self.write(
            protocol::output_write(output),
            protocol::output_read(),
            Expect::Cmd(cmd::OUTPUT),
        )?;
        match msg {
            Message::Output(x) if x == output => Ok(Written { value: x, ack }),
            Message::Output(x) => Err(Error::Mismatch {
                expected: format!("{output:?}"),
                read_back: format!("{x:?}"),
            }),
            m => Err(unexpected(cmd::OUTPUT, &m)),
        }
    }

    /// LED on / off.
    pub fn read_lighting_enabled(&self) -> Result<bool, Error> {
        match self.query(protocol::lighting_enabled_read(), Expect::LightingEnabled)? {
            Message::LightingEnabled(x) => Ok(x),
            m => Err(unexpected(cmd::LIGHTING, &m)),
        }
    }

    pub fn set_lighting_enabled(&self, on: bool) -> Result<Written<bool>, Error> {
        let (msg, ack) = self.write(
            protocol::lighting_enable_write(on),
            protocol::lighting_enabled_read(),
            Expect::LightingEnabled,
        )?;
        match msg {
            Message::LightingEnabled(x) if x == on => Ok(Written { value: x, ack }),
            Message::LightingEnabled(x) => Err(Error::Mismatch {
                expected: format!("lighting {on}"),
                read_back: format!("lighting {x}"),
            }),
            m => Err(unexpected(cmd::LIGHTING, &m)),
        }
    }

    /// LED color.
    pub fn read_lighting_color(&self) -> Result<Rgb, Error> {
        match self.query(protocol::lighting_color_read(), Expect::LightingColor)? {
            Message::LightingColor(x) => Ok(x),
            m => Err(unexpected(cmd::LIGHTING, &m)),
        }
    }

    /// Set the static LED color. Sends the static-mode frame and then the
    /// color frame, every time (the mode is the only one the device accepts).
    /// Does not switch the LED on.
    pub fn set_lighting_color(&self, rgb: Rgb) -> Result<Written<Rgb>, Error> {
        let (msg, ack) = self.write_all(
            vec![
                protocol::lighting_mode_static(),
                protocol::lighting_color_write(rgb),
            ],
            protocol::lighting_color_read(),
            Expect::LightingColor,
        )?;
        match msg {
            Message::LightingColor(x) if x == rgb => Ok(Written { value: x, ack }),
            Message::LightingColor(x) => Err(Error::Mismatch {
                expected: protocol::rgb_to_hex(rgb),
                read_back: protocol::rgb_to_hex(x),
            }),
            m => Err(unexpected(cmd::LIGHTING, &m)),
        }
    }

    /// Creative's hardware Direct Mode (`5a 39`), as the device reports it.
    pub fn read_hw_direct(&self) -> Result<bool, Error> {
        match self.query(protocol::hw_direct_read(), Expect::Cmd(cmd::HW_DIRECT))? {
            Message::HwDirect(x) => Ok(x),
            m => Err(unexpected(cmd::HW_DIRECT, &m)),
        }
    }

    /// Turn the hardware Direct Mode on or off. The write only takes effect
    /// with the commit (`5a 39 01 01`), which is also the read-back that
    /// confirms it. Off makes the device push a full profile dump.
    pub fn set_hw_direct(&self, on: bool) -> Result<Written<bool>, Error> {
        let (msg, ack) = self.write(
            protocol::hw_direct_write(on),
            protocol::hw_direct_read(),
            Expect::Cmd(cmd::HW_DIRECT),
        )?;
        match msg {
            Message::HwDirect(x) if x == on => Ok(Written { value: x, ack }),
            Message::HwDirect(x) => Err(Error::Mismatch {
                expected: format!("hardware direct mode {on}"),
                read_back: format!("hardware direct mode {x}"),
            }),
            m => Err(unexpected(cmd::HW_DIRECT, &m)),
        }
    }

    pub fn read_mic_boost(&self) -> Result<MicBoost, Error> {
        match self.query(protocol::mic_boost_read(), Expect::Cmd(cmd::MIC_BOOST))? {
            Message::MicBoost(x) => Ok(x),
            m => Err(unexpected(cmd::MIC_BOOST, &m)),
        }
    }

    pub fn set_mic_boost(&self, boost: MicBoost) -> Result<Written<MicBoost>, Error> {
        let (msg, ack) = self.write(
            protocol::mic_boost_write(boost),
            protocol::mic_boost_read(),
            Expect::Cmd(cmd::MIC_BOOST),
        )?;
        match msg {
            Message::MicBoost(x) if x == boost => Ok(Written { value: x, ack }),
            Message::MicBoost(x) => Err(Error::Mismatch {
                expected: format!("{} dB", boost.db()),
                read_back: format!("{} dB", x.db()),
            }),
            m => Err(unexpected(cmd::MIC_BOOST, &m)),
        }
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn unexpected(cmd: u8, m: &Message) -> Error {
    log::warn!("unexpected reply to {cmd:#04x}: {m:?}");
    Error::Timeout { cmd }
}

fn feature_value(feature: &Feature, msg: &Message) -> Result<f32, Error> {
    if let Message::Features(v) = msg {
        if let Some(e) = v
            .iter()
            .find(|e| e.family == feature.family && e.id == feature.id)
        {
            return Ok(feature.round(e.value));
        }
    }
    Err(unexpected(cmd::FEATURE_READ, msg))
}

// ---------------------------------------------------------------------------
// Device thread
// ---------------------------------------------------------------------------

struct Link {
    t: Box<dyn Transport>,
    writes: u32,
}

struct Worker {
    connector: Box<dyn Connector>,
    config: Config,
    events: Sender<Event>,
    cmds: Receiver<Cmd>,
    link: Option<Link>,
}

impl Worker {
    fn run(mut self) {
        let mut delays = self.config.backoff.delays();
        let mut attempt = 0u32;
        loop {
            if self.link.is_none() {
                attempt += 1;
                match transport::open_and_probe(&mut *self.connector, self.config.probe_timeout) {
                    Ok(t) => {
                        log::info!("G6 connected");
                        self.link = Some(Link { t, writes: 0 });
                        self.emit(Event::Connected);
                        delays = self.config.backoff.delays();
                        attempt = 0;
                    }
                    Err(e) => {
                        transport::log_connect_error(attempt, &e);
                        let wait = delays.next().unwrap_or(self.config.backoff.max);
                        if !self.idle_disconnected(wait) {
                            return;
                        }
                    }
                }
                continue;
            }
            match self.cmds.try_recv() {
                Ok(Cmd::Shutdown) | Err(TryRecvError::Disconnected) => return,
                Ok(c) => self.execute(c),
                Err(TryRecvError::Empty) => self.poll(),
            }
        }
    }

    fn emit(&self, e: Event) {
        // The receiver may have been dropped; the thread keeps serving calls.
        let _ = self.events.send(e);
    }

    /// While disconnected: answer commands with `NotConnected` for `wait`.
    /// Returns `false` on shutdown.
    fn idle_disconnected(&mut self, wait: Duration) -> bool {
        let deadline = Instant::now() + wait;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.cmds.recv_timeout(left) {
                Ok(Cmd::Shutdown) | Err(RecvTimeoutError::Disconnected) => return false,
                Ok(Cmd::Query { reply, .. }) => {
                    let _ = reply.send(Err(Error::NotConnected));
                }
                Ok(Cmd::Write { reply, .. }) => {
                    let _ = reply.send(Err(Error::NotConnected));
                }
                Ok(Cmd::IsConnected(r)) => {
                    let _ = r.send(false);
                }
                Err(RecvTimeoutError::Timeout) => return true,
            }
        }
    }

    fn lost(&mut self, e: &TransportError) {
        log::warn!("G6 disconnected: {e}");
        self.link = None;
        self.emit(Event::Disconnected);
    }

    fn poll(&mut self) {
        let timeout = self.config.poll_interval;
        let Some(link) = self.link.as_mut() else {
            return;
        };
        match link.t.read_frame(timeout) {
            Ok(Some(f)) => self.dispatch_push(protocol::decode(&f)),
            Ok(None) => {}
            Err(e) => self.lost(&e),
        }
    }

    fn execute(&mut self, c: Cmd) {
        match c {
            Cmd::Query {
                frame,
                expect,
                reply,
            } => {
                let r = self.do_query(&frame, expect);
                let _ = reply.send(r);
            }
            Cmd::Write {
                frames,
                readback,
                expect,
                reply,
            } => {
                let r = self.do_write(&frames, &readback, expect);
                let _ = reply.send(r);
            }
            Cmd::IsConnected(r) => {
                let _ = r.send(self.link.is_some());
            }
            Cmd::Shutdown => {}
        }
    }

    fn send(&mut self, frame: &Frame) -> Result<(), Error> {
        let link = self.link.as_mut().ok_or(Error::NotConnected)?;
        link.t.write_frame(frame).map_err(|e| {
            self.lost(&e);
            Error::Transport(e)
        })
    }

    /// Read until `want` matches or `timeout` passes; every other frame is a push.
    fn await_frame(
        &mut self,
        timeout: Duration,
        want: impl Fn(&Message) -> bool,
    ) -> Result<Option<Message>, Error> {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            let link = self.link.as_mut().ok_or(Error::NotConnected)?;
            match link.t.read_frame(left) {
                Ok(Some(f)) => {
                    let msg = protocol::decode(&f);
                    if want(&msg) {
                        return Ok(Some(msg));
                    }
                    self.dispatch_push(msg);
                }
                Ok(None) => {}
                Err(e) => {
                    self.lost(&e);
                    return Err(Error::Transport(e));
                }
            }
        }
    }

    fn do_query(&mut self, frame: &Frame, expect: Expect) -> Result<Message, Error> {
        self.send(frame)?;
        // A rejected read comes back as an ACK with an error status instead of
        // the reply (§2.2a); fail fast on it. Feature reads keep waiting: ids
        // outside the table are rejected the same way and callers expect a timeout.
        let cmd = frame[1];
        let rejects = |m: &Message| {
            cmd != protocol::cmd::FEATURE_READ
                && matches!(m, Message::Ack { cmd: c, status } if *c == cmd && !status.is_ok())
        };
        match self.await_frame(self.config.reply_timeout, |m| {
            expect.matches(m) || rejects(m)
        })? {
            Some(Message::Ack {
                status: protocol::AckStatus::Rejected(status),
                ..
            }) => Err(Error::Rejected { cmd, status }),
            Some(m) => Ok(m),
            None => Err(Error::Timeout { cmd: expect.cmd() }),
        }
    }

    /// Send one write frame and wait for its ACK. The first write after open
    /// is known not to ACK (§3 item 3); the read-back decides.
    fn send_acked(&mut self, frame: &Frame) -> Result<Ack, Error> {
        let write_cmd = frame[1];
        let first_write = {
            let link = self.link.as_mut().ok_or(Error::NotConnected)?;
            link.writes += 1;
            link.writes == 1
        };
        self.send(frame)?;
        let ack = self.await_frame(
            self.config.ack_timeout,
            |m| matches!(m, Message::Ack { cmd, .. } if *cmd == write_cmd),
        )?;
        Ok(match ack {
            Some(Message::Ack { status, .. }) if status.is_ok() => Ack::Ok,
            Some(Message::Ack {
                status: protocol::AckStatus::Rejected(status),
                ..
            }) => {
                return Err(Error::Rejected {
                    cmd: write_cmd,
                    status,
                })
            }
            _ => {
                if first_write {
                    log::debug!(
                        "no ACK for first write {write_cmd:#04x} after open; checking read-back"
                    );
                } else {
                    log::warn!("no ACK for write {write_cmd:#04x}; checking read-back");
                }
                Ack::Missing { first_write }
            }
        })
    }

    fn do_write(
        &mut self,
        frames: &[Frame],
        readback: &Frame,
        expect: Expect,
    ) -> Result<(Message, Ack), Error> {
        // A rejected frame stops the sequence; a missing ACK does not, and the
        // first missing one is the one reported.
        let mut ack = Ack::Ok;
        for frame in frames {
            let a = self.send_acked(frame)?;
            if ack == Ack::Ok {
                ack = a;
            }
        }
        let msg = self.do_query(readback, expect)?;
        // The read-back is the device's own report of the new value. Some
        // writes (DAC filter, mic boost) are not echoed as a push, so without
        // this the session state would never learn of them. Where the device
        // does echo, the duplicate changes nothing (`DeviceState::apply`).
        self.dispatch_push(msg.clone());
        Ok((msg, ack))
    }

    fn dispatch_push(&mut self, msg: Message) {
        match msg {
            Message::Features(v) => {
                for e in v {
                    match e.known() {
                        Some((feature, value)) => {
                            self.emit(Event::Changed(Change::Feature { feature, value }))
                        }
                        None => log::debug!(
                            "ignoring undocumented feature {:02x}:{:02x} = {}",
                            e.family,
                            e.id,
                            e.value
                        ),
                    }
                }
            }
            Message::Mask(m) => self.emit(Event::Changed(Change::Mask(m))),
            Message::Output(o) => self.emit(Event::OutputSwitched(o)),
            Message::DacFilter(x) => self.emit(Event::Changed(Change::DacFilter(x))),
            Message::MicBoost(x) => self.emit(Event::Changed(Change::MicBoost(x))),
            Message::LightingEnabled(x) => self.emit(Event::Changed(Change::LightingEnabled(x))),
            Message::LightingColor(x) => self.emit(Event::Changed(Change::LightingColor(x))),
            // The serial number is never logged (`Device::serial`).
            Message::Serial(_) => log::debug!("ignoring unsolicited serial reply"),
            Message::Unknown(b) if b.get(1) == Some(&cmd::SERIAL) => {
                log::debug!("ignoring malformed serial reply")
            }
            Message::Unknown(b) => log::debug!("ignoring unknown push {}", hex(&b)),
            other => log::debug!("ignoring unsolicited {other:?}"),
        }
    }
}

fn hex(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{x:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features;
    use crate::transport::fake::{FakeDevice, FAKE_FIRMWARE, FAKE_SERIAL};

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

    fn hex_bytes(s: &str) -> Vec<u8> {
        s.split_whitespace()
            .map(|b| u8::from_str_radix(b, 16).unwrap())
            .collect()
    }

    fn feat(name: &str) -> &'static Feature {
        features::by_name(name).unwrap()
    }

    fn start(dev: &FakeDevice) -> (Device, Receiver<Event>) {
        let (d, rx) = Device::spawn(Box::new(dev.connector()), fast());
        assert_eq!(next(&rx), Some(Event::Connected));
        (d, rx)
    }

    fn next(rx: &Receiver<Event>) -> Option<Event> {
        rx.recv_timeout(Duration::from_secs(2)).ok()
    }

    /// Wait for an event matching `pred`, skipping others.
    fn wait_for(rx: &Receiver<Event>, pred: impl Fn(&Event) -> bool) -> Event {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let e = rx.recv_timeout(left).expect("event not received");
            if pred(&e) {
                return e;
            }
        }
    }

    fn drain(rx: &Receiver<Event>) -> Vec<Event> {
        let mut v = Vec::new();
        while let Ok(e) = rx.recv_timeout(Duration::from_millis(50)) {
            v.push(e);
        }
        v
    }

    fn writes_with_cmd(dev: &FakeDevice, c: u8) -> usize {
        dev.written().iter().filter(|f| f[1] == c).count()
    }

    #[test]
    fn identity_queries() {
        let dev = FakeDevice::new();
        let (d, _rx) = start(&dev);
        assert_eq!(d.identity().unwrap(), vec![0x1f, 0, 0, 0]);
        assert_eq!(d.firmware().unwrap(), FAKE_FIRMWARE);
        assert_eq!(d.serial().unwrap(), FAKE_SERIAL);
        assert_eq!(d.hw_id().unwrap()[0], 0x97);
        assert_eq!(d.dsp_version().unwrap(), vec![0x30, 0x01, 0x10, 0x00]);
        assert!(d.is_connected());
    }

    #[test]
    fn read_feature_rounds_float_noise() {
        let dev = FakeDevice::new();
        // `fd ff 7f bf` from §2.4 ≈ -1.0
        dev.set_feature(0x96, 0x0b, f32::from_le_bytes([0xfd, 0xff, 0x7f, 0xbf]));
        let (d, _rx) = start(&dev);
        assert_eq!(d.read_feature(feat("eq_31")).unwrap(), -1.0);
    }

    #[test]
    fn write_feature_acks_and_confirms() {
        let dev = FakeDevice::new();
        let (d, rx) = start(&dev);
        let w = d.write_feature(feat("eq_1k"), 2.3).unwrap();
        assert_eq!(w.value, 2.5);
        assert_eq!(w.ack, Ack::Ok);
        assert_eq!(dev.feature(0x96, 0x10), Some(2.5));
        // The write's `5a 11` echo / read-back surfaces as a Changed event.
        let e = wait_for(&rx, |e| matches!(e, Event::Changed(_)));
        assert!(
            matches!(e, Event::Changed(Change::Feature { feature, value }) if feature.name == "eq_1k" && value == 2.5)
        );
    }

    #[test]
    fn missing_first_ack_is_unknown_and_read_back_decides() {
        let dev = FakeDevice::new();
        dev.set_drop_first_ack(true);
        let (d, _rx) = start(&dev);
        let w = d.write_feature(feat("eq"), 1.0).unwrap();
        assert_eq!(w.ack, Ack::Missing { first_write: true });
        let w = d.write_feature(feat("eq"), 0.0).unwrap();
        assert_eq!(w.ack, Ack::Ok);
    }

    #[test]
    fn rejected_write_is_error() {
        let dev = FakeDevice::new();
        dev.reject_next_write(0x81);
        let (d, _rx) = start(&dev);
        assert_eq!(
            d.write_feature(feat("surround"), 1.0),
            Err(Error::Rejected {
                cmd: cmd::FEATURE_WRITE,
                status: 0x81
            })
        );
        assert_eq!(dev.feature(0x96, 0x00), Some(0.0));
    }

    #[test]
    fn rejected_read_fails_fast() {
        let dev = FakeDevice::new();
        let (d, _rx) = start(&dev);
        // The fake rejects a colour read with unknown parameters with `83`.
        let frame = protocol::encode(cmd::LIGHTING, &[0x0b, 0x01, 0x00, 0x00, 0x01, 0x01]).unwrap();
        let t = Instant::now();
        assert_eq!(
            d.query(frame, Expect::Cmd(cmd::LIGHTING)),
            Err(Error::Rejected {
                cmd: cmd::LIGHTING,
                status: 0x83
            })
        );
        assert!(t.elapsed() < Duration::from_millis(250));
    }

    #[test]
    fn read_back_mismatch_is_error() {
        let dev = FakeDevice::new();
        dev.set_ignore_writes(true);
        let (d, _rx) = start(&dev);
        assert!(matches!(
            d.write_feature(feat("crystalizer"), 1.0),
            Err(Error::Mismatch { .. })
        ));
    }

    #[test]
    fn invalid_writes_never_reach_the_device() {
        let dev = FakeDevice::new();
        let (d, _rx) = start(&dev);
        assert!(matches!(
            d.write_feature(feat("eq_31"), 13.0),
            Err(Error::Protocol(_))
        ));
        assert!(matches!(
            d.write_feature(feat("eq_preamp"), 6.5),
            Err(Error::Protocol(_))
        ));
        assert_eq!(writes_with_cmd(&dev, cmd::FEATURE_WRITE), 0);
    }

    #[test]
    fn push_dump_becomes_changed_events_and_undocumented_ids_are_ignored() {
        let dev = FakeDevice::new();
        let (_d, rx) = start(&dev);
        dev.push_raw(&hex_bytes(
            "5a 11 1a 04 00 96 13 02 00 80 3f 96 14 03 00 00 3f 96 09 00 00 00 00 95 04 00 00 80 3f",
        ));
        // Undocumented 96:70 and unknown pushes produce no events.
        dev.push_raw(&hex_bytes("5a 11 08 01 00 96 70 00 00 80 3f"));
        dev.push_raw(&hex_bytes("5a 1a 02 02"));
        dev.push_raw(&hex_bytes("5a 6e 02 01"));
        let ev = drain(&rx);
        let got: Vec<(&str, f32)> = ev
            .iter()
            .map(|e| match e {
                Event::Changed(Change::Feature { feature, value }) => (feature.name, *value),
                other => panic!("unexpected event {other:?}"),
            })
            .collect();
        assert_eq!(
            got,
            [
                ("eq_8k", 1.0),
                ("eq_16k", 0.5),
                ("eq", 0.0),
                ("mic_clarity", 1.0)
            ]
        );
    }

    #[test]
    fn output_switch_and_mask_pushes() {
        let dev = FakeDevice::new();
        let (_d, rx) = start(&dev);
        dev.push_raw(&hex_bytes("5a 2c 05 01 02"));
        assert_eq!(next(&rx), Some(Event::OutputSwitched(Output::Speakers)));
        dev.push_raw(&hex_bytes("5a 26 0b 08 ff ff 05"));
        assert_eq!(next(&rx), Some(Event::Changed(Change::Mask(Mask(0x05)))));
        dev.switch_output(Output::Headphones);
        assert_eq!(next(&rx), Some(Event::OutputSwitched(Output::Headphones)));
        let ev = drain(&rx);
        let per_output = features::FEATURES
            .iter()
            .filter(|f| features::per_output(f))
            .count();
        assert_eq!(ev.len(), per_output, "dump of every per-output feature");
    }

    #[test]
    fn reply_is_matched_past_interleaved_pushes() {
        let dev = FakeDevice::new();
        dev.set_feature(0x96, 0x0c, 3.0);
        let (d, rx) = start(&dev);
        // A button press and a dump of a *different* id arrive before the reply.
        dev.push_before_next_reply(&hex_bytes("5a 26 0b 08 ff ff 01"));
        dev.push_before_next_reply(&hex_bytes("5a 11 08 01 00 96 0b 00 00 80 3f"));
        assert_eq!(d.read_feature(feat("eq_62")).unwrap(), 3.0);
        let ev = drain(&rx);
        assert!(ev.contains(&Event::Changed(Change::Mask(Mask(0x01)))));
        assert!(ev.contains(&Event::Changed(Change::Feature {
            feature: feat("eq_31"),
            value: 1.0
        })));
        // The matched reply itself is not re-emitted.
        assert!(!ev.iter().any(
            |e| matches!(e, Event::Changed(Change::Feature { feature, .. }) if feature.name == "eq_62")
        ));
    }

    #[test]
    fn multi_entry_push_does_not_answer_a_feature_read() {
        let dev = FakeDevice::new();
        dev.set_feature(0x96, 0x09, 1.0);
        let (d, rx) = start(&dev);
        // The Scout-on push (`docs/protocol-g6.md` §1): Surround off and a
        // wrong "EQ off", arriving while EQ is being read.
        dev.push_before_next_reply(&hex_bytes(
            "5a 11 0e 02 00 96 00 00 00 00 00 96 09 00 00 00 00",
        ));
        assert_eq!(d.read_feature(feat("eq")).unwrap(), 1.0);
        let ev = drain(&rx);
        for name in ["surround", "eq"] {
            assert!(
                ev.contains(&Event::Changed(Change::Feature {
                    feature: feat(name),
                    value: 0.0
                })),
                "{name} push not dispatched: {ev:?}"
            );
        }
    }

    #[test]
    fn unsolicited_serial_reply_is_not_an_event() {
        let dev = FakeDevice::new();
        let (_d, rx) = start(&dev);
        dev.push_raw(&hex_bytes("5a 10 08 ef 67 74 00 00 00 00 00"));
        dev.push_raw(&hex_bytes("5a 10 02 ef 67"));
        assert_eq!(drain(&rx), []);
    }

    #[test]
    fn mask_filter_and_boost_writes() {
        let dev = FakeDevice::new();
        let (d, _rx) = start(&dev);
        let w = d.set_mask(MaskTarget::Sbx, true).unwrap();
        assert!(w.value.sbx());
        let w = d.set_mask(MaskTarget::Scout, true).unwrap();
        assert!(w.value.scout() && !w.value.sbx());
        assert!(!d.set_mask(MaskTarget::Scout, false).unwrap().value.scout());
        // Scout off: the device restores the SBX state from before (§6).
        assert_eq!(d.read_mask().unwrap(), Mask(Mask::SBX));

        assert_eq!(
            d.set_dac_filter(DacFilter::Nos).unwrap().value,
            DacFilter::Nos
        );
        assert_eq!(d.read_dac_filter().unwrap(), DacFilter::Nos);
        assert_eq!(
            d.set_mic_boost(MicBoost::Db10).unwrap().value,
            MicBoost::Db10
        );
        assert_eq!(d.read_mic_boost().unwrap(), MicBoost::Db10);
        assert_eq!(d.read_output().unwrap(), Output::Headphones);
    }

    #[test]
    fn output_write_survives_the_ack_in_the_middle_of_the_dumps() {
        let dev = FakeDevice::new();
        let (d, rx) = start(&dev);
        // Mode reply, profile dump, mask, unknown push, with the ACK between
        // the dump frames: none of them may be taken for the ACK or the reply.
        let w = d.set_output(Output::Speakers).unwrap();
        assert_eq!(w.value, Output::Speakers);
        assert_eq!(w.ack, Ack::Ok);
        assert_eq!(dev.output(), Output::Speakers);
        let sent: Vec<_> = dev
            .written()
            .into_iter()
            .filter(|f| f[1] == cmd::OUTPUT)
            .collect();
        assert_eq!(sent[0][..8], hex_bytes("5a 2c 05 00 02 00 00 00")[..]);
        assert_eq!(sent[1][..4], hex_bytes("5a 2c 01 01")[..]);

        let ev = drain(&rx);
        assert!(ev.contains(&Event::OutputSwitched(Output::Speakers)));
        assert!(ev.contains(&Event::Changed(Change::Mask(Mask(0)))));
        let per_output = features::FEATURES
            .iter()
            .filter(|f| features::per_output(f))
            .count();
        let dumped = ev
            .iter()
            .filter(|e| matches!(e, Event::Changed(Change::Feature { .. })))
            .count();
        assert_eq!(dumped, per_output, "every dumped feature became an event");
        assert_eq!(
            d.set_output(Output::Headphones).unwrap().value,
            Output::Headphones
        );
        assert_eq!(d.read_output().unwrap(), Output::Headphones);
    }

    #[test]
    fn output_write_that_is_not_applied_is_a_mismatch() {
        let dev = FakeDevice::new();
        dev.set_ignore_writes(true);
        let (d, _rx) = start(&dev);
        assert!(matches!(
            d.set_output(Output::Speakers),
            Err(Error::Mismatch { .. })
        ));
    }

    #[test]
    fn lighting_color_sends_the_static_mode_then_the_color() {
        let dev = FakeDevice::new();
        let (d, rx) = start(&dev);
        let w = d.set_lighting_color([0, 255, 0]).unwrap();
        assert_eq!((w.value, w.ack), ([0, 255, 0], Ack::Ok));
        assert_eq!(dev.lighting_color(), [0, 255, 0]);
        assert!(dev.lighting_enabled(), "a color does not switch the LED");
        let sent: Vec<_> = dev
            .written()
            .into_iter()
            .filter(|f| f[1] == cmd::LIGHTING)
            .collect();
        let heads: Vec<_> = sent.iter().map(|f| f[2] as usize + 3).collect();
        assert_eq!(heads, [9, 12, 9]);
        assert_eq!(sent[0][..9], hex_bytes("5a 3a 06 04 00 03 01 00 01")[..]);
        assert_eq!(
            sent[1][..12],
            hex_bytes("5a 3a 09 0a 00 03 01 01 ff 00 ff 00")[..]
        );
        assert_eq!(sent[2][..9], hex_bytes("5a 3a 06 0b 01 00 01 01 01")[..]);
        assert_eq!(
            wait_for(&rx, |e| matches!(
                e,
                Event::Changed(Change::LightingColor(_))
            )),
            Event::Changed(Change::LightingColor([0, 255, 0]))
        );
        assert_eq!(d.read_lighting_color().unwrap(), [0, 255, 0]);
        // The mode frame goes first on every color set.
        d.set_lighting_color([0, 0, 255]).unwrap();
        let n = writes_with_cmd(&dev, cmd::LIGHTING);
        assert_eq!(n, 3 + 1 + 3, "two sets and one read in between");
    }

    #[test]
    fn lighting_color_stops_at_a_rejected_mode_frame() {
        let dev = FakeDevice::new();
        dev.reject_next_write(0x83);
        let (d, _rx) = start(&dev);
        assert_eq!(
            d.set_lighting_color([1, 2, 3]),
            Err(Error::Rejected {
                cmd: cmd::LIGHTING,
                status: 0x83
            })
        );
        assert_eq!(writes_with_cmd(&dev, cmd::LIGHTING), 1, "no color frame");
        assert_eq!(dev.lighting_color(), [255, 0, 0]);
    }

    #[test]
    fn lighting_color_missing_first_ack_is_reported_and_read_back_decides() {
        let dev = FakeDevice::new();
        dev.set_drop_first_ack(true);
        let (d, _rx) = start(&dev);
        let w = d.set_lighting_color([9, 8, 7]).unwrap();
        assert_eq!(w.value, [9, 8, 7]);
        assert_eq!(w.ack, Ack::Missing { first_write: true });
    }

    #[test]
    fn lighting_enable_write_and_read() {
        let dev = FakeDevice::new();
        let (d, rx) = start(&dev);
        assert!(d.read_lighting_enabled().unwrap());
        assert!(!d.set_lighting_enabled(false).unwrap().value);
        assert!(!dev.lighting_enabled());
        assert!(!d.read_lighting_enabled().unwrap());
        assert_eq!(
            wait_for(&rx, |e| matches!(
                e,
                Event::Changed(Change::LightingEnabled(_))
            )),
            Event::Changed(Change::LightingEnabled(false))
        );
        dev.set_ignore_writes(true);
        assert!(matches!(
            d.set_lighting_enabled(true),
            Err(Error::Mismatch { .. })
        ));
    }

    #[test]
    fn lighting_read_is_not_answered_by_the_other_lighting_reply() {
        let dev = FakeDevice::new();
        let (d, _rx) = start(&dev);
        // `5a 3a` carries both replies: a pushed "LED off" ahead of the colour
        // reply, and a pushed colour ahead of the LED status.
        dev.push_before_next_reply(&hex_bytes("5a 3a 02 07 00"));
        assert_eq!(d.read_lighting_color().unwrap(), [255, 0, 0]);
        dev.push_before_next_reply(&hex_bytes("5a 3a 09 0b 01 00 01 01 ff 00 ff 00"));
        assert!(d.read_lighting_enabled().unwrap());
    }

    #[test]
    fn hw_direct_write_is_committed_and_read_back() {
        let dev = FakeDevice::new();
        let (d, rx) = start(&dev);
        assert!(!d.read_hw_direct().unwrap());
        let w = d.set_hw_direct(true).unwrap();
        assert_eq!((w.value, w.ack), (true, Ack::Ok));
        assert!(dev.hw_direct());
        let sent: Vec<_> = dev
            .written()
            .into_iter()
            .filter(|f| f[1] == cmd::HW_DIRECT)
            .collect();
        assert_eq!(sent[0][..4], hex_bytes("5a 39 01 01")[..], "plain read");
        assert_eq!(sent[1][..6], hex_bytes("5a 39 03 00 05 01")[..]);
        assert_eq!(sent[2][..4], hex_bytes("5a 39 01 01")[..], "commit");
        drain(&rx);
        // Off pushes a profile dump; it must not be taken for the reply.
        assert!(!d.set_hw_direct(false).unwrap().value);
        assert!(!dev.hw_direct());
        assert!(drain(&rx)
            .iter()
            .any(|e| matches!(e, Event::Changed(Change::Feature { .. }))));
    }

    #[test]
    fn write_read_back_is_dispatched_as_an_event() {
        // The device does not echo DAC filter / mic boost writes as a push;
        // the read-back is the only report of the new value.
        let dev = FakeDevice::new();
        let (d, rx) = start(&dev);
        d.set_dac_filter(DacFilter::Nos).unwrap();
        assert_eq!(
            wait_for(&rx, |e| matches!(e, Event::Changed(Change::DacFilter(_)))),
            Event::Changed(Change::DacFilter(DacFilter::Nos))
        );
        d.set_mic_boost(MicBoost::Db10).unwrap();
        assert_eq!(
            wait_for(&rx, |e| matches!(e, Event::Changed(Change::MicBoost(_)))),
            Event::Changed(Change::MicBoost(MicBoost::Db10))
        );
    }

    #[test]
    fn unplug_emits_disconnected_and_reconnects() {
        let dev = FakeDevice::new();
        let (d, rx) = start(&dev);
        dev.unplug();
        assert_eq!(
            wait_for(&rx, |e| *e == Event::Disconnected),
            Event::Disconnected
        );
        assert_eq!(d.read_output(), Err(Error::NotConnected));
        assert!(!d.is_connected());
        // First open after replug fails, like the real device right after plug-in.
        dev.fail_next_connects(1);
        dev.replug();
        assert_eq!(wait_for(&rx, |e| *e == Event::Connected), Event::Connected);
        assert_eq!(d.read_output().unwrap(), Output::Headphones);
        // writes_since_open resets: the first write after reconnect is "first" again.
        dev.set_drop_first_ack(true);
        dev.unplug();
        wait_for(&rx, |e| *e == Event::Disconnected);
        dev.replug();
        wait_for(&rx, |e| *e == Event::Connected);
        let w = d.write_feature(feat("bass"), 1.0).unwrap();
        assert_eq!(w.ack, Ack::Missing { first_write: true });
    }

    #[test]
    fn starts_while_unplugged() {
        let dev = FakeDevice::new();
        dev.unplug();
        let (d, rx) = Device::spawn(Box::new(dev.connector()), fast());
        assert_eq!(d.identity(), Err(Error::NotConnected));
        dev.replug();
        assert_eq!(next(&rx), Some(Event::Connected));
        assert!(d.identity().is_ok());
    }

    #[test]
    fn drop_stops_thread() {
        let dev = FakeDevice::new();
        let (d, rx) = start(&dev);
        drop(d);
        // Thread exited: event sender dropped.
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(1)),
            Err(RecvTimeoutError::Disconnected)
        ));
    }

    #[test]
    fn silent_device_times_out() {
        let dev = FakeDevice::new();
        let (d, _rx) = start(&dev);
        dev.set_silent(true);
        assert_eq!(d.read_mask(), Err(Error::Timeout { cmd: cmd::MASK }));
    }
}
