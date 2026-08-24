//! FT8 receive tests: clean roundtrips of our own modulator output,
//! noise-floor pinning, LDPC decoder unit tests, CRC rejection, and
//! multi-signal captures.
//!
//! Sensitivity honesty: the −21 dB (in 2500 Hz) figure quoted for FT8
//! belongs to the reference implementation's decoder. THIS
//! decoder's measured floor, pinned below, is **−14 dB** for a single
//! signal at the default configuration (10/10 seeds still decode at
//! −16 dB; −18 dB is marginal); the clean-failure assertion sits at
//! −24 dB. Any improvement or regression must move these pins
//! consciously.
#![cfg(all(feature = "ft8", feature = "std"))]

use warble::SampleRate;
use warble::ft8::{
    CHECK_ROWS, CODEWORD_BITS, Ft8Config, Ft8Decoder, Ft8DecoderConfig, Ft8Error, Ft8Message,
    Ft8Modulator, Ft8RxError, Ft8Tail, LDPC_MAX_ITERS, PARITY_BITS, add_crc, ldpc_check,
    ldpc_decode, ldpc_encode, message_from_codeword, unpack_message, verify_crc,
};

/// A grid trailer from locator text, panicking on invalid input (locator
/// parsing has its own suite in `warble::geo`).
fn grid(text: &str) -> Ft8Tail {
    Ft8Tail::grid(text).expect("valid locator")
}

/// One transmission's samples at 12 kHz with the given tone-0
/// frequency, embedded in a capture of `total` samples starting at
/// `start`, scaled by `amp` (of full scale).
fn synth(msg: &Ft8Message, base_hz: u32, start: usize, total: usize, amp: f64) -> Vec<i16> {
    let config = Ft8Config::new(base_hz, SampleRate::new(12_000).expect("rate")).expect("config");
    let tx = Ft8Modulator::for_message(config, msg);
    let mut out = vec![0i16; total];
    for (i, s) in tx.enumerate() {
        if let Some(slot) = out.get_mut(start + i) {
            *slot = (f64::from(s) * amp) as i16;
        }
    }
    out
}

/// A 15 s capture length at 12 kHz (one full FT8 cycle).
const CAPTURE: usize = 15 * 12_000;

/// splitmix64 (crate precedent, tests/noise.rs): seeding + stream.
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Deterministic Gaussian-ish noise: sum of 4 uniforms (Irwin–Hall),
/// zero mean, unit-ish variance.
struct Noise(u64);

impl Noise {
    fn next_gauss(&mut self) -> f64 {
        let mut acc = 0.0f64;
        for _ in 0..4 {
            let u = splitmix64(&mut self.0) >> 11;
            acc += u as f64 / (1u64 << 53) as f64;
        }
        (acc - 2.0) * (3.0f64).sqrt()
    }
}

/// Adds noise so the signal-to-noise ratio in the 2500 Hz reference
/// bandwidth is `snr_db`. The tone's power is `amp²/2`; noise of
/// std-dev σ spread over the 6000 Hz Nyquist band leaves σ²·2500/6000
/// in the reference bandwidth.
fn add_noise(samples: &mut [i16], amp: f64, snr_db: f64, seed: u64) {
    let sig_power = amp * amp * 32767.0 * 32767.0 / 2.0;
    let noise_in_ref = sig_power / 10f64.powf(snr_db / 10.0);
    let sigma = (noise_in_ref * 6000.0 / 2500.0).sqrt();
    let mut rng = Noise(seed);
    for s in samples.iter_mut() {
        let v = f64::from(*s) + rng.next_gauss() * sigma;
        *s = v.round().clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16;
    }
}

fn decoder() -> Ft8Decoder {
    Ft8Decoder::new(Ft8DecoderConfig::new(1_500, 300).expect("config"))
}

// ---- pure-math (no_std pieces): the sparse H table ----

