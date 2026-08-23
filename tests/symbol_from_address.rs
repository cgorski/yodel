//! Chapter 20: the APRS symbol carried in the AX.25 **address** fields.
//!
//! Most APRS formats put the display symbol in the information field, as
//! a table + code pair. Two do not. Chapter 8 explains why: raw NMEA
//! beaconing "was a hack for early trackers with inadequate computing
//! resources", and "symbols had to go in the destination field using
//! names like `GPSxxx`". Chapter 20 adds a last-resort spelling in the
//! source address SSID. Neither is derivable from the information
//! field, so a decoder that never reads the addresses drops the icon.
//!
//! # Why this file exists as tier-2 vectors
//!
//! The destination spelling looks like a 188-row lookup chart
//! (94 codes x 2 tables), and a hand-typed 188-row chart is exactly the
//! sort of thing that is wrong in one cell and passes every test that
//! spot-checks five rows. It is not a chart: Appendix 2's mnemonics are
//! seven contiguous runs per table, with disjoint leading letters, so
//! the crate decodes them with arithmetic and no table at all.
//!
//! That makes the *chart* the independent oracle. `PRIMARY_XY` and
//! `ALTERNATE_XY` below are Appendix 2 transcribed row by row, in a
//! shape that shares no code with the implementation, and the totality
//! law drives every one of them through the decoder in every accepted
//! spelling. If a single run endpoint were off by one, the law fails on
//! the boundary row rather than passing quietly.
//!
//! The converse matters just as much: a false positive here does not
//! error out, it draws a *plausible wrong icon* on somebody's map. So
//! the sweep is exhaustive in both directions — every printable `xy`
//! pair outside the transcribed chart must decode to nothing.
#![cfg(feature = "aprs")]

use warble::aprs::Symbol;
use warble::aprs::symbol::{from_destination, from_source_ssid, resolve};

// ---------------------------------------------------------------------
// The oracle: APRS Symbols Appendix 2, transcribed
// ---------------------------------------------------------------------

/// The `xy` mnemonic of every **primary**-table symbol, indexed by
/// `code - b'!'`. Transcribed from the published chart; the crate
/// computes these instead of storing them, which is the point.
#[rustfmt::skip]
const PRIMARY_XY: [&[u8; 2]; 94] = [
    b"BB", b"BC", b"BD", b"BE", b"BF", b"BG", b"BH", b"BI", b"BJ",
    b"BK", b"BL", b"BM", b"BN", b"BO", b"BP", b"P0", b"P1", b"P2",
    b"P3", b"P4", b"P5", b"P6", b"P7", b"P8", b"P9", b"MR", b"MS",
    b"MT", b"MU", b"MV", b"MW", b"MX", b"PA", b"PB", b"PC", b"PD",
    b"PE", b"PF", b"PG", b"PH", b"PI", b"PJ", b"PK", b"PL", b"PM",
    b"PN", b"PO", b"PP", b"PQ", b"PR", b"PS", b"PT", b"PU", b"PV",
    b"PW", b"PX", b"PY", b"PZ", b"HS", b"HT", b"HU", b"HV", b"HW",
    b"HX", b"LA", b"LB", b"LC", b"LD", b"LE", b"LF", b"LG", b"LH",
    b"LI", b"LJ", b"LK", b"LL", b"LM", b"LN", b"LO", b"LP", b"LQ",
    b"LR", b"LS", b"LT", b"LU", b"LV", b"LW", b"LX", b"LY", b"LZ",
    b"J1", b"J2", b"J3", b"J4",
];

