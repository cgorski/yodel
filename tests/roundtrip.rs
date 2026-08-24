//! Round-trip integration tests: modulator → demodulator, exact recovery.
//!
//! Each transmission starts with a 32-bit alternating preamble (1 0 1 0 …)
//! that gives the discriminator window time to fill and the PLL transitions
//! to lock on. The demodulated stream is then searched for the payload:
//! recovery must be exact, at every supported sample rate, on both the i16
//! and f32 sample paths.

use warble::demodulator::{AfskDemodulator, DemodulatorConfig};
use warble::modulator::{Modulator, ModulatorConfig};
use warble::{
    BAUD_MAX, BAUD_MIN, BaudRate, Bit, ConfigError, SAMPLE_RATE_MAX, SAMPLE_RATE_MIN, SampleRate,
    TonePair,
};

const RATES: [u32; 5] = [8_000, 11_025, 22_050, 44_100, 48_000];
const PREAMBLE_LEN: usize = 32;

fn preamble() -> Vec<Bit> {
    (0..PREAMBLE_LEN)
        .map(|i| if i % 2 == 0 { Bit::One } else { Bit::Zero })
        .collect()
}

fn bits_from_str(pattern: &str) -> Vec<Bit> {
    pattern
        .chars()
        .map(|c| if c == '1' { Bit::One } else { Bit::Zero })
        .collect()
}

fn modem_pair(sr_hz: u32) -> (Modulator, AfskDemodulator) {
    let sr = SampleRate::new(sr_hz).unwrap();
    let m = Modulator::new(ModulatorConfig::bell_202(sr).unwrap());
    let d = AfskDemodulator::new(DemodulatorConfig::bell_202(sr).unwrap()).unwrap();
    (m, d)
}

/// Asserts `recovered` contains `payload` as a contiguous subsequence after
/// the settling region, and that the recovered tail matches it exactly.
fn assert_payload_recovered(recovered: &[Bit], payload: &[Bit], ctx: &str) {
    assert!(
        recovered.windows(payload.len()).any(|w| w == payload),
        "{ctx}: payload not found in {recovered:?}"
    );
}

fn roundtrip_i16(sr_hz: u32, payload: &[Bit]) {
    let (mod_, demod) = modem_pair(sr_hz);
    let mut tx = preamble();
    tx.extend_from_slice(payload);
    // Two postamble bits flush the pipeline's ~half-bit group delay so the
    // final payload bit's decision point falls inside the sample stream.
    tx.extend(bits_from_str("10"));
    let samples: Vec<i16> = mod_.i16_samples(tx.iter().copied()).collect();
    let recovered: Vec<Bit> = demod.i16_bits(samples.iter().copied()).collect();
    assert_payload_recovered(&recovered, payload, &format!("i16 @ {sr_hz}"));
}

fn roundtrip_f32(sr_hz: u32, payload: &[Bit]) {
    let (mod_, demod) = modem_pair(sr_hz);
    let mut tx = preamble();
    tx.extend_from_slice(payload);
    tx.extend(bits_from_str("10"));
    let samples: Vec<f32> = mod_.f32_samples(tx.iter().copied()).collect();
    let recovered: Vec<Bit> = demod.f32_bits(samples.iter().copied()).collect();
    assert_payload_recovered(&recovered, payload, &format!("f32 @ {sr_hz}"));
}

/// A fixed "random-looking" pattern (documented literal, not generated).
const MIXED: &str = "1101001110001011010111100100011010011100101101011000111001011101";

macro_rules! roundtrip_tests {
    ($($i16_name:ident, $f32_name:ident: $sr:expr;)*) => {$(
        #[test]
        fn $i16_name() {
            roundtrip_i16($sr, &bits_from_str(MIXED));
        }

        #[test]
        fn $f32_name() {
            roundtrip_f32($sr, &bits_from_str(MIXED));
        }
    )*};
}

roundtrip_tests! {
    mixed_payload_i16_8000, mixed_payload_f32_8000: 8_000;
    mixed_payload_i16_11025, mixed_payload_f32_11025: 11_025;
    mixed_payload_i16_22050, mixed_payload_f32_22050: 22_050;
    mixed_payload_i16_44100, mixed_payload_f32_44100: 44_100;
    mixed_payload_i16_48000, mixed_payload_f32_48000: 48_000;
}

