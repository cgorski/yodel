//! APRS telemetry reports (`T`).
//!
//! `T#SEQ,AAA,AAA,AAA,AAA,AAA,BBBBBBBB` per APRS 1.01 chapter 13: a
//! numeric sequence counter, up to five analog channel values and eight
//! binary digits. The `MIC` sequence form some trackers emit is **not**
//! supported: a non-numeric sequence parses to the typed error
//! [`AprsError::BadTelemetrySequence`].
//!
//! Chapter 13 gives the analog field three digits and the range
//! `000..=255`, but real senders exceed both, so a channel is stored as
//! a [`TelemetryValue`]: a decimal mantissa and a digit count, which
//! holds what the sender meant without quantising it.

use core::fmt;

use super::AprsError;
use super::position::{expect_byte, write_digits};

/// Analog channels a report can carry, per chapter 13.
const ANALOG_CHANNELS: usize = 5;
/// Digital bits in the trailing field.
const DIGITAL_BITS: usize = 8;
/// Widest sequence field accepted. Chapter 13 shows three digits, and
/// MEASURED over a 64 918-packet capture 3 312 reports use three while
/// 88 use four and 16 use five, the largest value being 46 144. Five is
/// the width at which a `u16` would overflow, so the field is `u32`.
const SEQ_DIGITS_MAX: usize = 5;
/// Narrowest sequence field written, chapter 13's own form.
const SEQ_DIGITS_MIN: usize = 3;
/// Narrowest analog field written, chapter 13's own form.
const ANALOG_DIGITS_MIN: usize = 3;

/// Widest fraction [`TelemetryValue`] accepts.
///
/// An `i64` mantissa holds at most 19 decimal digits, so 18 is the
/// widest fraction that can still be paired with a nonzero integer
/// digit. It is far beyond the wire: MEASURED over two independent
/// APRS-IS captures (64 918 and 30 301 packets, two days apart), the
/// widest analog field carries **13** decimal places in both.
const DECIMALS_MAX: u8 = 18;

/// Digits `build` writes for a sequence: chapter 13's three, widened
/// only when the value will not fit in them.
const fn seq_digits(seq: u32) -> usize {
    let mut width = SEQ_DIGITS_MIN;
    let mut limit = 1_000u32;
    while seq >= limit && width < 10 {
        width += 1;
        limit = limit.saturating_mul(10);
    }
    width
}

/// Decimal digits in `value`, at least one so that zero is `0`.
const fn digit_count(value: u64) -> usize {
    let mut count = 1;
    let mut limit = 10u64;
    while value >= limit && count < 20 {
        count += 1;
        limit = limit.saturating_mul(10);
    }
    count
}

/// Ten raised to `exponent`, saturating rather than wrapping.
///
/// Saturation is safe for the two callers: it can only happen above
/// `DECIMALS_MAX`, where the integer part of any `i64` mantissa is zero
/// and the remainder is the mantissa itself, which is what a saturated
/// divisor already yields.
const fn pow10(exponent: usize) -> u64 {
    let mut value = 1u64;
    let mut step = 0;
    while step < exponent {
        value = value.saturating_mul(10);
        step += 1;
    }
    value
}

/// One analog channel reading: `mantissa` scaled by ten to the minus
/// `decimals`, so `46.2` is `{ mantissa: 462, decimals: 1 }`.
///
/// # Why not a fixed scale, and why not a float
///
/// The crate is integer-only on its `i16` paths and runs on cores with
/// no floating-point unit, so `f32` is out. A fixed scale such as
/// milliunits is out because it quantises at parse: MEASURED over a
/// 64 918-packet capture, the widest analog field carries **13**
/// decimal places (`T#296,9.2362515628338,2000`) and the largest
/// magnitude is **32 767 646**, so `i32` milliunits would truncate the
/// first and overflow on nine fields. Quantising at parse is invisible
/// to a rebuild comparison, because `build` writes back whatever was
/// stored and the shortened value rebuilds byte-exactly.
///
/// Storing the sender's own decimal is exact instead. The largest
/// mantissa observed is 92 362 515 628 338, leaving `i64` about
/// 99 860x of headroom.
///
/// # Spelling
///
/// Leading zeros are spelling, not value: chapter 13 fixes the field
/// width, so `007` and `7` are one number. [`build`] writes chapter
/// 13's three digits and widens when the value needs more, exactly as
/// it does for the sequence counter. [`Display`] writes the minimal
/// spelling instead, which is what a JSON number requires.
///
/// [`build`]: Telemetry::build
/// [`Display`]: core::fmt::Display
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TelemetryValue {
    /// The reading's digits, without a decimal point.
    pub mantissa: i64,
    /// How many of those digits fall after the decimal point.
    pub decimals: u8,
}

impl TelemetryValue {
    /// A whole-numbered reading, chapter 13's own form.
    #[must_use]
    pub const fn integer(value: i64) -> Self {
        Self {
            mantissa: value,
            decimals: 0,
        }
    }

    /// Digits `build` writes before the decimal point, at least one.
    const fn integer_digits(self) -> usize {
        let digits = digit_count(self.mantissa.unsigned_abs());
        let decimals = self.decimals as usize;
        if digits > decimals {
            digits - decimals
        } else {
            1
        }
    }

    /// The serialized length of this value in bytes.
    #[must_use]
    pub const fn encoded_len(self) -> usize {
        let decimals = self.decimals as usize;
        if decimals > 0 {
            let sign = if self.mantissa < 0 { 1 } else { 0 };
            return sign + self.integer_digits() + 1 + decimals;
        }
        let digits = digit_count(self.mantissa.unsigned_abs());
        if self.mantissa < 0 {
            // A negative reading is outside chapter 13's unsigned
            // field, so there is no three-digit form to pad it to.
            // MEASURED: all 37 signed fields in the capture are written
            // bare, never as `-031`.
            return 1 + digits;
        }
        if digits < ANALOG_DIGITS_MIN {
            ANALOG_DIGITS_MIN
        } else {
            digits
        }
    }

    /// Writes the value into `out`, which must be exactly
    /// [`encoded_len`](Self::encoded_len) bytes.
    fn write(self, out: &mut [u8]) {
        let magnitude = self.mantissa.unsigned_abs();
        let mut at = 0;
        if self.mantissa < 0 {
            out[0] = b'-';
            at = 1;
        }
        let decimals = usize::from(self.decimals);
        if decimals == 0 {
            // `write_digits` pads from the right to fill the slice,
            // which is chapter 13's three-digit form for free.
            write_digits(&mut out[at..], magnitude);
            return;
        }
        let scale = pow10(decimals);
        let head = self.integer_digits();
        write_digits(&mut out[at..at + head], magnitude / scale);
        out[at + head] = b'.';
        write_digits(&mut out[at + head + 1..], magnitude % scale);
    }
}

