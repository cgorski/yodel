//! Physical quantities with their units in the type.
//!
//! APRS names the same physical quantity in a different unit almost
//! every time it appears: altitude is **feet** in a `/A=` comment,
//! **meters** in a Mic-E report and feet again (exponentially encoded)
//! in a compressed position; a weather report wants **degrees
//! Fahrenheit**, **tenths of a hectopascal**, **hundredths of an inch**
//! and **miles per hour** in one nine-field line, while the sensors
//! feeding it read Celsius, pascals, millimeters and meters per second.
//!
//! Those conversions used to live at the call site, which is where they
//! go wrong. The types here move each one behind a **named** constructor
//! and a **named** accessor, so a unit confusion has to be written out
//! in full before it compiles.
//!
//! # The quantities
//!
//! | Type | Canonical storage | Constructors / accessors |
//! |---|---|---|
//! | [`Distance`] | `i64` micrometers | `feet`, `meters`, `millimeters`, `kilometers`, `nautical_miles`, `statute_miles`, `inches`, `micrometers` |
//! | [`Speed`] | `i64` millimeters/hour | `knots`, `mph`, `kmh`, `meters_per_second`, `millimeters_per_second`, `millimeters_per_hour` |
//! | [`Rainfall`] | `i64` micrometers | `hundredths_inch`, `millimeters`, `micrometers` |
//! | [`Temperature`] | `i64` 1/45 000 °C | `celsius`, `fahrenheit`, `millidegrees_celsius` |
//! | [`Pressure`] | `i64` millipascals | `pascals`, `hpa`, `tenths_hpa`, `hundredths_inhg`, `millipascals` |
//! | [`Power`] | `i64` microwatts | `watts`, `milliwatts`, `microwatts`, `dbm` |
//! | [`Bearing`] | `u16` degrees, `0..=359` | `degrees`, [`Bearing::compass_point`] |
//! | [`Humidity`] | `u8` percent, `1..=100` | `percent`, `wire_percent` |
//!
//! # Why these units
//!
//! Each canonical unit is chosen so that **every unit APRS puts on the
//! wire is an exact integer number of it**. There is no
//! floating point anywhere in this module, and no wire value loses
//! precision by passing through it:
//!
//! | Conversion | Canonical value | Exact |
//! |---|---|---|
//! | 1 ft | 304 800 µm | yes |
//! | 1 m | 1 000 000 µm | yes |
//! | 1 nmi | 1 852 000 000 µm | yes |
//! | 1 statute mile | 1 609 344 000 µm | yes |
//! | 1 in | 25 400 µm | yes |
//! | 0.01 in (the rainfall wire unit) | 254 µm | yes |
//! | 1 knot | 1 852 000 mm/h | yes |
//! | 1 mph | 1 609 344 mm/h | yes |
//! | 1 km/h | 1 000 000 mm/h | yes |
//! | 0.1 hPa (the pressure wire unit) | 10 000 mPa | yes |
//! | 1 inHg | 3 386 389 mPa | yes (conventional inHg) |
//! | 1 °F step | 25 000 units | yes — see [`Temperature`] |
//!
//! # Rounding, stated once
//!
//! * **Same-unit access is exact.** `Distance::from_feet(n).feet() == n`
//!   for every representable `n`, and likewise for every other pair.
//! * **Cross-unit access rounds half away from zero.** 376 m reads as
//!   **1234 ft**, not 1233. Truncation would bias every conversion
//!   downwards, which accumulates in exactly the direction that makes an
//!   altitude report read low.
//!
//! # Quantities are unbounded; wire *fields* are not
//!
//! `Temperature::from_celsius(-100)` is a perfectly good temperature and
//! an invalid APRS `t` field. The range check therefore lives on the
//! wire setter, not in the quantity: clamping here would silently
//! corrupt a satellite operator's telemetry and a balloon's altitude at
//! 30 km, and would make these types useless for anything but APRS.
//!
//! Only [`Bearing`] and [`Humidity`] validate, because they are
//! *enumerable* ranges rather than accumulating measurements — an angle
//! outside `0..=359` is not a bigger angle, it is a different angle.
//!
//! # No panics, in release or debug
//!
//! Every operation here saturates. A debug build panics on overflow, so
//! "cannot panic" would be an empty claim without that; `tests/units.rs`
//! drives `i64::MIN`/`i64::MAX` through every constructor, accessor and
//! operator to prove it.
//!
//! # What is absent
//!
//! * `Deref`, `From<i32>`/`Into<i32>`, `.0`, `raw()`, `value()`, or any
//!   accessor that returns a number without naming its unit. Each one
//!   re-opens the hole this module exists to close.
//! * A unit-less `Display`. [`Debug`](core::fmt::Debug) is implemented
//!   instead, and prints **both** unit systems
//!   (`Distance(376 m / 1234 ft)`), so a log line or a failing assertion
//!   explains itself.
//! * Phantom-typed units (`Distance<Feet>` vs `Distance<Meters>`).
//!   Because the units above are exactly interconvertible there is only
//!   ever one `Distance` and so nothing to mix; the parameter would buy
//!   worse error messages and per-unit monomorphisation for no
//!   additional bug caught. The residual risk — passing a metres value
//!   to `from_feet` — is a property of an untyped integer arriving from
//!   a GPS or sensor driver, and no type system reaches it. The
//!   constructor's *name* is the last line of defence, which is why
//!   every one of them carries its unit.
//! * Arithmetic on [`Temperature`]. See that type: it is an interval
//!   scale, not a ratio scale, so adding two of them is meaningless.
//! * An exact haversine, and locale-aware formatting.

/// Micrometers per foot. Exact: a foot is defined as 0.3048 m.
const UM_PER_FOOT: i64 = 304_800;
/// Micrometers per meter.
const UM_PER_METER: i64 = 1_000_000;
/// Micrometers per kilometer.
const UM_PER_KILOMETER: i64 = 1_000_000_000;
/// Micrometers per nautical mile. Exact: a nautical mile is 1852 m.
const UM_PER_NAUTICAL_MILE: i64 = 1_852_000_000;
/// Micrometers per statute mile. Exact: 5280 ft.
const UM_PER_STATUTE_MILE: i64 = 1_609_344_000;
/// Micrometers per inch. Exact: 25.4 mm.
const UM_PER_INCH: i64 = 25_400;
/// Micrometers per hundredth of an inch — the APRS rainfall wire unit.
const UM_PER_HUNDREDTH_INCH: i64 = 254;
/// Micrometers per millimeter.
const UM_PER_MILLIMETER: i64 = 1_000;

