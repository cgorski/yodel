//! APRS weather reports (`_` positionless, and weather data carried in
//! a position report whose symbol is `_`).
//!
//! Two wire forms are supported (APRS 1.01 chapter 12):
//!
//! * **Positionless** ([`PositionlessWeather`]): the `_` identifier,
//!   an 8-digit month/day/hour/minute (MDHM) timestamp, then weather
//!   fields (`c220s004g005t077...`).
//! * **Position with weather** ([`PositionWeather`]): a `!`/`=`
//!   position report whose symbol code is `_`, with the weather data in
//!   the comment: wind direction/speed as `DDD/SSS` (course/speed
//!   style) followed by letter-tagged fields.
//!
//! All values are integers ([`WeatherReport`]); a missing value is
//! `None` and is encoded as spec-style dots (`...`). Parsing accepts
//! both dots and spaces as "not available".

use super::object::Timestamp;
use super::position::{
    LATLON_LEN, LatLonBlock, byte_at, expect_byte, parse_digits, parse_latlon, write_digits,
    write_latlon,
};
use super::{AprsError, Coordinates, Latitude, Longitude, Position, Symbol};
use crate::geo::Ambiguity;
use crate::units::{Humidity, Pressure, Rainfall, Speed, Temperature};

/// Weather measurements, as physical quantities rather than wire
/// integers.
///
/// # Why these are typed and the wire fields are not
///
/// The obvious design stores each field as the integer the wire
/// carries. It is smaller and it makes byte-exactness trivial. It is
/// also **unable to represent this data correctly**, which is not a
/// theoretical objection — it was measured.
///
/// The protocol reference spells wind speed two ways. Chapter 12's
/// positionless report carries `sNNN` in **miles per hour**. Its
/// Complete Weather Report replaces `cccc` and `ssss` with "the 7-byte
/// Wind Direction and Wind Speed **Data Extension**", which chapter 7
/// defines in **knots**. One `u16` cannot mean both, and this crate
/// spent several sessions with a field documented as mph that held
/// knots for every `!`/`=` weather position — a silent 15% error that
/// no round-trip test could see, because the value was never
/// converted, only mislabelled. `tests/aprs_differential.rs` found it
/// by asking an independent decoder.
///
/// [`Speed`] fixes it structurally: parsing calls `from_mph` or
/// `from_knots` according to the wire form, building calls the
/// matching accessor, and both are **exact** (`from_mph(n).mph() == n`
/// for every representable `n`), so byte-exactness survives
/// unchanged. The typed form exists because a bare integer once let a
/// gust value reach a wind-speed field — a silent 15% error that no
/// round-trip test can see, because it survives the round trip.
///
/// # The size this costs
///
/// Every quantity is `i64`-backed and `i64` has no niche, so each
/// `Option` is 16 bytes and the struct roughly triples. That is the
/// accepted price of the plan's §12.1 rule, and `tests/type_sizes.rs`
/// makes each increase a reviewed diff — including the 16 bytes
/// [`WeatherReport::snowfall`] added and the 8 that
/// [`WeatherReport::luminosity`] costs (a `u16` needs 2 bytes and a
/// discriminant, and the struct's 8-byte alignment rounds the result
/// up).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WeatherReport {
    /// Wind direction in degrees (`c` / leading `DDD`), `0..=360`.
    ///
    /// **Not** [`crate::units::Bearing`], which is the one field of
    /// this struct the plan's §4.3 gets wrong. `Bearing`'s
    /// domain is `0..=359` and it folds 360 to 0, but the wire spells
    /// due north as `360` in the data-extension form and reserves
    /// `000` for "unknown" there. Rewriting `360` as `000` would be
    /// both a byte-exactness regression and a change of meaning.
    pub wind_direction: Option<u16>,
    /// Sustained one-minute wind speed (`sNNN`, or the `SSS` half of
    /// the `DDD/SSS` data extension).
    ///
    /// The two wire forms disagree about their unit; see the struct
    /// documentation. The wire field holds three digits, so a speed
    /// above 999 in the unit the chosen form uses is rejected on build
    /// with [`AprsError::BadWeatherValue`].
    pub wind_speed: Option<Speed>,
    /// Gust: peak wind speed in the last 5 minutes (`g`), miles per
    /// hour in both wire forms, `0..=999`.
    pub gust: Option<Speed>,
    /// Temperature (`t`). The wire field is degrees Fahrenheit,
    /// `-99..=999`.
    pub temperature: Option<Temperature>,
    /// Rainfall in the last hour (`r`). The wire field is hundredths
    /// of an inch, `0..=999`.
    pub rain_1h: Option<Rainfall>,
    /// Rainfall in the last 24 hours (`p`), hundredths of an inch.
    pub rain_24h: Option<Rainfall>,
    /// Rainfall since midnight (`P`), hundredths of an inch.
    pub rain_midnight: Option<Rainfall>,
    /// Relative humidity (`h`). [`Humidity`] absorbs the wire's quirk
    /// that `00` means 100%.
    pub humidity: Option<Humidity>,
    /// Barometric pressure (`b`). The wire field is tenths of a
    /// hectopascal, `0..=99999`.
    pub barometric_pressure: Option<Pressure>,
    /// Luminosity in watts per square metre (`L` / `l`), `0..=1999`.
    ///
    /// # Why two tags for one measurement
    ///
    /// Chapter 12 gives the field three digits and buys the fourth with
    /// a second tag: `L` = *"luminosity (in watts per square meter) 000
    /// to 999"* and `l` (lower-case letter L) = the same *"1000 and
    /// above. (Actual value is 1000 more than 3 digit number.)"* So the
    /// letter is a total function of the value — below 1000 spells `L`,
    /// at or above spells `l` with the digits reduced by 1000 — and
    /// nothing has to remember which one arrived for the rebuild to be
    /// byte-exact.
    ///
    /// # Why a bare `u16` and not a unit type
    ///
    /// `units.rs` has no irradiance quantity, adding one is out of this
    /// change's scope, and the reason that module exists does not apply
    /// here: it stops a *depth* being confused with an *altitude* or a
    /// *speed*, and there is nothing on an APRS wire that a W/m² could
    /// be mistaken for. [`WeatherReport::wind_direction`] sets the
    /// precedent for a wire integer that is already unambiguous.
    ///
    /// # Why this is not just a missing field
    ///
    /// The tag scan stops at the first byte it does not know, so an
    /// undecoded `L` did not cost one field but every field behind it.
    /// MEASURED: `…g005t077r000L050p000P000h50b09900` yielded the rain
    /// last hour and then `p`, `P`, `h` and `b` **all** in `rest`.
    pub luminosity: Option<u16>,
    /// Snowfall in the last 24 hours (`s`). The wire field is three
    /// digits of whole **inches**, `0..=999` — not the hundredths the
    /// rain fields use.
    ///
    /// # Why the same `s` tag is two measurements
    ///
    /// Chapter 12 lists `s` = *"snowfall (in inches) in the last 24
    /// hours"* among the parameters "available on some weather station
    /// units", and separately says of the Complete Weather Report that
    /// *"the 7-byte Wind Direction and Wind Speed Data Extension
    /// **replace the cccc and ssss fields**"*.
    ///
    /// So the wind-speed slot is spent **exactly once**: positionally in
    /// a Complete Weather Report, and by the **first** `s` tag in a
    /// positionless one. Once it is spent, an `s` is snowfall in *both*
    /// layouts — which is why the scan threads a `wind_slot_spent` flag
    /// rather than asking the layout alone.
    ///
    /// VERIFIED before that flag existed. In the Complete layout,
    /// `!4903.50N/07201.75W_220/004…s050` decoded as wind speed **50
    /// mph**, silently overwriting the 4 knots the positional field had
    /// already read correctly. In the positionless layout, the sibling
    /// survived the first fix: `_10090556c220s004…b09900s012wRSW` came
    /// back as **12 mph** with no snow, and rebuilt as
    /// `_10090556c220s012…`, a frame that lies about the wind. An
    /// independent decoder reads the same bytes as "wind 4.0 mph …
    /// 12.0 snow in 24 hours".
    ///
    /// # Why [`Rainfall`]
    ///
    /// Snowfall is a precipitation depth, which is exactly what
    /// [`Rainfall`] is; a separate `Snowfall` type would carry the same
    /// canonical micrometers and buy nothing, because `units.rs` exists
    /// to stop a depth being confused with an *altitude* or a *speed*,
    /// not to partition one dimension by which sensor filled it.
    ///
    /// # How both layouts spell it
    ///
    /// As a second `sNNN` after the nine standard fields — exactly
    /// where the tag scan finds it again, and exactly where an
    /// independent decoder reads it. Both [`PositionlessWeather::build`]
    /// and [`PositionWeather::build`] emit it, and only when it is
    /// present: growing every snow-free report by a dotted `s...` would
    /// break the byte-exact rebuild an igate depends on.
    ///
    /// ```
    /// use warble::aprs::{AprsError, PositionlessWeather, WeatherReport};
    /// use warble::units::{Rainfall, Speed};
    ///
    /// let report = PositionlessWeather::new(
    ///     10,
    ///     9,
    ///     5,
    ///     56,
    ///     WeatherReport {
    ///         wind_speed: Some(Speed::from_mph(4)),
    ///         snowfall: Some(Rainfall::from_hundredths_inch(1_200)),
    ///         ..WeatherReport::default()
    ///     },
    /// )?;
    /// let mut buf = [0u8; 64];
    /// let len = report.build(&mut buf)?;
    /// // The wind keeps the standard block's `s`; the snow gets its own.
    /// // An absent field is omitted rather than dotted, so nothing
    /// // stands between the two.
    /// assert_eq!(&buf[..len], b"_10090556s004s012");
    /// assert_eq!(PositionlessWeather::parse(&buf[..len])?, report);
    /// # Ok::<(), AprsError>(())
    /// ```
    pub snowfall: Option<Rainfall>,
}

