//! Audio round-trip tests for the IL2P integration: modulate an IL2P
//! frame ([`il2p::tx_bits`] → NRZI → tone/baseband synthesis), receive
//! it through the demodulator + [`Il2pReceiver`] chain, and require
//! exact frame recovery — clean, under correctable corruption, and
//! coexisting with plain AX.25/HDLC traffic.
#![cfg(all(feature = "il2p", feature = "mod", feature = "demod"))]

use warble::SampleRate;
use warble::ax25::UiFrame;
use warble::demodulator::{AfskDemodulator, DemodulatorConfig};

use warble::il2p::{
    self, ENCODED_MAX, HEADER_LEN, HEADER_PARITY_LEN, Il2pParity, Il2pReceiver, SYNC_LEN,
    encode_ui_frame,
};
use warble::modulator::{Modulator, ModulatorConfig};

const RATE: u32 = 48_000;

/// Preamble/tail 0x55 bytes around each transmission: enough for the
/// receive clock recovery to lock before the sync word.
const PREAMBLE_BYTES: usize = 16;
const TAIL_BYTES: usize = 2;

fn sample_rate() -> SampleRate {
    SampleRate::new(RATE).unwrap()
}

fn addr(call: &[u8], ssid: u8) -> warble::ax25::Address {
    warble::ax25::Address::new(call, ssid).unwrap()
}

/// Encodes `frame` as IL2P, returning the encoded bytes (sync included).
fn encoded(frame: &UiFrame<'_>, parity: Il2pParity) -> Vec<u8> {
    let mut tx = [0u8; ENCODED_MAX];
    let len = encode_ui_frame(frame, parity, &mut tx).unwrap();
    tx[..len].to_vec()
}

/// Modulates IL2P frame bytes into 1200-baud Bell 202 `i16` audio.
fn modulate_afsk(bytes: &[u8]) -> Vec<i16> {
    Modulator::new(ModulatorConfig::bell_202(sample_rate()).unwrap())
        .i16_samples(il2p::tx_bits(bytes, PREAMBLE_BYTES, TAIL_BYTES))
        .collect()
}

/// Runs Bell 202 audio through demod → [`Il2pReceiver`], collecting
/// every recovered UI frame with its corrected-symbol count.
///
/// There is no NRZI stage: IL2P is not differentially encoded (spec
/// v0.6, "Interface to Physical Layer").
fn receive_afsk(audio: &[i16], parity: Il2pParity) -> Vec<(Vec<u8>, usize)> {
    let mut demod =
        AfskDemodulator::new(DemodulatorConfig::bell_202(sample_rate()).unwrap()).unwrap();
    let mut rx = Il2pReceiver::new(parity);
    let mut frames = Vec::new();
    for &s in audio {
        let Some(line) = demod.push_sample_i16(s) else {
            continue;
        };
        if let Some(Ok(rxf)) = rx.push(line) {
            let corrected = rxf.corrected();
            let ui = rxf.ui_frame().unwrap();
            let mut buf = [0u8; 1200];
            let len = ui.build(&mut buf).unwrap();
            frames.push((buf[..len].to_vec(), corrected));
        }
    }
    frames
}

/// The frame body a UI frame serializes to (for comparisons).
fn body(frame: &UiFrame<'_>) -> Vec<u8> {
    let mut buf = [0u8; 1200];
    let len = frame.build(&mut buf).unwrap();
    buf[..len].to_vec()
}

#[test]
fn il2p_afsk_1200_round_trip() {
    let frame = UiFrame::new(addr(b"APRS", 0), addr(b"N0CALL", 7), b">il2p over Bell 202");
    let tx = encoded(&frame, Il2pParity::Sixteen);
    let audio = modulate_afsk(&tx);
    let got = receive_afsk(&audio, Il2pParity::Sixteen);
    assert_eq!(got, vec![(body(&frame), 0)]);
}

