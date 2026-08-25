//! The APRS display symbol: a typed table + code pair with a lossless
//! raw wire hatch.
//!
//! # Where the tables and the descriptions come from
//!
//! The two symbol tables (primary `/`, alternate `\`) and the overlay
//! mechanism are specified in the APRS protocol reference — see
//! [`crate::aprs`] for the edition this crate implements, and Appendix 2
//! of the 1.0.1 document for the tabulated glyph lists.
//!
//! The maintained master list, and the one that has tracked the symbol
//! set as it grew, is:
//!
//! > Bruninga, B. (WB4APR), "APRS Symbols (Icons)", master symbol list.
//! > <https://www.aprs.org/symbols/symbolsX.txt>
//!
//! Both sources carry their own descriptions, and they differ from each
//! other: `symbolsX.txt` uses a terse, column-width-constrained operator
//! shorthand (`DIGI (white center)`, `HF GATEway`), Appendix 2 uses Ian
//! Wade's fuller prose (`Digi (green star with white center)`). The
//! strings returned by [`Symbol::describe`] follow neither, being
//! normalized to plain sentence-case English naming the same glyph
//! ("Digipeater", "Police station") so a decoder's output reads
//! consistently.
//!
//! Not that this makes them original, and it is worth being exact rather
//! than flattering: of the 37 descriptions here, 20 are substantively
//! different from `symbolsX.txt`, 15 differ from it only in letter case
//! ("Ambulance" for its `AMBULANCE`), and 2 — "Motorcycle", "School" —
//! are identical, because those are simply the English words for those
//! things and no rewording would be an improvement. A one- or two-word
//! factual label for a depicted object is not somewhere to look for
//! novelty.
//!
//! These are display text; no protocol depends on them. The glyph is the
//! wire bytes, which round-trip exactly whether or not a description
//! exists.
//!
//! Only a subset of glyphs is described. An undescribed symbol returns
//! [`SymbolDescription::Unknown`] rather than a guess, and its bytes are
//! preserved either way.
//!
//! On the wire a symbol is two bytes: a *table selector* (`/` primary,
//! `\` alternate, or an overlay character selecting the alternate table)
//! and a *code* choosing the glyph within the table. The types here make
//! the valid forms structural on the common path while guaranteeing that
//! **any** two bytes seen on air round-trip exactly.
//!
//! # Common path: named constants
//!
//! ```
//! use yodel::aprs::{Symbol, SymbolDescription};
//!
//! let sym = Symbol::CAR;
//! assert_eq!(sym.describe(), SymbolDescription::Known("Car"));
//! ```
//!
//! # Power user: fully typed construction and exhaustive matching
//!
//! ```
//! use yodel::aprs::{AprsError, OverlayId, Symbol, SymbolCode, SymbolTable};
//!
//! // A digipeater glyph with a 'W' overlay on the alternate table.
//! let wide_digi = Symbol::new(
//!     SymbolTable::Overlay(OverlayId::new(b'W')?),
//!     SymbolCode::new(b'#')?,
//! );
//! assert_eq!(wide_digi.to_wire(), (b'W', b'#'));
//!
//! match wide_digi.table() {
//!     Some(SymbolTable::Primary) => unreachable!(),
//!     Some(SymbolTable::Alternate) => unreachable!(),
//!     Some(SymbolTable::Overlay(id)) => assert_eq!(id.get(), b'W'),
//!     None => unreachable!(),
//! }
//! # Ok::<(), AprsError>(())
//! ```
//!
//! # Raw hatch: out-of-spec bytes are preserved, never rejected
//!
//! ```
//! use yodel::aprs::{Symbol, SymbolDescription};
//!
//! // Bytes no spec blesses — some hardware emits them anyway.
//! let odd = Symbol::from_wire(0x01, 0xFF);
//! assert_eq!(odd.to_wire(), (0x01, 0xFF)); // exact round-trip
//! assert_eq!(odd.describe(), SymbolDescription::Unknown);
//! ```

use super::AprsError;

/// An overlay character on the alternate symbol table.
///
/// The modern APRS convention allows `A-Z` and `0-9` as overlays.
/// Lowercase letters (used by some compressed encodings) and any other
/// byte are out of spec here and representable only through the raw
/// hatch [`Symbol::from_wire`], which preserves them verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverlayId(u8);

impl OverlayId {
    /// Creates an overlay identifier from its wire byte.
    ///
    /// # Errors
    ///
    /// [`AprsError::BadOverlay`] unless the byte is `A-Z` or `0-9`.
    pub const fn new(byte: u8) -> Result<Self, AprsError> {
        match byte {
            b'0'..=b'9' | b'A'..=b'Z' => Ok(Self(byte)),
            _ => Err(AprsError::BadOverlay { got: byte }),
        }
    }