#[test]
fn check_rows_are_orthogonal_to_codewords() {
    // Every CHECK_ROWS row must annihilate every codeword: proven over
    // random payloads (and this transitively cross-verifies the
    // derived sparse H against the embedded generator).
    let mut seed = 0xC0DE_C0DE_C0DEu64;
    for _ in 0..100 {
        let mut payload = [0u8; warble::ft8::PAYLOAD_LEN];
        for b in payload.iter_mut() {
            *b = (splitmix64(&mut seed) >> 32) as u8;
        }
        payload[9] &= 0xF8;
        let codeword = ldpc_encode(&add_crc(&payload));
        for (r, row) in CHECK_ROWS.iter().enumerate() {
            let mut parity = 0u8;
            for &v in row {
                if v != 255 {
                    let pos = usize::from(v);
                    parity ^= (codeword[pos / 8] >> (7 - pos % 8)) & 1;
                }
            }
            assert_eq!(parity, 0, "check {r} fails on a valid codeword");
        }
    }
}

#[test]
fn check_rows_shape() {
    // Column weight exactly 3 over all 174 bits; row weights 6 or 7.
    let mut cover = [0u32; 174];
    for row in &CHECK_ROWS {
        let w = row.iter().filter(|&&v| v != 255).count();
        assert!(w == 6 || w == 7, "row weight {w}");
        for &v in row {
            if v != 255 {
                cover[usize::from(v)] += 1;
            }
        }
    }
    assert!(cover.iter().all(|&c| c == 3));
    assert_eq!(CHECK_ROWS.len(), PARITY_BITS);
}

// ---- LDPC min-sum unit tests ----

/// Ideal LLRs for a codeword: +4 for bit 0, −4 for bit 1.
fn ideal_llrs(codeword: &[u8; warble::ft8::CODEWORD_LEN]) -> [f32; CODEWORD_BITS] {
    let mut llr = [0.0f32; CODEWORD_BITS];
    for (pos, slot) in llr.iter_mut().enumerate() {
        let bit = (codeword[pos / 8] >> (7 - pos % 8)) & 1;
        *slot = if bit == 0 { 4.0 } else { -4.0 };
    }
    llr
}

#[test]
fn ldpc_decodes_clean_llrs() {
    let msg = Ft8Message::standard("CQ", "K1ABC", false, grid("FN42")).unwrap();
    let codeword = ldpc_encode(&add_crc(&msg.payload()));
    let llr = ideal_llrs(&codeword);
    let decoded = ldpc_decode(&llr).expect("clean decode");
    assert_eq!(decoded, codeword);
    assert_eq!(ldpc_check(&decoded), 0);
}

#[test]
fn ldpc_corrects_flipped_bits() {
    // Flip k bits (erase-style: strong wrong LLR) and confirm the
    // decoder still recovers the codeword for small k.
    let msg = Ft8Message::free_text("LDPC FLIP").unwrap();
    let codeword = ldpc_encode(&add_crc(&msg.payload()));
    let mut seed = 99u64;
    for k in 1..=12usize {
        let mut llr = ideal_llrs(&codeword);
        let mut flipped = std::collections::BTreeSet::new();
        while flipped.len() < k {
            flipped.insert((splitmix64(&mut seed) % 174) as usize);
        }
        for &pos in &flipped {
            llr[pos] = -llr[pos];
        }
        let decoded = ldpc_decode(&llr).unwrap_or_else(|e| panic!("k={k}: {e}"));
        assert_eq!(decoded, codeword, "k={k}");
    }
}

#[test]
fn ldpc_iteration_cap_on_garbage() {
    // Random LLRs: no codeword nearby; the decoder must hit its cap
    // and return the rich error, in bounded time.
    let mut seed = 0xBAD_5EEDu64;
    let mut llr = [0.0f32; CODEWORD_BITS];
    for slot in llr.iter_mut() {
        let r = splitmix64(&mut seed);
        *slot = ((r % 2000) as f32 / 1000.0) - 1.0;
    }
    let start = std::time::Instant::now();
    let err = ldpc_decode(&llr);
    assert!(start.elapsed() < std::time::Duration::from_secs(1));
    match err {
        Err(Ft8Error::LdpcNotConverged) => {}
        other => panic!("expected LdpcNotConverged, got {other:?}"),
    }
    // The cap is a documented, sane constant.
    assert!((10..=100).contains(&LDPC_MAX_ITERS));
}

