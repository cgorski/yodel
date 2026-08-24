//! APRS object and item reports (`;` and `)`).
//!
//! An **object** (APRS 1.01 chapter 11) is `;` + a 9-character
//! space-padded name + `*` (live) or `_` (killed) + a 7-character
//! timestamp + an uncompressed position + optional comment. An
//! **item** is `)` + a 3-9 character name + `!` (live) or `_` (killed)
//! + an uncompressed position + comment; items carry no timestamp.

use super::AprsError;
use super::position::{
    CompressedCs, CompressionType, LATLON_LEN, Position, byte_at, parse_digits, write_digits,
};
use super::symbol::Symbol;
use crate::geo::{Ambiguity, Coordinates, Latitude, Longitude};

/// An APRS timestamp: day/hour/minute (zulu or local) or
/// hour/minute/second, per APRS 1.01 chapter 6.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timestamp {
    /// `DDHHMMz`: day of month, hour, minute, UTC.
    DhmZulu {
        /// Day of the month, `1..=31`.
        day: u8,
        /// Hour (24-hour clock), `0..=23`.
        hour: u8,
        /// Minute, `0..=59`.
        minute: u8,
    },
    /// `DDHHMM/`: day of month, hour, minute, station local time.
    DhmLocal {
        /// Day of the month, `1..=31`.
        day: u8,
        /// Hour (24-hour clock), `0..=23`.
        hour: u8,
        /// Minute, `0..=59`.
        minute: u8,
    },
    /// `HHMMSSh`: hour, minute, second, UTC.
    Hms {
        /// Hour (24-hour clock), `0..=23`.
        hour: u8,
        /// Minute, `0..=59`.
        minute: u8,
        /// Second, `0..=59`.
        second: u8,
    },
}

impl Timestamp {
    /// Serialized length: six digits plus the format letter.
    pub const LEN: usize = 7;

    /// Creates a `DDHHMMz` (UTC day/hour/minute) timestamp, validating
    /// the field ranges.
    ///
    /// # Errors
    ///
    /// [`AprsError::BadTimestamp`] on an out-of-range field.
    ///
    /// ```
    /// use warble::aprs::{AprsError, Timestamp};
    ///
    /// let ts = Timestamp::dhm_zulu(9, 23, 45)?;
    /// assert_eq!(ts, Timestamp::DhmZulu { day: 9, hour: 23, minute: 45 });
    /// assert!(Timestamp::dhm_zulu(32, 0, 0).is_err());
    /// # Ok::<(), AprsError>(())
    /// ```
    pub fn dhm_zulu(day: u8, hour: u8, minute: u8) -> Result<Self, AprsError> {
        check_dhm(i32::from(day), i32::from(hour), i32::from(minute))?;
        Ok(Self::DhmZulu { day, hour, minute })
    }

    /// Creates a `DDHHMM/` (station-local day/hour/minute) timestamp,
    /// validating the field ranges.
    ///
    /// # Errors
    ///
    /// [`AprsError::BadTimestamp`] on an out-of-range field.
    pub fn dhm_local(day: u8, hour: u8, minute: u8) -> Result<Self, AprsError> {
        check_dhm(i32::from(day), i32::from(hour), i32::from(minute))?;
        Ok(Self::DhmLocal { day, hour, minute })
    }

    /// Creates an `HHMMSSh` (UTC hour/minute/second) timestamp,
    /// validating the field ranges.
    ///
    /// # Errors
    ///
    /// [`AprsError::BadTimestamp`] on an out-of-range field.
    pub fn hms(hour: u8, minute: u8, second: u8) -> Result<Self, AprsError> {
        check_hms(i32::from(hour), i32::from(minute), i32::from(second))?;
        Ok(Self::Hms {
            hour,
            minute,
            second,
        })
    }

    /// Parses a 7-byte timestamp at `position`.
    ///
    /// # Errors
    ///
    /// [`AprsError::Truncated`] / [`AprsError::BadDigit`] on malformed
    /// digits, [`AprsError::BadTimestamp`] on out-of-range fields or an
    /// unknown format letter (carried as `field: b'?'`).
    pub fn parse(info: &[u8], position: usize) -> Result<Self, AprsError> {
        let a = parse_digits(info, position, 2)?;
        let b = parse_digits(info, position + 2, 2)?;
        let c = parse_digits(info, position + 4, 2)?;
        let letter = byte_at(info, position + 6)?;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        match letter {
            b'z' => {
                check_dhm(a, b, c)?;
                Ok(Self::DhmZulu {
                    day: a as u8,
                    hour: b as u8,
                    minute: c as u8,
                })
            }
            b'/' => {
                check_dhm(a, b, c)?;
                Ok(Self::DhmLocal {
                    day: a as u8,
                    hour: b as u8,
                    minute: c as u8,
                })
            }
            b'h' => {
                check_hms(a, b, c)?;
                Ok(Self::Hms {
                    hour: a as u8,
                    minute: b as u8,
                    second: c as u8,
                })
            }
            other => Err(AprsError::BadTimestamp {
                field: b'?',
                got: i32::from(other),
            }),
        }
    }

