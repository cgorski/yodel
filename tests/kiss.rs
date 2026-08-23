//! Integration tests for the KISS TNC framing layer.

#![cfg(feature = "kiss")]

use warble::kiss::{
    FEND, FESC, KissCommand, KissDeframer, KissError, KissFrameIter, KissPort, TFEND, TFESC,
    encode_into, encoded_len, frame_iter,
};

/// Encodes one frame into a fresh Vec via `encode_into`.
fn encode_vec(port: u8, command: KissCommand, payload: &[u8]) -> Vec<u8> {
    let port = KissPort::new(port).expect("test port in range");
    let mut buf = vec![0u8; encoded_len(port, command, payload)];
    let len = encode_into(port, command, payload, &mut buf).expect("buffer sized exactly");
    assert_eq!(len, buf.len());
    buf
}

/// An owned decode outcome: `(command, port number, payload)`.
type Decoded = (KissCommand, u8, Vec<u8>);

/// Runs bytes through a deframer, collecting owned results.
fn deframe_all<const N: usize>(bytes: &[u8]) -> Vec<Result<Decoded, KissError>> {
    let mut d = KissDeframer::<N>::new();
    let mut out = Vec::new();
    for &b in bytes {
        if let Some(r) = d.push(b) {
            out.push(r.map(|f| (f.command(), f.port().get(), f.payload().to_vec())));
        }
    }
    out
}

#[test]
fn constants_documented_values() {
    assert_eq!(FEND, 0xC0);
    assert_eq!(FESC, 0xDB);
    assert_eq!(TFEND, 0xDC);
    assert_eq!(TFESC, 0xDD);
}

#[test]
fn port_validation() {
    for p in 0..=15u8 {
        assert_eq!(KissPort::new(p).map(KissPort::get), Ok(p));
    }
    for p in [16u8, 17, 100, 255] {
        assert_eq!(KissPort::new(p), Err(KissError::PortOutOfRange { got: p }));
    }
}

#[test]
fn command_byte_for_every_variant_and_port() {
    let variants = [
        (KissCommand::Data, 0u8),
        (KissCommand::TxDelay, 1),
        (KissCommand::Persistence, 2),
        (KissCommand::SlotTime, 3),
        (KissCommand::TxTail, 4),
        (KissCommand::FullDuplex, 5),
        (KissCommand::SetHardware, 6),
    ];
    for port in 0..=15u8 {
        let kp = KissPort::new(port).expect("in range");
        for (cmd, nibble) in variants {
            let byte = cmd.to_byte(kp);
            assert_eq!(byte, (port << 4) | nibble);
            assert_eq!(KissCommand::from_byte(byte), Ok((cmd, kp)));
        }
        // Return ignores the port: the whole byte is 0xFF.
        assert_eq!(KissCommand::Return.to_byte(kp), 0xFF);
    }
    assert_eq!(
        KissCommand::from_byte(0xFF),
        Ok((KissCommand::Return, KissPort::new(0).expect("in range")))
    );
}

#[test]
fn unknown_command_nibbles_rejected() {
    for nibble in 7..=14u8 {
        for port in [0u8, 5, 15] {
            let byte = (port << 4) | nibble;
            assert_eq!(
                KissCommand::from_byte(byte),
                Err(KissError::UnknownCommand { got: nibble })
            );
        }
    }
    // 0x_F low nibble is only valid as the full 0xFF byte.
    for port in 0..15u8 {
        let byte = (port << 4) | 0x0F;
        assert_eq!(
            KissCommand::from_byte(byte),
            Err(KissError::UnknownCommand { got: 0x0F })
        );
    }
}

#[test]
fn encode_vectors_escaping() {
    // (payload, expected escaped payload bytes)
    let cases: &[(&[u8], &[u8])] = &[
        (&[], &[]),
        (&[0x01, 0x02], &[0x01, 0x02]),
        (&[FEND], &[FESC, TFEND]),
        (&[FESC], &[FESC, TFESC]),
        (&[FEND, FESC], &[FESC, TFEND, FESC, TFESC]),
        (&[FESC, FEND], &[FESC, TFESC, FESC, TFEND]),
        (&[FEND, FEND], &[FESC, TFEND, FESC, TFEND]),
        (&[FESC, FESC], &[FESC, TFESC, FESC, TFESC]),
        (&[FEND, 0x42], &[FESC, TFEND, 0x42]),
        (&[0x42, FEND], &[0x42, FESC, TFEND]),
        (&[0x42, FESC, 0x43], &[0x42, FESC, TFESC, 0x43]),
        // TFEND/TFESC alone are NOT escaped.
        (&[TFEND, TFESC], &[TFEND, TFESC]),
    ];
    let port = KissPort::new(0).expect("in range");
    for &(payload, escaped) in cases {
        let mut expected = vec![FEND, 0x00];
        expected.extend_from_slice(escaped);
        expected.push(FEND);
        assert_eq!(
            encode_vec(0, KissCommand::Data, payload),
            expected,
            "payload {payload:02X?}"
        );
        assert_eq!(
            encoded_len(port, KissCommand::Data, payload),
            expected.len()
        );
    }
}

