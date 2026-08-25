//! Regression tests for the bounded-latency receive configuration:
//! `RecoveryPolicy::None` must disable every repair sweep — including
//! the cross-chain voting path — and `TncConfig::bounded_latency()`
//! must leave clean-signal decoding intact.
#![cfg(feature = "tnc")]

use yodel::SampleRate;
use yodel::ax25::{Address, RecoveryPolicy, crc16_x25};
use yodel::tnc::{ChainVoting, DefaultTncReceiver, TncConfig, TncReceiver, TncTransmitter};

fn config() -> TncConfig {
    TncConfig::bell_202(SampleRate::new(44_100).unwrap()).unwrap()
}

fn addr(callsign: &[u8], ssid: u8) -> Address {
    Address::new(callsign, ssid).unwrap()
}

/// Serializes `body` + `fcs` to AFSK samples by hand (flags, LSB-first
/// bytes, zero stuffing) so a frame corrupted on purpose reaches the
/// deframers intact.
fn manual_afsk_samples(body: &[u8], fcs: u16) -> Vec<i16> {
    use yodel::modulator::{Modulator, ModulatorConfig};
    use yodel::{Bit, nrzi};

    let mut bits: Vec<Bit> = Vec::new();
    let push_flag = |bits: &mut Vec<Bit>| {
        for i in 0..8 {
            bits.push(if (0x7Eu8 >> i) & 1 == 1 {
                Bit::One
            } else {
                Bit::Zero
            });
        }
    };
    for _ in 0..16 {
        push_flag(&mut bits);
    }
    let mut ones = 0u32;
    for byte in body.iter().copied().chain(fcs.to_le_bytes()) {
        for i in 0..8 {
            if (byte >> i) & 1 == 1 {
                bits.push(Bit::One);
                ones += 1;
                if ones == 5 {
                    bits.push(Bit::Zero);
                    ones = 0;
                }
            } else {
                bits.push(Bit::Zero);
                ones = 0;
            }
        }
    }
    push_flag(&mut bits);
    push_flag(&mut bits);

    let sr = SampleRate::new(44_100).unwrap();
    let modulator = Modulator::new(ModulatorConfig::bell_202(sr).unwrap());
    modulator
        .i16_samples(nrzi::encode_iter(bits.into_iter()))
        .collect()
}

/// A single-bit-corrupted transmission of `text`, plus its intact body
/// for comparison.
fn corrupted_stream(text: &[u8]) -> Vec<i16> {
    let cfg = config();
    let tx = TncTransmitter::new(cfg);
    let mut frame_buf = [0u8; 330];
    let len = tx
        .build_frame_raw(
            addr(b"APRS", 0),
            addr(b"N0CALL", 7),
            &[],
            text,
            &mut frame_buf,
        )
        .unwrap();
    let body = &frame_buf[..len];
    let fcs = crc16_x25(body);
    // One flipped bit in the info field: recoverable by single-bit
    // repair, hence certainly by the pre-destuff sweep.
    let mut corrupted = body.to_vec();
    corrupted[len - 3] ^= 0x08;
    manual_afsk_samples(&corrupted, fcs)
}

/// `RecoveryPolicy::None` must be honored in the cross-chain voting
/// path too: a single-bit-corrupted frame that the repair machinery
/// WOULD recover (proven by the default-policy control) must stay
/// rejected — no repair sweep may run — even with `ChainVoting::On`
/// requested, whose window validation ends in a pre-destuff sweep.
#[test]
fn recovery_none_with_voting_on_performs_no_repair_sweep() {
    let samples = corrupted_stream(b"repairable");

    // Control: the default policy (PreDestuffFlip + voting) repairs it.
    let mut rx: DefaultTncReceiver = TncReceiver::new(config()).unwrap();
    let repaired = samples
        .iter()
        .filter(|&&s| rx.push_i16(s).is_some())
        .count();
    assert_eq!(repaired, 1, "control: default policy must repair the frame");

    // RecoveryPolicy::None + voting explicitly On: no sweep anywhere,
    // so the same stream must be rejected as a plain FCS error.
    let bounded = config()
        .with_recovery(RecoveryPolicy::None)
        .with_voting(ChainVoting::On);
    let mut rx: DefaultTncReceiver = TncReceiver::new(bounded).unwrap();
    let decoded = samples
        .iter()
        .filter(|&&s| rx.push_i16(s).is_some())
        .count();
    assert_eq!(decoded, 0, "RecoveryPolicy::None must reject, not repair");
    assert_eq!(rx.stats().frames_ok, 0);
    assert!(rx.stats().fcs_errors >= 1, "{:?}", rx.stats());
}

/// The documented bounded-latency preset sets the promised knobs and
/// rejects (never repairs) corrupted frames.
#[test]
fn bounded_latency_disables_all_repair() {
    let cfg = config().bounded_latency();
    assert_eq!(cfg.recovery(), RecoveryPolicy::None);
    assert_eq!(cfg.voting(), ChainVoting::Off);

    let samples = corrupted_stream(b"repairable");
    let mut rx: DefaultTncReceiver = TncReceiver::new(cfg).unwrap();
    let decoded = samples
        .iter()
        .filter(|&&s| rx.push_i16(s).is_some())
        .count();
    assert_eq!(decoded, 0, "bounded latency must reject corrupted frames");
}

/// Clean-signal decode is unaffected by the bounded-latency preset:
/// undamaged frames never enter any repair path.
#[test]
fn bounded_latency_decodes_clean_signal() {
    let cfg = config().bounded_latency();
    let tx = TncTransmitter::new(cfg);
    let mut rx: DefaultTncReceiver = TncReceiver::new(cfg).unwrap();
    let mut frame_buf = [0u8; 330];
    let len = tx
        .build_frame_raw(
            addr(b"APRS", 0),
            addr(b"N0CALL", 7),
            &[],
            b"clean bounded",
            &mut frame_buf,
        )
        .unwrap();
    let mut frames = 0u32;
    for s in tx.frame_samples_i16(&frame_buf[..len]) {
        if let Some(frame) = rx.push_i16(s) {
            assert_eq!(frame.info(), b"clean bounded");
            frames += 1;
        }
    }
    assert_eq!(frames, 1, "clean-signal decode must be unaffected");
    assert_eq!(rx.stats().frames_ok, 1);
}