/// A positionless weather report (`_` + MDHM timestamp + wx fields).
///
/// # Common path: one line, valid by construction
///
/// ```
/// use warble::aprs::{AprsError, PositionlessWeather, WeatherReport};
/// use warble::units::Temperature;
///
/// // A sensor that reads Celsius feeds a wire field in Fahrenheit,
/// // and nobody has to know: 25 C is 77 F.
/// let report = PositionlessWeather::new(
///     9,
///     23,
///     12,
///     34,
///     WeatherReport {
///         temperature: Some(Temperature::from_celsius(25)),
///         ..WeatherReport::default()
///     },
/// )?;
/// let mut buf = [0u8; 64];
/// let len = report.build(&mut buf)?;
/// assert!(buf[..len].starts_with(b"_09231234"));
/// # Ok::<(), AprsError>(())
/// ```
///
/// # Power user / raw hatch
///
/// The struct literal is the escape hatch: out-of-range timestamp
/// fields can be held (e.g. copied from weird traffic) and are only
/// rejected by [`PositionlessWeather::build`].
///
/// ```
/// use warble::aprs::{AprsError, PositionlessWeather, WeatherReport};
///
/// let odd = PositionlessWeather {
///     month: 13, // out of spec; held, rejected on build
///     day: 1,
///     hour: 0,
///     minute: 0,
///     weather: WeatherReport::default(),
///     rest: b"",
/// };
/// assert_eq!(
///     odd.build(&mut [0u8; 64]),
///     Err(AprsError::BadTimestamp { field: b'M', got: 13 })
/// );
/// # Ok::<(), AprsError>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PositionlessWeather<'a> {
    /// Month of the observation, `1..=12`.
    pub month: u8,
    /// Day of the month, `1..=31`.
    pub day: u8,
    /// Hour (24-hour clock), `0..=23`.
    pub hour: u8,
    /// Minute, `0..=59`.
    pub minute: u8,
    /// The weather measurements.
    pub weather: WeatherReport,
    /// Trailing bytes after the weather fields (APRS software and
    /// weather-unit indicators, uninterpreted).
    pub rest: &'a [u8],
}

/// A position report with weather data (`!`/`=`, symbol code `_`).
///
/// # Common path: one line, valid by construction
///
/// ```
/// use warble::aprs::{AprsError, Latitude, Longitude, PositionWeather, WeatherReport};
/// use warble::units::Speed;
///
/// let report = PositionWeather::new(
///     Latitude::from_degrees(49.0583)?,
///     Longitude::from_degrees(-72.0292)?,
///     WeatherReport {
///         wind_direction: Some(220),
///         // This layout's wind field is the `DDD/SSS` data
///         // extension, which chapter 7 defines in knots; the `sNNN`
///         // of a positionless report is miles per hour. Say which,
///         // and the builder writes the right digits.
///         wind_speed: Some(Speed::from_knots(4)),
///         ..WeatherReport::default()
///     },
/// );
/// let mut buf = [0u8; 96];
/// let len = report.build(&mut buf)?;
/// assert!(buf[..len].starts_with(b"!4903.50N/07201.75W_220/004"));
/// # Ok::<(), AprsError>(())
/// ```
///
/// # Power user: choose the alternate table, keep the `_` code
///
/// ```
/// use warble::aprs::{AprsError, Latitude, Longitude, PositionWeather, Symbol, WeatherReport};
///
/// let report = PositionWeather::new(
///     Latitude::new(0)?,
///     Longitude::new(0)?,
///     WeatherReport::default(),
/// )
/// .with_table(Symbol::alternate(warble::aprs::SymbolCode::new(b'_')?))
/// .with_messaging(true);
/// assert_eq!(report.symbol.to_wire(), (b'\\', b'_'));
/// # Ok::<(), AprsError>(())
/// ```
///
/// # Raw hatch: out-of-spec table bytes round-trip exactly
///
/// ```
/// use warble::aprs::{AprsError, Latitude, Longitude, PositionWeather, Symbol, WeatherReport};
///
/// let report = PositionWeather::new(Latitude::new(0)?, Longitude::new(0)?, WeatherReport::default())
///     .with_table(Symbol::from_wire(b'~', b'_')); // '~' held verbatim
/// let mut buf = [0u8; 96];
/// let len = report.build(&mut buf)?;
/// assert!(buf[..len].starts_with(b"!0000.00N~00000.00E_"));
/// assert_eq!(report.symbol.to_wire(), (b'~', b'_'));
/// # Ok::<(), AprsError>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PositionWeather<'a> {
    /// The station latitude.
    pub latitude: Latitude,
    /// The station longitude.
    pub longitude: Longitude,
    /// How many low-order coordinate digits the sender blanked.
    ///
    /// Chapter 6 position ambiguity; see [`Self::coordinates`], which
    /// applies it to both axes.
    pub ambiguity: Ambiguity,
    /// The display symbol. The spec fixes the code to `_` (weather
    /// station); building always emits `_` as the code and only the
    /// table byte of this symbol reaches the wire.
    pub symbol: Symbol,
    /// `true` builds/parsed the messaging identifier (`=`, or `@`
    /// when a timestamp is present), `false` the plain one (`!`/`/`).
    pub messaging: bool,
    /// The 7-byte timestamp, when this is one of chapter 12's
    /// timestamped Complete Weather Report layouts (`/` or `@`).
    ///
    /// MEASURED: 92 of the corpus frames use it — 54 directly and 38
    /// inside third-party wrappers — and every one of them was
    /// previously read as an ordinary timestamped position whose
    /// weather block stayed uninterpreted comment text.
    pub timestamp: Option<Timestamp>,
    /// The weather measurements.
    pub weather: WeatherReport,
    /// Trailing bytes after the weather fields (uninterpreted).
    pub rest: &'a [u8],
}

/// Every weather field tag this module knows: `(tag, digit width)`.
///
/// The first [`STANDARD_FIELDS`] entries are chapter 12's fixed-width
/// block in build order, and every report writes all of them, dotted
/// when absent. The entries after that are the chapter's "other
/// parameters that are available on some weather station units", and
/// they are written **only when present**: lengthening every report the
/// crate has ever emitted with dotted extras would break the byte-exact
/// rebuild an igate depends on for traffic that never mentioned them.
///
/// The `s` of that second group — snowfall — needs no entry of its own,
/// because it is the same tag and the same three digits as the wind
/// speed above; which measurement a given `s` carries is decided by
/// whether the wind slot has been spent yet. See
/// [`WeatherReport::snowfall`].
///
/// # The one "other parameter" that is absent
///
/// Chapter 12's raw rain counter. Its definition there, in full, is
/// "`#` = raw rain counter" — no width, no unit, no scaling, and no
/// worked example — so there is nothing to parse it *into*, and the
/// independent decoder does not decode it either.
/// Guessing a width would be the one failure mode this crate ranks
/// worst: a wrong value where there is currently a byte-exact one, since
/// an unknown `#` already reaches the caller intact in `rest`.
const TAGGED_FIELDS: [(u8, usize); 11] = [
    (b'c', 3),
    (b's', 3),
    (b'g', 3),
    (b't', 3),
    (b'r', 3),
    (b'p', 3),
    (b'P', 3),
    (b'h', 2),
    (b'b', 5),
    // Chapter 12's "other parameters" start here; see above.
    (b'L', 3),
    (b'l', 3),
];

/// How many leading entries of [`TAGGED_FIELDS`] form the standard
/// block that every report writes.
const STANDARD_FIELDS: usize = 9;

/// Parses a `width`-digit weather value at `position`; all-dots or
/// all-spaces is `None` ("not available").
fn parse_value(info: &[u8], position: usize, width: usize) -> Result<Option<u32>, AprsError> {
    let first = byte_at(info, position)?;
    if first == b'.' || first == b' ' {
        for offset in 1..width {
            expect_byte(info, position + offset, first)?;
        }
        return Ok(None);
    }
    #[allow(clippy::cast_sign_loss)]
    let value = parse_digits(info, position, width)? as u32;
    Ok(Some(value))
}

/// Parses a temperature value (`-` allowed as the first of three
/// bytes for `-1..=-99` degrees).
fn parse_temperature(info: &[u8], position: usize) -> Result<Option<i16>, AprsError> {
    let first = byte_at(info, position)?;
    if first == b'-' {
        let value = parse_digits(info, position + 1, 2)?;
        #[allow(clippy::cast_possible_truncation)]
        return Ok(Some(-(value as i16)));
    }
    #[allow(clippy::cast_possible_truncation)]
    Ok(parse_value(info, position, 3)?.map(|v| v as i16))
}

