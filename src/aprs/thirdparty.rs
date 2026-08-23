//! Third-party (encapsulated) traffic — the `}` data type identifier.
//!
//! Specified in chapter 17, "Network Tunneling and Third-Party
//! Digipeating", of the APRS protocol reference (see [`crate::aprs`] for
//! the edition this crate implements), under "Third-Party Header".
//!
//! An internet gateway that hears a packet on APRS-IS and re-transmits
//! it on RF wraps the original inside its own frame:
//!
//! ```text
//! IGATE>APRS,WIDE1-1:}N0CALL>APRS,TCPIP,IGATE*:!4903.50N/07201.75W-
//!                    │└──────── inner header ────────┘│└─ payload ─┘
//!                    └ the `}` data type identifier
//! ```
//!
//! The inner header is **TNC2 monitor text**, not binary AX.25: the
//! callsigns are ASCII, separated by `>` and `,`, terminated by `:`.
//! Everything after that first `:` is the original information field,
//! complete with its own data type identifier.
//!
//! # Why this type does not nest
//!
//! [`ThirdParty`] borrows the inner information field as bytes rather
//! than holding a parsed packet. That buys three things:
//!
//! * **It works without allocation.** A self-referential enum would
//!   need indirection, and this crate has no heap.
//! * **Recursion depth is bounded by construction.** The caller decides
//!   whether to descend, so a maliciously nested packet cannot exhaust
//!   the stack inside the parser. Nesting deeper than one level is
//!   malformed in practice; you almost certainly want to decode
//!   [`ThirdParty::payload`] exactly once and stop.
//! * **The bytes survive.** Even when the inner payload is a type this
//!   crate does not implement, the caller still gets it.
//!
//! # Addresses are not [`Address`]
//!
//! Traffic arriving from APRS-IS is not bound by AX.25 address rules:
//! the inner source may exceed six characters, contain lower case, or
//! carry a two-character alphanumeric SSID. Forcing it through
//! [`Address`] would reject exactly the packets this module exists to
//! recover, so the fields are raw slices and
//! [`ThirdParty::source_address`] / [`ThirdParty::dest_address`] offer
//! the conversion where it happens to be legal.
//!
//! ```
//! use warble::aprs::thirdparty::ThirdParty;
//!
//! let info = b"}N0CALL>APRS,TCPIP,IGATE*:>hello";
//! let tp = ThirdParty::parse(info)?;
//! assert_eq!(tp.source, b"N0CALL");
//! assert_eq!(tp.dest, b"APRS");
//! assert_eq!(tp.path, b"TCPIP,IGATE*");
//! assert_eq!(tp.payload, b">hello");
//! assert!(tp.is_from_internet());
//! # Ok::<(), warble::aprs::AprsError>(())
//! ```

use super::AprsError;
use super::monitor::{MonitorLine, parse_text_address};
use crate::ax25::Address;

/// The `}` data type identifier introducing third-party traffic.
pub const THIRD_PARTY_DTI: u8 = b'}';

/// Longest inner source/destination callsign accepted.
///
/// AX.25 allows six characters plus an SSID; APRS-IS permits up to nine
/// characters total including an alphanumeric SSID, which is the limit
/// applied here.
pub const CALLSIGN_MAX: usize = 9;

/// A third-party packet: an original transmission wrapped by a gateway.
///
/// Parsed from the information field with [`ThirdParty::parse`]. The
/// inner information field is left as bytes in [`ThirdParty::payload`];
/// see the module documentation for why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThirdParty<'a> {
    /// The original transmitting station, as text.
    ///
    /// May not be a legal AX.25 address; see
    /// [`ThirdParty::source_address`].
    pub source: &'a [u8],
    /// The original destination (tocall), as text.
    pub dest: &'a [u8],
    /// The comma-separated digipeater path between the destination and
    /// the `:`, without surrounding separators. Empty when absent.
    ///
    /// This routinely contains tokens that never appear on RF, such as
    /// `TCPIP`, `TCPXX`, `NOGATE`, `RFONLY` and the APRS-IS `q`
    /// constructs (`qAC`, `qAR`, `qAo`, …). They are preserved verbatim
    /// rather than validated.
    pub path: &'a [u8],
    /// The original information field, including its own data type
    /// identifier. Decode it with
    /// [`Decoded::decode`](super::Decoded::decode).
    pub payload: &'a [u8],
}

