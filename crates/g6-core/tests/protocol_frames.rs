//! Frame encoding and decoding against `docs/protocol-g6.md` §2.2 and §2.4. No device.

mod common;

use common::*;
use g6_core::features::{Kind, FEATURES};
use g6_core::protocol::*;

// ---------------------------------------------------------------------------
// §2.2 builders: exact bytes, LEN, zero padding, report-ID prefix
// ---------------------------------------------------------------------------

/// Every fixed builder with the bytes `docs/protocol-g6.md` §2.2 gives for it.
fn builder_table() -> Vec<(&'static str, Frame, &'static str)> {
    vec![
        ("identity", identity(), "5a 05 00"),
        ("firmware", firmware(), "5a 07 01 02"),
        ("hw_id", hw_id(), "5a 20 00"),
        ("dsp_version", dsp_version(), "5a 30 00"),
        ("mask_read", mask_read(), "5a 26 03 08 ff ff"),
        ("sbx on", sbx_write(true), "5a 26 05 07 01 00 01 00"),
        ("sbx off", sbx_write(false), "5a 26 05 07 01 00 00 00"),
        ("scout on", scout_write(true), "5a 26 05 07 02 00 01 00"),
        ("scout off", scout_write(false), "5a 26 05 07 02 00 00 00"),
        ("output_read", output_read(), "5a 2c 01 01"),
        ("dac_filter_read", dac_filter_read(), "5a 6c 01 01"),
        (
            "dac fast-min",
            dac_filter_write(DacFilter::FastMin),
            "5a 6c 03 00 01 00",
        ),
        (
            "dac slow-min",
            dac_filter_write(DacFilter::SlowMin),
            "5a 6c 03 00 02 00",
        ),
        (
            "dac nos",
            dac_filter_write(DacFilter::Nos),
            "5a 6c 03 00 03 00",
        ),
        (
            "dac fast-lin",
            dac_filter_write(DacFilter::FastLin),
            "5a 6c 03 00 04 00",
        ),
        (
            "dac slow-lin",
            dac_filter_write(DacFilter::SlowLin),
            "5a 6c 03 00 05 00",
        ),
        ("mic_boost_read", mic_boost_read(), "5a 3c 02 01 00"),
        (
            "boost 0",
            mic_boost_write(MicBoost::Db0),
            "5a 3c 04 00 00 02 00",
        ),
        (
            "boost 10",
            mic_boost_write(MicBoost::Db10),
            "5a 3c 04 00 00 02 0a",
        ),
        (
            "boost 20",
            mic_boost_write(MicBoost::Db20),
            "5a 3c 04 00 00 02 14",
        ),
        (
            "boost 30",
            mic_boost_write(MicBoost::Db30),
            "5a 3c 04 00 00 02 1e",
        ),
    ]
}

#[test]
fn every_fixed_builder_has_the_exact_bytes_of_plan_7_2() {
    for (name, frame, expected) in builder_table() {
        assert_eq!(frame.len(), REPORT_LEN, "{name}");
        assert_frame(&frame, expected);
    }
}

#[test]
fn len_byte_counts_the_payload_after_len_in_every_builder() {
    for (name, frame, expected) in builder_table() {
        let n = hex(expected).len();
        assert_eq!(frame[0], MAGIC, "{name}");
        assert_eq!(frame[2] as usize, n - 3, "{name}: LEN");
    }
}

#[test]
fn mask_write_matches_the_sbx_and_scout_helpers() {
    assert_eq!(mask_write(MaskTarget::Sbx, true), sbx_write(true));
    assert_eq!(mask_write(MaskTarget::Sbx, false), sbx_write(false));
    assert_eq!(mask_write(MaskTarget::Scout, true), scout_write(true));
    assert_eq!(mask_write(MaskTarget::Scout, false), scout_write(false));
}