/// The `xy` mnemonic of every **alternate**-table symbol, indexed by
/// `code - b'!'`.
#[rustfmt::skip]
const ALTERNATE_XY: [&[u8; 2]; 94] = [
    b"OB", b"OC", b"OD", b"OE", b"OF", b"OG", b"OH", b"OI", b"OJ",
    b"OK", b"OL", b"OM", b"ON", b"OO", b"OP", b"A0", b"A1", b"A2",
    b"A3", b"A4", b"A5", b"A6", b"A7", b"A8", b"A9", b"NR", b"NS",
    b"NT", b"NU", b"NV", b"NW", b"NX", b"AA", b"AB", b"AC", b"AD",
    b"AE", b"AF", b"AG", b"AH", b"AI", b"AJ", b"AK", b"AL", b"AM",
    b"AN", b"AO", b"AP", b"AQ", b"AR", b"AS", b"AT", b"AU", b"AV",
    b"AW", b"AX", b"AY", b"AZ", b"DS", b"DT", b"DU", b"DV", b"DW",
    b"DX", b"SA", b"SB", b"SC", b"SD", b"SE", b"SF", b"SG", b"SH",
    b"SI", b"SJ", b"SK", b"SL", b"SM", b"SN", b"SO", b"SP", b"SQ",
    b"SR", b"SS", b"ST", b"SU", b"SV", b"SW", b"SX", b"SY", b"SZ",
    b"Q1", b"Q2", b"Q3", b"Q4",
];

/// The three interchangeable generic-destination prefixes: `GPS` for
/// general use, `SPC` for special events, `SYM` reserved. Chapter 20
/// states all three name the same symbols.
const PREFIXES: [&[u8; 3]; 3] = [b"GPS", b"SPC", b"SYM"];

/// Floor on the number of `(destination spelling, symbol)` pairs the
/// totality law checks.
///
/// A law expressed as a loop is only as good as the loop running. This
/// crate has been bitten before by a sweep that narrowed to nothing and
/// stayed green, so the count is asserted rather than assumed.
///
/// MEASURED 1786, from: 94 codes x 2 tables x 3 prefixes = 564 bare
/// mnemonics, the same 564 again with the explicit space filler in the
/// `z` slot, 94 x 2 numeric `GPSCnn`/`GPSEnn` spellings = 188, 94
/// alternate rows x 4 sampled overlay characters = 376, and 94 primary
/// rows rejected for carrying an overlay = 94. The floor sits a little
/// under the measurement so a real loss reports rather than noise.
const MIN_CASES: usize = 1750;

/// Renders address bytes for an assertion message.
fn text(bytes: &[u8]) -> &str {
    core::str::from_utf8(bytes).unwrap_or("<non-utf8>")
}

/// Renders a symbol as its two wire bytes for readable assertions.
fn wire(symbol: Option<Symbol>) -> Option<(char, char)> {
    symbol.map(|s| {
        let (table, code) = s.to_wire();
        (table as char, code as char)
    })
}

/// Builds a destination callsign from a prefix, a mnemonic and an
/// optional `z` character.
fn dest(prefix: &[u8; 3], xy: &[u8; 2], z: Option<u8>) -> Vec<u8> {
    let mut out = prefix.to_vec();
    out.extend_from_slice(xy);
    if let Some(z) = z {
        out.push(z);
    }
    out
}

// ---------------------------------------------------------------------
// Tier 2: the vectors chapter 20 states in prose
// ---------------------------------------------------------------------

/// Chapter 20, verbatim: "GPSBM, SPCBM, SYMBM and GPSC12 all specify a
/// 'Boy Scouts' icon (from the Primary Symbol Table), and GPSOM, SPCOM,
/// SYMOM and GPSE12 all specify a 'Girl Scouts' icon (from the
/// Alternate Symbol Table)."
#[test]
fn chapter_20_scouts_sentence() {
    for spelling in [&b"GPSBM"[..], b"SPCBM", b"SYMBM", b"GPSC12"] {
        assert_eq!(
            wire(from_destination(spelling)),
            Some(('/', ',')),
            "{} should be the primary Boy Scouts symbol",
            text(spelling)
        );
    }
    for spelling in [&b"GPSOM"[..], b"SPCOM", b"SYMOM", b"GPSE12"] {
        assert_eq!(
            wire(from_destination(spelling)),
            Some(('\\', ',')),
            "{} should be the alternate Girl Scouts symbol",
            text(spelling)
        );
    }
}

