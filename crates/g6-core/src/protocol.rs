//! G6 frame encoding / decoding (`docs/protocol-g6.md` §2.1, §2.2, §2.4).
//!
//! Frame: `5a CMD LEN PAYLOAD…`, zero-padded to 64 bytes. `LEN` = number of
//! payload bytes after `LEN`. Only commands listed in §2.2 can be built.

use std::fmt;

use crate::features::{self, Feature, FeatureError};

/// Size of a HID report (without report ID).
pub const REPORT_LEN: usize = 64;
/// Size of the hidapi write buffer (report ID `0x00` + report).
pub const HID_WRITE_LEN: usize = REPORT_LEN + 1;
/// First byte of every frame.
pub const MAGIC: u8 = 0x5a;
/// Maximum payload length (`64 - 3` header bytes).
pub const MAX_PAYLOAD: usize = REPORT_LEN - 3;

/// Command codes.
pub mod cmd {
    pub const ACK: u8 = 0x02;
    pub const IDENTITY: u8 = 0x05;
    pub const FIRMWARE: u8 = 0x07;
    pub const FEATURE_READ: u8 = 0x11;
    pub const FEATURE_WRITE: u8 = 0x12;
    pub const HW_ID: u8 = 0x20;
    pub const MASK: u8 = 0x26;
    pub const OUTPUT: u8 = 0x2c;
    pub const DSP_VERSION: u8 = 0x30;
    pub const MIC_BOOST: u8 = 0x3c;
    pub const DAC_FILTER: u8 = 0x6c;
}

/// One 64-byte frame.
pub type Frame = [u8; REPORT_LEN];

/// Errors from building frames.
#[derive(Debug, Clone, PartialEq)]
pub enum ProtocolError {
    PayloadTooLong(usize),
    Feature(FeatureError),
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProtocolError::PayloadTooLong(n) => {
                write!(f, "payload of {n} bytes exceeds {MAX_PAYLOAD}")
            }
            ProtocolError::Feature(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for ProtocolError {}

impl From<FeatureError> for ProtocolError {
    fn from(e: FeatureError) -> Self {
        ProtocolError::Feature(e)
    }
}

/// Encode `5a CMD LEN payload` zero-padded to 64 bytes.
pub fn encode(cmd: u8, payload: &[u8]) -> Result<Frame, ProtocolError> {
    if payload.len() > MAX_PAYLOAD {
        return Err(ProtocolError::PayloadTooLong(payload.len()));
    }
    let mut f = [0u8; REPORT_LEN];
    f[0] = MAGIC;
    f[1] = cmd;
    f[2] = payload.len() as u8;
    f[3..3 + payload.len()].copy_from_slice(payload);
    Ok(f)
}

/// Infallible encode for the fixed builders below (payloads are all short).
fn frame(cmd: u8, payload: &[u8]) -> Frame {
    debug_assert!(payload.len() <= MAX_PAYLOAD);
    encode(cmd, payload).expect("fixed payload fits")
}

/// hidapi write buffer: report ID `0x00` followed by the 64-byte frame.
pub fn to_hid_write(frame: &Frame) -> [u8; HID_WRITE_LEN] {
    let mut buf = [0u8; HID_WRITE_LEN];
    buf[1..].copy_from_slice(frame);
    buf
}

/// Headphone / speaker output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Output {
    Speakers,
    Headphones,
}

impl Output {
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            0x02 => Some(Output::Speakers),
            0x04 => Some(Output::Headphones),
            _ => None,
        }
    }

    pub fn to_byte(self) -> u8 {
        match self {
            Output::Speakers => 0x02,
            Output::Headphones => 0x04,
        }
    }
}

/// DAC filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DacFilter {
    FastMin = 1,
    SlowMin = 2,
    Nos = 3,
    FastLin = 4,
    SlowLin = 5,
}

impl DacFilter {
    pub const ALL: [DacFilter; 5] = [
        DacFilter::FastMin,
        DacFilter::SlowMin,
        DacFilter::Nos,
        DacFilter::FastLin,
        DacFilter::SlowLin,
    ];

    pub fn from_byte(b: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|f| *f as u8 == b)
    }

    /// CLI name (`fast-min`, `slow-min`, `nos`, `fast-lin`, `slow-lin`).
    pub fn name(self) -> &'static str {
        match self {
            DacFilter::FastMin => "fast-min",
            DacFilter::SlowMin => "slow-min",
            DacFilter::Nos => "nos",
            DacFilter::FastLin => "fast-lin",
            DacFilter::SlowLin => "slow-lin",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|f| f.name().eq_ignore_ascii_case(s))
    }
}