/// A non-finite LLR is rejected, not silently decoded as zeros.
///
/// The decoder's hard decision is `posterior < 0.0`. `NaN < 0.0` is
/// FALSE, so every NaN posterior read as bit 0 and the decoder happily
/// returned an all-zero-ish codeword built from nothing. CRC-14 catches
/// most of that downstream, but "the answer is wrong for a reason the
/// decoder could see" is not something to leave to a 1-in-16384 gate.
///
/// `llrs_from_energies` divides by a mean floored at `f32::MIN_POSITIVE`
/// (see `src/ft8.rs`), so a single infinite energy anywhere in the
/// symbol window produces NaN LLRs here.
#[test]
fn ldpc_rejects_non_finite_llrs() {
    let payload = Ft8Message::standard("CQ", "K1ABC", false, grid("FN42"))
        .unwrap()
        .payload();
    let codeword = ldpc_encode(&add_crc(&payload));

    // A clean LLR set built from a real codeword decodes, so the only
    // difference below is the poisoned value.
    let clean = ideal_llrs(&codeword);
    assert!(ldpc_decode(&clean).is_ok(), "the control must decode");

    for (name, poison) in [
        ("NaN", f32::NAN),
        ("+inf", f32::INFINITY),
        ("-inf", f32::NEG_INFINITY),
    ] {
        // One poisoned bit is enough: it is one damaged symbol.
        for position in [0usize, 90, CODEWORD_BITS - 1] {
            let mut llr = clean;
            llr[position] = poison;
            match ldpc_decode(&llr) {
                Err(Ft8Error::LlrNotFinite) => {}
                other => panic!("{name} at bit {position} must be refused, got {other:?}"),
            }
        }
    }
}

// ---- CRC gate ----

#[test]
fn crc_rejects_wrong_codeword_convergence() {
    // Take a valid message, flip one payload bit AFTER encoding the
    // CRC (simulating an LDPC convergence to a near codeword whose
    // payload disagrees with its CRC): verify_crc must reject it.
    let msg = Ft8Message::standard("K1ABC", "W9XYZ", true, Ft8Tail::Report(-8)).unwrap();
    let mut message = add_crc(&msg.payload());
    message[3] ^= 0x10; // a payload bit
    assert_eq!(verify_crc(&message), Err(Ft8Error::CrcMismatch));
    // Untampered: accepted, payload preserved.
    let message = add_crc(&msg.payload());
    assert_eq!(verify_crc(&message), Ok(msg.payload()));
}

// ---- message unpack (inverse of TX packing) ----

#[test]
fn unpack_renders_standard_and_free_text() {
    for (msg, expected) in [
        (
            Ft8Message::standard("CQ", "K1ABC", false, grid("FN42")).unwrap(),
            "CQ K1ABC FN42",
        ),
        (
            Ft8Message::standard("K1ABC", "W9XYZ", true, Ft8Tail::Report(-8)).unwrap(),
            "K1ABC W9XYZ R-08",
        ),
        (
            Ft8Message::standard("K1ABC", "W9XYZ", false, Ft8Tail::Report(3)).unwrap(),
            "K1ABC W9XYZ +03",
        ),
        (
            Ft8Message::standard("QRZ", "KA9XYZ", false, Ft8Tail::Rr73).unwrap(),
            "QRZ KA9XYZ RR73",
        ),
        (
            Ft8Message::standard("K1ABC", "W9XYZ", false, Ft8Tail::None).unwrap(),
            "K1ABC W9XYZ",
        ),
        (
            Ft8Message::standard("DE", "ZZ9ZZZ", false, Ft8Tail::Seventy3).unwrap(),
            "DE ZZ9ZZZ 73",
        ),
        (
            Ft8Message::free_text("TNX BOB 73 GL").unwrap(),
            "TNX BOB 73 GL",
        ),
        (Ft8Message::free_text("HI").unwrap(), "HI"),
    ] {
        let text = unpack_message(&msg.payload()).expect("unpack");
        assert_eq!(text.as_str(), expected);
    }
}