    /// Writes the timestamp into `out` (at least [`Self::LEN`] bytes),
    /// validating field ranges first.
    pub(crate) fn write(self, out: &mut [u8]) -> Result<(), AprsError> {
        let (a, b, c, letter) = match self {
            Self::DhmZulu { day, hour, minute } => (day, hour, minute, b'z'),
            Self::DhmLocal { day, hour, minute } => (day, hour, minute, b'/'),
            Self::Hms {
                hour,
                minute,
                second,
            } => (hour, minute, second, b'h'),
        };
        match self {
            Self::DhmZulu { .. } | Self::DhmLocal { .. } => {
                check_dhm(i32::from(a), i32::from(b), i32::from(c))?;
            }
            Self::Hms { .. } => check_hms(i32::from(a), i32::from(b), i32::from(c))?,
        }
        write_digits(&mut out[0..2], u64::from(a));
        write_digits(&mut out[2..4], u64::from(b));
        write_digits(&mut out[4..6], u64::from(c));
        out[6] = letter;
        Ok(())
    }
}

/// Validates day/hour/minute ranges.
fn check_dhm(day: i32, hour: i32, minute: i32) -> Result<(), AprsError> {
    if !(1..=31).contains(&day) {
        return Err(AprsError::BadTimestamp {
            field: b'D',
            got: day,
        });
    }
    check_hm(hour, minute)
}

/// Validates hour/minute/second ranges.
fn check_hms(hour: i32, minute: i32, second: i32) -> Result<(), AprsError> {
    check_hm(hour, minute)?;
    if !(0..=59).contains(&second) {
        return Err(AprsError::BadTimestamp {
            field: b'S',
            got: second,
        });
    }
    Ok(())
}

