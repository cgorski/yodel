//! KISS TNC framing: byte-level escaping between host and TNC.
//!
//! The [KISS] protocol delimits frames with the `FEND` byte (`0xC0`). A
//! frame starts with a command byte — the low nibble selects the command
//! ([`KissCommand`]), the high nibble the TNC port ([`KissPort`]) — and is
//! followed by the payload. Occurrences of `FEND` and `FESC` *anywhere in
//! the frame* are escaped as `FESC TFEND` and `FESC TFESC` respectively,
//! so `FEND` on the wire always means a frame boundary.
//!
//! # Transparency covers the command byte
//!
//! The command byte is the first byte *of the frame*, not a delimiter, so
//! it is escaped like any other frame byte. Chepponis, M. (K3MC) and
//! Karn, P. (KA9Q), "The KISS TNC: A simple Host-to-TNC communications
//! protocol", ARRL 6th Computer Networking Conference, Redondo Beach CA,
//! 1987 (<https://www.ka9q.net/papers/kiss.html>), §3 "Transparency":
//! "In particular, the
//! FEND character is never sent over the channel except as an actual
//! end-of-frame indication." §4 places the type indicator first within
//! the frame, so the rule admits no exception for it.
//!
//! This matters for exactly one of the 112 constructible (port, command)
//! pairs: port 12 with [`KissCommand::Data`] is `(12 << 4) | 0 = 0xC0`,
//! which is `FEND`, and goes on the wire as `FESC TFEND`. No pair can
//! produce `FESC` (`0xDB`), because that needs command nibble 11 and
//! [`KissCommand`] has no such variant.
//!
//! Transmit side: [`encode_into`] (writes one escaped frame into a caller
//! buffer) or [`frame_iter`] / [`KissFrameIter`], a lazy allocation-free
//! iterator of encoded bytes. Receive side: [`KissDeframer`], a
//! push-one-byte state machine with a fixed const-generic buffer.
//!
//! # Round trip
//!
//! ```
//! use yodel::kiss::{KissCommand, KissDeframer, KissPort, encode_into, encoded_len};
//!
//! # fn main() -> Result<(), yodel::kiss::KissError> {
//! let payload = [0x01, 0xC0, 0xDB, 0x02];
//! let port = KissPort::new(3)?;
//! let mut buf = [0u8; 32];
//! let len = encode_into(port, KissCommand::Data, &payload, &mut buf)?;
//! assert_eq!(len, encoded_len(port, KissCommand::Data, &payload));
//!
//! let mut deframer = KissDeframer::<32>::new();
//! for &byte in &buf[..len] {
//!     if let Some(result) = deframer.push(byte) {
//!         let frame = result?;
//!         assert_eq!(frame.command(), KissCommand::Data);
//!         assert_eq!(frame.port(), port);
//!         assert_eq!(frame.payload(), payload);
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! [KISS]: https://en.wikipedia.org/wiki/KISS_(amateur_radio_protocol)

use core::fmt;

/// Frame delimiter: starts and ends every KISS frame.
pub const FEND: u8 = 0xC0;
/// Escape introducer inside a frame.
pub const FESC: u8 = 0xDB;
/// Escaped stand-in for [`FEND`] (sent as `FESC TFEND`).
pub const TFEND: u8 = 0xDC;
/// Escaped stand-in for [`FESC`] (sent as `FESC TFESC`).
pub const TFESC: u8 = 0xDD;

/// A KISS protocol violation: an invalid field value on encode, or a
/// malformed byte stream on decode.
///
/// Every variant carries the offending value together with the rule it
/// violated, so the rendered message is self-explanatory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KissError {
    /// A TNC port number was outside `0..=15`.
    PortOutOfRange {
        /// The rejected port number.
        got: u8,
    },
    /// A command byte carried an unknown command nibble.
    UnknownCommand {
        /// The rejected command nibble (low four bits of the command byte).
        got: u8,
    },
    /// The encoded frame did not fit in the output buffer.
    BufferTooSmall {
        /// The required length in bytes.
        needed: usize,
        /// The length of the buffer offered, in bytes.
        got: usize,
    },
    /// A `FESC` was followed by a byte other than `TFEND` or `TFESC`.
    InvalidEscape {
        /// The rejected byte that followed `FESC`.
        got: u8,
    },
    /// A received frame outgrew the deframer's fixed buffer.
    FrameTooLarge {
        /// The deframer's capacity in bytes (command byte + payload).
        capacity: usize,
    },
}

