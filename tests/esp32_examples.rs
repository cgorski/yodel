//! Host-side proof for the ESP32 RISC-V copy-paste examples.
//!
//! `examples/esp32-riscv/` is a detached `#![no_std]` sub-crate that
//! cross-compiles for riscv32imc/imac bare-metal targets (see
//! `scripts/check-embedded.sh`). To prove its DSP logic — not just its
//! compilability — this test includes the SAME source files via
//! `#[path]` and runs them on the host against the main crate's
//! transmit/receive chains:
//!
//! * the beacon module's samples must demodulate back to the expected
//!   APRS position frame;
//! * the demod module must decode transmitter-synthesized samples,
//!   including when they are fed in small odd-sized chunks (as from an
//!   ADC/I2S DMA buffer), proving chunk-boundary correctness;
//! * the digipeater module must relay a WIDE2-1 frame's audio with the
//!   documented H-bit mutation, suppress the duplicate within the dupe
//!   window, ignore a fully-used path, and relay again after the
//!   window expires (fake monotonic millis advanced by the test).
#![cfg(all(feature = "tnc", feature = "digipeat"))]

#[path = "../examples/esp32-riscv/src/beacon.rs"]
mod beacon;
#[path = "../examples/esp32-riscv/src/demod.rs"]
mod demod;
#[path = "../examples/esp32-riscv/src/digipeater.rs"]
mod digipeater;

use yodel::SampleRate;
use yodel::aprs::{AprsPacket, Latitude, Longitude, Position, Status, Symbol};
use yodel::ax25::{Address, PathHop, UiFrame};
use yodel::digipeat::WideLimit;
use yodel::tnc::{DefaultTncReceiver, TncConfig, TncReceiver, TncTransmitter};

/// 49.0583° N in signed 1/100 arc-minutes (degrees × 6000).
/// In 1/100 arc-minutes, the unit the ESP32 example's API takes.
const LAT_HUNDREDTHS: i64 = 294_350;
const LAT: i64 = LAT_HUNDREDTHS * yodel::geo::UNITS_PER_HUNDREDTH_MINUTE;
/// 72.0292° W in signed 1/100 arc-minutes.
const LON_HUNDREDTHS: i64 = -432_175;
const LON: i64 = LON_HUNDREDTHS * yodel::geo::UNITS_PER_HUNDREDTH_MINUTE;

fn addr(call: &[u8], ssid: u8) -> Address {
    match Address::new(call, ssid) {
        Ok(a) => a,
        Err(e) => panic!("{e}"),
    }
}

/// (a) The beacon module fills a buffer that the MAIN crate's receive
/// chain decodes back, and the APRS payload matches the typed input.
#[test]
fn beacon_round_trips_through_main_crate_receiver() {
    let mut pcm = vec![0i16; beacon::MAX_BEACON_SAMPLES];
    let n = beacon::fill_position_beacon(addr(b"N0CALL", 9), LAT, LON, b"yodel esp32", &mut pcm)
        .expect("beacon must render");
    assert!(n > 0, "beacon must produce samples");
    assert_eq!(
        n % beacon::SAMPLES_PER_BIT,
        0,
        "48 kHz / 1200 Bd is an integer samples-per-bit ratio"
    );
    assert!(n <= beacon::MAX_BEACON_SAMPLES);

    let cfg = TncConfig::bell_202(SampleRate::new(beacon::SAMPLE_RATE_HZ).unwrap()).unwrap();
    let mut rx: DefaultTncReceiver = TncReceiver::new(cfg).unwrap();
    let mut decoded = 0;
    for &s in &pcm[..n] {
        if let Some(frame) = rx.push_i16(s) {
            assert_eq!(frame.src().callsign.as_bytes(), b"N0CALL");
            assert_eq!(frame.src().ssid.value(), 9);
            assert_eq!(frame.dest().callsign.as_bytes(), b"APRS");
            let expected = AprsPacket::Position(
                Position::new(
                    Latitude::new(LAT).unwrap(),
                    Longitude::new(LON).unwrap(),
                    Symbol::CAR,
                )
                .with_comment(b"yodel esp32"),
            );
            assert_eq!(frame.aprs().expect("payload must parse"), expected);
            decoded += 1;
        }
    }
    assert_eq!(decoded, 1, "exactly one frame must decode");
}

/// The beacon rejects a too-small output buffer with a typed error
/// instead of truncating silently.
#[test]
fn beacon_reports_buffer_too_small() {
    let mut tiny = [0i16; 16];
    assert_eq!(
        beacon::fill_position_beacon(addr(b"N0CALL", 9), LAT, LON, b"", &mut tiny),
        Err(beacon::BeaconError::BufferTooSmall)
    );
}