/// Validates hour/minute ranges.
fn check_hm(hour: i32, minute: i32) -> Result<(), AprsError> {
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

/// An object report (`;`): a named, timestamped position placed by the
/// sending station.
///
/// # Common path: one line, valid by construction
///
/// ```
/// use warble::aprs::{AprsError, Latitude, Longitude, Object, Symbol, Timestamp};
///
/// let obj = Object::new(
///     b"LEADER",
///     Timestamp::dhm_zulu(9, 23, 45)?,
///     Latitude::from_degrees(49.0583)?,
///     Longitude::from_degrees(-72.0292)?,
///     Symbol::CAR,
/// )?
/// .with_comment(b"net control");
/// let mut buf = [0u8; 64];
/// let len = obj.build(&mut buf)?;
/// assert!(buf[..len].starts_with(b";LEADER   *092345z"));
/// # Ok::<(), AprsError>(())
/// ```
///
/// # Power user: fully typed symbol, exhaustively matched
///
/// ```
/// use warble::aprs::{
///     AprsError, Latitude, Longitude, Object, OverlayId, Symbol, SymbolCode, SymbolTable,
///     Timestamp,
/// };
///
/// let obj = Object::new(
///     b"RACE-1",
///     Timestamp::hms(23, 45, 17)?,
///     Latitude::new(0)?,
///     Longitude::new(0)?,
///     Symbol::overlay(OverlayId::new(b'3')?, SymbolCode::new(b'>')?),
/// )?;
/// match obj.symbol.table() {
///     Some(SymbolTable::Overlay(id)) => assert_eq!(id.get(), b'3'),
///     Some(SymbolTable::Primary | SymbolTable::Alternate) | None => unreachable!(),
/// }
/// # Ok::<(), AprsError>(())
/// ```
///
/// # Raw hatch: out-of-spec wire bytes round-trip exactly
///
/// ```
/// use warble::aprs::{AprsError, Latitude, Longitude, Object, Symbol, Timestamp};
///
/// let obj = Object::new(
///     b"ODD",
///     Timestamp::dhm_zulu(1, 0, 0)?,
///     Latitude::new(0)?,
///     Longitude::new(0)?,
///     Symbol::from_wire(b'~', b'$'), // no spec blesses '~'; held verbatim
/// )?
/// .with_live(false);
/// assert_eq!(obj.symbol.to_wire(), (b'~', b'$'));
/// let mut buf = [0u8; 64];
/// let len = obj.build(&mut buf)?;
/// assert_eq!(&buf[..len], b";ODD      _010000z0000.00N~00000.00E$");
/// # Ok::<(), AprsError>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Object<'a> {
    /// Object name: 1-9 printable-ASCII bytes (space padded to 9 on
    /// the wire; trailing spaces are stripped on parse).
    pub name: &'a [u8],
    /// `true` for a live object (`*`), `false` for a killed one (`_`).
    pub live: bool,
    /// The report timestamp.
    pub timestamp: Timestamp,
    /// The object latitude.
    pub latitude: Latitude,
    /// The object longitude.
    pub longitude: Longitude,
    /// The display symbol (table + code); [`Symbol::from_wire`] is
    /// the raw hatch for out-of-spec byte pairs.
    pub symbol: Symbol,
    /// How many low-order coordinate digits the sender blanked.
    ///
    /// Chapter 6 position ambiguity: a station that does not wish to
    /// publish an exact fix replaces trailing digits with spaces, one
    /// digit being a tenth of an arc-minute of vagueness and four a
    /// whole degree.
    ///
    /// **Read the position through [`Self::coordinates`]**, which
    /// applies this to *both* axes. The latitude's blanked digits
    /// arrive as zeros, so that field already reads at the declared
    /// precision, but chapter 6 lets the longitude carry its digits in
    /// full and leaves discarding them to the receiver.
    pub ambiguity: Ambiguity,
    /// `true` when the position is chapter 9's base-91 compressed form
    /// rather than `DDMM.hh`.
    ///
    /// Chapter 9 permits the compressed form in an object or item, and
    /// real traffic uses it: MEASURED over 205 635 live packets, 114
    /// objects and 42 items from 26 senders, including a National
    /// Weather Service alert set. They were refused outright before
    /// this field existed, so their positions were plotted nowhere.
    ///
    /// The compressed form carries a three-byte `cs` trailer, and an
    /// object does not keep it, exactly as [`Position`] does not. 43 of
    /// the 156 carry course, speed or altitude there and lose it; all
    /// 156 gain the position, which is the report's point. Reading the
    /// trailer would need a wrapper type, the way
    /// [`PositionCs`](super::PositionCs) wraps a position.
    pub compressed: bool,
    /// Free-text comment following the position.
    pub comment: &'a [u8],
}

impl<'a> Object<'a> {
    /// Wire length of the object name field.
    const NAME_LEN: usize = 9;
    /// Length with chapter 11's uncompressed position block. Kept for
    /// the doc tests and the length arithmetic they assert; the build
    /// path uses the position's own [`body_len`] so that chapter 9's
    /// shorter form is written at the right offsets.
    ///
    /// [`body_len`]: Position::body_len
    #[allow(dead_code)]
    const FIXED_LEN: usize = 1 + Self::NAME_LEN + 1 + Timestamp::LEN + LATLON_LEN;

    /// Creates a live object report with an empty comment, validating
    /// the name (1-9 printable-ASCII bytes); every other part is
    /// validated by its own type. Use the `with_*` methods to adjust
    /// the live flag and comment.
    ///
    /// # Errors
    ///
    /// [`AprsError::NameLengthInvalid`] on an empty or over-long name
    /// and [`AprsError::BadNameChar`] on a non-printable name byte.
    pub fn new(
        name: &'a [u8],
        timestamp: Timestamp,
        latitude: Latitude,
        longitude: Longitude,
        symbol: Symbol,
    ) -> Result<Self, AprsError> {
        if name.is_empty() || name.len() > Self::NAME_LEN {
            return Err(AprsError::NameLengthInvalid {
                len: name.len(),
                min: 1,
                max: Self::NAME_LEN,
            });
        }
        check_name_chars(name, false)?;
        Ok(Self {
            name,
            live: true,
            timestamp,
            latitude,
            longitude,
            ambiguity: Ambiguity::EXACT,
            symbol,
            // The convenience constructor keeps chapter 11's own
            // spelling; set the field to opt into chapter 9's.
            compressed: false,
            comment: b"",
        })
    }

    /// Returns the report with the given free-text comment.
    #[must_use]
    pub const fn with_comment(self, comment: &'a [u8]) -> Self {
        Self { comment, ..self }
    }

    /// Returns the report with the live flag set as given (`*` live
    /// when `true`, `_` killed when `false`).
    #[must_use]
    pub const fn with_live(self, live: bool) -> Self {
        Self { live, ..self }
    }

