//! FT8 transmit and receive: message packing, channel coding, audio
//! synthesis, and the capture-decoding engine.
//!
//! # What FT8 is
//!
//! FT8 (Franke–Taylor design, 8-FSK) is the weak-signal digital mode
//! described in the QEX paper "The FT4 and FT8 Communication Protocols"
//! (Franke, Somerville, Taylor). Stations exchange short structured
//! messages in strictly timed **15-second cycles**: a 77-bit payload is
//! protected by a 14-bit CRC and an LDPC(174,91) code, mapped onto 58
//! Gray-coded 8-FSK data symbols, framed by three 7×7 Costas sync
//! arrays into **79 channel symbols**, and sent as continuous-phase
//! 8-tone FSK with 6.25 Hz tone spacing and 0.16 s symbols
//! (~12.64 s of audio, leaving guard time inside the 15 s slot).
//!
//! This is an implementation from the **published protocol
//! definition**: the QEX paper named above, plus the authors' own
//! resource package `ft4_ft8_protocols.tgz` (reference \[14\] of that
//! paper), which §9 of the paper places in the **public domain** and
//! explicitly carves out of WSJT-X's GPLv3. The two channel-coding
//! matrices are embedded from that package and machine-checked against
//! it; every other constant carries its own provenance note. See
//! [`GENERATOR_BITS`], [`CHECK_ROWS`], and
//! `third_party/ft4_ft8_public/README.md`.
//!
//! # Protocol licence and conditions
//!
//! The public-domain dedication in §9 is **conditional**, and using the
//! name "FT8" — which this module does — accepts the conditions. Stated
//! condition by condition:
//!
//! * *"may use the names "FT4" and "FT8" only if they adhere to our
//!   protocol definitions for source encoding, error-correction coding,
//!   and modulation format."* — Adhered to for the implemented subset.
//!   Where this module cannot encode a message it **rejects it with a
//!   typed error** rather than emitting something non-conforming (see
//!   "Supported message subset" below).
//! * *"Presently unassigned message types [...] must not be assigned
//!   without our permission."* — Honoured. `i3 = 2..7` and the
//!   unassigned `n3` values are rejected, never repurposed.
//! * *"Multi-streaming with waveforms and message content similar to
//!   those used in FT8 DXpedition Mode [...]"* — Not applicable;
//!   DXpedition Mode is not implemented.
//! * *"Robotic or unattended QSOs must be explicitly disallowed."* —
//!   **So disallowed here.** This crate is a modem: it converts
//!   messages to samples and back, and holds no QSO state, so it cannot
//!   itself complete a QSO. But it is a building block for software that
//!   could, and the condition is on implementations of the protocol, not
//!   only on end-user applications. Therefore: **using this module to
//!   conduct robotic or unattended FT8 QSOs is not a supported use and
//!   is contrary to the protocol licence this module relies on.** An
//!   operator must be present and must initiate and confirm each
//!   exchange. Automated *beaconing or reception* — a decode logger, a
//!   propagation monitor, a WSPR-style unattended receiver — is not a
//!   QSO and is unaffected.
//! * *"Any implementation [...] that allows robotic, unattended, or
//!   non-conforming multi-streaming operation shall not use the names
//!   "FT4" or "FT8" and must be made incompatible by some means [...]"* —
//!   Follows from the above: a fork that removes the restriction above
//!   must also drop the name and make itself incompatible (the paper
//!   suggests changing the Costas arrays, i.e. [`COSTAS`]).
//!
//! # TX and RX
//!
//! This module is both halves of the loop. Transmit: message → 79
//! symbols → samples. Receive splits along the crate's usual memory
//! boundary (mirroring [`crate::wspr`]):
//!
//! * **no_std decode math (this file)**: per-bit LLRs from 8-tone
//!   soft symbol energies ([`llrs_from_energies`], Gray demap,
//!   max-log), the LDPC(174,91) min-sum belief-propagation decoder
//!   ([`ldpc_decode`], hard-capped at [`LDPC_MAX_ITERS`] iterations,
//!   ≈ 3.0 KB of working RAM inside the decoder plus the caller's
//!   ≈ 0.7 KB LLR array), CRC-14 verification ([`verify_crc`]) and
//!   message unpacking ([`unpack_message`], dispatching to the
//!   standard-message and [`unpack_free_text`] paths). All fixed-size
//!   arrays, no allocation.
//! * **std-gated capture engine** ([`Ft8Decoder`], `ft8` + `std`):
//!   the buffer-owning pipeline over a ~15 s 12 kHz capture — mix +
//!   decimate, FFT candidate search, Costas sync, coherent 8-tone
//!   demod. See `rx.rs` for its documented RAM budget.
//!
//! # Supported message subset
//!
//! FT8 defines a family of payload types selected by the `i3` (and,
//! for `i3 = 0`, the `n3`) fields. This module implements exactly two
//! of them, with validated constructors:
//!
//! * **`i3 = 1` — standard message** ([`Ft8Message::standard`]): two
//!   28-bit callsign fields (each accepting the special tokens `CQ`,
//!   `QRZ`, `DE` in the first position, or a *standard* callsign),
//!   the acknowledgement flag `R1`, and the 15-bit trailer `g15`
//!   carrying a 4-character grid, a signal report, `RRR`, `RR73`,
//!   `73`, or nothing. The per-callsign rover flags (`r1`) are always
//!   transmitted as 0: `/R` (and every other compound/suffixed
//!   callsign) is rejected, not silently mangled.
//! * **`i3 = 0, n3 = 0` — free text** ([`Ft8Message::free_text`]): up
//!   to 13 characters from the published 42-character alphabet,
//!   packed as a base-42 integer into 71 bits.
//!
//! Everything else — directed CQ (`CQ POTA …`), hashed/nonstandard
//! callsigns, EU VHF contest (`i3 = 2`), RTTY roundup (`i3 = 3`),
//! DXpedition, telemetry, WWROF, … — is **rejected with a specific
//! [`Ft8Error`]**. Nothing is ever silently mis-encoded.
//!
//! # The TX pipeline
//!
//! 1. **Payload packing** ([`Ft8Message`]): 77 bits, stored MSB-first
//!    and left-justified in 10 bytes ([`Ft8Message::payload`]).
//! 2. **CRC-14** ([`crc14`], [`add_crc`]): computed over the 77
//!    payload bits **zero-extended to 82 bits** (77 + 5 zeros), MSB
//!    first, polynomial [`CRC_POLY`] (`0x2757`), zero initial value —
//!    the published procedure. The 14 CRC bits are appended for a
//!    91-bit protected message.
//! 3. **LDPC(174,91) encode** ([`ldpc_encode`]): 83 parity bits from
//!    the published 83×91 generator matrix ([`GENERATOR_BITS`]);
//!    codeword = 91 message bits then 83 parity bits.
//!    [`ldpc_check`] verifies a codeword against the systematic-form
//!    parity-check matrix `H = [G | I₈₃]` derived from the generator.
//! 4. **Symbol mapping** ([`symbols_from_codeword`]): the 174 coded
//!    bits, three at a time MSB-first, index the Gray map
//!    ([`GRAY_MAP`]) to produce 58 tone numbers; the 7-symbol Costas
//!    array [`COSTAS`] is placed at symbol positions 0–6, 36–42 and
//!    72–78, the data symbols filling 7–35 and 43–71 → 79 symbols.
//! 5. **Audio synthesis** ([`Ft8Modulator`]): 8-tone continuous-phase
//!    FSK, tone spacing 6.25 Hz, symbol period `rate / 6.25` samples
//!    (1920 at the canonical 12 kHz, 0.16 s), caller-chosen base
//!    frequency, through the same never-reset `u32` phase-accumulator
//!    scheme as the crate's other modulators.
//!
//! # GFSK pulse shaping (documented approximation status)
//!
//! The published waveform smooths the instantaneous frequency across
//! symbol boundaries with a Gaussian frequency pulse of **BT = 2.0**
//! ([`GFSK_BT`]): the frequency contribution of each symbol is
//! `pulse(t) = ½·(erf(K·BT·(t+½)) − erf(K·BT·(t−½)))` with
//! `K = π·√(2/ln 2)` and `t` in symbol periods, a pulse spanning three
//! symbols ([`gfsk_pulse`]). This module implements that pulse
//! directly (with its own `no_std` erf/exp evaluation, accurate to
//! ~1e-7 — far below audio quantization), extending the first and last
//! symbols virtually so the frequency ramps in from and out to the
//! nominal edge tones. The phase itself is a single accumulator and is
//! exactly continuous by construction. If any residual difference from
//! the reference waveform exists it is confined to the erf
//! approximation tolerance; decode compatibility (our own RX slice is
//! the consumer) is the design target.
//!
//! # Example
//!
//! ```
//! use warble::SampleRate;
//! use warble::ft8::{Ft8Config, Ft8Message, Ft8Modulator, Ft8Tail};
//!
//! let msg = Ft8Message::standard("CQ", "K1ABC", false, Ft8Tail::grid("FN42")?)?;
//! let symbols = msg.channel_symbols();
//! assert_eq!(symbols.len(), 79);
//! assert!(symbols.iter().all(|&s| s <= 7));
//!
//! let config = Ft8Config::new(1_500, SampleRate::new(12_000)?)?;
//! let mut tx = Ft8Modulator::new(config, symbols);
//! assert_eq!(tx.total_samples(), 79 * 1_920);
//! let mut buf = [0i16; 256];
//! assert_eq!(tx.fill_i16(&mut buf), 256);
//! # Ok::<(), Box<dyn core::error::Error>>(())
//! ```

use core::fmt;

use crate::geo::{GeoError, MaidenheadGrid};
use crate::types::{SampleRate, sine_at};

#[cfg(feature = "std")]
mod rx;
#[cfg(feature = "std")]
pub use rx::{Ft8Decode, Ft8Decoder, Ft8DecoderConfig, Ft8RxError};

/// Number of channel symbols in an FT8 transmission (3×7 Costas + 58 data).
pub const SYMBOL_COUNT: usize = 79;

/// Number of source-encoded payload bits.
pub const PAYLOAD_BITS: usize = 77;

/// Number of bytes holding the 77-bit payload (left-justified).
pub const PAYLOAD_LEN: usize = 10;

