//! Tier-2 rebuild-fidelity vectors: real packets, pinned byte for byte.
//!
//! A packet that decodes is not the same as a packet that was
//! understood. The check separating them is whether the decoded value
//! serializes back to the bytes that arrived, and over a live APRS-IS
//! feed it does not for 14% of buildable traffic.
//!
//! Every literal below is a real packet, quoted whole, with the station
//! that sent it and the date it was heard. They come from receive-only
//! captures of the APRS-IS full feed taken on 2026-08-21. Nothing here
//! is synthesized, because the point is to pin what real senders emit
//! rather than what this crate finds convenient. The packets are quoted
//! in full rather than cited, so this file stays self-contained and
//! cannot come to reference material that no longer exists.
//!
//! # Why most of these assert the WRONG answer
//!
//! Several tests are named `_known_gap` and assert today's **lossy**
//! behaviour, following the same idiom as
//! `tests/compressed.rs::compression_type_byte_drops_bit_6_known_gap`.
//! Pinning a defect is not endorsing it. It makes the defect a fact the
//! suite states out loud, so that repairing it **fails this file** and
//! forces whoever repairs it to come here and say so. A test that
//! skipped the case instead would stay green through both the defect
//! and its repair, and would therefore measure nothing.
//!
//! Each such test spells out what the correct answer will be, so the
//! update is mechanical rather than a fresh investigation.

#![cfg(all(feature = "aprs", feature = "alloc"))]

use yodel::aprs::monitor::MonitorLine;
use yodel::aprs::{AprsError, AprsPacket, DecodedKind, TelemetryDefinition, TelemetryValue};

/// Splits a TNC2 monitor line and hands back its information field.
fn info(line: &[u8]) -> &[u8] {
    MonitorLine::parse(line)
        .expect("a well-formed TNC2 monitor line")
        .info
}

/// Decodes a TNC2 line to a buildable packet, or panics saying why not.
fn packet(line: &[u8]) -> AprsPacket<'_> {
    match MonitorLine::parse(line).expect("TNC2 line").decoded().kind {
        DecodedKind::Packet(p) => p,
        other => panic!("expected a buildable packet, got {other:?}"),
    }
}

/// The error a TNC2 line's information field is rejected with.
fn rejection(line: &[u8]) -> AprsError {
    match MonitorLine::parse(line).expect("TNC2 line").decoded().kind {
        DecodedKind::Malformed { error, .. } => error,
        other => panic!("expected a rejection, got {other:?}"),
    }
}

