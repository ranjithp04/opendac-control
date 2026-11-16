//! A device plus its live [`DeviceState`], persisted last-known settings and
//! re-apply on reconnect (the project plan §4 M1 item 8).
//!
//! [`Session`] owns the [`Device`], consumes its events on a second thread,
//! keeps a shared [`DeviceState`] current and forwards every event to the
//! caller.
//!
//! **Last known settings.** Next to the live state the session keeps the
//! settings to restore after a replug, saved as `last_known.json`. Only
//! changes seen while the session runs go into it: device events (pushes,
//! hardware buttons, the echoes of writes) and differences found by
//! [`Session::refresh`]. A sync on connect that did not re-apply never
//! replaces it, so a one-shot `g6 status` after a replug that reset the
//! device does not overwrite the settings that are meant to be restored.
//!
//! **Re-apply.** When the device reconnects (USB replug; the G6 was seen to
//! come back with Surround off, the project plan §5 #2) the session reads the device,
//! compares it with the last known state and writes back only the
//! differences: SBX / Scout, the active output's playback features, the
//! mic / decoder features, DAC filter and mic boost. Then it reads the
//! device again and drops the events that were queued meanwhile (the device's
//! own startup dump and the echoes of our writes), so stale values can never
//! overwrite the remembered settings.
//!
//! The very first connect of a session does **not** re-apply by default
//! ([`SessionConfig::reapply_on_first_connect`]): hardware buttons may have
//! been used while no program was running, and a one-shot CLI call must not
//! undo that. The device is left as it is and the live state shows it, but
//! the last known settings are kept (they are used by the next re-apply).
//! Only when nothing was saved yet does the device become the last known
//! state.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::events::{Change, Config, Device, Error, Event};
use crate::features::{self, FEATURES};
use crate::protocol::MaskTarget;
use crate::state::DeviceState;
use crate::store::{self, Paths};
use crate::transport::Connector;

/// Session options.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Where `last_known.json` lives; `None` = do not persist (and nothing
    /// to re-apply after a restart).
    pub paths: Option<Paths>,
    /// Also re-apply the stored settings on the first connect.
    pub reapply_on_first_connect: bool,
}

impl Default for SessionConfig {
    fn default() -> Self {
        SessionConfig {
            paths: Paths::system(),
            reapply_on_first_connect: false,
        }
    }
}

/// Result of a re-apply.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reapplied {
    /// Device writes issued.
    pub writes: usize,
    /// Steps that failed (the device was still reachable).
    pub failures: Vec<String>,
}

fn fatal(e: &Error) -> bool {
    matches!(
        e,
        Error::NotConnected | Error::Transport(_) | Error::Shutdown
    )
}

/// Record a step of [`reapply`]: an `Ok` write is counted, a non-fatal error
/// is logged and listed, a fatal one is returned.
fn step(out: &mut Reapplied, what: String, r: Result<bool, Error>) -> Result<(), Error> {
    match r {
        Ok(wrote) => {
            out.writes += usize::from(wrote);
            Ok(())
        }
        Err(e) if fatal(&e) => Err(e),
        Err(e) => {
            log::warn!("re-apply {what}: {e}");
            out.failures.push(format!("{what}: {e}"));
            Ok(())
        }
    }
}

/// A device write as a [`step`] result.
fn wrote<T>(r: Result<T, Error>) -> Result<bool, Error> {
    r.map(|_| true)
}

/// Copy into `into` the values that differ between `before` and `after`
/// (the connection flag is ignored). Returns whether `into` changed.
fn merge_changes(into: &mut DeviceState, before: &DeviceState, after: &DeviceState) -> bool {
    fn take<T: PartialEq + Copy>(
        into: &mut Option<T>,
        before: Option<T>,
        after: Option<T>,
    ) -> bool {
        if after.is_some() && after != before && *into != after {
            *into = after;
            true
        } else {
            false
        }
    }
    let mut changed = take(&mut into.output, before.output, after.output);
    changed |= take(&mut into.mask, before.mask, after.mask);
    changed |= take(&mut into.dac_filter, before.dac_filter, after.dac_filter);
    changed |= take(&mut into.mic_boost, before.mic_boost, after.mic_boost);
    for (dst, b, a) in [
        (&mut into.headphones, &before.headphones, &after.headphones),
        (&mut into.speakers, &before.speakers, &after.speakers),
        (&mut into.common, &before.common, &after.common),
    ] {
        for (k, v) in &a.0 {
            if b.0.get(k) != Some(v) && dst.0.get(k) != Some(v) {
                dst.0.insert(k.clone(), *v);
                changed = true;
            }
        }
    }
    changed
}