/// Millimeters/hour per knot. Exact: one nautical mile per hour.
const MMH_PER_KNOT: i64 = 1_852_000;
/// Millimeters/hour per mile per hour. Exact: one statute mile per hour.
const MMH_PER_MPH: i64 = 1_609_344;
/// Millimeters/hour per kilometer per hour.
const MMH_PER_KMH: i64 = 1_000_000;
/// Millimeters/hour per meter per second. Exact: 3600 s in an hour.
const MMH_PER_MS: i64 = 3_600_000;
/// Millimeters/hour per millimeter per second.
const MMH_PER_MMS: i64 = 3_600;

/// Canonical temperature units per whole degree Celsius.
///
/// 45 000 is the smallest unit in which an integer number of degrees
/// **Fahrenheit** is also exact, because 45 000 = 9000 × 5 is divisible
/// by 9: one Fahrenheit step is 5/9 °C = 25 000 units.
const UNITS_PER_CELSIUS: i64 = 45_000;
/// Canonical temperature units per whole degree Fahrenheit.
const UNITS_PER_FAHRENHEIT: i64 = 25_000;
/// Canonical temperature units per millidegree Celsius.
const UNITS_PER_MILLICELSIUS: i64 = 45;
/// The Fahrenheit scale's zero offset, in whole degrees Fahrenheit.
const FAHRENHEIT_OFFSET: i64 = 32;

/// Millipascals per pascal.
const MPA_PER_PASCAL: i64 = 1_000;
/// Millipascals per hectopascal.
const MPA_PER_HPA: i64 = 100_000;
/// Millipascals per tenth of a hectopascal — the APRS pressure wire unit.
const MPA_PER_TENTH_HPA: i64 = 10_000;
/// Millipascals per inch of mercury, using the conventional
/// 1 inHg = 3386.389 Pa.
const MPA_PER_INHG: i64 = 3_386_389;

/// Microwatts per watt.
const UW_PER_WATT: i64 = 1_000_000;
/// Microwatts per milliwatt.
const UW_PER_MILLIWATT: i64 = 1_000;

/// `round(10^(r/10) * 10^6)` for `r` in `0..10` — the mantissa of a
/// decibel-milliwatt value, one entry per tenth of a decade.
///
/// Splitting a dBm figure into whole decades plus one of these ten
/// mantissas is what lets [`Power::from_dbm`] evaluate a logarithm with
/// integer arithmetic only.
const DBM_MANTISSA: [i64; 10] = [
    1_000_000, 1_258_925, 1_584_893, 1_995_262, 2_511_886, 3_162_278, 3_981_072, 5_011_872,
    6_309_573, 7_943_282,
];

/// Powers of ten, for scaling [`DBM_MANTISSA`] by whole decades.
const POW10: [i64; 19] = [
    1,
    10,
    100,
    1_000,
    10_000,
    100_000,
    1_000_000,
    10_000_000,
    100_000_000,
    1_000_000_000,
    10_000_000_000,
    100_000_000_000,
    1_000_000_000_000,
    10_000_000_000_000,
    100_000_000_000_000,
    1_000_000_000_000_000,
    10_000_000_000_000_000,
    100_000_000_000_000_000,
    1_000_000_000_000_000_000,
];

/// The lowest dBm figure a positive microwatt value can represent
/// (1 µW).
const DBM_MIN: i32 = -30;
/// The highest dBm figure `i64` microwatts can represent.
const DBM_MAX: i32 = 160;

/// Divides, rounding half **away from zero**, for a positive divisor.
///
/// The saturating add is what makes the module's no-panic promise true
/// at the extremes: `value` may legitimately be `i64::MAX` after a
/// saturating multiply.
const fn div_round(value: i64, divisor: i64) -> i64 {
    let half = divisor / 2;
    if value >= 0 {
        value.saturating_add(half) / divisor
    } else {
        value.saturating_sub(half) / divisor
    }
}

/// Narrows to `i32`, saturating rather than wrapping.
const fn narrow(value: i64) -> i32 {
    if value > i32::MAX as i64 {
        i32::MAX
    } else if value < i32::MIN as i64 {
        i32::MIN
    } else {
        value as i32
    }
}

/// Failure of a validated quantity constructor.
///
/// Carries the offending value, in keeping with the crate's convention
/// that an error names what it rejected rather than just that it
/// rejected something.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum UnitError {
    /// A bearing outside `0..=360` (360 is accepted and folded to 0).
    BadBearing {
        /// The rejected value, in degrees.
        got: u16,
    },
    /// A relative humidity outside `1..=100` percent.
    BadHumidity {
        /// The rejected value, in percent.
        got: u8,
    },
}

impl core::fmt::Display for UnitError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::BadBearing { got } => {
                write!(f, "bearing {got} is outside 0..=360 degrees")
            }
            Self::BadHumidity { got } => {
                write!(f, "relative humidity {got} is outside 1..=100 percent")
            }
        }
    }
}

impl core::error::Error for UnitError {}

/// A length, stored as `i64` micrometers.
///
/// Micrometers because rainfall needs hundredths of an inch (254 µm
/// exactly) while station separation needs 20 000 km, and no 32-bit
/// integer holds both. The range is ±9.2 × 10¹² km, some 460 000× the
/// antipodal distance, so nothing APRS can express comes near the
/// limits.
///
/// ```
/// use yodel::units::Distance;
///
/// // A GPS gives meters; the `/A=` comment field wants feet.
/// let altitude = Distance::from_meters(376);
/// assert_eq!(altitude.feet(), 1234);
/// assert_eq!(altitude.meters(), 376);
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct Distance {
    /// The length in micrometers.
    micrometers: i64,
}

impl Distance {
    /// A zero length.
    pub const ZERO: Self = Self { micrometers: 0 };

    /// A length in whole feet.
    #[must_use]
    pub const fn from_feet(feet: i32) -> Self {
        Self {
            micrometers: (feet as i64).saturating_mul(UM_PER_FOOT),
        }
    }

    /// A length in whole meters.
    #[must_use]
    pub const fn from_meters(meters: i32) -> Self {
        Self {
            micrometers: (meters as i64).saturating_mul(UM_PER_METER),
        }
    }

