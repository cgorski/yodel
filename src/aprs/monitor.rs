//! TNC2 monitor-format lines: `SRC>DEST,PATH:information`.
//!
//! This is the text form APRS travels in when it is not on the air. It
//! is what an APRS-IS server streams, what most TNCs print in monitor
//! mode, and what sits inside a third-party frame after the `}`.
//! Parsing it is the way into this crate for anyone who has APRS text
//! rather than audio.
//!
//! ```text
//! N0CALL-7>APRS,WIDE1-1,qAR,IGATE-1:!4903.50N/07201.75W-hi
//! └─src──┘ └dst┘ └───── path ─────┘ └────── information ──┘
//! ```
//!
//! # Addresses are text, not [`Address`]
//!
//! APRS-IS is not bound by AX.25 address rules. A source may exceed six
//! characters, use lower case, or carry a two-character alphanumeric
//! SSID, and the path routinely holds tokens that never appear on RF:
//! `TCPIP`, `NOGATE`, `RFONLY`, and the `q` constructs. Forcing those
//! through [`Address`] would reject the traffic this module exists to
//! read, so every field is a raw slice. [`MonitorLine::source_address`]
//! and [`MonitorLine::dest_address`] convert where it is legal.
//!
//! # Bytes, not text
//!
//! The information field is arbitrary bytes. Mic-E position reports are
//! binary by construction, and comment fields carry both valid UTF-8
//! and bare Latin-1. Nothing here decodes, trims or validates it.
//!
//! ```
//! use yodel::aprs::monitor::MonitorLine;
//!
//! let line = MonitorLine::parse(b"N0CALL-7>APRS,WIDE1-1,qAR,IGATE-1:>hello")?;
//! assert_eq!(line.source, b"N0CALL-7");
//! assert_eq!(line.dest, b"APRS");
//! assert_eq!(line.info, b">hello");
//! assert_eq!(line.q_construct(), Some(&b"qAR"[..]));
//! assert_eq!(line.igate(), Some(&b"IGATE-1"[..]));
//! assert!(line.is_from_rf());
//! # Ok::<(), yodel::aprs::AprsError>(())
//! ```

use super::{AprsError, Decoded};
use crate::ax25::Address;

/// Longest callsign accepted in any header field.
///
/// AX.25 allows six characters plus a numeric SSID; APRS-IS permits up
/// to nine characters in total including an alphanumeric SSID, which is
/// the limit applied here.
pub const CALLSIGN_MAX: usize = 9;

/// Longest APRS-IS line, including the terminating CR/LF.
///
/// The APRS-IS specification caps a line at 512 bytes. Readers should
/// treat anything longer as a protocol violation rather than growing a
/// buffer to fit it.
pub const LINE_MAX: usize = 512;

/// One parsed TNC2 monitor line.
///
/// Every field borrows from the input. Build one with
/// [`MonitorLine::parse`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonitorLine<'a> {
    /// The transmitting station, as text.
    pub source: &'a [u8],
    /// The destination, which in APRS carries a device identifier
    /// rather than a routing target, or Mic-E latitude digits.
    pub dest: &'a [u8],
    /// Everything between the destination and the `:`, comma separated,
    /// without the surrounding separators. Empty when absent.
    pub path: &'a [u8],
    /// The information field, including its data type identifier.
    pub info: &'a [u8],
}

