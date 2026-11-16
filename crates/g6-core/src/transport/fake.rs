//! In-memory G6 simulator implementing [`Transport`] / [`Connector`], for
//! tests and for later layers (state, CLI) that need a device without
//! hardware.
//!
//! It answers the §2.2 commands the way the real device does (replies, ACK
//! `5a 02 0a CMD STATUS`, `5a 11` push after a feature write) and can inject
//! pushes, unplug / replug, fail connects, drop the first ACK, reject writes
//! or ignore writes (to test read-back mismatches).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use super::{Connector, Transport, TransportError};
use crate::features::FEATURES;
use crate::protocol::{self, cmd, DacFilter, Frame, MicBoost, Output, REPORT_LEN};

/// Firmware string the fake reports (from the test unit, `docs/protocol-g6.md`).
pub const FAKE_FIRMWARE: &str = "2.1.250903.1324";

/// Feature values (by family / id) and the SBX bit saved when Scout went on.
type BeforeScout = (Vec<((u8, u8), f32)>, bool);

struct Sim {
    features: HashMap<(u8, u8), f32>,
    mask: u8,
    output: Output,
    filter: DacFilter,
    boost: MicBoost,
    /// Device → host frames waiting to be read.
    inbox: VecDeque<Frame>,
    /// Frames injected ahead of the reply to the next host write.
    before_next_reply: VecDeque<Frame>,
    /// Every frame the host wrote, in order.
    written: Vec<Frame>,
    plugged: bool,
    generation: u64,
    connect_attempts: u32,
    fail_connects: u32,
    drop_first_ack: bool,
    writes_since_open: u32,
    reject_next: Option<u8>,
    ignore_writes: bool,
    silent: bool,
    /// Don't answer the next N host frames with this command.
    drop_replies: Option<(u8, u32)>,
    /// EQ / Smart Volume / SBX as they were before Scout was turned on by a
    /// write; restored when a write turns Scout off (`docs/protocol-g6.md` §1).
    before_scout: Option<BeforeScout>,
}

struct Shared {
    sim: Mutex<Sim>,
    cv: Condvar,
}

/// Handle to a simulated G6. Cheap to clone; all clones share one device.
#[derive(Clone)]
pub struct FakeDevice {
    shared: Arc<Shared>,
}

impl Default for FakeDevice {
    fn default() -> Self {
        Self::new()
    }
}

fn enc(command: u8, payload: &[u8]) -> Frame {
    protocol::encode(command, payload).expect("fake payload fits")
}

fn feature_frame(entries: &[(u8, u8, f32)]) -> Frame {
    let mut p = vec![entries.len() as u8, 0x00];
    for (fam, id, v) in entries {
        p.extend_from_slice(&[*fam, *id]);
        p.extend_from_slice(&v.to_le_bytes());
    }
    enc(cmd::FEATURE_READ, &p)
}

fn mask_frame(mask: u8) -> Frame {
    // LEN 0x0b as captured: 08 ff ff MASK + 7 bytes.
    enc(cmd::MASK, &[0x08, 0xff, 0xff, mask, 0, 0, 0, 0, 0, 0, 0])
}

fn output_frame(out: Output) -> Frame {
    enc(cmd::OUTPUT, &[0x01, out.to_byte(), 0, 0, 0])
}

impl FakeDevice {
    /// A plugged-in G6 on headphones; every feature at 0 clamped to its range.
    pub fn new() -> Self {
        let features = FEATURES
            .iter()
            .map(|f| ((f.family, f.id), 0f32.clamp(f.min, f.max)))
            .collect();
        let sim = Sim {
            features,
            mask: 0,
            output: Output::Headphones,
            filter: DacFilter::FastMin,
            boost: MicBoost::Db30,
            inbox: VecDeque::new(),
            before_next_reply: VecDeque::new(),
            written: Vec::new(),
            plugged: true,
            generation: 0,
            connect_attempts: 0,
            fail_connects: 0,
            drop_first_ack: false,
            writes_since_open: 0,
            reject_next: None,
            ignore_writes: false,
            silent: false,
            drop_replies: None,
            before_scout: None,
        };
        FakeDevice {
            shared: Arc::new(Shared {
                sim: Mutex::new(sim),
                cv: Condvar::new(),
            }),
        }
    }

    fn with<R>(&self, f: impl FnOnce(&mut Sim) -> R) -> R {
        let mut g = self.lock();
        let r = f(&mut g);
        self.shared.cv.notify_all();
        r
    }

    fn lock(&self) -> MutexGuard<'_, Sim> {
        self.shared.sim.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Connector handing out transports to this device.
    pub fn connector(&self) -> FakeConnector {
        FakeConnector { dev: self.clone() }
    }