    /// A length in whole millimeters.
    #[must_use]
    pub const fn from_millimeters(millimeters: i32) -> Self {
        Self {
            micrometers: (millimeters as i64).saturating_mul(UM_PER_MILLIMETER),
        }
    }

    /// A length in whole kilometers.
    #[must_use]
    pub const fn from_kilometers(kilometers: i32) -> Self {
        Self {
            micrometers: (kilometers as i64).saturating_mul(UM_PER_KILOMETER),
        }
    }

    /// A length in whole nautical miles.
    #[must_use]
    pub const fn from_nautical_miles(nautical_miles: i32) -> Self {
        Self {
            micrometers: (nautical_miles as i64).saturating_mul(UM_PER_NAUTICAL_MILE),
        }
    }

    /// A length in whole statute miles.
    #[must_use]
    pub const fn from_statute_miles(statute_miles: i32) -> Self {
        Self {
            micrometers: (statute_miles as i64).saturating_mul(UM_PER_STATUTE_MILE),
        }
    }

    /// A length in whole inches.
    #[must_use]
    pub const fn from_inches(inches: i32) -> Self {
        Self {
            micrometers: (inches as i64).saturating_mul(UM_PER_INCH),
        }
    }

    /// A length in micrometers, the canonical unit.
    #[must_use]
    pub const fn from_micrometers(micrometers: i64) -> Self {
        Self { micrometers }
    }

    /// The length in whole feet, rounded half away from zero.
    #[must_use]
    pub const fn feet(self) -> i32 {
        narrow(div_round(self.micrometers, UM_PER_FOOT))
    }

    /// The length in whole meters, rounded half away from zero.
    #[must_use]
    pub const fn meters(self) -> i32 {
        narrow(div_round(self.micrometers, UM_PER_METER))
    }

    /// The length in whole millimeters, rounded half away from zero.
    #[must_use]
    pub const fn millimeters(self) -> i32 {
        narrow(div_round(self.micrometers, UM_PER_MILLIMETER))
    }

    /// The length in whole kilometers, rounded half away from zero.
    #[must_use]
    pub const fn kilometers(self) -> i32 {
        narrow(div_round(self.micrometers, UM_PER_KILOMETER))
    }

    /// The length in whole nautical miles, rounded half away from zero.
    #[must_use]
    pub const fn nautical_miles(self) -> i32 {
        narrow(div_round(self.micrometers, UM_PER_NAUTICAL_MILE))
    }

    /// The length in whole statute miles, rounded half away from zero.
    #[must_use]
    pub const fn statute_miles(self) -> i32 {
        narrow(div_round(self.micrometers, UM_PER_STATUTE_MILE))
    }

    /// The length in whole inches, rounded half away from zero.
    #[must_use]
    pub const fn inches(self) -> i32 {
        narrow(div_round(self.micrometers, UM_PER_INCH))
    }

    /// The length in micrometers, the canonical unit (exact).
    #[must_use]
    pub const fn micrometers(self) -> i64 {
        self.micrometers
    }
}

/// A speed, stored as `i64` millimeters per hour.
///
/// Millimeters per hour keeps knots, miles per hour and kilometres per
/// hour all exact. `i64` rather than `i32` because a speed *computed*
/// from two positions is not bounded by the APRS field ranges: an ISS
/// pass is 2.76 × 10¹⁰ mm/h, some thirteen times what `i32` holds.
///
/// ```
/// use yodel::units::Speed;
///
/// let speed = Speed::from_knots(36);
/// assert_eq!(speed.mph(), 41);
/// assert_eq!(speed.kmh(), 67);
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct Speed {
    /// The speed in millimeters per hour.
    millimeters_per_hour: i64,
}

impl Speed {
    /// A zero speed.
    pub const ZERO: Self = Self {
        millimeters_per_hour: 0,
    };

    /// A speed in whole knots (nautical miles per hour).
    #[must_use]
    pub const fn from_knots(knots: i32) -> Self {
        Self {
            millimeters_per_hour: (knots as i64).saturating_mul(MMH_PER_KNOT),
        }
    }

    /// A speed in whole miles per hour.
    #[must_use]
    pub const fn from_mph(mph: i32) -> Self {
        Self {
            millimeters_per_hour: (mph as i64).saturating_mul(MMH_PER_MPH),
        }
    }

    /// A speed in whole kilometers per hour.
    #[must_use]
    pub const fn from_kmh(kmh: i32) -> Self {
        Self {
            millimeters_per_hour: (kmh as i64).saturating_mul(MMH_PER_KMH),
        }
    }

    /// A speed in whole meters per second.
    ///
    /// The unit an anemometer reports in, and the reason this type
    /// exists for a weather station: the APRS wire field is miles per
    /// hour, so every station transmitting a wind speed used to convert
    /// by hand.
    #[must_use]
    pub const fn from_meters_per_second(meters_per_second: i32) -> Self {
        Self {
            millimeters_per_hour: (meters_per_second as i64).saturating_mul(MMH_PER_MS),
        }
    }

    /// A speed in millimeters per second.
    ///
    /// Whole meters per second is a coarse step for a light breeze —
    /// 2.5 m/s would have to round to 2 or 3 — so a sensor with better
    /// resolution should come in through this constructor.
    #[must_use]
    pub const fn from_millimeters_per_second(millimeters_per_second: i64) -> Self {
        Self {
            millimeters_per_hour: millimeters_per_second.saturating_mul(MMH_PER_MMS),
        }
    }

    /// A speed in millimeters per hour, the canonical unit.
    #[must_use]
    pub const fn from_millimeters_per_hour(millimeters_per_hour: i64) -> Self {
        Self {
            millimeters_per_hour,
        }
    }

    /// The speed in whole knots, rounded half away from zero.
    #[must_use]
    pub const fn knots(self) -> i32 {
        narrow(div_round(self.millimeters_per_hour, MMH_PER_KNOT))
    }

    /// The speed in whole miles per hour, rounded half away from zero.
    #[must_use]
    pub const fn mph(self) -> i32 {
        narrow(div_round(self.millimeters_per_hour, MMH_PER_MPH))
    }

    /// The speed in whole kilometers per hour, rounded half away from
    /// zero.
    #[must_use]
    pub const fn kmh(self) -> i32 {
        narrow(div_round(self.millimeters_per_hour, MMH_PER_KMH))
    }

