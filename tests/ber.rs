//! Tier-1 physical-layer measurement suite: raw bit error rate, and the
//! 50%-frame-recovery sensitivity threshold in dB.
//!
//! Every other noise test in this crate counts **frames** (`tests/snr.rs`,
//! `tests/noise.rs`, `tests/g3ruh.rs`, `tests/benchmark.rs`). A frame
//! count conflates the demodulator with HDLC framing, the FCS, bit
//! de-stuffing and the receiver's repair heuristics: a DSP regression and
//! a de-stuffer regression look identical from there. The two metrics
//! here separate them.
//!
//! * **Metric 1** — BER at the *demodulator output*, before any framing
//!   exists: modulate a pseudo-random bit sequence, add seeded noise,
//!   demodulate, align, count mismatched bits.
//! * **Metric 2** — the SNR in dB at which frame recovery crosses 50%,
//!   located by bisection. This replaces "N frames out of 100 at one
//!   fixed noise level" (a proxy) with the quantity an operator cares
//!   about.
//!
//! Everything is hermetic and deterministic: fixed literal seeds, no wall
//! clock, no external data, no new dependencies. Failures reproduce
//! exactly.
//!
//! # SNR convention (shared with the rest of the suite)
//!
//! Identical to `tests/noise.rs`, `tests/snr.rs` and `tests/g3ruh.rs`, so
//! the dB numbers here are directly comparable with the ladders there:
//! uniform white noise of peak `A` is added to the modulator's full-scale
//! output, with `A = (32767/√2) / 10^(SNR/20) · √3` (uniform noise RMS is
//! `A/√3`; `32767/√2` is a full-scale sine's RMS). Sums are clamped into
//! `i16`, exactly as the other tests do.
//!
//! Two consequences of that convention, both **measured**, both worth
//! knowing when reading the tables:
//!
//! * The noise is white over the whole `0..sample_rate/2` band, so the
//!   per-bit energy ratio is much better than the wideband SNR:
//!   `Eb/N0 ≈ SNR_dB + 10·log10((sample_rate/2) / baud)`, i.e. +13.0 dB
//!   for Bell 202, +19.0 dB for HF 300 and +4.0 dB for G3RUH 9600 at
//!   48 kHz. Both columns are printed; only the SNR column is comparable
//!   with the rest of the suite.
//! * The signal is already full scale, so the clamp bites at *every*
//!   level in these tables: MEASURED 17% of samples clipped at 12 dB SNR
//!   and 36% at 0 dB. Re-running the sweep with signal and noise jointly
//!   attenuated to 0.15 (identical ratio, zero clipping) moved the
//!   Bell 202 transition by about 1 dB and the HF 300 transition by about
//!   2 dB, so clipping shifts these curves slightly but is not what they
//!   are showing.
//!
//! # What the measurement found (all 48 kHz, all MEASURED)
//!
//! The BER columns do not show a graceful roll-off, and the reason is
//! specific and reproducible: **the bit-clock loop loses lock permanently
//! and cannot re-acquire from random data.** Scoring the continuous run
//! in 2000-bit segments shows the signature clearly — a run of exactly
//! zero-error segments, then one segment at ~40% errors, then ~50% for
//! every segment to the end of the stream, forever. [`yodel::Slicer`]
//! nudges its phase on *every* metric zero crossing with no magnitude
//! qualification; once noise supplies spurious crossings the loop is
//! dragged around by them, drops to its fast "searching" gain, and has
//! nothing to re-acquire against unless an alternating preamble happens
//! to arrive. So the continuous-run BER is really a measure of *mean time
//! to unrecoverable loss of lock*, and it flips from 0 to ~0.5 inside
//! about 1 dB.
//!
//! Each mode is therefore reported in three columns, which between them
//! say where the DSP quality sits:
//!
//! | column | what it measures |
//! |---|---|
//! | continuous | one unbroken 20 000-bit run through one demodulator: 0 until the loop dies, ~0.5 after |
//! | burst | the same bit count and noise, but the demodulator is re-acquired from a fresh preamble every 512 bits, as a packet receiver does. Its own transition sits ~1 dB lower and is graded rather than binary, so this is the column carrying the informative ratchets |
//! | perfect clock | the discriminator metric hard-decided at ideal bit centres, no PLL at all. The crate's *achievable* curve: smooth and textbook |
//!
//! The gap between the last two columns is what bit-clock recovery costs,
//! and it is large. MEASURED at −1 dB, Bell 202: the correlator alone
//! errs on 7.0·10⁻⁴ of bits, the demodulator in 512-bit bursts on
//! 9.8·10⁻², and the demodulator over one continuous run on 5.0·10⁻¹.
//! Two orders of magnitude of that is timing recovery.
//!
//! Metric 2 lands **2–3 dB below** Metric 1's continuous transition
//! (Bell 202: −2.50 dB threshold against a transition just under 0 dB).
//! That is not a contradiction, it is the same finding from the other
//! side. Metric 1 exercises one bare [`yodel::AfskDemodulator`] over a
//! 20 000-bit stream; Metric 2 exercises `DefaultTncReceiver`, which runs
//! a bank of chains at staggered clock phases and re-acquires from every
//! frame's own HDLC preamble, so a lock loss costs at most one short
//! frame.
//!
//! One further measured quirk, called out because it broke an earlier
//! draft of this very file: the demodulator's output lag is **not** a
//! per-mode constant. HF APRS 300 settles on lag 0 or lag 1 depending on
//! the acquisition, because its 1.5-bit correlator window puts the
//! slicer's decision instant next to a bit boundary. The lag is therefore
//! re-established for every run — see [`score`].
//!
//! # What runs by default
//!
//! All of it, MEASURED at 1.3 s in release (3.1 s with
//! `--test-threads=1`):
//!
//! * `raw_ber_*` (3 tests) — Metric 1, the pinned three-column ladders.
//! * `alignment_is_unambiguous_when_clean` — guards the aligner, on which
//!   every BER number depends.
//! * `sensitivity_threshold_*` (3 tests) — Metric 2, by bisection.
//!
//! One test is `#[ignore]`d: `ber_curve_fine_sweep`, a dense 0.5 dB-step
//! curve across each mode's whole transition. It is a human-reading tool
//! for anyone tuning the slicer, costs ~4 s on its own — more than
//! everything above put together — and pins nothing the ladders do not
//! already pin:
//!
//! ```text
//! cargo test --release --all-features --test ber -- --ignored --nocapture
//! ```
//!
//! Use `--release`: MEASURED 13.4 s for this file in debug against 1.3 s
//! in release.
#![cfg(all(feature = "mod", feature = "demod"))]

use yodel::demodulator::{AfskDemodulator, DemodulatorConfig};
use yodel::modulator::{Modulator, ModulatorConfig};
use yodel::{Bit, ModemProfile, SampleRate};

/// The one sample rate everything here is measured at. 48 kHz is the rate
/// all three modes share (G3RUH 9600 needs ≥ 2 samples per bit; 48 kHz
/// gives it 5), so holding it fixed keeps the three tables on one axis.
const SR_HZ: u32 = 48_000;

