//! AX.25 address fields: callsigns, SSIDs, and their 7-byte wire encoding.
//!
//! Each AX.25 address occupies exactly seven octets on the wire: six
//! callsign characters shifted left one bit (space-padded on the right),
//! then an SSID octet laid out `C R R S S S S E` — command/response bit,
//! two reserved bits (transmitted as ones, `0x60`), the four-bit SSID, and
//! the extension bit `E` that is `1` only on the *final* address of the
//! address field.

use super::Ax25Error;

/// The wire size of one encoded address, in bytes.
pub const ADDRESS_LEN: usize = 7;

/// The has-been-repeated (H) bit mask within the SSID octet.
///
/// Bit 7 of the seventh address byte. On a *digipeater* address it means
/// "this hop has already repeated the frame"; on the destination and
/// source addresses the very same bit position carries the C
/// (command/response) bit instead — AX.25 2.0 overloads the position.
pub const H_BIT: u8 = 0x80;

/// A validated AX.25 callsign: one to six characters, each `A-Z` or `0-9`.
///
/// Lowercase input is rejected rather than folded — AX.25 callsigns are
/// uppercase on the wire, and silent case-folding would hide sender bugs.
///
/// # Validated construction
///
/// ```
/// use yodel::ax25::{Ax25Error, Callsign};
///
/// let call = Callsign::new(b"N0CALL")?;
/// assert_eq!(call.as_bytes(), b"N0CALL"); // padding never leaks out
///
/// // Lowercase is rejected with a typed error, not folded.
/// assert_eq!(
///     Callsign::new(b"n0call"),
///     Err(Ax25Error::InvalidCallsignChar { got: b'n' })
/// );
/// // Seven characters cannot fit the 6-byte wire field.
/// assert_eq!(
///     Callsign::new(b"TOOLONG"),
///     Err(Ax25Error::CallsignLengthInvalid { got: 7 })
/// );
/// # Ok::<(), Ax25Error>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Callsign {
    /// Space-padded to six bytes, ready for wire encoding.
    chars: [u8; 6],
    /// Number of significant (non-padding) characters, `1..=6`.
    len: u8,
}

impl Callsign {
    /// Creates a validated callsign from its ASCII bytes.
    ///
    /// # Errors
    ///
    /// [`Ax25Error::CallsignLengthInvalid`] when `text` is empty or longer
    /// than six bytes; [`Ax25Error::InvalidCallsignChar`] on the first byte
    /// outside `A-Z` / `0-9`.
    pub const fn new(text: &[u8]) -> Result<Self, Ax25Error> {
        if text.is_empty() || text.len() > 6 {
            return Err(Ax25Error::CallsignLengthInvalid { got: text.len() });
        }
        let mut chars = [b' '; 6];
        let mut i = 0;
        while i < text.len() {
            let c = text[i];
            if !(c.is_ascii_uppercase() || c.is_ascii_digit()) {
                return Err(Ax25Error::InvalidCallsignChar { got: c });
            }
            chars[i] = c;
            i += 1;
        }
        #[allow(clippy::cast_possible_truncation)] // len <= 6, checked above
        Ok(Self {
            chars,
            len: text.len() as u8,
        })
    }

    /// The callsign text, without padding.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.chars.get(..usize::from(self.len)).unwrap_or(&[])
    }

    /// The callsign text, space-padded to six bytes.
    ///
    /// This is the form the wire encoding and the Mic-E destination
    /// alphabet both work in: [`Address::encode`] shifts these six bytes
    /// left, and the `micE` feature's `mic_e::decode` requires exactly
    /// six destination characters. [`Callsign::as_bytes`] is the
    /// *unpadded* text, so every caller that needed the padded form used
    /// to rebuild it by hand.
    ///
    /// ```
    /// use yodel::ax25::{Ax25Error, Callsign};
    ///
    /// assert_eq!(&Callsign::new(b"APRS")?.as_padded(), b"APRS  ");
    /// assert_eq!(&Callsign::new(b"N0CALL")?.as_padded(), b"N0CALL");
    /// # Ok::<(), Ax25Error>(())
    /// ```
    #[must_use]
    pub const fn as_padded(&self) -> [u8; 6] {
        self.chars
    }
}