impl<'a> ThirdParty<'a> {
    /// Parses a third-party packet from a complete information field.
    ///
    /// `info` must begin with [`THIRD_PARTY_DTI`].
    ///
    /// # Errors
    ///
    /// * [`AprsError::InvalidDataType`] when `info` does not start
    ///   with `}`.
    /// * [`AprsError::Truncated`] when the header is incomplete — no
    ///   `>` separating source from destination, or no `:` ending the
    ///   header.
    /// * [`AprsError::BadCallsignLength`] when either callsign is empty
    ///   or longer than [`CALLSIGN_MAX`].
    pub const fn parse(info: &'a [u8]) -> Result<Self, AprsError> {
        let (&dti, rest) = match info.split_first() {
            Some(v) => v,
            None => {
                return Err(AprsError::Truncated {
                    expected: 1,
                    got: 0,
                });
            }
        };
        if dti != THIRD_PARTY_DTI {
            return Err(AprsError::InvalidDataType { got: dti });
        }

        let line = match MonitorLine::parse(rest) {
            Ok(l) => l,
            Err(e) => return Err(e),
        };
        Ok(Self {
            source: line.source,
            dest: line.dest,
            path: line.path,
            payload: line.info,
        })
    }

    /// The source as a validated AX.25 [`Address`], when it happens to
    /// be one.
    ///
    /// Returns `None` for the APRS-IS-only forms this module accepts:
    /// callsigns over six characters, lower case, or a non-numeric SSID.
    #[must_use]
    pub fn source_address(&self) -> Option<Address> {
        parse_text_address(self.source)
    }

    /// The destination as a validated AX.25 [`Address`], when it
    /// happens to be one. As [`ThirdParty::source_address`].
    #[must_use]
    pub fn dest_address(&self) -> Option<Address> {
        parse_text_address(self.dest)
    }

    /// Whether the path marks this packet as having traversed the
    /// internet (`TCPIP` or `TCPXX`).
    ///
    /// A gateway must not send such a packet back to APRS-IS: that is
    /// the loop-prevention rule. Purely informational for a receiver.
    #[must_use]
    pub fn is_from_internet(&self) -> bool {
        self.path_contains(b"TCPIP") || self.path_contains(b"TCPXX")
    }

    /// Whether the path forbids gating to the internet (`NOGATE` or
    /// `RFONLY`).
    #[must_use]
    pub fn is_gate_forbidden(&self) -> bool {
        self.path_contains(b"NOGATE") || self.path_contains(b"RFONLY")
    }

    /// Whether any comma-separated path element equals `needle`,
    /// ignoring a trailing `*` has-been-repeated marker.
    #[must_use]
    pub fn path_contains(&self, needle: &[u8]) -> bool {
        let mut rest = self.path;
        loop {
            let (element, tail) = match find(rest, b',') {
                Some(i) => (&rest[..i], &rest[i + 1..]),
                None => (rest, &[] as &[u8]),
            };
            let element = match element {
                [head @ .., b'*'] => head,
                all => all,
            };
            if element == needle {
                return true;
            }
            if tail.is_empty() {
                return false;
            }
            rest = tail;
        }
    }
}

