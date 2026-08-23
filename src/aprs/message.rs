//! APRS text messages, acknowledgements and rejections (`:`).
//!
//! A message is `:ADDRESSEE:text{id` (APRS 1.01 chapter 14): a `:`
//! identifier, a nine-character space-padded addressee, another `:`,
//! the message text, and an optional message id after `{` (1..=5
//! characters). Replies `ackXXXXX` / `rejXXXXX` are recognized as their
//! own [`MessageContent`] variants.
//!
//! APRS 1.1 (chapter 14, "New Message Number Format") gives that id an
//! internal structure, `MM}AA`, without changing its length or its
//! delimiter. It is stored and re-emitted as the same opaque slice;
//! [`MessageContent::reply_ack`] and [`MessageContent::acked_number`]
//! read the two halves back out of it on demand.

use super::AprsError;
use super::telemetry::TelemetryDefinition;

/// Maximum addressee length in characters.
pub const ADDRESSEE_MAX: usize = 9;
/// Maximum message id length in characters.
pub const MESSAGE_ID_MAX: usize = 5;
/// The message text length APRS 1.01 specifies as the maximum, in
/// characters.
///
/// Informational only: this crate does **not** enforce it. Neither
/// [`Message::build`] nor [`Message::parse`] consults this constant, and
/// nothing else in the crate references it. A 200-character text builds
/// and parses back unchanged, with no error. It is published so callers
/// that want to be spec-conformant have the number to check against
/// themselves.
pub const TEXT_MAX: usize = 67;

/// A message addressee: 1..=9 printable ASCII characters (no space or
/// `:`), stored space-padded to the fixed field width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Addressee {
    field: [u8; ADDRESSEE_MAX],
    len: usize,
}

impl Addressee {
    /// Validates and creates an addressee.
    ///
    /// # Errors
    ///
    /// [`AprsError::AddresseeEmpty`] / [`AprsError::AddresseeTooLong`]
    /// on a bad length; [`AprsError::InvalidAddresseeChar`] on a byte
    /// outside printable ASCII or equal to space or `:`.
    pub const fn new(name: &[u8]) -> Result<Self, AprsError> {
        if name.is_empty() {
            return Err(AprsError::AddresseeEmpty);
        }
        if name.len() > ADDRESSEE_MAX {
            return Err(AprsError::AddresseeTooLong { len: name.len() });
        }
        let mut field = [b' '; ADDRESSEE_MAX];
        let mut i = 0;
        while i < name.len() {
            let byte = name[i];
            if byte <= b' ' || byte > b'~' || byte == b':' {
                return Err(AprsError::InvalidAddresseeChar { got: byte });
            }
            field[i] = byte;
            i += 1;
        }
        Ok(Self {
            field,
            len: name.len(),
        })
    }

    /// The addressee characters (without padding).
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.field.get(..self.len.min(ADDRESSEE_MAX)).unwrap_or(&[])
    }

    /// The space-padded nine-byte field as sent on the air.
    #[must_use]
    pub const fn padded(&self) -> [u8; ADDRESSEE_MAX] {
        self.field
    }

    /// Parses a nine-byte space-padded addressee field.
    const fn decode(field: [u8; ADDRESSEE_MAX]) -> Result<Self, AprsError> {
        // Meaningful length: up to the last non-space byte.
        let mut len = ADDRESSEE_MAX;
        while len > 0 && field[len - 1] == b' ' {
            len -= 1;
        }
        if len == 0 {
            return Err(AprsError::AddresseeEmpty);
        }
        let mut i = 0;
        while i < len {
            let byte = field[i];
            if byte <= b' ' || byte > b'~' || byte == b':' {
                return Err(AprsError::InvalidAddresseeChar { got: byte });
            }
            i += 1;
        }
        Ok(Self { field, len })
    }
}

/// The body of a message: text, or an ack/rej reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageContent<'a> {
    /// Ordinary message text with an optional `{`-prefixed id.
    Text {
        /// The message text (may be empty).
        text: &'a [u8],
        /// The message id following `{`, when present (1..=5 bytes).
        id: Option<&'a [u8]>,
    },
    /// An acknowledgement: `ack` followed by the id being acked.
    Ack {
        /// The acknowledged message id (1..=5 bytes).
        id: &'a [u8],
    },
    /// A rejection: `rej` followed by the id being rejected.
    Reject {
        /// The rejected message id (1..=5 bytes).
        id: &'a [u8],
    },
}