impl<'a> MonitorLine<'a> {
    /// Parses one line.
    ///
    /// Any trailing CR or LF is ignored, so a line may be passed either
    /// with or without its terminator. The information field is
    /// everything after the **first** `:`, which matters because
    /// message packets begin their information field with another one.
    ///
    /// # Errors
    ///
    /// * [`AprsError::Truncated`] when there is no `>` separating
    ///   source from destination, or no `:` ending the header.
    /// * [`AprsError::BadCallsignLength`] when the source or
    ///   destination is empty or longer than [`CALLSIGN_MAX`].
    pub const fn parse(line: &'a [u8]) -> Result<Self, AprsError> {
        let line = trim_eol(line);

        let colon = match find(line, b':') {
            Some(i) => i,
            None => {
                return Err(AprsError::Truncated {
                    expected: line.len() + 1,
                    got: line.len(),
                });
            }
        };
        let (header, after) = line.split_at(colon);
        // `split_at` leaves the ':' at the head of `after`.
        let info = match after.split_first() {
            Some((_, p)) => p,
            None => &[],
        };

        let gt = match find(header, b'>') {
            Some(i) => i,
            None => {
                return Err(AprsError::Truncated {
                    expected: header.len() + 1,
                    got: header.len(),
                });
            }
        };
        let (source, dest_and_path) = header.split_at(gt);
        let dest_and_path = match dest_and_path.split_first() {
            Some((_, d)) => d,
            None => &[],
        };
        let (dest, path) = match find(dest_and_path, b',') {
            Some(i) => {
                let (d, p) = dest_and_path.split_at(i);
                match p.split_first() {
                    Some((_, p)) => (d, p),
                    None => (d, &[] as &[u8]),
                }
            }
            None => (dest_and_path, &[] as &[u8]),
        };

        if source.is_empty() || source.len() > CALLSIGN_MAX {
            return Err(AprsError::BadCallsignLength { len: source.len() });
        }
        if dest.is_empty() || dest.len() > CALLSIGN_MAX {
            return Err(AprsError::BadCallsignLength { len: dest.len() });
        }

        Ok(Self {
            source,
            dest,
            path,
            info,
        })
    }

    /// Decodes the information field into a typed payload.
    ///
    /// Mic-E reports carry half their position in the destination, so
    /// the destination is supplied when it is a legal AX.25 address.
    /// When it is not, the frame cannot be Mic-E anyway, and the
    /// information field alone decides.
    ///
    /// This is total: an unparseable field comes back labelled rather
    /// than lost. See [`Decoded`].
    #[must_use]
    pub fn decoded(&self) -> Decoded<'a> {
        match self.dest_address() {
            Some(dest) => Decoded::decode_frame(dest, self.info),
            None => Decoded::decode(self.info),
        }
    }

    /// The source as a validated AX.25 [`Address`], when it is one.
    ///
    /// Returns `None` for the APRS-IS-only forms this module accepts:
    /// callsigns over six characters, lower case, or a non-numeric
    /// SSID.
    #[must_use]
    pub fn source_address(&self) -> Option<Address> {
        parse_text_address(self.source)
    }

    /// The destination as a validated AX.25 [`Address`], when it is
    /// one. As [`MonitorLine::source_address`].
    #[must_use]
    pub fn dest_address(&self) -> Option<Address> {
        parse_text_address(self.dest)
    }

    /// Walks the path left to right.
    #[must_use]
    pub const fn hops(&self) -> Hops<'a> {
        Hops { rest: self.path }
    }

    /// The APRS-IS `q` construct, when the path carries one.
    ///
    /// A `q` construct records how a packet entered APRS-IS. It is
    /// inserted by the server, always appears as the pair
    /// `qXX,CALLSIGN`, and never travels on RF.
    #[must_use]
    pub fn q_construct(&self) -> Option<&'a [u8]> {
        for hop in self.hops() {
            if is_q_construct(hop.call) {
                return Some(hop.call);
            }
        }
        None
    }

    /// The station named immediately after the `q` construct.
    ///
    /// For `qAR` and its relatives this is the igate that heard the
    /// packet on RF. For `qAC` it is the server the client was logged
    /// in to.
    #[must_use]
    pub fn igate(&self) -> Option<&'a [u8]> {
        let mut hops = self.hops();
        while let Some(hop) = hops.next() {
            if is_q_construct(hop.call) {
                return hops.next().map(|h| h.call);
            }
        }
        None
    }

    /// Whether the packet reached APRS-IS from a radio.
    ///
    /// True for the gated constructs `qAR`, `qAr`, `qAO` and `qAo`.
    /// False for `qAC`, `qAS`, `qAU` and `qAX`, which mark traffic that
    /// was injected over the Internet, and false when there is no `q`
    /// construct at all.
    #[must_use]
    pub fn is_from_rf(&self) -> bool {
        matches!(self.q_construct(), Some(b"qAR" | b"qAr" | b"qAO" | b"qAo"))
    }

    /// Whether the path forbids gating this packet to RF.
    ///
    /// True when the path holds `NOGATE` or `RFONLY`.
    #[must_use]
    pub fn is_gate_forbidden(&self) -> bool {
        self.hops()
            .any(|h| eq_ignore_case(h.call, b"NOGATE") || eq_ignore_case(h.call, b"RFONLY"))
    }

    /// Whether the path contains `needle`, ignoring any `*` flag and
    /// ASCII case.
    #[must_use]
    pub fn path_contains(&self, needle: &[u8]) -> bool {
        self.hops().any(|h| eq_ignore_case(h.call, needle))
    }
}