impl fmt::Display for KissError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            KissError::PortOutOfRange { got } => {
                write!(f, "TNC port {got} is out of range: must be within 0..=15")
            }
            KissError::UnknownCommand { got } => write!(
                f,
                "command nibble 0x{got:X} is unknown: must be 0..=6, or the whole byte 0xFF"
            ),
            KissError::BufferTooSmall { needed, got } => write!(
                f,
                "output buffer of {got} bytes is too small: the encoded frame needs {needed} bytes"
            ),
            KissError::InvalidEscape { got } => write!(
                f,
                "escape FESC followed by 0x{got:02X} is invalid: only TFEND (0xDC) or TFESC (0xDD) may follow"
            ),
            KissError::FrameTooLarge { capacity } => write!(
                f,
                "received frame is too large: the buffer holds at most {capacity} bytes"
            ),
        }
    }
}

impl core::error::Error for KissError {}

/// A TNC port number, `0..=15` (the high nibble of the command byte).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct KissPort(u8);

impl KissPort {
    /// Creates a port, rejecting values outside `0..=15`.
    pub const fn new(port: u8) -> Result<Self, KissError> {
        if port <= 15 {
            Ok(Self(port))
        } else {
            Err(KissError::PortOutOfRange { got: port })
        }
    }

    /// The port number, `0..=15`.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// A KISS command (the low nibble of the command byte).
///
/// [`KissCommand::Return`] is special: it has no port nibble — the whole
/// command byte is `0xFF`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KissCommand {
    /// A data frame to transmit / a received data frame (`0`).
    Data,
    /// Set the transmitter keyup delay, in 10 ms units (`1`).
    TxDelay,
    /// Set the persistence parameter `p` (`2`).
    Persistence,
    /// Set the slot interval, in 10 ms units (`3`).
    SlotTime,
    /// Set the transmitter hold time after the frame, in 10 ms units (`4`).
    TxTail,
    /// Set full-duplex (nonzero) or half-duplex (zero) operation (`5`).
    FullDuplex,
    /// Set hardware-specific parameters (`6`).
    SetHardware,
    /// Exit KISS mode; the whole command byte is `0xFF` (no port nibble).
    Return,
}

impl KissCommand {
    /// Builds the raw command byte `(port << 4) | cmd`.
    ///
    /// For [`KissCommand::Return`] the byte is `0xFF` regardless of `port`.
    #[must_use]
    pub const fn to_byte(self, port: KissPort) -> u8 {
        match self {
            KissCommand::Data => port.0 << 4,
            KissCommand::TxDelay => (port.0 << 4) | 1,
            KissCommand::Persistence => (port.0 << 4) | 2,
            KissCommand::SlotTime => (port.0 << 4) | 3,
            KissCommand::TxTail => (port.0 << 4) | 4,
            KissCommand::FullDuplex => (port.0 << 4) | 5,
            KissCommand::SetHardware => (port.0 << 4) | 6,
            KissCommand::Return => 0xFF,
        }
    }

    /// Parses a raw command byte into `(command, port)`.
    ///
    /// The byte `0xFF` is [`KissCommand::Return`] with port 0. Otherwise
    /// the low nibble must be a known command (`0..=6`); an unknown nibble
    /// is reported as [`KissError::UnknownCommand`].
    pub const fn from_byte(byte: u8) -> Result<(Self, KissPort), KissError> {
        if byte == 0xFF {
            return Ok((KissCommand::Return, KissPort(0)));
        }
        let port = KissPort(byte >> 4);
        let command = match byte & 0x0F {
            0 => KissCommand::Data,
            1 => KissCommand::TxDelay,
            2 => KissCommand::Persistence,
            3 => KissCommand::SlotTime,
            4 => KissCommand::TxTail,
            5 => KissCommand::FullDuplex,
            6 => KissCommand::SetHardware,
            nibble => return Err(KissError::UnknownCommand { got: nibble }),
        };
        Ok((command, port))
    }
}

