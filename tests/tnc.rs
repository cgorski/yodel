//! Full-loop TNC pipeline tests: `AprsPacket` → PCM samples →
//! `AprsPacket`, for every packet kind, on both PCM paths, at two
//! sample rates.
#![cfg(feature = "tnc")]

use yodel::aprs::{
    Addressee, AprsPacket, Item, Latitude, Longitude, Message, MessageContent, Object, Position,
    PositionlessWeather, Status, Symbol, Telemetry, Timestamp, WeatherReport,
};
use yodel::ax25::Address;
use yodel::geo::Ambiguity;
use yodel::tnc::{DefaultTncReceiver, TncConfig, TncReceiver, TncTransmitter};
use yodel::units::{Humidity, Pressure, Rainfall, Speed, Temperature};
use yodel::{ModemProfile, SampleRate, TonePair};

const RATES: [u32; 2] = [11_025, 44_100];

fn config(sr_hz: u32) -> TncConfig {
    let sr = SampleRate::new(sr_hz).unwrap();
    TncConfig::bell_202(sr).unwrap()
}

fn addr(callsign: &[u8], ssid: u8) -> Address {
    Address::new(callsign, ssid).unwrap()
}

/// From 1/100 arc-minutes, the unit these fixtures are written in.
fn lat(hundredths: i64) -> Latitude {
    Latitude::new(hundredths * yodel::geo::UNITS_PER_HUNDREDTH_MINUTE).unwrap()
}

fn lon(hundredths: i64) -> Longitude {
    Longitude::new(hundredths * yodel::geo::UNITS_PER_HUNDREDTH_MINUTE).unwrap()
}

fn position_uncompressed() -> AprsPacket<'static> {
    AprsPacket::Position(Position {
        ambiguity: Ambiguity::EXACT,
        latitude: lat(49 * 6000 + 350),
        longitude: lon(-(72 * 6000 + 175)),
        symbol: Symbol::HOUSE,
        messaging: false,
        compressed: false,
        extension: None,
        comment: b"tnc loop",
    })
}

fn position_compressed() -> AprsPacket<'static> {
    // Whole degrees, which sit exactly on the base-91 grid: 90 - (-1)
    // is 91 whole latitude steps and 180 + 1 is 181 whole longitude
    // steps. This test is about the modem loop, so the coordinate is
    // chosen not to drag compressed quantisation into it; that is
    // `tests/compressed.rs`'s subject.
    AprsPacket::Position(Position {
        ambiguity: Ambiguity::EXACT,
        latitude: Latitude::new(-yodel::geo::UNITS_PER_DEGREE).unwrap(),
        longitude: Longitude::new(yodel::geo::UNITS_PER_DEGREE).unwrap(),
        symbol: Symbol::from_wire(b'\\', b'O'),
        messaging: true,
        compressed: true,
        extension: None,
        comment: b"cmp",
    })
}

fn status() -> AprsPacket<'static> {
    AprsPacket::Status(Status {
        text: b"TNC status check",
    })
}

fn message_with_ack_id() -> AprsPacket<'static> {
    AprsPacket::Message(Message {
        addressee: Addressee::new(b"N1CALL").unwrap(),
        content: MessageContent::Text {
            text: b"hello via tnc",
            id: Some(b"42"),
        },
    })
}

fn weather_positionless() -> AprsPacket<'static> {
    AprsPacket::Weather(PositionlessWeather {
        month: 6,
        day: 15,
        hour: 12,
        minute: 30,
        weather: WeatherReport {
            wind_direction: Some(220),
            // A positionless report spells wind speed in mph.
            wind_speed: Some(Speed::from_mph(4)),
            gust: Some(Speed::from_mph(5)),
            temperature: Some(Temperature::from_fahrenheit(77)),
            rain_1h: Some(Rainfall::from_hundredths_inch(0)),
            rain_24h: Some(Rainfall::from_hundredths_inch(0)),
            rain_midnight: Some(Rainfall::from_hundredths_inch(0)),
            humidity: Some(Humidity::new(50).expect("in range")),
            barometric_pressure: Some(Pressure::from_tenths_hpa(9900)),
            // Chapter 12's optional "other parameters" are written only
            // when present, so this report is the nine standard fields
            // and nothing else.
            luminosity: None,
            snowfall: None,
        },
        rest: b"",
    })
}

