//! AX.25 UI (unnumbered information) frame building and parsing.
//!
//! A UI frame body (the part the FCS covers) is: destination address,
//! source address, up to [`MAX_DIGIPEATERS`] digipeater addresses, control
//! `0x03` (UI), PID `0xF0` (no layer 3), then the information field.
//! [`UiFrame::build`] serializes into a caller-provided buffer;
//! [`UiFrame::parse`] validates and returns a typed view borrowing the
//! input.

use super::Ax25Error;
use super::addr::{ADDRESS_LEN, Address, PathHop};

/// The UI control byte, with the poll/final bit clear.
///
/// A UI frame is `000 P/F 0011`, so the poll/final bit is bit 4 and
/// `0x13` is the *same* frame type with P/F set. Compare against
/// [`CONTROL_UI`] through [`CONTROL_PF_MASK`] rather than for equality,
/// or real traffic will be rejected — see [`UiFrame::parse`].
pub const CONTROL_UI: u8 = 0x03;
/// Mask clearing the poll/final bit (bit 4) of a control byte.
///
/// AX.25 carries the command/response distinction in the C bits of the
/// address SSID octets, not in P/F, and APRS — being connectionless —
/// assigns no meaning to P/F at all. Both `0x03` and `0x13` are UI
/// frames carrying an APRS payload.
pub const CONTROL_PF_MASK: u8 = !0x10;
/// The no-layer-3 PID byte.
pub const PID_NO_LAYER3: u8 = 0xF0;
/// Maximum number of digipeater path addresses.
pub const MAX_DIGIPEATERS: usize = 8;
/// Minimum length of a UI frame body: two addresses, control, and PID.
pub const MIN_FRAME_LEN: usize = 2 * ADDRESS_LEN + 2;

/// Placeholder address used to fill unused digipeater slots.
const PLACEHOLDER: Address = match Address::new(b"N0CALL", 0) {
    Ok(a) => a,
    // Const-evaluated: a failure here is a compile-time error, not a
    // runtime panic.
    Err(_) => panic!("placeholder address must be valid"),
};

/// A parsed or to-be-built UI frame.
///
/// The digipeater path lives in a private fixed-capacity array exposed
/// as a slice via [`UiFrame::path`], so the stored count can never
/// disagree with the storage or exceed [`MAX_DIGIPEATERS`]; the info
/// field borrows from the parsed input or the caller's data.
///
/// # Build → parse round trip
///
/// [`UiFrame::build`] serializes exactly the bytes the FCS covers — no
/// flags, no FCS — into a caller-provided buffer; [`UiFrame::parse`]
/// validates them back into an equal typed view borrowing the buffer:
///
/// ```
/// use yodel::ax25::{Address, Ax25Error, UiFrame};
/// use yodel::ax25::frame::{CONTROL_UI, PID_NO_LAYER3};
///
/// let frame = UiFrame::with_path(
///     Address::new(b"APRS", 0)?,   // destination tocall
///     Address::new(b"N0CALL", 7)?, // source
///     &[Address::new(b"WIDE1", 1)?],
///     b">hello",
/// )?;
///
/// let mut buf = [0u8; 64];
/// let len = frame.build(&mut buf)?;
/// // Three 7-byte addresses + control + PID + 6 info bytes.
/// assert_eq!(len, 3 * 7 + 2 + 6);
/// assert_eq!(buf[21], CONTROL_UI);     // 0x03
/// assert_eq!(buf[22], PID_NO_LAYER3);  // 0xF0
///
/// let parsed = UiFrame::parse(&buf[..len])?;
/// assert_eq!(parsed, frame);
/// assert_eq!(parsed.src.callsign.as_bytes(), b"N0CALL");
/// assert_eq!(parsed.path().len(), 1);
/// assert_eq!(parsed.info, b">hello");
/// # Ok::<(), Ax25Error>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiFrame<'a> {
    /// Destination address (C bit set on the wire, APRS convention).
    pub dest: Address,
    /// Source address (C bit clear on the wire).
    pub src: Address,
    /// Digipeater path storage; only the first `digipeater_count`
    /// entries are meaningful. Private so the pair cannot disagree:
    /// construct via [`UiFrame::with_path`] / [`UiFrame::with_hops`],
    /// read via [`UiFrame::path`] / [`UiFrame::hops`].
    digipeaters: [Address; MAX_DIGIPEATERS],
    /// Number of digipeaters in `digipeaters`, `0..=MAX_DIGIPEATERS`.
    digipeater_count: usize,
    /// Per-hop has-been-repeated (H) bits, bit `i` for `digipeaters[i]`.
    /// Clear by default — [`UiFrame::new`] and [`UiFrame::with_path`]
    /// build transmit-style all-unused paths; [`UiFrame::parse`] and
    /// [`UiFrame::with_hops`] carry whatever the hops say.
    repeated_bits: u8,
    /// The information field.
    pub info: &'a [u8],
}

