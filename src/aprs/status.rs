//! APRS status reports (`>`).
//!
//! A status report is the `>` identifier followed by free text
//! (chapter 16). [`Status::text`] is that text verbatim, which is what
//! makes the wire round trip exact.
//!
//! Chapter 16 also names **three** structured things that hide inside
//! the text, and this module exposes each as a *view* over `text`
//! rather than as a stored field:
//!
//! * a leading `ddhhmmz` timestamp ([`Status::timestamp`]) — DHM zulu
//!   only, which the specification is explicit about;
//! * a leading Maidenhead grid locator and symbol
//!   ([`Status::grid`]) — the form HF and EMCOMM stations use to say
//!   where they are without a position report;
//! * a trailing `^HP` beam heading and effective radiated power
//!   ([`Status::beam`]), for meteor-scatter work.
//!
//! # Why views and not fields
//!
//! Keeping the text as the single carrier means [`Status::build`] is
//! still "write `>`, then the bytes", so byte-exactness cannot regress
//! and no caller has to migrate. The structured readings are derived on
//! demand and cost nothing when unused — the same treatment comment
//! fields get: keep the bytes, parse lazily, never widen the struct.
//!
//! [`Status::message`] is the complement: the human-readable remainder
//! with whichever of the three were recognised removed.

use super::AprsError;
use super::object::Timestamp;
use super::symbol::{Symbol, SymbolTable};
use crate::geo::MaidenheadGrid;
use crate::units::{Bearing, Power};

/// A beam heading and effective radiated power, from the `^HP` trailer.
///
/// Both characters are coarse by design — the point of the field is to
/// cost two bytes in a meteor-scatter exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BeamHeading {
    /// Beam heading, in whole tens of degrees.
    pub heading: Bearing,
    /// Effective radiated power.
    pub erp: Power,
}

/// A Maidenhead locator and the symbol that follows it, as a status
/// report spells them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusGrid {
    /// The locator, 4 or 6 characters.
    pub grid: MaidenheadGrid,
    /// The symbol table identifier and code that follow it.
    pub symbol: Symbol,
}

/// A status report: the `>` data type identifier plus free text.
///
/// # Wire round trip
///
/// The wire form is the single `>` identifier followed by the verbatim
/// text; `encoded_len` is therefore always `1 + text.len()`:
///
/// ```
/// use warble::aprs::{AprsError, Status};
///
/// let status = Status { text: b"Net Control Center" };
/// assert_eq!(status.encoded_len(), 1 + 18);
///
/// let mut buf = [0u8; 32];
/// let len = status.build(&mut buf)?;
/// assert_eq!(&buf[..len], b">Net Control Center");
/// assert_eq!(Status::parse(&buf[..len])?, status);
///
/// // A wrong identifier is a typed error, never a panic.
/// assert_eq!(
///     Status::parse(b"!x"),
///     Err(AprsError::InvalidDataType { got: b'!' })
/// );
/// # Ok::<(), AprsError>(())
/// ```
///
/// # Reading the structured parts
///
/// ```
/// use warble::aprs::{AprsError, Status};
///
/// // Chapter 16's own example: beam heading 110 degrees, ERP 490 W.
/// let status = Status { text: b"Hello^B7" };
/// let beam = status.beam().expect("a ^HP trailer");
/// assert_eq!(beam.heading.degrees(), 110);
/// assert_eq!(beam.erp.watts(), 490);
/// assert_eq!(status.message(), b"Hello");
///
/// // A locator, its symbol, and the text after the mandatory space.
/// let status = Status { text: b"IO91SX/G Hello world" };
/// let located = status.grid().expect("a locator");
/// // Stored canonically: fields upper case, subsquares lower.
/// assert_eq!(located.grid.as_bytes(), b"IO91sx");
/// assert_eq!(located.symbol.to_wire(), (b'/', b'G'));
/// assert_eq!(status.message(), b"Hello world");
/// # Ok::<(), AprsError>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status<'a> {
    /// The status text (everything after `>`).
    pub text: &'a [u8],
}

impl<'a> Status<'a> {
    /// Parses a `>` status report.
    ///
    /// # Errors
    ///
    /// [`AprsError::Truncated`] on an empty field and
    /// [`AprsError::InvalidDataType`] when the identifier is not `>`.
    pub fn parse(info: &'a [u8]) -> Result<Self, AprsError> {
        let dti = *info.first().ok_or(AprsError::Truncated {
            expected: 1,
            got: 0,
        })?;
        if dti != b'>' {
            return Err(AprsError::InvalidDataType { got: dti });
        }
        Ok(Self {
            text: info.get(1..).unwrap_or(&[]),
        })
    }