fn telemetry() -> AprsPacket<'static> {
    AprsPacket::Telemetry(Telemetry {
        seq: 5,
        analog: Telemetry::integer_channels([199, 0, 255, 73, 123]),
        digital: Some([false, true, true, false, true, false, false, true]),
        rest: b"",
    })
}

fn object() -> AprsPacket<'static> {
    AprsPacket::Object(Object {
        ambiguity: Ambiguity::EXACT,
        name: b"LEADER",
        live: true,
        timestamp: Timestamp::DhmZulu {
            day: 9,
            hour: 23,
            minute: 45,
        },
        latitude: lat(49 * 6000 + 350),
        longitude: lon(-(72 * 6000 + 175)),
        symbol: Symbol::CAR,
        compressed: false,
        comment: b"088/036",
    })
}

fn item() -> AprsPacket<'static> {
    AprsPacket::Item(Item {
        ambiguity: Ambiguity::EXACT,
        name: b"AID#2",
        live: true,
        latitude: lat(6000),
        longitude: lon(-6000),
        symbol: Symbol::from_wire(b'/', b'8'),
        compressed: false,
        comment: b"first aid",
    })
}

fn all_packets() -> [AprsPacket<'static>; 8] {
    [
        position_uncompressed(),
        position_compressed(),
        status(),
        message_with_ack_id(),
        weather_positionless(),
        telemetry(),
        object(),
        item(),
    ]
}

const DEST: &[u8] = b"APRS";
const SRC: &[u8] = b"N0CALL";

/// Transmits `packet` and decodes it back over the i16 path.
fn loop_i16(packet: &AprsPacket<'_>, sr_hz: u32) {
    let cfg = config(sr_hz);
    let tx = TncTransmitter::new(cfg);
    let mut rx: DefaultTncReceiver = TncReceiver::new(cfg).unwrap();
    let path = [addr(b"WIDE1", 1)];
    let mut info_buf = [0u8; 330];
    let mut frame_buf = [0u8; 330];
    let samples = tx
        .transmit_i16(
            packet,
            addr(DEST, 0),
            addr(SRC, 7),
            &path,
            &mut info_buf,
            &mut frame_buf,
        )
        .unwrap();
    let mut decoded = 0u32;
    for s in samples {
        if let Some(frame) = rx.push_i16(s) {
            assert_eq!(frame.src(), addr(SRC, 7));
            assert_eq!(frame.dest(), addr(DEST, 0));
            assert_eq!(frame.path(), &[addr(b"WIDE1", 1)]);
            assert_eq!(frame.aprs().unwrap(), *packet);
            decoded += 1;
        }
    }
    assert_eq!(decoded, 1, "i16 @ {sr_hz}: expected exactly one frame");
    assert_eq!(rx.stats().frames_ok, 1);
    assert_eq!(rx.stats().fcs_errors, 0);
    assert_eq!(rx.stats().oversize, 0);
}

/// The `f32` twin of [`loop_i16`].
fn loop_f32(packet: &AprsPacket<'_>, sr_hz: u32) {
    let cfg = config(sr_hz);
    let tx = TncTransmitter::new(cfg);
    let mut rx: DefaultTncReceiver = TncReceiver::new(cfg).unwrap();
    let mut info_buf = [0u8; 330];
    let mut frame_buf = [0u8; 330];
    let samples = tx
        .transmit_f32(
            packet,
            addr(DEST, 0),
            addr(SRC, 7),
            &[],
            &mut info_buf,
            &mut frame_buf,
        )
        .unwrap();
    let mut decoded = 0u32;
    for s in samples {
        if let Some(frame) = rx.push_f32(s) {
            assert_eq!(frame.src(), addr(SRC, 7));
            assert_eq!(frame.aprs().unwrap(), *packet);
            decoded += 1;
        }
    }
    assert_eq!(decoded, 1, "f32 @ {sr_hz}: expected exactly one frame");
    assert_eq!(rx.stats().frames_ok, 1);
}

macro_rules! loop_tests {
    ($($name:ident: $builder:ident;)*) => {$(
        #[test]
        fn $name() {
            for sr in RATES {
                let packet = $builder();
                loop_i16(&packet, sr);
                loop_f32(&packet, sr);
            }
        }
    )*};
}