/// Number of CRC-protected message bits (77 payload + 14 CRC).
pub const MESSAGE_BITS: usize = 91;

/// Number of bytes holding the 91-bit CRC-protected message.
pub const MESSAGE_LEN: usize = 12;

/// Number of LDPC codeword bits (91 message + 83 parity).
pub const CODEWORD_BITS: usize = 174;

/// Number of bytes holding the 174-bit codeword (left-justified).
pub const CODEWORD_LEN: usize = 22;

/// Number of LDPC parity checks / parity bits.
pub const PARITY_BITS: usize = 83;

/// The CRC-14 polynomial, `x¹⁴` term implicit.
///
/// Provenance: the published FT8 protocol CRC polynomial (QEX paper /
/// open protocol documentation), `0x2757`. The CRC is computed over
/// the 77 payload bits zero-extended to 82 bits (77 + 5 zeros), MSB
/// first, with a zero initial register — see [`crc14`].
pub const CRC_POLY: u16 = 0x2757;

/// The 7×7 Costas array used for synchronization, as a tone sequence.
///
/// Provenance: the published FT8 sync sequence (QEX paper). Placed at
/// channel-symbol positions 0–6, 36–42 and 72–78.
pub const COSTAS: [u8; 7] = [3, 1, 4, 0, 6, 5, 2];

/// Gray map from a 3-bit group (MSB-first) to the transmitted tone.
///
/// Provenance: the published FT8 bits→tone Gray code. Adjacent tones
/// differ in exactly one bit, so a one-tone demodulation error costs
/// one bit error.
pub const GRAY_MAP: [u8; 8] = [0, 1, 3, 2, 5, 6, 4, 7];

/// Number of `c28` values reserved for special tokens and directed CQ.
///
/// Provenance: published c28 field layout. `0 = DE`, `1 = QRZ`,
/// `2 = CQ`; the remainder up to `NTOKENS` encode directed-CQ forms
/// (not produced by this module). Consistency proof: `NTOKENS + MAX22
/// + 37·36·10·27³ = 2²⁸` exactly (the c28 field is full).
pub const NTOKENS: u32 = 2_063_592;

/// Number of `c28` values reserved for 22-bit callsign hashes
/// (`2²²`; not produced by this module). See [`NTOKENS`].
pub const MAX22: u32 = 4_194_304;

/// Compile-time proof that the c28 partition fills 28 bits exactly.
const _C28_FULL: () =
    assert!(NTOKENS as u64 + MAX22 as u64 + 37 * 36 * 10 * 27 * 27 * 27 == 1 << 28);

/// Largest `g15` value encoding a 4-character grid; larger values are
/// the special trailers (blank/RRR/RR73/73/report). Published field
/// layout: `18·18·10·10 = 32400`.
pub const MAXGRID4: u16 = 32_400;

/// The 42-character free-text alphabet: `f71` is a base-42 integer over
/// this set.
///
/// Provenance: `free_text_to_f71.f90` in the authors' public-domain
/// resource package, vendored at
/// `third_party/ft4_ft8_public/free_text_to_f71.f90` and checked by
/// `tests/ft8.rs::alphabets_match_public_domain_files`.
pub const FREE_TEXT_ALPHABET: &[u8; 42] = b" 0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ+-./?";

/// The four positional character sets of the `c28` callsign field, for a
/// callsign aligned to the 6-character shape. Position 0 uses
/// `C28_SETS[0]`, position 1 `C28_SETS[1]`, position 2 `C28_SETS[2]`,
/// and positions 3..6 all use `C28_SETS[3]`.
///
/// Provenance: `std_call_to_c28.f90` in the authors' public-domain
/// resource package (its `a1`/`a2`/`a3`/`a4`), vendored at
/// `third_party/ft4_ft8_public/std_call_to_c28.f90` and checked by
/// `tests/ft8.rs::alphabets_match_public_domain_files`.
pub const C28_SETS: [&[u8]; 4] = [
    b" 0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ",
    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ",
    b"0123456789",
    b" ABCDEFGHIJKLMNOPQRSTUVWXYZ",
];

/// Gaussian frequency-pulse bandwidth-time product used by FT8.
///
/// Provenance: BT = 2.0 per the published waveform description.
pub const GFSK_BT: f64 = 2.0;

/// `K = π·√(2/ln 2)` in the GFSK pulse definition (mathematical
/// constant, not a transcribed table value).
const GFSK_K: f64 = 5.336_446_256_636_997;

/// The 83×91 generator matrix of the FT8 LDPC(174,91) code, one row per
/// parity bit, each row a **91-bit integer**: matrix column `j` is bit
/// `90 - j`, i.e. the row read MSB-first. Parity bit `i` is the GF(2)
/// dot product of row `i` with the 91-bit CRC-protected message.
///
/// # Provenance
///
/// This is a **public-domain protocol constant**, not a reimplementation
/// of anything. It is the content of `generator.dat` from the authors'
/// resource package `ft4_ft8_protocols.tgz` — reference \[14\] of the QEX
/// paper — which §9 of that paper places in the public domain and
/// explicitly excludes from WSJT-X's GPLv3. The file is vendored at
/// `third_party/ft4_ft8_public/generator.dat` (see the README beside it
/// for the full dedication text and its conditions).
///
/// `generator.dat` stores each row as 91 binary digits. The integer form
/// here is this crate's own encoding of those rows, and
/// `tests/ft8.rs::generator_bits_match_public_domain_file` reads the
/// vendored file and proves the two agree — so the provenance is
/// machine-checked on every CI run rather than asserted in a comment.
/// It is **not** the 23- or 24-hex-character-per-row form used by some
/// implementations: 91 bits is not a whole number of hex digits, so
/// those forms embed a padding choice belonging to a particular source
/// file rather than to the protocol.
///
/// Beyond that file, correctness is pinned by internal consistency
/// (encoder ↔ [`ldpc_check`], single-bit-flip detection) and by the RX
/// slice closing the loop end to end — the latter over this crate's own
/// transmissions only, so it is self-consistency rather than
/// verification against independently produced signals.
#[rustfmt::skip]
pub const GENERATOR_BITS: [u128; 83] = [
    0x4194e708df98f57a84f93fe, 0x3b0e132712e12c99aa49899, 0x6e132c817d93be320850dee,
    0x0d9fa0bc2c6696e99f63fb1, 0x04fed27f7020cafe81a3c1d, 0x03be66608dc439f6ae1ea45,
    0x14db157f1e501b7a7f0d4ed, 0x302a7d7af9aecb69d86461f, 0x7103cc7218877693c425748,
    0x3bae4e047407136ed72b18c, 0x585c08814615fccb909a43e, 0x0c5064918fe3056fae2f519,
    0x3b238f418150390f00d895c, 0x7fde65c06541a0fd7da3d97, 0x3353950ac7c992d15fb38b8,
    0x62121b44ff42d8e289b1d0c, 0x06ffb9ca0a68d0d9a58e138, 0x0ada441831b645ccc4a4b97,
    0x14d44e069ef40eb32a44d87, 0x2789379bfd28e5f30deb5ca, 0x4ce2391ce86cbe9e42704a0,
    0x0c8cdba88cbb2b10dda78f4, 0x04ed896b98fd7705c36fb5c, 0x2447e19efa1fdef752757da,
    0x413a11f7205b3afbab75aff, 0x55f0cbe24265ba3ab8a254d, 0x15a80725e0762d3695edee8,
    0x623a5529eb810c3b0b349b0, 0x475d0d09ed99c85eb38c676, 0x3a9c22339d13bc166210097,
    0x037fc1d0a2e1b81ad2e0934, 0x1d9ba0bc2c6616e99f61fb1, 0x4d252d14770be54e1924216,
    0x5e14fa32984e4bbf44b0852, 0x1331d736efc5ae715d94a44, 0x237918f7f22b81a60c0a20c,
    0x1fd96742d5f4d86397037df, 0x6f43a40f94160a9cb8d0517, 0x7e6be6791e34fd4cddd0a09,
    0x78130a23f4a48654723a676, 0x220808ac0c0cb7cae6eb809, 0x0447e18efa5fdef152757da,
    0x5c7f78db183b94fd8503c60, 0x2d7f53d6665bbdde4eccd48, 0x24d380b56329fb2f66e483b,
    0x0ca26842df273ed46b663e8, 0x128fb156e2019787738a001, 0x2b238fc38150390f005895c,
    0x15c72491f96ea8f16a9bfd0, 0x35aa85205337a3aaef4ae13, 0x50c56946a713ff49527b642,
    0x086172c31c465c151ec03ac, 0x779a520c0bf701099ed9758, 0x3f4e062a192d4e0ac1b7000,
    0x1b49f2b968fef266f83cf43, 0x5fd96762d5f0d8639703fdf, 0x3f70c11862c1e6662bea584,
    0x50336597f6d7e4fa9332093, 0x5d91b92d5e23e62fa662669, 0x6f6cedd1df72062cdab04da,
    0x6cd380b56329f36f66e481b, 0x4d6a3576afb83f94055afe2, 0x72c90e3bc112c398b6be9e1,
    0x278a6d4121545c36e5399a9, 0x45c5a83d6a33ea220efbb87, 0x11418e4e788b4a33d6825b4,
    0x109dc1c7f1572a61c7738c0, 0x2ec935b6eb8f8428c0d2709, 0x3355bcea594f73734a84f2b,
    0x4ac0a43416ba451c6eb45d5, 0x5c6701067834e195391d58a, 0x7a198eb6a30b03f4aba93a3,
    0x36d11dd2125cacb099e7ce4, 0x531b5e5e3d9862fdf5733ff, 0x2e586c3503efb2a54844d10,
    0x788f8834243c07e4f66ec05, 0x0fdda9b27dc6964eb986add, 0x7e5c35e3852864e8152e81a,
    0x529a219814f560af99171a6, 0x64c4ece3e1e9dc62aeba898, 0x3dd9c59780c36a3321d74b1,
    0x132275d6f5a25ca33e8fa16, 0x3046642baca5fddaaeb4b00,
];

