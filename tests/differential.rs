//! Seeded differential test harness: hundreds of deterministic APRS
//! packets, checked three ways against the external reference tools
//! (see `tests/oracle.rs` for the env-var convention):
//!
//! 1. our encode -> our decode round-trips to equal typed values;
//! 2. our TNC transmit -> WAV -> reference decoder agrees on the
//!    source, destination, path and info bytes;
//! 3. our monitor text -> reference generator -> WAV -> our TNC
//!    receiver recovers the same frame.
//!
//! Plus a quantified SNR shootout: the same seeded-noise audio decoded
//! by our `TncReceiver` and by the reference decoder, with the
//! assertion that our success count is >= the reference's at every
//! noise level.
//!
//! All tests are `#[ignore]`-gated: they need the reference binaries
//! via `YODEL_REF_GEN` / `YODEL_REF_DECODE`. Everything is seeded
//! (LCG) — no time, no external randomness.
//!
//! ## Normalization of the reference decoder's output
//!
//! The reference decoder prints one monitor line per frame in the form
//! `[chan] SRC>DEST[,PATH]:INFO`, rendering printable ASCII verbatim
//! and control bytes as `<0xNN>` (lowercase hex). Our corpus is
//! restricted to printable info bytes, except that the reference
//! *generator* carries the frame file's trailing newline into the info
//! field; the expected-line builders below escape via the same rule and
//! append the `<0x0a>` where a generator-sourced newline is expected.

#![cfg(all(feature = "tnc", feature = "micE"))]

use std::path::PathBuf;
use std::process::Command;

use yodel::SampleRate;
use yodel::aprs::mic_e::{self, MicE, MicEFix, MicEMessage};
use yodel::aprs::{
    Addressee, AprsPacket, CompressedCs, CompressionType, Item, Latitude, Longitude, Message,
    MessageContent, NmeaSource, Object, Position, PositionCs, PositionTimestamped, PositionWeather,
    PositionlessWeather, Status, Symbol, Telemetry, Timestamp, WeatherReport,
};
use yodel::ax25::Address;
use yodel::geo::{Ambiguity, LatitudeHemisphere, LongitudeHemisphere, UNITS_PER_HUNDREDTH_MINUTE};
use yodel::tnc::{DefaultTncReceiver, TncConfig, TncTransmitter};
use yodel::units::{Humidity, Pressure, Rainfall, Speed, Temperature};
use yodel::{ModemProfile, TonePair};

const SAMPLE_RATE: u32 = 44_100;

// ---------------------------------------------------------------------
// Reference-binary plumbing (same conventions as tests/oracle.rs).
// ---------------------------------------------------------------------

fn env_binary(var: &str) -> PathBuf {
    let path = std::env::var_os(var).unwrap_or_else(|| {
        panic!(
            "{var} is not set. Set YODEL_REF_GEN and YODEL_REF_DECODE to the \
             reference generator/decoder binaries, then run `cargo test -- --ignored`."
        )
    });
    let path = PathBuf::from(path);
    assert!(
        path.is_file(),
        "{var}={} does not point to an existing file. Unset the variable \
         to skip this suite deliberately; leaving it set and wrong would \
         pass without testing anything.",
        path.display()
    );
    path
}

/// Resolves one reference-binary variable: `None` when unset (a
/// legitimate skip), a hard failure when set to something that is not a
/// file.
fn ref_binary(var: &str) -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os(var)?);
    assert!(
        path.is_file(),
        "{var}={} does not point to an existing file. Unset the variable \
         to skip this suite deliberately; leaving it set and wrong would \
         pass without testing anything.",
        path.display()
    );
    Some(path)
}

fn scratch_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scratch");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Writes mono 16-bit PCM to a WAV in the scratch directory.
fn write_wav(name: &str, samples: &[i16]) -> PathBuf {
    let wav_path = scratch_dir().join(name);
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&wav_path, spec).unwrap();
    for s in samples {
        writer.write_sample(*s).unwrap();
    }
    writer.finalize().unwrap();
    wav_path
}

/// Runs the reference decoder over a WAV and returns its stdout.
fn run_ref_decoder(wav_path: &PathBuf) -> String {
    run_ref_decoder_args(wav_path, &[])
}

/// Like [`run_ref_decoder`] with extra leading arguments (e.g. a baud
/// selection).
fn run_ref_decoder_args(wav_path: &PathBuf, extra: &[&str]) -> String {
    let decode = env_binary("YODEL_REF_DECODE");
    let output = Command::new(&decode)
        .args(extra)
        .arg(wav_path)
        .output()
        .expect("failed to run reference decoder");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Parses the reference decoder's "<n> packets decoded" trailer.
fn decoded_packet_count(stdout: &str) -> usize {
    let line = stdout
        .lines()
        .find(|l| l.contains("packets decoded"))
        .unwrap_or_else(|| panic!("no 'packets decoded' line in decoder output:\n{stdout}"));
    let head = line.split("packets decoded").next().unwrap();
    let digits: String = head
        .trim_end()
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    digits
        .parse()
        .unwrap_or_else(|_| panic!("could not parse packet count from line: {line}"))
}

/// Runs the reference generator over a monitor-format frame file and
/// returns the generated WAV's samples.
fn run_ref_generator(frame_file: &PathBuf, wav_name: &str) -> Vec<i16> {
    run_ref_generator_args(frame_file, wav_name, &[])
}

/// Like [`run_ref_generator`] with extra arguments (e.g. a baud
/// selection).
fn run_ref_generator_args(frame_file: &PathBuf, wav_name: &str, extra: &[&str]) -> Vec<i16> {
    let gen_bin = env_binary("YODEL_REF_GEN");
    let wav_path = scratch_dir().join(wav_name);
    let output = Command::new(&gen_bin)
        .args(extra)
        .arg("-r")
        .arg(SAMPLE_RATE.to_string())
        .arg("-o")
        .arg(&wav_path)
        .arg(frame_file)
        .output()
        .expect("failed to run reference generator");
    assert!(
        output.status.success(),
        "reference generator failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut reader = hound::WavReader::open(&wav_path).unwrap();
    assert_eq!(reader.spec().sample_rate, SAMPLE_RATE);
    reader.samples::<i16>().map(|s| s.unwrap()).collect()
}

/// Renders raw info bytes the way the reference decoder prints them in
/// its monitor line: printable ASCII verbatim, control bytes as
/// `<0xNN>` (lowercase hex).
fn monitor_escape(bytes: &[u8]) -> String {
    let mut out = String::new();
    for &b in bytes {
        if b < 0x20 || b == 0x7f {
            out.push_str(&format!("<0x{b:02x}>"));
        } else {
            out.push(b as char);
        }
    }
    out
}

/// True when both reference-binary env vars name real files; otherwise
/// prints a skip notice and returns false.
///
/// Both variables are resolved *before* the skip decision, on purpose:
/// [`ref_binary`] fails on a set-but-wrong path, so a typo in one is
/// reported even when the other is absent. Testing `is_none()` first --
/// which is what this did -- let `YODEL_REF_GEN=/typo` with
/// `YODEL_REF_DECODE` unset skip in silence, the one combination that
/// slipped through the rule this suite is built on.
fn ref_binaries_available() -> bool {
    let generator = ref_binary("YODEL_REF_GEN");
    let decoder = ref_binary("YODEL_REF_DECODE");
    if generator.is_none() || decoder.is_none() {
        eprintln!(
            "skipping: set YODEL_REF_GEN and YODEL_REF_DECODE to the \
             reference generator/decoder binaries to run this test"
        );
        return false;
    }
    true
}

// ---------------------------------------------------------------------
// Seeded LCG (same constants as the crate's other seeded sweeps).
// ---------------------------------------------------------------------

struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Lcg(seed)
    }

    fn next(&mut self, bound: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % bound
    }

    /// Uniform in [-1.0, 1.0).
    fn next_f64(&mut self) -> f64 {
        (self.next(1 << 31) as f64 / f64::from(1u32 << 30)) - 1.0
    }
}