loop_tests! {
    loop_position_uncompressed: position_uncompressed;
    loop_position_compressed: position_compressed;
    loop_status: status;
    loop_message_with_ack_id: message_with_ack_id;
    loop_weather_positionless: weather_positionless;
    loop_telemetry: telemetry;
    loop_object: object;
    loop_item: item;
}

/// Every packet kind, transmitted back-to-back into one receiver, all
/// recovered in order.
#[test]
fn multi_frame_back_to_back_decode() {
    for sr in RATES {
        let cfg = config(sr);
        let tx = TncTransmitter::new(cfg);
        let mut rx: DefaultTncReceiver = TncReceiver::new(cfg).unwrap();
        let packets = all_packets();
        let mut decoded = 0usize;
        for packet in &packets {
            let mut info_buf = [0u8; 330];
            let mut frame_buf = [0u8; 330];
            let samples = tx
                .transmit_i16(
                    packet,
                    addr(DEST, 0),
                    addr(SRC, 7),
                    &[],
                    &mut info_buf,
                    &mut frame_buf,
                )
                .unwrap();
            for s in samples {
                if let Some(frame) = rx.push_i16(s) {
                    assert_eq!(frame.aprs().unwrap(), packets[decoded], "@ {sr}");
                    decoded += 1;
                }
            }
        }
        assert_eq!(decoded, packets.len(), "@ {sr}");
        assert_eq!(rx.stats().frames_ok, packets.len() as u32);
        assert_eq!(rx.stats().fcs_errors, 0);
        assert_eq!(rx.stats().oversize, 0);
        assert_eq!(rx.stats().malformed, 0);
    }
}

/// A noise-free but truncated sample stream (cut mid-frame) must yield
/// no frame at all — and no error counters either: without a closing
/// flag the deframer never closes the frame.
#[test]
fn truncated_stream_yields_no_false_frame() {
    for sr in RATES {
        let cfg = config(sr);
        let tx = TncTransmitter::new(cfg);
        let mut rx: DefaultTncReceiver = TncReceiver::new(cfg).unwrap();
        let packet = status();
        let mut info_buf = [0u8; 330];
        let mut frame_buf = [0u8; 330];
        let samples: Vec<i16> = tx
            .transmit_i16(
                &packet,
                addr(DEST, 0),
                addr(SRC, 7),
                &[],
                &mut info_buf,
                &mut frame_buf,
            )
            .unwrap()
            .collect();
        // Cut the stream at 60% — well inside the frame body, past the
        // preamble but before the closing flag.
        let cut = samples.len() * 6 / 10;
        for &s in &samples[..cut] {
            assert!(rx.push_i16(s).is_none(), "@ {sr}: false frame");
        }
        assert_eq!(rx.stats().frames_ok, 0, "@ {sr}");
        assert_eq!(rx.stats().fcs_errors, 0, "@ {sr}");
        assert_eq!(rx.stats().oversize, 0, "@ {sr}");
        assert_eq!(rx.stats().malformed, 0, "@ {sr}");
    }
}