    /// The overlay character as its wire byte.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// Which symbol table a symbol comes from, with overlays modeled
/// explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolTable {
    /// `/` — the primary table.
    Primary,
    /// `\` — the alternate table, no overlay.
    Alternate,
    /// An overlay character (`0-9`, `A-Z`) selecting the alternate
    /// table with an overlay glyph.
    Overlay(OverlayId),
}

impl SymbolTable {
    /// The table selector as its wire byte.
    #[must_use]
    pub const fn to_wire(self) -> u8 {
        match self {
            SymbolTable::Primary => b'/',
            SymbolTable::Alternate => b'\\',
            SymbolTable::Overlay(id) => id.get(),
        }
    }

    /// Interprets a wire byte as a table selector.
    ///
    /// Returns `None` for bytes that are neither `/`, `\` nor a valid
    /// overlay character; such bytes are representable only through
    /// [`Symbol::from_wire`].
    #[must_use]
    pub const fn from_wire(byte: u8) -> Option<Self> {
        match byte {
            b'/' => Some(SymbolTable::Primary),
            b'\\' => Some(SymbolTable::Alternate),
            b'0'..=b'9' | b'A'..=b'Z' => match OverlayId::new(byte) {
                Ok(id) => Some(SymbolTable::Overlay(id)),
                Err(_) => None,
            },
            _ => None,
        }
    }
}

/// A symbol code: the glyph selector within a table, validated to
/// printable ASCII (`!`..=`~`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SymbolCode(u8);

impl SymbolCode {
    /// Creates a symbol code from its wire byte.
    ///
    /// # Errors
    ///
    /// [`AprsError::BadSymbolCode`] unless the byte is printable ASCII
    /// (`0x21..=0x7E`).
    pub const fn new(byte: u8) -> Result<Self, AprsError> {
        match byte {
            0x21..=0x7E => Ok(Self(byte)),
            _ => Err(AprsError::BadSymbolCode { got: byte }),
        }
    }

    /// The code as its wire byte.
    #[must_use]
    pub const fn as_byte(self) -> u8 {
        self.0
    }

    /// The code as a `char` (always printable ASCII).
    #[must_use]
    pub const fn as_char(self) -> char {
        self.0 as char
    }
}

/// The private representation: either a validated pair or the verbatim
/// out-of-spec wire bytes. One representation, no information loss.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SymbolRepr {
    /// A structurally valid table + code pair.
    Valid {
        /// The symbol table.
        table: SymbolTable,
        /// The glyph code.
        code: SymbolCode,
    },
    /// Out-of-spec bytes preserved exactly as received.
    Raw {
        /// The verbatim table selector byte.
        table: u8,
        /// The verbatim code byte.
        code: u8,
    },
}

/// A complete APRS symbol: table + code.
///
/// The common-path constructors ([`Symbol::new`], the named constants)
/// only build structurally valid symbols; the raw hatch
/// [`Symbol::from_wire`] is total and round-trips any byte pair exactly
/// through [`Symbol::to_wire`].
///
/// ```
/// use yodel::aprs::{Symbol, SymbolDescription};
///
/// let sym = Symbol::WEATHER_STATION;
/// assert_eq!(sym.to_wire(), (b'/', b'_'));
/// assert_eq!(sym.describe(), SymbolDescription::Known("Weather station"));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Symbol {
    repr: SymbolRepr,
}

/// The human meaning of a symbol, explicit about the limits of the chart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolDescription {
    /// A symbol with a well-known community meaning.
    Known(&'static str),
    /// Structurally valid but not in the curated chart, or raw
    /// out-of-spec bytes. Never invented, never an error.
    Unknown,
}

/// Builds a standard symbol from literal bytes (crate-internal, for the
/// named constants; the bytes must already satisfy the invariants).
const fn standard(table: SymbolTable, code: u8) -> Symbol {
    Symbol {
        repr: SymbolRepr::Valid {
            table,
            code: SymbolCode(code),
        },
    }
}

