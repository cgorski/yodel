//! M17 packet-mode tests: callsign addressing, CRC-16 KATs, Golay
//! encode/correct sweeps, convolutional + puncture roundtrips,
//! interleaver/randomizer laws, LSF roundtrips, and full baseband audio
//! packet roundtrips (clean, corrupted-within-capacity, garbage).
#![cfg(feature = "m17")]

use yodel::SampleRate;
use yodel::m17::{
    self, Address, FRAME_BITS, FRAME_BYTES, Lsf, M17Error, M17FrameEvent, M17PacketTx, M17Receiver,
    MAX_PACKET_PAYLOAD, PACKET_FRAME_PAYLOAD, PacketAssembler, PacketFrame, convolutional_encode,
    crc16, deinterleave, depuncture, dibit_to_symbol, golay24_decode, golay24_encode, interleave,
    interleave_index, lsf_decode, lsf_encode, packet_frame_decode, packet_frame_encode, puncture,
    randomize, symbol_to_dibit, sync_symbols, viterbi_decode,
};

// ---------------------------------------------------------------------------
// Callsign / address
// ---------------------------------------------------------------------------

#[test]
fn callsign_roundtrips() {
    for cs in ["N0CALL", "AB1CDE", "W1AW", "SP5WWP", "A", "W1AW/P", "K1-9."] {
        let addr = Address::from_callsign(cs).unwrap();
        let mut buf = [0u8; 9];
        assert_eq!(addr.callsign(&mut buf), cs, "roundtrip of {cs}");
    }
}

#[test]
fn callsign_case_folds() {
    let a = Address::from_callsign("n0call").unwrap();
    let b = Address::from_callsign("N0CALL").unwrap();
    assert_eq!(a, b);
}

#[test]
fn callsign_rejects_bad_input() {
    assert_eq!(
        Address::from_callsign(""),
        Err(M17Error::CallsignLength { len: 0 })
    );
    assert_eq!(
        Address::from_callsign("ABCDEFGHIJ"),
        Err(M17Error::CallsignLength { len: 10 })
    );
    assert_eq!(
        Address::from_callsign("AB CD"),
        Err(M17Error::CallsignChar { ch: ' ' })
    );
    assert_eq!(
        Address::from_callsign("AB#"),
        Err(M17Error::CallsignChar { ch: '#' })
    );
}

#[test]
fn address_reserved_and_broadcast() {
    assert!(Address::broadcast().is_broadcast());
    assert_eq!(Address::broadcast().raw(), 0xFFFF_FFFF_FFFF);
    assert_eq!(
        Address::from_raw(0),
        Err(M17Error::ReservedAddress { value: 0 })
    );
    // Just above the last encodable callsign, below broadcast: reserved.
    let reserved = m17::MAX_CALLSIGN_ADDRESS + 1;
    assert_eq!(
        Address::from_raw(reserved),
        Err(M17Error::ReservedAddress { value: reserved })
    );
    assert_eq!(
        Address::from_raw(1 << 48),
        Err(M17Error::AddressRange { value: 1 << 48 })
    );
    assert!(Address::from_raw(0xFFFF_FFFF_FFFF).unwrap().is_broadcast());
}

#[test]
fn callsign_base40_first_char_is_least_significant() {
    // Per spec the first character is the least significant base-40
    // digit: "A" encodes to 1, "AB" to 1 + 40*2 = 81.
    assert_eq!(Address::from_callsign("A").unwrap().raw(), 1);
    assert_eq!(Address::from_callsign("AB").unwrap().raw(), 81);
}

// ---------------------------------------------------------------------------
// CRC-16
// ---------------------------------------------------------------------------

/// KATs from the published M17 spec's CRC section check values.
#[test]
fn crc16_known_answers() {
    assert_eq!(crc16(&[]), 0xFFFF);
    assert_eq!(crc16(b"A"), 0x206E);
    assert_eq!(crc16(b"123456789"), 0x772B);
}

#[test]
fn crc16_detects_single_flip() {
    let data = b"the quick brown fox";
    let good = crc16(data);
    let mut bad = *data;
    bad[3] ^= 0x10;
    assert_ne!(crc16(&bad), good);
}