/// A validated AX.25 secondary station identifier, `0..=15`.
///
/// The SSID occupies four bits of the wire address octet, so 15 is a
/// hard ceiling:
///
/// ```
/// use yodel::ax25::{Ax25Error, Ssid};
///
/// assert_eq!(Ssid::new(15)?.value(), 15);
/// assert_eq!(Ssid::new(16), Err(Ax25Error::SsidOutOfRange { got: 16 }));
/// assert_eq!(Ssid::ZERO.value(), 0); // the conventional default
/// # Ok::<(), Ax25Error>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ssid(u8);

impl Ssid {
    /// SSID zero, the conventional default.
    pub const ZERO: Self = Self(0);

    /// Creates a validated SSID.
    ///
    /// # Errors
    ///
    /// [`Ax25Error::SsidOutOfRange`] when `value > 15`.
    pub const fn new(value: u8) -> Result<Self, Ax25Error> {
        if value <= 15 {
            Ok(Self(value))
        } else {
            Err(Ax25Error::SsidOutOfRange { got: value })
        }
    }

    /// The SSID value.
    #[must_use]
    pub const fn value(self) -> u8 {
        self.0
    }
}

/// A complete AX.25 address: callsign plus SSID.
///
/// # Wire round trip
///
/// Each address is exactly [`ADDRESS_LEN`] (7) bytes on the wire: the
/// callsign shifted left one bit, then the SSID octet `C R R S S S S E`.
/// Encode/decode round-trips exactly, including the extension bit:
///
/// ```
/// use yodel::ax25::{Address, Ax25Error};
///
/// let addr = Address::new(b"N0CALL", 7)?;
/// let wire = addr.encode(false, true); // source position, final address
/// assert_eq!(wire[0], b'N' << 1);      // callsign chars are shifted
/// assert_eq!(wire[6], 0x60 | (7 << 1) | 1); // reserved bits, SSID 7, ext 1
///
/// let (decoded, last) = Address::decode(&wire)?;
/// assert_eq!(decoded, addr);
/// assert!(last);
/// # Ok::<(), Ax25Error>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Address {
    /// The station callsign.
    pub callsign: Callsign,
    /// The secondary station identifier.
    pub ssid: Ssid,
}

impl Address {
    /// Convenience constructor validating both parts at once.
    ///
    /// # Errors
    ///
    /// Propagates the [`Callsign::new`] and [`Ssid::new`] errors.
    pub const fn new(callsign: &[u8], ssid: u8) -> Result<Self, Ax25Error> {
        // `?` on Result in const fn is fine, but keep matches explicit for
        // MSRV-independent const-ness.
        let callsign = match Callsign::new(callsign) {
            Ok(c) => c,
            Err(e) => return Err(e),
        };
        let ssid = match Ssid::new(ssid) {
            Ok(s) => s,
            Err(e) => return Err(e),
        };
        Ok(Self { callsign, ssid })
    }

    /// Encodes the address into its 7-byte wire form.
    ///
    /// `command` sets the C bit (bit 7 of the SSID octet): per the APRS
    /// convention it is set on the destination and clear on the source and
    /// digipeaters. `last` sets the extension bit (bit 0), terminating the
    /// address field; it must be set only on the final address.
    #[must_use]
    pub fn encode(&self, command: bool, last: bool) -> [u8; ADDRESS_LEN] {
        let mut out = [b' ' << 1; ADDRESS_LEN];
        for (slot, &c) in out.iter_mut().zip(self.callsign.chars.iter()) {
            *slot = c << 1;
        }
        out[6] = 0x60 | (self.ssid.value() << 1) | u8::from(last);
        if command {
            out[6] |= 0x80;
        }
        out
    }

