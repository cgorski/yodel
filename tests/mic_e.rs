//! Integration tests for the Mic-E encoder/decoder (`micE` feature).
#![cfg(feature = "micE")]

use warble::aprs::mic_e::{self, MicE, MicEError, MicEFix, MicEMessage};
use warble::aprs::{Ambiguity, Latitude, Longitude, Symbol};

/// A coordinate magnitude in 1/100 arc-minutes, the unit every fixture
/// in this file is written in. The storage unit is finer, so this
/// rounds; anything asserting the finer value says so explicitly.
fn hundredths(units: i64) -> i64 {
    let step = warble::geo::UNITS_PER_HUNDREDTH_MINUTE;
    let half = if units < 0 { -step / 2 } else { step / 2 };
    (units + half) / step
}

/// Convenience constructor with quiet defaults.
/// A report from 1/100 arc-minutes, the unit every fixture here is
/// written in and the unit Mic-E carries on the wire.
fn report(lat: i64, lon: i64) -> MicE<'static> {
    MicE {
        latitude: Latitude::new(lat * warble::geo::UNITS_PER_HUNDREDTH_MINUTE).unwrap(),
        longitude: Longitude::new(lon * warble::geo::UNITS_PER_HUNDREDTH_MINUTE).unwrap(),
        speed: 0,
        course: 0,
        symbol: Symbol::CAR,
        message: MicEMessage::OffDuty,
        fix: MicEFix::Current,
        altitude: None,
        device_prefix: None,
        ambiguity: 0,
        status: b"",
    }
}

/// Hand-derived vector per APRS 1.01 chapter 10 formulas.
///
/// Position 33 deg 25.64 min N, 112 deg 07.74 min W; 20 knots, course
/// 251; symbol `/j`; standard message bits 100 (Returning).
///
/// Destination: lat digits 3 3 2 5 6 4. Bits A/B/C = 1/0/0 so column 1
/// is `P`+3 = `S`, columns 2-3 stay digits `3` `2`. Column 4 is north:
/// `P`+5 = `U`. Longitude 112 needs the +100 offset: column 5 is
/// `P`+6 = `V`. West: column 6 is `P`+4 = `T`. => `S32UVT`.
///
/// Info: d=112 => 112-72 = 40 = `(`; m=7 (0..=9) => 7+88 = 95 = `_`;
/// h=74 => 74+28 = 102 = `f`. Speed+800 = 820, course+400 = 651:
/// SP = 820/10+28 = 110 = `n`; DC = (820%10)*10 + 651/100 + 28 = 34 =
/// `"`; SE = 651%100+28 = 79 = `O`. Then code `j`, table `/`.
#[test]
fn spec_vector_encodes() {
    let mut r = report(33 * 6000 + 2564, -(112 * 6000 + 774));
    r.speed = 20;
    r.course = 251;
    r.symbol = Symbol::from_wire(b'/', b'j');
    r.message = MicEMessage::Returning;
    let mut dest = [0u8; 6];
    let mut info = [0u8; 32];
    let len = r.encode(&mut dest, &mut info).unwrap();
    assert_eq!(&dest, b"S32UVT");
    assert_eq!(&info[..len], b"`(_fn\"Oj/");
}

#[test]
fn spec_vector_decodes() {
    let got = mic_e::decode(b"S32UVT", b"`(_fn\"Oj/").unwrap();
    assert_eq!(hundredths(got.latitude.units()), 33 * 6000 + 2564);
    assert_eq!(hundredths(got.longitude.units()), -(112 * 6000 + 774));
    assert_eq!(got.speed, 20);
    assert_eq!(got.course, 251);
    assert_eq!(got.symbol.to_wire(), (b'/', b'j'));
    assert_eq!(got.message, MicEMessage::Returning);
    assert_eq!(got.fix, MicEFix::Current);
    assert_eq!(got.altitude, None);
    assert_eq!(got.ambiguity, 0);
    assert_eq!(got.status, b"");
}

/// Hand-derived vector: south/east, custom message, low longitude.
///
/// Position 5 deg 06.07 min S, 8 deg 09.01 min E, stationary. Digits
/// 0 5 0 6 0 7; custom bits 101 (Custom2) => `A`+0, `5`, `A`+0.
/// Column 4 south => digit `6`; longitude 8 is in the 0-9 band which
/// encodes with the +100 offset set => `P`+0 = `P`; east => digit
/// `7`. => `A5A6P7`.
///
/// Info: d=8 (0..=9) => 8+118 = 126 = `~`; m=9 => 9+88 = 97 = `a`;
/// h=1 => 29. Speed+800 = 800, course+400 = 400: SP = 108 = `l`;
/// DC = 0+4+28 = 32 = ` `; SE = 0+28 = 28.
#[test]
fn south_east_custom_vector() {
    let mut r = report(-(5 * 6000 + 607), 8 * 6000 + 901);
    r.message = MicEMessage::Custom2;
    let mut dest = [0u8; 6];
    let mut info = [0u8; 32];
    let len = r.encode(&mut dest, &mut info).unwrap();
    assert_eq!(&dest, b"A5A6P7");
    assert_eq!(&info[..len], b"`~a\x1dl \x1c>/");
    let got = mic_e::decode(&dest, &info[..len]).unwrap();
    assert_eq!(got, r);
}

/// Altitude 61 m: 61+10000 = 10061 = 1*91*91 + 19*91 + 51 =>
/// `"` `4` `T` `}` appended before the status text.
#[test]
fn altitude_and_status() {
    let mut r = report(200_564, -672_774);
    r.altitude = Some(61);
    r.status = b"Test 001234";
    let mut dest = [0u8; 6];
    let mut info = [0u8; 40];
    let len = r.encode(&mut dest, &mut info).unwrap();
    assert_eq!(&info[9..13], b"\"4T}");
    assert_eq!(&info[13..len], b"Test 001234");
    let got = mic_e::decode(&dest, &info[..len]).unwrap();
    assert_eq!(got, r);
}

/// The `'` (old GPS data) identifier is accepted and reported.
#[test]
fn old_fix_type_byte() {
    let mut r = report(200_564, -672_774);
    r.fix = MicEFix::Old;
    let mut dest = [0u8; 6];
    let mut info = [0u8; 16];
    let len = r.encode(&mut dest, &mut info).unwrap();
    assert_eq!(info[0], b'\'');
    let got = mic_e::decode(&dest, &info[..len]).unwrap();
    assert_eq!(got.fix, MicEFix::Old);
}

const ALL_MESSAGES: [MicEMessage; 15] = [
    MicEMessage::OffDuty,
    MicEMessage::EnRoute,
    MicEMessage::InService,
    MicEMessage::Returning,
    MicEMessage::Committed,
    MicEMessage::Special,
    MicEMessage::Priority,
    MicEMessage::Emergency,
    MicEMessage::Custom0,
    MicEMessage::Custom1,
    MicEMessage::Custom2,
    MicEMessage::Custom3,
    MicEMessage::Custom4,
    MicEMessage::Custom5,
    MicEMessage::Custom6,
];

