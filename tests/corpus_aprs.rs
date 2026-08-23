//! APRS-layer coverage over the real off-air corpus.
//!
//! `tests/benchmark.rs` pins how many AX.25 frames the demodulator
//! recovers from the corpus recordings. It never asks whether those
//! frames then *parse as APRS* — so the whole `aprs` module was
//! previously unexercised against real traffic, and only ever tested
//! against hand-written spec examples.
//!
//! This test closes that loop: every frame the receiver emits is pushed
//! through `RxFrame::decoded`, the total frame-level decode —
//! destination address included, because Mic-E lives outside
//! `AprsPacket` and needs it. The result is a structured-coverage ratio
//! over real off-air VHF traffic.
//!
//! Note what the totals below do and do not say. `AprsPacket::parse` is
//! only one of the decoders counted; the others are Mic-E, raw NMEA,
//! Ultimeter and third-party. The lines are labelled accordingly,
//! because they were not always and the resulting overstatement of
//! `AprsPacket`'s reach outlived the measurement that produced it.
//!
//! The pinned ratio below is a floor, not a target. A large share of
//! real traffic is currently *not* representable by this crate's types
//! (raw NMEA `$`, third-party `}`, station capabilities `<`, and
//! non-APRS beacons), and several supported types reject spec-legal or
//! commonplace forms. Raising the floor is the point; see the gap
//! analysis in `docs/APRS_CONFORMANCE.md`.
//!
//! The corpus is operator-provided and gitignored, so this test is
//! `#[ignore]`d and passes with a message when `corpus/` is absent:
//!
//! ```text
//! cargo test --all-features --test corpus_aprs -- --ignored --nocapture
//! ```
#![cfg(all(feature = "tnc", feature = "micE"))]

mod common;

use std::collections::BTreeMap;
use std::path::Path;

use common::{Asymmetry, classify};

use warble::SampleRate;
use warble::aprs::{AprsPacket, DataExtension, DecodedKind};
use warble::tnc::{DefaultTncReceiver, TncConfig};

/// Corpus tracks, mirroring `tests/benchmark.rs`.
const FILES: &[&str] = &[
    "01_40-Mins-Traffic_-on-144.39.wav",
    "02_100-Mic-E-Bursts-DE-emphasized.wav",
    "03_100-Mic-E-Bursts-Flat.wav",
    "04_25-MIns-Drive-Test.wav",
];

/// Pinned floor for frames that yield *some* structured APRS value
/// (`AprsPacket` or `MicE`), as a percentage of all decoded frames.
///
/// This is a ratchet: raise it whenever the parser improves, never
/// lower it. The floor sits a little under the measurement so ordinary
/// demodulator jitter cannot fail the build.
///
/// | Measured | Change |
/// |---|---|
/// | 71.2% (1554/2182) | first measurement |
/// | 74.3% (1621/2182) | weather unit-code trailers, weather-symbol positions with non-wind comments, and message ids with a trailing CR all stopped being rejected |
/// | 91.8% (2004/2182) | raw NMEA, Ultimeter weather, third-party encapsulation and station capabilities implemented |
/// | 93.0% (2030/2182) | Mic-E stopped rejecting reports over an out-of-spec symbol table byte |
/// | **93.1% (2032/2182)** | hemisphere letters accepted case-insensitively on receive |
///
/// This figure counts every AX.25 frame heard, including the ones that
/// are not APRS. It therefore has a ceiling below 100% that no parser
/// improvement can reach, which is why
/// [`MIN_APRS_STRUCTURED_PERCENT`] exists beside it.
///
/// # What the remaining 6.9% is
///
/// Measured frame by frame rather than assumed. Of the 150 frames that
/// yield no typed value, **75 are not APRS at all** and 75 are APRS
/// this crate refuses on purpose:
///
/// | count | why | should it decode? |
/// |---|---|---|
/// | 75 | plain-text beacons: 42 station identifications to `ID`, 23 beacon texts to `BEACON`, 4 firmware banners to `UIDIGI`, 6 human-written weather bulletins | **Not APRS**, so not a parser failure. Chapter 5's table rules out `A`-`S`, `U`-`Z`, `a`-`z` and `0`-`9` as identifiers, and these frames open with one. They now decode as `DecodedKind::Text` and are excluded from the APRS denominator |
/// | 10 | an information field holding one CR and nothing else | **No** — there is no content, APRS or otherwise |
/// | 58 | `!0000.000/00000.000>…` | **No** — a GPS with no fix, sending `'0'` where the hemisphere belongs. Decoding it would place the station at 0,0 in the Gulf of Guinea instead of reporting that it has no position |
/// | 6 | Mic-E, `BadLongitudeByte { got: 190 }` | **No** — an FCS-valid frame carrying a 0xBE where a longitude byte belongs |
/// | 1 | bytes corrupted mid-frame (`BadDigit`) | **No** — an FCS-valid frame whose payload is visibly damaged |
///
/// The first row used to read "85 frames with data-type identifiers
/// `W`, `K`, `L`, `U`, `0x0d`, `0x20`", which was wrong twice over.
/// Those bytes are not data-type identifiers: chapter 5 marks their
/// ranges "[Do not use]", so the frames are not APRS and have no
/// identifier to report. And filing them beside real parse failures
/// put non-APRS traffic in the denominator of an APRS coverage figure.
/// Both are fixed: 75 of the 85 are now positively classified as text,
/// and the 10 that are a bare CR stay unclassified because they have no
/// content.
///
/// So every one of the remaining 150 is traffic that *ought* not to
/// produce an APRS value. Do not chase this number upward by loosening
/// validation: the 58 no-fix beacons are exactly the case where
/// accepting bad input produces confidently wrong output.
const MIN_STRUCTURED_PERCENT: f64 = 93.0;