// ---------------------------------------------------------------------------
// Golay(24,12)
// ---------------------------------------------------------------------------

#[test]
fn golay_encode_is_systematic_with_even_weight() {
    for data in [0u16, 1, 0x123, 0xABC, 0xFFF] {
        let cw = golay24_encode(data);
        assert_eq!((cw >> 12) as u16, data, "systematic data field");
        assert_eq!(cw.count_ones() % 2, 0, "extended parity is even");
    }
}

#[test]
fn golay_min_distance_is_eight() {
    // Spot-check the extended code's distance on a sample of pairs.
    for a in (0..4096u16).step_by(97) {
        for b in (0..4096u16).step_by(89) {
            if a != b {
                let d = (golay24_encode(a) ^ golay24_encode(b)).count_ones();
                assert!(d >= 8, "d({a},{b}) = {d}");
            }
        }
    }
}

#[test]
fn golay_corrects_up_to_three_errors() {
    // Sweep every 1-bit error and samples of 2-/3-bit errors on a
    // spread of data words.
    for data in [0u16, 0x5A5, 0xFFF, 0x001, 0x800, 0x3C7] {
        let cw = golay24_encode(data);
        for i in 0..24 {
            let (d, n) = golay24_decode(cw ^ (1 << i)).unwrap();
            assert_eq!((d, n), (data, 1));
        }
        for i in 0..24 {
            for j in (i + 1)..24 {
                let (d, n) = golay24_decode(cw ^ (1 << i) ^ (1 << j)).unwrap();
                assert_eq!((d, n), (data, 2));
            }
        }
        for i in (0..24).step_by(3) {
            for j in ((i + 1)..24).step_by(2) {
                for k in (j + 1)..24 {
                    let (d, n) = golay24_decode(cw ^ (1 << i) ^ (1 << j) ^ (1 << k)).unwrap();
                    assert_eq!((d, n), (data, 3));
                }
            }
        }
    }
}

#[test]
fn golay_rejects_four_errors() {
    let cw = golay24_encode(0x2F1);
    // 4 flips are ≥ 4 from the original and, by d = 8, ≥ 4 from every
    // other codeword too: must report uncorrectable.
    let corrupted = cw ^ 0b1111;
    assert_eq!(golay24_decode(corrupted), None);
}

// ---------------------------------------------------------------------------
// Convolutional code + puncturing
// ---------------------------------------------------------------------------

#[test]
fn conv_encode_zero_input_is_zero() {
    let data = [0u8; 4];
    let mut out = [0u8; 9];
    let n = convolutional_encode(&data, 32, &mut out);
    assert_eq!(n, 72);
    assert!(out.iter().all(|&b| b == 0));
}

#[test]
fn conv_viterbi_roundtrip_clean_and_with_errors() {
    let data: Vec<u8> = (0..26u8)
        .map(|i| i.wrapping_mul(37).wrapping_add(11))
        .collect();
    let nbits = 206;
    let mut coded = [0u8; 53];
    let n = convolutional_encode(&data, nbits, &mut coded);
    assert_eq!(n, 420);

    // Clean decode.
    let mut bits = [0u8; 420];
    let mut known = [true; 420];
    for (i, b) in bits.iter_mut().enumerate() {
        *b = (coded[i / 8] >> (7 - i % 8)) & 1;
    }
    let mut out = [0u8; 26];
    let metric = viterbi_decode(&bits, &known, nbits, &mut out);
    assert_eq!(metric, 0);
    assert_eq!(&out[..26], &data[..]);

    // Scattered bit errors (well within the free-distance budget when
    // spread out).
    for &e in &[13usize, 97, 205, 333] {
        bits[e] ^= 1;
    }
    let metric = viterbi_decode(&bits, &known, nbits, &mut out);
    assert_eq!(metric, 4);
    assert_eq!(&out[..26], &data[..]);

    // Erasures instead of errors.
    for (i, b) in bits.iter_mut().enumerate() {
        *b = (coded[i / 8] >> (7 - i % 8)) & 1;
    }
    for &e in &[20usize, 21, 150, 300] {
        known[e] = false;
    }
    let metric = viterbi_decode(&bits, &known, nbits, &mut out);
    assert_eq!(metric, 0);
    assert_eq!(&out[..26], &data[..]);
}