/// Every message type round-trips through the destination bits.
#[test]
fn message_types_exhaustive() {
    for msg in ALL_MESSAGES {
        let mut r = report(200_564, -672_774);
        r.message = msg;
        let mut dest = [0u8; 6];
        let mut info = [0u8; 16];
        let len = r.encode(&mut dest, &mut info).unwrap();
        let got = mic_e::decode(&dest, &info[..len]).unwrap();
        assert_eq!(got.message, msg, "dest {dest:?}");
    }
}

/// N/S, E/W and the longitude +100 offset in all combinations.
#[test]
fn hemispheres_and_offset() {
    for &lat in &[200_564i64, -200_564] {
        // Degrees 5 and 105 sit on either side of the offset rule;
        // 0 and 179 are the extremes.
        for &lon_deg in &[0i64, 5, 9, 10, 42, 99, 100, 105, 109, 110, 179] {
            for &east in &[false, true] {
                let lon = (lon_deg * 6000 + 1234) * if east { 1 } else { -1 };
                let r = report(lat, lon);
                let mut dest = [0u8; 6];
                let mut info = [0u8; 16];
                let len = r.encode(&mut dest, &mut info).unwrap();
                let got = mic_e::decode(&dest, &info[..len]).unwrap();
                assert_eq!(got, r, "lat {lat} lon {lon}");
            }
        }
    }
}

/// Every ambiguity level 0..=4 encodes the documented blank characters
/// and decodes back (blanked digits read as zero).
#[test]
fn ambiguity_levels() {
    for amb in 0u8..=4 {
        let mut r = report(33 * 6000 + 2564, -672_774);
        r.ambiguity = amb;
        let mut dest = [0u8; 6];
        let mut info = [0u8; 16];
        let len = r.encode(&mut dest, &mut info).unwrap();
        let got = mic_e::decode(&dest, &info[..len]).unwrap();
        assert_eq!(got.ambiguity, amb);
        // Blanked digits decode as zero: lat digits 3 3 2 5 6 4.
        let digits = [3i32, 3, 2, 5, 6, 4];
        let mut kept = 0i64;
        for (i, d) in digits.iter().enumerate() {
            if i < 6 - amb as usize {
                kept += i64::from(*d) * [100_000, 10_000, 1_000, 100, 10, 1][i];
            }
        }
        let deg = kept / 10_000;
        let rest = kept % 10_000;
        assert_eq!(
            hundredths(got.latitude.units()),
            deg * 6000 + rest,
            "ambiguity {amb}"
        );
    }
    // Standard-set blank is 'Z' where the overlaid bit is 1 (here
    // north, +100 offset and west are all set), 'L' for a 0 bit;
    // custom-set blank is 'K' in the message columns.
    let mut r = report(200_564, -672_774);
    r.ambiguity = 4;
    r.message = MicEMessage::OffDuty;
    let mut dest = [0u8; 6];
    let mut info = [0u8; 16];
    r.encode(&mut dest, &mut info).unwrap();
    assert_eq!(&dest[2..6], b"ZZZZ");
    r.message = MicEMessage::Custom0;
    r.encode(&mut dest, &mut info).unwrap();
    assert_eq!(&dest[2..6], b"KZZZ");
    // Southern/eastern low-longitude report: those bits are 0 => 'L'.
    let mut r = report(-200_564, 300_000);
    r.ambiguity = 4;
    r.message = MicEMessage::Emergency;
    r.encode(&mut dest, &mut info).unwrap();
    assert_eq!(&dest[2..6], b"LLLL");
}

/// The declared ambiguity travels with the position, per level.
///
/// `MicE::coordinates()` used to call `Coordinates::new`, which
/// hard-codes `Ambiguity::EXACT`: a caller that read the returned pair
/// instead of the `ambiguity` field was told a blurred position was
/// exact. Blanked digits decode as zero, so the position alone cannot
/// reveal the blur -- 52 deg 09.00 min is a perfectly plausible exact fix.
#[test]
fn declared_ambiguity_reaches_the_coordinates() {
    for amb in 0u8..=4 {
        let r = report(33 * 6000 + 2564, -672_774)
            .with_ambiguity(amb)
            .unwrap();
        let mut dest = [0u8; 6];
        let mut info = [0u8; 16];
        let len = r.encode(&mut dest, &mut info).unwrap();
        let got = mic_e::decode(&dest, &info[..len]).unwrap();
        let at = got.coordinates();
        assert_eq!(
            at.ambiguity,
            Ambiguity::new(amb).unwrap(),
            "ambiguity {amb}"
        );
        assert_eq!(at.ambiguity.digits(), amb, "ambiguity {amb}");
        // Level 0 -- and only level 0 -- still means exact.
        assert_eq!(at.ambiguity.is_exact(), amb == 0, "ambiguity {amb}");
        // The pair reports the position MASKED to the declared
        // precision, which is not the same as the decoded fields: the
        // wire always carries a full-precision longitude and chapter 10
        // makes discarding the matching digits the receiver's job.
        let level = Ambiguity::new(amb).unwrap();
        assert_eq!(
            at.latitude.units(),
            level.mask(got.latitude.units()),
            "ambiguity {amb}"
        );
        assert_eq!(
            at.longitude.units(),
            level.mask(got.longitude.units()),
            "ambiguity {amb}"
        );
        // Masking never invents precision, and above level 0 on this
        // fixture it always removes some.
        assert!(at.longitude.units().abs() <= got.longitude.units().abs());
        assert_eq!(
            at.longitude.units() == got.longitude.units(),
            amb == 0,
            "only an exact report may pass the longitude through: {amb}"
        );
    }
}

/// Decode side, real wire bytes: a frame that declares two blanked
/// digits reports two on `coordinates()`.
///
/// These are the wire bytes of the `mice_b2` vector that
/// `tests/oracle.rs` drives through the full radio stack; they were
/// produced by the reference encoder, not by this crate. Destination
/// `F2A9ZL` blanks latitude columns 5 and 6 ('Z' where the overlaid bit
/// is set, 'L' where it is clear), so the sender declared 52 deg 09.  '
/// S -- precise to an arc-minute and no finer.
#[test]
fn declared_ambiguity_survives_a_real_wire_payload() {
    let got = mic_e::decode(b"F2A9ZL", b"'v:&lg!-/\"6)}hello").unwrap();
    assert_eq!(got.ambiguity, 2);
    let at = got.coordinates();
    assert_eq!(at.ambiguity, Ambiguity::new(2).unwrap());
    assert_eq!(at.ambiguity.digits(), 2);
    assert!(!at.ambiguity.is_exact());
    // Blanked digits read as zero, which is why the count has to travel
    // with the position rather than beside it. The latitude arrives
    // already at whole minutes, because that is what the destination
    // spelled.
    assert_eq!(hundredths(at.latitude.units()), -(52 * 6000 + 900));
    assert_eq!(hundredths(got.latitude.units()), -(52 * 6000 + 900));
    // The longitude does not. The wire carries 30.10 arc-minutes and
    // the sender declared precision to the arc-minute, so the reported
    // position is 30.00 and the decoded field keeps what arrived. The
    // difference is 0.10 arc-minutes, about 185 m: small enough to look
    // like a plausible fix and large enough to matter.
    assert_eq!(hundredths(got.longitude.units()), 30 * 100 + 10);
    assert_eq!(hundredths(at.longitude.units()), 30 * 100);
}

