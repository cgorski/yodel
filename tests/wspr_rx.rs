//! WSPR receive tests: clean roundtrips of our own modulator output,
//! noise-floor pinning, Fano cap behavior, and multi-signal captures.
//!
//! Sensitivity honesty: the −31 dB (in 2500 Hz) figure quoted for WSPR
//! belongs to the reference implementation's decoder. THIS decoder's
//! measured floor, pinned below, is **−22 dB** for a single signal at
//! the default configuration; the clean-failure assertion sits at
//! −30 dB. Any improvement or regression must move these pins
//! consciously.
#![cfg(all(feature = "wspr", feature = "std"))]

use warble::wspr::{
    FANO_DELTA, PACKED_LEN, SYMBOL_COUNT, WsprConfig, WsprDecoder, WsprDecoderConfig, WsprError,
    WsprMessage, WsprModulator, WsprRxError, deinterleave, fano_decode, interleave,
};
use warble::{MaidenheadGrid, SampleRate};

/// A locator from text, panicking on invalid input (locator parsing has
/// its own suite in `warble::geo`).
fn grid(text: &str) -> MaidenheadGrid {
    MaidenheadGrid::new(text).expect("valid locator")
}

/// One transmission's samples at 12 kHz with the given tone-0
/// frequency, embedded in a capture of `total` samples starting at
/// `start`, scaled by `amp` (of full scale).
fn synth(msg: &WsprMessage, base_hz: u32, start: usize, total: usize, amp: f64) -> Vec<i16> {
    let config = WsprConfig::new(base_hz, SampleRate::new(12_000).expect("rate")).expect("config");
    let tx = WsprModulator::for_message(config, msg);
    let mut out = vec![0i16; total];
    for (i, s) in tx.enumerate() {
        if let Some(slot) = out.get_mut(start + i) {
            *slot = (f64::from(s) * amp) as i16;
        }
    }
    out
}

/// A ~114 s capture length at 12 kHz.
const CAPTURE: usize = 114 * 12_000;