impl Symbol {
    /// `/` `>` — car.
    pub const CAR: Self = standard(SymbolTable::Primary, b'>');
    /// `/` `-` — house (home station).
    pub const HOUSE: Self = standard(SymbolTable::Primary, b'-');
    /// `/` `[` — jogger / person on foot.
    pub const JOGGER: Self = standard(SymbolTable::Primary, b'[');
    /// `/` `b` — bicycle.
    pub const BICYCLE: Self = standard(SymbolTable::Primary, b'b');
    /// `/` `<` — motorcycle.
    pub const MOTORCYCLE: Self = standard(SymbolTable::Primary, b'<');
    /// `/` `k` — truck.
    pub const TRUCK: Self = standard(SymbolTable::Primary, b'k');
    /// `/` `U` — bus.
    pub const BUS: Self = standard(SymbolTable::Primary, b'U');
    /// `/` `Y` — sailboat.
    pub const BOAT: Self = standard(SymbolTable::Primary, b'Y');
    /// `/` `O` — balloon.
    pub const BALLOON: Self = standard(SymbolTable::Primary, b'O');
    /// `/` `'` — small aircraft.
    pub const AIRCRAFT: Self = standard(SymbolTable::Primary, b'\'');
    /// `/` `X` — helicopter.
    pub const HELICOPTER: Self = standard(SymbolTable::Primary, b'X');
    /// `/` `#` — digipeater.
    pub const DIGI: Self = standard(SymbolTable::Primary, b'#');
    /// `/` `&` — gateway station (HF gateway / igate).
    pub const IGATE: Self = standard(SymbolTable::Primary, b'&');
    /// `/` `_` — weather station.
    pub const WEATHER_STATION: Self = standard(SymbolTable::Primary, b'_');
    /// `/` `/` — dot (small red marker).
    pub const DOT: Self = standard(SymbolTable::Primary, b'/');
    /// `/` `;` — campground / tent.
    pub const CAMPGROUND: Self = standard(SymbolTable::Primary, b';');
    /// `/` `;` — tent (alias of [`Symbol::CAMPGROUND`]).
    pub const TENT: Self = Self::CAMPGROUND;
    /// `/` `a` — ambulance.
    pub const AMBULANCE: Self = standard(SymbolTable::Primary, b'a');
    /// `/` `:` — fire (fire station / fire scene).
    pub const FIRE_STATION: Self = standard(SymbolTable::Primary, b':');
    /// `/` `!` — police station.
    pub const POLICE: Self = standard(SymbolTable::Primary, b'!');
    /// `/` `$` — telephone.
    pub const PHONE: Self = standard(SymbolTable::Primary, b'$');
    /// `\` `S` — satellite.
    pub const SATELLITE: Self = standard(SymbolTable::Alternate, b'S');
    /// `/` `+` — red cross.
    pub const RED_CROSS: Self = standard(SymbolTable::Primary, b'+');

    /// Builds a standard (structurally valid) symbol from typed parts.
    #[must_use]
    pub const fn new(table: SymbolTable, code: SymbolCode) -> Self {
        Symbol {
            repr: SymbolRepr::Valid { table, code },
        }
    }

    /// Builds a primary-table symbol.
    #[must_use]
    pub const fn primary(code: SymbolCode) -> Self {
        Self::new(SymbolTable::Primary, code)
    }

    /// Builds an alternate-table symbol (no overlay).
    #[must_use]
    pub const fn alternate(code: SymbolCode) -> Self {
        Self::new(SymbolTable::Alternate, code)
    }

    /// Builds an overlay symbol on the alternate table.
    #[must_use]
    pub const fn overlay(id: OverlayId, code: SymbolCode) -> Self {
        Self::new(SymbolTable::Overlay(id), code)
    }

    /// The infallible raw hatch: accepts **any** two bytes.
    ///
    /// In-spec bytes normalize into the validated representation;
    /// everything else is preserved verbatim, so out-of-spec traffic
    /// (digipeating, logging, forensics) is never rejected or mangled.
    #[must_use]
    pub const fn from_wire(table: u8, code: u8) -> Self {
        match (SymbolTable::from_wire(table), SymbolCode::new(code)) {
            (Some(t), Ok(c)) => Self::new(t, c),
            (Some(_), Err(_)) | (None, Ok(_)) | (None, Err(_)) => Symbol {
                repr: SymbolRepr::Raw { table, code },
            },
        }
    }

    /// The exact wire bytes: the inverse of [`Symbol::from_wire`] for
    /// **all** inputs — `Symbol::from_wire(t, c).to_wire() == (t, c)`
    /// for every byte pair.
    #[must_use]
    pub const fn to_wire(self) -> (u8, u8) {
        match self.repr {
            SymbolRepr::Valid { table, code } => (table.to_wire(), code.as_byte()),
            SymbolRepr::Raw { table, code } => (table, code),
        }
    }

    /// The typed table, or `None` for a raw out-of-spec symbol.
    #[must_use]
    pub const fn table(self) -> Option<SymbolTable> {
        match self.repr {
            SymbolRepr::Valid { table, code: _ } => Some(table),
            SymbolRepr::Raw { .. } => None,
        }
    }

    /// The typed code, or `None` for a raw out-of-spec symbol.
    #[must_use]
    pub const fn code(self) -> Option<SymbolCode> {
        match self.repr {
            SymbolRepr::Valid { table: _, code } => Some(code),
            SymbolRepr::Raw { .. } => None,
        }
    }