/// Writes `value` zero-padded into `out`, or fills it with dots.
fn write_value(out: &mut [u8], value: Option<u32>) {
    match value {
        Some(v) => write_digits(out, u64::from(v)),
        None => {
            for slot in out.iter_mut() {
                *slot = b'.';
            }
        }
    }
}

/// Writes a temperature: `-NN` for negative values, else 3 digits or
/// dots. The range was checked by [`WeatherReport::check`].
fn write_temperature(out: &mut [u8], value: Option<Temperature>) {
    match value.map(Temperature::fahrenheit) {
        Some(v) if v < 0 => {
            out[0] = b'-';
            write_digits(&mut out[1..3], u64::from(v.unsigned_abs()));
        }
        #[allow(clippy::cast_sign_loss)]
        Some(v) => write_digits(out, v as u64),
        None => write_value(out, None),
    }
}

/// The number of whole inches a snow depth spells in the `s` field,
/// rounded half away from zero.
///
/// Chapter 12's `s` is three digits of *inches*, unlike the `r`/`p`/`P`
/// fields' hundredths, and [`Rainfall`] — written for those — exposes no
/// `inches` accessor, so the last division happens here. Rounding rather
/// than rejecting is what every other coarse field in this module
/// already does (a millimetre of rain rounds into `r`'s hundredths, a
/// km/h wind into `s`'s mph), and anything [`WeatherReport::read_tagged`]
/// produces is a whole number of inches, so the round trip stays exact.
///
/// The saturating bias mirrors `units::div_round`: a `Rainfall` may hold
/// a depth whose hundredths saturate `i32`, and this must not panic.
const fn snowfall_inches(depth: Rainfall) -> i32 {
    let hundredths = depth.hundredths_inch();
    let half = if hundredths < 0 { -50 } else { 50 };
    hundredths.saturating_add(half) / 100
}

/// The tag and the three digits a luminosity spells.
///
/// Chapter 12 buys a fourth digit with a second tag rather than a wider
/// field: `L` carries 000–999 W/m² and `l` (lower-case letter L) carries
/// "1000 and above", where "actual value is 1000 more than 3 digit
/// number". The letter is therefore a *total function* of the value, so
/// [`WeatherReport::luminosity`] need not remember which one arrived for
/// the rebuild to be byte-exact. The range was checked by
/// [`WeatherReport::check`], so the subtraction cannot wrap.
#[allow(clippy::cast_lossless)]
const fn luminosity_wire(watts_per_square_meter: u16) -> (u8, u32) {
    if watts_per_square_meter < 1000 {
        (b'L', watts_per_square_meter as u32)
    } else {
        (b'l', (watts_per_square_meter - 1000) as u32)
    }
}

/// Range-checks a value on build; `field` tags the error.
fn check_range(field: u8, got: i32, min: i32, max: i32) -> Result<(), AprsError> {
    if got < min || got > max {
        Err(AprsError::BadWeatherValue { field, got })
    } else {
        Ok(())
    }
}

/// The wind-speed unit of the wire form being written.
///
/// The two Complete Weather Report layouts do not agree, and the
/// difference is 15%, so the choice cannot be left implicit at a call
/// site — see [`WeatherReport`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindUnit {
    /// The `sNNN` tagged field of a positionless report (chapter 12).
    MilesPerHour,
    /// The `SSS` half of the `DDD/SSS` data extension (chapter 7).
    Knots,
}

impl WindUnit {
    /// The wind speed as this form spells it.
    const fn wire(self, speed: Speed) -> i32 {
        match self {
            Self::MilesPerHour => speed.mph(),
            Self::Knots => speed.knots(),
        }
    }
}

/// Which of chapter 12's two weather layouts a scan or a build is in.
///
/// Not a `bool`, and not left implicit at the call site, because the
/// layout decides what the wire *means* twice over:
///
/// * the unit of the wind speed — [`WindUnit`], a 15% difference;
/// * **where the wind-speed slot is**, which then decides what the `s`
///   tag carries. A Positionless Weather Report spells the sustained
///   one-minute wind speed `sNNN`. A Complete Weather Report has the
///   7-byte Wind Direction and Wind Speed Data Extension "replace the
///   cccc and ssss fields" (chapter 12, Complete Weather Reports), so
///   its wind is positional and its very first `s` is already the extra
///   parameter the same chapter defines as "snowfall (in inches) in the
///   last 24 hours".
///
/// The layout is only half of that second question, and answering it
/// with the layout **alone** is the defect this type was first added
/// for, one layout over. The slot is spent exactly once, so a
/// positionless report's *second* `s` is snowfall too; the scan and the
/// build both carry a `wind_slot_spent` flag that the layout only
/// initialises. See [`WeatherReport::snowfall`].
///
/// Reading it wrong is not a missing field but a *wrong* one on top of
/// a right one, and — because `build` then writes the wrong value back
/// into the wind field — a rebuilt frame that lies about the weather.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WeatherLayout {
    /// The `_MDHM…` positionless report: the wind speed is the standard
    /// block's `sNNN`, in miles per hour, and any *later* `s` is
    /// snowfall.
    Positionless,
    /// The Complete Weather Report carried by a `!`/`=`/`/`/`@`
    /// position whose symbol code is `_`: the wind is the positional
    /// `DDD/SSS` extension in knots, so the slot is already spent and
    /// every `sNNN` is snowfall in inches.
    Complete,
}

impl WeatherLayout {
    /// The unit this layout spells the wind speed in.
    const fn wind_unit(self) -> WindUnit {
        match self {
            Self::Positionless => WindUnit::MilesPerHour,
            Self::Complete => WindUnit::Knots,
        }
    }
}

impl WeatherReport {
    /// The bytes chapter 12's optional "other parameters" add to the
    /// standard block.
    ///
    /// Each is `1 + 3` when present and **nothing** when absent, in both
    /// layouts: every report the crate has ever emitted ends at `b`, and
    /// lengthening all of them to carry dotted extras would break the
    /// byte-exact rebuild an igate depends on for traffic that never
    /// mentioned snow or sunlight.
    const fn extras_len(&self) -> usize {
        let luminosity = if self.luminosity.is_some() { 1 + 3 } else { 0 };
        let snowfall = if self.snowfall.is_some() { 1 + 3 } else { 0 };
        luminosity + snowfall
    }

    /// Bytes [`Self::write_fields`] will write for `layout`.
    ///
    /// This has to mirror that function exactly rather than assume a
    /// fixed block, because absent fields are now left out rather than
    /// written as dots. A length that over-counts would leave a run of
    /// zero bytes between the tagged block and `rest`.
    const fn fields_len(&self, layout: WeatherLayout) -> usize {
        let wind_is_positional = matches!(layout, WeatherLayout::Complete);
        let mut total = 0;
        let mut i = 0;
        while i < STANDARD_FIELDS {
            let (tag, width) = TAGGED_FIELDS[i];
            i += 1;
            if wind_is_positional && (tag == b'c' || tag == b's') {
                continue;
            }
            if self.has_tagged_value(tag, layout, wind_is_positional) {
                total += 1 + width;
            }
        }
        total + self.extras_len()
    }

    /// Validates every present quantity against the range its wire
    /// field can hold, in the unit and the field layout `layout` says
    /// that form uses.
    ///
    /// This is the boundary the data-model plan (§10.2) puts the
    /// validation on: a quantity is a physical value and has no range,
    /// a *field* is three digits and has one. `Speed::from_kmh(2000)`
    /// is a perfectly good speed and an impossible `sNNN`, and the
    /// error says so with the value.
    fn check(&self, layout: WeatherLayout) -> Result<(), AprsError> {
        if let Some(v) = self.wind_direction {
            check_range(b'c', i32::from(v), 0, 360)?;
        }
        if let Some(v) = self.wind_speed {
            check_range(b's', layout.wind_unit().wire(v), 0, 999)?;
        }
        // Three digits of whole inches, in *both* layouts: the wind slot
        // is spent by the time this field is written either way, so a
        // positionless report can spell a snow depth after its standard
        // block just as a Complete one can.
        if let Some(v) = self.snowfall {
            check_range(b's', snowfall_inches(v), 0, 999)?;
        }
        if let Some(v) = self.gust {
            check_range(b'g', v.mph(), 0, 999)?;
        }
        if let Some(v) = self.temperature {
            check_range(b't', v.fahrenheit(), -99, 999)?;
        }
        if let Some(v) = self.rain_1h {
            check_range(b'r', v.hundredths_inch(), 0, 999)?;
        }
        if let Some(v) = self.rain_24h {
            check_range(b'p', v.hundredths_inch(), 0, 999)?;
        }
        if let Some(v) = self.rain_midnight {
            check_range(b'P', v.hundredths_inch(), 0, 999)?;
        }
        if let Some(v) = self.humidity {
            check_range(b'h', i32::from(v.percent()), 1, 100)?;
        }
        if let Some(v) = self.barometric_pressure {
            check_range(b'b', v.tenths_hpa(), 0, 99_999)?;
        }
        // Three digits plus the choice of tag: `L` reaches 999 and `l`
        // another 1000 on top. The error names `L` for either, since
        // that is the letter the spec names the measurement with and a
        // value that overflows is above both.
        if let Some(v) = self.luminosity {
            check_range(b'L', i32::from(v), 0, 1999)?;
        }
        Ok(())
    }

