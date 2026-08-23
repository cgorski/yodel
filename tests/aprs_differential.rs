//! APRS **field-level** differential against an independent decoder.
//!
//! `tests/differential.rs` already compares AX.25 *frames* with the
//! reference implementation. It never compares what those frames
//! *mean*: two decoders can agree byte-for-byte on a frame and still
//! disagree about the latitude it encodes. Mic-E in particular packs
//! position across the destination address and the information field
//! with several documented errata, so agreeing on the bytes proves very
//! little.
//!
//! This test closes that gap on real off-air traffic. Every frame the
//! receiver recovers from the corpus is rendered as a monitor-format
//! line, the same lines are piped through the reference's APRS decoder,
//! and the two dissections are compared **field by field**.
//!
//! # Why every field, and not just the position
//!
//! Until this suite compared more than coordinates, the APRS-layer
//! ratchets in `tests/corpus_aprs.rs` — 258 altitudes, 199 course/speed
//! extensions, 139 PHG, 54 wind — rested entirely on our own opinion.
//! They count values we recovered; nothing checked that the values were
//! *right*. A decoder that reads course from the wrong offset, or that
//! calls knots miles per hour, produces exactly the same counts. That
//! internal consistency proves nothing is the lesson this project keeps
//! relearning (`docs/APRS_CONFORMANCE.md` §6.1), so the fields the
//! corpus celebrates are the fields an outsider has to confirm.
//!
//! Two independent quantities are reported per field, and they should
//! not be conflated:
//!
//! * **accuracy** — of the frames where *both* decoders produce a
//!   value, how many agree. Any disagreement is a bug in one of them,
//!   and is a hard failure here.
//! * **coverage** — frames where only one decoder produces a value.
//!   Ours being lower is a missing feature, not a defect, and is
//!   tracked as a ratchet rather than an equality.
//!
//! # Units
//!
//! The reference renders each quantity in a display unit of its own
//! choosing (km/h *and* mph for speed, metres *and* feet for altitude,
//! inches of mercury for pressure). We convert **our** wire value into
//! the same unit using the published conversion factors and compare
//! there. The factor is a spec constant, and it is the decoded wire
//! value — not the arithmetic — that is under test. Where a quantity is
//! printed in two units both are compared, which is what stops a
//! knots/mph mix-up passing on a loose tolerance.
//!
//! Needs the corpus plus `WARBLE_REF_APRS` pointing at the reference's
//! APRS decoder binary (see CONTRIBUTING.md; it is a separate binary
//! from `WARBLE_REF_DECODE`, which decodes *audio*):
//!
//! ```text
//! WARBLE_REF_APRS=/path/to/aprs-decoder \
//!   cargo test --all-features --test aprs_differential -- --ignored --nocapture
//! ```
//!
//! Unset skips, set-but-wrong fails: see [`ref_binary`].
#![cfg(all(feature = "tnc", feature = "micE"))]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use warble::SampleRate;
use warble::aprs::extension::{self, DataExtension};
use warble::aprs::symbol::resolve as resolve_symbol;
use warble::aprs::{
    AprsPacket, CompressedCs, CompressionOrigin, CompressionType, Coordinates, Decoded,
    DecodedKind, Latitude, Longitude, NmeaSource, Position, PositionCs, Symbol, WeatherReport,
    mic_e,
};
use warble::geo::Ambiguity;
use warble::tnc::{DefaultTncReceiver, TncConfig};
use warble::units::{Bearing, Distance, Humidity, Rainfall, Speed, Temperature};

const FILES: &[&str] = &[
    "01_40-Mins-Traffic_-on-144.39.wav",
    "02_100-Mic-E-Bursts-DE-emphasized.wav",
    "03_100-Mic-E-Bursts-Flat.wav",
    "04_25-MIns-Drive-Test.wav",
];

/// Resolves the reference decoder: `None` when the variable is unset,
/// which the caller turns into a skip, and a hard failure when it is set
/// to a path that is not a file.
///
/// Unset means "this contributor does not have the binary", a legitimate
/// skip. Set-but-wrong means somebody typed a path and meant to run this
/// suite, so it is a hard failure -- otherwise a single typo turns an
/// entire interoperability suite green while it tests nothing at all.
///
/// This suite reached the same outcome only by accident and only
/// sometimes: it read the variable with no existence check, so a bad
/// path failed later, at `spawn`, deep inside [`ask_reference`] -- after
/// several minutes of corpus decoding, and *not at all* when the corpus
/// happened to be absent, since that check came first and returned a
/// silent skip. Checking here makes the failure immediate and
/// independent of what other material is present.
fn ref_binary(var: &str) -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os(var)?);
    assert!(
        path.is_file(),
        "{var}={} does not point to an existing file. Unset the variable \
         to skip this suite deliberately; leaving it set and wrong would \
         pass without testing anything.",
        path.display()
    );
    // Absolute, because [`ask_reference`] runs the decoder beside its own
    // data files. Rust documents a relative program path combined with
    // `current_dir` as platform specific, and where it does not work the
    // failure reads as a missing binary -- indistinguishable from the typo
    // case this function exists to report.
    Some(
        path.canonicalize()
            .unwrap_or_else(|e| panic!("{var}={}: {e}", path.display())),
    )
}

// ---------------------------------------------------------------------
// Ratchets
// ---------------------------------------------------------------------

/// Floor for frames where both decoders produced a value, per field.
///
/// Guards against a regression that trivially satisfies the accuracy
/// assertion by decoding nothing — with zero frames compared, "no
/// disagreements" is vacuously true. Raise these whenever the record
/// improves; never lower one.
///
/// MEASURED on the corpus, each floor a little below the measurement
/// so the ratchet reports a real loss rather than noise: position
/// 1724, symbol 1743, course 1204, speed 1263, altitude 1096, PHG 120,
/// wind 148, wind direction 148, gust 136, temperature 148, humidity
/// 130, barometer 130, rain 134/146/118.
///
/// The position row is the pre-existing `MIN_COMPARED`: 1347
/// originally, then 1382 after the over-strictness fixes, 1633 once
/// raw NMEA and third-party traffic were implemented, 1722 after
/// Mic-E stopped rejecting an out-of-spec symbol byte, then 1724 once
/// hemisphere letters were accepted case-insensitively.
///
/// The symbol row was 1475 until chapter 20's address-borne symbols
/// were implemented ([`warble::aprs::symbol::from_destination`] and
/// [`warble::aprs::symbol::from_source_ssid`]); the 268 raw-NMEA
/// frames that carry their icon in the AX.25 addresses rather than the
/// information field then joined the comparison, all 268 agreeing.
///
/// **`range` is 0 on purpose.** Neither `RNGrrrr` nor a compressed
/// radio-range trailer occurs anywhere in this corpus, so the row is
/// inert here and proves nothing; it is kept so that the day one
/// appears the comparison is already wired. What covers radio range is
/// `tests/aprs.rs`, in tier 2 — which is the rule `CONTRIBUTING.md`
/// states: tiers 1–2 must suffice.
const MIN_COMPARED: &[(&str, usize)] = &[
    ("position", 1700),
    ("symbol", 1720),
    ("course", 1180),
    ("speed km/h", 1260),
    ("speed mph", 1260),
    ("altitude", 1080),
    ("range", 0),
    ("phg", 115),
    ("wx wind", 145),
    ("wx direction", 145),
    ("wx gust", 130),
    ("wx temperature", 145),
    ("wx humidity", 125),
    ("wx barometer", 125),
    ("wx rain 1h", 130),
    ("wx rain 24h", 140),
    ("wx rain midnight", 112),
];