    /// The human meaning from a curated chart of well-known community
    /// symbols. Total: unknown, experimental or raw symbols describe as
    /// [`SymbolDescription::Unknown`], never an error.
    ///
    /// An overlay symbol is described by its alternate-table glyph; the
    /// overlay character itself does not change the base meaning.
    #[must_use]
    pub const fn describe(self) -> SymbolDescription {
        match self.repr {
            SymbolRepr::Valid { table, code } => {
                let alternate = match table {
                    SymbolTable::Primary => false,
                    SymbolTable::Alternate | SymbolTable::Overlay(_) => true,
                };
                describe_glyph(alternate, code.as_byte())
            }
            SymbolRepr::Raw { .. } => SymbolDescription::Unknown,
        }
    }
}

// ---------------------------------------------------------------------
// Chapter 20: the symbol carried in the AX.25 address fields
// ---------------------------------------------------------------------

/// Decodes a generic-destination `xy` mnemonic into a table + code.
///
/// Chapter 20 spells a symbol that cannot ride in the information field
/// as two characters `xy` in the destination callsign. Appendix 2
/// tabulates all 188 of them (94 codes × 2 tables) and the table looks
/// arbitrary, but it is not: it is **seven contiguous runs per table**,
/// keyed by `x`, over which `y` advances in step with the symbol code.
/// This crate therefore needs no 188-entry chart — only the run
/// endpoints, which were re-derived from Appendix 2 and checked against
/// every published row (`tests/symbol_from_address.rs` keeps the chart
/// itself, transcribed, as the oracle):
///
/// | codes | count | primary `x` | alternate `x` | `y` runs over |
/// |---|---|---|---|---|
/// | `!`..=`/` | 15 | `B` | `O` | `B`..=`P` |
/// | `0`..=`9` | 10 | `P` | `A` | `0`..=`9` |
/// | `:`..=`@` | 7 | `M` | `N` | `R`..=`X` |
/// | `A`..=`Z` | 26 | `P` | `A` | `A`..=`Z` |
/// | `[`..=`` ` `` | 6 | `H` | `D` | `S`..=`X` |
/// | `a`..=`z` | 26 | `L` | `S` | `A`..=`Z` |
/// | `{`..=`~` | 4 | `J` | `Q` | `1`..=`4` |
///
/// The twelve leading letters `B P M H L J` (primary) and `O A N D S Q`
/// (alternate) are **disjoint**, so `x` alone names the table and `y`
/// alone names the position within the run: the pair is decodable with
/// no ambiguity and no lookup. `P` and `A` each cover two runs, and on
/// both of them the mapping is the identity — the mnemonic's second
/// character *is* the symbol code.
///
/// Returns `(alternate_table, code)`, or `None` when `x` is not one of
/// the twelve leading letters or `y` falls outside its run.
const fn mnemonic(x: u8, y: u8) -> Option<(bool, u8)> {
    // (alternate table, first code of the run, first `y` of the run,
    //  length of the run)
    let (alternate, first_code, first_y, len) = match x {
        b'B' => (false, b'!', b'B', 15),
        b'O' => (true, b'!', b'B', 15),
        b'M' => (false, b':', b'R', 7),
        b'N' => (true, b':', b'R', 7),
        b'H' => (false, b'[', b'S', 6),
        b'D' => (true, b'[', b'S', 6),
        b'L' => (false, b'a', b'A', 26),
        b'S' => (true, b'a', b'A', 26),
        b'J' => (false, b'{', b'1', 4),
        b'Q' => (true, b'{', b'1', 4),
        // The two identity runs, `0`-`9` and `A`-`Z`, share a leading
        // letter and need no arithmetic at all.
        b'P' | b'A' => {
            return match y {
                b'0'..=b'9' | b'A'..=b'Z' => Some((x == b'A', y)),
                _ => None,
            };
        }
        _ => return None,
    };
    if y < first_y {
        return None;
    }
    let offset = y - first_y;
    if offset >= len {
        return None;
    }
    Some((alternate, first_code + offset))
}

