//! Seeded-noise property suite: exact recovery through additive noise.
//!
//! # PRNG
//!
//! Hand-rolled, allocation-free, deterministic. Seeding uses **splitmix64**
//! (state += 0x9E3779B97F4A7C15 — the 64-bit golden-ratio increment — then
//! two xor-shift-multiply finalization rounds with the constants
//! 0xBF58476D1CE4E5B9 and 0x94D049BB133111EB); the stream generator is
//! **xorshift64\*** (shifts 12/25/27, output multiplier
//! 0x2545F4914F6CDD1D). Both are standard public-domain PRNG recipes with
//! well-studied constants. No wall clock is consulted anywhere: every case
//! derives from a fixed literal seed, so failures reproduce exactly.
//!
//! # SNR
//!
//! Additive white noise is mixed in at a target signal-to-noise ratio
//! defined against the tone's RMS: for a full-scale sine (peak 32767,
//! RMS 32767/√2 ≈ 23170), noise samples are drawn uniformly from
//! `[-A, A]` where `A = rms / 10^(SNR_dB/20) · √3` (uniform noise RMS is
//! `A/√3`).
//!
//! # Pinned behavior
//!
//! * At **20 dB SNR** the modem recovers 100% of payloads across 2000+
//!   seeded cases spanning all five sample rates (established empirically,
//!   pinned here — regressions fail loudly).
//! * At **0 dB SNR** (noise as strong as signal) recovery still succeeds in
//!   the majority of cases at 48 kHz; the pinned floor documents the
//!   observed behavior without promising perfection.

use yodel::demodulator::{AfskDemodulator, DemodulatorConfig};
use yodel::modulator::{Modulator, ModulatorConfig};
use yodel::{Bit, SampleRate};

const RATES: [u32; 5] = [8_000, 11_025, 22_050, 44_100, 48_000];

/// splitmix64: golden-ratio increment + two finalization rounds.
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// xorshift64* stream generator (shifts 12/25/27, multiplier below).
struct Rng(u64);

impl Rng {
    /// Seeds via splitmix64 so nearby integer seeds give unrelated streams.
    fn new(seed: u64) -> Self {
        let mut s = seed;
        let mut state = splitmix64(&mut s);
        if state == 0 {
            state = 0x2545_F491_4F6C_DD1D; // xorshift state must be nonzero
        }
        Rng(state)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in [-1.0, 1.0).
    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 52) as f64 - 1.0
    }

    fn next_bit(&mut self) -> Bit {
        if self.next_u64() & 1 == 1 {
            Bit::One
        } else {
            Bit::Zero
        }
    }
}

/// Peak amplitude of uniform noise for a target SNR (dB) against a
/// full-scale sine's RMS. See module docs.
fn noise_peak(snr_db: f64) -> f64 {
    let signal_rms = 32_767.0 / core::f64::consts::SQRT_2;
    let noise_rms = signal_rms / 10f64.powf(snr_db / 20.0);
    noise_rms * 3f64.sqrt()
}

/// One seeded case: random payload, modulate, add noise, demodulate.
/// Returns true when the payload is recovered exactly.
fn case_recovers(sr_hz: u32, seed: u64, snr_db: f64, payload_len: usize) -> bool {
    let mut rng = Rng::new(seed);
    let payload: Vec<Bit> = (0..payload_len).map(|_| rng.next_bit()).collect();

    let mut tx: Vec<Bit> = (0..32)
        .map(|i| if i % 2 == 0 { Bit::One } else { Bit::Zero })
        .collect();
    tx.extend_from_slice(&payload);
    tx.push(Bit::One);
    tx.push(Bit::Zero);

    let sr = SampleRate::new(sr_hz).unwrap();
    let modulator = Modulator::new(ModulatorConfig::bell_202(sr).unwrap());
    let peak = noise_peak(snr_db);
    let noisy = modulator.i16_samples(tx.iter().copied()).map(|s| {
        let n = rng.next_f64() * peak;
        (s as f64 + n).clamp(i16::MIN as f64, i16::MAX as f64) as i16
    });

    let demod = AfskDemodulator::new(DemodulatorConfig::bell_202(sr).unwrap()).unwrap();
    let recovered: Vec<Bit> = demod.i16_bits(noisy).collect();
    recovered
        .windows(payload.len())
        .any(|w| w == payload.as_slice())
}

/// Pinned moderate SNR at which recovery must be perfect (see module docs).
const CLEAN_SNR_DB: f64 = 20.0;
const CASES_PER_RATE: u64 = 500; // 5 rates × 500 = 2500 seeded cases

macro_rules! snr20_tests {
    ($($name:ident: $sr:expr, $seed_base:expr;)*) => {$(
        #[test]
        fn $name() {
            let mut failures = 0u32;
            for i in 0..CASES_PER_RATE {
                if !case_recovers($sr, $seed_base + i, CLEAN_SNR_DB, 48) {
                    failures += 1;
                }
            }
            assert_eq!(failures, 0, "rate {}: {failures}/{CASES_PER_RATE} failed at 20 dB", $sr);
        }
    )*};
}