/// Mic boost in dB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MicBoost {
    Db0 = 0x00,
    Db10 = 0x0a,
    Db20 = 0x14,
    Db30 = 0x1e,
}

impl MicBoost {
    pub const ALL: [MicBoost; 4] = [
        MicBoost::Db0,
        MicBoost::Db10,
        MicBoost::Db20,
        MicBoost::Db30,
    ];

    pub fn from_byte(b: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|m| *m as u8 == b)
    }

    pub fn from_db(db: u8) -> Option<Self> {
        Self::from_byte(db)
    }

    pub fn db(self) -> u8 {
        self as u8
    }
}

/// Global mask (`5a 26` reply / push).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Mask(pub u8);

impl Mask {
    pub const SBX: u8 = 0x01;
    pub const SCOUT: u8 = 0x02;
    pub const VOICE_CLARITY: u8 = 0x04;

    pub fn sbx(self) -> bool {
        self.0 & Self::SBX != 0
    }
    pub fn scout(self) -> bool {
        self.0 & Self::SCOUT != 0
    }
    pub fn voice_clarity(self) -> bool {
        self.0 & Self::VOICE_CLARITY != 0
    }
}

/// Which mask bit to write (`5a 26 05 07 [01|02] …`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MaskTarget {
    Sbx = 0x01,
    Scout = 0x02,
}

// ---------------------------------------------------------------------------
// Command builders (§2.2)
// ---------------------------------------------------------------------------

/// `5a 05 00`
pub fn identity() -> Frame {
    frame(cmd::IDENTITY, &[])
}

/// `5a 07 01 02`
pub fn firmware() -> Frame {
    frame(cmd::FIRMWARE, &[0x02])
}

/// `5a 20 00`
pub fn hw_id() -> Frame {
    frame(cmd::HW_ID, &[])
}

/// `5a 30 00`
pub fn dsp_version() -> Frame {
    frame(cmd::DSP_VERSION, &[])
}

/// `5a 11 03 01 FAM ID` — only for features in the allow-list.
pub fn feature_read(family: u8, id: u8) -> Result<Frame, ProtocolError> {
    features::lookup(family, id).ok_or(FeatureError::Unknown { family, id })?;
    Ok(frame(cmd::FEATURE_READ, &[0x01, family, id]))
}

/// `5a 12 07 01 FAM ID f32le` — validates against the allow-list and
/// rounds the value. Returns the frame and the value actually sent.
pub fn feature_write(family: u8, id: u8, value: f32) -> Result<(Frame, f32), ProtocolError> {
    let feat = features::lookup(family, id).ok_or(FeatureError::Unknown { family, id })?;
    feature_write_for(feat, value)
}

/// Same as [`feature_write`] for a known table entry.
pub fn feature_write_for(feat: &Feature, value: f32) -> Result<(Frame, f32), ProtocolError> {
    // Guard against a hand-made `Feature` not in the table.
    let table = features::lookup(feat.family, feat.id).ok_or(FeatureError::Unknown {
        family: feat.family,
        id: feat.id,
    })?;
    let v = table.validate_write(value)?;
    let b = v.to_le_bytes();
    let f = frame(
        cmd::FEATURE_WRITE,
        &[0x01, table.family, table.id, b[0], b[1], b[2], b[3]],
    );
    Ok((f, v))
}

/// `5a 26 03 08 ff ff`
pub fn mask_read() -> Frame {
    frame(cmd::MASK, &[0x08, 0xff, 0xff])
}

/// `5a 26 05 07 TARGET 00 ON 00` (*documented*, not verified).
pub fn mask_write(target: MaskTarget, on: bool) -> Frame {
    frame(cmd::MASK, &[0x07, target as u8, 0x00, on as u8, 0x00])
}

/// SBX on/off.
pub fn sbx_write(on: bool) -> Frame {
    mask_write(MaskTarget::Sbx, on)
}

/// Scout Mode on/off.
pub fn scout_write(on: bool) -> Frame {
    mask_write(MaskTarget::Scout, on)
}

/// `5a 2c 01 01`
pub fn output_read() -> Frame {
    frame(cmd::OUTPUT, &[0x01])
}

/// `5a 6c 01 01`
pub fn dac_filter_read() -> Frame {
    frame(cmd::DAC_FILTER, &[0x01])
}

