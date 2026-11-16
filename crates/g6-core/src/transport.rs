//! hidapi transport: open HID interface 4 only (usage page 0xFF00), write/read, retry.
//!
//! Device access goes through the [`Transport`] / [`Connector`] traits so the
//! event layer and tests can run against [`fake::FakeDevice`] instead of
//! hardware.
//!
//! **Interface 3 (consumer control / volume knob) is never opened**: opening it
//! on macOS stops the OS from receiving the knob until a USB replug (`docs/protocol-g6.md` §1).

use std::fmt;
use std::time::{Duration, Instant};

use crate::protocol::{self, Frame, Message, HID_WRITE_LEN, REPORT_LEN};

pub mod fake;

/// Creative vendor id.
pub const VENDOR_ID: u16 = 0x041e;
/// Sound BlasterX G6 product id.
pub const PRODUCT_ID: u16 = 0x3256;
/// The only HID interface we ever open.
pub const CONTROL_INTERFACE: i32 = 4;
/// Vendor usage page of the control interface.
pub const CONTROL_USAGE_PAGE: u16 = 0xFF00;
/// Consumer-control interface carrying the volume knob. Never opened.
pub const KNOB_INTERFACE: i32 = 3;

/// Transport-level error. Carries the real error text from hidapi.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    /// No G6 control interface (041e:3256, interface 4, usage page 0xFF00) found.
    NotFound,
    /// Error reported by hidapi (or the fake), with its message.
    Hid(String),
    /// hidapi wrote fewer bytes than a full report.
    ShortWrite { written: usize },
    /// The device did not answer the connect probe in time.
    ProbeTimeout,
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransportError::NotFound => write!(
                f,
                "G6 not found ({VENDOR_ID:04x}:{PRODUCT_ID:04x} interface {CONTROL_INTERFACE}, usage page {CONTROL_USAGE_PAGE:#06x})"
            ),
            TransportError::Hid(e) => write!(f, "HID error: {e}"),
            TransportError::ShortWrite { written } => {
                write!(f, "short HID write: {written} of {HID_WRITE_LEN} bytes")
            }
            TransportError::ProbeTimeout => write!(f, "device did not answer identity probe"),
        }
    }
}

impl std::error::Error for TransportError {}

impl From<hidapi::HidError> for TransportError {
    fn from(e: hidapi::HidError) -> Self {
        TransportError::Hid(e.to_string())
    }
}

/// An open connection to the G6 control interface.
pub trait Transport: Send {
    /// Write one 64-byte frame (the implementation adds report ID 0).
    fn write_frame(&mut self, frame: &Frame) -> Result<(), TransportError>;
    /// Read one frame; `Ok(None)` on timeout. Short reads are zero-padded.
    fn read_frame(&mut self, timeout: Duration) -> Result<Option<Frame>, TransportError>;
}

/// Opens [`Transport`]s; called again on every reconnect.
pub trait Connector: Send {
    fn connect(&mut self) -> Result<Box<dyn Transport>, TransportError>;
}

// ---------------------------------------------------------------------------
// Interface selection
// ---------------------------------------------------------------------------

/// The fields of an enumerated HID interface that decide whether we open it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HidCandidate {
    pub vendor_id: u16,
    pub product_id: u16,
    pub interface_number: i32,
    pub usage_page: u16,
}

/// `true` only for the G6 control interface: 041e:3256, interface 4, usage
/// page 0xFF00. Interface 3 is always refused, and an interface whose number
/// the platform does not report (`-1`) is refused too.
pub fn is_control_interface(c: &HidCandidate) -> bool {
    c.vendor_id == VENDOR_ID
        && c.product_id == PRODUCT_ID
        && c.interface_number != KNOB_INTERFACE
        && c.interface_number == CONTROL_INTERFACE
        && c.usage_page == CONTROL_USAGE_PAGE
}