/// Ceiling on values the reference decodes and we do not, per field.
///
/// The complement of [`MIN_COMPARED`]: that floor stops us decoding
/// *fewer* frames, this ceiling stops us decoding a *smaller share* of
/// what is decodable. Without it, a change could hold the compared
/// count while quietly losing ground to the reference on new traffic.
///
/// A ratchet: lower it when coverage improves, never raise it. Every
/// row below is MEASURED and diagnosed — the test prints one whole
/// frame per data-type identifier for each gap, so none of these is a
/// number nobody has looked at.
///
/// * **`position` = 2.** MEASURED 344, then 4 once the receive-only
///   formats were implemented, then 2 once hemisphere letters were
///   accepted case-insensitively. Both remaining frames are
///   third-party (`}`) wrappers around a *Mic-E* payload. Mic-E needs
///   the destination address, and [`warble::aprs::ThirdParty`] does not
///   nest — it borrows the encapsulated payload and leaves descending
///   to the caller, which bounds recursion by construction — so the
///   harness cannot recover the inner destination. A property of the
///   test, not a parser gap.
/// * **`course` = 12, `speed` = 10.** This entry used to read "almost
///   all `000/000` extensions", and that diagnosis was wrong: of the
///   26 the speed gap once held, only **2** were the pair sentinel.
///   Sixteen were a station reporting a real course beside a real
///   speed of *zero knots* (`315/000`, `194/000`, `035/000`), which
///   this crate discarded because it collapsed a zero in either half
///   independently. Chapter 7 states the sentinel for the **pair**, so
///   the sixteen are now decoded and the gap fell 26 → 10 with no new
///   disagreement. What is left is 6 Mic-E frames with a corrupt
///   longitude, 2 third-party Mic-E, and the 2 `000/000` sentinels,
///   which should not close: chapter 7 says outright that the pair
///   means *unknown*, so we report `None` where the reference reports
///   zero. The `course` gap is 6 real `000/sss` — right to keep, since
///   chapter 7 gives the course domain as `001-360` — plus the same 6
///   corrupt Mic-E. (Both were 200 and 214 until `NmeaSentence` grew
///   unit-typed `course` and `speed` accessors and this harness
///   started asking for them — the crate had parsed the fields all
///   along.)
/// * **`altitude` = 6.** Mic-E frames with a corrupt longitude byte
///   (0xBE, outside the 38–127 chapter 10 permits): we reject the
///   whole report, the reference prints "Invalid Longitude" and
///   salvages the other fields. Being strict about a position report
///   with no valid position is the intended behaviour.
/// * **`symbol` = 28.** MEASURED 296, and the old diagnosis of that
///   number was wrong in an instructive way: it said 268 of them were
///   raw NMEA "for which the reference substitutes a symbol that is
///   nowhere in the sentence". The symbol was not nowhere. It was in
///   the AX.25 *addresses*, which chapter 8 says is where early
///   trackers had to put it ("symbols had to go in the destination
///   field using names like `GPSxxx`") and which chapter 20 specifies
///   in full: 211 frames name it in the destination (`GPSLJ` jeep,
///   `GPSLK` truck, `GPSMV` car) and 57 in the source SSID. Reading
///   both closed all 268 with no new disagreement. What is left is 26
///   Mic-E frames whose symbol table byte is outside the specification
///   — we decline to name those and the reference falls back to the
///   primary table, a difference of policy (see `set_symbol`) — and the
///   2 third-party frames above.
/// * **`wx *` = 0.** Was 92 — chapter 12 defines *five* Complete
///   Weather Report layouts and only one was implemented. The two
///   uncompressed timestamped spellings (`/` and `@`) now are, which
///   closed 54 frames, and relaxing the trailer rule closed the other
///   38: a report ending in a manufacturer's `v6` stamp was being
///   rejected outright. The compressed and object-borne layouts are
///   still unimplemented and do not occur in this corpus, so this row
///   cannot see them — `tests/aprs_extras.rs` is where they will have
///   to be pinned when they land.
const MAX_GAP: &[(&str, usize)] = &[
    ("position", 2),
    ("symbol", 28),
    ("course", 12),
    ("speed km/h", 10),
    ("speed mph", 10),
    ("altitude", 6),
    ("range", 0),
    ("phg", 0),
    ("wx wind", 0),
    ("wx direction", 0),
    ("wx gust", 0),
    ("wx temperature", 0),
    ("wx humidity", 0),
    ("wx barometer", 0),
    ("wx rain 1h", 0),
    ("wx rain 24h", 0),
    ("wx rain midnight", 0),
];

/// Exact frame counts for `synthetic_formats_agree_with_reference_decoder`.
///
/// Equalities rather than ratchets: every frame in that test is one it
/// built itself, so the count is a property of the case list. It moving
/// means either the sweep changed — fine, update it and say why — or
/// the reference's output format changed under us, which is the failure
/// this guards. Without it, a parsing regression in the harness would
/// look like a clean pass over zero comparisons.
const SYNTHETIC_COMPARED: &[(&str, usize)] = &[
    ("position", 64),
    ("course", 42),
    ("speed km/h", 42),
    ("speed mph", 42),
    ("altitude", 7),
    ("range", 14),
];

/// Distinct symbols whose chart description must map one-to-one with
/// the reference's. A ratchet on the *breadth* of the symbol check:
/// agreeing about three symbols would be easy and worthless.
/// MEASURED: 30, then 32 once the address-borne symbols of chapter 20
/// joined the comparison. Worth knowing *which* two, because it is not
/// the obvious pair: the destination addresses in this corpus
/// (`GPSLJ`, `GPSLK`, `GPSMV`) name a jeep, a truck and a car, and all
/// three were already reached through some other format. The two new
/// glyphs — `/U` bus and `/a` ambulance — come from the *source SSID*,
/// the last and least-used rule in chapter 20, which nothing else in
/// this corpus exercises at all.
const MIN_DISTINCT_SYMBOLS: usize = 30;

/// Positions agreeing to within this many degrees count as identical.
/// The reference prints minutes to 4 decimal places, so its own
/// quantization floor is ~1.7e-6 degrees; this is two orders looser.
const TOLERANCE_DEG: f64 = 0.0001;

// ---------------------------------------------------------------------
// The shape both sides are projected into
// ---------------------------------------------------------------------

/// A decoded position in decimal degrees.
///
/// A named struct rather than `(f64, f64)`: the two members of that
/// tuple are mutually assignable, so a transposition would compile
/// silently and this test would then "agree" with the reference on
/// mirrored coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Degrees {
    latitude: f64,
    longitude: f64,
}

/// Transmitter capability, in the four physical quantities both
/// decoders derive from the four `PHGphgd` wire codes.
///
/// Compared as a unit rather than field by field: the four codes live
/// in four adjacent bytes, so an off-by-one in the offsets shifts all
/// of them, and comparing the tuple names that failure in one line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PhgFields {
    power_watts: u32,
    height_feet: u32,
    gain_dbi: u32,
    /// Eight-point compass abbreviation, or `"omni"`.
    directivity: &'static str,
}