    /// The speed in whole meters per second, rounded half away from
    /// zero.
    #[must_use]
    pub const fn meters_per_second(self) -> i32 {
        narrow(div_round(self.millimeters_per_hour, MMH_PER_MS))
    }

    /// The speed in millimeters per second, rounded half away from zero.
    #[must_use]
    pub const fn millimeters_per_second(self) -> i64 {
        div_round(self.millimeters_per_hour, MMH_PER_MMS)
    }

    /// The speed in millimeters per hour, the canonical unit (exact).
    #[must_use]
    pub const fn millimeters_per_hour(self) -> i64 {
        self.millimeters_per_hour
    }
}

/// A rainfall depth, stored as `i64` micrometers.
///
/// The same canonical unit as [`Distance`] but a **separate type**, on
/// purpose. Comparing a rainfall depth with an altitude is meaningless,
/// and APRS position reports carry both — a weather report has three
/// rain fields sitting a few bytes from a `/A=` altitude, and
/// `Option<Rainfall>` versus `Option<Distance>` is what stops them being
/// interchanged.
///
/// ```
/// use yodel::units::Rainfall;
///
/// // The APRS `r`/`p`/`P` fields are hundredths of an inch.
/// let rain = Rainfall::from_hundredths_inch(254);
/// assert_eq!(rain.millimeters(), 65);
/// assert_eq!(rain.hundredths_inch(), 254);
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct Rainfall {
    /// The depth in micrometers.
    micrometers: i64,
}

impl Rainfall {
    /// No rainfall.
    pub const ZERO: Self = Self { micrometers: 0 };

    /// A depth in hundredths of an inch, the APRS wire unit.
    #[must_use]
    pub const fn from_hundredths_inch(hundredths: i32) -> Self {
        Self {
            micrometers: (hundredths as i64).saturating_mul(UM_PER_HUNDREDTH_INCH),
        }
    }

    /// A depth in whole millimeters.
    #[must_use]
    pub const fn from_millimeters(millimeters: i32) -> Self {
        Self {
            micrometers: (millimeters as i64).saturating_mul(UM_PER_MILLIMETER),
        }
    }

    /// A depth in micrometers, the canonical unit.
    #[must_use]
    pub const fn from_micrometers(micrometers: i64) -> Self {
        Self { micrometers }
    }

    /// The depth in hundredths of an inch, rounded half away from zero.
    #[must_use]
    pub const fn hundredths_inch(self) -> i32 {
        narrow(div_round(self.micrometers, UM_PER_HUNDREDTH_INCH))
    }

    /// The depth in whole millimeters, rounded half away from zero.
    #[must_use]
    pub const fn millimeters(self) -> i32 {
        narrow(div_round(self.micrometers, UM_PER_MILLIMETER))
    }

    /// The depth in micrometers, the canonical unit (exact).
    #[must_use]
    pub const fn micrometers(self) -> i64 {
        self.micrometers
    }
}

/// A temperature, stored as `i64` forty-five-thousandths of a degree
/// Celsius.
///
/// # Why that unit
///
/// Celsius and Fahrenheit are related by an **affine** map with a 5/9
/// factor, so no integer unit is exact for both — millidegrees Celsius,
/// the obvious choice, cannot represent 1 °F. 1/45 000 °C is the
/// **smallest** unit that makes whole degrees Celsius, whole degrees
/// Fahrenheit *and* millidegrees Celsius all exact, because 45 000 is
/// divisible by 9 (one °F step is exactly 25 000 units) and by 1000.
///
/// That matters for more than tidiness: the APRS `t` field is degrees
/// Fahrenheit, so an inexact unit would make parse → build lose a
/// degree, which is the crate's byte-exactness invariant broken by
/// arithmetic.
///
/// ```
/// use yodel::units::Temperature;
///
/// // A BME280 reads Celsius; the APRS `t` field wants Fahrenheit.
/// assert_eq!(Temperature::from_celsius(100).fahrenheit(), 212);
/// assert_eq!(Temperature::from_fahrenheit(32).celsius(), 0);
///
/// // Every whole degree Fahrenheit survives the round trip exactly.
/// assert_eq!(Temperature::from_fahrenheit(72).fahrenheit(), 72);
/// ```
///
/// Note that the *cross*-scale reading rounds, as everywhere else in
/// this module: 21 °C is 69.8 °F and reads as 70.
///
/// # No arithmetic, and no `Default`
///
/// Temperature is an **interval** scale, not a ratio scale: its two
/// user-facing units disagree about where zero is, so unlike every
/// other quantity here it has no meaningful origin. `21 °C + 21 °C`
/// is not 42 °C in any sense a reader would expect (it is 42 °C on one
/// scale and 102 °F on the other, which are different temperatures),
/// and negating a temperature is meaningless. Subtraction *is*
/// meaningful but yields a temperature **difference**, which is a
/// different kind of thing again.
///
/// Rather than model that distinction with a second type nothing in
/// this crate needs, `Temperature` simply implements none of them.
/// Comparison is kept, because ordering temperatures is meaningful, and
/// callers wanting a difference can subtract two same-unit readings.
/// [`Self::FREEZING`] stands in for the `Default` that would otherwise
/// silently mean "0 °C".
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Temperature {
    /// The temperature in 1/45 000 °C.
    units: i64,
}

impl Temperature {
    /// The freezing point of water: 0 °C, 32 °F.
    pub const FREEZING: Self = Self { units: 0 };

    /// A temperature in whole degrees Celsius.
    #[must_use]
    pub const fn from_celsius(celsius: i32) -> Self {
        Self {
            units: (celsius as i64).saturating_mul(UNITS_PER_CELSIUS),
        }
    }

    /// A temperature in whole degrees Fahrenheit.
    #[must_use]
    pub const fn from_fahrenheit(fahrenheit: i32) -> Self {
        Self {
            units: (fahrenheit as i64)
                .saturating_sub(FAHRENHEIT_OFFSET)
                .saturating_mul(UNITS_PER_FAHRENHEIT),
        }
    }

    /// A temperature in millidegrees Celsius.
    #[must_use]
    pub const fn from_millidegrees_celsius(millidegrees: i32) -> Self {
        Self {
            units: (millidegrees as i64).saturating_mul(UNITS_PER_MILLICELSIUS),
        }
    }