/// Pinned floor for structured coverage of the frames that **are**
/// APRS, which is the figure that measures the parser.
///
/// [`MIN_STRUCTURED_PERCENT`] divides by every AX.25 frame heard on
/// 144.39 MHz, and some of that traffic is not APRS: station
/// identifications, TNC beacon banners, human-written bulletins. Those
/// frames can never yield an APRS value however good the parser gets,
/// so counting them against it measures the channel rather than the
/// crate. This ratchet divides by APRS frames alone.
///
/// The two are kept side by side rather than one replacing the other.
/// The all-frames figure is the one a user cares about ("what fraction
/// of what I hear does this understand?"); this one is the one a
/// contributor cares about ("what fraction of APRS do we still get
/// wrong?"). Reporting only the second would flatter the crate.
///
/// | Measured | Change |
/// |---|---|
/// | **96.4% (2032/2107)** | first measurement, when `DecodedKind::Text` made the non-APRS frames countable |
///
/// The 75 frames short of 100% are the 58 no-fix beacons, 6 Mic-E
/// reports carrying an out-of-range longitude byte, 10 empty fields and
/// 1 visibly corrupted payload. All four should stay rejected, so this
/// figure is near its true ceiling.
const MIN_APRS_STRUCTURED_PERCENT: f64 = 96.0;

/// Ceiling on frames set aside as non-APRS.
///
/// [`MIN_APRS_STRUCTURED_PERCENT`] can be flattered by classifying more
/// traffic as text, because that shrinks its denominator. This bounds
/// the shrinking: if text classification ever starts swallowing APRS,
/// the count rises and the build fails.
const MAX_NON_APRS_FRAMES: usize = 80;

/// Pinned floor for total frames, so a demodulator regression cannot
/// quietly satisfy the ratio above by decoding fewer, easier frames.
const MIN_FRAMES: usize = 2100;

/// Pinned floors for **field-level** decoding, one row per structured
/// field that used to be handed back as opaque comment text.
///
/// These are the complement of [`MIN_STRUCTURED_PERCENT`], and they
/// exist because that percentage is blind to them: a position report
/// yields a typed value whether or not its course, speed, wind, antenna
/// capability and altitude were understood, so 76% of the content of
/// real comments could be — and was — discarded at 93% "coverage".
///
/// MEASURED at the time of writing, over 2182 off-air frames:
///
/// | field | frames |
/// |---|---|
/// | `/A=` altitude | 258 |
/// | course/speed | 199 |
/// | `PHG` | 139 |
/// | wind (extension or weather report) | 82 |
/// | Mic-E altitude | 829 |
/// | Mic-E altitude behind a device prefix | 606 |
/// | Mic-E device prefix (with or without altitude) | 641 |
/// | status: timestamp | 78 |
///
/// Chapter 16's other two structured status forms — a leading
/// Maidenhead locator and a trailing `^HP` beam heading — are **0** in
/// this corpus and have no row. They are HF and meteor-scatter
/// features and 144.39 MHz VHF traffic does not carry them, so what
/// covers them is the tier-2 suite in `src/aprs/status.rs`, which
/// asserts every row of the published ERP table. A corpus is a sample,
/// not a specification.
///
/// The wind row is the one to watch. `ddd/sss` is byte-identical for
/// course/speed and wind; only the symbol code distinguishes them, so a
/// parser that ignores the symbol silently reports 54 wind readings as
/// vehicle course and speed. Floors are set a little under the
/// measurement so demodulator jitter cannot fail the build.
///
/// The three Mic-E rows are the newest, and the second and third exist
/// because the first cannot fail alone. Chapter 10 puts the optional
/// altitude after any device-identifier prefix (`>`, `]`, `` ` ``,
/// `'`), and this crate read only the unprefixed spelling until
/// `tests/aprs_differential.rs` compared altitudes with an independent
/// decoder: **73% of the Mic-E altitudes in this corpus sit behind a
/// prefix**, and every one of them was silently discarded while every
/// round-trip test passed. Pinning the prefixed count separately is
/// what makes a re-regression visible rather than a 27% dip in a
/// number nobody reads.
///
/// The third row splits the prefix off the altitude, because chapter 10
/// does: 35 frames carry a prefix with **no altitude field at all**
/// (`]Stopped`, `]`, `]Palomar REACT Digi`), and the prefix row could
/// not see them while its predicate and its label disagreed. The
/// altitude-behind-a-prefix row now says what it measures — altitude
/// *and* prefix, 606 — and the new row counts prefixes however they
/// are spelled, 641. Both must hold: 606 alone would go on passing if
/// the bare-prefix reading were reverted, and 641 alone would go on
/// passing if the altitude behind it were dropped again.
///
/// That row's floor is the measurement exactly, not a little under it.
/// The whole signal it guards is 35 frames out of 641, so the usual
/// slack would swallow most of a regression before the assert fired;
/// and unlike the demodulator-sensitive rows, this one is a pure
/// function of frames already decoded, so there is no jitter to absorb.
const MIN_FIELDS: &[(&str, usize)] = &[
    ("/A= altitude", 250),
    ("data extension: course/speed", 190),
    ("data extension: PHG", 130),
    ("wind", 78),
    ("Mic-E altitude", 800),
    ("Mic-E altitude behind a device prefix", 590),
    ("Mic-E device prefix", 641),
    ("status: timestamp", 75),
];