/// Weather measurements in the units the reference prints them in.
///
/// `wind` is miles per hour with one decimal place because that is what
/// the reference renders, whatever the wire form said. The two forms
/// disagree about their own unit — the `sNNN` field of a positionless
/// report is mph while the `DDD/SSS` data extension of a position
/// report is knots (protocol reference 1.2, chapters 7 and 12) — and
/// projecting both onto one physical unit is how this suite found that
/// the crate was conflating them.
///
/// **The wind row cannot re-catch that defect, and must not be relied
/// on to.** MEASURED by mutation: restoring the conflation (reading
/// `DDD/SSS` as mph) changes nothing here. Every position-weather
/// report in this corpus is becalmed — at three knots or less the two
/// units round to the same tenth of a mile per hour, which is all the
/// reference prints. What caught it originally was the *gap* between
/// our reading and theirs at the time, and what guards it now is the
/// tier-2 spec vector in `tests/aprs_extras.rs`, which asserts four
/// knots reads back as four knots and five miles per hour. That is the
/// rule `CONTRIBUTING.md` states, running in the direction people
/// forget: a real defect that only tiers 1–2 can see is not a reason
/// to trust tier 4 less, but it is a reason not to let the ratchet
/// here stand in for a vector there.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct Weather {
    wind_mph: Option<f64>,
    direction: Option<u16>,
    gust_mph: Option<u16>,
    temperature_f: Option<i32>,
    rain_1h_inch: Option<f64>,
    rain_24h_inch: Option<f64>,
    rain_midnight_inch: Option<f64>,
    humidity: Option<u8>,
    barometer_inhg: Option<f64>,
}

/// One frame's dissection, in whichever units the reference prints.
#[derive(Debug, Clone, Default)]
struct Fields {
    position: Option<Degrees>,
    course: Option<u16>,
    /// Degrees of slack the course comparison is allowed, which is the
    /// resolution the *source* carries rather than a fudge factor.
    ///
    /// Zero for every APRS encoding, because `ddd/sss`, Mic-E and the
    /// compressed `csT` trailer all state whole degrees and there is
    /// nothing to round. One for raw NMEA, which states hundredths:
    /// this crate rounds half away from zero per its own stated rule
    /// (`units` §3.1) and the reference truncates, so `090.5` is 91
    /// here and 90 there. MEASURED: 9 corpus frames differ, every one
    /// of them exactly on a half-degree tie and by exactly one degree.
    /// Neither convention is wrong at a tie, so the slack is granted
    /// where the resolution justifies it and nowhere else — an
    /// unconditional tolerance would have swallowed the off-by-one
    /// mutation that this row is here to catch.
    course_slack: u16,
    speed_kmh: Option<f64>,
    speed_mph: Option<f64>,
    altitude_feet: Option<i32>,
    range_miles: Option<f64>,
    phg: Option<PhgFields>,
    weather: Weather,
    /// Ours: the two wire bytes. Theirs: the chart description. Neither
    /// side's symbol chart is derivable from the other's, so the two
    /// are compared as a *relation* — see `symbol_mapping_report`.
    symbol_wire: Option<(u8, u8)>,
    symbol_text: Option<String>,
}

// ---------------------------------------------------------------------
// Our side: project each decoded packet into `Fields`
// ---------------------------------------------------------------------

/// Converts crate coordinates into decimal degrees, field by field.
fn degrees(coordinates: Coordinates) -> Degrees {
    Degrees {
        latitude: coordinates.latitude.to_degrees(),
        longitude: coordinates.longitude.to_degrees(),
    }
}

/// Records a speed in both units the reference prints.
///
/// Goes through [`warble::units::Speed`] rather than restating the
/// conversion factors here, so the test measures the crate's own
/// arithmetic instead of a second copy of it that could drift. Only
/// the last step — canonical millimetres per hour into the
/// reference's display unit — is done in floating point, and both
/// divisors are exact by definition (1 km = 10^6 mm, 1 international
/// mile = 1 609 344 mm).
fn set_speed(fields: &mut Fields, speed: Speed) {
    #[allow(clippy::cast_precision_loss)]
    let mm_per_hour = speed.millimeters_per_hour() as f64;
    fields.speed_kmh = Some(mm_per_hour / 1_000_000.0);
    fields.speed_mph = Some(mm_per_hour / 1_609_344.0);
}

/// Miles per hour, for the wind fields, which the reference prints to
/// one decimal place in that unit whatever the wire form said.
fn mph(speed: Speed) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let mm_per_hour = speed.millimeters_per_hour() as f64;
    mm_per_hour / 1_609_344.0
}

/// Records the symbol, unless its table byte is one the crate declines
/// to interpret.
///
/// [`Symbol::from_wire`] keeps out-of-spec table bytes losslessly and
/// [`Symbol::table`] then answers `None` — real Mic-E traffic carries
/// table bytes outside `/ \ 0-9 A-Z` and the position is still good.
/// The reference instead falls back to the primary table, so it renders
/// `>`/`v` and `/`/`v` with the same words. Including those would break
/// the one-to-one check below for a difference of policy rather than a
/// difference of decoding, so a symbol we refuse to name is not
/// compared.
fn set_symbol(fields: &mut Fields, symbol: Symbol) {
    if symbol.table().is_some() {
        fields.symbol_wire = Some(symbol.to_wire());
    }
}

/// Folds a data extension into the projection.
fn apply_extension(fields: &mut Fields, ext: &DataExtension) {
    match ext {
        DataExtension::CourseSpeed { course, speed } => {
            fields.course = course.degrees();
            if let Some(knots) = speed.knots() {
                set_speed(fields, Speed::from_knots(i32::from(knots)));
            }
        }
        DataExtension::Wind { direction, speed } => {
            fields.weather.direction = direction.degrees();
            fields.weather.wind_mph = speed
                .knots()
                .map(|knots| mph(Speed::from_knots(i32::from(knots))));
        }
        DataExtension::Phg(phg) => {
            // A zero power code is how the reference spells "no PHG";
            // keep the two sides comparable rather than inventing a
            // value it never prints.
            if phg.power_watts() != 0 {
                fields.phg = Some(PhgFields {
                    power_watts: u32::from(phg.power_watts()),
                    height_feet: phg.height_feet(),
                    gain_dbi: u32::from(phg.gain_dbi()),
                    directivity: directivity_name(phg.directivity_degrees()),
                });
            }
        }
        DataExtension::Range { miles } => {
            if *miles != 0 {
                fields.range_miles = Some(f64::from(*miles));
            }
        }
        // The reference does not dissect `DFS`; nothing to compare.
        DataExtension::Dfs(_) => {}
        _ => {}
    }
}

/// The eight-point compass abbreviation for a PHG directivity, which
/// the protocol reference defines as the wire code times 45 degrees.
fn directivity_name(degrees: Option<u16>) -> &'static str {
    match degrees {
        None => "omni",
        Some(d) => match d % 360 {
            0 => "N",
            45 => "NE",
            90 => "E",
            135 => "SE",
            180 => "S",
            225 => "SW",
            270 => "W",
            315 => "NW",
            _ => "?",
        },
    }
}

/// Folds a compressed `csT` trailer into the projection.
fn apply_cs(fields: &mut Fields, cs: CompressedCs) {
    match cs {
        CompressedCs::NoData => {}
        CompressedCs::CourseSpeed { course, speed } => {
            fields.course = Some(course);
            set_speed(fields, Speed::from_knots(i32::from(speed)));
        }
        CompressedCs::RadioRange { miles } => fields.range_miles = Some(f64::from(miles)),
        CompressedCs::Altitude { feet } => fields.altitude_feet = i32::try_from(feet).ok(),
    }
}