/// `5a 6c 03 00 FILTER 00` (*documented*).
pub fn dac_filter_write(filter: DacFilter) -> Frame {
    frame(cmd::DAC_FILTER, &[0x00, filter as u8, 0x00])
}

/// `5a 3c 02 01 00`
pub fn mic_boost_read() -> Frame {
    frame(cmd::MIC_BOOST, &[0x01, 0x00])
}

/// `5a 3c 04 00 00 02 DB` (*documented*).
pub fn mic_boost_write(boost: MicBoost) -> Frame {
    frame(cmd::MIC_BOOST, &[0x00, 0x00, 0x02, boost as u8])
}

// ---------------------------------------------------------------------------
// Decoder
// ---------------------------------------------------------------------------

/// ACK status byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckStatus {
    Ok,
    /// `80` / `81` / `83` or any other non-zero status.
    Rejected(u8),
}

impl AckStatus {
    pub fn from_byte(b: u8) -> Self {
        if b == 0 {
            AckStatus::Ok
        } else {
            AckStatus::Rejected(b)
        }
    }

    pub fn is_ok(self) -> bool {
        self == AckStatus::Ok
    }
}

/// One `FAM ID f32le` entry from a `5a 11` frame. The value is raw (with
/// device float noise); use [`FeatureValue::known`] for a rounded value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FeatureValue {
    pub family: u8,
    pub id: u8,
    pub value: f32,
}

impl FeatureValue {
    /// Table entry and rounded value, or `None` for undocumented ids
    /// (e.g. `96:70–72`, `95:0a–2d`) that must be ignored.
    pub fn known(&self) -> Option<(&'static Feature, f32)> {
        features::lookup(self.family, self.id).map(|f| (f, f.round(self.value)))
    }
}

/// A decoded incoming frame.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// `5a 02 0a CMD STATUS`
    Ack { cmd: u8, status: AckStatus },
    /// `5a 11 LEN COUNT 00 [FAM ID f32le]*COUNT` — feature read reply
    /// (COUNT = 1) or multi-entry push dump. Includes undocumented ids.
    Features(Vec<FeatureValue>),
    /// `5a 26 0b 08 ff ff MASK`
    Mask(Mask),
    /// `5a 2c 05 01 OUT`
    Output(Output),
    /// `5a 6c 03 01 FILTER 00`
    DacFilter(DacFilter),
    /// `5a 3c 04 01 00 02 DB`
    MicBoost(MicBoost),
    /// `5a 05 04 b0 b1 b2 b3`
    Identity(Vec<u8>),
    /// `5a 07 LEN ascii` (trailing NULs stripped)
    Firmware(String),
    /// `5a 20 04 …`
    HwId(Vec<u8>),
    /// `5a 30 04 …`
    DspVersion(Vec<u8>),
    /// Anything else (unknown pushes such as `5a 1a 02 02`, `5a 6e 02 01`,
    /// malformed frames). Holds the frame trimmed to `3 + LEN` bytes when
    /// possible.
    Unknown(Vec<u8>),
}

impl Message {
    /// Command code of the frame this message was decoded from (`None` for
    /// frames without the `5a` header).
    pub fn cmd(&self) -> Option<u8> {
        Some(match self {
            Message::Ack { .. } => cmd::ACK,
            Message::Features(_) => cmd::FEATURE_READ,
            Message::Mask(_) => cmd::MASK,
            Message::Output(_) => cmd::OUTPUT,
            Message::DacFilter(_) => cmd::DAC_FILTER,
            Message::MicBoost(_) => cmd::MIC_BOOST,
            Message::Identity(_) => cmd::IDENTITY,
            Message::Firmware(_) => cmd::FIRMWARE,
            Message::HwId(_) => cmd::HW_ID,
            Message::DspVersion(_) => cmd::DSP_VERSION,
            Message::Unknown(b) => {
                if b.len() >= 2 && b[0] == MAGIC {
                    b[1]
                } else {
                    return None;
                }
            }
        })
    }
}

/// Decode one incoming report (64 bytes, or shorter — padding is ignored).
pub fn decode(buf: &[u8]) -> Message {
    if buf.len() < 3 || buf[0] != MAGIC {
        return Message::Unknown(buf.to_vec());
    }
    let len = buf[2] as usize;
    if 3 + len > buf.len() {
        return Message::Unknown(buf.to_vec());
    }
    let p = &buf[3..3 + len];
    decode_payload(buf[1], p).unwrap_or_else(|| Message::Unknown(buf[..3 + len].to_vec()))
}