snr20_tests! {
    perfect_recovery_20db_8000: 8_000, 0x0800_0000;
    perfect_recovery_20db_11025: 11_025, 0x1102_5000;
    perfect_recovery_20db_22050: 22_050, 0x2205_0000;
    perfect_recovery_20db_44100: 44_100, 0x4410_0000;
    perfect_recovery_20db_48000: 48_000, 0x4800_0000;
}

macro_rules! snr10_tests {
    ($($name:ident: $sr:expr, $seed_base:expr;)*) => {$(
        /// 10 dB is still comfortably above the correlator's noise floor:
        /// pinned perfect recovery over 200 seeded cases per rate.
        #[test]
        fn $name() {
            let mut failures = 0u32;
            for i in 0..200u64 {
                if !case_recovers($sr, $seed_base + i, 10.0, 48) {
                    failures += 1;
                }
            }
            assert_eq!(failures, 0, "rate {}: {failures}/200 failed at 10 dB", $sr);
        }
    )*};
}

snr10_tests! {
    perfect_recovery_10db_8000: 8_000, 0xA800_0000;
    perfect_recovery_10db_11025: 11_025, 0xA110_2500;
    perfect_recovery_10db_22050: 22_050, 0xA220_5000;
    perfect_recovery_10db_44100: 44_100, 0xA441_0000;
    perfect_recovery_10db_48000: 48_000, 0xA480_0000;
}

/// Noise-floor record: at 0 dB SNR the modem still recovers most payloads
/// at 48 kHz. The bound is loose (>= 60%) — it documents observed
/// behavior and catches catastrophic regressions, not a spec.
#[test]
fn noise_floor_behavior_0db_48000() {
    let mut ok = 0u32;
    for i in 0..200u64 {
        if case_recovers(48_000, 0xF000_0000 + i, 0.0, 48) {
            ok += 1;
        }
    }
    assert!(ok >= 120, "0 dB recovery collapsed: {ok}/200");
}

/// Long payloads (256 bits) at the pinned 20 dB SNR, all rates.
#[test]
fn long_payloads_20db_all_rates() {
    for (r, sr) in RATES.iter().enumerate() {
        for i in 0..20u64 {
            assert!(
                case_recovers(*sr, 0xBEEF_0000 + (r as u64) * 100 + i, CLEAN_SNR_DB, 256),
                "rate {sr} seed {i}: long payload lost at 20 dB"
            );
        }
    }
}

/// Determinism: the same seed always yields the same outcome.
#[test]
fn cases_are_deterministic() {
    for seed in [1u64, 42, 0xDEAD_BEEF] {
        let a = case_recovers(48_000, seed, 6.0, 48);
        let b = case_recovers(48_000, seed, 6.0, 48);
        assert_eq!(a, b, "seed {seed} not deterministic");
    }
}

/// PRNG sanity: splitmix64 finalizer produces the documented reference
/// stream for seed 0 (first output pinned) and xorshift64* never yields 0.
#[test]
fn prng_reference_values() {
    let mut s = 0u64;
    assert_eq!(splitmix64(&mut s), 0xE220_A839_7B1D_CDAF);
    let mut rng = Rng::new(12345);
    for _ in 0..10_000 {
        assert_ne!(rng.next_u64(), 0);
    }
}

/// The uniform noise generator is roughly zero-mean (deterministic seed).
#[test]
fn noise_is_zero_mean() {
    let mut rng = Rng::new(777);
    let mean: f64 = (0..100_000).map(|_| rng.next_f64()).sum::<f64>() / 100_000.0;
    assert!(mean.abs() < 0.01, "biased noise: mean {mean}");
}

macro_rules! snr15_tests {
    ($($name:ident: $sr:expr, $seed_base:expr;)*) => {$(
        /// 15 dB sits between the pinned 20 dB and 10 dB perfect-recovery
        /// points; recovery must likewise be perfect over 100 seeded cases.
        #[test]
        fn $name() {
            let mut failures = 0u32;
            for i in 0..100u64 {
                if !case_recovers($sr, $seed_base + i, 15.0, 48) {
                    failures += 1;
                }
            }
            assert_eq!(failures, 0, "rate {}: {failures}/100 failed at 15 dB", $sr);
        }
    )*};
}

snr15_tests! {
    perfect_recovery_15db_8000: 8_000, 0x1580_0000;
    perfect_recovery_15db_11025: 11_025, 0x1511_0250;
    perfect_recovery_15db_22050: 22_050, 0x1522_0500;
    perfect_recovery_15db_44100: 44_100, 0x1544_1000;
    perfect_recovery_15db_48000: 48_000, 0x1548_0000;
}