    /// The object position, pairing the `latitude` and `longitude`
    /// fields so call sites need not rely on tuple ordering.
    #[must_use]
    pub const fn coordinates(&self) -> Coordinates {
        // Masked to the declared precision, like
        // [`Position::coordinates`](super::Position::coordinates).
        // Chapter 6 lets the longitude carry its digits in full beside
        // a blanked latitude, so reading the fields directly publishes
        // a position finer than the sender claimed.
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

    /// Parses a `;` object report.
    ///
    /// # Errors
    ///
    /// [`AprsError::Truncated`] on a short field,
    /// [`AprsError::BadNameChar`] on a non-printable name byte,
    /// [`AprsError::NameLengthInvalid`] on an all-space name,
    /// [`AprsError::BadLiveKilled`] on a live/killed byte other than
    /// `*`/`_`, plus the timestamp and position errors of
    /// [`Timestamp::parse`] and the position module.
    pub fn parse(info: &'a [u8]) -> Result<Self, AprsError> {
        let dti = byte_at(info, 0)?;
        if dti != b';' {
            return Err(AprsError::InvalidDataType { got: dti });
        }
        // The compressed body is shorter than the uncompressed one, so
        // the precheck can only demand the header plus the shorter of
        // the two; the position parser then reports its own shortfall.
        if info.len() < Self::MIN_LEN {
            return Err(AprsError::Truncated {
                expected: Self::MIN_LEN,
                got: info.len(),
            });
        }
        let raw_name = info.get(1..1 + Self::NAME_LEN).unwrap_or(&[]);
        let name = check_name(raw_name, 1, Self::NAME_LEN)?;
        let live = match byte_at(info, 1 + Self::NAME_LEN)? {
            b'*' => true,
            b'_' => false,
            other => return Err(AprsError::BadLiveKilled { got: other }),
        };
        let timestamp = Timestamp::parse(info, 2 + Self::NAME_LEN)?;
        let at = 2 + Self::NAME_LEN + Timestamp::LEN;
        // Chapter 9's compressed form is legal here too, and the
        // discriminator is the one `Position` already uses.
        let (position, _cs, _t) = Position::parse_body(info, at, false)?;
        Ok(Self {
            name,
            live,
            timestamp,
            latitude: position.latitude,
            longitude: position.longitude,
            ambiguity: position.ambiguity,
            symbol: position.symbol,
            compressed: position.compressed,
            // NOT `position.comment`. A position report parses the
            // seven bytes after the coordinates as a data extension and
            // leaves them out of its comment; an object has no
            // extension field, so those bytes belong to the comment
            // here and taking the position's would silently drop
            // `088/036` off every object that carries one.
            comment: info.get(at + position.body_len()..).unwrap_or(&[]),
        })
    }

    /// Byte offset of the position field, after `;`, the name, the
    /// live/killed flag and the timestamp.
    const POSITION_AT: usize = 2 + Self::NAME_LEN + Timestamp::LEN;

    /// Shortest legal object: the header plus chapter 9's compressed
    /// body, which is six bytes shorter than `DDMM.hh`.
    const MIN_LEN: usize = Self::POSITION_AT + Position::COMPRESSED_BODY;

    /// This report as a bare position, so the shared body writer can
    /// spell the position field in whichever form the object carries.
    fn as_position(&self) -> Position<'a> {
        Position {
            latitude: self.latitude,
            longitude: self.longitude,
            symbol: self.symbol,
            ambiguity: self.ambiguity,
            messaging: false,
            compressed: self.compressed,
            extension: None,
            comment: b"",
        }
    }

