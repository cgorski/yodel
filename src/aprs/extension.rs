//! APRS data extensions: the 7-byte field between the symbol and the
//! comment.
//!
//! A position report, object or item may carry a **data extension**
//! immediately after the symbol code: course/speed, wind, transmitter
//! capability (`PHG`), pre-calculated range (`RNG`) or omni-DF signal
//! strength (`DFS`). It is not optional decoration — it is where course,
//! speed, wind and antenna capability live.
//!
//! How common: MEASURED over 2182 real off-air frames, of the 516
//! position reports carrying anything after the symbol, **253** begin
//! with a `ddd/sss` extension and **139** with a `PHG` — so **76% of
//! that text was structured data** the crate previously handed back as
//! an opaque comment.
//!
//! # Wire codes are stored, not decoded values
//!
//! [`Phg`] and [`Dfs`] hold the **wire characters**, and expose the
//! decoded physical quantities through methods. That is why the type
//! round-trips:
//!
//! * the wire encodings are not linear — power is the *square* of its
//!   code and height is `10 · 2^code` — so most physical values have no
//!   wire representation at all. A `Phg` storing `power_watts: 5` could
//!   not be serialised, and a builder that silently rounded to 4 W would
//!   be worse than one that cannot represent it;
//! * storing the code makes every representable value exactly
//!   re-serialisable, so parse → write is byte-identical by construction
//!   rather than by careful inverse arithmetic.
//!
//! # Strictness, and why it is the safe direction here
//!
//! Recognition is strict: all-digit fields, exact literal prefixes, and
//! a mandatory `/` terminator on the 9-byte `PHGR` form. A prefix that
//! does not match is simply **not an extension**, and its bytes stay in
//! the comment where [`DataExtension::write`] reproduces them.
//!
//! That asymmetry is the whole point. A false negative costs a missed
//! decode; a false positive silently eats seven bytes of someone's
//! comment and can invent a speed. A well-known independent
//! implementation dispatches course/speed on "byte 3 is a slash" with no
//! digit check at all, and consequently decodes the comment `Hwy/101
//! north of town` as a course of 101 knots while destroying the text.
//! Comments like `Hwy/101` and `KG6/W6ABC` are entirely plausible, so
//! that is not a hypothetical.
//!
//! # Provenance
//!
//! Field layouts and the power/height/gain/directivity code tables are
//! from the **UNOFFICIAL APRS Protocol Reference, Document Version
//! Draft 1.2 c** (date of issue November 2024), chapter 7 "APRS Data
//! Extensions". `PHGR` is a 1.2 addition, defined in the same chapter
//! under 'PHGR "probes"'.
//!
//! On why that document and not `APRS101.pdf`: the 2000-vintage 1.0.1
//! reference is the only formally *approved* edition, but its own
//! publisher now ships it as a one-page notice saying it is obsolete and
//! that implementing from it "is likely to produce something
//! incompatible with contemporary practices". The versioning mess is
//! laid out in `docs/APRS_CONFORMANCE.md` §1.

use crate::geo::UNITS_PER_DEGREE;

use super::AprsError;

use super::symbol::Symbol;

/// The APRS weather symbol code. A `ddd/sss` extension following this
/// symbol is **wind direction and speed**, not course and speed (1.2
/// ch. 12: "the Symbol Code in every case is the `_` (underscore) …
/// the 7-byte Wind Direction and Wind Speed Data Extension replace the
/// cccc and ssss fields").
const WEATHER_SYMBOL_CODE: u8 = b'_';

/// A `ddd/sss` field pair, either of which may be "unknown".
///
/// The specification states the meaningful range as `001-360` for a
/// bearing, so `000` is outside the domain and reads as unknown here
/// alongside the `...` and three-space spellings. `None` therefore
/// means *the station said it does not know*, which is different
/// information from a bearing of zero, and different again from the
/// field being absent.
///
/// The paired [`Speed`] does **not** share that rule; see its docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bearing {
    degrees: Option<u16>,
    /// The exact bytes received, so `write` reproduces whichever of the
    /// three unknown spellings arrived.
    wire: [u8; 3],
}

impl Bearing {
    /// Degrees true, `1..=360`, or `None` if the station reported the
    /// value as unknown.
    #[must_use]
    pub const fn degrees(&self) -> Option<u16> {
        self.degrees
    }
}

/// A speed in knots, which may be "unknown".
///
/// **`000` on its own is a speed of zero, not an unknown speed.** The
/// specification gives the unknown sentinel for the *pair* — "if the
/// course **and** speed are unknown or not relevant, they can be set to
/// `000/000` or `.../...` or `␣␣␣/␣␣␣`" — and nothing excludes zero
/// from the speed domain. `315/000` is a stationary tracker saying it
/// is stationary, which is information; discarding it would report the
/// same thing as a tracker with no speed sensor at all.
///
/// MEASURED: 18 corpus frames report a real course beside a zero speed
/// (`194/000`, `315/000`, `035/000`) against only 2 that are the
/// `000/000` sentinel pair; reading zero as a speed closed 16 of them
/// against the independent decoder (its `speed` coverage gap fell from
/// 26 to 10) with no new disagreement.
///
/// Reading the two fields independently also contradicted this crate's
/// own weather decoder, which has always read the `DDD/SSS` wind
/// extension of a Complete Weather Report as calm rather than unknown
/// — 48 corpus frames carry `ddd/000` under the `_` symbol, and the
/// same seven bytes meant two different things depending on which path
/// reached them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Speed {
    knots: Option<u16>,
    wire: [u8; 3],
}

impl Speed {
    /// Speed in knots, or `None` if reported as unknown.
    #[must_use]
    pub const fn knots(&self) -> Option<u16> {
        self.knots
    }
}

/// Parses one three-byte `ddd` field, accepting the unknown spellings.
///
/// Only `...` and three spaces are unknown *here*: `000` is a numeric
/// zero, because whether zero means "unknown" depends on which half of
/// the pair it is in and on what the other half says. That decision
/// belongs to [`DataExtension::parse`], which can see both fields.
fn field3(b: &[u8]) -> Option<(Option<u16>, [u8; 3])> {
    let wire = [b[0], b[1], b[2]];
    if b == b"..." || b == b"   " {
        return Some((None, wire));
    }
    if !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let v = u16::from(b[0] - b'0') * 100 + u16::from(b[1] - b'0') * 10 + u16::from(b[2] - b'0');
    Some((Some(v), wire))
}

/// The `000` spelling, which means unknown only as a whole pair.
const ZERO_TRIPLE: [u8; 3] = *b"000";

/// Transmitter capability: `PHGphgd`, or `PHGphgdr/` in the 1.2 form.
///
/// Fields are the wire codes; see the [module docs](self) for why, and
/// use the accessors for physical quantities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Phg {
    power: u8,
    /// Height code as an offset from `'0'`. **Not** limited to 9: the
    /// specification says the code "may in fact be any ASCII character
    /// 0–9 and above … so that larger heights for balloons, aircraft or
    /// satellites may be specified", giving `:` = 10240 ft.
    height: u8,
    gain: u8,
    directivity: u8,
    rate: Option<PhgRate>,
}