impl fmt::Display for TelemetryValue {
    /// The minimal spelling, without chapter 13's zero padding, so that
    /// the output is a valid JSON number.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let magnitude = self.mantissa.unsigned_abs();
        if self.mantissa < 0 {
            f.write_str("-")?;
        }
        let decimals = usize::from(self.decimals);
        if decimals == 0 {
            return write!(f, "{magnitude}");
        }
        let scale = pow10(decimals);
        write!(f, "{}.{:0decimals$}", magnitude / scale, magnitude % scale)
    }
}

/// Channels a definition message names: five analog, eight digital.
const DEFINITION_LABELS: usize = ANALOG_CHANNELS + DIGITAL_BITS;

/// Coefficients an `EQNS.` message carries: `a`, `b` and `c` for each
/// of the five analog channels.
const EQUATION_COEFFICIENTS: usize = ANALOG_CHANNELS * 3;

/// The names or units a `PARM.`/`UNIT.` message gives the channels.
///
/// `None` is a slot the sender's list stopped before; `Some(b"")` is a
/// field that was sent and left empty. Chapter 13 says the list "may
/// stop at any field", and MEASURED over 95 219 packets both spellings
/// occur, 776 fields being present and empty.
///
/// Chapter 13 also gives each slot its own maximum width, from 7 bytes
/// down to 3. Those are not enforced. The spec's own compatibility note
/// says the widths are "a legacy arising from earlier limitations in
/// display screen width", that names "with a dozen characters or more"
/// are common, and that new applications "should handle what is in
/// common use". MEASURED, the longest label on the air is 34 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TelemetryLabels<'a> {
    /// Names or units for the five analog channels.
    pub analog: [Option<&'a [u8]>; ANALOG_CHANNELS],
    /// Names or labels for the eight digital channels.
    pub digital: [Option<&'a [u8]>; DIGITAL_BITS],
}

/// The `EQNS.` coefficients, three per analog channel.
///
/// Chapter 13 turns a raw reading `v` into a final value with
/// `a*v^2 + b*v + c`. This crate types the coefficients and stops
/// there: applying them is the caller's business, because the result
/// carries a unit that only the matching `UNIT.` message names, and
/// squaring a raw count in fixed point is a decision with no single
/// right answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TelemetryEquations {
    /// `a`, `b`, `c` for channel 1, then for channel 2, and so on.
    /// `None` is a coefficient the sender left empty or never sent.
    pub coefficients: [Option<TelemetryValue>; EQUATION_COEFFICIENTS],
}

impl TelemetryEquations {
    /// The `(a, b, c)` triple for a zero-based analog channel, when all
    /// three were given.
    #[must_use]
    pub const fn channel(
        &self,
        channel: usize,
    ) -> Option<(TelemetryValue, TelemetryValue, TelemetryValue)> {
        if channel >= ANALOG_CHANNELS {
            return None;
        }
        let at = channel * 3;
        match (
            self.coefficients[at],
            self.coefficients[at + 1],
            self.coefficients[at + 2],
        ) {
            (Some(a), Some(b), Some(c)) => Some((a, b, c)),
            _ => None,
        }
    }
}

/// The `BITS.` message: the sense of each digital channel, and the
/// project title.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TelemetryBitSense<'a> {
    /// The sense of each digital channel, or `None` when the message
    /// carried no bit pattern.
    ///
    /// MEASURED over 95 219 packets, 10 `BITS.` messages open straight
    /// into the project title with no bits at all
    /// (`BITS.Solar Power WX Station`), so this is not hypothetical.
    pub sense: Option<[bool; DIGITAL_BITS]>,
    /// The project title, verbatim and possibly empty.
    ///
    /// Chapter 13 allows 0 to 23 bytes; 30 captured messages exceed
    /// that and the longest is 52, so the width is not enforced.
    pub title: &'a [u8],
}