/// Copy into `into` the values of `from` that `into` has none for yet
/// (e.g. a setting added to the state after `into` was saved). Values `into`
/// already has are kept.
fn fill_missing(into: &mut DeviceState, from: &DeviceState) {
    into.mask = into.mask.or(from.mask);
    into.dac_filter = into.dac_filter.or(from.dac_filter);
    into.mic_boost = into.mic_boost.or(from.mic_boost);
    for (dst, src) in [
        (&mut into.headphones, &from.headphones),
        (&mut into.speakers, &from.speakers),
        (&mut into.common, &from.common),
    ] {
        for (k, v) in &src.0 {
            dst.0.entry(k.clone()).or_insert(*v);
        }
    }
}

/// Write back what differs between `want` (last known) and the device.
///
/// `have` must be a fresh read of the device (`DeviceState::refresh`); it is
/// refreshed again if Scout had to be switched off (the device then restores
/// its own EQ / SBX state). Features are compared against the device, never
/// written blindly. When `want` has Scout on, the features Scout sets itself
/// ([`features::scout_owned`]: EQ, Smart Volume) are not written: the
/// remembered values are Scout's, and writing them before turning Scout on
/// would make the device keep the Scout curve as the user's EQ. Errors only
/// when the device is lost.
pub fn reapply(
    dev: &Device,
    want: &DeviceState,
    have: &mut DeviceState,
) -> Result<Reapplied, Error> {
    let mut out = Reapplied::default();

    // Scout off first: the device restores its previous EQ / SBX state.
    if let (Some(w), Some(h)) = (want.mask, have.mask) {
        if !w.scout() && h.scout() {
            step(
                &mut out,
                "scout off".into(),
                wrote(dev.set_mask(MaskTarget::Scout, false)),
            )?;
            step(
                &mut out,
                "re-read after scout off".into(),
                have.refresh(dev).map(|_| false),
            )?;
        }
    }
    if let (Some(w), Some(h)) = (want.mask, have.mask) {
        if w.sbx() != h.sbx() {
            step(
                &mut out,
                format!("sbx {}", w.sbx()),
                wrote(dev.set_mask(MaskTarget::Sbx, w.sbx())),
            )?;
        }
    }

    let scout_target = want.mask.is_some_and(|m| m.scout());
    for f in FEATURES
        .iter()
        .filter(|f| f.exposed && !(scout_target && features::scout_owned(f)))
    {
        // Per-output values belong to one output: the one that is active now.
        let w = if features::per_output(f) {
            have.output.and_then(|o| want.profile(o).get(f))
        } else {
            want.common.get(f)
        };
        let Some(w) = w else { continue };
        if have.feature(f) == Some(w) {
            continue;
        }
        step(&mut out, f.name.to_string(), wrote(dev.write_feature(f, w)))?;
    }

    // Scout on last: turning it on makes the device apply its own EQ curve.
    if let (Some(w), Some(_)) = (want.mask, have.mask) {
        if w.scout() {
            let r = match dev.read_mask() {
                Ok(m) if m.scout() => Ok(false),
                Ok(_) => wrote(dev.set_mask(MaskTarget::Scout, true)),
                Err(e) => Err(e),
            };
            step(&mut out, "scout on".into(), r)?;
        }
    }
    if let Some(w) = want.dac_filter {
        if have.dac_filter != Some(w) {
            step(
                &mut out,
                format!("dac filter {}", w.name()),
                wrote(dev.set_dac_filter(w)),
            )?;
        }
    }
    if let Some(w) = want.mic_boost {
        if have.mic_boost != Some(w) {
            step(
                &mut out,
                format!("mic boost {} dB", w.db()),
                wrote(dev.set_mic_boost(w)),
            )?;
        }
    }
    Ok(out)
}