/// The symbol named by a generic APRS **destination** callsign
/// (chapter 20, *Symbols in the AX.25 Destination Address*).
///
/// # Why a symbol lives in an address at all
///
/// Chapter 8 is blunt about it: raw NMEA beaconing "was a hack for early
/// trackers with inadequate computing resources", and "symbols had to go
/// in the destination field using names like `GPSxxx`". A raw GPS
/// sentence has nowhere to put a table + code pair, so the destination
/// callsign — which APRS does not use for routing — carries it instead.
/// Traffic like this is still on the air, so a decoder that only ever
/// looks in the information field silently drops the icon.
///
/// # Wire layout
///
/// The AX.25 destination is six characters, space padded. Two spellings
/// are accepted, and chapter 20 states they are interchangeable:
///
/// * `GPSxyz`, `SPCxyz`, `SYMxyz` — `xy` is the Appendix 2 mnemonic and
///   `z` is an overlay character or the space filler. `GPS` is the
///   general-purpose prefix, `SPC` is for special events and `SYM` is
///   reserved; all three name the same symbols. Appendix 2's 188
///   mnemonics are not an arbitrary chart: they are seven contiguous
///   runs per table with disjoint leading letters, so `x` alone names
///   the table and `y` the position within the run, and this decodes
///   with arithmetic rather than a lookup.
/// * `GPSCnn`, `GPSEnn` — `nn` is the two-digit Appendix 2 row number
///   `01`..=`94`, `C` selecting the primary table and `E` the alternate.
///   Row `nn` is code `b'!' + (nn - 1)`, so `GPSC12` is `/,` and
///   `GPSC30` is `/>`. Chapter 20 states outright that these two
///   "can not have overlays", and the numeric form is spelled only with
///   the `GPS` prefix, so `SPCC12` is not a symbol here.
///
/// Trailing spaces are stripped, so both the padded six-byte field and
/// the trimmed [`crate::ax25::Callsign::as_bytes`] form are accepted.
///
/// # Overlays
///
/// A non-space `z` is an overlay character (`0`-`9` or `A`-`Z`) and, as
/// everywhere else in APRS, an overlay implies the alternate table:
/// `GPSNV3` is the alternate car overlaid with `3`. Chapter 20 says
/// "none of the symbols in the Primary Symbol Table can be overlaid", so
/// a **primary** mnemonic carrying a non-space `z` is contradictory
/// (`GPSMV3` asks for a primary symbol *and* an overlay) and decodes to
/// `None` rather than to a guess about which half the sender meant. The
/// overlay is not checked against Appendix 2's "overlay capable" column:
/// that column is a hint about how the icon renders, and discarding an
/// overlay the sender transmitted would change the icon.
///
/// # Precedence
///
/// **A destination symbol is only correct when the information field has
/// none.** See [`resolve`], which applies chapter 20's ordering so a
/// caller cannot get it backwards.
///
/// This lookup does **not** apply to Mic-E frames (data-type identifier
/// `` ` `` or `'`): there the destination address encodes latitude, not
/// a mnemonic, and Mic-E always carries its own symbol in the
/// information field ([`super::mic_e::decode`]).
///
/// # Examples
///
/// ```
/// use yodel::aprs::symbol::from_destination;
/// use yodel::aprs::Symbol;
///
/// // Chapter 20: GPSBM, SPCBM, SYMBM and GPSC12 are all "Boy Scouts".
/// assert_eq!(from_destination(b"GPSBM").map(Symbol::to_wire), Some((b'/', b',')));
/// assert_eq!(from_destination(b"SYMBM ").map(Symbol::to_wire), Some((b'/', b',')));
/// assert_eq!(from_destination(b"GPSC12").map(Symbol::to_wire), Some((b'/', b',')));
/// // ...and GPSOM / GPSE12 are the alternate-table "Girl Scouts".
/// assert_eq!(from_destination(b"GPSE12").map(Symbol::to_wire), Some((b'\\', b',')));
/// // A car overlaid with the digit 3.
/// assert_eq!(from_destination(b"GPSNV3").map(Symbol::to_wire), Some((b'3', b'>')));
/// // Ordinary destinations name no symbol.
/// assert_eq!(from_destination(b"APRS"), None);
/// assert_eq!(from_destination(b"BEACON"), None);
/// ```
#[must_use]
pub const fn from_destination(callsign: &[u8]) -> Option<Symbol> {
    // AX.25 pads addresses to six characters; chapter 20 calls the
    // trailing space "a filler character", so it carries no meaning.
    let mut len = callsign.len();
    while len > 0 && callsign[len - 1] == b' ' {
        len -= 1;
    }
    // `GPSxy` is the shortest form and `GPSxyz` the longest; an AX.25
    // address cannot exceed six characters in any case.
    if len < 5 || len > 6 {
        return None;
    }

    let generic = matches!(
        (callsign[0], callsign[1], callsign[2]),
        (b'G', b'P', b'S') | (b'S', b'P', b'C') | (b'S', b'Y', b'M')
    );
    if !generic {
        return None;
    }

    // `GPSCnn` / `GPSEnn`, the numeric spelling. `C` and `E` are not
    // leading letters of any `xy` mnemonic, so trying this first cannot
    // shadow the mnemonic form.
    if len == 6 && callsign[0] == b'G' {
        let numeric_table = match callsign[3] {
            b'C' => Some(SymbolTable::Primary),
            b'E' => Some(SymbolTable::Alternate),
            _ => None,
        };
        if let Some(table) = numeric_table {
            let (tens, ones) = (callsign[4], callsign[5]);
            if tens.is_ascii_digit() && ones.is_ascii_digit() {
                let nn = (tens - b'0') * 10 + (ones - b'0');
                // Appendix 2 numbers its rows 01..=94.
                if nn >= 1 && nn <= 94 {
                    return Some(standard(table, b'!' + (nn - 1)));
                }
            }
            return None;
        }
    }

    let (alternate, code) = match mnemonic(callsign[3], callsign[4]) {
        Some(pair) => pair,
        None => return None,
    };
    // A five-character address is `GPSxy` with the `z` slot padded away.
    let z = if len == 6 { callsign[5] } else { b' ' };
    let table = if alternate {
        match OverlayId::new(z) {
            Ok(id) => SymbolTable::Overlay(id),
            // Not an overlay character: only the space filler is still
            // in spec, and anything else makes the address unreadable.
            Err(_) if z == b' ' => SymbolTable::Alternate,
            Err(_) => return None,
        }
    } else if z == b' ' {
        SymbolTable::Primary
    } else {
        // Chapter 20: no primary-table symbol can be overlaid.
        return None;
    };
    Some(standard(table, code))
}