/// The `PHGR` beacon rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhgRate {
    /// Beacons per hour, from wire `1`..=`9` then `A`..=`Z` (`A` = 10).
    PerHour(u8),
    /// Wire `0`: the packet was sent **outside** its normal schedule
    /// (typically answering a query), and must be excluded from
    /// reliability statistics. Not a rate of zero.
    Unscheduled,
}

/// Largest height code whose `10 << code` fits `u32`.
///
/// The specification places no upper bound on the height character, but
/// arithmetic does: code 28 is already 2.7 billion feet, which is well
/// past any balloon, aircraft or satellite.
pub const MAX_HEIGHT_CODE: u8 = 28;

impl Phg {
    /// Builds a `PHG` from its four wire codes.
    ///
    /// `power` and `gain` are `0..=9`. `height` may exceed 9 (see the
    /// field docs). `directivity` is `0..=9`, where 0 is
    /// omnidirectional and 9 is unassigned by the specification and
    /// reported as an unknown direction.
    ///
    /// # Errors
    ///
    /// [`AprsError::BadDigit`] if `power` or `gain` exceeds 9, or if
    /// `height` exceeds [`MAX_HEIGHT_CODE`].
    pub const fn new(power: u8, height: u8, gain: u8, directivity: u8) -> Result<Self, AprsError> {
        if power > 9 {
            return Err(AprsError::BadDigit {
                got: b'0'.wrapping_add(power),
                position: 0,
            });
        }
        if gain > 9 {
            return Err(AprsError::BadDigit {
                got: b'0'.wrapping_add(gain),
                position: 2,
            });
        }
        if height > MAX_HEIGHT_CODE {
            return Err(AprsError::BadDigit {
                got: b'0'.wrapping_add(height),
                position: 1,
            });
        }
        Ok(Self {
            power,
            height,
            gain,
            directivity,
            rate: None,
        })
    }

    /// Adds the 1.2 beacon rate, making this the 9-byte `PHGR` form.
    #[must_use]
    pub const fn with_rate(self, rate: PhgRate) -> Self {
        Self {
            rate: Some(rate),
            ..self
        }
    }

    /// Transmitter power in watts: the code squared, so `0..=9` maps to
    /// 0, 1, 4, 9, 16, 25, 36, 49, 64, 81 W.
    #[must_use]
    pub const fn power_watts(&self) -> u16 {
        let d = self.power as u16;
        d * d
    }

    /// Antenna height above average terrain in feet: `10 · 2^code`, so
    /// `0..=9` maps to 10 ft through 5120 ft and `:` (code 10) to
    /// 10240 ft.
    #[must_use]
    pub const fn height_feet(&self) -> u32 {
        10u32 << self.height
    }

    /// Antenna gain in **dBi** — the code directly.
    ///
    /// The 1.2 errata are explicit that the PHG table's gain column is
    /// dBi (the `DFS` table's is dB); the code-to-value mapping is the
    /// same in both.
    #[must_use]
    pub const fn gain_dbi(&self) -> u8 {
        self.gain
    }

    /// Main-lobe direction in degrees true, or `None` for an
    /// omnidirectional antenna (code `0`) or an unassigned code.
    #[must_use]
    pub const fn directivity_degrees(&self) -> Option<u16> {
        match self.directivity {
            1..=8 => Some(self.directivity as u16 * 45),
            _ => None,
        }
    }

    /// The `PHGR` beacon rate, or `None` for the 7-byte `PHG` form.
    #[must_use]
    pub const fn rate(&self) -> Option<PhgRate> {
        self.rate
    }
}

/// Omni-DF signal strength: `DFSshgd`.
///
/// Same shape as [`Phg`] with a received signal strength in place of a
/// transmitter power. Fields are wire codes; use the accessors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dfs {
    strength: u8,
    height: u8,
    gain: u8,
    directivity: u8,
}

impl Dfs {
    /// Builds a `DFS` from its four wire codes.
    ///
    /// # Errors
    ///
    /// [`AprsError::BadDigit`] if `strength` or `gain` exceeds 9, or if
    /// `height` exceeds [`MAX_HEIGHT_CODE`].
    pub const fn new(
        strength: u8,
        height: u8,
        gain: u8,
        directivity: u8,
    ) -> Result<Self, AprsError> {
        if strength > 9 {
            return Err(AprsError::BadDigit {
                got: b'0'.wrapping_add(strength),
                position: 0,
            });
        }
        if gain > 9 {
            return Err(AprsError::BadDigit {
                got: b'0'.wrapping_add(gain),
                position: 2,
            });
        }
        if height > MAX_HEIGHT_CODE {
            return Err(AprsError::BadDigit {
                got: b'0'.wrapping_add(height),
                position: 1,
            });
        }
        Ok(Self {
            strength,
            height,
            gain,
            directivity,
        })
    }

    /// Received signal strength in S-points, `0..=9`.
    ///
    /// Zero is the *most* significant value, not a missing reading: the
    /// specification notes APRS uses zero-strength reports "to draw
    /// (usually black) circles where the jammer is not heard".
    #[must_use]
    pub const fn strength_s_points(&self) -> u8 {
        self.strength
    }

    /// Antenna height above average terrain in feet: `10 · 2^code`.
    #[must_use]
    pub const fn height_feet(&self) -> u32 {
        10u32 << self.height
    }

    /// Antenna gain in dB.
    #[must_use]
    pub const fn gain_db(&self) -> u8 {
        self.gain
    }

    /// Main-lobe direction in degrees true, or `None` for omni or an
    /// unassigned code.
    #[must_use]
    pub const fn directivity_degrees(&self) -> Option<u16> {
        match self.directivity {
            1..=8 => Some(self.directivity as u16 * 45),
            _ => None,
        }
    }
}

/// One of the APRS data extensions.
///
/// `#[non_exhaustive]`: the specification defines further extensions
/// (area-object descriptors, the DF `/BRG/NRQ` follow-on), and adding
/// one should not be a breaking release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DataExtension {
    /// `CSE/SPD` — course over ground and speed.
    CourseSpeed {
        /// Course in degrees true.
        course: Bearing,
        /// Speed over ground.
        speed: Speed,
    },
    /// `DIR/SPD` — wind direction and sustained one-minute wind speed.
    ///
    /// Byte-identical on the wire to [`Self::CourseSpeed`]; the two are
    /// distinguished **only** by the symbol code being `_` (weather).
    /// MEASURED: 26% of `ddd/sss` extensions in the corpus are wind, so
    /// a parser that ignores the symbol mislabels a quarter of them.
    Wind {
        /// Wind direction in degrees true.
        direction: Bearing,
        /// Sustained one-minute wind speed, in knots (1.2 ch. 7).
        speed: Speed,
    },
    /// `PHGphgd` / `PHGphgdr/` — transmitter capability.
    Phg(Phg),
    /// `RNGrrrr` — pre-calculated omnidirectional range in statute
    /// miles, `0..=9999`.
    Range {
        /// Radio range in statute miles.
        miles: u16,
    },
    /// `DFSshgd` — omni-DF signal strength.
    Dfs(Dfs),
}