#[test]
fn unpack_rejects_unsupported_types() {
    // i3 = 2 (EU VHF contest): rejected with the rich error.
    let mut payload = [0u8; warble::ft8::PAYLOAD_LEN];
    payload[9] = 2 << 3;
    assert_eq!(
        unpack_message(&payload),
        Err(Ft8Error::UnsupportedMessageType)
    );
    // i3 = 0, n3 = 1 (DXpedition): rejected.
    let mut payload = [0u8; warble::ft8::PAYLOAD_LEN];
    payload[9] = 0x40; // n3 = 1
    assert_eq!(
        unpack_message(&payload),
        Err(Ft8Error::UnsupportedMessageType)
    );
}

// ---- decoder config validation ----

#[test]
fn config_rejects_bad_windows_and_candidates() {
    assert!(matches!(
        Ft8DecoderConfig::new(1_500, 20),
        Err(Ft8RxError::WindowInvalid { .. })
    ));
    assert!(matches!(
        Ft8DecoderConfig::new(5_900, 300),
        Err(Ft8RxError::WindowInvalid { .. })
    ));
    let ok = Ft8DecoderConfig::new(1_500, 300).expect("valid");
    assert!(matches!(
        ok.max_candidates(0),
        Err(Ft8RxError::CandidatesInvalid { got: 0 })
    ));
    assert!(matches!(
        ok.max_candidates(33),
        Err(Ft8RxError::CandidatesInvalid { got: 33 })
    ));
}

#[test]
fn decode_rejects_short_capture() {
    let err = decoder().decode(&[0i16; 1000]);
    assert!(matches!(err, Err(Ft8RxError::CaptureTooShort { .. })));
}

// ---- clean roundtrips ----

#[test]
fn clean_roundtrip_cq_center() {
    let msg = Ft8Message::standard("CQ", "K1ABC", false, grid("FN42")).unwrap();
    let capture = synth(&msg, 1_500, 6_000, CAPTURE, 0.5);
    let decodes = decoder().decode(&capture).expect("decode");
    assert_eq!(decodes.len(), 1, "expected one decode: {decodes:?}");
    assert_eq!(decodes[0].message.as_str(), "CQ K1ABC FN42");
    assert_eq!(decodes[0].payload, msg.payload());
    assert!((decodes[0].freq_hz - 1_500.0).abs() < 2.0, "{decodes:?}");
    assert!((decodes[0].dt_seconds - 0.5).abs() < 0.15, "{decodes:?}");
    assert!(decodes[0].sync_score > 0.5, "{decodes:?}");
}

#[test]
fn clean_roundtrip_band_and_messages() {
    // Standard exchange, free text, across the audio band.
    for (msg, base) in [
        (
            Ft8Message::standard("K1ABC", "W9XYZ", true, Ft8Tail::Report(-8)).unwrap(),
            1_210u32,
        ),
        (
            Ft8Message::standard("QRZ", "KA9XYZ", false, Ft8Tail::Rr73).unwrap(),
            1_790,
        ),
        (Ft8Message::free_text("TNX BOB 73 GL").unwrap(), 1_502),
    ] {
        let capture = synth(&msg, base, 12_000, CAPTURE, 0.4);
        let decodes = decoder().decode(&capture).expect("decode");
        assert_eq!(decodes.len(), 1, "base {base} Hz: {decodes:?}");
        assert_eq!(decodes[0].payload, msg.payload(), "base {base} Hz");
    }
}

