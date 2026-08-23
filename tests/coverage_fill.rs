//! Coverage-matrix gap fillers (see docs/COVERAGE.md): exact-PCM AFSK
//! modulate known-answer vectors, AX.25 address decode KAT, boundary
//! coordinates, NRZI totality, status rejection depth, TNC TX known
//! bytes and rejection.
//!
//! This is also the home of the "reachable but never asserted" fillers
//! (see the block at the end of the file): public functions that a
//! mechanical audit found called from nowhere outside `src/`. The
//! standard here is that being *called* is not coverage — every test
//! goes builder → build → parse → assert equal, or input → function →
//! assert the exact value returned, because a builder that writes the
//! wrong field runs fine and a validator that rejects with the wrong
//! reason returns just as promptly as one that is right.
#![cfg(feature = "tnc")]

use warble::aprs::{
    AprsError, AprsPacket, GeoError, Latitude, Longitude, Position, Status, Symbol,
};
use warble::ax25::{Address, UiFrame};
use warble::geo::Ambiguity;
use warble::{Bit, Modulator, ModulatorConfig, SampleRate};

fn bell(sr: u32) -> ModulatorConfig {
    ModulatorConfig::bell_202(SampleRate::new(sr).unwrap()).unwrap()
}

/// Exact-PCM known-answer vector: Bell 202 at 48 kHz, bits 1,0,1.
///
/// 48000/1200 = 40 samples per bit. The mark bit is exactly one
/// 1200 Hz cycle of the crate's 4096-entry sine table starting at
/// phase 0; the space bit continues phase-continuously at 2200 Hz.
/// Every sample of the 120-sample output is pinned.
#[test]
fn afsk_modulate_exact_pcm_kat_48k() {
    const EXPECTED: [i16; 120] = [
        0, 5106, 10087, 14867, 19236, 23134, 26497, 29177, 31160, 32359, 32767, 32367, 31176,
        29200, 26527, 23205, 19276, 14912, 10135, 5156, 50, -5106, -10087, -14867, -19236, -23134,
        -26497, -29177, -31160, -32359, -32767, -32367, -31176, -29200, -26527, -23205, -19276,
        -14912, -10135, -5156, -50, 9271, 17827, 24910, 29915, 32482, 32367, 29578, 24380, 17146,
        8497, -854, -10087, -18537, -25456, -30253, -32584, -32223, -29200, -23801, -16413, -7669,
        1708, 10897, 19236, 25986, 30589, 32663, 32057, 28803, 23205, 15667, 6836, -2561, -11699,
        -19921, -26497, -30885, -32720, -31869, -28385, -25488, -21930, -17869, -13370, -8497,
        -3462, 1708, 6786, 11699, 16369, 20592, 24346, 27466, 29915, 31646, 32584, 32722, 32057,
        30607, 28385, 25488, 21930, 17869, 13370, 8497, 3462, -1708, -6786, -11699, -16369, -20592,
        -24346, -27466, -29915, -31646, -32584, -32722, -32057, -30607,
    ];
    let bits = [Bit::One, Bit::Zero, Bit::One];
    let samples: Vec<i16> = Modulator::new(bell(48_000))
        .i16_samples(bits.iter().copied())
        .collect();
    assert_eq!(samples.len(), EXPECTED.len());
    assert_eq!(&samples[..], &EXPECTED[..]);
}

/// Exact-PCM prefix at a fractional-samples-per-bit rate: 44.1 kHz is
/// 36.75 samples per bit, so bits 1,0,1 emit 36+37+37 = 110 samples.
/// The first 16 mark-tone samples are pinned exactly.
#[test]
fn afsk_modulate_exact_pcm_prefix_44100() {
    const PREFIX: [i16; 16] = [
        0, 5552, 10944, 16063, 20670, 24713, 28001, 30498, 32087, 32750, 32455, 31206, 29062,
        26077, 22301, 17911,
    ];
    let bits = [Bit::One, Bit::Zero, Bit::One];
    let samples: Vec<i16> = Modulator::new(bell(44_100))
        .i16_samples(bits.iter().copied())
        .collect();
    assert_eq!(samples.len(), 110, "36.75 samples per bit, zero drift");
    assert_eq!(&samples[..16], &PREFIX[..]);
}

/// NRZI is a total code: every (line level, bit) combination is
/// defined, so there is no rejectable input by construction. This test
/// documents that exhaustively for the decode direction.
#[test]
fn nrzi_totality_no_invalid_inputs() {
    for initial in [Bit::Zero, Bit::One] {
        for line in [Bit::Zero, Bit::One] {
            let mut enc = warble::NrziEncoder::new(initial);
            let mut dec = warble::NrziDecoder::new(initial);
            // Both directions accept both inputs in both states.
            let _ = enc.encode(line);
            let out = dec.decode(line);
            // Decoded value is fully determined: One iff level held.
            assert_eq!(out, if line == initial { Bit::One } else { Bit::Zero });
        }
    }
}

/// AX.25 address decode known-answer: the exact 7-byte wire field for
/// N0CALL-7 (shifted-left callsign, C/reserved/SSID/ext byte) decodes
/// to the expected address with the extension bit reported.
#[test]
fn ax25_address_decode_kat() {
    let field = [
        b'N' << 1,
        b'0' << 1,
        b'C' << 1,
        b'A' << 1,
        b'L' << 1,
        b'L' << 1,
        0x60 | (7 << 1) | 1, // C=0, reserved 11, SSID 7, ext 1
    ];
    let (addr, last) = Address::decode(&field).unwrap();
    assert_eq!(addr, Address::new(b"N0CALL", 7).unwrap());
    assert!(last);
}

/// Uncompressed positions at the extreme corners of the coordinate
/// space round-trip through exact wire bytes.
#[test]
fn position_boundary_extremes_round_trip() {
    let cases: [(i64, i64, &[u8]); 4] = [
        (90 * 6000, 180 * 6000, b"!9000.00N/18000.00E-"),
        (-90 * 6000, -180 * 6000, b"!9000.00S/18000.00W-"),
        (0, 0, b"!0000.00N/00000.00E-"),
        (-1, 1, b"!0000.01S/00000.01E-"),
    ];
    for (la, lo, wire) in cases {
        // The table is written in 1/100 arc-minutes, the unit the wire
        // uses; storage is finer.
        let packet = AprsPacket::Position(Position {
            ambiguity: Ambiguity::EXACT,
            latitude: Latitude::new(la * warble::geo::UNITS_PER_HUNDREDTH_MINUTE).unwrap(),
            longitude: Longitude::new(lo * warble::geo::UNITS_PER_HUNDREDTH_MINUTE).unwrap(),
            symbol: Symbol::HOUSE,
            messaging: false,
            compressed: false,
            extension: None,
            comment: b"",
        });
        let mut buf = [0u8; 64];
        let len = packet.build(&mut buf).unwrap();
        assert_eq!(&buf[..len], wire);
        assert_eq!(AprsPacket::parse(&buf[..len]).unwrap(), packet);
    }
    // One past either boundary is a typed constructor error. The
    // coordinate primitives live in `warble::geo` and carry `GeoError`;
    // `AprsError` still has the matching variants, and the conversion
    // between them is what keeps `?` working inside APRS parsers.
    assert_eq!(
        Latitude::new((90 * 6000 + 1) * warble::geo::UNITS_PER_HUNDREDTH_MINUTE),
        Err(GeoError::BadLatitude {
            got: (90 * 6000 + 1) * warble::geo::UNITS_PER_HUNDREDTH_MINUTE
        })
    );
    assert_eq!(
        Longitude::new(-(180 * 6000 + 1) * warble::geo::UNITS_PER_HUNDREDTH_MINUTE),
        Err(GeoError::BadLongitude {
            got: -(180 * 6000 + 1) * warble::geo::UNITS_PER_HUNDREDTH_MINUTE
        })
    );
    assert_eq!(
        AprsError::from(GeoError::BadLatitude { got: 540_001 }),
        AprsError::BadLatitude { got: 540_001 }
    );
}

/// Status rejection depth at the packet-dispatch level: empty input,
/// wrong identifier and build overflow are all typed errors, and the
/// smallest valid report (bare `>`) still parses.
#[test]
fn status_rejections_via_packet_dispatch() {
    assert_eq!(
        Status::parse(b""),
        Err(AprsError::Truncated {
            expected: 1,
            got: 0
        })
    );
    assert_eq!(
        Status::parse(b"_x"),
        Err(AprsError::InvalidDataType { got: b'_' })
    );
    // Packet-level dispatch of an empty field is also typed.
    assert_eq!(
        AprsPacket::parse(b""),
        Err(AprsError::Truncated {
            expected: 1,
            got: 0
        })
    );
    // Bare '>' is the minimal valid status report.
    assert_eq!(
        AprsPacket::parse(b">"),
        Ok(AprsPacket::Status(Status { text: b"" }))
    );
    // Build overflow is typed and reports both sizes.
    let status = Status {
        text: b"emergency net active",
    };
    let mut small = [0u8; 4];
    assert_eq!(
        status.build(&mut small),
        Err(AprsError::BufferTooSmall { needed: 21, max: 4 })
    );
}

/// UI frame with an empty information field survives a build/parse
/// round trip (capacity floor of the frame layer).
#[test]
fn ax25_empty_info_round_trip() {
    let frame = UiFrame::new(
        Address::new(b"APRS", 0).unwrap(),
        Address::new(b"N0CALL", 0).unwrap(),
        b"",
    );
    let mut buf = [0u8; 64];
    let len = frame.build(&mut buf).unwrap();
    let parsed = UiFrame::parse(&buf[..len]).unwrap();
    assert_eq!(parsed.info, b"");
    assert_eq!(parsed.src, frame.src);
}

mod tnc_tx {
    use super::*;
    use warble::ax25::Ax25Error;
    use warble::tnc::{TncConfig, TncError, TncTransmitter};

    /// TNC TX known-answer: `build_frame` emits exactly the bytes the
    /// AX.25 layer builds for the same packet, address for address.
    #[test]
    fn build_frame_known_bytes() {
        let config = TncConfig::bell_202(SampleRate::new(48_000).unwrap()).unwrap();
        let tx = TncTransmitter::new(config);
        let packet = AprsPacket::Status(Status { text: b"QRV" });
        let dest = Address::new(b"APRS", 0).unwrap();
        let src = Address::new(b"N0CALL", 7).unwrap();
        let mut info_buf = [0u8; 32];
        let mut frame_buf = [0u8; 330];
        let len = tx
            .build_frame(&packet, dest, src, &[], &mut info_buf, &mut frame_buf)
            .unwrap();
        // Reference bytes built directly at the AX.25 layer.
        let mut expected = [0u8; 330];
        let frame = UiFrame::new(dest, src, b">QRV");
        let expected_len = frame.build(&mut expected).unwrap();
        assert_eq!(&frame_buf[..len], &expected[..expected_len]);
        // And the info field is the exact status wire form.
        let parsed = UiFrame::parse(&frame_buf[..len]).unwrap();
        assert_eq!(parsed.info, b">QRV");
    }