/// [`GENERATOR_BITS`] repacked at compile time: 91 bits per row,
/// left-justified in 12 bytes (the 5 low bits of the last byte zero),
/// which is the layout [`ldpc_encode`] and [`ldpc_check`] walk.
static GENERATOR_ROWS: [[u8; MESSAGE_LEN]; PARITY_BITS] = parse_generator();

/// Repacks the 91-bit generator rows into left-justified bytes (const
/// context). Matrix column `j` is bit `90 - j` of the integer and bit
/// `7 - j % 8` of byte `j / 8`.
const fn parse_generator() -> [[u8; MESSAGE_LEN]; PARITY_BITS] {
    let mut rows = [[0u8; MESSAGE_LEN]; PARITY_BITS];
    let mut r = 0;
    while r < PARITY_BITS {
        let row = GENERATOR_BITS[r];
        assert!(row >> MESSAGE_BITS == 0, "generator row exceeds 91 bits");
        let mut j = 0;
        while j < MESSAGE_BITS {
            if (row >> (MESSAGE_BITS - 1 - j)) & 1 == 1 {
                rows[r][j / 8] |= 1 << (7 - j % 8);
            }
            j += 1;
        }
        r += 1;
    }
    rows
}

/// Errors from FT8 message or configuration validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Ft8Error {
    /// The callsign contains `/`: compound/suffixed calls (portable,
    /// rover, …) need hashed or nonstandard-call message types that
    /// this module does not implement.
    CallsignCompound,
    /// The callsign is empty or longer than six characters.
    CallsignLength {
        /// The rejected length in characters.
        len: usize,
    },
    /// A callsign character is outside the allowed set for its
    /// position in the aligned 6-character standard form (position 0:
    /// space/letter/digit; 1: letter/digit; 2: digit; 3–5:
    /// letter/space).
    CallsignChar {
        /// The rejected character (upper-cased).
        ch: char,
        /// Its zero-based position in the space-aligned 6-char form.
        index: usize,
    },
    /// The callsign cannot be aligned so its third character is a
    /// digit (the standard-callsign shape rule).
    CallsignShape,
    /// `CQ`/`QRZ`/`DE` appeared as the second callsign, where the
    /// standard message requires a real callsign.
    TokenNotAllowedHere,
    /// A directed CQ (`CQ DX`, `CQ POTA`, `CQ 001`, …) was requested;
    /// only plain `CQ` is in the supported subset.
    DirectedCqUnsupported,
    /// The locator is finer than the square the `g15` field can
    /// carry: it holds two field letters and two square digits and
    /// nothing else, so a 6- or 8-character [`MaidenheadGrid`] is
    /// rejected rather than silently truncated.
    GridLength {
        /// The rejected locator length in characters (6 or 8).
        len: usize,
    },
    /// The signal report is outside the representable/practical range
    /// `-30..=+49` dB.
    ReportOutOfRange {
        /// The rejected report in dB.
        got: i8,
    },
    /// The acknowledgement flag `R` was combined with a trailer that
    /// cannot carry it (only a grid or a report may be R-flagged).
    AckFlagInvalid,
    /// The free text is longer than 13 characters.
    FreeTextLength {
        /// The rejected length in characters.
        len: usize,
    },
    /// A free-text character is outside the published 42-character
    /// alphabet (space, `0-9`, `A-Z`, `+-./?`).
    FreeTextChar {
        /// The rejected character (upper-cased).
        ch: char,
        /// Its zero-based position in the text.
        index: usize,
    },
    /// The requested message shape needs an FT8 payload type (`i3`
    /// / `n3` combination) outside this module's documented subset
    /// (standard `i3 = 1` and free text `i3.n3 = 0.0`).
    UnsupportedMessageType,
    /// The LDPC min-sum decoder reached its hard iteration cap
    /// ([`LDPC_MAX_ITERS`]) with parity checks still failing: the
    /// input is noise or below the decodable floor.
    LdpcNotConverged,
    /// The LDPC decode converged to a codeword whose CRC-14 does not
    /// match its payload (a wrong-codeword convergence, rejected).
    CrcMismatch,
    /// The sample rate is not an exact multiple of 25 Hz, so a symbol
    /// (`rate / 6.25` samples) would not span a whole number of
    /// samples.
    SampleRateInexact {
        /// The rejected sample rate in Hz.
        got: u32,
    },
    /// The highest tone (base + 7 × 6.25 Hz) would reach or exceed
    /// the Nyquist frequency, or the base frequency is zero.
    ToneOutOfRange {
        /// The requested base audio frequency in Hz.
        base_hz: u32,
        /// The configured sample rate in Hz.
        sample_rate: u32,
    },
}

impl fmt::Display for Ft8Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::CallsignCompound => write!(
                f,
                "callsign contains '/': compound calls need message types outside the supported subset"
            ),
            Self::CallsignLength { len } => write!(
                f,
                "callsign length {len} is invalid: must be 1..=6 characters"
            ),
            Self::CallsignChar { ch, index } => write!(
                f,
                "callsign character {ch:?} is invalid at aligned position {index}"
            ),
            Self::CallsignShape => write!(
                f,
                "callsign cannot be aligned to the standard shape (third character must be a digit)"
            ),
            Self::TokenNotAllowedHere => write!(
                f,
                "CQ/QRZ/DE cannot stand as the second callsign of a standard message"
            ),
            Self::DirectedCqUnsupported => write!(
                f,
                "directed CQ is outside the supported subset: only plain \"CQ\" is encoded"
            ),
            Self::GridLength { len } => write!(
                f,
                "grid locator length {len} is invalid: must be exactly 4 characters"
            ),
            Self::ReportOutOfRange { got } => write!(
                f,
                "report {got} dB is out of range: must be within -30..=+49"
            ),
            Self::AckFlagInvalid => write!(
                f,
                "the R flag can only accompany a grid or a report trailer"
            ),
            Self::FreeTextLength { len } => write!(
                f,
                "free text length {len} is invalid: must be at most 13 characters"
            ),
            Self::FreeTextChar { ch, index } => write!(
                f,
                "free-text character {ch:?} at position {index} is outside the 42-character alphabet"
            ),
            Self::UnsupportedMessageType => write!(
                f,
                "message needs an FT8 payload type outside the supported subset (standard i3=1, free text i3.n3=0.0)"
            ),
            Self::LdpcNotConverged => write!(
                f,
                "LDPC decode did not converge within the {LDPC_MAX_ITERS}-iteration cap"
            ),
            Self::CrcMismatch => write!(
                f,
                "CRC-14 mismatch: the LDPC decode converged to a wrong codeword"
            ),
            Self::SampleRateInexact { got } => write!(
                f,
                "sample rate {got} Hz cannot time FT8 symbols exactly: must be a multiple of 25 Hz"
            ),
            Self::ToneOutOfRange {
                base_hz,
                sample_rate,
            } => write!(
                f,
                "base frequency {base_hz} Hz is invalid at {sample_rate} Hz: tones must be nonzero and below Nyquist"
            ),
        }
    }
}

impl core::error::Error for Ft8Error {}

/// Hard cap on LDPC min-sum belief-propagation iterations.
///
/// The decoder exits early as soon as every parity check is satisfied
/// (`H·ĉ = 0`); on undecodable input it runs exactly this many
/// iterations and returns [`Ft8Error::LdpcNotConverged`] — decode time
/// is bounded by construction, never by luck.
pub const LDPC_MAX_ITERS: u32 = 40;

/// Normalization factor of the normalized-min-sum check update
/// (attenuates the min-sum over-estimate; 0.8 is the textbook choice).
const MIN_SUM_ALPHA: f32 = 0.8;