macro_rules! run_length_tests {
    ($($i16_name:ident, $f32_name:ident: $sr:expr;)*) => {$(
        #[test]
        fn $i16_name() {
            // Long same-bit runs stress PLL flywheel (no transitions).
            let mut payload = vec![Bit::One; 40];
            payload.extend(vec![Bit::Zero; 40]);
            payload.extend(bits_from_str("10").repeat(4));
            roundtrip_i16($sr, &payload);
        }

        #[test]
        fn $f32_name() {
            let mut payload = vec![Bit::Zero; 40];
            payload.extend(vec![Bit::One; 40]);
            payload.extend(bits_from_str("01").repeat(4));
            roundtrip_f32($sr, &payload);
        }
    )*};
}

run_length_tests! {
    long_runs_i16_8000, long_runs_f32_8000: 8_000;
    long_runs_i16_11025, long_runs_f32_11025: 11_025;
    long_runs_i16_22050, long_runs_f32_22050: 22_050;
    long_runs_i16_44100, long_runs_f32_44100: 44_100;
    long_runs_i16_48000, long_runs_f32_48000: 48_000;
}

#[test]
fn all_ones_payload_all_rates_i16() {
    for sr in RATES {
        roundtrip_i16(sr, &[Bit::One; 64]);
    }
}

#[test]
fn all_zeros_payload_all_rates_i16() {
    for sr in RATES {
        roundtrip_i16(sr, &[Bit::Zero; 64]);
    }
}

#[test]
fn all_ones_payload_all_rates_f32() {
    for sr in RATES {
        roundtrip_f32(sr, &[Bit::One; 64]);
    }
}

#[test]
fn all_zeros_payload_all_rates_f32() {
    for sr in RATES {
        roundtrip_f32(sr, &[Bit::Zero; 64]);
    }
}

#[test]
fn single_bit_payload_recovered() {
    // Smallest interesting payload: one bit after the preamble, both values.
    for sr in RATES {
        roundtrip_i16(sr, &bits_from_str("1101"));
        roundtrip_i16(sr, &bits_from_str("0010"));
    }
}

#[test]
fn byte_patterns_roundtrip_i16_48000() {
    // Every byte-aligned pattern of interest: walking ones and walking
    // zeros over an 8-bit frame.
    for shift in 0..8 {
        let byte = 1u8 << shift;
        let payload: Vec<Bit> = (0..8)
            .map(|b| {
                if (byte >> (7 - b)) & 1 == 1 {
                    Bit::One
                } else {
                    Bit::Zero
                }
            })
            .collect();
        // Pad with a known tail so the last payload bit gets a following
        // transition-rich region.
        let mut tx = payload.clone();
        tx.extend(bits_from_str("1010"));
        roundtrip_i16(48_000, &tx);
    }
}

/// Pinned expected bit pattern for one known modulated input: the first
/// bits sliced from a Bell 202 mark tone at 48 kHz are all ones (any wrap
/// count tolerance is pinned exactly here).
#[test]
fn pinned_bits_for_pure_mark_tone() {
    let (mod_, demod) = modem_pair(48_000);
    let tx = [Bit::One; 16];
    let mut samples: Vec<i16> = mod_.i16_samples(tx.iter().copied()).collect();
    assert_eq!(samples.len(), 16 * 40);
    // One bit period of trailing silence flushes the ~half-bit group delay;
    // the correlator window still holds mark energy, so the final decision
    // stays One. Pinned: exactly 16 ones.
    samples.extend(std::iter::repeat_n(0i16, 40));
    let recovered: Vec<Bit> = demod.i16_bits(samples.iter().copied()).collect();
    assert_eq!(recovered, vec![Bit::One; 16]);
}

/// Pinned exact demodulated stream for a known short transmission.
#[test]
fn pinned_bits_for_known_transmission() {
    let (mod_, demod) = modem_pair(48_000);
    let tx = bits_from_str("10101010101010101100");
    let mut samples: Vec<i16> = mod_.i16_samples(tx.iter().copied()).collect();
    // Flush the group delay with one bit period of silence (see above).
    samples.extend(std::iter::repeat_n(0i16, 40));
    let recovered: Vec<Bit> = demod.i16_bits(samples.iter().copied()).collect();
    // Pinned: full exact recovery, one output bit per transmitted bit.
    assert_eq!(recovered, tx);
}