/// Chapter 10's own worked example of the longitude discard.
///
/// The strongest vector available for this rule, because the spec
/// states both the input and the answer:
///
/// > The position ambiguity is specified for the latitude (in the
/// > destination address). The same degree of ambiguity will then also
/// > apply to the longitude. For example, if the destination address is
/// > `T4SQZZ`, the last two digits of the latitude are ambiguous
/// > (represented by `ZZ`). Then, if the longitude data in the
/// > Information field is `(_f`, as in the above example, the last two
/// > digits of the computed longitude will be ignored -- that is, the
/// > longitude will be 112 degrees 7 minutes.
///
/// The arithmetic, so the expected value is checkable by hand. The
/// longitude bytes are offset by 28: `(` is 40, so 12 degrees, plus the
/// 100-degree offset the destination's fifth character requests, giving
/// 112. `_` is 95, so 67 minutes, and 67 is above 60, so 7. `f` is 102,
/// so 74 hundredths. Full precision is 112 degrees 7.74 minutes, and
/// the spec says to report 112 degrees 7 minutes.
///
/// Before this rule was applied the crate answered 112 degrees 7.74
/// minutes: **1373 m more precise than the sender declared**, on a
/// report that had correctly decoded the declaration two fields
/// earlier.
#[test]
fn spec_chapter_10_ignores_the_low_longitude_digits() {
    let got = mic_e::decode(b"T4SQZZ", b"'(_f \x1c>/]").expect("chapter 10's own vector");
    assert_eq!(got.ambiguity, 2, "the destination declares two digits");
    // The field keeps the wire: 112 deg 7.74 min west.
    assert_eq!(
        hundredths(got.longitude.units()),
        -(112 * 6000 + 7 * 100 + 74)
    );
    // The reported position is what the spec says it is.
    let at = got.coordinates();
    assert_eq!(
        hundredths(at.longitude.units()),
        -(112 * 6000 + 7 * 100),
        "chapter 10: the longitude will be 112 degrees 7 minutes"
    );
    assert_eq!(at.ambiguity, Ambiguity::new(2).unwrap());
}

/// `coordinates()` carries the declared ambiguity through.
///
/// This test used to pin that the accessor stayed callable in a `const`
/// context. It no longer is, and the trade was made knowingly:
/// `coordinates()` now also applies a `!DAO!` refinement found in the
/// status text, which means scanning a byte slice, and a scan cannot be
/// `const`. Dropping `const` from a public fn is semver-relevant, so it
/// is recorded here rather than discovered.
///
/// The alternative was a second accessor, one refined and one not, and
/// that is the trap this project has already fallen into twice: every
/// renderer read the raw fields instead of the masking accessor. One
/// accessor that is always right beats two where the caller picks.
#[test]
fn coordinates_carries_the_declared_ambiguity() {
    const BLURRED: MicE<'static> = MicE {
        latitude: match Latitude::new(-(52 * 6000 + 900)) {
            Ok(l) => l,
            Err(_) => panic!("in range"),
        },
        longitude: match Longitude::new(3010) {
            Ok(l) => l,
            Err(_) => panic!("in range"),
        },
        speed: 0,
        course: 0,
        symbol: Symbol::CAR,
        message: MicEMessage::OffDuty,
        fix: MicEFix::Current,
        altitude: None,
        device_prefix: None,
        ambiguity: 2,
        status: b"",
    };
    let ambiguity: Ambiguity = BLURRED.coordinates().ambiguity;
    assert_eq!(ambiguity.digits(), 2);
    assert!(!ambiguity.is_exact());
}

/// An out-of-range `ambiguity` saturates at four digits, and nothing
/// panics.
///
/// `MicE::ambiguity` is a public `u8`, so a struct literal can hold a
/// count the wire format has no room for even though `MicE::decode`,
/// `MicE::with_ambiguity` and `MicE::encode` all reject it. Saturating
/// errs toward *less* claimed precision; reporting `Ambiguity::EXACT`
/// would be the one answer already known to be false.
#[test]
fn out_of_range_ambiguity_saturates_at_four_digits() {
    for bogus in [5u8, 6, 100, u8::MAX] {
        let mut r = report(33 * 6000 + 2564, -672_774);
        // Only reachable through the public field; both validating
        // paths refuse this value, as re-asserted below.
        r.ambiguity = bogus;
        let at = r.coordinates();
        assert_eq!(
            at.ambiguity,
            Ambiguity::new(4).unwrap(),
            "ambiguity {bogus}"
        );
        assert_eq!(at.ambiguity.digits(), 4, "ambiguity {bogus}");
        assert!(!at.ambiguity.is_exact(), "ambiguity {bogus}");
        // The position is masked at the saturated level, four digits,
        // which is a whole degree. Saturating errs toward less claimed
        // precision and the masking has to follow it there, or the
        // report would claim one degree of vagueness while naming a
        // position to the hundredth of a minute.
        let four = Ambiguity::new(4).unwrap();
        assert_eq!(
            at.latitude.units(),
            four.mask(r.latitude.units()),
            "ambiguity {bogus}"
        );
        assert_eq!(
            at.longitude.units(),
            four.mask(r.longitude.units()),
            "ambiguity {bogus}"
        );
        // The value still never reaches the wire, or a builder.
        let mut dest = [0u8; 6];
        let mut info = [0u8; 16];
        assert_eq!(
            r.encode(&mut dest, &mut info),
            Err(MicEError::BadAmbiguity { got: bogus })
        );
        assert_eq!(
            r.with_ambiguity(bogus),
            Err(MicEError::BadAmbiguity { got: bogus })
        );
    }
}

/// Speed and course boundaries: 0, wrap thresholds, and the maxima.
#[test]
fn speed_course_boundaries() {
    for &(speed, course) in &[
        (0u16, 0u16),
        (0, 360),
        (1, 1),
        (199, 359),
        (200, 360),
        (28, 100),
        (799, 0),
        (799, 360),
    ] {
        let mut r = report(200_564, -672_774);
        r.speed = speed;
        r.course = course;
        let mut dest = [0u8; 6];
        let mut info = [0u8; 16];
        let len = r.encode(&mut dest, &mut info).unwrap();
        let got = mic_e::decode(&dest, &info[..len]).unwrap();
        assert_eq!((got.speed, got.course), (speed, course));
    }
}

