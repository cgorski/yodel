//! G3RUH 9600-baud end-to-end tests: TNC transmit → baseband PCM →
//! TNC receive, plus TX pipeline-order pinning.
#![cfg(all(feature = "tnc", feature = "g3ruh"))]

use warble::ax25::Address;
use warble::nrzi;
use warble::tnc::{DefaultTncReceiver, TncConfig, TncTransmitter};
use warble::{
    BasebandModulator, BaudRate, Bit, ModemProfile, ModulationScheme, SampleRate, Scrambler,
};

fn config(sr_hz: u32) -> TncConfig {
    let sr = SampleRate::new(sr_hz).unwrap();
    TncConfig::from_profile(sr, ModemProfile::G3RUH_9600).unwrap()
}

fn addr(callsign: &[u8], ssid: u8) -> Address {
    Address::new(callsign, ssid).unwrap()
}

/// Full round-trip at a given rate: N status frames TX'd through the
/// baseband path back-to-back, decoded back; returns the recovery count.
fn round_trip(sr_hz: u32, n: usize) -> usize {
    let cfg = config(sr_hz);
    let tx = TncTransmitter::new(cfg);
    let mut rx = DefaultTncReceiver::new(cfg).unwrap();
    let dest = addr(b"APRS", 0);
    let src = addr(b"N0CALL", 7);

    let mut recovered = 0;
    for i in 0..n {
        let mut text = *b"G3RUH loop frame 000";
        text[17] = b'0' + ((i / 100) % 10) as u8;
        text[18] = b'0' + ((i / 10) % 10) as u8;
        text[19] = b'0' + (i % 10) as u8;
        let packet_text = text;
        let mut frame_buf = [0u8; 330];
        let len = tx
            .build_frame_raw(dest, src, &[], &packet_text, &mut frame_buf)
            .unwrap();
        for s in tx.frame_samples_i16(&frame_buf[..len]) {
            if let Some(frame) = rx.push_i16(s) {
                assert_eq!(frame.src().callsign.as_bytes(), b"N0CALL");
                assert_eq!(frame.info(), &text);
                recovered += 1;
            }
        }
        // A short inter-frame gap of silence.
        for _ in 0..(sr_hz / 100) {
            if rx.push_i16(0).is_some() {
                recovered += 1;
            }
        }
    }
    recovered
}

/// Peak of a uniform noise source giving `snr_db` against a full-scale
/// signal. Mirrors the convention in `tests/noise.rs`.
fn noise_peak(snr_db: f64) -> f64 {
    let signal_rms = 32_767.0 / core::f64::consts::SQRT_2;
    let noise_rms = signal_rms / 10f64.powf(snr_db / 20.0);
    noise_rms * 3f64.sqrt()
}