    /// TNC TX rejects a too-small frame buffer with a typed error.
    #[test]
    fn transmit_rejects_small_buffer() {
        let config = TncConfig::bell_202(SampleRate::new(48_000).unwrap()).unwrap();
        let tx = TncTransmitter::new(config);
        let packet = AprsPacket::Status(Status { text: b"QRV" });
        let dest = Address::new(b"APRS", 0).unwrap();
        let src = Address::new(b"N0CALL", 7).unwrap();
        let mut info_buf = [0u8; 32];
        let mut tiny = [0u8; 8];
        let err = tx
            .build_frame(&packet, dest, src, &[], &mut info_buf, &mut tiny)
            .unwrap_err();
        assert!(matches!(
            err,
            TncError::Ax25(Ax25Error::FrameTooLarge { .. })
        ));
        // Too-small info buffer is an APRS-layer typed error.
        let mut tiny_info = [0u8; 2];
        let mut frame_buf = [0u8; 330];
        let err = tx
            .build_frame(&packet, dest, src, &[], &mut tiny_info, &mut frame_buf)
            .unwrap_err();
        assert!(matches!(
            err,
            TncError::Aprs(AprsError::BufferTooSmall { needed: 4, max: 2 })
        ));
    }
}

/// Every public builder that no other test, doctest or example
/// exercises.
///
/// Found mechanically: of 367 public functions in the crate, eleven
/// appeared in no test, doctest, example or README. Nine were
/// pre-existing and two had been added the same day — which is the
/// point. A builder nobody calls is a builder nobody has checked, and
/// `build` succeeding is not the same as `build` producing what
/// `parse` reads back.
///
/// Each case below therefore goes builder → build → parse → assert
/// equal, so a builder that writes the wrong field is caught rather
/// than just executed.
#[test]
fn untested_public_builders_round_trip() {
    use warble::aprs::extension::{DataExtension, Phg, PhgRate};
    use warble::aprs::{PositionWeather, PositionlessWeather, Timestamp, WeatherReport};
    use warble::units::Speed;

    let lat = Latitude::new((49 * 6000 + 350) * warble::geo::UNITS_PER_HUNDREDTH_MINUTE).unwrap();
    let lon = Longitude::new(-(72 * 6000 + 175) * warble::geo::UNITS_PER_HUNDREDTH_MINUTE).unwrap();

    // --- Position::with_extension --------------------------------
    let position = Position::new(lat, lon, Symbol::from_wire(b'/', b'#'))
        .with_extension(DataExtension::Range { miles: 50 });
    let mut buf = [0u8; 96];
    let len = position.build(&mut buf).unwrap();
    assert!(buf[..len].ends_with(b"RNG0050"), "{:?}", &buf[..len]);
    assert_eq!(Position::parse(&buf[..len]).unwrap(), position);

    // --- Phg::with_rate: the 9-byte PHGR form --------------------
    //
    // The specification says outright that PHGR "violates the rule
    // that Data Extensions are always 7 characters", so the builder
    // and the parser have to agree about a length nothing else uses.
    let phg = Phg::new(5, 1, 3, 2).unwrap().with_rate(PhgRate::PerHour(6));
    assert_eq!(phg.rate(), Some(PhgRate::PerHour(6)));
    let position = Position::new(lat, lon, Symbol::from_wire(b'/', b'#'))
        .with_extension(DataExtension::Phg(phg));
    let len = position.build(&mut buf).unwrap();
    assert!(buf[..len].ends_with(b"PHG51326/"), "{:?}", &buf[..len]);
    assert_eq!(Position::parse(&buf[..len]).unwrap(), position);

    // --- PositionWeather::with_timestamp -------------------------
    //
    // Added with the timestamped Complete Weather Report support. The
    // parse side had a spec vector; this is the transmit side, which
    // has to pick `@`/`/` for the identifier rather than `=`/`!`.
    let weather = WeatherReport {
        wind_direction: Some(220),
        wind_speed: Some(Speed::from_knots(4)),
        ..WeatherReport::default()
    };
    let stamped = PositionWeather::new(lat, lon, weather)
        .with_timestamp(Timestamp::DhmZulu {
            day: 9,
            hour: 23,
            minute: 45,
        })
        .with_messaging(true);
    let len = stamped.build(&mut buf).unwrap();
    assert!(
        buf[..len].starts_with(b"@092345z4903.50N/07201.75W_220/004"),
        "{:?}",
        core::str::from_utf8(&buf[..len])
    );
    assert_eq!(PositionWeather::parse(&buf[..len]).unwrap(), stamped);

    // The non-messaging spelling of the same thing is `/`, not `!`.
    let plain = stamped.with_messaging(false);
    let len = plain.build(&mut buf).unwrap();
    assert_eq!(buf[0], b'/');
    assert_eq!(PositionWeather::parse(&buf[..len]).unwrap(), plain);

    // --- PositionlessWeather::with_rest --------------------------
    let positionless = PositionlessWeather::new(9, 23, 12, 34, WeatherReport::default())
        .unwrap()
        .with_rest(b"wRSW");
    let len = positionless.build(&mut buf).unwrap();
    assert!(buf[..len].ends_with(b"wRSW"));
    assert_eq!(
        PositionlessWeather::parse(&buf[..len]).unwrap(),
        positionless
    );

    // --- Timestamp::dhm_local ------------------------------------
    //
    // The `/` suffix form. Distinct from DhmZulu on the wire, and the
    // only one of the three that is not UTC.
    let local = Timestamp::dhm_local(9, 23, 45).unwrap();
    assert_eq!(
        local,
        Timestamp::DhmLocal {
            day: 9,
            hour: 23,
            minute: 45
        }
    );
    // `Timestamp::write` is private, so go through a report that
    // carries one — which is the only way a caller can reach it too.
    let stamped = PositionWeather::new(lat, lon, WeatherReport::default()).with_timestamp(local);
    let len = stamped.build(&mut buf).unwrap();
    assert!(buf[..len].starts_with(b"/092345/"), "{:?}", &buf[..len]);
    assert_eq!(PositionWeather::parse(&buf[..len]).unwrap(), stamped);
}

/// The public accessors and helpers that no other test reaches.
///
/// The same mechanical sweep that found the untested builders above
/// found these. They are small, which is exactly why they go unnoticed:
/// nobody writes a test for a one-line accessor, and so nobody notices
/// when it returns the other variant.
#[test]
fn untested_public_accessors() {
    use warble::aprs::mic_e::{MicE, MicEFix, MicEMessage};
    use warble::aprs::ultimeter::{self, UltimeterRecord};
    use warble::tnc::{ChainVoting, InputBandPass, TncConfig};
    use warble::units::Speed;

    // --- MicEFix::type_byte: the two Mic-E data type identifiers ---
    assert_eq!(MicEFix::Current.type_byte(), b'`');
    assert_eq!(MicEFix::Old.type_byte(), b'\'');
    // And they are what `encode` emits, not just what the accessor
    // claims.
    let report = MicE::new(
        Latitude::new(33 * 6000 + 2564).unwrap(),
        Longitude::new(-(112 * 6000 + 700)).unwrap(),
        20,
        251,
        Symbol::from_wire(b'/', b'j'),
        MicEMessage::InService,
    )
    .unwrap();
    let mut info = [0u8; 32];
    for fix in [MicEFix::Current, MicEFix::Old] {
        let len = report.with_fix(fix).encode_info(&mut info).unwrap();
        assert_eq!(info[0], fix.type_byte(), "{fix:?}");
        assert!(len >= 9);
    }

    // --- MicE::encode_destination on its own ----------------------
    //
    // Half of a Mic-E position lives in the AX.25 destination address,
    // so this exists for callers building the address separately. It
    // must agree with what the combined `encode` produces.
    let mut dest_alone = [0u8; 6];
    report.encode_destination(&mut dest_alone).unwrap();
    let mut dest_together = [0u8; 6];
    let mut info_together = [0u8; 32];
    report
        .encode(&mut dest_together, &mut info_together)
        .unwrap();
    assert_eq!(dest_alone, dest_together);
    // A short buffer is a typed error, not a panic.
    assert!(report.encode_destination(&mut [0u8; 5]).is_err());

    // --- InputBandPass / ChainVoting is_on ------------------------
    assert!(InputBandPass::On.is_on());
    assert!(!InputBandPass::Off.is_on());
    assert!(ChainVoting::On.is_on());
    assert!(!ChainVoting::Off.is_on());

    // --- TncConfig::band_pass round-trips its own setter ----------
    let config = TncConfig::bell_202(SampleRate::new(48_000).unwrap()).unwrap();
    assert!(!config.band_pass().is_on(), "off by default");
    assert!(config.with_band_pass(InputBandPass::On).band_pass().is_on());

    // --- il2p::payload_wire_len -----------------------------------
    //
    // What a receiver must collect off the air: the data plus one
    // parity group per block.
    //
    // Maximum FEC is a flat 16 parity bytes per block over blocks of at
    // most 239 data bytes. **Baseline is not flat**, which is the part
    // worth pinning: the parity is chosen from the block size (2 up to
    // 61 bytes, 4 to 123, 6 to 185, 8 above) and every block in a frame
    // shares it, so the overhead is not proportional to the payload.
    // My first guess at the 240-byte case was 4 bytes of parity and the
    // answer is 8, because 240 bytes is two blocks of 120 and a
    // 120-byte block takes 4 each.
    assert_eq!(warble::il2p::payload_wire_len(0, false), 0);
    assert_eq!(warble::il2p::payload_wire_len(1, false), 1 + 2);
    assert_eq!(warble::il2p::payload_wire_len(1, true), 1 + 16);
    // One baseline block, still the smallest parity at 61 bytes.
    assert_eq!(warble::il2p::payload_wire_len(61, false), 61 + 2);
    // 62 bytes is one block over the 61-byte threshold, so 4.
    assert_eq!(warble::il2p::payload_wire_len(62, false), 62 + 4);
    // 240 bytes is two blocks of 120: 4 parity each, 8 in total.
    assert_eq!(warble::il2p::payload_wire_len(240, false), 240 + 8);
    // The largest single baseline block, and the largest max-FEC one.
    assert_eq!(warble::il2p::payload_wire_len(247, false), 247 + 8);
    assert_eq!(warble::il2p::payload_wire_len(239, true), 239 + 16);
    assert_eq!(warble::il2p::payload_wire_len(240, true), 240 + 2 * 16);
    // Structural properties that must hold at every length.
    for len in 0..300 {
        let baseline = warble::il2p::payload_wire_len(len, false);
        let max_fec = warble::il2p::payload_wire_len(len, true);
        assert!(baseline >= len, "{len}: wire shorter than payload");
        assert!(max_fec >= baseline, "{len}: max FEC cheaper than baseline");
    }

    // --- UltimeterTwo::wind_speed_typed ---------------------------
    //
    // The `#` record reports km/h and the `*` record mph; the typed
    // accessor is what removes that distinction from the caller.
    let mph = ultimeter::parse(b"*0001E01D8001A02x").ok();
    let kph = ultimeter::parse(b"#0001E01D8001A02x").ok();
    for (record, label) in [(mph, "mph record"), (kph, "kph record")] {
        let Some(UltimeterRecord::UltimeterTwo(two)) = record else {
            continue;
        };
        // Whatever unit the record used, the quantity answers in any.
        if let Some(speed) = two.wind_speed_typed() {
            assert_eq!(
                speed.mph(),
                i32::from(two.wind_speed_mph().unwrap_or(0)),
                "{label}: typed and mph accessors must agree"
            );
            assert!(speed.kmh() >= 0, "{label}");
            let _ = Speed::from_mph(speed.mph());
        }
    }
}