    /// Reads one tagged field's value into `self`; the caller matched
    /// `tag` against [`TAGGED_FIELDS`]. `position` is the tag byte;
    /// the value starts one past it.
    ///
    /// `wind_slot_spent` says whether chapter 12's `ssss` slot has been
    /// consumed yet; reading an `s` in a layout that still has it spends
    /// it. See [`Self::parse_tagged`], which owns the flag.
    fn read_tagged(
        &mut self,
        info: &[u8],
        tag: u8,
        position: usize,
        layout: WeatherLayout,
        wind_slot_spent: &mut bool,
    ) -> Result<usize, AprsError> {
        let at = position + 1;
        #[allow(clippy::cast_possible_truncation)]
        match tag {
            // Chapter 12 retires `cccc` along with `ssss` in a Complete
            // Weather Report, and unlike `s` it gives `c` no second
            // meaning there — the "other parameters" list has exactly
            // four entries (`L`, `l`, `s`, `#`). So the fix is to stop
            // it writing, not to decode something else: the scan ends
            // and the bytes reach the caller in `rest`, verbatim.
            //
            // VERIFIED: `…_220/004c123g005t077` used to rebuild as
            // `…_123/004g005t077`, because `build` skips the `c` *tag*
            // and then writes `wind_direction` into the positional
            // `DDD` field. 220 became 123 on the wire.
            b'c' => match layout {
                WeatherLayout::Positionless => {
                    self.wind_direction = parse_value(info, at, 3)?.map(|v| v as u16);
                }
                WeatherLayout::Complete => {
                    return Err(AprsError::UnknownWeatherField { got: b'c' });
                }
            },
            // Chapter 12 gives these three digits to two different
            // measurements and the *slot*, not the layout, picks which;
            // see [`WeatherLayout`] and [`WeatherReport::snowfall`].
            b's' => {
                if *wind_slot_spent {
                    // The wind is already in hand — positionally in a
                    // Complete report, or from an earlier `s` in a
                    // positionless one — so this is snowfall, in whole
                    // inches rather than the hundredths the rain fields
                    // use. Touching `wind_speed` would overwrite a value
                    // that was already correct.
                    //
                    // Chapter 12 also permits a decimal point (`s0.5`).
                    // That is *not* parsed: three digits is the only
                    // spelling this crate writes, and `parse_tagged`
                    // already tolerates the other one by ending the
                    // scan, so `s0.5…` lands in `rest` intact and
                    // byte-exact rather than being mis-read.
                    self.snowfall = parse_value(info, at, 3)?
                        .map(|v| Rainfall::from_hundredths_inch((v * 100) as i32));
                } else {
                    // The tagged form is the sustained wind speed in
                    // miles per hour; the `DDD/SSS` extension form is
                    // knots and is read in `PositionWeather::parse`.
                    self.wind_speed = parse_value(info, at, 3)?.map(|v| Speed::from_mph(v as i32));
                    // Spent even when the value was `...`: `c...s...`
                    // uses the slot up and leaves `wind_speed` `None`,
                    // which is why this cannot be derived from
                    // `self.wind_speed.is_some()`.
                    *wind_slot_spent = true;
                }
            }
            b'g' => self.gust = parse_value(info, at, 3)?.map(|v| Speed::from_mph(v as i32)),
            b't' => {
                self.temperature = parse_temperature(info, at)?
                    .map(|f| Temperature::from_fahrenheit(i32::from(f)));
            }
            b'r' => {
                self.rain_1h =
                    parse_value(info, at, 3)?.map(|v| Rainfall::from_hundredths_inch(v as i32));
            }
            b'p' => {
                self.rain_24h =
                    parse_value(info, at, 3)?.map(|v| Rainfall::from_hundredths_inch(v as i32));
            }
            b'P' => {
                self.rain_midnight =
                    parse_value(info, at, 3)?.map(|v| Rainfall::from_hundredths_inch(v as i32));
            }
            b'h' => {
                // Two digits, so the wire value is always 0..=99 and
                // `from_wire_percent` (which reads 0 as 100%) cannot
                // fail; the error arm exists so a future width change
                // cannot silently clamp.
                self.humidity = match parse_value(info, at, 2)? {
                    None => None,
                    Some(v) => Some(Humidity::from_wire_percent(v as u8).map_err(|_| {
                        AprsError::BadWeatherValue {
                            field: b'h',
                            got: v as i32,
                        }
                    })?),
                };
            }
            b'b' => {
                self.barometric_pressure =
                    parse_value(info, at, 5)?.map(|v| Pressure::from_tenths_hpa(v as i32));
            }
            // Chapter 12's two spellings of one measurement: `L` is
            // 000-999 W/m^2, `l` is "1000 and above", the digits being
            // 1000 less than the value.
            //
            // The parenthetical "(L is inserted in place of one of the
            // rain values)" is guidance about the fixed-width diagram
            // and means nothing to a tag scanner -- VERIFIED against the
            // independent decoder, which reads
            // `...r000L050p000P000h50b09900` as all three rain fields
            // *plus* 50 W/m^2, with no field displaced.
            b'L' => self.luminosity = parse_value(info, at, 3)?.map(|v| v as u16),
            b'l' => self.luminosity = parse_value(info, at, 3)?.map(|v| (v + 1000) as u16),
            _ => return Err(AprsError::UnknownWeatherField { got: tag }),
        }
        let width = TAGGED_FIELDS
            .iter()
            .find(|&&(t, _)| t == tag)
            .map_or(0, |&(_, w)| w);
        Ok(position + 1 + width)
    }

    /// Parses letter-tagged fields starting at `position` until a byte
    /// that cannot start a weather field. Returns the offset where the
    /// uninterpreted `rest` begins.
    ///
    /// A weather report legally ends with a software-type letter plus a
    /// 2–4 character station-unit code (`tU2k` for an Ultimeter 2000,
    /// `wRSW`, and so on), and the spec permits *any* such code rather
    /// than a fixed list. Those trailers routinely begin with a byte
    /// that is also a field tag — `t` for temperature being the common
    /// case — so a tag whose value does not parse is treated as the
    /// start of the trailer, not as an error: the scan stops and the
    /// remaining bytes are handed back as `rest`. Real traffic depends
    /// on this; rejecting the packet would discard a complete and valid
    /// weather report over its manufacturer stamp.
    fn parse_tagged(
        &mut self,
        info: &[u8],
        mut position: usize,
        layout: WeatherLayout,
    ) -> Result<usize, AprsError> {
        // Chapter 12's `ssss` slot is consumed exactly once: positionally
        // in a Complete Weather Report, whose caller has already read the
        // `DDD/SSS` extension, or by the first `s` tag in a positionless
        // one. After that an `s` is snowfall in *either* layout.
        let mut wind_slot_spent = matches!(layout, WeatherLayout::Complete);
        // In the Complete layout that positional block *is* a
        // successfully read field, so the trailer tolerance below is
        // already in force at the first tag — both when a known tag's
        // value is malformed and when an unknown letter+digit turns up.
        // Without this, `…_220/004X123` lost the entire typed weather
        // report over a manufacturer stamp, and the tagged-`c` rejection
        // above would degrade the whole frame to a plain position
        // instead of ending the scan.
        let mut parsed_any = matches!(layout, WeatherLayout::Complete);
        while let Some(&tag) = info.get(position) {
            let is_known = TAGGED_FIELDS.iter().any(|&(t, _)| t == tag);
            if is_known {
                match self.read_tagged(info, tag, position, layout, &mut wind_slot_spent) {
                    Ok(next) => {
                        position = next;
                        parsed_any = true;
                        continue;
                    }
                    // Malformed value for a known tag. If we have
                    // already read a field this is the unit trailer;
                    // otherwise the block is broken.
                    Err(e) => {
                        if parsed_any {
                            break;
                        }
                        return Err(e);
                    }
                }
            }
            // A letter immediately followed by a digit, dot or minus
            // looks like a weather field we do not know -- but only
            // if nothing has been read yet.
            //
            // Once a real measurement is in hand, the same shape is
            // the trailer: chapter 12 ends a report with a
            // software-type letter and a 2-4 character unit code that
            // "users may specify" freely, so `X123` and `v6` are
            // ordinary. MEASURED: 38 corpus frames -- every
            // third-party-wrapped weather report from one igate --
            // were rejected outright over a two-byte `v6` stamp,
            // losing a complete and valid weather report to its
            // manufacturer's signature. This is the same reasoning as
            // the malformed-known-tag arm above, and the same reason.
            let next = info.get(position + 1).copied();
            if !parsed_any
                && tag.is_ascii_alphabetic()
                && matches!(next, Some(b) if b.is_ascii_digit() || b == b'.' || b == b'-')
            {
                return Err(AprsError::UnknownWeatherField { got: tag });
            }
            break;
        }
        Ok(position)
    }