/// The sparse parity-check matrix of the LDPC(174,91) code: one row
/// per check, the codeword bit indices it covers (weight 6 or 7; the
/// value 255 pads weight-6 rows).
///
/// # Provenance, established twice over
///
/// This table is pinned from two independent directions: one ties it to
/// the published source, the other to the mathematics.
///
/// 1. **The published source.** It is the transpose of `parity.dat` from
///    the authors' public-domain resource package — reference \[14\] of
///    the QEX paper, vendored at
///    `third_party/ft4_ft8_public/parity.dat`. That file lists, for each
///    of the 174 columns, the three one-based rows holding a one;
///    transposing gives exactly these 83 rows.
///    `tests/ft8.rs::check_rows_match_public_domain_parity_file` proves
///    it. (Row *order* is arbitrary in a parity-check matrix and differs
///    from the file's, so the test compares the rows as a multiset.)
/// 2. **The mathematics.** It is also *derivable* from
///    [`GENERATOR_BITS`] alone: these are the 83 low-weight (≤ 7) rows
///    of the dual code spanned by `[G | I₈₃]`, and
///    `tests/ft8.rs::check_rows_match_derivation_from_generator`
///    re-derives them by randomized Gaussian elimination. `tests/ft8_rx.rs`
///    additionally checks that every row annihilates every codeword,
///    every codeword bit appears in exactly 3 checks, and the 83 rows
///    have full rank.
///
/// This is the sparse form the published FT8 code is defined by (row
/// weights 6–7, column weight 3).
#[rustfmt::skip]
pub const CHECK_ROWS: [[u8; 7]; PARITY_BITS] = [
    [16, 26, 88, 102, 115, 152, 255], [3, 28, 67, 119, 133, 172, 255],
    [27, 28, 83, 87, 116, 142, 149], [13, 29, 82, 112, 124, 169, 255],
    [2, 23, 29, 71, 103, 138, 255], [30, 68, 132, 149, 154, 168, 255],
    [13, 30, 78, 97, 131, 163, 255], [27, 31, 71, 102, 131, 165, 255],
    [0, 32, 71, 105, 106, 156, 255], [5, 32, 84, 107, 115, 155, 255],
    [4, 33, 64, 77, 97, 106, 153], [22, 33, 70, 93, 126, 152, 255],
    [28, 33, 86, 96, 146, 161, 255], [34, 81, 132, 141, 170, 173, 255],
    [8, 34, 65, 98, 138, 145, 255], [9, 35, 66, 99, 106, 125, 255],
    [17, 35, 75, 88, 112, 113, 142], [10, 36, 66, 86, 100, 138, 157],
    [16, 36, 73, 80, 108, 130, 153], [20, 36, 72, 137, 151, 168, 255],
    [11, 37, 67, 101, 104, 154, 255], [18, 37, 76, 103, 115, 162, 255],
    [24, 37, 64, 98, 121, 159, 255], [4, 38, 74, 101, 135, 166, 255],
    [12, 38, 68, 102, 148, 161, 255], [7, 39, 69, 81, 103, 113, 144],
    [8, 39, 89, 105, 133, 150, 255], [13, 40, 70, 87, 101, 122, 155],
    [25, 40, 76, 108, 140, 147, 255], [16, 41, 74, 128, 169, 171, 255],
    [17, 41, 78, 143, 145, 151, 255], [11, 42, 65, 88, 96, 134, 158],
    [15, 42, 72, 107, 140, 159, 255], [22, 42, 78, 119, 130, 144, 255],
    [2, 43, 79, 123, 126, 168, 255], [9, 43, 81, 90, 110, 143, 148],
    [10, 43, 74, 109, 120, 165, 255], [20, 44, 77, 82, 116, 120, 150],
    [0, 25, 44, 79, 127, 146, 255], [7, 45, 70, 111, 118, 165, 255],
    [18, 45, 80, 116, 134, 166, 255], [19, 45, 64, 79, 119, 139, 169],
    [15, 46, 75, 129, 136, 153, 255], [19, 46, 69, 91, 137, 164, 255],
    [1, 47, 73, 112, 127, 159, 255], [2, 12, 47, 77, 94, 122, 255],
    [27, 47, 69, 84, 104, 128, 157], [10, 48, 87, 91, 141, 156, 255],
    [6, 49, 80, 98, 131, 172, 255], [51, 83, 109, 114, 144, 167, 255],
    [23, 51, 75, 128, 147, 148, 255], [9, 52, 65, 83, 111, 127, 164],
    [21, 52, 67, 108, 120, 173, 255], [24, 52, 68, 89, 100, 129, 155],
    [1, 53, 85, 100, 134, 163, 255], [20, 53, 76, 99, 139, 170, 255],
    [22, 54, 66, 94, 171, 173, 255], [17, 48, 54, 123, 140, 166, 255],
    [14, 55, 86, 107, 118, 170, 255], [26, 39, 55, 123, 124, 125, 255],
    [25, 50, 55, 90, 121, 136, 167], [21, 56, 84, 92, 139, 158, 255],
    [50, 56, 97, 162, 164, 171, 255], [0, 3, 51, 56, 85, 135, 151],
    [21, 46, 57, 117, 126, 163, 255], [6, 48, 57, 89, 99, 104, 167],
    [3, 30, 58, 90, 91, 95, 152], [18, 34, 58, 72, 109, 124, 160],
    [14, 41, 58, 105, 122, 158, 255], [4, 31, 59, 92, 114, 145, 255],
    [29, 49, 59, 85, 136, 141, 161], [14, 57, 59, 73, 110, 149, 162],
    [5, 23, 60, 93, 121, 150, 255], [11, 49, 60, 117, 118, 143, 255],
    [6, 32, 61, 94, 95, 142, 255], [15, 38, 61, 111, 133, 157, 255],
    [1, 26, 40, 60, 61, 114, 132], [7, 24, 62, 82, 92, 95, 147],
    [19, 35, 62, 93, 135, 160, 255], [8, 53, 62, 130, 146, 154, 255],
    [5, 31, 63, 96, 125, 137, 255], [12, 50, 63, 113, 117, 156, 255],
    [44, 54, 63, 110, 129, 160, 172],
];

/// `|x|` without the std/core-version question: clears the sign bit.
#[inline]
fn fabs(x: f32) -> f32 {
    f32::from_bits(x.to_bits() & 0x7FFF_FFFF)
}

/// Per-bit log-likelihood ratios from 8-tone soft symbol energies.
///
/// `energies[j][t]` is the demodulated energy of tone `t` in the j-th
/// **data** symbol (the 58 non-Costas symbols in transmission order).
/// For each of the 3 bits of a symbol the Gray map is inverted with
/// the max-log approximation: `LLR = max(energy over tones whose bit
/// is 0) − max(energy over tones whose bit is 1)`, normalized by the
/// mean energy so the scale is capture-independent (min-sum decoding
/// is invariant to a common positive scale anyway).
///
/// Sign convention: **positive LLR means the bit is more likely 0**
/// (the convention [`ldpc_decode`] expects).
#[must_use]
pub fn llrs_from_energies(energies: &[[f32; 8]; 58]) -> [f32; CODEWORD_BITS] {
    // Inverse Gray map: tone -> 3-bit group.
    let mut inv = [0u8; 8];
    for (bits, &tone) in GRAY_MAP.iter().enumerate() {
        inv[usize::from(tone)] = bits as u8;
    }
    let mut mean = 0.0f32;
    for e in energies {
        for &v in e {
            mean += v;
        }
    }
    mean = (mean / (58.0 * 8.0)).max(f32::MIN_POSITIVE);
    let mut llr = [0.0f32; CODEWORD_BITS];
    for (j, e) in energies.iter().enumerate() {
        for b in 0..3 {
            let mut max0 = f32::MIN;
            let mut max1 = f32::MIN;
            for (tone, &v) in e.iter().enumerate() {
                if (inv[tone] >> (2 - b)) & 1 == 0 {
                    if v > max0 {
                        max0 = v;
                    }
                } else if v > max1 {
                    max1 = v;
                }
            }
            llr[3 * j + b] = (max0 - max1) / mean;
        }
    }
    llr
}

/// LDPC(174,91) soft decode: normalized min-sum belief propagation
/// over the sparse parity-check matrix [`CHECK_ROWS`].
///
/// `llr[i]` is the log-likelihood ratio of codeword bit `i`, positive
/// meaning bit 0 (see [`llrs_from_energies`]). Iterates at most
/// [`LDPC_MAX_ITERS`] times with an early exit as soon as the hard
/// decision satisfies every check, and returns the packed 174-bit
/// codeword (whose first 91 bits are the CRC-protected message — pass
/// them on to [`verify_crc`]).
///
/// RAM class (all on the stack, no allocation): 174 f32 posteriors
/// plus 83×7 f32 check-to-variable messages, ≈ **3.0 KB** inside this
/// function — the ~3 KB budget of the weak-signal plan. The caller's
/// 174 f32 channel-LLR array adds ≈ 0.7 KB, so the decode path costs
/// ≈ 3.7 KB end to end.
///
/// # Errors
///
/// [`Ft8Error::LdpcNotConverged`] when the iteration cap is reached
/// with parity checks still failing (the input is noise or the signal
/// is below the decodable floor).
pub fn ldpc_decode(llr: &[f32; CODEWORD_BITS]) -> Result<[u8; CODEWORD_LEN], Ft8Error> {
    // Check-to-variable messages, one slot per (check, edge).
    let mut c2v = [[0.0f32; 7]; PARITY_BITS];
    let mut posterior = [0.0f32; CODEWORD_BITS];
    for iter in 0..LDPC_MAX_ITERS {
        // Posterior = channel LLR + sum of incoming check messages.
        posterior.copy_from_slice(llr);
        for (row, msgs) in CHECK_ROWS.iter().zip(c2v.iter()) {
            for (&v, &m) in row.iter().zip(msgs.iter()) {
                if v != 255 {
                    posterior[usize::from(v)] += m;
                }
            }
        }
        // Early exit on a valid hard decision.
        if checks_satisfied(&posterior) {
            let _ = iter;
            return Ok(pack_hard_decision(&posterior));
        }
        // Check update: normalized min-sum on extrinsic messages.
        for (row, msgs) in CHECK_ROWS.iter().zip(c2v.iter_mut()) {
            let mut v2c = [0.0f32; 7];
            let mut sign_all = 1.0f32;
            let (mut min1, mut min2) = (f32::MAX, f32::MAX);
            let mut min_at = 0usize;
            for (e, &v) in row.iter().enumerate() {
                if v == 255 {
                    continue;
                }
                let m = posterior[usize::from(v)] - msgs[e];
                v2c[e] = m;
                if m < 0.0 {
                    sign_all = -sign_all;
                }
                let a = fabs(m);
                if a < min1 {
                    min2 = min1;
                    min1 = a;
                    min_at = e;
                } else if a < min2 {
                    min2 = a;
                }
            }
            for (e, &v) in row.iter().enumerate() {
                if v == 255 {
                    continue;
                }
                let mag = if e == min_at { min2 } else { min1 };
                let sign = if v2c[e] < 0.0 { -sign_all } else { sign_all };
                msgs[e] = MIN_SUM_ALPHA * sign * mag;
            }
        }
    }
    Err(Ft8Error::LdpcNotConverged)
}

/// True when the hard decision of `posterior` satisfies all 83 checks.
fn checks_satisfied(posterior: &[f32; CODEWORD_BITS]) -> bool {
    CHECK_ROWS.iter().all(|row| {
        let mut parity = 0u8;
        for &v in row {
            if v != 255 && posterior[usize::from(v)] < 0.0 {
                parity ^= 1;
            }
        }
        parity == 0
    })
}

/// Packs the hard decision (negative posterior = bit 1) into 22 bytes.
fn pack_hard_decision(posterior: &[f32; CODEWORD_BITS]) -> [u8; CODEWORD_LEN] {
    let mut out = [0u8; CODEWORD_LEN];
    for (pos, &p) in posterior.iter().enumerate() {
        if p < 0.0 {
            out[pos / 8] |= 1 << (7 - pos % 8);
        }
    }
    out
}

/// Extracts the 91-bit CRC-protected message from a decoded codeword
/// (the code is systematic: the first 91 bits).
#[must_use]
pub fn message_from_codeword(codeword: &[u8; CODEWORD_LEN]) -> [u8; MESSAGE_LEN] {
    let mut out = [0u8; MESSAGE_LEN];
    out.copy_from_slice(&codeword[..MESSAGE_LEN]);
    out[MESSAGE_LEN - 1] &= 0xE0;
    out
}