    /// The serialized length of this report in bytes.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        Self::POSITION_AT + self.as_position().body_len() + self.comment.len()
    }

    /// Serializes the report into `buf`, returning the written length.
    ///
    /// # Errors
    ///
    /// [`AprsError::NameLengthInvalid`] / [`AprsError::BadNameChar`] on
    /// a bad name, [`AprsError::BadTimestamp`] on an out-of-range
    /// timestamp and [`AprsError::BufferTooSmall`] when `buf` cannot
    /// hold the report.
    pub fn build(&self, buf: &mut [u8]) -> Result<usize, AprsError> {
        if self.name.is_empty() || self.name.len() > Self::NAME_LEN {
            return Err(AprsError::NameLengthInvalid {
                len: self.name.len(),
                min: 1,
                max: Self::NAME_LEN,
            });
        }
        check_name_chars(self.name, false)?;
        let needed = self.encoded_len();
        let max = buf.len();
        let out = buf
            .get_mut(..needed)
            .ok_or(AprsError::BufferTooSmall { needed, max })?;
        out[0] = b';';
        for (slot, at) in out[1..1 + Self::NAME_LEN].iter_mut().zip(0..) {
            *slot = self.name.get(at).copied().unwrap_or(b' ');
        }
        out[1 + Self::NAME_LEN] = if self.live { b'*' } else { b'_' };
        self.timestamp
            .write(&mut out[2 + Self::NAME_LEN..Self::POSITION_AT])?;
        let body_end = Self::POSITION_AT + self.as_position().body_len();
        self.as_position().write_body(
            &mut out[Self::POSITION_AT..body_end],
            CompressedCs::NoData,
            CompressionType::default(),
        )?;
        // `body_end`, not `FIXED_LEN`: the compressed body is six bytes
        // shorter, so skipping the uncompressed length would leave a
        // six-byte hole and truncate the comment.
        for (slot, byte) in out.iter_mut().skip(body_end).zip(self.comment.iter()) {
            *slot = *byte;
        }
        Ok(needed)
    }
}

/// An item report (`)`): like an object but with a variable-length
/// name (3-9 characters) and no timestamp.
///
/// # Common path: one line, valid by construction
///
/// ```
/// use warble::aprs::{AprsError, Item, Latitude, Longitude, Symbol};
///
/// let item = Item::new(
///     b"AID#2",
///     Latitude::from_degrees(49.0583)?,
///     Longitude::from_degrees(-72.0292)?,
///     Symbol::RED_CROSS,
/// )?
/// .with_comment(b"first aid");
/// let mut buf = [0u8; 64];
/// let len = item.build(&mut buf)?;
/// assert!(buf[..len].starts_with(b")AID#2!"));
/// # Ok::<(), AprsError>(())
/// ```
///
/// # Power user: fully typed symbol, exhaustively matched
///
/// ```
/// use warble::aprs::{
///     AprsError, Item, Latitude, Longitude, Symbol, SymbolCode, SymbolTable,
/// };
///
/// let item = Item::new(
///     b"GATE",
///     Latitude::new(0)?,
///     Longitude::new(0)?,
///     Symbol::alternate(SymbolCode::new(b'g')?),
/// )?;
/// match item.symbol.table() {
///     Some(SymbolTable::Alternate) => {}
///     Some(SymbolTable::Primary | SymbolTable::Overlay(_)) | None => unreachable!(),
/// }
/// # Ok::<(), AprsError>(())
/// ```
///
/// # Raw hatch: out-of-spec wire bytes round-trip exactly
///
/// ```
/// use warble::aprs::{AprsError, Item, Latitude, Longitude, Symbol};
///
/// let item = Item::new(
///     b"ODD",
///     Latitude::new(0)?,
///     Longitude::new(0)?,
///     Symbol::from_wire(b'~', b'$'), // held verbatim, never rejected
/// )?
/// .with_live(false);
/// let mut buf = [0u8; 64];
/// let len = item.build(&mut buf)?;
/// assert_eq!(&buf[..len], b")ODD_0000.00N~00000.00E$");
/// assert_eq!(item.symbol.to_wire(), (b'~', b'$'));
/// # Ok::<(), AprsError>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Item<'a> {
    /// Item name: 3-9 printable-ASCII bytes; `!` and `_` are excluded
    /// because they terminate the name on the wire.
    pub name: &'a [u8],
    /// `true` for a live item (`!`), `false` for a killed one (`_`).
    pub live: bool,
    /// The item latitude.
    pub latitude: Latitude,
    /// The item longitude.
    pub longitude: Longitude,
    /// The display symbol (table + code); [`Symbol::from_wire`] is
    /// the raw hatch for out-of-spec byte pairs.
    pub symbol: Symbol,
    /// How many low-order coordinate digits the sender blanked.
    ///
    /// Chapter 6 position ambiguity: a station that does not wish to
    /// publish an exact fix replaces trailing digits with spaces, one
    /// digit being a tenth of an arc-minute of vagueness and four a
    /// whole degree.
    ///
    /// **Read the position through [`Self::coordinates`]**, which
    /// applies this to *both* axes. The latitude's blanked digits
    /// arrive as zeros, so that field already reads at the declared
    /// precision, but chapter 6 lets the longitude carry its digits in
    /// full and leaves discarding them to the receiver.
    pub ambiguity: Ambiguity,
    /// `true` when the position is chapter 9's base-91 compressed form
    /// rather than `DDMM.hh`. See [`Object::compressed`], which carries
    /// the same meaning and the same caveat about the `cs` trailer.
    pub compressed: bool,
    /// Free-text comment following the position.
    pub comment: &'a [u8],
}