/// Pinned floors for **rebuild exactness**, per packet kind.
///
/// A frame that decodes is not a frame that was understood. These rows
/// ask the stronger question: re-serialize what we decoded, and does it
/// equal the bytes that arrived? A parser reading a field wrongly still
/// yields a typed value, so neither the coverage percentage nor the
/// field counts above can see it, and a relaxation that starts
/// accepting packets by misreading them shows up here and nowhere else.
///
/// Each row is `(kind, minimum % exact, minimum frames compared)`. The
/// second floor matters as much as the first: every assertion here is
/// inside a loop over whatever the corpus happened to contain, so a kind
/// that fell to zero frames would pass a percentage test vacuously.
///
/// Raise these as defects close; never lower one.
///
/// # Two kinds of difference, and only one of them is a defect
///
/// A rebuild that does not match the wire is not automatically wrong,
/// and treating it as wrong leads somewhere bad. Chasing this figure to
/// 100% means preserving whatever the sender emitted, including the
/// things the specification forbids, which would make the builder
/// transmit malformed packets in order to improve a diagnostic. The
/// diagnostic exists to detect **misunderstanding**; a difference
/// caused by *correcting* the sender is not a misunderstanding.
///
/// So a difference is sorted into one of two buckets:
///
/// * **rewritten**: the sender's spelling was legal and this crate
///   chose a different legal one. That is a defect. Chapter 12 says
///   the weather parameters "may be in a different order", so
///   reordering them rewrites a valid packet into another valid packet
///   and any station forwarding it puts bytes on the air nobody sent.
/// * **normalised**: the sender's spelling was **not** legal and this
///   crate emits the legal one. That is correct, and pinning it as a
///   failure would be pinning the bug. Chapter 14 says "Do not put any
///   carriage return (0x0d) or line feed (0x0a) at the end", and adds
///   that igates strip them "resulting in slightly different
///   contents", so the specification itself expects this difference.
///   Chapter 6 likewise specifies "the upper case letter N for north
///   or S for south".
///
/// The floors below are on `exact + normalised`, because those are the
/// correct outcomes, and `rewritten` is the number to drive to zero.
///
/// MEASURED at the time of writing, all of it found by diffing bytes
/// rather than by reading the parsers:
///
/// | kind | exact | term | case | rewritten | correct |
/// |---|---:|---:|---:|---:|---:|
/// | `Capabilities` | 8 | 0 | 0 | 0 | 100% |
/// | `Message` | 0 | **30** | 0 | 0 | **100%** |
/// | `Object` | 27 | 0 | 0 | 0 | 100% |
/// | `Position` | 345 | 0 | **2** | 0 | **100%** |
/// | `PositionTimestamped` | 115 | 0 | 0 | 0 | 100% |
/// | `Status` | 126 | 0 | 0 | 0 | 100% |
/// | `Weather` | 28 | 0 | 0 | 0 | 100% |
/// | `PositionWeather` | 32 | 0 | 0 | **50** | 39.0% |
///
/// The `Message` row is why this grew a classification rather than a
/// bigger number. Scored on byte-identity it reads as a catastrophic
/// 0%, the difference is one byte per frame, and it is invisible in any
/// diagnostic that prints the two strings side by side, because both
/// render identically. It is also **correct**, and the attempt to
/// "fix" it is instructive: preserving that byte would have grown
/// `encoded_len` for every caller, put a spec-forbidden byte back on
/// the air, made two messages with identical content compare unequal on
/// their terminator (so ack matching and dedup would see them as
/// different), and added a public field to a wire record. All to move a
/// diagnostic.
///
/// `Weather` reached 100% and `PositionWeather` went 4.9% to 39.0%
/// when absent fields stopped being written as dotted placeholders.
/// The two rows moved for different reasons, and the difference is the
/// whole point of the classification:
///
/// * `Weather`'s two failures were **F2**, the mandatory property. A
///   four-character temperature (`t-103`, which chapter 12 has no room
///   for) stopped the tag scan, the remainder went to `rest`, and build
///   wrote a placeholder run *before* it: 53 bytes in, 74 out, with
///   five tags appearing twice. That output is malformed. Omission
///   cannot lengthen a packet, so it cannot do that.
/// * `PositionWeather`'s remaining 50 are **F5**, which is optional:
///   tag order. The sender's `b10161h38` still returns as `h38b10161`.
///
/// The change also cost 413 packets on the live feed that spelled
/// absence with dots and now get it back omitted, against 786 gained
/// that omitted. Both directions are legal spellings of the same
/// value, so that trade is F5 either way and the F2 argument decides
/// it. MEASURED: 1 308 live weather reports have both a non-empty
/// `rest` and an absent standard field, which is the population where
/// placeholders insert synthetic bytes into the middle of content the
/// sender did send.
///
/// Closing the last 50 needs the received order, which is F5 and costs
/// the diagnostic; see the raw-carrier discussion in
/// `docs/APRS_CONFORMANCE.md` §4.
const MIN_REBUILD_EXACT: &[(&str, f64, usize)] = &[
    ("Position", 100.0, 340),
    ("PositionTimestamped", 100.0, 110),
    ("PositionWeather", 35.0, 78),
    ("Weather", 100.0, 26),
    ("Status", 100.0, 120),
    ("Message", 100.0, 28),
    ("Object", 100.0, 25),
    ("Capabilities", 100.0, 7),
];