/// A chapter 13 telemetry definition message.
///
/// These arrive as ordinary APRS messages and this crate keeps them
/// that way: [`Message::telemetry_definition`] is a **view** over the
/// message text, not a replacement for it. The text still parses,
/// builds and rebuilds byte for byte exactly as before, so typing these
/// cannot reject a packet that used to decode. A definition this type
/// cannot represent returns `None` and the caller still has the text.
///
/// [`Message::telemetry_definition`]: super::Message::telemetry_definition
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryDefinition<'a> {
    /// `PARM.`: what each channel measures.
    Parameters(TelemetryLabels<'a>),
    /// `UNIT.`: the unit of each analog channel and the label of each
    /// digital one.
    Units(TelemetryLabels<'a>),
    /// `EQNS.`: the coefficients that scale a raw analog reading.
    Equations(TelemetryEquations),
    /// `BITS.`: the sense of each digital channel, and a project title.
    BitSense(TelemetryBitSense<'a>),
}

impl<'a> TelemetryDefinition<'a> {
    /// Parses the text of an APRS message as a definition message.
    ///
    /// Returns `None` when the text is not one of the four forms, or
    /// carries more fields than chapter 13 defines, or holds a
    /// coefficient that is not a number. That is the whole failure
    /// mode: the caller keeps the message text either way, so a `None`
    /// costs nothing beyond the typing. MEASURED over 5 805 definition
    /// messages in 95 219 packets, 5 799 type (99.90%) and 6 return
    /// `None`: 3 carry 17 coefficients where chapter 13 has 15, and 3 hold
    /// corrupt bytes.
    #[must_use]
    pub fn parse(text: &'a [u8]) -> Option<Self> {
        let body = text.get(5..)?;
        match text.get(..5)? {
            b"PARM." => labels(body).map(Self::Parameters),
            b"UNIT." => labels(body).map(Self::Units),
            b"EQNS." => equations(body).map(Self::Equations),
            b"BITS." => Some(Self::BitSense(bit_sense(body))),
            _ => None,
        }
    }
}

/// Splits a `PARM.`/`UNIT.` body into its thirteen slots.
///
/// `None` when the sender listed more fields than chapter 13 has
/// channels. MEASURED over 95 219 packets, no captured message does
/// that, and silently dropping the overflow would hide a sender this
/// crate does not understand.
fn labels(body: &[u8]) -> Option<TelemetryLabels<'_>> {
    let mut analog = [None; ANALOG_CHANNELS];
    let mut digital = [None; DIGITAL_BITS];
    for (index, field) in body.split(|&b| b == b',').enumerate() {
        if index >= DEFINITION_LABELS {
            return None;
        }
        if index < ANALOG_CHANNELS {
            analog[index] = Some(field);
        } else {
            digital[index - ANALOG_CHANNELS] = Some(field);
        }
    }
    Some(TelemetryLabels { analog, digital })
}

/// Parses an `EQNS.` body into fifteen coefficients.
///
/// `None` when a field is not a number, or when there are more than
/// chapter 13's fifteen. An empty field is a coefficient the sender
/// left blank, which is a `None` slot rather than a rejection: 192
/// captured coefficients are written that way.
///
/// Surrounding spaces are trimmed, which recovers 9 of the 15 captured
/// messages that would otherwise stay untyped (`EQNS.0,0.392,-20, 0,…`
/// and a trailing `0 `). A space around a number cannot change it, and
/// one *inside* still fails, so this widens what is accepted without
/// widening what any value means.
///
/// The trim is scoped to coefficients on purpose. In a `T#` report the
/// analog field is a fixed-width reading and stray bytes are worth
/// rejecting; here the field is a coefficient in a metadata message
/// that this crate only reads.
fn equations(body: &[u8]) -> Option<TelemetryEquations> {
    let mut coefficients = [None; EQUATION_COEFFICIENTS];
    for (index, field) in body.split(|&b| b == b',').enumerate() {
        if index >= EQUATION_COEFFICIENTS {
            return None;
        }
        let trimmed = trim_ascii_spaces(field);
        // The offset is unused here because a definition message is a
        // view and reports no positional errors; only success matters.
        coefficients[index] = parse_value(trimmed, 0).ok()?;
    }
    Some(TelemetryEquations { coefficients })
}

/// Drops ASCII spaces and tabs from both ends of a field.
fn trim_ascii_spaces(field: &[u8]) -> &[u8] {
    let mut out = field;
    while let [b' ' | b'\t', rest @ ..] = out {
        out = rest;
    }
    while let [rest @ .., b' ' | b'\t'] = out {
        out = rest;
    }
    out
}

/// Splits a `BITS.` body into the bit pattern and the project title.
///
/// The bit pattern is eight `0`/`1` characters, and the format table
/// puts the title straight after them while the spec's own example puts
/// a comma in between. MEASURED over 95 219 packets, 862 use the comma
/// and 10 use none, so both are accepted and one comma is consumed when
/// present.
///
/// A body that does not open with eight bits is all title. That is not
/// a repair for corruption: 10 captured messages are written that way,
/// naming a project and declaring no senses at all.
fn bit_sense(body: &[u8]) -> TelemetryBitSense<'_> {
    let is_bit = |b: &u8| *b == b'0' || *b == b'1';
    let has_bits = body.len() >= DIGITAL_BITS
        && body[..DIGITAL_BITS].iter().all(is_bit)
        && !matches!(body.get(DIGITAL_BITS), Some(b) if is_bit(b));
    if !has_bits {
        return TelemetryBitSense {
            sense: None,
            title: body,
        };
    }
    let mut sense = [false; DIGITAL_BITS];
    for (slot, &byte) in sense.iter_mut().zip(body.iter()) {
        *slot = byte == b'1';
    }
    let rest = body.get(DIGITAL_BITS..).unwrap_or(&[]);
    let title = match rest {
        [b',', tail @ ..] => tail,
        all => all,
    };
    TelemetryBitSense {
        sense: Some(sense),
        title,
    }
}

/// Whether a comma-separated field is the eight-bit digital byte.
///
/// It must *begin* with eight `0`/`1` characters, not consist of
/// exactly eight: chapter 13 permits a telemetry comment and does not
/// comma-delimit it, so the bits can be followed immediately by text
/// in the same field (`00000000 AI6KG`, `00000000VK3ERW aprslog`, and
/// one sender who leaves a single trailing space). MEASURED: relaxing
/// "exactly" to "begins with" recovers 24 more reports.
///
/// Requiring the ninth character not to be a digit is what stops a
/// nine-digit analog value matching. MEASURED across two independent
/// captures (64 918 packets on 2026-08-21 and 30 301 on 2026-08-23):
/// **zero** reports offer two candidates, so the scan either finds the
/// digital field or finds nothing, and the failure mode is a report
/// with no digital data rather than a wrong one. Re-run that census on
/// a fresh capture before trusting it; it is a property of observed
/// traffic, not a theorem.
fn is_digital_field(field: &[u8]) -> bool {
    field.len() >= DIGITAL_BITS
        && field[..DIGITAL_BITS]
            .iter()
            .all(|b| *b == b'0' || *b == b'1')
        && !matches!(field.get(DIGITAL_BITS), Some(b) if b.is_ascii_digit())
}

/// Byte offset of comma-separated field `index` within `body`.
///
/// `split` yields the fields but not where they started, and the
/// trailing comment has to be a slice of the original input rather than
/// a re-joined copy, because this crate does not allocate.
fn tail_offset(body: &[u8], index: usize) -> usize {
    let mut at = 0;
    for _ in 0..index {
        match body
            .get(at..)
            .and_then(|rest| rest.iter().position(|&b| b == b','))
        {
            Some(comma) => at += comma + 1,
            // Unreachable: `index` came from enumerating the same split.
            None => return body.len(),
        }
    }
    at
}

/// Parses one analog field into a channel reading, or `None` when the
/// field is empty.
///
/// An empty field is a channel the sender left blank rather than a
/// zero: MEASURED, 52 fields across 104 reports are written that way
/// (`T#188,0,0,0,-59,`), and reporting them as `0` would assert a
/// reading that was never sent.
///
/// Width is not enforced, and neither is chapter 13's `0..=255` range.
/// The fixed-width parser this replaced demanded exactly three digits
/// and capped at 255; MEASURED over 95 219 packets, that alone
/// rejected 2 574 reports whose values are perfectly ordinary.
fn parse_value(field: &[u8], at: usize) -> Result<Option<TelemetryValue>, AprsError> {
    if field.is_empty() {
        return Ok(None);
    }
    let sign_len = usize::from(matches!(field.first(), Some(b'-' | b'+')));
    let negative = field.first() == Some(&b'-');
    let mut mantissa: i64 = 0;
    let mut decimals: Option<u8> = None;
    let mut seen_digit = false;
    for (offset, &byte) in field[sign_len..].iter().enumerate() {
        // Absolute in the information field, like every other
        // positional error here: a caller pointing at the byte needs
        // the offset it can index with.
        let position = at + sign_len + offset;
        if byte == b'.' {
            if decimals.is_some() {
                return Err(AprsError::BadDigit {
                    got: byte,
                    position,
                });
            }
            decimals = Some(0);
            continue;
        }
        if !byte.is_ascii_digit() {
            return Err(AprsError::BadDigit {
                got: byte,
                position,
            });
        }
        seen_digit = true;
        mantissa = mantissa
            .checked_mul(10)
            .and_then(|scaled| scaled.checked_add(i64::from(byte - b'0')))
            .ok_or(AprsError::BadAnalogValue { position: at })?;
        if let Some(count) = decimals {
            if count + 1 > DECIMALS_MAX {
                return Err(AprsError::BadAnalogValue { position: at });
            }
            decimals = Some(count + 1);
        }
    }
    if !seen_digit {
        return Err(AprsError::BadDigit {
            got: *field.last().unwrap_or(&b','),
            position: at + sign_len,
        });
    }
    Ok(Some(TelemetryValue {
        mantissa: if negative { -mantissa } else { mantissa },
        decimals: decimals.unwrap_or(0),
    }))
}