/// The symbol named by a non-zero **source** SSID (chapter 20, *Symbol
/// in the Source Address SSID*).
///
/// # Wire layout
///
/// The SSID is the low four bits of the seventh source-address octet, so
/// it is a plain `0..=15` — [`crate::ax25::Ssid::value`]. Chapter 20
/// gives fifteen of those sixteen values a fixed icon, all from the
/// **primary** table; SSID 0 is "no icon" and is the conventional
/// default, so it is the one value that means nothing here.
///
/// | SSID | icon | | SSID | icon |
/// |---|---|---|---|---|
/// | -1 | `/a` ambulance | | -9 | `/>` car |
/// | -2 | `/U` bus | | -10 | `/<` motorcycle |
/// | -3 | `/f` fire truck | | -11 | `/O` balloon |
/// | -4 | `/b` bicycle | | -12 | `/j` jeep |
/// | -5 | `` /Y `` yacht | | -13 | `/R` recreational vehicle |
/// | -6 | `/X` helicopter | | -14 | `/k` truck |
/// | -7 | `/'` small aircraft | | -15 | `/v` van |
/// | -8 | `/s` ship | | | |
///
/// # Precedence
///
/// This is the **last** resort of the three chapter 20 lists, and it is
/// the one most often applied wrongly: an SSID is just a station's
/// second radio to nearly every operator, so reading an icon out of one
/// that carries a symbol elsewhere puts a bus or a motorcycle on the map
/// for no reason. Use it only when the information field and the
/// destination address both name nothing — which is what [`resolve`]
/// enforces. Chapter 20 intends it for "stand-alone trackers where
/// there is no other method", i.e. raw NMEA beacons.
///
/// Returns `None` for SSID 0 and for any value above 15 (which no AX.25
/// address can produce).
///
/// # Examples
///
/// ```
/// use yodel::aprs::symbol::from_source_ssid;
/// use yodel::aprs::Symbol;
///
/// assert_eq!(from_source_ssid(9), Some(Symbol::CAR));
/// assert_eq!(from_source_ssid(14), Some(Symbol::TRUCK));
/// assert_eq!(from_source_ssid(0), None); // "no icon", not a symbol
/// ```
#[must_use]
pub const fn from_source_ssid(ssid: u8) -> Option<Symbol> {
    /// Chapter 20's table, indexed by SSID. Index 0 is the "no icon"
    /// row and is never read; it is present so the index is the SSID.
    const CODES: [u8; 16] = [
        b' ',  // -0  no icon
        b'a',  // -1  ambulance
        b'U',  // -2  bus
        b'f',  // -3  fire truck
        b'b',  // -4  bicycle
        b'Y',  // -5  yacht (sailboat)
        b'X',  // -6  helicopter
        b'\'', // -7  small aircraft
        b's',  // -8  ship (power boat)
        b'>',  // -9  car
        b'<',  // -10 motorcycle
        b'O',  // -11 balloon
        b'j',  // -12 jeep
        b'R',  // -13 recreational vehicle
        b'k',  // -14 truck
        b'v',  // -15 van
    ];
    if ssid == 0 || ssid > 15 {
        return None;
    }
    Some(standard(SymbolTable::Primary, CODES[ssid as usize]))
}