/// Verifies the CRC-14 of a 91-bit message and returns the 77-bit
/// payload on success.
///
/// # Errors
///
/// [`Ft8Error::CrcMismatch`] when the appended CRC does not match the
/// payload (an LDPC "success" that converged to a wrong codeword — the
/// CRC is the final arbiter).
pub fn verify_crc(message: &[u8; MESSAGE_LEN]) -> Result<[u8; PAYLOAD_LEN], Ft8Error> {
    let mut payload = [0u8; PAYLOAD_LEN];
    payload.copy_from_slice(&message[..PAYLOAD_LEN]);
    payload[PAYLOAD_LEN - 1] &= 0xF8;
    let mut crc: u16 = 0;
    for pos in PAYLOAD_BITS..MESSAGE_BITS {
        crc = (crc << 1) | u16::from((message[pos / 8] >> (7 - pos % 8)) & 1);
    }
    if crc14(&payload) == crc {
        Ok(payload)
    } else {
        Err(Ft8Error::CrcMismatch)
    }
}

/// The rendered text of an unpacked FT8 message: a fixed-buffer,
/// no_std string (at most [`Ft8Text::CAPACITY`] bytes of ASCII).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ft8Text {
    buf: [u8; Self::CAPACITY],
    len: usize,
}

impl Ft8Text {
    /// Maximum rendered length: two 6-char callsigns, the `R` flag and
    /// a 4-char trailer with separators fit well inside 24 bytes (free
    /// text is at most 13).
    pub const CAPACITY: usize = 24;

    fn new() -> Self {
        Self {
            buf: [0; Self::CAPACITY],
            len: 0,
        }
    }

    fn push_bytes(&mut self, s: &[u8]) {
        for &b in s {
            if self.len < Self::CAPACITY {
                self.buf[self.len] = b;
                self.len += 1;
            }
        }
    }

    /// The message text (trimmed, single-space separated fields).
    #[must_use]
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl fmt::Display for Ft8Text {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Unpacks a 77-bit payload of the supported subset into its message
/// text: `i3 = 1` standard messages (`"CQ K1ABC FN42"`,
/// `"K1ABC W9XYZ R-08"`, …) and `i3 = 0, n3 = 0` free text (trailing
/// padding spaces trimmed). The exact inverse of [`Ft8Message`]'s
/// constructors over their supported domain.
///
/// # Errors
///
/// [`Ft8Error::UnsupportedMessageType`] for every payload type outside
/// the subset (other `i3`/`n3`, hashed or rover-flagged callsigns) and
/// [`Ft8Error::DirectedCqUnsupported`] for directed-CQ token values —
/// a decode of such a signal reports *why* it cannot be rendered
/// rather than mis-rendering it.
pub fn unpack_message(payload: &[u8; PAYLOAD_LEN]) -> Result<Ft8Text, Ft8Error> {
    let i3 = (payload[9] >> 3) & 0x7;
    match i3 {
        0 => {
            let chars = unpack_free_text(payload)?;
            // Trim both ends: the transmitter right-justifies (see
            // `Ft8Message::free_text`), older software left-justifies,
            // and the padding is indistinguishable from a space the
            // operator typed. Trimming one end only would render half
            // the world's free text with a ragged margin.
            let start = chars.iter().position(|&c| c != b' ').unwrap_or(chars.len());
            let end = chars
                .iter()
                .rposition(|&c| c != b' ')
                .map_or(start, |p| p + 1);
            let mut text = Ft8Text::new();
            text.push_bytes(&chars[start..end]);
            Ok(text)
        }
        1 => unpack_standard(payload),
        _ => Err(Ft8Error::UnsupportedMessageType),
    }
}

/// Reads `count` bits MSB-first starting at bit `pos` of the payload.
fn read_bits(payload: &[u8; PAYLOAD_LEN], pos: usize, count: usize) -> u32 {
    let mut v = 0u32;
    for i in pos..pos + count {
        v = (v << 1) | u32::from((payload[i / 8] >> (7 - i % 8)) & 1);
    }
    v
}

/// Unpacks an `i3 = 1` standard message (see [`unpack_message`]).
fn unpack_standard(payload: &[u8; PAYLOAD_LEN]) -> Result<Ft8Text, Ft8Error> {
    let c28a = read_bits(payload, 0, 28);
    let r1a = read_bits(payload, 28, 1);
    let c28b = read_bits(payload, 29, 28);
    let r1b = read_bits(payload, 57, 1);
    let r = read_bits(payload, 58, 1) == 1;
    let g15 = read_bits(payload, 59, 15) as u16;
    if r1a != 0 || r1b != 0 {
        // /R rover suffixes are outside the supported subset (the TX
        // side never produces them either).
        return Err(Ft8Error::UnsupportedMessageType);
    }
    let mut text = Ft8Text::new();
    unpack_c28(c28a, &mut text)?;
    text.push_bytes(b" ");
    unpack_c28(c28b, &mut text)?;
    match g15 {
        v if v < MAXGRID4 => {
            text.push_bytes(if r { b" R " } else { b" " });
            let d2 = v % 10;
            let d1 = (v / 10) % 10;
            let f2 = (v / 100) % 18;
            let f1 = v / 1800;
            text.push_bytes(&[
                b'A' + f1 as u8,
                b'A' + f2 as u8,
                b'0' + d1 as u8,
                b'0' + d2 as u8,
            ]);
        }
        v if v == MAXGRID4 + 1 => {}
        v if v == MAXGRID4 + 2 => text.push_bytes(b" RRR"),
        v if v == MAXGRID4 + 3 => text.push_bytes(b" RR73"),
        v if v == MAXGRID4 + 4 => text.push_bytes(b" 73"),
        v if (MAXGRID4 + 5..=MAXGRID4 + 84).contains(&v) => {
            let report = (v - MAXGRID4) as i16 - 35;
            text.push_bytes(if r { b" R" } else { b" " });
            text.push_bytes(if report < 0 { b"-" } else { b"+" });
            let mag = report.unsigned_abs();
            text.push_bytes(&[b'0' + (mag / 10) as u8, b'0' + (mag % 10) as u8]);
        }
        _ => return Err(Ft8Error::UnsupportedMessageType),
    }
    Ok(text)
}

/// Renders one `c28` field value (the inverse of [`pack_c28`] over the
/// supported subset).
fn unpack_c28(v: u32, text: &mut Ft8Text) -> Result<(), Ft8Error> {
    match v {
        0 => {
            text.push_bytes(b"DE");
            return Ok(());
        }
        1 => {
            text.push_bytes(b"QRZ");
            return Ok(());
        }
        2 => {
            text.push_bytes(b"CQ");
            return Ok(());
        }
        _ => {}
    }
    if v < NTOKENS {
        return Err(Ft8Error::DirectedCqUnsupported);
    }
    if v < NTOKENS + MAX22 {
        // 22-bit callsign hash: not representable as text here.
        return Err(Ft8Error::UnsupportedMessageType);
    }
    let mut n = v - NTOKENS - MAX22;
    let [s0, s1, s2, s3] = C28_SETS;
    let c5 = s3[(n % 27) as usize];
    n /= 27;
    let c4 = s3[(n % 27) as usize];
    n /= 27;
    let c3 = s3[(n % 27) as usize];
    n /= 27;
    let c2 = s2[(n % 10) as usize];
    n /= 10;
    let c1 = s1[(n % 36) as usize];
    n /= 36;
    let c0 = s0[n as usize];
    let aligned = [c0, c1, c2, c3, c4, c5];
    let start = aligned.iter().position(|&c| c != b' ').unwrap_or(0);
    let end = aligned
        .iter()
        .rposition(|&c| c != b' ')
        .map_or(0, |p| p + 1);
    text.push_bytes(&aligned[start..end.max(start)]);
    Ok(())
}

/// The `g15` trailer of a standard message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ft8Tail {
    /// No trailer (`g15` encodes "blank").
    None,
    /// A Maidenhead locator. Only [`GridPrecision::Square`] fits the
    /// `g15` field — [`pack_g15`] rejects anything finer.
    ///
    /// [`GridPrecision::Square`]: crate::geo::GridPrecision::Square
    Grid(MaidenheadGrid),
    /// A signal report in dB, `-30..=+49` (e.g. `-8` → `"-08"`).
    Report(i8),
    /// `RRR` — all received.
    Rrr,
    /// `RR73` — all received, best regards.
    ///
    /// Encoded as the Maidenhead square `RR73`, not as the reserved
    /// token above [`MAXGRID4`]; see [`pack_g15`] for why.
    Rr73,
    /// `73` — best regards.
    Seventy3,
}

impl Ft8Tail {
    /// Builds a [`Ft8Tail::Grid`] from locator text, e.g. `"FN42"`.
    ///
    /// The one text-accepting entry point kept in this module: people
    /// type locators, and a variant cannot validate its own payload.
    /// The text is parsed into a [`MaidenheadGrid`] immediately, so
    /// nothing downstream ever holds an unvalidated string.
    ///
    /// ```
    /// use warble::ft8::Ft8Tail;
    /// use warble::geo::MaidenheadGrid;
    ///
    /// assert_eq!(Ft8Tail::grid("fn42")?, Ft8Tail::Grid(MaidenheadGrid::new("FN42")?));
    /// # Ok::<(), warble::geo::GeoError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// The [`GeoError`] from [`MaidenheadGrid::new`]. A well-formed but
    /// too-fine locator is accepted here and rejected by [`pack_g15`]
    /// with [`Ft8Error::GridLength`]: parsing and field capacity are
    /// different questions.
    pub const fn grid(text: &str) -> Result<Self, GeoError> {
        match MaidenheadGrid::new(text) {
            Ok(grid) => Ok(Self::Grid(grid)),
            Err(e) => Err(e),
        }
    }
}

/// A validated FT8 message holding its packed 77-bit payload.
///
/// Constructed only through the validating constructors
/// ([`Ft8Message::standard`], [`Ft8Message::free_text`]), so a value of
/// this type always channel-encodes successfully. See the module
/// documentation for the exact supported payload-type subset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ft8Message {
    /// 77 payload bits, MSB-first, left-justified (3 low bits of
    /// byte 9 are zero).
    payload: [u8; PAYLOAD_LEN],
}

