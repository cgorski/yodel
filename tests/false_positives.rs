//! Receiver *specificity*: the decoder must not invent frames.
//!
//! `tests/noise.rs` measures sensitivity — how weak a real signal can be
//! and still decode. This file measures the opposite property, which is
//! just as important and is otherwise untested: when there is no valid
//! frame present, the receiver must emit nothing.
//!
//! The risk is concrete. [`RecoveryPolicy::SingleBitFlip`] and
//! [`RecoveryPolicy::PreDestuffFlip`] repair a frame by flipping bits
//! until the FCS passes. The FCS is only 16 bits, so a brute-force
//! search over a damaged window can stumble onto a checksum that
//! matches by luck — manufacturing a frame that was never transmitted.
//! `TncConfig` enables `PreDestuffFlip` *and* cross-chain voting by
//! default, so this crate takes that risk on the default path and owes
//! itself a guard.
//!
//! A frame counted but never validated is worse than a frame missed: it
//! is silent, plausible-looking, wrong data. Count-based benchmarks
//! (`tests/benchmark.rs`) cannot see it, because a false positive and a
//! true positive both increment the same counter.
#![cfg(feature = "tnc")]

use warble::ax25::RecoveryPolicy;
use warble::tnc::{ChainVoting, DefaultTncReceiver, TncConfig};
use warble::{Bit, SampleRate};

/// Every recovery/voting combination, including the default.
const POLICIES: &[(&str, RecoveryPolicy, ChainVoting)] = &[
    ("None + voting off", RecoveryPolicy::None, ChainVoting::Off),
    ("None + voting on", RecoveryPolicy::None, ChainVoting::On),
    (
        "SingleBitFlip + voting off",
        RecoveryPolicy::SingleBitFlip,
        ChainVoting::Off,
    ),
    (
        "SingleBitFlip + voting on",
        RecoveryPolicy::SingleBitFlip,
        ChainVoting::On,
    ),
    (
        "PreDestuffFlip + voting off",
        RecoveryPolicy::PreDestuffFlip,
        ChainVoting::Off,
    ),
    // The `TncConfig` default.
    (
        "PreDestuffFlip + voting on (default)",
        RecoveryPolicy::PreDestuffFlip,
        ChainVoting::On,
    ),
];

fn receiver(rate: SampleRate, rec: RecoveryPolicy, vote: ChainVoting) -> DefaultTncReceiver {
    let cfg = TncConfig::bell_202(rate)
        .expect("bell 202")
        .with_recovery(rec)
        .with_voting(vote);
    DefaultTncReceiver::new(cfg).expect("receiver")
}

/// Seeded LCG so failures are reproducible.
struct Lcg(u32);

impl Lcg {
    fn next_i16(&mut self) -> i16 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        ((self.0 >> 16) as i16).wrapping_sub(16_384)
    }
}

/// Feeds `samples` through every policy and asserts nothing is emitted.
fn assert_no_frames(rate: SampleRate, samples: &[i16], what: &str) {
    for &(name, rec, vote) in POLICIES {
        let mut rx = receiver(rate, rec, vote);
        let mut frames = 0u32;
        for &s in samples {
            if rx.push_i16(s).is_some() {
                frames += 1;
            }
        }
        assert_eq!(
            frames, 0,
            "{what}: {name} manufactured {frames} frame(s) from input containing none"
        );
    }
}

