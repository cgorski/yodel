//! APRS object and item reports (`;` and `)`).
//!
//! An **object** (APRS 1.01 chapter 11) is `;` + a 9-character
//! space-padded name + `*` (live) or `_` (killed) + a 7-character
//! timestamp + an uncompressed position + optional comment. An
//! **item** is `)` + a 3-9 character name + `!` (live) or `_` (killed)
//! + an uncompressed position + comment; items carry no timestamp.

use super::AprsError;
use super::position::{
    LATLON_LEN, LatLonBlock, byte_at, parse_digits, parse_latlon, write_digits, write_latlon,
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
    /// Free-text comment following the position.
    pub comment: &'a [u8],
}

impl<'a> Object<'a> {
    /// Wire length of the object name field.
    const NAME_LEN: usize = 9;
    /// Fixed body length: DTI + name + live/killed + timestamp +
    /// position block.
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
        if info.len() < Self::FIXED_LEN {
            return Err(AprsError::Truncated {
                expected: Self::FIXED_LEN,
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
        let block = parse_latlon(info, 2 + Self::NAME_LEN + Timestamp::LEN)?;
        Ok(Self {
            name,
            live,
            timestamp,
            latitude: block.latitude,
            longitude: block.longitude,
            ambiguity: block.ambiguity,
            symbol: block.symbol,
            comment: info.get(Self::FIXED_LEN..).unwrap_or(&[]),
        })
    }

    /// The serialized length of this report in bytes.
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        Self::FIXED_LEN + self.comment.len()
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
            .write(&mut out[2 + Self::NAME_LEN..2 + Self::NAME_LEN + Timestamp::LEN])?;
        write_latlon(
            &mut out[2 + Self::NAME_LEN + Timestamp::LEN..Self::FIXED_LEN],
            &LatLonBlock {
                latitude: self.latitude,
                longitude: self.longitude,
                symbol: self.symbol,
                ambiguity: self.ambiguity,
            },
        );
        for (slot, byte) in out
            .iter_mut()
            .skip(Self::FIXED_LEN)
            .zip(self.comment.iter())
        {
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
        let block = parse_latlon(info, pos_at)?;
        Ok(Self {
            name,
            live,
            latitude: block.latitude,
            longitude: block.longitude,
            ambiguity: block.ambiguity,
            symbol: block.symbol,
            comment: info.get(pos_at + LATLON_LEN..).unwrap_or(&[]),
        })
    }

    /// The serialized length of this report in bytes.
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        1 + self.name.len() + 1 + LATLON_LEN + self.comment.len()
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
        write_latlon(
            &mut out[at..at + LATLON_LEN],
            &LatLonBlock {
                latitude: self.latitude,
                longitude: self.longitude,
                symbol: self.symbol,
                ambiguity: self.ambiguity,
            },
        );
        at += LATLON_LEN;
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