impl<'a> UiFrame<'a> {
    /// Creates a UI frame with no digipeater path.
    #[must_use]
    pub const fn new(dest: Address, src: Address, info: &'a [u8]) -> Self {
        Self {
            dest,
            src,
            digipeaters: [PLACEHOLDER; MAX_DIGIPEATERS],
            digipeater_count: 0,
            repeated_bits: 0,
            info,
        }
    }

    /// Creates a UI frame with a digipeater path, all H bits clear.
    ///
    /// This is the transmit-side default: an originating station always
    /// sends its requested path unused. To build a frame with explicit
    /// per-hop has-been-repeated bits (e.g. when relaying), use
    /// [`UiFrame::with_hops`].
    ///
    /// # Errors
    ///
    /// [`Ax25Error::TooManyDigipeaters`] when `path.len() > MAX_DIGIPEATERS`.
    pub fn with_path(
        dest: Address,
        src: Address,
        path: &[Address],
        info: &'a [u8],
    ) -> Result<Self, Ax25Error> {
        if path.len() > MAX_DIGIPEATERS {
            return Err(Ax25Error::TooManyDigipeaters {
                got: path.len(),
                max: MAX_DIGIPEATERS,
            });
        }
        let mut frame = Self::new(dest, src, info);
        for (slot, addr) in frame.digipeaters.iter_mut().zip(path.iter()) {
            *slot = *addr;
        }
        frame.digipeater_count = path.len();
        Ok(frame)
    }

    /// Creates a UI frame with a digipeater path carrying explicit
    /// has-been-repeated (H) bits.
    ///
    /// A digipeater relaying a frame uses this to keep the already-used
    /// hops marked on the wire; [`UiFrame::with_path`] is the simpler
    /// all-clear transmit form.
    ///
    /// ```
    /// use yodel::ax25::{Address, Ax25Error, PathHop, UiFrame};
    ///
    /// let hops = [
    ///     PathHop { address: Address::new(b"N0CALL", 1)?, repeated: true },
    ///     PathHop::unused(Address::new(b"WIDE2", 1)?),
    /// ];
    /// let frame = UiFrame::with_hops(
    ///     Address::new(b"APRS", 0)?,
    ///     Address::new(b"N0CALL", 7)?,
    ///     &hops,
    ///     b">relayed",
    /// )?;
    /// let mut buf = [0u8; 64];
    /// let len = frame.build(&mut buf)?;
    /// // The used hop keeps its H bit on the wire (bit 7 of its SSID octet).
    /// assert_eq!(buf[14 + 6] & 0x80, 0x80);
    /// let parsed = UiFrame::parse(&buf[..len])?;
    /// let mut parsed_hops = parsed.hops();
    /// assert_eq!(parsed_hops.next(), Some(hops[0]));
    /// assert_eq!(parsed_hops.next(), Some(hops[1]));
    /// assert_eq!(parsed_hops.next(), None);
    /// # Ok::<(), Ax25Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// [`Ax25Error::TooManyDigipeaters`] when `hops.len() > MAX_DIGIPEATERS`.
    pub fn with_hops(
        dest: Address,
        src: Address,
        hops: &[PathHop],
        info: &'a [u8],
    ) -> Result<Self, Ax25Error> {
        if hops.len() > MAX_DIGIPEATERS {
            return Err(Ax25Error::TooManyDigipeaters {
                got: hops.len(),
                max: MAX_DIGIPEATERS,
            });
        }
        let mut frame = Self::new(dest, src, info);
        for (i, hop) in hops.iter().enumerate() {
            if let Some(slot) = frame.digipeaters.get_mut(i) {
                *slot = hop.address;
            }
            if hop.repeated {
                frame.repeated_bits |= 1 << i;
            }
        }
        frame.digipeater_count = hops.len();
        Ok(frame)
    }