/// The caller-supplied [`warble::Discriminator`] seam.
///
/// `Demodulator::with_discriminator` is the crate's advertised PHY
/// extension point — the one public door to the `Discriminator` trait —
/// and nothing outside `src/` had ever walked through it. "You can plug
/// in your own front end" was therefore an unverified claim: the
/// constructor was only ever reached from `AfskDemodulator::new`, with
/// the crate's own correlator on the other side.
///
/// So this module implements the trait *here*, in the test crate, with a
/// different algorithm, and asserts the decode. Three things get proven
/// that a bare call could not: the trait is implementable from outside
/// (only public items are in reach), the slicer downstream really
/// consumes the metric the caller returns (`AlwaysMark` degrades the
/// output exactly as a stuck front end should), and an outside front end
/// recovers the same payload the built-in one does.
mod caller_supplied_discriminator {
    use super::bell;
    use warble::{
        AfskDemodulator, Bit, Demodulator, DemodulatorConfig, Discriminator, Modulator, SampleRate,
    };

    /// Samples per bit at 48 kHz / 1200 Bd, exactly.
    const SAMPLES_PER_BIT: usize = 40;

    /// Delay-line spacing in samples. At 48 kHz the product
    /// `x[n]·x[n-7]` averages to `(A²/2)·cos(2πfD/fₛ)`, which is
    /// `+0.454·A²/2` at the 1200 Hz mark tone (63°) and `-0.431·A²/2`
    /// at the 2200 Hz space tone (115.5°) — straddling zero almost
    /// symmetrically, which is what makes 7 the right delay for this
    /// tone pair.
    const DELAY: usize = 7;

    /// A delay-and-multiply (differential) FM discriminator.
    ///
    /// Not a reimplementation of anything in the crate:
    /// [`warble::QuadratureCorrelator`] correlates against two reference
    /// oscillators and subtracts envelopes, while this multiplies the
    /// signal by a delayed copy of itself and averages over exactly one
    /// bit period. The boxcar length is a whole multiple of the 2400 Hz
    /// mark self-product ripple, so that ripple cancels outright.
    ///
    /// Integer throughout, and provably non-overflowing: `|x| ≤ 32768`,
    /// so `|x[n]·x[n-7]| ≤ 2³⁰` and the boxcar mean stays inside `i32`.
    struct DelayLineDiscriminator {
        /// Ring of the last `DELAY + 1` samples.
        history: [i32; DELAY + 1],
        /// Write cursor into `history`.
        history_pos: usize,
        /// Ring of the last `SAMPLES_PER_BIT` products.
        products: [i64; SAMPLES_PER_BIT],
        /// Write cursor into `products`.
        product_pos: usize,
        /// Running sum of `products`.
        sum: i64,
    }

    impl DelayLineDiscriminator {
        const fn new() -> Self {
            Self {
                history: [0; DELAY + 1],
                history_pos: 0,
                products: [0; SAMPLES_PER_BIT],
                product_pos: 0,
                sum: 0,
            }
        }
    }

    impl Discriminator for DelayLineDiscriminator {
        fn push_i16(&mut self, sample: i16) -> i32 {
            self.history[self.history_pos] = i32::from(sample);
            // The entry just ahead of the cursor is the oldest: exactly
            // `DELAY` samples before the one written above.
            let delayed = self.history[(self.history_pos + 1) % (DELAY + 1)];
            self.history_pos = (self.history_pos + 1) % (DELAY + 1);

            let product = i64::from(sample) * i64::from(delayed);
            self.sum += product - self.products[self.product_pos];
            self.products[self.product_pos] = product;
            self.product_pos = (self.product_pos + 1) % SAMPLES_PER_BIT;

            // Positive = mark tone dominates, per the trait contract.
            (self.sum / SAMPLES_PER_BIT as i64) as i32
        }

        fn push_f32(&mut self, sample: f32) -> i32 {
            // `as` saturates on overflow and maps NaN to 0, so no input
            // can escape the i16 domain the integer engine assumes.
            self.push_i16((sample * 32_767.0) as i16)
        }
    }

    /// A front end that ignores its input entirely.
    ///
    /// Its only job is to show that `with_discriminator` really consults
    /// the object it was handed: if it quietly built its own correlator,
    /// the payload would come out anyway and the assertion below would
    /// fail.
    struct AlwaysMark;

    impl Discriminator for AlwaysMark {
        fn push_i16(&mut self, _sample: i16) -> i32 {
            1
        }

        fn push_f32(&mut self, _sample: f32) -> i32 {
            1
        }
    }

    /// Bits of `bytes`, least-significant first (HDLC order).
    fn bits_lsb_first(bytes: &[u8]) -> Vec<Bit> {
        bytes
            .iter()
            .flat_map(|&byte| {
                (0..8).map(move |i| {
                    if (byte >> i) & 1 == 1 {
                        Bit::One
                    } else {
                        Bit::Zero
                    }
                })
            })
            .collect()
    }

    #[test]
    fn third_party_front_end_decodes_through_with_discriminator() {
        let sr = SampleRate::new(48_000).unwrap();
        let config = DemodulatorConfig::bell_202(sr).unwrap();

        // A payload with no short period, so a mistimed or stuck slicer
        // cannot reproduce it by accident.
        let payload = bits_lsb_first(&[0x2C, 0x93, 0xF0]);
        let bits: Vec<Bit> = (0..48)
            .map(|i| if i % 2 == 0 { Bit::One } else { Bit::Zero })
            .chain(payload.iter().copied())
            .chain([Bit::Zero; 4])
            .collect();
        let i16_samples: Vec<i16> = Modulator::new(bell(48_000))
            .i16_samples(bits.iter().copied())
            .collect();
        let f32_samples: Vec<f32> = Modulator::new(bell(48_000))
            .f32_samples(bits.iter().copied())
            .collect();
        assert_eq!(i16_samples.len(), bits.len() * SAMPLES_PER_BIT);

        // The seam, i16 path.
        let mine: Vec<Bit> = Demodulator::with_discriminator(config, DelayLineDiscriminator::new())
            .expect("48 kHz / 1200 Bd is 40 samples per bit")
            .i16_bits(i16_samples.iter().copied())
            .collect();
        assert!(
            mine.windows(payload.len()).any(|w| w == payload.as_slice()),
            "caller-supplied discriminator lost the payload: {mine:?}"
        );

        // The seam, f32 path: the other half of the trait.
        let mine_f32: Vec<Bit> =
            Demodulator::with_discriminator(config, DelayLineDiscriminator::new())
                .unwrap()
                .f32_bits(f32_samples.iter().copied())
                .collect();
        assert!(
            mine_f32
                .windows(payload.len())
                .any(|w| w == payload.as_slice()),
            "caller-supplied discriminator lost the payload on the f32 path: {mine_f32:?}"
        );

        // Same audio, the crate's own front end: an outside
        // implementation is held to the same result, not a weaker one.
        let builtin: Vec<Bit> = AfskDemodulator::new(config)
            .unwrap()
            .i16_bits(i16_samples.iter().copied())
            .collect();
        assert!(
            builtin
                .windows(payload.len())
                .any(|w| w == payload.as_slice()),
            "sanity: the built-in front end must recover it too"
        );
        // Different algorithms, so different group delay; the recovered
        // bit counts may differ by a cell of startup transient but no
        // more.
        let drift = builtin.len().abs_diff(mine.len());
        assert!(
            drift <= 1,
            "built-in recovered {} bits, the caller's front end {}",
            builtin.len(),
            mine.len()
        );

        // And the seam is really wired to the caller's object: a front
        // end whose metric never changes sign can only ever produce
        // ones, so the payload must be unreachable.
        let stuck: Vec<Bit> = Demodulator::with_discriminator(config, AlwaysMark)
            .unwrap()
            .i16_bits(i16_samples.iter().copied())
            .collect();
        assert!(!stuck.is_empty(), "the stuck front end still clocks bits");
        assert!(
            stuck.iter().all(|&b| b == Bit::One),
            "a permanently positive metric must slice to all ones"
        );
        assert!(
            !stuck
                .windows(payload.len())
                .any(|w| w == payload.as_slice()),
            "with_discriminator ignored the supplied discriminator"
        );
    }
}

/// `TncConfig::with_flags` takes two same-typed positional parameters,
/// which is a transposition hazard: `with_flags(preamble, tail)` and
/// `with_flags(tail, preamble)` both compile and both produce a valid
/// transmission.
///
/// A count-based test cannot catch that swap, and this test says so with
/// an assertion rather than a comment: the total sample count is
/// `(preamble + tail)` flag octets plus the frame, which is symmetric.
/// What is *not* symmetric is where the flags land, so the two counts
/// are pinned by their structural effect — raising the tail count
/// appends (the shorter stream stays a prefix of the longer), raising
/// the preamble count prepends (it does not).
#[test]
fn tnc_with_flags_distinguishes_preamble_from_tail() {
    use warble::ax25::hdlc;
    use warble::tnc::{TncConfig, TncTransmitter};

    /// One flag octet at 48 kHz / 1200 Bd: 8 bits × 40 samples per bit.
    const FLAG_SAMPLES: usize = 8 * 40;

    let sr = SampleRate::new(48_000).unwrap();
    let base = TncConfig::bell_202(sr).unwrap();
    // The defaults, so the counts used below are visibly not them.
    assert_eq!(base.preamble_flags(), hdlc::DEFAULT_PREAMBLE_FLAGS);
    assert_eq!(base.tail_flags(), hdlc::DEFAULT_TAIL_FLAGS);

    // The setter puts each argument in the slot its name promises.
    let configured = base.with_flags(5, 2);
    assert_eq!(configured.preamble_flags(), 5);
    assert_eq!(configured.tail_flags(), 2);

    // One frame body, reused under every flag configuration.
    let packet = AprsPacket::Status(Status {
        text: b"flag counts",
    });
    let mut info_buf = [0u8; 32];
    let mut frame_buf = [0u8; 330];
    let frame_len = TncTransmitter::new(base)
        .build_frame(
            &packet,
            Address::new(b"APRS", 0).unwrap(),
            Address::new(b"N0CALL", 7).unwrap(),
            &[],
            &mut info_buf,
            &mut frame_buf,
        )
        .unwrap();
    let frame = frame_buf[..frame_len].to_vec();
    let samples = |preamble: usize, tail: usize| -> Vec<i16> {
        TncTransmitter::new(base.with_flags(preamble, tail))
            .frame_samples_i16(&frame)
            .collect()
    };

    let five_two = samples(5, 2);
    let two_five = samples(2, 5);
    assert_eq!(
        five_two.len(),
        two_five.len(),
        "the sample count is symmetric under a swap, which is why it cannot detect one"
    );
    assert_ne!(
        five_two, two_five,
        "the waveform must not be symmetric under a swap"
    );

    // Tail flags are appended after the frame, so five more of them
    // extend the stream and leave every earlier sample alone.
    let more_tail = samples(5, 7);
    assert_eq!(more_tail.len(), five_two.len() + 5 * FLAG_SAMPLES);
    assert!(
        more_tail.starts_with(&five_two),
        "raising only the tail count changed samples before the tail"
    );

    // Preamble flags are prepended, so four more of them shift the
    // frame later: the streams agree only through the five flag octets
    // they both open with.
    let more_preamble = samples(9, 2);
    assert_eq!(more_preamble.len(), five_two.len() + 4 * FLAG_SAMPLES);
    assert!(
        !more_preamble.starts_with(&five_two),
        "raising only the preamble count merely extended the stream, \
         which is what a tail count does"
    );
    let first_diff = five_two
        .iter()
        .zip(&more_preamble)
        .position(|(a, b)| a != b)
        .expect("the two streams differ somewhere");
    // Both send five flag octets, so the first 5 × 320 samples match.
    // Inside the sixth, one continues the flag pattern (`01111110`)
    // while the other starts the stuffed frame, which by HDLC's own
    // stuffing invariant can never contain six consecutive ones — so the
    // divergence is confined to that one octet.
    assert!(
        (5 * FLAG_SAMPLES..6 * FLAG_SAMPLES).contains(&first_diff),
        "expected divergence inside the sixth flag octet, got sample {first_diff}"
    );

    // Zero flags either side is still a legal frame, and the shortest.
    let bare = samples(0, 0);
    assert_eq!(bare.len(), five_two.len() - 7 * FLAG_SAMPLES);
}