impl<'a> MessageContent<'a> {
    /// The message id this content carries, if any.
    ///
    /// The three variants spell it differently on the wire — after `{`
    /// for text, after the literal `ack`/`rej` for a reply — but it is
    /// the same field in each, so both accessors below share this one
    /// lookup.
    const fn id_slice(&self) -> Option<&'a [u8]> {
        match *self {
            Self::Text { id, .. } => id,
            Self::Ack { id } | Self::Reject { id } => Some(id),
        }
    }

    /// Splits a reply-ACK id into the number being sent and the number
    /// being acknowledged, as `(MM, AA)`.
    ///
    /// # Wire layout
    ///
    /// Chapter 14 ("New Message Number Format", December 1999) redefines
    /// the id trailing a message line as `{MM}AA`, where `MM` is the
    /// sender's own outgoing message number and `AA` is a "free ACK"
    /// piggy-backed onto it. With nothing to acknowledge the id is the
    /// bare `{MM}`: the chapter makes the lone trailing `}` meaningful,
    /// since "even if there is no ACK, the presence of the trailing `}`
    /// tells the other end that the sender is REPLY-ACK capable". `AA`
    /// is then *empty*, not absent. A plain 1.01 id carries no `}` at
    /// all.
    ///
    /// The `{` is the delimiter and is not part of the id, so `MM}AA`
    /// still fits the [`MESSAGE_ID_MAX`] bytes an id is allowed and the
    /// length check needs no special case for it.
    ///
    /// # Returns
    ///
    /// `None` when this content has no id, or when the id is a plain
    /// 1.01 number with no `}`. Otherwise the id split at its **first**
    /// `}`, which is not consumed by either half. Either half may be
    /// empty, and the tail is returned verbatim — a malformed id with a
    /// second `}` keeps it in `AA`.
    ///
    /// This also answers for [`MessageContent::Ack`] and
    /// [`MessageContent::Reject`], whose id is by chapter 14's rule an
    /// exact copy of the id being answered and so has the same layout.
    /// To match such an ack against an outgoing queue, use
    /// [`acked_number`](Self::acked_number) instead.
    ///
    /// # Why this is an accessor and not a variant
    ///
    /// A `TextReplyAck { text, id, ack }` variant would carry a third
    /// slice, taking [`Message`] from 64 to 80 bytes for *every*
    /// message including the 1.01 ones that never use the format; it
    /// would be a breaking change to a public enum; and it would oblige
    /// [`Message::build`] to reconstruct `{MM}AA` from the pieces,
    /// which means deciding whether an empty `ack` re-emits the
    /// capability-announcing `}` or drops it — different bytes on the
    /// air, and only the original frame knows which was sent. Borrowing
    /// out of the id slice that is already stored yields the same
    /// information for none of that, and keeps the round trip
    /// byte-exact by construction: the id is never taken apart, so it
    /// cannot be put back together differently.
    ///
    /// ```
    /// use warble::aprs::{AprsError, Message, MessageContent};
    ///
    /// // "...happy Thanksgiving{Re}1j": message "Re", acking "1j".
    /// let msg = Message::parse(b":WA6LDQ   :Okay will do soon{Re}1j")?;
    /// assert_eq!(msg.content.reply_ack(), Some((&b"Re"[..], &b"1j"[..])));
    ///
    /// // Bare capability marker: no ACK pending.
    /// let cap = Message::parse(b":WA6LDQ   :Okay will do soon{Re}")?;
    /// assert_eq!(cap.content.reply_ack(), Some((&b"Re"[..], &b""[..])));
    ///
    /// // A plain 1.01 id is not a reply-ACK.
    /// let old = Message::parse(b":WA6LDQ   :Okay will do soon{003")?;
    /// assert_eq!(old.content.reply_ack(), None);
    /// # Ok::<(), AprsError>(())
    /// ```
    #[must_use]
    pub fn reply_ack(&self) -> Option<(&'a [u8], &'a [u8])> {
        let id = self.id_slice()?;
        let brace = id.iter().position(|&b| b == b'}')?;
        Some((
            id.get(..brace).unwrap_or(&[]),
            id.get(brace + 1..).unwrap_or(&[]),
        ))
    }

    /// The message number an [`Ack`](Self::Ack) or
    /// [`Reject`](Self::Reject) is answering.
    ///
    /// # Wire layout
    ///
    /// Chapter 14 leaves the acknowledgement itself alone — "even if
    /// XXX.. is `MM}AA` then the ack is just the exact copy as before
    /// `ackMM}AA`" — so the id of an `ack`/`rej` is whatever the
    /// original message put after its `{`, copied byte for byte. The
    /// station being acknowledged, however, has only `{MM}` outstanding
    /// in its queue and never sent the `}AA` tail, so the chapter's
    /// matching rule is to "pull out the `MM` here and use IT to match":
    /// take everything before the first `}`.
    ///
    /// # Returns
    ///
    /// The id truncated at its first `}` when there is one — the
    /// `ackMM}AA` spelling — and the whole id otherwise, which is the
    /// 1.01 `ackXXXXX` case. `None` for [`Text`](Self::Text), which
    /// acknowledges nothing.
    ///
    /// A degenerate id beginning with `}` yields an empty slice. That is
    /// allowed rather than special-cased away: [`Message::build`]
    /// rejects an empty id, so no message number can ever have been sent
    /// empty, and such an ack correctly matches nothing.
    ///
    /// ```
    /// use warble::aprs::{AprsError, Message, MessageContent};
    ///
    /// // Chapter 14's "pull out the MM": this acks message "Re".
    /// let new = Message::parse(b":WA6UVQ   :ackRe}1j")?;
    /// assert_eq!(new.content, MessageContent::Ack { id: b"Re}1j" });
    /// assert_eq!(new.content.acked_number(), Some(&b"Re"[..]));
    ///
    /// // A 1.01 ack refers to its whole id.
    /// let old = Message::parse(b":WA6UVQ   :ack003")?;
    /// assert_eq!(old.content.acked_number(), Some(&b"003"[..]));
    /// # Ok::<(), AprsError>(())
    /// ```
    #[must_use]
    pub fn acked_number(&self) -> Option<&'a [u8]> {
        let id = match *self {
            Self::Ack { id } | Self::Reject { id } => id,
            Self::Text { .. } => return None,
        };
        Some(match id.iter().position(|&b| b == b'}') {
            Some(brace) => id.get(..brace).unwrap_or(&[]),
            None => id,
        })
    }
}