/// Renders one status transmission with the main crate's transmitter.
fn synthesize_status(text: &'static [u8]) -> Vec<i16> {
    let cfg = TncConfig::bell_202(SampleRate::new(demod::SAMPLE_RATE_HZ).unwrap()).unwrap();
    let tx = TncTransmitter::new(cfg);
    let mut info_buf = [0u8; 330];
    let mut frame_buf = [0u8; 330];
    tx.transmit_i16(
        &AprsPacket::Status(Status { text }),
        addr(b"APRS", 0),
        addr(b"N0CALL", 7),
        &[],
        &mut info_buf,
        &mut frame_buf,
    )
    .expect("transmit must succeed")
    .collect()
}

/// (b) The demod module decodes samples synthesized by the main crate's
/// transmitter when fed as one contiguous chunk.
#[test]
fn demod_module_decodes_synthesized_samples() {
    let samples = synthesize_status(b"QRV from esp32 example");
    let mut decoder = demod::AprsDecoder::new().expect("valid fixed config");
    let mut seen = 0;
    let frames = decoder.feed(&samples, |frame| {
        assert_eq!(frame.src().callsign.as_bytes(), b"N0CALL");
        assert_eq!(
            demod::parse_aprs(frame).expect("payload must parse"),
            AprsPacket::Status(Status {
                text: b"QRV from esp32 example",
            })
        );
        seen += 1;
    });
    assert_eq!(frames, 1);
    assert_eq!(seen, 1);
    assert_eq!(decoder.stats().frames_ok, 1);
}

/// (b, continued) Feeding the same samples in small odd-sized chunks —
/// like successive ADC/I2S DMA buffers — decodes identically, proving
/// the decoder keeps its state across chunk boundaries.
#[test]
fn demod_module_survives_dma_like_odd_chunks() {
    let samples = synthesize_status(b"chunked");
    // Awkward chunk sizes: prime, tiny, and non-divisors of both the
    // samples-per-bit ratio (40) and the total length.
    for chunk_len in [1usize, 7, 31, 173, 997] {
        let mut decoder = demod::AprsDecoder::new().expect("valid fixed config");
        let mut total = 0;
        for chunk in samples.chunks(chunk_len) {
            total += decoder.feed(chunk, |frame| {
                assert_eq!(frame.info(), b">chunked");
            });
        }
        assert_eq!(total, 1, "chunk size {chunk_len} must decode one frame");
        assert_eq!(decoder.stats().frames_ok, 1);
    }
}

// ====================================================================
// Digipeater module: end-to-end audio proof
// ====================================================================

/// Renders one UI frame with an explicit hop list (H bits included)
/// into Bell 202 samples using the main crate's transmitter.
fn synthesize_with_hops(src: Address, hops: &[PathHop], info: &[u8]) -> Vec<i16> {
    let cfg = TncConfig::bell_202(SampleRate::new(digipeater::SAMPLE_RATE_HZ).unwrap()).unwrap();
    let tx = TncTransmitter::new(cfg);
    let frame = UiFrame::with_hops(addr(b"APRS", 0), src, hops, info).expect("legal path");
    let mut frame_buf = [0u8; 330];
    let len = frame.build(&mut frame_buf).expect("frame must build");
    tx.frame_samples_i16(&frame_buf[..len]).collect()
}

/// Feeds RX audio to the digipeater and collects any relayed audio.
fn feed_digi(digi: &mut digipeater::Digipeater, rx: &[i16], now_ms: u64) -> Vec<Vec<i16>> {
    let mut tx_buf = vec![0i16; digipeater::MAX_RELAY_SAMPLES];
    let mut out: Vec<Vec<i16>> = Vec::new();
    // DMA-like chunks: the digipeater must keep state across them.
    for chunk in rx.chunks(512) {
        digi.feed(chunk, now_ms, &mut tx_buf, |samples| {
            out.push(samples.to_vec());
        });
    }
    out
}

/// Decodes exactly one frame from audio, returning (src, dest, hops, info).
fn decode_one(samples: &[i16]) -> (Address, Address, Vec<PathHop>, Vec<u8>) {
    let cfg = TncConfig::bell_202(SampleRate::new(digipeater::SAMPLE_RATE_HZ).unwrap()).unwrap();
    let mut rx: DefaultTncReceiver = TncReceiver::new(cfg).unwrap();
    let mut decoded = None;
    for &s in samples {
        if let Some(frame) = rx.push_i16(s) {
            assert!(decoded.is_none(), "exactly one frame expected");
            decoded = Some((
                frame.src(),
                frame.dest(),
                frame.ui_frame().hops().collect(),
                frame.info().to_vec(),
            ));
        }
    }
    decoded.expect("relayed audio must decode")
}