    /// Writes the tagged field block for `layout` into `out`, returning
    /// the number of bytes written.
    ///
    /// The standard nine (minus the two the Complete layout retires) are
    /// always written, dotted when absent; chapter 12's "other
    /// parameters" only when present. `out` must hold at least
    /// [`Self::fields_len`] bytes, which is what the callers'
    /// `encoded_len` reserves.
    fn write_fields(&self, out: &mut [u8], layout: WeatherLayout) -> usize {
        let wind_is_positional = matches!(layout, WeatherLayout::Complete);
        let mut at = 0;
        for &(tag, width) in &TAGGED_FIELDS[..STANDARD_FIELDS] {
            // In a Complete Weather Report the positional `DDD/SSS`
            // extension "replace[d] the cccc and ssss fields" (chapter
            // 12), so neither tag is written here; `s` is re-used for
            // the snowfall below.
            if wind_is_positional && (tag == b'c' || tag == b's') {
                continue;
            }
            // An absent field is written by leaving it out, not by
            // emitting a dotted placeholder.
            //
            // Chapter 12 allows both spellings, saying the parameters
            // "may not even exist", so this is a choice between two
            // legal forms rather than a correctness question. It is
            // made this way because the other choice can produce
            // output this crate would itself reject: a placeholder run
            // lengthens the packet, and when a tag has already been
            // swallowed into `rest` the run is written *before* it, so
            // the tag appears twice. MEASURED on one such frame,
            // `_11230221c298s000g000t-103r000p000P000h10b10163wRSW`,
            // where a four-character temperature stops the scan: 53
            // bytes in, 74 out, with `r`, `p`, `P`, `h` and `b` each
            // appearing twice. Omission cannot lengthen a packet and so
            // cannot do either.
            //
            // The cost is the other direction: a sender that spelled an
            // absent field with dots gets it back omitted. MEASURED
            // over 5 517 weather packets from the live feed, 27.5%
            // spell at least one field with dots and 72.5% omit, and
            // the two are indistinguishable once parsed, because the
            // field type has two states and the wire has three. No
            // build strategy recovers that; only a three-state field
            // would.
            if self.has_tagged_value(tag, layout, wind_is_positional) {
                out[at] = tag;
                at += 1;
                match tag {
                    b't' => write_temperature(&mut out[at..at + width], self.temperature),
                    _ => write_value(
                        &mut out[at..at + width],
                        self.tagged_value(tag, layout, wind_is_positional),
                    ),
                }
                at += width;
            }
            // The luminosity sits directly after the `r` slot, and it
            // goes there whether or not `r` itself was written. This
            // is outside the presence check on purpose: skipping the
            // whole iteration when `r` is absent silently dropped the
            // luminosity, which `weather_luminosity_recovers_the_rest_of_the_block`
            // caught.
            if tag == b'r' {
                at += self.write_luminosity(&mut out[at..]);
            }
        }
        at += self.write_snowfall(&mut out[at..], layout);
        at
    }

    /// Writes `LNNN` / `lNNN` when a luminosity is present, returning
    /// the bytes written.
    ///
    /// Chapter 12 puts the field "in place of one of the rain values",
    /// and the independent decoder's own reading of
    /// `…r000L050p000P000h50b09900` keeps all three rain fields *and*
    /// the 50 W/m² — so nothing is displaced and directly after `r` is
    /// where a rebuild puts it back, byte for byte.
    fn write_luminosity(&self, out: &mut [u8]) -> usize {
        let Some(value) = self.luminosity else {
            return 0;
        };
        let (tag, digits) = luminosity_wire(value);
        out[0] = tag;
        write_digits(&mut out[1..4], u64::from(digits));
        1 + 3
    }

    /// Writes the trailing `sNNN` snowfall field when one is present,
    /// returning the bytes written.
    ///
    /// Chapter 12's extra parameters follow the nine standard fields,
    /// which is where the traffic that carries snow puts it
    /// (`…b09900s012wRSW`) and where the tag scan finds it again — so
    /// this is the byte-exact spelling, in **both** layouts. By here the
    /// wind slot is spent either way, which is exactly what
    /// [`Self::tagged_value`] is being told.
    fn write_snowfall(&self, out: &mut [u8], layout: WeatherLayout) -> usize {
        if self.snowfall.is_none() {
            return 0;
        }
        out[0] = b's';
        write_value(&mut out[1..4], self.tagged_value(b's', layout, true));
        1 + 3
    }

    /// The wind speed as the wire value `layout` spells it.
    ///
    /// Separate from [`Self::tagged_value`] because in a Complete
    /// Weather Report the wind is *positional* and the `s` tag belongs
    /// to the snowfall, so "the `s` field" and "the wind speed" are two
    /// questions there.
    #[allow(clippy::cast_sign_loss)]
    fn wind_wire(&self, layout: WeatherLayout) -> Option<u32> {
        self.wind_speed.map(|v| layout.wind_unit().wire(v) as u32)
    }

    /// Whether `tag` has a value to write, mirroring
    /// [`Self::tagged_value`] with `t` added, since the temperature is
    /// signed and travels a separate path there.
    ///
    /// Kept `const` and separate from `tagged_value` because
    /// [`Self::fields_len`] runs in a `const fn` and `Option::map` does
    /// not.
    const fn has_tagged_value(
        &self,
        tag: u8,
        _layout: WeatherLayout,
        wind_slot_spent: bool,
    ) -> bool {
        match tag {
            b'c' => self.wind_direction.is_some(),
            b's' => {
                if wind_slot_spent {
                    self.snowfall.is_some()
                } else {
                    // The `s` slot is POSITIONAL, not just tagged:
                    // chapter 12 spends `ssss` on the wind speed once,
                    // so the first `s` in a positionless report is the
                    // wind and a later one is the snowfall. Omitting an
                    // absent wind speed would promote the snowfall's
                    // `s` into first place, where it reads back as a
                    // wind speed. So the slot is held open with dots
                    // whenever a snowfall follows it.
                    //
                    // This is the one field where omission is not safe,
                    // and it is the reason the rule is "omit absent
                    // fields" rather than "omit absent fields, always".
                    // Caught by
                    // `positionless_weather_snowfall_builds_and_round_trips`,
                    // which built a report carrying only snowfall and
                    // read back a wind speed of zero.
                    self.wind_speed.is_some() || self.snowfall.is_some()
                }
            }
            b'g' => self.gust.is_some(),
            b't' => self.temperature.is_some(),
            b'r' => self.rain_1h.is_some(),
            b'p' => self.rain_24h.is_some(),
            b'P' => self.rain_midnight.is_some(),
            b'h' => self.humidity.is_some(),
            b'b' => self.barometric_pressure.is_some(),
            _ => false,
        }
    }

    /// The unsigned wire value carried by `tag` in `layout` (temperature
    /// excluded: it is signed and handled separately).
    ///
    /// `wind_slot_spent` is the build-side half of the question
    /// [`Self::read_tagged`] asks: an `s` written before chapter 12's
    /// `ssss` slot is used up is the wind speed, and one written after
    /// it is the snowfall — in either layout. The luminosity has no arm
    /// here because its *tag* depends on its value, so it is written by
    /// [`Self::write_luminosity`] instead.
    ///
    /// Every conversion here is exact, and every range was already
    /// verified by [`Self::check`], which the two `build` methods call
    /// first — so the narrowing casts cannot lose anything.
    #[allow(clippy::cast_sign_loss)]
    fn tagged_value(&self, tag: u8, layout: WeatherLayout, wind_slot_spent: bool) -> Option<u32> {
        match tag {
            b'c' => self.wind_direction.map(u32::from),
            b's' => {
                if wind_slot_spent {
                    self.snowfall.map(|v| snowfall_inches(v) as u32)
                } else {
                    self.wind_wire(layout)
                }
            }
            b'g' => self.gust.map(|v| v.mph() as u32),
            b'r' => self.rain_1h.map(|v| v.hundredths_inch() as u32),
            b'p' => self.rain_24h.map(|v| v.hundredths_inch() as u32),
            b'P' => self.rain_midnight.map(|v| v.hundredths_inch() as u32),
            b'h' => self.humidity.map(|v| u32::from(v.wire_percent())),
            b'b' => self.barometric_pressure.map(|v| v.tenths_hpa() as u32),
            _ => None,
        }
    }
}

impl<'a> PositionlessWeather<'a> {
    /// Length of the fixed prefix: DTI + MDHM timestamp.
    const PREFIX_LEN: usize = 1 + 8;

    /// Creates a positionless weather report with an empty trailing
    /// `rest`, validating the MDHM timestamp fields up front.
    ///
    /// # Errors
    ///
    /// [`AprsError::BadTimestamp`] on an out-of-range field.
    pub fn new(
        month: u8,
        day: u8,
        hour: u8,
        minute: u8,
        weather: WeatherReport,
    ) -> Result<Self, AprsError> {
        check_mdhm(
            i32::from(month),
            i32::from(day),
            i32::from(hour),
            i32::from(minute),
        )?;
        Ok(Self {
            month,
            day,
            hour,
            minute,
            weather,
            rest: b"",
        })
    }

    /// Returns the report with the given uninterpreted trailing bytes.
    #[must_use]
    pub const fn with_rest(self, rest: &'a [u8]) -> Self {
        Self { rest, ..self }
    }