/// An APRS message: addressee plus content.
///
/// # Wire round trip
///
/// The wire form is `:ADDRESSEE:text{id` — the addressee is always
/// space-padded to nine characters, and the optional message id follows
/// a `{`. Build and parse are exact inverses:
///
/// ```
/// use warble::aprs::{Addressee, AprsError, Message, MessageContent};
///
/// let msg = Message {
///     addressee: Addressee::new(b"N0CALL")?,
///     content: MessageContent::Text {
///         text: b"Testing",
///         id: Some(b"003"),
///     },
/// };
/// let mut buf = [0u8; 32];
/// let len = msg.build(&mut buf)?;
/// assert_eq!(&buf[..len], b":N0CALL   :Testing{003"); // 9-char padded field
/// assert_eq!(Message::parse(&buf[..len])?, msg);
///
/// // Replies are recognized as their own variants, not as text.
/// let ack = Message::parse(b":N0CALL   :ack003")?;
/// assert_eq!(ack.content, MessageContent::Ack { id: b"003" });
/// # Ok::<(), AprsError>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Message<'a> {
    /// Who the message is for.
    pub addressee: Addressee,
    /// The message body.
    pub content: MessageContent<'a>,
}

impl<'a> Message<'a> {
    /// The fixed prefix: `:`, nine addressee bytes, `:`.
    const HEADER_LEN: usize = 1 + ADDRESSEE_MAX + 1;