/// Formats a TNC2 monitor line from its parts.
///
/// The inverse of [`MonitorLine::parse`], for writing capture files,
/// feeding another program, or building the text an APRS-IS client
/// would send. Pass an empty `path` to omit it.
///
/// The information field is copied verbatim, so this round-trips any
/// bytes that do not contain CR, LF or the separators themselves.
///
/// ```
/// # #[cfg(all(feature = "aprs", feature = "alloc"))] {
/// use yodel::aprs::monitor::{MonitorLine, format_line};
///
/// let line = format_line(b"N0CALL-7", b"APRS", b"WIDE1-1", b">hello");
/// assert_eq!(line, b"N0CALL-7>APRS,WIDE1-1:>hello");
///
/// // And it parses back to what went in.
/// let back = MonitorLine::parse(&line)?;
/// assert_eq!(back.source, b"N0CALL-7");
/// assert_eq!(back.info, b">hello");
/// # }
/// # Ok::<(), yodel::aprs::AprsError>(())
/// ```
#[cfg(feature = "alloc")]
#[must_use]
pub fn format_line(source: &[u8], dest: &[u8], path: &[u8], info: &[u8]) -> alloc::vec::Vec<u8> {
    let mut out =
        alloc::vec::Vec::with_capacity(source.len() + dest.len() + path.len() + info.len() + 3);
    out.extend_from_slice(source);
    out.push(b'>');
    out.extend_from_slice(dest);
    if !path.is_empty() {
        out.push(b',');
        out.extend_from_slice(path);
    }
    out.push(b':');
    out.extend_from_slice(info);
    out
}

/// One element of a digipeater path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hop<'a> {
    /// The callsign, with any trailing `*` removed.
    pub call: &'a [u8],
    /// Whether the element carried `*`, meaning the frame has already
    /// been repeated by that station.
    pub repeated: bool,
}

/// Iterator over the elements of a [`MonitorLine`] path.
#[derive(Debug, Clone, Copy)]
pub struct Hops<'a> {
    rest: &'a [u8],
}

impl<'a> Iterator for Hops<'a> {
    type Item = Hop<'a>;

    fn next(&mut self) -> Option<Hop<'a>> {
        if self.rest.is_empty() {
            return None;
        }
        let (elem, rest) = match find(self.rest, b',') {
            Some(i) => {
                let (e, r) = self.rest.split_at(i);
                (e, &r[1..])
            }
            None => (self.rest, &[] as &[u8]),
        };
        self.rest = rest;
        let repeated = matches!(elem.last(), Some(b'*'));
        let call = if repeated {
            &elem[..elem.len() - 1]
        } else {
            elem
        };
        Some(Hop { call, repeated })
    }
}

/// Whether a path element is an APRS-IS `q` construct.
///
/// The form is `q` followed by two characters, the first of which is
/// upper case `A`.
#[must_use]
pub const fn is_q_construct(elem: &[u8]) -> bool {
    elem.len() == 3 && elem[0] == b'q' && elem[1] == b'A'
}

const fn trim_eol(mut line: &[u8]) -> &[u8] {
    while let Some((&last, head)) = line.split_last() {
        if last == b'\r' || last == b'\n' {
            line = head;
        } else {
            break;
        }
    }
    line
}