// ---------------------------------------------------------------------
// Corpus generation.
// ---------------------------------------------------------------------

/// One corpus case: full frame addressing plus the raw info bytes
/// (already round-tripped through our own typed encode/decode).
struct TxCase {
    src: String,
    /// Destination text; for Mic-E this carries half the position.
    dest: String,
    path: Vec<String>,
    info: Vec<u8>,
    kind: &'static str,
    /// True when the info field is Mic-E (typed re-decode needs the
    /// destination).
    mic_e: bool,
}

impl TxCase {
    /// The monitor-format header `SRC>DEST[,PATH]`.
    fn header(&self) -> String {
        let mut h = format!("{}>{}", self.src, self.dest);
        for digi in &self.path {
            h.push(',');
            h.push_str(digi);
        }
        h
    }

    /// The monitor line we feed the reference generator (no newline).
    fn monitor_line(&self) -> String {
        format!("{}:{}", self.header(), monitor_escape(&self.info))
    }
}

const COMMENTS: [&[u8]; 4] = [b"", b"case ", b"yodel diff ", b"trail "];
const SYMBOL_CODES: [u8; 5] = [b'#', b'>', b'j', b'O', b'-'];
const SYMBOL_TABLES: [u8; 3] = [b'/', b'\\', b'Q'];

/// A latitude in coordinate storage units for quadrant `q & 1`
/// (0 north), drawn on the 1/100 arc-minute grid.
///
/// The draw is composed in hundredths of an arc-minute -- the
/// resolution every wire format here carries -- and scaled to storage
/// units at the end, because that is what `Latitude::new` counts. The
/// scaling is not cosmetic: a bare hundredths count is a legal
/// magnitude, so `Latitude::new` accepts it silently and every case in
/// the corpus becomes 0000.00N/00000.00W. Staying on the hundredths
/// grid also matters, because both the uncompressed and Mic-E formats
/// round to it on the way out and an off-grid value would fail its own
/// round trip for reasons that are nobody's bug.
fn rand_lat(rng: &mut Lcg, q: u64) -> i64 {
    let hundredths = (rng.next(90) * 6000 + rng.next(60) * 100 + rng.next(100)) as i64;
    let v = hundredths * UNITS_PER_HUNDREDTH_MINUTE;
    if q & 1 == 0 { v } else { -v }
}

/// A longitude in coordinate storage units for quadrant bit `q & 2`
/// (0 east), drawn on the 1/100 arc-minute grid. See [`rand_lat`].
fn rand_lon(rng: &mut Lcg, q: u64) -> i64 {
    let hundredths = (rng.next(180) * 6000 + rng.next(60) * 100 + rng.next(100)) as i64;
    let v = hundredths * UNITS_PER_HUNDREDTH_MINUTE;
    if q & 2 == 0 { v } else { -v }
}

fn rand_timestamp(rng: &mut Lcg) -> Timestamp {
    match rng.next(3) {
        0 => Timestamp::DhmZulu {
            day: rng.next(28) as u8 + 1,
            hour: rng.next(24) as u8,
            minute: rng.next(60) as u8,
        },
        1 => Timestamp::DhmLocal {
            day: rng.next(28) as u8 + 1,
            hour: rng.next(24) as u8,
            minute: rng.next(60) as u8,
        },
        _ => Timestamp::Hms {
            hour: rng.next(24) as u8,
            minute: rng.next(60) as u8,
            second: rng.next(60) as u8,
        },
    }
}

/// A random weather report.
///
/// The wind speed is built from the unit of whichever wire form the
/// caller will write it into, because the two disagree: a positionless
/// report spells `sNNN` in miles per hour, a position report's
/// `DDD/SSS` data extension in knots. Building in the wrong one and
/// asserting a byte-exact round trip would still pass — the value is
/// never converted — which is exactly the defect this typing removes,
/// so the caller has to say.
fn rand_weather(rng: &mut Lcg, wind_knots: bool) -> WeatherReport {
    let wind = rng.next(200) as i32;
    WeatherReport {
        wind_direction: Some(rng.next(360) as u16),
        wind_speed: Some(if wind_knots {
            Speed::from_knots(wind)
        } else {
            Speed::from_mph(wind)
        }),
        gust: Some(Speed::from_mph(rng.next(200) as i32)),
        temperature: Some(Temperature::from_fahrenheit(rng.next(170) as i32 - 50)),
        rain_1h: Some(Rainfall::from_hundredths_inch(rng.next(500) as i32)),
        rain_24h: if rng.next(2) == 0 {
            Some(Rainfall::from_hundredths_inch(rng.next(500) as i32))
        } else {
            None
        },
        rain_midnight: None,
        humidity: Humidity::new(rng.next(99) as u8 + 1).ok(),
        barometric_pressure: Some(Pressure::from_tenths_hpa(9_000 + rng.next(2_000) as i32)),
        // Chapter 12's optional "other parameters". Both layouts can now
        // spell both, but only when present, and these reports are built
        // in both — so leaving them out keeps the generator's output the
        // nine standard fields in either form.
        luminosity: None,
        snowfall: None,
    }
}

/// A unique free-text tail carrying the case index.
fn tag(i: usize) -> Vec<u8> {
    format!("c{i:03}").into_bytes()
}

/// Builds `packet` into bytes and asserts our decode returns exactly
/// equal typed values.
fn build_and_round_trip(packet: &AprsPacket<'_>, kind: &str, i: usize) -> Vec<u8> {
    let mut buf = [0u8; 256];
    let len = packet.build(&mut buf).unwrap();
    let info = buf[..len].to_vec();
    let reparsed = AprsPacket::parse(&info)
        .unwrap_or_else(|e| panic!("case {i} ({kind}): our decode failed: {e}"));
    assert_eq!(&reparsed, packet, "case {i} ({kind}): round trip mismatch");
    info
}

/// Builds the canonical (wire-stable) form of a report whose wire
/// format cannot hold every value we can ask for, and asserts the
/// round trip at the fixed point instead of on the first build.
///
/// Two independent quantizations need this:
///
/// * the exponential course/speed/range/altitude `csT` wire codes round
///   *to the nearest representable value* on build (and the altitude
///   parse truncates to whole feet);
/// * **any base-91 compressed position**, `csT` or not. The compressed
///   coordinate grid is one step per 900 000 000 storage units, and the
///   1/100 arc-minute grid the cases are drawn on is 57 138 900 000 --
///   neither divides the other, so a legal drawn coordinate is almost
///   never representable and build lands on a neighbouring grid point.
///   That is the format's documented resolution, not a defect, so
///   `pos_compressed_nodata` belongs here rather than in
///   [`build_and_round_trip`] despite carrying no `cs` field at all.
///
/// build+parse is iterated to its fixed point (a few steps: the parsed
/// value only ever moves toward a representable code) and the fixed
/// point is asserted.
fn canonicalize_cs(packet: &AprsPacket<'_>, kind: &str, i: usize) -> Vec<u8> {
    let mut buf = [0u8; 256];
    let len = packet.build(&mut buf).unwrap();
    let mut wire = buf[..len].to_vec();
    for _ in 0..8 {
        let canon = AprsPacket::parse(&wire)
            .unwrap_or_else(|e| panic!("case {i} ({kind}): our decode failed: {e}"));
        let mut buf2 = [0u8; 256];
        let len2 = canon.build(&mut buf2).unwrap();
        let next = buf2[..len2].to_vec();
        if next == wire {
            // Exact typed round trip at the fixed point.
            let reparsed = AprsPacket::parse(&wire).unwrap();
            assert_eq!(reparsed, canon, "case {i} ({kind}): fixed point broken");
            return wire;
        }
        wire = next;
    }
    panic!("case {i} ({kind}): canonical form did not converge");
}