impl<'a> Item<'a> {
    /// Minimum item name length.
    const NAME_MIN: usize = 3;
    /// Maximum item name length.
    const NAME_MAX: usize = 9;

    /// Creates a live item report with an empty comment, validating
    /// the name (3-9 printable-ASCII bytes, excluding the wire
    /// terminators `!` and `_`); every other part is validated by its
    /// own type. Use the `with_*` methods to adjust the live flag and
    /// comment.
    ///
    /// # Errors
    ///
    /// [`AprsError::NameLengthInvalid`] on a name outside 3-9 bytes
    /// and [`AprsError::BadNameChar`] on a non-printable or terminator
    /// name byte.
    pub fn new(
        name: &'a [u8],
        latitude: Latitude,
        longitude: Longitude,
        symbol: Symbol,
    ) -> Result<Self, AprsError> {
        if name.len() < Self::NAME_MIN || name.len() > Self::NAME_MAX {
            return Err(AprsError::NameLengthInvalid {
                len: name.len(),
                min: Self::NAME_MIN,
                max: Self::NAME_MAX,
            });
        }
        check_name_chars(name, true)?;
        Ok(Self {
            name,
            live: true,
            latitude,
            longitude,
            ambiguity: Ambiguity::EXACT,
            symbol,
            // Chapter 11's own spelling; set the field for chapter 9's.
            compressed: false,
            comment: b"",
        })
    }

    /// Returns the report with the given free-text comment.
    #[must_use]
    pub const fn with_comment(self, comment: &'a [u8]) -> Self {
        Self { comment, ..self }
    }

    /// Returns the report with the live flag set as given (`!` live
    /// when `true`, `_` killed when `false`).
    #[must_use]
    pub const fn with_live(self, live: bool) -> Self {
        Self { live, ..self }
    }

    /// The item position, pairing the `latitude` and `longitude`
    /// fields so call sites need not rely on tuple ordering.
    #[must_use]
    pub const fn coordinates(&self) -> Coordinates {
        // Masked to the declared precision, like
        // [`Position::coordinates`](super::Position::coordinates).
        // Chapter 6 lets the longitude carry its digits in full beside
        // a blanked latitude, so reading the fields directly publishes
        // a position finer than the sender claimed.
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

    /// Parses a `)` item report.
    ///
    /// # Errors
    ///
    /// [`AprsError::Truncated`] on a short field,
    /// [`AprsError::NameLengthInvalid`] on a name outside 3-9 bytes,
    /// [`AprsError::BadNameChar`] on a non-printable name byte, plus
    /// the position errors of the position module.
    pub fn parse(info: &'a [u8]) -> Result<Self, AprsError> {
        let dti = byte_at(info, 0)?;
        if dti != b')' {
            return Err(AprsError::InvalidDataType { got: dti });
        }
        // The name runs to the first '!' (live) or '_' (killed).
        let mut name_end = 1;
        let live = loop {
            match byte_at(info, name_end)? {
                b'!' => break true,
                b'_' => break false,
                byte if !(0x20..=0x7e).contains(&byte) => {
                    return Err(AprsError::BadNameChar {
                        got: byte,
                        position: name_end,
                    });
                }
                _ if name_end > Self::NAME_MAX => {
                    return Err(AprsError::NameLengthInvalid {
                        len: name_end,
                        min: Self::NAME_MIN,
                        max: Self::NAME_MAX,
                    });
                }
                _ => name_end += 1,
            }
        };
        let name = info.get(1..name_end).unwrap_or(&[]);
        if name.len() < Self::NAME_MIN || name.len() > Self::NAME_MAX {
            return Err(AprsError::NameLengthInvalid {
                len: name.len(),
                min: Self::NAME_MIN,
                max: Self::NAME_MAX,
            });
        }
        let pos_at = name_end + 1;
        if info.len() < pos_at + LATLON_LEN {
            return Err(AprsError::Truncated {
                expected: pos_at + LATLON_LEN,
                got: info.len(),
            });
        }
        // Chapter 9's compressed form is legal here too; the shared
        // body parser picks the spelling by its first byte.
        let (position, _cs, _t) = Position::parse_body(info, pos_at, false)?;
        Ok(Self {
            name,
            live,
            latitude: position.latitude,
            longitude: position.longitude,
            ambiguity: position.ambiguity,
            symbol: position.symbol,
            compressed: position.compressed,
            // As on `Object`: an item has no data-extension field, so
            // the seven bytes a position report would take for one stay
            // in the comment here.
            comment: info.get(pos_at + position.body_len()..).unwrap_or(&[]),
        })
    }

    /// This report as a bare position, so the shared body writer can
    /// spell the position field in whichever form the item carries.
    fn as_position(&self) -> Position<'a> {
        Position {
            latitude: self.latitude,
            longitude: self.longitude,
            symbol: self.symbol,
            ambiguity: self.ambiguity,
            messaging: false,
            compressed: self.compressed,
            extension: None,
            comment: b"",
        }
    }