    // --- state access -----------------------------------------------------

    /// Raw stored value (no rounding).
    pub fn feature(&self, family: u8, id: u8) -> Option<f32> {
        self.with(|s| s.features.get(&(family, id)).copied())
    }
    /// Set a value silently (no push).
    pub fn set_feature(&self, family: u8, id: u8, value: f32) {
        self.with(|s| {
            s.features.insert((family, id), value);
        })
    }
    pub fn mask(&self) -> u8 {
        self.with(|s| s.mask)
    }
    pub fn output(&self) -> Output {
        self.with(|s| s.output)
    }
    pub fn dac_filter(&self) -> DacFilter {
        self.with(|s| s.filter)
    }
    pub fn mic_boost(&self) -> MicBoost {
        self.with(|s| s.boost)
    }
    /// Every frame written by the host so far.
    pub fn written(&self) -> Vec<Frame> {
        self.with(|s| s.written.clone())
    }
    pub fn connect_attempts(&self) -> u32 {
        self.with(|s| s.connect_attempts)
    }

    // --- behaviour knobs --------------------------------------------------

    /// The next `n` connect attempts fail with a simulated HID error.
    pub fn fail_next_connects(&self, n: u32) {
        self.with(|s| s.fail_connects = n)
    }
    /// Don't ACK the first write after each open (`docs/protocol-g6.md` §3 item 3).
    pub fn set_drop_first_ack(&self, on: bool) {
        self.with(|s| s.drop_first_ack = on)
    }
    /// ACK the next write with this status (e.g. `0x81`) and don't apply it.
    pub fn reject_next_write(&self, status: u8) {
        self.with(|s| s.reject_next = Some(status))
    }
    /// ACK writes with `00` but don't change any value.
    pub fn set_ignore_writes(&self, on: bool) {
        self.with(|s| s.ignore_writes = on)
    }
    /// Don't answer anything (writes are still recorded).
    pub fn set_silent(&self, on: bool) {
        self.with(|s| s.silent = on)
    }
    /// Don't answer the next `n` host frames with command `command`
    /// (requests time out; other commands are answered).
    pub fn drop_replies(&self, command: u8, n: u32) {
        self.with(|s| s.drop_replies = Some((command, n)))
    }

    // --- pushes -----------------------------------------------------------

    /// Queue a raw device → host frame (padded to 64 bytes).
    pub fn push_raw(&self, bytes: &[u8]) {
        let mut f = [0u8; REPORT_LEN];
        let n = bytes.len().min(REPORT_LEN);
        f[..n].copy_from_slice(&bytes[..n]);
        self.with(|s| s.inbox.push_back(f))
    }
    /// Queue a raw frame to be delivered just before the reply to the next
    /// host write (to test replies interleaved with pushes).
    pub fn push_before_next_reply(&self, bytes: &[u8]) {
        let mut f = [0u8; REPORT_LEN];
        let n = bytes.len().min(REPORT_LEN);
        f[..n].copy_from_slice(&bytes[..n]);
        self.with(|s| s.before_next_reply.push_back(f))
    }
    /// Headphone jack switch: `5a 2c` push followed by a dump of the
    /// per-output features (playback and mic) (the real device sends the new output's profile).
    pub fn switch_output(&self, out: Output) {
        self.with(|s| {
            s.output = out;
            s.inbox.push_back(output_frame(out));
            let entries: Vec<_> = FEATURES
                .iter()
                .filter(|f| crate::features::per_output(f))
                .map(|f| (f.family, f.id, s.features[&(f.family, f.id)]))
                .collect();
            for chunk in entries.chunks(8) {
                s.inbox.push_back(feature_frame(chunk));
            }
        })
    }
    /// Scout button, as seen on hardware: Scout on applies Scout's settings
    /// (`docs/protocol-g6.md` §1) but the push only carries the mask and "EQ off", not the
    /// Scout curve or Smart Volume; Scout off restores the previous state and
    /// pushes the mask and a dump of the playback features.
    pub fn press_scout(&self, on: bool) {
        self.with(|s| {
            let p = crate::features::FAMILY_PLAYBACK;
            if on {
                s.scout_on();
                s.mask = (s.mask | protocol::Mask::SCOUT) & !protocol::Mask::SBX;
                s.inbox.push_back(mask_frame(s.mask));
                let eq = crate::features::by_name("eq").expect("in table").id;
                s.inbox.push_back(feature_frame(&[(p, eq, 0.0)]));
            } else {
                s.scout_off();
                s.mask &= !protocol::Mask::SCOUT;
                s.inbox.push_back(mask_frame(s.mask));
                let entries: Vec<_> = FEATURES
                    .iter()
                    .filter(|f| f.family == p)
                    .map(|f| (f.family, f.id, s.features[&(f.family, f.id)]))
                    .collect();
                for chunk in entries.chunks(8) {
                    s.inbox.push_back(feature_frame(chunk));
                }
            }
        })
    }
    /// Hardware button changed the mask: `5a 26` push.
    pub fn press_mask(&self, mask: u8) {
        self.with(|s| {
            s.mask = mask;
            s.inbox.push_back(mask_frame(mask));
        })
    }