impl DataExtension {
    /// Wire length of every extension except the `PHGR` form.
    pub const LEN: usize = 7;

    /// Wire length of the `PHGR` form: `PHG` + four codes + rate + the
    /// mandatory `/` terminator.
    ///
    /// The specification says outright that this "violates the rule that
    /// Data Extensions are always 7 characters", so fixed-width scanning
    /// desynchronises on it. Use [`Self::wire_len`].
    pub const LEN_PHGR: usize = 9;

    /// Bytes this extension occupies on the wire.
    #[must_use]
    pub const fn wire_len(&self) -> usize {
        match self {
            Self::Phg(p) if p.rate.is_some() => Self::LEN_PHGR,
            _ => Self::LEN,
        }
    }

    /// Parses a data extension from the start of `bytes`, given the
    /// report's `symbol`.
    ///
    /// The symbol is required, not optional: a `ddd/sss` field is course
    /// and speed for every symbol except the weather symbol `_`, where
    /// the identical bytes are wind direction and speed.
    ///
    /// Returns `None` when `bytes` does not begin with an extension —
    /// the common case for a plain free-text comment. Infallible: an
    /// unrecognised prefix is not an error, because the field is optional
    /// and what follows the symbol is otherwise arbitrary text.
    #[must_use]
    pub fn parse(bytes: &[u8], symbol: Symbol) -> Option<Self> {
        if bytes.len() < Self::LEN {
            return None;
        }
        // ddd/sss — course/speed, or wind for the weather symbol.
        if bytes[3] == b'/' {
            let (a, aw) = field3(&bytes[..3])?;
            let (b, bw) = field3(&bytes[4..7])?;
            // A bearing above 360 is not a bearing, so the bytes are not
            // an extension and stay comment text.
            if a.is_some_and(|v| v > 360) {
                return None;
            }
            // The unknown sentinel is stated for the *pair*: "if the
            // course and speed are unknown or not relevant, they can be
            // set to 000/000". So only 000/000 as a whole erases the
            // speed. `315/000` keeps its zero, which is what a
            // stationary tracker is reporting.
            let pair_unknown = aw == ZERO_TRIPLE && bw == ZERO_TRIPLE;
            let first = Bearing {
                // A bearing has its own rule and needs no help from the
                // pair: the stated course domain is 001-360, so 000 is
                // outside it however the other field reads.
                degrees: a.filter(|&v| v != 0),
                wire: aw,
            };
            let second = Speed {
                knots: if pair_unknown { None } else { b },
                wire: bw,
            };
            return Some(if symbol.to_wire().1 == WEATHER_SYMBOL_CODE {
                Self::Wind {
                    direction: first,
                    speed: second,
                }
            } else {
                Self::CourseSpeed {
                    course: first,
                    speed: second,
                }
            });
        }
        match &bytes[..3] {
            b"PHG" => {
                let codes = &bytes[3..7];
                if !codes[0].is_ascii_digit()
                    || !codes[2].is_ascii_digit()
                    || !codes[3].is_ascii_digit()
                    || codes[1] < b'0'
                {
                    return None;
                }
                let phg = Phg::new(
                    codes[0] - b'0',
                    codes[1] - b'0',
                    codes[2] - b'0',
                    codes[3] - b'0',
                )
                .ok()?;
                // PHGR is recognised only by the mandatory '/' at index
                // 8. Testing "index 7 is a digit" is not enough: both
                // `PHGabcd/` (7-byte extension, '/' as a free-text
                // separator) and `PHGabcdr/` end in a slash, and a plain
                // PHG followed by a digit — `PHG5260146.520MHz` — would
                // otherwise be eaten.
                if bytes.len() >= Self::LEN_PHGR
                    && bytes[Self::LEN_PHGR - 1] == b'/'
                    && let Some(rate) = phgr_rate(bytes[Self::LEN])
                {
                    return Some(Self::Phg(phg.with_rate(rate)));
                }
                Some(Self::Phg(phg))
            }
            b"RNG" => {
                if !bytes[3..7].iter().all(u8::is_ascii_digit) {
                    return None;
                }
                let miles = bytes[3..7]
                    .iter()
                    .fold(0u16, |a, &c| a * 10 + u16::from(c - b'0'));
                Some(Self::Range { miles })
            }
            b"DFS" => {
                let codes = &bytes[3..7];
                if !codes[0].is_ascii_digit()
                    || !codes[2].is_ascii_digit()
                    || !codes[3].is_ascii_digit()
                    || codes[1] < b'0'
                {
                    return None;
                }
                Some(Self::Dfs(
                    Dfs::new(
                        codes[0] - b'0',
                        codes[1] - b'0',
                        codes[2] - b'0',
                        codes[3] - b'0',
                    )
                    .ok()?,
                ))
            }
            _ => None,
        }
    }

    /// Writes the wire form into `out`, returning the bytes written, or
    /// 0 if `out` is shorter than [`Self::wire_len`].
    pub fn write(&self, out: &mut [u8]) -> usize {
        let n = self.wire_len();
        if out.len() < n {
            return 0;
        }
        match self {
            Self::CourseSpeed { course, speed } => {
                out[..3].copy_from_slice(&course.wire);
                out[3] = b'/';
                out[4..7].copy_from_slice(&speed.wire);
            }
            Self::Wind { direction, speed } => {
                out[..3].copy_from_slice(&direction.wire);
                out[3] = b'/';
                out[4..7].copy_from_slice(&speed.wire);
            }
            Self::Phg(p) => {
                out[..3].copy_from_slice(b"PHG");
                out[3] = b'0' + p.power;
                out[4] = b'0' + p.height;
                out[5] = b'0' + p.gain;
                out[6] = b'0' + p.directivity;
                if let Some(rate) = p.rate {
                    out[7] = match rate {
                        PhgRate::Unscheduled => b'0',
                        PhgRate::PerHour(n @ 1..=9) => b'0' + n,
                        PhgRate::PerHour(n) => b'A' + (n - 10).min(25),
                    };
                    // Mandatory terminator, not padding: the spec's own
                    // example is `PHG72604/`.
                    out[8] = b'/';
                }
            }
            Self::Range { miles } => {
                out[..3].copy_from_slice(b"RNG");
                let m = *miles;
                out[3] = b'0' + (m / 1000 % 10) as u8;
                out[4] = b'0' + (m / 100 % 10) as u8;
                out[5] = b'0' + (m / 10 % 10) as u8;
                out[6] = b'0' + (m % 10) as u8;
            }
            Self::Dfs(d) => {
                out[..3].copy_from_slice(b"DFS");
                out[3] = b'0' + d.strength;
                out[4] = b'0' + d.height;
                out[5] = b'0' + d.gain;
                out[6] = b'0' + d.directivity;
            }
        }
        n
    }
}