    /// Reads this message as a chapter 13 telemetry definition, when it
    /// is one.
    ///
    /// `PARM.`, `UNIT.`, `EQNS.` and `BITS.` describe what a station's
    /// telemetry channels mean. They travel as ordinary messages, and
    /// this is a **view** over the text rather than a variant of
    /// [`MessageContent`], for the reason recorded on
    /// [`MessageContent::reply_ack`]: the text still parses and
    /// rebuilds byte for byte, so reading these cannot reject a packet
    /// that used to decode, and a form this crate cannot type returns
    /// `None` with the text still in hand.
    ///
    /// # Bind these to the sender, not to the addressee
    ///
    /// A definition message describes the telemetry of the station that
    /// **sent** it, and usually addresses itself, which makes the
    /// addressee look like the right key. It is not.
    ///
    /// MEASURED over 95 219 packets, **277 of 5 805** definition
    /// messages address a different callsign: an EchoLink and SvxLink
    /// family that sends from `KJ6ZD` addressed to `EL-KJ6ZD`, another
    /// prefixing `ER-`, and 91 addressing something unrelated. A
    /// decoder that keys this metadata on [`addressee`] never binds it
    /// to the station whose telemetry it describes, and nothing errors:
    /// the definitions simply never arrive. Key on the source address
    /// of the frame, which this type does not carry, and use
    /// [`addressee`] only if you have a reason to.
    ///
    /// [`addressee`]: Self::addressee
    /// [`MessageContent::reply_ack`]: MessageContent::reply_ack
    ///
    /// ```
    /// use warble::aprs::{Message, TelemetryDefinition};
    ///
    /// let m = Message::parse(b":N0QBF-11 :BITS.10110000,N0QBF's Big Balloon")?;
    /// let Some(TelemetryDefinition::BitSense(bits)) = m.telemetry_definition() else {
    ///     panic!("a BITS. message");
    /// };
    /// assert_eq!(bits.sense.map(|s| s[0]), Some(true));
    /// assert_eq!(bits.title, &b"N0QBF's Big Balloon"[..]);
    /// # Ok::<(), warble::aprs::AprsError>(())
    /// ```
    #[must_use]
    pub fn telemetry_definition(&self) -> Option<TelemetryDefinition<'a>> {
        match self.content {
            MessageContent::Text { text, .. } => TelemetryDefinition::parse(text),
            // An ack or a reject carries an id, never a definition.
            MessageContent::Ack { .. } | MessageContent::Reject { .. } => None,
        }
    }

    /// Parses a `:` message.
    ///
    /// # Errors
    ///
    /// [`AprsError::Truncated`] when shorter than the fixed header;
    /// [`AprsError::InvalidDataType`] when the identifier is not `:`;
    /// [`AprsError::ExpectedByte`] when the second `:` is missing;
    /// addressee errors from validation;
    /// [`AprsError::MessageIdLengthInvalid`] on an empty or overlong
    /// message id.
    pub fn parse(info: &'a [u8]) -> Result<Self, AprsError> {
        let dti = *info.first().ok_or(AprsError::Truncated {
            expected: Self::HEADER_LEN,
            got: info.len(),
        })?;
        if dti != b':' {
            return Err(AprsError::InvalidDataType { got: dti });
        }
        let field: [u8; ADDRESSEE_MAX] = info
            .get(1..1 + ADDRESSEE_MAX)
            .and_then(|s| s.try_into().ok())
            .ok_or(AprsError::Truncated {
                expected: Self::HEADER_LEN,
                got: info.len(),
            })?;
        let addressee = Addressee::decode(field)?;
        let sep = *info.get(1 + ADDRESSEE_MAX).ok_or(AprsError::Truncated {
            expected: Self::HEADER_LEN,
            got: info.len(),
        })?;
        if sep != b':' {
            return Err(AprsError::ExpectedByte {
                expected: b':',
                got: sep,
                position: 1 + ADDRESSEE_MAX,
            });
        }
        let body = info.get(Self::HEADER_LEN..).unwrap_or(&[]);
        let content = Self::parse_body(body)?;
        Ok(Self { addressee, content })
    }

    fn parse_body(body: &'a [u8]) -> Result<MessageContent<'a>, AprsError> {
        // Some transmitters append a stray CR (and occasionally LF) after
        // the message id. The spec forbids it — "Do not put any carriage
        // return or line feed at the end" — and IGates strip it, but it
        // reaches us on the air, where it pushed a legal 5-character
        // reply-ACK id (`MM}AA`) over the length limit and cost the whole
        // packet. Trim it here rather than reject.
        let body = match body {
            [rest @ .., b'\r' | b'\n'] => rest,
            all => all,
        };
        let body = match body {
            [rest @ .., b'\r'] => rest,
            all => all,
        };

        // ack/rej replies: "ack"/"rej" plus an id, no '{' allowed.
        //
        // The id has to be a *valid* one for this to be a reply at all.
        // A body that just starts with "ack" and runs on for another
        // sixty bytes is message text whose first three characters
        // happen to spell it, and chapter 14 gives the reply form no
        // room for that: the identifier is "up to 5 alphanumeric
        // characters". MEASURED over a 64 918-packet capture: 20
        // messages, all `:MYANET   :ack1/2} I have a LoRa 433Mhz Igate
        // running (KE4PIC-11) but ...`, were rejected outright for it.
        if let Some(id) = body.strip_prefix(b"ack")
            && !id.is_empty()
            && !id.contains(&b'{')
            && check_id(id).is_ok()
        {
            return Ok(MessageContent::Ack { id });
        }
        if let Some(id) = body.strip_prefix(b"rej")
            && !id.is_empty()
            && !id.contains(&b'{')
            && check_id(id).is_ok()
        {
            return Ok(MessageContent::Reject { id });
        }
        // Ordinary text, id after the last '{'.
        //
        // Same rule, same reason. Chapter 14 says the identifier is
        // "appended to the message text" and is "up to 5 alphanumeric
        // characters", so a trailing '{' run that is not a valid
        // identifier is not one, and the '{' belongs to the text.
        // MEASURED: 183 messages from 24 senders open their text with
        // '{' (an EchoLink-family status line, `:{EM|v1|ONLINE|...`)
        // and were rejected for a "message id" of 34 to 66 bytes.
        //
        // Neither relaxation can make the parser credulous. A message
        // with no valid identifier is one chapter 14 says must **not**
        // be acknowledged, and `id: None` is exactly that: the reading
        // is more conservative than the rejection it replaces, not
        // less.
        match body.iter().rposition(|&b| b == b'{') {
            Some(brace) if check_id(body.get(brace + 1..).unwrap_or(&[])).is_ok() => {
                Ok(MessageContent::Text {
                    text: body.get(..brace).unwrap_or(&[]),
                    id: body.get(brace + 1..),
                })
            }
            _ => Ok(MessageContent::Text {
                text: body,
                id: None,
            }),
        }
    }

    /// The serialized length of this message in bytes.
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        Self::HEADER_LEN
            + match self.content {
                MessageContent::Text { text, id } => {
                    text.len()
                        + match id {
                            Some(id) => 1 + id.len(),
                            None => 0,
                        }
                }
                MessageContent::Ack { id } | MessageContent::Reject { id } => 3 + id.len(),
            }
    }

    /// Serializes the message into `buf`, returning the written length.
    ///
    /// # Errors
    ///
    /// [`AprsError::BufferTooSmall`] when `buf` cannot hold the message;
    /// [`AprsError::MessageIdLengthInvalid`] on an invalid id length.
    pub fn build(&self, buf: &mut [u8]) -> Result<usize, AprsError> {
        let (prefix, text, id): (&[u8], &[u8], Option<&[u8]>) = match self.content {
            MessageContent::Text { text, id } => (b"", text, id),
            MessageContent::Ack { id } => (b"ack", b"", Some(id)),
            MessageContent::Reject { id } => (b"rej", b"", Some(id)),
        };
        if let Some(id) = id {
            check_id(id)?;
        }
        let needed = self.encoded_len();
        let max = buf.len();
        let out = buf
            .get_mut(..needed)
            .ok_or(AprsError::BufferTooSmall { needed, max })?;
        let explicit_brace = matches!(self.content, MessageContent::Text { id: Some(_), .. });
        let brace: &[u8] = if explicit_brace { b"{" } else { b"" };
        let padded = self.addressee.padded();
        let bytes = core::iter::once(&b':')
            .chain(padded.iter())
            .chain(core::iter::once(&b':'))
            .chain(prefix.iter())
            .chain(text.iter())
            .chain(brace.iter())
            .chain(id.unwrap_or(&[]).iter());
        for (slot, byte) in out.iter_mut().zip(bytes) {
            *slot = *byte;
        }
        Ok(needed)
    }
}