    /// A temperature in **tenths** of a degree Fahrenheit.
    ///
    /// Exact, like the whole-degree form: a tenth of a degree
    /// Fahrenheit is 2500 canonical units. This is the unit several
    /// weather-station wire formats report in (the Peet Bros Ultimeter
    /// among them), and rounding it to whole degrees on the way in
    /// throws away half a degree for no reason — the wire field that
    /// eventually carries it can round at the point it is written.
    ///
    /// ```
    /// use yodel::units::Temperature;
    ///
    /// assert_eq!(Temperature::from_tenths_fahrenheit(725).fahrenheit(), 73);
    /// assert_eq!(Temperature::from_tenths_fahrenheit(320), Temperature::FREEZING);
    /// ```
    #[must_use]
    pub const fn from_tenths_fahrenheit(tenths: i32) -> Self {
        Self {
            units: (tenths as i64)
                .saturating_sub(FAHRENHEIT_OFFSET * 10)
                .saturating_mul(UNITS_PER_FAHRENHEIT / 10),
        }
    }

    /// The temperature in whole degrees Celsius, rounded half away from
    /// zero.
    #[must_use]
    pub const fn celsius(self) -> i32 {
        narrow(div_round(self.units, UNITS_PER_CELSIUS))
    }

    /// The temperature in whole degrees Fahrenheit, rounded half away
    /// from zero.
    ///
    /// Note the order: the offset is applied **before** the division,
    /// not after. Doing it the other way rounds the distance from the
    /// freezing point rather than the temperature, so 0.5 °F came back
    /// as 0 and 31.5 °F as 31 — away from zero on a scale whose zero
    /// is 32 degrees off. Nothing round-tripped wrong, which is why it
    /// survived: `from_fahrenheit(n).fahrenheit() == n` holds either
    /// way, and only a value that is not a whole number of degrees to
    /// begin with can tell the difference.
    #[must_use]
    pub const fn fahrenheit(self) -> i32 {
        narrow(div_round(
            self.units
                .saturating_add(FAHRENHEIT_OFFSET * UNITS_PER_FAHRENHEIT),
            UNITS_PER_FAHRENHEIT,
        ))
    }

    /// The temperature in millidegrees Celsius, rounded half away from
    /// zero.
    #[must_use]
    pub const fn millidegrees_celsius(self) -> i32 {
        narrow(div_round(self.units, UNITS_PER_MILLICELSIUS))
    }

    /// The temperature in tenths of a degree Fahrenheit, rounded half
    /// away from zero. See [`Self::fahrenheit`] for the ordering.
    #[must_use]
    pub const fn tenths_fahrenheit(self) -> i32 {
        narrow(div_round(
            self.units
                .saturating_add(FAHRENHEIT_OFFSET * UNITS_PER_FAHRENHEIT),
            UNITS_PER_FAHRENHEIT / 10,
        ))
    }
}

/// A pressure, stored as `i64` millipascals.
///
/// Millipascals make the APRS wire unit — tenths of a hectopascal —
/// exact, at 10 000 mPa. The full APRS field range (0..=9999.9 hPa) uses
/// under 10⁹ of the available 9.2 × 10¹⁸.
///
/// ```
/// use yodel::units::Pressure;
///
/// // The APRS `b` field is tenths of a hectopascal.
/// let slp = Pressure::from_tenths_hpa(10132);
/// assert_eq!(slp.hpa(), 1013);
/// assert_eq!(slp.pascals(), 101_320);
/// // ...and the same reading in the unit a US barometer shows.
/// assert_eq!(slp.hundredths_inhg(), 2992); // 29.92 inHg
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct Pressure {
    /// The pressure in millipascals.
    millipascals: i64,
}

impl Pressure {
    /// Zero pressure.
    pub const ZERO: Self = Self { millipascals: 0 };

    /// A pressure in whole pascals.
    #[must_use]
    pub const fn from_pascals(pascals: i32) -> Self {
        Self {
            millipascals: (pascals as i64).saturating_mul(MPA_PER_PASCAL),
        }
    }

    /// A pressure in whole hectopascals (millibars).
    #[must_use]
    pub const fn from_hpa(hpa: i32) -> Self {
        Self {
            millipascals: (hpa as i64).saturating_mul(MPA_PER_HPA),
        }
    }

    /// A pressure in tenths of a hectopascal, the APRS wire unit.
    #[must_use]
    pub const fn from_tenths_hpa(tenths: i32) -> Self {
        Self {
            millipascals: (tenths as i64).saturating_mul(MPA_PER_TENTH_HPA),
        }
    }

    /// A pressure in hundredths of an inch of mercury.
    #[must_use]
    pub const fn from_hundredths_inhg(hundredths: i32) -> Self {
        Self {
            millipascals: div_round((hundredths as i64).saturating_mul(MPA_PER_INHG), 100),
        }
    }

    /// A pressure in millipascals, the canonical unit.
    #[must_use]
    pub const fn from_millipascals(millipascals: i64) -> Self {
        Self { millipascals }
    }

    /// The pressure in whole pascals, rounded half away from zero.
    #[must_use]
    pub const fn pascals(self) -> i32 {
        narrow(div_round(self.millipascals, MPA_PER_PASCAL))
    }

    /// The pressure in whole hectopascals, rounded half away from zero.
    #[must_use]
    pub const fn hpa(self) -> i32 {
        narrow(div_round(self.millipascals, MPA_PER_HPA))
    }

    /// The pressure in tenths of a hectopascal, rounded half away from
    /// zero.
    #[must_use]
    pub const fn tenths_hpa(self) -> i32 {
        narrow(div_round(self.millipascals, MPA_PER_TENTH_HPA))
    }

    /// The pressure in hundredths of an inch of mercury, rounded half
    /// away from zero.
    #[must_use]
    pub const fn hundredths_inhg(self) -> i32 {
        narrow(div_round(
            self.millipascals.saturating_mul(100),
            MPA_PER_INHG,
        ))
    }

    /// The pressure in millipascals, the canonical unit (exact).
    #[must_use]
    pub const fn millipascals(self) -> i64 {
        self.millipascals
    }
}

/// A transmitter power, stored as `i64` microwatts.
///
/// ```
/// use yodel::units::Power;
///
/// // APRS PHG encodes power as the square of a digit, in watts.
/// assert_eq!(Power::from_watts(49).watts(), 49);
/// assert_eq!(Power::from_milliwatts(2500).watts(), 3); // rounds
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct Power {
    /// The power in microwatts.
    microwatts: i64,
}