// ---------------------------------------------------------------------
// Determinism primitives
// ---------------------------------------------------------------------

/// 64-bit LCG (Knuth MMIX constants), the generator `tests/snr.rs` uses.
/// Deterministic and allocation-free; no wall clock is consulted anywhere
/// in this file.
struct Lcg(u64);

impl Lcg {
    fn next_u64(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0
    }

    /// Uniform in `[-1.0, 1.0)`.
    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 52) as f64 - 1.0
    }

    /// One pseudo-random bit from the *top* bit of the state: an LCG's low
    /// bits have short periods, its high bits do not.
    fn next_bit(&mut self) -> Bit {
        if self.next_u64() >> 63 == 1 {
            Bit::One
        } else {
            Bit::Zero
        }
    }
}

/// Peak amplitude of uniform noise for a target SNR in dB against a
/// full-scale sine's RMS (uniform noise RMS is `peak/√3`). The convention
/// of `tests/noise.rs`, `tests/snr.rs` and `tests/g3ruh.rs`, unchanged —
/// see the module docs.
fn noise_peak(snr_db: f64) -> f64 {
    let signal_rms = 32_767.0 / core::f64::consts::SQRT_2;
    let noise_rms = signal_rms / 10f64.powf(snr_db / 20.0);
    noise_rms * 3f64.sqrt()
}

/// Adds one noise sample and clamps into `i16`, as the other ladders do.
fn mix(sample: i16, rng: &mut Lcg, peak: f64) -> i16 {
    (f64::from(sample) + rng.next_f64() * peak).clamp(f64::from(i16::MIN), f64::from(i16::MAX))
        as i16
}

/// Per-bit energy ratio implied by a wideband SNR under this convention:
/// the noise occupies `sample_rate/2`, one bit occupies `1/baud`.
/// Reported for orientation; nothing is pinned against it.
fn eb_n0_db(snr_db: f64, baud_bps: u32) -> f64 {
    snr_db + 10.0 * (f64::from(SR_HZ / 2) / f64::from(baud_bps)).log10()
}

// ---------------------------------------------------------------------
// Modes
// ---------------------------------------------------------------------

/// The modes under measurement: a profile plus the front end that
/// decodes it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// 1200 baud, 1200/2200 Hz — VHF APRS.
    Bell202,
    /// 300 baud, 1600/1800 Hz — HF APRS.
    Hf300,
    /// 9600 baud scrambled direct baseband.
    #[cfg(feature = "g3ruh")]
    G3ruh9600,
}

/// Every mode this file measures, in table order.
const MODES: &[Mode] = &[
    Mode::Bell202,
    Mode::Hf300,
    #[cfg(feature = "g3ruh")]
    Mode::G3ruh9600,
];

/// Floor on the modes swept by the tests that loop over [`MODES`].
///
/// Their assertions are all inside that loop, so an empty list would pass
/// having measured nothing. Two rather than three because the third mode
/// is behind the `g3ruh` feature and legitimately absent from a default
/// build.
const MIN_MODES: usize = 2;

impl Mode {
    fn label(self) -> &'static str {
        match self {
            Mode::Bell202 => "Bell 202 1200 Bd",
            Mode::Hf300 => "HF APRS 300 Bd",
            #[cfg(feature = "g3ruh")]
            Mode::G3ruh9600 => "G3RUH 9600 Bd",
        }
    }

    fn profile(self) -> ModemProfile {
        match self {
            Mode::Bell202 => ModemProfile::BELL_202,
            Mode::Hf300 => ModemProfile::HF_APRS_300,
            #[cfg(feature = "g3ruh")]
            Mode::G3ruh9600 => ModemProfile::G3RUH_9600,
        }
    }

    fn baud_bps(self) -> u32 {
        self.profile().baud().bps()
    }

    /// Modulates `bits`, adds seeded noise at `snr_db`, and returns the
    /// demodulator's raw bit decisions — no NRZI, no descrambling, no
    /// framing. This is the stream Metric 1 scores.
    ///
    /// Each call builds a *fresh* modulator and demodulator, so calling
    /// it once per burst is exactly a re-acquisition.
    fn channel(self, bits: &[Bit], snr_db: f64, seed: u64) -> Vec<Bit> {
        #[cfg(feature = "g3ruh")]
        if self == Mode::G3ruh9600 {
            return self.baseband_channel(bits, snr_db, seed);
        }
        self.tone_channel(bits, snr_db, seed)
    }

    fn tone_channel(self, bits: &[Bit], snr_db: f64, seed: u64) -> Vec<Bit> {
        let sr = SampleRate::new(SR_HZ).unwrap();
        let profile = self.profile();
        let mcfg = ModulatorConfig::new(sr, profile.baud(), profile.tones()).unwrap();
        let dcfg = DemodulatorConfig::new(sr, profile.baud(), profile.tones()).unwrap();
        let mut demod = AfskDemodulator::new(dcfg).unwrap();
        let mut rng = Lcg(seed);
        let peak = noise_peak(snr_db);
        let mut out = Vec::with_capacity(bits.len() + 16);
        for s in Modulator::new(mcfg).i16_samples(bits.iter().copied()) {
            if let Some(b) = demod.push_sample_i16(mix(s, &mut rng, peak)) {
                out.push(b);
            }
        }
        out
    }

    /// The G3RUH leg. The bits are *not* run through the
    /// scrambler/descrambler pair: descrambling triples every error
    /// (`x^17 + x^12 + 1` has three taps), which would fold a line-coding
    /// property into what is meant to be a DSP metric. A pseudo-random
    /// payload already has the transition density the scrambler exists to
    /// guarantee.
    #[cfg(feature = "g3ruh")]
    fn baseband_channel(self, bits: &[Bit], snr_db: f64, seed: u64) -> Vec<Bit> {
        use yodel::{BasebandDemodulator, BasebandModulator};
        let sr = SampleRate::new(SR_HZ).unwrap();
        let baud = self.profile().baud();
        let mut demod = BasebandDemodulator::new(sr, baud).unwrap();
        let mut rng = Lcg(seed);
        let peak = noise_peak(snr_db);
        let mut out = Vec::with_capacity(bits.len() + 16);
        for s in BasebandModulator::new(sr, baud)
            .unwrap()
            .i16_samples(bits.iter().copied())
        {
            if let Some(b) = demod.push_i16(mix(s, &mut rng, peak)) {
                out.push(b);
            }
        }
        out
    }
}

// ---------------------------------------------------------------------
// Metric 1: bit sequences, alignment, and the three BER columns
// ---------------------------------------------------------------------

/// Alternating `1 0 1 0 …` bits prefixed to every transmitted sequence so
/// the PLL has something to lock onto before the scored region. 64 bits is
/// eight HDLC flags' worth of transitions, far more than the slicer's
/// 7-crossing lock threshold needs.
const PREAMBLE_BITS: usize = 64;