    /// Parses a `_` positionless weather report.
    ///
    /// # Errors
    ///
    /// [`AprsError::Truncated`] on a short field,
    /// [`AprsError::BadDigit`] on non-digit timestamp or value bytes,
    /// [`AprsError::BadTimestamp`] on out-of-range timestamp fields and
    /// [`AprsError::UnknownWeatherField`] on an unrecognized field tag.
    pub fn parse(info: &'a [u8]) -> Result<Self, AprsError> {
        expect_byte(info, 0, b'_')?;
        if info.len() < Self::PREFIX_LEN {
            return Err(AprsError::Truncated {
                expected: Self::PREFIX_LEN,
                got: info.len(),
            });
        }
        let month = parse_digits(info, 1, 2)?;
        let day = parse_digits(info, 3, 2)?;
        let hour = parse_digits(info, 5, 2)?;
        let minute = parse_digits(info, 7, 2)?;
        check_mdhm(month, day, hour, minute)?;
        let mut weather = WeatherReport::default();
        let rest_at = weather.parse_tagged(info, Self::PREFIX_LEN, WeatherLayout::Positionless)?;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Ok(Self {
            month: month as u8,
            day: day as u8,
            hour: hour as u8,
            minute: minute as u8,
            weather,
            rest: info.get(rest_at..).unwrap_or(&[]),
        })
    }

    /// The serialized length of this report in bytes.
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        Self::PREFIX_LEN + self.weather.fields_len(WeatherLayout::Positionless) + self.rest.len()
    }

    /// Serializes the report into `buf`, returning the written length.
    ///
    /// # Errors
    ///
    /// [`AprsError::BadTimestamp`] on an out-of-range timestamp field,
    /// [`AprsError::BadWeatherValue`] on an out-of-range measurement and
    /// [`AprsError::BufferTooSmall`] when `buf` cannot hold the report.
    pub fn build(&self, buf: &mut [u8]) -> Result<usize, AprsError> {
        check_mdhm(
            i32::from(self.month),
            i32::from(self.day),
            i32::from(self.hour),
            i32::from(self.minute),
        )?;
        self.weather.check(WeatherLayout::Positionless)?;
        let needed = self.encoded_len();
        let max = buf.len();
        let out = buf
            .get_mut(..needed)
            .ok_or(AprsError::BufferTooSmall { needed, max })?;
        out[0] = b'_';
        write_digits(&mut out[1..3], u64::from(self.month));
        write_digits(&mut out[3..5], u64::from(self.day));
        write_digits(&mut out[5..7], u64::from(self.hour));
        write_digits(&mut out[7..9], u64::from(self.minute));
        let written = self
            .weather
            .write_fields(&mut out[Self::PREFIX_LEN..], WeatherLayout::Positionless);
        for (slot, byte) in out
            .iter_mut()
            .skip(Self::PREFIX_LEN + written)
            .zip(self.rest.iter())
        {
            *slot = *byte;
        }
        Ok(needed)
    }
}

/// Validates an MDHM timestamp; `field` in the error names the bad
/// component (`'M'` month, `'D'` day, `'H'` hour, `'m'` minute).
fn check_mdhm(month: i32, day: i32, hour: i32, minute: i32) -> Result<(), AprsError> {
    if !(1..=12).contains(&month) {
        return Err(AprsError::BadTimestamp {
            field: b'M',
            got: month,
        });
    }
    if !(1..=31).contains(&day) {
        return Err(AprsError::BadTimestamp {
            field: b'D',
            got: day,
        });
    }
    if !(0..=23).contains(&hour) {
        return Err(AprsError::BadTimestamp {
            field: b'H',
            got: hour,
        });
    }
    if !(0..=59).contains(&minute) {
        return Err(AprsError::BadTimestamp {
            field: b'm',
            got: minute,
        });
    }
    Ok(())
}

impl<'a> PositionWeather<'a> {
    /// Byte offset of the lat/lon block, which the optional timestamp
    /// pushes back by seven.
    const fn body_at(&self) -> usize {
        match self.timestamp {
            Some(_) => 1 + Timestamp::LEN,
            None => 1,
        }
    }

    /// Byte offset of the wind block, just past the lat/lon.
    const fn wind_at(&self) -> usize {
        self.body_at() + LATLON_LEN
    }

    /// Creates a non-messaging position-with-weather report on the
    /// primary table (the spec fixes the symbol code to `_`), with an
    /// empty trailing `rest`. Every part is validated by its own
    /// type; use the `with_*` methods to adjust the flags, table and
    /// trailing bytes.
    #[must_use]
    pub const fn new(latitude: Latitude, longitude: Longitude, weather: WeatherReport) -> Self {
        Self {
            latitude,
            longitude,
            ambiguity: Ambiguity::EXACT,
            symbol: Symbol::WEATHER_STATION,
            messaging: false,
            timestamp: None,
            weather,
            rest: b"",
        }
    }

    /// Returns the report as one of the **timestamped** Complete
    /// Weather Report layouts (`/` without messaging, `@` with).
    #[must_use]
    pub const fn with_timestamp(self, timestamp: Timestamp) -> Self {
        Self {
            timestamp: Some(timestamp),
            ..self
        }
    }

    /// Returns the report with the table byte of `symbol` (the code
    /// is always built as `_`, which the spec fixes for weather).
    #[must_use]
    pub const fn with_table(self, symbol: Symbol) -> Self {
        Self {
            symbol: Symbol::from_wire(symbol.to_wire().0, b'_'),
            ..self
        }
    }

    /// Returns the report with the messaging flag set as given (`=`
    /// DTI when `true`, `!` when `false`).
    #[must_use]
    pub const fn with_messaging(self, messaging: bool) -> Self {
        Self { messaging, ..self }
    }

    /// Returns the report with the given uninterpreted trailing bytes.
    #[must_use]
    pub const fn with_rest(self, rest: &'a [u8]) -> Self {
        Self { rest, ..self }
    }

    /// The station position, pairing the `latitude` and `longitude`
    /// fields so call sites need not rely on tuple ordering.
    #[must_use]
    pub const fn coordinates(&self) -> Coordinates {
        // Masked to the declared precision, like
        // [`Position::coordinates`]. Chapter 6 lets the longitude carry
        // its digits in full beside a blanked latitude.
        let latitude = match Latitude::new(self.ambiguity.mask(self.latitude.units())) {
            Ok(value) => value,
            // Unreachable: masking only reduces a magnitude.
            Err(_) => self.latitude,
        };
        let longitude = match Longitude::new(self.ambiguity.mask(self.longitude.units())) {
            Ok(value) => value,
            Err(_) => self.longitude,
        };
        Coordinates::new(latitude, longitude).with_ambiguity(self.ambiguity)
    }

    /// Parses an uncompressed Complete Weather Report: a position
    /// report whose symbol code is `_` and whose body carries the
    /// `DDD/SSSg...t...` weather block.
    ///
    /// Accepts all four uncompressed spellings chapter 12 defines:
    /// `!` and `=` without a timestamp, `/` and `@` with one. The
    /// compressed layouts are not implemented.
    ///
    /// # Errors
    ///
    /// The position errors of [`Position::parse`], plus
    /// [`AprsError::BadDigit`] / [`AprsError::ExpectedByte`] on
    /// malformed wind fields and [`AprsError::UnknownWeatherField`] on
    /// an unrecognized field tag.
    pub fn parse(info: &'a [u8]) -> Result<Self, AprsError> {
        let dti = byte_at(info, 0)?;
        let (messaging, timestamped) = match dti {
            b'=' => (true, false),
            b'!' => (false, false),
            b'@' => (true, true),
            b'/' => (false, true),
            other => return Err(AprsError::InvalidDataType { got: other }),
        };
        let timestamp = if timestamped {
            Some(Timestamp::parse(info, 1)?)
        } else {
            None
        };
        let body_at = if timestamped { 1 + Timestamp::LEN } else { 1 };
        let prefix_len = body_at + LATLON_LEN;
        if info.len() < prefix_len {
            return Err(AprsError::Truncated {
                expected: prefix_len,
                got: info.len(),
            });
        }
        let block = parse_latlon(info, body_at)?;
        let (symbol_table, symbol_code) = block.symbol.to_wire();
        if symbol_code != b'_' {
            return Err(AprsError::ExpectedByte {
                expected: b'_',
                got: symbol_code,
                position: prefix_len - 1,
            });
        }
        let mut weather = WeatherReport::default();
        // Wind direction / wind speed as course/speed: DDD/SSS.
        #[allow(clippy::cast_possible_truncation)]
        {
            weather.wind_direction = parse_value(info, prefix_len, 3)?.map(|v| v as u16);
            expect_byte(info, prefix_len + 3, b'/')?;
            // Chapter 12: this 7-byte field *is* the Wind Direction and
            // Wind Speed Data Extension, which chapter 7 defines in
            // knots -- not the miles per hour of the positionless
            // report's `sNNN`. Reading it as mph is a silent 15% error
            // that only an independent decoder can see.
            weather.wind_speed =
                parse_value(info, prefix_len + 4, 3)?.map(|v| Speed::from_knots(v as i32));
        }
        let rest_at = weather.parse_tagged(info, prefix_len + 7, WeatherLayout::Complete)?;
        Ok(Self {
            latitude: block.latitude,
            longitude: block.longitude,
            ambiguity: block.ambiguity,
            symbol: Symbol::from_wire(symbol_table, b'_'),
            messaging,
            timestamp,
            weather,
            rest: info.get(rest_at..).unwrap_or(&[]),
        })
    }