// ---------------------------------------------------------------------------
// hidapi implementation
// ---------------------------------------------------------------------------

/// hidapi-backed transport for the G6 control interface.
pub struct HidTransport {
    dev: hidapi::HidDevice,
}

impl Transport for HidTransport {
    fn write_frame(&mut self, frame: &Frame) -> Result<(), TransportError> {
        let buf = protocol::to_hid_write(frame);
        let n = self.dev.write(&buf)?;
        // Some backends report the length without the report ID byte.
        if n < REPORT_LEN {
            return Err(TransportError::ShortWrite { written: n });
        }
        Ok(())
    }

    fn read_frame(&mut self, timeout: Duration) -> Result<Option<Frame>, TransportError> {
        let mut buf = [0u8; REPORT_LEN];
        let ms = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
        let n = self.dev.read_timeout(&mut buf, ms)?;
        Ok(if n == 0 { None } else { Some(buf) })
    }
}

/// Connector that enumerates with hidapi and opens only the control interface
/// by path (never `open(vid, pid)`, which may pick interface 3).
#[derive(Default)]
pub struct HidConnector {
    api: Option<hidapi::HidApi>,
}

impl HidConnector {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Connector for HidConnector {
    fn connect(&mut self) -> Result<Box<dyn Transport>, TransportError> {
        let api = match &mut self.api {
            Some(api) => {
                api.refresh_devices()?;
                api
            }
            None => self.api.insert(hidapi::HidApi::new()?),
        };
        let info = api
            .device_list()
            .find(|d| {
                is_control_interface(&HidCandidate {
                    vendor_id: d.vendor_id(),
                    product_id: d.product_id(),
                    interface_number: d.interface_number(),
                    usage_page: d.usage_page(),
                })
            })
            .ok_or(TransportError::NotFound)?;
        log::debug!(
            "opening G6 interface {} usage page {:#06x} at {:?}",
            info.interface_number(),
            info.usage_page(),
            info.path()
        );
        let dev = api.open_path(info.path())?;
        Ok(Box::new(HidTransport { dev }))
    }
}

// ---------------------------------------------------------------------------
// Connect with retry
// ---------------------------------------------------------------------------

/// Exponential backoff policy for connecting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    pub initial: Duration,
    pub max: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Backoff {
            initial: Duration::from_millis(100),
            max: Duration::from_secs(2),
        }
    }
}

impl Backoff {
    /// Infinite sequence of delays: `initial`, doubled each time, capped at `max`.
    pub fn delays(&self) -> impl Iterator<Item = Duration> {
        let max = self.max;
        std::iter::successors(Some(self.initial.min(max)), move |d| {
            Some(d.saturating_mul(2).min(max))
        })
    }
}

/// Open the device and check it answers an identity query. The first reads
/// right after plug-in have failed in testing (`docs/protocol-g6.md` §3 item 1), so a transport
/// is only returned once it has produced a valid reply. Other frames read
/// during the probe are dropped (callers read full state after connecting).
pub fn open_and_probe(
    connector: &mut dyn Connector,
    timeout: Duration,
) -> Result<Box<dyn Transport>, TransportError> {
    let mut t = connector.connect()?;
    t.write_frame(&protocol::identity())?;
    let deadline = Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(TransportError::ProbeTimeout);
        }
        if let Some(f) = t.read_frame(left)? {
            match protocol::decode(&f) {
                Message::Identity(_) => return Ok(t),
                other => log::debug!("probe: dropping {other:?}"),
            }
        }
    }
}

/// Blocking connect: [`open_and_probe`] up to `attempts` times with backoff,
/// logging each real error. Returns the last error if every attempt fails.
pub fn connect_with_retry(
    connector: &mut dyn Connector,
    backoff: Backoff,
    attempts: u32,
    probe_timeout: Duration,
) -> Result<Box<dyn Transport>, TransportError> {
    let mut delays = backoff.delays();
    let mut attempt = 0;
    loop {
        attempt += 1;
        match open_and_probe(connector, probe_timeout) {
            Ok(t) => return Ok(t),
            Err(e) => {
                log_connect_error(attempt, &e);
                if attempt >= attempts {
                    return Err(e);
                }
                std::thread::sleep(delays.next().unwrap_or(backoff.max));
            }
        }
    }
}