/// Every accepted `(speed, course)` pair encodes to 7-bit ASCII, and
/// still decodes back to itself.
///
/// Chapter 10's SP+28 table is two tables printed side by side, and
/// reading it as one is how the defect this pins got in. Speeds 0-199
/// knots have **two** equally valid spellings, `tens + 108` and
/// `tens + 28`; speeds 200-799 have only `tens + 28`. The "+800 knots"
/// offset in the spec's decoding algorithm *is* the `tens + 108`
/// column, so applying it unconditionally walks off the end of that
/// column at 200 knots.
///
/// VERIFIED before the fix: this sweep failed at the first case with
/// `speed` 200, where `encode` emitted 0x80 for SP+28, rising to 0xBB
/// at 799 knots -- eight-bit bytes in an information field, which
/// nothing but our own decoder is obliged to accept. Neither the
/// round-trip sweep nor the corpus could see it: 0x80 wraps back
/// through the very `>= 800` rule that wrote it, so `round_trip_sweep`
/// passed on the broken encoder, and the corpus is decode-only anyway.
/// MEASURED over the corpus: 894 Mic-E frames, fastest 61 knots, none
/// above 199 -- real off-air traffic never reaches the boundary, which
/// is exactly why an on-air defect could sit here unseen.
///
/// The floor is the whole accepted domain, `800 * 361` pairs, so the
/// loop cannot pass having narrowed to nothing.
#[test]
fn every_accepted_speed_and_course_encodes_to_seven_bit_ascii() {
    const MIN_CASES: usize = 800 * 361;
    let mut cases = 0usize;
    for speed in 0..=799u16 {
        for course in 0..=360u16 {
            let mut r = report(33 * 6000 + 2564, -(112 * 6000 + 774));
            r.speed = speed;
            r.course = course;
            let mut dest = [0u8; 6];
            let mut info = [0u8; 16];
            let len = r
                .encode(&mut dest, &mut info)
                .unwrap_or_else(|e| panic!("{speed} kn / {course} deg rejected: {e:?}"));
            for (i, &b) in info[..len].iter().enumerate() {
                assert!(
                    b <= 0x7F,
                    "{speed} kn / {course} deg put 0x{b:02X} at info[{i}]: an APRS \
                     information field is 7-bit ASCII"
                );
            }
            // The destination address is the other half of what goes on
            // the air, and it is derived from the same call.
            for (i, &b) in dest.iter().enumerate() {
                assert!(
                    b <= 0x7F,
                    "{speed} kn / {course} deg put 0x{b:02X} at dest[{i}]"
                );
            }
            // Encodable is not enough; it has to still say what it meant.
            let got = mic_e::decode(&dest, &info[..len])
                .unwrap_or_else(|e| panic!("{speed} kn / {course} deg failed to decode: {e:?}"));
            assert_eq!((got.speed, got.course), (speed, course));
            cases += 1;
        }
    }
    assert!(
        cases >= MIN_CASES,
        "the sweep checked {cases} speed/course pairs, floor is {MIN_CASES} -- \
         the domain narrowed and the law stopped proving anything"
    );
}

/// The SP+28 column switch, pinned byte by byte at the boundary.
///
/// 199 knots is the last speed with two spellings and 200 the first
/// with one, so 199/200 is where the offset column has to be dropped.
/// 799 knots is the last row of the table, and 800 the first speed that
/// is out of range -- chapter 10 states the speed range as
/// 0-799 knots, and `tens + 28` covers all of it inside 7-bit ASCII, so
/// nothing between 200 and 799 needs rejecting.
///
/// Course is fixed at 0 here, which makes DC+28 `units * 10 + 4 + 28`
/// and SE+28 28.
#[test]
fn sp28_drops_the_offset_column_above_one_hundred_ninety_nine_knots() {
    // (speed, SP+28, DC+28), with course fixed at 0.
    #[rustfmt::skip]
    let rows = [
        // Two-column region: `tens + 108`, which is the +800 offset.
        (0u16, 0x6Cu8, 0x20u8),  // `l`, units 0
        (40, 0x70, 0x20),        // `p`, units 0
        (190, 0x7F, 0x20),       // DEL, units 0 -- last two-column row
        (199, 0x7F, 0x7A),       // DEL, units 9
        // Single-column region: `tens + 28`. Pre-fix these were 0x80
        // and 0xBB, outside 7-bit ASCII.
        (200, 0x30, 0x20),       // `0`, units 0 -- first single-column row
        (799, 0x6B, 0x7A),       // `k`, units 9 -- last row of the table
    ];
    for &(speed, sp, dc) in &rows {
        let mut r = report(33 * 6000 + 2564, -(112 * 6000 + 774));
        r.speed = speed;
        let mut dest = [0u8; 6];
        let mut info = [0u8; 16];
        let len = r.encode(&mut dest, &mut info).expect("in range");
        assert_eq!(info[4], sp, "SP+28 for {speed} kn");
        assert_eq!(info[5], dc, "DC+28 for {speed} kn");
        assert_eq!(info[6], 28, "SE+28 for course 0");
        let got = mic_e::decode(&dest, &info[..len]).expect("decodes");
        assert_eq!((got.speed, got.course), (speed, 0));
    }
    // 800 knots is the first rejected speed, and it is rejected in both
    // places: `MicE::new` up front, and `encode` again at the point the
    // bytes would go on the air, for the caller who built by literal.
    let mut r = report(33 * 6000 + 2564, -(112 * 6000 + 774));
    r.speed = 800;
    let mut dest = [0u8; 6];
    let mut info = [0u8; 16];
    assert_eq!(
        r.encode(&mut dest, &mut info),
        Err(MicEError::BadSpeed { got: 800 })
    );
    assert_eq!(
        r.encode_info(&mut info),
        Err(MicEError::BadSpeed { got: 800 })
    );
    assert_eq!(
        MicE::new(r.latitude, r.longitude, 800, 0, r.symbol, r.message),
        Err(MicEError::BadSpeed { got: 800 })
    );
    // ...and 799 is accepted by both.
    assert!(MicE::new(r.latitude, r.longitude, 799, 360, r.symbol, r.message).is_ok());
}

/// The two single-column speeds, adjudicated against an independent
/// decoder.
///
/// Same position, symbol and message bits as `spec_vector_encodes`
/// (33 deg 25.64 min N, 112 deg 07.74 min W, `/j`, Returning), so only
/// the three speed/course bytes differ from the hand-derived vector.
///
/// 200 knots, course 251: SP+28 = `20 + 28` = 48 = `0`;
/// DC+28 = `0 * 10 + 651 / 100 + 28` = 34 = `"`;
/// SE+28 = `651 % 100 + 28` = 79 = `O`.
///
/// 799 knots, course 360: SP+28 = `79 + 28` = 107 = `k`;
/// DC+28 = `9 * 10 + 760 / 100 + 28` = 125 = `}`;
/// SE+28 = `760 % 100 + 28` = 88 = `X`.
///
/// ADJUDICATED: fed to the reference decoder as TNC2 monitor lines,
/// `K1ABC-9>S32UVT:` + each information field below, it reports
/// `N 33 25.6400, W 112 07.7400, 370 km/h (230 MPH), course 251` and
/// `N 33 25.6400, W 112 07.7400, 1480 km/h (919 MPH), course 0`.
/// 230 MPH is 200 knots and 919 MPH is 799 knots; the reference renders
/// chapter 10's course 360 (due north) as 0 degrees true and reserves
/// its own "unknown" for chapter 10's course 0, so `course 0` there is
/// the 360 asked for. Both fields are 7-bit ASCII throughout.
#[test]
fn single_column_speeds_adjudicate_against_the_reference() {
    for &(speed, course, want) in &[
        (200u16, 251u16, b"`(_f0\"Oj/".as_slice()),
        (799, 360, b"`(_fk}Xj/".as_slice()),
    ] {
        let mut r = report(33 * 6000 + 2564, -(112 * 6000 + 774));
        r.speed = speed;
        r.course = course;
        r.symbol = Symbol::from_wire(b'/', b'j');
        r.message = MicEMessage::Returning;
        let mut dest = [0u8; 6];
        let mut info = [0u8; 32];
        let len = r.encode(&mut dest, &mut info).expect("in range");
        assert_eq!(&dest, b"S32UVT", "{speed} kn / {course} deg");
        assert_eq!(&info[..len], want, "{speed} kn / {course} deg");
        assert!(info[..len].iter().all(|&b| b <= 0x7F));
        let got = mic_e::decode(&dest, &info[..len]).expect("decodes");
        assert_eq!((got.speed, got.course), (speed, course));
    }
}