    /// The serialized length of this report in bytes.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        1 + self.name.len() + 1 + self.as_position().body_len() + self.comment.len()
    }

    /// Serializes the report into `buf`, returning the written length.
    ///
    /// # Errors
    ///
    /// [`AprsError::NameLengthInvalid`] / [`AprsError::BadNameChar`] on
    /// a bad name and [`AprsError::BufferTooSmall`] when `buf` cannot
    /// hold the report.
    pub fn build(&self, buf: &mut [u8]) -> Result<usize, AprsError> {
        if self.name.len() < Self::NAME_MIN || self.name.len() > Self::NAME_MAX {
            return Err(AprsError::NameLengthInvalid {
                len: self.name.len(),
                min: Self::NAME_MIN,
                max: Self::NAME_MAX,
            });
        }
        check_name_chars(self.name, true)?;
        let needed = self.encoded_len();
        let max = buf.len();
        let out = buf
            .get_mut(..needed)
            .ok_or(AprsError::BufferTooSmall { needed, max })?;
        out[0] = b')';
        for (slot, byte) in out[1..].iter_mut().zip(self.name.iter()) {
            *slot = *byte;
        }
        let mut at = 1 + self.name.len();
        out[at] = if self.live { b'!' } else { b'_' };
        at += 1;
        let body = self.as_position().body_len();
        self.as_position().write_body(
            &mut out[at..at + body],
            CompressedCs::NoData,
            CompressionType::default(),
        )?;
        at += body;
        for (slot, byte) in out.iter_mut().skip(at).zip(self.comment.iter()) {
            *slot = *byte;
        }
        Ok(needed)
    }
}

/// Strips trailing space padding from a fixed-width name and validates
/// the remaining bytes; `offset` locates the field for error reporting.
fn check_name(raw: &[u8], offset: usize, width: usize) -> Result<&[u8], AprsError> {
    let mut end = raw.len();
    while end > 0 && raw.get(end - 1) == Some(&b' ') {
        end -= 1;
    }
    if end == 0 {
        return Err(AprsError::NameLengthInvalid {
            len: 0,
            min: 1,
            max: width,
        });
    }
    let name = raw.get(..end).unwrap_or(&[]);
    for (at, &byte) in name.iter().enumerate() {
        if !(0x20..=0x7e).contains(&byte) {
            return Err(AprsError::BadNameChar {
                got: byte,
                position: offset + at,
            });
        }
    }
    Ok(name)
}