/// Decodes a `PHGR` rate character: `0` is the unscheduled sentinel,
/// `1`..=`9` are literal, `A`..=`Z` continue from 10.
fn phgr_rate(c: u8) -> Option<PhgRate> {
    match c {
        b'0' => Some(PhgRate::Unscheduled),
        b'1'..=b'9' => Some(PhgRate::PerHour(c - b'0')),
        b'A'..=b'Z' => Some(PhgRate::PerHour(10 + (c - b'A'))),
        _ => None,
    }
}

/// Finds a `/A=nnnnnn` altitude, in feet, anywhere in `bytes`.
///
/// # Why this is a search and not a field
///
/// Unlike a data extension, the specification places `/A=` *inside the
/// comment text*: "The altitude may appear anywhere in the comment."
/// Promoting it to a struct field would mean either losing byte-exact
/// round-tripping (a builder would have to choose somewhere to
/// re-insert it) or storing an offset beside it, which is worse than
/// re-scanning nine bytes. So altitude is a **view** of the comment.
///
/// Exactly six characters follow `/A=`. The negative form `/A=-ddddd`
/// — a minus sign and five digits, keeping the field nine bytes wide —
/// is accepted: the specification notes it is "not in the official
/// standard" but that "many applications also recognize" it, and
/// below-sea-level stations are real (Death Valley, the Dead Sea, the
/// Salton Sea).
#[must_use]
pub fn altitude_feet(bytes: &[u8]) -> Option<i32> {
    bytes.windows(9).find_map(|w| {
        if &w[..3] != b"/A=" {
            return None;
        }
        let f = &w[3..];
        if f.iter().all(u8::is_ascii_digit) {
            Some(f.iter().fold(0i32, |a, &c| a * 10 + i32::from(c - b'0')))
        } else if f[0] == b'-' && f[1..].iter().all(u8::is_ascii_digit) {
            Some(
                -f[1..]
                    .iter()
                    .fold(0i32, |a, &c| a * 10 + i32::from(c - b'0')),
            )
        } else {
            None
        }
    })
}

/// Lowest byte of the APRS base-91 alphabet, worth 0.
const BASE91_LOW: u8 = b'!';
/// Highest byte of the APRS base-91 alphabet, worth 90.
const BASE91_HIGH: u8 = b'{';

/// Values a base-91 telemetry block can carry: a sequence counter, up
/// to five analog channels, and one packed digital channel.
const COMMENT_TELEMETRY_VALUES_MAX: usize = 7;
/// The block must carry at least a sequence counter and one channel.
const COMMENT_TELEMETRY_VALUES_MIN: usize = 2;

/// Base-91 comment telemetry: chapter 13's compact in-comment form.
///
/// Two bytes per value, so each is `0..=8280` rather than the `T#`
/// form's `0..=255`. The values are raw counts with no unit until the
/// station's `UNIT.` and `EQNS.` messages give them one, which is why
/// they are plain integers here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommentTelemetry {
    /// The sequence counter.
    pub seq: u16,
    /// Up to five analog channels, `None` where none was sent.
    pub analog: [Option<u16>; 5],
    /// The eight digital channels, or `None` when the block carried
    /// none.
    pub digital: Option<[bool; 8]>,
}

/// Decodes two base-91 bytes, most significant first.
const fn base91_pair(hi: u8, lo: u8) -> Option<u16> {
    if hi < BASE91_LOW || hi > BASE91_HIGH || lo < BASE91_LOW || lo > BASE91_HIGH {
        return None;
    }
    // 90 * 91 + 90 = 8280, so a u16 holds every value.
    Some((hi - BASE91_LOW) as u16 * 91 + (lo - BASE91_LOW) as u16)
}

/// The byte range of a base-91 telemetry block, delimiters included.
///
/// This exists so that [`dao`] can look everywhere *except* here. A
/// telemetry payload is arbitrary base-91 bytes, `!` among them, so it
/// produces `!x??!` sequences that look exactly like a `!DAO!` field.
/// MEASURED over a 64 918-packet capture, scanning for `!DAO!` without
/// excluding this block yields **51** false positives, three of them
/// inside the telemetry of a compressed position, where applying the
/// bogus refinement would move the position it claims to refine
/// (`APRS Digi|$d!X!Y!U&!!(|` matches `!X!Y!`). With the exclusion, and
/// MEASURED over 95 219 packets, all 773 surviving matches carry a
/// datum letter chapter 5 assigns and none carries an unassigned one,
/// which is what a false positive would not do.
fn telemetry_span(bytes: &[u8]) -> Option<(usize, usize)> {
    let open = bytes.iter().position(|&b| b == b'|')?;
    let rest = bytes.get(open + 1..)?;
    let len = rest.iter().position(|&b| b == b'|')?;
    let payload = rest.get(..len)?;
    if !is_comment_telemetry_payload(payload) {
        return None;
    }
    Some((open, open + 1 + len))
}

/// Whether `payload` is a well-formed base-91 telemetry payload.
///
/// Even length, 4 to 14 bytes, every byte in the base-91 alphabet. That
/// class alone is what keeps operator text out: MEASURED, it rejects
/// every pipe occurrence in ordinary operator comments, and all 1 262
/// payloads it accepts across 95 219 packets are even-length, with no
/// odd-length candidate anywhere.
fn is_comment_telemetry_payload(payload: &[u8]) -> bool {
    let len = payload.len();
    (COMMENT_TELEMETRY_VALUES_MIN * 2..=COMMENT_TELEMETRY_VALUES_MAX * 2).contains(&len)
        && len.is_multiple_of(2)
        && payload
            .iter()
            .all(|&b| (BASE91_LOW..=BASE91_HIGH).contains(&b))
}

/// Finds base-91 comment telemetry anywhere in `bytes`.
///
/// # Wire layout
///
/// Chapter 13: a `|`-delimited run of two-byte base-91 values, the
/// first being the sequence counter and the rest channels. Digital
/// values, when present, "MUST appear last in the extension, after all
/// 5 analog channels", so they are unambiguous only in the full
/// seven-value form; a shorter block is all analog.
///
/// **The digital bits run the other way round from `T#`.** The spec
/// puts them in one base-91 integer "where the LSB corresponds to B1",
/// while the `T#` form writes B1 first as text. Reading this block in
/// `T#` order inverts every channel and reports it with no error at
/// all. Bits 9 to 13 of the integer are reserved and ignored.
#[must_use]
pub fn comment_telemetry(bytes: &[u8]) -> Option<CommentTelemetry> {
    let (open, close) = telemetry_span(bytes)?;
    let payload = bytes.get(open + 1..close)?;
    let count = payload.len() / 2;
    let mut values = [0u16; COMMENT_TELEMETRY_VALUES_MAX];
    for (index, slot) in values.iter_mut().take(count).enumerate() {
        let pair = payload.get(index * 2..index * 2 + 2)?;
        *slot = base91_pair(pair[0], pair[1])?;
    }

    // Digital only in the full form. With six values after the counter
    // the last is the packed digital word; with fewer, every one of
    // them is an analog channel.
    let has_digital = count == COMMENT_TELEMETRY_VALUES_MAX;
    let analog_count = if has_digital { 5 } else { count - 1 };
    let mut analog = [None; 5];
    for (index, slot) in analog.iter_mut().take(analog_count).enumerate() {
        *slot = Some(values[index + 1]);
    }
    let digital = has_digital.then(|| {
        let packed = values[COMMENT_TELEMETRY_VALUES_MAX - 1];
        let mut bits = [false; 8];
        for (index, bit) in bits.iter_mut().enumerate() {
            *bit = packed & (1 << index) != 0;
        }
        bits
    });
    Some(CommentTelemetry {
        seq: values[0],
        analog,
        digital,
    })
}