impl Power {
    /// Zero power.
    pub const ZERO: Self = Self { microwatts: 0 };

    /// A power in whole watts.
    #[must_use]
    pub const fn from_watts(watts: i32) -> Self {
        Self {
            microwatts: (watts as i64).saturating_mul(UW_PER_WATT),
        }
    }

    /// A power in whole milliwatts.
    #[must_use]
    pub const fn from_milliwatts(milliwatts: i32) -> Self {
        Self {
            microwatts: (milliwatts as i64).saturating_mul(UW_PER_MILLIWATT),
        }
    }

    /// A power in microwatts, the canonical unit.
    #[must_use]
    pub const fn from_microwatts(microwatts: i64) -> Self {
        Self { microwatts }
    }

    /// A power in whole decibel-milliwatts.
    ///
    /// The unit hams state transmitter power in on the weak-signal
    /// modes: 0 dBm is 1 mW, 30 dBm is 1 W, 37 dBm is 5 W, 60 dBm is
    /// 1 kW.
    ///
    /// **This is the one conversion in this module that is not exact**,
    /// because the decibel is logarithmic. It is evaluated as whole
    /// decades times a ten-entry mantissa table, giving seven
    /// significant figures — far finer than any transmitter is
    /// calibrated to — and [`Self::dbm`] recovers every whole dBm figure
    /// at or above **-24 dBm** (4 µW). Below that the microwatt storage
    /// floor is reached and neighbouring dBm figures become
    /// indistinguishable: -30 and -29 dBm both store 1 µW. No
    /// transmitter operates there — 0 dBm is a milliwatt — so the floor
    /// is stated rather than worked around.
    #[must_use]
    pub const fn from_dbm(dbm: i32) -> Self {
        // 0 dBm is 1 mW is 1000 µW, so shifting by 30 puts the whole
        // scale in microwatts: µW = 10^((dbm + 30) / 10).
        let shifted = (dbm as i64).saturating_add(30);
        let decade = shifted.div_euclid(10);
        let mantissa = DBM_MANTISSA[shifted.rem_euclid(10) as usize];
        // The table is scaled by 10^6, which the decade must undo.
        let microwatts = if decade >= 6 {
            let shift = decade - 6;
            if shift as usize >= POW10.len() {
                i64::MAX
            } else {
                mantissa.saturating_mul(POW10[shift as usize])
            }
        } else {
            let shift = 6 - decade;
            if shift as usize >= POW10.len() {
                0
            } else {
                div_round(mantissa, POW10[shift as usize])
            }
        };
        Self { microwatts }
    }

    /// The power in whole decibel-milliwatts, or `None` for zero or
    /// negative power.
    ///
    /// Zero watts is negative infinity on a logarithmic scale, so there
    /// is no integer to return; saying so with `Option` is more accurate
    /// than inventing a sentinel. Every value produced by
    /// [`Self::from_dbm`] at or above -24 dBm comes back unchanged; see
    /// that method for the floor below it.
    #[must_use]
    pub const fn dbm(self) -> Option<i32> {
        if self.microwatts <= 0 {
            return None;
        }
        // Pick the candidate whose power is nearest in the LOG domain,
        // which is the geometric rather than arithmetic midpoint: take
        // the first d with `microwatts^2 <= P(d) * P(d + 1)`. i128
        // because that product reaches 10^38 at the top of the range.
        let squared = (self.microwatts as i128) * (self.microwatts as i128);
        let mut candidate = DBM_MIN;
        let mut power = Self::from_dbm(candidate).microwatts as i128;
        while candidate < DBM_MAX {
            let next = Self::from_dbm(candidate + 1).microwatts as i128;
            if squared <= power * next {
                return Some(candidate);
            }
            candidate += 1;
            power = next;
        }
        Some(DBM_MAX)
    }

    /// The power in whole watts, rounded half away from zero.
    #[must_use]
    pub const fn watts(self) -> i32 {
        narrow(div_round(self.microwatts, UW_PER_WATT))
    }

    /// The power in whole milliwatts, rounded half away from zero.
    #[must_use]
    pub const fn milliwatts(self) -> i32 {
        narrow(div_round(self.microwatts, UW_PER_MILLIWATT))
    }

    /// The power in microwatts, the canonical unit (exact).
    #[must_use]
    pub const fn microwatts(self) -> i64 {
        self.microwatts
    }
}

/// A compass bearing in whole degrees, `0..=359`.
///
/// Validated rather than widened, because a bearing is an *enumerable*
/// range: 400 degrees is not a larger bearing, it is a different one, so
/// there is nothing for extra width to hold.
///
/// ```
/// use yodel::units::{Bearing, CompassPoint};
///
/// let heading = Bearing::new(88)?;
/// assert_eq!(heading.compass_point(), CompassPoint::East);
/// assert_eq!(heading.reciprocal().degrees(), 268);
///
/// // 360 is the same direction as 0, and is accepted as such.
/// assert_eq!(Bearing::new(360)?.degrees(), 0);
/// # Ok::<(), yodel::units::UnitError>(())
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct Bearing {
    /// The bearing in degrees, `0..=359`.
    degrees: u16,
}

impl Bearing {
    /// Due north, 0 degrees.
    pub const NORTH: Self = Self { degrees: 0 };

    /// A bearing in whole degrees.
    ///
    /// 360 is accepted and folded to 0: APRS wire fields spell due north
    /// as `360` (a `000` course means "unknown"), so rejecting it would
    /// reject legal traffic.
    ///
    /// # Errors
    ///
    /// [`UnitError::BadBearing`] above 360.
    pub const fn new(degrees: u16) -> Result<Self, UnitError> {
        if degrees > 360 {
            Err(UnitError::BadBearing { got: degrees })
        } else if degrees == 360 {
            Ok(Self { degrees: 0 })
        } else {
            Ok(Self { degrees })
        }
    }

    /// The bearing in whole degrees, `0..=359`.
    #[must_use]
    pub const fn degrees(self) -> u16 {
        self.degrees
    }

    /// The nearest of the sixteen named compass points.
    ///
    /// Boundaries fall halfway between adjacent points, 11.25 degrees
    /// apart, so 11 degrees is still north and 12 is north-northeast.
    #[must_use]
    pub const fn compass_point(self) -> CompassPoint {
        // (degrees * 16 + 180) / 360 rounds to the nearest sixteenth of
        // a turn; the modulo folds the wrap past 348.75 back to north.
        let index = ((self.degrees as u32 * 16 + 180) / 360) % 16;
        CompassPoint::ALL[index as usize]
    }