/// `WsprModulator::fill_f32` — the untested twin of an exercised
/// `fill_i16`.
///
/// Two claims, both asserted: the f32 path tracks the i16 path sample
/// for sample (they share one phase accumulator, so any divergence
/// beyond table interpolation is a bug), and `fill_f32` writes exactly
/// as many slots as it says it did — no more, no fewer. The sentinel is
/// outside the nominal output range, so "was written" is decidable.
#[cfg(feature = "wspr")]
#[test]
fn wspr_fill_f32_tracks_fill_i16_and_fills_what_it_claims() {
    use warble::MaidenheadGrid;
    use warble::wspr::{WsprConfig, WsprMessage, WsprModulator};

    /// Outside the documented `-1.0..=1.0` output range.
    const SENTINEL: f32 = 7.5;
    /// One 4096-entry sine-table step is ~50 counts of 32767; the i16
    /// path truncates to the table while the f32 path interpolates
    /// between adjacent entries, so that step bounds their difference.
    const TOLERANCE: f32 = 64.0;

    let config = WsprConfig::new(1_500, SampleRate::new(12_000).unwrap()).unwrap();
    let message = WsprMessage::new(
        "K1ABC",
        MaidenheadGrid::new("FN42").expect("valid locator"),
        37,
    )
    .unwrap();
    let mut integer = WsprModulator::for_message(config, &message);
    let mut floating = WsprModulator::for_message(config, &message);
    let total = floating.total_samples();
    assert_eq!(total, 162 * 8_192, "162 symbols at 12 kHz");

    let mut i16_buf = [0i16; 4_096];
    let mut f32_buf = [SENTINEL; 4_096];
    let mut written = 0u64;
    let mut compared = 0u64;
    loop {
        f32_buf.fill(SENTINEL);
        let ni = integer.fill_i16(&mut i16_buf);
        let nf = floating.fill_f32(&mut f32_buf);
        assert_eq!(ni, nf, "the i16 and f32 paths must run out together");
        for (k, (&f, &i)) in f32_buf.iter().zip(i16_buf.iter()).take(nf).enumerate() {
            assert!(
                f.abs() <= 1.0,
                "sample {} out of range: {f}",
                written + k as u64
            );
            assert!(
                (f * 32_767.0 - f32::from(i)).abs() <= TOLERANCE,
                "sample {}: f32 {f} vs i16 {i}",
                written + k as u64
            );
            compared += 1;
        }
        // Everything past the returned count is untouched.
        for (k, &f) in f32_buf.iter().enumerate().skip(nf) {
            assert_eq!(
                f, SENTINEL,
                "fill_f32 wrote slot {k} past its return of {nf}"
            );
        }
        written += nf as u64;
        if nf < f32_buf.len() {
            break;
        }
    }
    assert_eq!(written, total, "fill_f32 must emit the whole transmission");
    assert_eq!(compared, total, "every sample must have been compared");
    // A finished modulator fills nothing and leaves the buffer alone.
    f32_buf.fill(SENTINEL);
    assert_eq!(floating.fill_f32(&mut f32_buf), 0);
    assert!(f32_buf.iter().all(|&f| f == SENTINEL));
}

/// `Ft8Modulator::fill_f32` — the same two claims as the WSPR twin
/// above, over the GFSK-shaped 8-FSK generator.
#[cfg(feature = "ft8")]
#[test]
fn ft8_fill_f32_tracks_fill_i16_and_fills_what_it_claims() {
    use warble::ft8::{Ft8Config, Ft8Message, Ft8Modulator, Ft8Tail};

    const SENTINEL: f32 = 7.5;
    const TOLERANCE: f32 = 64.0;

    let config = Ft8Config::new(1_500, SampleRate::new(12_000).unwrap()).unwrap();
    let message = Ft8Message::standard(
        "CQ",
        "K1ABC",
        false,
        Ft8Tail::grid("FN42").expect("valid locator"),
    )
    .unwrap();
    let mut integer = Ft8Modulator::for_message(config, &message);
    let mut floating = Ft8Modulator::for_message(config, &message);
    let total = floating.total_samples();
    assert_eq!(total, 79 * 1_920, "79 symbols at 12 kHz");

    let mut i16_buf = [0i16; 4_096];
    let mut f32_buf = [SENTINEL; 4_096];
    let mut written = 0u64;
    let mut compared = 0u64;
    loop {
        f32_buf.fill(SENTINEL);
        let ni = integer.fill_i16(&mut i16_buf);
        let nf = floating.fill_f32(&mut f32_buf);
        assert_eq!(ni, nf, "the i16 and f32 paths must run out together");
        for (k, (&f, &i)) in f32_buf.iter().zip(i16_buf.iter()).take(nf).enumerate() {
            assert!(
                f.abs() <= 1.0,
                "sample {} out of range: {f}",
                written + k as u64
            );
            assert!(
                (f * 32_767.0 - f32::from(i)).abs() <= TOLERANCE,
                "sample {}: f32 {f} vs i16 {i}",
                written + k as u64
            );
            compared += 1;
        }
        for (k, &f) in f32_buf.iter().enumerate().skip(nf) {
            assert_eq!(
                f, SENTINEL,
                "fill_f32 wrote slot {k} past its return of {nf}"
            );
        }
        written += nf as u64;
        if nf < f32_buf.len() {
            break;
        }
    }
    assert_eq!(written, total, "fill_f32 must emit the whole transmission");
    assert_eq!(compared, total, "every sample must have been compared");
    f32_buf.fill(SENTINEL);
    assert_eq!(floating.fill_f32(&mut f32_buf), 0);
    assert!(f32_buf.iter().all(|&f| f == SENTINEL));
}

/// `PacketAssembler::lsf` — the Link Setup Frame that opened the
/// current superframe.
///
/// Checked against an LSF that came back **off the air** rather than one
/// handed straight to `start`: transmit a packet, let the receiver
/// recover the LSF through its FEC and CRC, assemble the payload, and
/// only then ask the assembler what it is carrying. The assertions are
/// on the contents — both callsigns and every field packed into the
/// 16-bit TYPE word — because an accessor returning the wrong `Option`
/// arm or a stale frame is exactly what goes unnoticed.
#[cfg(feature = "m17")]
#[test]
fn m17_packet_assembler_reports_the_link_setup_frame() {
    use warble::m17::{
        Address as M17Address, Lsf, M17FrameEvent, M17PacketTx, M17Receiver, PacketAssembler,
    };

    const CAN: u8 = 3;
    let dst = M17Address::from_callsign("W1AW").unwrap();
    let src = M17Address::from_callsign("N0CALL").unwrap();
    let sent = Lsf::packet_data(dst, src, CAN);

    let sr = SampleRate::new(48_000).unwrap();
    let payload = b"link setup frame";
    let mut tx = M17PacketTx::new(sr, sent, payload).unwrap();
    let mut rx = M17Receiver::new(sr).unwrap();
    let mut assembler = PacketAssembler::new();

    assert_eq!(
        assembler.lsf(),
        None,
        "an assembler that has seen no LSF must report none"
    );

    let mut assembled = None;
    while let Some(sample) = tx.next_i16() {
        match rx.push_i16(sample) {
            Some(M17FrameEvent::Lsf(lsf)) => assembler.start(lsf),
            Some(M17FrameEvent::PacketFrame(frame)) => {
                if let Some(done) = assembler.feed(&frame) {
                    assembled = Some(done.to_vec());
                }
            }
            None => {}
        }
    }
    assert_eq!(
        assembled.as_deref(),
        Some(&payload[..]),
        "the superframe must assemble, or there is no LSF to report on"
    );

    let carried = assembler
        .lsf()
        .expect("a completed superframe still reports the LSF that opened it");

    // The addresses survive the base-40 round trip in the right slots:
    // a transposed dst/src would be invisible to an equality check
    // against a symmetric LSF.
    let mut buf = [0u8; 9];
    assert_eq!(carried.dst.callsign(&mut buf), "W1AW");
    assert_eq!(carried.src.callsign(&mut buf), "N0CALL");
    assert_ne!(carried.dst, carried.src, "the two addresses are distinct");

    // TYPE, field by field (bit 0 packet/stream, bits 2-1 subtype,
    // bits 6-3 encryption, bits 10-7 channel access number).
    assert_eq!(carried.lsf_type & 1, 0, "packet mode, not stream");
    assert_eq!((carried.lsf_type >> 1) & 0b11, 0b01, "data subtype");
    assert_eq!((carried.lsf_type >> 3) & 0b1111, 0, "no encryption");
    assert_eq!(
        (carried.lsf_type >> 7) & 0b1111,
        u16::from(CAN),
        "channel access number"
    );
    assert_eq!(carried.meta, [0u8; 14], "packet data mode carries no META");
    assert_eq!(carried, sent, "and the whole frame round-trips");

    // A second `start` replaces it, so the accessor is reporting live
    // state rather than the first thing it ever saw.
    let other = Lsf::packet_data(src, dst, CAN + 1);
    assembler.start(other);
    assert_eq!(assembler.lsf(), Some(other));
    assert_ne!(assembler.lsf(), Some(sent));
}