/// The exact encoded length of the frame [`encode_into`] would write.
///
/// Counts the opening and closing [`FEND`], the command byte, and the
/// payload, with each [`FEND`]/[`FESC`] expanded to two bytes — the
/// command byte included, since it is part of the frame and is escaped
/// like any other byte (see the [module docs](self)). The command and
/// port are therefore required: port 12 with [`KissCommand::Data`]
/// encodes to two bytes, not one.
#[must_use]
pub fn encoded_len(port: KissPort, command: KissCommand, payload: &[u8]) -> usize {
    let cmd = command.to_byte(port);
    let escapes = payload
        .iter()
        .chain(core::iter::once(&cmd))
        .filter(|&&b| b == FEND || b == FESC)
        .count();
    3 + payload.len() + escapes
}

/// Encodes one KISS frame into a fresh vector.
///
/// The ergonomic counterpart to [`encode_into`], for hosts with a heap.
/// KISS is the protocol between a TNC and a host computer, so that is
/// most callers. The length comes from [`encoded_len`], so this cannot
/// fail.
///
/// ```
/// # #[cfg(all(feature = "kiss", feature = "alloc"))] {
/// use yodel::kiss::{KissCommand, KissPort, encode_to_vec};
///
/// // 0xC0 is the frame delimiter, so a payload containing one is escaped.
/// let wire = encode_to_vec(KissPort::new(0)?, KissCommand::Data, &[0x82, 0xC0]);
/// assert_eq!(wire, [0xC0, 0x00, 0x82, 0xDB, 0xDC, 0xC0]);
/// # }
/// # Ok::<(), yodel::kiss::KissError>(())
/// ```
#[cfg(feature = "alloc")]
#[must_use]
pub fn encode_to_vec(port: KissPort, command: KissCommand, payload: &[u8]) -> alloc::vec::Vec<u8> {
    let mut out = alloc::vec![0u8; encoded_len(port, command, payload)];
    let n = encode_into(port, command, payload, &mut out)
        .expect("a buffer of encoded_len() always fits");
    out.truncate(n);
    out
}

/// Encodes one KISS frame into `out`, returning the number of bytes written.
///
/// The frame is `FEND`, then the command byte (see
/// [`KissCommand::to_byte`]) and the payload with every `FEND`/`FESC`
/// escaped, then a closing `FEND`. Fails with
/// [`KissError::BufferTooSmall`] if `out` is shorter than
/// [`encoded_len`].
///
/// The command byte goes through the same escaping path as the payload,
/// because it is a frame byte rather than a delimiter (see the [module
/// docs](self)). Port 12 with [`KissCommand::Data`] is the only pair
/// where that is observable:
///
/// ```
/// use yodel::kiss::{FEND, FESC, KissCommand, KissPort, TFEND, encode_into};
///
/// # fn main() -> Result<(), yodel::kiss::KissError> {
/// let port = KissPort::new(12)?;
/// let mut buf = [0u8; 8];
/// let len = encode_into(port, KissCommand::Data, &[0x11], &mut buf)?;
/// // The 0xC0 type indicator is sent as FESC TFEND, never as a bare FEND.
/// assert_eq!(&buf[..len], &[FEND, FESC, TFEND, 0x11, FEND]);
/// # Ok(())
/// # }
/// ```
pub fn encode_into(
    port: KissPort,
    command: KissCommand,
    payload: &[u8],
    out: &mut [u8],
) -> Result<usize, KissError> {
    let needed = encoded_len(port, command, payload);
    if out.len() < needed {
        return Err(KissError::BufferTooSmall {
            needed,
            got: out.len(),
        });
    }
    let cmd = command.to_byte(port);
    let mut pos = 0;
    let mut put = |slot: &mut [u8], byte: u8| {
        if let Some(cell) = slot.get_mut(pos) {
            *cell = byte;
        }
        pos += 1;
    };
    put(out, FEND);
    // One escaping rule for the whole frame: the command byte is just
    // its first byte.
    for &byte in core::iter::once(&cmd).chain(payload.iter()) {
        match byte {
            FEND => {
                put(out, FESC);
                put(out, TFEND);
            }
            FESC => {
                put(out, FESC);
                put(out, TFESC);
            }
            other => put(out, other),
        }
    }
    put(out, FEND);
    Ok(pos)
}