impl Ft8Message {
    /// Builds a standard (`i3 = 1`) message: `call_a call_b [R] tail`.
    ///
    /// `call_a` may be a standard callsign or one of the tokens `CQ`,
    /// `QRZ`, `DE`; `call_b` must be a standard callsign. `r` sets the
    /// acknowledgement flag and is valid only with a grid or report
    /// trailer (`"R FN42"`, `"R-08"`).
    ///
    /// # Errors
    ///
    /// The specific [`Ft8Error`] naming the rejected field: compound
    /// or unalignable callsigns, tokens in the second position,
    /// directed CQ, bad grids, out-of-range reports, or an invalid `R`
    /// combination.
    pub fn standard(call_a: &str, call_b: &str, r: bool, tail: Ft8Tail) -> Result<Self, Ft8Error> {
        let c28a = pack_c28(call_a)?;
        let c28b = pack_c28(call_b)?;
        if c28b < NTOKENS + MAX22 {
            return Err(Ft8Error::TokenNotAllowedHere);
        }
        if r && !matches!(tail, Ft8Tail::Grid(_) | Ft8Tail::Report(_)) {
            return Err(Ft8Error::AckFlagInvalid);
        }
        let g15 = pack_g15(tail)?;
        let mut w = BitWriter::new();
        w.push(u64::from(c28a), 28);
        w.push(0, 1); // r1a: rover flag, always 0 in this subset
        w.push(u64::from(c28b), 28);
        w.push(0, 1); // r1b: rover flag, always 0 in this subset
        w.push(u64::from(r), 1); // R1
        w.push(u64::from(g15), 15);
        w.push(1, 3); // i3 = 1: standard message
        Ok(Self {
            payload: w.finish(),
        })
    }

    /// Builds a free-text (`i3 = 0`, `n3 = 0`) message of up to 13
    /// characters from the published alphabet
    /// `" 0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ+-./?"` (lowercase
    /// letters are upper-cased).
    ///
    /// Shorter texts are **right-justified**: the field is one base-42
    /// integer, so which end the padding goes on changes every bit,
    /// and the network right-justifies. [`unpack_message`] trims both
    /// ends, so leading and trailing spaces in `text` are not
    /// preserved — the wire format has no way to distinguish them
    /// from padding.
    ///
    /// # Errors
    ///
    /// [`Ft8Error::FreeTextLength`] beyond 13 characters,
    /// [`Ft8Error::FreeTextChar`] for characters outside the alphabet.
    pub fn free_text(text: &str) -> Result<Self, Ft8Error> {
        let len = text.chars().count();
        if len > 13 {
            return Err(Ft8Error::FreeTextLength { len });
        }
        // Right-justified: the padding goes on the **left**.
        //
        // The field is a single base-42 integer, so padding side is a
        // multiplication by 42^n and the two choices differ in every
        // bit. MEASURED against an independent encoder: "HELLO WORLD"
        // packs as "  HELLO WORLD" on the air. Left-justifying is
        // intelligible — both decoders trim — but it is not the same
        // transmission, and bit-identity is the bar this crate holds
        // its other modes to.
        let mut chars = [b' '; 13];
        let offset = 13 - len;
        for (index, ch) in text.chars().enumerate() {
            let up = ch.to_ascii_uppercase();
            if !up.is_ascii() || free_text_index(up).is_none() {
                return Err(Ft8Error::FreeTextChar { ch: up, index });
            }
            chars[offset + index] = up as u8;
        }
        // Base-42 pack, first character most significant: fits 71 bits
        // (42¹³ < 2⁷¹).
        let mut value: u128 = 0;
        for &c in &chars {
            let idx = free_text_index(c as char).unwrap_or(0);
            value = value * 42 + u128::from(idx);
        }
        let mut w = BitWriter::new();
        w.push((value >> 64) as u64, 7);
        w.push(value as u64, 64);
        w.push(0, 3); // n3 = 0
        w.push(0, 3); // i3 = 0
        Ok(Self {
            payload: w.finish(),
        })
    }

    /// The packed 77-bit payload, MSB-first, left-justified in 10
    /// bytes (the 3 low bits of the last byte are zero).
    #[must_use]
    pub fn payload(&self) -> [u8; PAYLOAD_LEN] {
        self.payload
    }

    /// Runs the full channel encoding for this message: CRC-14 →
    /// LDPC(174,91) → Gray-mapped tones with Costas sync — the 79
    /// channel symbols, each in `0..=7`.
    #[must_use]
    pub fn channel_symbols(&self) -> [u8; SYMBOL_COUNT] {
        let message = add_crc(&self.payload);
        let codeword = ldpc_encode(&message);
        symbols_from_codeword(&codeword)
    }
}

/// Accumulates MSB-first bits into a fixed 10-byte payload.
struct BitWriter {
    bytes: [u8; PAYLOAD_LEN],
    used: usize,
}

impl BitWriter {
    fn new() -> Self {
        Self {
            bytes: [0; PAYLOAD_LEN],
            used: 0,
        }
    }

    /// Appends the `count` low bits of `value`, MSB first.
    fn push(&mut self, value: u64, count: usize) {
        for i in (0..count).rev() {
            let bit = (value >> i) & 1;
            let pos = self.used;
            self.bytes[pos / 8] |= (bit as u8) << (7 - pos % 8);
            self.used += 1;
        }
    }

    fn finish(self) -> [u8; PAYLOAD_LEN] {
        debug_assert_eq!(self.used, PAYLOAD_BITS);
        self.bytes
    }
}

/// Index of `ch` in the published 42-character free-text alphabet, or
/// `None` when outside it.
fn free_text_index(ch: char) -> Option<u8> {
    FREE_TEXT_ALPHABET
        .iter()
        .position(|&a| a == ch as u8 && ch.is_ascii())
        .map(|i| i as u8)
}

/// Unpacks a free-text payload back to its 13 characters (the inverse
/// of [`Ft8Message::free_text`]; trailing padding spaces included).
///
/// # Errors
///
/// [`Ft8Error::UnsupportedMessageType`] when the payload is not
/// `i3 = 0, n3 = 0`, or the 71-bit value exceeds `42¹³ − 1`.
pub fn unpack_free_text(payload: &[u8; PAYLOAD_LEN]) -> Result<[u8; 13], Ft8Error> {
    // i3 = bits 74..77; n3 = bits 71..74 (bit 71 is byte 8 LSB,
    // bits 72..74 are byte 9 bits 7..6).
    let i3 = (payload[9] >> 3) & 0x7;
    let n3 = ((payload[8] & 1) << 2) | ((payload[9] >> 6) & 0x3);
    if i3 != 0 || n3 != 0 {
        return Err(Ft8Error::UnsupportedMessageType);
    }
    let mut value: u128 = 0;
    for pos in 0..71 {
        let bit = (payload[pos / 8] >> (7 - pos % 8)) & 1;
        value = (value << 1) | u128::from(bit);
    }
    if value >= 42u128.pow(13) {
        return Err(Ft8Error::UnsupportedMessageType);
    }
    let mut chars = [b' '; 13];
    for i in (0..13).rev() {
        chars[i] = FREE_TEXT_ALPHABET[(value % 42) as usize];
        value /= 42;
    }
    Ok(chars)
}

/// Packs a callsign (or the tokens `CQ`, `QRZ`, `DE`) into its 28-bit
/// `c28` field value.
///
/// Tokens: `DE` → 0, `QRZ` → 1, `CQ` → 2. A standard callsign is
/// aligned to the 6-character shape (third character a digit,
/// prepending a space when needed: `"K1ABC"` → `" K1ABC"`), then
/// packed positionally over the published character sets and offset by
/// `NTOKENS + MAX22`. Directed CQ, hashes, and compound calls are
/// rejected — see the module docs.
///
/// # Errors
///
/// [`Ft8Error::DirectedCqUnsupported`], [`Ft8Error::CallsignCompound`],
/// [`Ft8Error::CallsignLength`], [`Ft8Error::CallsignShape`], or
/// [`Ft8Error::CallsignChar`].
pub fn pack_c28(call: &str) -> Result<u32, Ft8Error> {
    let mut up = [0u8; 8];
    let mut len = 0usize;
    for ch in call.chars() {
        let c = ch.to_ascii_uppercase();
        if c == '/' {
            return Err(Ft8Error::CallsignCompound);
        }
        if c == ' ' {
            // "CQ DX" and friends arrive as one string with a space.
            if len >= 2 && &up[..2] == b"CQ" {
                return Err(Ft8Error::DirectedCqUnsupported);
            }
            return Err(Ft8Error::CallsignChar { ch: c, index: len });
        }
        if !c.is_ascii() || len >= 7 {
            return Err(Ft8Error::CallsignLength {
                len: call.chars().count(),
            });
        }
        up[len] = c as u8;
        len += 1;
    }
    match &up[..len] {
        b"DE" => return Ok(0),
        b"QRZ" => return Ok(1),
        b"CQ" => return Ok(2),
        _ => {}
    }
    if len == 0 || len > 6 {
        return Err(Ft8Error::CallsignLength { len });
    }
    // Align: the third character of the 6-char form must be a digit.
    let mut aligned = [b' '; 6];
    if len >= 3 && up[2].is_ascii_digit() {
        aligned[..len].copy_from_slice(&up[..len]);
    } else if (2..=5).contains(&len) && up[1].is_ascii_digit() {
        aligned[1..=len].copy_from_slice(&up[..len]);
    } else {
        return Err(Ft8Error::CallsignShape);
    }
    // Positional character sets (published): pos 0 space/digit/letter,
    // pos 1 digit/letter, pos 2 digit, pos 3..=5 space/letter.
    let i0 = c28_index(aligned[0], C28_SETS[0], 0)?;
    let i1 = c28_index(aligned[1], C28_SETS[1], 1)?;
    let i2 = c28_index(aligned[2], C28_SETS[2], 2)?;
    let i3 = c28_index(aligned[3], C28_SETS[3], 3)?;
    let i4 = c28_index(aligned[4], C28_SETS[3], 4)?;
    let i5 = c28_index(aligned[5], C28_SETS[3], 5)?;
    let n = ((((i0 * 36 + i1) * 10 + i2) * 27 + i3) * 27 + i4) * 27 + i5;
    Ok(n + NTOKENS + MAX22)
}

/// Looks up one aligned-callsign character in its positional set.
fn c28_index(c: u8, set: &[u8], index: usize) -> Result<u32, Ft8Error> {
    set.iter()
        .position(|&s| s == c)
        .map(|i| i as u32)
        .ok_or(Ft8Error::CallsignChar {
            ch: c as char,
            index,
        })
}