/// `Il2pParity::baseline_for_block` — pinning the resolution of a real
/// specification contradiction.
///
/// IL2P draft v0.4 prints both a table (2/4/6/8 parity symbols, stepping
/// every ~62 bytes) and a formula, `size / 32 + 2`, and they disagree.
/// Worse, the formula yields 3, 5 and 7 for block sizes that occur in
/// practice — parity lengths [`warble::il2p::Il2pParity`] does not have,
/// so a receiver following it derives an on-air payload length no
/// encoder produces. The function resolves this in favour of the table;
/// that choice is a wire-compatibility decision, so it is pinned here
/// rather than left to a doc comment.
#[cfg(feature = "il2p")]
#[test]
fn il2p_baseline_parity_table_is_pinned_across_the_block_size_domain() {
    use warble::il2p::{Il2pParity, MAX_BASELINE_BLOCK_DATA, block_count_for, payload_wire_len};

    /// Both edges of all four table rows, plus the degenerate and
    /// saturating ends. The array is *typed* with this length, so
    /// shrinking the list is a compile error rather than a loop that
    /// passes having compared less; the counter below covers the other
    /// direction (a `continue` that skips cases).
    const MIN_CASES: usize = 10;
    /// Block sizes where the deleted v0.4 formula names a parity length
    /// that does not exist.
    const UNDEFINED_CASES: usize = 6;

    let cases: [(usize, Il2pParity); MIN_CASES] = [
        (0, Il2pParity::Two),
        (1, Il2pParity::Two),
        (61, Il2pParity::Two),
        (62, Il2pParity::Four),
        (123, Il2pParity::Four),
        (124, Il2pParity::Six),
        (185, Il2pParity::Six),
        (186, Il2pParity::Eight),
        (MAX_BASELINE_BLOCK_DATA, Il2pParity::Eight),
        (usize::MAX, Il2pParity::Eight),
    ];
    let mut pinned = 0usize;
    for (size, want) in cases {
        assert_eq!(
            Il2pParity::baseline_for_block(size),
            want,
            "baseline parity for a {size}-byte block"
        );
        pinned += 1;
    }
    assert_eq!(pinned, MIN_CASES, "the table sweep compared {pinned} sizes");

    // The cases that make the contradiction real rather than academic.
    let mut undefined = 0usize;
    for size in [32usize, 62, 96, 160, 186, 224] {
        let formula = size / 32 + 2;
        assert!(
            !formula.is_multiple_of(2),
            "sanity: {size} was chosen because `size / 32 + 2` is odd there, got {formula}"
        );
        let table = Il2pParity::baseline_for_block(size).len();
        assert!(
            table.is_multiple_of(2),
            "a {size}-byte block must take an even, defined parity length, got {table}"
        );
        assert_ne!(table, formula, "block size {size}");
        undefined += 1;
    }
    assert_eq!(
        undefined, UNDEFINED_CASES,
        "the discrepancy case list shrank"
    );

    // Domain-wide laws: monotone non-decreasing, never as strong as max
    // FEC, never announcing the max-FEC header bit, and `correctable`
    // always half the parity.
    let mut previous = Il2pParity::baseline_for_block(0).len();
    for size in 0..=512usize {
        let parity = Il2pParity::baseline_for_block(size);
        assert!(
            parity.len() >= previous,
            "baseline parity fell at block size {size}"
        );
        assert!(parity.len() <= Il2pParity::Sixteen.len(), "{size}");
        assert!(
            !parity.is_max_fec(),
            "baseline must never set the max-FEC header bit ({size})"
        );
        assert_eq!(parity.correctable(), parity.len() / 2, "{size}");
        previous = parity.len();
    }

    // And the table is what the on-air arithmetic spends: for any
    // single-block payload the wire overhead is exactly the table's
    // answer for the whole payload.
    let mut single_block = 0usize;
    for len in 1..=MAX_BASELINE_BLOCK_DATA {
        assert_eq!(block_count_for(len, false), 1, "{len} should be one block");
        assert_eq!(
            payload_wire_len(len, false) - len,
            Il2pParity::baseline_for_block(len).len(),
            "single-block payload of {len} bytes"
        );
        single_block += 1;
    }
    assert_eq!(single_block, MAX_BASELINE_BLOCK_DATA);
}

/// `Fx25Frame::is_empty` — the `len`-companion clippy asks for.
///
/// It returns a constant `false`, and **that is the whole domain**:
/// `Fx25Frame` has private fields and only `wrap`/`wrap_with` construct
/// one, so the shortest transmission any caller can obtain is 8 tag
/// bytes plus a 32-byte codeblock. There is no `true` case to assert,
/// and inventing one would mean asserting something no caller can
/// observe.
///
/// The strongest available statement is therefore the *equivalence*
/// `is_empty() == (len() == 0)`, asserted over the entire constructible
/// domain: every published correlation tag, both the explicit and the
/// smallest-fit selection, across payload sizes that move `len()`. That
/// catches the failure that matters — `len()` learning to return 0 while
/// `is_empty()` keeps saying `false`.
#[cfg(feature = "fx25")]
#[test]
fn fx25_frame_is_empty_agrees_with_len_over_the_whole_domain() {
    use warble::fx25::{CorrelationTag, TAG_BYTES, WRAP_MAX, stuff_frame, wrap, wrap_with};

    /// Eleven tags × the explicit path, plus the smallest-fit path at
    /// several sizes. A floor, so the loops cannot pass over nothing.
    const MIN_CASES: usize = 11 + 4;

    let body = {
        let frame = UiFrame::new(
            Address::new(b"APRS", 0).unwrap(),
            Address::new(b"N0CALL", 7).unwrap(),
            b">fx",
        );
        let mut buf = [0u8; 330];
        let len = frame.build(&mut buf).unwrap();
        buf[..len].to_vec()
    };
    let mut stuffed = [0u8; 512];
    let stuffed_len = stuff_frame(&body, &mut stuffed).unwrap();
    assert!(
        stuffed_len <= 32,
        "the fixture must fit the smallest tag's 32 data bytes, got {stuffed_len}"
    );

    let mut checked = 0usize;
    let mut lengths = Vec::new();
    for tag in CorrelationTag::ALL {
        let mut out = [0u8; WRAP_MAX];
        let frame = wrap_with(tag, &stuffed[..stuffed_len], &mut out).unwrap();
        // Bound the length first: `is_empty() == (len() == 0)` written
        // inline would just be clippy's `len_zero` suggestion back.
        let len = frame.len();
        assert!(
            !frame.is_empty(),
            "{tag:?}: a wrapped transmission is never empty"
        );
        assert_eq!(
            frame.is_empty(),
            len == 0,
            "{tag:?}: is_empty and len disagree"
        );
        assert_eq!(len, TAG_BYTES + tag.block_len(), "{tag:?}");
        assert_eq!(frame.tag(), tag, "{tag:?}: explicit tag must be honored");
        lengths.push(len);
        checked += 1;
    }
    // The lengths really do vary, so the equivalence above was checked
    // against more than one value of `len()`.
    lengths.sort_unstable();
    lengths.dedup();
    assert!(
        lengths.len() >= 4,
        "expected several distinct transmission lengths, got {lengths:?}"
    );

    // The smallest-fit path, at sizes that select different tags.
    for info_len in [1usize, 40, 90, 150] {
        let info: Vec<u8> = core::iter::once(b'>')
            .chain((0..info_len).map(|i| b'a' + (i % 26) as u8))
            .collect();
        let frame = UiFrame::new(
            Address::new(b"APRS", 0).unwrap(),
            Address::new(b"N0CALL", 7).unwrap(),
            &info,
        );
        let mut buf = [0u8; 330];
        let len = frame.build(&mut buf).unwrap();
        let stuffed_len = stuff_frame(&buf[..len], &mut stuffed).unwrap();
        let mut out = [0u8; WRAP_MAX];
        let wrapped = wrap(&stuffed[..stuffed_len], &mut out).unwrap();
        let len = wrapped.len();
        assert!(!wrapped.is_empty(), "{info_len}-byte info");
        assert_eq!(wrapped.is_empty(), len == 0, "{info_len}");
        assert!(len >= TAG_BYTES + 32, "{info_len}");
        checked += 1;
    }
    assert!(
        checked >= MIN_CASES,
        "case list shrank to {checked}, below the {MIN_CASES} floor"
    );
}

// =====================================================================
// Reachable, but never asserted.
//
// A second mechanical audit (632 `pub fn` definitions in `src/`,
// excluding `src/bin/` and in-module `#[cfg(test)]` blocks, matched
// against all of `tests/`, `examples/`, `README.md`, every doc comment
// and every in-module test body, and requiring a *call* rather than a
// bare name mention) left exactly four public functions that nothing
// outside `src/` implementation code calls:
//
//   * `wav::check_spec`              (called from wav.rs:154 and :233)
//   * `wav::decode_frames`           (called from asynk/mod.rs:129)
//   * `ft8::llrs_from_energies`      (called from ft8/rx.rs:365)
//   * `ax25::fcs::locate_single_bit_error` (called from hdlc.rs:522)
//
// None is dead: each is reached indirectly on a path the suite covers.
// But "reached" is not "checked" — nothing had ever pinned what one of
// them *returns*, which left every rejection path, sign convention and
// edge case below unverified. The five tests that follow assert values
// (one per function, plus a second one for the WAV decode's failure
// half, which is a different set of claims from its success half).
// =====================================================================

/// Writes 16-bit samples to `path` with the given header.
#[cfg(feature = "wav")]
fn write_wav(path: &std::path::Path, spec: hound::WavSpec, samples: &[i16]) {
    let mut writer = hound::WavWriter::create(path, spec).expect("create the fixture WAV");
    for &s in samples {
        writer.write_sample(s).expect("write a sample");
    }
    writer.finalize().expect("finalize the fixture WAV");
}

/// A mono 16-bit integer PCM header at `hz`.
#[cfg(feature = "wav")]
fn mono_spec(hz: u32) -> hound::WavSpec {
    hound::WavSpec {
        channels: 1,
        sample_rate: hz,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    }
}

/// A scratch WAV path in the system temp directory, named after the
/// case.
///
/// Generated rather than read from the repository root's `beacon.wav`:
/// that file is `.gitignore`d (`*.wav`) and untracked, so a test that
/// gates CI cannot depend on it. The PID keeps two concurrent
/// `cargo test` runs from fighting over one path and enters no
/// assertion (the idiom of tests/asynk.rs and tests/cli.rs).
#[cfg(feature = "wav")]
fn scratch_wav(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "warble-coverage-fill-{}-{tag}.wav",
        std::process::id()
    ))
}