/// Scored payload of the continuous run. 20 000 bits puts the resolution
/// at 5·10⁻⁵ (one error) and costs ~3.2 M samples per rung in the
/// slowest mode (300 baud = 160 samples per bit).
const PAYLOAD_BITS: usize = 20_000;

/// Payload bits per burst in the burst column, and the burst count. 512
/// bits is the order of a long AX.25 frame, so a lock loss costs about
/// what it costs a real packet receiver; 40 bursts keeps the total scored
/// count within a few percent of the continuous run's.
const BURST_PAYLOAD_BITS: usize = 512;
const BURSTS: usize = 40;

/// Bits skipped at each end of a scored region. The front end needs
/// roughly a bit period of history plus a few transitions to settle, and
/// the final cells can be truncated by the end of the sample stream; two
/// dozen bits clears both, and the continuous run uses double that
/// because it can afford to.
///
/// This is defensive margin, not a load-bearing constant: dropping the
/// head guard entirely (scoring from bit 0, preamble included) was tried
/// and changed no result in any ladder — at 20 dB the slicer is already
/// correct on its first decision. The guard exists so that a *future*
/// front end with a longer settling transient does not quietly show up as
/// a handful of bit errors attributed to noise.
const GUARD_BITS: usize = 24;

/// Widest alignment lag searched, in bits. MEASURED: 0 for both tone
/// modes, 1 for G3RUH (the baseband modulator's half-cosine shaping
/// carries one bit of lookahead). 12 leaves an order of magnitude of
/// headroom, and [`alignment_is_unambiguous_when_clean`] asserts the
/// winner is not sitting on the window edge — which is what a too-narrow
/// window would look like.
const MAX_ALIGN: usize = 12;

/// SNR at which the alignment lag is established. Well clear of every
/// mode's threshold, so the lag is measured on a channel where the answer
/// is not in doubt.
const CLEAN_SNR_DB: f64 = 20.0;

/// Outcome of scoring one received stream against what was transmitted.
struct Aligned {
    /// Bit lag that minimised the error count.
    offset: usize,
    errors: usize,
    compared: usize,
    /// Error count of the *second* best lag. On a clean channel this must
    /// be enormous next to `errors`: that is what makes the winning lag
    /// trustworthy rather than lucky.
    runner_up_errors: usize,
}

/// The transmitted sequence: alternating preamble, then a deterministic
/// pseudo-random payload of `payload_bits`.
fn tx_sequence(seed: u64, payload_bits: usize) -> Vec<Bit> {
    let mut rng = Lcg(seed);
    let mut v: Vec<Bit> = (0..PREAMBLE_BITS)
        .map(|i| {
            if i.is_multiple_of(2) {
                Bit::One
            } else {
                Bit::Zero
            }
        })
        .collect();
    v.extend((0..payload_bits).map(|_| rng.next_bit()));
    v
}

/// Counts bit errors between `rx` and `tx` at a known lag over the
/// half-open bit range `start..end`.
fn count_at(tx: &[Bit], rx: &[Bit], offset: usize, start: usize, end: usize) -> usize {
    (start..end).filter(|&i| rx[i + offset] != tx[i]).count()
}

/// Scores `rx` against `tx`: cross-correlates over `0..=MAX_ALIGN` bits of
/// lag and counts errors at the winning lag.
///
/// Only non-negative lags exist — a demodulator's decisions lag the
/// transmitted stream by its settling and group delay, they never lead it.
///
/// The lag has to be re-established for **every** run, not fixed once per
/// mode, and that is a MEASURED property of this crate rather than a
/// precaution: HF APRS 300 at 48 kHz settles on lag 0 or lag 1 depending
/// on the acquisition. Its correlator window is 1.5 bits (240 samples),
/// so the metric's group delay puts the slicer's decision instant right
/// next to a bit boundary and the loop can lock either side of it. An
/// earlier draft of this file established the lag once on a clean channel
/// and reused it, and duly reported BER 0.496 for HF 300 at 12 dB SNR.
///
/// Taking the best of `MAX_ALIGN + 1` candidates biases the error count
/// downwards, but only where the candidates are competitive with each
/// other — i.e. only once the loop is dead and every lag scores ~50%. At
/// every rung carrying a tight ceiling the correct lag wins by three or
/// more decades, so those numbers carry no selection bias; at the loose
/// threshold-region rungs it still wins by 5x or better, and their
/// ceilings are sized for the variance anyway.
fn score(tx: &[Bit], rx: &[Bit], guard: usize) -> Aligned {
    // One interval, shared by every candidate lag: reserving MAX_ALIGN
    // bits of tail headroom means `rx[i + off]` is in range for all of
    // them, so the candidates are compared over identical bit counts.
    let start = PREAMBLE_BITS + guard;
    let end = (tx.len() - guard).min(rx.len().saturating_sub(MAX_ALIGN));
    assert!(
        end > start + (tx.len() - PREAMBLE_BITS) / 2,
        "demodulator produced only {} bits for {} transmitted: nothing to score",
        rx.len(),
        tx.len()
    );
    let mut best = (usize::MAX, 0usize);
    let mut runner_up = usize::MAX;
    for off in 0..=MAX_ALIGN {
        let errors = count_at(tx, rx, off, start, end);
        if errors < best.0 {
            runner_up = best.0;
            best = (errors, off);
        } else if errors < runner_up {
            runner_up = errors;
        }
    }
    Aligned {
        offset: best.1,
        errors: best.0,
        compared: end - start,
        runner_up_errors: runner_up,
    }
}

/// Scores a mode's alignment on a nearly clean channel: the reference the
/// unambiguity guard checks.
fn clean_alignment(mode: Mode) -> Aligned {
    let tx = tx_sequence(0xA119_0001, PAYLOAD_BITS);
    let rx = mode.channel(&tx, CLEAN_SNR_DB, 0xA119_1000);
    score(&tx, &rx, GUARD_BITS * 2)
}

/// Column 1: one unbroken run through one demodulator.
fn continuous_ber(mode: Mode, snr_db: f64, seed: u64) -> Aligned {
    let tx = tx_sequence(seed ^ 0x5EED_B173, PAYLOAD_BITS);
    let rx = mode.channel(&tx, snr_db, seed);
    score(&tx, &rx, GUARD_BITS * 2)
}