const fn find(haystack: &[u8], needle: u8) -> Option<usize> {
    let mut i = 0;
    while i < haystack.len() {
        if haystack[i] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn eq_ignore_case(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.eq_ignore_ascii_case(y))
}

/// Parses `CALL` or `CALL-SSID` text into an AX.25 [`Address`].
///
/// Returns `None` when the text is not a legal AX.25 address, which
/// APRS-IS traffic frequently is not.
pub(super) fn parse_text_address(text: &[u8]) -> Option<Address> {
    let (call, ssid) = match find(text, b'-') {
        Some(i) => {
            let (c, s) = text.split_at(i);
            let digits = &s[1..];
            if digits.is_empty() || digits.len() > 2 {
                return None;
            }
            let mut n: u8 = 0;
            for &d in digits {
                if !d.is_ascii_digit() {
                    return None;
                }
                n = n.checked_mul(10)?.checked_add(d - b'0')?;
            }
            (c, n)
        }
        None => (text, 0),
    };
    Address::new(call, ssid).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aprs::{AprsPacket, DecodedKind};

    /// The information field is everything after the *first* colon.
    /// Message packets open theirs with another one, and splitting on
    /// the wrong colon is the classic APRS-IS parsing bug.
    #[test]
    fn message_packet_splits_on_the_first_colon_only() {
        let line =
            MonitorLine::parse(b"KQ4ZAX-5>APFII0,TCPIP*,qAC,APRSFI::OTA      :CQ{D447B").unwrap();
        assert_eq!(line.source, b"KQ4ZAX-5");
        assert_eq!(line.dest, b"APFII0");
        assert_eq!(line.info, b":OTA      :CQ{D447B");
        assert!(matches!(
            line.decoded().kind,
            DecodedKind::Packet(AprsPacket::Message(_))
        ));
    }

    /// A status report's information field opens with `>`, so the
    /// source must be split on the first `>` as well.
    #[test]
    fn status_report_gt_in_payload_does_not_confuse_the_source() {
        let line =
            MonitorLine::parse(b"W8JES>APU25N,TCPIP*,qAC,T2MCI:>210002zFindlay's IGate").unwrap();
        assert_eq!(line.source, b"W8JES");
        assert_eq!(line.info[0], b'>');
    }

    /// `*` marks the last station that repeated the frame. It belongs
    /// to the path element, not to the callsign.
    #[test]
    fn hops_strip_the_has_been_repeated_flag() {
        let line = MonitorLine::parse(b"N8TAG-1>SXUX4Y,W8BLV,WIDE1*,WIDE2-1,qAR,KF8I:x").unwrap();
        let hops: [(&[u8], bool); 5] = [
            (b"W8BLV", false),
            (b"WIDE1", true),
            (b"WIDE2-1", false),
            (b"qAR", false),
            (b"KF8I", false),
        ];
        for (got, (call, repeated)) in line.hops().zip(hops) {
            assert_eq!(got.call, call);
            assert_eq!(got.repeated, repeated);
        }
        assert_eq!(line.hops().count(), 5);
    }

    /// The q construct records how a packet reached APRS-IS, and the
    /// element after it names the entry station.
    #[test]
    fn q_constructs_separate_rf_from_internet() {
        let rf = MonitorLine::parse(b"K3RTA>APWW10,WIDE1-1,qAR,W3ISR-10:!x").unwrap();
        assert_eq!(rf.q_construct(), Some(&b"qAR"[..]));
        assert_eq!(rf.igate(), Some(&b"W3ISR-10"[..]));
        assert!(rf.is_from_rf());

        let net = MonitorLine::parse(b"KT4ROY-10>APRS,TCPIP*,qAC,SA7AUX:!x").unwrap();
        assert_eq!(net.q_construct(), Some(&b"qAC"[..]));
        assert_eq!(net.igate(), Some(&b"SA7AUX"[..]));
        assert!(!net.is_from_rf(), "qAC is injected over the Internet");

        let bare = MonitorLine::parse(b"N0CALL>APRS,WIDE1-1:!x").unwrap();
        assert_eq!(bare.q_construct(), None);
        assert!(!bare.is_from_rf());
    }

    /// Mic-E carries half its position in the destination, so
    /// `decoded` must pass the destination through.
    #[cfg(feature = "micE")]
    #[test]
    fn mic_e_needs_the_destination_and_decoded_supplies_it() {
        let info = b"`(_fn\"Oj/";
        let mut line_buf = [0u8; 64];
        let header = b"KD9RDO-8>S32UVT,WIDE1-1,qAR,AD9BU-10:";
        line_buf[..header.len()].copy_from_slice(header);
        line_buf[header.len()..header.len() + info.len()].copy_from_slice(info);
        let line = MonitorLine::parse(&line_buf[..header.len() + info.len()]).unwrap();
        assert_eq!(line.dest, b"S32UVT");
        // Without the destination the same field is only "needs destination".
        assert!(matches!(
            Decoded::decode(line.info).kind,
            DecodedKind::NeedsDestination { .. }
        ));
        assert!(matches!(line.decoded().kind, DecodedKind::MicE(_)));
    }

    /// APRS-IS callsigns are not bound by AX.25 rules, so the text
    /// fields accept what `Address` cannot represent.
    #[test]
    fn addresses_convert_only_when_legal() {
        let line = MonitorLine::parse(b"N0CALL-7>APRS:!x").unwrap();
        assert_eq!(line.source_address().unwrap().ssid.value(), 7);

        let long = MonitorLine::parse(b"LONGCALL1>APRS:!x").unwrap();
        assert_eq!(long.source, b"LONGCALL1");
        assert!(long.source_address().is_none(), "9 chars is not AX.25");

        let alpha = MonitorLine::parse(b"N0CALL-AB>APRS:!x").unwrap();
        assert!(alpha.source_address().is_none(), "SSID must be numeric");
    }

    /// A line may arrive with or without its terminator.
    #[test]
    fn trailing_crlf_is_ignored() {
        let with = MonitorLine::parse(b"N0CALL>APRS:>hi\r\n").unwrap();
        let without = MonitorLine::parse(b"N0CALL>APRS:>hi").unwrap();
        assert_eq!(with, without);
    }

    #[test]
    fn gate_forbidden_and_path_search_ignore_case_and_flags() {
        let line = MonitorLine::parse(b"N0CALL>APRS,WIDE1*,NOGATE,qAR,IGATE:!x").unwrap();
        assert!(line.is_gate_forbidden());
        assert!(line.path_contains(b"wide1"), "case and * are ignored");
        assert!(!line.path_contains(b"WIDE2"));
    }

    #[test]
    fn malformed_lines_are_rejected_not_panicked_on() {
        assert!(MonitorLine::parse(b"").is_err());
        assert!(MonitorLine::parse(b"no separators at all").is_err());
        assert!(MonitorLine::parse(b"N0CALL>APRS no colon").is_err());
        assert!(MonitorLine::parse(b">APRS:x").is_err(), "empty source");
        assert!(MonitorLine::parse(b"N0CALL>:x").is_err(), "empty dest");
        assert!(MonitorLine::parse(b"TOOLONGCALL>APRS:x").is_err());
    }

    /// Every byte sequence either parses or returns an error. Nothing
    /// panics, and the information field is always a suffix of input.
    #[test]
    fn parsing_is_total_over_adversarial_input() {
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut buf = [0u8; 48];
        for _ in 0..20_000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let len = (state as usize) % buf.len();
            for (i, b) in buf[..len].iter_mut().enumerate() {
                *b = match (state >> (i % 8)) as u8 % 8 {
                    0 => b'>',
                    1 => b':',
                    2 => b',',
                    3 => b'*',
                    n => b'A' + n,
                };
            }
            if let Ok(line) = MonitorLine::parse(&buf[..len]) {
                assert!(line.info.len() <= len);
                let _ = line.decoded();
                assert_eq!(line.hops().count(), line.hops().count());
            }
        }
    }
}