/// Frames whose rebuild does not parse back to the value it was built
/// from, which is information loss rather than re-spelling.
///
/// Zero on this corpus, and pinned at zero.
///
/// This floor proved nothing for a while, and it is worth recording why
/// rather than trusting it. The live APRS-IS feed was **not** zero: 302
/// of 57 731 buildable packets there failed this, all of them
/// compressed positions carrying an altitude in the `cs` trailer, where
/// parse truncates `1.002^n` to whole feet and build then picked the
/// code nearest that foot count, which is one lower. This corpus
/// carries no compressed positions at all, so it stayed green through
/// the whole defect and through its repair.
///
/// Both feeds are now at zero. The guard that can see this class is the
/// tier-2 pair in `tests/rebuild_fidelity.rs`
/// (`compressed_altitude_round_trips_byte_exactly` and
/// `compressed_altitude_keeps_its_value_when_the_code_is_respelled`),
/// plus the whole-domain sweep in `src/aprs/position.rs`. A ratchet
/// over a sample cannot cover what the sample does not contain.
const MAX_VALUE_CHANGED: usize = 0;

fn variant(p: &AprsPacket<'_>) -> &'static str {
    match p {
        AprsPacket::Position(_) => "Position",
        AprsPacket::PositionCs(_) => "PositionCs",
        AprsPacket::PositionTimestamped(_) => "PositionTimestamped",
        AprsPacket::PositionWeather(_) => "PositionWeather",
        AprsPacket::Weather(_) => "Weather",
        AprsPacket::Telemetry(_) => "Telemetry",
        AprsPacket::Object(_) => "Object",
        AprsPacket::Item(_) => "Item",
        AprsPacket::Status(_) => "Status",
        AprsPacket::Message(_) => "Message",
        AprsPacket::Capabilities(_) => "Capabilities",
        _ => "other",
    }
}

/// Names a Data Type Identifier, including the ones this crate does not
/// implement — the point of the histogram is to show what we are missing.
fn dti_name(b: u8) -> String {
    match b {
        b'!' => "! position (no ts, no msg)".into(),
        b'=' => "= position (no ts, msg)".into(),
        b'/' => "/ position (ts, no msg)".into(),
        b'@' => "@ position (ts, msg)".into(),
        b'_' => "_ weather (positionless)".into(),
        b'T' => "T telemetry".into(),
        b';' => "; object".into(),
        b')' => ") item".into(),
        b'>' => "> status".into(),
        b':' => ": message".into(),
        b'`' => "` Mic-E (current)".into(),
        b'\'' => "' Mic-E (old / TM-D700 current)".into(),
        b'}' => "} third-party".into(),
        b'?' => "? query".into(),
        b'<' => "< station capabilities".into(),
        b'$' => "$ raw NMEA / Ultimeter".into(),
        b'{' => "{ user-defined".into(),
        b'#' | b'*' => "# or * Peet Bros".into(),
        b'%' => "% Agrelo DF".into(),
        b',' => ", invalid or test data".into(),
        b'[' => "[ Maidenhead beacon (obsolete)".into(),
        0x1c | 0x1d => "0x1c/0x1d Mic-E (obsolete beta)".into(),
        c if c.is_ascii_graphic() => format!("{} (unassigned / non-APRS beacon)", c as char),
        c => format!("0x{c:02x} (non-graphic)"),
    }
}

/// Track 03 of the source recording is one Mic-E burst copied and pasted
/// 100 times, so every decoded frame must be **byte-identical**. That
/// makes it a ground-truth probe for content corruption, which a frame
/// *count* cannot detect: a bit-flip repair that turns a damaged burst
/// into a different-but-valid frame still counts as one frame.
///
/// Paired with `tests/false_positives.rs`, this closes the specificity
/// gap on real off-air audio rather than synthetic input.
#[test]
#[ignore = "requires the operator-provided corpus/ recordings"]
fn corpus_track3_frames_are_all_identical() {
    let path = Path::new("corpus").join(FILES[2]);
    if !path.is_file() {
        eprintln!("corpus absent — skipping");
        return;
    }
    let mut reader = hound::WavReader::open(&path).expect("opening corpus WAV");
    let rate = SampleRate::new(reader.spec().sample_rate).expect("corpus sample rate");
    let pcm: Vec<i16> = reader
        .samples::<i16>()
        .map(|s| s.expect("sample"))
        .collect();

    let cfg = TncConfig::bell_202(rate).expect("bell 202 config");
    let mut rx: DefaultTncReceiver = DefaultTncReceiver::new(cfg).expect("receiver");

    let mut distinct: BTreeMap<Vec<u8>, usize> = BTreeMap::new();
    let mut total = 0usize;
    for s in pcm {
        if let Some(frame) = rx.push_i16(s) {
            total += 1;
            let dest = frame.dest();
            let src = frame.src();
            let mut key = dest.callsign.as_bytes().to_vec();
            key.push(b'>');
            key.extend_from_slice(src.callsign.as_bytes());
            key.push(b':');
            key.extend_from_slice(frame.info());
            *distinct.entry(key).or_default() += 1;
        }
    }

    for (key, n) in &distinct {
        let rendered: String = key
            .iter()
            .map(|&b| {
                if b.is_ascii_graphic() || b == b' ' {
                    b as char
                } else {
                    '.'
                }
            })
            .collect();
        println!("  {n:>4}x  {rendered}");
    }

    assert_eq!(total, 100, "track 03 carries exactly 100 identical bursts");
    assert_eq!(
        distinct.len(),
        1,
        "track 03's 100 bursts are byte-identical, so decoding them must \
         yield exactly one distinct frame; {} distinct variants means a \
         corrupted frame passed the FCS",
        distinct.len()
    );
}

/// Re-serializes every corpus frame that decoded to a buildable packet
/// and pins how often the bytes come back unchanged.
///
/// This is the corpus-side counterpart of `tests/rebuild_fidelity.rs`,
/// which pins the same property on individual hand-picked packets. The
/// vectors there say *what* is wrong and *why*; this says *how much* of
/// real traffic it costs, and refuses to let that share fall.
///
/// Note what the corpus can and cannot speak for. It is VHF RF traffic,
/// so every information field fits an AX.25 frame and none of the
/// long-packet behaviour that APRS-IS carries is exercised here. It also
/// contains **no** base-91 compressed positions at all, so the largest
/// known rebuild defect is invisible to this test and is covered by the
/// tier-2 vectors instead. A corpus is a sample, not a specification.
#[cfg(feature = "alloc")]
#[test]
#[ignore = "requires the operator-provided corpus/ recordings"]
fn corpus_rebuild_exactness_never_regresses() {
    let dir = Path::new("corpus");
    if !dir.is_dir() {
        eprintln!("corpus/ absent — skipping APRS rebuild test");
        return;
    }

    // kind -> how each frame's rebuild compared with the wire
    let mut tally: BTreeMap<&'static str, BTreeMap<Asymmetry, usize>> = BTreeMap::new();
    let mut examples: BTreeMap<&'static str, String> = BTreeMap::new();

    for name in FILES {
        let path = dir.join(name);
        if !path.is_file() {
            eprintln!("corpus/{name} absent — skipping APRS rebuild test");
            return;
        }
        let mut reader = hound::WavReader::open(&path).expect("opening corpus WAV");
        let rate = SampleRate::new(reader.spec().sample_rate).expect("corpus sample rate");
        let cfg = TncConfig::bell_202(rate).expect("bell 202 config");
        let mut rx: DefaultTncReceiver = DefaultTncReceiver::new(cfg).expect("receiver");

        let pcm: Vec<i16> = reader
            .samples::<i16>()
            .map(|s| s.expect("sample"))
            .collect();
        for s in pcm {
            let Some(frame) = rx.push_i16(s) else {
                continue;
            };
            let info = frame.info().to_vec();
            let DecodedKind::Packet(p) = frame.decoded().kind else {
                continue;
            };
            let kind = variant(&p);
            let outcome = match p.to_vec() {
                Err(_) => Asymmetry::BuildFailed,
                Ok(ref built) => {
                    // Semantic idempotence first: a rebuild that does
                    // not parse back to the same value has lost
                    // information, and no spelling question outranks
                    // that.
                    let survived = AprsPacket::parse(built).as_ref() == Ok(&p);
                    classify(&info, built, survived)
                }
            };
            *tally.entry(kind).or_default().entry(outcome).or_default() += 1;
            if !outcome.is_acceptable()
                && let Ok(built) = p.to_vec()
            {
                examples.entry(kind).or_insert_with(|| {
                    // Render the first differing byte offset as well as
                    // the text. Some differences are invisible in a
                    // lossy string render (a trailing CR, a non-graphic
                    // byte), and printing only the text then shows two
                    // identical-looking lines, which sends a reader
                    // looking for a bug in the test.
                    let at = info
                        .iter()
                        .zip(built.iter())
                        .position(|(a, b)| a != b)
                        .unwrap_or(info.len().min(built.len()));
                    let (a, b) = (info.get(at).copied(), built.get(at).copied());
                    format!(
                        "{outcome:?}\n            wire  {:?} ({} bytes)\n            built {:?} ({} bytes)\n            \
                         first difference at byte {at}: wire {a:02x?} vs built {b:02x?}",
                        String::from_utf8_lossy(&info),
                        info.len(),
                        String::from_utf8_lossy(&built),
                        built.len()
                    )
                });
            }
        }
    }

    let count = |kind: &str, a: Asymmetry| -> usize {
        tally
            .get(kind)
            .and_then(|m| m.get(&a))
            .copied()
            .unwrap_or(0)
    };

    println!("\n== rebuild asymmetry by kind ==");
    println!(
        "  {:<22} {:>6} {:>7} {:>5} {:>10} {:>8} {:>7}  correct",
        "kind", "exact", "term", "case", "REWRITTEN", "VALUE", "failed"
    );
    for kind in tally.keys() {
        let e = count(kind, Asymmetry::Exact);
        let t = count(kind, Asymmetry::NormalisedTerminator);
        let c = count(kind, Asymmetry::NormalisedCase);
        let r = count(kind, Asymmetry::Rewritten);
        let v = count(kind, Asymmetry::ValueChanged);
        let f = count(kind, Asymmetry::BuildFailed);
        let total = e + t + c + r + v + f;
        let ok = (e + t + c) as f64 / total as f64 * 100.0;
        println!("  {kind:<22} {e:>6} {t:>7} {c:>5} {r:>10} {v:>8} {f:>7}  {ok:>6.1}%");
    }
    println!("\n  term = trailing CR/LF the spec forbids and the builder declines to emit");
    println!("  case = a hemisphere letter the spec requires in upper case");
    println!("  both are CORRECT; REWRITTEN and VALUE are the defects");

    println!("\n== first unacceptable rebuild per kind ==");
    for (kind, ex) in &examples {
        println!("  {kind}:\n            {ex}");
    }

    let value_changed: usize = tally
        .values()
        .filter_map(|m| m.get(&Asymmetry::ValueChanged))
        .sum();
    assert!(
        value_changed == MAX_VALUE_CHANGED,
        "{value_changed} frames rebuilt to something that parses back \
         DIFFERENTLY, floor is {MAX_VALUE_CHANGED}. That is information \
         loss rather than a spelling difference, and it outranks every \
         other row in this test. Raise the floor only to record a \
         defect being accepted on purpose, never to make the suite green."
    );

    for &(kind, floor_pct, floor_n) in MIN_REBUILD_EXACT {
        let e = count(kind, Asymmetry::Exact);
        let t = count(kind, Asymmetry::NormalisedTerminator);
        let c = count(kind, Asymmetry::NormalisedCase);
        let r = count(kind, Asymmetry::Rewritten);
        let v = count(kind, Asymmetry::ValueChanged);
        let f = count(kind, Asymmetry::BuildFailed);
        let total = e + t + c + r + v + f;
        assert!(
            total >= floor_n,
            "only {total} '{kind}' frames were compared, floor is {floor_n}. \
             A percentage over an empty set passes while proving nothing, \
             which is what this floor exists to prevent."
        );
        if total == 0 {
            continue;
        }
        let pct = (e + t + c) as f64 / total as f64 * 100.0;
        assert!(
            pct >= floor_pct,
            "acceptable rebuilds for '{kind}' regressed to {pct:.1}% \
             (exact {e}, terminator {t}, case {c}, REWRITTEN {r}, \
             VALUE CHANGED {v}, failed {f}, of {total}), floor is \
             {floor_pct}%. Note that a normalisation is not a failure: \
             only `rewritten` and `value changed` are."
        );
    }
}

#[test]
#[ignore = "requires the operator-provided corpus/ recordings"]
fn corpus_aprs_structured_coverage_never_regresses() {
    let dir = Path::new("corpus");
    if !dir.is_dir() {
        eprintln!("corpus/ absent — skipping APRS coverage test");
        return;
    }

    let mut frames = 0usize;
    let mut aprs_ok = 0usize;
    let mut packet_ok = 0usize;
    let mut mice_ok = 0usize;
    let mut dti_seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut dti_lost: BTreeMap<String, usize> = BTreeMap::new();
    let mut variants: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut fields: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut errors: BTreeMap<String, usize> = BTreeMap::new();
    let mut non_aprs_frames = 0usize;

    for name in FILES {
        let path = dir.join(name);
        if !path.is_file() {
            eprintln!("corpus/{name} absent — skipping APRS coverage test");
            return;
        }
        let mut reader = hound::WavReader::open(&path).expect("opening corpus WAV");
        let rate = SampleRate::new(reader.spec().sample_rate).expect("corpus sample rate");
        let cfg = TncConfig::bell_202(rate).expect("bell 202 config");
        let mut rx: DefaultTncReceiver = DefaultTncReceiver::new(cfg).expect("receiver");

        let (mut n, mut ok, mut pkt, mut mice) = (0usize, 0usize, 0usize, 0usize);
        let mut non_aprs = 0usize;
        let pcm: Vec<i16> = reader
            .samples::<i16>()
            .map(|s| s.expect("sample"))
            .collect();
        for s in pcm {
            let Some(frame) = rx.push_i16(s) else {
                continue;
            };
            n += 1;
            let info = frame.info();
            let dti = info.first().copied().unwrap_or(0);
            *dti_seen.entry(dti_name(dti)).or_default() += 1;

            // The frame-level decode: `RxFrame::decoded` passes the
            // destination address as well as the information field, so
            // Mic-E lands in the same `match` as everything else
            // instead of in a fallback that re-pads the callsign by
            // hand.
            match frame.decoded().kind {
                DecodedKind::Packet(p) => {
                    ok += 1;
                    pkt += 1;
                    *variants.entry(variant(&p)).or_default() += 1;
                    // A wind reading counts the same whether it
                    // arrived as a `ddd/sss` data extension on a plain
                    // position or as the wind block of a Complete
                    // Weather Report. The two are the same seven bytes
                    // and the same measurement; which variant carries
                    // them is this crate's modelling choice, and a
                    // ratchet that tracked one of them would fall to
                    // zero the moment the other started being
                    // recognised -- which is exactly what happened
                    // when the timestamped layouts were implemented.
                    match &p {
                        AprsPacket::PositionWeather(w)
                            if w.weather.wind_direction.is_some()
                                || w.weather.wind_speed.is_some() =>
                        {
                            *fields.entry("wind").or_default() += 1;
                        }
                        // Chapter 16 names three structured things that
                        // hide inside status text. They used to stay
                        // part of the free text; count what is now read
                        // out of it, for the same reason the data
                        // extensions are counted.
                        AprsPacket::Status(s) => {
                            if s.timestamp().is_some() {
                                *fields.entry("status: timestamp").or_default() += 1;
                            }
                            if s.grid().is_some() {
                                *fields.entry("status: Maidenhead locator").or_default() += 1;
                            }
                            if s.beam().is_some() {
                                *fields.entry("status: beam heading / ERP").or_default() += 1;
                            }
                        }
                        _ => {}
                    }
                    // Field-level coverage: data extensions and the
                    // `/A=` altitude used to be handed back as opaque
                    // comment text. Counting them separately is what
                    // stops that silently regressing -- a frame keeps
                    // its typed value either way, so the coverage
                    // percentage above cannot see it.
                    let pos = match p {
                        AprsPacket::Position(x) => Some(x),
                        AprsPacket::PositionTimestamped(x) => Some(x.position),
                        _ => None,
                    };
                    if let Some(pos) = pos {
                        match pos.extension {
                            Some(DataExtension::CourseSpeed { .. }) => {
                                *fields.entry("data extension: course/speed").or_default() += 1
                            }
                            Some(DataExtension::Wind { .. }) => {
                                *fields.entry("wind").or_default() += 1;
                            }
                            Some(DataExtension::Phg(g)) => {
                                *fields.entry("data extension: PHG").or_default() += 1;
                                if g.rate().is_some() {
                                    *fields.entry("data extension: PHGR rate").or_default() += 1;
                                }
                            }
                            Some(DataExtension::Range { .. }) => {
                                *fields.entry("data extension: RNG").or_default() += 1;
                            }
                            Some(DataExtension::Dfs(_)) => {
                                *fields.entry("data extension: DFS").or_default() += 1;
                            }
                            Some(_) | None => {}
                        }
                        if pos.altitude_feet().is_some() {
                            *fields.entry("/A= altitude").or_default() += 1;
                        }
                    }
                }
                DecodedKind::Nmea(_) => {
                    ok += 1;
                    *variants.entry("Nmea").or_default() += 1;
                }
                DecodedKind::Ultimeter(_) => {
                    ok += 1;
                    *variants.entry("Ultimeter").or_default() += 1;
                }
                DecodedKind::ThirdParty(_) => {
                    ok += 1;
                    *variants.entry("ThirdParty").or_default() += 1;
                }
                // Mic-E is not an `AprsPacket` variant — it needs the
                // AX.25 destination address, so `AprsPacket::build`
                // could never round-trip it — but it *is* a
                // `DecodedKind` variant, so it is an ordinary arm here.
                DecodedKind::MicE(report) => {
                    mice += 1;
                    *variants.entry("MicE (outside AprsPacket)").or_default() += 1;
                    if report.altitude.is_some() {
                        *fields.entry("Mic-E altitude").or_default() += 1;
                    }
                    if report.altitude.is_some() && report.device_prefix.is_some() {
                        *fields
                            .entry("Mic-E altitude behind a device prefix")
                            .or_default() += 1;
                    }
                    if report.device_prefix.is_some() {
                        *fields.entry("Mic-E device prefix").or_default() += 1;
                    }
                }
                // Not APRS at all, by chapter 5's own table of data
                // type identifiers. Counted apart from the failures,
                // because a frame that is not APRS is not a frame the
                // APRS parser failed on.
                DecodedKind::Text { .. } => {
                    non_aprs += 1;
                    *dti_lost.entry(dti_name(dti)).or_default() += 1;
                    *errors.entry("Text (not APRS)".to_string()).or_default() += 1;
                }
                other => {
                    *dti_lost.entry(dti_name(dti)).or_default() += 1;
                    let label = match other {
                        DecodedKind::Malformed { error, .. } => format!("{error:?}"),
                        DecodedKind::NeedsDestination { .. } => "NeedsDestination".to_string(),
                        _ => "Unsupported".to_string(),
                    };
                    *errors.entry(label).or_default() += 1;
                }
            }
        }
        println!(
            "{name}: {n} frames, info-field {ok} (AprsPacket {pkt}), Mic-E {mice}, \
             structured {:.1}%",
            (ok + mice) as f64 / n as f64 * 100.0
        );
        frames += n;
        aprs_ok += ok;
        packet_ok += pkt;
        mice_ok += mice;
        non_aprs_frames += non_aprs;
    }

    let structured = aprs_ok + mice_ok;
    let pct = structured as f64 / frames as f64 * 100.0;
    // The denominator that answers "how much APRS does this decode?".
    // The one above answers "how much of everything heard on the
    // channel", which is a different question and is bounded by how
    // many non-APRS beacons happen to share the frequency.
    let aprs_frames = frames - non_aprs_frames;
    let aprs_pct = structured as f64 / aprs_frames as f64 * 100.0;

    println!("\n== totals ==");
    println!("frames decoded (AX.25):  {frames}");
    // Split out on purpose. `aprs_ok` is every information-field
    // decoder together -- `AprsPacket::parse` plus raw NMEA, Ultimeter
    // and third-party -- and printing it under `AprsPacket::parse ok`
    // overstated that parser's reach by 375 frames, which is a good
    // part of why "Mic-E is not in AprsPacket" has read as a larger
    // hole than it measures.
    println!("information-field decode ok: {aprs_ok}");
    println!("  of which AprsPacket::parse: {packet_ok}");
    println!(
        "  of which Nmea/Ultimeter/ThirdParty: {}",
        aprs_ok - packet_ok
    );
    println!("Mic-E (needs the destination): {mice_ok}");
    println!("no structured value:     {}", frames - structured);
    println!("  of which not APRS:     {non_aprs_frames} (plain-text beacons)");
    println!("structured coverage:     {pct:.1}% of all frames heard");
    println!("                         {aprs_pct:.1}% of the {aprs_frames} APRS frames");

    println!("\n== DTI histogram (count, then frames we could not structure) ==");
    let mut rows: Vec<_> = dti_seen.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1));
    for (dti, n) in rows {
        println!(
            "  {n:>5}  lost {:>4}  {dti}",
            dti_lost.get(dti).copied().unwrap_or(0)
        );
    }

    println!("\n== structured fields recovered from comment text ==");
    {
        let mut rows: Vec<_> = fields.iter().collect();
        rows.sort_by(|a, b| b.1.cmp(a.1));
        for (k, v) in rows {
            println!("{v:7}  {k}");
        }
    }

    println!("\n== typed as ==");
    let mut rows: Vec<_> = variants.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1));
    for (v, n) in rows {
        println!("  {n:>5}  {v}");
    }

    println!("\n== parse errors ==");
    let mut rows: Vec<_> = errors.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1));
    for (e, n) in rows {
        println!("  {n:>5}  {e}");
    }

    assert!(
        frames >= MIN_FRAMES,
        "corpus decoded {frames} frames, expected at least {MIN_FRAMES} \
         (a demodulator regression, not an APRS one)"
    );
    assert!(
        pct >= MIN_STRUCTURED_PERCENT,
        "APRS structured coverage regressed to {pct:.1}%, floor is \
         {MIN_STRUCTURED_PERCENT}% ({structured} of {frames} frames)"
    );
    assert!(
        aprs_pct >= MIN_APRS_STRUCTURED_PERCENT,
        "coverage of APRS frames regressed to {aprs_pct:.1}%, floor is \
         {MIN_APRS_STRUCTURED_PERCENT}% ({structured} of {aprs_frames} APRS frames)"
    );
    // A parser that stopped recognising non-APRS text would flatter
    // `aprs_pct` by shrinking its denominator, so the count of frames
    // removed from it is itself ratcheted.
    assert!(
        non_aprs_frames <= MAX_NON_APRS_FRAMES,
        "{non_aprs_frames} frames were set aside as non-APRS, above the \
         {MAX_NON_APRS_FRAMES} ceiling: text classification must not \
         start swallowing APRS"
    );
    for &(key, floor) in MIN_FIELDS {
        let got = fields.get(key).copied().unwrap_or(0);
        assert!(
            got >= floor,
            "field coverage regressed: {got} × '{key}', floor is {floor}. \
             The frame still gets a typed value either way, so the coverage \
             percentage above cannot catch this — that is why these are pinned \
             separately."
        );
    }
}