/// Deterministic LCG round-trip sweep over the whole parameter space.
///
/// `next(800)` is already exactly chapter 10's accepted speed range,
/// `0..=799`, so the domain here needed no adjustment for the SP+28
/// column fix -- every value it draws is encodable, and now all of them
/// encode to 7-bit ASCII as well.
#[test]
fn round_trip_sweep() {
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move |bound: u64| {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) % bound
    };
    // The four device-identifier prefixes of chapter 10, plus "none":
    // a received report must re-encode to the bytes it arrived as,
    // whichever radio wrote it.
    const PREFIXES: [Option<u8>; 5] = [None, Some(b'>'), Some(b']'), Some(b'`'), Some(b'\'')];
    for i in 0..2000 {
        let lat = next(2 * 540_001) as i64 - 540_000;
        // Mic-E longitude tops out at 179 deg 59.99 min.
        let lon = next(2 * 1_079_999 + 1) as i64 - 1_079_999;
        let altitude = if next(2) == 0 {
            Some(next(753_571) as i32 - 10_000)
        } else {
            None
        };
        let r = MicE {
            latitude: Latitude::new(lat * warble::geo::UNITS_PER_HUNDREDTH_MINUTE).unwrap(),
            longitude: Longitude::new(lon * warble::geo::UNITS_PER_HUNDREDTH_MINUTE).unwrap(),
            speed: next(800) as u16,
            course: next(361) as u16,
            symbol: Symbol::from_wire(
                [b'/', b'\\', b'3', b'Q'][next(4) as usize],
                (next(94) + 33) as u8,
            ),
            message: ALL_MESSAGES[next(15) as usize],
            fix: if next(2) == 0 {
                MicEFix::Current
            } else {
                MicEFix::Old
            },
            altitude,
            // Swept independently of the altitude, because chapter 10
            // makes the two optional separately: `]"4T}`, `]Stopped`
            // and `"4T}` are all well-formed, and all three must
            // survive a round trip.
            device_prefix: PREFIXES[next(5) as usize],
            ambiguity: 0,
            // Status text that does not itself begin with one of the
            // four prefix bytes. When it does the wire form is still a
            // fixed point but the struct is not, which is the whole
            // one-byte cost of the reading and is pinned on its own in
            // `status_beginning_with_a_prefix_byte_is_the_bounded_cost`.
            status: b"status text",
        };
        let mut dest = [0u8; 6];
        let mut info = [0u8; 32];
        let len = r.encode(&mut dest, &mut info).unwrap();
        let got = mic_e::decode(&dest, &info[..len]).unwrap();
        assert_eq!(got, r, "iteration {i}");
    }
}

// ---- device-identifier prefixes (chapter 10) ----

/// The spec's own three spellings of one altitude: `"4T}`, `>"4T}`,
/// `]"4T}`. All three are 61 m; only the prefix differs.
#[test]
fn spec_three_spellings_of_one_altitude() {
    for (tail, prefix) in [
        (&b"\"4T}"[..], None),
        (&b">\"4T}"[..], Some(b'>')),
        (&b"]\"4T}"[..], Some(b']')),
    ] {
        let mut info = [0u8; 32];
        info[..9].copy_from_slice(b"`(_fn\"Oj/");
        info[9..9 + tail.len()].copy_from_slice(tail);
        let got = mic_e::decode(b"S32UVT", &info[..9 + tail.len()]).unwrap();
        assert_eq!(got.altitude, Some(61), "{:?}", tail.escape_ascii());
        assert_eq!(got.device_prefix, prefix, "{:?}", tail.escape_ascii());
        assert_eq!(got.status, b"");
    }
}

/// Chapter 10's Maidenhead status examples, `>IO91SX/G Helloworld`
/// and `]IO91SX/G Helloworld`, carry a device prefix and **no**
/// altitude field. Gating the prefix on a following altitude left the
/// `>` stranded at the front of the status text, which is neither what
/// the spec prints nor what an application is told to display.
#[test]
fn spec_maidenhead_status_is_a_prefix_with_no_altitude() {
    for prefix in [b'>', b']'] {
        let mut info = [0u8; 40];
        info[..9].copy_from_slice(b"`(_fn\"Oj/");
        info[9] = prefix;
        info[10..29].copy_from_slice(b"IO91SX/G Helloworld");
        let got = mic_e::decode(b"S32UVT", &info[..29]).unwrap();
        assert_eq!(got.device_prefix, Some(prefix));
        assert_eq!(got.altitude, None);
        assert_eq!(got.status, b"IO91SX/G Helloworld");

        // Byte-exact: the prefix is held, not normalised away.
        let mut out = [0u8; 40];
        let n = got.encode_info(&mut out).unwrap();
        assert_eq!(&out[..n], &info[..29]);
    }
}

/// Two real corpus frames from `AE6GR-7`, one radio in one session,
/// spelling the same device two ways: `]"6[}` (prefix + altitude,
/// empty status) and `]Stopped` (prefix, no altitude). Reading only
/// the first is what left 35 frames with a `]` glued to their status
/// text; the reference decoder names both a Kenwood TM-D700 and
/// displays the second as `Stopped`.
#[test]
fn corpus_kenwood_spells_its_prefix_both_ways() {
    let with_alt = mic_e::decode(b"S4PXYW", b"'._|l tv/]\"6[}\r").unwrap();
    assert_eq!(with_alt.device_prefix, Some(b']'));
    assert_eq!(with_alt.altitude, Some(250));
    assert_eq!(with_alt.status, b"\r");

    let without = mic_e::decode(b"STPYVU", b"'._wlzzv/]Stopped\r").unwrap();
    assert_eq!(without.device_prefix, Some(b']'));
    assert_eq!(without.altitude, None);
    assert_eq!(without.status, b"Stopped\r");

    // Both stations are the same van, within a few hundredths of a
    // minute of each other, so the fix survived either spelling.
    assert_eq!(with_alt.symbol.to_wire(), without.symbol.to_wire());

    // And both re-encode to the bytes they arrived as.
    for (dest, wire) in [
        (&b"S4PXYW"[..], &b"'._|l tv/]\"6[}\r"[..]),
        (&b"STPYVU"[..], &b"'._wlzzv/]Stopped\r"[..]),
    ] {
        let dest: [u8; 6] = dest.try_into().unwrap();
        let got = mic_e::decode(&dest, wire).unwrap();
        let mut out = [0u8; 64];
        let n = got.encode_info(&mut out).unwrap();
        assert_eq!(&out[..n], wire);
    }
}