    // --- USB --------------------------------------------------------------

    /// Unplug: open transports start failing.
    pub fn unplug(&self) {
        self.with(|s| {
            s.plugged = false;
            s.generation += 1;
            s.inbox.clear();
            s.before_next_reply.clear();
        })
    }
    pub fn replug(&self) {
        self.with(|s| s.plugged = true)
    }
}

/// Scout Mode's EQ curve (`docs/protocol-g6.md` §1), 31 Hz … 16 kHz.
pub const SCOUT_CURVE: [f32; 10] = [2.0, 3.0, 3.0, -2.0, -2.0, 0.0, 3.0, 4.0, 3.0, 2.0];

impl Sim {
    /// Scout turned on by a write: remember EQ / Smart Volume / SBX, then
    /// apply Scout's own settings.
    fn scout_on(&mut self) {
        let owned: Vec<_> = FEATURES
            .iter()
            .filter(|f| crate::features::scout_owned(f))
            .map(|f| ((f.family, f.id), self.features[&(f.family, f.id)]))
            .collect();
        self.before_scout = Some((owned, self.mask & protocol::Mask::SBX != 0));
        let p = crate::features::FAMILY_PLAYBACK;
        let id = |name: &str| crate::features::by_name(name).expect("in table").id;
        self.features.insert((p, id("eq")), 1.0);
        self.features.insert((p, id("smartvol")), 1.0);
        self.features.insert((p, id("smartvol_level")), 0.9);
        for (i, v) in SCOUT_CURVE.iter().enumerate() {
            self.features
                .insert((p, crate::features::EQ_FIRST_ID + i as u8), *v);
        }
    }

    /// Scout turned off by a write: the device restores its previous EQ /
    /// SBX state (only known if Scout was turned on by a write).
    fn scout_off(&mut self) {
        if let Some((owned, sbx)) = self.before_scout.take() {
            self.features.extend(owned);
            if sbx {
                self.mask |= protocol::Mask::SBX;
            }
        }
    }

    /// ACK frame for a write, unless this is the first write after open and
    /// the fake is set to drop it.
    fn ack(&self, command: u8, status: u8) -> Option<Frame> {
        if self.writes_since_open == 1 && self.drop_first_ack {
            return None;
        }
        Some(enc(cmd::ACK, &[command, status]))
    }

    /// Count the write; returns the ACK status and whether to apply it.
    fn write_status(&mut self) -> (u8, bool) {
        self.writes_since_open += 1;
        match self.reject_next.take() {
            Some(st) => (st, false),
            None => (0x00, !self.ignore_writes),
        }
    }