/// A telemetry report: sequence counter, analog and digital channels.
///
/// # Wire round trip
///
/// The canonical wire form is `T#SEQ,AAA,AAA,AAA,AAA,AAA,BBBBBBBB`: a
/// three-digit sequence, five three-digit analog values, and eight
/// binary digits (most significant channel first). The parser accepts
/// the shapes real senders emit around it, and `build` writes chapter
/// 13's form for whatever the sender sent:
///
/// ```
/// use warble::aprs::{AprsError, Telemetry, TelemetryValue};
///
/// let wire = b"T#005,199,000,255,073,123,01010101";
/// let report = Telemetry::parse(wire)?;
/// assert_eq!(report.analog[0], Some(TelemetryValue::integer(199)));
/// assert_eq!(report.digital, Some([false, true, false, true, false, true, false, true]));
/// let mut buf = [0u8; 64];
/// let len = report.build(&mut buf)?;
/// assert_eq!(&buf[..len], wire);
///
/// // A decimal is stored as the sender wrote it, not quantised: this
/// // is the widest field in a 64 918-packet capture.
/// let decimal = Telemetry::parse(b"T#296,9.2362515628338,2000")?;
/// assert_eq!(
///     decimal.analog[0],
///     Some(TelemetryValue { mantissa: 92_362_515_628_338, decimals: 13 })
/// );
/// let len = decimal.build(&mut buf)?;
/// assert_eq!(&buf[..len], b"T#296,9.2362515628338,2000");
///
/// // Channels the sender did not send stay absent rather than
/// // rebuilding as zero, and a report with no digital field does not
/// // gain eight clear bits it never asserted.
/// let short = Telemetry::parse(b"T#477,114,087,040,255")?;
/// assert_eq!(short.analog[4], None);
/// assert_eq!(short.digital, None);
/// let len = short.build(&mut buf)?;
/// assert_eq!(&buf[..len], b"T#477,114,087,040,255");
/// # Ok::<(), AprsError>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Telemetry<'a> {
    /// Sequence counter.
    ///
    /// Chapter 13 shows three digits, and `build` writes at least
    /// three so that the usual form is reproduced exactly. Wider
    /// sequences are on the air: MEASURED over a 64 918-packet capture,
    /// 3 312 reports use three digits, 88 use four and 16 use five, the
    /// largest being 46 144. `u32` rather than `u16` because five
    /// digits reach 99 999, and truncating a sequence counter silently
    /// re-labels a reading.
    pub seq: u32,
    /// The five analog channels, `None` where the sender sent none.
    ///
    /// `None` is not zero. MEASURED: 104 reports leave at least one
    /// channel unsent, and rebuilding those as `000` would publish a
    /// reading the sender never made.
    pub analog: [Option<TelemetryValue>; ANALOG_CHANNELS],
    /// Eight digital channel bits, most significant first on the wire,
    /// or `None` when the report carried no digital field.
    ///
    /// `None` is not eight clear bits. MEASURED: 166 reports carry no
    /// digital field, and `T#477,114,087,040,255` rebuilding as
    /// `T#477,114,087,040,255,000,00000000` would assert eight clear
    /// bits and a fifth channel that were never sent.
    pub digital: Option<[bool; DIGITAL_BITS]>,
    /// Trailing bytes after the digital block (uninterpreted comment).
    pub rest: &'a [u8],
}

impl<'a> Telemetry<'a> {
    /// Five whole-numbered analog channels, chapter 13's own form.
    ///
    /// The common case for a transmitting station, which reads five
    /// integer sensors and has no absent channel to express:
    ///
    /// ```
    /// use warble::aprs::Telemetry;
    ///
    /// let report = Telemetry {
    ///     seq: 5,
    ///     analog: Telemetry::integer_channels([199, 0, 255, 73, 123]),
    ///     digital: Some([false; 8]),
    ///     rest: b"",
    /// };
    /// let mut buf = [0u8; 40];
    /// let len = report.build(&mut buf)?;
    /// assert_eq!(&buf[..len], b"T#005,199,000,255,073,123,00000000");
    /// # Ok::<(), warble::aprs::AprsError>(())
    /// ```
    #[must_use]
    pub const fn integer_channels(
        values: [i64; ANALOG_CHANNELS],
    ) -> [Option<TelemetryValue>; ANALOG_CHANNELS] {
        let mut out = [None; ANALOG_CHANNELS];
        let mut index = 0;
        while index < ANALOG_CHANNELS {
            out[index] = Some(TelemetryValue::integer(values[index]));
            index += 1;
        }
        out
    }

    /// Analog channels `build` writes: up to and including the last one
    /// the sender provided.
    ///
    /// Trailing absent channels are dropped rather than written as
    /// empty fields, and an absent channel before a present one is
    /// written as the empty field it was. Both spellings parse back to
    /// the same value, so F3 holds either way.
    const fn channels_written(&self) -> usize {
        let mut written = 0;
        let mut index = 0;
        while index < ANALOG_CHANNELS {
            if self.analog[index].is_some() {
                written = index + 1;
            }
            index += 1;
        }
        written
    }