/// Validates a message id: 1..=5 bytes.
///
/// Note that the 1.1 reply-ACK form `{MM}AA` occupies the same five
/// bytes as a plain id, so no extra allowance is needed for it — the
/// `}` is simply an ordinary id byte to this check.
/// [`MessageContent::reply_ack`] interprets it after the fact.
///
/// This is the only gate on what reaches the accessors from the wire:
/// via [`Message::parse`] an id is 1..=5 bytes of *anything* except
/// `{` (the text id is taken after the last `{`, and an `ack`/`rej`
/// operand containing one is re-read as text). Values constructed by
/// hand are unconstrained, including empty, so the accessors are total
/// over `&[u8]`.
const fn check_id(id: &[u8]) -> Result<(), AprsError> {
    if id.is_empty() || id.len() > MESSAGE_ID_MAX {
        Err(AprsError::MessageIdLengthInvalid { len: id.len() })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addressee(name: &[u8]) -> Addressee {
        match Addressee::new(name) {
            Ok(a) => a,
            Err(e) => panic!("{e}"),
        }
    }

    #[test]
    fn message_with_id_round_trip() {
        let msg = Message {
            addressee: addressee(b"N0CALL"),
            content: MessageContent::Text {
                text: b"Testing",
                id: Some(b"003"),
            },
        };
        let mut buf = [0u8; 64];
        let len = match msg.build(&mut buf) {
            Ok(n) => n,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(&buf[..len], b":N0CALL   :Testing{003");
        assert_eq!(Message::parse(&buf[..len]), Ok(msg));
    }

    #[test]
    fn message_without_id_round_trip() {
        let msg = Message {
            addressee: addressee(b"EMAIL-2"),
            content: MessageContent::Text {
                text: b"hi there",
                id: None,
            },
        };
        let mut buf = [0u8; 64];
        let len = match msg.build(&mut buf) {
            Ok(n) => n,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(&buf[..len], b":EMAIL-2  :hi there");
        assert_eq!(Message::parse(&buf[..len]), Ok(msg));
    }

    #[test]
    fn ack_and_rej_round_trip() {
        let ack = Message {
            addressee: addressee(b"N1CALL-14"),
            content: MessageContent::Ack { id: b"003" },
        };
        let mut buf = [0u8; 32];
        let len = match ack.build(&mut buf) {
            Ok(n) => n,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(&buf[..len], b":N1CALL-14:ack003");
        assert_eq!(Message::parse(&buf[..len]), Ok(ack));

        let rej = Message {
            addressee: addressee(b"N0CALL"),
            content: MessageContent::Reject { id: b"9" },
        };
        let len = match rej.build(&mut buf) {
            Ok(n) => n,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(&buf[..len], b":N0CALL   :rej9");
        assert_eq!(Message::parse(&buf[..len]), Ok(rej));
    }

    #[test]
    fn ack_like_text_with_brace_is_text() {
        // "ack" followed by a '{' id is an ordinary message.
        let parsed = match Message::parse(b":N0CALL   :acknowledge{7") {
            Ok(m) => m,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(
            parsed.content,
            MessageContent::Text {
                text: b"acknowledge",
                id: Some(b"7"),
            }
        );
    }

    /// Round trips `content` through [`Message::build`] and
    /// [`Message::parse`], asserting the exact wire bytes and that the
    /// parse gives the same value back.
    fn round_trip(content: MessageContent<'static>, wire: &[u8]) -> MessageContent<'static> {
        let msg = Message {
            addressee: addressee(b"WA6LDQ"),
            content,
        };
        let mut buf = [0u8; 64];
        let len = match msg.build(&mut buf) {
            Ok(n) => n,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(&buf[..len], wire, "build is not byte-exact");
        assert_eq!(Message::parse(wire), Ok(msg), "parse is not the inverse");
        content
    }

    #[test]
    fn reply_ack_splits_the_spec_forms() {
        // Chapter 14: "{MM}AA" where MM is the outgoing message number
        // and AA is the free ACK.
        let with_ack = round_trip(
            MessageContent::Text {
                text: b"Okay",
                id: Some(b"Re}1j"),
            },
            b":WA6LDQ   :Okay{Re}1j",
        );
        assert_eq!(with_ack.reply_ack(), Some((&b"Re"[..], &b"1j"[..])));
        assert_eq!(with_ack.acked_number(), None);

        // "If no ACK is pending, then the message # is \"{MM}\"" — the
        // trailing '}' still announces reply-ACK capability.
        let bare = round_trip(
            MessageContent::Text {
                text: b"Okay",
                id: Some(b"Re}"),
            },
            b":WA6LDQ   :Okay{Re}",
        );
        assert_eq!(bare.reply_ack(), Some((&b"Re"[..], &b""[..])));

        // A plain 1.01 id is not a reply-ACK.
        let plain = round_trip(
            MessageContent::Text {
                text: b"Okay",
                id: Some(b"003"),
            },
            b":WA6LDQ   :Okay{003",
        );
        assert_eq!(plain.reply_ack(), None);

        // No id at all: nothing to split.
        let none = round_trip(
            MessageContent::Text {
                text: b"Okay",
                id: None,
            },
            b":WA6LDQ   :Okay",
        );
        assert_eq!(none.reply_ack(), None);
        assert_eq!(none.acked_number(), None);
    }

    #[test]
    fn acked_number_pulls_out_the_mm() {
        // "...if you get the old \"ackMM}AA\" ack, then you must pull out
        // the \"MM\" here and use IT to match with the outstanding
        // \"{MM}\" in your outgoing message queue."
        let ack = round_trip(MessageContent::Ack { id: b"Re}1j" }, b":WA6LDQ   :ackRe}1j");
        assert_eq!(ack.acked_number(), Some(&b"Re"[..]));
        // The same id still splits, since it is a byte-for-byte copy.
        assert_eq!(ack.reply_ack(), Some((&b"Re"[..], &b"1j"[..])));

        let old = round_trip(MessageContent::Ack { id: b"003" }, b":WA6LDQ   :ack003");
        assert_eq!(old.acked_number(), Some(&b"003"[..]));
        assert_eq!(old.reply_ack(), None);

        // Rejections take the identical treatment.
        let rej = round_trip(
            MessageContent::Reject { id: b"Re}1j" },
            b":WA6LDQ   :rejRe}1j",
        );
        assert_eq!(rej.acked_number(), Some(&b"Re"[..]));
        let rej_old = round_trip(MessageContent::Reject { id: b"9" }, b":WA6LDQ   :rej9");
        assert_eq!(rej_old.acked_number(), Some(&b"9"[..]));
        assert_eq!(rej_old.reply_ack(), None);
    }

    #[test]
    fn reply_ack_degenerate_ids_do_not_panic() {
        // An id of exactly '}': both halves empty.
        let solo = round_trip(
            MessageContent::Text {
                text: b"x",
                id: Some(b"}"),
            },
            b":WA6LDQ   :x{}",
        );
        assert_eq!(solo.reply_ack(), Some((&b""[..], &b""[..])));
        let solo_ack = round_trip(MessageContent::Ack { id: b"}" }, b":WA6LDQ   :ack}");
        // Empty: matches no outstanding number, which is the right answer.
        assert_eq!(solo_ack.acked_number(), Some(&b""[..]));

        // '}' first: MM empty, AA is the rest.
        let first = round_trip(
            MessageContent::Text {
                text: b"x",
                id: Some(b"}1j"),
            },
            b":WA6LDQ   :x{}1j",
        );
        assert_eq!(first.reply_ack(), Some((&b""[..], &b"1j"[..])));

        // '}' last is the capability marker, already covered above; the
        // maximum-length form is the interesting boundary.
        let full = round_trip(
            MessageContent::Text {
                text: b"x",
                id: Some(b"abcd}"),
            },
            b":WA6LDQ   :x{abcd}",
        );
        assert_eq!(full.reply_ack(), Some((&b"abcd"[..], &b""[..])));

        // Two '}': the split is at the first, the second stays in AA.
        let two = round_trip(
            MessageContent::Text {
                text: b"x",
                id: Some(b"a}b}c"),
            },
            b":WA6LDQ   :x{a}b}c",
        );
        assert_eq!(two.reply_ack(), Some((&b"a"[..], &b"b}c"[..])));
        let two_ack = round_trip(MessageContent::Ack { id: b"a}b}c" }, b":WA6LDQ   :acka}b}c");
        assert_eq!(two_ack.acked_number(), Some(&b"a"[..]));

        // Non-ASCII bytes: the scan is over bytes, not characters.
        let utf8 = round_trip(
            MessageContent::Text {
                text: b"x",
                id: Some("\u{e9}}\u{e9}".as_bytes()),
            },
            b":WA6LDQ   :x{\xc3\xa9}\xc3\xa9",
        );
        assert_eq!(utf8.reply_ack(), Some((&b"\xc3\xa9"[..], &b"\xc3\xa9"[..])));
        let raw = round_trip(
            MessageContent::Text {
                text: b"x",
                id: Some(b"\xff\xfe"),
            },
            b":WA6LDQ   :x{\xff\xfe",
        );
        assert_eq!(raw.reply_ack(), None);
    }

    #[test]
    fn accessors_are_total_on_hand_built_content() {
        // An empty id cannot come off the wire — `check_id` rejects it —
        // but the enum is public, so the accessors must still answer.
        let empty_text = MessageContent::Text {
            text: b"x",
            id: Some(b""),
        };
        assert_eq!(empty_text.reply_ack(), None);
        assert_eq!(empty_text.acked_number(), None);
        let empty_ack = MessageContent::Ack { id: b"" };
        assert_eq!(empty_ack.reply_ack(), None);
        assert_eq!(empty_ack.acked_number(), Some(&b""[..]));

        // And it is still rejected on build, so it cannot be emitted.
        let msg = Message {
            addressee: addressee(b"WA6LDQ"),
            content: empty_ack,
        };
        let mut buf = [0u8; 64];
        assert_eq!(
            msg.build(&mut buf),
            Err(AprsError::MessageIdLengthInvalid { len: 0 })
        );

        // Over-length ids are likewise unreachable from the wire, and
        // the accessors do not care.
        let long = MessageContent::Ack { id: b"aa}bbbb" };
        assert_eq!(long.acked_number(), Some(&b"aa"[..]));
    }

    #[test]
    fn addressee_validation() {
        assert_eq!(Addressee::new(b""), Err(AprsError::AddresseeEmpty));
        assert_eq!(
            Addressee::new(b"ABCDEFGHIJ"),
            Err(AprsError::AddresseeTooLong { len: 10 })
        );
        assert_eq!(
            Addressee::new(b"A B"),
            Err(AprsError::InvalidAddresseeChar { got: b' ' })
        );
        assert_eq!(
            Addressee::new(b"A:B"),
            Err(AprsError::InvalidAddresseeChar { got: b':' })
        );
        let a = addressee(b"N0CALL-11");
        assert_eq!(a.as_bytes(), b"N0CALL-11");
        assert_eq!(&a.padded(), b"N0CALL-11");
    }

    /// A `{` or an `ack` prefix that cannot be an identifier is text.
    ///
    /// Chapter 14 puts the identifier at the end of the message text
    /// and caps it at five characters, so anything longer is not one,
    /// and the bytes belong to the text. Rejecting the packet instead
    /// discards a message that is well formed.
    ///
    /// The two arms fail differently and both were wrong, which is why
    /// this covers both: `ack1/2} ...` never reached the brace logic at
    /// all, because `strip_prefix(b"ack")` matched first.
    ///
    /// Built into a fixed buffer rather than with `concat`, because
    /// this module's unit tests compile under feature sets without
    /// `alloc` (`scripts/check-embedded.sh` pass 2).
    #[test]
    fn a_brace_or_ack_that_is_not_an_id_stays_in_the_text() {
        /// One case: the body after the addressee, the text it must
        /// parse to, and the identifier it must carry.
        type Case = (&'static [u8], &'static [u8], Option<&'static [u8]>);
        let cases: [Case; 9] = [
            // Real traffic: an EchoLink-family status line.
            (
                b"{EM|v1|ONLINE|0.0000|0.0000|Android",
                b"{EM|v1|ONLINE|0.0000|0.0000|Android",
                None,
            ),
            // Real traffic: text that opens with "ack".
            (b"ack1/2} I have a LoRa", b"ack1/2} I have a LoRa", None),
            // A brace in the middle, nothing that could be an id.
            (b"cost is {50 or so", b"cost is {50 or so", None),
            // Trailing brace, empty id.
            (b"trailing brace{", b"trailing brace{", None),
            // The length boundary, both sides.
            (b"hello{12345", b"hello", Some(b"12345")),
            (b"hello{123456", b"hello{123456", None),
            // A valid id still wins, and still comes from the LAST brace.
            (b"a{b}c{42", b"a{b}c", Some(b"42")),
            // "ack" with no id at all is text, not an empty-id error.
            (b"ack", b"ack", None),
            // A bare brace is text.
            (b"{", b"{", None),
        ];
        let mut buf = [0u8; 96];
        buf[..11].copy_from_slice(b":N0CALL   :");
        for (body, want_text, want_id) in cases {
            let end = 11 + body.len();
            buf[11..end].copy_from_slice(body);
            let parsed = match Message::parse(&buf[..end]) {
                Ok(m) => m,
                Err(e) => panic!("{e} rejected a well-formed message"),
            };
            match parsed.content {
                MessageContent::Text { text, id } => {
                    assert_eq!(text, want_text, "text of {body:?}");
                    assert_eq!(id, want_id, "id of {body:?}");
                }
                other => panic!("expected text for {body:?}, got {other:?}"),
            }
        }
    }

    /// A well-formed reply is still a reply, and still round trips.
    #[test]
    fn a_valid_ack_or_rej_is_unaffected() {
        let mut buf = [0u8; 32];
        buf[..11].copy_from_slice(b":N0CALL   :");
        for (body, want) in [
            (&b"ack01"[..], MessageContent::Ack { id: b"01" }),
            (&b"rej42"[..], MessageContent::Reject { id: b"42" }),
            (&b"ackAB12C"[..], MessageContent::Ack { id: b"AB12C" }),
        ] {
            let end = 11 + body.len();
            buf[11..end].copy_from_slice(body);
            let parsed = Message::parse(&buf[..end]).expect("a valid reply");
            assert_eq!(parsed.content, want, "{body:?}");
            let mut out = [0u8; 32];
            let len = parsed.build(&mut out).expect("rebuild");
            assert_eq!(&out[..len], &buf[..end], "{body:?}");
        }
    }

    #[test]
    fn parse_rejections() {
        assert_eq!(
            Message::parse(b":N0CALL"),
            Err(AprsError::Truncated {
                expected: 11,
                got: 7
            })
        );
        assert_eq!(
            Message::parse(b">status"),
            Err(AprsError::InvalidDataType { got: b'>' })
        );
        assert_eq!(
            Message::parse(b":N0CALL    hello"),
            Err(AprsError::ExpectedByte {
                expected: b':',
                got: b' ',
                position: 10
            })
        );
        assert_eq!(
            Message::parse(b":         :text"),
            Err(AprsError::AddresseeEmpty)
        );
        // A '{' or an "ack" prefix that cannot be an identifier is
        // text, not a rejection: see `a_brace_or_ack_that_is_not_an_id_
        // stays_in_the_text` below for the rule and the traffic.
        // `MessageIdLengthInvalid` survives only where the identifier
        // is the entire payload and there is no text to fall back to.
        assert_eq!(
            Message::parse(b":N0CALL   :ack"),
            Ok(Message {
                addressee: addressee(b"N0CALL"),
                content: MessageContent::Text {
                    text: b"ack",
                    id: None
                },
            })
        );
    }

    #[test]
    fn build_overflow_and_bad_id() {
        let msg = Message {
            addressee: addressee(b"N0CALL"),
            content: MessageContent::Text {
                text: b"Testing",
                id: Some(b"003"),
            },
        };
        let mut small = [0u8; 12];
        assert_eq!(
            msg.build(&mut small),
            Err(AprsError::BufferTooSmall {
                needed: 22,
                max: 12
            })
        );
        let bad = Message {
            addressee: addressee(b"N0CALL"),
            content: MessageContent::Ack { id: b"123456" },
        };
        let mut buf = [0u8; 64];
        assert_eq!(
            bad.build(&mut buf),
            Err(AprsError::MessageIdLengthInvalid { len: 6 })
        );
    }
}