    /// The reciprocal bearing, 180 degrees opposed.
    #[must_use]
    pub const fn reciprocal(self) -> Self {
        Self {
            degrees: (self.degrees + 180) % 360,
        }
    }
}

/// One of the sixteen named points of the compass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub enum CompassPoint {
    /// North, 0 degrees.
    #[default]
    North,
    /// North-northeast, 22.5 degrees.
    NorthNortheast,
    /// Northeast, 45 degrees.
    Northeast,
    /// East-northeast, 67.5 degrees.
    EastNortheast,
    /// East, 90 degrees.
    East,
    /// East-southeast, 112.5 degrees.
    EastSoutheast,
    /// Southeast, 135 degrees.
    Southeast,
    /// South-southeast, 157.5 degrees.
    SouthSoutheast,
    /// South, 180 degrees.
    South,
    /// South-southwest, 202.5 degrees.
    SouthSouthwest,
    /// Southwest, 225 degrees.
    Southwest,
    /// West-southwest, 247.5 degrees.
    WestSouthwest,
    /// West, 270 degrees.
    West,
    /// West-northwest, 292.5 degrees.
    WestNorthwest,
    /// Northwest, 315 degrees.
    Northwest,
    /// North-northwest, 337.5 degrees.
    NorthNorthwest,
}

impl CompassPoint {
    /// All sixteen points, in clockwise order from north.
    pub const ALL: [Self; 16] = [
        Self::North,
        Self::NorthNortheast,
        Self::Northeast,
        Self::EastNortheast,
        Self::East,
        Self::EastSoutheast,
        Self::Southeast,
        Self::SouthSoutheast,
        Self::South,
        Self::SouthSouthwest,
        Self::Southwest,
        Self::WestSouthwest,
        Self::West,
        Self::WestNorthwest,
        Self::Northwest,
        Self::NorthNorthwest,
    ];

    /// The conventional abbreviation, e.g. `"NNE"`.
    #[must_use]
    pub const fn abbreviation(self) -> &'static str {
        match self {
            Self::North => "N",
            Self::NorthNortheast => "NNE",
            Self::Northeast => "NE",
            Self::EastNortheast => "ENE",
            Self::East => "E",
            Self::EastSoutheast => "ESE",
            Self::Southeast => "SE",
            Self::SouthSoutheast => "SSE",
            Self::South => "S",
            Self::SouthSouthwest => "SSW",
            Self::Southwest => "SW",
            Self::WestSouthwest => "WSW",
            Self::West => "W",
            Self::WestNorthwest => "WNW",
            Self::Northwest => "NW",
            Self::NorthNorthwest => "NNW",
        }
    }

    /// The bearing at the centre of this point's sector.
    #[must_use]
    pub const fn bearing(self) -> Bearing {
        let index = self as u16;
        Bearing {
            // 22.5 degrees per point, rounded up to whole degrees. The
            // largest is 15 * 22.5 = 337.5 -> 338, so no wrap is
            // possible and none is applied.
            degrees: (index * 45).div_ceil(2),
        }
    }
}

/// A relative humidity in whole percent, `1..=100`.
///
/// The type exists to absorb one wire quirk: APRS sends 100% humidity as
/// the two characters `00`, because the field is two digits wide. That
/// encoding has bitten every naive parser (it reads as 0%, the driest
/// possible air, rather than the wettest), so it is handled **once**,
/// here, and no caller sees it again.
///
/// ```
/// use yodel::units::Humidity;
///
/// // `h00` on the wire means 100%, not 0%.
/// assert_eq!(Humidity::from_wire_percent(0)?.percent(), 100);
/// assert_eq!(Humidity::new(100)?.wire_percent(), 0);
/// assert_eq!(Humidity::new(55)?.wire_percent(), 55);
/// # Ok::<(), yodel::units::UnitError>(())
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Humidity {
    /// The relative humidity in percent, `1..=100`.
    percent: u8,
}

impl Humidity {
    /// A humidity in whole percent.
    ///
    /// # Errors
    ///
    /// [`UnitError::BadHumidity`] outside `1..=100`. Zero percent
    /// relative humidity does not occur in the atmosphere and is the
    /// wire's spelling of 100%, so it is rejected here rather than
    /// silently reinterpreted — use [`Self::from_wire_percent`] when
    /// decoding.
    pub const fn new(percent: u8) -> Result<Self, UnitError> {
        if percent == 0 || percent > 100 {
            Err(UnitError::BadHumidity { got: percent })
        } else {
            Ok(Self { percent })
        }
    }

    /// A humidity from the two-digit wire field, where `00` means 100%.
    ///
    /// # Errors
    ///
    /// [`UnitError::BadHumidity`] above 100.
    pub const fn from_wire_percent(wire: u8) -> Result<Self, UnitError> {
        if wire == 0 {
            Ok(Self { percent: 100 })
        } else {
            Self::new(wire)
        }
    }

    /// The relative humidity in whole percent, `1..=100`.
    #[must_use]
    pub const fn percent(self) -> u8 {
        self.percent
    }

    /// The value as the two-digit wire field spells it: 100% as `0`.
    #[must_use]
    pub const fn wire_percent(self) -> u8 {
        if self.percent == 100 { 0 } else { self.percent }
    }
}

/// Implements `Debug` printing both unit systems, plus saturating
/// `Add`/`Sub`/`Neg` and `Sum`-free arithmetic within the type.
///
/// The `Debug` output is the point of the macro: a log line or a failing
/// assertion that reads `Distance(376 m / 1234 ft)` explains itself,
/// which a bare canonical integer never does.
macro_rules! quantity_debug {
    ($type:ident, $fmt:literal, $($accessor:ident),+) => {
        impl core::fmt::Debug for $type {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                write!(f, concat!(stringify!($type), "(", $fmt, ")"), $(self.$accessor()),+)
            }
        }
    };
}