/// Serializes a frame into KISS wire bytes, lazily.
///
/// The returned iterator yields, one at a time and without allocating:
/// `FEND`, the escaped command byte, the escaped payload, and the closing
/// `FEND` — exactly the bytes [`encode_into`] would write, byte for byte.
pub fn frame_iter(port: KissPort, command: KissCommand, payload: &[u8]) -> KissFrameIter<'_> {
    KissFrameIter {
        payload,
        state: EncState::OpenFend,
        cmd_byte: command.to_byte(port),
    }
}

/// Encoder state of [`KissFrameIter`].
#[derive(Debug, Clone, Copy)]
enum EncState {
    /// Emitting the opening `FEND`.
    OpenFend,
    /// Emitting frame content byte `pos` (or, when past the end, the
    /// closing `FEND`). The command byte and the payload share this one
    /// state so they share one escaping rule.
    Content {
        /// Index into the frame content: 0 is the command byte, `1..` the
        /// payload. See [`KissFrameIter::content`].
        pos: usize,
        /// Escape follow-up byte pending from the previous content byte.
        pending: Option<u8>,
    },
    /// All bytes emitted.
    Done,
}

/// Lazy iterator of KISS wire bytes for one frame.
///
/// Created by [`frame_iter`]. Yields the opening `FEND`, the escaped
/// command byte, the escaped payload, and the closing `FEND`.
#[derive(Debug, Clone)]
pub struct KissFrameIter<'a> {
    payload: &'a [u8],
    state: EncState,
    cmd_byte: u8,
}

impl KissFrameIter<'_> {
    /// The unescaped frame content byte at `pos`: index 0 is the command
    /// byte, `1..` index the payload. `None` marks the end of the frame.
    fn content(&self, pos: usize) -> Option<u8> {
        match pos.checked_sub(1) {
            None => Some(self.cmd_byte),
            Some(i) => self.payload.get(i).copied(),
        }
    }
}

impl Iterator for KissFrameIter<'_> {
    type Item = u8;

    fn next(&mut self) -> Option<u8> {
        match self.state {
            EncState::OpenFend => {
                self.state = EncState::Content {
                    pos: 0,
                    pending: None,
                };
                Some(FEND)
            }
            EncState::Content { pos, pending } => {
                if let Some(byte) = pending {
                    self.state = EncState::Content {
                        pos: pos + 1,
                        pending: None,
                    };
                    return Some(byte);
                }
                match self.content(pos) {
                    Some(FEND) => {
                        self.state = EncState::Content {
                            pos,
                            pending: Some(TFEND),
                        };
                        Some(FESC)
                    }
                    Some(FESC) => {
                        self.state = EncState::Content {
                            pos,
                            pending: Some(TFESC),
                        };
                        Some(FESC)
                    }
                    Some(byte) => {
                        self.state = EncState::Content {
                            pos: pos + 1,
                            pending: None,
                        };
                        Some(byte)
                    }
                    None => {
                        self.state = EncState::Done;
                        Some(FEND)
                    }
                }
            }
            EncState::Done => None,
        }
    }
}

/// A complete, unescaped KISS frame borrowed from a [`KissDeframer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KissFrame<'a> {
    command: KissCommand,
    port: KissPort,
    payload: &'a [u8],
}

impl<'a> KissFrame<'a> {
    /// The parsed command from the frame's command byte.
    #[must_use]
    pub const fn command(&self) -> KissCommand {
        self.command
    }

    /// The parsed TNC port from the frame's command byte.
    ///
    /// [`KissCommand::Return`] frames carry no port nibble; port 0 is
    /// reported.
    #[must_use]
    pub const fn port(&self) -> KissPort {
        self.port
    }

    /// The unescaped payload bytes (borrowed until the next push).
    #[must_use]
    pub const fn payload(&self) -> &'a [u8] {
        self.payload
    }
}