/// Chapter 20: "a tracker could use the Destination Address GPSMV_ or
/// GPS30 to specify a 'car' icon".
///
/// The second half of that sentence is a known typo in the published
/// text — the numeric spelling always carries its table letter, so the
/// car is `GPSC30`, and a bare `GPS30` is not a symbol at all (`3` is
/// not a leading letter of any mnemonic). Both readings are pinned
/// here so the typo cannot be "fixed" back in by a later reader.
#[test]
fn chapter_20_car_sentence_and_its_typo() {
    assert_eq!(wire(from_destination(b"GPSMV")), Some(('/', '>')));
    assert_eq!(wire(from_destination(b"GPSMV ")), Some(('/', '>')));
    assert_eq!(wire(from_destination(b"GPSC30")), Some(('/', '>')));
    assert_eq!(from_destination(b"GPS30"), None);
    assert_eq!(from_destination(b"GPS30 "), None);
}

/// Chapter 20: "if the 'car' icon is to be overlaid with a digit '3',
/// the Destination Address will be GPSNV3", and "even if the address is
/// overlay-capable, it is not actually necessary to specify an overlay;
/// e.g. GPSNV_".
#[test]
fn chapter_20_overlay_sentence() {
    // The overlay character becomes the table selector, exactly as it
    // does in an information field, and the code comes from the
    // alternate table.
    assert_eq!(wire(from_destination(b"GPSNV3")), Some(('3', '>')));
    assert_eq!(wire(from_destination(b"SPCNVW")), Some(('W', '>')));
    assert_eq!(wire(from_destination(b"GPSNV ")), Some(('\\', '>')));
    assert_eq!(wire(from_destination(b"GPSNV")), Some(('\\', '>')));

    // "None of the symbols in the Primary Symbol Table can be
    // overlaid": a primary mnemonic plus an overlay is a contradiction,
    // and guessing which half the sender meant is how a map ends up
    // with the wrong icon.
    assert_eq!(from_destination(b"GPSMV3"), None);

    // "GPSCnn and GPSEnn symbols can not have overlays" — there is no
    // room for one, and a seventh character is not an AX.25 address.
    assert_eq!(from_destination(b"GPSC303"), None);
}

/// The three destinations the corpus carries, and what the chart says
/// they mean.
#[test]
fn corpus_destinations() {
    assert_eq!(wire(from_destination(b"GPSLJ")), Some(('/', 'j'))); // Jeep
    assert_eq!(wire(from_destination(b"GPSLK")), Some(('/', 'k'))); // Truck
    assert_eq!(wire(from_destination(b"GPSMV")), Some(('/', '>'))); // Car
    // The same three, named: the crate's own chart has to agree with
    // what the address spelling resolves to, or the differential
    // against an outside decoder would be comparing two of our own
    // opinions.
    assert_eq!(from_destination(b"GPSLK"), Some(Symbol::TRUCK));
    assert_eq!(from_destination(b"GPSMV"), Some(Symbol::CAR));
}

/// Chapter 20's source-SSID table, all fifteen rows plus the two edges.
#[test]
fn chapter_20_source_ssid_table() {
    let expected: [(u8, char); 15] = [
        (1, 'a'),  // ambulance
        (2, 'U'),  // bus
        (3, 'f'),  // fire truck
        (4, 'b'),  // bicycle
        (5, 'Y'),  // yacht
        (6, 'X'),  // helicopter
        (7, '\''), // small aircraft
        (8, 's'),  // ship
        (9, '>'),  // car
        (10, '<'), // motorcycle
        (11, 'O'), // balloon
        (12, 'j'), // jeep
        (13, 'R'), // recreational vehicle
        (14, 'k'), // truck
        (15, 'v'), // van
    ];
    for (ssid, code) in expected {
        assert_eq!(
            wire(from_source_ssid(ssid)),
            Some(('/', code)),
            "SSID -{ssid}"
        );
    }
    // "-0  [no icon]" is the conventional default SSID, so reading a
    // symbol out of it would put an icon on every station on the band.
    assert_eq!(from_source_ssid(0), None);
    // No AX.25 address can carry more than four bits of SSID.
    for ssid in 16..=u8::MAX {
        assert_eq!(from_source_ssid(ssid), None, "SSID {ssid}");
    }
}