/// Accumulated false-accept **exposure**, and the rate it implies.
///
/// The tests above are binary: they assert nothing was fabricated on
/// the inputs tried. That is necessary but it cannot see a regression
/// that makes fabrication *more likely* without yet having happened —
/// and "we got away with it" is not a measurement.
///
/// This is the quantity behind the risk. Every frame the deframer
/// closes is one FCS trial: 16 bits, so a candidate built from noise
/// passes by luck with probability 2⁻¹⁶. `TncStats::fcs_errors` counts
/// exactly those trials that failed, so it is a direct proxy for how
/// much exposure the receiver accumulates per unit of dead air — and
/// the diversity bank multiplies it, because eleven chains each close
/// their own candidates.
///
/// Reported as trials per hour of open squelch, with the implied
/// fabrication rate, per policy. The assertion is a **ratcheted
/// ceiling on the trial rate**: it may be tightened, never loosened.
/// A change that doubles the number of candidates examined doubles the
/// chance of a fabricated frame reaching a user, and this is the only
/// test that would notice.
///
/// # What this counts, and what it does not
///
/// It counts **closed candidate frames** — one FCS trial each. It does
/// **not** count the FCS checks performed *inside* the repair search:
/// [`RecoveryPolicy::PreDestuffFlip`] re-checks the frame once per
/// candidate flipped bit over a window of up to 4096 bits, so a single
/// failing candidate can represent thousands of additional trials. The
/// true exposure under the default policy is therefore this figure
/// multiplied by roughly the mean repair-window length, and this test
/// is a **lower bound**.
///
/// That is still the right thing to pin, for two reasons. The closure
/// count is the term a receiver change moves (the repair multiplier is
/// a property of the policy, which is explicit and chosen), and it is a
/// property of the **deframer**, not of the policy: MEASURED, all six
/// policies in [`POLICIES`] report an identical count, which is itself
/// the finding that repair never *succeeds* on noise. If it did,
/// closures would migrate from `fcs_errors` into `frames_ok` and the
/// emitted-frame assertion would fire.
///
/// Because of that, this measures only the two bracketing policies
/// rather than all six — the weakest (no repair, no voting) and the
/// shipped default. Every policy is still checked for *emitting*
/// nothing by the tests below; this one adds the rate.
#[test]
fn false_accept_exposure_stays_bounded() {
    /// Closed-candidate FCS trials per hour, per policy, above which
    /// this is a regression. MEASURED at the time of writing: 420/hour
    /// for every policy, i.e. one fabricated frame per ~156 hours of
    /// continuous open squelch before the repair multiplier. The
    /// ceiling sits ~5x above that so it flags a step change in
    /// behaviour rather than seed noise.
    const MAX_TRIALS_PER_HOUR: f64 = 2_000.0;

    let hz = 22_050u32;
    let seconds = 60u32;
    let rate = SampleRate::new(hz).expect("rate");
    let mut rng = Lcg(0x0BAD_F00D);
    let samples: Vec<i16> = (0..hz * seconds).map(|_| rng.next_i16()).collect();

    let bracket = [POLICIES[0], POLICIES[POLICIES.len() - 1]];
    println!(
        "false-accept exposure over {seconds} s of full-scale noise @{hz} Hz\n\
         {:<38} {:>8} {:>14} {:>16}",
        "policy", "trials", "trials/hour", "P(fabricate)/hr"
    );
    for &(name, rec, vote) in &bracket {
        let mut rx = receiver(rate, rec, vote);
        let mut emitted = 0u32;
        for &s in &samples {
            if rx.push_i16(s).is_some() {
                emitted += 1;
            }
        }
        let stats = rx.stats();
        // Every closed candidate is one 16-bit FCS trial, whether it
        // passed (frames_ok / malformed) or failed (fcs_errors).
        let trials =
            u64::from(stats.fcs_errors) + u64::from(stats.frames_ok) + u64::from(stats.malformed);
        let per_hour = trials as f64 * 3600.0 / f64::from(seconds);
        let p_fabricate = per_hour / 65_536.0;
        println!("{name:<38} {trials:>8} {per_hour:>14.0} {p_fabricate:>16.4}");

        assert_eq!(
            emitted, 0,
            "{name} fabricated {emitted} frame(s) from pure noise"
        );
        assert!(
            per_hour <= MAX_TRIALS_PER_HOUR,
            "{name}: {per_hour:.0} closed-candidate FCS trials per hour of dead air \
             exceeds the pinned ceiling of {MAX_TRIALS_PER_HOUR:.0}. Each trial is a \
             2^-16 chance of manufacturing a frame -- and the repair search multiplies \
             it further -- so raising this raises the rate at which users see invented \
             data. Tighten the ceiling when it improves; never loosen it."
        );
    }
}

/// 30 s of full-scale white noise at each tested rate.
///
/// This is the canonical false-positive probe: an open squelch on a dead
/// channel, which is what a receiver sees most of the time.
#[test]
fn white_noise_yields_no_frames() {
    for &hz in &[8_000u32, 11_025, 22_050, 44_100, 48_000] {
        let rate = SampleRate::new(hz).expect("rate");
        let mut rng = Lcg(0x1234_5678 ^ hz);
        let samples: Vec<i16> = (0..hz * 30).map(|_| rng.next_i16()).collect();
        assert_no_frames(rate, &samples, &format!("white noise @{hz}"));
    }
}