/// Streaming KISS deframer: wire bytes in, unescaped frames out.
///
/// Push one received byte at a time with [`KissDeframer::push`]. The
/// deframer hunts for [`FEND`], unescapes `FESC TFEND` / `FESC TFESC`,
/// and accumulates the command byte plus payload into a fixed `[u8; N]`
/// buffer. Back-to-back `FEND`s (empty frames) are skipped silently, and
/// after any error the deframer re-syncs to the next `FEND`.
///
/// `N` is the largest frame (command byte plus unescaped payload) that
/// can be received; frames larger than that are reported as
/// [`KissError::FrameTooLarge`].
#[derive(Debug, Clone)]
pub struct KissDeframer<const N: usize> {
    /// Byte buffer for the frame being accumulated (command byte + payload).
    buf: [u8; N],
    /// Unescaped bytes stored in `buf` (or seen, when overflowed).
    len: usize,
    /// Whether an opening `FEND` has been seen (accumulating content).
    in_frame: bool,
    /// Whether a `FESC` is awaiting its follow-up byte.
    escaping: bool,
    /// Whether the current frame outgrew `buf`.
    overflowed: bool,
}

impl<const N: usize> KissDeframer<N> {
    /// Creates an empty deframer, hunting for an opening `FEND`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buf: [0; N],
            len: 0,
            in_frame: false,
            escaping: false,
            overflowed: false,
        }
    }

    /// Resets the per-frame accumulation state.
    const fn reset_frame(&mut self) {
        self.len = 0;
        self.escaping = false;
        self.overflowed = false;
    }

    /// Pushes one received byte.
    ///
    /// Returns `Some(Ok(frame))` when a closing `FEND` completes a
    /// non-empty frame — the frame borrows the internal buffer until the
    /// next push. Returns `Some(Err(_))` for a frame rejected with a
    /// diagnosable cause ([`KissError::InvalidEscape`],
    /// [`KissError::FrameTooLarge`], [`KissError::UnknownCommand`]); the
    /// deframer then re-syncs to the next `FEND`. Bytes before an opening
    /// `FEND` and empty frames (back-to-back `FEND`s) are discarded
    /// silently (`None`).
    pub fn push(&mut self, byte: u8) -> Option<Result<KissFrame<'_>, KissError>> {
        if byte == FEND {
            let close = self.in_frame && self.len > 0;
            let escaping = self.escaping;
            let overflowed = self.overflowed;
            let len = self.len;
            self.in_frame = true;
            self.reset_frame();
            if !close {
                return None;
            }
            if escaping {
                // A dangling FESC at the frame boundary: the escape was
                // never completed; the offending follow-up byte is FEND.
                return Some(Err(KissError::InvalidEscape { got: FEND }));
            }
            if overflowed {
                return Some(Err(KissError::FrameTooLarge { capacity: N }));
            }
            let bytes = self.buf.get(..len).unwrap_or(&[]);
            let (&cmd_byte, payload) = bytes.split_first()?;
            return Some(
                KissCommand::from_byte(cmd_byte).map(|(command, port)| KissFrame {
                    command,
                    port,
                    payload,
                }),
            );
        }
        if !self.in_frame {
            return None;
        }
        if self.escaping {
            self.escaping = false;
            match byte {
                TFEND => self.store(FEND),
                TFESC => self.store(FESC),
                bad => {
                    // Re-sync: discard until the next FEND.
                    self.in_frame = false;
                    self.reset_frame();
                    return Some(Err(KissError::InvalidEscape { got: bad }));
                }
            }
            return None;
        }
        if byte == FESC {
            self.escaping = true;
            return None;
        }
        self.store(byte);
        None
    }

    /// Stores one unescaped byte, tracking overflow past capacity.
    const fn store(&mut self, byte: u8) {
        if self.len < N {
            self.buf[self.len] = byte;
        } else {
            self.overflowed = true;
        }
        // Saturate so endless garbage cannot overflow the counter.
        self.len = self.len.saturating_add(1);
    }
}

impl<const N: usize> Default for KissDeframer<N> {
    /// Same as [`KissDeframer::new`].
    fn default() -> Self {
        Self::new()
    }
}