/// A properly flag-delimited frame carrying a wrong FCS is counted as
/// an FCS error, not surfaced as a frame. The bit stream is crafted
/// manually (flags, LSB-first bytes, zero stuffing) so the bad FCS
/// reaches the deframer intact.
#[test]
fn bad_fcs_counts_fcs_error() {
    use yodel::ax25::crc16_x25;
    use yodel::modulator::{Modulator, ModulatorConfig};
    use yodel::{Bit, SampleRate as Sr, nrzi};

    let sr = Sr::new(44_100).unwrap();
    let cfg = config(44_100);
    let mut rx: DefaultTncReceiver = TncReceiver::new(cfg).unwrap();

    // A valid UI frame body with a wrong FCS.
    let tx = TncTransmitter::new(cfg);
    let mut frame_buf = [0u8; 330];
    let len = tx
        .build_frame_raw(addr(DEST, 0), addr(SRC, 7), &[], b"bad fcs", &mut frame_buf)
        .unwrap();
    let wrong_fcs = crc16_x25(&frame_buf[..len]) ^ 0x0001;

    // Manual HDLC serialization: preamble flags, stuffed bits, tail flag.
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
    let bytes = frame_buf[..len]
        .iter()
        .copied()
        .chain(wrong_fcs.to_le_bytes());
    for byte in bytes {
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

    let modulator = Modulator::new(ModulatorConfig::bell_202(sr).unwrap());
    let mut frames = 0u32;
    for s in modulator.i16_samples(nrzi::encode_iter(bits.into_iter())) {
        if rx.push_i16(s).is_some() {
            frames += 1;
        }
    }
    assert_eq!(frames, 0, "bad-FCS frame must not decode");
    assert_eq!(rx.stats().frames_ok, 0);
    assert_eq!(rx.stats().fcs_errors, 1, "{:?}", rx.stats());
}

/// An FCS-valid transmission whose contents are not a UI frame is
/// counted as malformed, not surfaced.
#[test]
fn garbage_frame_counts_malformed() {
    let cfg = config(44_100);
    let tx = TncTransmitter::new(cfg);
    let mut rx: DefaultTncReceiver = TncReceiver::new(cfg).unwrap();
    // 20 bytes that decode to no valid AX.25 address field; frame_bits
    // appends a correct FCS, so only UiFrame::parse rejects it.
    let garbage = [0xAAu8; 20];
    let mut frames = 0u32;
    for s in tx.frame_samples_i16(&garbage) {
        if rx.push_i16(s).is_some() {
            frames += 1;
        }
    }
    assert_eq!(frames, 0, "garbage must not decode");
    assert_eq!(rx.stats().frames_ok, 0);
    assert_eq!(rx.stats().malformed, 1, "{:?}", rx.stats());
}

/// Mic-E loop: the destination callsign carries half the position, so
/// the frame is built with the encoded Mic-E destination and decoded
/// via `RxFrame::mic_e`.
#[cfg(feature = "micE")]
#[test]
fn mic_e_loop() {
    use yodel::aprs::{MicE, MicEFix, MicEMessage};

    let report = MicE {
        latitude: lat(33 * 6000 + 2564),
        longitude: lon(-(112 * 6000 + 700)),
        speed: 20,
        course: 251,
        symbol: Symbol::from_wire(b'/', b'j'),
        message: MicEMessage::InService,
        fix: MicEFix::Current,
        altitude: Some(61),
        device_prefix: None,
        ambiguity: 0,
        status: b"tnc",
    };
    let mut dest_text = [0u8; 6];
    let mut info = [0u8; 64];
    let info_len = report.encode(&mut dest_text, &mut info).unwrap();

    for sr in RATES {
        let cfg = config(sr);
        let tx = TncTransmitter::new(cfg);
        let mut rx: DefaultTncReceiver = TncReceiver::new(cfg).unwrap();
        let dest = addr(&dest_text, 0);
        let mut frame_buf = [0u8; 330];
        let len = tx
            .build_frame_raw(dest, addr(SRC, 9), &[], &info[..info_len], &mut frame_buf)
            .unwrap();
        let mut decoded = 0u32;
        for s in tx.frame_samples_i16(&frame_buf[..len]) {
            if let Some(frame) = rx.push_i16(s) {
                let got = frame.mic_e().unwrap();
                assert_eq!(got, report, "@ {sr}");
                decoded += 1;
            }
        }
        assert_eq!(decoded, 1, "@ {sr}");
    }
}

/// The alloc conveniences collect the same samples the lazy iterators
/// produce.
#[cfg(feature = "alloc")]
#[test]
fn alloc_render_matches_lazy_iterators() {
    let cfg = config(44_100);
    let tx = TncTransmitter::new(cfg);
    let packet = status();
    let dest = addr(DEST, 0);
    let src = addr(SRC, 7);

    let mut info_buf = [0u8; 330];
    let mut frame_buf = [0u8; 330];
    let lazy_i16: Vec<i16> = tx
        .transmit_i16(&packet, dest, src, &[], &mut info_buf, &mut frame_buf)
        .unwrap()
        .collect();
    assert_eq!(
        tx.transmit_to_vec_i16(&packet, dest, src, &[]).unwrap(),
        lazy_i16
    );

    let mut info_buf = [0u8; 330];
    let mut frame_buf = [0u8; 330];
    let lazy_f32: Vec<f32> = tx
        .transmit_f32(&packet, dest, src, &[], &mut info_buf, &mut frame_buf)
        .unwrap()
        .collect();
    assert_eq!(
        tx.transmit_to_vec_f32(&packet, dest, src, &[]).unwrap(),
        lazy_f32
    );
}

/// The raw-info entry carries arbitrary (non-APRS) payload bytes.
#[test]
fn raw_info_frame_round_trips() {
    let sr = 44_100;
    let cfg = config(sr);
    let tx = TncTransmitter::new(cfg);
    let mut rx: DefaultTncReceiver = TncReceiver::new(cfg).unwrap();
    let info = b"raw bytes, not APRS";
    let mut frame_buf = [0u8; 330];
    let len = tx
        .build_frame_raw(addr(DEST, 0), addr(SRC, 7), &[], info, &mut frame_buf)
        .unwrap();
    let mut decoded = 0u32;
    for s in tx.frame_samples_i16(&frame_buf[..len]) {
        if let Some(frame) = rx.push_i16(s) {
            assert_eq!(frame.info(), info);
            decoded += 1;
        }
    }
    assert_eq!(decoded, 1);
}

// ---------------------------------------------------------------------
// Named modem presets: validation and full 300-baud loopback.
// ---------------------------------------------------------------------

/// Transmits one packet with `profile` at `rate_hz` and decodes it
/// back through a receiver built from the same profile, on the chosen
/// sample path.
fn preset_loopback(profile: ModemProfile, sr_hz: u32, use_f32: bool) {
    let rate = SampleRate::new(sr_hz).unwrap();
    let cfg = TncConfig::from_profile(rate, profile).unwrap();
    let tx = TncTransmitter::new(cfg);
    let mut rx: DefaultTncReceiver = TncReceiver::new(cfg).unwrap();
    let packet = position_uncompressed();
    let mut info_buf = [0u8; 330];
    let mut frame_buf = [0u8; 330];
    let mut decoded = 0u32;
    if use_f32 {
        let samples = tx
            .transmit_f32(
                &packet,
                addr(DEST, 0),
                addr(SRC, 7),
                &[],
                &mut info_buf,
                &mut frame_buf,
            )
            .unwrap();
        for s in samples {
            if let Some(frame) = rx.push_f32(s) {
                assert_eq!(frame.aprs().unwrap(), packet);
                decoded += 1;
            }
        }
    } else {
        let samples = tx
            .transmit_i16(
                &packet,
                addr(DEST, 0),
                addr(SRC, 7),
                &[],
                &mut info_buf,
                &mut frame_buf,
            )
            .unwrap();
        for s in samples {
            if let Some(frame) = rx.push_i16(s) {
                assert_eq!(frame.aprs().unwrap(), packet);
                decoded += 1;
            }
        }
    }
    assert_eq!(
        decoded,
        1,
        "{profile:?} @ {sr_hz} Hz ({}): expected exactly one frame",
        if use_f32 { "f32" } else { "i16" }
    );
    assert_eq!(rx.stats().frames_ok, 1);
}

macro_rules! preset_loop_tests {
    ($($name:ident: $profile:expr;)*) => {$(
        #[test]
        fn $name() {
            // Two sample rates, both PCM paths, per preset.
            for sr in RATES {
                preset_loopback($profile, sr, false);
                preset_loopback($profile, sr, true);
            }
        }
    )*};
}

preset_loop_tests! {
    loop_preset_hf_aprs_300: ModemProfile::HF_APRS_300;
    loop_preset_bell_103_originate: ModemProfile::BELL_103_ORIGINATE;
    loop_preset_bell_103_answer: ModemProfile::BELL_103_ANSWER;
    loop_preset_bell_202: ModemProfile::BELL_202;
}

/// 300-baud loopback also at the rate extremes (8 kHz and 48 kHz),
/// exercising the shortest and longest correlator windows.
#[test]
fn loop_preset_300_baud_rate_extremes() {
    for sr in [8_000, 48_000] {
        preset_loopback(ModemProfile::HF_APRS_300, sr, false);
        preset_loopback(ModemProfile::HF_APRS_300, sr, true);
    }
}

#[test]
fn preset_constants_pinned() {
    assert_eq!(ModemProfile::BELL_202.baud().bps(), 1_200);
    assert_eq!(ModemProfile::BELL_202.tones(), TonePair::BELL_202);
    assert_eq!(ModemProfile::HF_APRS_300.baud().bps(), 300);
    assert_eq!(ModemProfile::HF_APRS_300.tones(), TonePair::HF_APRS);
    assert_eq!(TonePair::HF_APRS.mark_hz(), 1_600);
    assert_eq!(TonePair::HF_APRS.space_hz(), 1_800);
    assert_eq!(ModemProfile::BELL_103, ModemProfile::BELL_103_ORIGINATE);
    assert_eq!(ModemProfile::BELL_103.baud().bps(), 300);
    assert_eq!(TonePair::BELL_103_ORIGINATE.mark_hz(), 1_270);
    assert_eq!(TonePair::BELL_103_ORIGINATE.space_hz(), 1_070);
    assert_eq!(ModemProfile::BELL_103_ANSWER.baud().bps(), 300);
    assert_eq!(TonePair::BELL_103_ANSWER.mark_hz(), 2_225);
    assert_eq!(TonePair::BELL_103_ANSWER.space_hz(), 2_025);
}

#[test]
fn preset_configs_valid_at_all_tested_rates() {
    for hz in [8_000, 11_025, 22_050, 44_100, 48_000] {
        let rate = SampleRate::new(hz).unwrap();
        for profile in [
            ModemProfile::BELL_202,
            ModemProfile::HF_APRS_300,
            ModemProfile::BELL_103_ORIGINATE,
            ModemProfile::BELL_103_ANSWER,
        ] {
            assert!(
                TncConfig::from_profile(rate, profile).is_ok(),
                "{profile:?} rejected at {hz} Hz"
            );
        }
    }
}

/// Every chain-bank configuration must still decode.
///
/// The receiver skips correlator banks no active chain consumes, which
/// is worth a 2.5x speedup on the single-chain presets embedded users
/// are steered towards. The risk is obvious: mis-compute which banks are
/// live and the chains read a bank that was never advanced, so the
/// receiver silently decodes nothing. Each configuration below drives a
/// different subset of the three banks:
///
/// * `UNITY` — one raw chain, so *only* the raw bank is live;
/// * `DEFAULT` — raw, band-passed and pre-emphasized chains, all three;
/// * `InputBandPass::On` — every chain band-passed, so the raw and
///   pre-emphasized banks are both dead.
#[test]
fn every_chain_bank_configuration_decodes() {
    use yodel::tnc::{ChainVoting, InputBandPass, SpaceGainSweep};

    let rate = SampleRate::new(48_000).unwrap();
    let base = TncConfig::bell_202(rate).unwrap();
    let packet = AprsPacket::Status(Status {
        text: b"bank gating",
    });

    let configs = [
        (
            "UNITY (raw only)",
            base.with_space_gain_sweep(SpaceGainSweep::UNITY),
        ),
        ("DEFAULT (all three)", base),
        (
            "band-pass on (filtered only)",
            base.with_band_pass(InputBandPass::On),
        ),
        (
            "UNITY + band-pass on",
            base.with_space_gain_sweep(SpaceGainSweep::UNITY)
                .with_band_pass(InputBandPass::On),
        ),
        ("voting off", base.with_voting(ChainVoting::Off)),
    ];

    for (label, cfg) in configs {
        let tx = TncTransmitter::new(cfg);
        let mut info = [0u8; 64];
        let mut frame = [0u8; 330];
        let samples: Vec<i16> = tx
            .transmit_i16(
                &packet,
                Address::new(b"APRS", 0).unwrap(),
                Address::new(b"N0CALL", 7).unwrap(),
                &[],
                &mut info,
                &mut frame,
            )
            .unwrap()
            .collect();

        let mut rx: DefaultTncReceiver = TncReceiver::new(cfg).unwrap();
        let mut got = 0;
        for s in samples {
            if let Some(f) = rx.push_i16(s) {
                assert_eq!(f.src().callsign.as_bytes(), b"N0CALL", "{label}");
                got += 1;
            }
        }
        assert_eq!(got, 1, "{label}: expected exactly one frame");
    }
}

/// The profile path must be behavior-identical to the historical Bell
/// 202 constructor: same config, bit for bit.
#[test]
fn bell_202_profile_matches_bell_202_constructor() {
    for hz in [8_000, 11_025, 22_050, 44_100, 48_000] {
        let rate = SampleRate::new(hz).unwrap();
        assert_eq!(
            TncConfig::from_profile(rate, ModemProfile::BELL_202).unwrap(),
            TncConfig::bell_202(rate).unwrap()
        );
    }
}
