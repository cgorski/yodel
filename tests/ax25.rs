//! Integration tests for the `ax25` feature: UI frame build/parse
//! round-trips, corrupted-FCS and oversize rejection, and (with the `mod`
//! and `demod` features) the full pipeline through real AFSK modulation
//! and demodulation on both sample paths and multiple sample rates.
#![cfg(feature = "ax25")]

use yodel::Bit;
use yodel::ax25::{Address, Ax25Error, HdlcDeframer, UiFrame, crc16_x25, hdlc};

fn addr(call: &[u8], ssid: u8) -> Address {
    Address::new(call, ssid).unwrap()
}

fn balloon_frame(info: &[u8]) -> UiFrame<'_> {
    UiFrame::with_path(
        addr(b"APRS", 0),
        addr(b"N0CALL", 11),
        &[addr(b"WIDE1", 1), addr(b"WIDE2", 1)],
        info,
    )
    .unwrap()
}

#[test]
fn crc_check_value() {
    assert_eq!(crc16_x25(b"123456789"), 0x906E);
}

#[test]
fn frame_build_parse_round_trip() {
    let info = b"!4903.50N/07201.75W-Test balloon 001";
    let frame = balloon_frame(info);
    let mut buf = [0u8; 330];
    let len = frame.build(&mut buf).unwrap();
    let parsed = UiFrame::parse(&buf[..len]).unwrap();
    assert_eq!(parsed.dest, frame.dest);
    assert_eq!(parsed.src, frame.src);
    assert_eq!(parsed.path(), frame.path());
    assert_eq!(parsed.info, info);
}

#[test]
fn corrupted_fcs_is_rejected() {
    let frame = balloon_frame(b"corruption test");
    let mut buf = [0u8; 330];
    let len = frame.build(&mut buf).unwrap();

    let mut bits: Vec<Bit> = hdlc::frame_bits(&buf[..len], 4, 2).collect();
    // Flip a payload bit well inside the data section.
    let idx = 4 * 8 + 20;
    bits[idx] = match bits[idx] {
        Bit::Zero => Bit::One,
        Bit::One => Bit::Zero,
    };
    let mut deframer = HdlcDeframer::<330>::new();
    let mut errors = Vec::new();
    let mut frames = 0;
    for b in bits {
        match deframer.push(b) {
            Some(Ok(_)) => frames += 1,
            Some(Err(e)) => errors.push(e),
            None => {}
        }
    }
    assert_eq!(frames, 0);
    assert_eq!(errors.len(), 1);
    assert!(matches!(errors[0], Ax25Error::FcsMismatch { .. }));
}

#[test]
fn oversize_frame_is_rejected() {
    // Build succeeds into a big buffer, but the receive buffer is smaller.
    let info = [0x55u8; 64];
    let frame = balloon_frame(&info);
    let mut buf = [0u8; 330];
    let len = frame.build(&mut buf).unwrap();

    let mut deframer = HdlcDeframer::<32>::new();
    let mut got = Vec::new();
    for b in hdlc::frame_bits(&buf[..len], 4, 2) {
        if let Some(r) = deframer.push(b) {
            got.push(r.map(<[u8]>::to_vec));
        }
    }
    assert_eq!(got.len(), 1);
    assert!(matches!(
        got[0],
        Err(Ax25Error::FrameTooLarge { max: 32, .. })
    ));

    // Build itself also rejects a too-small caller buffer.
    let mut small = [0u8; 16];
    assert!(matches!(
        frame.build(&mut small),
        Err(Ax25Error::FrameTooLarge { .. })
    ));
}

#[test]
fn hdlc_round_trip_without_dsp() {
    let frame = balloon_frame(b"pure bit layer");
    let mut buf = [0u8; 330];
    let len = frame.build(&mut buf).unwrap();
    let mut deframer = HdlcDeframer::<330>::new();
    let mut recovered = Vec::new();
    for line in yodel::nrzi::encode_iter(hdlc::frame_bits(&buf[..len], 8, 2)) {
        // NRZI decode is folded into the receiver in the DSP tests; here
        // exercise the layers separately.
        recovered.push(line);
    }
    let mut dec = yodel::NrziDecoder::default();
    let mut frames = Vec::new();
    for line in recovered {
        if let Some(Ok(f)) = deframer.push(dec.decode(line)) {
            frames.push(f.to_vec());
        }
    }
    assert_eq!(frames, [buf[..len].to_vec()]);
}