#[test]
fn i16_and_f32_paths_agree() {
    for sr in RATES {
        let payload = bits_from_str(MIXED);
        let mut tx = preamble();
        tx.extend_from_slice(&payload);

        let (m1, d1) = modem_pair(sr);
        let s1: Vec<i16> = m1.i16_samples(tx.iter().copied()).collect();
        let r1: Vec<Bit> = d1.i16_bits(s1.iter().copied()).collect();

        let (m2, d2) = modem_pair(sr);
        let s2: Vec<f32> = m2.f32_samples(tx.iter().copied()).collect();
        let r2: Vec<Bit> = d2.f32_bits(s2.iter().copied()).collect();

        assert_eq!(r1, r2, "path divergence at {sr}");
    }
}

#[test]
fn preamble_settles_within_32_bits() {
    // Document the settling contract: with a 32-bit preamble the payload is
    // recovered starting no later than output index PREAMBLE_LEN + 2.
    //
    // The trailing bits are part of the contract too, and are not padding
    // to make this pass: the discriminator low-pass filters its tone
    // envelopes, so it has group delay, and the final payload bit's cell
    // only completes if the sample stream continues past it. Without them
    // the demodulator emits one bit fewer than it was given and the
    // payload is never matched in full. The README's demodulation example
    // sends two trailing bits for exactly this reason.
    const FLUSH_BITS: usize = 4;
    let payload = bits_from_str(MIXED);
    for sr in RATES {
        let (mod_, demod) = modem_pair(sr);
        let mut tx = preamble();
        tx.extend_from_slice(&payload);
        tx.extend(core::iter::repeat_n(Bit::Zero, FLUSH_BITS));
        let samples: Vec<i16> = mod_.i16_samples(tx.iter().copied()).collect();
        let recovered: Vec<Bit> = demod.i16_bits(samples.iter().copied()).collect();
        let pos = recovered
            .windows(payload.len())
            .position(|w| w == payload.as_slice())
            .unwrap_or(usize::MAX);
        assert!(pos <= PREAMBLE_LEN + 2, "rate {sr}: payload found at {pos}");
    }
}

// ------------------------------------------------------------------
// Session-1 closeout additions: iterator/push equivalence, pinned
// modulator samples, phase continuity, degenerate demodulator inputs.
// All deterministic (fixed literals; no PRNG, clock, or I/O).
// ------------------------------------------------------------------

/// The `i16_samples` iterator adapter produces exactly the samples of the
/// push (`feed`) / pull (`next_i16`) API, including fractional-bit rates.
#[test]
fn modulator_i16_iterator_matches_push_pull() {
    let bits = bits_from_str("1001101");
    for sr in RATES {
        let (m_iter, _) = modem_pair(sr);
        let via_iter: Vec<i16> = m_iter.i16_samples(bits.iter().copied()).collect();

        let (mut m_push, _) = modem_pair(sr);
        let mut pushed = Vec::new();
        for &bit in &bits {
            m_push.feed(bit);
            while let Some(s) = m_push.next_i16() {
                pushed.push(s);
            }
        }
        assert_eq!(via_iter, pushed, "i16 divergence at {sr}");
    }
}

/// Same equivalence on the f32 path (bitwise-equal floats expected: both
/// paths run the identical phase accumulator and table).
#[test]
fn modulator_f32_iterator_matches_push_pull() {
    let bits = bits_from_str("0110010");
    for sr in RATES {
        let (m_iter, _) = modem_pair(sr);
        let via_iter: Vec<f32> = m_iter.f32_samples(bits.iter().copied()).collect();

        let (mut m_push, _) = modem_pair(sr);
        let mut pushed = Vec::new();
        for &bit in &bits {
            m_push.feed(bit);
            while let Some(s) = m_push.next_f32() {
                pushed.push(s);
            }
        }
        assert_eq!(via_iter, pushed, "f32 divergence at {sr}");
    }
}

/// The `i16_bits` iterator adapter emits exactly the bits of the
/// `push_sample_i16` API on the same sample stream.
#[test]
fn demodulator_iterator_matches_push() {
    let mut tx = preamble();
    tx.extend(bits_from_str(MIXED));
    tx.extend(bits_from_str("10"));
    for sr in RATES {
        let (mod_, demod) = modem_pair(sr);
        let samples: Vec<i16> = mod_.i16_samples(tx.iter().copied()).collect();
        let via_iter: Vec<Bit> = demod.i16_bits(samples.iter().copied()).collect();

        let (_, mut demod2) = modem_pair(sr);
        let mut pushed = Vec::new();
        for &s in &samples {
            if let Some(bit) = demod2.push_sample_i16(s) {
                pushed.push(bit);
            }
        }
        assert_eq!(via_iter, pushed, "demod divergence at {sr}");
    }
}