/// "Not found" is routine while unplugged; anything else is the real error we
/// want to see (`docs/protocol-g6.md` §3 item 1).
pub(crate) fn log_connect_error(attempt: u32, e: &TransportError) {
    if *e == TransportError::NotFound {
        log::debug!("connect attempt {attempt}: {e}");
    } else {
        log::warn!("connect attempt {attempt} failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::fake::FakeDevice;

    fn cand(iface: i32, page: u16) -> HidCandidate {
        HidCandidate {
            vendor_id: VENDOR_ID,
            product_id: PRODUCT_ID,
            interface_number: iface,
            usage_page: page,
        }
    }

    #[test]
    fn only_interface_4_vendor_page_is_selected() {
        assert!(is_control_interface(&cand(4, 0xFF00)));
        // Interface 3 (volume knob) never, whatever the usage page.
        assert!(!is_control_interface(&cand(3, 0xFF00)));
        assert!(!is_control_interface(&cand(3, 0x000C)));
        // Wrong usage page on interface 4, unknown interface number.
        assert!(!is_control_interface(&cand(4, 0x000C)));
        assert!(!is_control_interface(&cand(-1, 0xFF00)));
        // Other devices.
        let mut c = cand(4, 0xFF00);
        c.product_id = 0x3255;
        assert!(!is_control_interface(&c));
        c = cand(4, 0xFF00);
        c.vendor_id = 0x046d;
        assert!(!is_control_interface(&c));
    }

    #[test]
    fn backoff_doubles_and_caps() {
        let b = Backoff {
            initial: Duration::from_millis(100),
            max: Duration::from_millis(500),
        };
        let d: Vec<u64> = b.delays().take(5).map(|d| d.as_millis() as u64).collect();
        assert_eq!(d, [100, 200, 400, 500, 500]);
    }

    #[test]
    fn hid_write_is_65_bytes_with_report_id_0() {
        // What HidTransport hands to hidapi.
        let b = protocol::to_hid_write(&protocol::identity());
        assert_eq!(b.len(), HID_WRITE_LEN);
        assert_eq!(&b[..4], &[0x00, 0x5a, 0x05, 0x00]);
    }

    #[test]
    fn connect_retries_until_probe_succeeds() {
        let dev = FakeDevice::new();
        dev.fail_next_connects(2);
        let mut conn = dev.connector();
        let b = Backoff {
            initial: Duration::from_millis(1),
            max: Duration::from_millis(2),
        };
        let t = connect_with_retry(&mut conn, b, 5, Duration::from_millis(100));
        assert!(t.is_ok());
        assert_eq!(dev.connect_attempts(), 3);
    }

    #[test]
    fn connect_gives_up_with_real_error() {
        let dev = FakeDevice::new();
        dev.fail_next_connects(10);
        let mut conn = dev.connector();
        let b = Backoff {
            initial: Duration::from_millis(1),
            max: Duration::from_millis(1),
        };
        let e = connect_with_retry(&mut conn, b, 3, Duration::from_millis(50)).err();
        assert!(matches!(e, Some(TransportError::Hid(ref m)) if m.contains("simulated")));
        assert_eq!(dev.connect_attempts(), 3);
    }

    #[test]
    fn probe_times_out_when_device_is_silent() {
        let dev = FakeDevice::new();
        dev.set_silent(true);
        let mut conn = dev.connector();
        let e = open_and_probe(&mut conn, Duration::from_millis(20)).err();
        assert_eq!(e, Some(TransportError::ProbeTimeout));
    }
}
