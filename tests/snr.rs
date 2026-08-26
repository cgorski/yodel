//! Pinned seeded-noise SNR ladder for the full TNC pipeline.
//!
//! Thirty deterministic APRS frames are modulated (11 025 Hz for the
//! 20/10 dB rungs to keep runtime low; 44 100 Hz for the harder 5/0 dB
//! rungs, where the correlator has more samples per bit), mixed
//! with seeded uniform white noise at exact SNR levels (defined against
//! a full-scale sine's RMS, the modulator's output level), and pushed
//! through [`TncReceiver`]. The per-level success counts are exact and
//! reproducible (fixed-seed LCG, no wall clock), so the minimums pinned
//! here are the measured values — any regression fails loudly.
//!
//! | SNR (dB) | rate (Hz) | measured | pinned minimum (of 30) |
//! |---------:|----------:|---------:|-----------------------:|
//! |       20 |    11 025 |       30 |                     30 |
//! |       10 |    11 025 |       30 |                     30 |
//! |        5 |    44 100 |       30 |                     30 |
//! |        0 |    44 100 |       24 |                     24 |

#![cfg(feature = "tnc")]

use yodel::SampleRate;
use yodel::aprs::{AprsPacket, Latitude, Longitude, Position, Status, Symbol};
use yodel::ax25::Address;
use yodel::geo::{Ambiguity, UNITS_PER_HUNDREDTH_MINUTE};
use yodel::tnc::{DefaultTncReceiver, TncConfig, TncReceiver, TncTransmitter};

/// 64-bit LCG (Knuth MMIX constants). Deterministic.
struct Lcg(u64);

impl Lcg {
    fn next_u64(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0
    }

    /// Uniform in [-1.0, 1.0).
    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 52) as f64 - 1.0
    }
}

/// Peak amplitude of uniform noise for a target SNR (dB) against a
/// full-scale sine's RMS (uniform noise RMS is `peak/√3`).
fn noise_peak(snr_db: f64) -> f64 {
    let signal_rms = 32_767.0 / core::f64::consts::SQRT_2;
    let noise_rms = signal_rms / 10f64.powf(snr_db / 20.0);
    noise_rms * 3f64.sqrt()
}

const FRAMES: usize = 30;

/// The `i`-th deterministic frame of the ladder corpus: alternating
/// position and status packets with varying content, each transmitted
/// as clean PCM at `sr_hz`.
fn frame_samples(i: usize, sr_hz: u32) -> Vec<i16> {
    let sr = SampleRate::new(sr_hz).unwrap();
    let cfg = TncConfig::bell_202(sr).unwrap();
    let tx = TncTransmitter::new(cfg);

    let comment_pool: [&[u8]; 3] = [b"snr ladder", b"yodel", b"fixed corpus frame"];
    let status_pool: [&[u8]; 3] = [b"SNR ladder status", b"ok", b"thirty frames of pcm"];
    let packet = if i.is_multiple_of(2) {
        AprsPacket::Position(Position {
            ambiguity: Ambiguity::EXACT,
            latitude: Latitude::new(
                ((i as i64 - 15) * 6000 + (i as i64) * 37) * UNITS_PER_HUNDREDTH_MINUTE,
            )
            .unwrap(),
            longitude: Longitude::new(
                ((15 - i as i64) * 6000 - (i as i64) * 53) * UNITS_PER_HUNDREDTH_MINUTE,
            )
            .unwrap(),
            symbol: Symbol::HOUSE,
            messaging: i.is_multiple_of(4),
            compressed: i.is_multiple_of(6),
            extension: None,
            comment: comment_pool[i % 3],
        })
    } else {
        AprsPacket::Status(Status {
            text: status_pool[i % 3],
        })
    };

    let dest = Address::new(b"APRS", 0).unwrap();
    let src = Address::new(b"N0CALL", (i % 16) as u8).unwrap();
    let mut info_buf = [0u8; 330];
    let mut frame_buf = [0u8; 330];
    tx.transmit_i16(&packet, dest, src, &[], &mut info_buf, &mut frame_buf)
        .unwrap()
        .collect()
}

/// Decodes all thirty frames at `snr_db`; returns the success count.
fn successes_at(sr_hz: u32, snr_db: f64, seed: u64) -> u32 {
    let sr = SampleRate::new(sr_hz).unwrap();
    let cfg = TncConfig::bell_202(sr).unwrap();
    let peak = noise_peak(snr_db);
    let mut rng = Lcg(seed);
    let mut ok = 0u32;
    for i in 0..FRAMES {
        let mut rx: DefaultTncReceiver = TncReceiver::new(cfg).unwrap();
        let mut decoded = false;
        for s in frame_samples(i, sr_hz) {
            let noisy = (f64::from(s) + rng.next_f64() * peak)
                .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16;
            if rx.push_i16(noisy).is_some() {
                decoded = true;
            }
        }
        if decoded {
            ok += 1;
        }
    }
    ok
}

#[test]
fn snr_ladder_20db_perfect() {
    assert_eq!(
        successes_at(11_025, 20.0, 0x5EED_2020),
        30,
        "20 dB must be perfect"
    );
}

#[test]
fn snr_ladder_10db_perfect() {
    assert_eq!(
        successes_at(11_025, 10.0, 0x5EED_1010),
        30,
        "10 dB must be perfect"
    );
}

#[test]
fn snr_ladder_5db_pinned() {
    let ok = successes_at(44_100, 5.0, 0x5EED_0505);
    assert!(ok >= 30, "5 dB: {ok}/30, pinned minimum 30");
}

#[test]
fn snr_ladder_0db_pinned() {
    let ok = successes_at(44_100, 0.0, 0x5EED_0000);
    assert!(ok >= 24, "0 dB: {ok}/30, pinned minimum 24 (measured: 24)");
}