/// Packs a standard-message trailer into its 15-bit `g15` field value.
///
/// A grid `"AAnn"` packs positionally to `0..=32399`; the specials sit
/// above [`MAXGRID4`]: blank → `+1`, `RRR` → `+2`, `73` → `+4`, and a
/// report `r` dB → `MAXGRID4 + r + 35` (published field layout).
///
/// `RR73` is the exception: it packs as the *grid* `RR73` rather than
/// the reserved `+3` token, because that is what goes out on the air.
/// See `RR73_AS_GRID` in this module for the reasoning and the
/// measurement.  [`unpack_message`] accepts both spellings.
///
/// # Errors
///
/// [`Ft8Error::GridLength`] for a locator finer than a square,
/// [`Ft8Error::ReportOutOfRange`] outside `-30..=+49`.
pub fn pack_g15(tail: Ft8Tail) -> Result<u16, Ft8Error> {
    match tail {
        Ft8Tail::None => Ok(MAXGRID4 + 1),
        Ft8Tail::Rrr => Ok(MAXGRID4 + 2),
        Ft8Tail::Rr73 => Ok(RR73_AS_GRID),
        Ft8Tail::Seventy3 => Ok(MAXGRID4 + 4),
        Ft8Tail::Report(r) => {
            if !(-30..=49).contains(&r) {
                return Err(Ft8Error::ReportOutOfRange { got: r });
            }
            Ok(MAXGRID4 + (i16::from(r) + 35) as u16)
        }
        Ft8Tail::Grid(grid) => {
            // The locator's own invariant already guarantees the
            // alphabet (`A`–`R` fields, `0`–`9` squares); the only
            // question left is capacity, so the four-byte pattern is
            // both the length check and the field extraction.
            let &[f1, f2, d1, d2] = grid.as_bytes() else {
                return Err(Ft8Error::GridLength {
                    len: grid.precision().characters(),
                });
            };
            // v = ((f1·18 + f2)·10 + d1)·10 + d2, the published layout.
            let v = (u16::from(f1 - b'A') * 18 + u16::from(f2 - b'A')) * 10 + u16::from(d1 - b'0');
            Ok(v * 10 + u16::from(d2 - b'0'))
        }
    }
}

/// `RR73` packed as the Maidenhead square it also spells.
///
/// `MAXGRID4 + 3` is a perfectly good encoding of `RR73`, is what the
/// reserved-token table says, and every decoder must accept it — this
/// crate's own does, and still does. But no real transmitter *emits*
/// it. The dominant implementation's packer asks "is the last word a
/// valid four-character locator?" **before** it consults the token
/// list, and `RR73` is one, so an acknowledgement on the air carries
/// the grid index 32 373. The overload is safe because RR73 is a
/// square in the Arctic Ocean north of Siberia — 83.5°N, 175°E by
/// [`crate::geo::Coordinates::from_maidenhead`], which `tests/ft8.rs`
/// asserts rather than takes on trust — so no station transmits from
/// it. The same implementation's decoder special-cases the string
/// `RR73` where it would otherwise treat a grid as a grid.
///
/// Matching it is the point of having a differential at all: a warble
/// transmission is then **bit-identical** to the reference's rather
/// than just intelligible to it, which is the bar this crate already
/// holds WSPR to (all 162 channel symbols identical). Both spellings
/// decode to the same text either way, so nothing is lost by picking
/// the one the network uses.
///
/// Derivation, from the published four-character grid layout:
/// `((('R' - 'A') * 18 + ('R' - 'A')) * 10 + 7) * 10 + 3 = 32_373`.
const RR73_AS_GRID: u16 = 32_373;

/// Computes the FT8 CRC-14 of a 77-bit payload.
///
/// The published procedure: the 77 payload bits are zero-extended to
/// 82 bits (77 + 5 zeros) and divided MSB-first by the polynomial
/// `x¹⁴ + …` with low coefficients [`CRC_POLY`] (`0x2757`), zero
/// initial register, no final XOR; the 14-bit remainder is the CRC.
#[must_use]
pub fn crc14(payload: &[u8; PAYLOAD_LEN]) -> u16 {
    let mut reg: u16 = 0;
    for pos in 0..82 {
        // Bits 77..82 are the zero extension.
        let bit = if pos < PAYLOAD_BITS {
            (payload[pos / 8] >> (7 - pos % 8)) & 1
        } else {
            0
        };
        let top = (reg >> 13) & 1;
        reg = (reg << 1) & 0x3FFF;
        if top ^ u16::from(bit) == 1 {
            reg ^= CRC_POLY;
        }
    }
    reg
}

/// Appends the CRC-14 to a 77-bit payload: the 91-bit protected
/// message, MSB-first, left-justified in 12 bytes (5 low bits of the
/// last byte zero).
#[must_use]
pub fn add_crc(payload: &[u8; PAYLOAD_LEN]) -> [u8; MESSAGE_LEN] {
    let crc = crc14(payload);
    let mut out = [0u8; MESSAGE_LEN];
    out[..PAYLOAD_LEN].copy_from_slice(payload);
    // Payload bit 76 is byte 9 bit 3; CRC bits 13..0 follow at bits
    // 77..91.
    for i in 0..14 {
        let bit = (crc >> (13 - i)) & 1;
        let pos = PAYLOAD_BITS + i;
        out[pos / 8] |= (bit as u8) << (7 - pos % 8);
    }
    out
}

/// LDPC(174,91) systematic encode: appends the 83 parity bits from
/// the published generator matrix ([`GENERATOR_BITS`]) to the 91-bit
/// message. The codeword is the 91 message bits followed by the 83
/// parity bits, MSB-first, left-justified in 22 bytes (2 low bits of
/// the last byte zero).
#[must_use]
pub fn ldpc_encode(message: &[u8; MESSAGE_LEN]) -> [u8; CODEWORD_LEN] {
    let mut out = [0u8; CODEWORD_LEN];
    // Message bits 0..91 occupy codeword bits 0..91.
    out[..MESSAGE_LEN].copy_from_slice(message);
    out[MESSAGE_LEN - 1] &= 0xE0; // keep the 5 spare bits clean
    for (i, row) in GENERATOR_ROWS.iter().enumerate() {
        let mut parity = 0u8;
        for (a, b) in row.iter().zip(message.iter()) {
            parity ^= a & b;
        }
        let bit = (parity.count_ones() & 1) as u8;
        let pos = MESSAGE_BITS + i;
        out[pos / 8] |= bit << (7 - pos % 8);
    }
    out
}

/// Verifies a codeword against the systematic-form parity-check matrix
/// `H = [G | I₈₃]`, returning the number of failed checks (0 for every
/// valid codeword).
///
/// Honesty note: this `H` is **derived from the embedded generator**
/// (each check row is one generator row concatenated with the matching
/// identity column), so it proves the encoder computes exactly the
/// embedded matrix — every codeword satisfies all 83 checks and any
/// single-bit corruption fails at least one — but it is not an
/// independent transcription of the sparse spec `H`. The RX slice's
/// min-sum decoder carries the sparse form in [`CHECK_ROWS`], which
/// *is* independently pinned: it matches the public-domain `parity.dat`
/// as well as being derivable from this generator (see its own
/// provenance section). What neither form has yet been exercised
/// against is independently produced *signals* — only this crate's own
/// transmissions.
#[must_use]
pub fn ldpc_check(codeword: &[u8; CODEWORD_LEN]) -> u32 {
    let mut failed = 0u32;
    for (i, row) in GENERATOR_ROWS.iter().enumerate() {
        let mut parity = 0u8;
        for (a, b) in row.iter().zip(codeword.iter()) {
            parity ^= a & b;
        }
        let mut sum = parity.count_ones() & 1;
        let pos = MESSAGE_BITS + i;
        sum ^= u32::from((codeword[pos / 8] >> (7 - pos % 8)) & 1);
        failed += sum;
    }
    failed
}

/// Maps a 174-bit codeword to the 79 channel symbols: 58 Gray-mapped
/// 3-bit data tones ([`GRAY_MAP`], bits MSB-first) at positions 7–35
/// and 43–71, with the Costas array [`COSTAS`] at 0–6, 36–42 and
/// 72–78.
#[must_use]
pub fn symbols_from_codeword(codeword: &[u8; CODEWORD_LEN]) -> [u8; SYMBOL_COUNT] {
    let mut symbols = [0u8; SYMBOL_COUNT];
    symbols[0..7].copy_from_slice(&COSTAS);
    symbols[36..43].copy_from_slice(&COSTAS);
    symbols[72..79].copy_from_slice(&COSTAS);
    for j in 0..58 {
        let mut bits = 0u8;
        for b in 0..3 {
            let pos = 3 * j + b;
            bits = (bits << 1) | ((codeword[pos / 8] >> (7 - pos % 8)) & 1);
        }
        let tone = GRAY_MAP[usize::from(bits)];
        let position = if j < 29 { 7 + j } else { 43 + (j - 29) };
        symbols[position] = tone;
    }
    symbols
}

/// The GFSK frequency pulse at BT = [`GFSK_BT`], `t` in symbol periods
/// measured from the pulse center:
/// `pulse(t) = ½·(erf(K·BT·(t+½)) − erf(K·BT·(t−½)))`,
/// `K = π·√(2/ln 2)`. Support is effectively `|t| < 1.5` (three symbol
/// periods); the pulses of consecutive symbols sum to 1 at every
/// instant, so the instantaneous frequency always stays inside the
/// tone span.
#[must_use]
pub fn gfsk_pulse(t: f64) -> f64 {
    let c = GFSK_K * GFSK_BT;
    0.5 * (erf(c * (t + 0.5)) - erf(c * (t - 0.5)))
}