    /// Parses a `T` telemetry report.
    ///
    /// # Errors
    ///
    /// [`AprsError::BadTelemetrySequence`] on a non-numeric sequence,
    /// [`AprsError::BadDigit`] on a non-digit analog byte,
    /// [`AprsError::BadAnalogValue`] on a value too large or too
    /// precise to represent, [`AprsError::ExpectedByte`] on a missing
    /// `T`/`#`, [`AprsError::BadDigitalBit`] on a digital byte other
    /// than `0`/`1`, and [`AprsError::Truncated`] on more than chapter
    /// 13's five analog channels.
    pub fn parse(info: &'a [u8]) -> Result<Self, AprsError> {
        expect_byte(info, 0, b'T')?;
        expect_byte(info, 1, b'#')?;

        // Strip the terminator BEFORE splitting. The digital anchor
        // below keys on a field of eight characters, and a trailing CR
        // defeats it. MEASURED over a 64 918-packet capture: with the
        // CR attached, 2 734 of 3 442 reports appear to have no digital
        // field; stripped, 166 do. The spec forbids the terminator
        // (chapter 14: "Do not put any carriage return or line feed at
        // the end") and igates strip it, but it reaches us on the air.
        let body = match info.get(2..).unwrap_or(&[]) {
            [rest @ .., b'\r' | b'\n'] => rest,
            all => all,
        };

        let fields = body.split(|&b| b == b',');
        let count = fields.clone().count();

        // Find the digital field FIRST. This ordering is what makes the
        // hazard impossible rather than unlikely.
        //
        // 56 captured reports carry fewer than five analog channels, so
        // their last comma-separated field is the eight-bit digital
        // byte, not another analog value:
        //
        //     T#046,400,007,00000000
        //
        // A parser that assigned analog slots left to right would read
        // `00000000` as `analog[2] = 0` and then find no digital field,
        // turning a loudly rejected packet into a silently wrong one.
        // That is worse than the rejection it replaces, and no
        // rejection count can see it.
        let digital_at = fields
            .clone()
            .enumerate()
            .filter(|(_, field)| is_digital_field(field))
            .map(|(index, _)| index)
            .last();

        let sequence = fields.clone().next().unwrap_or(&[]);
        if sequence.is_empty() || sequence.len() > SEQ_DIGITS_MAX {
            return Err(AprsError::BadTelemetrySequence {
                got: *sequence.first().unwrap_or(&0),
            });
        }
        let mut seq: u32 = 0;
        for &byte in sequence {
            if !byte.is_ascii_digit() {
                return Err(AprsError::BadTelemetrySequence { got: byte });
            }
            seq = seq * 10 + u32::from(byte - b'0');
        }

        // Analog channels run from field 1 up to the digital field, or
        // to the end when there is none. Zero to five is legal; chapter
        // 13 has five slots and more than that is a report this crate
        // cannot represent.
        let analog_end = digital_at.unwrap_or(count);
        let analog_count = analog_end.saturating_sub(1);
        if analog_count > ANALOG_CHANNELS {
            // More analog fields than chapter 13 has slots. Usually
            // that means the digital field is malformed rather than
            // absent, because a field of eight bad bits fails the
            // anchor and then counts as analog. Say so when the shape
            // fits, rather than reporting a channel count the sender
            // never intended: the byte that broke it is the useful
            // thing to point at.
            let after_analog = fields.clone().nth(1 + ANALOG_CHANNELS).unwrap_or(&[]);
            if after_analog.len() >= DIGITAL_BITS {
                let base = 2 + tail_offset(body, 1 + ANALOG_CHANNELS);
                for (offset, &byte) in after_analog.iter().take(DIGITAL_BITS).enumerate() {
                    if byte != b'0' && byte != b'1' {
                        return Err(AprsError::BadDigitalBit {
                            got: byte,
                            position: base + offset,
                        });
                    }
                }
            }
            return Err(AprsError::Truncated {
                expected: ANALOG_CHANNELS,
                got: analog_count,
            });
        }
        let mut analog = [None; ANALOG_CHANNELS];
        for (index, (slot, field)) in analog
            .iter_mut()
            .zip(fields.clone().skip(1).take(analog_count))
            .enumerate()
        {
            // `+ 2` puts the offset back in the information field's
            // frame, past the `T#` this parser stripped.
            *slot = parse_value(field, 2 + tail_offset(body, index + 1))?;
        }

        // With no digital field, every remaining field was analog and
        // the report simply did not carry one. MEASURED: 166 reports.
        let (digital, rest) = match digital_at {
            Some(index) => {
                let field = fields.clone().nth(index).unwrap_or(&[]);
                let mut bits = [false; DIGITAL_BITS];
                for (slot, &byte) in bits.iter_mut().zip(field.iter()) {
                    *slot = byte == b'1';
                }
                // Anything after the eight bits belongs to the comment,
                // whether it is glued to them or in a later field.
                let tail_at = tail_offset(body, index) + DIGITAL_BITS;
                (Some(bits), body.get(tail_at..).unwrap_or(&[]))
            }
            None => (None, &body[body.len()..]),
        };

        Ok(Self {
            seq,
            analog,
            digital,
            rest,
        })
    }