/// Validates name bytes on build: printable ASCII. When
/// `exclude_terminators` (items), `!` and `_` are also rejected since
/// they would terminate the name early on the wire.
fn check_name_chars(name: &[u8], exclude_terminators: bool) -> Result<(), AprsError> {
    for (at, &byte) in name.iter().enumerate() {
        let terminator = exclude_terminators && (byte == b'!' || byte == b'_');
        if !(0x20..=0x7e).contains(&byte) || terminator {
            return Err(AprsError::BadNameChar {
                got: byte,
                position: at,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_parse_all_formats() {
        assert_eq!(
            Timestamp::parse(b"092345z", 0),
            Ok(Timestamp::DhmZulu {
                day: 9,
                hour: 23,
                minute: 45
            })
        );
        assert_eq!(
            Timestamp::parse(b"092345/", 0),
            Ok(Timestamp::DhmLocal {
                day: 9,
                hour: 23,
                minute: 45
            })
        );
        assert_eq!(
            Timestamp::parse(b"234517h", 0),
            Ok(Timestamp::Hms {
                hour: 23,
                minute: 45,
                second: 17
            })
        );
        // Offset parsing.
        assert_eq!(
            Timestamp::parse(b"xx092345z", 2),
            Ok(Timestamp::DhmZulu {
                day: 9,
                hour: 23,
                minute: 45
            })
        );
    }

    #[test]
    fn timestamp_parse_rejections() {
        // Unknown format letter.
        assert_eq!(
            Timestamp::parse(b"092345x", 0),
            Err(AprsError::BadTimestamp {
                field: b'?',
                got: i32::from(b'x')
            })
        );
        // Out-of-range fields.
        assert_eq!(
            Timestamp::parse(b"322345z", 0),
            Err(AprsError::BadTimestamp {
                field: b'D',
                got: 32
            })
        );
        assert_eq!(
            Timestamp::parse(b"092445z", 0),
            Err(AprsError::BadTimestamp {
                field: b'H',
                got: 24
            })
        );
        assert_eq!(
            Timestamp::parse(b"092360z", 0),
            Err(AprsError::BadTimestamp {
                field: b'm',
                got: 60
            })
        );
        assert_eq!(
            Timestamp::parse(b"234560h", 0),
            Err(AprsError::BadTimestamp {
                field: b'S',
                got: 60
            })
        );
        // Non-digit and truncation.
        assert_eq!(
            Timestamp::parse(b"09x345z", 0),
            Err(AprsError::BadDigit {
                got: b'x',
                position: 2
            })
        );
        assert_eq!(
            Timestamp::parse(b"092345", 0),
            Err(AprsError::Truncated {
                expected: 7,
                got: 6
            })
        );
    }

    #[test]
    fn timestamp_write_known_answer() {
        let mut out = [0u8; Timestamp::LEN];
        let ts = Timestamp::Hms {
            hour: 23,
            minute: 45,
            second: 17,
        };
        match ts.write(&mut out) {
            Ok(()) => {}
            Err(e) => panic!("{e}"),
        }
        assert_eq!(&out, b"234517h");
        // Write validates ranges too.
        let bad = Timestamp::DhmZulu {
            day: 0,
            hour: 0,
            minute: 0,
        };
        assert_eq!(
            bad.write(&mut out),
            Err(AprsError::BadTimestamp {
                field: b'D',
                got: 0
            })
        );
    }

    #[test]
    fn coordinates_pair_the_fields() {
        let object = match Object::parse(b";LEADER   *092345z4903.50N/07201.75W>") {
            Ok(o) => o,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(
            object.coordinates(),
            Coordinates::new(object.latitude, object.longitude)
        );

        let item = match Item::parse(b")AID #2!4903.50N/07201.75WA") {
            Ok(i) => i,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(
            item.coordinates(),
            Coordinates::new(item.latitude, item.longitude)
        );
    }

    #[test]
    fn name_scanner_strips_padding_and_validates() {
        assert_eq!(check_name(b"LEADER   ", 1, 9), Ok(&b"LEADER"[..]));
        assert_eq!(check_name(b"A B", 1, 9), Ok(&b"A B"[..]));
        assert_eq!(
            check_name(b"         ", 1, 9),
            Err(AprsError::NameLengthInvalid {
                len: 0,
                min: 1,
                max: 9
            })
        );
        assert_eq!(
            check_name(b"AB\x07     ", 1, 9),
            Err(AprsError::BadNameChar {
                got: 0x07,
                position: 3
            })
        );
    }

    #[test]
    fn build_name_char_rules() {
        assert_eq!(check_name_chars(b"CAB-4", false), Ok(()));
        // Items may not contain their own wire terminators.
        assert_eq!(
            check_name_chars(b"A!B", true),
            Err(AprsError::BadNameChar {
                got: b'!',
                position: 1
            })
        );
        assert_eq!(
            check_name_chars(b"A_B", true),
            Err(AprsError::BadNameChar {
                got: b'_',
                position: 1
            })
        );
        // Objects may: '_' only terminates item names.
        assert_eq!(check_name_chars(b"A_B", false), Ok(()));
        assert_eq!(
            check_name_chars(b"A\x1fB", false),
            Err(AprsError::BadNameChar {
                got: 0x1f,
                position: 1
            })
        );
    }

    #[test]
    fn dhm_hms_range_helpers() {
        assert_eq!(check_dhm(1, 0, 0), Ok(()));
        assert_eq!(check_dhm(31, 23, 59), Ok(()));
        assert_eq!(
            check_dhm(0, 0, 0),
            Err(AprsError::BadTimestamp {
                field: b'D',
                got: 0
            })
        );
        assert_eq!(check_hms(23, 59, 59), Ok(()));
        assert_eq!(
            check_hms(0, 0, 60),
            Err(AprsError::BadTimestamp {
                field: b'S',
                got: 60
            })
        );
        assert_eq!(
            check_hm(24, 0),
            Err(AprsError::BadTimestamp {
                field: b'H',
                got: 24
            })
        );
        assert_eq!(
            check_hm(0, 60),
            Err(AprsError::BadTimestamp {
                field: b'm',
                got: 60
            })
        );
    }
}