/// Folds a [`WeatherReport`] into the projection, in display units.
///
/// Note what is **not** here any more: a flag saying which wire form
/// the report came from. It used to be needed, because
/// `WeatherReport::wind_speed` was a bare integer that meant miles per
/// hour in one layout and knots in another, and only the caller knew
/// which. Now it is a [`Speed`] and the question does not arise — the
/// unit is decided once, where the bytes are read.
fn apply_weather(fields: &mut Fields, report: &WeatherReport) {
    let w = &mut fields.weather;
    if let Some(direction) = report.wind_direction {
        w.direction = Some(direction);
    }
    w.wind_mph = report.wind_speed.map(mph);
    w.gust_mph = report.gust.and_then(|g| u16::try_from(g.mph()).ok());
    w.temperature_f = report.temperature.map(Temperature::fahrenheit);
    w.rain_1h_inch = report.rain_1h.map(rain_inches);
    w.rain_24h_inch = report.rain_24h.map(rain_inches);
    w.rain_midnight_inch = report.rain_midnight.map(rain_inches);
    w.humidity = report.humidity.map(Humidity::percent);
    w.barometer_inhg = report
        .barometric_pressure
        .map(|p| f64::from(p.hundredths_inhg()) / 100.0);
}

/// Rainfall in inches, which is what the reference prints.
fn rain_inches(rain: Rainfall) -> f64 {
    f64::from(rain.hundredths_inch()) / 100.0
}

/// Everything we can say about one `AprsPacket`.
fn packet_fields(packet: &AprsPacket<'_>) -> Fields {
    let mut fields = Fields::default();
    match packet {
        AprsPacket::Position(p) => {
            fields.position = Some(degrees(p.coordinates()));
            set_symbol(&mut fields, p.symbol);
            if let Some(ext) = &p.extension {
                apply_extension(&mut fields, ext);
            }
            fields.altitude_feet = p.altitude_feet();
        }
        AprsPacket::PositionCs(p) => {
            fields.position = Some(degrees(p.coordinates()));
            set_symbol(&mut fields, p.position.symbol);
            apply_cs(&mut fields, p.cs);
            if let Some(feet) = p.position.altitude_feet() {
                fields.altitude_feet = Some(feet);
            }
        }
        AprsPacket::PositionTimestamped(p) => {
            fields.position = Some(degrees(p.coordinates()));
            set_symbol(&mut fields, p.position.symbol);
            if let Some(ext) = &p.position.extension {
                apply_extension(&mut fields, ext);
            }
            apply_cs(&mut fields, p.cs);
            if let Some(feet) = p.position.altitude_feet() {
                fields.altitude_feet = Some(feet);
            }
        }
        AprsPacket::PositionWeather(w) => {
            fields.position = Some(degrees(w.coordinates()));
            set_symbol(&mut fields, w.symbol);
            apply_weather(&mut fields, &w.weather);
            fields.altitude_feet = extension::altitude_feet(w.rest);
        }
        AprsPacket::Weather(w) => apply_weather(&mut fields, &w.weather),
        AprsPacket::Object(o) => {
            fields.position = Some(degrees(o.coordinates()));
            set_symbol(&mut fields, o.symbol);
            fields.altitude_feet = extension::altitude_feet(o.comment);
        }
        AprsPacket::Item(i) => {
            fields.position = Some(degrees(i.coordinates()));
            set_symbol(&mut fields, i.symbol);
            fields.altitude_feet = extension::altitude_feet(i.comment);
        }
        _ => {}
    }
    fields
}

/// warble's dissection of one frame.
///
/// Goes through the total [`Decoded`] entry point so that raw NMEA and
/// third-party traffic are covered too — those are the formats most
/// likely to harbour a coordinate bug, since NMEA uses a different
/// lat/lon encoding from every other APRS format.
fn our_fields(dest_call: &[u8; 6], source_ssid: u8, info: &[u8]) -> Fields {
    match Decoded::decode(info).kind {
        DecodedKind::Packet(p) => packet_fields(&p),
        DecodedKind::Nmea(sentence) => {
            let mut fields = Fields {
                position: sentence.position().map(degrees),
                course: sentence.course().map(Bearing::degrees),
                altitude_feet: sentence.altitude().map(Distance::feet),
                // NMEA states course in hundredths of a degree; see
                // `Fields::course_slack`.
                course_slack: 1,
                ..Fields::default()
            };
            if let Some(speed) = sentence.speed() {
                set_speed(&mut fields, speed);
            }
            // Chapter 20: a raw NMEA sentence has nowhere to put a
            // symbol, so it goes in the AX.25 destination address
            // (`GPSxyz`) or, failing that, the source SSID. The `None`
            // here is exact rather than a guess — this format has no
            // symbol field at all — which is the precondition
            // `resolve` documents. Every other arm of this match does
            // *not* consult the addresses: a Mic-E destination is
            // packed latitude, and a position report carries its own
            // symbol, which takes precedence even when `set_symbol`
            // declines to name it.
            if let Some(symbol) = resolve_symbol(None, &dest_call[..], source_ssid) {
                set_symbol(&mut fields, symbol);
            }
            fields
        }
        // The inner payload of a third-party packet is the original
        // transmission; descend exactly one level, which is what the
        // reference decoder does too.
        DecodedKind::ThirdParty(tp) => match Decoded::decode(tp.payload).kind {
            DecodedKind::Packet(p) => packet_fields(&p),
            _ => Fields::default(),
        },
        // Mic-E is not an `AprsPacket` variant: it needs the destination.
        _ => match mic_e::decode(dest_call, info) {
            Ok(m) => {
                let mut fields = Fields {
                    position: Some(degrees(m.coordinates())),
                    course: Some(m.course),
                    altitude_feet: m
                        .altitude
                        .map(|meters| Distance::from_meters(meters).feet())
                        .or_else(|| extension::altitude_feet(m.status)),
                    ..Fields::default()
                };
                set_symbol(&mut fields, m.symbol);
                set_speed(&mut fields, Speed::from_knots(i32::from(m.speed)));
                fields
            }
            Err(_) => Fields::default(),
        },
    }
}

// ---------------------------------------------------------------------
// Their side: parse the reference decoder's text dissection
// ---------------------------------------------------------------------

/// Strips ANSI CSI sequences from the reference decoder's coloured output.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\u{1b}' {
            for c2 in it.by_ref() {
                if c2.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Parses `"N 34 16.9600, W 118 24.1000, 2 km/h, course 319, ..."`.
/// Only the two leading coordinates are consumed; trailing fields vary.
fn parse_ref_latlon(s: &str) -> Option<Degrees> {
    let (lat, rest) = s.split_once(", ")?;
    Some(Degrees {
        latitude: parse_coord(lat)?,
        longitude: parse_coord(rest)?,
    })
}

fn parse_coord(s: &str) -> Option<f64> {
    let mut it = s.split_whitespace();
    let hemi = it.next()?;
    let deg: f64 = it.next()?.trim_end_matches(',').parse().ok()?;
    let min: f64 = it.next()?.trim_end_matches(',').parse().ok()?;
    let v = deg + min / 60.0;
    Some(if hemi == "S" || hemi == "W" { -v } else { v })
}

/// Is this line the reference's coordinate line?
fn is_coordinate_line(t: &str) -> bool {
    (t.starts_with("N ") || t.starts_with("S ")) && t.contains(", ")
}

/// Is this line the reference's weather line?
///
/// It is a comma-separated list ending in the quoted residual comment,
/// so the closing quote plus at least one weather keyword distinguishes
/// it from the plain comment line that follows.
fn is_weather_line(t: &str) -> bool {
    const KEYS: &[&str] = &[
        "wind ",
        "direction ",
        "gust ",
        "temperature ",
        "rain ",
        "humidity ",
        "barometer ",
    ];
    t.ends_with('"') && KEYS.iter().any(|k| t.contains(k))
}

/// Pulls `<value>` out of a `"<keyword> <value>"` segment.
fn segment_after<'a>(segments: &[&'a str], keyword: &str) -> Option<&'a str> {
    segments
        .iter()
        .find_map(|s| s.trim().strip_prefix(keyword))
        .map(str::trim)
}