/// `wav::check_spec` — the header validator, at both edges of what it
/// accepts and on every rejection it can produce.
///
/// It is called from inside `src/wav.rs` only (`decode_frames` and
/// `sniff_pcm`), so the *accept* path rode along with every WAV test in
/// the suite while nothing had ever checked a rejection — and rejecting
/// is the entire job of a validator. Both error variants carry the
/// offending numbers, so each case below is matched to the variant *and*
/// its fields: a validator that rejects a stereo file while reporting
/// one channel is exactly as broken as one that accepts it.
#[cfg(feature = "wav")]
#[test]
fn wav_check_spec_accepts_16_bit_mono_pcm_and_names_every_rejection() {
    use hound::{SampleFormat, WavSpec};
    use warble::wav::{WavError, check_spec};

    /// Both edges of the supported `8_000..=48_000` Hz range plus the
    /// four rates the crate's own fixtures use, out of that continuum.
    /// The array is typed at this length, so shrinking it is a compile
    /// error rather than a loop that passes having checked less.
    const ACCEPT_CASES: usize = 6;
    /// Every distinct rejection the function can produce: three channel
    /// counts, three bit depths, two float formats, five rates, and one
    /// header that fails both tests at once.
    const REJECT_CASES: usize = 14;

    let spec = |channels: u16, sample_rate: u32, bits_per_sample: u16, sample_format| WavSpec {
        channels,
        sample_rate,
        bits_per_sample,
        sample_format,
    };

    let accepted: [u32; ACCEPT_CASES] = [8_000, 11_025, 12_000, 22_050, 44_100, 48_000];
    let mut accepts = 0usize;
    for hz in accepted {
        let rate = check_spec(&spec(1, hz, 16, SampleFormat::Int))
            .unwrap_or_else(|e| panic!("{hz} Hz mono 16-bit integer PCM rejected: {e}"));
        // The *validated rate* is the header's rate, not a default: a
        // validator that returned some other supported rate would still
        // return `Ok`, and every later stage would run at the wrong
        // baud.
        assert_eq!(rate, SampleRate::new(hz).unwrap(), "{hz} Hz");
        assert_eq!(rate.hz(), hz, "{hz} Hz: wrong rate returned");
        accepts += 1;
    }
    assert!(
        accepts >= ACCEPT_CASES,
        "accept list shrank to {accepts}, below the {ACCEPT_CASES} floor"
    );

    /// What a rejection has to be, field for field.
    enum Reject {
        Format {
            channels: u16,
            bits: u16,
            float: bool,
        },
        Rate {
            hz: u32,
        },
    }

    let rejected: [(&str, WavSpec, Reject); REJECT_CASES] = [
        (
            "zero channels",
            spec(0, 48_000, 16, SampleFormat::Int),
            Reject::Format {
                channels: 0,
                bits: 16,
                float: false,
            },
        ),
        (
            "stereo",
            spec(2, 48_000, 16, SampleFormat::Int),
            Reject::Format {
                channels: 2,
                bits: 16,
                float: false,
            },
        ),
        (
            "eight channels",
            spec(8, 48_000, 16, SampleFormat::Int),
            Reject::Format {
                channels: 8,
                bits: 16,
                float: false,
            },
        ),
        (
            "8-bit",
            spec(1, 48_000, 8, SampleFormat::Int),
            Reject::Format {
                channels: 1,
                bits: 8,
                float: false,
            },
        ),
        (
            "24-bit",
            spec(1, 48_000, 24, SampleFormat::Int),
            Reject::Format {
                channels: 1,
                bits: 24,
                float: false,
            },
        ),
        (
            "32-bit integer",
            spec(1, 48_000, 32, SampleFormat::Int),
            Reject::Format {
                channels: 1,
                bits: 32,
                float: false,
            },
        ),
        (
            "32-bit float",
            spec(1, 48_000, 32, SampleFormat::Float),
            Reject::Format {
                channels: 1,
                bits: 32,
                float: true,
            },
        ),
        // 16-bit *float* is the case that proves the reported `float`
        // flag comes from the sample format and not from the bit depth:
        // the depth is the accepted one, the format is not.
        (
            "16-bit float",
            spec(1, 48_000, 16, SampleFormat::Float),
            Reject::Format {
                channels: 1,
                bits: 16,
                float: true,
            },
        ),
        (
            "0 Hz",
            spec(1, 0, 16, SampleFormat::Int),
            Reject::Rate { hz: 0 },
        ),
        (
            "one Hz below the minimum",
            spec(1, 7_999, 16, SampleFormat::Int),
            Reject::Rate { hz: 7_999 },
        ),
        (
            "one Hz above the maximum",
            spec(1, 48_001, 16, SampleFormat::Int),
            Reject::Rate { hz: 48_001 },
        ),
        (
            "96 kHz",
            spec(1, 96_000, 16, SampleFormat::Int),
            Reject::Rate { hz: 96_000 },
        ),
        (
            "192 kHz",
            spec(1, 192_000, 16, SampleFormat::Int),
            Reject::Rate { hz: 192_000 },
        ),
        // Both wrong at once. The format test runs first, so the format
        // error is what comes back — a caller that switches on the
        // variant to tell the user what to fix depends on that order.
        (
            "stereo at 96 kHz",
            spec(2, 96_000, 16, SampleFormat::Int),
            Reject::Format {
                channels: 2,
                bits: 16,
                float: false,
            },
        ),
    ];

    let mut rejects = 0usize;
    for (label, header, want) in rejected {
        let err = match check_spec(&header) {
            Ok(rate) => panic!("{label}: accepted, reporting {} Hz", rate.hz()),
            Err(e) => e,
        };
        match (want, &err) {
            (
                Reject::Format {
                    channels,
                    bits,
                    float,
                },
                WavError::UnsupportedFormat {
                    channels: got_channels,
                    bits_per_sample: got_bits,
                    float: got_float,
                },
            ) => assert_eq!(
                (*got_channels, *got_bits, *got_float),
                (channels, bits, float),
                "{label}: UnsupportedFormat carries the wrong numbers ({err})"
            ),
            (Reject::Rate { hz }, WavError::UnsupportedRate { hz: got_hz }) => assert_eq!(
                *got_hz, hz,
                "{label}: UnsupportedRate carries the wrong rate ({err})"
            ),
            (_, other) => panic!("{label}: wrong error variant: {other:?}"),
        }
        rejects += 1;
    }
    assert!(
        rejects >= REJECT_CASES,
        "reject list shrank to {rejects}, below the {REJECT_CASES} floor"
    );

    // The messages carry the numbers a user needs in order to convert
    // the file, which is the only reason the variants have fields.
    let format = check_spec(&spec(2, 96_000, 32, SampleFormat::Float))
        .expect_err("stereo 32-bit float is not acceptable")
        .to_string();
    assert!(
        format.contains("2 channel(s), 32 bits, float samples"),
        "{format}"
    );
    assert!(
        format.contains("16-bit mono integer PCM is required"),
        "{format}"
    );
    let rate = check_spec(&spec(1, 96_000, 16, SampleFormat::Int))
        .expect_err("96 kHz is out of range")
        .to_string();
    assert!(rate.contains("96000 Hz"), "{rate}");
    assert!(rate.contains("supported: 8000..=48000 Hz"), "{rate}");
}

/// `wav::decode_frames` — the public whole-file WAV decode.
///
/// The async layer drives it (`asynk::decode_wav`), which is how it is
/// reached today, but no test had ever called it or looked at what it
/// returns. Four claims are asserted here, all of them documented and
/// none of them previously checked:
///
/// 1. it recovers the frames the file holds, field for field — and the
///    same ones the sync `DefaultTncReceiver` path recovers from the
///    same samples, which is what "decodes through a Bell 202 receiver"
///    means;
/// 2. the receiver runs at **the file's** sample rate, proven by
///    decoding the same two-frame fixture from a 48 kHz *and* a
///    22.05 kHz WAV — a hardcoded rate would decode one of them to
///    nothing;
/// 3. the returned [`warble::tnc::TncStats`] are the receiver's real
///    counters (all four fields pinned, not just `frames_ok`);
/// 4. the sink contract: returning `false` stops delivery *and*
///    decoding, so the sink is never called again and the statistics
///    stop where the sink stopped.
#[cfg(feature = "wav")]
#[test]
fn wav_decode_frames_recovers_frames_reports_stats_and_honors_the_sink() {
    use warble::tnc::{DefaultTncReceiver, OwnedFrame, TncConfig, TncStats, TncTransmitter};
    use warble::wav::decode_frames;

    /// Sample rates the fixture is built and decoded at. Typed at this
    /// length so dropping one is a compile error: with a single rate,
    /// claim 2 above would be untested.
    const RATE_CASES: usize = 2;
    const RATES: [u32; RATE_CASES] = [48_000, 22_050];

    let dest = Address::new(b"APRS", 0).unwrap();
    let first = Address::new(b"W1AW", 0).unwrap();
    let second = Address::new(b"K2XYZ", 7).unwrap();

    let mut checked = 0usize;
    for hz in RATES {
        let config = TncConfig::bell_202(SampleRate::new(hz).unwrap()).unwrap();
        let tx = TncTransmitter::new(config);
        let mut samples = tx
            .transmit_to_vec_i16(
                &AprsPacket::Status(Status {
                    text: b"wav decode one",
                }),
                dest,
                first,
                &[],
            )
            .unwrap();
        samples.extend(std::iter::repeat_n(0i16, 2_000));
        samples.extend(
            tx.transmit_to_vec_i16(
                &AprsPacket::Status(Status {
                    text: b"wav decode two",
                }),
                dest,
                second,
                &[],
            )
            .unwrap(),
        );

        // The yardstick: the same samples through the sync receiver the
        // documentation says `decode_frames` builds.
        let mut rx = DefaultTncReceiver::new(config).unwrap();
        let mut expected = Vec::new();
        for &s in &samples {
            if let Some(frame) = rx.push_i16(s) {
                expected.push(OwnedFrame::new(&frame).unwrap());
            }
        }
        let expected_stats = rx.stats();
        assert_eq!(expected.len(), 2, "{hz} Hz fixture must hold two frames");

        let path = scratch_wav(&format!("decode-frames-{hz}"));
        write_wav(&path, mono_spec(hz), &samples);

        let mut got = Vec::new();
        let stats = decode_frames(&path, |frame| {
            got.push(frame);
            true
        })
        .unwrap_or_else(|e| panic!("{hz} Hz: decode_frames failed: {e}"));

        // Same file, a sink that refuses the first frame.
        let mut calls = 0usize;
        let stopped = decode_frames(&path, |_| {
            calls += 1;
            false
        })
        .unwrap_or_else(|e| panic!("{hz} Hz: decode_frames failed: {e}"));
        let _ = std::fs::remove_file(&path);

        assert_eq!(got.len(), 2, "{hz} Hz: both frames must be delivered");
        assert_eq!(got[0].src(), first, "{hz} Hz: first frame's source");
        assert_eq!(got[0].dest(), dest, "{hz} Hz: first frame's destination");
        assert_eq!(got[0].info(), b">wav decode one", "{hz} Hz");
        assert!(got[0].hops().is_empty(), "{hz} Hz: no digipeater path");
        assert_eq!(got[1].src(), second, "{hz} Hz: second frame's source");
        assert_eq!(got[1].info(), b">wav decode two", "{hz} Hz");
        assert_eq!(
            got, expected,
            "{hz} Hz: the WAV path must recover exactly what the sync receiver does"
        );

        assert_eq!(
            stats,
            TncStats {
                frames_ok: 2,
                fcs_errors: 0,
                oversize: 0,
                malformed: 0,
            },
            "{hz} Hz: wrong statistics"
        );
        assert_eq!(
            stats, expected_stats,
            "{hz} Hz: the statistics must be the receiver's own"
        );

        assert_eq!(
            calls, 1,
            "{hz} Hz: a sink returning false must not be called again"
        );
        assert_eq!(
            stopped,
            TncStats {
                frames_ok: 1,
                ..TncStats::default()
            },
            "{hz} Hz: the remaining samples must be skipped, not decoded"
        );
        checked += 1;
    }
    assert!(
        checked >= RATE_CASES,
        "rate list shrank to {checked}, below the {RATE_CASES} floor"
    );
}