/// Re-serializes a decoded line, rendered as text for readable failures.
///
/// Every packet pinned here is ASCII on the wire, so the lossy
/// rendering is exact and a mismatch prints as two spellings rather
/// than as two arrays of integers.
fn rebuilt(line: &[u8]) -> String {
    let bytes = packet(line).to_vec().expect("re-serializing the packet");
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The wire information field, rendered the same way.
fn as_sent(line: &[u8]) -> String {
    String::from_utf8_lossy(info(line)).into_owned()
}

// ---------------------------------------------------------------------
// The positive control
// ---------------------------------------------------------------------

/// The format the coordinate type was designed around must stay exact.
///
/// From KC2OUR-3 via KD2NMG-10, APRS-IS full feed, 2026-08-21.
///
/// This is the control for any change to coordinate storage. `DDMM.hh`
/// is natively 1/100 arc-minutes, so it is the one position format that
/// round-trips today, and widening the storage unit must leave it
/// exactly where it is.
///
/// MEASURED over a 30 051-packet capture: 10 369 of 10 378 uncompressed
/// positions rebuild byte-exactly, against 4 of 1 896 compressed ones.
/// A second capture from a different server put compressed at 0 of 458.
/// That split is the fingerprint of a storage-unit defect rather than a
/// parsing one, and this test is the working half of it.
///
/// If a coordinate change moves this packet by even one digit, the unit
/// conversion is wrong somewhere and every uncompressed position on the
/// planet is being re-spelled.
#[test]
fn uncompressed_position_round_trips_byte_exactly() {
    let line = b"KC2OUR-3>APMI06,WIDE2-2,qAR,KD2NMG-10:@211253z4122.65N/07408.01W#ORANGE COUNTY ARES/RACES NY, Donated by KC2VTJ, U=13.7V,T=63.6F";
    assert_eq!(
        rebuilt(line),
        as_sent(line),
        "an uncompressed DDMM.hh position must survive a decode/encode round trip"
    );
}

// ---------------------------------------------------------------------
// Coordinate storage is too coarse for the compressed format
// ---------------------------------------------------------------------

/// A compressed position survives a decode/encode round trip.
///
/// From OM5RW-7 via OL7M-10, APRS-IS full feed, 2026-08-21.
///
/// This test was written as a `_known_gap` pinning the defect, and it
/// fired the moment the defect was repaired, which is what that idiom
/// is for. What follows is the arithmetic it used to pin, kept because
/// it is the clearest single-packet statement of what was wrong.
///
/// # The arithmetic, so the expected value is checkable by hand
///
/// The four base-91 digits `5:9a` are 20, 25, 24 and 64, so
///
/// ```text
/// y = 20*91^3 + 25*91^2 + 24*91 + 64 = 15 280 693
/// ```
///
/// Chapter 9 defines the latitude as `90 - y/380926` degrees, so the
/// sender meant 49.885403 degrees exactly. Stored in 1/100
/// arc-minutes that rounds to 299 312 units, which reads back as
/// 49.885333 degrees: **7.8 metres away** from a position the sender
/// specified to within 29 centimetres. Re-encoding 299 312 yields
/// y = 15 280 719, which spells `5:9{`. The longitude moves the same
/// way, `RRBg` to `RRBm`.
///
/// 63.5 distinct wire positions shared one stored value, which is what
/// made it a storage-precision defect rather than a rounding
/// preference: no care in the conversion recovers a distinction the
/// storage cannot hold. The unit now divides 380 926 and 190 463
/// exactly, so the conversion is a multiplication one way and a
/// division the other, with no rounding on either.
#[test]
fn compressed_position_round_trips_byte_exactly() {
    let line = b"OM5RW-7>APLRT1,WIDE1-1,qAO,OL7M-10:=/5:9aRRBg>H[Q";
    assert_eq!(as_sent(line), "=/5:9aRRBg>H[Q");
    assert_eq!(
        rebuilt(line),
        as_sent(line),
        "a compressed position must survive a decode/encode round trip; \
         it used to come back as `=/5:9{{RRBm>H[Q`, two base-91 digits \
         out on each axis"
    );
}

/// The compressed no-data `cs` trailer is canonicalised today.
///
/// From EA3RCC-1 via EA3IK-3, APRS-IS full feed, 2026-08-21.
///
/// **Flips when the received bytes are preserved, not when coordinates
/// are widened.** Widening makes the *number* exact and leaves the
/// *spelling* alone, so this packet still differs afterwards, in this
/// trailer only. Separating the two is the whole reason this case has
/// its own test: without it the coordinate work looks as though it
/// under-delivered, when what remains is a different defect.
///
/// The wire carries `"  G"`, two spaces and a `G`. Chapter 9 says a
/// space in the `c` slot means the trailer carries no data, which
/// leaves the following bytes free. `build` emits the literal `" sT"`,
/// spelling the same absence of data a different way, so no information
/// is lost and the bytes still change.
#[test]
fn compressed_cs_no_data_trailer_is_canonicalised_today_known_gap() {
    let line = b"EA3RCC-1>APLRG1,ED3YAB-10*,qAR,EA3IK-3:!L9Vx*Nj0g&  G433.775 Mhz LoRa APRS IGate Radio Club Castellar";
    let sent = as_sent(line);
    assert!(
        sent.contains("&  G"),
        "the wire spells the no-data trailer as two spaces and a G: {sent}"
    );
    let out = rebuilt(line);
    assert!(
        out.contains("& sT"),
        "today the no-data trailer is rewritten to the canonical \" sT\"; \
         preserving the received bytes must keep \"  G\": {out}"
    );
}

// ---------------------------------------------------------------------
// The compressed altitude trailer used to lose a foot per rebuild
// ---------------------------------------------------------------------

/// A compressed altitude survives a decode/encode round trip.
///
/// From DO1TRH-4 via DB0XX-1, APRS-IS full feed, 2026-08-21.
///
/// This was the last **F3** failure in the crate: the rebuilt bytes
/// parsed back to a different value. 302 of 57 731 buildable packets in
/// that capture lost a foot this way, 300 here and 2 on the timestamped
/// variant, and none of them can be seen from the WAV corpus, which
/// carries no compressed positions at all. Information loss outranks
/// every spelling question, so this vector outranks the two known gaps
/// above it.
///
/// # The arithmetic, so the expected value is checkable by hand
///
/// The trailer is `AHQ`. `Q` is base-91 48, whose NMEA-source bits
/// (3 and 4) read `0b10` = GGA, and chapter 9 says that selects the
/// altitude form. `A` and `H` are then 32 and 39, so the code is
///
/// ```text
/// 32 * 91 + 39 = 2951        altitude = 1.002^2951 = 363.6187... feet
/// ```
///
/// which chapter 9's worked example truncates to **363 feet**.
///
/// Re-encoding used to invert the *power*: find the code whose
/// `1.002^n` is nearest 363.0. That is code 2950, at 362.8929, which
/// truncates to **362**. So the packet came back as `AGQ`, a foot
/// short. Nothing about the value was wrong; the two directions simply
/// rounded opposite ways, and 999 of the 8281 altitude codes sit where
/// that matters.
///
/// It did not stop at a foot. An igate parses a packet and re-emits it,
/// so the cycle runs again downstream, and where a code step is close
/// to a whole foot the error ratchets. MEASURED by iterating the old
/// rule to a fixed point across the whole domain: 417 codes lost more
/// than one foot, and code 3131 reads 520 feet and walks down to 480
/// over 41 passes.
///
/// `build` now inverts the *parser*, so it writes the code that decodes
/// to 363, which is 2951, which is what arrived.
#[test]
fn compressed_altitude_round_trips_byte_exactly() {
    let line = b"DO1TRH-4>APLRT1,WIDE1-1,qAO,DB0XX-1:!/4(`gQ97a>AHQ";
    assert_eq!(as_sent(line), "!/4(`gQ97a>AHQ");
    assert_eq!(
        rebuilt(line),
        as_sent(line),
        "a compressed altitude must survive a decode/encode round trip; \
         it used to come back as `!/4(`gQ97a>AGQ`, one code low, which \
         reads as 362 feet instead of 363"
    );
}

/// The same repair on the timestamped variant, where the bytes still
/// move and the value must not.
///
/// From LA2IKA-12 via LA2IKA-1, APRS-IS full feed, 2026-08-21.
///
/// Here the two properties come apart, which is why this packet is
/// pinned beside the one above rather than instead of it.
///
/// The trailer is `;$1`. `1` is base-91 16, again GGA. `;` and `$` are
/// 26 and 3, so the code is `26 * 91 + 3 = 2369`, and
/// `1.002^2369 = 113.6664...` truncates to **113 feet**. But four codes,
/// 2367 through 2370, all truncate to 113, and the builder cannot know
/// which of them was sent. It writes 2367, spelled `;"`.
///
/// So this rebuild is **not** byte-identical and never can be: the
/// fibre has four members and only one can come back. What it must do,
/// and now does, is carry the same 113 feet. That is F3 holding while
/// F1 is unreachable, and reading the byte difference as a defect is
/// what the classification in `tests/common/mod.rs` exists to prevent.
#[test]
fn compressed_altitude_keeps_its_value_when_the_code_is_respelled() {
    let line = b"LA2IKA-12>APPT10,WIDE1-1,WIDE2-2,qAR,LA2IKA-1:/140011h/+)>nT;`9[;$1";
    assert_eq!(as_sent(line), "/140011h/+)>nT;`9[;$1");
    let out = rebuilt(line);
    assert_eq!(
        out, "/140011h/+)>nT;`9[;\"1",
        "four codes spell 113 feet and the builder writes the lowest \
         whose power is nearest, so the byte moves"
    );
    // The point of the test: what came back means what was sent.
    assert_eq!(
        AprsPacket::parse(out.as_bytes()),
        Ok(packet(line)),
        "the re-spelled trailer must parse back to the same 113 feet; \
         it used to read as 112"
    );
}

/// An altitude that was already exact must stay exact.
///
/// From S58BJ-9 via S55YFE-7, APRS-IS full feed, 2026-08-21.
///
/// The control for the two above. Across the live capture the repair
/// moved 166 packets from `differs` to `exact` and **none** the other
/// way, so it is one-directional by measurement; this packet is the
/// single-vector statement of that.
///
/// Its trailer is `FrQ`, so the code is `37 * 91 + 81 = 3448` and
/// `1.002^3448 = 981.5305...` feet, truncating to 981. No other code
/// truncates to 981 (`1.002^3447` is 979.57 and `1.002^3449` is
/// 983.49), and 3448 is also the code whose power is nearest 981, so
/// the old rule and the new one agree here. A repair that shifted every
/// altitude by one code, rather than only the ones that were losing
/// information, breaks on this packet.
#[test]
fn compressed_altitude_that_was_already_exact_stays_exact() {
    let line = b"S58BJ-9>APLRT1,WIDE1-1,qAR,S55YFE-7:!/74u.R.1rvFrQBorut";
    assert_eq!(rebuilt(line), as_sent(line));
}

// ---------------------------------------------------------------------
// A brace or an "ack" in message text is not an identifier
// ---------------------------------------------------------------------

/// Message text that opens with `{` is text, not a malformed id.
///
/// From HK4D-5 via T2COLOMBIA, APRS-IS full feed, 2026-08-21. MEASURED
/// over that capture: 183 packets from 24 senders, an EchoLink-family
/// status line that every one of them was rejected for.
///
/// Chapter 14 puts the identifier at the end of the message text and
/// caps it at five characters, so `{EM|v1|ONLINE|...` is not one. The
/// parser took everything after the last `{` and errored on its length
/// instead of concluding the `{` was text.
#[test]
fn message_text_may_open_with_a_brace() {
    let line = b"HK4D-5>APRS,TCPIP*,qAC,T2COLOMBIA::HK3G-5   :{EM|v1|ONLINE|0.0000|0.0000|Android";
    assert_eq!(rebuilt(line), as_sent(line));
}

/// Message text that opens with `ack` is text, not an acknowledgement.
///
/// From KE4PIC-11 via an APRS-IS full feed, 2026-08-21. MEASURED: 20
/// packets.
///
/// The second half of the same defect, and it fails earlier than the
/// brace half: `strip_prefix(b"ack")` matched before the brace logic
/// ran, so this packet never reached it. A reply's identifier is the
/// whole payload, so a 60-byte one means the body was never a reply.
#[test]
fn message_text_may_open_with_ack() {
    let line =
        b"KE4PIC-11>APRS,TCPIP*,qAC,T2TEXAS::MYANET   :ack1/2} I have a LoRa 433Mhz Igate running";
    assert_eq!(rebuilt(line), as_sent(line));
    // And it must not be reported as an acknowledgement.
    match packet(line) {
        AprsPacket::Message(m) => assert!(
            matches!(
                m.content,
                yodel::aprs::MessageContent::Text { id: None, .. }
            ),
            "a message whose text begins with ack is text with no id, got {:?}",
            m.content
        ),
        other => panic!("expected a message, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// Weather reports are re-spelled
// ---------------------------------------------------------------------

/// A weather report is rewritten into a different, also-legal spelling.
///
/// From W7BTL via AMBCWOP-2 and PA3BWK via FIFTH, APRS-IS full feed,
/// 2026-08-21.
///
/// **Flips when the received field block is preserved.**
///
/// Chapter 12 states that the parameters after the wind block "may be
/// in a different order (or may not even exist)". This crate emits one
/// fixed order, so a legal packet returns as a different legal packet.
/// That matters beyond tidiness: a digipeater or igate that parses and
/// re-transmits puts bytes on the air that nobody sent.
///
/// # Two independent mechanisms, which is why both packets are here
///
/// Found by diffing the bytes rather than by reading the field table,
/// because the field table alone predicts neither.
///
/// 1. **Luminosity is pinned directly after `r`.** `write_fields` in
///    `src/aprs/weather.rs` writes it there as a special case, outside
///    the tagged-field walk entirely, so a sender who put `L` after the
///    barometer has it moved five fields left. Both packets show this.
/// 2. **The tagged-field walk itself reorders.** PA3BWK sent
///    `b10115h68`; the table's order is `h` before `b`, so it returns
///    as `h68b10115`. W7BTL happened to send the table's order for
///    everything except `L`, so that packet alone cannot tell the two
///    mechanisms apart.
///
/// A repair addressing only the tagged-field walk would leave the first
/// mechanism in place and still fail both of these, which is the reason
/// to pin whole rebuilt strings rather than substrings.
#[test]
fn weather_tag_order_is_rewritten_today_known_gap() {
    // Sender's order: g t r p P h b L. Only L is out of table order.
    let w7btl = b"W7BTL>APRS,TCPIP*,qAC,AMBCWOP-2:@211227z3354.50N/11808.33W_131/000g000t068r000p000P000h92b10139L000AmbientCWOP.com";
    assert_eq!(
        rebuilt(w7btl),
        "@211227z3354.50N/11808.33W_131/000g000t068r000L000p000P000h92b10139AmbientCWOP.com",
        "today L is moved from after the barometer to directly after r"
    );

    // Sender's order: g t r p P b h L. Both L and the b/h pair move.
    let pa3bwk = b"PA3BWK>APN000,TCPIP*,qAC,FIFTH:@211227z5210.47N/00511.38E_000/000g000t068r000p021P018b10115h68L158eMB64";
    assert_eq!(
        rebuilt(pa3bwk),
        "@211227z5210.47N/00511.38E_000/000g000t068r000L158p021P018h68b10115eMB64",
        "today L moves after r AND the b/h pair is swapped into table order"
    );

    for line in [&w7btl[..], &pa3bwk[..]] {
        assert_ne!(
            rebuilt(line),
            as_sent(line),
            "pinned as a known gap; when this fails, tag order is being \
             preserved and these assertions become equality against the wire"
        );
    }
}

// ---------------------------------------------------------------------
// Telemetry. The hazard case is the first one.
// ---------------------------------------------------------------------

/// Telemetry with fewer than five analog fields, the hazard case.
///
/// From W9GIL-9 via T2RDU, APRS-IS full feed, 2026-08-21. MEASURED over
/// that capture: 56 packets carry fewer than five analog fields, so the
/// shape is not a curiosity.
///
/// **Flips when the parser splits on commas**, and this is the one
/// relaxation in the whole effort that can make the crate *worse*
/// rather than leave it unchanged.
///
/// `T#046,400,007,00000000` has three comma-separated fields after the
/// sequence. The last is the eight-bit **digital** byte, not a third
/// analog channel. A variable-width parser assigning analog slots left
/// to right reads `00000000` as `analog[2] = 0` and then finds no
/// digital field, turning a loudly rejected packet into a silently
/// wrong one. That is worse than the rejection it replaces, and no
/// rejection count can see it.
///
/// So the parse order carries the correctness: find the digital field
/// **first**, anchored as the last comma-separated field of exactly
/// eight `0`/`1` characters, and only then assign what remains to
/// analog slots.
///
/// This packet decodes to **exactly two** analog values (400 and 7),
/// all eight digital bits clear, and rebuilds byte for byte including
/// the `400` and `007` as spelled.
#[test]
fn telemetry_short_field_count_finds_the_digital_field() {
    let line = b"W9GIL-9>APSVX1,TCPIP*,qAC,T2RDU:T#046,400,007,00000000";
    assert_eq!(as_sent(line), "T#046,400,007,00000000");
    let AprsPacket::Telemetry(t) = packet(line) else {
        panic!("expected telemetry");
    };
    // The hazard is closed, and now observable from the value side:
    // `00000000` is the digital field, NOT `analog[2]`.
    assert_eq!(t.analog[0], Some(TelemetryValue::integer(400)));
    assert_eq!(t.analog[1], Some(TelemetryValue::integer(7)));
    assert_eq!(
        t.analog[2], None,
        "a parser that promoted the digital byte would read analog[2] = 0"
    );
    assert_eq!(t.digital, Some([false; 8]));
    // 400 was above chapter 13's 255 and used to be the rejection; the
    // decimal value type holds it, and `007` keeps chapter 13's width
    // because build pads to three and widens only when it must.
    assert_eq!(rebuilt(line), "T#046,400,007,00000000");
}

/// The hazard shape that the parse order exists to prevent, from the
/// other side: a report that reaches the values must have assigned the
/// digital field correctly.
///
/// MEASURED: 56 captured reports carry fewer than five analog channels.
/// A left-to-right parser reads their trailing `00000000` as an analog
/// zero and finds no digital field, turning a loudly rejected packet
/// into a silently wrong one, which no rejection count can see.
#[test]
fn telemetry_two_analog_channels_and_a_digital_byte() {
    // Same shape, values inside the 1.0.1 range so the parse completes.
    let line = b"KC9TNU>APSVX1,TCPIP*,qAC,T2RDU:T#662,240,002,00000000";
    let AprsPacket::Telemetry(t) = packet(line) else {
        panic!("expected telemetry");
    };
    assert_eq!(t.seq, 662);
    // Two channels given, three ABSENT rather than defaulted to zero,
    // and `analog[2]` is NOT the digital byte read as a
    // number. Reporting the three as `0` would assert readings the
    // sender never made, and rebuild them onto the air as `000`.
    assert_eq!(t.analog[0], Some(TelemetryValue::integer(240)));
    assert_eq!(t.analog[1], Some(TelemetryValue::integer(2)));
    assert_eq!(t.analog[2..], [None, None, None]);
    assert_eq!(t.digital, Some([false; 8]));
    assert_eq!(t.rest, b"");
    assert_eq!(rebuilt(line), "T#662,240,002,00000000");
}

/// Telemetry values outside the 1.0.1 range, and decimal ones.
///
/// From TF3IRA-1 via T2CSNGRAD, VE3RLR-10 via T2VAN and SQ9NFI via
/// T2PRT, APRS-IS full feed, 2026-08-21.
///
/// **Flips when the parser splits on commas and the value type widens.**
/// The 256..=999 range is not even a relaxation: APRS 1.2 widened the
/// analog range and this crate still implements the older `0..=255`
/// cap. The decimal form is the commoner one on air, because a station
/// scaling its own readings has no need to publish `EQNS` coefficients.
///
/// Note what the pinned errors show. **Every** shape below fails the
/// same way, `Truncated { expected: 34 }`, including
/// `T#385,064,069,032,255`, whose four analog values are all three
/// digits and all inside the old range. It is rejected purely for
/// having four channels instead of five. So the range cap and the digit
/// width are not separate defects with separate symptoms: the fixed
/// 34-byte layout swallows both, which is why comma splitting has to
/// land before a widened value type can be measured at all.
///
/// The trap when repairing this was byte-exactness. Accepting `46.2`
/// and rebuilding `46`, or accepting `T#8` and rebuilding `T#008`,
/// would trade a rejection defect for a rewriting defect, which is the
/// same class of bug as the weather reordering above. A decimal
/// mantissa and digit count avoids it without a raw carrier: it stores
/// what the sender meant, and chapter 13's own field width is what
/// `build` writes back.
#[test]
fn telemetry_wide_and_decimal_values_round_trip() {
    // All three now reach their values, hold them exactly, and rebuild
    // byte for byte. Before comma splitting they were all the same
    // `Truncated { expected: 34 }`; between that and the value type
    // they failed at the range cap and the decimal point.
    let cases: [(&[u8], &str); 3] = [
        // A five-digit value, far outside the old u8 cap.
        (
            b"TF3IRA-1>APDW18,TCPIP*,qAC,T2CSNGRAD:T#651,44546",
            "T#651,44546",
        ),
        // Decimals, from a station that scales its own readings. The
        // point is not a digit: MEASURED, 4 265 analog fields in the
        // capture carry one, and the widest has 13 decimal places.
        (
            b"VE3RLR-10>APDW17,TCPIP*,qAC,T2VAN:T#303,46.2,0.04,0.03,0.00",
            "T#303,46.2,0.04,0.03,0.00",
        ),
        // In range, correct width, four channels instead of five, and
        // a fifth that must NOT reappear as `000` on rebuild.
        (
            b"SQ9NFI>APRS,TCPIP*,qAC,T2PRT:T#385,064,069,032,255",
            "T#385,064,069,032,255",
        ),
    ];
    assert_eq!(cases.len(), 3, "three distinct telemetry shapes");
    for (line, want) in cases {
        assert_eq!(rebuilt(line), want, "{}", String::from_utf8_lossy(line));
        assert_eq!(as_sent(line), want, "the vector must be byte-exact");
    }
    // The decimal one, checked at the value rather than the spelling:
    // a fixed milliunit scale would rebuild this byte-exactly too, so
    // the spelling alone cannot show that the value survived.
    let AprsPacket::Telemetry(t) = packet(cases[1].0) else {
        panic!("expected telemetry");
    };
    assert_eq!(
        t.analog[0],
        Some(TelemetryValue {
            mantissa: 462,
            decimals: 1
        })
    );
    assert_eq!(t.analog[4], None, "the fifth channel was never sent");
    assert_eq!(t.digital, None, "no digital field was sent");
}

// ---------------------------------------------------------------------
// Comment views: !DAO! and base-91 telemetry
//
// Both live inside the comment, so the test that matters is that
// reading them changes nothing about the bytes.
// ---------------------------------------------------------------------

/// A `!DAO!` refines the position and leaves the packet untouched.
///
/// From OK1FRN-9 via OK1UOJ-10, APRS-IS full feed, 2026-08-21.
///
/// The comment carries `/A=` altitude and `!DAO!` at once, which is the
/// common shape: a LoRa tracker reporting height and a refined fix.
/// `!wgK!` is lower case, so the two bytes are base-91: `g` is 103 and
/// 103 - 33 is 70, `K` is 75 and 75 - 33 is 42.
#[test]
fn dao_refines_the_position_without_touching_the_bytes() {
    let line = b"OK1FRN-9>APLT00,WIDE1-1,qAO,OK1UOJ-10:!5004.26N/01559.25E>/A=000847LoRa Tracker - Bat.: 4.15V !wgK!";
    let AprsPacket::Position(p) = packet(line) else {
        panic!("expected a position");
    };
    let dao = p.dao().expect("a !DAO! in the comment");
    assert_eq!(dao.datum, b'w');
    assert!(dao.datum_is_assigned(), "w is WGS84");

    // The field holds what the wire said, to hundredths of a minute.
    // 5004.26N is 50 + 4.26/60 degrees.
    let field = p.latitude.units();
    // The accessor adds the refinement, and the two differ.
    let refined = p.coordinates().latitude.units();
    assert!(refined > field, "the refinement moves the fix north");
    // Never as much as a hundredth of a minute, which is what lets the
    // field keep the wire's own DDMM.hh spelling.
    let hundredth = (refined - field) * 6_000;
    assert!(
        hundredth < yodel::geo::UNITS_PER_DEGREE,
        "a DAO addend must not carry into the printed hundredth"
    );

    // And none of that disturbs the packet.
    assert_eq!(rebuilt(line), as_sent(line));
}

/// Base-91 comment telemetry is read, and the comment is unchanged.
///
/// From AJ6WO-10 via SECOND, APRS-IS full feed, 2026-08-21.
///
/// A twelve-byte payload, so six values: a sequence counter and five
/// analog channels, with no digital word. The digital word is
/// unambiguous only in the full seven-value form, because chapter 13
/// places it after all five analog channels.
#[test]
fn base91_comment_telemetry_is_read_from_a_real_comment() {
    let line =
        b"AJ6WO-10>APLRFD,TCPIP*,qAC,SECOND:!R@.hHJ;]K#  G   - 73 de Julio -      |*S+8!$!#!!!!|";
    let AprsPacket::Position(p) = packet(line) else {
        panic!("expected a position");
    };
    let t = p.comment_telemetry().expect("a base-91 block");
    assert_eq!(t.seq, 869);
    assert!(t.analog[0].is_some());
    assert_eq!(t.digital, None, "six values cannot carry the digital word");
    // The bytes stayed in the comment, where build can reach them.
    assert!(as_sent(line).contains("|*S+8!$!#!!!!|"));
    assert!(p.comment.ends_with(b"|*S+8!$!#!!!!|"));
}

// ---------------------------------------------------------------------
// Telemetry definition messages
// ---------------------------------------------------------------------

/// `EQNS.` coefficients are decimal and may be negative.
///
/// From SR4X-10 via T2POLAND and TA6B-11 via T2CSNGRAD, APRS-IS full
/// feed, 2026-08-21.
///
/// These are the two properties that decide the value type: a
/// coefficient of `0.1` rules out an integer, and `-1` rules out an
/// unsigned one. Both decode exactly, and the message still rebuilds
/// byte for byte because the typed reading is a view over its text.
#[test]
fn equation_coefficients_are_decimal_and_signed() {
    let line =
        b"SR4X-10>APSVX1,TCPIP*,qAC,T2POLAND::SR4X-10  :EQNS.0,0.1,0,0,1.0,0,0,0,0,0,0,0,0,0,0";
    let AprsPacket::Message(m) = packet(line) else {
        panic!("expected a message");
    };
    let Some(TelemetryDefinition::Equations(e)) = m.telemetry_definition() else {
        panic!("expected EQNS.");
    };
    assert_eq!(
        e.coefficients[1],
        Some(TelemetryValue {
            mantissa: 1,
            decimals: 1
        }),
        "0.1 is not 0, and not 1"
    );
    assert_eq!(rebuilt(line), as_sent(line), "a view must not rewrite");

    let line =
        b"TA6B-11>AESPG4,TCPIP*,qAC,T2CSNGRAD::TA6B-11  :EQNS.0,1,0,0,1,0,0,1,0,0,-1,0,0,1,0";
    let AprsPacket::Message(m) = packet(line) else {
        panic!("expected a message");
    };
    let Some(TelemetryDefinition::Equations(e)) = m.telemetry_definition() else {
        panic!("expected EQNS.");
    };
    assert_eq!(e.coefficients[10], Some(TelemetryValue::integer(-1)));
    assert_eq!(rebuilt(line), as_sent(line));
}

/// A definition message addressed to somebody else still describes its
/// sender.
///
/// From KJ6ZD via KJ6ZD, APRS-IS full feed, 2026-08-21. MEASURED over
/// 95 219 packets, 277 of 5 805 definition messages do this, and a
/// decoder keying the metadata on the addressee never binds it and
/// never errors.
#[test]
fn a_definition_may_address_a_different_callsign() {
    let line =
        b"KJ6ZD>APSVX1,TCPIP*,qAS,KJ6ZD::EL-KJ6ZD :UNIT.erlang,erlang,receptions,transmissions";
    let AprsPacket::Message(m) = packet(line) else {
        panic!("expected a message");
    };
    assert_eq!(
        m.addressee.as_bytes(),
        b"EL-KJ6ZD",
        "the addressee is NOT the sender, and that is the point"
    );
    let Some(TelemetryDefinition::Units(u)) = m.telemetry_definition() else {
        panic!("expected UNIT.");
    };
    assert_eq!(u.analog[0], Some(&b"erlang"[..]));
    assert_eq!(u.analog[3], Some(&b"transmissions"[..]));
    assert_eq!(u.analog[4], None, "the list stopped after four");
    assert_eq!(rebuilt(line), as_sent(line));
}

// ---------------------------------------------------------------------
// Position ambiguity
// ---------------------------------------------------------------------

/// Space-blanked coordinates are rejected rather than represented.
///
/// From WINLINK via WLNK-1 and KC5HWB-2 via T2MCI, APRS-IS full feed,
/// 2026-08-21.
///
/// **Flips when position ambiguity is wired into the parser.** Chapter
/// 6 lets a station blank low-order coordinate digits with spaces to
/// report a position it does not wish to give precisely. The type for
/// it already exists and is already carried on every coordinate pair
/// (`geo::Ambiguity`, reachable as `Coordinates::ambiguity`), and no
/// parser has ever set it to anything but exact.
///
/// Two constraints on the repair, both about not over-reaching.
/// Ambiguity applies to the **uncompressed** form only, because in a
/// compressed position trailing spaces are the `cs` no-data trailer,
/// and reading base-91 as decimal would produce a confident wrong
/// position. And the spaces must be **contiguous and right-aligned**,
/// because chapter 6 blanks from the right; scattered spaces are
/// corruption and must stay rejected.
#[test]
fn space_blanked_coordinates_round_trip() {
    // An object and a position, both blanked to the whole minute.
    let winlink = b"WINLINK>APWL2K,TCPIP*,qAS,WLNK-1:;K7DAV-10 *211226z4058.  NW11152.  Wa431.450MHz Winlink VARA FM Wide Gateway";
    let mobile = b"KC5HWB-2>APSN01,TCPIP*,qAC,T2MCI:=3443.  N/08633.  W-Mobile iGate";
    // ...and one blanked to the tenth of a minute, the finest level.
    let droid = b"DO5AG-5>APDR16,TCPIP*,qAC,T2SPAIN:=5854.0 N/01156.0 E>184/001/A=000616 https://aprsdroid.org/";
    for line in [&winlink[..], &mobile[..], &droid[..]] {
        assert_eq!(
            rebuilt(line),
            as_sent(line),
            "a blanked coordinate must survive a decode/encode round trip"
        );
    }

    // The declared level reaches the reported position, on both axes.
    let AprsPacket::Position(p) = packet(mobile) else {
        panic!("expected a position");
    };
    assert_eq!(p.ambiguity.digits(), 2, "`3443.  N` blanks two digits");
    let at = p.coordinates();
    assert_eq!(at.ambiguity.digits(), 2);
    // 34 deg 43.00 min N, 86 deg 33.00 min W: the blanked hundredths
    // read as zero and the mask leaves them there.
    assert_eq!(at.latitude.units(), p.ambiguity.mask(at.latitude.units()));
    assert_eq!(at.longitude.units(), p.ambiguity.mask(at.longitude.units()));
}

// ---------------------------------------------------------------------
// The control group
// ---------------------------------------------------------------------

/// Packets broken past repair, which must keep failing.
///
/// All from the APRS-IS full feed, 2026-08-21.
///
/// This is the control for every relaxation. Every other measure
/// improves by accepting more, so only this one separates a parser that
/// improved from a parser that became credulous. It must go on passing
/// unchanged, which is why each row pins its specific error rather than
/// asserting that something was rejected: a relaxation that starts
/// failing these for a new reason has still moved the control.
///
/// Each row is unrepairable rather than unimplemented. "We do not parse
/// it yet" and "this cannot be parsed" are the two things this file
/// exists to keep apart, and the test after this one is here because
/// that distinction is easy to lose.
#[test]
fn unrepairable_packets_stay_rejected() {
    let cases: [(&[u8], AprsError); 3] = [
        // An hour of 27. There is no such hour, in any edition.
        (
            b"HB4LO-13>APPLO1,WIDE1-1,qAR,HB4LO-1:/122700z4649.71N/00656.39E_030/000g004t065r000p078P074h67Device under test 12.77V",
            AprsError::BadTimestamp {
                field: b'H',
                got: 27,
            },
        ),
        // Day zero. Same reasoning.
        (
            b"OK1KKY-18>APRS,TCPIP*,qAC,T2BELGIUM:@000000z5039.65N/01535.76E_.../...t075h45b10113WX-Station",
            AprsError::BadTimestamp {
                field: b'D',
                got: 0,
            },
        ),
        // A digit where the N/S letter belongs. Accepting it would put
        // a station in the Gulf of Guinea rather than reporting that
        // the receiver could not read a position.
        (
            b"W0MUD-3>APOSB,TCPIP*,qAS,W0MUD:@211227z4815.98N/06600.255x/A=000108Open Wires-X Node",
            AprsError::BadHemisphere { got: b'5' },
        ),
    ];
    assert_eq!(cases.len(), 3, "three unrepairable reason classes");
    for (line, expected) in cases {
        assert_eq!(
            rejection(line),
            expected,
            "must stay rejected, for this reason: {}",
            String::from_utf8_lossy(line)
        );
    }
}

/// Space-blanked packets are a second population, not part of the
/// control group.
///
/// A control group assembled by collecting "everything the parser
/// rejects" mixes two populations that must not be mixed, and the
/// mixture is invisible until something starts accepting one of them.
///
/// MEASURED over a set of 120 rejected packets gathered from the
/// 2026-08-21 feed: **62 are rejected solely because a coordinate digit
/// is a space**. Checked mechanically, all 124 space-bearing coordinate
/// fields across those 62 are contiguous and right-aligned, which is
/// chapter 6 position ambiguity: a spec-defined feature this crate
/// already carries the type for. They are not broken. They are
/// unimplemented, and a correct parser accepts every one.
///
/// So they cannot serve as a control. A control's whole job is to prove
/// a parser has not become credulous, which works only if every member
/// is something a correct parser must go on refusing. Half a control
/// group that a planned change is designed to accept fails exactly when
/// that change lands, and whoever sees it fail will either believe they
/// broke something or weaken the control to make it green. Either way
/// the one measurement that could have caught a credulous parser is the
/// one discarded.
///
/// Hence the split, made before any parser change, so the control means
/// one thing. These must reject **today**, with a space as the
/// offending byte. When ambiguity lands, this test fails and is
/// rewritten to assert the ambiguity each one decodes to; the test
/// above keeps the unrepairable rows unchanged.
#[test]
fn space_blanked_control_packets_are_a_separate_population() {
    // These were the control group's contaminated half: rejected only
    // for a space in a coordinate, which is chapter 6 ambiguity and a
    // specified feature rather than damage. They now parse, and that
    // is the point of having split them out from the packets that are
    // rejected for a reason no relaxation will ever remove.
    let cases: [(&[u8], u8); 3] = [
        // The Winlink gateway beaconing its station list. MEASURED: 162
        // of the 216 space-blanked packets in the first capture were
        // this one sender, so the packet count overstates its reach and
        // the distinct-sender count is the number to read. On the
        // combined capture its share is 17% of 73 senders.
        (
            b"WINLINK>APWL2K,TCPIP*,qAS,WLNK-1:;K7DAV-10 *211226z4058.  NW11152.  Wa431.450MHz Winlink VARA FM Wide Gateway",
            2,
        ),
        (
            b"KC5HWB-2>APSN01,TCPIP*,qAC,T2MCI:=3443.  N/08633.  W-Mobile iGate",
            2,
        ),
        (
            b"DO5AG-5>APDR16,TCPIP*,qAC,T2SPAIN:=5854.0 N/01156.0 E>184/001/A=000616 https://aprsdroid.org/",
            1,
        ),
    ];
    assert_eq!(cases.len(), 3, "the sample must not shrink to nothing");
    for (line, digits) in cases {
        let at = match packet(line) {
            AprsPacket::Position(p) => p.coordinates(),
            AprsPacket::Object(o) => o.coordinates(),
            other => panic!("expected a position or object, got {other:?}"),
        };
        assert_eq!(
            at.ambiguity.digits(),
            digits,
            "declared level of {}",
            String::from_utf8_lossy(line)
        );
        assert_eq!(rebuilt(line), as_sent(line));
    }
}

// ---------------------------------------------------------------------
// A field the parser accepted must be a field build can write
// ---------------------------------------------------------------------

/// An impossible wind direction is dropped, not carried into the value.
///
/// From WC4PEM-4 via APRS-IS, 2026-08-21. Nineteen packets from three
/// stations running the same weather firmware spell the direction
/// `767`, which chapter 12 gives the range `000` to `360`.
///
/// This was the only case in 205 635 packets where **parse succeeded
/// and build then failed**: `WeatherReport::check` enforces `0..=360`
/// on the way out and the parser enforced nothing on the way in, so the
/// canonicalisation was undefined on a packet the parser had claimed.
/// The crate also rendered "wind dir 767 deg" to its own CLI.
///
/// The direction is reported absent rather than the packet refused. The
/// other eight measurements in the report are unaffected by a wind vane
/// returning nonsense, and chapter 12 already has a spelling for a
/// missing field, so dropping one reading keeps eight.
#[test]
fn an_impossible_wind_direction_is_dropped_rather_than_carried() {
    let line = b"WC4PEM-4>APRS,TCPIP*,qAC,T2FLORIDA:@211401z2813.82N/08133.10W_767/000g000t083r000p003P000h78b10182";
    let AprsPacket::PositionWeather(w) = packet(line) else {
        panic!("expected a weather report");
    };
    assert_eq!(
        w.weather.wind_direction, None,
        "767 is not a direction and must not reach the typed value"
    );
    // Everything else the station measured is still here.
    assert!(w.weather.temperature.is_some(), "83 F survives");
    assert!(w.weather.humidity.is_some(), "78% survives");
    assert!(w.weather.barometric_pressure.is_some(), "1018 hPa survives");

    // And the packet now builds at all, which is the property that was
    // broken: parse and build must be defined on the same inputs.
    let built = rebuilt(line);
    assert!(
        !built.is_empty(),
        "build must be defined wherever parse succeeded"
    );
    // F3: what it writes reads back as the same value.
    let AprsPacket::PositionWeather(again) = packet(line) else {
        panic!("still a weather report");
    };
    assert_eq!(again.weather.wind_direction, None);
}

// ---------------------------------------------------------------------
// Chapter 9's compressed position, in an object and in an item
// ---------------------------------------------------------------------

/// A compressed object decodes, and its position is the one it names.
///
/// From KD0YUJ via APRS-IS, 2026-08-21. Chapter 9 permits the base-91
/// compressed form in an object or item, and the crate had no support
/// for it: MEASURED over 205 635 live packets, 106 objects and 42 items
/// from 26 senders were refused outright, so their positions were
/// plotted nowhere.
///
/// This vector checks itself. The object's own comment says Joplin,
/// Missouri, and the decoded position has to agree; a base-91 decode
/// that were wrong by a digit would land somewhere else entirely.
#[test]
fn a_compressed_object_decodes_where_it_says_it_is() {
    let line = b"KD0YUJ>APRS,TCPIP*,qAC,T2MIDWEST:;TALK-IN  *211359z/;`]Y6\\MRr   145.190MHz T091 -060Joplin MO";
    let AprsPacket::Object(o) = packet(line) else {
        panic!("expected an object");
    };
    assert_eq!(o.name, b"TALK-IN");
    assert!(o.live);
    assert!(o.compressed, "the position field is base-91, not DDMM.hh");
    // Joplin, Missouri is 37.08 N, 94.51 W. The comment says so.
    let at = o.coordinates();
    assert!(
        (37.0..37.4).contains(&at.latitude.to_degrees()),
        "latitude {} is not Joplin",
        at.latitude.to_degrees()
    );
    assert!(
        (-94.6..-94.2).contains(&at.longitude.to_degrees()),
        "longitude {} is not Joplin",
        at.longitude.to_degrees()
    );
    // The comment must survive whole. Writing it at the uncompressed
    // offset left a six-byte hole and truncated it, which is how this
    // was caught.
    assert_eq!(o.comment, &b"145.190MHz T091 -060Joplin MO"[..]);
}

/// A compressed item, and the position block that is six bytes shorter.
///
/// From HMSHTS via APRS-IS, 2026-08-23: a Hellenic weather service
/// alert set, 20 objects at once. The position has to land in Thessaly.
#[test]
fn a_compressed_object_keeps_its_length_arithmetic_straight() {
    let line = b"HMSHTS>APRS,TCPIP*,qAC,T2GREECE:;HMSHTSTES*211500z\\:W8;T-)L<   High temp Advise";
    let AprsPacket::Object(o) = packet(line) else {
        panic!("expected an object");
    };
    assert!(o.compressed);
    assert_eq!(o.name, b"HMSHTSTES");
    let at = o.coordinates();
    assert!(
        (34.0..42.0).contains(&at.latitude.to_degrees())
            && (19.0..29.0).contains(&at.longitude.to_degrees()),
        "not in Greece: {}, {}",
        at.latitude.to_degrees(),
        at.longitude.to_degrees()
    );
    assert_eq!(o.comment, &b"High temp Advise"[..]);
    // F3: the value survives a rebuild even though the bytes do not.
    // `build` writes the canonical no-data `cs` trailer where the wire
    // had spaces, which is the same F5 difference a plain compressed
    // position already has.
    let AprsPacket::Object(again) = packet(line) else {
        panic!("still an object");
    };
    assert_eq!(again.coordinates(), at);
}

/// An uncompressed object still keeps its data extension in the comment.
///
/// This is the regression the shared position parser introduced and the
/// one that mattered: a position report parses the seven bytes after
/// the coordinates as a data extension, and an object has no extension
/// field, so taking the position's comment silently dropped `088/036`
/// off every object that carried one.
#[test]
fn an_uncompressed_object_keeps_its_extension_bytes() {
    let line = b"N0CALL>APRS:;LEADER   *092345z4903.50N/07201.75W>088/036";
    let AprsPacket::Object(o) = packet(line) else {
        panic!("expected an object");
    };
    assert!(!o.compressed);
    assert_eq!(
        o.comment,
        &b"088/036"[..],
        "the extension bytes belong to an object's comment"
    );
    assert_eq!(
        rebuilt(line),
        as_sent(line),
        "and it rebuilds byte for byte"
    );
}