/// Coordinate units in one thousandth of a minute, the added digit an
/// upper-case `!DAO!` carries.
///
/// Exact: [`UNITS_PER_DEGREE`] is divisible by 60 000.
const UNITS_PER_MILLI_MINUTE: i64 = UNITS_PER_DEGREE / 60_000;

/// Coordinate units in one ninety-first of a hundredth of a minute, the
/// step a lower-case `!DAO!` counts in.
///
/// Also exact: [`UNITS_PER_DEGREE`] is divisible by 546 000, so a
/// base-91 addend needs no rounding and no division at read time.
const UNITS_PER_BASE91_STEP: i64 = UNITS_PER_DEGREE / 546_000;

/// The `!DAO!` datum-and-precision option (APRS 1.2, chapter 5).
///
/// Five bytes anywhere in a position comment: `!`, a datum byte, an
/// added latitude digit, an added longitude digit, `!`. It refines a
/// `DDMM.hh` position without disturbing it, so older decoders keep
/// working.
///
/// # This is an addend, not a replacement
///
/// The latitude and longitude fields of a position already hold what
/// the wire said, rounded to hundredths of a minute. `!DAO!` adds to
/// them. That direction is forced rather than chosen: the builder
/// rounds to hundredths, so a field carrying the addend would rebuild
/// `4903.50` as `4903.51`. The addend is always below `0.01` minutes,
/// so it can never carry into the printed field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dao {
    /// The datum byte exactly as sent.
    ///
    /// Opaque on purpose. Chapter 5 assigns `W` (WGS84), `N` (NAD27)
    /// and `O` (OSGB36) and promises a table of 26 that was never
    /// published, so a decoder that mapped the other 23 would be
    /// inventing them. MEASURED over 95 219 packets, every one of the
    /// 773 real matches carries `w` (710) or `W` (63). Treat an
    /// unrecognised byte as WGS84, the specification's stated default.
    pub datum: u8,
    /// Added latitude precision, in [`UNITS_PER_DEGREE`] units, toward
    /// the hemisphere the position already declares.
    pub latitude_units: i64,
    /// Added longitude precision, the same way.
    pub longitude_units: i64,
}

impl Dao {
    /// Whether the datum byte is one of the three chapter 5 assigns.
    ///
    /// Anything else is still carried in [`datum`](Self::datum); this
    /// only says whether the specification names it.
    #[must_use]
    pub const fn datum_is_assigned(self) -> bool {
        matches!(self.datum.to_ascii_uppercase(), b'W' | b'N' | b'O')
    }
}

/// One added-precision byte, as units of coordinate.
///
/// A space is the specification's NULL form: the field is present to
/// carry the datum and claims no added precision.
const fn dao_addend(byte: u8, base91: bool) -> Option<i64> {
    if byte == b' ' {
        return Some(0);
    }
    if base91 {
        if byte < BASE91_LOW || byte > BASE91_HIGH {
            return None;
        }
        return Some((byte - BASE91_LOW) as i64 * UNITS_PER_BASE91_STEP);
    }
    if !byte.is_ascii_digit() {
        return None;
    }
    Some((byte - b'0') as i64 * UNITS_PER_MILLI_MINUTE)
}