/// `wav::decode_frames` — the failure half of the same entry point.
///
/// A whole-file decode can fail before it ever reaches a sample, and
/// the doc comment promises which typed error each way out produces.
/// All three are asserted, together with the claim a caller depends on
/// most: when the call fails, the sink is never invoked, so no partial
/// or bogus frame is delivered alongside the error.
#[cfg(feature = "wav")]
#[test]
fn wav_decode_frames_surfaces_open_and_header_failures() {
    use warble::wav::{WavError, decode_frames};

    /// One IO failure plus the two `check_spec` rejections that a real
    /// file can carry into `decode_frames`. Typed arrays below hold the
    /// header cases, so the list cannot shrink silently.
    const MIN_CASES: usize = 3;
    const HEADER_CASES: usize = 2;

    let mut cases = 0usize;

    // 1. A path that is not there: hound's IO error, surfaced as
    //    `WavError::Wav`, and not a panic.
    let missing = scratch_wav("decode-frames-missing");
    let _ = std::fs::remove_file(&missing);
    let mut calls = 0usize;
    let err = decode_frames(&missing, |_| {
        calls += 1;
        true
    })
    .expect_err("a missing file cannot decode");
    match err {
        WavError::Wav(hound::Error::IoError(io)) => assert_eq!(
            io.kind(),
            std::io::ErrorKind::NotFound,
            "wrong IO error kind for a missing file"
        ),
        other => panic!("expected WavError::Wav(IoError), got {other:?}"),
    }
    assert_eq!(calls, 0, "nothing can be delivered from a missing file");
    cases += 1;

    // 2/3. Headers `check_spec` rejects, reaching it through a real
    //      file: the rejection must survive the trip out of
    //      `decode_frames` with its fields intact.
    let stereo = hound::WavSpec {
        channels: 2,
        sample_rate: 48_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let headers: [(&str, hound::WavSpec); HEADER_CASES] =
        [("stereo", stereo), ("96 kHz", mono_spec(96_000))];
    for (tag, spec) in headers {
        let path = scratch_wav(&format!("decode-frames-{}", tag.replace(' ', "-")));
        write_wav(&path, spec, &[0i16; 64]);
        let mut calls = 0usize;
        let err = decode_frames(&path, |_| {
            calls += 1;
            true
        })
        .expect_err("a header the modem cannot accept must not decode");
        let _ = std::fs::remove_file(&path);
        match err {
            WavError::UnsupportedFormat {
                channels,
                bits_per_sample,
                float,
            } => {
                assert_eq!(tag, "stereo", "{tag}: unexpected format rejection");
                assert_eq!((channels, bits_per_sample, float), (2, 16, false), "{tag}");
            }
            WavError::UnsupportedRate { hz } => {
                assert_eq!(tag, "96 kHz", "{tag}: unexpected rate rejection");
                assert_eq!(hz, 96_000, "{tag}");
            }
            other => panic!("{tag}: wrong error variant: {other:?}"),
        }
        assert_eq!(calls, 0, "{tag}: a rejected header delivers no frames");
        cases += 1;
    }
    assert!(
        cases >= MIN_CASES,
        "case list shrank to {cases}, below the {MIN_CASES} floor"
    );
}

/// `ft8::llrs_from_energies` — the max-log Gray demapper, against
/// hand-computed answers.
///
/// This is the one stage of the FT8 receiver whose errors a round trip
/// cannot see. Flip the sign convention, or transpose two entries of
/// the inverse Gray map, and encode → demap → decode still agrees with
/// itself — while every real capture decodes to noise. So every
/// assertion below is a known answer.
///
/// The arithmetic is exact, so the expectations are `assert_eq!` on f32
/// rather than tolerances: when all 58 symbols carry the same total
/// energy `S`, the mean the function normalizes by is
/// `58·S / (58·8) = S/8`, so a symbol holding 1.0 on one tone and
/// nothing elsewhere yields `±(1.0 − 0.0) / 0.125 = ±8.0` exactly.
///
/// Sign convention, quoted from the function's own doc comment:
/// **positive LLR means the bit is more likely 0** (`llr =
/// max(energies whose bit is 0) − max(energies whose bit is 1)`), which
/// is also what `ldpc_decode` expects — case G closes that loop.
#[cfg(feature = "ft8")]
#[test]
fn ft8_llrs_from_energies_known_answers() {
    use warble::ft8::{
        CODEWORD_BITS, CODEWORD_LEN, Ft8Message, Ft8Tail, GRAY_MAP, add_crc, ldpc_decode,
        ldpc_encode, llrs_from_energies, symbols_from_codeword,
    };

    /// Tone → 3-bit group: the inverse of [`GRAY_MAP`], written out as a
    /// literal so this test *states* the answer instead of recomputing
    /// the implementation's inversion and agreeing with itself.
    const INV_GRAY: [u8; 8] = [0, 1, 3, 2, 6, 4, 5, 7];
    /// Expected LLR triple (MSB of the symbol's 3-bit group first) for a
    /// symbol whose entire energy sits on tone `t`, in an array of 58
    /// such symbols: `+8.0` where the demapped bit is 0, `-8.0` where it
    /// is 1.
    const ONE_HOT_LLRS: [[f32; 3]; 8] = [
        [8.0, 8.0, 8.0],    // tone 0 -> bits 000
        [8.0, 8.0, -8.0],   // tone 1 -> bits 001
        [8.0, -8.0, -8.0],  // tone 2 -> bits 011
        [8.0, -8.0, 8.0],   // tone 3 -> bits 010
        [-8.0, -8.0, 8.0],  // tone 4 -> bits 110
        [-8.0, 8.0, 8.0],   // tone 5 -> bits 100
        [-8.0, 8.0, -8.0],  // tone 6 -> bits 101
        [-8.0, -8.0, -8.0], // tone 7 -> bits 111
    ];
    /// Every codeword bit position is asserted individually, out of the
    /// 174 [`CODEWORD_BITS`] the function returns.
    const MIN_POSITIONS: usize = 174;
    /// All eight tones must appear in the sweep, out of the 8-FSK
    /// alphabet: a Gray-map transposition between two tones that never
    /// occur is invisible.
    const MIN_TONES: u8 = 8;
    /// The data symbol whose energies are made ambiguous in case D.
    const AMBIGUOUS: usize = 29;

    assert_eq!(CODEWORD_BITS, MIN_POSITIONS);
    assert_eq!(CODEWORD_BITS, 3 * 58, "three bits per 8-FSK data symbol");

    // The literal tables above are self-consistent with the published
    // Gray map: INV_GRAY inverts GRAY_MAP, and the sign pattern of
    // ONE_HOT_LLRS is that map read MSB-first with + meaning bit 0.
    for (tone, (&bits, expect)) in INV_GRAY.iter().zip(ONE_HOT_LLRS.iter()).enumerate() {
        assert_eq!(
            GRAY_MAP[usize::from(bits)],
            u8::try_from(tone).unwrap(),
            "INV_GRAY is not GRAY_MAP's inverse at tone {tone}"
        );
        for (b, &llr) in expect.iter().enumerate() {
            let bit = (bits >> (2 - b)) & 1;
            assert_eq!(llr > 0.0, bit == 0, "tone {tone} bit {b}");
            assert_eq!(llr.abs(), 8.0, "tone {tone} bit {b}");
        }
    }

    // --- Case A: one confident tone per symbol, all eight tones ------
    let mut energies = [[0.0f32; 8]; 58];
    for (j, symbol) in energies.iter_mut().enumerate() {
        symbol[j % 8] = 1.0;
    }
    let llr = llrs_from_energies(&energies);
    assert_eq!(llr.len(), CODEWORD_BITS, "one LLR per codeword bit");
    let mut positions = 0usize;
    let mut tones_seen = 0u8;
    for j in 0..58 {
        let tone = j % 8;
        tones_seen |= 1 << tone;
        for (b, &want) in ONE_HOT_LLRS[tone].iter().enumerate() {
            // Position, sign and magnitude at once: this is what pins
            // the three bits of symbol j to codeword bits 3j..3j+3.
            assert_eq!(
                llr[3 * j + b],
                want,
                "symbol {j} (tone {tone}) bit {b}: bits {:03b}",
                INV_GRAY[tone]
            );
            positions += 1;
        }
    }
    assert_eq!(
        tones_seen.count_ones(),
        u32::from(MIN_TONES),
        "the sweep must cover all {MIN_TONES} tones, covered {tones_seen:#010b}"
    );
    assert!(
        positions >= MIN_POSITIONS,
        "the sweep shrank to {positions} positions, below the {MIN_POSITIONS} floor"
    );

    // --- Case B: a confidently-zero and a confidently-one symbol -----
    //
    // Tone 0 demaps to bits 000 and tone 7 to bits 111, so 58 of either
    // pins the sign convention on its own: all-positive or all-negative,
    // with nothing in between to average away a flipped sign.
    let mut zeros = [[0.0f32; 8]; 58];
    for symbol in &mut zeros {
        symbol[0] = 1.0;
    }
    let zero_llrs = llrs_from_energies(&zeros);
    assert_eq!(
        zero_llrs.iter().position(|&v| v != 8.0),
        None,
        "a confident tone 0 (bits 000) must give +8.0 at every bit: positive means 0"
    );
    let mut ones = [[0.0f32; 8]; 58];
    for symbol in &mut ones {
        symbol[7] = 1.0;
    }
    let one_llrs = llrs_from_energies(&ones);
    assert_eq!(
        one_llrs.iter().position(|&v| v != -8.0),
        None,
        "a confident tone 7 (bits 111) must give -8.0 at every bit: negative means 1"
    );

    // --- Case C: ambiguity decides nothing ---------------------------
    let flat = [[1.0f32; 8]; 58];
    let flat_llrs = llrs_from_energies(&flat);
    assert_eq!(
        flat_llrs.iter().position(|&v| v != 0.0),
        None,
        "eight equal tone energies carry no information: every LLR must be exactly 0"
    );
    // The degenerate silent capture: the mean is clamped away from zero,
    // so the output is zeros rather than NaNs poisoning the decoder.
    let silent = [[0.0f32; 8]; 58];
    let silent_llrs = llrs_from_energies(&silent);
    assert_eq!(
        silent_llrs.iter().position(|&v| v != 0.0 || !v.is_finite()),
        None,
        "an all-zero capture must give finite zeros, not NaN"
    );

    // --- Case D: ambiguity is per symbol, not global -----------------
    //
    // Symbol 29 spreads the same total energy over all eight tones, so
    // the mean stays 1/8: the other 57 symbols keep their exact ±8.0
    // while the ambiguous one contributes three zeros in its own three
    // codeword positions and nowhere else.
    let mut mixed = [[0.0f32; 8]; 58];
    for (j, symbol) in mixed.iter_mut().enumerate() {
        if j == AMBIGUOUS {
            *symbol = [0.125; 8];
        } else {
            symbol[5] = 1.0;
        }
    }
    let mixed_llrs = llrs_from_energies(&mixed);
    for j in 0..58 {
        for (b, &want) in ONE_HOT_LLRS[5].iter().enumerate() {
            let got = mixed_llrs[3 * j + b];
            if j == AMBIGUOUS {
                assert_eq!(got, 0.0, "ambiguous symbol {j} bit {b} must be 0");
            } else {
                assert_eq!(got, want, "confident symbol {j} (tone 5) bit {b}");
            }
        }
    }

    // --- Case E: max-log is a max over the tone *class* --------------
    //
    // 1.5 on tone 1 and 0.5 on tone 3 (total 2.0 per symbol, mean 0.25):
    //   bit 0 splits tones {0,1,2,3} from {4,5,6,7}: (1.5-0.0)/0.25 = +6
    //   bit 1 splits {0,1,5,6} from {2,3,4,7}:       (1.5-0.5)/0.25 = +4
    //   bit 2 splits {0,3,4,5} from {1,2,6,7}:       (0.5-1.5)/0.25 = -4
    // The runner-up tone decides bits 1 and 2, so an implementation that
    // only looked at the peak tone (or normalized by the peak instead of
    // the mean) cannot produce these three numbers.
    const GRADED_LLRS: [f32; 3] = [6.0, 4.0, -4.0];
    let mut graded = [[0.0f32; 8]; 58];
    for symbol in &mut graded {
        symbol[1] = 1.5;
        symbol[3] = 0.5;
    }
    let graded_llrs = llrs_from_energies(&graded);
    for j in 0..58 {
        for (b, &want) in GRADED_LLRS.iter().enumerate() {
            assert_eq!(graded_llrs[3 * j + b], want, "graded symbol {j} bit {b}");
        }
    }

    // --- Case F: the normalization really is capture-independent -----
    let mut louder = energies;
    for symbol in &mut louder {
        for v in symbol.iter_mut() {
            *v *= 4.0;
        }
    }
    assert_eq!(
        llrs_from_energies(&louder),
        llr,
        "a four-times louder capture must give identical LLRs"
    );

    // --- Case G: this is the convention ldpc_decode expects ----------
    //
    // A real codeword, mapped to tones the way the transmitter does,
    // handed back as noiseless energies: the hard decisions on the LLRs
    // must reproduce the codeword bit for bit (a global sign error would
    // reproduce its complement), and the decoder must agree.
    let payload = Ft8Message::standard("CQ", "K1ABC", false, Ft8Tail::grid("FN42").unwrap())
        .unwrap()
        .payload();
    let codeword = ldpc_encode(&add_crc(&payload));
    let symbols = symbols_from_codeword(&codeword);
    let mut clean = [[0.0f32; 8]; 58];
    for (j, symbol) in clean.iter_mut().enumerate() {
        // The data symbols, Costas blocks excluded (as in ft8/rx.rs).
        let position = if j < 29 { 7 + j } else { 43 + (j - 29) };
        symbol[usize::from(symbols[position])] = 1.0;
    }
    let clean_llrs = llrs_from_energies(&clean);
    let mut hard = [0u8; CODEWORD_LEN];
    for pos in 0..CODEWORD_BITS {
        // Negative LLR means bit 1.
        hard[pos / 8] |= u8::from(clean_llrs[pos] < 0.0) << (7 - pos % 8);
    }
    assert_eq!(
        hard, codeword,
        "hard decisions on the LLRs must reproduce the transmitted codeword"
    );
    assert_eq!(
        ldpc_decode(&clean_llrs).expect("noiseless LLRs must decode"),
        codeword,
        "the LLR sign convention must be the one ldpc_decode reads"
    );
}

/// `ax25::fcs::locate_single_bit_error` — the CRC-syndrome locator,
/// swept over its whole reachable domain.
///
/// Pure arithmetic with a wide domain, reached only from
/// `HdlcDeframer`'s single-bit repair policy, and asserted nowhere: the
/// recovery tests observe the *repaired frame*, which leaves "did it
/// name the right bit" implicit — a locator that named the wrong bit
/// would simply fail to repair, and be indistinguishable from a locator
/// that found nothing.
///
/// So the positive direction is swept directly: for eight content
/// lengths up to the receiver's 330-byte `MAX_FRAME_BYTES`, flip every
/// bit of every byte in turn, compute the real syndrome with
/// [`warble::ax25::fcs::crc16_x25`], and demand the function name
/// exactly that position — then apply the `(index, mask)` it reported
/// and demand the frame and its FCS come back. Because every one of the
/// 5416 flips gets its own position back, the sweep also proves the
/// syndromes are distinct over that domain: the documented "returns the
/// first match" is unambiguous, because the first match is the only one.
///
/// The negative direction is where the real contract lives:
///
/// * a zero syndrome (the FCS already matches) is `None`, never a
///   location;
/// * a **two-bit** error is `None` — never a misreported single-bit
///   location. That is not luck: CRC-16/X.25 keeps Hamming distance 4
///   at these lengths, so no weight-3 error pattern is a codeword, and a
///   two-bit syndrome therefore cannot equal any single-bit syndrome
///   (nor have population count 1, which the `InFcs` shortcut would
///   report). Asserted exhaustively over every pair of the 80 and 144
///   bit positions — content bits *and* FCS bits — of an 8- and a
///   16-byte frame, then over sampled pairs at 63, 255 and 330 bytes.
///
/// Out of the reachable domain the guarantee does lapse, which is worth
/// recording: at 4095 content bytes (32760 bits, past this CRC's
/// 32751-bit HD=4 bound) single-bit syndromes begin to alias and
/// population-count-1 syndromes appear, so a content flip there would be
/// reported as `InFcs`. No AX.25 frame can be that long — the deframer
/// caps a frame at 330 bytes — so this test sweeps the reachable domain
/// and does not pin behaviour no caller can observe.
#[cfg(feature = "ax25")]
#[test]
fn ax25_locate_single_bit_error_names_every_flipped_bit() {
    use warble::ax25::fcs::{SingleBitError, crc16_x25, locate_single_bit_error};

    /// Content lengths swept: the shortest frame, a few small ones, and
    /// the receiver's `MAX_FRAME_BYTES` ceiling. The array is typed at
    /// this length, so shrinking the list is a compile error.
    const MIN_LENGTHS: usize = 8;
    const LENGTHS: [usize; MIN_LENGTHS] = [1, 2, 3, 7, 16, 63, 255, 330];
    /// Single-bit content flips: 8 per byte over every length above.
    const MIN_CONTENT_CASES: usize = 8 * (1 + 2 + 3 + 7 + 16 + 63 + 255 + 330);
    /// Single-bit FCS-field flips: the 16 FCS bits at every length,
    /// plus the empty-content frame.
    const MIN_FCS_CASES: usize = 16 * (MIN_LENGTHS + 1);
    /// Sampled two-bit errors per long length (exhaustive would be
    /// millions of pairs at 330 bytes).
    const SAMPLED_PAIRS: usize = 1_000;
    /// Two-bit errors: every pair of the 80 bit positions of an 8-byte
    /// frame and of the 144 of a 16-byte frame, plus the sampled pairs
    /// at three longer lengths.
    const MIN_PAIR_CASES: usize = (80 * 79) / 2 + (144 * 143) / 2 + 3 * SAMPLED_PAIRS;
    /// Frame contents come from this seed; every failure message repeats
    /// it, so any case reproduces exactly.
    const SEED: u64 = 0xA905_2024_0FC5;

    /// 64-bit LCG (MMIX constants), the tests/fuzz_decode.rs idiom.
    struct Lcg(u64);

    impl Lcg {
        fn next_u64(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0
        }

        /// One pseudo-random byte (upper bits: LCG low bits are weak).
        fn next_u8(&mut self) -> u8 {
            (self.next_u64() >> 56) as u8
        }

        /// Uniform-ish in `0..bound` (bound > 0).
        fn below(&mut self, bound: usize) -> usize {
            (self.next_u64() >> 33) as usize % bound
        }
    }

    // Deterministic frame contents of the given length.
    let content_of = |len: usize| -> Vec<u8> {
        let mut rng = Lcg(SEED ^ len as u64);
        (0..len).map(|_| rng.next_u8()).collect()
    };

    // Flips bits at `positions` — `0..8*len` are content bits (bit
    // `p % 8` of byte `p / 8`), `8*len..8*len+16` the transmitted FCS
    // field — and returns the `(expected, computed)` pair a receiver
    // would hand the locator: the FCS carried by the frame and the FCS
    // computed over the received contents.
    let corrupt = |content: &[u8], positions: &[usize]| -> (u16, u16) {
        let content_bits = 8 * content.len();
        let mut bytes = content.to_vec();
        let mut carried = crc16_x25(content);
        for &p in positions {
            if p < content_bits {
                bytes[p / 8] ^= 1u8 << (p % 8);
            } else {
                carried ^= 1u16 << (p - content_bits);
            }
        }
        (carried, crc16_x25(&bytes))
    };

    let mut content_cases = 0usize;
    let mut fcs_cases = 0usize;
    for len in LENGTHS {
        let content = content_of(len);
        let expected = crc16_x25(&content);

        // A frame whose FCS already matches is not a single-bit error.
        assert_eq!(
            locate_single_bit_error(len, expected, expected),
            None,
            "len {len}: a matching FCS must not be reported as a flip (seed {SEED:#x})"
        );

        for j in 0..len {
            for k in 0..8u32 {
                let mask = 1u8 << k;
                let mut received = content.clone();
                received[j] ^= mask;
                let computed = crc16_x25(&received);
                assert_ne!(
                    computed, expected,
                    "len {len} byte {j} bit {k}: the CRC did not notice a single-bit \
                     flip (seed {SEED:#x})"
                );
                let found = locate_single_bit_error(len, expected, computed);
                assert_eq!(
                    found,
                    Some(SingleBitError::InContent { index: j, mask }),
                    "len {len} byte {j} bit {k} (seed {SEED:#x})"
                );
                // Apply the repair the caller was told to apply — the
                // reported values, not the known ones.
                let Some(SingleBitError::InContent {
                    index,
                    mask: repair,
                }) = found
                else {
                    unreachable!("asserted immediately above");
                };
                received[index] ^= repair;
                assert_eq!(
                    crc16_x25(&received),
                    expected,
                    "len {len} byte {j} bit {k}: the reported repair did not restore \
                     the FCS (seed {SEED:#x})"
                );
                assert_eq!(
                    received, content,
                    "len {len} byte {j} bit {k}: the reported repair did not restore \
                     the frame (seed {SEED:#x})"
                );
                content_cases += 1;
            }
        }

        // A flip in the transmitted FCS field: the contents are intact,
        // so the answer is `InFcs` and no content byte is named.
        for b in 0..16u32 {
            assert_eq!(
                locate_single_bit_error(len, expected ^ (1u16 << b), expected),
                Some(SingleBitError::InFcs),
                "len {len}: a flip in FCS bit {b} must be reported as InFcs (seed {SEED:#x})"
            );
            fcs_cases += 1;
        }
    }

    // A frame with no contents at all: only the FCS field can hold a
    // flip, and a syndrome no single bit explains is `None`.
    let empty = crc16_x25(&[]);
    for b in 0..16u32 {
        assert_eq!(
            locate_single_bit_error(0, empty ^ (1u16 << b), empty),
            Some(SingleBitError::InFcs),
            "empty frame: a flip in FCS bit {b} must be reported as InFcs"
        );
        fcs_cases += 1;
    }
    assert_eq!(
        locate_single_bit_error(0, empty ^ 0b11, empty),
        None,
        "empty frame: no content byte exists to carry a two-bit syndrome"
    );

    // Two-bit errors, exhaustively over every pair of bit positions of
    // a short frame (content bits and FCS bits alike).
    let mut pair_cases = 0usize;
    for len in [8usize, 16] {
        let content = content_of(len);
        let positions = 8 * len + 16;
        for a in 0..positions {
            for b in (a + 1)..positions {
                let (expected, computed) = corrupt(&content, &[a, b]);
                assert_eq!(
                    locate_single_bit_error(len, expected, computed),
                    None,
                    "len {len}: the two-bit error at bits {a},{b} was reported as a \
                     single-bit flip (seed {SEED:#x})"
                );
                pair_cases += 1;
            }
        }
    }
    // And sampled pairs at the lengths where exhaustive is out of reach.
    for len in [63usize, 255, 330] {
        let content = content_of(len);
        let positions = 8 * len + 16;
        let mut rng = Lcg(SEED ^ 0xBEEF ^ len as u64);
        for case in 0..SAMPLED_PAIRS {
            let a = rng.below(positions);
            let b = {
                let pick = rng.below(positions);
                if pick == a {
                    (pick + 1) % positions
                } else {
                    pick
                }
            };
            let (expected, computed) = corrupt(&content, &[a, b]);
            assert_eq!(
                locate_single_bit_error(len, expected, computed),
                None,
                "len {len} case {case}: the two-bit error at bits {a},{b} was reported \
                 as a single-bit flip (seed {SEED:#x})"
            );
            pair_cases += 1;
        }
    }

    assert!(
        content_cases >= MIN_CONTENT_CASES,
        "the content sweep shrank to {content_cases}, below the {MIN_CONTENT_CASES} floor"
    );
    assert!(
        fcs_cases >= MIN_FCS_CASES,
        "the FCS sweep shrank to {fcs_cases}, below the {MIN_FCS_CASES} floor"
    );
    assert!(
        pair_cases >= MIN_PAIR_CASES,
        "the two-bit sweep shrank to {pair_cases}, below the {MIN_PAIR_CASES} floor"
    );
}
