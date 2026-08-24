//! JSON Lines (NDJSON) rendering of decoded AX.25/APRS frames, for
//! `warble decode --output-format jsonl`.
//!
//! One self-contained JSON object per frame, one per line, so decoder
//! output pipes straight into `jq`, a log shipper or a database. The
//! schema is documented once, in `README.md` under "JSON Lines output
//! (`--output-format jsonl`)"; this module is its implementation and
//! the two must be changed together.
//!
//! # Why hand-rolled
//!
//! The library core is `#![no_std]`, allocation-free and
//! **zero-dependency**, and the binary is not allowed to drag a
//! serialization framework in through the side door. The writer below
//! is a few hundred lines of `String` appending with no dependency and
//! no reflection, which is all a fixed, hand-designed schema needs.
//!
//! # Shape, so this can later move into the library
//!
//! Split into two halves that could be promoted behind a `json` feature
//! without redesign. That promotion is a not-yet rather than an
//! oversight; the reasoning, and what it would cost, is recorded in
//! `docs/ARCHITECTURE.md`, "In scope, not yet taken".
//!
//! The two halves are:
//!
//! * [`Object`] / [`Array`] — the writer. Every value is *appended to a
//!   caller-owned `&mut String`*, never returned, so a promoted version
//!   only has to swap `String` for `core::fmt::Write` to work in
//!   `no_std` + `alloc`. Closing braces are written by `Drop`, so a
//!   nesting can be neither forgotten nor mismatched.
//! * `push_*` — one **free function per library type**, each taking the
//!   object it writes into. No trait, no derive, no inherent `impl` on
//!   a foreign type: the projection of `MicE` (say) into JSON is a
//!   function of `&MicE`, which is exactly what a `warble::json` module
//!   would export.
//!
//! # Bytes versus strings: the `_hex` sibling rule
//!
//! APRS information fields, comments, status text and object names are
//! **arbitrary bytes**; JSON strings are UTF-8. Rather than pretend
//! otherwise, every byte-slice field is written by
//! [`Object::field_bytes`], which emits:
//!
//! * `"<key>"` — always, the bytes as a UTF-8 **lossy** string (invalid
//!   sequences become U+FFFD). Readable, greppable, `jq`-able.
//! * `"<key>_hex"` — **only when the bytes are not valid UTF-8**,
//!   carrying them exactly as lowercase hex.
//!
//! So the presence of `"<key>_hex"` is the machine-readable signal that
//! `"<key>"` is lossy, and a line is byte-lossless without doubling the
//! size of the lines that do not need it. MEASURED: 17 of 2182 real
//! off-air frames carry a non-UTF-8 information field, so the other
//! 99.2% pay nothing. The most important instance is `info` /
//! `info_hex`.
//!
//! A `\u00XX` Latin-1 escaping convention was considered and rejected:
//! it *looks* lossless, but `\u00BE` is U+00BE, and any consumer that
//! re-encodes the string as UTF-8 silently gets two bytes where the air
//! carried one.
//!
//! # Determinism
//!
//! Nothing here reads a clock or a random number. A frame is identified
//! by `sample` (the sample index at which it completed) and `t` (the
//! same thing in seconds), both functions of the input alone, so the
//! whole output of a decode is byte-reproducible and can be pinned in a
//! test. `--wall-clock` adds `unix_time` and is off by default.

use std::fmt::Write as _;

use warble::SampleRate;
use warble::aprs::monitor::MonitorLine;
use warble::aprs::{
    AprsPacket, CompressedCs, DataExtension, Decoded, DecodedKind, Message, MessageContent,
    MicEFix, MicEMessage, NmeaSentence, PhgRate, Position, PositionWeather, PositionlessWeather,
    Status, Symbol, TelemetryDefinition, TelemetryLabels, ThirdParty, Timestamp, UltimeterFormat,
    UltimeterRecord, WeatherReport, decoded_from_ui,
};
use warble::ax25::UiFrame;

use crate::shared::format_address;

/// Schema version, the first key of every line.
///
/// Bumped only for a **breaking** change (a key removed, retyped, or
/// given a new meaning). Adding a key is not breaking and does not bump
/// it.
///
/// | version | change |
/// |---|---|
/// | 1 | first |
/// | 2 | `rebuild` narrowed: it used to compare bytes only, so `differs` covered both a harmless re-spelling and a changed value. It now re-parses, `differs` means the value survived, and `value_changed` and `rejected` are new |
pub const SCHEMA_VERSION: u32 = 2;

/// Decimal places used for every floating-point value.
///
/// Six is exact enough to invert the two deterministic quantities that
/// use it: latitude and longitude are stored as 1/100 arc-minutes
/// (1/6000 of a degree, ≈ 1.7e-4), and `t` is a sample index over a
/// rate of at most 48 kHz (≈ 2.1e-5 s). The third, `unix_time`, is a
/// wall clock and gets microseconds because that is all an `f64` has
/// left at 1.8e9 seconds — which is far more than a wall clock on a
/// log line is worth anyway.
///
/// Rust's float formatting is implemented in `core` and is not
/// platform-dependent, so a fixed precision keeps the output
/// byte-reproducible.
const DECIMALS: usize = 6;

/// How many levels of third-party encapsulation are decoded into nested
/// objects.
///
/// One. A `}` payload is decoded and nested under
/// `third_party.payload`; a `}` inside *that* is not descended into.
/// Real traffic does not carry double encapsulation, and the bytes are
/// never lost either way — they are on the enclosing `info`.
const MAX_THIRD_PARTY_DEPTH: u8 = 1;