/// Finds a `!DAO!` field anywhere in `bytes`, outside any telemetry.
///
/// # Why the scan skips base-91 telemetry
///
/// See the private `telemetry_span` helper. A telemetry payload is
/// arbitrary base-91
/// bytes and produces `!x??!` sequences that are indistinguishable from
/// this field by shape alone.
///
/// # Precision
///
/// The datum byte's **case** selects the encoding: upper case means the
/// two bytes are decimal digits worth thousandths of a minute, lower
/// case means they are base-91 worth ninety-firsts of a hundredth of a
/// minute. A digit datum, which chapter 5 offers for "local custom"
/// use, has no case and so cannot say which, and is not accepted;
/// MEASURED, no captured packet sends one.
///
/// The base-91 addend here is exactly `v / 91 x 0.01` minutes. Chapter
/// 5 tells implementers to scale the two digits "by 1.10" instead,
/// which is an approximation of `100/91 = 1.0989…`; the exact form
/// costs nothing because [`UNITS_PER_DEGREE`] divides by 546 000.
///
/// Note also that chapter 5's second worked example is wrong: it gives
/// `!w:\!` as adding "27" of latitude, where `:` is 58 and 58 - 33 is
/// **25**. Do not use it as a test vector.
#[must_use]
pub fn dao(bytes: &[u8]) -> Option<Dao> {
    let skip = telemetry_span(bytes);
    bytes.windows(5).enumerate().find_map(|(at, w)| {
        if let Some((open, close)) = skip
            && at + 4 >= open
            && at <= close
        {
            return None;
        }
        if w[0] != b'!' || w[4] != b'!' {
            return None;
        }
        let datum = w[1];
        if !datum.is_ascii_alphabetic() {
            return None;
        }
        let base91 = datum.is_ascii_lowercase();
        Some(Dao {
            datum,
            latitude_units: dao_addend(w[2], base91)?,
            longitude_units: dao_addend(w[3], base91)?,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Chapter 13's own base-91 telemetry example, value for value.
    ///
    /// The spec spells out every decode, so this is a known-answer test
    /// rather than an interpretation of one.
    #[test]
    fn comment_telemetry_spec_vector() {
        let t = comment_telemetry(b"|ss1122334455!\"|").expect("the spec's full form");
        assert_eq!(t.seq, 7544);
        assert_eq!(t.analog[0], Some(1472));
        assert_eq!(t.analog[1], Some(1564));
        assert_eq!(t.analog[2], Some(1656));
        assert_eq!(t.analog[3], Some(1748));
        assert_eq!(t.analog[4], Some(1840));
        // "'!\"' decodes to decimal 1, binary values 10000000, B1 is 1,
        // B2 to B8 are 0." The LSB is B1, which is the reverse of the
        // `T#` text form; reading it the other way inverts every
        // channel and reports no error.
        assert_eq!(
            t.digital,
            Some([true, false, false, false, false, false, false, false])
        );

        // The spec's two shorter forms: no digital word, because it is
        // unambiguous only when all five analog channels precede it.
        let short = comment_telemetry(b"|ss11|").expect("seq and one channel");
        assert_eq!(short.seq, 7544);
        assert_eq!(short.analog[0], Some(1472));
        assert_eq!(short.analog[1], None);
        assert_eq!(short.digital, None);

        let three = comment_telemetry(b"|ss112233|").expect("seq and three channels");
        assert_eq!(three.analog[2], Some(1656));
        assert_eq!(three.analog[3], None);
        assert_eq!(
            three.digital, None,
            "six bytes cannot carry the digital word"
        );
    }

    /// The byte class is what keeps operator text out.
    #[test]
    fn comment_telemetry_rejects_operator_text() {
        // MEASURED: this class rejects all 1 189 pipe occurrences in
        // ordinary comment text across the capture.
        assert_eq!(
            comment_telemetry(b"see you|73|"),
            None,
            "not base-91 length"
        );
        assert_eq!(comment_telemetry(b"|abc|"), None, "odd length");
        assert_eq!(comment_telemetry(b"|ab|"), None, "one value, needs two");
        assert_eq!(comment_telemetry(b"|ss1122334455!\"66|"), None, "too long");
        assert_eq!(comment_telemetry(b"|ss 1|"), None, "space is not base-91");
        assert_eq!(comment_telemetry(b"no pipes here"), None);
        assert_eq!(comment_telemetry(b"|unterminated"), None);
    }

    /// Chapter 5's `!DAO!` examples, and the one that is wrong.
    #[test]
    fn dao_spec_vectors() {
        // Upper case: human-readable thousandths of a minute.
        let d = dao(b"!W23!").expect("the spec's first example");
        assert_eq!(d.datum, b'W');
        assert!(d.datum_is_assigned());
        assert_eq!(d.latitude_units, 2 * UNITS_PER_MILLI_MINUTE);
        assert_eq!(d.longitude_units, 3 * UNITS_PER_MILLI_MINUTE);

        // Lower case: base-91. The spec's own arithmetic, "A" is 65 - 33
        // = 32 and "b" is 98 - 33 = 65.
        let d = dao(b"!wAb!").expect("the spec's second example");
        assert_eq!(d.datum, b'w');
        assert_eq!(d.latitude_units, 32 * UNITS_PER_BASE91_STEP);
        assert_eq!(d.longitude_units, 65 * UNITS_PER_BASE91_STEP);

        // Chapter 5's third example claims ':' adds "27". It does not:
        // ':' is 58 and 58 - 33 is 25. The spec is wrong, and this test
        // pins the arithmetic rather than the prose.
        let d = dao(b"!w:\\!").expect("parses, whatever the spec says it means");
        assert_eq!(d.latitude_units, 25 * UNITS_PER_BASE91_STEP);
        assert_eq!(d.longitude_units, 59 * UNITS_PER_BASE91_STEP);

        // The NULL form: datum only, no added precision claimed.
        let d = dao(b"!W  !").expect("the NULL form");
        assert_eq!(d.latitude_units, 0);
        assert_eq!(d.longitude_units, 0);
    }

    /// The addend can never carry into the printed hundredth.
    ///
    /// This is what lets the position fields keep the wire's own
    /// `DDMM.hh` while the accessor adds the refinement. If an addend
    /// could reach `0.01` minutes, a field carrying it would rebuild
    /// `4903.50` as `4903.51`.
    #[test]
    fn dao_addend_is_always_under_a_hundredth_of_a_minute() {
        let hundredth = UNITS_PER_DEGREE / 6_000;
        assert_eq!(9 * UNITS_PER_MILLI_MINUTE, hundredth * 9 / 10);
        assert!(9 * UNITS_PER_MILLI_MINUTE < hundredth, "widest decimal");
        assert!(90 * UNITS_PER_BASE91_STEP < hundredth, "widest base-91");
        // And both divide exactly, so no reading rounds.
        assert_eq!(UNITS_PER_DEGREE % 60_000, 0);
        assert_eq!(UNITS_PER_DEGREE % 546_000, 0);
    }

    /// A `!DAO!` may sit anywhere, and most do not sit at the end.
    ///
    /// MEASURED over the capture: 201 mid-comment against 165 trailing.
    #[test]
    fn dao_is_found_anywhere_in_the_comment() {
        assert!(dao(b"hello !w!!! there").is_some());
        assert!(dao(b"trailing !W12!").is_some());
        assert_eq!(dao(b"no dao here"), None);
        // A digit datum has no case, so it cannot say which encoding
        // the two bytes use. Chapter 5 offers digits for local custom
        // use; MEASURED, no captured packet sends one.
        assert_eq!(dao(b"!512!"), None);
        // Wrong shape.
        assert_eq!(dao(b"!W1!"), None);
        assert_eq!(dao(b"!W1X!"), None, "X is not a decimal digit");
    }

    /// Telemetry is recognised first, so its bytes cannot be read as a
    /// `!DAO!` that would move the position.
    ///
    /// MEASURED: scanning without this exclusion produces 51 false
    /// positives, three of them inside the telemetry of a compressed
    /// position. This is the real packet from that set.
    #[test]
    fn telemetry_bytes_are_not_mistaken_for_dao() {
        let comment = b"APRS Digi|$d!X!Y!U&!!(|";
        // The payload really does contain something DAO-shaped.
        assert_eq!(&comment[12..17], b"!X!Y!");
        // It is telemetry, and it is read as telemetry.
        assert!(comment_telemetry(comment).is_some());
        // And it is NOT read as a position refinement.
        assert_eq!(
            dao(comment),
            None,
            "a telemetry payload must not be read as DAO"
        );
        // A real DAO after a telemetry block is still found.
        let both = b"hi|$d!X!Y!U&!!(|!w12!";
        assert!(comment_telemetry(both).is_some());
        assert_eq!(dao(both).map(|d| d.datum), Some(b'w'));
    }

    /// Not the weather symbol, so `ddd/sss` is course/speed.
    fn car() -> Symbol {
        Symbol::from_wire(b'/', b'>')
    }

    /// The weather symbol, so `ddd/sss` is wind.
    fn wx() -> Symbol {
        Symbol::from_wire(b'/', b'_')
    }

    /// Every wire form must survive parse -> write byte-identically.
    /// This is the property the wire-code storage exists to guarantee,
    /// and the PHGR forms are the ones a naive implementation breaks.
    #[test]
    fn wire_round_trip_is_byte_exact() {
        for s in [
            &b"125/007"[..],
            b"000/000",
            b".../...",
            b"   /   ",
            b"360/999",
            b"PHG2360",
            b"PHG0000",
            b"PHG9998",
            b"PHG72604/", // the spec's own PHGR example
            b"PHG52605/", // both from the corpus
            b"PHG92603/",
            b"PHG5260A/", // letter rate, 10/hour
            b"PHG52600/", // unscheduled sentinel
            b"RNG0050",
            b"RNG9999",
            b"DFS2360",
            b"DFS0000",
        ] {
            for sym in [car(), wx()] {
                let ext = DataExtension::parse(s, sym)
                    .unwrap_or_else(|| panic!("did not parse: {:?}", core::str::from_utf8(s)));
                assert_eq!(ext.wire_len(), s.len(), "wire_len for {s:?}");
                let mut out = [0u8; DataExtension::LEN_PHGR];
                let n = ext.write(&mut out);
                assert_eq!(&out[..n], s, "round trip for {:?}", core::str::from_utf8(s));
            }
        }
    }

    #[test]
    fn phgr_needs_the_mandatory_slash() {
        // The spec's example: PHG72604/ is 4 beacons per hour.
        let Some(DataExtension::Phg(p)) = DataExtension::parse(b"PHG72604/", car()) else {
            panic!("expected PHGR")
        };
        assert_eq!(p.rate(), Some(PhgRate::PerHour(4)));
        assert_eq!(p.power_watts(), 49);

        // `PHGabcd/` is the 7-byte form with '/' as a free-text
        // separator -- by far the commonest shape on the air.
        let e = DataExtension::parse(b"PHG2100/WinAPRS", car()).unwrap();
        assert_eq!(e.wire_len(), 7);
        let DataExtension::Phg(q) = e else { panic!() };
        assert_eq!(q.rate(), None);

        // A plain PHG followed by digits must NOT be eaten. Testing
        // "byte 7 is a digit" would consume `14` here.
        let e = DataExtension::parse(b"PHG5260146.520MHz", car()).unwrap();
        assert_eq!(e.wire_len(), 7, "must not swallow the frequency");

        // Letter rates continue from 10.
        let Some(DataExtension::Phg(r)) = DataExtension::parse(b"PHG5260A/", car()) else {
            panic!()
        };
        assert_eq!(r.rate(), Some(PhgRate::PerHour(10)));

        // Rate 0 is a sentinel, not "zero per hour".
        let Some(DataExtension::Phg(z)) = DataExtension::parse(b"PHG52600/", car()) else {
            panic!()
        };
        assert_eq!(z.rate(), Some(PhgRate::Unscheduled));
    }

    #[test]
    fn phg_code_tables() {
        // PHG2360: 2^2 = 4 W, 10*2^3 = 80 ft, 6 dBi, omni.
        let Some(DataExtension::Phg(p)) = DataExtension::parse(b"PHG2360", car()) else {
            panic!()
        };
        assert_eq!((p.power_watts(), p.height_feet(), p.gain_dbi()), (4, 80, 6));
        assert_eq!(p.directivity_degrees(), None);

        let Some(DataExtension::Phg(hi)) = DataExtension::parse(b"PHG9998", car()) else {
            panic!()
        };
        assert_eq!(
            (hi.power_watts(), hi.height_feet(), hi.gain_dbi()),
            (81, 5120, 9)
        );
        assert_eq!(hi.directivity_degrees(), Some(360));

        // Directivity 9 is blank in the spec table. Accept the
        // extension and report no direction rather than discarding a
        // perfectly good power/height/gain.
        let Some(DataExtension::Phg(d9)) = DataExtension::parse(b"PHG5139", car()) else {
            panic!("directivity 9 must not reject the whole extension")
        };
        assert_eq!(d9.power_watts(), 25);
        assert_eq!(d9.directivity_degrees(), None);
    }

    /// The height code is explicitly allowed above '9' "so that larger
    /// heights for balloons, aircraft or satellites may be specified".
    /// This crate ships balloon-tracker examples, so it matters.
    #[test]
    fn height_codes_above_nine() {
        let Some(DataExtension::Phg(p)) = DataExtension::parse(b"PHG5:32", car()) else {
            panic!("':' is a legal height code")
        };
        assert_eq!(p.height_feet(), 10_240);
        let Some(DataExtension::Phg(q)) = DataExtension::parse(b"PHG5;32", car()) else {
            panic!()
        };
        assert_eq!(q.height_feet(), 20_480);
        // And it still round-trips.
        let mut out = [0u8; 9];
        let n = DataExtension::parse(b"PHG5:32", car())
            .unwrap()
            .write(&mut out);
        assert_eq!(&out[..n], b"PHG5:32");
    }

    #[test]
    fn wind_versus_course_depends_on_the_symbol() {
        let bytes = b"220/004";
        assert!(matches!(
            DataExtension::parse(bytes, wx()),
            Some(DataExtension::Wind { .. })
        ));
        assert!(matches!(
            DataExtension::parse(bytes, car()),
            Some(DataExtension::CourseSpeed { .. })
        ));
    }

    #[test]
    fn unknown_spellings_are_preserved_and_distinguished() {
        for s in [&b"000/000"[..], b".../...", b"   /   "] {
            let Some(DataExtension::CourseSpeed { course, speed }) = DataExtension::parse(s, car())
            else {
                panic!("{:?} is a spec-legal unknown form", core::str::from_utf8(s))
            };
            assert_eq!(course.degrees(), None, "{s:?}");
            assert_eq!(speed.knots(), None, "{s:?}");
        }
        // A real bearing is not confused with unknown.
        let Some(DataExtension::CourseSpeed { course, speed }) =
            DataExtension::parse(b"360/012", car())
        else {
            panic!()
        };
        assert_eq!(course.degrees(), Some(360));
        assert_eq!(speed.knots(), Some(12));
    }

    /// The specification's own sentence is the case list: the unknown
    /// sentinel is `000/000` **as a pair**, so a zero speed beside a
    /// real course is a speed of zero. The independent reference reads
    /// `315/000` as "0 km/h (0 MPH), course 315".
    #[test]
    fn zero_speed_is_only_unknown_as_a_whole_pair() {
        for (wire, course, knots) in [
            // Both halves zero: the spec's sentinel, both unknown.
            (&b"000/000"[..], None, None),
            // A real course beside a standing start. MEASURED in the
            // corpus: 12 frames spell 315/000 or 194/000 and 6 spell
            // 035/000, and the independent reference reports a speed
            // for every one of them.
            (b"315/000", Some(315), Some(0)),
            (b"194/000", Some(194), Some(0)),
            (b"035/000", Some(35), Some(0)),
            // Course unknown, speed known: `000` is outside the stated
            // 001-360 course domain, and the pair rule does not fire.
            (b"000/048", None, Some(48)),
            // The other two unknown spellings are untouched.
            (b".../...", None, None),
            (b"   /   ", None, None),
            // Boundaries of the stated course domain.
            (b"360/010", Some(360), Some(10)),
            (b"001/010", Some(1), Some(10)),
        ] {
            let Some(DataExtension::CourseSpeed {
                course: c,
                speed: s,
            }) = DataExtension::parse(wire, car())
            else {
                panic!("{:?} is a legal extension", core::str::from_utf8(wire))
            };
            assert_eq!(c.degrees(), course, "course of {wire:?}");
            assert_eq!(s.knots(), knots, "speed of {wire:?}");
        }
    }

    /// The whole point of the change: `DDD/SSS` must mean the same
    /// thing whichever path in this crate reads it. `weather.rs` has
    /// always read `240/000` in a Complete Weather Report as a calm
    /// `Some(0)`; the `_`-symbol extension now agrees instead of
    /// calling the identical bytes unknown.
    #[test]
    fn wind_reads_zero_the_same_way_the_weather_decoder_does() {
        for (wire, direction, knots) in [
            (&b"240/000"[..], Some(240), Some(0)),
            (b"090/000", Some(90), Some(0)),
            (b"000/000", None, None),
            (b"000/012", None, Some(12)),
            (b"360/004", Some(360), Some(4)),
        ] {
            let Some(DataExtension::Wind {
                direction: d,
                speed: s,
            }) = DataExtension::parse(wire, wx())
            else {
                panic!("{:?} is a legal wind extension", core::str::from_utf8(wire))
            };
            assert_eq!(d.degrees(), direction, "direction of {wire:?}");
            assert_eq!(s.knots(), knots, "wind speed of {wire:?}");
        }
    }

    /// A law, not a table: **every** `ddd/sss` that parses as an
    /// extension writes back byte-identically, for both symbol
    /// readings. `Bearing`/`Speed` store `wire: [u8; 3]` and `write`
    /// copies it verbatim precisely so that changing what the *decoded*
    /// value means — as the zero-speed fix does — cannot move a byte.
    /// Bearings above 360 are pinned as "not an extension" in the same
    /// sweep, since that is the other half of the domain.
    #[test]
    fn every_ddd_sss_round_trips_byte_exactly() {
        let mut wire = *b"000/000";
        for d in 0u16..=999 {
            wire[0] = b'0' + (d / 100) as u8;
            wire[1] = b'0' + (d / 10 % 10) as u8;
            wire[2] = b'0' + (d % 10) as u8;
            for s in 0u16..=999 {
                wire[4] = b'0' + (s / 100) as u8;
                wire[5] = b'0' + (s / 10 % 10) as u8;
                wire[6] = b'0' + (s % 10) as u8;
                for sym in [car(), wx()] {
                    let parsed = DataExtension::parse(&wire, sym);
                    if d > 360 {
                        assert_eq!(parsed, None, "{d:03} is not a bearing");
                        continue;
                    }
                    let ext = parsed.unwrap_or_else(|| panic!("did not parse: {wire:?}"));
                    let mut out = [0u8; DataExtension::LEN_PHGR];
                    let n = ext.write(&mut out);
                    assert_eq!(n, DataExtension::LEN);
                    assert_eq!(&out[..n], &wire[..], "round trip for {wire:?}");
                }
            }
        }
        // The non-numeric spellings obey the same law.
        for s in [&b".../..."[..], b"   /   ", b"000/...", b".../000"] {
            for sym in [car(), wx()] {
                let ext = DataExtension::parse(s, sym).expect("legal unknown spelling");
                let mut out = [0u8; DataExtension::LEN_PHGR];
                let n = ext.write(&mut out);
                assert_eq!(&out[..n], s, "round trip for {s:?}");
            }
        }
    }

    /// `RNG`, `PHG` and `DFS` were checked for the same independent
    /// zero-collapse and have none: every zero they carry is a value.
    /// Pinned so a later "tidy-up" cannot introduce one.
    #[test]
    fn the_other_extensions_have_no_zero_collapse() {
        // RNG0000 is a range of zero miles, not a missing range. It is
        // a plain u16 with no unknown state to collapse into.
        assert_eq!(
            DataExtension::parse(b"RNG0000", car()),
            Some(DataExtension::Range { miles: 0 })
        );
        // PHG power code 0 is 0 W and height code 0 is 10 ft; both are
        // table entries. Only the PHGR *rate* has a zero sentinel, and
        // that one is the specification's own (`Unscheduled`), spelled
        // as a distinct variant rather than as a `None`.
        let Some(DataExtension::Phg(p)) = DataExtension::parse(b"PHG0000", car()) else {
            panic!()
        };
        assert_eq!((p.power_watts(), p.height_feet(), p.gain_dbi()), (0, 10, 0));
        // DFS strength 0 is the most significant reading there is --
        // "where the jammer is not heard" -- and must stay a Some.
        let Some(DataExtension::Dfs(d)) = DataExtension::parse(b"DFS0000", car()) else {
            panic!()
        };
        assert_eq!(d.strength_s_points(), 0);
    }

    #[test]
    fn out_of_range_bearing_is_not_an_extension() {
        // 361 degrees is not a bearing, so the bytes stay comment text
        // rather than being decoded as a nonsense course.
        assert_eq!(DataExtension::parse(b"361/000", car()), None);
        assert_eq!(DataExtension::parse(b"999/999", car()), None);
    }

    #[test]
    fn plain_text_is_not_an_extension() {
        for s in [
            &b"hello there"[..],
            b"",
            b"short",
            b"/A=001234",
            b"Ed's remote WX",
            b"PHG",
            b"ab/cdef",
            // The cases an implementation that dispatches on "byte 3 is
            // a slash" destroys.
            b"Hwy/101 north of town",
            b"KG6/W6ABC portable",
            b"abc/def letters with slash",
        ] {
            assert_eq!(
                DataExtension::parse(s, car()),
                None,
                "{:?}",
                core::str::from_utf8(s)
            );
        }
    }

    #[test]
    fn range_and_dfs() {
        assert_eq!(
            DataExtension::parse(b"RNG0050", car()),
            Some(DataExtension::Range { miles: 50 })
        );
        let Some(DataExtension::Dfs(d)) = DataExtension::parse(b"DFS2360", car()) else {
            panic!()
        };
        assert_eq!(d.strength_s_points(), 2);
        assert_eq!((d.height_feet(), d.gain_db()), (80, 6));
        assert_eq!(d.directivity_degrees(), None);
        assert_eq!(DataExtension::parse(b"RNG00X0", car()), None);
    }

    #[test]
    fn altitude_anywhere_in_the_comment_including_negative() {
        assert_eq!(altitude_feet(b"/A=004530 hello"), Some(4530));
        assert_eq!(altitude_feet(b"hello /A=000600 there"), Some(600));
        assert_eq!(altitude_feet(b"125/007/A=000984"), Some(984));
        // The de-facto negative form: minus plus five digits.
        assert_eq!(altitude_feet(b"/A=-00123 below sea level"), Some(-123));
        assert_eq!(altitude_feet(b"/A=-0123"), None); // four digits
        assert_eq!(altitude_feet(b"/A=00098"), None); // five digits
        assert_eq!(altitude_feet(b"/A=0009X4"), None);
        assert_eq!(altitude_feet(b"no altitude here"), None);
    }
}