/// Round trip with seeded additive noise; returns the recovery count.
fn round_trip_noisy(sr_hz: u32, n: usize, snr_db: f64, seed: u64) -> usize {
    let cfg = config(sr_hz);
    let tx = TncTransmitter::new(cfg);
    let mut rx = DefaultTncReceiver::new(cfg).unwrap();
    let dest = addr(b"APRS", 0);
    let src = addr(b"N0CALL", 7);

    // xorshift64*, so a failure reproduces exactly from the seed.
    let mut state = seed | 1;
    let mut noise = move || {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let v = state.wrapping_mul(0x2545_F491_4F6C_DD1D);
        ((v >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
    };
    let peak = noise_peak(snr_db);

    let mut recovered = 0;
    for i in 0..n {
        let mut text = *b"G3RUH noisy frame 000";
        text[18] = b'0' + ((i / 100) % 10) as u8;
        text[19] = b'0' + ((i / 10) % 10) as u8;
        text[20] = b'0' + (i % 10) as u8;
        let mut frame_buf = [0u8; 330];
        let len = tx
            .build_frame_raw(dest, src, &[], &text, &mut frame_buf)
            .unwrap();
        for s in tx.frame_samples_i16(&frame_buf[..len]) {
            let noisy = (f64::from(s) + noise() * peak).clamp(-32768.0, 32767.0) as i16;
            if let Some(frame) = rx.push_i16(noisy)
                && frame.info() == text
            {
                recovered += 1;
            }
        }
        for _ in 0..(sr_hz / 100) {
            let noisy = (noise() * peak).clamp(-32768.0, 32767.0) as i16;
            let _ = rx.push_i16(noisy);
        }
    }
    recovered
}

/// Coarse sensitivity floor for the 9600-baud baseband receiver.
///
/// This exists because a sensitivity deficit that cost nearly half the
/// achievable frames once sat undetected for several sessions: every
/// automated 9600 test used *clean* audio, where almost any decision
/// threshold works, and the only thing measuring noisy performance was
/// a manual script comparing against an external binary.
///
/// # What this test cannot do
///
/// It is labelled *coarse*. Transmitting and receiving
/// with our own modem measures a **matched** TX/RX pair, and a matched
/// pair is far more forgiving than a real channel: the receive filter
/// span could be cut to a third with no measurable loss here, while
/// costing real frames against an independently generated waveform.
/// A self-round-trip validates internal consistency, not the quality of
/// the receiver as an *interoperating* one.
///
/// So this catches catastrophic breakage (a wrong decision threshold
/// drops it to zero) but will not catch a few dB of lost sensitivity.
/// The sharp measurement is the reference-generator comparison in
/// `scripts/benchmark.sh`, whose numbers are pinned in
/// `docs/BENCHMARKS.md`; that needs external tooling, so it cannot gate
/// CI. Treat the two as complementary rather than redundant.
///
/// A ratchet: raise the floors when the receiver improves,
/// never lower them. They sit under the measured values so ordinary
/// seed-to-seed variation cannot fail the build.
#[test]
fn sensitivity_floor_under_noise() {
    const FRAMES: usize = 40;
    for (sr, snr_db, floor) in [
        // MEASURED 40/40 except 44.1 kHz at 4 dB, which gives 38/40.
        (44_100u32, 12.0f64, 37usize),
        (44_100, 9.0, 37),
        (44_100, 6.0, 36),
        (44_100, 4.0, 32),
        (48_000, 12.0, 37),
        (48_000, 9.0, 37),
        (48_000, 6.0, 36),
        (48_000, 4.0, 34),
    ] {
        let got = round_trip_noisy(sr, FRAMES, snr_db, 0x9600_0000 + u64::from(sr));
        assert!(
            got >= floor,
            "9600 baud at {sr} Hz, {snr_db} dB SNR: recovered {got} of {FRAMES}, floor {floor}"
        );
    }
}

#[test]
fn round_trip_100_frames_9600_baud_44100() {
    let recovered = round_trip(44_100, 100);
    assert!(recovered >= 99, "recovered only {recovered}/100");
}

#[test]
fn round_trip_20_frames_9600_baud_48000() {
    let recovered = round_trip(48_000, 20);
    assert!(recovered >= 19, "recovered only {recovered}/20");
}

#[test]
fn f32_path_round_trip_44100() {
    let cfg = config(44_100);
    let tx = TncTransmitter::new(cfg);
    let mut rx = DefaultTncReceiver::new(cfg).unwrap();
    let packet_text = b"f32 loop";
    let mut frame_buf = [0u8; 330];
    let len = tx
        .build_frame_raw(
            addr(b"APRS", 0),
            addr(b"N0CALL", 1),
            &[],
            packet_text,
            &mut frame_buf,
        )
        .unwrap();
    let mut got = 0;
    for s in tx.frame_samples_f32(&frame_buf[..len]) {
        if let Some(frame) = rx.push_f32(s) {
            assert_eq!(frame.info(), b"f32 loop");
            got += 1;
        }
    }
    assert_eq!(got, 1);
}

#[test]
fn profile_selects_baseband_scheme() {
    let cfg = config(44_100);
    assert_eq!(cfg.scheme(), ModulationScheme::ScrambledBaseband);
    assert_eq!(cfg.baud().bps(), 9_600);
    let tone = TncConfig::bell_202(SampleRate::new(44_100).unwrap()).unwrap();
    assert_eq!(tone.scheme(), ModulationScheme::ToneAfsk);
}

/// Pins the TX pipeline order: stuffed HDLC bits → NRZI → scrambler →
/// waveform synthesis. A known short bit pattern is pushed through
/// NRZI-then-scrambler at unit level, synthesized with the baseband
/// modulator, and compared against the samples produced by feeding the
/// scrambled-NRZI sequence to the modulator directly — i.e. the composed
/// stages, in that order, define the waveform exactly.
#[test]
fn tx_pipeline_order_is_nrzi_then_scramble_then_synthesis() {
    let sr = SampleRate::new(48_000).unwrap();
    let baud = BaudRate::new(9_600).unwrap();
    // 24 bits: long enough that the scrambler's 12/17-delay taps engage
    // (from zero state, scrambling is the identity for the first 12
    // bits) and NRZI produces a distinct sequence.
    let bits: Vec<Bit> = (0..24)
        .map(|i: u32| Bit::from((i * 5).is_multiple_of(3)))
        .collect();

    // Stage by stage, by hand: NRZI first, scrambler second.
    let nrzi_bits: Vec<Bit> = nrzi::encode_iter(bits.iter().copied()).collect();
    let scrambled: Vec<Bit> = Scrambler::default()
        .scramble_iter(nrzi_bits.iter().copied())
        .collect();
    // NRZI is not the identity here, and scrambling changes it again —
    // otherwise this test could not detect a swapped order.
    assert_ne!(nrzi_bits, bits);
    assert_ne!(scrambled, nrzi_bits);

    // The composed chain, as one iterator (the TNC's TX composition).
    let composed: Vec<i16> = BasebandModulator::new(sr, baud)
        .unwrap()
        .i16_samples(Scrambler::default().scramble_iter(nrzi::encode_iter(bits.iter().copied())))
        .collect();
    // Synthesis of the hand-derived scrambled-NRZI bits.
    let expected: Vec<i16> = BasebandModulator::new(sr, baud)
        .unwrap()
        .i16_samples(scrambled.iter().copied())
        .collect();
    assert_eq!(composed, expected);

    // And the wrong order (scramble before NRZI) yields different bits,
    // hence a different waveform.
    let swapped: Vec<Bit> =
        nrzi::encode_iter(Scrambler::default().scramble_iter(bits.iter().copied())).collect();
    assert_ne!(swapped, scrambled);
}

/// The receiver's stats must account the baseband path like the tone
/// path: one clean frame is one `frames_ok`.
#[test]
fn baseband_receiver_stats_count_frames() {
    let cfg = config(48_000);
    let tx = TncTransmitter::new(cfg);
    let mut rx = DefaultTncReceiver::new(cfg).unwrap();
    let packet_text = b"stats";
    let mut frame_buf = [0u8; 330];
    let len = tx
        .build_frame_raw(
            addr(b"APRS", 0),
            addr(b"N0CALL", 2),
            &[],
            packet_text,
            &mut frame_buf,
        )
        .unwrap();
    for s in tx.frame_samples_i16(&frame_buf[..len]) {
        let _ = rx.push_i16(s);
    }
    assert_eq!(rx.stats().frames_ok, 1);
}