#[cfg(all(feature = "mod", feature = "demod"))]
mod pipeline {
    use super::*;
    use yodel::SampleRate;
    use yodel::ax25::{FrameReceiver, tx_f32, tx_i16};
    use yodel::demodulator::DemodulatorConfig;
    use yodel::modulator::{Modulator, ModulatorConfig};

    const RATES: [u32; 3] = [22_050, 44_100, 48_000];
    const BUF: usize = 330;

    fn modulator(sr_hz: u32) -> Modulator {
        let sr = SampleRate::new(sr_hz).unwrap();
        Modulator::new(ModulatorConfig::bell_202(sr).unwrap())
    }

    fn receiver(sr_hz: u32) -> FrameReceiver<BUF> {
        let sr = SampleRate::new(sr_hz).unwrap();
        let demod = yodel::AfskDemodulator::new(DemodulatorConfig::bell_202(sr).unwrap()).unwrap();
        FrameReceiver::new(demod)
    }

    fn built_frame(buf: &mut [u8; BUF]) -> usize {
        let frame = balloon_frame(b"!4903.50N/07201.75W-Full pipeline");
        frame.build(buf).unwrap()
    }

    fn assert_recovered(frames: &[Vec<u8>], expected: &[u8], ctx: &str) {
        assert!(
            frames.iter().any(|f| f == expected),
            "{ctx}: frame not recovered ({} candidates)",
            frames.len()
        );
        let parsed = UiFrame::parse(expected).unwrap();
        assert_eq!(parsed.info, b"!4903.50N/07201.75W-Full pipeline");
        assert_eq!(parsed.src, addr(b"N0CALL", 11));
        assert_eq!(parsed.dest, addr(b"APRS", 0));
    }

    #[test]
    fn full_pipeline_i16_all_rates() {
        for sr in RATES {
            let mut buf = [0u8; BUF];
            let len = built_frame(&mut buf);
            let mut rx = receiver(sr);
            let mut frames = Vec::new();
            for s in tx_i16(&buf[..len], modulator(sr)) {
                if let Some(Ok(f)) = rx.push_sample_i16(s) {
                    frames.push(f.to_vec());
                }
            }
            assert_recovered(&frames, &buf[..len], &format!("i16 @ {sr}"));
        }
    }

    #[test]
    fn full_pipeline_f32_all_rates() {
        for sr in RATES {
            let mut buf = [0u8; BUF];
            let len = built_frame(&mut buf);
            let mut rx = receiver(sr);
            let mut frames = Vec::new();
            for s in tx_f32(&buf[..len], modulator(sr)) {
                if let Some(Ok(f)) = rx.push_sample_f32(s) {
                    frames.push(f.to_vec());
                }
            }
            assert_recovered(&frames, &buf[..len], &format!("f32 @ {sr}"));
        }
    }

    #[test]
    fn full_pipeline_stuffing_heavy_info() {
        // An info field full of 0xFF and 0x7E stresses stuffing through
        // the whole DSP chain.
        let info = [0xFFu8, 0x7E, 0xFF, 0x7E, 0xFF, 0xFF, 0xFF, 0x7E];
        let frame = UiFrame::new(addr(b"APRS", 0), addr(b"K1ABC", 15), &info);
        let mut buf = [0u8; BUF];
        let len = frame.build(&mut buf).unwrap();
        let sr = 44_100;
        let mut rx = receiver(sr);
        let mut frames = Vec::new();
        for s in tx_i16(&buf[..len], modulator(sr)) {
            if let Some(Ok(f)) = rx.push_sample_i16(s) {
                frames.push(f.to_vec());
            }
        }
        assert_eq!(frames, [buf[..len].to_vec()]);
        let parsed = UiFrame::parse(&frames[0]).unwrap();
        assert_eq!(parsed.info, info);
    }
}