struct Shared {
    /// Live state (what the device shows).
    state: Mutex<DeviceState>,
    /// Last known settings: what `last_known.json` holds and a re-apply
    /// restores. Lock order: `state` before `persisted`.
    persisted: Mutex<DeviceState>,
    changed: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, DeviceState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn persisted(&self) -> MutexGuard<'_, DeviceState> {
        self.persisted.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// Persist `state` (never as `connected`).
fn persist(paths: &Option<Paths>, state: &DeviceState) {
    let Some(paths) = paths else { return };
    let mut s = state.clone();
    s.connected = false;
    if let Err(e) = store::write_json(&paths.last_known_file(), &s) {
        log::warn!("could not save last known settings: {e}");
    }
}

/// Handle to a running session. Dropping it stops both threads.
pub struct Session {
    device: Arc<Device>,
    shared: Arc<Shared>,
    paths: Option<Paths>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Session {
    /// Start on the real G6 (hidapi, interface 4 only).
    pub fn open_hid(config: Config, session: SessionConfig) -> (Session, Receiver<Event>) {
        Self::start(
            Box::new(crate::transport::HidConnector::new()),
            config,
            session,
        )
    }

    /// Start with any connector (e.g. a fake device). The returned receiver
    /// carries every device event, after the session state has been updated;
    /// [`Event::Connected`] arrives once the state is synced (and re-applied).
    pub fn start(
        connector: Box<dyn Connector>,
        config: Config,
        session: SessionConfig,
    ) -> (Session, Receiver<Event>) {
        let mut initial = DeviceState::new();
        if let Some(p) = &session.paths {
            match store::read_json::<DeviceState>(&p.last_known_file()) {
                Ok(Some(s)) => initial = s,
                Ok(None) => {}
                Err(e) => log::warn!("ignoring last known settings: {e}"),
            }
        }
        initial.connected = false;

        let (device, events) = Device::spawn(connector, config);
        let device = Arc::new(device);
        let shared = Arc::new(Shared {
            state: Mutex::new(initial.clone()),
            persisted: Mutex::new(initial),
            changed: Condvar::new(),
        });
        let stop = Arc::new(AtomicBool::new(false));
        let (out, rx) = mpsc::channel();
        let worker = Worker {
            device: device.clone(),
            events,
            out,
            shared: shared.clone(),
            paths: session.paths.clone(),
            stop: stop.clone(),
            reapply_first: session.reapply_on_first_connect,
            seen_connect: false,
            scout_changed: false,
        };
        let thread = thread::Builder::new()
            .name("g6-session".into())
            .spawn(move || worker.run())
            .expect("spawn g6-session thread");
        (
            Session {
                device,
                shared,
                paths: session.paths,
                stop,
                thread: Some(thread),
            },
            rx,
        )
    }

    /// The device, for reads and writes. Changes show up in [`Session::state`]
    /// through the device's echo events; call [`Session::refresh`] to be sure.
    pub fn device(&self) -> &Device {
        &self.device
    }

    /// Copy of the current state.
    pub fn state(&self) -> DeviceState {
        self.shared.lock().clone()
    }

    /// Wait until the device is connected and its state synced.
    pub fn wait_connected(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut g = self.shared.lock();
        while !g.connected {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            g = self
                .shared
                .changed
                .wait_timeout(g, left)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
        true
    }

    /// Re-read the active output's settings from the device into the state.
    /// What changed since the last read is saved as last known settings.
    pub fn refresh(&self) -> Result<DeviceState, Error> {
        let mut s = self.state();
        s.refresh(&self.device)?;
        let mut g = self.shared.lock();
        // Keep the connection flag the event thread maintains.
        s.connected = g.connected;
        let save = {
            let mut p = self.shared.persisted();
            merge_changes(&mut p, &g, &s).then(|| p.clone())
        };
        *g = s.clone();
        drop(g);
        if let Some(p) = save {
            persist(&self.paths, &p);
        }
        Ok(s)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        // `device` drops after this (its thread is the last Arc holder).
    }
}

/// Sync attempts after a connect when reads fail without losing the device.
const SYNC_ATTEMPTS: u32 = 5;
const SYNC_RETRY_INITIAL: Duration = Duration::from_millis(100);

struct Worker {
    device: Arc<Device>,
    events: Receiver<Event>,
    out: Sender<Event>,
    shared: Arc<Shared>,
    paths: Option<Paths>,
    stop: Arc<AtomicBool>,
    reapply_first: bool,
    seen_connect: bool,
    /// Scout was switched since the last burst: re-read what Scout owns.
    scout_changed: bool,
}

impl Worker {
    fn run(mut self) {
        while !self.stop.load(Ordering::SeqCst) {
            let first = match self.events.recv_timeout(Duration::from_millis(50)) {
                Ok(e) => e,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return,
            };
            // Handle bursts (a dump is several events), then save once.
            let mut dirty = self.handle(first);
            while let Ok(e) = self.events.try_recv() {
                dirty |= self.handle(e);
            }
            if std::mem::take(&mut self.scout_changed) {
                dirty |= self.resync_scout();
            }
            if dirty {
                let s = self.shared.persisted().clone();
                persist(&self.paths, &s);
            }
        }
    }

    /// Process one event; returns whether the last known settings need
    /// saving.
    fn handle(&mut self, ev: Event) -> bool {
        if ev == Event::Connected {
            return self.on_connected();
        }
        let dirty = {
            let mut g = self.shared.lock();
            if let Event::Changed(Change::Mask(m)) = ev {
                if g.mask.is_some_and(|was| was.scout() != m.scout()) {
                    self.scout_changed = true;
                }
            }
            g.apply(&ev);
            // A setting the device reports while connected is a real change
            // (button, push, echo of a write): remember it.
            matches!(ev, Event::Changed(_) | Event::OutputSwitched(_))
                && self.shared.persisted().apply(&ev)
        };
        self.shared.changed.notify_all();
        let _ = self.out.send(ev);
        dirty
    }

    /// Scout was switched: re-read the settings Scout owns. When Scout turns
    /// on, the device's push reports EQ off and leaves out the Scout curve and
    /// Smart Volume (seen on hardware), so the pushes can't be trusted.
    /// Returns whether the last known settings need saving.
    fn resync_scout(&mut self) -> bool {
        let mut read = Vec::new();
        for f in FEATURES
            .iter()
            .filter(|f| f.exposed && features::scout_owned(f))
        {
            match self.device.read_feature(f) {
                Ok(value) => read.push(Event::Changed(Change::Feature { feature: f, value })),
                Err(e) if fatal(&e) => {
                    log::warn!("re-reading after a Scout change failed: {e}");
                    return false;
                }
                Err(e) => log::warn!("re-reading {} after a Scout change failed: {e}", f.name),
            }
        }
        // Scout-owned values pushed while reading are older than the reads.
        let mut dirty = false;
        while let Ok(e) = self.events.try_recv() {
            if let Event::Changed(Change::Feature { feature, .. }) = &e {
                if features::scout_owned(feature) {
                    log::debug!("ignoring pushed {} after a Scout change", feature.name);
                    continue;
                }
            }
            dirty |= self.handle(e);
        }
        for e in read {
            dirty |= self.handle(e);
        }
        dirty
    }

    /// Sleep up to `d`; false when the session is stopping.
    fn pause(&self, d: Duration) -> bool {
        let deadline = Instant::now() + d;
        while !self.stop.load(Ordering::SeqCst) {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return true;
            }
            thread::sleep(left.min(Duration::from_millis(20)));
        }
        false
    }

    /// Sync + re-apply after a connect (saves the state itself).
    fn on_connected(&mut self) -> bool {
        let mut first = !self.seen_connect;
        self.seen_connect = true;
        let mut attempt = 0;
        let mut backoff = SYNC_RETRY_INITIAL;
        loop {
            let intended = self.shared.persisted().clone();
            let reapplying = (!first || self.reapply_first) && intended.output.is_some();
            let mut fresh = intended.clone();
            let synced = (|| -> Result<(), Error> {
                let mut have = DeviceState::new();
                have.refresh(&self.device)?;
                if reapplying {
                    let r = reapply(&self.device, &intended, &mut have)?;
                    if r.writes > 0 || !r.failures.is_empty() {
                        log::info!(
                            "re-applied last known settings: {} writes, {} failures",
                            r.writes,
                            r.failures.len()
                        );
                    }
                }
                // The device is the truth now; the inactive profile stays.
                fresh.refresh(&self.device)
            })();
            if let Err(e) = synced {
                if fatal(&e) {
                    // The Disconnected event follows; the next Connected retries.
                    log::warn!("sync after connect failed: {e}");
                    return false;
                }
                // Still connected (e.g. a read timed out): no lifecycle
                // event will come to retry, so retry here.
                attempt += 1;
                if attempt >= SYNC_ATTEMPTS {
                    log::error!(
                        "sync after connect failed {attempt} times, giving up until the next reconnect: {e}"
                    );
                    return false;
                }
                log::warn!("sync after connect failed (attempt {attempt}), retrying: {e}");
                if !self.pause(backoff) {
                    return false;
                }
                backoff *= 2;
                continue;
            }

            // Events queued meanwhile are stale (startup dump, our own echoes).
            // They are forwarded but not applied; a lifecycle event decides.
            let mut forward = Vec::new();
            let mut last_life = None;
            while let Ok(e) = self.events.try_recv() {
                match e {
                    Event::Connected | Event::Disconnected => last_life = Some(e),
                    other => forward.push(other),
                }
            }
            if last_life == Some(Event::Disconnected) {
                self.shared.lock().connected = false;
                self.shared.changed.notify_all();
                let _ = self.out.send(Event::Disconnected);
                return false;
            }
            if last_life == Some(Event::Connected) {
                // Unplugged and back while we were busy: start over.
                first = false;
                continue;
            }

            // Save before announcing, so a reader of the file sees the sync.
            // Without a re-apply the device's values are not taken as the
            // settings to restore (a replug may have reset them), unless
            // nothing was known yet; only the active output (jack state) and
            // settings with no remembered value yet are.
            {
                let mut p = self.shared.persisted();
                if reapplying || intended.output.is_none() {
                    *p = fresh.clone();
                } else {
                    p.output = fresh.output;
                    fill_missing(&mut p, &fresh);
                }
                persist(&self.paths, &p);
            }
            {
                let mut g = self.shared.lock();
                fresh.connected = true;
                *g = fresh;
            }
            self.shared.changed.notify_all();
            let _ = self.out.send(Event::Connected);
            for e in forward {
                let _ = self.out.send(e);
            }
            return false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::{by_name, Feature};
    use crate::protocol::{cmd, DacFilter, Mask, MicBoost, Output};
    use crate::state::tests::fast;
    use crate::store::testutil::TempDir;
    use crate::transport::fake::FakeDevice;

    fn feat(name: &str) -> &'static Feature {
        by_name(name).unwrap()
    }

    fn set(fake: &FakeDevice, name: &str, v: f32) {
        let f = feat(name);
        fake.set_feature(f.family, f.id, v);
    }

    fn get(fake: &FakeDevice, name: &str) -> f32 {
        let f = feat(name);
        fake.feature(f.family, f.id).unwrap()
    }

    fn cfg(dir: &TempDir, reapply_first: bool) -> SessionConfig {
        SessionConfig {
            paths: Some(Paths::under(dir.path())),
            reapply_on_first_connect: reapply_first,
        }
    }

    fn wait_event(rx: &Receiver<Event>, pred: impl Fn(&Event) -> bool) -> Event {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let e = rx.recv_timeout(left).expect("event not received");
            if pred(&e) {
                return e;
            }
        }
    }

    fn start(fake: &FakeDevice, c: SessionConfig) -> (Session, Receiver<Event>) {
        let (s, rx) = Session::start(Box::new(fake.connector()), fast(), c);
        assert!(s.wait_connected(Duration::from_secs(3)));
        (s, rx)
    }

    fn feature_writes(fake: &FakeDevice) -> usize {
        fake.written()
            .iter()
            .filter(|f| f[1] == cmd::FEATURE_WRITE)
            .count()
    }

    #[test]
    fn state_is_synced_on_connect_and_follows_events() {
        let fake = FakeDevice::new();
        set(&fake, "surround", 1.0);
        let t = TempDir::new();
        let (s, rx) = start(&fake, cfg(&t, false));
        let st = s.state();
        assert_eq!(st.output, Some(Output::Headphones));
        assert_eq!(st.feature(feat("surround")), Some(1.0));
        assert!(st.connected);

        fake.press_mask(Mask::SBX);
        wait_event(&rx, |e| {
            matches!(e, Event::Changed(crate::events::Change::Mask(_)))
        });
        assert!(s.state().mask.unwrap().sbx());

        fake.switch_output(Output::Speakers);
        wait_event(&rx, |e| {
            matches!(e, Event::OutputSwitched(Output::Speakers))
        });
        let st = s.state();
        assert_eq!(st.output, Some(Output::Speakers));
        assert!(!st.headphones.is_empty(), "headphones profile kept");
    }

    #[test]
    fn scout_button_state_is_re_read_not_taken_from_the_push() {
        let fake = FakeDevice::new();
        set(&fake, "eq", 0.0);
        set(&fake, "eq_31", -1.0);
        let t = TempDir::new();
        let (s, rx) = start(&fake, cfg(&t, false));

        // The push says "EQ off" and nothing about the curve or Smart Volume.
        fake.press_scout(true);
        wait_event(&rx, |e| {
            matches!(e, Event::Changed(Change::Feature { feature, value })
                if feature.name == "smartvol_level" && *value == 0.9)
        });
        let st = s.state();
        assert!(st.mask.unwrap().scout());
        assert_eq!(st.feature(feat("eq")), Some(1.0));
        assert_eq!(st.feature(feat("eq_31")), Some(2.0));
        assert_eq!(st.feature(feat("smartvol")), Some(1.0));

        fake.press_scout(false);
        wait_event(&rx, |e| {
            matches!(e, Event::Changed(Change::Feature { feature, value })
                if feature.name == "eq_31" && *value == -1.0)
        });
        let st = s.state();
        assert!(!st.mask.unwrap().scout());
        assert_eq!(st.feature(feat("eq")), Some(0.0));
    }

    #[test]
    fn device_writes_reach_the_state_and_the_file() {
        let fake = FakeDevice::new();
        let t = TempDir::new();
        let (s, _rx) = start(&fake, cfg(&t, false));
        s.device().write_feature(feat("eq_2k"), 4.5).unwrap();
        let st = s.refresh().unwrap();
        assert_eq!(st.speakers.get(feat("eq_2k")), None);
        assert_eq!(st.headphones.get(feat("eq_2k")), Some(4.5));
        let on_disk: DeviceState = store::read_json(&Paths::under(t.path()).last_known_file())
            .unwrap()
            .unwrap();
        assert_eq!(on_disk.headphones.get(feat("eq_2k")), Some(4.5));
        assert!(!on_disk.connected);
    }

    #[test]
    fn reconnect_reapplies_what_the_device_forgot() {
        let fake = FakeDevice::new();
        let t = TempDir::new();
        let (s, rx) = start(&fake, cfg(&t, false));
        s.device().write_feature(feat("surround"), 1.0).unwrap();
        s.device()
            .write_feature(feat("surround_level"), 0.8)
            .unwrap();
        s.device().set_mask(MaskTarget::Sbx, true).unwrap();
        s.device().set_dac_filter(DacFilter::Nos).unwrap();
        s.device().set_mic_boost(MicBoost::Db10).unwrap();
        s.refresh().unwrap();

        // USB replug: the device comes back with defaults.
        fake.unplug();
        wait_event(&rx, |e| *e == Event::Disconnected);
        set(&fake, "surround", 0.0);
        set(&fake, "surround_level", 0.0);
        fake.press_mask(0);
        fake.replug();
        wait_event(&rx, |e| *e == Event::Connected);

        assert_eq!(get(&fake, "surround"), 1.0);
        assert_eq!(get(&fake, "surround_level"), 0.8);
        assert!(fake.mask() & Mask::SBX != 0);
        let st = s.state();
        assert!(st.connected);
        assert_eq!(st.feature(feat("surround")), Some(1.0));
        assert_eq!(st.dac_filter, Some(DacFilter::Nos));
        assert_eq!(st.mic_boost, Some(MicBoost::Db10));
    }

    #[test]
    fn reapply_writes_only_differences() {
        let fake = FakeDevice::new();
        let t = TempDir::new();
        let (s, rx) = start(&fake, cfg(&t, false));
        s.device().write_feature(feat("bass"), 1.0).unwrap();
        s.refresh().unwrap();
        let before = feature_writes(&fake);
        fake.unplug();
        wait_event(&rx, |e| *e == Event::Disconnected);
        fake.replug();
        wait_event(&rx, |e| *e == Event::Connected);
        assert_eq!(feature_writes(&fake), before, "nothing differed");
        assert_eq!(get(&fake, "bass"), 1.0);
    }

    #[test]
    fn first_connect_keeps_the_device_unless_asked_otherwise() {
        let fake = FakeDevice::new();
        let t = TempDir::new();
        // Remembered from an earlier run: Surround on.
        let mut old = DeviceState::new();
        old.output = Some(Output::Headphones);
        old.headphones.set(feat("surround"), 1.0);
        store::write_json(&Paths::under(t.path()).last_known_file(), &old).unwrap();

        // Default: the device is left alone (a button may have been used
        // meanwhile), but the remembered settings are not overwritten.
        let (s, _rx) = start(&fake, cfg(&t, false));
        assert_eq!(get(&fake, "surround"), 0.0);
        assert_eq!(s.state().feature(feat("surround")), Some(0.0));
        s.refresh().unwrap(); // e.g. `g6 status`
        let on_disk: DeviceState = store::read_json(&Paths::under(t.path()).last_known_file())
            .unwrap()
            .unwrap();
        assert_eq!(on_disk.headphones.get(feat("surround")), Some(1.0));
        drop(s);

        // Opt in: the remembered settings win.
        store::write_json(&Paths::under(t.path()).last_known_file(), &old).unwrap();
        let (s, _rx) = start(&fake, cfg(&t, true));
        assert_eq!(get(&fake, "surround"), 1.0);
        assert_eq!(s.state().feature(feat("surround")), Some(1.0));
    }

    #[test]
    fn one_shot_sessions_save_settings_not_remembered_yet() {
        let fake = FakeDevice::new();
        set(&fake, "mic_eq", 1.0);
        let t = TempDir::new();
        // Saved by an older run: Surround on, nothing about the mic.
        let mut old = DeviceState::new();
        old.output = Some(Output::Headphones);
        old.headphones.set(feat("surround"), 1.0);
        store::write_json(&Paths::under(t.path()).last_known_file(), &old).unwrap();

        let (s, _rx) = start(&fake, cfg(&t, false));
        drop(s);
        let saved = on_disk(&t);
        assert_eq!(saved.headphones.get(feat("mic_eq")), Some(1.0), "filled in");
        assert_eq!(saved.headphones.get(feat("surround")), Some(1.0), "kept");
    }

    fn on_disk(t: &TempDir) -> DeviceState {
        store::read_json(&Paths::under(t.path()).last_known_file())
            .unwrap()
            .unwrap()
    }

    #[test]
    fn one_shot_sessions_keep_settings_lost_in_a_replug() {
        let fake = FakeDevice::new();
        let t = TempDir::new();
        // Run 1: the user turns Surround on.
        let (s, _rx) = start(&fake, cfg(&t, false));
        s.device().write_feature(feat("surround"), 1.0).unwrap();
        s.refresh().unwrap();
        drop(s);
        assert_eq!(on_disk(&t).headphones.get(feat("surround")), Some(1.0));

        // USB replug while nothing runs: the device forgets Surround.
        set(&fake, "surround", 0.0);

        // Run 2 (`g6 status`, then a change of something else).
        let (s, rx) = start(&fake, cfg(&t, false));
        assert_eq!(s.state().feature(feat("surround")), Some(0.0));
        s.refresh().unwrap();
        s.device().write_feature(feat("bass"), 1.0).unwrap();
        wait_event(
            &rx,
            |e| matches!(e, Event::Changed(crate::events::Change::Feature { feature, .. }) if feature.name == "bass"),
        );
        s.refresh().unwrap();
        drop(s);
        let d = on_disk(&t);
        assert_eq!(d.headphones.get(feat("surround")), Some(1.0), "kept");
        assert_eq!(d.headphones.get(feat("bass")), Some(1.0), "change saved");

        // An explicit change of the forgotten setting itself is saved.
        let (s, rx) = start(&fake, cfg(&t, false));
        s.device().write_feature(feat("surround"), 0.0).unwrap();
        wait_event(
            &rx,
            |e| matches!(e, Event::Changed(crate::events::Change::Feature { feature, .. }) if feature.name == "surround"),
        );
        thread::sleep(Duration::from_millis(100));
        drop(s);
        assert_eq!(on_disk(&t).headphones.get(feat("surround")), Some(0.0));

        // A session that re-applies restores what was kept.
        set(&fake, "surround", 0.0);
        let mut d = on_disk(&t);
        d.headphones.set(feat("surround"), 1.0);
        store::write_json(&Paths::under(t.path()).last_known_file(), &d).unwrap();
        let (_s, _rx) = start(&fake, cfg(&t, true));
        assert_eq!(get(&fake, "surround"), 1.0);
        assert_eq!(get(&fake, "bass"), 1.0);
    }

    #[test]
    fn sync_retries_when_a_read_times_out_after_connect() {
        let fake = FakeDevice::new();
        set(&fake, "dialog", 1.0);
        // The first output read after connecting gets no answer.
        fake.drop_replies(cmd::OUTPUT, 1);
        let (s, rx) = Session::start(
            Box::new(fake.connector()),
            fast(),
            cfg(&TempDir::new(), false),
        );
        assert!(s.wait_connected(Duration::from_secs(3)));
        assert_eq!(s.state().feature(feat("dialog")), Some(1.0));
        wait_event(&rx, |e| *e == Event::Connected);
    }

    #[test]
    fn mask_filter_and_boost_timeouts_do_not_fail_the_sync() {
        let fake = FakeDevice::new();
        fake.drop_replies(cmd::MASK, 1);
        let (s, _rx) = start(&fake, cfg(&TempDir::new(), false));
        assert_eq!(s.state().output, Some(Output::Headphones));
    }

    #[test]
    fn reapply_with_scout_on_keeps_the_users_eq() {
        let fake = FakeDevice::new();
        // The user's own EQ with Scout off.
        set(&fake, "eq_1k", 5.0);
        let (dev, _rx) = crate::state::tests::start(&fake);
        // Remembered while Scout was on: Scout's curve and Smart Volume.
        let mut want = DeviceState::new();
        want.output = Some(Output::Headphones);
        want.mask = Some(Mask(Mask::SCOUT));
        want.headphones.set(feat("eq"), 1.0);
        want.headphones.set(feat("smartvol"), 1.0);
        want.headphones.set(feat("smartvol_level"), 0.9);
        for (i, v) in crate::transport::fake::SCOUT_CURVE.iter().enumerate() {
            want.headphones
                .set(crate::features::eq_band(i).unwrap(), *v);
        }
        want.headphones.set(feat("bass"), 1.0);
        let mut have = DeviceState::new();
        have.refresh(&dev).unwrap();
        let r = reapply(&dev, &want, &mut have).unwrap();
        assert_eq!(
            r,
            Reapplied {
                writes: 2,
                failures: vec![]
            },
            "bass + scout on"
        );
        assert_eq!(fake.mask() & Mask::SCOUT, Mask::SCOUT);
        dev.set_mask(MaskTarget::Scout, false).unwrap();
        assert_eq!(get(&fake, "eq_1k"), 5.0);
        assert_eq!(get(&fake, "eq"), 0.0);
        assert_eq!(get(&fake, "smartvol"), 0.0);
    }

    #[test]
    fn startup_dump_after_reconnect_does_not_overwrite_last_known() {
        let fake = FakeDevice::new();
        let t = TempDir::new();
        let (s, rx) = start(&fake, cfg(&t, false));
        s.device().write_feature(feat("dialog"), 1.0).unwrap();
        s.refresh().unwrap();
        fake.unplug();
        wait_event(&rx, |e| *e == Event::Disconnected);
        set(&fake, "dialog", 0.0);
        fake.replug();
        // The device announces its (reset) state right after connecting.
        fake.push_raw(&[0x5a, 0x11, 0x08, 0x01, 0x00, 0x96, 0x02, 0, 0, 0, 0]);
        wait_event(&rx, |e| *e == Event::Connected);
        thread::sleep(Duration::from_millis(100));
        assert_eq!(get(&fake, "dialog"), 1.0);
        assert_eq!(s.state().feature(feat("dialog")), Some(1.0));
        let on_disk: DeviceState = store::read_json(&Paths::under(t.path()).last_known_file())
            .unwrap()
            .unwrap();
        assert_eq!(on_disk.headphones.get(feat("dialog")), Some(1.0));
    }

    #[test]
    fn reapply_skips_features_the_user_may_not_write() {
        let fake = FakeDevice::new();
        let (dev, _rx) = crate::state::tests::start(&fake);
        let mut have = DeviceState::new();
        have.refresh(&dev).unwrap();
        let mut want = have.clone();
        want.headphones.set(feat("eq_preamp"), 5.0);
        want.headphones.set(feat("bass"), 1.0);
        let r = reapply(&dev, &want, &mut have).unwrap();
        assert_eq!(
            r,
            Reapplied {
                writes: 1,
                failures: vec![]
            }
        );
        assert_eq!(get(&fake, "eq_preamp"), 0.0);
        assert_eq!(get(&fake, "bass"), 1.0);
    }

    #[test]
    fn reapply_uses_the_profile_of_the_output_active_now() {
        let fake = FakeDevice::new();
        fake.switch_output(Output::Speakers);
        let (dev, _rx) = crate::state::tests::start(&fake);
        let mut have = DeviceState::new();
        have.refresh(&dev).unwrap();
        // Remembered while on headphones only.
        let mut want = DeviceState::new();
        want.output = Some(Output::Headphones);
        want.headphones.set(feat("surround"), 1.0);
        let r = reapply(&dev, &want, &mut have).unwrap();
        assert_eq!(r.writes, 0);
        assert_eq!(get(&fake, "surround"), 0.0);
    }

    #[test]
    fn reapply_reports_failures_and_continues() {
        let fake = FakeDevice::new();
        let (dev, _rx) = crate::state::tests::start(&fake);
        let mut have = DeviceState::new();
        have.refresh(&dev).unwrap();
        let mut want = have.clone();
        want.headphones.set(feat("bass"), 1.0);
        want.headphones.set(feat("eq_31"), 2.0);
        fake.reject_next_write(0x81);
        let r = reapply(&dev, &want, &mut have).unwrap();
        assert_eq!(r.failures.len(), 1, "{r:?}");
        assert_eq!(r.writes, 1);
    }

    #[test]
    fn reapply_restores_scout_state_in_order() {
        let fake = FakeDevice::new();
        fake.press_mask(Mask::SCOUT);
        let (dev, _rx) = crate::state::tests::start(&fake);
        let mut have = DeviceState::new();
        have.refresh(&dev).unwrap();
        let mut want = have.clone();
        want.mask = Some(Mask(Mask::SBX));
        want.headphones.set(feat("eq_31"), 3.0);
        reapply(&dev, &want, &mut have).unwrap();
        assert_eq!(fake.mask(), Mask::SBX);
        assert_eq!(get(&fake, "eq_31"), 3.0);
    }

    #[test]
    fn drop_stops_the_threads() {
        let fake = FakeDevice::new();
        let (s, rx) = Session::start(
            Box::new(fake.connector()),
            fast(),
            SessionConfig {
                paths: None,
                reapply_on_first_connect: false,
            },
        );
        assert!(s.wait_connected(Duration::from_secs(3)));
        drop(s);
        // Output channel is closed once the session is gone.
        assert!(wait_closed(&rx));
    }

    fn wait_closed(rx: &Receiver<Event>) -> bool {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Err(RecvTimeoutError::Disconnected) = rx.recv_timeout(Duration::from_millis(50))
            {
                return true;
            }
        }
        false
    }
}