/// Implements saturating `Add`/`Sub`/`Neg` for a **ratio**-scale
/// quantity.
///
/// Not applied to [`Temperature`], which is an interval scale; see
/// that type.
macro_rules! quantity_arithmetic {
    ($type:ident, $field:ident) => {
        impl core::ops::Add for $type {
            type Output = Self;
            fn add(self, rhs: Self) -> Self {
                Self {
                    $field: self.$field.saturating_add(rhs.$field),
                }
            }
        }

        impl core::ops::Sub for $type {
            type Output = Self;
            fn sub(self, rhs: Self) -> Self {
                Self {
                    $field: self.$field.saturating_sub(rhs.$field),
                }
            }
        }

        impl core::ops::Neg for $type {
            type Output = Self;
            fn neg(self) -> Self {
                Self {
                    $field: self.$field.saturating_neg(),
                }
            }
        }
    };
}

quantity_debug!(Distance, "{} m / {} ft", meters, feet);
quantity_debug!(Speed, "{} kn / {} mph / {} km/h", knots, mph, kmh);
quantity_debug!(
    Rainfall,
    "{} mm / {} hundredths-inch",
    millimeters,
    hundredths_inch
);
quantity_debug!(Temperature, "{} C / {} F", celsius, fahrenheit);
quantity_debug!(
    Pressure,
    "{} hPa / {} hundredths-inHg",
    hpa,
    hundredths_inhg
);
quantity_debug!(Power, "{} W", watts);

quantity_arithmetic!(Distance, micrometers);
quantity_arithmetic!(Speed, millimeters_per_hour);
quantity_arithmetic!(Rainfall, micrometers);
quantity_arithmetic!(Pressure, millipascals);
quantity_arithmetic!(Power, microwatts);

impl core::fmt::Debug for Bearing {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "Bearing({} deg / {})",
            self.degrees,
            self.compass_point().abbreviation()
        )
    }
}

impl core::fmt::Debug for Humidity {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Humidity({}%)", self.percent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The canonical stored values, which no public accessor can
    /// recover.
    ///
    /// `tests/units.rs` pins every quantity's canonical value except
    /// [`Temperature`]'s, because 1/45 000 °C is finer than any accessor
    /// and the module exposes no raw reader. This is the one place the
    /// field is visible, so this is where the known-answer vectors for
    /// it live — without them a change to `UNITS_PER_CELSIUS` and
    /// `UNITS_PER_FAHRENHEIT` *together* could stay self-consistent and
    /// go unnoticed.
    #[test]
    fn temperature_canonical_values_are_pinned() {
        let cases = [
            (Temperature::from_fahrenheit(-99), -3_275_000),
            (Temperature::from_fahrenheit(32), 0),
            (Temperature::from_fahrenheit(72), 1_000_000),
            (Temperature::from_fahrenheit(999), 24_175_000),
            (Temperature::from_celsius(21), 945_000),
            (Temperature::from_celsius(100), 4_500_000),
            (Temperature::from_millidegrees_celsius(21_400), 963_000),
        ];
        for (temperature, units) in cases {
            assert_eq!(temperature.units, units, "{temperature:?}");
        }
        // 45 000 is divisible by 9 (so a whole °F is exact) and by 1000
        // (so a millidegree is exact). Both are load-bearing.
        assert_eq!(UNITS_PER_CELSIUS % 9, 0);
        assert_eq!(UNITS_PER_CELSIUS % 1000, 0);
        assert_eq!(UNITS_PER_CELSIUS * 5 / 9, UNITS_PER_FAHRENHEIT);
    }

    #[test]
    fn div_round_goes_away_from_zero_on_a_tie() {
        assert_eq!(div_round(5, 10), 1);
        assert_eq!(div_round(-5, 10), -1);
        assert_eq!(div_round(4, 10), 0);
        assert_eq!(div_round(-4, 10), 0);
    }

    #[test]
    fn narrow_saturates_instead_of_wrapping() {
        assert_eq!(narrow(i64::from(i32::MAX) + 1), i32::MAX);
        assert_eq!(narrow(i64::from(i32::MIN) - 1), i32::MIN);
        assert_eq!(narrow(0), 0);
    }

    #[test]
    fn compass_boundaries_land_where_the_sector_edges_are() {
        // Sector edges sit at 11.25 degrees; integer degrees either side.
        assert_eq!(
            Bearing::new(11).unwrap().compass_point(),
            CompassPoint::North
        );
        assert_eq!(
            Bearing::new(12).unwrap().compass_point(),
            CompassPoint::NorthNortheast
        );
        // ...and the wrap back to north past 348.75.
        assert_eq!(
            Bearing::new(348).unwrap().compass_point(),
            CompassPoint::NorthNorthwest
        );
        assert_eq!(
            Bearing::new(349).unwrap().compass_point(),
            CompassPoint::North
        );
    }

    #[test]
    fn every_compass_point_round_trips_through_its_own_bearing() {
        for point in CompassPoint::ALL {
            assert_eq!(point.bearing().compass_point(), point, "{point:?}");
        }
    }

    /// A fixed-capacity `core::fmt::Write` sink, so the `Debug` tests
    /// need neither `alloc` nor `std`.
    struct Buf {
        bytes: [u8; 64],
        len: usize,
    }

    impl Buf {
        fn new() -> Self {
            Self {
                bytes: [0; 64],
                len: 0,
            }
        }

        fn as_str(&self) -> &str {
            core::str::from_utf8(&self.bytes[..self.len]).expect("ascii")
        }
    }

    impl core::fmt::Write for Buf {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            let end = self.len + s.len();
            self.bytes
                .get_mut(self.len..end)
                .ok_or(core::fmt::Error)?
                .copy_from_slice(s.as_bytes());
            self.len = end;
            Ok(())
        }
    }

    fn debug_of(value: &dyn core::fmt::Debug) -> Buf {
        use core::fmt::Write;
        let mut buf = Buf::new();
        write!(&mut buf, "{value:?}").expect("fits");
        buf
    }

    #[test]
    fn debug_output_names_both_unit_systems() {
        // The whole reason Debug is hand-written: a failing assertion
        // has to explain itself without the reader knowing the canonical
        // unit.
        assert_eq!(
            debug_of(&Distance::from_meters(376)).as_str(),
            "Distance(376 m / 1234 ft)"
        );
        assert_eq!(
            debug_of(&Temperature::from_celsius(100)).as_str(),
            "Temperature(100 C / 212 F)"
        );
        assert_eq!(
            debug_of(&Bearing::new(88).expect("valid")).as_str(),
            "Bearing(88 deg / E)"
        );
        assert_eq!(
            debug_of(&Speed::from_knots(36)).as_str(),
            "Speed(36 kn / 41 mph / 67 km/h)"
        );
    }
}