#[test]
fn puncture_depuncture_are_inverse_on_kept_positions() {
    let mut coded = [0u8; 61];
    for (i, b) in coded.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(151).wrapping_add(7);
    }
    let mut kept = [0u8; FRAME_BYTES];
    let n = puncture(&coded, 488, &m17::PUNCTURE_P1, &mut kept);
    assert_eq!(n, FRAME_BITS, "P1 takes 488 to 368");

    let mut bits = [0u8; 488];
    let mut known = [false; 488];
    let consumed = depuncture(&kept, n, &m17::PUNCTURE_P1, 488, &mut bits, &mut known);
    assert_eq!(consumed, FRAME_BITS);
    let mut kept_count = 0;
    for i in 0..488 {
        let orig = (coded[i / 8] >> (7 - i % 8)) & 1;
        if known[i] {
            assert_eq!(bits[i], orig, "kept bit {i}");
            kept_count += 1;
        }
    }
    assert_eq!(kept_count, FRAME_BITS);
    // P3 rate check: 420 -> 368.
    let mut kept3 = [0u8; FRAME_BYTES];
    let n3 = puncture(&coded, 420, &m17::PUNCTURE_P3, &mut kept3);
    assert_eq!(n3, FRAME_BITS, "P3 takes 420 to 368");
}

// ---------------------------------------------------------------------------
// Interleaver + randomizer
// ---------------------------------------------------------------------------

#[test]
fn interleaver_is_a_permutation() {
    let mut seen = [false; FRAME_BITS];
    for i in 0..FRAME_BITS {
        let j = interleave_index(i);
        assert!(!seen[j], "index {j} hit twice");
        seen[j] = true;
    }
    assert!(seen.iter().all(|&s| s));
}

#[test]
fn interleave_deinterleave_roundtrip() {
    let mut frame = [0u8; FRAME_BYTES];
    for (i, b) in frame.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(101).wrapping_add(3);
    }
    assert_eq!(deinterleave(&interleave(&frame)), frame);
    assert_ne!(interleave(&frame), frame, "permutation is nontrivial");
}

#[test]
fn randomizer_is_self_inverse() {
    let mut frame = [0u8; FRAME_BYTES];
    for (i, b) in frame.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(29);
    }
    let orig = frame;
    randomize(&mut frame);
    assert_ne!(frame, orig);
    randomize(&mut frame);
    assert_eq!(frame, orig);
}

// ---------------------------------------------------------------------------
// LSF
// ---------------------------------------------------------------------------

#[test]
fn lsf_bytes_roundtrip_and_crc() {
    let lsf = Lsf::packet_data(
        Address::broadcast(),
        Address::from_callsign("SP5WWP").unwrap(),
        7,
    );
    let bytes = lsf.to_bytes();
    let back = Lsf::from_bytes(&bytes).unwrap();
    assert_eq!(back, lsf);
    // TYPE: packet (bit0 = 0), data subtype 0b01, CAN 7.
    assert_eq!(lsf.lsf_type & 1, 0);
    assert_eq!((lsf.lsf_type >> 1) & 0b11, 0b01);
    assert_eq!((lsf.lsf_type >> 7) & 0xF, 7);

    let mut bad = bytes;
    bad[0] ^= 0x40;
    assert_eq!(Lsf::from_bytes(&bad), Err(M17Error::Crc));
}

#[test]
fn lsf_channel_coding_roundtrip() {
    let lsf = Lsf::packet_data(
        Address::from_callsign("N0CALL").unwrap(),
        Address::from_callsign("W1AW").unwrap(),
        3,
    );
    let coded = lsf_encode(&lsf);
    assert_eq!(lsf_decode(&coded).unwrap(), lsf);

    // A few channel bit errors survive the rate-1/2 FEC.
    let mut noisy = coded;
    for &bit in &[10usize, 100, 260, 350] {
        noisy[bit / 8] ^= 1 << (7 - bit % 8);
    }
    assert_eq!(lsf_decode(&noisy).unwrap(), lsf);

    // Garbage does not pass the CRC.
    let mut garbage = [0u8; FRAME_BYTES];
    for (i, b) in garbage.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(173).wrapping_add(55);
    }
    assert!(lsf_decode(&garbage).is_err());
}