/// The reference's summary line: packet type, symbol, and sometimes the
/// transmitter capability.
///
/// It is the last comma-bearing line before the coordinates, because
/// the advisory lines the decoder interleaves ("use of ... is
/// obsolete", "didn't find wind gust in form g999") never contain a
/// `", "`. When a frame carries no position — a positionless weather
/// report, a status, a message — it is the first such line instead.
fn find_type_line(block: &[String]) -> Option<&str> {
    let lines: Vec<&str> = block.iter().map(|l| l.trim()).collect();
    let end = lines
        .iter()
        .position(|t| is_coordinate_line(t))
        .unwrap_or(lines.len());
    lines[..end]
        .iter()
        .rev()
        .find(|t| t.contains(", "))
        .copied()
        .or_else(|| lines[..end].iter().find(|t| t.contains(", ")).copied())
}

/// The symbol's chart description, as the reference spells it.
///
/// It is the segment after the packet type, or after the quoted object
/// or item name when there is one. Descriptions containing `", "` are
/// truncated here, which is harmless: the truncation is a deterministic
/// function of the symbol, so the mapping check below still holds.
fn their_symbol_text(type_line: &str) -> Option<String> {
    let mut segments = type_line.split(", ").skip(1);
    let first = segments.next()?;
    let text = if first.starts_with('"') {
        segments.next()?
    } else {
        first
    };
    let text = text.trim();
    if text.is_empty() || text.contains("vendor/model") || text.contains("height(HAAT)=") {
        return None;
    }
    Some(text.to_string())
}

/// Parses `"25 W height(HAAT)=20ft=6m 3dBi E"`.
fn parse_ref_phg(type_line: &str) -> Option<PhgFields> {
    let at = type_line.find(" W height(HAAT)=")?;
    let head = &type_line[..at];
    let power_watts: u32 = head.rsplit([' ', ',']).next()?.parse().ok()?;
    let rest = &type_line[at + " W height(HAAT)=".len()..];
    let (height, rest) = rest.split_once("ft=")?;
    let height_feet: u32 = height.parse().ok()?;
    // "6m 3dBi E" — skip the metric restatement of the same height.
    let mut words = rest.split_whitespace().skip(1);
    let gain_dbi: u32 = words.next()?.trim_end_matches("dBi").parse().ok()?;
    let directivity = match words.next()? {
        "omni" => "omni",
        "N" => "N",
        "NE" => "NE",
        "E" => "E",
        "SE" => "SE",
        "S" => "S",
        "SW" => "SW",
        "W" => "W",
        "NW" => "NW",
        _ => "?",
    };
    Some(PhgFields {
        power_watts,
        height_feet,
        gain_dbi,
        directivity,
    })
}

/// Pipes monitor-format lines through the reference decoder and
/// returns its dissection of each, in order.
///
/// Shared by the corpus comparison and the synthetic one. The
/// reference resolves its data files relative to the working
/// directory, so it is run beside them.
fn ask_reference(decoder: &Path, lines: &str, expected: usize) -> Vec<Fields> {
    let cwd = decoder
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .map(|p| p.join("data"))
        .filter(|p| p.is_dir());
    let mut cmd = Command::new(decoder);
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawning the reference APRS decoder");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(lines.as_bytes())
        .expect("writing frames");
    let out = child.wait_with_output().expect("reference decoder output");
    let text = strip_ansi(&String::from_utf8_lossy(&out.stdout));

    // The decoder echoes each input line, then prints its dissection.
    // Split on the echoes; everything until the next echo is one block.
    let mut blocks: Vec<Vec<String>> = Vec::new();
    for line in text.lines() {
        let is_echo = line.contains('>') && line.contains(':') && !line.starts_with(' ');
        if is_echo {
            blocks.push(Vec::new());
            continue;
        }
        if let Some(block) = blocks.last_mut() {
            block.push(line.to_string());
        }
    }
    let theirs: Vec<Fields> = blocks.iter().map(|b| their_fields(b)).collect();
    assert_eq!(
        theirs.len(),
        expected,
        "block alignment lost: {expected} frames in, {} dissections out",
        theirs.len()
    );
    theirs
}

/// The reference's dissection of one frame.
fn their_fields(block: &[String]) -> Fields {
    let mut fields = Fields::default();

    if let Some(line) = block
        .iter()
        .map(|l| l.trim())
        .find(|t| is_coordinate_line(t))
    {
        fields.position = parse_ref_latlon(line);
        let segments: Vec<&str> = line.split(", ").collect();
        for segment in segments.iter().map(|s| s.trim()) {
            // "67 km/h (41 MPH)"
            if let Some((kmh, mph)) = segment.split_once(" km/h") {
                fields.speed_kmh = kmh.trim().parse().ok();
                fields.speed_mph = mph
                    .trim()
                    .trim_start_matches('(')
                    .trim_end_matches(')')
                    .trim_end_matches(" MPH")
                    .trim()
                    .parse()
                    .ok();
            }
            // "alt 376 m (1234 ft)"
            if let Some(rest) = segment.strip_prefix("alt ")
                && let Some((_, feet)) = rest.split_once('(')
            {
                fields.altitude_feet = feet
                    .trim_end_matches(')')
                    .trim_end_matches(" ft")
                    .parse()
                    .ok();
            }
        }
        fields.course = segment_after(&segments, "course ").and_then(|v| v.parse().ok());
    }

    if let Some(line) = find_type_line(block) {
        fields.symbol_text = their_symbol_text(line);
        fields.phg = parse_ref_phg(line);
        if let Some(rest) = line.split("range=").nth(1) {
            fields.range_miles = rest.split(", ").next().and_then(|v| v.trim().parse().ok());
        }
    }

    if let Some(line) = block.iter().map(|l| l.trim()).find(|t| is_weather_line(t)) {
        let segments: Vec<&str> = line.split(", ").collect();
        let w = &mut fields.weather;
        w.wind_mph =
            segment_after(&segments, "wind ").and_then(|v| v.trim_end_matches(" mph").parse().ok());
        w.direction = segment_after(&segments, "direction ").and_then(|v| v.parse().ok());
        w.gust_mph = segment_after(&segments, "gust ").and_then(|v| v.parse().ok());
        w.temperature_f = segment_after(&segments, "temperature ").and_then(|v| v.parse().ok());
        w.humidity = segment_after(&segments, "humidity ").and_then(|v| v.parse().ok());
        w.barometer_inhg = segment_after(&segments, "barometer ").and_then(|v| v.parse().ok());
        for segment in segments.iter().map(|s| s.trim()) {
            let Some(rest) = segment.strip_prefix("rain ") else {
                continue;
            };
            let Some((value, when)) = rest.split_once(' ') else {
                continue;
            };
            let value: Option<f64> = value.parse().ok();
            match when {
                "in last hour" => w.rain_1h_inch = value,
                "in last 24 hours" => w.rain_24h_inch = value,
                "since midnight" => w.rain_midnight_inch = value,
                _ => {}
            }
        }
    }

    fields
}

// ---------------------------------------------------------------------
// Comparison bookkeeping
// ---------------------------------------------------------------------

/// Per-field agreement counters.
#[derive(Default)]
struct Tally {
    compared: usize,
    only_ours: usize,
    only_theirs: usize,
    disagreements: Vec<String>,
    /// The coverage gap broken down by data-type identifier, which is
    /// what turns "we are behind by 92" into a diagnosis.
    gap_by_dti: BTreeMap<char, usize>,
    /// A few whole frames from the gap, one per data-type identifier.
    /// A count says a gap exists; a frame says what it is, and the
    /// next session should not have to re-instrument this file to
    /// find out.
    gap_examples: BTreeMap<char, String>,
}