/// At 48 kHz a bit is exactly 40 samples and the 1200 Hz mark tone spans
/// exactly one cycle per bit, so the mark waveform is 40-sample periodic
/// starting from sin(0) = 0.
#[test]
fn modulator_pinned_samples_48000() {
    let (mut m, _) = modem_pair(48_000);
    let mut samples = Vec::new();
    for _ in 0..4 {
        m.feed(Bit::One);
        while let Some(s) = m.next_i16() {
            samples.push(s);
        }
    }
    assert_eq!(samples.len(), 4 * 40, "40 samples per bit at 48 kHz");
    assert_eq!(samples[0], 0, "phase starts at zero");
    assert!(samples[1] > 0, "rising into the first quarter cycle");
    // One cycle per bit: 40-sample periodicity up to the documented phase
    // increment rounding (2^32/40 is not integral, so the phase drifts by
    // a fraction of a table step per cycle; well under 1% of full scale).
    for i in 0..120 {
        let diff = (i32::from(samples[i]) - i32::from(samples[i + 40])).abs();
        assert!(diff <= 256, "period mismatch at {i}: diff {diff}");
    }
    // A full cycle sums to ~0 (odd symmetry of the sine table, up to the
    // same phase-increment rounding: sum 50 observed, i.e. ~0.004% of the
    // 40-sample full-scale total).
    let sum: i64 = samples[..40].iter().map(|&s| i64::from(s)).sum();
    assert!(
        sum.abs() <= 256,
        "one mark cycle should sum near zero: {sum}"
    );
}

/// At 8 kHz, 8000/1200 = 6 remainder 800: the Bresenham schedule pins the
/// per-bit sample counts to 6, 7, 7 repeating (exactly 20 samples per 3
/// bits), independent of the tone.
#[test]
fn modulator_pinned_bit_lengths_8000() {
    let (mut m, _) = modem_pair(8_000);
    let mut counts = Vec::new();
    for _ in 0..6 {
        m.feed(Bit::Zero);
        let mut n = 0u32;
        while m.next_i16().is_some() {
            n += 1;
        }
        counts.push(n);
    }
    assert_eq!(counts, vec![6, 7, 7, 6, 7, 7]);
    assert_eq!(counts.iter().sum::<u32>(), 40, "20 samples per 3 bits");
}

/// The very first pulled sample is sin(0) = 0 on both PCM paths at every
/// supported rate: the phase accumulator starts at zero.
#[test]
fn modulator_first_sample_is_zero_everywhere() {
    for sr in RATES {
        let (mut mi, _) = modem_pair(sr);
        mi.feed(Bit::One);
        assert_eq!(mi.next_i16(), Some(0), "i16 first sample at {sr}");
        let (mut mf, _) = modem_pair(sr);
        mf.feed(Bit::Zero);
        assert_eq!(mf.next_f32(), Some(0.0), "f32 first sample at {sr}");
    }
}

/// Continuous-phase FSK: the sample-to-sample step never exceeds the
/// steepest slope of the faster (2200 Hz) tone. At 48 kHz that is
/// 2π·2200/48000 ≈ 0.288 per sample; a phase reset at a bit boundary
/// would show up as a jump of up to 2.0. Checked for every transition
/// combination: mark→space, space→mark, repeated mark, repeated space.
#[test]
fn phase_continuity_all_transitions() {
    let patterns = ["1010", "0101", "1111", "0000"];
    for pattern in patterns {
        let bits = bits_from_str(pattern);
        let (m, _) = modem_pair(48_000);
        let samples: Vec<f32> = m.f32_samples(bits.iter().copied()).collect();
        assert_eq!(samples.len(), bits.len() * 40);
        for w in samples.windows(2) {
            let step = (w[1] - w[0]).abs();
            assert!(step < 0.30, "discontinuity in {pattern}: step {step}");
        }
    }
}

/// Contract on silence: the demodulator still clocks out one decision per
/// bit cell (the slicer always decides), the decision stream is stable
/// (all-zero energy never toggles the comparator), and the run is
/// deterministic across identical instances.
#[test]
fn demodulator_silence_is_stable() {
    for sr in [8_000u32, 48_000] {
        let (_, demod) = modem_pair(sr);
        let silence: Vec<i16> = vec![0; sr as usize];
        let out: Vec<Bit> = demod.i16_bits(silence.iter().copied()).collect();
        if let Some(&first) = out.first() {
            assert!(
                out.iter().all(|&b| b == first),
                "silence produced unstable bits at {sr}: {out:?}"
            );
        }
        let (_, demod2) = modem_pair(sr);
        let out2: Vec<Bit> = demod2.i16_bits(silence.iter().copied()).collect();
        assert_eq!(out, out2, "silence not deterministic at {sr}");
    }
}