    /// The serialized length of this report in bytes.
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        1 + self.text.len()
    }

    /// Serializes the report into `buf`, returning the written length.
    ///
    /// # Errors
    ///
    /// [`AprsError::BufferTooSmall`] when `buf` cannot hold the report.
    pub fn build(&self, buf: &mut [u8]) -> Result<usize, AprsError> {
        let needed = self.encoded_len();
        let max = buf.len();
        let out = buf
            .get_mut(..needed)
            .ok_or(AprsError::BufferTooSmall { needed, max })?;
        let mut bytes = core::iter::once(&b'>').chain(self.text.iter());
        for slot in out.iter_mut() {
            match bytes.next() {
                Some(b) => *slot = *b,
                None => break,
            }
        }
        Ok(needed)
    }

    /// The leading `ddhhmmz` timestamp, when the text begins with one.
    ///
    /// Chapter 16: a status report's timestamp *can only* be DHM zulu,
    /// so the `h` (HMS) and `/` (local) forms a position report accepts
    /// are **not** recognised here. A report with a locator cannot have
    /// a timestamp, and this returns `None` for one.
    #[must_use]
    pub fn timestamp(&self) -> Option<Timestamp> {
        let head = self.text.get(..Timestamp::LEN)?;
        if head[6] != b'z' || !head[..6].iter().all(u8::is_ascii_digit) {
            return None;
        }
        match Timestamp::parse(self.text, 0) {
            Ok(timestamp @ Timestamp::DhmZulu { .. }) => Some(timestamp),
            Ok(_) | Err(_) => None,
        }
    }

    /// The leading Maidenhead locator and its symbol, when the text
    /// begins with one.
    ///
    /// Chapter 16 requires the locator to follow the `>` immediately, to
    /// be 4 or 6 characters, and to be followed by a symbol table
    /// identifier and symbol code. If status text follows, its first
    /// character must be a space.
    ///
    /// Those last two rules are **enforced**, not just documented,
    /// because without them the form is ambiguous with ordinary prose:
    /// `>FN42AB some text` is four locator characters followed by two
    /// letters in the subsquare range, and a parser that takes the
    /// longest match eats the symbol out of a real sentence. Requiring
    /// a valid table and code plus the mandatory space resolves it.
    #[must_use]
    pub fn grid(&self) -> Option<StatusGrid> {
        // Longest first, then fall back — but only accepting a length
        // whose trailing symbol and separator also check out, which is
        // what makes the longest-first order safe.
        for len in [6usize, 4] {
            if let Some(found) = self.grid_of_length(len) {
                return Some(found);
            }
        }
        None
    }

    /// Tries to read a locator of exactly `len` characters.
    fn grid_of_length(&self, len: usize) -> Option<StatusGrid> {
        let grid = MaidenheadGrid::new(core::str::from_utf8(self.text.get(..len)?).ok()?).ok()?;
        let table = *self.text.get(len)?;
        let code = *self.text.get(len + 1)?;
        SymbolTable::from_wire(table)?;
        let symbol = Symbol::from_wire(table, code);
        symbol.code()?;
        // Either the text ends here, or the remainder must begin with
        // the space chapter 16 mandates.
        match self.text.get(len + 2) {
            None => Some(StatusGrid { grid, symbol }),
            Some(b' ') => Some(StatusGrid { grid, symbol }),
            Some(_) => None,
        }
    }

    /// The trailing `^HP` beam heading and effective radiated power.
    ///
    /// Chapter 16 encodes the heading as tens of degrees — `0`–`9` for
    /// 0 to 90, `A`–`Z` for 100 to 350 — and the power by a code whose
    /// value is `(code - '0')² × 10` watts, which reproduces the whole
    /// of that chapter's ERP table (`1` is 10 W, `:` is 1000 W, `K` is
    /// 7290 W).
    #[must_use]
    pub fn beam(&self) -> Option<BeamHeading> {
        let [b'^', h, p] = *self.text.get(self.text.len().checked_sub(3)?..)? else {
            return None;
        };
        let tens = match h {
            b'0'..=b'9' => u16::from(h - b'0'),
            b'A'..=b'Z' => 10 + u16::from(h - b'A'),
            _ => return None,
        };
        if !(b'1'..=b'K').contains(&p) {
            return None;
        }
        let code = i32::from(p - b'0');
        Some(BeamHeading {
            heading: Bearing::new(tens * 10).ok()?,
            erp: Power::from_watts(code * code * 10),
        })
    }

    /// The human-readable remainder: the text with whichever of the
    /// timestamp, locator and `^HP` trailer were recognised removed.
    ///
    /// The space that chapter 16 mandates after a locator belongs to the
    /// separator rather than to the message, so it is removed too.
    #[must_use]
    pub fn message(&self) -> &'a [u8] {
        let mut text = self.text;
        if self.timestamp().is_some() {
            text = text.get(Timestamp::LEN..).unwrap_or(&[]);
        } else if let Some(found) = self.grid() {
            let skip = found.grid.as_bytes().len() + 2;
            text = text.get(skip..).unwrap_or(&[]);
            if let [b' ', rest @ ..] = text {
                text = rest;
            }
        }
        if self.beam().is_some() {
            text = text.get(..text.len().saturating_sub(3)).unwrap_or(&[]);
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let status = Status {
            text: b"Net Control Center",
        };
        let mut buf = [0u8; 32];
        let len = match status.build(&mut buf) {
            Ok(n) => n,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(&buf[..len], b">Net Control Center");
        assert_eq!(Status::parse(&buf[..len]), Ok(status));
    }

    #[test]
    fn empty_text_round_trips() {
        assert_eq!(Status::parse(b">"), Ok(Status { text: b"" }));
    }

    #[test]
    fn rejections() {
        assert_eq!(
            Status::parse(b""),
            Err(AprsError::Truncated {
                expected: 1,
                got: 0
            })
        );
        assert_eq!(
            Status::parse(b"!x"),
            Err(AprsError::InvalidDataType { got: b'!' })
        );
    }

    #[test]
    fn build_overflow() {
        let status = Status { text: b"hello" };
        let mut buf = [0u8; 3];
        assert_eq!(
            status.build(&mut buf),
            Err(AprsError::BufferTooSmall { needed: 6, max: 3 })
        );
    }

    /// Chapter 16's two worked examples, and its ERP table.
    #[test]
    fn beam_heading_and_erp_known_answers() {
        // "^B7 means a beam heading of 110 degrees and an ERP of 490
        // watts" -- the specification's own sentence.
        let beam = Status { text: b"Hello^B7" }.beam().expect("trailer");
        assert_eq!(beam.heading.degrees(), 110);
        assert_eq!(beam.erp.watts(), 490);

        // The three anchors of the published ERP table, which the
        // square-times-ten rule has to reproduce or it is the wrong
        // rule: 1 -> 10 W, ':' -> 1000 W, 'K' -> 7290 W.
        for (code, watts) in [(b'1', 10), (b':', 1000), (b'K', 7290)] {
            let text = [b'^', b'0', code];
            let beam = Status { text: &text }.beam().expect("trailer");
            assert_eq!(beam.erp.watts(), watts, "ERP code {:?}", code as char);
        }
        // And every other row of it.
        for (code, watts) in [
            (b'2', 40),
            (b'3', 90),
            (b'4', 160),
            (b'5', 250),
            (b'6', 360),
            (b'7', 490),
            (b'8', 640),
            (b'9', 810),
            (b';', 1210),
            (b'<', 1440),
            (b'=', 1690),
            (b'>', 1960),
            (b'?', 2250),
            (b'@', 2560),
            (b'A', 2890),
            (b'B', 3240),
            (b'C', 3610),
            (b'D', 4000),
            (b'E', 4410),
            (b'F', 4840),
            (b'G', 5290),
            (b'H', 5760),
            (b'I', 6250),
            (b'J', 6760),
        ] {
            let text = [b'^', b'0', code];
            assert_eq!(
                Status { text: &text }.beam().map(|b| b.erp.watts()),
                Some(watts),
                "ERP code {:?}",
                code as char
            );
        }

        // Headings: digits are tens, letters continue from 100.
        for (h, degrees) in [
            (b'0', 0u16),
            (b'9', 90),
            (b'A', 100),
            (b'B', 110),
            (b'Z', 350),
        ] {
            let text = [b'^', h, b'1'];
            assert_eq!(
                Status { text: &text }.beam().map(|b| b.heading.degrees()),
                Some(degrees),
                "heading {:?}",
                h as char
            );
        }

        // Not a trailer: wrong marker, unknown power code, too short.
        for text in [
            &b"Hello~B7"[..],
            &b"Hello^B0"[..],
            &b"Hello^BL"[..],
            &b"^B"[..],
            &b""[..],
        ] {
            assert_eq!(Status { text }.beam(), None, "{text:?}");
        }
    }

    /// Chapter 16's locator form, including the ambiguity it invites.
    #[test]
    fn maidenhead_locator_form() {
        let status = Status {
            text: b"IO91SX/G Hello world",
        };
        let found = status.grid().expect("a locator");
        // `MaidenheadGrid` stores the canonical spelling — fields upper
        // case, subsquares lower — so a locator sent in all capitals,
        // as chapter 16 requires on transmit, comes back normalised.
        assert_eq!(found.grid.as_bytes(), b"IO91sx");
        assert_eq!(found.symbol.to_wire(), (b'/', b'G'));
        assert_eq!(status.message(), b"Hello world");

        // Four characters, and the text may be absent entirely.
        let status = Status { text: b"FN42/G" };
        assert_eq!(
            status.grid().map(|g| g.grid.as_bytes().to_vec()),
            Some(b"FN42".to_vec())
        );
        assert_eq!(status.message(), b"");

        // Lower case is accepted on receive, per chapter 16.
        assert!(
            Status {
                text: b"io91sx/G x"
            }
            .grid()
            .is_some()
        );

        // The six-character reading is tried first, and the trailing
        // rules are what make that safe. In `FN42AB some text`,
        // "FN42AB" *is* a well-formed six-character locator, so a
        // parser that stopped at the longest match would take ' ' as
        // the symbol table and 's' as the code and eat two characters
        // out of a real sentence. Requiring a valid table rejects it.
        //
        // What is left is ambiguous and chapter 16 gives no way to
        // settle it: the four-character reading `FN42` + overlay `A` +
        // code `B` + the mandated space is *also* well formed, so that
        // is what this returns. Recorded as behaviour rather than
        // hidden, because a caller who cares can look at `message()`
        // and judge.
        let ambiguous = Status {
            text: b"FN42AB some text",
        };
        let found = ambiguous.grid().expect("the four-character reading");
        assert_eq!(found.grid.as_bytes(), b"FN42");
        assert_eq!(found.symbol.to_wire(), (b'A', b'B'));
        assert_eq!(ambiguous.message(), b"some text");

        // Where the symbol is *not* valid there is no ambiguity left:
        // lower case is no overlay table, so this is prose throughout.
        assert_eq!(
            Status {
                text: b"FN42ab some text"
            }
            .grid(),
            None
        );
        assert_eq!(
            Status {
                text: b"FN42ab some text"
            }
            .message(),
            b"FN42ab some text"
        );
        // A bare locator with no symbol after it is not the locator form.
        assert_eq!(Status { text: b"FN42" }.grid(), None);
        // Nor is one whose "text" does not begin with the mandated space.
        assert_eq!(Status { text: b"FN42/Gx" }.grid(), None);
    }

    #[test]
    fn timestamp_is_dhm_zulu_only() {
        let status = Status {
            text: b"092345zNet Control Center",
        };
        assert_eq!(
            status.timestamp(),
            Some(Timestamp::DhmZulu {
                day: 9,
                hour: 23,
                minute: 45
            })
        );
        assert_eq!(status.message(), b"Net Control Center");

        // Chapter 16 permits only the zulu form here, so the HMS and
        // local spellings a position report accepts are not timestamps
        // in a status report -- they are text.
        for text in [&b"092345hHello"[..], &b"092345/Hello"[..]] {
            assert_eq!(Status { text }.timestamp(), None, "{text:?}");
            assert_eq!(Status { text }.message(), text);
        }
        // Plain text that just starts with digits is not a timestamp.
        assert_eq!(
            Status {
                text: b"12345 miles"
            }
            .timestamp(),
            None
        );
    }

    /// The three views compose, which is what chapter 16 says: "the HP
    /// value may be combined with the Maidenhead grid locator ... or
    /// with any other plain language status text."
    #[test]
    fn views_compose_and_message_is_the_remainder() {
        let status = Status {
            text: b"IO91SX/G Hello^B7",
        };
        assert!(status.grid().is_some());
        assert_eq!(status.beam().map(|b| b.erp.watts()), Some(490));
        assert_eq!(status.message(), b"Hello");

        let status = Status {
            text: b"092345zNet Control^A1",
        };
        assert!(status.timestamp().is_some());
        assert_eq!(status.beam().map(|b| b.heading.degrees()), Some(100));
        assert_eq!(status.message(), b"Net Control");

        // Nothing structured at all: message is the text.
        let status = Status {
            text: b"just talking",
        };
        assert_eq!(status.timestamp(), None);
        assert_eq!(status.grid(), None);
        assert_eq!(status.beam(), None);
        assert_eq!(status.message(), b"just talking");
    }
}