    /// Handle one host frame; queue the device's answers after any frames
    /// already pending (and after frames from `push_before_next_reply`).
    fn handle(&mut self, f: &Frame) {
        self.written.push(*f);
        if self.silent || f[0] != protocol::MAGIC {
            return;
        }
        if let Some((c, n)) = self.drop_replies {
            if c == f[1] && n > 0 {
                self.drop_replies = (n > 1).then_some((c, n - 1));
                return;
            }
        }
        let len = (f[2] as usize).min(REPORT_LEN - 3);
        let p = &f[3..3 + len];
        let mut out: Vec<Frame> = self.before_next_reply.drain(..).collect();
        match (f[1], p) {
            (cmd::IDENTITY, []) => out.push(enc(cmd::IDENTITY, &[0x1f, 0, 0, 0])),
            (cmd::FIRMWARE, [0x02]) => {
                let mut s = FAKE_FIRMWARE.as_bytes().to_vec();
                s.push(0);
                out.push(enc(cmd::FIRMWARE, &s));
            }
            (cmd::HW_ID, []) => out.push(enc(cmd::HW_ID, &[0x97, 0, 0, 0])),
            (cmd::DSP_VERSION, []) => out.push(enc(cmd::DSP_VERSION, &[0x30, 0x01, 0x10, 0x00])),
            (cmd::FEATURE_READ, &[0x01, fam, id]) => {
                let v = self.features.get(&(fam, id)).copied().unwrap_or(0.0);
                out.push(feature_frame(&[(fam, id, v)]));
            }
            (cmd::FEATURE_WRITE, &[0x01, fam, id, a, b, c, d]) => {
                let (st, apply) = self.write_status();
                out.extend(self.ack(cmd::FEATURE_WRITE, st));
                if apply {
                    self.features
                        .insert((fam, id), f32::from_le_bytes([a, b, c, d]));
                }
                if st == 0 {
                    let v = self.features.get(&(fam, id)).copied().unwrap_or(0.0);
                    out.push(feature_frame(&[(fam, id, v)]));
                }
            }
            (cmd::MASK, [0x08, 0xff, 0xff]) => out.push(mask_frame(self.mask)),
            (cmd::MASK, &[0x07, bit, 0x00, on, 0x00]) => {
                let (st, apply) = self.write_status();
                if apply {
                    let scout = bit == protocol::Mask::SCOUT;
                    let was_scout = self.mask & protocol::Mask::SCOUT != 0;
                    if on != 0 {
                        if scout && !was_scout {
                            self.scout_on();
                        }
                        self.mask |= bit;
                        if scout {
                            self.mask &= !protocol::Mask::SBX;
                        }
                    } else {
                        self.mask &= !bit;
                        if scout && was_scout {
                            self.scout_off();
                        }
                    }
                }
                out.extend(self.ack(cmd::MASK, st));
                if st == 0 {
                    out.push(mask_frame(self.mask));
                }
            }
            (cmd::OUTPUT, [0x01]) => out.push(output_frame(self.output)),
            (cmd::DAC_FILTER, [0x01]) => {
                out.push(enc(cmd::DAC_FILTER, &[0x01, self.filter as u8, 0]))
            }
            (cmd::DAC_FILTER, &[0x00, flt, 0x00]) => {
                let (st, apply) = self.write_status();
                if let (true, Some(x)) = (apply, DacFilter::from_byte(flt)) {
                    self.filter = x;
                }
                out.extend(self.ack(cmd::DAC_FILTER, st));
            }
            (cmd::MIC_BOOST, [0x01, 0x00]) => {
                out.push(enc(cmd::MIC_BOOST, &[0x01, 0x00, 0x02, self.boost as u8]))
            }
            (cmd::MIC_BOOST, &[0x00, 0x00, 0x02, db]) => {
                let (st, apply) = self.write_status();
                if let (true, Some(x)) = (apply, MicBoost::from_byte(db)) {
                    self.boost = x;
                }
                out.extend(self.ack(cmd::MIC_BOOST, st));
            }
            _ => out.push(enc(cmd::ACK, &[f[1], 0x83])),
        }
        self.inbox.extend(out);
    }
}

/// Connector for a [`FakeDevice`].
pub struct FakeConnector {
    dev: FakeDevice,
}

impl Connector for FakeConnector {
    fn connect(&mut self) -> Result<Box<dyn Transport>, TransportError> {
        let generation = self.dev.with(|s| {
            s.connect_attempts += 1;
            if s.fail_connects > 0 {
                s.fail_connects -= 1;
                return Err(TransportError::Hid("simulated open failure".into()));
            }
            if !s.plugged {
                return Err(TransportError::NotFound);
            }
            s.writes_since_open = 0;
            s.inbox.clear();
            Ok(s.generation)
        })?;
        Ok(Box::new(FakeTransport {
            dev: self.dev.clone(),
            generation,
        }))
    }
}

/// Transport for a [`FakeDevice`]; fails once the device is unplugged.
pub struct FakeTransport {
    dev: FakeDevice,
    generation: u64,
}

fn gone() -> TransportError {
    TransportError::Hid("simulated device disconnected".into())
}

impl Transport for FakeTransport {
    fn write_frame(&mut self, frame: &Frame) -> Result<(), TransportError> {
        let generation = self.generation;
        self.dev.with(|s| {
            if !s.plugged || s.generation != generation {
                return Err(gone());
            }
            s.handle(frame);
            Ok(())
        })
    }

    fn read_frame(&mut self, timeout: Duration) -> Result<Option<Frame>, TransportError> {
        let deadline = Instant::now() + timeout;
        let mut g = self.dev.lock();
        loop {
            let s = &mut *g;
            if !s.plugged || s.generation != self.generation {
                return Err(gone());
            }
            if let Some(f) = s.inbox.pop_front() {
                return Ok(Some(f));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            g = self
                .dev
                .shared
                .cv
                .wait_timeout(g, left)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }
}
