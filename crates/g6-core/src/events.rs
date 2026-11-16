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
    self, cmd, DacFilter, Frame, Mask, MaskTarget, Message, MicBoost, Output, ProtocolError,
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
    /// A `5a 11` frame containing this family / id.
    Feature(u8, u8),
}

impl Expect {
    fn cmd(self) -> u8 {
        match self {
            Expect::Cmd(c) => c,
            Expect::Feature(..) => cmd::FEATURE_READ,
        }
    }

    fn matches(self, msg: &Message) -> bool {
        match (self, msg) {
            (_, Message::Ack { .. } | Message::Unknown(_)) => false,
            (Expect::Feature(fam, id), Message::Features(v)) => {
                v.iter().any(|e| e.family == fam && e.id == id)
            }
            (Expect::Feature(..), _) => false,
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
        frame: Frame,
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
        self.call(|reply| Cmd::Write {
            frame,
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
                frame,
                readback,
                expect,
                reply,
            } => {
                let r = self.do_write(&frame, &readback, expect);
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
                        // A multi-entry dump that happens to carry our id:
                        // the other entries are still news.
                        if let Message::Features(v) = &msg {
                            if v.len() > 1 {
                                self.dispatch_push(msg.clone());
                            }
                        }
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
        self.await_frame(self.config.reply_timeout, |m| expect.matches(m))?
            .ok_or(Error::Timeout { cmd: expect.cmd() })
    }

    fn do_write(
        &mut self,
        frame: &Frame,
        readback: &Frame,
        expect: Expect,
    ) -> Result<(Message, Ack), Error> {
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
        let ack = match ack {
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
        };
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
    use crate::transport::fake::{FakeDevice, FAKE_FIRMWARE};

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