/// The other three corpus spellings of a bare prefix, including the
/// five `AE6NM-1` frames whose entire status text is the prefix.
#[test]
fn corpus_bare_prefix_frames() {
    for (dest, wire, status) in [
        (&b"S4PWPW"[..], &b"'-M(l \x1cK\\]\r"[..], &b"\r"[..]),
        (
            &b"S3PYTW"[..],
            &b"'-'Tl \x1c#/]Palomar REACT Digi\r"[..],
            &b"Palomar REACT Digi\r"[..],
        ),
        (
            &b"S3URPP"[..],
            &b"'._0l \x1c-/]Ted@Home in Lakewood,CA.USA\r"[..],
            &b"Ted@Home in Lakewood,CA.USA\r"[..],
        ),
        (
            &b"S3PRWP"[..],
            &b"',Pdl\"Gk/]Tania's Crusin\r"[..],
            &b"Tania's Crusin\r"[..],
        ),
    ] {
        let dest: [u8; 6] = dest.try_into().unwrap();
        let got = mic_e::decode(&dest, wire).unwrap();
        assert_eq!(
            (got.device_prefix, got.altitude, got.status),
            (Some(b']'), None, status),
            "{:?}",
            wire.escape_ascii()
        );
        let mut out = [0u8; 64];
        let n = got.encode_info(&mut out).unwrap();
        assert_eq!(&out[..n], wire, "{:?}", wire.escape_ascii());
    }
}

/// A prefix with no altitude behind it round-trips byte-exactly, for
/// each of the four prefixes and with and without status text. This
/// is the law the old reading could not state: `encode` already wrote
/// the prefix and the altitude independently, so nothing on the
/// encoding side had to change for it to hold.
#[test]
fn prefix_without_altitude_round_trips_byte_exactly() {
    // Status texts: empty, short, and one long enough to push the info
    // field well past the prefix and altitude bytes.
    for prefix in [b'>', b']', b'`', b'\''] {
        for status in [&b""[..], &b"Stopped"[..], &b"Mobile station, 25 W"[..]] {
            let r = report(200_564, -672_774)
                .with_status(status)
                .with_device_prefix(Some(prefix))
                .unwrap();
            let mut dest = [0u8; 6];
            let mut info = [0u8; 64];
            let len = r.encode(&mut dest, &mut info).unwrap();
            assert_eq!(info[9], prefix);
            assert_eq!(&info[10..len], status);

            let got = mic_e::decode(&dest, &info[..len]).unwrap();
            assert_eq!(got, r, "{:?} + {:?}", prefix as char, status.escape_ascii());

            let mut out = [0u8; 64];
            let n = got.encode_info(&mut out).unwrap();
            assert_eq!(&out[..n], &info[..len]);
        }
    }
}

/// An unprefixed altitude whose leading base-91 digit happens to be a
/// prefix byte still reads as an altitude, or this crate's own encoder
/// stops being invertible: 39 686 m encodes to `'!!}` and 40 000 m to
/// `'$J}`. The reference decoder reads both of those as altitudes too.
#[test]
fn unprefixed_altitude_outranks_a_prefix_shaped_first_digit() {
    // (metres, the four base-91 bytes it encodes to)
    for (metres, wire) in [
        (39_686i32, &b"'!!}"[..]),
        (40_000, &b"'$J}"[..]),
        (230_149, &b">!!}"[..]),
        (492_471, &b"]^]}"[..]),
        (511_703, &b"`!!}"[..]),
    ] {
        let r = report(200_564, -672_774)
            .with_altitude(Some(metres))
            .with_status(b"up");
        let mut dest = [0u8; 6];
        let mut info = [0u8; 32];
        let len = r.encode(&mut dest, &mut info).unwrap();
        assert_eq!(&info[9..13], wire, "{metres} m");

        let got = mic_e::decode(&dest, &info[..len]).unwrap();
        assert_eq!(got.altitude, Some(metres), "{metres} m");
        assert_eq!(got.device_prefix, None, "{metres} m");
        assert_eq!(got.status, b"up", "{metres} m");
        assert_eq!(got, r, "{metres} m");
    }
}

/// The bounded cost of the reading, stated as a test rather than only
/// as a comment: status text that itself begins with `>` loses exactly
/// one byte to `device_prefix`. The **wire** is still a fixed point,
/// which is what an igate needs, and a caller that disagrees with the
/// guess can put the byte back.
#[test]
fn status_beginning_with_a_prefix_byte_is_the_bounded_cost() {
    let r = report(200_564, -672_774).with_status(b">not a radio");
    let mut dest = [0u8; 6];
    let mut info = [0u8; 32];
    let len = r.encode(&mut dest, &mut info).unwrap();

    let got = mic_e::decode(&dest, &info[..len]).unwrap();
    assert_ne!(got, r, "the struct is deliberately not a fixed point");
    assert_eq!(got.device_prefix, Some(b'>'));
    assert_eq!(got.status, b"not a radio");

    // Byte-exact re-encode: nothing was dropped, only relabelled.
    let mut out = [0u8; 32];
    let n = got.encode_info(&mut out).unwrap();
    assert_eq!(&out[..n], &info[..len]);

    // Exactly one byte moved, and the caller can move it back.
    let mut rejoined = [0u8; 12];
    rejoined[0] = got.device_prefix.unwrap();
    rejoined[1..].copy_from_slice(got.status);
    assert_eq!(&rejoined[..], r.status);
}

// ---- per-variant decode errors ----

/// A minimal valid info field for error tests.
const INFO: &[u8] = b"`(_fn\"Oj/";