// ---------------------------------------------------------------------------
// Packet frames
// ---------------------------------------------------------------------------

#[test]
fn packet_frame_content_roundtrip() {
    let mut data = [0u8; PACKET_FRAME_PAYLOAD];
    for (i, d) in data.iter_mut().enumerate() {
        *d = i as u8;
    }
    for (eof, counter) in [(false, 0u8), (false, 31), (true, 25), (true, 1)] {
        let f = PacketFrame { data, eof, counter };
        assert_eq!(PacketFrame::from_content(&f.to_content()), f);
    }
}

#[test]
fn packet_frame_channel_coding_roundtrip_with_errors() {
    let mut data = [0u8; PACKET_FRAME_PAYLOAD];
    for (i, d) in data.iter_mut().enumerate() {
        *d = (i as u8).wrapping_mul(13).wrapping_add(200);
    }
    let frame = PacketFrame {
        data,
        eof: true,
        counter: 25,
    };
    let coded = packet_frame_encode(&frame);
    let (back, metric) = packet_frame_decode(&coded);
    assert_eq!(back, frame);
    assert_eq!(metric, 0);

    let mut noisy = coded;
    for &bit in &[5usize, 111, 222, 333] {
        noisy[bit / 8] ^= 1 << (7 - bit % 8);
    }
    let (back, metric) = packet_frame_decode(&noisy);
    assert_eq!(back, frame);
    assert!(metric > 0);
}

// ---------------------------------------------------------------------------
// Symbols + sync
// ---------------------------------------------------------------------------

#[test]
fn dibit_symbol_mapping_is_the_spec_table() {
    assert_eq!(dibit_to_symbol(0b01), 3);
    assert_eq!(dibit_to_symbol(0b00), 1);
    assert_eq!(dibit_to_symbol(0b10), -1);
    assert_eq!(dibit_to_symbol(0b11), -3);
    for d in 0..4u8 {
        assert_eq!(symbol_to_dibit(dibit_to_symbol(d)), d);
    }
}

#[test]
fn sync_words_are_extreme_symbols() {
    // The published sync words map to ±3 symbols only (maximum energy).
    for sync in [
        m17::SYNC_LSF,
        m17::SYNC_STREAM,
        m17::SYNC_PACKET,
        m17::SYNC_BERT,
    ] {
        for s in sync_symbols(sync) {
            assert!(s == 3 || s == -3, "sync {sync:#06x} symbol {s}");
        }
    }
    assert_eq!(m17::SYNC_LSF, 0x55F7);
    assert_eq!(m17::SYNC_PACKET, 0x75FF);
    assert_eq!(m17::SYNC_STREAM, 0xFF5D);
    assert_eq!(m17::SYNC_BERT, 0xDF55);
}

// ---------------------------------------------------------------------------
// Full baseband audio roundtrips
// ---------------------------------------------------------------------------

fn roundtrip(payload: &[u8], mangle: impl Fn(usize, i16) -> i16) -> Option<Vec<u8>> {
    let lsf = Lsf::packet_data(
        Address::broadcast(),
        Address::from_callsign("N0CALL").unwrap(),
        0,
    );
    let sr = SampleRate::new(48_000).unwrap();
    let mut tx = M17PacketTx::new(sr, lsf, payload).unwrap();
    let mut rx = M17Receiver::new(sr).unwrap();
    let mut asm = PacketAssembler::new();
    let mut got = None;
    let mut i = 0usize;
    while let Some(s) = tx.next_i16() {
        let s = mangle(i, s);
        i += 1;
        match rx.push_i16(s) {
            Some(M17FrameEvent::Lsf(l)) => asm.start(l),
            Some(M17FrameEvent::PacketFrame(f)) => {
                if let Some(p) = asm.feed(&f) {
                    got = Some(p.to_vec());
                }
            }
            None => {}
        }
    }
    got
}