/// Silence, and a steady mark tone with no framing — neither carries a
/// frame, and a flag-hunting deframer must not synthesize one from the
/// long run of identical bits.
#[test]
fn silence_and_unmodulated_tone_yield_no_frames() {
    let rate = SampleRate::new(44_100).expect("rate");

    let silence = vec![0i16; 44_100 * 5];
    assert_no_frames(rate, &silence, "silence");

    // A continuous 1200 Hz mark tone: valid audio, no HDLC structure.
    let tone: Vec<i16> = (0..44_100u32 * 5)
        .map(|i| {
            let phase = i as f64 * 1200.0 * std::f64::consts::TAU / 44_100.0;
            (phase.sin() * 12_000.0) as i16
        })
        .collect();
    assert_no_frames(rate, &tone, "unmodulated mark tone");
}

/// Random *bits* pushed through the real modulator: a well-formed AFSK
/// signal carrying no HDLC framing at all. This is a sharper probe than
/// white noise, because the demodulator locks and the slicer produces a
/// clean bit stream — only the framing layer stands between it and a
/// bogus frame.
#[test]
fn random_bitstream_yields_no_frames() {
    #[cfg(feature = "mod")]
    {
        use warble::{Modulator, ModulatorConfig};

        let rate = SampleRate::new(44_100).expect("rate");
        let mut rng = Lcg(0xC0FF_EE01);
        let bits: Vec<Bit> = (0..40_000)
            .map(|_| {
                if rng.next_i16() >= 0 {
                    Bit::One
                } else {
                    Bit::Zero
                }
            })
            .collect();

        let cfg = ModulatorConfig::bell_202(rate).expect("modulator config");
        let samples: Vec<i16> = Modulator::new(cfg).i16_samples(bits.into_iter()).collect();

        assert_no_frames(rate, &samples, "random unframed bitstream");
    }
}

/// A real frame whose payload has been comprehensively destroyed should
/// be rejected, not "repaired" into a different valid frame.
///
/// This is the case the bit-flip policies are most likely to get wrong:
/// unlike noise, the input has valid flags, valid framing and plausible
/// structure, so the deframer engages fully and the repair search runs.
#[test]
fn heavily_corrupted_frame_is_not_repaired_into_a_different_frame() {
    #[cfg(all(feature = "mod", feature = "alloc"))]
    {
        use warble::aprs::{AprsPacket, Status};
        use warble::ax25::Address;
        use warble::tnc::TncTransmitter;

        let rate = SampleRate::new(44_100).expect("rate");
        let tx = TncTransmitter::new(TncConfig::bell_202(rate).expect("config"));
        let packet = AprsPacket::Status(Status {
            text: b"specificity probe payload",
        });
        let clean: Vec<i16> = tx
            .transmit_to_vec_i16(
                &packet,
                Address::new(b"APRS", 0).expect("dest"),
                Address::new(b"N0CALL", 1).expect("src"),
                &[],
            )
            .expect("transmit");

        // Sanity: the clean signal must decode, or the test proves nothing.
        let mut rx = receiver(rate, RecoveryPolicy::PreDestuffFlip, ChainVoting::On);
        let decoded = clean.iter().filter(|&&s| rx.push_i16(s).is_some()).count();
        assert_eq!(decoded, 1, "clean control signal must decode exactly once");

        // Now obliterate the middle of the burst. Whatever survives must
        // not be emitted as a frame: too much is gone to reconstruct.
        let mut rng = Lcg(0x5EED_5EED);
        let mut wrecked = clean.clone();
        let (lo, hi) = (wrecked.len() / 3, wrecked.len() * 2 / 3);
        for s in &mut wrecked[lo..hi] {
            *s = rng.next_i16();
        }

        for &(name, rec, vote) in POLICIES {
            let mut rx = receiver(rate, rec, vote);
            for &s in &wrecked {
                if let Some(frame) = rx.push_i16(s) {
                    // Emitting *the original* frame would mean the burst
                    // was recoverable after all, which is acceptable.
                    // Emitting anything else is a fabricated frame.
                    assert_eq!(
                        frame.info(),
                        b">specificity probe payload",
                        "{name}: repaired a destroyed burst into a DIFFERENT frame"
                    );
                }
            }
        }
    }
}