#[test]
fn hid_write_buffer_is_report_id_zero_plus_frame_for_every_builder() {
    for (name, frame, _) in builder_table() {
        let buf = to_hid_write(&frame);
        assert_eq!(buf.len(), HID_WRITE_LEN, "{name}");
        assert_eq!(buf[0], 0x00, "{name}: report ID");
        assert_eq!(&buf[1..], &frame[..], "{name}");
        // The report ID is not the magic byte: the device sees 5a first.
        assert_eq!(buf[1], MAGIC, "{name}");
    }
}

#[test]
fn feature_read_builds_for_every_table_entry() {
    for f in FEATURES {
        let frame = feature_read(f.family, f.id).unwrap();
        assert_frame(
            &frame,
            &format!("5a 11 03 01 {:02x} {:02x}", f.family, f.id),
        );
    }
}

#[test]
fn feature_read_refuses_ids_outside_the_table() {
    for (fam, id) in [
        (0x96, 0x70),
        (0x96, 0x71),
        (0x96, 0x72),
        (0x95, 0x0a),
        (0x95, 0x2d),
        (0x00, 0x00),
        (0x97, 0x00),
    ] {
        assert!(feature_read(fam, id).is_err(), "{fam:02x}:{id:02x}");
    }
}

/// A value that is valid for the feature and not zero where possible.
fn sample_values(f: &g6_core::features::Feature) -> Vec<f32> {
    match f.kind {
        Kind::Bool => vec![0.0, 1.0],
        Kind::Choice(list) => list.iter().map(|(v, _)| *v as f32).collect(),
        _ => vec![f.min, f.max, f.round((f.min + f.max) / 2.0)],
    }
}

#[test]
fn feature_write_has_the_exact_bytes_for_every_exposed_feature() {
    for f in FEATURES.iter().filter(|f| f.exposed) {
        for v in sample_values(f) {
            let (frame, sent) = feature_write(f.family, f.id, v).unwrap();
            assert_eq!(sent, v, "{}", f.name);
            let b = v.to_le_bytes();
            assert_frame(
                &frame,
                &format!(
                    "5a 12 07 01 {:02x} {:02x} {:02x} {:02x} {:02x} {:02x}",
                    f.family, f.id, b[0], b[1], b[2], b[3]
                ),
            );
            assert_eq!(frame, feature_write_for(f, v).unwrap().0, "{}", f.name);
        }
    }
}

#[test]
fn feature_write_literal_bytes_for_known_values() {
    let cases: &[(u8, u8, f32, &str)] = &[
        (0x96, 0x09, 1.0, "00 00 80 3f"),   // EQ on
        (0x96, 0x09, 0.0, "00 00 00 00"),   // EQ off
        (0x96, 0x0b, -1.0, "00 00 80 bf"),  // 31 Hz -1 dB
        (0x96, 0x0d, 0.5, "00 00 00 3f"),   // 125 Hz +0.5 dB
        (0x96, 0x10, 12.0, "00 00 40 41"),  // 1k +12 dB
        (0x96, 0x14, -12.0, "00 00 40 c1"), // 16k -12 dB
        (0x96, 0x17, 150.0, "00 00 16 43"), // surround distance
        (0x96, 0x17, 10.0, "00 00 20 41"),
        (0x96, 0x17, 300.0, "00 00 96 43"),
        (0x96, 0x06, 2.0, "00 00 00 40"), // smart volume night
        (0x97, 0x02, 3.0, "00 00 40 40"), // decoder night
        (0x95, 0x05, 0.5, "00 00 00 3f"), // mic NR level, device scale
    ];
    for (fam, id, v, tail) in cases {
        let (frame, _) = feature_write(*fam, *id, *v).unwrap();
        assert_frame(&frame, &format!("5a 12 07 01 {fam:02x} {id:02x} {tail}"));
    }
}