#[test]
fn il2p_afsk_multiple_frames_back_to_back() {
    let frames = [
        UiFrame::new(addr(b"APRS", 0), addr(b"N1CALL", 1), b">first"),
        UiFrame::new(
            addr(b"APRS", 0),
            addr(b"N2CALL", 2),
            b"!4903.50N/07201.75W-",
        ),
        UiFrame::new(
            addr(b"APRS", 0),
            addr(b"N3CALL", 3),
            b":N1CALL   :hi il2p{7",
        ),
    ];
    let mut audio = Vec::new();
    for frame in &frames {
        let tx = encoded(frame, Il2pParity::Eight);
        audio.extend(modulate_afsk(&tx));
    }
    let got = receive_afsk(&audio, Il2pParity::Eight);
    let expect: Vec<(Vec<u8>, usize)> = frames.iter().map(|f| (body(f), 0)).collect();
    assert_eq!(got, expect);
}

#[test]
fn il2p_afsk_corrects_payload_symbol_errors() {
    // Corrupt t payload-region bytes of the modulated stream (in the
    // frame bytes before modulation — the audio carries them verbatim)
    // and require exact recovery with a nonzero corrected count.
    let frame = UiFrame::new(
        addr(b"APRS", 0),
        addr(b"N0CALL", 7),
        b">forward error correction of the payload blocks",
    );
    let parity = Il2pParity::Sixteen;
    let mut tx = encoded(&frame, parity);
    let t = parity.correctable();
    // Payload region: after sync + header codeblock; a single block
    // here, so its data spans up to the block parity at the end.
    let payload_at = SYNC_LEN + HEADER_LEN + HEADER_PARITY_LEN;
    let payload_len = frame.info.len();
    for e in 0..t {
        tx[payload_at + (e * payload_len) / t] ^= 0xA5;
    }
    let audio = modulate_afsk(&tx);
    let got = receive_afsk(&audio, parity);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].0, body(&frame));
    assert_eq!(got[0].1, t);
}

#[test]
fn il2p_afsk_rejects_beyond_t_errors() {
    let frame = UiFrame::new(addr(b"APRS", 0), addr(b"N0CALL", 7), b">too much damage");
    let parity = Il2pParity::Two;
    let mut tx = encoded(&frame, parity);
    let payload_at = SYNC_LEN + HEADER_LEN + HEADER_PARITY_LEN;
    for e in 0..(parity.correctable() + 1) {
        tx[payload_at + 3 * e] ^= 0x5A;
    }
    let audio = modulate_afsk(&tx);
    let got = receive_afsk(&audio, parity);
    assert!(got.is_empty());
}

#[test]
fn il2p_sync_word_tolerates_one_bit_error() {
    let frame = UiFrame::new(addr(b"APRS", 0), addr(b"N0CALL", 7), b">sync damage");
    let mut tx = encoded(&frame, Il2pParity::Sixteen);
    tx[1] ^= 0x08; // one flipped bit inside the sync word
    let audio = modulate_afsk(&tx);
    let got = receive_afsk(&audio, Il2pParity::Sixteen);
    assert_eq!(got, vec![(body(&frame), 0)]);
}

/// Coexistence: plain AX.25/HDLC audio still decodes through the
/// ordinary [`warble::tnc::TncReceiver`] with the `il2p` feature
/// compiled in (the feature adds a parallel codec; the default receive
/// paths are untouched).
#[cfg(all(feature = "tnc", feature = "alloc"))]
#[test]
fn plain_ax25_rx_unaffected_with_il2p_enabled() {
    use warble::aprs::{AprsPacket, Status};
    use warble::tnc::{DefaultTncReceiver, TncConfig, TncTransmitter};

    let config = TncConfig::bell_202(sample_rate()).unwrap();
    let tx = TncTransmitter::new(config);
    let samples = tx
        .transmit_to_vec_i16(
            &AprsPacket::Status(Status { text: b"coexist" }),
            addr(b"APRS", 0),
            addr(b"N0CALL", 7),
            &[],
        )
        .unwrap();
    let mut rx = DefaultTncReceiver::new(config).unwrap();
    let mut decoded = 0;
    for s in samples {
        if let Some(frame) = rx.push_i16(s) {
            assert_eq!(frame.info(), b">coexist");
            decoded += 1;
        }
    }
    assert_eq!(decoded, 1);
}