#[test]
fn decode_errors() {
    // BadDestLength
    assert_eq!(
        mic_e::decode(b"S32UV", INFO),
        Err(MicEError::BadDestLength { got: 5 })
    );
    // BadDestChar: 'a' is never legal; 'A' is illegal past column 3.
    assert_eq!(
        mic_e::decode(b"a32UVT", INFO),
        Err(MicEError::BadDestChar {
            got: b'a',
            column: 0
        })
    );
    assert_eq!(
        mic_e::decode(b"S32AVT", INFO),
        Err(MicEError::BadDestChar {
            got: b'A',
            column: 3
        })
    );
    // MixedMessageBits: standard 'S' with custom 'A'.
    assert_eq!(
        mic_e::decode(b"SA2UVT", INFO),
        Err(MicEError::MixedMessageBits {
            got: [b'S', b'A', b'2']
        })
    );
    // NonTrailingAmbiguity: blank followed by a digit.
    assert_eq!(
        mic_e::decode(b"S3L5VT", INFO),
        Err(MicEError::NonTrailingAmbiguity { column: 3 })
    );
    // BadAmbiguity: five blanks.
    assert_eq!(
        mic_e::decode(b"SZLLLL", INFO),
        Err(MicEError::BadAmbiguity { got: 5 })
    );
    // Truncated info.
    assert_eq!(
        mic_e::decode(b"S32UVT", b"`(_fn"),
        Err(MicEError::Truncated {
            expected: 9,
            got: 5
        })
    );
    // InvalidDataType.
    assert_eq!(
        mic_e::decode(b"S32UVT", b"!(_fn\"Oj/"),
        Err(MicEError::InvalidDataType { got: b'!' })
    );
    // BadLongitudeByte: degrees byte below 28 / minutes byte above
    // 28+69 / hundredths byte above 28+99.
    assert_eq!(
        mic_e::decode(b"S32UVT", b"`\x1b_fn\"Oj/"),
        Err(MicEError::BadLongitudeByte {
            got: 0x1B,
            position: 1
        })
    );
    assert_eq!(
        mic_e::decode(b"S32UVT", b"`(bfn\"Oj/"),
        Err(MicEError::BadLongitudeByte {
            got: b'b',
            position: 2
        })
    );
    assert_eq!(
        mic_e::decode(b"S32UVT", b"`(_\x80n\"Oj/"),
        Err(MicEError::BadLongitudeByte {
            got: 0x80,
            position: 3
        })
    );
    // BadSpeedCourseByte: below the +28 floor.
    assert_eq!(
        mic_e::decode(b"S32UVT", b"`(_f\x1b\"Oj/"),
        Err(MicEError::BadSpeedCourseByte {
            got: 0x1B,
            position: 4
        })
    );
    // An out-of-spec symbol table identifier does NOT fail the decode:
    // it says nothing about whether the position decoded, and real
    // traffic carries such bytes. The raw pair is preserved losslessly
    // and `Symbol::table()` reports `None` for the byte it cannot name.
    let odd = mic_e::decode(b"S32UVT", b"`(_fn\"Oj~").expect("position still decodes");
    assert_eq!(odd.symbol.to_wire(), (b'~', b'j'));
    assert!(odd.symbol.table().is_none(), "'~' is not a nameable table");
    // ...but encoding one is still rejected: we never transmit a table
    // identifier we would not accept.
    assert_eq!(
        MicE::new(
            odd.latitude,
            odd.longitude,
            odd.speed,
            odd.course,
            odd.symbol,
            odd.message
        ),
        Err(MicEError::BadSymbolTable { got: b'~' })
    );
    // BadAltitudeChar: '}' terminator present but a byte outside
    // base-91 before it.
    assert_eq!(
        mic_e::decode(b"S32UVT", b"`(_fn\"Oj/\x20AB}"),
        Err(MicEError::BadAltitudeChar {
            got: 0x20,
            position: 9
        })
    );
    // BadLatitude: 99 degrees from the destination digits.
    assert_eq!(
        mic_e::decode(b"Y92UVT", INFO),
        Err(MicEError::BadLatitude {
            got: 596_564 * warble::geo::UNITS_PER_HUNDREDTH_MINUTE
        })
    );
}

#[test]
fn encode_errors() {
    let mut dest = [0u8; 6];
    let mut info = [0u8; 32];
    // BadLongitude: exactly 180 degrees is inexpressible.
    let r = report(200_564, 1_080_000);
    assert_eq!(
        r.encode(&mut dest, &mut info),
        Err(MicEError::BadLongitude {
            got: 1_080_000 * warble::geo::UNITS_PER_HUNDREDTH_MINUTE
        })
    );
    // BadSpeed / BadCourse.
    let mut r = report(200_564, -672_774);
    r.speed = 800;
    assert_eq!(
        r.encode(&mut dest, &mut info),
        Err(MicEError::BadSpeed { got: 800 })
    );
    r.speed = 0;
    r.course = 361;
    assert_eq!(
        r.encode(&mut dest, &mut info),
        Err(MicEError::BadCourse { got: 361 })
    );
    // BadAmbiguity.
    let mut r = report(200_564, -672_774);
    r.ambiguity = 5;
    assert_eq!(
        r.encode(&mut dest, &mut info),
        Err(MicEError::BadAmbiguity { got: 5 })
    );
    // BadSymbolTable.
    let mut r = report(200_564, -672_774);
    r.symbol = Symbol::from_wire(b'~', b'>');
    assert_eq!(
        r.encode(&mut dest, &mut info),
        Err(MicEError::BadSymbolTable { got: b'~' })
    );
    // BadAltitude.
    let mut r = report(200_564, -672_774);
    r.altitude = Some(743_571);
    assert_eq!(
        r.encode(&mut dest, &mut info),
        Err(MicEError::BadAltitude { got: 743_571 })
    );
    // BufferTooSmall on both sides.
    let r = report(200_564, -672_774);
    assert_eq!(
        r.encode(&mut dest[..5], &mut info),
        Err(MicEError::BufferTooSmall { needed: 6, max: 5 })
    );
    assert_eq!(
        r.encode(&mut dest, &mut info[..8]),
        Err(MicEError::BufferTooSmall { needed: 9, max: 8 })
    );
}

/// Plain `AprsPacket::parse` rejects the Mic-E identifiers with the
/// existing typed error (dest context is required).
#[test]
fn aprs_parse_rejects_mic_e_ids() {
    use warble::aprs::{AprsError, AprsPacket};
    assert_eq!(
        AprsPacket::parse(INFO),
        Err(AprsError::InvalidDataType { got: b'`' })
    );
    assert_eq!(
        AprsPacket::parse(b"'(_fn\"Oj/"),
        Err(AprsError::InvalidDataType { got: b'\'' })
    );
}

/// The companion to [`aprs_parse_rejects_mic_e_ids`]: what a receiver
/// should call instead, and what each call says.
///
/// Mic-E stays out of [`AprsPacket`] because that type's invariant is
/// "every variant is something the crate can also *build*", and
/// `AprsPacket::build` writes the information field only while
/// `build_ui_frame` takes the destination from its caller — so an
/// `AprsPacket::MicE` would let a caller transmit a Mic-E field under a
/// contradicting tocall from a call that returned `Ok`. The frame-level
/// decode lives on [`Decoded`] instead, and the information-field-only
/// entry point says which half it is missing rather than claiming the
/// type is unimplemented.
#[test]
fn decoded_needs_the_destination_and_decode_frame_supplies_it() {
    use warble::aprs::{Decoded, DecodedKind};
    use warble::ax25::Address;

    // Without a destination: labelled `NeedsDestination`; bytes never lost.
    for info in [INFO, b"'(_fn\"Oj/"] {
        let d = Decoded::decode(info);
        assert_eq!(d.info, info);
        assert_eq!(
            d.kind,
            DecodedKind::NeedsDestination { dti: info[0] },
            "decode() must ask for the destination, not call Mic-E unsupported"
        );
        assert!(!d.is_typed());
        assert!(d.mic_e().is_none());
        assert!(d.packet().is_none());
    }

    // With it: the same report `mic_e::decode` produces, and the SSID
    // is ignored exactly as it is there.
    let expected = mic_e::decode(b"S32UVT", INFO).expect("spec vector decodes");
    for ssid in [0u8, 15] {
        let dest = Address::new(b"S32UVT", ssid).expect("valid destination");
        let d = Decoded::decode_frame(dest, INFO);
        assert_eq!(d.info, INFO);
        assert!(d.is_typed());
        assert!(d.packet().is_none(), "a Mic-E report is not an AprsPacket");
        assert_eq!(d.mic_e(), Some(&expected));
        assert!(matches!(d.kind, DecodedKind::MicE(_)));
    }

    // A destination the Mic-E alphabet rejects travels the *existing*
    // `Malformed` path, carrying the underlying `MicEError` rather than
    // needing a variant of its own. `APRS` is the case worth pinning:
    // it is the generic tocall, and pairing it with a Mic-E information
    // field is exactly the wrong-value-on-the-air mistake that keeps
    // Mic-E out of `AprsPacket`. It must be reported, not decoded.
    let generic = Address::new(b"APRS", 0).expect("valid destination");
    assert_eq!(
        Decoded::decode_frame(generic, INFO).kind,
        DecodedKind::Malformed {
            dti: b'`',
            error: MicEError::MixedMessageBits {
                got: [b'A', b'P', b'R'],
            }
            .into(),
        }
    );
    // … and a destination that is only outside the column alphabet
    // reports the column, unflattened.
    let bad = Address::new(b"S32AVT", 0).expect("valid destination");
    assert_eq!(
        Decoded::decode_frame(bad, INFO).kind,
        DecodedKind::Malformed {
            dti: b'`',
            error: MicEError::BadDestChar {
                got: b'A',
                column: 3,
            }
            .into(),
        }
    );
}