/// Chapter 20's *Symbol Precedence* rule, applied once so no caller can
/// apply it backwards.
///
/// A frame can, erroneously, carry three different symbols at once —
/// chapter 20 gives the worked example `G3NRW-7>GPSMV:!0123.45N/01234.56Wj`,
/// which claims a small aircraft, a car and a jeep simultaneously. The
/// rule that settles it is:
///
/// 1. the symbol in the **information field** wins outright;
/// 2. failing that, the symbol in the **destination address**
///    ([`from_destination`]);
/// 3. failing that, the symbol in the **source SSID**
///    ([`from_source_ssid`]).
///
/// Getting that order wrong does not fail loudly — it draws a plausible
/// wrong icon on a map — which is exactly why it is worth spending a
/// function on.
///
/// # Arguments
///
/// * `information` — the symbol the information field carries, if any:
///   [`super::Position::symbol`], [`super::Object::symbol`] and friends.
///   Pass `None` only when the format has no symbol field (a raw NMEA
///   sentence, a status report), **not** when a symbol was present but
///   failed to parse; falling through in that case would promote an
///   unrelated address symbol over the real one.
/// * `destination` — the destination callsign text, as
///   [`crate::ax25::Callsign::as_bytes`] returns it.
/// * `source_ssid` — the source address's SSID, `0..=15`.
///
/// # Mic-E
///
/// Do not call this for Mic-E frames (data-type identifier `` ` `` or
/// `'`). Their destination address is packed latitude and could spell
/// a mnemonic by coincidence; the symbol always comes from
/// [`super::mic_e::decode`] instead.
///
/// # Examples
///
/// ```
/// use yodel::aprs::symbol::resolve;
/// use yodel::aprs::Symbol;
///
/// // Chapter 20's three-symbol example: the information field wins.
/// let jeep = Symbol::from_wire(b'/', b'j');
/// assert_eq!(resolve(Some(jeep), b"GPSMV", 7), Some(jeep));
/// // No information-field symbol: the destination is next.
/// assert_eq!(resolve(None, b"GPSMV", 7), Some(Symbol::CAR));
/// // Neither: the source SSID is the last resort.
/// assert_eq!(resolve(None, b"APRS", 7), Some(Symbol::AIRCRAFT));
/// // Nothing anywhere.
/// assert_eq!(resolve(None, b"APRS", 0), None);
/// ```
#[must_use]
pub const fn resolve(
    information: Option<Symbol>,
    destination: &[u8],
    source_ssid: u8,
) -> Option<Symbol> {
    match information {
        Some(symbol) => Some(symbol),
        None => match from_destination(destination) {
            Some(symbol) => Some(symbol),
            None => from_source_ssid(source_ssid),
        },
    }
}

