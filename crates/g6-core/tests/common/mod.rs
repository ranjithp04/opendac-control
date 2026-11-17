//! Helpers shared by the integration tests. Public API of `g6-core` only.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use g6_core::events::{Config, Device, Event};
use g6_core::features::{self, Feature};
use g6_core::protocol::{Frame, REPORT_LEN};
use g6_core::transport::fake::FakeDevice;
use g6_core::transport::Backoff;

/// Unique temp directory, removed on drop.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "g6-it-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        // Left over from an earlier run with the same pid: start empty.
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Default for TempDir {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `"5a 05 00"` -> bytes.
pub fn hex(s: &str) -> Vec<u8> {
    s.split_whitespace()
        .map(|b| u8::from_str_radix(b, 16).unwrap())
        .collect()
}

/// A capture padded to a full 64-byte report, as read from the device.
pub fn report(s: &str) -> Frame {
    let v = hex(s);
    let mut f = [0u8; REPORT_LEN];
    f[..v.len()].copy_from_slice(&v);
    f
}

/// Assert `frame` is exactly `expected` followed by zero padding.
#[track_caller]
pub fn assert_frame(frame: &Frame, expected: &str) {
    let v = hex(expected);
    assert_eq!(
        &frame[..v.len()],
        &v[..],
        "frame bytes differ from `{expected}`"
    );
    assert!(
        frame[v.len()..].iter().all(|b| *b == 0),
        "frame is not zero-padded after `{expected}`: {frame:02x?}"
    );
}

/// Short timeouts so the fake-device tests stay fast.
pub fn fast() -> Config {
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

/// Spawn a `Device` on the fake and wait for `Event::Connected`.
pub fn start(fake: &FakeDevice) -> (Device, Receiver<Event>) {
    let (dev, rx) = Device::spawn(Box::new(fake.connector()), fast());
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(2)),
        Ok(Event::Connected),
        "fake device did not connect"
    );
    (dev, rx)
}

pub fn feat(name: &str) -> &'static Feature {
    features::by_name(name).unwrap_or_else(|| panic!("no feature named {name}"))
}

/// Raw value stored in the fake.
pub fn get(fake: &FakeDevice, name: &str) -> f32 {
    let f = feat(name);
    fake.feature(f.family, f.id).unwrap()
}

/// Set a value in the fake without a push.
pub fn set(fake: &FakeDevice, name: &str, v: f32) {
    let f = feat(name);
    fake.set_feature(f.family, f.id, v);
}

/// Every table feature's stored value in the fake, by name.
pub fn snapshot(fake: &FakeDevice) -> Vec<(&'static str, f32)> {
    features::FEATURES
        .iter()
        .map(|f| (f.name, fake.feature(f.family, f.id).unwrap()))
        .collect()
}

/// Is this host frame a state-changing write (as opposed to a read)?
pub fn is_write(f: &Frame) -> bool {
    match f[1] {
        0x12 => true,
        0x26 => f[3] == 0x07,
        0x6c | 0x3c => f[3] == 0x00,
        _ => false,
    }
}

/// Only the writes among the frames the host has sent.
pub fn writes(fake: &FakeDevice) -> Vec<Frame> {
    fake.written().into_iter().filter(is_write).collect()
}