impl Tally {
    /// Compares one frame's value, given an equality predicate that
    /// absorbs the reference's display rounding.
    fn compare<T: Copy + Debug>(
        &mut self,
        frame: &Frame<'_>,
        ours: Option<T>,
        theirs: Option<T>,
        agree: impl Fn(T, T) -> bool,
    ) {
        match (ours, theirs) {
            (Some(a), Some(b)) => {
                self.compared += 1;
                if !agree(a, b) {
                    self.disagreements.push(format!(
                        "frame {}: ours {a:?} vs reference {b:?}\n      {}",
                        frame.index, frame.line
                    ));
                }
            }
            (Some(_), None) => self.only_ours += 1,
            (None, Some(_)) => {
                self.only_theirs += 1;
                *self.gap_by_dti.entry(frame.dti).or_default() += 1;
                self.gap_examples
                    .entry(frame.dti)
                    .or_insert_with(|| frame.line.to_string());
            }
            (None, None) => {}
        }
    }
}

/// One frame's identity, for diagnostics.
struct Frame<'a> {
    index: usize,
    dti: char,
    line: &'a str,
}

/// Equality for values the reference prints as a rounded integer: allow
/// one unit of the printed resolution, and no more.
fn within(tolerance: f64) -> impl Fn(f64, f64) -> bool {
    move |a, b| (a - b).abs() <= tolerance
}

fn exact<T: PartialEq>(a: T, b: T) -> bool {
    a == b
}

/// Renders info-field bytes as monitor text: printable ASCII verbatim,
/// everything else as `<0xNN>`. Mirrors the convention in
/// `tests/differential.rs` and the reference's own text format.
fn monitor_escape(bytes: &[u8]) -> String {
    let mut out = String::new();
    for &b in bytes {
        if (0x20..0x7f).contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("<0x{b:02x}>"));
        }
    }
    out
}

/// Checks that our symbol wire bytes and the reference's chart text are
/// in one-to-one correspondence across the corpus.
///
/// Neither implementation's symbol chart can be derived from the
/// other's — the descriptions are independently worded — so the
/// comparable property is the *relation*: one wire pair must always
/// mean one thing, and one thing must always come from one wire pair.
/// Reading the symbol from the wrong offset in any single format breaks
/// this immediately, because that format's frames then scatter one
/// bogus pair across every description its true symbols carry.
///
/// Returns the offending mappings and the number of distinct symbols.
fn symbol_mapping_report(pairs: &[((u8, u8), String)]) -> (Vec<String>, usize) {
    let mut forward: BTreeMap<(u8, u8), BTreeSet<&str>> = BTreeMap::new();
    let mut backward: BTreeMap<&str, BTreeSet<(u8, u8)>> = BTreeMap::new();
    for (wire, text) in pairs {
        forward.entry(*wire).or_default().insert(text.as_str());
        backward.entry(text.as_str()).or_default().insert(*wire);
    }
    let mut problems = Vec::new();
    for (wire, texts) in &forward {
        if texts.len() > 1 {
            problems.push(format!(
                "symbol {:?}/{:?} described {} different ways by the reference: {:?}",
                wire.0 as char,
                wire.1 as char,
                texts.len(),
                texts
            ));
        }
    }
    for (text, wires) in &backward {
        if wires.len() > 1 {
            let rendered: Vec<String> = wires
                .iter()
                .map(|(t, c)| format!("{:?}/{:?}", *t as char, *c as char))
                .collect();
            problems.push(format!(
                "reference description {text:?} came from {} different symbols of ours: {rendered:?}",
                wires.len()
            ));
        }
    }
    (problems, forward.len())
}

// ---------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------