/// The named constants agree with the SSID chart, which is the one
/// place the two independently-written tables in this crate can be
/// cross-checked against each other.
#[test]
fn source_ssid_matches_the_named_constants() {
    assert_eq!(from_source_ssid(1), Some(Symbol::AMBULANCE));
    assert_eq!(from_source_ssid(2), Some(Symbol::BUS));
    assert_eq!(from_source_ssid(4), Some(Symbol::BICYCLE));
    assert_eq!(from_source_ssid(5), Some(Symbol::BOAT));
    assert_eq!(from_source_ssid(6), Some(Symbol::HELICOPTER));
    assert_eq!(from_source_ssid(7), Some(Symbol::AIRCRAFT));
    assert_eq!(from_source_ssid(9), Some(Symbol::CAR));
    assert_eq!(from_source_ssid(10), Some(Symbol::MOTORCYCLE));
    assert_eq!(from_source_ssid(11), Some(Symbol::BALLOON));
    assert_eq!(from_source_ssid(14), Some(Symbol::TRUCK));
}

// ---------------------------------------------------------------------
// The totality law
// ---------------------------------------------------------------------

/// Every symbol the mnemonic scheme can express round-trips from its
/// destination spelling, in every spelling chapter 20 accepts.
///
/// This is the law the arithmetic decomposition has to satisfy to be a
/// legitimate replacement for the published 188-row chart: for every
/// row `s` of that chart, and every way of writing `s` in a destination
/// address, `from_destination` returns exactly `s`.
#[test]
fn totality_over_every_charted_symbol() {
    let mut cases = 0usize;

    for (alternate, chart) in [(false, &PRIMARY_XY), (true, &ALTERNATE_XY)] {
        let table = if alternate { b'\\' } else { b'/' };
        for (index, xy) in chart.iter().enumerate() {
            let code = b'!' + u8::try_from(index).unwrap();
            let expected = Some((table as char, code as char));

            for prefix in PREFIXES {
                // `GPSxy`, as an AX.25 address arrives once its padding
                // is stripped...
                assert_eq!(
                    wire(from_destination(&dest(prefix, xy, None))),
                    expected,
                    "{}{} bare",
                    text(prefix),
                    text(*xy)
                );
                // ...and `GPSxy_`, with chapter 20's explicit space
                // filler still in the `z` slot.
                assert_eq!(
                    wire(from_destination(&dest(prefix, xy, Some(b' ')))),
                    expected,
                    "{}{} space-filled",
                    text(prefix),
                    text(*xy)
                );
                cases += 2;
            }

            // The numeric spelling of the same row: `nn` is the row
            // number `01..=94`, `C` primary and `E` alternate.
            let nn = index + 1;
            let numeric = format!("GPS{}{nn:02}", if alternate { 'E' } else { 'C' });
            assert_eq!(
                wire(from_destination(numeric.as_bytes())),
                expected,
                "{numeric}"
            );
            cases += 1;

            if alternate {
                // Every alternate row accepts an overlay, which
                // replaces the table selector and leaves the code.
                for overlay in [b'0', b'9', b'A', b'Z'] {
                    assert_eq!(
                        wire(from_destination(&dest(b"GPS", xy, Some(overlay)))),
                        Some((overlay as char, code as char)),
                        "GPS{} overlay {}",
                        text(*xy),
                        overlay as char
                    );
                    cases += 1;
                }
            } else {
                // No primary row accepts one.
                assert_eq!(
                    from_destination(&dest(b"GPS", xy, Some(b'7'))),
                    None,
                    "GPS{} must not take an overlay",
                    text(*xy)
                );
                cases += 1;
            }
        }
    }

    assert!(
        cases >= MIN_CASES,
        "the totality law compared only {cases} spellings, expected at \
         least {MIN_CASES} — the sweep narrowed and the law stopped \
         proving anything"
    );
}

