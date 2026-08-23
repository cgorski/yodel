//! Parallel slicer bank: tilted-channel decode and duplicate-emission
//! tests through the public `TncReceiver` path.
//!
//! A pre- or de-emphasized channel attenuates one of the two AFSK tones
//! by several dB. The receiver's swept space-gain slicer bank must decode
//! a full frame with either tone ~5 dB down, and a frame decodable by
//! several chains at once must be emitted exactly once.

use warble::SampleRate;
use warble::aprs::{AprsPacket, Latitude, Longitude, Position, Symbol};
use warble::ax25::Address;
use warble::geo::Ambiguity;
use warble::tnc::{DefaultTncReceiver, SpaceGainSweep, TncConfig, TncReceiver, TncTransmitter};

const SR: u32 = 48_000;

fn config() -> TncConfig {
    TncConfig::bell_202(SampleRate::new(SR).unwrap()).unwrap()
}

fn packet() -> AprsPacket<'static> {
    AprsPacket::Position(Position {
        ambiguity: Ambiguity::EXACT,
        latitude: Latitude::new(34 * 6000 + 1234).unwrap(),
        longitude: Longitude::new(-(118 * 6000 + 4321)).unwrap(),
        symbol: Symbol::CAR,
        messaging: false,
        compressed: false,
        extension: None,
        comment: b"slicer bank test",
    })
}

/// Transmits one frame with per-tone amplitude scaling (`num/32`; 18/32
/// is about -5 dB) applied to the modulated samples according to the
/// instantaneous bit, and counts decoded frames.
///
/// Scaling the composite waveform per sample is a crude tilt model, but
/// mirrors what an emphasis network does to the tone amplitudes closely
/// enough for a decode/no-decode assertion.
fn decode_count_tilted(mark_num: i32, space_num: i32) -> u32 {
    let cfg = config();
    let tx = TncTransmitter::new(cfg);
    let mut rx: DefaultTncReceiver = TncReceiver::new(cfg).unwrap();
    let dest = Address::new(b"APRS", 0).unwrap();
    let src = Address::new(b"N0CALL", 7).unwrap();
    let mut info_buf = [0u8; 330];
    let mut frame_buf = [0u8; 330];
    let samples = tx
        .transmit_i16(&packet(), dest, src, &[], &mut info_buf, &mut frame_buf)
        .unwrap();
    // Track the dominant tone by the sample-to-sample phase step of the
    // (continuous-phase) modulator output: approximate by attenuating
    // through a one-pole split is overkill — instead, tilt by frequency
    // using a simple first-order high/low shelf: y[n] = a·x[n] + b·x[n-1]
    // chosen so 1200 Hz and 2200 Hz see the requested amplitudes.
    // For a decode/no-decode test a two-tap FIR suffices: with
    // y[n] = g0·x[n] − g1·x[n−1], the magnitude response at f is
    // sqrt(g0² + g1² − 2·g0·g1·cos(2πf/Fs)), monotone in f, so solving the
    // two-tap gains for the two tone amplitudes realizes the tilt.
    let (g0, g1) = solve_two_tap(mark_num as f64 / 32.0, space_num as f64 / 32.0);
    let mut prev = 0.0f64;
    let mut frames = 0u32;
    for s in samples {
        let x = s as f64;
        let y = g0 * x + g1 * prev;
        prev = x;
        let tilted = y.clamp(-32768.0, 32767.0) as i16;
        if rx.push_i16(tilted).is_some() {
            frames += 1;
        }
    }
    // Flush with a bit of silence so trailing state settles.
    for _ in 0..(SR / 100) {
        if rx.push_i16(0).is_some() {
            frames += 1;
        }
    }
    frames
}

/// Solves `y[n] = g0·x[n] + g1·x[n−1]` for gains `a_mark` at 1200 Hz and
/// `a_space` at 2200 Hz (both at 48 kHz).
fn solve_two_tap(a_mark: f64, a_space: f64) -> (f64, f64) {
    // |H(f)|² = g0² + g1² + 2·g0·g1·cos(w). Two equations, two unknowns;
    // solve numerically by bisection on the ratio r = g1/g0.
    let wm = 2.0 * core::f64::consts::PI * 1200.0 / SR as f64;
    let ws = 2.0 * core::f64::consts::PI * 2200.0 / SR as f64;
    let target = (a_space / a_mark).powi(2);
    let ratio_at = |r: f64| {
        let num = 1.0 + r * r + 2.0 * r * ws.cos();
        let den = 1.0 + r * r + 2.0 * r * wm.cos();
        num / den
    };
    let (mut lo, mut hi) = (-0.999f64, 0.999f64);
    for _ in 0..200 {
        let mid = 0.5 * (lo + hi);
        // ratio_at is monotone decreasing in r on (-1, 1) for wm < ws.
        if ratio_at(mid) > target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let r = 0.5 * (lo + hi);
    let g0 = a_mark / (1.0 + r * r + 2.0 * r * wm.cos()).sqrt();
    (g0, g0 * r)
}

#[test]
fn flat_channel_decodes_once() {
    assert_eq!(decode_count_tilted(32, 32), 1);
}

#[test]
fn space_tone_attenuated_5db_decodes() {
    // De-emphasized channel: 2200 Hz space ~5 dB below mark.
    assert_eq!(decode_count_tilted(32, 18), 1);
}

#[test]
fn mark_tone_attenuated_5db_decodes() {
    // Pre-emphasized channel: 1200 Hz mark ~5 dB below space.
    assert_eq!(decode_count_tilted(18, 32), 1);
}

/// A clean flat-channel frame is decodable by several adjacent-gain
/// chains simultaneously; the merge must still emit it exactly once
/// (asserted by `flat_channel_decodes_once` above with the full default
/// sweep, and here with a sweep of two identical unity chains, which by
/// construction decode the same frame on the same sample).
#[test]
fn duplicate_chains_emit_once() {
    let sweep = SpaceGainSweep::new(&[256, 256]).unwrap();
    let cfg = config().with_space_gain_sweep(sweep);
    let tx = TncTransmitter::new(cfg);
    let mut rx: DefaultTncReceiver = TncReceiver::new(cfg).unwrap();
    let dest = Address::new(b"APRS", 0).unwrap();
    let src = Address::new(b"N0CALL", 7).unwrap();
    let mut info_buf = [0u8; 330];
    let mut frame_buf = [0u8; 330];
    let samples = tx
        .transmit_i16(&packet(), dest, src, &[], &mut info_buf, &mut frame_buf)
        .unwrap();
    let mut frames = 0u32;
    for s in samples {
        if rx.push_i16(s).is_some() {
            frames += 1;
        }
    }
    assert_eq!(frames, 1, "duplicate chain emissions not merged");
    assert_eq!(rx.stats().frames_ok, 1);
}

/// Sweep validation: empty, oversized, and zero-gain sweeps are rejected
/// with typed errors.
#[test]
fn sweep_validation() {
    assert!(SpaceGainSweep::new(&[]).is_err());
    assert!(SpaceGainSweep::new(&[256; 10]).is_err());
    assert!(SpaceGainSweep::new(&[256, 0, 128]).is_err());
    let ok = SpaceGainSweep::new(&[128, 256, 512]).unwrap();
    assert_eq!(ok.gains(), &[128, 256, 512]);
    assert_eq!(ok.len(), 3);
    assert!(!ok.is_empty());
}