/// Index of the first `byte` in `haystack`.
const fn find(haystack: &[u8], byte: u8) -> Option<usize> {
    let mut i = 0;
    while i < haystack.len() {
        if haystack[i] == byte {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_gateway_wrapped_position() {
        let info = b"}W6AHM>APRS,TCPIP,N6EX-3*:@230135z3350.28N/11818.85W_269/010";
        let tp = ThirdParty::parse(info).expect("parse");
        assert_eq!(tp.source, b"W6AHM");
        assert_eq!(tp.dest, b"APRS");
        assert_eq!(tp.path, b"TCPIP,N6EX-3*");
        assert_eq!(tp.payload, b"@230135z3350.28N/11818.85W_269/010");
        assert!(tp.is_from_internet());
        assert!(!tp.is_gate_forbidden());
    }

    #[test]
    fn parses_without_a_path() {
        let tp = ThirdParty::parse(b"}N0CALL>APRS:>status").expect("parse");
        assert_eq!(tp.source, b"N0CALL");
        assert_eq!(tp.dest, b"APRS");
        assert_eq!(tp.path, b"");
        assert_eq!(tp.payload, b">status");
        assert!(!tp.is_from_internet());
    }

    #[test]
    fn payload_may_be_empty() {
        let tp = ThirdParty::parse(b"}N0CALL>APRS:").expect("parse");
        assert_eq!(tp.payload, b"");
    }

    #[test]
    fn a_colon_inside_the_payload_does_not_split_the_header() {
        // Message payloads contain colons; only the first one ends the header.
        let tp = ThirdParty::parse(b"}N0CALL>APRS,TCPIP*::WB2OSZ   :hi{1").expect("parse");
        assert_eq!(tp.path, b"TCPIP*");
        assert_eq!(tp.payload, b":WB2OSZ   :hi{1");
    }

    #[test]
    fn recognizes_aprs_is_q_constructs_and_gate_markers() {
        let tp = ThirdParty::parse(b"}N0CALL>APRS,qAR,IGATE:>x").expect("parse");
        assert!(tp.path_contains(b"qAR"));
        assert!(tp.path_contains(b"IGATE"));
        assert!(!tp.is_from_internet());

        let tp = ThirdParty::parse(b"}N0CALL>APRS,NOGATE:>x").expect("parse");
        assert!(tp.is_gate_forbidden());
    }

    #[test]
    fn has_been_repeated_marker_does_not_defeat_path_matching() {
        let tp = ThirdParty::parse(b"}N0CALL>APRS,WIDE1-1*,TCPIP*:>x").expect("parse");
        assert!(tp.path_contains(b"WIDE1-1"));
        assert!(tp.is_from_internet());
    }

    #[test]
    fn accepts_addresses_that_ax25_would_reject() {
        // Nine characters and a lower-case tail: legal on APRS-IS only.
        let tp = ThirdParty::parse(b"}LONGCALL1>APRS,TCPIP*:>x").expect("parse");
        assert_eq!(tp.source, b"LONGCALL1");
        assert!(tp.source_address().is_none(), "not a legal AX.25 address");
        assert!(tp.dest_address().is_some());
    }

    #[test]
    fn converts_legal_addresses() {
        let tp = ThirdParty::parse(b"}N0CALL-5>APRS:>x").expect("parse");
        let src = tp.source_address().expect("legal address");
        assert_eq!(src.callsign.as_bytes(), b"N0CALL");
        assert_eq!(src.ssid.value(), 5);
    }

    #[test]
    fn rejects_malformed_headers() {
        assert!(matches!(
            ThirdParty::parse(b"!not third party"),
            Err(AprsError::InvalidDataType { got: b'!' })
        ));
        // No ':' terminating the header.
        assert!(matches!(
            ThirdParty::parse(b"}N0CALL>APRS,TCPIP"),
            Err(AprsError::Truncated { .. })
        ));
        // No '>' separating source from destination.
        assert!(matches!(
            ThirdParty::parse(b"}N0CALL:payload"),
            Err(AprsError::Truncated { .. })
        ));
        // Empty and oversized callsigns.
        assert!(matches!(
            ThirdParty::parse(b"}>APRS:x"),
            Err(AprsError::BadCallsignLength { len: 0 })
        ));
        assert!(matches!(
            ThirdParty::parse(b"}WAYTOOLONGCALL>APRS:x"),
            Err(AprsError::BadCallsignLength { len: 14 })
        ));
        assert!(matches!(
            ThirdParty::parse(b""),
            Err(AprsError::Truncated { .. })
        ));
    }

    #[test]
    fn never_panics_on_arbitrary_input() {
        let mut state = 0x1234_5678u32;
        let mut buf = [0u8; 64];
        for _ in 0..20_000 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let len = (state >> 24) as usize % buf.len();
            for (i, b) in buf.iter_mut().enumerate().take(len) {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                // Bias towards the structural bytes so headers form often.
                *b = match (state >> 16) % 8 {
                    0 => b'>',
                    1 => b',',
                    2 => b':',
                    3 => b'-',
                    4 => b'*',
                    _ => (state >> 8) as u8,
                };
                let _ = i;
            }
            buf[0] = b'}';
            let _ = ThirdParty::parse(&buf[..len.max(1)]);
        }
    }
}