/// The mnemonic scheme is injective: no two charted rows share an `xy`.
///
/// This is what lets `x` alone name the table. If it ever failed, one
/// mnemonic would mean two icons and the totality law above would be
/// satisfiable by a decoder that still answers wrongly half the time.
#[test]
fn every_charted_mnemonic_is_distinct() {
    let mut seen = std::collections::BTreeSet::new();
    for chart in [&PRIMARY_XY, &ALTERNATE_XY] {
        for xy in chart {
            assert!(seen.insert(*xy), "mnemonic {xy:?} appears twice");
        }
    }
    assert_eq!(seen.len(), 188);

    // The twelve leading letters partition cleanly by table, which is
    // the structural fact the decoder relies on.
    let primary_leads: std::collections::BTreeSet<u8> = PRIMARY_XY.iter().map(|xy| xy[0]).collect();
    let alternate_leads: std::collections::BTreeSet<u8> =
        ALTERNATE_XY.iter().map(|xy| xy[0]).collect();
    assert_eq!(primary_leads, [b'B', b'H', b'J', b'L', b'M', b'P'].into());
    assert_eq!(alternate_leads, [b'A', b'D', b'N', b'O', b'Q', b'S'].into());
    assert!(primary_leads.is_disjoint(&alternate_leads));
}

// ---------------------------------------------------------------------
// No false positives
// ---------------------------------------------------------------------

/// Every printable `xy` pair that is **not** in the chart decodes to
/// nothing, under all three prefixes.
///
/// The expensive direction to get right. A decoder that accepts one
/// letter too many at a run boundary does not fail — it silently
/// relabels somebody's station, and the frame still parses. The sweep
/// is exhaustive over printable ASCII so a boundary cannot hide.
#[test]
fn no_uncharted_mnemonic_decodes() {
    let charted: std::collections::BTreeSet<[u8; 2]> = PRIMARY_XY
        .iter()
        .chain(ALTERNATE_XY.iter())
        .map(|xy| **xy)
        .collect();

    let mut accepted = 0usize;
    let mut rejected = 0usize;
    for x in 0x20..=0x7eu8 {
        for y in 0x20..=0x7eu8 {
            let expected_charted = charted.contains(&[x, y]);
            for prefix in PREFIXES {
                let got = from_destination(&dest(prefix, &[x, y], None));
                assert_eq!(
                    got.is_some(),
                    expected_charted,
                    "{}{}{} decoded to {:?}",
                    text(prefix),
                    x as char,
                    y as char,
                    wire(got)
                );
                if expected_charted {
                    accepted += 1;
                } else {
                    rejected += 1;
                }
            }
        }
    }
    assert_eq!(accepted, 188 * 3);
    assert_eq!(rejected, 95 * 95 * 3 - 188 * 3);
}

/// The numeric spelling accepts exactly rows `01`..=`94` of the chart,
/// spelled with two digits, under the `GPS` prefix only.
#[test]
fn numeric_spelling_edges() {
    for table_letter in ['C', 'E'] {
        let table = if table_letter == 'C' { '/' } else { '\\' };
        for nn in 0..=99u32 {
            let spelling = format!("GPS{table_letter}{nn:02}");
            let got = wire(from_destination(spelling.as_bytes()));
            let expected = if (1..=94).contains(&nn) {
                let code = b'!' + u8::try_from(nn - 1).unwrap();
                Some((table, code as char))
            } else {
                None
            };
            assert_eq!(got, expected, "{spelling}");
        }
        // Not two digits: `nn` is a fixed-width field, and the leftover
        // characters are not a mnemonic either (`C` and `E` lead no
        // run).
        for spelling in ["GPSC1", "GPSC1X", "GPSCX1", "GPSC 1", "GPSC"] {
            assert_eq!(from_destination(spelling.as_bytes()), None, "{spelling}");
        }
        // Chapter 20 lists the numeric form only with the `GPS`
        // prefix; the mnemonic reading of `C1`/`E1` is empty too, so
        // there is no second interpretation to fall back on.
        for prefix in ["SPC", "SYM"] {
            let spelling = format!("{prefix}{table_letter}12");
            assert_eq!(from_destination(spelling.as_bytes()), None, "{spelling}");
        }
    }
}