#[test]
#[ignore = "requires corpus/ and WARBLE_REF_APRS"]
fn aprs_fields_agree_with_reference_decoder() {
    let dir = Path::new("corpus");
    let Some(decoder) = ref_binary("WARBLE_REF_APRS") else {
        eprintln!("WARBLE_REF_APRS not set — skipping");
        return;
    };
    if !dir.is_dir() {
        eprintln!("corpus/ absent — skipping");
        return;
    }

    let mut lines = String::new();
    let mut ours: Vec<Fields> = Vec::new();
    let mut dtis: Vec<u8> = Vec::new();

    for name in FILES {
        let path = dir.join(name);
        if !path.is_file() {
            eprintln!("corpus/{name} absent — skipping");
            return;
        }
        let mut reader = hound::WavReader::open(&path).expect("opening corpus WAV");
        let rate = SampleRate::new(reader.spec().sample_rate).expect("rate");
        let mut rx: DefaultTncReceiver =
            DefaultTncReceiver::new(TncConfig::bell_202(rate).expect("config")).expect("receiver");
        let pcm: Vec<i16> = reader
            .samples::<i16>()
            .map(|s| s.expect("sample"))
            .collect();

        for s in pcm {
            let Some(frame) = rx.push_i16(s) else {
                continue;
            };
            let (dest, src) = (frame.dest(), frame.src());

            let mut dest_call = [b' '; 6];
            let cb = dest.callsign.as_bytes();
            let take = cb.len().min(6);
            dest_call[..take].copy_from_slice(&cb[..take]);

            let render = |a: &warble::ax25::Address| {
                let call = core::str::from_utf8(a.callsign.as_bytes())
                    .unwrap_or("?")
                    .to_string();
                if a.ssid.value() == 0 {
                    call
                } else {
                    format!("{call}-{}", a.ssid.value())
                }
            };
            lines.push_str(&format!(
                "{}>{}:{}\n",
                render(&src),
                render(&dest),
                monitor_escape(frame.info())
            ));
            ours.push(our_fields(&dest_call, src.ssid.value(), frame.info()));
            dtis.push(frame.info().first().copied().unwrap_or(0));
        }
    }

    let theirs = ask_reference(&decoder, &lines, ours.len());

    // ---- compare -----------------------------------------------------
    let mut tallies: BTreeMap<&str, Tally> = BTreeMap::new();
    let mut symbol_pairs: Vec<((u8, u8), String)> = Vec::new();
    let rendered: Vec<&str> = lines.lines().collect();

    for (index, (a, b)) in ours.iter().zip(theirs.iter()).enumerate() {
        let c = dtis[index];
        let frame = Frame {
            index,
            dti: if c.is_ascii_graphic() { c as char } else { '.' },
            line: rendered.get(index).copied().unwrap_or(""),
        };

        tallies.entry("position").or_default().compare(
            &frame,
            a.position,
            b.position,
            |x: Degrees, y: Degrees| {
                (x.latitude - y.latitude).abs() <= TOLERANCE_DEG
                    && (x.longitude - y.longitude).abs() <= TOLERANCE_DEG
            },
        );

        // The symbol is compared as a relation, not a value, and only
        // where at least one decoder sees a position — otherwise the
        // reference's summary line has no symbol in it and the segment
        // we would read is some other field entirely.
        if a.position.is_some() || b.position.is_some() {
            let symbol = tallies.entry("symbol").or_default();
            match (a.symbol_wire, b.symbol_text.as_ref()) {
                (Some(wire), Some(t)) => {
                    symbol.compared += 1;
                    symbol_pairs.push((wire, t.clone()));
                }
                (Some(_), None) => symbol.only_ours += 1,
                (None, Some(_)) => {
                    symbol.only_theirs += 1;
                    *symbol.gap_by_dti.entry(frame.dti).or_default() += 1;
                    symbol
                        .gap_examples
                        .entry(frame.dti)
                        .or_insert_with(|| frame.line.to_string());
                }
                (None, None) => {}
            }
        }

        let course_slack = a.course_slack;
        tallies.entry("course").or_default().compare(
            &frame,
            a.course,
            b.course,
            |x: u16, y: u16| x.abs_diff(y).min(360 - x.abs_diff(y)) <= course_slack,
        );
        // One km/h and one mph of slack, which is the reference's own
        // printed resolution — and no more, so that a knots/mph mix-up
        // (15%) or a factor-of-two cannot hide.
        tallies.entry("speed km/h").or_default().compare(
            &frame,
            a.speed_kmh,
            b.speed_kmh,
            within(1.0),
        );
        tallies.entry("speed mph").or_default().compare(
            &frame,
            a.speed_mph,
            b.speed_mph,
            within(1.0),
        );
        tallies.entry("altitude").or_default().compare(
            &frame,
            a.altitude_feet,
            b.altitude_feet,
            |x, y| (x - y).abs() <= 1,
        );
        tallies.entry("range").or_default().compare(
            &frame,
            a.range_miles,
            b.range_miles,
            within(0.6),
        );
        tallies
            .entry("phg")
            .or_default()
            .compare(&frame, a.phg, b.phg, exact);

        let (wa, wb) = (&a.weather, &b.weather);
        tallies.entry("wx wind").or_default().compare(
            &frame,
            wa.wind_mph,
            wb.wind_mph,
            within(0.1),
        );
        tallies.entry("wx direction").or_default().compare(
            &frame,
            wa.direction,
            wb.direction,
            exact,
        );
        tallies
            .entry("wx gust")
            .or_default()
            .compare(&frame, wa.gust_mph, wb.gust_mph, exact);
        tallies.entry("wx temperature").or_default().compare(
            &frame,
            wa.temperature_f,
            wb.temperature_f,
            exact,
        );
        tallies
            .entry("wx humidity")
            .or_default()
            .compare(&frame, wa.humidity, wb.humidity, exact);
        tallies.entry("wx barometer").or_default().compare(
            &frame,
            wa.barometer_inhg,
            wb.barometer_inhg,
            within(0.011),
        );
        tallies.entry("wx rain 1h").or_default().compare(
            &frame,
            wa.rain_1h_inch,
            wb.rain_1h_inch,
            within(0.005),
        );
        tallies.entry("wx rain 24h").or_default().compare(
            &frame,
            wa.rain_24h_inch,
            wb.rain_24h_inch,
            within(0.005),
        );
        tallies.entry("wx rain midnight").or_default().compare(
            &frame,
            wa.rain_midnight_inch,
            wb.rain_midnight_inch,
            within(0.005),
        );
    }

    // ---- report ------------------------------------------------------
    println!("frames: {}", ours.len());
    println!(
        "\n{:<18} {:>9} {:>9} {:>10} {:>7}",
        "field", "compared", "disagree", "only ours", "gap"
    );
    for (name, t) in &tallies {
        println!(
            "{:<18} {:>9} {:>9} {:>10} {:>7}",
            name,
            t.compared,
            t.disagreements.len(),
            t.only_ours,
            t.only_theirs
        );
    }
    println!("\ncoverage gap by data type identifier:");
    for (name, t) in &tallies {
        if t.gap_by_dti.is_empty() {
            continue;
        }
        let mut rows: Vec<_> = t.gap_by_dti.iter().collect();
        rows.sort_by(|a, b| b.1.cmp(a.1));
        let counts: Vec<String> = rows.iter().map(|(d, n)| format!("{n}x'{d}'")).collect();
        println!("  {name:<18} {}", counts.join("  "));
        for (dti, example) in &t.gap_examples {
            println!("    '{dti}': {example}");
        }
    }
    for (name, t) in &tallies {
        for line in t.disagreements.iter().take(10) {
            println!("  {name}: {line}");
        }
    }

    let (symbol_problems, distinct_symbols) = symbol_mapping_report(&symbol_pairs);
    println!("\ndistinct symbols cross-checked: {distinct_symbols}");
    for problem in symbol_problems.iter().take(20) {
        println!("  {problem}");
    }

    // ---- assert ------------------------------------------------------
    let mut failures: Vec<String> = Vec::new();
    for &(name, floor) in MIN_COMPARED {
        let compared = tallies.get(name).map_or(0, |t| t.compared);
        if compared < floor {
            failures.push(format!(
                "{name}: only {compared} frames compared, expected at least {floor} — \
                 decoding regressed"
            ));
        }
    }
    for &(name, ceiling) in MAX_GAP {
        let gap = tallies.get(name).map_or(0, |t| t.only_theirs);
        if gap > ceiling {
            failures.push(format!(
                "{name}: coverage gap is {gap}, at most {ceiling} expected — the \
                 reference decodes values we no longer do"
            ));
        }
    }
    for (name, t) in &tallies {
        if !t.disagreements.is_empty() {
            failures.push(format!(
                "{name}: {} disagreement(s) with the reference decoder; every value \
                 we decode must match an independent implementation",
                t.disagreements.len()
            ));
        }
    }
    if distinct_symbols < MIN_DISTINCT_SYMBOLS {
        failures.push(format!(
            "only {distinct_symbols} distinct symbols cross-checked, expected at \
             least {MIN_DISTINCT_SYMBOLS}"
        ));
    }
    failures.extend(symbol_problems);

    assert!(
        failures.is_empty(),
        "field-level differential failed:\n  {}",
        failures.join("\n  ")
    );
}

// ---------------------------------------------------------------------
// Synthetic formats: what the corpus does not contain
// ---------------------------------------------------------------------

/// Builds one `AprsPacket` and returns its information field.
fn build_info(packet: &AprsPacket<'_>, label: &str) -> Vec<u8> {
    let mut buf = [0u8; 256];
    let len = packet
        .build(&mut buf)
        .unwrap_or_else(|e| panic!("{label}: build failed: {e}"));
    buf[..len].to_vec()
}

/// The chapter 9 example coordinates: 49°30'N, 72°45'W, symbol `/>`.
fn compressed_position(cs: CompressedCs) -> PositionCs<'static> {
    PositionCs {
        position: Position {
            ambiguity: Ambiguity::EXACT,
            latitude: Latitude::new(49 * 6000 + 3000).expect("latitude"),
            longitude: Longitude::new(-(72 * 6000 + 4500)).expect("longitude"),
            symbol: Symbol::CAR,
            messaging: true,
            compressed: true,
            extension: None,
            comment: b"",
        },
        cs,
        // A non-GGA source, so `cs` is read as course/speed or range;
        // `PositionCs::build` forces GGA for the altitude variant.
        compression_type: CompressionType {
            current_fix: true,
            nmea_source: NmeaSource::Rmc,
            origin: CompressionOrigin::Software,
        },
    }
}