    /// The serialized length of this report in bytes.
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        let mut len = 2 + seq_digits(self.seq);
        let written = self.channels_written();
        let mut index = 0;
        while index < written {
            len += 1;
            if let Some(value) = self.analog[index] {
                len += value.encoded_len();
            }
            index += 1;
        }
        if self.digital.is_some() {
            len += 1 + DIGITAL_BITS;
        }
        len + self.rest.len()
    }

    /// Serializes the report into `buf`, returning the written length.
    ///
    /// # Errors
    ///
    /// [`AprsError::TelemetrySequenceOutOfRange`] when `seq` needs more
    /// than five digits, the widest field seen on the air,
    /// [`AprsError::TelemetryDecimalsOutOfRange`] when a channel
    /// carries a fraction wider than an `i64` mantissa can pair with an
    /// integer digit, and [`AprsError::BufferTooSmall`] when `buf`
    /// cannot hold the report.
    pub fn build(&self, buf: &mut [u8]) -> Result<usize, AprsError> {
        if seq_digits(self.seq) > SEQ_DIGITS_MAX {
            return Err(AprsError::TelemetrySequenceOutOfRange { got: self.seq });
        }
        // The fields are public, so a caller can hand `build` a value
        // the parser would refuse. Say so rather than writing a report
        // that cannot be read back.
        for value in self.analog.iter().flatten() {
            if value.decimals > DECIMALS_MAX {
                return Err(AprsError::TelemetryDecimalsOutOfRange {
                    got: value.decimals,
                });
            }
        }
        let needed = self.encoded_len();
        let max = buf.len();
        let out = buf
            .get_mut(..needed)
            .ok_or(AprsError::BufferTooSmall { needed, max })?;
        out[0] = b'T';
        out[1] = b'#';
        // At least three digits, so the usual form is reproduced
        // exactly, and wider when the value needs it: writing 1812 as
        // three digits would report sequence 812.
        let width = seq_digits(self.seq);
        write_digits(&mut out[2..2 + width], u64::from(self.seq));
        let mut at = 2 + width;
        for value in self.analog.iter().take(self.channels_written()) {
            out[at] = b',';
            at += 1;
            if let Some(value) = value {
                let len = value.encoded_len();
                value.write(&mut out[at..at + len]);
                at += len;
            }
        }
        if let Some(bits) = self.digital {
            out[at] = b',';
            at += 1;
            for bit in bits {
                out[at] = if bit { b'1' } else { b'0' };
                at += 1;
            }
        }
        for (slot, byte) in out.iter_mut().skip(at).zip(self.rest.iter()) {
            *slot = *byte;
        }
        Ok(needed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use Telemetry as T;

    /// Every analog channel present, as an integer.
    fn analog(values: [i64; ANALOG_CHANNELS]) -> [Option<TelemetryValue>; ANALOG_CHANNELS] {
        T::integer_channels(values)
    }

    #[test]
    fn parse_known_answer() {
        let t = match Telemetry::parse(b"T#005,199,000,255,073,123,01101001") {
            Ok(t) => t,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(t.seq, 5);
        assert_eq!(t.analog, analog([199, 0, 255, 73, 123]));
        assert_eq!(
            t.digital,
            Some([false, true, true, false, true, false, false, true])
        );
        assert_eq!(t.rest, b"");
    }

    #[test]
    fn build_known_answer() {
        let t = Telemetry {
            seq: 5,
            analog: analog([199, 0, 255, 73, 123]),
            digital: Some([false, true, true, false, true, false, false, true]),
            rest: b",comment",
        };
        let mut buf = [0u8; 64];
        let len = match t.build(&mut buf) {
            Ok(n) => n,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(&buf[..len], b"T#005,199,000,255,073,123,01101001,comment");
        assert_eq!(Telemetry::parse(&buf[..len]), Ok(t));
    }

    #[test]
    fn sequence_boundaries_round_trip() {
        // Chapter 13's three-digit range, plus the four- and
        // five-digit forms real trackers emit.
        for seq in [0u32, 999, 1000, 1812, 46_144, 99_999] {
            let t = Telemetry {
                seq,
                analog: analog([0; 5]),
                digital: Some([false; 8]),
                rest: b"",
            };
            let mut buf = [0u8; 64];
            let len = match t.build(&mut buf) {
                Ok(n) => n,
                Err(e) => panic!("{e}"),
            };
            assert_eq!(Telemetry::parse(&buf[..len]), Ok(t));
        }
    }

    /// A decimal survives parse unquantised, and rebuilds as written.
    ///
    /// This is the case a fixed milliunit scale would have silently
    /// truncated, and the rebuild check could not have seen it: a
    /// shortened value rebuilds byte-exactly against its own shortened
    /// self.
    #[test]
    fn decimal_values_round_trip() {
        // The widest and largest fields in the 64 918-packet capture,
        // and the widest in the 30 301-packet one two days later.
        for (wire, mantissa, decimals) in [
            (
                &b"T#296,9.2362515628338,2000"[..],
                92_362_515_628_338i64,
                13u8,
            ),
            (&b"T#850,7.5673282146454,700"[..], 75_673_282_146_454, 13),
            (&b"T#001,32767646"[..], 32_767_646, 0),
            (&b"T#023,0.00,0.94,000,019,0.0"[..], 0, 2),
            (&b"T#188,-59"[..], -59, 0),
            (&b"T#188,-0.005"[..], -5, 3),
        ] {
            let t = match Telemetry::parse(wire) {
                Ok(t) => t,
                Err(e) => panic!("{}: {e}", core::str::from_utf8(wire).unwrap_or("?")),
            };
            assert_eq!(
                t.analog[0],
                Some(TelemetryValue { mantissa, decimals }),
                "{}",
                core::str::from_utf8(wire).unwrap_or("?")
            );
            let mut buf = [0u8; 64];
            let len = match t.build(&mut buf) {
                Ok(n) => n,
                Err(e) => panic!("{e}"),
            };
            assert_eq!(
                &buf[..len],
                wire,
                "{}",
                core::str::from_utf8(wire).unwrap_or("?")
            );
        }
    }

    /// An absent channel is not a zero, and an absent digital field is
    /// not eight clear bits.
    ///
    /// This is the assertion the comma-splitting commit left behind:
    /// `T#477,114,087,040,255` rebuilt as
    /// `T#477,114,087,040,255,000,00000000`, stating a fifth reading
    /// and eight digital bits the sender never sent.
    #[test]
    fn absent_channels_are_not_zero() {
        let t = match Telemetry::parse(b"T#477,114,087,040,255") {
            Ok(t) => t,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(t.analog[3], Some(TelemetryValue::integer(255)));
        assert_eq!(t.analog[4], None);
        assert_eq!(t.digital, None);
        let mut buf = [0u8; 64];
        let len = match t.build(&mut buf) {
            Ok(n) => n,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(&buf[..len], b"T#477,114,087,040,255");
    }

    /// An empty field between two present ones keeps its slot.
    #[test]
    fn interior_absent_channel_keeps_its_slot() {
        let t = match Telemetry::parse(b"T#047,,0,0.00,,,00000000") {
            Ok(t) => t,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(t.analog[0], None);
        assert_eq!(t.analog[1], Some(TelemetryValue::integer(0)));
        assert_eq!(
            t.analog[2],
            Some(TelemetryValue {
                mantissa: 0,
                decimals: 2
            })
        );
        assert_eq!(t.analog[3], None);
        let mut buf = [0u8; 64];
        let len = match t.build(&mut buf) {
            Ok(n) => n,
            Err(e) => panic!("{e}"),
        };
        // The trailing absent channels go; the interior one stays, so
        // channel 2 is still channel 2 when this is read back.
        assert_eq!(&buf[..len], b"T#047,,000,0.00,00000000");
        assert_eq!(Telemetry::parse(&buf[..len]), Ok(t));
    }

    /// A fixed-size sink, because the crate does not allocate.
    struct Buf {
        bytes: [u8; 64],
        len: usize,
    }

    impl Buf {
        fn as_str(&self) -> &str {
            core::str::from_utf8(&self.bytes[..self.len]).expect("ascii")
        }
    }

    impl fmt::Write for Buf {
        fn write_str(&mut self, s: &str) -> fmt::Result {
            let end = self.len + s.len();
            self.bytes
                .get_mut(self.len..end)
                .ok_or(fmt::Error)?
                .copy_from_slice(s.as_bytes());
            self.len = end;
            Ok(())
        }
    }

    /// The minimal spelling, which is what a JSON number needs.
    #[test]
    fn display_is_a_valid_json_number() {
        use core::fmt::Write as _;

        for (value, want) in [
            (TelemetryValue::integer(7), "7"),
            (TelemetryValue::integer(0), "0"),
            (TelemetryValue::integer(-31), "-31"),
            (
                TelemetryValue {
                    mantissa: 462,
                    decimals: 1,
                },
                "46.2",
            ),
            (
                TelemetryValue {
                    mantissa: 94,
                    decimals: 2,
                },
                "0.94",
            ),
            (
                TelemetryValue {
                    mantissa: -5,
                    decimals: 3,
                },
                "-0.005",
            ),
            (
                TelemetryValue {
                    mantissa: 92_362_515_628_338,
                    decimals: 13,
                },
                "9.2362515628338",
            ),
        ] {
            let mut out = Buf {
                bytes: [0; 64],
                len: 0,
            };
            let _ = write!(out, "{value}");
            assert_eq!(out.as_str(), want);
        }
    }

    /// Chapter 13's own four worked examples.
    #[test]
    fn definition_spec_vectors() {
        let Some(TelemetryDefinition::Parameters(parm)) =
            TelemetryDefinition::parse(b"PARM.Battery,Btemp,ATemp,Pres,Alt,Camra,Chut,Sun,10m,ATV")
        else {
            panic!("PARM.");
        };
        assert_eq!(parm.analog[0], Some(&b"Battery"[..]));
        assert_eq!(parm.analog[4], Some(&b"Alt"[..]));
        assert_eq!(parm.digital[0], Some(&b"Camra"[..]));
        assert_eq!(parm.digital[4], Some(&b"ATV"[..]));
        // The list stopped after ten fields, so the last three digital
        // slots were never sent. Absent, not empty.
        assert_eq!(parm.digital[5], None);

        let Some(TelemetryDefinition::Units(unit)) =
            TelemetryDefinition::parse(b"UNIT.v/100,deg.F,deg.F,Mbar,Kft,Click,OPEN,on,on,hi")
        else {
            panic!("UNIT.");
        };
        // `deg.F` contains the same '.' that ends the keyword, which a
        // prefix scan looking for the last dot would trip over.
        assert_eq!(unit.analog[1], Some(&b"deg.F"[..]));
        assert_eq!(unit.digital[2], Some(&b"on"[..]));

        // The spec's example carries a leading-dot decimal and two
        // negatives, both of which the value type holds exactly.
        let Some(TelemetryDefinition::Equations(eqns)) =
            TelemetryDefinition::parse(b"EQNS.0,5.2,0,0,.53,-32,3,4.39,49,-32,3,18,1,2,3")
        else {
            panic!("EQNS.");
        };
        assert_eq!(
            eqns.channel(0),
            Some((
                TelemetryValue::integer(0),
                TelemetryValue {
                    mantissa: 52,
                    decimals: 1
                },
                TelemetryValue::integer(0)
            ))
        );
        assert_eq!(
            eqns.coefficients[4],
            Some(TelemetryValue {
                mantissa: 53,
                decimals: 2
            }),
            ".53 is 0.53, not 53"
        );
        assert_eq!(eqns.coefficients[5], Some(TelemetryValue::integer(-32)));

        let Some(TelemetryDefinition::BitSense(bits)) =
            TelemetryDefinition::parse(b"BITS.10110000,N0QBF's Big Balloon")
        else {
            panic!("BITS.");
        };
        assert_eq!(
            bits.sense,
            Some([true, false, true, true, false, false, false, false])
        );
        assert_eq!(bits.title, &b"N0QBF's Big Balloon"[..]);
    }

    /// `BITS.` with and without the comma the format table omits.
    ///
    /// The table shows the project title straight after the eighth bit
    /// while the spec's own example puts a comma there. MEASURED over
    /// 95 219 packets: 862 with, 10 without.
    #[test]
    fn bit_sense_accepts_both_separators() {
        for wire in [
            &b"BITS.11111111,WX3in1Plus20"[..],
            &b"BITS.11111111WX3in1Plus20"[..],
        ] {
            let Some(TelemetryDefinition::BitSense(bits)) = TelemetryDefinition::parse(wire) else {
                panic!("BITS.");
            };
            assert_eq!(bits.sense, Some([true; 8]));
            assert_eq!(bits.title, &b"WX3in1Plus20"[..]);
        }
        // No bit pattern at all: 10 captured messages open straight
        // into the title. The bits are absent, not eight clear ones.
        let Some(TelemetryDefinition::BitSense(bits)) =
            TelemetryDefinition::parse(b"BITS.Solar Power WX Station")
        else {
            panic!("BITS.");
        };
        assert_eq!(bits.sense, None);
        assert_eq!(bits.title, &b"Solar Power WX Station"[..]);
        // Bits and nothing else.
        let Some(TelemetryDefinition::BitSense(bits)) =
            TelemetryDefinition::parse(b"BITS.00000000")
        else {
            panic!("BITS.");
        };
        assert_eq!(bits.sense, Some([false; 8]));
        assert_eq!(bits.title, b"");
    }

    /// What stays untyped, and why each one has to.
    #[test]
    fn definition_rejections() {
        // Not a definition at all.
        assert!(TelemetryDefinition::parse(b"hello").is_none());
        assert!(TelemetryDefinition::parse(b"PARM").is_none());
        // More coefficients than chapter 13 defines. MEASURED: one
        // sender, three captured messages.
        assert!(
            TelemetryDefinition::parse(b"EQNS.0,10,0,0,10,0,0,1,0,0,0,1,0,0,0,1,0").is_none(),
            "17 coefficients where chapter 13 has 15"
        );
        // A coefficient that is not a number.
        assert!(TelemetryDefinition::parse(b"EQNS.0,1,0,0,0.1,0,0,0.1,0,0,1,0,0,1,0A").is_none());
        // More names than there are channels.
        assert!(TelemetryDefinition::parse(b"PARM.a,b,c,d,e,f,g,h,i,j,k,l,m,n").is_none());
        // Exactly thirteen is fine.
        assert!(TelemetryDefinition::parse(b"PARM.a,b,c,d,e,f,g,h,i,j,k,l,m").is_some());
    }

    /// A space around a coefficient is formatting, not value.
    ///
    /// MEASURED: 9 of the 15 captured messages that would otherwise
    /// stay untyped differ only by a space, and trimming leaves the 6
    /// that are structurally wrong still untyped.
    #[test]
    fn equation_coefficients_tolerate_surrounding_space() {
        let Some(TelemetryDefinition::Equations(e)) =
            TelemetryDefinition::parse(b"EQNS.0,0.392,-20, 0,0.235,0, 0,0.1,0, 0,1,0, 0,1,0")
        else {
            panic!("EQNS.");
        };
        assert_eq!(e.coefficients[3], Some(TelemetryValue::integer(0)));
        assert_eq!(e.coefficients[2], Some(TelemetryValue::integer(-20)));
        // A trailing space on the last field, the commonest form.
        assert!(TelemetryDefinition::parse(b"EQNS.0,1,0,0,1,0,0,1,0,0,1,0,0,1,0 ").is_some());
        // A space INSIDE a number is still not a number.
        assert!(TelemetryDefinition::parse(b"EQNS.1 2,1,0,0,1,0,0,1,0,0,1,0,0,1,0").is_none());
    }

    /// An empty coefficient is a blank slot, not a rejection.
    #[test]
    fn empty_coefficients_are_absent_not_zero() {
        let Some(TelemetryDefinition::Equations(e)) = TelemetryDefinition::parse(b"EQNS.0,,3")
        else {
            panic!("EQNS.");
        };
        assert_eq!(e.coefficients[0], Some(TelemetryValue::integer(0)));
        assert_eq!(e.coefficients[1], None);
        assert_eq!(e.coefficients[2], Some(TelemetryValue::integer(3)));
        // Not all three given, so the channel has no usable triple.
        assert_eq!(e.channel(0), None);
        assert_eq!(e.channel(1), None);
        assert_eq!(e.channel(9), None, "out of range");
    }

    #[test]
    fn parse_rejections() {
        // Non-numeric (MIC-style) sequence is a typed error.
        assert_eq!(
            Telemetry::parse(b"T#MIC,199,000,255,073,123,01101001"),
            Err(AprsError::BadTelemetrySequence { got: b'M' })
        );
        // Non-digit analog byte.
        assert_eq!(
            Telemetry::parse(b"T#005,19x,000,255,073,123,01101001"),
            Err(AprsError::BadDigit {
                got: b'x',
                position: 8
            })
        );
        // Two decimal points in one field.
        assert_eq!(
            Telemetry::parse(b"T#005,1.9.9,000,255,073,01101001"),
            Err(AprsError::BadDigit {
                got: b'.',
                position: 9
            })
        );
        // A sign with no digits behind it.
        assert_eq!(
            Telemetry::parse(b"T#005,-,000,255,073,123,01101001"),
            Err(AprsError::BadDigit {
                got: b'-',
                position: 7
            })
        );
        // A fraction wider than the mantissa can pair with an integer
        // digit. Nineteen decimals; the widest on the air is 13.
        assert_eq!(
            Telemetry::parse(b"T#005,0.0000000000000000001"),
            Err(AprsError::BadAnalogValue { position: 6 })
        );
        // A mantissa past i64.
        assert_eq!(
            Telemetry::parse(b"T#005,99999999999999999999"),
            Err(AprsError::BadAnalogValue { position: 6 })
        );
        // Digital byte other than 0/1.
        assert_eq!(
            Telemetry::parse(b"T#005,199,000,255,073,123,01101002"),
            Err(AprsError::BadDigitalBit {
                got: b'2',
                position: 33
            })
        );
        // No comma after the sequence, so `005.199` is the sequence
        // field and it is not digits. The fixed-width parser reported a
        // missing comma; a comma-splitting one cannot, because there is
        // nothing that says where the field should have ended.
        assert_eq!(
            Telemetry::parse(b"T#005.199,000,255,073,123,01101001"),
            Err(AprsError::BadTelemetrySequence { got: b'0' })
        );
        // NOT truncated: one analog channel and no digital field is a
        // shape chapter 13 permits and 166 captured reports use. The
        // fixed-width parser demanded all 34 bytes.
        let mut one = [None; ANALOG_CHANNELS];
        one[0] = Some(TelemetryValue::integer(199));
        assert_eq!(
            Telemetry::parse(b"T#005,199"),
            Ok(Telemetry {
                seq: 5,
                analog: one,
                digital: None,
                rest: b"",
            })
        );
        // Above chapter 13's 255 is now a value, not an error: 1 724
        // captured reports were rejected only for this.
        let mut wide = [None; ANALOG_CHANNELS];
        wide[0] = Some(TelemetryValue::integer(400));
        wide[1] = Some(TelemetryValue::integer(7));
        assert_eq!(
            Telemetry::parse(b"T#046,400,007,00000000"),
            Ok(Telemetry {
                seq: 46,
                analog: wide,
                digital: Some([false; 8]),
                rest: b"",
            })
        );
        // More analog fields than chapter 13 has slots.
        assert_eq!(
            Telemetry::parse(b"T#005,1,2,3,4,5,6,7"),
            Err(AprsError::Truncated {
                expected: 5,
                got: 7
            })
        );
        // Wrong identifier.
        assert_eq!(
            Telemetry::parse(b"X#005,199,000,255,073,123,01101001"),
            Err(AprsError::ExpectedByte {
                expected: b'T',
                got: b'X',
                position: 0
            })
        );
    }

    #[test]
    fn build_rejections() {
        let t = Telemetry {
            seq: 1_000_000,
            analog: analog([0; 5]),
            digital: Some([false; 8]),
            rest: b"",
        };
        let mut buf = [0u8; 64];
        // Five digits is the widest field seen on the air; six has
        // nowhere to go.
        assert_eq!(
            t.build(&mut buf),
            Err(AprsError::TelemetrySequenceOutOfRange { got: 1_000_000 })
        );
        // A fraction the parser would refuse to read back.
        let mut wide = [None; ANALOG_CHANNELS];
        wide[0] = Some(TelemetryValue {
            mantissa: 1,
            decimals: 19,
        });
        let precise = Telemetry {
            seq: 1,
            analog: wide,
            digital: None,
            rest: b"",
        };
        assert_eq!(
            precise.build(&mut buf),
            Err(AprsError::TelemetryDecimalsOutOfRange { got: 19 })
        );
        let ok = Telemetry {
            seq: 1,
            analog: analog([0; 5]),
            digital: Some([false; 8]),
            rest: b"",
        };
        let mut small = [0u8; 8];
        assert_eq!(
            ok.build(&mut small),
            Err(AprsError::BufferTooSmall {
                needed: ok.encoded_len(),
                max: 8
            })
        );
    }
}