    /// The serialized length of this report in bytes.
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        // 7 for the positional `DDD/SSS` wind block, which the
        // Complete layout always writes.
        self.wind_at() + 7 + self.weather.fields_len(WeatherLayout::Complete) + self.rest.len()
    }

    /// Serializes the report into `buf`, returning the written length.
    ///
    /// # Errors
    ///
    /// [`AprsError::BadWeatherValue`] on an out-of-range measurement
    /// and [`AprsError::BufferTooSmall`] when `buf` cannot hold the
    /// report.
    pub fn build(&self, buf: &mut [u8]) -> Result<usize, AprsError> {
        self.weather.check(WeatherLayout::Complete)?;
        let needed = self.encoded_len();
        let max = buf.len();
        let out = buf
            .get_mut(..needed)
            .ok_or(AprsError::BufferTooSmall { needed, max })?;
        out[0] = match (self.timestamp.is_some(), self.messaging) {
            (false, false) => b'!',
            (false, true) => b'=',
            (true, false) => b'/',
            (true, true) => b'@',
        };
        if let Some(timestamp) = self.timestamp {
            timestamp.write(&mut out[1..1 + Timestamp::LEN])?;
        }
        let body_at = self.body_at();
        write_latlon(
            &mut out[body_at..body_at + LATLON_LEN],
            &LatLonBlock {
                latitude: self.latitude,
                longitude: self.longitude,
                symbol: Symbol::from_wire(self.symbol.to_wire().0, b'_'),
                ambiguity: self.ambiguity,
            },
        );
        let mut at = self.wind_at();
        write_value(
            &mut out[at..at + 3],
            self.weather.wind_direction.map(u32::from),
        );
        out[at + 3] = b'/';
        write_value(
            &mut out[at + 4..at + 7],
            self.weather.wind_wire(WeatherLayout::Complete),
        );
        at += 7;
        at += self
            .weather
            .write_fields(&mut out[at..], WeatherLayout::Complete);
        for (slot, byte) in out.iter_mut().skip(at).zip(self.rest.iter()) {
            *slot = *byte;
        }
        Ok(needed)
    }

    /// Converts to a plain [`Position`] (weather ignored, comment
    /// empty), a convenience for symbol-oriented consumers.
    #[must_use]
    pub const fn position(&self) -> Position<'static> {
        Position {
            latitude: self.latitude,
            longitude: self.longitude,
            ambiguity: self.ambiguity,
            symbol: Symbol::from_wire(self.symbol.to_wire().0, b'_'),
            messaging: self.messaging,
            compressed: false,
            // A weather report's wind lives in its own fields, not in a
            // borrowed data extension.
            extension: None,
            comment: b"",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_value_digits_dots_and_spaces() {
        assert_eq!(parse_value(b"220", 0, 3), Ok(Some(220)));
        assert_eq!(parse_value(b"...", 0, 3), Ok(None));
        assert_eq!(parse_value(b"   ", 0, 3), Ok(None));
        // A mixed run is a typed error, not silently None.
        assert_eq!(
            parse_value(b".2.", 0, 3),
            Err(AprsError::ExpectedByte {
                expected: b'.',
                got: b'2',
                position: 1
            })
        );
        assert_eq!(
            parse_value(b"2x0", 0, 3),
            Err(AprsError::BadDigit {
                got: b'x',
                position: 1
            })
        );
        // Truncated value.
        assert_eq!(
            parse_value(b"22", 0, 3),
            Err(AprsError::Truncated {
                expected: 3,
                got: 2
            })
        );
    }

    #[test]
    fn parse_temperature_signs() {
        assert_eq!(parse_temperature(b"077", 0), Ok(Some(77)));
        assert_eq!(parse_temperature(b"-01", 0), Ok(Some(-1)));
        assert_eq!(parse_temperature(b"-99", 0), Ok(Some(-99)));
        assert_eq!(parse_temperature(b"...", 0), Ok(None));
        assert_eq!(
            parse_temperature(b"-x1", 0),
            Err(AprsError::BadDigit {
                got: b'x',
                position: 1
            })
        );
    }

    #[test]
    fn write_value_and_temperature_layout() {
        let mut out = [0u8; 3];
        write_value(&mut out, Some(7));
        assert_eq!(&out, b"007");
        write_value(&mut out, None);
        assert_eq!(&out, b"...");
        write_temperature(&mut out, Some(Temperature::from_fahrenheit(-5)));
        assert_eq!(&out, b"-05");
        write_temperature(&mut out, Some(Temperature::from_fahrenheit(999)));
        assert_eq!(&out, b"999");
        write_temperature(&mut out, None);
        assert_eq!(&out, b"...");
    }

    #[test]
    fn coordinates_pair_the_fields() {
        let report = match PositionWeather::parse(b"!4903.50N/07201.75W_220/004g005t077") {
            Ok(r) => r,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(
            report.coordinates(),
            Coordinates::new(report.latitude, report.longitude)
        );
    }

    #[test]
    fn humidity_wire_convention() {
        // 100% is sent as "00"; "00" parses back to 100%.
        let report = WeatherReport {
            humidity: Humidity::new(100).ok(),
            ..WeatherReport::default()
        };
        assert_eq!(
            report.tagged_value(b'h', WeatherLayout::Positionless, false),
            Some(0)
        );
        let mut parsed = WeatherReport::default();
        let mut spent = false;
        let end = match parsed.read_tagged(b"h00", b'h', 0, WeatherLayout::Positionless, &mut spent)
        {
            Ok(n) => n,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(end, 3);
        assert_eq!(parsed.humidity.map(Humidity::percent), Some(100));
    }

    #[test]
    fn tagged_scanner_stops_at_unknown_trailer() {
        let mut report = WeatherReport::default();
        // 'X' followed by a letter cannot start a weather field: it is
        // the uninterpreted rest, not an error.
        let rest_at = match report.parse_tagged(b"c220s004Xyz", 0, WeatherLayout::Positionless) {
            Ok(n) => n,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(rest_at, 8);
        assert_eq!(report.wind_direction, Some(220));
        // The tagged `s` field is miles per hour.
        assert_eq!(report.wind_speed, Some(Speed::from_mph(4)));
        assert_eq!(report.snowfall, None);

        // A letter followed by a digit *after* real measurements is
        // the software-type-and-unit trailer chapter 12 ends a report
        // with, not an unknown field: `X` is the published code for
        // X-APRS, and the unit code after it is free-form. Rejecting
        // the packet here threw away 38 corpus frames' worth of
        // complete weather over a two-byte signature.
        let mut report = WeatherReport::default();
        assert_eq!(
            report.parse_tagged(b"c220X123", 0, WeatherLayout::Positionless),
            Ok(4)
        );
        assert_eq!(report.wind_direction, Some(220));
        let mut report = WeatherReport::default();
        assert_eq!(
            report.parse_tagged(b"c220s004g010t065v6", 0, WeatherLayout::Positionless),
            Ok(16)
        );

        // With nothing parsed yet it is still an error: a block that
        // begins with a field we do not know is broken, and saying so
        // with the offending byte beats guessing.
        let mut report = WeatherReport::default();
        assert_eq!(
            report.parse_tagged(b"X123c220", 0, WeatherLayout::Positionless),
            Err(AprsError::UnknownWeatherField { got: b'X' })
        );
    }

    /// The same three digits, read twice, because the *slot* decides
    /// which measurement they are.
    #[test]
    fn tagged_s_is_wind_in_one_layout_and_snow_in_the_other() {
        let mut positionless = WeatherReport::default();
        assert_eq!(
            positionless.parse_tagged(b"s050", 0, WeatherLayout::Positionless),
            Ok(4)
        );
        assert_eq!(positionless.wind_speed, Some(Speed::from_mph(50)));
        assert_eq!(positionless.snowfall, None);

        // In a Complete Weather Report the positional extension already
        // holds the wind, so the scan must leave it alone.
        let mut complete = WeatherReport {
            wind_speed: Some(Speed::from_knots(4)),
            ..WeatherReport::default()
        };
        assert_eq!(
            complete.parse_tagged(b"s050", 0, WeatherLayout::Complete),
            Ok(4)
        );
        assert_eq!(complete.wind_speed, Some(Speed::from_knots(4)));
        assert_eq!(
            complete.snowfall,
            Some(Rainfall::from_hundredths_inch(5_000))
        );
        assert_eq!(complete.snowfall.map(snowfall_inches), Some(50));
    }

    /// The slot is spent **once**, so a positionless report's second `s`
    /// is snow too — the half of the defect that gating on the layout
    /// alone left live.
    #[test]
    fn second_tagged_s_is_snow_in_the_positionless_layout_too() {
        let mut report = WeatherReport::default();
        assert_eq!(
            report.parse_tagged(b"s004g005s012", 0, WeatherLayout::Positionless),
            Ok(12)
        );
        assert_eq!(report.wind_speed, Some(Speed::from_mph(4)));
        assert_eq!(report.gust, Some(Speed::from_mph(5)));
        assert_eq!(report.snowfall, Some(Rainfall::from_hundredths_inch(1_200)));

        // The flag cannot be derived from `wind_speed.is_some()`: an
        // explicitly absent wind (`s...`) spends the slot and leaves the
        // value `None`, so the next `s` is still snow.
        let mut dotted = WeatherReport::default();
        assert_eq!(
            dotted.parse_tagged(b"c...s...s012", 0, WeatherLayout::Positionless),
            Ok(12)
        );
        assert_eq!(dotted.wind_direction, None);
        assert_eq!(dotted.wind_speed, None);
        assert_eq!(dotted.snowfall, Some(Rainfall::from_hundredths_inch(1_200)));
    }

    /// Chapter 12's "other parameters" list has exactly four entries
    /// (`L`, `l`, `s`, `#`), so a tagged `c` has no second meaning in a
    /// Complete Weather Report: the scan ends and the bytes are `rest`.
    #[test]
    fn tagged_c_ends_the_scan_in_the_complete_layout() {
        let mut complete = WeatherReport {
            wind_direction: Some(220),
            ..WeatherReport::default()
        };
        assert_eq!(
            complete.parse_tagged(b"c123g005t077", 0, WeatherLayout::Complete),
            Ok(0)
        );
        assert_eq!(complete.wind_direction, Some(220));

        // A bare `read_tagged` says why, for a caller that wants to know.
        let mut spent = true;
        assert_eq!(
            WeatherReport::default().read_tagged(
                b"c123",
                b'c',
                0,
                WeatherLayout::Complete,
                &mut spent
            ),
            Err(AprsError::UnknownWeatherField { got: b'c' })
        );

        // The positionless layout still reads it: there `cccc` is the
        // only wind direction there is.
        let mut positionless = WeatherReport::default();
        assert_eq!(
            positionless.parse_tagged(b"c123", 0, WeatherLayout::Positionless),
            Ok(4)
        );
        assert_eq!(positionless.wind_direction, Some(123));
    }

    /// One unknown mid-block tag used to cost every field behind it, so
    /// luminosity is not a "missing field" but a truncated report.
    #[test]
    fn luminosity_is_read_mid_block_and_spells_itself_back() {
        let mut report = WeatherReport::default();
        assert_eq!(
            report.parse_tagged(b"r000L050p001P002", 0, WeatherLayout::Complete),
            Ok(16)
        );
        assert_eq!(report.luminosity, Some(50));
        // The four downstream fields the scan used to abandon.
        assert_eq!(report.rain_1h, Some(Rainfall::from_hundredths_inch(0)));
        assert_eq!(report.rain_24h, Some(Rainfall::from_hundredths_inch(1)));
        assert_eq!(
            report.rain_midnight,
            Some(Rainfall::from_hundredths_inch(2))
        );

        // `l` is the same measurement 1000 higher.
        let mut high = WeatherReport::default();
        assert_eq!(
            high.parse_tagged(b"l050", 0, WeatherLayout::Positionless),
            Ok(4)
        );
        assert_eq!(high.luminosity, Some(1050));

        // Which letter a value spells is a total function of the value,
        // so the round trip needs no memory of which one arrived.
        assert_eq!(luminosity_wire(0), (b'L', 0));
        assert_eq!(luminosity_wire(999), (b'L', 999));
        assert_eq!(luminosity_wire(1000), (b'l', 0));
        assert_eq!(luminosity_wire(1999), (b'l', 999));
    }

    /// Inches is a coarser unit than [`Rainfall`] carries, so the wire
    /// value rounds half away from zero — the same rule `units::div_round`
    /// uses, and the same rounding `r`'s hundredths already do.
    #[test]
    fn snowfall_wire_inches_round_half_away_from_zero() {
        assert_eq!(snowfall_inches(Rainfall::ZERO), 0);
        assert_eq!(snowfall_inches(Rainfall::from_hundredths_inch(49)), 0);
        assert_eq!(snowfall_inches(Rainfall::from_hundredths_inch(50)), 1);
        assert_eq!(snowfall_inches(Rainfall::from_hundredths_inch(149)), 1);
        assert_eq!(snowfall_inches(Rainfall::from_hundredths_inch(150)), 2);
        assert_eq!(snowfall_inches(Rainfall::from_hundredths_inch(-50)), -1);
        // Whole inches, which is all a parse can produce, are exact.
        for inches in [0, 1, 12, 999] {
            assert_eq!(
                snowfall_inches(Rainfall::from_hundredths_inch(inches * 100)),
                inches
            );
        }
        // A saturated depth must not panic on the rounding bias.
        assert_eq!(
            snowfall_inches(Rainfall::from_micrometers(i64::MAX)),
            i32::MAX / 100
        );
    }

    #[test]
    fn mdhm_bounds() {
        assert_eq!(check_mdhm(1, 1, 0, 0), Ok(()));
        assert_eq!(check_mdhm(12, 31, 23, 59), Ok(()));
        assert_eq!(
            check_mdhm(0, 1, 0, 0),
            Err(AprsError::BadTimestamp {
                field: b'M',
                got: 0
            })
        );
        assert_eq!(
            check_mdhm(13, 1, 0, 0),
            Err(AprsError::BadTimestamp {
                field: b'M',
                got: 13
            })
        );
        assert_eq!(
            check_mdhm(1, 32, 0, 0),
            Err(AprsError::BadTimestamp {
                field: b'D',
                got: 32
            })
        );
        assert_eq!(
            check_mdhm(1, 1, 24, 0),
            Err(AprsError::BadTimestamp {
                field: b'H',
                got: 24
            })
        );
        assert_eq!(
            check_mdhm(1, 1, 0, 60),
            Err(AprsError::BadTimestamp {
                field: b'm',
                got: 60
            })
        );
    }

    /// A quantity is unbounded; a *field* is three digits. The wire
    /// boundary is where that difference is enforced, and the error
    /// names the field and carries the offending value.
    #[test]
    fn range_checks_on_build() {
        let bad = WeatherReport {
            wind_direction: Some(361),
            ..WeatherReport::default()
        };
        assert_eq!(
            bad.check(WeatherLayout::Positionless),
            Err(AprsError::BadWeatherValue {
                field: b'c',
                got: 361
            })
        );
        let bad = WeatherReport {
            barometric_pressure: Some(Pressure::from_tenths_hpa(100_000)),
            ..WeatherReport::default()
        };
        assert_eq!(
            bad.check(WeatherLayout::Positionless),
            Err(AprsError::BadWeatherValue {
                field: b'b',
                got: 100_000
            })
        );
        // A perfectly good speed that no three-digit field can hold.
        let gale = WeatherReport {
            wind_speed: Some(Speed::from_kmh(2000)),
            ..WeatherReport::default()
        };
        assert_eq!(
            gale.check(WeatherLayout::Positionless),
            Err(AprsError::BadWeatherValue {
                field: b's',
                got: 1243
            })
        );
        // ...and which *is* representable in the other form's unit,
        // because 2000 km/h is 1080 knots but 1243 mph. The unit is
        // not a detail.
        assert_eq!(
            gale.check(WeatherLayout::Complete),
            Err(AprsError::BadWeatherValue {
                field: b's',
                got: 1080
            })
        );
        let brisk = WeatherReport {
            wind_speed: Some(Speed::from_kmh(1800)),
            ..WeatherReport::default()
        };
        assert_eq!(brisk.check(WeatherLayout::Complete), Ok(()));
        assert_eq!(
            brisk.check(WeatherLayout::Positionless),
            Err(AprsError::BadWeatherValue {
                field: b's',
                got: 1118
            })
        );
        // A snow depth is three digits of inches in *both* layouts: the
        // wind slot is spent before the field is written either way, so
        // there is a byte-exact spelling for it in each.
        let snow = WeatherReport {
            snowfall: Some(Rainfall::from_hundredths_inch(5_000)),
            ..WeatherReport::default()
        };
        assert_eq!(snow.check(WeatherLayout::Positionless), Ok(()));
        assert_eq!(snow.check(WeatherLayout::Complete), Ok(()));
        let blizzard = WeatherReport {
            snowfall: Some(Rainfall::from_hundredths_inch(100_000)),
            ..WeatherReport::default()
        };
        for layout in [WeatherLayout::Positionless, WeatherLayout::Complete] {
            assert_eq!(
                blizzard.check(layout),
                Err(AprsError::BadWeatherValue {
                    field: b's',
                    got: 1000
                }),
                "{layout:?}"
            );
        }
        // Luminosity is three digits plus the choice of tag, so `l999`
        // is the ceiling and one more is out of range.
        let bright = WeatherReport {
            luminosity: Some(1999),
            ..WeatherReport::default()
        };
        assert_eq!(bright.check(WeatherLayout::Complete), Ok(()));
        let brighter = WeatherReport {
            luminosity: Some(2000),
            ..WeatherReport::default()
        };
        assert_eq!(
            brighter.check(WeatherLayout::Complete),
            Err(AprsError::BadWeatherValue {
                field: b'L',
                got: 2000
            })
        );
    }
}