#[test]
fn encode_buffer_too_small() {
    let payload = [FEND, 0x01];
    let port = KissPort::new(0).expect("in range");
    let mut buf = [0u8; 4];
    assert_eq!(
        encode_into(port, KissCommand::Data, &payload, &mut buf),
        Err(KissError::BufferTooSmall { needed: 6, got: 4 })
    );
}

#[test]
fn iterator_encoder_equals_buffer_encoder() {
    let payloads: &[&[u8]] = &[
        &[],
        &[0x00],
        &[FEND],
        &[FESC],
        &[FEND, FESC, TFEND, TFESC, 0x55],
        &[FESC, TFEND],
        b"hello world",
    ];
    for &payload in payloads {
        for (port, cmd) in [
            (0u8, KissCommand::Data),
            (7, KissCommand::TxDelay),
            (15, KissCommand::Return),
        ] {
            let kp = KissPort::new(port).expect("in range");
            let from_iter: Vec<u8> = frame_iter(kp, cmd, payload).collect();
            let from_buf = {
                let mut buf = vec![0u8; encoded_len(kp, cmd, payload)];
                let len = encode_into(kp, cmd, payload, &mut buf).expect("buffer sized exactly");
                assert_eq!(len, buf.len());
                buf
            };
            assert_eq!(from_iter, from_buf, "payload {payload:02X?}");
        }
    }
    // Iterator is cloneable and restartable from its current state.
    let it: KissFrameIter<'_> = frame_iter(
        KissPort::new(1).expect("in range"),
        KissCommand::Data,
        &[FEND],
    );
    assert_eq!(it.clone().count(), it.count());
}

#[test]
fn escaped_pair_in_payload_survives_round_trip() {
    // The classic edge case: a payload that *contains* the escape
    // sequence bytes [FESC, TFEND] must decode back to those two bytes,
    // never to a literal FEND.
    for payload in [[FESC, TFEND], [FESC, TFESC], [TFEND, TFESC]] {
        let wire = encode_vec(0, KissCommand::Data, &payload);
        let frames = deframe_all::<32>(&wire);
        assert_eq!(frames, [Ok((KissCommand::Data, 0, payload.to_vec()))]);
    }
}

#[test]
fn streaming_decode_across_split_pushes() {
    let payload = [0x10, FEND, 0x20, FESC, 0x30];
    let wire = encode_vec(5, KissCommand::Data, &payload);
    let mut d = KissDeframer::<32>::new();
    let mut got = Vec::new();
    // Feed one byte per push, in three arbitrary chunks, checking that
    // nothing completes until the closing FEND.
    let (a, rest) = wire.split_at(3);
    let (b, c) = rest.split_at(rest.len() - 2);
    for chunk in [a, b, c] {
        for &byte in chunk {
            if let Some(r) = d.push(byte) {
                let f = r.expect("valid frame");
                got.push((f.command(), f.port().get(), f.payload().to_vec()));
            }
        }
    }
    assert_eq!(got, [(KissCommand::Data, 5, payload.to_vec())]);
}

#[test]
fn empty_frames_and_garbage_before_fend() {
    // Garbage before the first FEND, then back-to-back FENDs, then a frame.
    let mut wire = vec![0x11, 0x22, FESC, 0x33];
    wire.extend_from_slice(&[FEND, FEND, FEND]);
    wire.extend_from_slice(&encode_vec(2, KissCommand::SlotTime, &[0x0A]));
    let frames = deframe_all::<16>(&wire);
    assert_eq!(frames, [Ok((KissCommand::SlotTime, 2, vec![0x0A]))]);
}

#[test]
fn return_frame_decodes() {
    let wire = [FEND, 0xFF, FEND];
    let frames = deframe_all::<8>(&wire);
    assert_eq!(frames, [Ok((KissCommand::Return, 0, vec![]))]);
}