#[test]
fn feature_write_rounds_before_encoding() {
    // Float noise from a UI / previous read never goes on the wire.
    let (frame, sent) = feature_write(0x96, 0x0c, 0.499_999_9).unwrap();
    assert_eq!(sent, 0.5);
    assert_frame(&frame, "5a 12 07 01 96 0c 00 00 00 3f");

    let (frame, sent) = feature_write(0x96, 0x0c, 1.3).unwrap();
    assert_eq!(sent, 1.5);
    assert_frame(&frame, "5a 12 07 01 96 0c 00 00 c0 3f");

    let (frame, sent) = feature_write(0x96, 0x01, 0.899_999_9).unwrap();
    assert_eq!(sent, 0.9);
    assert_eq!(&frame[6..10], &0.9f32.to_le_bytes());

    // Bool: 0.9999 -> 1.0.
    let (frame, sent) = feature_write(0x96, 0x00, 0.999_9).unwrap();
    assert_eq!(sent, 1.0);
    assert_frame(&frame, "5a 12 07 01 96 00 00 00 80 3f");
}

#[test]
fn feature_write_never_sends_negative_zero() {
    for v in [-0.0f32, -0.1, -1.0e-7] {
        let (frame, sent) = feature_write(0x96, 0x0b, v).unwrap();
        assert!(sent.is_sign_positive(), "{v}");
        assert_frame(&frame, "5a 12 07 01 96 0b 00 00 00 00");
    }
}

#[test]
fn feature_write_refuses_what_the_table_does_not_allow() {
    // Undocumented ids.
    for (fam, id) in [(0x96, 0x70), (0x96, 0x72), (0x95, 0x0a), (0x95, 0x2d)] {
        assert!(feature_write(fam, id, 0.0).is_err(), "{fam:02x}:{id:02x}");
    }
    // The pre-amp (96:0a) is writable within +-6 dB and refused outside.
    for v in [-6.0, -2.5, 0.0, 6.0] {
        let (frame, sent) = feature_write(0x96, 0x0a, v).unwrap();
        assert_eq!(sent, v);
        let b = v.to_le_bytes();
        assert_frame(
            &frame,
            &format!(
                "5a 12 07 01 96 0a {:02x} {:02x} {:02x} {:02x}",
                b[0], b[1], b[2], b[3]
            ),
        );
    }
    for v in [-6.5, 6.5, 12.0] {
        assert!(matches!(
            feature_write(0x96, 0x0a, v),
            Err(ProtocolError::Feature(
                g6_core::features::FeatureError::OutOfRange { .. }
            ))
        ));
    }
    assert!(feature_read(0x96, 0x0a).is_ok());
}

#[test]
fn encode_limits() {
    assert_eq!(encode(0x11, &[]).unwrap()[2], 0);
    let max = encode(0x11, &[0xaa; MAX_PAYLOAD]).unwrap();
    assert_eq!(max[2] as usize, MAX_PAYLOAD);
    assert!(max[3..].iter().all(|b| *b == 0xaa));
    assert_eq!(
        encode(0x11, &[0; MAX_PAYLOAD + 1]),
        Err(ProtocolError::PayloadTooLong(MAX_PAYLOAD + 1))
    );
}

// ---------------------------------------------------------------------------
// §2.4 captures, verbatim
// ---------------------------------------------------------------------------

const CAP_OUTPUT: &str = "5a 2c 05 01 02";
const CAP_MASK: &str = "5a 26 0b 08 ff ff 05";
const CAP_DUMP4: &str =
    "5a 11 1a 04 00 96 13 02 00 80 3f 96 14 03 00 00 3f 96 09 00 00 00 00 95 04 00 00 80 3f";
const CAP_DUMP8: &str = "5a 11 32 08 00 96 0b fd ff 7f bf 96 0c 00 00 40 34 96 0d 03 00 00 3f 96 0e 02 00 80 3f 96 0f 02 00 80 3f 96 10 02 00 c0 3f 96 11 02 00 80 3f 96 12 02 00 c0 3f";