#[test]
fn clean_roundtrip_fractional_freq_and_time_offset() {
    // The modulator takes integer Hz; a fractional tone offset is
    // synthesized by decoding with a shifted window center: generate
    // at 1500 Hz and search around 1503 Hz ⇒ the signal sits at
    // −3.1 Hz off every search grid line. Plus a +0.4 s time offset.
    let msg = Ft8Message::standard("CQ", "N0CAL", false, grid("JN58")).unwrap();
    let start = 6_000 + 4_800; // +0.4 s beyond the nominal 0.5 s
    let capture = synth(&msg, 1_503, start, CAPTURE, 0.5);
    let decodes = decoder().decode(&capture).expect("decode");
    assert_eq!(decodes.len(), 1, "{decodes:?}");
    assert_eq!(decodes[0].payload, msg.payload());
    assert!((decodes[0].freq_hz - 1_503.0).abs() < 2.0, "{decodes:?}");
    assert!((decodes[0].dt_seconds - 0.9).abs() < 0.15, "{decodes:?}");
}

// ---- noise floor (the measured sensitivity pins) ----

/// PINNED: our decoder copies a single signal at −14 dB SNR in the
/// 2500 Hz reference bandwidth (measured across seeds: 10/10 decodes
/// at −16 dB, marginal at −18 dB — the pin sits at −14 dB for margin;
/// the reference decoder's −21 dB belongs to that implementation, not
/// this one).
#[test]
fn noise_decode_succeeds_at_pinned_snr() {
    let msg = Ft8Message::standard("CQ", "K1ABC", false, grid("FN42")).unwrap();
    let mut capture = synth(&msg, 1_500, 6_000, CAPTURE, 0.25);
    add_noise(&mut capture, 0.25, -14.0, 7);
    let decodes = decoder().decode(&capture).expect("decode");
    assert!(
        decodes.iter().any(|d| d.payload == msg.payload()),
        "no decode at -14 dB: {decodes:?}"
    );
}

/// Well below the pinned floor the decoder fails cleanly (no decode,
/// no panic, bounded time) rather than hallucinating a message.
#[test]
fn noise_decode_fails_cleanly_below_floor() {
    let msg = Ft8Message::standard("CQ", "K1ABC", false, grid("FN42")).unwrap();
    let mut capture = synth(&msg, 1_500, 6_000, CAPTURE, 0.25);
    add_noise(&mut capture, 0.25, -24.0, 11);
    let decodes = decoder().decode(&capture).expect("decode");
    assert!(
        decodes.iter().all(|d| d.payload != msg.payload()),
        "unexpected decode at -24 dB"
    );
}

// ---- multi-signal ----

#[test]
fn two_signals_in_one_capture_both_decode() {
    let a = Ft8Message::standard("CQ", "K1ABC", false, grid("FN42")).unwrap();
    let b = Ft8Message::standard("K1ABC", "W9XYZ", false, Ft8Tail::Report(-3)).unwrap();
    let cap_a = synth(&a, 1_320, 6_000, CAPTURE, 0.35);
    let cap_b = synth(&b, 1_680, 9_600, CAPTURE, 0.30);
    let capture: Vec<i16> = cap_a
        .iter()
        .zip(cap_b.iter())
        .map(|(&x, &y)| x.saturating_add(y))
        .collect();
    let decodes = decoder().decode(&capture).expect("decode");
    let payloads: Vec<_> = decodes.iter().map(|d| d.payload).collect();
    assert!(payloads.contains(&a.payload()), "missing A: {decodes:?}");
    assert!(payloads.contains(&b.payload()), "missing B: {decodes:?}");
}

// ---- quality metrics sanity ----

#[test]
fn snr_estimate_tracks_actual_snr() {
    let msg = Ft8Message::standard("CQ", "K1ABC", false, grid("FN42")).unwrap();
    let mut capture = synth(&msg, 1_500, 6_000, CAPTURE, 0.25);
    add_noise(&mut capture, 0.25, -5.0, 3);
    let decodes = decoder().decode(&capture).expect("decode");
    let d = decodes
        .iter()
        .find(|d| d.payload == msg.payload())
        .expect("decode at -5 dB");
    assert!(
        (d.snr_db - -5.0).abs() < 6.0,
        "SNR estimate {} far from -5 dB",
        d.snr_db
    );
    assert!(d.sync_score > 0.0);
    let _ = message_from_codeword(&[0u8; warble::ft8::CODEWORD_LEN]);
}