/// Every field the reference decodes must agree on the formats the
/// **corpus does not contain**.
///
/// # Why this test exists
///
/// `aprs_positions_agree_with_reference_decoder` above is worth a great
/// deal and covers only what happened to be on the air in southern
/// California on one afternoon. MEASURED: of 462 position reports in
/// that corpus, **zero are compressed**, and there is not one `RNG` or
/// `DFS` extension in the whole 2182 frames.
///
/// So the base-91 compressed position family — a first-class APRS
/// format, and the one with *exponential* wire encodings for speed
/// (`1.08^n`) and altitude (`1.002^n`) — had no independent
/// verification whatsoever. `tests/differential.rs` looks like it
/// covers this and does not: it builds a compressed position, parses it
/// with **our own** decoder, asserts equality, and then checks the
/// reference recovers the same *bytes* off the air. That proves the
/// modem. Nobody ever asked the reference what latitude it read.
///
/// A wrong exponent base would pass every test in this crate. That is
/// the exact shape of the two defects the FT8 differential found and of
/// the IL2P defect before it, so leaving it unchecked was not a
/// defensible risk.
///
/// # Why it needs no corpus
///
/// The reference decoder takes monitor-format text on standard input,
/// so a comparison needs neither audio nor recordings — only our own
/// builders. That makes this cheaper than the corpus test *and* able to
/// sweep values the air never happened to carry.
#[test]
#[ignore = "requires WARBLE_REF_APRS"]
fn synthetic_formats_agree_with_reference_decoder() {
    let Some(decoder) = ref_binary("WARBLE_REF_APRS") else {
        eprintln!("WARBLE_REF_APRS not set — skipping");
        return;
    };

    // (label, packet) pairs, built once so `ours` and the lines stay in
    // step by construction.
    let mut labels: Vec<String> = Vec::new();
    let mut lines = String::new();
    let mut ours: Vec<Fields> = Vec::new();

    // Our side is the projection of the **re-parsed** bytes, not of the
    // packet that was asked for.
    //
    // That distinction is the whole point here and it is easy to get
    // wrong — this test did, on its first run. The compressed formats
    // are lossy: speed is an exponent of 1.08 and altitude of 1.002,
    // so "500 knots" is not representable and the nearest wire value
    // means 508.6. Comparing the *request* against the reference's
    // reading of the *bytes* produces six disagreements that are
    // nobody's bug.
    //
    // Re-parsing asks the question that matters: given these exact
    // bytes, do the two implementations agree what they mean? A shared
    // wrong exponent base in our encoder and decoder still agrees with
    // itself and now disagrees with the reference, which is the failure
    // this exists to catch.
    let mut push = |label: String, packet: &AprsPacket<'_>| {
        let info = build_info(packet, &label);
        let reparsed = AprsPacket::parse(&info)
            .unwrap_or_else(|e| panic!("{label}: our own decoder rejected our bytes: {e}"));
        lines.push_str(&format!("K1ABC>APN123:{}\n", monitor_escape(&info)));
        ours.push(packet_fields(&reparsed));
        labels.push(label);
    };

    // ---- compressed, no cs data ------------------------------------
    push(
        "compressed/nodata".to_string(),
        &AprsPacket::Position(compressed_position(CompressedCs::NoData).position),
    );

    // ---- compressed course/speed, swept ----------------------------
    //
    // Course has 4-degree wire resolution and speed is an exponent of
    // 1.08, so this sweep is where a wrong base shows up: at 1018 knots
    // the top of the range, a base of 1.07 instead of 1.08 would be out
    // by more than a factor of three.
    for course in [0u16, 4, 88, 180, 264, 356] {
        for speed in [0u16, 1, 10, 36, 100, 500, 1018] {
            push(
                format!("compressed/cs course={course} speed={speed}"),
                &AprsPacket::PositionCs(compressed_position(CompressedCs::CourseSpeed {
                    course,
                    speed,
                })),
            );
        }
    }

    // ---- compressed radio range, swept -----------------------------
    for miles in [2u16, 4, 10, 20, 100, 500, 2038] {
        push(
            format!("compressed/range miles={miles}"),
            &AprsPacket::PositionCs(compressed_position(CompressedCs::RadioRange { miles })),
        );
    }

    // ---- compressed altitude, swept --------------------------------
    //
    // 1.002^n over two base-91 digits, so the exponent is applied up to
    // 8280 times; a wrong base is enormous at the top of the range.
    for feet in [1u32, 10, 100, 1234, 10_000, 100_000, 1_000_000] {
        push(
            format!("compressed/altitude feet={feet}"),
            &AprsPacket::PositionCs(compressed_position(CompressedCs::Altitude { feet })),
        );
    }

    // ---- uncompressed RNG extension, swept -------------------------
    for miles in [1u16, 5, 20, 50, 100, 1000, 9999] {
        let position = Position {
            ambiguity: Ambiguity::EXACT,
            latitude: Latitude::new(49 * 6000 + 350).expect("latitude"),
            longitude: Longitude::new(-(72 * 6000 + 175)).expect("longitude"),
            symbol: Symbol::from_wire(b'/', b'#'),
            messaging: false,
            compressed: false,
            extension: Some(DataExtension::Range { miles }),
            comment: b"",
        };
        push(
            format!("uncompressed/RNG miles={miles}"),
            &AprsPacket::Position(position),
        );
    }

    let theirs = ask_reference(&decoder, &lines, ours.len());

    // ---- compare ---------------------------------------------------
    let mut tallies: BTreeMap<&str, Tally> = BTreeMap::new();
    for (index, (a, b)) in ours.iter().zip(theirs.iter()).enumerate() {
        let frame = Frame {
            index,
            dti: '~',
            line: &labels[index],
        };
        tallies.entry("position").or_default().compare(
            &frame,
            a.position,
            b.position,
            |x: Degrees, y: Degrees| {
                (x.latitude - y.latitude).abs() <= TOLERANCE_DEG
                    && (x.longitude - y.longitude).abs() <= TOLERANCE_DEG
            },
        );
        tallies
            .entry("course")
            .or_default()
            .compare(&frame, a.course, b.course, exact);
        tallies.entry("speed km/h").or_default().compare(
            &frame,
            a.speed_kmh,
            b.speed_kmh,
            within(1.0),
        );
        tallies.entry("speed mph").or_default().compare(
            &frame,
            a.speed_mph,
            b.speed_mph,
            within(1.0),
        );
        // The reference prints altitude in whole metres and whole feet;
        // one foot of slack is its own printed resolution.
        tallies.entry("altitude").or_default().compare(
            &frame,
            a.altitude_feet,
            b.altitude_feet,
            |x, y| (x - y).abs() <= 1,
        );
        tallies.entry("range").or_default().compare(
            &frame,
            a.range_miles,
            b.range_miles,
            within(0.6),
        );
    }

    println!("synthetic frames: {}", ours.len());
    println!(
        "\n{:<12} {:>9} {:>9} {:>10} {:>7}",
        "field", "compared", "disagree", "only ours", "gap"
    );
    for (name, t) in &tallies {
        println!(
            "{:<12} {:>9} {:>9} {:>10} {:>7}",
            name,
            t.compared,
            t.disagreements.len(),
            t.only_ours,
            t.only_theirs
        );
    }
    for (name, t) in &tallies {
        for line in t.disagreements.iter().take(12) {
            println!("  {name}: {line}");
        }
    }

    // ---- assert ----------------------------------------------------
    //
    // These are equalities, not ratchets: every frame here is one this
    // test built, so the counts are a property of the case list rather
    // than of what was on the air. A count that moves means somebody
    // changed the sweep, and should say so.
    let mut failures: Vec<String> = Vec::new();
    for &(name, want) in SYNTHETIC_COMPARED {
        let got = tallies.get(name).map_or(0, |t| t.compared);
        if got != want {
            failures.push(format!(
                "{name}: {got} frames compared, expected exactly {want} — the case \
                 list or the reference's output format changed"
            ));
        }
    }
    for (name, t) in &tallies {
        if !t.disagreements.is_empty() {
            failures.push(format!(
                "{name}: {} disagreement(s) with the reference decoder",
                t.disagreements.len()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "synthetic differential failed:\n  {}",
        failures.join("\n  ")
    );
}