fn decode_payload(command: u8, p: &[u8]) -> Option<Message> {
    match command {
        cmd::ACK if p.len() >= 2 => Some(Message::Ack {
            cmd: p[0],
            status: AckStatus::from_byte(p[1]),
        }),
        cmd::FEATURE_READ => decode_features(p).map(Message::Features),
        cmd::MASK if p.len() >= 4 && p[..3] == [0x08, 0xff, 0xff] => {
            Some(Message::Mask(Mask(p[3])))
        }
        cmd::OUTPUT if p.len() >= 2 && p[0] == 0x01 => Output::from_byte(p[1]).map(Message::Output),
        cmd::DAC_FILTER if p.len() >= 2 && p[0] == 0x01 => {
            DacFilter::from_byte(p[1]).map(Message::DacFilter)
        }
        cmd::MIC_BOOST if p.len() >= 4 && p[..3] == [0x01, 0x00, 0x02] => {
            MicBoost::from_byte(p[3]).map(Message::MicBoost)
        }
        cmd::IDENTITY => Some(Message::Identity(p.to_vec())),
        cmd::FIRMWARE => {
            let s = String::from_utf8_lossy(p);
            Some(Message::Firmware(s.trim_matches('\0').to_string()))
        }
        cmd::HW_ID => Some(Message::HwId(p.to_vec())),
        cmd::DSP_VERSION => Some(Message::DspVersion(p.to_vec())),
        _ => None,
    }
}