// ---------------------------------------------------------------------
// The writer.
// ---------------------------------------------------------------------

/// A JSON object being appended to a caller-owned `String`.
///
/// `{` is written on construction and `}` by [`Drop`], so an object
/// cannot be left unclosed. Nested objects and arrays reborrow the same
/// buffer, so the whole line is built with a single allocation.
pub struct Object<'a> {
    out: &'a mut String,
    empty: bool,
}

impl<'a> Object<'a> {
    /// Opens an object, writing `{`.
    pub fn open(out: &'a mut String) -> Self {
        out.push('{');
        Self { out, empty: true }
    }

    /// Writes the separator and the quoted key, leaving the cursor
    /// where the value goes.
    fn key(&mut self, key: &str) {
        if !self.empty {
            self.out.push(',');
        }
        self.empty = false;
        push_quoted(self.out, key);
        self.out.push(':');
    }

    /// A string-valued field. `value` is escaped, never checked: it is
    /// already a Rust `str` and therefore valid UTF-8.
    pub fn field_str(&mut self, key: &str, value: &str) {
        self.key(key);
        push_quoted(self.out, value);
    }

    /// A **byte-slice**-valued field, under the `_hex` sibling rule
    /// documented at the module level: always `"<key>"` as a UTF-8
    /// lossy string, plus `"<key>_hex"` when — and only when — the
    /// bytes are not valid UTF-8.
    pub fn field_bytes(&mut self, key: &str, value: &[u8]) {
        match std::str::from_utf8(value) {
            Ok(text) => self.field_str(key, text),
            Err(_) => {
                self.key(key);
                push_quoted(self.out, &String::from_utf8_lossy(value));
                self.key(&format!("{key}_hex"));
                self.out.push('"');
                for byte in value {
                    let _ = write!(self.out, "{byte:02x}");
                }
                self.out.push('"');
            }
        }
    }

    /// A signed-integer field.
    pub fn field_i64(&mut self, key: &str, value: i64) {
        self.key(key);
        let _ = write!(self.out, "{value}");
    }

    /// An unsigned-integer field.
    pub fn field_u64(&mut self, key: &str, value: u64) {
        self.key(key);
        let _ = write!(self.out, "{value}");
    }

    /// A floating-point field, at the module's fixed precision.
    ///
    /// Every value reaching this comes from integer arithmetic, so
    /// neither NaN nor an infinity (which JSON cannot spell) can occur.
    pub fn field_f64(&mut self, key: &str, value: f64) {
        self.key(key);
        let _ = write!(self.out, "{value:.DECIMALS$}");
    }

    /// A boolean field.
    pub fn field_bool(&mut self, key: &str, value: bool) {
        self.key(key);
        self.out.push_str(if value { "true" } else { "false" });
    }

    /// A field the sender did not provide.
    ///
    /// `null` rather than a zero or an empty array, so that a consumer
    /// can tell an absent reading from a reading of nothing.
    pub fn field_null(&mut self, key: &str) {
        self.key(key);
        self.out.push_str("null");
    }

    /// Opens a nested object under `key`.
    pub fn object(&mut self, key: &str) -> Object<'_> {
        self.key(key);
        Object::open(self.out)
    }

    /// Opens a nested array under `key`.
    pub fn array(&mut self, key: &str) -> Array<'_> {
        self.key(key);
        Array::open(self.out)
    }
}

impl Drop for Object<'_> {
    fn drop(&mut self) {
        self.out.push('}');
    }
}

/// A JSON array being appended to a caller-owned `String`; the peer of
/// [`Object`], closed by [`Drop`] the same way.
pub struct Array<'a> {
    out: &'a mut String,
    empty: bool,
}

impl<'a> Array<'a> {
    /// Opens an array, writing `[`.
    pub fn open(out: &'a mut String) -> Self {
        out.push('[');
        Self { out, empty: true }
    }

    /// Writes the element separator.
    fn sep(&mut self) {
        if !self.empty {
            self.out.push(',');
        }
        self.empty = false;
    }

    /// Appends a boolean element.
    pub fn push_bool(&mut self, value: bool) {
        self.sep();
        self.out.push_str(if value { "true" } else { "false" });
    }

    /// Appends an element rendered by its [`Display`] impl.
    ///
    /// The caller owes a spelling that is a valid JSON number.
    /// [`TelemetryValue`]'s `Display` is the minimal one for exactly
    /// this reason: chapter 13's zero-padded `007` is not valid JSON.
    ///
    /// [`Display`]: std::fmt::Display
    /// [`TelemetryValue`]: warble::aprs::TelemetryValue
    pub fn push_number(&mut self, value: impl std::fmt::Display) {
        self.sep();
        let _ = write!(self.out, "{value}");
    }

    /// Appends an element the sender did not provide.
    pub fn push_null(&mut self) {
        self.sep();
        self.out.push_str("null");
    }

    /// Appends a byte-slice element, rendered UTF-8 lossy.
    ///
    /// Unlike [`Object::field_bytes`] there is no `_hex` sibling: an
    /// array element has no key to hang one off. Callers use this for
    /// short operator-typed labels, where a replacement character is a
    /// better answer than dropping the element.
    pub fn push_bytes(&mut self, value: &[u8]) {
        self.sep();
        match std::str::from_utf8(value) {
            Ok(text) => push_quoted(self.out, text),
            Err(_) => push_quoted(self.out, &String::from_utf8_lossy(value)),
        }
    }