#[test]
fn audio_roundtrip_short_packet() {
    let payload = b"Hello, M17!";
    assert_eq!(roundtrip(payload, |_, s| s).as_deref(), Some(&payload[..]));
}

#[test]
fn audio_roundtrip_multi_frame_packet() {
    // 3 full frames + a short EOF frame.
    let payload: Vec<u8> = (0..90u8)
        .map(|i| i.wrapping_mul(7).wrapping_add(1))
        .collect();
    assert_eq!(roundtrip(&payload, |_, s| s).as_deref(), Some(&payload[..]));
}

#[test]
fn audio_roundtrip_max_single_frame_boundary() {
    // Exactly 23 payload bytes + 2 CRC = one full 25-byte frame.
    let payload = [0xA5u8; 23];
    assert_eq!(roundtrip(&payload, |_, s| s).as_deref(), Some(&payload[..]));
}

#[test]
fn audio_roundtrip_survives_symbol_errors() {
    // Slam a few isolated symbol centers to zero: within the conv
    // code's correction capacity when spread across the frame.
    let payload = b"FEC carries this packet through corrupted symbols";
    let got = roundtrip(payload, |i, s| {
        // Corrupt one sample every ~600 (≈ every 60th symbol) in the
        // payload region past the preamble.
        if i > 2_500 && i % 600 == 0 { 0 } else { s }
    });
    assert_eq!(got.as_deref(), Some(&payload[..]));
}

#[test]
fn audio_roundtrip_survives_additive_noise() {
    // Deterministic pseudo-noise, modest amplitude relative to ±3
    // symbol peaks (~30000 FS).
    let payload = b"noise tolerance check";
    let mut seed = 0x1234_5678u32;
    let mut noise = move || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        ((seed >> 16) as i16 as i32 / 24) as i16
    };
    let noise_vec: Vec<i16> = (0..200_000).map(|_| noise()).collect();
    let got = roundtrip(payload, |i, s| {
        s.saturating_add(noise_vec[i % noise_vec.len()])
    });
    assert_eq!(got.as_deref(), Some(&payload[..]));
}

#[test]
fn receiver_rejects_garbage_and_silence() {
    let sr = SampleRate::new(48_000).unwrap();
    let mut rx = M17Receiver::new(sr).unwrap();
    // Silence.
    for _ in 0..48_000 {
        assert_eq!(rx.push_i16(0), None);
    }
    // Deterministic garbage: no frame event may carry a valid LSF, and
    // no assembled packet may appear.
    let mut asm = PacketAssembler::new();
    let mut seed = 0xDEAD_BEEFu32;
    let mut completed = 0;
    for _ in 0..96_000 {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let s = (seed >> 16) as i16;
        match rx.push_i16(s / 2) {
            Some(M17FrameEvent::Lsf(l)) => asm.start(l),
            Some(M17FrameEvent::PacketFrame(f)) if asm.feed(&f).is_some() => {
                completed += 1;
            }
            Some(_) => {}
            None => {}
        }
    }
    assert_eq!(completed, 0, "garbage must not produce a CRC-valid packet");
}

#[test]
fn tx_rejects_oversized_payload() {
    let lsf = Lsf::packet_data(
        Address::broadcast(),
        Address::from_callsign("N0CALL").unwrap(),
        0,
    );
    let sr = SampleRate::new(48_000).unwrap();
    let big = vec![0u8; MAX_PACKET_PAYLOAD + 1];
    assert_eq!(
        M17PacketTx::new(sr, lsf, &big).err(),
        Some(M17Error::PayloadTooLong {
            len: MAX_PACKET_PAYLOAD + 1
        })
    );
}

#[test]
fn tx_rejects_inexact_sample_rate() {
    let lsf = Lsf::packet_data(
        Address::broadcast(),
        Address::from_callsign("N0CALL").unwrap(),
        0,
    );
    let sr = SampleRate::new(44_100).unwrap();
    assert_eq!(
        M17PacketTx::new(sr, lsf, b"x").err(),
        Some(M17Error::SampleRateInexact { got: 44_100 })
    );
}