/// 9600-baud G3RUH-style baseband path: the IL2P bits ride the same
/// scrambled direct-baseband machinery as G3RUH packet (TX: NRZI →
/// scramble → baseband synthesis; RX: baseband demod → descramble →
/// NRZI decode → [`Il2pReceiver`]).
#[cfg(feature = "g3ruh")]
mod baseband_9600 {
    use super::*;
    use warble::baseband::{BasebandDemodulator, BasebandModulator};
    use warble::scrambler::{Descrambler, Scrambler};
    use warble::{BaudRate, Bit};

    fn baud() -> BaudRate {
        BaudRate::new(9_600).unwrap()
    }

    /// Modulates IL2P frame bytes into 9600-baud scrambled baseband
    /// audio (longer preamble: the baseband slicer needs level lock).
    fn modulate_baseband(bytes: &[u8]) -> Vec<i16> {
        BasebandModulator::new(sample_rate(), baud())
            .unwrap()
            .i16_samples(Scrambler::default().scramble_iter(il2p::tx_bits(
                bytes,
                4 * PREAMBLE_BYTES,
                TAIL_BYTES,
            )))
            .collect()
    }

    /// Runs baseband audio through demod → descramble →
    /// [`Il2pReceiver`].
    ///
    /// No NRZI stage: IL2P is not differentially encoded (spec v0.6,
    /// "Interface to Physical Layer"). The G3RUH scrambler here is the
    /// 9600-baud PHY's own, not part of IL2P.
    fn receive_baseband(audio: &[i16], parity: Il2pParity) -> Vec<(Vec<u8>, usize)> {
        let mut demod = BasebandDemodulator::new(sample_rate(), baud()).unwrap();
        let mut descrambler = Descrambler::default();
        let mut rx = Il2pReceiver::new(parity);
        let mut frames = Vec::new();
        for &s in audio {
            let Some(line) = demod.push_i16(s) else {
                continue;
            };
            let bit: Bit = descrambler.descramble(line);
            if let Some(Ok(rxf)) = rx.push(bit) {
                let corrected = rxf.corrected();
                let ui = rxf.ui_frame().unwrap();
                let mut buf = [0u8; 1200];
                let len = ui.build(&mut buf).unwrap();
                frames.push((buf[..len].to_vec(), corrected));
            }
        }
        frames
    }

    #[test]
    fn il2p_baseband_9600_round_trip() {
        let frame = UiFrame::new(addr(b"APRS", 0), addr(b"N0CALL", 7), b">il2p at 9600 baud");
        let tx = encoded(&frame, Il2pParity::Sixteen);
        let audio = modulate_baseband(&tx);
        let got = receive_baseband(&audio, Il2pParity::Sixteen);
        assert_eq!(got, vec![(body(&frame), 0)]);
    }

    #[test]
    fn il2p_baseband_9600_corrects_payload_errors() {
        let frame = UiFrame::new(
            addr(b"APRS", 0),
            addr(b"N0CALL", 7),
            b">baseband corruption recovery",
        );
        let parity = Il2pParity::Sixteen;
        let mut tx = encoded(&frame, parity);
        // The corruption is injected into the frame bytes ahead of the
        // G3RUH scrambler, so each flipped byte is exactly one symbol
        // error in its RS block — 2 here, well within t = 8.
        let payload_at = SYNC_LEN + HEADER_LEN + HEADER_PARITY_LEN;
        tx[payload_at] ^= 0x01;
        tx[payload_at + frame.info.len() / 2] ^= 0x01;
        let audio = modulate_baseband(&tx);
        let got = receive_baseband(&audio, parity);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, body(&frame));
        assert_eq!(got[0].1, 2);
    }
}