    /// Decodes a 7-byte wire address field.
    ///
    /// Returns the address plus its extension bit (`true` when this was the
    /// final address of the field). The C and reserved bits are accepted in
    /// any state; the callsign characters are validated.
    ///
    /// # Errors
    ///
    /// [`Ax25Error::InvalidCallsignChar`] on a shifted byte outside
    /// `A-Z` / `0-9` / space, or [`Ax25Error::CallsignLengthInvalid`] when
    /// the callsign is empty or has embedded padding.
    pub fn decode(field: &[u8; ADDRESS_LEN]) -> Result<(Self, bool), Ax25Error> {
        let mut len = 0usize;
        let mut chars = [b' '; 6];
        let mut ended = false;
        for (i, slot) in chars.iter_mut().enumerate() {
            let raw = match field.get(i) {
                Some(&b) => b,
                None => return Err(Ax25Error::CallsignLengthInvalid { got: i }),
            };
            let c = raw >> 1;
            if c == b' ' {
                ended = true;
                continue;
            }
            if ended {
                // Padding followed by a character: malformed callsign.
                return Err(Ax25Error::InvalidCallsignChar { got: c });
            }
            if !(c.is_ascii_uppercase() || c.is_ascii_digit()) {
                return Err(Ax25Error::InvalidCallsignChar { got: c });
            }
            *slot = c;
            len += 1;
        }
        if len == 0 {
            return Err(Ax25Error::CallsignLengthInvalid { got: 0 });
        }
        let ssid_octet = field[6];
        let ssid = Ssid::new((ssid_octet >> 1) & 0x0F)?;
        let last = ssid_octet & 1 == 1;
        #[allow(clippy::cast_possible_truncation)] // len <= 6 by loop bound
        Ok((
            Self {
                callsign: Callsign {
                    chars,
                    len: len as u8,
                },
                ssid,
            },
            last,
        ))
    }
}

/// One digipeater path entry: an [`Address`] plus its has-been-repeated
/// (H) bit.
///
/// The H bit is bit 7 ([`H_BIT`]) of the SSID octet on digipeater
/// addresses: clear means the hop has not yet repeated the frame, set
/// means it has. Both fields are plain data with no invariant beyond the
/// [`Address`]'s own, so they are public.
///
/// # Wire round trip
///
/// ```
/// use yodel::ax25::{Ax25Error, Address, PathHop};
///
/// // An unused hop, as a station requests it on transmit …
/// let fresh = PathHop::unused(Address::new(b"WIDE1", 1)?);
/// assert!(!fresh.repeated);
///
/// // … and the same hop after a digipeater marks it used.
/// let used = PathHop { repeated: true, ..fresh };
/// let wire = used.encode(true);
/// assert_eq!(wire[6] & 0x80, 0x80); // H bit set on the SSID octet
///
/// let (decoded, last) = PathHop::decode(&wire)?;
/// assert_eq!(decoded, used);
/// assert!(last);
/// # Ok::<(), Ax25Error>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathHop {
    /// The digipeater address.
    pub address: Address,
    /// The has-been-repeated (H) bit: `true` once the hop has repeated
    /// the frame.
    pub repeated: bool,
}

impl PathHop {
    /// A hop with the H bit clear — how every hop leaves the originating
    /// station.
    #[must_use]
    pub const fn unused(address: Address) -> Self {
        Self {
            address,
            repeated: false,
        }
    }

    /// Encodes the hop into its 7-byte wire form.
    ///
    /// `last` sets the extension bit exactly as in [`Address::encode`];
    /// the H bit is written from [`PathHop::repeated`]. (The H bit
    /// occupies the same position the C bit does on the destination and
    /// source addresses, so this delegates to [`Address::encode`].)
    #[must_use]
    pub fn encode(&self, last: bool) -> [u8; ADDRESS_LEN] {
        self.address.encode(self.repeated, last)
    }

    /// Decodes a 7-byte wire digipeater address, preserving the H bit.
    ///
    /// Returns the hop plus its extension bit (`true` when this was the
    /// final address of the field).
    ///
    /// # Errors
    ///
    /// Exactly those of [`Address::decode`].
    pub fn decode(field: &[u8; ADDRESS_LEN]) -> Result<(Self, bool), Ax25Error> {
        let (address, last) = Address::decode(field)?;
        Ok((
            Self {
                address,
                repeated: field[6] & H_BIT != 0,
            },
            last,
        ))
    }
}