fn make_digi() -> digipeater::Digipeater {
    digipeater::Digipeater::new(addr(b"N0CALL", 1), WideLimit::TWO).expect("valid fixed config")
}

/// (a) A WIDE2-1 frame's audio in → exactly one relayed audio output
/// whose decoded frame carries the documented mutation: the WIDE2-1
/// hop consumed in place (H bit set, SSID unchanged — the last
/// requested hop is served without callsign insertion).
#[test]
fn digipeater_relays_wide2_1_with_h_bit_set() {
    let heard = [PathHop::unused(addr(b"WIDE2", 1))];
    let rx = synthesize_with_hops(addr(b"K1ABC", 9), &heard, b">digi me");

    let mut digi = make_digi();
    let relays = feed_digi(&mut digi, &rx, 1_000);
    assert_eq!(relays.len(), 1, "exactly one relayed transmission");

    let (src, dest, hops, info) = decode_one(&relays[0]);
    assert_eq!(src, addr(b"K1ABC", 9), "source is preserved");
    assert_eq!(dest, addr(b"APRS", 0), "destination is preserved");
    assert_eq!(info, b">digi me", "payload is preserved");
    assert_eq!(
        hops,
        vec![PathHop {
            address: addr(b"WIDE2", 1),
            repeated: true,
        }],
        "WIDE2-1 must be consumed in place: H bit set, no insertion"
    );
    assert_eq!(digi.stats().relayed, 1);
    assert_eq!(digi.rx_stats().frames_ok, 1, "decoder saw one RX frame");
}

/// (a, continued) A WIDE2-2 request exercises the other documented
/// mutation: our callsign inserted used, the WIDE SSID decremented.
#[test]
fn digipeater_decrements_wide2_2_and_inserts_itself() {
    let heard = [PathHop::unused(addr(b"WIDE2", 2))];
    let rx = synthesize_with_hops(addr(b"K1ABC", 9), &heard, b">two hops");

    let mut digi = make_digi();
    let relays = feed_digi(&mut digi, &rx, 1_000);
    assert_eq!(relays.len(), 1);

    let (_, _, hops, _) = decode_one(&relays[0]);
    assert_eq!(
        hops,
        vec![
            PathHop {
                address: addr(b"N0CALL", 1),
                repeated: true,
            },
            PathHop::unused(addr(b"WIDE2", 1)),
        ],
        "WIDE2-2 must become N0CALL-1*,WIDE2-1"
    );
}

/// (b) The SAME frame heard again within the dupe window → no relay.
/// (d) After the window expires (fake millis advanced) → relays again.
#[test]
fn digipeater_suppresses_dupes_until_window_expires() {
    let heard = [PathHop::unused(addr(b"WIDE2", 1))];
    let rx = synthesize_with_hops(addr(b"K1ABC", 9), &heard, b">once");

    let mut digi = make_digi();
    assert_eq!(feed_digi(&mut digi, &rx, 1_000).len(), 1, "first hearing");
    // 5 s later, well inside the 30 s default window: suppressed.
    assert_eq!(feed_digi(&mut digi, &rx, 6_000).len(), 0, "dupe");
    assert_eq!(digi.stats().dupes, 1);
    // One window past the FIRST hearing: fresh again.
    assert_eq!(feed_digi(&mut digi, &rx, 31_001).len(), 1, "window expired");
    assert_eq!(digi.stats().relayed, 2);
}

/// (c) A frame whose path is fully used (every H bit set) → no relay:
/// structural loop prevention from the library core.
#[test]
fn digipeater_never_relays_a_fully_used_path() {
    let heard = [
        PathHop {
            address: addr(b"N0CALL", 1),
            repeated: true,
        },
        PathHop {
            address: addr(b"WIDE2", 1),
            repeated: true,
        },
    ];
    let rx = synthesize_with_hops(addr(b"K1ABC", 9), &heard, b">spent");

    let mut digi = make_digi();
    assert_eq!(feed_digi(&mut digi, &rx, 1_000).len(), 0);
    assert_eq!(digi.stats().heard, 1, "the frame WAS decoded");
    assert_eq!(digi.stats().ignored, 1, "... and ignored, not relayed");
    assert_eq!(digi.stats().relayed, 0);
}

/// A frame addressed to someone else's path is left alone, too.
#[test]
fn digipeater_ignores_paths_not_for_us() {
    let heard = [PathHop::unused(addr(b"K9XYZ", 3))];
    let rx = synthesize_with_hops(addr(b"K1ABC", 9), &heard, b">not you");

    let mut digi = make_digi();
    assert_eq!(feed_digi(&mut digi, &rx, 1_000).len(), 0);
    assert_eq!(digi.stats().ignored, 1);
}