    /// Appends an object element.
    pub fn push_object(&mut self) -> Object<'_> {
        self.sep();
        Object::open(self.out)
    }
}

impl Drop for Array<'_> {
    fn drop(&mut self) {
        self.out.push(']');
    }
}

/// Appends `text` as a quoted, escaped JSON string.
///
/// Escaped: `"` and `\` (mandatory), the five short forms
/// `\b \t \n \f \r`, every other C0 control as `\u00xx`, and DEL
/// (0x7f) as `\u007f`. DEL is legal raw in JSON but is not legal in a
/// terminal, and NDJSON is read by people as often as by programs.
/// Everything else — including non-ASCII — is emitted verbatim as
/// UTF-8, which is what NDJSON consumers expect.
pub fn push_quoted(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{9}' => out.push_str("\\t"),
            '\u{a}' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\u{d}' => out.push_str("\\r"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

// ---------------------------------------------------------------------
// Frame-level projection.
// ---------------------------------------------------------------------

/// Everything about a received frame that is not in the frame: where in
/// the input stream it landed, and (opt-in only) when it was heard.
#[derive(Clone, Copy)]
pub struct StreamPos {
    /// Index of the sample at which the frame completed.
    pub sample: u64,
    /// The sample rate the index counts at.
    pub rate: SampleRate,
    /// Seconds since the Unix epoch, present only under
    /// `--wall-clock`. Off by default precisely so the output stays
    /// byte-reproducible for a given input.
    pub unix_time: Option<f64>,
}

/// Renders one received frame as a complete JSONL line (no trailing
/// newline) appended to `out`.
///
/// The whole schema starts here; the key order below **is** the key
/// order on the wire.
pub fn push_frame(out: &mut String, at: StreamPos, frame: &UiFrame<'_>) {
    let mut obj = Object::open(out);
    obj.field_u64("v", u64::from(SCHEMA_VERSION));
    obj.field_u64("sample", at.sample);
    #[allow(clippy::cast_precision_loss)] // sample counts stay far under 2^53
    obj.field_f64("t", at.sample as f64 / f64::from(at.rate.hz()));
    if let Some(unix_time) = at.unix_time {
        obj.field_f64("unix_time", unix_time);
    }
    push_envelope(&mut obj, frame);
    push_decoded(&mut obj, &decoded_from_ui(frame), 0);
}

/// One line of TNC2 monitor text, in the same schema as [`push_frame`].
///
/// The envelope comes from the text fields rather than from a parsed
/// [`UiFrame`], because APRS-IS is not bound by AX.25 address rules: a
/// source may exceed six characters, use lower case, or carry an
/// alphanumeric SSID, and the path holds `q` constructs and pseudo-calls
/// that never appear on RF. Validating them would drop exactly the
/// traffic this path exists to read.
///
/// There is no sample clock behind a text capture, so `sample` and `t`
/// are omitted and `n` carries the line number instead.
pub fn push_monitor_line(
    out: &mut String,
    line_no: u64,
    line: &MonitorLine<'_>,
    verify_rebuild: bool,
) {
    let decoded = line.decoded();
    let mut obj = Object::open(out);
    obj.field_u64("v", u64::from(SCHEMA_VERSION));
    obj.field_u64("n", line_no);
    obj.field_bytes("src", line.source);
    obj.field_bytes("dst", line.dest);
    let mut path = obj.array("path");
    for hop in line.hops() {
        let mut entry = path.push_object();
        entry.field_bytes("call", hop.call);
        entry.field_bool("repeated", hop.repeated);
    }
    drop(path);
    if verify_rebuild {
        obj.field_str("rebuild", rebuild_verdict(&decoded));
    }
    push_decoded(&mut obj, &decoded, 0);
}

/// Compares what we understood against what arrived.
///
/// This is the crate's main diagnostic, and it used to stop at a byte
/// comparison. That made `differs` mean two very different things at
/// once: a packet re-spelled without loss, and a packet whose value
/// changed. The second is the only one that matters, and it was
/// invisible.
///
/// The verdicts map onto the round-trip vocabulary in
/// `docs/APRS_CONFORMANCE.md` section 4, where `p` is parse, `b` is
/// build, and `k = b . p`:
///
/// | verdict | meaning | property |
/// |---|---|---|
/// | `exact` | `k(w) = w` | F1 holds |
/// | `differs` | bytes changed, the value did not | F5 fails, F3 holds |
/// | `value_changed` | it does not parse back to the same value | **F3 fails** |
/// | `rejected` | the output is not something this crate accepts | **F2 fails**, the worst |
/// | `failed` | `b` is undefined where `p` succeeded | a gap between two partial maps |
/// | `n/a` | receive-only format, no builder by design | not measurable |
///
/// `value_changed` compares the typed values, so it is strict about
/// fields rather than about what a caller would see. Chapter 6
/// ambiguity shows up here: a latitude that declares ambiguity leaves
/// the longitude field carrying digits `build` then writes as spaces,
/// so the struct differs while [`coordinates`] masks both to the same
/// position. That is the intended reading of the field-versus-accessor
/// rule, and it is reported rather than suppressed, because a check
/// that quietly forgave some value changes would be the same mistake
/// this function exists to fix.
///
/// [`coordinates`]: warble::aprs::Position::coordinates
fn rebuild_verdict(decoded: &Decoded<'_>) -> &'static str {
    let DecodedKind::Packet(ref packet) = decoded.kind else {
        return "n/a";
    };
    let Ok(built) = packet.to_vec() else {
        return "failed";
    };
    if built == decoded.info {
        return "exact";
    }
    match AprsPacket::parse(&built) {
        Ok(ref again) if again == packet => "differs",
        Ok(_) => "value_changed",
        Err(_) => "rejected",
    }
}

/// The AX.25 envelope: source, destination and digipeater path.
///
/// The path is a **structured** array, `[{"call":…,"repeated":…}]`,
/// rather than the TNC2 monitor spelling `WIDE1-1*`. Both were on the
/// table; structured won because this is a machine-readable format and
/// a suffix that changes a callsign into a callsign-plus-a-flag is
/// exactly the "one field meaning two things" the crate's `units`
/// module exists to prevent. Reconstructing the monitor form is a
/// one-liner and is given in the README.
fn push_envelope(obj: &mut Object<'_>, frame: &UiFrame<'_>) {
    // Callsigns are validated `A-Z0-9` on parse, so these are always
    // plain ASCII and `field_str` is right rather than `field_bytes`.
    obj.field_str("src", &format_address(&frame.src));
    obj.field_str("dst", &format_address(&frame.dest));
    let mut path = obj.array("path");
    for hop in frame.hops() {
        let mut entry = path.push_object();
        entry.field_str("call", &format_address(&hop.address));
        entry.field_bool("repeated", hop.repeated);
    }
}

/// The decode outcome: the `kind` discriminant, the typed object named
/// by it, an `error` for a failed parse, and the information field.
///
/// Invariant, relied on by the README's `jq` recipes: **`line[line.kind]`
/// is always an object.** Every kind has one, including the three that
/// mean "not typed" (they carry the data type identifier that was not
/// understood).
pub fn push_decoded(obj: &mut Object<'_>, decoded: &Decoded<'_>, depth: u8) {
    match decoded.kind {
        DecodedKind::Packet(ref packet) => push_packet(obj, packet),
        DecodedKind::MicE(ref report) => {
            obj.field_str("kind", "mic_e");
            let mut m = obj.object("mic_e");
            // Through `coordinates()`, never the fields. Mic-E always
            // transmits the longitude at full precision, and chapter 10
            // makes discarding the low-order digits the receiver's job,
            // so reading `report.longitude` publishes a position more
            // precise than the sender declared. The accessor applies
            // the declared ambiguity to both axes.
            let at = report.coordinates();
            m.field_f64("lat_deg", at.latitude.to_degrees());
            m.field_f64("lon_deg", at.longitude.to_degrees());
            m.field_u64("speed_kt", u64::from(report.speed));
            m.field_u64("course_deg", u64::from(report.course));
            push_symbol(&mut m, report.symbol);
            m.field_str("message", mic_e_message(report.message));
            m.field_str(
                "fix",
                match report.fix {
                    MicEFix::Current => "current",
                    MicEFix::Old => "old",
                },
            );
            if let Some(altitude) = report.altitude {
                m.field_i64("altitude_m", i64::from(altitude));
            }
            if let Some(prefix) = report.device_prefix {
                m.field_bytes("device_prefix", &[prefix]);
            }
            m.field_u64("ambiguity_digits", u64::from(report.ambiguity));
            m.field_bytes("status", report.status);
            // Chapter 13 allows base-91 telemetry in Mic-E too, and a
            // `!DAO!` may refine the position from the status text.
            push_comment_views(&mut m, report.status);
        }
        DecodedKind::Nmea(ref sentence) => push_nmea(obj, sentence),
        DecodedKind::Ultimeter(record) => push_ultimeter(obj, record),
        DecodedKind::ThirdParty(ref tp) => push_third_party(obj, tp, depth),
        DecodedKind::NeedsDestination { dti } => {
            obj.field_str("kind", "needs_destination");
            push_dti(obj, "needs_destination", dti);
        }
        DecodedKind::Malformed { dti, error } => {
            obj.field_str("kind", "malformed");
            push_dti(obj, "malformed", dti);
            obj.field_str("error", &error.to_string());
        }
        DecodedKind::Unsupported { dti } => {
            obj.field_str("kind", "unsupported");
            push_dti(obj, "unsupported", dti);
        }
        // Not APRS at all, so there is no `dti` to report: the first
        // byte is ordinary text, and calling it an identifier is what
        // this variant exists to stop.
        DecodedKind::Text { text } => {
            obj.field_str("kind", "text");
            let mut o = obj.object("text");
            o.field_bytes("text", text);
        }
        // `DecodedKind` is `#[non_exhaustive]`: a variant added later
        // is still labelled, with its bytes intact on `info`.
        _ => {
            let dti = decoded.info.first().copied().unwrap_or(0);
            obj.field_str("kind", "unsupported");
            push_dti(obj, "unsupported", dti);
        }
    }
    obj.field_bytes("info", decoded.info);
}

/// The typed object of an untypeable outcome: the data type identifier
/// as both its byte value and its character.
fn push_dti(obj: &mut Object<'_>, key: &str, dti: u8) {
    let mut o = obj.object(key);
    o.field_u64("dti", u64::from(dti));
    o.field_bytes("dti_char", &[dti]);
}

// ---------------------------------------------------------------------
// Per-type projections.
// ---------------------------------------------------------------------

/// Projects an [`AprsPacket`], writing both `kind` and its object.
pub fn push_packet(obj: &mut Object<'_>, packet: &AprsPacket<'_>) {
    match *packet {
        AprsPacket::Position(ref p) => {
            obj.field_str("kind", "position");
            let mut o = obj.object("position");
            push_position_body(&mut o, p);
        }
        AprsPacket::PositionCs(ref p) => {
            obj.field_str("kind", "position");
            let mut o = obj.object("position");
            push_position_body(&mut o, &p.position);
            push_compressed_cs(&mut o, p.cs);
        }
        AprsPacket::PositionTimestamped(ref p) => {
            obj.field_str("kind", "position");
            let mut o = obj.object("position");
            push_timestamp(&mut o, "timestamp", p.timestamp);
            push_position_body(&mut o, &p.position);
            push_compressed_cs(&mut o, p.cs);
        }
        AprsPacket::PositionWeather(ref w) => {
            obj.field_str("kind", "weather");
            let mut o = obj.object("weather");
            push_position_weather(&mut o, w);
        }
        AprsPacket::Weather(ref w) => {
            obj.field_str("kind", "weather");
            let mut o = obj.object("weather");
            push_positionless_weather(&mut o, w);
        }
        AprsPacket::Telemetry(ref t) => {
            obj.field_str("kind", "telemetry");
            let mut o = obj.object("telemetry");
            o.field_u64("seq", u64::from(t.seq));
            {
                // `null` for a channel the sender did not send. Zero
                // would state a reading that was never made.
                let mut analog = o.array("analog");
                for value in t.analog {
                    match value {
                        Some(value) => analog.push_number(value),
                        None => analog.push_null(),
                    }
                }
            }
            match t.digital {
                Some(bits) => {
                    let mut digital = o.array("digital");
                    for bit in bits {
                        digital.push_bool(bit);
                    }
                }
                // Not eight clear bits: the report carried no digital
                // field at all.
                None => o.field_null("digital"),
            }
            o.field_bytes("rest", t.rest);
        }
        AprsPacket::Object(ref item) => {
            obj.field_str("kind", "object");
            let mut o = obj.object("object");
            o.field_bytes("name", item.name);
            o.field_bool("live", item.live);
            push_timestamp(&mut o, "timestamp", item.timestamp);
            push_masked_position(&mut o, &item.coordinates());
            push_symbol(&mut o, item.symbol);
            o.field_bytes("comment", item.comment);
        }
        AprsPacket::Item(ref item) => {
            obj.field_str("kind", "item");
            let mut o = obj.object("item");
            o.field_bytes("name", item.name);
            o.field_bool("live", item.live);
            push_masked_position(&mut o, &item.coordinates());
            push_symbol(&mut o, item.symbol);
            o.field_bytes("comment", item.comment);
        }
        AprsPacket::Status(ref s) => {
            obj.field_str("kind", "status");
            let mut o = obj.object("status");
            push_status_body(&mut o, s);
        }
        AprsPacket::Capabilities(ref c) => {
            obj.field_str("kind", "capabilities");
            let mut o = obj.object("capabilities");
            o.field_bytes("body", c.body);
        }
        AprsPacket::Message(ref m) => {
            obj.field_str("kind", "message");
            let mut o = obj.object("message");
            push_message_body(&mut o, m);
        }
        // `AprsPacket` is `#[non_exhaustive]`: a data type added to the
        // library later still produces a line, labelled `unsupported`,
        // with its bytes on `info`.
        _ => {
            obj.field_str("kind", "unsupported");
            let mut o = obj.object("unsupported");
            o.field_str("note", "packet type not projected by this schema version");
        }
    }
}

/// Writes the position of any report that can declare chapter 6
/// ambiguity, through `coordinates()` rather than the fields.
///
/// The accessor masks both axes to the declared precision. Reading
/// `latitude` / `longitude` directly publishes a position finer than
/// the sender claimed, because chapter 6 lets the longitude carry its
/// digits in full beside a blanked latitude. This renderer made that
/// exact mistake for Mic-E once already.
fn push_masked_position(obj: &mut Object<'_>, at: &warble::geo::Coordinates) {
    obj.field_f64("lat_deg", at.latitude.to_degrees());
    obj.field_f64("lon_deg", at.longitude.to_degrees());
    if !at.ambiguity.is_exact() {
        obj.field_u64("ambiguity_digits", u64::from(at.ambiguity.digits()));
    }
}

/// The shared body of every position-bearing report.
pub fn push_position_body(obj: &mut Object<'_>, p: &Position<'_>) {
    push_comment_views(obj, p.comment);
    push_masked_position(obj, &p.coordinates());
    push_symbol(obj, p.symbol);
    obj.field_bool("messaging", p.messaging);
    obj.field_bool("compressed", p.compressed);
    if let Some(feet) = p.altitude_feet() {
        obj.field_i64("altitude_ft", i64::from(feet));
    }
    if let Some(extension) = p.extension {
        push_extension(obj, extension);
    }
    obj.field_bytes("comment", p.comment);
}

/// The two symbol bytes, table first, under the `_hex` sibling rule —
/// [`Symbol::from_wire`] holds any byte pair verbatim, including pairs
/// that are not valid UTF-8.
pub fn push_symbol(obj: &mut Object<'_>, symbol: Symbol) {
    let (table, code) = symbol.to_wire();
    obj.field_bytes("symbol", &[table, code]);
}

/// The 7-byte data extension between the symbol and the comment.
pub fn push_extension(obj: &mut Object<'_>, extension: DataExtension) {
    let mut e = obj.object("extension");
    match extension {
        DataExtension::CourseSpeed { course, speed } => {
            e.field_str("type", "course_speed");
            if let Some(degrees) = course.degrees() {
                e.field_u64("course_deg", u64::from(degrees));
            }
            if let Some(knots) = speed.knots() {
                e.field_u64("speed_kt", u64::from(knots));
            }
        }
        DataExtension::Wind { direction, speed } => {
            e.field_str("type", "wind");
            if let Some(degrees) = direction.degrees() {
                e.field_u64("wind_dir_deg", u64::from(degrees));
            }
            if let Some(knots) = speed.knots() {
                e.field_u64("wind_speed_kt", u64::from(knots));
            }
        }
        DataExtension::Phg(phg) => {
            e.field_str("type", "phg");
            e.field_u64("power_w", u64::from(phg.power_watts()));
            e.field_u64("height_ft", u64::from(phg.height_feet()));
            e.field_u64("gain_dbi", u64::from(phg.gain_dbi()));
            if let Some(degrees) = phg.directivity_degrees() {
                e.field_u64("directivity_deg", u64::from(degrees));
            }
            match phg.rate() {
                Some(PhgRate::PerHour(n)) => e.field_u64("rate_per_hour", u64::from(n)),
                Some(PhgRate::Unscheduled) => e.field_bool("unscheduled", true),
                None => {}
            }
        }
        DataExtension::Range { miles } => {
            e.field_str("type", "range");
            e.field_u64("range_mi", u64::from(miles));
        }
        DataExtension::Dfs(dfs) => {
            e.field_str("type", "dfs");
            e.field_u64("strength_s_points", u64::from(dfs.strength_s_points()));
            e.field_u64("height_ft", u64::from(dfs.height_feet()));
            e.field_u64("gain_db", u64::from(dfs.gain_db()));
            if let Some(degrees) = dfs.directivity_degrees() {
                e.field_u64("directivity_deg", u64::from(degrees));
            }
        }
        // `DataExtension` is `#[non_exhaustive]`.
        _ => e.field_str("type", "other"),
    }
}

/// The `cs` trailer of a compressed position. Omitted entirely when it
/// carries no data, which is what the no-data spelling means.
pub fn push_compressed_cs(obj: &mut Object<'_>, cs: CompressedCs) {
    match cs {
        CompressedCs::NoData => {}
        CompressedCs::CourseSpeed { course, speed } => {
            let mut o = obj.object("cs");
            o.field_str("type", "course_speed");
            o.field_u64("course_deg", u64::from(course));
            o.field_u64("speed_kt", u64::from(speed));
        }
        CompressedCs::RadioRange { miles } => {
            let mut o = obj.object("cs");
            o.field_str("type", "radio_range");
            o.field_u64("range_mi", u64::from(miles));
        }
        CompressedCs::Altitude { feet } => {
            let mut o = obj.object("cs");
            o.field_str("type", "altitude");
            o.field_u64("altitude_ft", u64::from(feet));
        }
    }
}

/// An APRS timestamp, under `key`. The `form` field names which of the
/// three chapter-6 layouts arrived, because they are not
/// interchangeable: two are day-based and one is not, and one of the
/// two is station-local rather than UTC.
pub fn push_timestamp(obj: &mut Object<'_>, key: &str, timestamp: Timestamp) {
    let mut t = obj.object(key);
    match timestamp {
        Timestamp::DhmZulu { day, hour, minute } => {
            t.field_str("form", "dhm_zulu");
            t.field_u64("day", u64::from(day));
            t.field_u64("hour", u64::from(hour));
            t.field_u64("minute", u64::from(minute));
        }
        Timestamp::DhmLocal { day, hour, minute } => {
            t.field_str("form", "dhm_local");
            t.field_u64("day", u64::from(day));
            t.field_u64("hour", u64::from(hour));
            t.field_u64("minute", u64::from(minute));
        }
        Timestamp::Hms {
            hour,
            minute,
            second,
        } => {
            t.field_str("form", "hms");
            t.field_u64("hour", u64::from(hour));
            t.field_u64("minute", u64::from(minute));
            t.field_u64("second", u64::from(second));
        }
    }
}

/// A status report: the raw text plus whatever chapter 16 structure it
/// turned out to carry.
pub fn push_status_body(obj: &mut Object<'_>, s: &Status<'_>) {
    obj.field_bytes("text", s.text);
    obj.field_bytes("message", s.message());
    if let Some(timestamp) = s.timestamp() {
        push_timestamp(obj, "timestamp", timestamp);
    }
    if let Some(grid) = s.grid() {
        obj.field_str("grid", grid.grid.as_str());
    }
    if let Some(beam) = s.beam() {
        obj.field_u64("beam_heading_deg", u64::from(beam.heading.degrees()));
        obj.field_i64("beam_erp_w", i64::from(beam.erp.watts()));
    }
}

/// A text message, ack or rej.
pub fn push_message_body(obj: &mut Object<'_>, m: &Message<'_>) {
    obj.field_bytes("to", m.addressee.as_bytes());
    match m.content {
        MessageContent::Text { text, id } => {
            obj.field_str("type", "text");
            obj.field_bytes("text", text);
            if let Some(id) = id {
                obj.field_bytes("id", id);
            }
        }
        MessageContent::Ack { id } => {
            obj.field_str("type", "ack");
            obj.field_bytes("id", id);
        }
        MessageContent::Reject { id } => {
            obj.field_str("type", "rej");
            obj.field_bytes("id", id);
        }
    }
    if let Some(definition) = m.telemetry_definition() {
        let mut d = obj.object("telemetry_definition");
        push_telemetry_definition(&mut d, &definition);
    }
}

/// A chapter 13 telemetry definition carried by a message.
///
/// This is a view over the message text, so `text` above still holds
/// the bytes verbatim and this object sits beside it rather than
/// replacing it.
pub fn push_telemetry_definition(obj: &mut Object<'_>, d: &TelemetryDefinition<'_>) {
    match *d {
        TelemetryDefinition::Parameters(ref labels) => {
            obj.field_str("kind", "parm");
            push_telemetry_labels(obj, labels);
        }
        TelemetryDefinition::Units(ref labels) => {
            obj.field_str("kind", "unit");
            push_telemetry_labels(obj, labels);
        }
        TelemetryDefinition::Equations(ref eqns) => {
            obj.field_str("kind", "eqns");
            let mut a = obj.array("coefficients");
            for value in eqns.coefficients {
                match value {
                    Some(value) => a.push_number(value),
                    None => a.push_null(),
                }
            }
        }
        TelemetryDefinition::BitSense(ref bits) => {
            obj.field_str("kind", "bits");
            match bits.sense {
                Some(sense) => {
                    let mut a = obj.array("sense");
                    for bit in sense {
                        a.push_bool(bit);
                    }
                }
                None => obj.field_null("sense"),
            }
            obj.field_bytes("title", bits.title);
        }
    }
}

/// The thirteen channel names or units of a `PARM.`/`UNIT.` message.
fn push_telemetry_labels(obj: &mut Object<'_>, labels: &TelemetryLabels<'_>) {
    let mut analog = obj.array("analog");
    for label in labels.analog {
        match label {
            Some(label) => analog.push_bytes(label),
            None => analog.push_null(),
        }
    }
    drop(analog);
    let mut digital = obj.array("digital");
    for label in labels.digital {
        match label {
            Some(label) => digital.push_bytes(label),
            None => digital.push_null(),
        }
    }
}

/// The comment views: base-91 telemetry and `!DAO!`.
///
/// `/A=` altitude is already emitted by the position body. These sit
/// beside `comment`, which still holds the bytes verbatim, because they
/// are views rather than fields.
fn push_comment_views(obj: &mut Object<'_>, comment: &[u8]) {
    if let Some(t) = warble::aprs::comment_telemetry(comment) {
        let mut o = obj.object("comment_telemetry");
        o.field_u64("seq", u64::from(t.seq));
        {
            let mut a = o.array("analog");
            for value in t.analog {
                match value {
                    Some(value) => a.push_number(value),
                    None => a.push_null(),
                }
            }
        }
        match t.digital {
            Some(bits) => {
                let mut d = o.array("digital");
                for bit in bits {
                    d.push_bool(bit);
                }
            }
            None => o.field_null("digital"),
        }
    }
    if let Some(d) = warble::aprs::dao(comment) {
        let mut o = obj.object("dao");
        o.field_bytes("datum", &[d.datum]);
        o.field_bool("datum_assigned", d.datum_is_assigned());
        // The refinement is already folded into lat_deg/lon_deg above;
        // these say how much of it came from here.
        o.field_i64("added_lat_units", d.latitude_units);
        o.field_i64("added_lon_units", d.longitude_units);
    }
}

/// A Complete Weather Report: position, symbol and measurements.
pub fn push_position_weather(obj: &mut Object<'_>, w: &PositionWeather<'_>) {
    push_masked_position(obj, &w.coordinates());
    push_symbol(obj, w.symbol);
    obj.field_bool("messaging", w.messaging);
    if let Some(timestamp) = w.timestamp {
        push_timestamp(obj, "timestamp", timestamp);
    }
    push_weather_fields(obj, &w.weather);
    obj.field_bytes("rest", w.rest);
}

/// A positionless weather report: its own date/time plus measurements.
pub fn push_positionless_weather(obj: &mut Object<'_>, w: &PositionlessWeather<'_>) {
    obj.field_u64("month", u64::from(w.month));
    obj.field_u64("day", u64::from(w.day));
    obj.field_u64("hour", u64::from(w.hour));
    obj.field_u64("minute", u64::from(w.minute));
    push_weather_fields(obj, &w.weather);
    obj.field_bytes("rest", w.rest);
}

/// The measurement fields shared by every weather-bearing form,
/// including the Ultimeter records.
///
/// Each key names the unit it is in, and each unit is the one the wire
/// field uses, so the value is exact rather than converted-and-rounded:
/// wind in mph, temperature in whole °F, rain in hundredths of an inch,
/// pressure in tenths of a hectopascal. `WeatherReport` will convert to
/// anything else on request; a log line should carry what was received.
pub fn push_weather_fields(obj: &mut Object<'_>, w: &WeatherReport) {
    if let Some(v) = w.wind_direction {
        obj.field_u64("wind_dir_deg", u64::from(v));
    }
    if let Some(v) = w.wind_speed {
        obj.field_i64("wind_speed_mph", i64::from(v.mph()));
    }
    if let Some(v) = w.gust {
        obj.field_i64("gust_mph", i64::from(v.mph()));
    }
    if let Some(v) = w.temperature {
        obj.field_i64("temperature_f", i64::from(v.fahrenheit()));
    }
    if let Some(v) = w.rain_1h {
        obj.field_i64("rain_1h_hundredths_inch", i64::from(v.hundredths_inch()));
    }
    if let Some(v) = w.rain_24h {
        obj.field_i64("rain_24h_hundredths_inch", i64::from(v.hundredths_inch()));
    }
    if let Some(v) = w.rain_midnight {
        obj.field_i64(
            "rain_midnight_hundredths_inch",
            i64::from(v.hundredths_inch()),
        );
    }
    if let Some(v) = w.humidity {
        obj.field_u64("humidity_pct", u64::from(v.percent()));
    }
    if let Some(v) = w.barometric_pressure {
        obj.field_i64("pressure_tenths_hpa", i64::from(v.tenths_hpa()));
    }
    if let Some(v) = w.luminosity {
        obj.field_u64("luminosity_wm2", u64::from(v));
    }
    if let Some(v) = w.snowfall {
        obj.field_i64("snowfall_hundredths_inch", i64::from(v.hundredths_inch()));
    }
}

/// A raw NMEA 0183 sentence, writing both `kind` and its object.
pub fn push_nmea(obj: &mut Object<'_>, sentence: &NmeaSentence<'_>) {
    use warble::aprs::nmea::{ChecksumStatus, FixQuality};

    obj.field_str("kind", "nmea");
    let mut o = obj.object("nmea");
    o.field_bytes("talker", &sentence.talker.as_bytes());
    o.field_bytes("formatter", &sentence.formatter().as_bytes());
    o.field_str(
        "checksum",
        match sentence.checksum {
            ChecksumStatus::Valid => "valid",
            ChecksumStatus::Invalid { .. } => "invalid",
            ChecksumStatus::Absent => "absent",
        },
    );
    if let Some(fix) = sentence.fix() {
        o.field_str(
            "fix",
            match fix {
                FixQuality::Valid => "valid",
                FixQuality::Degraded => "degraded",
                FixQuality::Invalid => "invalid",
            },
        );
    }
    if let Some(at) = sentence.position() {
        o.field_f64("lat_deg", at.latitude.to_degrees());
        o.field_f64("lon_deg", at.longitude.to_degrees());
    }
    if let Some(course) = sentence.course() {
        o.field_u64("course_deg", u64::from(course.degrees()));
    }
    if let Some(speed) = sentence.speed() {
        o.field_i64("speed_kt", i64::from(speed.knots()));
    }
    if let Some(altitude) = sentence.altitude() {
        o.field_i64("altitude_m", i64::from(altitude.meters()));
    }
}

/// A Peet Bros Ultimeter weather record, writing both `kind` and its
/// object. The measurements use the same keys as every other weather
/// form, via [`push_weather_fields`].
pub fn push_ultimeter(obj: &mut Object<'_>, record: UltimeterRecord<'_>) {
    use warble::aprs::ultimeter::WindUnit;

    obj.field_str("kind", "ultimeter");
    let mut o = obj.object("ultimeter");
    match record.format() {
        UltimeterFormat::Packet => o.field_str("format", "packet"),
        UltimeterFormat::DataLogger => o.field_str("format", "data_logger"),
        UltimeterFormat::UltimeterTwo(unit) => {
            o.field_str("format", "ultimeter_two");
            o.field_str(
                "wire_wind_unit",
                match unit {
                    WindUnit::Mph => "mph",
                    WindUnit::Kph => "kmh",
                },
            );
        }
    }
    push_weather_fields(&mut o, &record.to_weather_report());
}

/// Gateway-encapsulated third-party traffic, writing both `kind` and
/// its object.
///
/// The encapsulated payload is decoded one level deep into a nested
/// `payload` object with the same shape as a top-level line's
/// kind/typed/info trio; see [`MAX_THIRD_PARTY_DEPTH`].
pub fn push_third_party(obj: &mut Object<'_>, tp: &ThirdParty<'_>, depth: u8) {
    obj.field_str("kind", "third_party");
    let mut o = obj.object("third_party");
    // These are *text* copied off the wire, not AX.25 addresses: an
    // internet gateway writes `qAC`, `TCPIP*` and other constructs the
    // address validator would reject. Hence `field_bytes`.
    o.field_bytes("src", tp.source);
    o.field_bytes("dst", tp.dest);
    o.field_bytes("path", tp.path);
    if depth < MAX_THIRD_PARTY_DEPTH {
        let mut payload = o.object("payload");
        push_decoded(&mut payload, &Decoded::decode(tp.payload), depth + 1);
    }
}

/// The Mic-E message type as a snake_case discriminant.
fn mic_e_message(message: MicEMessage) -> &'static str {
    use MicEMessage as M;
    match message {
        M::OffDuty => "off_duty",
        M::EnRoute => "en_route",
        M::InService => "in_service",
        M::Returning => "returning",
        M::Committed => "committed",
        M::Special => "special",
        M::Priority => "priority",
        M::Emergency => "emergency",
        M::Custom0 => "custom0",
        M::Custom1 => "custom1",
        M::Custom2 => "custom2",
        M::Custom3 => "custom3",
        M::Custom4 => "custom4",
        M::Custom5 => "custom5",
        M::Custom6 => "custom6",
    }
}