/// `exp(x)` for `x <= 0` via range reduction and a Taylor polynomial —
/// core-only math for `no_std` (std's `f64::exp` is unavailable).
/// Absolute error well below 1e-15 on the domain used here.
fn exp_neg(x: f64) -> f64 {
    debug_assert!(x <= 0.0);
    if x < -700.0 {
        return 0.0;
    }
    const LN2: f64 = core::f64::consts::LN_2;
    // x = k·ln2 + r with k = round(x/ln2), so |r| <= ln2/2 ≈ 0.347.
    // x <= 0, so round via truncation of the nonnegative -x/ln2.
    let k = -(((-x) / LN2 + 0.5) as i64);
    let r = x - (k as f64) * LN2;
    // Taylor: sum r^n / n!, n = 0..=13 (error < 0.35¹⁴/14! ≈ 4e-18).
    let mut term = 1.0;
    let mut sum = 1.0;
    for n in 1..=13 {
        term *= r / f64::from(n);
        sum += term;
    }
    // 2^k by exponent-field construction (k ∈ [-1011, 0] here, so the
    // biased exponent stays in the normal range).
    let two_k = f64::from_bits(((k + 1023) as u64) << 52);
    sum * two_k
}

/// `erf(x)` via the Abramowitz–Stegun 7.1.26 rational approximation
/// (max absolute error ≈ 1.5e-7 — far below audio quantization).
fn erf(x: f64) -> f64 {
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = if x < 0.0 { -x } else { x };
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let poly = t
        * (0.254_829_592
            + t * (-0.284_496_736
                + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));
    sign * (1.0 - poly * exp_neg(-x * x))
}

/// Validated FT8 audio parameters: base tone frequency and sample rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ft8Config {
    /// The symbol-0 tone frequency in Hz.
    base_hz: u32,
    /// Output sample rate; must be a multiple of 25 Hz.
    sample_rate: SampleRate,
}

impl Ft8Config {
    /// Creates a configuration for the given base audio frequency (the
    /// tone-0 frequency, conventionally somewhere in 200–3000 Hz) and
    /// sample rate.
    ///
    /// # Errors
    ///
    /// [`Ft8Error::SampleRateInexact`] when the rate is not a multiple
    /// of 25 Hz (a 0.16 s symbol must span a whole number of samples);
    /// [`Ft8Error::ToneOutOfRange`] when the base frequency is zero or
    /// the highest tone (base + 7 × 6.25 Hz) reaches Nyquist.
    pub const fn new(base_hz: u32, sample_rate: SampleRate) -> Result<Self, Ft8Error> {
        let sr = sample_rate.hz();
        if !sr.is_multiple_of(25) {
            return Err(Ft8Error::SampleRateInexact { got: sr });
        }
        // Highest tone: base + 7 × 6.25 Hz; require < sr / 2.
        // Scaled by 4: base·4 + 175 < sr·2.
        if base_hz == 0 || (base_hz as u64) * 4 + 175 >= (sr as u64) * 2 {
            return Err(Ft8Error::ToneOutOfRange {
                base_hz,
                sample_rate: sr,
            });
        }
        Ok(Self {
            base_hz,
            sample_rate,
        })
    }

    /// The base audio frequency (tone 0) in Hz.
    #[must_use]
    pub const fn base_hz(self) -> u32 {
        self.base_hz
    }

    /// The configured sample rate.
    #[must_use]
    pub const fn sample_rate(self) -> SampleRate {
        self.sample_rate
    }

    /// Samples per channel symbol at this rate (exact by
    /// construction): `rate × 4 / 25` — 1920 at the canonical 12 kHz
    /// (0.16 s).
    #[must_use]
    pub const fn samples_per_symbol(self) -> u32 {
        self.sample_rate.hz() / 25 * 4
    }
}

/// Streaming continuous-phase GFSK-shaped 8-FSK generator for one FT8
/// transmission.
///
/// Mirrors [`WsprModulator`](crate::wspr::WsprModulator): a single
/// `u32` phase accumulator (full range = one cycle) advanced once per
/// sample and **never reset**, so the waveform is exactly phase
/// continuous. The per-sample phase increment follows the GFSK
/// frequency trajectory (BT = [`GFSK_BT`]; see [`gfsk_pulse`] and the
/// module docs), so tone transitions are smoothed across symbol
/// boundaries as published. Owns no buffers beyond the 79 symbols;
/// allocation-free. Each sample evaluates the Gaussian pulse in `f64`
/// (six erf calls) — trivial on a host, soft-float work on a bare-metal
/// MCU (documented cost, not hidden).
///
/// Pull samples with [`Ft8Modulator::next_i16`] /
/// [`Ft8Modulator::next_f32`], fill caller buffers with
/// [`Ft8Modulator::fill_i16`] / [`Ft8Modulator::fill_f32`], or use the
/// `Iterator<Item = i16>` implementation. The i16 and f32 paths share
/// the one phase accumulator; use one modulator per transmission.
#[derive(Debug, Clone)]
pub struct Ft8Modulator {
    /// Phase accumulator; full u32 range == one waveform cycle.
    phase: u32,
    /// The 79 channel symbols.
    symbols: [u8; SYMBOL_COUNT],
    /// Index of the symbol currently sounding.
    symbol_idx: usize,
    /// Samples already emitted for the current symbol.
    emitted_in_symbol: u32,
    /// Samples per symbol at the configured rate.
    samples_per_symbol: u32,
    /// Base (tone 0) frequency in Hz.
    base_hz: f64,
    /// `2³² / sample_rate`: Hz → per-sample phase increment.
    phase_per_hz: f64,
}

impl Ft8Modulator {
    /// Creates a generator for the given channel symbols (see
    /// [`Ft8Message::channel_symbols`]).
    #[must_use]
    pub fn new(config: Ft8Config, symbols: [u8; SYMBOL_COUNT]) -> Self {
        Self {
            phase: 0,
            symbols,
            symbol_idx: 0,
            emitted_in_symbol: 0,
            samples_per_symbol: config.samples_per_symbol(),
            base_hz: f64::from(config.base_hz()),
            phase_per_hz: 4_294_967_296.0 / f64::from(config.sample_rate().hz()),
        }
    }

    /// Creates a generator directly from a validated message.
    #[must_use]
    pub fn for_message(config: Ft8Config, message: &Ft8Message) -> Self {
        Self::new(config, message.channel_symbols())
    }

    /// Total samples this transmission spans: `79 × samples_per_symbol`
    /// (151 680 at 12 kHz ≈ 12.64 s, inside the 15 s cycle).
    #[must_use]
    pub fn total_samples(&self) -> u64 {
        SYMBOL_COUNT as u64 * u64::from(self.samples_per_symbol)
    }

    /// The GFSK-smoothed instantaneous frequency (Hz) of the sample
    /// about to be emitted.
    fn current_hz(&self) -> f64 {
        let i = self.symbol_idx;
        let cur = f64::from(self.symbols[i]);
        // Virtually extend the first/last symbols so the frequency
        // ramps from/to the edge tones (the standard edge treatment).
        let prev = if i == 0 {
            cur
        } else {
            f64::from(self.symbols[i - 1])
        };
        let next = if i + 1 == SYMBOL_COUNT {
            cur
        } else {
            f64::from(self.symbols[i + 1])
        };
        // Sample center within the symbol, in symbol periods, relative
        // to the symbol center: t ∈ (-0.5, 0.5).
        let t =
            (f64::from(self.emitted_in_symbol) + 0.5) / f64::from(self.samples_per_symbol) - 0.5;
        let blend = prev * gfsk_pulse(t + 1.0) + cur * gfsk_pulse(t) + next * gfsk_pulse(t - 1.0);
        self.base_hz + 6.25 * blend
    }

    /// Advances the phase accumulator past the sample just emitted.
    fn advance(&mut self) {
        let inc = (self.current_hz() * self.phase_per_hz + 0.5) as u32;
        self.phase = self.phase.wrapping_add(inc);
        self.emitted_in_symbol += 1;
        if self.emitted_in_symbol == self.samples_per_symbol {
            self.emitted_in_symbol = 0;
            self.symbol_idx += 1;
        }
    }

    /// Pulls the next i16 PCM sample, or `None` when the transmission
    /// is complete.
    pub fn next_i16(&mut self) -> Option<i16> {
        if self.symbol_idx >= SYMBOL_COUNT {
            return None;
        }
        let sample = sine_at(self.phase);
        self.advance();
        Some(sample)
    }

    /// Pulls the next f32 PCM sample (nominal range `-1.0..=1.0`), or
    /// `None` when the transmission is complete.
    pub fn next_f32(&mut self) -> Option<f32> {
        if self.symbol_idx >= SYMBOL_COUNT {
            return None;
        }
        let sample = sine_at_f32(self.phase);
        self.advance();
        Some(sample)
    }

    /// Fills `buf` with as many i16 samples as remain, returning the
    /// count written (less than `buf.len()` only at the end).
    pub fn fill_i16(&mut self, buf: &mut [i16]) -> usize {
        let mut written = 0;
        for slot in buf.iter_mut() {
            match self.next_i16() {
                Some(s) => {
                    *slot = s;
                    written += 1;
                }
                None => break,
            }
        }
        written
    }

    /// Fills `buf` with as many f32 samples as remain, returning the
    /// count written.
    pub fn fill_f32(&mut self, buf: &mut [f32]) -> usize {
        let mut written = 0;
        for slot in buf.iter_mut() {
            match self.next_f32() {
                Some(s) => {
                    *slot = s;
                    written += 1;
                }
                None => break,
            }
        }
        written
    }
}

impl Iterator for Ft8Modulator {
    type Item = i16;

    fn next(&mut self) -> Option<i16> {
        self.next_i16()
    }
}

/// Looks up the f32 sine of a 32-bit phase with linear interpolation
/// over the crate's shared sine table (a local twin of the modulator's
/// f32 path, so `ft8` alone does not need the `mod` feature).
fn sine_at_f32(phase: u32) -> f32 {
    use crate::types::{SINE_I16, TABLE_BITS, TABLE_MASK};
    let idx = (phase >> (32 - TABLE_BITS)) as usize & TABLE_MASK;
    let frac_bits = phase & ((1 << (32 - TABLE_BITS)) - 1);
    let frac = frac_bits as f32 / (1u32 << (32 - TABLE_BITS)) as f32;
    let a = SINE_I16.get(idx).copied().unwrap_or(0) as f32;
    let b = SINE_I16.get((idx + 1) & TABLE_MASK).copied().unwrap_or(0) as f32;
    (a + (b - a) * frac) / 32_767.0
}