/// Column 2: the same total bit count, re-acquired every
/// [`BURST_PAYLOAD_BITS`] bits from a fresh preamble, as a packet
/// receiver does. The noise seed advances per burst (by the 64-bit
/// golden-ratio constant) so no two bursts see the same noise, and each
/// burst is aligned on its own — re-acquisition can land on a different
/// lag, as `score` explains.
///
/// Returns `(errors, compared, widest lag seen)`.
fn burst_ber(mode: Mode, snr_db: f64, seed: u64) -> (usize, usize, usize) {
    let mut errors = 0usize;
    let mut compared = 0usize;
    let mut max_lag = 0usize;
    for b in 0..BURSTS {
        let step = (b as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let tx = tx_sequence(seed ^ 0x8082_5741 ^ step, BURST_PAYLOAD_BITS);
        let rx = mode.channel(&tx, snr_db, seed.wrapping_add(step));
        let a = score(&tx, &rx, GUARD_BITS);
        errors += a.errors;
        compared += a.compared;
        max_lag = max_lag.max(a.offset);
    }
    (errors, compared, max_lag)
}

/// Column 3: the discriminator metric hard-decided at ideal bit centres,
/// bypassing [`yodel::Slicer`] entirely — same waveform, same seeded
/// noise, perfect timing. This is the crate's *achievable* curve at the
/// tone correlator, and pinning it separately means a real correlator
/// regression (a bad tap design, a broken orthogonal window) cannot hide
/// behind the slicer's cliff.
///
/// The sampling **delay** is chosen **once, on a noise-free run**, as the
/// delay maximising the mean correlation margin `metric · (+1 for One,
/// −1 for Zero)`. That is a property of the correlator's impulse
/// response, so the whole ladder shares one delay and no rung picks the
/// value that flatters it.
///
/// The search spans **two** bit periods, not one. The front end's group
/// delay is not a fixed fraction of a bit: the envelope smoother adds
/// its own lag on top of the correlation window, and at 300 baud the
/// window is itself 1.5 bits wide for tone orthogonality. A search
/// confined to one bit cell silently reports a mis-sampled reference as
/// a correlator regression — which is exactly what happened when
/// envelope smoothing was added to this path. Searching the full
/// two-bit span keeps this column meaning "the best any fixed clock
/// could do", which is the only thing worth comparing the loop against.
///
/// `None` for G3RUH: the baseband front end's filter is private, so there
/// is no public way to tap its metric.
fn perfect_clock_ber(mode: Mode, snr_db: f64, seed: u64) -> Option<f64> {
    #[cfg(feature = "g3ruh")]
    if mode == Mode::G3ruh9600 {
        return None;
    }
    use yodel::{Discriminator, QuadratureCorrelator};

    let sr = SampleRate::new(SR_HZ).unwrap();
    let profile = mode.profile();
    let baud = profile.baud();
    assert_eq!(
        SR_HZ % baud.bps(),
        0,
        "ideal-centre indexing assumes a whole number of samples per bit"
    );
    let spb = (SR_HZ / baud.bps()) as usize;
    let bits = tx_sequence(seed ^ 0x1DEA_1C10, PAYLOAD_BITS);
    let start = PREAMBLE_BITS + GUARD_BITS * 2;
    let end = bits.len() - GUARD_BITS * 2;

    let metrics = |peak: f64| -> Vec<i32> {
        let mut rng = Lcg(seed);
        let mcfg = ModulatorConfig::new(sr, baud, profile.tones()).unwrap();
        let mut corr = QuadratureCorrelator::new(sr, baud, profile.tones()).unwrap();
        Modulator::new(mcfg)
            .i16_samples(bits.iter().copied())
            .map(|s| corr.push_i16(mix(s, &mut rng, peak)))
            .collect()
    };

    let clean = metrics(0.0);
    // Leave a whole bit of headroom at the end so the widest delay
    // cannot index past the metric stream.
    let end = end.min(clean.len() / spb - 2);
    // Floor on what this column compares, mirroring `score`'s guard on the
    // other two: the BER below is a ratio over `end - start` bit centres,
    // and a front end that yielded a handful of them would report a
    // flattering near-zero (or, with `end` under `start`, a wrapped count)
    // rather than a failure.
    assert!(
        end > start + PAYLOAD_BITS / 2,
        "{}: only {} bit centres available to score of {PAYLOAD_BITS} \
         transmitted — nothing to conclude from",
        mode.label(),
        end.saturating_sub(start)
    );
    let margin = |src: &[i32], d: usize| -> i64 {
        (start..end)
            .map(|k| {
                let m = i64::from(src[k * spb + d]);
                if bits[k] == Bit::One { m } else { -m }
            })
            .sum()
    };
    let delay = (0..2 * spb)
        .max_by_key(|&d| margin(&clean, d))
        .expect("at least one sample per bit");
    assert!(
        margin(&clean, delay) > 0,
        "{}: no sampling delay in two bit periods gives a positive clean-signal \
         correlation margin — the reference tap is broken, not merely mistimed",
        mode.label()
    );

    let noisy = metrics(noise_peak(snr_db));
    let errors = (start..end)
        .filter(|&k| {
            let decided = if noisy[k * spb + delay] > 0 {
                Bit::One
            } else {
                Bit::Zero
            };
            decided != bits[k]
        })
        .count();
    Some(errors as f64 / (end - start) as f64)
}

// ---------------------------------------------------------------------
// Metric 1: the pinned ladders
// ---------------------------------------------------------------------

/// One rung of a BER ladder: an SNR plus a ratcheted ceiling per column.
///
/// RATCHET, for every `Some` below: tighten when the receiver improves,
/// never loosen. `None` means "not pinned at this rung" — used where the
/// value is dominated by lost clock lock, which is not a DSP quantity
/// worth pinning a ceiling on, but is still worth printing and still
/// feeds the monotonicity check.
struct Rung {
    snr_db: f64,
    continuous: Option<f64>,
    burst: Option<f64>,
    perfect_clock: Option<f64>,
}

/// Absolute part of the monotonicity slack. 4·10⁻⁴ is eight bit errors in
/// 20 000: enough that two rungs on the same near-zero floor cannot fail
/// on sampling noise, far too small to hide a rung sliding onto the cliff
/// (a three-to-four-decade jump).
const MONOTONE_SLACK: f64 = 4e-4;

/// Relative part of the slack, for the "adjacent near-equal points"
/// case. Two rungs deep below a mode's threshold both sit at ~0.5 and
/// their order there is a coin toss on the noise draw, so 8% of the
/// worse rung is allowed on top of [`MONOTONE_SLACK`]. At a pinned rung
/// (BER ≤ 10⁻²) that is at most 8·10⁻⁴ of extra tolerance.
const MONOTONE_REL_SLACK: f64 = 0.08;

/// Floor on the number of rungs a ladder must carry.
///
/// Every assertion in [`check_ladder`] lives inside a loop over the
/// rungs, so a ladder trimmed to nothing would report a pass having
/// measured nothing. Emptiness alone is already fatal further down (the
/// cleanest-rung check unwraps the last element), but a ladder cut from
/// six rungs to one is not, and a one-rung ladder cannot see a curve
/// move. Each of the three ladders below carries exactly six rungs:
/// several below the mode's transition, one on the knee, two clean.
const MIN_RUNGS: usize = 6;

/// Floor on the number of ceilings a ladder asserts.
///
/// The complement of [`MIN_RUNGS`]. Rungs can all be present while every
/// ceiling on them is `None`, and `None` asserts nothing at all --
/// `None` is the legitimate way to say "not pinned at this rung" (see
/// [`Rung`]), which is precisely why the number still pinned needs a
/// floor of its own. Without one, disarming this file is a matter of
/// replacing values with `None` and the ladders keep printing and keep
/// passing.
///
/// MEASURED, per ladder: 12 ceilings for Bell 202 (2 continuous, 4 burst,
/// 6 perfect clock), 14 for HF APRS 300 (3 / 5 / 6), and 8 for G3RUH
/// (3 / 5 / 0 -- it has no public front-end tap, so its perfect-clock
/// column is never measured). The floor is the smallest of the three.
const MIN_PINNED_CEILINGS: usize = 8;

/// Seeds pooled per SNR rung.
///
/// Near the bit-clock loop's threshold the continuous column is
/// **bistable rather than noisy**: within one run the loop either holds
/// lock throughout, or loses it and sits at chance for the remainder.
/// A single seed therefore samples a Bernoulli variable, which is not
/// monotone in SNR and cannot carry a meaningful ceiling. Pooling
/// errors and comparisons across seeds — pooling the counts, not
/// averaging the ratios — estimates the expectation, which is.
///
/// Four is enough to make the ladders monotone at every rung while
/// keeping this file inside its runtime budget; it is not enough to
/// make the near-threshold rungs *precise*, which is why the ceilings
/// there are loose and labelled as such.
const SEEDS_PER_RUNG: usize = 4;

/// Measures one mode's ladder, prints the three columns, and checks the
/// ceilings, monotonicity, and clean-channel behaviour.
///
/// `rungs` must be ordered by **ascending SNR**.
fn check_ladder(mode: Mode, rungs: &[Rung], seed: u64) {
    assert!(
        rungs.len() >= MIN_RUNGS,
        "{}: ladder carries {} rungs, floor is {MIN_RUNGS}",
        mode.label(),
        rungs.len()
    );
    let baud = mode.baud_bps();
    println!(
        "\n{} @ {SR_HZ} Hz — raw BER at the demodulator output\n\
         {PAYLOAD_BITS} bits continuous, {BURSTS}x{BURST_PAYLOAD_BITS} burst; \
         `lag` is the winning alignment (continuous / widest across bursts)",
        mode.label()
    );
    println!("  SNR dB | Eb/N0 dB |   lag |   continuous |        burst | perfect clock");
    println!("  -------|----------|-------|--------------|--------------|--------------");

    let mut continuous = Vec::with_capacity(rungs.len());
    let mut burst = Vec::with_capacity(rungs.len());
    let mut perfect = Vec::with_capacity(rungs.len());
    // Ceilings asserted, counted rather than assumed; see
    // [`MIN_PINNED_CEILINGS`].
    let mut pinned = 0usize;

    for rung in rungs {
        // Pool several seeds per rung. Near the clock loop's threshold
        // the continuous column is **bistable**, not noisy: the loop
        // either holds lock for a whole run or loses it and sits at
        // chance for the remainder. A single seed therefore samples a
        // Bernoulli variable, which is not monotone in SNR and cannot
        // be given a meaningful ceiling. Pooling errors and comparisons
        // across seeds (not averaging the ratios) estimates the
        // expectation, which is monotone.
        let mut cerr = 0u64;
        let mut ccmp = 0u64;
        let mut berr = 0u64;
        let mut bcmp = 0u64;
        let mut blag = 0usize;
        let mut clag = 0usize;
        for s in 0..SEEDS_PER_RUNG {
            let seed = seed ^ (s as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
            let ca = continuous_ber(mode, rung.snr_db, seed);
            cerr += ca.errors as u64;
            ccmp += ca.compared as u64;
            clag = clag.max(ca.offset);
            let (be, bn, bl) = burst_ber(mode, rung.snr_db, seed);
            berr += be as u64;
            bcmp += bn as u64;
            blag = blag.max(bl);
        }
        let (be, bn) = (berr as usize, bcmp as usize);
        let c = cerr as f64 / ccmp as f64;
        let b = berr as f64 / bcmp as f64;
        let p = perfect_clock_ber(mode, rung.snr_db, seed);
        let head = format!(
            "  {:6.1} | {:8.1} | {:2} /{:2} | {:12.3e} | {:12.3e} | ",
            rung.snr_db,
            eb_n0_db(rung.snr_db, baud),
            clag,
            blag,
            c,
            b
        );
        match p {
            Some(p) => println!("{head}{p:13.3e}"),
            None => println!("{head}(no public FE)"),
        }
        pin(
            mode,
            rung.snr_db,
            "continuous",
            c,
            cerr as usize,
            ccmp as usize,
            rung.continuous,
        );
        pin(mode, rung.snr_db, "burst", b, be, bn, rung.burst);
        pinned += usize::from(rung.continuous.is_some()) + usize::from(rung.burst.is_some());
        if let (Some(p), Some(ceiling)) = (p, rung.perfect_clock) {
            pinned += 1;
            assert!(
                p <= ceiling,
                "{}: perfect-clock BER at {} dB SNR is {p:.4e}, above the pinned \
                 ceiling {ceiling:.4e} — the tone correlator itself has regressed",
                mode.label(),
                rung.snr_db
            );
        }
        continuous.push(c);
        burst.push(b);
        if let Some(p) = p {
            perfect.push(p);
        }
    }

    check_monotonic(mode, "continuous", &continuous);
    check_monotonic(mode, "burst", &burst);
    // Empty for G3RUH, whose baseband front end has no public metric tap;
    // `check_monotonic` and the zero check below then have nothing to say
    // about a column that was never measured.
    check_monotonic(mode, "perfect clock", &perfect);

    // The cleanest rung must be error-free in every measured column: a
    // modem that cannot manage zero errors on a nearly clean channel is
    // broken outright, whatever its noise performance.
    let top = rungs.last().expect("non-empty ladder").snr_db;
    let columns = [
        Some(("continuous", *continuous.last().unwrap())),
        Some(("burst", *burst.last().unwrap())),
        perfect.last().map(|&p| ("perfect clock", p)),
    ];
    for (name, v) in columns.into_iter().flatten() {
        assert_eq!(
            v,
            0.0,
            "{}: {name} BER is {v:.4e} at the cleanest rung ({top} dB) — \
             must be exactly zero",
            mode.label()
        );
    }

    assert!(
        pinned >= MIN_PINNED_CEILINGS,
        "{}: only {pinned} of this ladder's ceilings were asserted, floor is \
         {MIN_PINNED_CEILINGS}. A ladder whose columns are all `None` \
         measures and prints exactly as much as one that is pinned, and \
         proves nothing.",
        mode.label()
    );
}

fn pin(
    mode: Mode,
    snr_db: f64,
    column: &str,
    ber: f64,
    errors: usize,
    compared: usize,
    ceiling: Option<f64>,
) {
    if let Some(ceiling) = ceiling {
        assert!(
            ber <= ceiling,
            "{}: {column} BER at {snr_db} dB SNR is {ber:.4e} ({errors} errors in \
             {compared} bits), above the pinned ceiling {ceiling:.4e}",
            mode.label()
        );
    }
}

/// BER must not rise as SNR rises, within an absolute plus relative
/// slack (see [`MONOTONE_SLACK`] / [`MONOTONE_REL_SLACK`]).
fn check_monotonic(mode: Mode, column: &str, bers: &[f64]) {
    for w in bers.windows(2) {
        let slack = MONOTONE_SLACK + MONOTONE_REL_SLACK * w[0];
        assert!(
            w[1] <= w[0] + slack,
            "{}: {column} BER is not monotonic in SNR — {:.4e} at the lower rung \
             but {:.4e} at the higher one (slack {slack:.2e}); ladder {bers:?}",
            mode.label(),
            w[0],
            w[1]
        );
    }
}

/// Bell 202, 1200 baud, 48 kHz. Threshold-region rungs are 1 dB apart
/// because the whole transition is about that wide.
///
/// MEASURED at this test's seed:
///
/// | SNR dB | continuous | burst | perfect clock |
/// |-------:|-----------:|------:|--------------:|
/// |     −3 |   4.96e−1  | 4.26e−1 |     1.77e−2 |
/// |     −2 |   4.83e−1  | 3.53e−1 |     5.28e−3 |
/// |     −1 |   4.96e−1  | 9.80e−2 |     7.03e−4 |
/// |      0 |   5.02e−5  | 3.77e−4 |     1.00e−4 |
/// |      1 |        0   |     0   |          0  |
/// |     12 |        0   |     0   |          0  |
///
/// The ceilings on the two columns that depend on clock lock are loose in
/// the threshold region. Repeating the sweep over four unrelated seeds
/// gives, at 0 dB, continuous anywhere in 0 … 6.1·10⁻² and burst anywhere
/// in 0 … 1.7·10⁻²: whether the loop happens to die inside a given
/// 20 000-bit run is close to a coin toss there, so a tight pin would
/// fail on a change that was neutral or better. The perfect-clock column
/// has no such catastrophic mode (its spread over the same four seeds is
/// ±20%), which is why its pins are the tight, quantitative ones.
/// Continuous is robustly zero from 1 dB upwards; that is where it gets
/// pinned.
#[test]
fn raw_ber_bell_202() {
    check_ladder(
        Mode::Bell202,
        &[
            Rung {
                snr_db: -3.0,
                continuous: None, // below the transition; see `Rung`
                burst: None,      // likewise
                perfect_clock: Some(4.0e-2),
            },
            Rung {
                snr_db: -2.0,
                continuous: None,
                burst: None,
                perfect_clock: Some(1.2e-2),
            },
            Rung {
                snr_db: -1.0,
                continuous: None,
                // 4-seed worst case 6.8e-2.
                burst: Some(2.0e-1),
                perfect_clock: Some(2.0e-3),
            },
            Rung {
                snr_db: 0.0,
                continuous: None,    // 4-seed worst case 6.1e-2: a coin toss
                burst: Some(4.0e-2), // 4-seed worst case 1.7e-2
                perfect_clock: Some(3.0e-4),
            },
            Rung {
                // The sharpest rung in the file. One dB lower the clock
                // is a coin toss; here all four seeds are clean. A dB of
                // lost receiver quality fails every column at once.
                snr_db: 1.0,
                continuous: Some(1.0e-3),
                burst: Some(1.0e-4),
                perfect_clock: Some(1.0e-4),
            },
            Rung {
                snr_db: 12.0,
                continuous: Some(1.0e-4),
                burst: Some(1.0e-4),
                perfect_clock: Some(1.0e-4),
            },
        ],
        0x0BE1_1202,
    );
}

/// HF APRS 300 baud, 48 kHz. Its 19.0 dB of wideband processing gain buys
/// about 4–6 dB over Bell 202, as both this ladder and the two
/// sensitivity thresholds show.
///
/// MEASURED at this test's seed:
///
/// | SNR dB | continuous | burst | perfect clock |
/// |-------:|-----------:|------:|--------------:|
/// |     −8 |   4.90e−1  | 3.93e−1 |     3.98e−2 |
/// |     −6 |   4.53e−1  | 4.86e−2 |     1.18e−2 |
/// |     −5 |   1.74e−1  | 5.12e−3 |     4.67e−3 |
/// |     −4 |        0   |     0   |     1.16e−3 |
/// |     −2 |        0   |     0   |          0  |
/// |     12 |        0   |     0   |          0  |
///
/// Same ceiling policy as Bell 202: loose where clock lock is a coin toss
/// (4-seed spread at −5 dB is 1.1·10⁻⁴ … 2.3·10⁻² for burst and
/// 1.1·10⁻¹ … 4.0·10⁻¹ for continuous), tight on the perfect-clock column
/// and from −4 dB upwards, where all four seeds are clean.
#[test]
fn raw_ber_hf_300() {
    check_ladder(
        Mode::Hf300,
        &[
            Rung {
                snr_db: -8.0,
                continuous: None, // below the transition
                burst: None,
                perfect_clock: Some(8.0e-2),
            },
            Rung {
                snr_db: -6.0,
                continuous: None,
                burst: Some(2.0e-1), // 4-seed worst case 8.8e-2
                perfect_clock: Some(2.5e-2),
            },
            Rung {
                snr_db: -5.0,
                continuous: None,    // 4-seed worst case 4.0e-1
                burst: Some(6.0e-2), // 4-seed worst case 2.3e-2
                perfect_clock: Some(1.0e-2),
            },
            Rung {
                // First rung all four seeds decode cleanly through.
                snr_db: -4.0,
                continuous: Some(1.0e-3),
                burst: Some(1.0e-3),
                perfect_clock: Some(3.0e-3),
            },
            Rung {
                snr_db: -2.0,
                continuous: Some(1.0e-4),
                burst: Some(1.0e-4),
                perfect_clock: Some(2.0e-4),
            },
            Rung {
                snr_db: 12.0,
                continuous: Some(1.0e-4),
                burst: Some(1.0e-4),
                perfect_clock: Some(1.0e-4),
            },
        ],
        0x0300_0300,
    );
}

/// G3RUH 9600 baud baseband, 48 kHz. Only 5 samples per bit and only
/// 4.0 dB of wideband processing gain, so its threshold sits several dB
/// higher in SNR than Bell 202's — which is the expected ordering, not a
/// defect: it is carrying eight times the data rate in the same audio
/// bandwidth.
///
/// MEASURED (no perfect-clock column: the baseband front end's filter is
/// private, so there is no public metric to tap):
///
/// | SNR dB | continuous | burst |
/// |-------:|-----------:|------:|
/// |     −1 |   3.82e−1  | 1.37e−1 |
/// |      1 |   1.39e−1  | 6.52e−3 |
/// |      2 |   4.52e−4  | 4.42e−3 |
/// |      3 |        0   | 5.39e−5 |
/// |      5 |        0   |     0   |
/// |     12 |        0   |     0   |
///
/// 4-seed spreads: at 1 dB continuous ranges over 2.2·10⁻³ … 2.7·10⁻¹ and
/// burst over 2.0·10⁻³ … 2.0·10⁻²; from 4 dB upwards both are zero for
/// every seed. The ceilings follow that.
#[cfg(feature = "g3ruh")]
#[test]
fn raw_ber_g3ruh_9600() {
    check_ladder(
        Mode::G3ruh9600,
        &[
            Rung {
                snr_db: -1.0,
                continuous: None, // below the transition; see `Rung`
                burst: None,
                perfect_clock: None, // no public baseband front end
            },
            Rung {
                snr_db: 1.0,
                continuous: None,    // 4-seed worst case 2.7e-1
                burst: Some(6.0e-2), // 4-seed worst case 2.0e-2
                perfect_clock: None,
            },
            Rung {
                snr_db: 2.0,
                // Still inside the transition: G3RUH's measured
                // 50%-frame threshold is +1.25 dB, so this rung sits
                // only 0.75 dB above it and the continuous column is
                // bistable. RE-BASELINED when the ladder moved from a
                // single seed to a 4-seed pooled estimate: the pooled
                // value is 1.2e-2 against the 1.5e-3 a single seed
                // suggested. This is an estimator change, not a
                // receiver regression -- G3RUH decodes through
                // `BasebandDemodulator`, never the tone discriminator,
                // and its benchmark row is unchanged at 61 frames.
                continuous: Some(4.0e-2),
                burst: Some(8.0e-3), // 4-seed pooled 1.3e-3
                perfect_clock: None,
            },
            Rung {
                snr_db: 3.0,
                continuous: Some(5.0e-4), // 4-seed worst case 5.0e-5
                burst: Some(5.0e-4),      // 4-seed worst case 5.4e-5
                perfect_clock: None,
            },
            Rung {
                snr_db: 5.0,
                continuous: Some(1.0e-4),
                burst: Some(1.0e-4),
                perfect_clock: None,
            },
            Rung {
                snr_db: 12.0,
                continuous: Some(1.0e-4),
                burst: Some(1.0e-4),
                perfect_clock: None,
            },
        ],
        0x9600_9600,
    );
}

/// Guards the aligner, which is the part of Metric 1 most likely to be
/// silently wrong. An aligner stuck at "lag 0" would still report BER 0
/// on a clean channel for the tone modes (whose true lag *is* 0) while
/// quietly reporting BER ≈ 0.5 for G3RUH; an aligner scoring the wrong
/// interval would report nonsense everywhere.
///
/// On a nearly clean channel this asserts, per mode, that
///
/// * the winning lag scores zero errors;
/// * the runner-up lag scores at least a quarter of all scored bits, so
///   the minimum is a sharp unambiguous spike rather than a shallow dip;
/// * the winner is strictly inside the search window, so the true
///   minimum was not truncated by [`MAX_ALIGN`];
/// * the scored interval really is most of the payload, not a sliver.
#[test]
fn alignment_is_unambiguous_when_clean() {
    assert!(
        MODES.len() >= MIN_MODES,
        "{} modes to align, floor is {MIN_MODES}",
        MODES.len()
    );
    for &mode in MODES {
        let a = clean_alignment(mode);
        println!(
            "{:17} clean alignment: lag {}, errors {}, runner-up {} of {} scored",
            mode.label(),
            a.offset,
            a.errors,
            a.runner_up_errors,
            a.compared
        );
        assert_eq!(
            a.errors,
            0,
            "{}: {} errors at {CLEAN_SNR_DB} dB with the best of {} lags",
            mode.label(),
            a.errors,
            MAX_ALIGN + 1
        );
        assert!(
            a.runner_up_errors * 4 >= a.compared,
            "{}: alignment is ambiguous — winning lag {} scored {} errors but the \
             runner-up scored only {} of {} bits; a real alignment spike leaves the \
             runner-up near 50%",
            mode.label(),
            a.offset,
            a.errors,
            a.runner_up_errors,
            a.compared
        );
        assert!(
            a.offset < MAX_ALIGN,
            "{}: winning lag {} sits on the edge of the {}-bit search window; the \
             true minimum may lie outside it",
            mode.label(),
            a.offset,
            MAX_ALIGN
        );
        assert!(
            a.compared >= PAYLOAD_BITS - 4 * GUARD_BITS,
            "{}: only {} of {PAYLOAD_BITS} payload bits were scored",
            mode.label(),
            a.compared
        );
    }
}

/// Dense curve across each mode's whole transition, for humans.
///
/// `#[ignore]`d purely on cost: 0.5 dB steps over a 10 dB span for three
/// modes, three columns each, MEASURED at ~4 s in release against ~1.3 s
/// for the whole default set of this file — and it pins nothing the
/// ladders do not already pin. Run it when tuning the slicer or the
/// correlator, where seeing exactly *where* the transition sits is the
/// entire point.
#[test]
#[ignore = "dense 0.5 dB-step BER sweep; slow. Run with -- --ignored --nocapture"]
fn ber_curve_fine_sweep() {
    let spans: &[(Mode, f64, f64)] = &[
        (Mode::Bell202, -5.0, 5.0),
        (Mode::Hf300, -11.0, -1.0),
        #[cfg(feature = "g3ruh")]
        (Mode::G3ruh9600, -1.0, 9.0),
    ];
    for &(mode, lo, hi) in spans {
        println!("\n{} @ {SR_HZ} Hz — fine sweep, 0.5 dB steps", mode.label());
        println!("  SNR dB | Eb/N0 dB |   lag |   continuous |        burst | perfect clock");
        println!("  -------|----------|-------|--------------|--------------|--------------");
        let steps = ((hi - lo) / 0.5).round() as i32;
        for i in 0..=steps {
            let snr = lo + f64::from(i) * 0.5;
            let ca = continuous_ber(mode, snr, 0xF14E_0001);
            let (be, bn, blag) = burst_ber(mode, snr, 0xF14E_0001);
            let p = perfect_clock_ber(mode, snr, 0xF14E_0001);
            let head = format!(
                "  {:6.1} | {:8.1} | {:2} /{:2} | {:12.3e} | {:12.3e} | ",
                snr,
                eb_n0_db(snr, mode.baud_bps()),
                ca.offset,
                blag,
                ca.errors as f64 / ca.compared as f64,
                be as f64 / bn as f64
            );
            match p {
                Some(p) => println!("{head}{p:13.3e}"),
                None => println!("{head}(no public FE)"),
            }
        }
    }
}

// ---------------------------------------------------------------------
// Metric 2: sensitivity threshold in dB (50% frame recovery)
// ---------------------------------------------------------------------

#[cfg(feature = "tnc")]
mod sensitivity {
    use super::{Lcg, Mode, SR_HZ, mix, noise_peak};
    use yodel::SampleRate;
    use yodel::ax25::Address;
    use yodel::tnc::{DefaultTncReceiver, TncConfig, TncReceiver, TncTransmitter};

    /// Frames decoded per SNR evaluation. 24 makes the 50% crossing
    /// (12 frames) a clear majority decision while keeping a bisection
    /// affordable. The threshold is quoted to 0.25 dB, which is finer
    /// than 24 frames can really justify — treat the last digit as
    /// arbitration, not precision.
    const FRAMES: usize = 24;

    /// Bisection steps. Every bracket below is 16 dB wide, so 6 halvings
    /// resolve it to 0.25 dB.
    const BISECT_STEPS: u32 = 6;

    /// The `i`-th frame's information field: deterministic, varying in
    /// content so the measurement is not an artifact of one bit pattern.
    fn info(i: usize) -> [u8; 23] {
        let mut buf = *b"yodel sensitivity 0000 ";
        buf[18] = b'0' + ((i / 1000) % 10) as u8;
        buf[19] = b'0' + ((i / 100) % 10) as u8;
        buf[20] = b'0' + ((i / 10) % 10) as u8;
        buf[21] = b'0' + (i % 10) as u8;
        buf[22] = b'a' + (i % 26) as u8;
        buf
    }

    /// Decodes [`FRAMES`] frames at `snr_db` and returns how many came
    /// back with their information field byte-for-byte intact.
    ///
    /// Checking the payload, not just "a frame was emitted", is what
    /// makes this a recovery count rather than an FCS-collision count.
    /// A fresh receiver per frame keeps the trials independent — each
    /// frame acquires its clock from its own HDLC preamble, as a real
    /// burst does — while the noise stream runs continuously so no two
    /// frames see the same noise.
    fn recovered(mode: Mode, snr_db: f64, seed: u64) -> usize {
        let sr = SampleRate::new(SR_HZ).unwrap();
        let cfg = TncConfig::from_profile(sr, mode.profile()).unwrap();
        let tx = TncTransmitter::new(cfg);
        let dest = Address::new(b"APRS", 0).unwrap();
        let peak = noise_peak(snr_db);
        let mut rng = Lcg(seed);
        let mut ok = 0usize;
        for i in 0..FRAMES {
            let src = Address::new(b"N0CALL", (i % 16) as u8).unwrap();
            let payload = info(i);
            let mut frame_buf = [0u8; 330];
            let len = tx
                .build_frame_raw(dest, src, &[], &payload, &mut frame_buf)
                .unwrap();
            let mut rx: DefaultTncReceiver = TncReceiver::new(cfg).unwrap();
            let mut got = false;
            for s in tx.frame_samples_i16(&frame_buf[..len]) {
                if let Some(frame) = rx.push_i16(mix(s, &mut rng, peak))
                    && frame.info() == payload
                {
                    got = true;
                }
            }
            if got {
                ok += 1;
            }
        }
        ok
    }

    /// Locates the SNR in dB at which recovery crosses 50%, by bisection.
    ///
    /// Both bracket ends are asserted: `lo` must fail the majority and
    /// `hi` must pass it. Without those, an invalid bracket would make
    /// the returned "threshold" a fixed function of `lo` and `hi` — a
    /// number that could never regress and could never fail, which is
    /// worse than no test.
    ///
    /// Recovery is not guaranteed *strictly* monotonic in SNR, so what
    /// bisection converges on is *a* crossing inside the bracket. With a
    /// transition barely a dB wide (module docs) there is only one.
    fn threshold_db(mode: Mode, lo: f64, hi: f64, seed: u64) -> f64 {
        let majority = FRAMES / 2;
        let at_lo = recovered(mode, lo, seed);
        assert!(
            at_lo < majority,
            "{}: bracket floor {lo} dB already recovers {at_lo}/{FRAMES} — the 50% \
             crossing is below the bracket, so bisection would return {lo} \
             regardless of receiver quality",
            mode.label()
        );
        let at_hi = recovered(mode, hi, seed);
        assert!(
            at_hi >= majority,
            "{}: bracket ceiling {hi} dB recovers only {at_hi}/{FRAMES} — the 50% \
             crossing is above the bracket",
            mode.label()
        );

        let (mut lo, mut hi) = (lo, hi);
        for _ in 0..BISECT_STEPS {
            let mid = 0.5 * (lo + hi);
            let ok = recovered(mode, mid, seed);
            println!("  {:17} {mid:6.2} dB -> {ok:2}/{FRAMES}", mode.label());
            if ok >= majority { hi = mid } else { lo = mid }
        }
        hi
    }

    /// Measures the threshold and ratchets it.
    ///
    /// RATCHET: lower `pinned_db` when the receiver improves, never
    /// raise it. Unlike the BER ladders, this quantity is stable across
    /// noise draws — it averages 24 independent frames, so there is no
    /// single coin toss to lose. MEASURED over five seeds per mode the
    /// spread is one bisection step (0.25 dB) for Bell 202 and HF 300 and
    /// two for G3RUH, so each pin sits one step above the worst seed.
    fn check_threshold(mode: Mode, bracket: (f64, f64), pinned_db: f64, seed: u64) {
        println!(
            "\n{} @ {SR_HZ} Hz — 50% frame-recovery threshold, bisecting [{}, {}] dB",
            mode.label(),
            bracket.0,
            bracket.1
        );
        let got = threshold_db(mode, bracket.0, bracket.1, seed);
        println!(
            "  => threshold {got:.2} dB SNR (pinned: must stay at or below {pinned_db:.2} dB)"
        );
        assert!(
            got <= pinned_db,
            "{}: 50% frame-recovery threshold regressed to {got:.2} dB SNR; the \
             pinned record is {pinned_db:.2} dB (lower is better)",
            mode.label()
        );
    }

    /// Bell 202 at 48 kHz. MEASURED −2.50 dB SNR at this test's seed;
    /// −2.25 dB was the worst of four other unrelated seeds, so the pin
    /// sits one bisection step (0.25 dB) above that worst case.
    #[test]
    fn sensitivity_threshold_bell_202() {
        check_threshold(Mode::Bell202, (-8.0, 8.0), -2.0, 0x5E11_1202);
    }

    /// HF APRS 300 at 48 kHz. MEASURED −6.75 dB SNR (worst of five seeds
    /// −6.75) — 4.25 dB better than Bell 202, in the neighbourhood of the
    /// 6.0 dB of extra wideband processing gain its narrower baud buys.
    #[test]
    fn sensitivity_threshold_hf_300() {
        check_threshold(Mode::Hf300, (-14.0, 2.0), -6.5, 0x5E11_0300);
    }

    /// G3RUH 9600 baseband at 48 kHz. MEASURED 1.25 dB SNR (worst of five
    /// seeds 1.25; two of them reached 0.75). Consistent with
    /// `tests/g3ruh.rs`, which records 34/40 frames at 4 dB — comfortably
    /// above the crossing.
    #[cfg(feature = "g3ruh")]
    #[test]
    fn sensitivity_threshold_g3ruh_9600() {
        check_threshold(Mode::G3ruh9600, (-6.0, 10.0), 1.5, 0x5E11_9600);
    }
}