impl From<Address> for PathHop {
    /// Converts a plain address into an unused (H-bit-clear) hop.
    fn from(address: Address) -> Self {
        Self::unused(address)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callsign_accepts_valid() {
        for text in [&b"A"[..], b"N0CALL", b"APRS", b"9A9AAA", b"K1ABC"] {
            let c = match Callsign::new(text) {
                Ok(c) => c,
                Err(e) => panic!("{e}"),
            };
            assert_eq!(c.as_bytes(), text);
        }
    }

    #[test]
    fn callsign_rejects_bad_lengths() {
        assert_eq!(
            Callsign::new(b""),
            Err(Ax25Error::CallsignLengthInvalid { got: 0 })
        );
        assert_eq!(
            Callsign::new(b"TOOLONG"),
            Err(Ax25Error::CallsignLengthInvalid { got: 7 })
        );
    }

    #[test]
    fn callsign_rejects_bad_chars() {
        for (text, bad) in [
            (&b"n0call"[..], b'n'),
            (b"AB-C", b'-'),
            (b"A B", b' '),
            (b"AB\0", 0),
        ] {
            assert_eq!(
                Callsign::new(text),
                Err(Ax25Error::InvalidCallsignChar { got: bad })
            );
        }
    }

    #[test]
    fn ssid_boundaries() {
        assert_eq!(Ssid::new(0).map(Ssid::value), Ok(0));
        assert_eq!(Ssid::new(15).map(Ssid::value), Ok(15));
        assert_eq!(Ssid::new(16), Err(Ax25Error::SsidOutOfRange { got: 16 }));
        assert_eq!(Ssid::new(255), Err(Ax25Error::SsidOutOfRange { got: 255 }));
    }

    #[test]
    fn encode_known_layout() {
        let addr = match Address::new(b"APRS", 0) {
            Ok(a) => a,
            Err(e) => panic!("{e}"),
        };
        let enc = addr.encode(true, false);
        assert_eq!(
            enc,
            [
                b'A' << 1,
                b'P' << 1,
                b'R' << 1,
                b'S' << 1,
                b' ' << 1,
                b' ' << 1,
                0xE0, // C=1, reserved 11, SSID 0, ext 0
            ]
        );
    }

    #[test]
    fn encode_source_last_with_ssid() {
        let addr = match Address::new(b"N0CALL", 7) {
            Ok(a) => a,
            Err(e) => panic!("{e}"),
        };
        let enc = addr.encode(false, true);
        assert_eq!(enc[6], 0x60 | (7 << 1) | 1); // C=0, reserved, SSID 7, ext 1
    }

    #[test]
    fn round_trip_ssid_extremes() {
        for ssid in [0u8, 15] {
            for last in [false, true] {
                for command in [false, true] {
                    let addr = match Address::new(b"K1ABC", ssid) {
                        Ok(a) => a,
                        Err(e) => panic!("{e}"),
                    };
                    let enc = addr.encode(command, last);
                    let (dec, dec_last) = match Address::decode(&enc) {
                        Ok(d) => d,
                        Err(e) => panic!("{e}"),
                    };
                    assert_eq!(dec, addr);
                    assert_eq!(dec_last, last);
                }
            }
        }
    }

    #[test]
    fn path_hop_round_trips_h_bit() {
        let address = match Address::new(b"WIDE2", 1) {
            Ok(a) => a,
            Err(e) => panic!("{e}"),
        };
        for repeated in [false, true] {
            for last in [false, true] {
                let hop = PathHop { address, repeated };
                let wire = hop.encode(last);
                assert_eq!(wire[6] & H_BIT != 0, repeated);
                let (decoded, dec_last) = match PathHop::decode(&wire) {
                    Ok(d) => d,
                    Err(e) => panic!("{e}"),
                };
                assert_eq!(decoded, hop);
                assert_eq!(dec_last, last);
            }
        }
    }

    #[test]
    fn path_hop_unused_matches_from() {
        let address = match Address::new(b"N0CALL", 0) {
            Ok(a) => a,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(PathHop::unused(address), PathHop::from(address));
        assert!(!PathHop::unused(address).repeated);
    }

    #[test]
    fn decode_rejects_garbage() {
        // Lowercase char after shifting.
        let mut field = match Address::new(b"APRS", 0) {
            Ok(a) => a.encode(true, true),
            Err(e) => panic!("{e}"),
        };
        field[0] = b'a' << 1;
        assert!(Address::decode(&field).is_err());
        // Embedded padding.
        let mut field2 = match Address::new(b"AB", 0) {
            Ok(a) => a.encode(true, true),
            Err(e) => panic!("{e}"),
        };
        field2[3] = b'C' << 1; // "AB C" — char after a space pad
        assert!(Address::decode(&field2).is_err());
        // All spaces.
        let blank = [b' ' << 1, 0x40, 0x40, 0x40, 0x40, 0x40, 0x61];
        assert!(Address::decode(&blank).is_err());
    }
}