    /// The digipeater path as a slice of addresses (H bits not visible;
    /// see [`UiFrame::hops`] for the per-hop repeated flags).
    #[must_use]
    pub fn path(&self) -> &[Address] {
        self.digipeaters
            .get(..self.digipeater_count.min(MAX_DIGIPEATERS))
            .unwrap_or(&[])
    }

    /// The digipeater path as per-hop (address, has-been-repeated) pairs.
    ///
    /// On a parsed frame the `repeated` flags reflect the received H
    /// bits; on a frame built via [`UiFrame::new`] / [`UiFrame::with_path`]
    /// they are all clear.
    pub fn hops(&self) -> impl Iterator<Item = PathHop> + '_ {
        self.path().iter().enumerate().map(|(i, &address)| PathHop {
            address,
            repeated: self.repeated_bits & (1 << i) != 0,
        })
    }

    /// The serialized length of this frame in bytes (excluding FCS).
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        (2 + self.digipeater_count) * ADDRESS_LEN + 2 + self.info.len()
    }

    /// Serializes the frame into a fresh vector.
    ///
    /// The ergonomic counterpart to [`UiFrame::build`]. The length is
    /// known in advance from [`UiFrame::encoded_len`], so unlike the
    /// buffer form this cannot fail.
    #[cfg(feature = "alloc")]
    #[must_use]
    pub fn to_vec(&self) -> alloc::vec::Vec<u8> {
        let mut out = alloc::vec![0u8; self.encoded_len()];
        let n = self
            .build(&mut out)
            .expect("a buffer of encoded_len() always fits");
        out.truncate(n);
        out
    }

    /// Serializes the frame body into `buf`, returning the written length.
    ///
    /// The output is what the FCS covers: it contains no FCS and no flags
    /// (the HDLC layer adds those; see [`super::hdlc::frame_bits`]).
    /// Address C bits follow the APRS convention (destination set, source
    /// and digipeaters clear); the extension bit is set on the final
    /// address only.
    ///
    /// # Errors
    ///
    /// [`Ax25Error::FrameTooLarge`] when `buf` is too small (nothing is
    /// written).
    pub fn build(&self, buf: &mut [u8]) -> Result<usize, Ax25Error> {
        let needed = self.encoded_len();
        if buf.len() < needed {
            return Err(Ax25Error::FrameTooLarge {
                len: needed,
                max: buf.len(),
            });
        }
        let mut pos = 0usize;
        let mut put = |bytes: &[u8], pos: &mut usize| -> bool {
            match buf.get_mut(*pos..*pos + bytes.len()) {
                Some(slot) => {
                    slot.copy_from_slice(bytes);
                    *pos += bytes.len();
                    true
                }
                None => false,
            }
        };
        let path = self.path();
        let src_is_last = path.is_empty();
        let mut ok = put(&self.dest.encode(true, false), &mut pos);
        ok &= put(&self.src.encode(false, src_is_last), &mut pos);
        for (i, digi) in path.iter().enumerate() {
            let repeated = self.repeated_bits & (1 << i) != 0;
            ok &= put(&digi.encode(repeated, i + 1 == path.len()), &mut pos);
        }
        ok &= put(&[CONTROL_UI, PID_NO_LAYER3], &mut pos);
        ok &= put(self.info, &mut pos);
        if ok {
            Ok(pos)
        } else {
            // Unreachable given the length check above, but degrade to a
            // typed error rather than trusting the invariant.
            Err(Ax25Error::FrameTooLarge {
                len: needed,
                max: buf.len(),
            })
        }
    }

    /// Parses a UI frame body (no FCS, no flags — as yielded by
    /// [`super::HdlcDeframer`]).
    ///
    /// # Errors
    ///
    /// [`Ax25Error::FrameTooShort`] when the body cannot hold two
    /// addresses, control and PID; address errors from
    /// [`Address::decode`]; [`Ax25Error::TooManyDigipeaters`] on an
    /// overlong path; [`Ax25Error::InvalidControl`] /
    /// [`Ax25Error::InvalidPid`] on non-UI control or PID bytes.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Ax25Error> {
        if bytes.len() < MIN_FRAME_LEN {
            return Err(Ax25Error::FrameTooShort {
                len: bytes.len(),
                min: MIN_FRAME_LEN,
            });
        }
        let mut pos = 0usize;
        let next_hop = |pos: &mut usize| -> Result<(PathHop, bool), Ax25Error> {
            let field: &[u8; ADDRESS_LEN] = bytes
                .get(*pos..*pos + ADDRESS_LEN)
                .and_then(|s| s.try_into().ok())
                .ok_or(Ax25Error::FrameTooShort {
                    len: bytes.len(),
                    min: *pos + ADDRESS_LEN + 2,
                })?;
            *pos += ADDRESS_LEN;
            PathHop::decode(field)
        };
        let (dest_hop, dest_last) = next_hop(&mut pos)?;
        let dest = dest_hop.address;
        if dest_last {
            // Extension bit on the destination: no source address follows.
            return Err(Ax25Error::FrameTooShort {
                len: bytes.len(),
                min: MIN_FRAME_LEN,
            });
        }
        let (src_hop, mut last) = next_hop(&mut pos)?;
        let mut frame = Self::new(dest, src_hop.address, &[]);
        while !last {
            if frame.digipeater_count == MAX_DIGIPEATERS {
                return Err(Ax25Error::TooManyDigipeaters {
                    got: MAX_DIGIPEATERS + 1,
                    max: MAX_DIGIPEATERS,
                });
            }
            let (digi, digi_last) = next_hop(&mut pos)?;
            if let Some(slot) = frame.digipeaters.get_mut(frame.digipeater_count) {
                *slot = digi.address;
            }
            if digi.repeated {
                frame.repeated_bits |= 1 << frame.digipeater_count;
            }
            frame.digipeater_count += 1;
            last = digi_last;
        }
        let control = bytes.get(pos).copied().ok_or(Ax25Error::FrameTooShort {
            len: bytes.len(),
            min: pos + 2,
        })?;
        // Accept UI with the poll/final bit either way: 0x03 and 0x13
        // are the same frame type, and both occur on the air.
        if control & CONTROL_PF_MASK != CONTROL_UI {
            return Err(Ax25Error::InvalidControl { got: control });
        }
        let pid = bytes
            .get(pos + 1)
            .copied()
            .ok_or(Ax25Error::FrameTooShort {
                len: bytes.len(),
                min: pos + 2,
            })?;
        if pid != PID_NO_LAYER3 {
            return Err(Ax25Error::InvalidPid { got: pid });
        }
        frame.info = bytes.get(pos + 2..).unwrap_or(&[]);
        Ok(frame)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use std::vec::Vec;

    use super::super::addr::Address;
    use super::*;

    fn addr(call: &[u8], ssid: u8) -> Address {
        match Address::new(call, ssid) {
            Ok(a) => a,
            Err(e) => panic!("{e}"),
        }
    }

    #[test]
    fn build_parse_round_trip_no_path() {
        let frame = UiFrame::new(addr(b"APRS", 0), addr(b"N0CALL", 7), b"hello world");
        let mut buf = [0u8; 64];
        let len = match frame.build(&mut buf) {
            Ok(n) => n,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(len, frame.encoded_len());
        let parsed = match UiFrame::parse(&buf[..len]) {
            Ok(p) => p,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(parsed.dest, frame.dest);
        assert_eq!(parsed.src, frame.src);
        assert_eq!(parsed.path(), &[]);
        assert_eq!(parsed.info, b"hello world");
    }

    #[test]
    fn build_parse_round_trip_with_path() {
        let path = [addr(b"WIDE1", 1), addr(b"WIDE2", 2)];
        let frame = match UiFrame::with_path(addr(b"APRS", 0), addr(b"K1ABC", 15), &path, b">test")
        {
            Ok(f) => f,
            Err(e) => panic!("{e}"),
        };
        let mut buf = [0u8; 128];
        let len = match frame.build(&mut buf) {
            Ok(n) => n,
            Err(e) => panic!("{e}"),
        };
        let parsed = match UiFrame::parse(&buf[..len]) {
            Ok(p) => p,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(parsed.path(), &path);
        assert_eq!(parsed.info, b">test");
        assert_eq!(parsed.src, frame.src);
    }

    #[test]
    fn wire_layout_control_and_pid() {
        let frame = UiFrame::new(addr(b"APRS", 0), addr(b"N0CALL", 0), b"x");
        let mut buf = [0u8; 32];
        let len = match frame.build(&mut buf) {
            Ok(n) => n,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(len, 17);
        assert_eq!(buf[14], 0x03);
        assert_eq!(buf[15], 0xF0);
        assert_eq!(buf[16], b'x');
        // Dest C bit set, src C bit clear, src extension bit set.
        assert_eq!(buf[6] & 0x80, 0x80);
        assert_eq!(buf[6] & 0x01, 0x00);
        assert_eq!(buf[13] & 0x80, 0x00);
        assert_eq!(buf[13] & 0x01, 0x01);
    }

    #[test]
    fn build_rejects_small_buffer() {
        let frame = UiFrame::new(addr(b"APRS", 0), addr(b"N0CALL", 0), b"payload");
        let mut buf = [0u8; 10];
        assert_eq!(
            frame.build(&mut buf),
            Err(Ax25Error::FrameTooLarge { len: 23, max: 10 })
        );
        // Nothing usable written on failure is not asserted (contents
        // unspecified), but the call must not panic.
    }

    #[test]
    fn with_path_rejects_too_many() {
        let digi = addr(b"WIDE1", 1);
        let path = [digi; MAX_DIGIPEATERS + 1];
        assert_eq!(
            UiFrame::with_path(addr(b"APRS", 0), addr(b"N0CALL", 0), &path, b""),
            Err(Ax25Error::TooManyDigipeaters {
                got: MAX_DIGIPEATERS + 1,
                max: MAX_DIGIPEATERS,
            })
        );
    }

    #[test]
    fn parse_rejects_short() {
        assert_eq!(
            UiFrame::parse(&[0u8; 5]),
            Err(Ax25Error::FrameTooShort { len: 5, min: 16 })
        );
    }

    #[test]
    fn parse_rejects_bad_control_and_pid() {
        let frame = UiFrame::new(addr(b"APRS", 0), addr(b"N0CALL", 0), b"");
        let mut buf = [0u8; 32];
        let len = match frame.build(&mut buf) {
            Ok(n) => n,
            Err(e) => panic!("{e}"),
        };
        let mut bad_control = buf;
        bad_control[14] = 0x2F;
        assert_eq!(
            UiFrame::parse(&bad_control[..len]),
            Err(Ax25Error::InvalidControl { got: 0x2F })
        );
        let mut bad_pid = buf;
        bad_pid[15] = 0xCC;
        assert_eq!(
            UiFrame::parse(&bad_pid[..len]),
            Err(Ax25Error::InvalidPid { got: 0xCC })
        );
    }

    /// `0x13` is a UI frame with the poll/final bit set — the same
    /// frame type as `0x03`, which APRS ignores. Real stations transmit
    /// it, and rejecting it also blocks HDLC bit-flip recovery, which is
    /// gated on a successful UI parse.
    #[test]
    fn parse_accepts_ui_with_poll_final_bit_set() {
        let frame = UiFrame::new(addr(b"APRS", 0), addr(b"N0CALL", 0), b"payload");
        let mut buf = [0u8; 32];
        let len = match frame.build(&mut buf) {
            Ok(n) => n,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(buf[14], CONTROL_UI);

        let mut pf_set = buf;
        pf_set[14] = 0x13;
        let parsed = match UiFrame::parse(&pf_set[..len]) {
            Ok(f) => f,
            Err(e) => panic!("0x13 must parse as a UI frame: {e}"),
        };
        assert_eq!(parsed.info, b"payload");

        // Only the P/F bit is forgiven; other U-format frames still fail.
        for control in [0x2F_u8, 0x3F, 0x43, 0x53, 0x63, 0x73, 0x87, 0x00] {
            let mut other = buf;
            other[14] = control;
            assert_eq!(
                UiFrame::parse(&other[..len]),
                Err(Ax25Error::InvalidControl { got: control }),
                "control {control:#04x} must not parse as UI"
            );
        }
    }

    #[test]
    fn parse_rejects_unterminated_address_field() {
        // Ten addresses with the extension bit never set: too many digis.
        let a = addr(b"WIDE1", 1);
        let mut bytes = Vec::new();
        for _ in 0..11 {
            bytes.extend_from_slice(&a.encode(false, false));
        }
        bytes.extend_from_slice(&[0x03, 0xF0]);
        assert_eq!(
            UiFrame::parse(&bytes),
            Err(Ax25Error::TooManyDigipeaters {
                got: MAX_DIGIPEATERS + 1,
                max: MAX_DIGIPEATERS,
            })
        );
    }

    #[test]
    fn parse_rejects_truncated_after_addresses() {
        let frame = UiFrame::new(addr(b"APRS", 0), addr(b"N0CALL", 0), b"");
        let mut buf = [0u8; 32];
        let len = match frame.build(&mut buf) {
            Ok(n) => n,
            Err(e) => panic!("{e}"),
        };
        // Drop the PID byte: 15 bytes < MIN_FRAME_LEN, rejected as short.
        assert!(UiFrame::parse(&buf[..len - 1]).is_err());
    }

    #[test]
    fn h_bits_round_trip_and_default_clear() {
        use super::super::addr::PathHop;
        let hops = [
            PathHop {
                address: addr(b"N0CALL", 1),
                repeated: true,
            },
            PathHop::unused(addr(b"WIDE2", 1)),
        ];
        let frame = match UiFrame::with_hops(addr(b"APRS", 0), addr(b"K1ABC", 0), &hops, b">h") {
            Ok(f) => f,
            Err(e) => panic!("{e}"),
        };
        let mut buf = [0u8; 64];
        let len = match frame.build(&mut buf) {
            Ok(n) => n,
            Err(e) => panic!("{e}"),
        };
        // First digi SSID octet (byte 20) carries the H bit; second does not.
        assert_eq!(buf[20] & 0x80, 0x80);
        assert_eq!(buf[27] & 0x80, 0x00);
        let parsed = match UiFrame::parse(&buf[..len]) {
            Ok(p) => p,
            Err(e) => panic!("{e}"),
        };
        let parsed_hops: Vec<PathHop> = parsed.hops().collect();
        assert_eq!(parsed_hops, hops);

        // The plain-address builders keep every H bit clear (TX default),
        // and stay byte-identical to an all-unused with_hops build.
        let path = [hops[0].address, hops[1].address];
        let plain = match UiFrame::with_path(addr(b"APRS", 0), addr(b"K1ABC", 0), &path, b">h") {
            Ok(f) => f,
            Err(e) => panic!("{e}"),
        };
        let mut plain_buf = [0u8; 64];
        let plain_len = match plain.build(&mut plain_buf) {
            Ok(n) => n,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(plain_buf[20] & 0x80, 0x00);
        assert!(plain.hops().all(|h| !h.repeated));
        let all_clear = [PathHop::unused(path[0]), PathHop::unused(path[1])];
        let via_hops =
            match UiFrame::with_hops(addr(b"APRS", 0), addr(b"K1ABC", 0), &all_clear, b">h") {
                Ok(f) => f,
                Err(e) => panic!("{e}"),
            };
        let mut hops_buf = [0u8; 64];
        let hops_len = match via_hops.build(&mut hops_buf) {
            Ok(n) => n,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(&plain_buf[..plain_len], &hops_buf[..hops_len]);
    }

    #[test]
    fn max_digipeaters_round_trip() {
        let digi = addr(b"WIDE2", 2);
        let path = [digi; MAX_DIGIPEATERS];
        let frame = match UiFrame::with_path(addr(b"APRS", 0), addr(b"N0CALL", 3), &path, b"deep") {
            Ok(f) => f,
            Err(e) => panic!("{e}"),
        };
        let mut buf = [0u8; 128];
        let len = match frame.build(&mut buf) {
            Ok(n) => n,
            Err(e) => panic!("{e}"),
        };
        let parsed = match UiFrame::parse(&buf[..len]) {
            Ok(p) => p,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(parsed.path().len(), MAX_DIGIPEATERS);
        assert_eq!(parsed.info, b"deep");
    }
}