#[test]
fn invalid_escape_error_then_next_frame_ok() {
    let mut wire = vec![FEND, 0x00, 0x01, FESC, 0x99, 0x02, 0x03, FEND];
    wire.extend_from_slice(&encode_vec(1, KissCommand::Data, &[0x42]));
    let frames = deframe_all::<16>(&wire);
    assert_eq!(
        frames,
        [
            Err(KissError::InvalidEscape { got: 0x99 }),
            Ok((KissCommand::Data, 1, vec![0x42])),
        ]
    );
}

#[test]
fn dangling_escape_at_frame_end_is_invalid() {
    let mut wire = vec![FEND, 0x00, 0x01, FESC, FEND];
    wire.extend_from_slice(&encode_vec(0, KissCommand::Data, &[0x42]));
    let frames = deframe_all::<16>(&wire);
    assert_eq!(
        frames,
        [
            Err(KissError::InvalidEscape { got: FEND }),
            Ok((KissCommand::Data, 0, vec![0x42])),
        ]
    );
}

#[test]
fn overflow_error_then_next_frame_ok() {
    let big = [0x55u8; 20];
    let mut wire = encode_vec(0, KissCommand::Data, &big);
    wire.extend_from_slice(&encode_vec(0, KissCommand::Data, &[0x01]));
    let frames = deframe_all::<8>(&wire);
    assert_eq!(
        frames,
        [
            Err(KissError::FrameTooLarge { capacity: 8 }),
            Ok((KissCommand::Data, 0, vec![0x01])),
        ]
    );
}

#[test]
fn unknown_command_byte_reported_on_decode() {
    let wire = [FEND, 0x07, 0xAA, FEND];
    let frames = deframe_all::<8>(&wire);
    assert_eq!(frames, [Err(KissError::UnknownCommand { got: 7 })]);
}

/// Every [`KissCommand`] variant, in low-nibble order.
const COMMANDS: [KissCommand; 8] = [
    KissCommand::Data,
    KissCommand::TxDelay,
    KissCommand::Persistence,
    KissCommand::SlotTime,
    KissCommand::TxTail,
    KissCommand::FullDuplex,
    KissCommand::SetHardware,
    KissCommand::Return,
];

/// The four bytes KISS framing gives special meaning to. `FEND`/`FESC`
/// must be escaped; `TFEND`/`TFESC` must NOT be, and are included so the
/// sweep would catch over-escaping as well as under-escaping.
const SPECIALS: [u8; 4] = [FEND, FESC, TFEND, TFESC];

/// Deterministic payloads saturated with `FEND`/`FESC`/`TFEND`/`TFESC`:
/// every sequence over those four bytes of length 0..=3, plus longer runs,
/// cycles, and mixtures with ordinary bytes.
fn saturated_payloads() -> Vec<Vec<u8>> {
    let mut out = vec![Vec::new()];
    for a in SPECIALS {
        out.push(vec![a]);
        for b in SPECIALS {
            out.push(vec![a, b]);
            for c in SPECIALS {
                out.push(vec![a, b, c]);
            }
        }
    }
    for s in SPECIALS {
        out.push(vec![s; 16]);
    }
    out.push(SPECIALS.iter().copied().cycle().take(32).collect());
    out.push(SPECIALS.iter().rev().copied().cycle().take(32).collect());
    // Escapes adjacent to ordinary bytes, in both orders.
    out.push(SPECIALS.iter().flat_map(|&s| [s, 0x00, s, 0x7F]).collect());
    out.push(SPECIALS.iter().flat_map(|&s| [0x41, s, 0xFF, s]).collect());
    out
}