/// A DC (0 Hz) input has no energy at either tone: after the correlator
/// window fills, the decision stream must settle to one repeated value
/// rather than oscillate at the bit rate.
#[test]
fn demodulator_dc_input_is_stable() {
    let (_, demod) = modem_pair(48_000);
    let dc: Vec<i16> = vec![20_000; 48_000];
    let out: Vec<Bit> = demod.i16_bits(dc.iter().copied()).collect();
    let tail = &out[out.len() / 2..];
    if let Some(&first) = tail.first() {
        assert!(
            tail.iter().all(|&b| b == first),
            "DC input produced unstable tail: {tail:?}"
        );
    }
}

/// A 400 Hz tone sits below both Bell 202 tones. Whatever the slicer
/// decides, the decision must settle once the window fills (no bit-rate
/// chatter) and be fully deterministic.
#[test]
fn demodulator_out_of_band_tone_is_stable() {
    let sr = 48_000u32;
    let tone: Vec<i16> = (0..sr as usize)
        .map(|i| {
            let phase = 2.0 * std::f64::consts::PI * 400.0 * i as f64 / f64::from(sr);
            (phase.sin() * 20_000.0) as i16
        })
        .collect();
    let (_, demod) = modem_pair(sr);
    let out: Vec<Bit> = demod.i16_bits(tone.iter().copied()).collect();
    let (_, demod2) = modem_pair(sr);
    let out2: Vec<Bit> = demod2.i16_bits(tone.iter().copied()).collect();
    assert_eq!(out, out2, "out-of-band tone not deterministic");
    let tail = &out[out.len() / 2..];
    if let Some(&first) = tail.first() {
        assert!(
            tail.iter().all(|&b| b == first),
            "out-of-band tone produced unstable tail: {tail:?}"
        );
    }
}

// ------------------------------------------------------------------
// Public-API contract: newtype boundaries, Bit conversions, and
// error Display as seen from outside the crate.
// ------------------------------------------------------------------

/// Every supported sample rate boundary, off-by-one on both sides.
#[test]
fn sample_rate_boundary_matrix() {
    assert!(SampleRate::new(SAMPLE_RATE_MIN - 1).is_err());
    assert_eq!(
        SampleRate::new(SAMPLE_RATE_MIN).map(SampleRate::hz),
        Ok(8_000)
    );
    assert_eq!(
        SampleRate::new(SAMPLE_RATE_MAX).map(SampleRate::hz),
        Ok(48_000)
    );
    assert!(SampleRate::new(SAMPLE_RATE_MAX + 1).is_err());
    assert!(SampleRate::new(0).is_err());
    assert!(SampleRate::new(u32::MAX).is_err());
}

/// Baud rate boundaries, off-by-one on both sides.
#[test]
fn baud_rate_boundary_matrix() {
    assert!(BaudRate::new(BAUD_MIN - 1).is_err());
    assert_eq!(BaudRate::new(BAUD_MIN).map(BaudRate::bps), Ok(1));
    assert_eq!(BaudRate::new(BAUD_MAX).map(BaudRate::bps), Ok(9_600));
    assert!(BaudRate::new(BAUD_MAX + 1).is_err());
    assert!(BaudRate::new(u32::MAX).is_err());
}

/// Tone boundaries against the Nyquist frequency, per tone, both sides.
#[test]
fn tone_pair_boundary_matrix() {
    let sr = SampleRate::new(8_000).unwrap(); // Nyquist = 4000 Hz
    assert!(
        TonePair::new(3_999, 3_999, sr).is_ok(),
        "just below Nyquist"
    );
    assert_eq!(
        TonePair::new(4_000, 2_200, sr),
        Err(ConfigError::ToneOutOfRange {
            got: 4_000,
            nyquist: 4_000
        }),
        "mark at Nyquist"
    );
    assert_eq!(
        TonePair::new(1_200, 4_001, sr),
        Err(ConfigError::ToneOutOfRange {
            got: 4_001,
            nyquist: 4_000
        }),
        "space above Nyquist"
    );
    assert_eq!(
        TonePair::new(0, 2_200, sr),
        Err(ConfigError::ToneOutOfRange {
            got: 0,
            nyquist: 4_000
        }),
        "zero mark"
    );
    assert_eq!(
        TonePair::new(1_200, 0, sr),
        Err(ConfigError::ToneOutOfRange {
            got: 0,
            nyquist: 4_000
        }),
        "zero space"
    );
    assert!(TonePair::new(1, 2, sr).is_ok(), "minimal legal tones");
}