#[test]
fn capture_output_speakers_and_headphones() {
    assert_eq!(
        decode(&report(CAP_OUTPUT)),
        Message::Output(Output::Speakers)
    );
    assert_eq!(
        decode(&report("5a 2c 05 01 04")),
        Message::Output(Output::Headphones)
    );
    // Unknown output code is not guessed.
    assert!(matches!(
        decode(&report("5a 2c 05 01 03")),
        Message::Unknown(_)
    ));
}

#[test]
fn capture_mask_sbx_plus_voice_clarity() {
    let Message::Mask(m) = decode(&report(CAP_MASK)) else {
        panic!("not a mask")
    };
    assert_eq!(m.0, 0x05);
    assert!(m.sbx() && !m.scout() && m.voice_clarity());
}

#[test]
fn capture_mask_every_bit_combination() {
    for bits in 0u8..8 {
        let m = decode(&report(&format!("5a 26 0b 08 ff ff {bits:02x}")));
        let Message::Mask(m) = m else {
            panic!("{bits}")
        };
        assert_eq!(m.sbx(), bits & 1 != 0);
        assert_eq!(m.scout(), bits & 2 != 0);
        assert_eq!(m.voice_clarity(), bits & 4 != 0);
    }
}

#[test]
fn capture_dump4_entries_raw_and_rounded() {
    let Message::Features(v) = decode(&report(CAP_DUMP4)) else {
        panic!("not features")
    };
    assert_eq!(v.len(), 4);
    let ids: Vec<(u8, u8)> = v.iter().map(|e| (e.family, e.id)).collect();
    assert_eq!(
        ids,
        [(0x96, 0x13), (0x96, 0x14), (0x96, 0x09), (0x95, 0x04)]
    );
    // Raw bits are untouched device noise ...
    assert_eq!(v[0].value.to_bits(), 0x3f80_0002);
    assert_eq!(v[1].value.to_bits(), 0x3f00_0003);
    assert_eq!(v[2].value.to_bits(), 0);
    assert_eq!(v[3].value.to_bits(), 0x3f80_0000);
    // ... and known() gives the clean value.
    let clean: Vec<f32> = v.iter().map(|e| e.known().unwrap().1).collect();
    assert_eq!(clean, [1.0, 0.5, 0.0, 1.0]);
    let names: Vec<&str> = v.iter().map(|e| e.known().unwrap().0.name).collect();
    assert_eq!(names, ["eq_8k", "eq_16k", "eq", "mic_clarity"]);
}

#[test]
fn capture_dump8_entries_raw_and_rounded() {
    let Message::Features(v) = decode(&report(CAP_DUMP8)) else {
        panic!("not features")
    };
    assert_eq!(v.len(), 8);
    // `fd ff 7f bf` and `00 00 40 34` are the float noise the plan names.
    assert_eq!(v[0].value.to_bits(), 0xbf7f_fffd);
    assert_eq!(v[1].value.to_bits(), 0x3440_0000);
    let names: Vec<&str> = v.iter().map(|e| e.known().unwrap().0.name).collect();
    assert_eq!(
        names,
        ["eq_31", "eq_62", "eq_125", "eq_250", "eq_500", "eq_1k", "eq_2k", "eq_4k"]
    );
    let clean: Vec<f32> = v.iter().map(|e| e.known().unwrap().1).collect();
    assert_eq!(clean, [-1.0, 0.0, 0.5, 1.0, 1.0, 1.5, 1.0, 1.5]);
    // 0.0 must be +0.0, not -0.0.
    assert!(clean[1].is_sign_positive());
}

#[test]
fn dump_captures_decode_identically_with_and_without_padding() {
    // Only the dumps are complete in §2.4; the short captures omit LEN's
    // trailing zero bytes and are always read as padded 64-byte reports.
    for cap in [CAP_DUMP4, CAP_DUMP8] {
        assert_eq!(decode(&hex(cap)), decode(&report(cap)), "{cap}");
    }
}