/// splitmix64 (crate precedent, tests/noise.rs): seeding + stream.
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Deterministic Gaussian-ish noise: sum of 4 uniforms (Irwin–Hall,
/// good enough for channel noise), zero mean, unit-ish variance.
struct Noise(u64);

impl Noise {
    fn next_gauss(&mut self) -> f64 {
        let mut acc = 0.0f64;
        for _ in 0..4 {
            let u = splitmix64(&mut self.0) >> 11;
            acc += u as f64 / (1u64 << 53) as f64;
        }
        // Sum of 4 U(0,1): mean 2, var 1/3. Normalize.
        (acc - 2.0) * (3.0f64).sqrt()
    }
}

/// Adds noise so the signal-to-noise ratio in the 2500 Hz reference
/// bandwidth is `snr_db`. The tone's power is `amp²/2` (full-scale
/// sine at `amp`); noise of std-dev σ spread over the 6000 Hz Nyquist
/// band leaves σ²·2500/6000 in the reference bandwidth.
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

fn decoder() -> WsprDecoder {
    WsprDecoder::new(WsprDecoderConfig::new(1_500, 100).expect("config"))
}

// ---- pure-math (no_std pieces) ----

#[test]
fn deinterleave_inverts_interleave() {
    let mut coded = [0u8; SYMBOL_COUNT];
    for (i, c) in coded.iter_mut().enumerate() {
        *c = (i % 2) as u8 ^ ((i / 3) % 2) as u8;
    }
    let channel = interleave(&coded);
    let mut back = [0u8; SYMBOL_COUNT];
    deinterleave(&channel, &mut back);
    assert_eq!(back, coded);
}

#[test]
fn unpack_inverts_pack() {
    for (call, locator, power) in [
        ("K1ABC", "FN42", 37),
        ("G4JNT", "IO90", 30),
        ("KA9XYZ", "EM10", 0),
        ("W1A", "AA00", 60),
    ] {
        let msg = WsprMessage::new(call, grid(locator), power).expect("valid message");
        let packed = msg.pack();
        assert_eq!(WsprMessage::unpack(&packed).expect("unpack"), msg);
    }
}

#[test]
fn unpack_rejects_garbage_tail_bits() {
    let msg = WsprMessage::new("K1ABC", grid("FN42"), 37).expect("valid");
    let mut packed = msg.pack();
    packed[10] = 0x01; // bits past bit 50 must be zero
    assert_eq!(WsprMessage::unpack(&packed), Err(WsprError::UnpackInvalid));
}

#[test]
fn fano_decodes_ideal_metrics() {
    let msg = WsprMessage::new("N0CAL", grid("JN58"), 23).expect("valid");
    let coded = warble::wspr::convolutional_encode(&msg.pack());
    let mut metrics = [[0i32; 2]; SYMBOL_COUNT];
    for (m, &c) in metrics.iter_mut().zip(coded.iter()) {
        m[usize::from(c)] = 8;
        m[usize::from(1 - c)] = -64;
    }
    let packed = fano_decode(&metrics, FANO_DELTA, 400_000).expect("decode");
    assert_eq!(packed, msg.pack());
}

#[test]
fn fano_cap_bounds_garbage_input() {
    // Adversarial metrics: everything looks equally plausible-bad, so
    // the search cannot finish and must hit its cap, not hang.
    let mut metrics = [[0i32; 2]; SYMBOL_COUNT];
    let mut seed = 42u64;
    for m in metrics.iter_mut() {
        let r = splitmix64(&mut seed);
        m[0] = -((r % 24) as i32);
        m[1] = -(((r >> 32) % 24) as i32);
    }
    let start = std::time::Instant::now();
    let err = fano_decode(&metrics, FANO_DELTA, 50_000);
    assert!(start.elapsed() < std::time::Duration::from_secs(5));
    match err {
        Err(e) => assert!(e.to_string().contains("node-visit cap")),
        Ok(_) => panic!("garbage metrics should not decode"),
    }
}

// ---- decoder config validation ----

#[test]
fn config_rejects_bad_windows_and_caps() {
    assert!(matches!(
        WsprDecoderConfig::new(1_500, 5),
        Err(WsprRxError::WindowInvalid { .. })
    ));
    assert!(matches!(
        WsprDecoderConfig::new(5_950, 100),
        Err(WsprRxError::WindowInvalid { .. })
    ));
    let ok = WsprDecoderConfig::new(1_500, 100).expect("valid");
    assert!(matches!(
        ok.max_candidates(0),
        Err(WsprRxError::CandidatesInvalid { got: 0 })
    ));
    assert!(matches!(ok.fano_cap(0), Err(WsprRxError::CapInvalid)));
}

#[test]
fn decode_rejects_short_capture() {
    let err = decoder().decode(&[0i16; 1000]);
    assert!(matches!(err, Err(WsprRxError::CaptureTooShort { .. })));
}

// ---- clean roundtrips ----

#[test]
fn clean_roundtrip_center() {
    let msg = WsprMessage::new("K1ABC", grid("FN42"), 37).expect("valid");
    let capture = synth(&msg, 1_500, 12_000, CAPTURE, 0.5);
    let decodes = decoder().decode(&capture).expect("decode");
    assert_eq!(decodes.len(), 1, "expected one decode: {decodes:?}");
    assert_eq!(decodes[0].message, msg);
    assert!((decodes[0].freq_hz - 1_500.0).abs() < 1.0, "{decodes:?}");
    assert!((decodes[0].dt_seconds - 1.0).abs() < 0.3, "{decodes:?}");
}

#[test]
fn clean_roundtrip_band_edges_and_messages() {
    // Different callsigns/grids/powers at different band offsets.
    for (call, locator, power, base) in [
        ("G4JNT", "IO90", 30, 1_420u32),
        ("KA9XYZ", "EM10", 0, 1_580),
        ("W1A", "AA00", 60, 1_447),
    ] {
        let msg = WsprMessage::new(call, grid(locator), power).expect("valid");
        let capture = synth(&msg, base, 6_000, CAPTURE, 0.4);
        let decodes = decoder().decode(&capture).expect("decode");
        assert_eq!(decodes.len(), 1, "{call} at {base} Hz: {decodes:?}");
        assert_eq!(decodes[0].message, msg, "{call} at {base} Hz");
    }
}

#[test]
fn clean_roundtrip_fractional_freq_and_time_offset() {
    // +1.7 Hz off the grid (the modulator only takes integer Hz, so
    // synthesize at 1502 Hz and decode — 2 Hz is already off-grid vs
    // the 1.4648 Hz spacing) plus a +0.3 s time offset.
    let msg = WsprMessage::new("N0CAL", grid("JN58"), 23).expect("valid");
    let start = 12_000 + 3_600; // +0.3 s beyond the nominal 1 s
    let capture = synth(&msg, 1_502, start, CAPTURE, 0.5);
    let decodes = decoder().decode(&capture).expect("decode");
    assert_eq!(decodes.len(), 1, "{decodes:?}");
    assert_eq!(decodes[0].message, msg);
    assert!((decodes[0].freq_hz - 1_502.0).abs() < 1.0, "{decodes:?}");
    assert!((decodes[0].dt_seconds - 1.3).abs() < 0.3, "{decodes:?}");
}

// ---- noise floor (the measured sensitivity pins) ----

/// PINNED: our decoder copies a single signal at −22 dB SNR in the
/// 2500 Hz reference bandwidth (measured; the reference decoder's
/// −31 dB belongs to that implementation, not this one).
#[test]
fn noise_decode_succeeds_at_pinned_snr() {
    let msg = WsprMessage::new("K1ABC", grid("FN42"), 37).expect("valid");
    let mut capture = synth(&msg, 1_500, 12_000, CAPTURE, 0.25);
    add_noise(&mut capture, 0.25, -22.0, 7);
    let decodes = decoder().decode(&capture).expect("decode");
    assert!(
        decodes.iter().any(|d| d.message == msg),
        "no decode at -22 dB: {decodes:?}"
    );
}

/// Well below the pinned floor the decoder fails cleanly (no decode,
/// no panic, bounded time) rather than hallucinating a message.
#[test]
fn noise_decode_fails_cleanly_below_floor() {
    let msg = WsprMessage::new("K1ABC", grid("FN42"), 37).expect("valid");
    let mut capture = synth(&msg, 1_500, 12_000, CAPTURE, 0.25);
    add_noise(&mut capture, 0.25, -30.0, 11);
    let decodes = decoder().decode(&capture).expect("decode");
    assert!(
        decodes.iter().all(|d| d.message != msg),
        "unexpected decode at -30 dB"
    );
}

// ---- multi-signal ----

#[test]
fn two_signals_in_one_capture_both_decode() {
    let a = WsprMessage::new("K1ABC", grid("FN42"), 37).expect("valid");
    let b = WsprMessage::new("G4JNT", grid("IO90"), 30).expect("valid");
    let cap_a = synth(&a, 1_460, 12_000, CAPTURE, 0.35);
    let cap_b = synth(&b, 1_540, 9_000, CAPTURE, 0.30);
    let capture: Vec<i16> = cap_a
        .iter()
        .zip(cap_b.iter())
        .map(|(&x, &y)| x.saturating_add(y))
        .collect();
    let decodes = decoder().decode(&capture).expect("decode");
    let msgs: Vec<_> = decodes.iter().map(|d| d.message).collect();
    assert!(msgs.contains(&a), "missing signal A: {decodes:?}");
    assert!(msgs.contains(&b), "missing signal B: {decodes:?}");
}

// ---- quality metrics sanity ----

#[test]
fn snr_estimate_tracks_actual_snr() {
    let msg = WsprMessage::new("K1ABC", grid("FN42"), 37).expect("valid");
    let mut capture = synth(&msg, 1_500, 12_000, CAPTURE, 0.25);
    add_noise(&mut capture, 0.25, -15.0, 3);
    let decodes = decoder().decode(&capture).expect("decode");
    let d = decodes
        .iter()
        .find(|d| d.message == msg)
        .expect("decode at -15 dB");
    assert!(
        (d.snr_db - -15.0).abs() < 6.0,
        "SNR estimate {} far from -15 dB",
        d.snr_db
    );
    assert!(d.sync_score > 0.0);
    let _ = PACKED_LEN; // silence unused-import if assertions change
}