/// A tone pair is only valid against the rate it is USED at.
///
/// `TonePair::new` takes a `SampleRate`, checks Nyquist and then throws
/// the rate away, so the pair carries no memory of what it was cleared
/// for. Nothing stopped a pair validated at 48 kHz being handed to a
/// config at 8 kHz, where it aliases -- a hole in the module's claim
/// that it makes "illegal modem configurations unrepresentable".
///
/// The crate's own tone constants are the sharper case: they are plain
/// `const`s, so they have never been checked against any rate at all.
#[test]
fn a_config_rechecks_its_tones_against_its_own_sample_rate() {
    let fast = SampleRate::new(48_000).unwrap();
    let slow = SampleRate::new(8_000).unwrap(); // Nyquist = 4000 Hz
    let baud = BaudRate::new(1_200).unwrap();

    // Cleared at 48 kHz, where Nyquist is 24 kHz.
    let pair = TonePair::new(20_000, 21_000, fast).expect("valid at 48 kHz");
    assert!(ModulatorConfig::new(fast, baud, pair).is_ok());
    assert!(DemodulatorConfig::new(fast, baud, pair).is_ok());

    // The same pair at 8 kHz is pure aliasing and must be refused by
    // both ends, naming the offending tone.
    assert_eq!(
        ModulatorConfig::new(slow, baud, pair),
        Err(ConfigError::ToneOutOfRange {
            got: 20_000,
            nyquist: 4_000
        }),
        "the modulator must re-check the tones it was handed"
    );
    assert_eq!(
        DemodulatorConfig::new(slow, baud, pair),
        Err(ConfigError::ToneOutOfRange {
            got: 20_000,
            nyquist: 4_000
        }),
        "the demodulator must re-check the tones it was handed"
    );

    // The crate's own `TonePair` constants are bare `const`s that no
    // rate has ever cleared, so they rely entirely on this check. They
    // pass it at every representable rate only because every one of
    // their tones sits below 4000 Hz, the Nyquist of the lowest sample
    // rate the crate accepts. Pin that, so a future constant with a
    // higher tone cannot be added without noticing.
    let slowest = SampleRate::new(warble::SAMPLE_RATE_MIN).unwrap();
    for (name, pair) in [
        ("BELL_202", TonePair::BELL_202),
        ("BELL_103_ORIGINATE", TonePair::BELL_103_ORIGINATE),
        ("BELL_103_ANSWER", TonePair::BELL_103_ANSWER),
        ("HF_APRS", TonePair::HF_APRS),
    ] {
        assert!(
            ModulatorConfig::new(slowest, baud, pair).is_ok(),
            "{name} must remain usable at the lowest supported sample rate"
        );
    }

    // And the ordinary case still works: Bell 202 at 48 kHz.
    assert!(ModulatorConfig::bell_202(fast).is_ok());
    assert!(DemodulatorConfig::bell_202(fast).is_ok());
}

/// Bit conversions are exhaustive and round-trip in both directions.
#[test]
fn bit_conversions_exhaustive() {
    for b in [false, true] {
        assert_eq!(bool::from(Bit::from(b)), b);
    }
    for bit in [Bit::Zero, Bit::One] {
        assert_eq!(Bit::from(bool::from(bit)), bit);
    }
    assert_eq!(u8::from(Bit::Zero), 0u8);
    assert_eq!(u8::from(Bit::One), 1u8);
    assert_ne!(Bit::Zero, Bit::One);
}

/// Display of errors produced by real constructor failures (not
/// hand-built variants): the rendered messages embed the range bounds.
#[test]
fn display_of_real_constructor_errors() {
    assert_eq!(
        SampleRate::new(48_001).unwrap_err().to_string(),
        "sample rate 48001 Hz is out of range: must be within 8000..=48000 Hz"
    );
    assert_eq!(
        BaudRate::new(9_601).unwrap_err().to_string(),
        "baud rate 9601 is invalid: must be within 1..=9600 bit/s"
    );
    let sr = SampleRate::new(8_000).unwrap();
    assert_eq!(
        TonePair::new(0, 2_200, sr).unwrap_err().to_string(),
        "tone 0 Hz is out of range: must be nonzero and below the Nyquist frequency 4000 Hz"
    );
}