const ALL_MIC_E_MESSAGES: [MicEMessage; 15] = [
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

/// The number of distinct packet kinds in the corpus rotation.
const KINDS: usize = 16;
/// Rounds through the kind rotation: KINDS * ROUNDS cases total.
const ROUNDS: usize = 20;

/// Floor on the corpus, out of the `KINDS * ROUNDS` = 320 cases
/// [`generate_corpus`] builds.
///
/// Every comparison in [`differential_corpus`] is inside a loop over the
/// corpus, so a corpus that came back short -- or empty -- would report
/// agreement having compared little or nothing.
const MIN_CORPUS_CASES: usize = 300;

/// Generates the full deterministic corpus (KINDS * ROUNDS = 320
/// cases), asserting our typed encode -> decode round trip for each.
fn generate_corpus() -> Vec<TxCase> {
    let mut rng = Lcg::new(0x5EED_D1FF_0000_0001);
    let mut cases = Vec::new();
    for round in 0..ROUNDS {
        for kind_idx in 0..KINDS {
            let i = cases.len();
            let q = (round + kind_idx) as u64 % 4; // quadrant rotation
            let src = match rng.next(16) {
                0 => format!("N{}CALL", rng.next(10)),
                ssid => format!("N{}CALL-{ssid}", rng.next(10)),
            };
            let dest = if kind_idx == 15 {
                String::new() // Mic-E overwrites below
            } else {
                ["APRS", "APZ001", "APWDIF"][rng.next(3) as usize].to_string()
            };
            let path = match rng.next(3) {
                0 => vec![],
                1 => vec!["WIDE1-1".to_string()],
                _ => vec!["WIDE1-1".to_string(), "WIDE2-1".to_string()],
            };
            let mut comment = COMMENTS[rng.next(4) as usize].to_vec();
            comment.extend_from_slice(&tag(i));

            let (kind, dest, info, is_mic_e) = match kind_idx {
                0 | 1 => {
                    let kind = if kind_idx == 0 {
                        "pos_uncompressed"
                    } else {
                        "pos_uncompressed_msg"
                    };
                    let p = AprsPacket::Position(Position {
                        ambiguity: Ambiguity::EXACT,
                        latitude: Latitude::new(rand_lat(&mut rng, q)).unwrap(),
                        longitude: Longitude::new(rand_lon(&mut rng, q)).unwrap(),
                        symbol: Symbol::from_wire(
                            SYMBOL_TABLES[rng.next(3) as usize],
                            SYMBOL_CODES[rng.next(5) as usize],
                        ),
                        messaging: kind_idx == 1,
                        compressed: false,
                        extension: None,
                        comment: &comment,
                    });
                    (kind, dest, build_and_round_trip(&p, kind, i), false)
                }
                2 => {
                    let p = AprsPacket::Position(Position {
                        ambiguity: Ambiguity::EXACT,
                        latitude: Latitude::new(rand_lat(&mut rng, q)).unwrap(),
                        longitude: Longitude::new(rand_lon(&mut rng, q)).unwrap(),
                        symbol: Symbol::from_wire(b'/', SYMBOL_CODES[rng.next(5) as usize]),
                        messaging: rng.next(2) == 0,
                        compressed: true,
                        extension: None,
                        comment: &comment,
                    });
                    let kind = "pos_compressed_nodata";
                    // Compressed: quantized to the base-91 coordinate
                    // grid, so the fixed point is the only stable form.
                    (kind, dest, canonicalize_cs(&p, kind, i), false)
                }
                3..=5 => {
                    let (kind, cs, t) = match kind_idx {
                        3 => (
                            "pos_cs_course_speed",
                            CompressedCs::CourseSpeed {
                                course: rng.next(360) as u16,
                                speed: rng.next(1019) as u16,
                            },
                            CompressionType {
                                current_fix: rng.next(2) == 0,
                                ..CompressionType::default()
                            },
                        ),
                        4 => (
                            "pos_cs_radio_range",
                            CompressedCs::RadioRange {
                                miles: 2 + rng.next(2_037) as u16,
                            },
                            CompressionType::default(),
                        ),
                        _ => (
                            "pos_cs_altitude",
                            CompressedCs::Altitude {
                                feet: 1 + rng.next(100_000) as u32,
                            },
                            CompressionType {
                                current_fix: true,
                                nmea_source: NmeaSource::Gga,
                                ..CompressionType::default()
                            },
                        ),
                    };
                    let p = AprsPacket::PositionCs(PositionCs {
                        position: Position {
                            ambiguity: Ambiguity::EXACT,
                            latitude: Latitude::new(rand_lat(&mut rng, q)).unwrap(),
                            longitude: Longitude::new(rand_lon(&mut rng, q)).unwrap(),
                            symbol: Symbol::from_wire(b'/', SYMBOL_CODES[rng.next(5) as usize]),
                            messaging: rng.next(2) == 0,
                            compressed: true,
                            extension: None,
                            comment: &comment,
                        },
                        cs,
                        compression_type: t,
                    });
                    let info = canonicalize_cs(&p, kind, i);
                    (kind, dest, info, false)
                }
                6 | 7 => {
                    let compressed = kind_idx == 7;
                    let kind = if compressed {
                        "pos_ts_compressed"
                    } else {
                        "pos_ts_uncompressed"
                    };
                    let p = AprsPacket::PositionTimestamped(PositionTimestamped {
                        timestamp: rand_timestamp(&mut rng),
                        position: Position {
                            ambiguity: Ambiguity::EXACT,
                            latitude: Latitude::new(rand_lat(&mut rng, q)).unwrap(),
                            longitude: Longitude::new(rand_lon(&mut rng, q)).unwrap(),
                            symbol: Symbol::from_wire(b'/', SYMBOL_CODES[rng.next(5) as usize]),
                            messaging: rng.next(2) == 0, // '@' vs '/'
                            compressed,
                            extension: None,
                            comment: &comment,
                        },
                        cs: CompressedCs::NoData,
                        compression_type: CompressionType::default(),
                    });
                    // Same split as kinds 2 and 3..=5: the compressed
                    // spelling quantizes the coordinate to the base-91
                    // grid and only its fixed point round-trips, while
                    // the uncompressed one carries the drawn hundredths
                    // exactly and must round-trip on the first build.
                    let info = if compressed {
                        canonicalize_cs(&p, kind, i)
                    } else {
                        build_and_round_trip(&p, kind, i)
                    };
                    (kind, dest, info, false)
                }
                8 => {
                    let id = tag(i);
                    let text_buf;
                    let content = match rng.next(4) {
                        0 => MessageContent::Text {
                            text: {
                                text_buf = [b"hello " as &[u8], &tag(i)].concat();
                                &text_buf
                            },
                            id: None,
                        },
                        1 => MessageContent::Text {
                            text: {
                                text_buf = [b"ping " as &[u8], &tag(i)].concat();
                                &text_buf
                            },
                            id: Some(&id[..4]),
                        },
                        2 => MessageContent::Ack { id: &id[1..4] },
                        _ => MessageContent::Reject { id: &id[1..4] },
                    };
                    let p = AprsPacket::Message(Message {
                        addressee: Addressee::new(b"N9CALL-9").unwrap(),
                        content,
                    });
                    (
                        "message",
                        dest,
                        build_and_round_trip(&p, "message", i),
                        false,
                    )
                }
                9 => {
                    let text = [b"status " as &[u8], &tag(i)].concat();
                    let p = AprsPacket::Status(Status { text: &text });
                    ("status", dest, build_and_round_trip(&p, "status", i), false)
                }
                10 => {
                    // Weather `rest` starts with a non-tag byte so the
                    // tagged-field scanner cannot re-consume it.
                    let rest = [b"wx " as &[u8], &tag(i)].concat();
                    let p = AprsPacket::Weather(PositionlessWeather {
                        month: rng.next(12) as u8 + 1,
                        day: rng.next(28) as u8 + 1,
                        hour: rng.next(24) as u8,
                        minute: rng.next(60) as u8,
                        weather: rand_weather(&mut rng, false),
                        rest: &rest,
                    });
                    let kind = "wx_positionless";
                    (kind, dest, build_and_round_trip(&p, kind, i), false)
                }
                11 => {
                    let rest = [b"wx " as &[u8], &tag(i)].concat();
                    let p = AprsPacket::PositionWeather(PositionWeather {
                        ambiguity: Ambiguity::EXACT,
                        latitude: Latitude::new(rand_lat(&mut rng, q)).unwrap(),
                        longitude: Longitude::new(rand_lon(&mut rng, q)).unwrap(),
                        symbol: Symbol::WEATHER_STATION,
                        messaging: rng.next(2) == 0,
                        // Chapter 12's four uncompressed spellings:
                        // `!`/`=` plain, `/`/`@` timestamped.
                        timestamp: if rng.next(2) == 0 {
                            None
                        } else {
                            Some(rand_timestamp(&mut rng))
                        },
                        weather: rand_weather(&mut rng, true),
                        rest: &rest,
                    });
                    let kind = "wx_position";
                    (kind, dest, build_and_round_trip(&p, kind, i), false)
                }
                12 => {
                    let rest = [b"," as &[u8], &tag(i)].concat();
                    let p = AprsPacket::Telemetry(Telemetry {
                        seq: rng.next(1000) as u32,
                        // Bounded by 256, so the cast is exact.
                        analog: Telemetry::integer_channels([
                            rng.next(256) as i64,
                            rng.next(256) as i64,
                            rng.next(256) as i64,
                            rng.next(256) as i64,
                            rng.next(256) as i64,
                        ]),
                        digital: Some([
                            rng.next(2) == 0,
                            rng.next(2) == 0,
                            rng.next(2) == 0,
                            rng.next(2) == 0,
                            rng.next(2) == 0,
                            rng.next(2) == 0,
                            rng.next(2) == 0,
                            rng.next(2) == 0,
                        ]),
                        rest: &rest,
                    });
                    let kind = "telemetry";
                    (kind, dest, build_and_round_trip(&p, kind, i), false)
                }
                13 => {
                    let name = format!("MK{}{:03}", (b'A' + rng.next(26) as u8) as char, i);
                    let p = AprsPacket::Object(Object {
                        ambiguity: Ambiguity::EXACT,
                        name: name.as_bytes(),
                        live: rng.next(2) == 0,
                        timestamp: rand_timestamp(&mut rng),
                        latitude: Latitude::new(rand_lat(&mut rng, q)).unwrap(),
                        longitude: Longitude::new(rand_lon(&mut rng, q)).unwrap(),
                        symbol: Symbol::from_wire(b'/', SYMBOL_CODES[rng.next(5) as usize]),
                        compressed: false,
                        comment: &comment,
                    });
                    ("object", dest, build_and_round_trip(&p, "object", i), false)
                }
                14 => {
                    let name = format!("IT{}{:03}", (b'A' + rng.next(26) as u8) as char, i);
                    let p = AprsPacket::Item(Item {
                        ambiguity: Ambiguity::EXACT,
                        name: name.as_bytes(),
                        live: rng.next(2) == 0,
                        latitude: Latitude::new(rand_lat(&mut rng, q)).unwrap(),
                        longitude: Longitude::new(rand_lon(&mut rng, q)).unwrap(),
                        symbol: Symbol::from_wire(b'/', SYMBOL_CODES[rng.next(5) as usize]),
                        compressed: false,
                        comment: &comment,
                    });
                    ("item", dest, build_and_round_trip(&p, "item", i), false)
                }
                _ => {
                    // Mic-E: rotate quadrants, cycle message codes and
                    // ambiguity 0-4, vary altitude/speed/course.
                    //
                    // Blanked (ambiguous) digits decode as zero, so the
                    // corresponding latitude digits are zeroed up front
                    // for an exact typed round trip. Values are also
                    // constrained so every info byte stays printable
                    // ASCII (32..=126): the reference generator takes
                    // monitor *text*, so control/DEL info bytes cannot
                    // be synthesized for direction (c). Documented in
                    // docs/COVERAGE.md.
                    let ambiguity = (round % 5) as u8;
                    let lat_deg = rng.next(90) as i64;
                    let mut lat_min = rng.next(60) as i64;
                    let mut lat_hh = rng.next(100) as i64;
                    match ambiguity {
                        0 => {}
                        1 => lat_hh = lat_hh / 10 * 10,
                        2 => lat_hh = 0,
                        3 => {
                            lat_hh = 0;
                            lat_min = lat_min / 10 * 10;
                        }
                        _ => {
                            lat_hh = 0;
                            lat_min = 0;
                        }
                    }
                    let lat_mag = lat_deg * 6000 + lat_min * 100 + lat_hh;
                    let lat = if q & 1 == 0 { lat_mag } else { -lat_mag };
                    // Longitude: avoid 9 degrees (encodes to DEL) and
                    // keep hundredths in 4..=98 (bytes >= 32).
                    let mut lon_deg = rng.next(180) as i64;
                    if lon_deg == 9 {
                        lon_deg = 19;
                    }
                    if lon_deg == 99 {
                        lon_deg = 98;
                    }
                    let lon_mag =
                        lon_deg * 6000 + rng.next(60) as i64 * 100 + (4 + rng.next(95)) as i64;
                    let lon = if q & 2 == 0 { lon_mag } else { -lon_mag };
                    let status = [b"/status " as &[u8], &tag(i)].concat();
                    // `lat`/`lon` are hundredths of an arc-minute,
                    // because that is the grid Mic-E carries and the
                    // grid the ambiguity blanking above operates on.
                    // The typed constructors count storage units, so
                    // scale here rather than blanking in units and
                    // losing the digit arithmetic.
                    let report = MicE {
                        latitude: Latitude::new(lat * UNITS_PER_HUNDREDTH_MINUTE).unwrap(),
                        longitude: Longitude::new(lon * UNITS_PER_HUNDREDTH_MINUTE).unwrap(),
                        // Speed <= 189 keeps the SP byte printable;
                        // course % 100 in 4..=98 keeps SE printable.
                        speed: rng.next(190) as u16,
                        course: (rng.next(3) * 100 + 4 + rng.next(95)) as u16,
                        symbol: Symbol::from_wire(b'/', SYMBOL_CODES[rng.next(5) as usize]),
                        message: ALL_MIC_E_MESSAGES[(i / 5) % 15],
                        fix: if rng.next(2) == 0 {
                            MicEFix::Current
                        } else {
                            MicEFix::Old
                        },
                        altitude: if rng.next(2) == 0 {
                            Some(rng.next(10_000) as i32 - 500)
                        } else {
                            None
                        },
                        device_prefix: None,
                        ambiguity,
                        status: &status,
                    };
                    let mut dest_bytes = [0u8; 6];
                    let mut info_buf = [0u8; 64];
                    let len = report.encode(&mut dest_bytes, &mut info_buf).unwrap();
                    let info = info_buf[..len].to_vec();
                    // Our encode -> our decode round-trips exactly.
                    let got = mic_e::decode(&dest_bytes, &info)
                        .unwrap_or_else(|e| panic!("case {i} (mic_e): decode failed: {e}"));
                    assert_eq!(got, report, "case {i} (mic_e): round trip mismatch");
                    let dest_text = std::str::from_utf8(&dest_bytes).unwrap().to_string();
                    ("mic_e", dest_text, info, true)
                }
            };
            cases.push(TxCase {
                src,
                dest,
                path,
                info,
                kind,
                mic_e: is_mic_e,
            });
        }
    }
    cases
}

fn split_ssid(addr: &str) -> (&str, u8) {
    match addr.split_once('-') {
        Some((call, ssid)) => (call, ssid.parse().unwrap()),
        None => (addr, 0),
    }
}

fn address(text: &str) -> Address {
    let (call, ssid) = split_ssid(text);
    Address::new(call.as_bytes(), ssid).unwrap()
}

/// Builds the AX.25 UI frame body (no FCS) for one case.
fn case_frame(tx: &TncTransmitter, case: &TxCase) -> Vec<u8> {
    let dest = address(&case.dest);
    let src = address(&case.src);
    let path: Vec<Address> = case.path.iter().map(|d| address(d)).collect();
    let mut frame_buf = [0u8; 512];
    let len = tx
        .build_frame_raw(dest, src, &path, &case.info, &mut frame_buf)
        .unwrap();
    frame_buf[..len].to_vec()
}

/// Modulates every case into one PCM stream with silence gaps.
fn transmit_all(tx: &TncTransmitter, cases: &[TxCase]) -> Vec<i16> {
    let gap = (SAMPLE_RATE / 10) as usize;
    let mut samples: Vec<i16> = vec![0; gap];
    for case in cases {
        let frame = case_frame(tx, case);
        samples.extend(tx.frame_samples_i16(&frame));
        samples.extend(std::iter::repeat_n(0i16, gap));
    }
    samples
}

/// Runs our full RX pipeline over PCM and returns (header, info) per
/// decoded frame, with the header rendered in monitor format.
fn receive_all(samples: &[i16]) -> Vec<(String, Vec<u8>)> {
    let config = TncConfig::bell_202(SampleRate::new(SAMPLE_RATE).unwrap()).unwrap();
    let mut rx = DefaultTncReceiver::new(config).unwrap();
    let mut out = Vec::new();
    for &s in samples {
        if let Some(frame) = rx.push_i16(s) {
            let mut header = format!("{}>{}", render_addr(frame.src()), render_addr(frame.dest()));
            for digi in frame.path() {
                header.push(',');
                header.push_str(&render_addr(*digi));
            }
            out.push((header, frame.info().to_vec()));
        }
    }
    out
}

fn render_addr(a: Address) -> String {
    let call = std::str::from_utf8(a.callsign.as_bytes())
        .unwrap()
        .to_string();
    if a.ssid.value() == 0 {
        call
    } else {
        format!("{call}-{}", a.ssid.value())
    }
}

// ---------------------------------------------------------------------
// Direction (a) on its own: the half of this file that needs nothing.
// ---------------------------------------------------------------------

/// The corpus builds, and every case round-trips through our own
/// encoder and decoder to an equal typed value.
///
/// **This test is deliberately not `#[ignore]`d.** [`generate_corpus`]
/// asserts direction (a) for all 320 cases and touches no external
/// binary, but its only callers were the five reference-gated tests
/// below, each of which returns early from [`ref_binaries_available`]
/// before reaching it. So on any machine without the reference tools --
/// which is every CI runner -- the whole 320-case fixture was compiled
/// and never executed.
///
/// That gap was not hypothetical. The fixtures drifted onto the wrong
/// coordinate unit (see CONTRIBUTING.md, "A suite CI compiles but never
/// runs rots at the fixtures"), every case collapsed to
/// 0000.00N/00000.00W, and case 0 failed this very assertion -- for as
/// long as it took someone to run the suite by hand. Direction (a) is
/// the cheapest and most fixture-sensitive of the three directions, so
/// it is the one that belongs in the default run: it costs
/// milliseconds, needs no audio, and fails loudly the moment a fixture
/// stops meaning what it says.
///
/// Directions (b) and (c) stay `#[ignore]`d, because those genuinely
/// need the reference binaries.
#[test]
fn corpus_round_trips_without_any_reference_binary() {
    let cases = generate_corpus();
    assert!(
        cases.len() >= MIN_CORPUS_CASES,
        "corpus too small: {} cases, floor is {MIN_CORPUS_CASES}",
        cases.len()
    );

    // Every kind in the rotation must actually be represented. A
    // `match` arm that stopped producing cases would otherwise shrink
    // the corpus silently, and the floor above is loose enough to
    // absorb one missing kind (320 - 20 = 300).
    let mut kinds: Vec<&'static str> = cases.iter().map(|c| c.kind).collect();
    kinds.sort_unstable();
    kinds.dedup();
    assert_eq!(
        kinds.len(),
        KINDS,
        "expected {KINDS} distinct packet kinds, got {}: {kinds:?}",
        kinds.len()
    );
}

/// The coordinate fixtures are in storage units and land exactly on the
/// 1/100 arc-minute grid.
///
/// This is the assertion that would have caught the unit drift on the
/// day it landed, and it is worth stating separately from the corpus
/// round trip because it names the defect instead of merely tripping
/// over it.
///
/// Two independent properties, and both are needed:
///
/// * **Scale.** A hundredths count handed to `Latitude::new` is a legal
///   latitude of about nine millionths of a degree, so every draw reads
///   back as 0 degrees 0.00 minutes. Requiring the draws to span whole
///   degrees fails immediately under the wrong unit and cannot be
///   satisfied by accident.
/// * **Grid.** Storage units are finer than the wire resolution, so a
///   draw that is merely *large* can still sit between two representable
///   hundredths and fail its own round trip through the uncompressed and
///   Mic-E formats for reasons that are nobody's bug. Rebuilding each
///   draw from its own degrees/hundredths reading pins it to the grid.
#[test]
fn coordinate_fixtures_are_storage_units_on_the_hundredths_grid() {
    let mut rng = Lcg::new(0x5EED_D1FF_0000_0001);
    let mut max_lat_degrees = 0u16;
    let mut max_lon_degrees = 0u16;

    for _ in 0..200 {
        let lat = Latitude::new(rand_lat(&mut rng, 0)).expect("latitude in range");
        // The two spellings must agree, which is what makes
        // `from_hundredths_minute` a safe replacement anywhere a
        // fixture currently scales by hand.
        assert_eq!(
            Latitude::from_hundredths_minute(lat.hundredths_minute()).expect("in range"),
            lat
        );
        let dm = lat.degrees_minutes();
        assert_eq!(
            Latitude::from_degrees_minutes(
                dm.degrees,
                dm.hundredths_of_minute,
                LatitudeHemisphere::North
            )
            .expect("latitude in range"),
            lat,
            "a drawn latitude is not on the 1/100 arc-minute grid"
        );
        max_lat_degrees = max_lat_degrees.max(dm.degrees);

        let lon = Longitude::new(rand_lon(&mut rng, 0)).expect("longitude in range");
        assert_eq!(
            Longitude::from_hundredths_minute(lon.hundredths_minute()).expect("in range"),
            lon
        );
        let dm = lon.degrees_minutes();
        assert_eq!(
            Longitude::from_degrees_minutes(
                dm.degrees,
                dm.hundredths_of_minute,
                LongitudeHemisphere::East
            )
            .expect("longitude in range"),
            lon,
            "a drawn longitude is not on the 1/100 arc-minute grid"
        );
        max_lon_degrees = max_lon_degrees.max(dm.degrees);
    }

    assert!(
        max_lat_degrees > 45,
        "drawn latitudes reach only {max_lat_degrees} degrees; the fixtures are \
         composed in hundredths of an arc-minute and must be scaled by \
         UNITS_PER_HUNDREDTH_MINUTE before `Latitude::new`, which counts \
         storage units"
    );
    assert!(
        max_lon_degrees > 90,
        "drawn longitudes reach only {max_lon_degrees} degrees; see the latitude \
         message above"
    );
}

// ---------------------------------------------------------------------
// The differential corpus test (directions a, b and c).
// ---------------------------------------------------------------------

#[test]
#[ignore = "requires YODEL_REF_GEN / YODEL_REF_DECODE"]
fn differential_corpus() {
    if !ref_binaries_available() {
        return;
    }
    let cases = generate_corpus(); // direction (a) asserted inside
    assert!(
        cases.len() >= MIN_CORPUS_CASES,
        "corpus too small: {} cases, floor is {MIN_CORPUS_CASES}",
        cases.len()
    );

    // Per-kind census (printed; recorded in docs/COVERAGE.md).
    let mut census: Vec<(&'static str, usize)> = Vec::new();
    for case in &cases {
        match census.iter_mut().find(|(k, _)| *k == case.kind) {
            Some((_, n)) => *n += 1,
            None => census.push((case.kind, 1)),
        }
    }
    println!("corpus: {} cases", cases.len());
    for (kind, n) in &census {
        println!("  {kind:24} {n}");
    }

    // Direction (b): our TNC transmit -> WAV -> reference decoder.
    let config = TncConfig::bell_202(SampleRate::new(SAMPLE_RATE).unwrap()).unwrap();
    let tx = TncTransmitter::new(config);
    let samples = transmit_all(&tx, &cases);
    let wav = write_wav("differential_us_to_ref.wav", &samples);
    let stdout = run_ref_decoder(&wav);
    assert_eq!(
        decoded_packet_count(&stdout),
        cases.len(),
        "reference decoder did not recover every transmitted frame"
    );
    let mut agree_b = 0usize;
    for case in &cases {
        let expected = case.monitor_line();
        assert!(
            stdout.lines().any(|l| l.contains(&expected)),
            "case ({}) not reported byte-for-byte by the reference decoder: `{expected}`",
            case.kind
        );
        agree_b += 1;
    }
    println!(
        "direction (b): {agree_b}/{} frames agreed byte-for-byte",
        cases.len()
    );

    // Direction (c): our monitor text -> reference generator -> WAV ->
    // our TNC receiver. The generator modulates the info text verbatim
    // (including Mic-E fed as a pre-built raw info field) and carries
    // the file's newline into the info field.
    let mut lines = String::new();
    for case in &cases {
        lines.push_str(&case.monitor_line());
        lines.push('\n');
    }
    let frame_file = scratch_dir().join("differential_ref_to_us.txt");
    std::fs::write(&frame_file, &lines).unwrap();
    let gen_samples = run_ref_generator(&frame_file, "differential_ref_to_us.wav");
    let received = receive_all(&gen_samples);
    assert_eq!(
        received.len(),
        cases.len(),
        "our receiver did not recover every reference-generated frame"
    );
    let mut agree_c = 0usize;
    for case in &cases {
        let header = case.header();
        let mut want = case.info.clone();
        want.push(b'\n'); // generator-appended newline
        let hit = received
            .iter()
            .find(|(h, info)| *h == header && *info == want);
        assert!(
            hit.is_some(),
            "case ({}) `{header}` not recovered byte-for-byte from reference audio",
            case.kind
        );
        // The received info must still decode with our typed parsers
        // (the trailing newline lands in the free-text tail).
        let (_, info) = hit.unwrap();
        if case.mic_e {
            let mut dest = [b' '; 6];
            for (slot, &c) in dest.iter_mut().zip(case.dest.as_bytes()) {
                *slot = c;
            }
            mic_e::decode(&dest, info).unwrap();
        } else {
            AprsPacket::parse(info).unwrap();
        }
        agree_c += 1;
    }
    println!(
        "direction (c): {agree_c}/{} frames agreed byte-for-byte",
        cases.len()
    );
}

// ---------------------------------------------------------------------
// Quantified SNR shootout.
// ---------------------------------------------------------------------

/// Peak amplitude of uniform noise for a target SNR (dB) against a
/// full-scale sine's RMS (same model as tests/noise.rs).
fn noise_peak(snr_db: f64) -> f64 {
    let signal_rms = 32_767.0 / core::f64::consts::SQRT_2;
    let noise_rms = signal_rms / 10f64.powf(snr_db / 20.0);
    noise_rms * 3f64.sqrt()
}

/// Frames of the shootout sub-corpus.
const SHOOTOUT_FRAMES: usize = 50;

/// Floor for both decoders on the shootout's noise-free rung, out of
/// [`SHOOTOUT_FRAMES`].
///
/// The ladder's own assertion is `ours >= reference`, which is a
/// comparison and not a measurement: `0 >= 0` satisfies it. A
/// transmitter that emitted silence, or a reference binary that decoded
/// nothing at all, would therefore post a clean sweep of every rung
/// while the two decoders read nothing and agreed about it.
///
/// This is not a sensitivity claim -- the clean rung's audio is our own
/// unmodified transmission, so full recovery by both sides is a
/// correctness property, and it anchors the noisy rungs above it.
/// MEASURED: 50 for both decoders at every rung, clean through 1.5 dB.
const MIN_CLEAN_RECOVERED: usize = SHOOTOUT_FRAMES;

#[test]
#[ignore = "requires YODEL_REF_GEN / YODEL_REF_DECODE"]
fn snr_shootout() {
    if !ref_binaries_available() {
        return;
    }
    let cases = generate_corpus();
    let sub = &cases[..SHOOTOUT_FRAMES];
    let config = TncConfig::bell_202(SampleRate::new(SAMPLE_RATE).unwrap()).unwrap();
    let tx = TncTransmitter::new(config);
    let clean = transmit_all(&tx, sub);
    let frames: Vec<Vec<u8>> = sub.iter().map(|c| case_frame(&tx, c)).collect();

    // Ladder: clean, then decreasing SNR down to the 1.5 dB edge —
    // below this the seeded-uniform-noise channel is beyond both
    // decoders' clean-decode region and counts fall off steeply (see
    // docs/COVERAGE.md for measured sub-threshold numbers).
    // `None` means no noise.
    let levels: [(Option<f64>, &str); 6] = [
        (None, "clean"),
        (Some(10.0), "10 dB"),
        (Some(5.0), "5 dB"),
        (Some(3.0), "3 dB"),
        (Some(2.0), "2 dB"),
        (Some(1.5), "1.5dB"),
    ];

    println!("SNR shootout ({SHOOTOUT_FRAMES} frames per level, seeded noise):");
    println!("  level   ours  reference");
    for (li, (snr, label)) in levels.iter().enumerate() {
        // Synthesize the noisy audio ONCE and hand the SAME WAV to
        // both decoders.
        let noisy: Vec<i16> = match snr {
            None => clean.clone(),
            Some(db) => {
                let peak = noise_peak(*db);
                let mut rng = Lcg::new(0xA0D5_0000 + li as u64);
                clean
                    .iter()
                    .map(|&s| {
                        let n = rng.next_f64() * peak;
                        (f64::from(s) + n).clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
                    })
                    .collect()
            }
        };
        let wav = write_wav(&format!("shootout_{li}.wav"), &noisy);

        // Ours: count sub-corpus frames recovered byte-exactly.
        let mut rx = DefaultTncReceiver::new(config).unwrap();
        let mut got = vec![false; frames.len()];
        let mut raw_frames: Vec<Vec<u8>> = Vec::new();
        for &s in &noisy {
            if let Some(frame) = rx.push_i16(s) {
                raw_frames.push(reassemble(&frame));
            }
        }
        for f in &raw_frames {
            if let Some(idx) = frames.iter().position(|w| w == f) {
                got[idx] = true;
            }
        }
        let ours = got.iter().filter(|&&g| g).count();

        // Reference: its own reported decode count on the same WAV.
        let stdout = run_ref_decoder(&wav);
        let reference = decoded_packet_count(&stdout);

        println!("  {label:7} {ours:4}  {reference:9}");
        assert!(
            ours >= reference,
            "reference decoder beat us at {label}: ours={ours} reference={reference}"
        );
        if snr.is_none() {
            // See `MIN_CLEAN_RECOVERED`: without this the whole ladder is
            // satisfied by both decoders recovering nothing.
            assert!(
                ours >= MIN_CLEAN_RECOVERED,
                "we recovered only {ours} of {SHOOTOUT_FRAMES} frames from our \
                 own noise-free audio, floor is {MIN_CLEAN_RECOVERED}"
            );
            assert!(
                reference >= MIN_CLEAN_RECOVERED,
                "the reference decoder recovered only {reference} of \
                 {SHOOTOUT_FRAMES} frames from noise-free audio, floor is \
                 {MIN_CLEAN_RECOVERED} — the rungs below it are comparisons \
                 against a decoder that is not decoding"
            );
        }
    }
}

/// Rebuilds the raw UI-frame body bytes from a received frame so it
/// can be compared byte-for-byte with what was transmitted.
fn reassemble(frame: &yodel::tnc::RxFrame<'_>) -> Vec<u8> {
    let mut buf = [0u8; 512];
    let len = frame.ui_frame().build(&mut buf).unwrap();
    buf[..len].to_vec()
}

// ---------------------------------------------------------------------
// FX.25 differential leg (1200 baud).
// ---------------------------------------------------------------------

/// Differential leg for FX.25: the reference generator supports FX.25
/// transmit via `-X 1` and the reference decoder decodes FX.25
/// codeblocks automatically (its `-d x` flag only adds debug detail;
/// there is no switch to *disable* FX.25 receive, so the
/// "reference-as-plain-AX.25" direction cannot be isolated there — the
/// additive guarantee is demonstrated with our own plain, non-FX.25
/// receiver instead). Both directions on a 100-frame sub-corpus:
///
/// * (a) reference `-X 1` FX.25 WAV -> our FX.25-aware receive path
///   (and our *plain* receiver on the same WAV, for the additive
///   guarantee);
/// * (b) our FX.25-wrapped TX -> reference decoder.
///
/// Measured at first ship: 100/100 in every direction (recorded in
/// docs/BENCHMARKS.md), so full equality is asserted.
#[cfg(feature = "fx25")]
#[test]
#[ignore = "requires YODEL_REF_GEN / YODEL_REF_DECODE"]
fn differential_fx25() {
    use yodel::demodulator::{AfskDemodulator, DemodulatorConfig};
    use yodel::fx25::{Fx25Receiver, WRAP_MAX, byte_bits, stuff_frame, wrap};
    use yodel::modulator::{Modulator, ModulatorConfig};
    use yodel::nrzi::{self, NrziDecoder};

    if !ref_binaries_available() {
        return;
    }
    let cases = generate_corpus();
    let sub = &cases[..100];
    let rate = SampleRate::new(SAMPLE_RATE).unwrap();
    let config = TncConfig::bell_202(rate).unwrap();

    // Runs audio through the FX.25-aware receive path (demod -> NRZI ->
    // tag hunter / parallel plain HDLC), returning (header, info) pairs.
    let receive_fx25 = |samples: &[i16]| -> Vec<(String, Vec<u8>)> {
        let mut demod = AfskDemodulator::new(DemodulatorConfig::bell_202(rate).unwrap()).unwrap();
        let mut nrzi = NrziDecoder::default();
        let mut rx = Fx25Receiver::<512>::new();
        let mut out = Vec::new();
        for &s in samples {
            let Some(line) = demod.push_sample_i16(s) else {
                continue;
            };
            if let Some(Ok(frame)) = rx.push(nrzi.decode(line))
                && let Ok(ui) = yodel::ax25::UiFrame::parse(frame)
            {
                let mut header = format!("{}>{}", render_addr(ui.src), render_addr(ui.dest));
                for digi in ui.path() {
                    header.push(',');
                    header.push_str(&render_addr(*digi));
                }
                out.push((header, ui.info.to_vec()));
            }
        }
        out
    };

    // Direction (a): reference generator -X 1 -> WAV -> our FX.25 path,
    // and the same WAV through our plain (non-FX.25) receiver.
    let mut lines = String::new();
    for case in sub {
        lines.push_str(&case.monitor_line());
        lines.push('\n');
    }
    let frame_file = scratch_dir().join("differential_fx25_ref_to_us.txt");
    std::fs::write(&frame_file, &lines).unwrap();
    let gen_samples =
        run_ref_generator_args(&frame_file, "differential_fx25_ref_to_us.wav", &["-X", "1"]);
    let received = receive_fx25(&gen_samples);
    let mut agree_a = 0usize;
    for case in sub {
        let header = case.header();
        let mut want = case.info.clone();
        want.push(b'\n'); // generator-appended newline
        if received
            .iter()
            .any(|(h, info)| *h == header && *info == want)
        {
            agree_a += 1;
        }
    }
    println!(
        "FX.25 direction (a): ours decoded {}/{} ({agree_a} byte-for-byte)",
        received.len(),
        sub.len()
    );
    assert_eq!(
        received.len(),
        sub.len(),
        "our FX.25 receiver did not recover every reference-generated FX.25 frame"
    );
    assert_eq!(agree_a, sub.len());

    // Additive guarantee: the same FX.25 WAV through our plain receiver
    // (the embedded frame keeps its flags, stuffing and FCS intact).
    let plain = receive_all(&gen_samples);
    let mut agree_plain = 0usize;
    for case in sub {
        let header = case.header();
        let mut want = case.info.clone();
        want.push(b'\n');
        if plain.iter().any(|(h, info)| *h == header && *info == want) {
            agree_plain += 1;
        }
    }
    println!(
        "FX.25 additive: plain receiver decoded {agree_plain}/{} from the FX.25 audio",
        sub.len()
    );
    assert_eq!(agree_plain, sub.len());

    // Direction (b): our FX.25-wrapped TX -> WAV -> reference decoder
    // (its FX.25 receive is always on; `-d x` is debug detail only).
    let tx = TncTransmitter::new(config);
    let gap = (SAMPLE_RATE / 10) as usize;
    let mut samples: Vec<i16> = vec![0; gap];
    for case in sub {
        let modulator = Modulator::new(ModulatorConfig::bell_202(rate).unwrap());
        let body = case_frame(&tx, case);
        let mut stuffed = [0u8; 1024];
        let stuffed_len = stuff_frame(&body, &mut stuffed).unwrap();
        let mut wrapped = [0u8; WRAP_MAX];
        let frame = wrap(&stuffed[..stuffed_len], &mut wrapped).unwrap();
        let mut bytes = vec![0x7Eu8; 32];
        bytes.extend_from_slice(&wrapped[..frame.len()]);
        bytes.extend_from_slice(&[0x7E, 0x7E]);
        samples.extend(modulator.i16_samples(nrzi::encode_iter(byte_bits(&bytes))));
        samples.extend(std::iter::repeat_n(0i16, gap));
    }
    let wav = write_wav("differential_fx25_us_to_ref.wav", &samples);
    let stdout = run_ref_decoder(&wav);
    let ref_count = decoded_packet_count(&stdout);
    let mut agree_b = 0usize;
    for case in sub {
        if stdout.lines().any(|l| l.contains(&case.monitor_line())) {
            agree_b += 1;
        }
    }
    println!(
        "FX.25 direction (b): reference decoded {ref_count}/{} ({agree_b} byte-for-byte)",
        sub.len()
    );
    assert_eq!(
        ref_count,
        sub.len(),
        "reference decoder did not recover every FX.25-wrapped transmitted frame"
    );
    assert_eq!(agree_b, sub.len());
}

// ---------------------------------------------------------------------
// 300-baud HF APRS differential leg.
// ---------------------------------------------------------------------

/// Like [`receive_all`] but with an explicit configuration.
fn receive_all_with(config: TncConfig, samples: &[i16]) -> Vec<(String, Vec<u8>)> {
    let mut rx = DefaultTncReceiver::new(config).unwrap();
    let mut out = Vec::new();
    for &s in samples {
        if let Some(frame) = rx.push_i16(s) {
            let mut header = format!("{}>{}", render_addr(frame.src()), render_addr(frame.dest()));
            for digi in frame.path() {
                header.push(',');
                header.push_str(&render_addr(*digi));
            }
            out.push((header, frame.info().to_vec()));
        }
    }
    out
}

/// Differential leg at 300 baud: the reference tools select 1600/1800 Hz
/// AFSK automatically for baud rates below 600, matching
/// [`ModemProfile::HF_APRS_300`]. Both directions are exercised on a
/// sub-corpus: our 300-baud TX -> reference decoder (`-B 300`), and
/// reference generator (`-B 300`) -> our 300-baud receiver.
#[test]
#[ignore = "requires YODEL_REF_GEN / YODEL_REF_DECODE"]
fn differential_300_baud() {
    if !ref_binaries_available() {
        return;
    }
    let cases = generate_corpus();
    let sub = &cases[..100];
    let rate = SampleRate::new(SAMPLE_RATE).unwrap();
    let tones = TonePair::new(
        ModemProfile::HF_APRS_300.tones().mark_hz(),
        ModemProfile::HF_APRS_300.tones().space_hz(),
        rate,
    )
    .unwrap();
    assert_eq!(tones, ModemProfile::HF_APRS_300.tones());
    let config = TncConfig::from_profile(rate, ModemProfile::HF_APRS_300).unwrap();

    // Direction (b): our 300-baud TX -> WAV -> reference decoder -B 300.
    let tx = TncTransmitter::new(config);
    let samples = transmit_all(&tx, sub);
    let wav = write_wav("differential_300_us_to_ref.wav", &samples);
    let stdout = run_ref_decoder_args(&wav, &["-B", "300"]);
    let ref_count = decoded_packet_count(&stdout);
    let mut agree_b = 0usize;
    for case in sub {
        if stdout.lines().any(|l| l.contains(&case.monitor_line())) {
            agree_b += 1;
        }
    }
    println!(
        "300 baud direction (b): reference decoded {ref_count}/{} \
         ({agree_b} byte-for-byte)",
        sub.len()
    );
    assert_eq!(
        ref_count,
        sub.len(),
        "reference decoder did not recover every 300-baud transmitted frame"
    );
    assert_eq!(agree_b, sub.len());

    // Direction (c): reference generator -B 300 -> WAV -> our receiver.
    let mut lines = String::new();
    for case in sub {
        lines.push_str(&case.monitor_line());
        lines.push('\n');
    }
    let frame_file = scratch_dir().join("differential_300_ref_to_us.txt");
    std::fs::write(&frame_file, &lines).unwrap();
    let gen_samples = run_ref_generator_args(
        &frame_file,
        "differential_300_ref_to_us.wav",
        &["-B", "300"],
    );
    let received = receive_all_with(config, &gen_samples);
    let mut agree_c = 0usize;
    for case in sub {
        let header = case.header();
        let mut want = case.info.clone();
        want.push(b'\n'); // generator-appended newline
        if received
            .iter()
            .any(|(h, info)| *h == header && *info == want)
        {
            agree_c += 1;
        }
    }
    println!(
        "300 baud direction (c): ours decoded {}/{} ({agree_c} byte-for-byte)",
        received.len(),
        sub.len()
    );
    assert_eq!(
        received.len(),
        sub.len(),
        "our receiver did not recover every reference-generated 300-baud frame"
    );
    assert_eq!(agree_c, sub.len());
}

// ---------------------------------------------------------------------
// 9600-baud G3RUH differential leg.
// ---------------------------------------------------------------------

/// Differential leg at 9600 baud: the reference tools select the G3RUH
/// scrambled-baseband scheme automatically for `-B 9600`, matching
/// [`ModemProfile::G3RUH_9600`]. Both directions are exercised on a
/// sub-corpus: our 9600-baud TX -> reference decoder (`-B 9600`), and
/// reference generator (`-B 9600`) -> our 9600-baud receiver.
/// Measured at first ship: 100/100 byte-for-byte in both directions
/// (recorded in docs/BENCHMARKS.md), so full equality is asserted,
/// matching the 300-baud leg.
#[cfg(feature = "g3ruh")]
#[test]
#[ignore = "requires YODEL_REF_GEN / YODEL_REF_DECODE"]
fn differential_9600_baud() {
    if !ref_binaries_available() {
        return;
    }
    let cases = generate_corpus();
    let sub = &cases[..100];
    let rate = SampleRate::new(SAMPLE_RATE).unwrap();
    let config = TncConfig::from_profile(rate, ModemProfile::G3RUH_9600).unwrap();

    // Direction (b): our 9600-baud TX -> WAV -> reference decoder -B 9600.
    let tx = TncTransmitter::new(config);
    let samples = transmit_all(&tx, sub);
    let wav = write_wav("differential_9600_us_to_ref.wav", &samples);
    let stdout = run_ref_decoder_args(&wav, &["-B", "9600"]);
    let ref_count = decoded_packet_count(&stdout);
    let mut agree_b = 0usize;
    for case in sub {
        if stdout.lines().any(|l| l.contains(&case.monitor_line())) {
            agree_b += 1;
        }
    }
    println!(
        "9600 baud direction (b): reference decoded {ref_count}/{} \
         ({agree_b} byte-for-byte)",
        sub.len()
    );
    assert_eq!(
        ref_count,
        sub.len(),
        "reference decoder did not recover every 9600-baud transmitted frame"
    );
    assert_eq!(agree_b, sub.len());

    // Direction (c): reference generator -B 9600 -> WAV -> our receiver.
    let mut lines = String::new();
    for case in sub {
        lines.push_str(&case.monitor_line());
        lines.push('\n');
    }
    let frame_file = scratch_dir().join("differential_9600_ref_to_us.txt");
    std::fs::write(&frame_file, &lines).unwrap();
    let gen_samples = run_ref_generator_args(
        &frame_file,
        "differential_9600_ref_to_us.wav",
        &["-B", "9600"],
    );
    let received = receive_all_with(config, &gen_samples);
    let mut agree_c = 0usize;
    for case in sub {
        let header = case.header();
        let mut want = case.info.clone();
        want.push(b'\n'); // generator-appended newline
        if received
            .iter()
            .any(|(h, info)| *h == header && *info == want)
        {
            agree_c += 1;
        }
    }
    println!(
        "9600 baud direction (c): ours decoded {}/{} ({agree_c} byte-for-byte)",
        received.len(),
        sub.len()
    );
    assert_eq!(
        received.len(),
        sub.len(),
        "our receiver did not recover every reference-generated 9600-baud frame"
    );
    assert_eq!(agree_c, sub.len());
}