/// Real destinations that are not symbol addresses.
///
/// Every one of these is traffic a receiver sees constantly. Decoding
/// a symbol from any of them would paint an icon on a station that
/// never asked for one.
#[test]
fn ordinary_destinations_name_no_symbol() {
    for spelling in [
        &b""[..],
        b" ",
        b"      ",
        b"APRS",
        b"APRS  ",
        b"APN123",
        b"APDW17",
        b"APZ001",
        b"BEACON",
        b"CQ",
        b"ID",
        b"TEST",
        b"WIDE1",
        b"GPS",
        b"GPS   ",
        b"SPC",
        b"SYM",
        b"GP",
        b"G",
        // Six characters of a mnemonic-shaped prefix that is not one.
        b"GPSXYZ",
        b"GPSZZZ",
        b"GPS???",
        b"SYMBOL",
        // Seven characters is not an AX.25 address at all.
        b"GPSMVXX",
        // Mic-E destinations are packed latitude, not mnemonics; these
        // are real ones from the corpus files.
        b"T2SX8Y",
        b"SUSXQR",
    ] {
        assert_eq!(
            from_destination(spelling),
            None,
            "{:?} should name no symbol",
            text(spelling)
        );
    }
}

/// A `z` character that is neither the space filler nor a legal overlay
/// leaves the address unreadable, and unreadable is not a licence to
/// guess.
#[test]
fn illegal_overlay_characters_are_not_guessed() {
    // Lowercase overlays exist only as the *compressed* information
    // field's `a`-`j` spelling; chapter 20 states the destination
    // range as "0-9 or A-Z" and nothing else.
    for z in [b'a', b'j', b'z', b'!', b'-', b'/', b'\\', b'#', 0x00, 0x7f] {
        assert_eq!(
            from_destination(&dest(b"GPS", b"NV", Some(z))),
            None,
            "GPSNV with z = {z:#04x}"
        );
    }
}

// ---------------------------------------------------------------------
// Precedence
// ---------------------------------------------------------------------

/// Chapter 20's *Symbol Precedence* worked example, which builds a
/// single frame claiming three different symbols at once:
/// `G3NRW-7 > GPSMV : !0123.45N/01234.56Wj` — small aircraft (source
/// SSID), car (destination) and jeep (information field).
#[test]
fn chapter_20_precedence_example() {
    let jeep = Symbol::from_wire(b'/', b'j');

    // "The symbol in the Information field takes precedence over any
    // other symbol."
    assert_eq!(resolve(Some(jeep), b"GPSMV", 7), Some(jeep));

    // "If there is no symbol in the Information field, the symbol in
    // the Destination Address takes precedence over the symbol in the
    // Source Address SSID."
    assert_eq!(resolve(None, b"GPSMV", 7), Some(Symbol::CAR));

    // Last resort.
    assert_eq!(resolve(None, b"APRS", 7), Some(Symbol::AIRCRAFT));

    // Nothing anywhere is a legitimate answer, not a fallback icon.
    assert_eq!(resolve(None, b"APRS", 0), None);
}

/// `resolve` is exactly the three lookups in order, for every
/// combination of the three sources being present or absent.
#[test]
fn precedence_is_total_over_the_eight_combinations() {
    let info = Symbol::from_wire(b'/', b'b');
    for information in [None, Some(info)] {
        for destination in [&b"APRS"[..], b"GPSMV"] {
            for ssid in [0u8, 9] {
                let expected = information
                    .or_else(|| from_destination(destination))
                    .or_else(|| from_source_ssid(ssid));
                assert_eq!(
                    resolve(information, destination, ssid),
                    expected,
                    "{information:?} / {destination:?} / -{ssid}"
                );
            }
        }
    }
}

/// The lookups are `const fn`, so a caller can name a symbol from an
/// address at compile time with no code emitted at all.
#[test]
fn usable_in_const_context() {
    const JEEP: Option<Symbol> = from_destination(b"GPSLJ");
    const CAR: Option<Symbol> = from_source_ssid(9);
    const RESOLVED: Option<Symbol> = resolve(None, b"GPSLK", 9);
    assert_eq!(wire(JEEP), Some(('/', 'j')));
    assert_eq!(CAR, Some(Symbol::CAR));
    assert_eq!(RESOLVED, Some(Symbol::TRUCK));
}