#[test]
fn captured_ack_statuses() {
    // ACK: 5a 02 0a CMD STATUS; 00 ok, 80/81/83 rejected.
    for (status, ok) in [(0x00u8, true), (0x80, false), (0x81, false), (0x83, false)] {
        let m = decode(&report(&format!("5a 02 0a 12 {status:02x}")));
        let Message::Ack { cmd, status: s } = m else {
            panic!("{status:02x}")
        };
        assert_eq!(cmd, 0x12);
        assert_eq!(s.is_ok(), ok, "{status:02x}");
        if !ok {
            assert_eq!(s, AckStatus::Rejected(status));
        }
    }
}

#[test]
fn replies_of_plan_7_2_decode() {
    assert_eq!(
        decode(&report("5a 05 04 1f 00 00 00")),
        Message::Identity(vec![0x1f, 0, 0, 0])
    );
    assert!(matches!(decode(&report("5a 20 04 97 00 00 00")), Message::HwId(v) if v[0] == 0x97));
    assert_eq!(
        decode(&report("5a 30 04 30 01 10 00")),
        Message::DspVersion(vec![0x30, 0x01, 0x10, 0x00])
    );
    assert_eq!(
        decode(&report("5a 6c 03 01 03 00")),
        Message::DacFilter(DacFilter::Nos)
    );
    for b in MicBoost::ALL {
        let m = decode(&report(&format!("5a 3c 04 01 00 02 {:02x}", b.db())));
        assert_eq!(m, Message::MicBoost(b));
    }
    // Firmware with the trailing NUL the plan mentions.
    let mut fw = vec![0x5a, 0x07, 0x10];
    fw.extend_from_slice(b"2.1.250903.1324\0");
    assert_eq!(decode(&fw), Message::Firmware("2.1.250903.1324".into()));
    // Feature read reply: value at bytes 7..10.
    let m = decode(&report("5a 11 08 01 00 96 0b 00 00 c0 3f"));
    let Message::Features(v) = m else { panic!() };
    assert_eq!((v[0].family, v[0].id, v[0].value), (0x96, 0x0b, 1.5));
}

#[test]
fn every_reply_reports_its_command_code() {
    let cases = [
        ("5a 02 0a 12 00", 0x02),
        ("5a 05 04 1f 00 00 00", 0x05),
        ("5a 07 02 31 00", 0x07),
        ("5a 11 08 01 00 96 09 00 00 80 3f", 0x11),
        ("5a 20 04 97 00 00 00", 0x20),
        ("5a 26 0b 08 ff ff 01", 0x26),
        ("5a 2c 05 01 04", 0x2c),
        ("5a 30 04 30 01 10 00", 0x30),
        ("5a 3c 04 01 00 02 1e", 0x3c),
        ("5a 6c 03 01 01 00", 0x6c),
        ("5a 1a 02 02", 0x1a),
        ("5a 6e 02 01", 0x6e),
    ];
    for (cap, c) in cases {
        assert_eq!(decode(&report(cap)).cmd(), Some(c), "{cap}");
    }
    assert_eq!(decode(&report("00 11 00")).cmd(), None);
}

// ---------------------------------------------------------------------------
// Unknown / undocumented content is kept out of the way
// ---------------------------------------------------------------------------

#[test]
fn unknown_pushes_decode_as_unknown_and_keep_their_command() {
    for (cap, cmd) in [("5a 1a 02 02", 0x1a), ("5a 6e 02 01", 0x6e)] {
        let m = decode(&report(cap));
        assert!(matches!(m, Message::Unknown(_)), "{cap}");
        assert_eq!(m.cmd(), Some(cmd));
    }
}