fn decode_features(p: &[u8]) -> Option<Vec<FeatureValue>> {
    if p.len() < 2 || p[1] != 0x00 {
        return None;
    }
    let count = p[0] as usize;
    let entries = &p[2..];
    if entries.len() < count * 6 {
        return None;
    }
    Some(
        entries
            .as_chunks::<6>()
            .0
            .iter()
            .take(count)
            .map(|e| FeatureValue {
                family: e[0],
                id: e[1],
                value: f32::from_le_bytes([e[2], e[3], e[4], e[5]]),
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace()
            .map(|b| u8::from_str_radix(b, 16).unwrap())
            .collect()
    }

    /// Pad a capture to a full 64-byte report, as read from the device.
    fn report(s: &str) -> Frame {
        let v = hex(s);
        let mut f = [0u8; REPORT_LEN];
        f[..v.len()].copy_from_slice(&v);
        f
    }

    fn head(f: &Frame, s: &str) {
        let v = hex(s);
        assert_eq!(&f[..v.len()], &v[..]);
        assert!(f[v.len()..].iter().all(|b| *b == 0), "not zero-padded");
    }

    // Captures from `docs/protocol-g6.md` §2.4, verbatim.
    const CAP_OUTPUT: &str = "5a 2c 05 01 02";
    const CAP_MASK: &str = "5a 26 0b 08 ff ff 05";
    const CAP_DUMP4: &str =
        "5a 11 1a 04 00 96 13 02 00 80 3f 96 14 03 00 00 3f 96 09 00 00 00 00 95 04 00 00 80 3f";
    const CAP_DUMP8: &str = "5a 11 32 08 00 96 0b fd ff 7f bf 96 0c 00 00 40 34 96 0d 03 00 00 3f 96 0e 02 00 80 3f 96 0f 02 00 80 3f 96 10 02 00 c0 3f 96 11 02 00 80 3f 96 12 02 00 c0 3f";

    #[test]
    fn encode_pads_and_sets_len() {
        let f = encode(0x11, &[1, 2, 3]).unwrap();
        head(&f, "5a 11 03 01 02 03");
        assert_eq!(f.len(), 64);
        assert!(encode(0x11, &[0; MAX_PAYLOAD]).is_ok());
        assert_eq!(
            encode(0x11, &[0; MAX_PAYLOAD + 1]),
            Err(ProtocolError::PayloadTooLong(62))
        );
    }

    #[test]
    fn hid_write_buffer_has_report_id_zero() {
        let f = identity();
        let b = to_hid_write(&f);
        assert_eq!(b.len(), 65);
        assert_eq!(b[0], 0x00);
        assert_eq!(&b[1..], &f[..]);
    }

    #[test]
    fn builders_match_plan_7_2() {
        head(&identity(), "5a 05 00");
        head(&firmware(), "5a 07 01 02");
        head(&hw_id(), "5a 20 00");
        head(&dsp_version(), "5a 30 00");
        head(&feature_read(0x96, 0x09).unwrap(), "5a 11 03 01 96 09");
        head(&mask_read(), "5a 26 03 08 ff ff");
        head(&sbx_write(true), "5a 26 05 07 01 00 01 00");
        head(&sbx_write(false), "5a 26 05 07 01 00 00 00");
        head(&scout_write(true), "5a 26 05 07 02 00 01 00");
        head(&scout_write(false), "5a 26 05 07 02 00 00 00");
        head(&output_read(), "5a 2c 01 01");
        head(&dac_filter_read(), "5a 6c 01 01");
        head(&dac_filter_write(DacFilter::Nos), "5a 6c 03 00 03 00");
        head(&dac_filter_write(DacFilter::SlowLin), "5a 6c 03 00 05 00");
        head(&mic_boost_read(), "5a 3c 02 01 00");
        head(&mic_boost_write(MicBoost::Db30), "5a 3c 04 00 00 02 1e");
        head(&mic_boost_write(MicBoost::Db0), "5a 3c 04 00 00 02 00");
    }

    #[test]
    fn feature_write_encodes_f32le() {
        let (f, v) = feature_write(0x96, 0x09, 1.0).unwrap();
        assert_eq!(v, 1.0);
        head(&f, "5a 12 07 01 96 09 00 00 80 3f");
        let (f, v) = feature_write(0x96, 0x0b, -1.2).unwrap();
        assert_eq!(v, -1.0);
        head(&f, "5a 12 07 01 96 0b 00 00 80 bf");
        let (f, _) = feature_write(0x96, 0x17, 150.0).unwrap();
        head(&f, "5a 12 07 01 96 17 00 00 16 43");
    }

    #[test]
    fn feature_write_rejects_disallowed() {
        assert!(matches!(
            feature_write(0x96, 0x70, 0.0),
            Err(ProtocolError::Feature(FeatureError::Unknown { .. }))
        ));
        assert!(matches!(
            feature_write(0x96, 0x0a, 6.5),
            Err(ProtocolError::Feature(FeatureError::OutOfRange { .. }))
        ));
        assert!(matches!(
            feature_write(0x96, 0x0b, 12.5),
            Err(ProtocolError::Feature(FeatureError::OutOfRange { .. }))
        ));
        assert!(feature_read(0x95, 0x0a).is_err());
        // The pre-amp can be read and written.
        assert!(feature_read(0x96, 0x0a).is_ok());
    }

    #[test]
    fn decode_capture_output() {
        assert_eq!(
            decode(&report(CAP_OUTPUT)),
            Message::Output(Output::Speakers)
        );
        assert_eq!(
            decode(&report("5a 2c 05 01 04")),
            Message::Output(Output::Headphones)
        );
    }

    #[test]
    fn decode_capture_mask() {
        let m = decode(&report(CAP_MASK));
        let Message::Mask(mask) = m else {
            panic!("{m:?}")
        };
        assert_eq!(mask, Mask(0x05));
        assert!(mask.sbx());
        assert!(!mask.scout());
        assert!(mask.voice_clarity());
    }

    #[test]
    fn decode_capture_dump4() {
        let m = decode(&report(CAP_DUMP4));
        let Message::Features(v) = m else {
            panic!("{m:?}")
        };
        let got: Vec<(u8, u8, f32)> = v
            .iter()
            .map(|e| {
                let (f, val) = e.known().unwrap();
                (f.family, f.id, val)
            })
            .collect();
        assert_eq!(
            got,
            vec![
                (0x96, 0x13, 1.0),
                (0x96, 0x14, 0.5),
                (0x96, 0x09, 0.0),
                (0x95, 0x04, 1.0),
            ]
        );
        // Raw values carry device noise.
        assert_eq!(v[0].value, f32::from_bits(0x3f80_0002));
    }

    #[test]
    fn decode_capture_dump8() {
        let m = decode(&report(CAP_DUMP8));
        let Message::Features(v) = m else {
            panic!("{m:?}")
        };
        assert_eq!(v.len(), 8);
        let vals: Vec<f32> = v.iter().map(|e| e.known().unwrap().1).collect();
        assert_eq!(vals, vec![-1.0, 0.0, 0.5, 1.0, 1.0, 1.5, 1.0, 1.5]);
        let ids: Vec<u8> = v.iter().map(|e| e.id).collect();
        assert_eq!(ids, (0x0b..=0x12).collect::<Vec<u8>>());
        assert!(v.iter().all(|e| e.family == 0x96));
    }

    #[test]
    fn decode_unpadded_capture() {
        // Captures decode the same without padding.
        assert!(matches!(decode(&hex(CAP_DUMP8)), Message::Features(v) if v.len() == 8));
    }

    #[test]
    fn decode_feature_read_reply() {
        let m = decode(&report("5a 11 08 01 00 96 09 00 00 80 3f"));
        assert_eq!(
            m,
            Message::Features(vec![FeatureValue {
                family: 0x96,
                id: 0x09,
                value: 1.0
            }])
        );
    }

    #[test]
    fn undocumented_ids_are_kept_raw_but_not_known() {
        let m = decode(&report(
            "5a 11 0e 02 00 96 70 00 00 80 3f 95 0a 00 00 00 00",
        ));
        let Message::Features(v) = m else {
            panic!("{m:?}")
        };
        assert_eq!(v.len(), 2);
        assert!(v.iter().all(|e| e.known().is_none()));
    }

    #[test]
    fn decode_ack() {
        assert_eq!(
            decode(&report("5a 02 0a 12 00")),
            Message::Ack {
                cmd: 0x12,
                status: AckStatus::Ok
            }
        );
        for s in [0x80u8, 0x81, 0x83] {
            let m = decode(&report(&format!("5a 02 0a 12 {s:02x}")));
            assert_eq!(
                m,
                Message::Ack {
                    cmd: 0x12,
                    status: AckStatus::Rejected(s)
                }
            );
        }
        assert!(AckStatus::Ok.is_ok());
        assert!(!AckStatus::Rejected(0x80).is_ok());
    }

    #[test]
    fn decode_identity_replies() {
        assert_eq!(
            decode(&report("5a 05 04 1f 00 00 00")),
            Message::Identity(vec![0x1f, 0, 0, 0])
        );
        assert_eq!(
            decode(&report("5a 30 04 30 01 10 00")),
            Message::DspVersion(vec![0x30, 0x01, 0x10, 0x00])
        );
        assert!(matches!(
            decode(&report("5a 20 04 97 00 00 00")),
            Message::HwId(v) if v[0] == 0x97
        ));
        let mut fw = vec![0x5a, 0x07, 16];
        fw.extend_from_slice(b"2.1.250903.1324\0");
        assert_eq!(
            decode(&fw),
            Message::Firmware("2.1.250903.1324".to_string())
        );
    }

    #[test]
    fn decode_dac_filter_and_mic_boost() {
        assert_eq!(
            decode(&report("5a 6c 03 01 01 00")),
            Message::DacFilter(DacFilter::FastMin)
        );
        assert_eq!(
            decode(&report("5a 3c 04 01 00 02 1e")),
            Message::MicBoost(MicBoost::Db30)
        );
        assert!(matches!(
            decode(&report("5a 6c 03 01 09 00")),
            Message::Unknown(_)
        ));
    }

    #[test]
    fn decode_unknown_pushes() {
        assert_eq!(
            decode(&report("5a 1a 02 02")),
            Message::Unknown(hex("5a 1a 02 02 00"))
        );
        let m = decode(&report("5a 6e 02 01"));
        assert!(matches!(m, Message::Unknown(_)));
        assert_eq!(m.cmd(), Some(0x6e));
        // Garbage / truncated frames.
        assert!(matches!(decode(&[]), Message::Unknown(_)));
        assert!(matches!(decode(&report("00 11 00")), Message::Unknown(_)));
        assert!(matches!(
            decode(&hex("5a 11 32 08 00")),
            Message::Unknown(_)
        ));
        // COUNT larger than LEN allows.
        assert!(matches!(
            decode(&report("5a 11 08 02 00 96 09 00 00 80 3f")),
            Message::Unknown(_)
        ));
    }

    #[test]
    fn enum_names() {
        for f in DacFilter::ALL {
            assert_eq!(DacFilter::from_name(f.name()), Some(f));
            assert_eq!(DacFilter::from_byte(f as u8), Some(f));
        }
        assert_eq!(MicBoost::from_db(20), Some(MicBoost::Db20));
        assert_eq!(MicBoost::from_db(15), None);
        assert_eq!(
            Output::from_byte(Output::Headphones.to_byte()),
            Some(Output::Headphones)
        );
    }
}