/// The curated chart lookup: `alternate` selects the table, `code` the
/// glyph. Descriptions are short original wordings of the well-known
/// community meanings; anything else is `Unknown`.
const fn describe_glyph(alternate: bool, code: u8) -> SymbolDescription {
    let known = if alternate {
        match code {
            b'#' => "Digipeater (alternate)",
            b'&' => "Gateway / igate",
            b'S' => "Satellite",
            b'_' => "Weather site",
            _ => return SymbolDescription::Unknown,
        }
    } else {
        match code {
            b'!' => "Police station",
            b'#' => "Digipeater",
            b'$' => "Telephone",
            b'&' => "Gateway station (HF gateway / igate)",
            b'\'' => "Small aircraft",
            b'(' => "Mobile satellite station",
            b'+' => "Red cross",
            b'-' => "House",
            b'/' => "Dot",
            b':' => "Fire",
            b';' => "Campground / tent",
            b'<' => "Motorcycle",
            b'=' => "Railroad engine",
            b'>' => "Car",
            b'A' => "Aid station",
            b'K' => "School",
            b'O' => "Balloon",
            b'R' => "Recreational vehicle",
            b'U' => "Bus",
            b'X' => "Helicopter",
            b'Y' => "Sailboat",
            b'[' => "Jogger",
            b'_' => "Weather station",
            b'a' => "Ambulance",
            b'b' => "Bicycle",
            b'f' => "Fire truck",
            b'h' => "Hospital",
            b'j' => "Jeep",
            b'k' => "Truck",
            b'r' => "Repeater tower",
            b's' => "Power boat / ship",
            b'u' => "Semi-trailer truck",
            b'v' => "Van",
            _ => return SymbolDescription::Unknown,
        }
    };
    SymbolDescription::Known(known)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_wire_to_wire_round_trips_every_pair() {
        for table in 0..=u8::MAX {
            for code in 0..=u8::MAX {
                let sym = Symbol::from_wire(table, code);
                assert_eq!(sym.to_wire(), (table, code));
            }
        }
    }

    #[test]
    fn overlay_validation_edges() {
        assert!(OverlayId::new(b'A').is_ok());
        assert!(OverlayId::new(b'Z').is_ok());
        assert!(OverlayId::new(b'0').is_ok());
        assert!(OverlayId::new(b'9').is_ok());
        // Neighbors of the valid ranges are rejected.
        for byte in [
            b'A' - 1,
            b'Z' + 1,
            b'0' - 1,
            b'9' + 1,
            b'a',
            b'z',
            b'/',
            b'\\',
            0x00,
            0xFF,
        ] {
            assert_eq!(
                OverlayId::new(byte),
                Err(AprsError::BadOverlay { got: byte })
            );
        }
    }

    #[test]
    fn symbol_code_validation_edges() {
        assert!(SymbolCode::new(0x21).is_ok());
        assert!(SymbolCode::new(0x7E).is_ok());
        for byte in [0x20, 0x7F, 0x00, 0xFF] {
            assert_eq!(
                SymbolCode::new(byte),
                Err(AprsError::BadSymbolCode { got: byte })
            );
        }
    }

    #[test]
    fn table_wire_round_trip() {
        for byte in 0..=u8::MAX {
            if let Some(table) = SymbolTable::from_wire(byte) {
                assert_eq!(table.to_wire(), byte);
            }
        }
        assert_eq!(SymbolTable::from_wire(b'/'), Some(SymbolTable::Primary));
        assert_eq!(SymbolTable::from_wire(b'\\'), Some(SymbolTable::Alternate));
        assert_eq!(SymbolTable::from_wire(b'a'), None);
    }

    #[test]
    fn every_named_constant_is_standard_and_known() {
        let constants = [
            Symbol::CAR,
            Symbol::HOUSE,
            Symbol::JOGGER,
            Symbol::BICYCLE,
            Symbol::MOTORCYCLE,
            Symbol::TRUCK,
            Symbol::BUS,
            Symbol::BOAT,
            Symbol::BALLOON,
            Symbol::AIRCRAFT,
            Symbol::HELICOPTER,
            Symbol::DIGI,
            Symbol::IGATE,
            Symbol::WEATHER_STATION,
            Symbol::DOT,
            Symbol::CAMPGROUND,
            Symbol::TENT,
            Symbol::AMBULANCE,
            Symbol::FIRE_STATION,
            Symbol::POLICE,
            Symbol::PHONE,
            Symbol::SATELLITE,
            Symbol::RED_CROSS,
        ];
        for sym in constants {
            assert!(sym.table().is_some(), "constant is not standard: {sym:?}");
            assert!(sym.code().is_some(), "constant is not standard: {sym:?}");
            assert!(
                matches!(sym.describe(), SymbolDescription::Known(_)),
                "constant is not Known: {sym:?}"
            );
            // Constants normalize through the raw hatch to themselves.
            let (t, c) = sym.to_wire();
            assert_eq!(Symbol::from_wire(t, c), sym);
        }
        assert_eq!(Symbol::CAR.to_wire(), (b'/', b'>'));
        assert_eq!(Symbol::HOUSE.to_wire(), (b'/', b'-'));
        assert_eq!(Symbol::DIGI.to_wire(), (b'/', b'#'));
        assert_eq!(Symbol::WEATHER_STATION.to_wire(), (b'/', b'_'));
    }

    #[test]
    fn describe_is_total() {
        // Every byte pair describes without error; raw pairs are Unknown.
        for table in 0..=u8::MAX {
            for code in [0u8, b' ', b'!', b'>', b'~', 0x7F, 0xFF] {
                let d = Symbol::from_wire(table, code).describe();
                match d {
                    SymbolDescription::Known(s) => assert!(!s.is_empty()),
                    SymbolDescription::Unknown => {}
                }
            }
        }
        assert_eq!(
            Symbol::from_wire(0x01, 0xFF).describe(),
            SymbolDescription::Unknown
        );
        // A structurally valid but uncharted symbol is Unknown, not an error.
        assert_eq!(
            Symbol::from_wire(b'/', b'~').describe(),
            SymbolDescription::Unknown
        );
        // Overlay symbols describe by their alternate-table glyph.
        assert_eq!(
            Symbol::from_wire(b'W', b'#').describe(),
            SymbolDescription::Known("Digipeater (alternate)")
        );
    }

    #[test]
    fn accessors_expose_typed_parts() {
        let sym = Symbol::from_wire(b'3', b'>');
        match sym.table() {
            Some(SymbolTable::Overlay(id)) => assert_eq!(id.get(), b'3'),
            Some(SymbolTable::Primary) | Some(SymbolTable::Alternate) | None => {
                panic!("expected an overlay table")
            }
        }
        let code = sym.code();
        assert!(code.is_some());
        if let Some(c) = code {
            assert_eq!(c.as_byte(), b'>');
            assert_eq!(c.as_char(), '>');
        }
        let raw = Symbol::from_wire(0x00, b'>');
        assert_eq!(raw.table(), None);
        assert_eq!(raw.code(), None);
    }
}