#[test]
fn exhaustive_port_command_sweep_round_trips() {
    // The whole 16x8 (port, command) grid, not a sample of it: the one
    // command byte that collides with FEND (port 12 + Data = 0xC0) sits at
    // a single point in this grid and is invisible to any sweep that
    // happens to skip it.
    const MIN_CASES: usize = 8_192;
    let payloads = saturated_payloads();
    let mut cases = 0usize;
    let mut pairs = 0usize;
    let mut commands_needing_escape = 0usize;

    for port_n in 0..=15u8 {
        let port = KissPort::new(port_n).expect("port in range");
        for cmd in COMMANDS {
            pairs += 1;
            let cmd_byte = cmd.to_byte(port);
            if cmd_byte == FEND || cmd_byte == FESC {
                commands_needing_escape += 1;
            }
            // Return is port-agnostic (0xFF) and decodes back as port 0;
            // every other command carries its port in the high nibble.
            let want_port = if cmd == KissCommand::Return {
                0
            } else {
                port_n
            };

            for payload in &payloads {
                let mut buf = [0u8; 128];
                let len =
                    encode_into(port, cmd, payload, &mut buf).expect("buffer generously sized");
                let wire = buf.get(..len).expect("len within buf");
                let ctx = format!("port {port_n} {cmd:?} payload {payload:02X?} wire {wire:02X?}");

                // encoded_len must predict exactly what was written.
                assert_eq!(
                    encoded_len(port, cmd, payload),
                    len,
                    "encoded_len mispredicts: {ctx}"
                );

                // Transparency: FEND appears only as the two delimiters.
                let interior = wire.get(1..len - 1).expect("frame has both delimiters");
                assert_eq!(
                    interior.iter().filter(|&&b| b == FEND).count(),
                    0,
                    "bare FEND inside the frame: {ctx}"
                );

                // Both encoders escape the command byte, so they must agree.
                let from_iter: Vec<u8> = frame_iter(port, cmd, payload).collect();
                assert_eq!(
                    from_iter, wire,
                    "frame_iter differs from encode_into: {ctx}"
                );

                // Byte-exact round trip through the crate's own deframer.
                assert_eq!(
                    deframe_all::<192>(wire),
                    [Ok((cmd, want_port, payload.clone()))],
                    "round trip broken: {ctx}"
                );

                cases += 1;
            }
        }
    }

    assert_eq!(pairs, 16 * 8, "sweep must cover every (port, command) pair");
    assert_eq!(
        commands_needing_escape, 1,
        "port 12 + Data must be the only command byte needing an escape"
    );
    assert!(
        cases >= MIN_CASES,
        "sweep ran only {cases} cases, expected at least {MIN_CASES}"
    );
}

#[test]
fn port_12_data_command_byte_is_escaped() {
    // (12 << 4) | Data = 0xC0 = FEND. Escaping the command byte is what
    // makes this frame readable at all.
    let port = KissPort::new(12).expect("port in range");
    let payload = [0x11u8, 0x22, 0x33];
    let wire = encode_vec(12, KissCommand::Data, &payload);
    assert_eq!(wire, [FEND, FESC, TFEND, 0x11, 0x22, 0x33, FEND]);
    assert_eq!(encoded_len(port, KissCommand::Data, &payload), 7);
    assert_eq!(encoded_len(port, KissCommand::Data, &payload), wire.len());
    assert_eq!(
        deframe_all::<32>(&wire),
        [Ok((KissCommand::Data, 12, payload.to_vec()))]
    );

    // The buffer requirement accounts for the escape: a buffer sized for an
    // unescaped command byte is now correctly rejected rather than filled
    // with an unreadable frame.
    let mut small = [0u8; 6];
    assert_eq!(
        encode_into(port, KissCommand::Data, &payload, &mut small),
        Err(KissError::BufferTooSmall { needed: 7, got: 6 })
    );

    // Why escaping it is not optional: with a bare 0xC0 command byte the
    // frame is not just lossy, it decodes as a *different* command on a
    // different port with a truncated payload, and reports no error.
    assert_eq!(
        deframe_all::<32>(&[FEND, FEND, 0x11, 0x22, 0x33, FEND]),
        [Ok((KissCommand::TxDelay, 1, vec![0x22, 0x33]))]
    );
    // With a payload whose first byte is not a valid command byte, the same
    // collision surfaces as a spurious error instead — which is why the
    // defect was not consistently visible.
    assert_eq!(
        deframe_all::<32>(&[FEND, FEND, 0x0B, 0x22, FEND]),
        [Err(KissError::UnknownCommand { got: 0x0B })]
    );
}

#[test]
fn round_trip_sweep_all_byte_values() {
    // Deterministic sweep: payload patterns covering every byte value,
    // in runs, alternations, and a full 0..=255 ramp.
    let mut payloads: Vec<Vec<u8>> = Vec::new();
    payloads.push((0u16..=255).map(|b| b as u8).collect());
    for b in 0u16..=255 {
        payloads.push(vec![b as u8; 5]);
        payloads.push(vec![b as u8, FEND, b as u8, FESC, b as u8]);
    }
    for payload in &payloads {
        let wire = encode_vec(9, KissCommand::Data, payload);
        let iter_wire: Vec<u8> = frame_iter(
            KissPort::new(9).expect("in range"),
            KissCommand::Data,
            payload,
        )
        .collect();
        assert_eq!(wire, iter_wire);
        let frames = deframe_all::<600>(&wire);
        assert_eq!(frames, [Ok((KissCommand::Data, 9, payload.clone()))]);
    }
}