#[test]
fn undocumented_feature_ids_from_dumps_are_never_known() {
    // §2.4: dumps include 96:70-72 and 95:0a-2d. Build frames of up to 9
    // entries each and check that none of them is in the table.
    let mut entries: Vec<(u8, u8)> = (0x70..=0x72).map(|i| (0x96, i)).collect();
    // 95:0a-2d minus the ids that are documented (mic EQ, recording smart volume).
    entries.extend(
        (0x0a..=0x2du8)
            .filter(|id| g6_core::features::lookup(0x95, *id).is_none())
            .map(|id| (0x95, id)),
    );
    assert!(entries.len() > 20);
    let mut seen = 0;
    for chunk in entries.chunks(9) {
        let mut payload = vec![chunk.len() as u8, 0x00];
        for (fam, id) in chunk {
            payload.extend_from_slice(&[*fam, *id, 0x00, 0x00, 0x80, 0x3f]);
        }
        let frame = encode(0x11, &payload).unwrap();
        let Message::Features(v) = decode(&frame) else {
            panic!("{chunk:?}")
        };
        assert_eq!(v.len(), chunk.len());
        for e in &v {
            assert!(e.known().is_none(), "{:02x}:{:02x}", e.family, e.id);
            seen += 1;
        }
    }
    assert_eq!(seen, entries.len());
}

#[test]
fn mixed_dump_keeps_known_entries_and_marks_the_rest() {
    let frame = report("5a 11 14 03 00 96 09 00 00 80 3f 96 70 00 00 00 00 95 04 00 00 80 3f");
    let Message::Features(v) = decode(&frame) else {
        panic!()
    };
    let known: Vec<bool> = v.iter().map(|e| e.known().is_some()).collect();
    assert_eq!(known, [true, false, true]);
}

#[test]
fn malformed_frames_are_unknown_and_never_panic() {
    for bad in [
        "",
        "5a",
        "5a 11",
        "00 11 03 01 96 09",
        "5a 11 32 08 00",                   // LEN beyond the buffer
        "5a 11 08 02 00 96 09 00 00 80 3f", // COUNT larger than the data
        "5a 11 08 01 01 96 09 00 00 80 3f", // second byte must be 00
        "5a 26 0b 08 ff fe 05",             // not the mask marker
        "5a 3c 04 01 00 03 1e",             // not the boost marker
        "5a 3c 04 01 00 02 15",             // 21 dB is not a step
        "5a 6c 03 01 00 00",                // filter 0
        "5a 6c 03 01 06 00",                // filter 6
        "5a 02 01 12",                      // ACK without status
    ] {
        let m = decode(&hex(bad));
        assert!(matches!(m, Message::Unknown(_)), "`{bad}` -> {m:?}");
    }
}

#[test]
fn decode_survives_every_header_and_pseudo_random_tails() {
    // Deterministic xorshift; not a full fuzzer, but every CMD/LEN pair.
    let mut x: u32 = 0x1234_5678;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        (x & 0xff) as u8
    };
    for cmd in 0..=255u8 {
        for len in 0..=255u8 {
            let mut f = [0u8; REPORT_LEN];
            for b in f.iter_mut() {
                *b = next();
            }
            f[0] = MAGIC;
            f[1] = cmd;
            f[2] = len;
            let _ = decode(&f);
            // Truncated copies too.
            let _ = decode(&f[..(len as usize % REPORT_LEN)]);
        }
    }
}

#[test]
fn enum_byte_codes_round_trip() {
    for f in DacFilter::ALL {
        assert_eq!(DacFilter::from_byte(f as u8), Some(f));
        assert_eq!(DacFilter::from_name(f.name()), Some(f));
    }
    assert_eq!(DacFilter::ALL.map(|f| f as u8), [1, 2, 3, 4, 5]);
    assert_eq!(DacFilter::from_byte(0), None);
    assert_eq!(DacFilter::from_byte(6), None);
    for b in MicBoost::ALL {
        assert_eq!(MicBoost::from_byte(b as u8), Some(b));
    }
    assert_eq!(MicBoost::ALL.map(|b| b.db()), [0x00, 0x0a, 0x14, 0x1e]);
    assert_eq!(Output::Speakers.to_byte(), 0x02);
    assert_eq!(Output::Headphones.to_byte(), 0x04);
    assert_eq!(Output::from_byte(0x00), None);
}