/// An out-of-range longitude byte stays rejected, and the tempting
/// one-bit repair is why.
///
/// From AC6VV-9, destination `S4PXYX`, off-air on the TNC Test CD:
/// six byte-identical frames across two of its tracks, each carrying
/// `0xBE` where the `d+28` longitude byte belongs. Chapter 10 puts that
/// byte in `38..=127`, so 190 is outside it and the report is refused.
///
/// # Why not just clear the high bit
///
/// `0xBE & 0x7F` is `0x3E`, which *is* in range, so masking looks like
/// an obvious repair for a station whose transmitter sets bit 7. It is
/// not, and this test exists to stop it being tried.
///
/// The same station sends the same destination valid frames that decode
/// to 34.149667 N, 118.133167 W, in the San Gabriel Valley. Clearing
/// bit 7 alone decodes instead to **134.133167 W**, roughly 1470 km due
/// west in the Pacific, and it does so *cleanly*: a well-formed Mic-E
/// report with no error for a caller to notice. Reaching the position
/// the station actually had needs bits 7 **and** 4 cleared, which is
/// not a decode but a guess checked against an answer already known
/// from other frames.
///
/// So the choice is between refusing six frames and publishing a
/// confident position 1470 km out to sea. These six stay refused, and
/// they are 0.27% of the corpus.
#[test]
fn an_out_of_range_longitude_byte_is_refused_rather_than_repaired() {
    use warble::aprs::{Decoded, DecodedKind};
    use warble::ax25::Address;

    let dest = Address::new(b"S4PXYX", 0).expect("valid destination");
    // The frame exactly as received, six times, on two tracks.
    const WIRE: &[u8] = b"\x60\xbe\x5f\x7f\x6c\x23\x35\x3e\x2f\x5d\x22\x36\x6e\x7d";

    let decoded = Decoded::decode_frame(dest, WIRE);
    assert!(
        matches!(
            decoded.kind,
            DecodedKind::Malformed { .. } | DecodedKind::Unsupported { .. }
        ),
        "0xBE is outside chapter 10's 38..=127, got {:?}",
        decoded.kind
    );
    assert!(!decoded.is_typed());
    assert_eq!(decoded.info, WIRE, "the bytes are still handed back");

    // The measurement behind the doc comment, so the argument is
    // checked rather than asserted. Both repairs decode; they disagree
    // by 16 degrees of longitude.
    let mut bit7 = WIRE.to_vec();
    bit7[1] = 0x3e; // clear bit 7 only
    let mut bits74 = WIRE.to_vec();
    bits74[1] = 0x2e; // clear bits 7 and 4

    let one_bit = Decoded::decode_frame(dest, &bit7);
    let DecodedKind::MicE(ref out_to_sea) = one_bit.kind else {
        panic!("clearing bit 7 produces a well-formed report, which is the hazard");
    };
    let two_bit = Decoded::decode_frame(dest, &bits74);
    let DecodedKind::MicE(ref on_land) = two_bit.kind else {
        panic!("clearing bits 7 and 4 reaches the station's real position");
    };

    let sea = hundredths(out_to_sea.longitude.units());
    let land = hundredths(on_land.longitude.units());
    assert_eq!(land, -(118 * 6000 + 7 * 100 + 99), "118 07.99 W, as sent");
    assert_eq!(
        sea,
        -(134 * 6000 + 7 * 100 + 99),
        "134 07.99 W, the Pacific"
    );
    assert_eq!(
        (land - sea) / 6000,
        16,
        "the two repairs differ by 16 degrees of longitude, and only one \
         of them is checkable against this station's other frames"
    );
}

/// A course the wire cannot mean is reported as unknown, not published.
///
/// Chapter 10 packs the course as `(DC mod 10) * 100 + SE`, which
/// reaches 999, and then says to subtract 400 if the result is 400 or
/// more. One subtraction leaves 400..=599 reachable, while the field is
/// defined over `0..=360`. A comment in the decoder used to assert
/// "course < 400 after the wrap", which is false, and
/// [`MicE::course`]'s own documentation promises `0..=360`, so the
/// decoder was breaking the invariant its own type states.
///
/// MEASURED over 205 635 live packets: 5 reports from three Swiss
/// stations decode to 366 or 466 degrees.
///
/// Reported as 0 rather than refused. Chapter 10 already spells 0 as
/// "unknown or indefinite", and the decoder declines to reject on an
/// out-of-spec symbol table byte for the same reason: a field the
/// sender got wrong says nothing about whether the position decoded.
/// Throwing away a good fix to punish a bad course would cost more than
/// it saves.
#[test]
fn a_course_outside_the_field_is_reported_as_unknown() {
    // Bytes 4..=6 are SP+28, DC+28, SE+28. The three below give a raw
    // course of 999, 936 and 855, all of which stay above 360 after the
    // single subtraction chapter 10 prescribes.
    for (sp, dc, se, raw) in [(0x21, 0x25, 0x7f, 999), (0x21, 0x25, 0x40, 936)] {
        let info = [
            0x60, 0x7d, 0x38, 0x67, sp, dc, se, 0x3e, 0x2f, 0x5d, 0x22, 0x35, 0x68, 0x7d,
        ];
        let report = mic_e::decode(b"S32UVT", &info).expect("the position still decodes");
        assert_eq!(
            report.course, 0,
            "a raw course of {raw} is not a direction and must not be published"
        );
        assert!(
            report.course <= 360,
            "MicE::course documents 0..=360 and the decoder must honour it"
        );
        // The point of not refusing: the fix survives.
        assert_ne!(report.latitude.units(), 0);
        assert_ne!(report.longitude.units(), 0);
    }

    // A legal course is untouched, including the 360 boundary that
    // chapter 10 distinguishes from 0.
    for (se, want) in [(0x53u8, 355u16), (0x58, 360)] {
        let info = [
            0x60, 0x7d, 0x38, 0x67, 0x26, 0x29, se, 0x3e, 0x2f, 0x5d, 0x22, 0x35, 0x68, 0x7d,
        ];
        let report = mic_e::decode(b"S32UVT", &info).expect("decodes");
        assert_eq!(report.course, want, "a legal course must survive verbatim");
    }
}
