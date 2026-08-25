//! WSPR (Weak Signal Propagation Reporter) transmit beacon and
//! receive engine.
//!
//! # What WSPR is
//!
//! WSPR is a beacon mode for probing radio propagation paths: a station
//! transmits its callsign, 4-character Maidenhead grid locator and power
//! level in a ~110.6 s burst of 4-tone continuous-phase FSK with tones
//! only 12000/8192 ≈ 1.4648 Hz apart. Heavy forward error correction
//! (a rate-1/2, constraint-length-32 convolutional code) plus the very
//! long symbols let the *reference implementation's decoder* copy
//! signals down to roughly −31 dB SNR in a 2500 Hz bandwidth. That
//! sensitivity claim belongs to that decoder — this module's own
//! [`WsprDecoder`] is a simpler single-pass engine whose measured
//! sensitivity is pinned by the test suite (see `tests/wspr_rx.rs`):
//! it decodes our own transmissions down to **−22 dB** SNR in the same
//! 2500 Hz reference bandwidth, and fails cleanly well below that.
//!
//! Implemented from the **published description of the WSPR coding
//! process** (G4JNT, "The WSPR Coding Process") and the publicly
//! documented WSPR protocol parameters. Where a constant was
//! transcribed from the published description rather than derived, its
//! provenance is noted on the item so a correction is a one-line change.
//!
//! # The TX pipeline
//!
//! 1. **Source encoding** ([`WsprMessage`]): a type-1 standard message
//!    (callsign + 4-char grid + power in dBm) packs into 50 bits —
//!    28 for the callsign, 15 for the grid, 7 for the power — stored
//!    left-justified in 11 bytes ([`WsprMessage::pack`]).
//! 2. **Convolutional encoding** ([`convolutional_encode`]): the 50
//!    data bits plus 31 zero tail bits (81 in total, MSB-first from the
//!    packed bytes) are shifted through a K=32, rate-1/2 encoder with
//!    polynomials [`POLY_A`]/[`POLY_B`], producing 162 coded bits.
//! 3. **Interleaving** ([`interleave`]): coded bit `k` lands at the
//!    k-th bit-reversed-index position below 162.
//! 4. **Sync merge** ([`WsprMessage::channel_symbols`]): symbol
//!    `i = SYNC_VECTOR[i] + 2 * data[i]`, a 4-FSK symbol in `0..=3`.
//! 5. **Audio synthesis** ([`WsprModulator`]): each symbol keys the
//!    tone `base + symbol × 12000/8192 Hz` for 8192 samples at 12 kHz
//!    (0.6827 s; scaled exactly at other supported rates), through the
//!    same never-reset `u32` phase-accumulator scheme as the crate's
//!    AFSK [`Modulator`](crate::Modulator) — the waveform is phase
//!    continuous across symbol boundaries.
//!
//! # Example
//!
//! ```
//! use yodel::{MaidenheadGrid, SampleRate};
//! use yodel::wspr::{WsprConfig, WsprMessage, WsprModulator};
//!
//! let msg = WsprMessage::new("K1ABC", MaidenheadGrid::new("FN42")?, 37)?;
//! let symbols = msg.channel_symbols();
//! assert_eq!(symbols.len(), 162);
//! assert!(symbols.iter().all(|&s| s <= 3));
//!
//! let config = WsprConfig::new(1_500, SampleRate::new(12_000)?)?;
//! let mut tx = WsprModulator::new(config, symbols);
//! assert_eq!(tx.total_samples(), 162 * 8_192);
//! let first: i16 = tx.next_i16().unwrap(); // phase starts at 0 → sin(0)
//! assert_eq!(first, 0);
//! # Ok::<(), Box<dyn core::error::Error>>(())
//! ```
//!
//! # Bit-order contract (documented exactly)
//!
//! * Packed bytes: bit 27 (MSB) of the 28-bit callsign value is bit 7
//!   of byte 0; the 22-bit grid+power value follows immediately, so its
//!   MSB is bit 3 of byte 3. Bits 50..88 of the 11 bytes are zero.
//! * Encoder input: the 81 bits are read MSB-first from the packed
//!   bytes (byte 0 bit 7 first). Each input bit is shifted into the
//!   **LSB** of the 32-bit register (`reg = reg << 1 | bit`).
//! * Encoder output: for every input bit, the [`POLY_A`] parity is
//!   emitted first, then the [`POLY_B`] parity — coded bits `2k` and
//!   `2k + 1`.
//!
//! # The RX pipeline and the no_std / std split
//!
//! Receive is split along the memory boundary:
//!
//! * **no_std, always compiled with `wspr`** — the pure-math pieces
//!   that own no buffers: [`deinterleave`] (the inverse permutation),
//!   [`fano_decode`] (the K=32 sequential decoder over caller-supplied
//!   per-bit metrics, hard-capped at [`FANO_NODE_CAP`] node visits by
//!   default) and [`WsprMessage::unpack`] (the inverse of
//!   [`WsprMessage::pack`]).
//! * **std only (`wspr` + `std`)** — [`WsprDecoder`], the buffered
//!   engine for a whole ~114 s capture: 12 kHz i16 → complex 375 Hz
//!   baseband (mixer + two cascaded FIR decimators), FFT candidate
//!   search on the 1.4648 Hz grid, sync-vector time/frequency
//!   alignment, per-symbol 4-bin DFT soft demod, then the no_std
//!   pieces above. RAM, measured: the decimated capture that survives
//!   decoding is complex f32 — 114 s × 375 Hz × 8 bytes ≈ 342 KB —
//!   but the **measured peak is 15 574 696 B ≈ 14.85 MiB** for a full
//!   114 s / 12 kHz capture, because the mixer and both decimation
//!   stages are alive simultaneously: a padded `Vec<i16>` copy of the
//!   capture (2 768 768 B), the mixed complex-f32 signal at the input
//!   rate (11 075 072 B), the 1500 Hz stage-1 output (1 384 384 B) and
//!   the 375 Hz stage-2 output (346 096 B). i16 pairs would halve the
//!   float buffers, but f32 keeps the FFT and DFT numerics simple and
//!   this path is std-only by design — a peak of that size is exactly
//!   why [`WsprDecoder`] is gated behind `std` and stays out of the
//!   embedded build. The metric mapping (log-sigmoid) also uses std
//!   float transcendentals, which is why the Fano *search* takes
//!   pre-computed metrics instead of raw LLRs.

use core::fmt;

use crate::geo::{GridPrecision, MaidenheadGrid};
use crate::types::{SampleRate, sine_at};

#[cfg(feature = "std")]
mod rx;
#[cfg(feature = "std")]
pub use rx::{WsprDecode, WsprDecoder, WsprDecoderConfig, WsprRxError};

/// Looks up the f32 sine of a 32-bit phase with linear interpolation
/// over the crate's shared sine table (a local twin of the modulator's
/// f32 path, so `wspr` alone does not need the `mod` feature).
fn sine_at_f32(phase: u32) -> f32 {
    use crate::types::{SINE_I16, TABLE_BITS, TABLE_MASK};
    let idx = (phase >> (32 - TABLE_BITS)) as usize & TABLE_MASK;
    let frac_bits = phase & ((1 << (32 - TABLE_BITS)) - 1);
    let frac = frac_bits as f32 / (1u32 << (32 - TABLE_BITS)) as f32;
    let a = SINE_I16.get(idx).copied().unwrap_or(0) as f32;
    let b = SINE_I16.get((idx + 1) & TABLE_MASK).copied().unwrap_or(0) as f32;
    (a + (b - a) * frac) / 32_767.0
}

/// Number of channel symbols in a WSPR transmission.
pub const SYMBOL_COUNT: usize = 162;

/// Number of source-coded data bits (28 callsign + 15 grid + 7 power).
pub const DATA_BITS: usize = 50;

/// Number of packed source-encoding bytes (50 bits left-justified).
pub const PACKED_LEN: usize = 11;

/// First convolutional polynomial (Layland–Lushbaugh), K=32 rate 1/2.
///
/// Value as published in the WSPR coding-process description.
pub const POLY_A: u32 = 0xF2D0_5351;

/// Second convolutional polynomial (Layland–Lushbaugh), K=32 rate 1/2.
///
/// Value as published in the WSPR coding-process description.
pub const POLY_B: u32 = 0xE461_3C47;

/// Tone spacing numerator: spacing = 12000/8192 Hz ≈ 1.4648 Hz.
pub const TONE_SPACING_NUM: u32 = 12_000;

/// Tone spacing denominator: spacing = 12000/8192 Hz ≈ 1.4648 Hz.
pub const TONE_SPACING_DEN: u32 = 8_192;

/// The published 162-element pseudo-random sync vector.
///
/// Provenance: transcribed from the sync-vector table published in
/// Andy Talbot G4JNT, "The WSPR Coding Process", 2009, section "Sync
/// Vector" (the same table is reproduced across the amateur
/// literature).
///
/// **This is the one substantial table in the crate without an
/// executable provenance check.** Tests spot-check entries against
/// literals, which detects a later edit but cannot detect a
/// transcription that was wrong from the start. The shipped RX slice
/// closes the loop end to end, but only over this crate's own
/// transmissions, so this vector (like [`POLY_A`] and [`POLY_B`]) is
/// verified self-consistent rather than against independently produced
/// signals. The only independent check is the `#[ignore]`d differential
/// leg in `tests/wspr_differential.rs`, which needs an external
/// encoder. Channel symbol `i` is `SYNC_VECTOR[i] + 2 * data[i]`.
#[rustfmt::skip]
pub const SYNC_VECTOR: [u8; SYMBOL_COUNT] = [
    1, 1, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 1, 1, 1, 0, 0, 0, 1, 0, 0, 1, 0, 1, 1, 1, 1, 0, 0, 0, //
    0, 0, 0, 0, 1, 0, 0, 1, 0, 1, 0, 0, 0, 0, 0, 0, 1, 0, 1, 1, 0, 0, 1, 1, 0, 1, 0, 0, 0, 1, //
    1, 0, 1, 0, 0, 0, 0, 1, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 0, 1, 0, 0, 1, 0, 1, 1, 0, 0, 0, 1, //
    1, 0, 1, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 1, 0, 0, 1, 0, 0, 1, 1, 1, 0, 1, 1, 0, 0, 1, 1, //
    0, 1, 0, 0, 0, 1, 1, 1, 0, 0, 0, 0, 0, 1, 0, 1, 0, 0, 1, 1, 0, 0, 0, 0, 0, 0, 0, 1, 1, 0, //
    1, 0, 1, 1, 0, 0, 0, 1, 1, 0, 0, 0,
];

/// Errors from WSPR message or configuration validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum WsprError {
    /// The callsign contains `/`: compound/suffixed calls (portable,
    /// rover, …) cannot be carried by a type-1 standard message.
    CallsignCompound,
    /// The callsign is empty or longer than six characters.
    CallsignLength {
        /// The rejected length in characters.
        len: usize,
    },
    /// A callsign character is outside the allowed set for its position
    /// (positions 0–2: letters/digits, digit required third; positions
    /// 3–5: letters only).
    CallsignChar {
        /// The rejected character (upper-cased).
        ch: char,
        /// Its zero-based position in the space-aligned 6-char form.
        index: usize,
    },
    /// The callsign cannot be aligned so its third character is a
    /// digit (the type-1 shape rule).
    CallsignShape,
    /// The locator is finer than the square a type-1 message can
    /// carry: the 15-bit grid field holds two field letters and two
    /// square digits and nothing else, so a 6- or 8-character
    /// [`MaidenheadGrid`] has no room for its subsquare and is
    /// rejected rather than silently truncated.
    GridLength {
        /// The rejected locator length in characters (6 or 8).
        len: usize,
    },
    /// The power is outside `0..=60` dBm.
    PowerOutOfRange {
        /// The rejected power in dBm.
        got: u8,
    },
    /// The power does not end in 0, 3 or 7 — the only values the
    /// published protocol tables use (…, 27, 30, 33, 37, 40, …).
    PowerNotStandard {
        /// The rejected power in dBm.
        got: u8,
    },
    /// A 50-bit payload handed to [`WsprMessage::unpack`] does not
    /// decode to a valid type-1 standard message (callsign, grid or
    /// power field value out of range — e.g. a wrong Fano codeword).
    UnpackInvalid,
    /// The sample rate is not an exact multiple of 375 Hz, so a symbol
    /// (8192 samples at 12 kHz = 256/375 s × rate) would not span a
    /// whole number of samples.
    SampleRateInexact {
        /// The rejected sample rate in Hz.
        got: u32,
    },
    /// The highest tone (base + 3 × 12000/8192 Hz) would reach or
    /// exceed the Nyquist frequency, or the base frequency is zero.
    ToneOutOfRange {
        /// The requested base audio frequency in Hz.
        base_hz: u32,
        /// The configured sample rate in Hz.
        sample_rate: u32,
    },
}

impl fmt::Display for WsprError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::CallsignCompound => write!(
                f,
                "callsign contains '/': compound calls do not fit a type-1 WSPR message"
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
                "callsign cannot be aligned to the type-1 shape (third character must be a digit)"
            ),
            Self::GridLength { len } => write!(
                f,
                "grid locator length {len} is invalid: must be exactly 4 characters"
            ),
            Self::PowerOutOfRange { got } => {
                write!(f, "power {got} dBm is out of range: must be within 0..=60")
            }
            Self::PowerNotStandard { got } => write!(
                f,
                "power {got} dBm is not a standard WSPR value: must end in 0, 3 or 7"
            ),
            Self::UnpackInvalid => write!(
                f,
                "packed 50-bit payload does not decode to a valid type-1 message"
            ),
            Self::SampleRateInexact { got } => write!(
                f,
                "sample rate {got} Hz cannot time WSPR symbols exactly: must be a multiple of 375 Hz"
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

impl core::error::Error for WsprError {}

/// A validated type-1 standard WSPR message: callsign, 4-character
/// Maidenhead grid, power in dBm.
///
/// Construction via [`WsprMessage::new`] validates every field, so a
/// value of this type always source-encodes successfully — illegal
/// messages are unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WsprMessage {
    /// Space-aligned 6-character callsign (third character a digit).
    callsign: [u8; 6],
    /// Square-precision locator: [`WsprMessage::new`] rejects finer
    /// ones, so this always renders as exactly four characters.
    grid: MaidenheadGrid,
    /// Power in dBm, `0..=60`, ending in 0/3/7.
    power_dbm: u8,
}

impl WsprMessage {
    /// Validates and normalizes a type-1 standard message.
    ///
    /// The callsign is upper-cased and space-aligned so its third
    /// character is a digit (`"G4JNT"` → `" G4JNT"`, `"K1ABC"` →
    /// `" K1ABC"`, `"KA1ABC"` stays). The locator arrives already
    /// parsed and canonicalized as a [`MaidenheadGrid`] and must be
    /// [`GridPrecision::Square`] — the type also makes the callsign and
    /// grid arguments impossible to transpose. Power must be `0..=60`
    /// dBm and end in 0, 3 or 7 (the values the published protocol
    /// uses).
    ///
    /// ```
    /// use yodel::MaidenheadGrid;
    /// use yodel::wspr::{WsprError, WsprMessage};
    ///
    /// let msg = WsprMessage::new("K1ABC", MaidenheadGrid::new("fn42")?, 37)?;
    /// assert_eq!(msg.grid().as_str(), "FN42");
    ///
    /// // A subsquare does not fit the 15-bit grid field.
    /// assert_eq!(
    ///     WsprMessage::new("K1ABC", MaidenheadGrid::new("FN42ab")?, 37),
    ///     Err(WsprError::GridLength { len: 6 })
    /// );
    /// # Ok::<(), Box<dyn core::error::Error>>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns the specific [`WsprError`] variant describing the
    /// rejected field: compound callsigns (`/`), bad lengths, invalid
    /// characters, unalignable shapes, locators finer than a square,
    /// and out-of-range or nonstandard power values.
    pub fn new(callsign: &str, grid: MaidenheadGrid, power_dbm: u8) -> Result<Self, WsprError> {
        let callsign = normalize_callsign(callsign)?;
        if !matches!(grid.precision(), GridPrecision::Square) {
            return Err(WsprError::GridLength {
                len: grid.precision().characters(),
            });
        }
        if power_dbm > 60 {
            return Err(WsprError::PowerOutOfRange { got: power_dbm });
        }
        if !matches!(power_dbm % 10, 0 | 3 | 7) {
            return Err(WsprError::PowerNotStandard { got: power_dbm });
        }
        Ok(Self {
            callsign,
            grid,
            power_dbm,
        })
    }

    /// The space-aligned 6-character callsign.
    #[must_use]
    pub fn callsign(&self) -> &[u8; 6] {
        &self.callsign
    }

    /// The Maidenhead locator, always [`GridPrecision::Square`] (four
    /// characters). Use [`MaidenheadGrid::as_bytes`] for the wire
    /// characters or [`MaidenheadGrid::center`] for coordinates.
    #[must_use]
    pub fn grid(&self) -> MaidenheadGrid {
        self.grid
    }

    /// The power in dBm.
    #[must_use]
    pub fn power_dbm(&self) -> u8 {
        self.power_dbm
    }

    /// Source-encodes the message into 50 bits, left-justified in 11
    /// bytes (the convolutional encoder's input; bits 50..88 are zero).
    ///
    /// Layout: the 28-bit callsign value `N` occupies bits 0..28
    /// (byte 0 bit 7 = bit 27 of `N`), followed by the 22-bit value
    /// `M * 128 + power + 64` where `M` is the 15-bit packed grid.
    #[must_use]
    pub fn pack(&self) -> [u8; PACKED_LEN] {
        let n = pack_callsign(&self.callsign);
        let m = pack_grid(self.grid) * 128 + u32::from(self.power_dbm) + 64;
        let mut bytes = [0u8; PACKED_LEN];
        // 28 bits of n, then 22 bits of m, MSB-first: a 50-bit value.
        let bits: u64 = (u64::from(n) << 22) | u64::from(m);
        let left = bits << (64 - 50); // left-justify in a u64
        for (i, byte) in bytes.iter_mut().enumerate().take(7) {
            *byte = (left >> (56 - 8 * i)) as u8;
        }
        bytes
    }

    /// The full 162-element channel-symbol sequence (4-FSK, `0..=3`):
    /// source encoding, convolutional encoding, interleaving, and the
    /// sync-vector merge `symbol = sync + 2 * data`.
    #[must_use]
    pub fn channel_symbols(&self) -> [u8; SYMBOL_COUNT] {
        let coded = convolutional_encode(&self.pack());
        let data = interleave(&coded);
        let mut symbols = [0u8; SYMBOL_COUNT];
        for i in 0..SYMBOL_COUNT {
            symbols[i] = SYNC_VECTOR[i] + 2 * data[i];
        }
        symbols
    }

    /// Inverts [`WsprMessage::pack`]: reconstructs the message from the
    /// 50-bit source encoding (e.g. the output of [`fano_decode`]).
    ///
    /// no_std, buffer-free — part of the RX math that works without
    /// the `std`-gated engine.
    ///
    /// # Errors
    ///
    /// [`WsprError::UnpackInvalid`] when the payload is not a valid
    /// type-1 standard message: nonzero bits past bit 50, a callsign
    /// value above the packing range or with characters/shape the
    /// encoder could never produce, a grid value ≥ 32400, or a power
    /// field outside the standard 0..=60 dBm ending in 0/3/7. The
    /// grid *value* bound is the live check; the four reconstructed
    /// characters are then handed to [`MaidenheadGrid::from_bytes`],
    /// which cannot fail here (`g < 32400` already forces both field
    /// letters into `A..=R`) but keeps the locator's own invariant the
    /// single authority on what a locator is.
    pub fn unpack(packed: &[u8; PACKED_LEN]) -> Result<Self, WsprError> {
        if packed[7..].iter().any(|&b| b != 0) {
            return Err(WsprError::UnpackInvalid);
        }
        let mut left: u64 = 0;
        for (i, &byte) in packed.iter().enumerate().take(7) {
            left |= u64::from(byte) << (56 - 8 * i);
        }
        if left & ((1u64 << 14) - 1) != 0 {
            return Err(WsprError::UnpackInvalid);
        }
        let bits = left >> 14;
        let n = (bits >> 22) as u32;
        let m = (bits & 0x3F_FFFF) as u32;

        // Callsign: undo the mixed-radix packing of `pack_callsign`.
        fn tail_char(v: u32) -> Result<u8, WsprError> {
            match v {
                0..=25 => Ok(b'A' + v as u8),
                26 => Ok(b' '),
                _ => Err(WsprError::UnpackInvalid),
            }
        }
        fn head_char(v: u32) -> Result<u8, WsprError> {
            match v {
                0..=9 => Ok(b'0' + v as u8),
                10..=35 => Ok(b'A' + (v - 10) as u8),
                36 => Ok(b' '),
                _ => Err(WsprError::UnpackInvalid),
            }
        }
        let mut v = n;
        let c6 = tail_char(v % 27)?;
        v /= 27;
        let c5 = tail_char(v % 27)?;
        v /= 27;
        let c4 = tail_char(v % 27)?;
        v /= 27;
        let c3 = b'0' + (v % 10) as u8;
        v /= 10;
        let c2 = head_char(v % 36)?;
        v /= 36;
        let c1 = head_char(v)?;
        let aligned = [c1, c2, c3, c4, c5, c6];
        // Trim the alignment spaces and let the validating constructor
        // reject anything the encoder could not have produced (interior
        // spaces, empty calls, unalignable shapes).
        let start = aligned.iter().position(|&c| c != b' ').unwrap_or(6);
        let end = 6 - aligned.iter().rev().position(|&c| c != b' ').unwrap_or(6);
        if start >= end {
            return Err(WsprError::UnpackInvalid);
        }
        let call =
            core::str::from_utf8(&aligned[start..end]).map_err(|_| WsprError::UnpackInvalid)?;

        // Grid + power: undo `M * 128 + power + 64`.
        let power = m % 128;
        if !(64..=124).contains(&power) {
            return Err(WsprError::UnpackInvalid);
        }
        let power = (power - 64) as u8;
        let g = m / 128;
        if g >= 180 * 180 {
            return Err(WsprError::UnpackInvalid);
        }
        let lat = g % 180;
        let lon = 179 - g / 180;
        let grid = [
            b'A' + (lon / 10) as u8,
            b'A' + (lat / 10) as u8,
            b'0' + (lon % 10) as u8,
            b'0' + (lat % 10) as u8,
        ];
        let grid = MaidenheadGrid::from_bytes(&grid).map_err(|_| WsprError::UnpackInvalid)?;
        Self::new(call, grid, power).map_err(|_| WsprError::UnpackInvalid)
    }
}

/// Callsign character value: digits 0–9, letters 10–35, space 36.
fn char_value(c: u8) -> u32 {
    match c {
        b'0'..=b'9' => u32::from(c - b'0'),
        b'A'..=b'Z' => u32::from(c - b'A') + 10,
        _ => 36, // space
    }
}

/// Validates, upper-cases and space-aligns a callsign to the type-1
/// 6-character shape (third character a digit).
fn normalize_callsign(call: &str) -> Result<[u8; 6], WsprError> {
    let mut buf = [b' '; 6];
    let mut len = 0usize;
    for ch in call.chars() {
        if ch == '/' {
            return Err(WsprError::CallsignCompound);
        }
        let up = ch.to_ascii_uppercase();
        if !(up.is_ascii_uppercase() || up.is_ascii_digit()) {
            return Err(WsprError::CallsignChar { ch: up, index: len });
        }
        if len == 6 {
            return Err(WsprError::CallsignLength {
                len: call.chars().count(),
            });
        }
        buf[len] = up as u8;
        len += 1;
    }
    if len == 0 {
        return Err(WsprError::CallsignLength { len: 0 });
    }
    // Alignment rule: the third character must be a digit. Calls with
    // the digit in second position (G4JNT, K1ABC) shift right behind a
    // leading space.
    if !buf[2].is_ascii_digit() {
        if len >= 6 || !buf[1].is_ascii_digit() {
            return Err(WsprError::CallsignShape);
        }
        buf.copy_within(0..5, 1);
        buf[0] = b' ';
    }
    // Positional character classes: the last three positions are
    // letters or (trailing) space only.
    for (index, &c) in buf.iter().enumerate().skip(3) {
        if !(c.is_ascii_uppercase() || c == b' ') {
            return Err(WsprError::CallsignChar {
                ch: c as char,
                index,
            });
        }
    }
    Ok(buf)
}

/// Packs an aligned 6-character callsign into its 28-bit value:
/// `((((c1 * 36 + c2) * 10 + c3) * 27 + c4') * 27 + c5') * 27 + c6'`
/// where the last three use letter values 0–25 and space 26.
fn pack_callsign(call: &[u8; 6]) -> u32 {
    let mut n = char_value(call[0]);
    n = n * 36 + char_value(call[1]);
    n = n * 10 + char_value(call[2]);
    for &c in &call[3..] {
        n = n * 27 + (char_value(c) - 10);
    }
    n
}

/// Packs a locator into WSPR's 15-bit grid value
/// `(179 − 10·lon_field − lon_square) · 180 + 10·lat_field + lat_square`.
///
/// This is a *wire* encoding, not a coordinate conversion: only the
/// four square characters take part. [`WsprMessage::new`] admits nothing
/// finer, and every locator is at least four characters, so the seed
/// values below are never used — they exist so the byte arithmetic is
/// total (no panic, no underflow) without a fallible return.
fn pack_grid(grid: MaidenheadGrid) -> u32 {
    let mut wire = [b'A', b'A', b'0', b'0'];
    for (dst, &src) in wire.iter_mut().zip(grid.as_bytes()) {
        *dst = src;
    }
    let lon_field = u32::from(wire[0] - b'A');
    let lat_field = u32::from(wire[1] - b'A');
    let lon_square = u32::from(wire[2] - b'0');
    let lat_square = u32::from(wire[3] - b'0');
    (179 - 10 * lon_field - lon_square) * 180 + 10 * lat_field + lat_square
}

/// Convolutionally encodes the 81 input bits (50 data + 31 zero tail,
/// MSB-first from the 11 packed bytes) with the K=32 rate-1/2 code.
///
/// Each input bit shifts into the LSB of a 32-bit register; the
/// [`POLY_A`] parity is emitted first, then [`POLY_B`] — 162 coded
/// bits, each `0` or `1`.
#[must_use]
pub fn convolutional_encode(packed: &[u8; PACKED_LEN]) -> [u8; SYMBOL_COUNT] {
    let mut out = [0u8; SYMBOL_COUNT];
    let mut reg: u32 = 0;
    for k in 0..SYMBOL_COUNT / 2 {
        let bit = u32::from((packed[k / 8] >> (7 - k % 8)) & 1);
        reg = (reg << 1) | bit;
        out[2 * k] = ((reg & POLY_A).count_ones() & 1) as u8;
        out[2 * k + 1] = ((reg & POLY_B).count_ones() & 1) as u8;
    }
    out
}

/// Interleaves 162 coded bits by bit-reversed index ordering.
///
/// Walking `i` over `0..=255`, each bit-reversed value `j` below 162
/// receives the next sequential input bit: `out[reverse(i)] = in[k]`.
/// The mapping is a permutation of `0..162` (proven in tests).
#[must_use]
pub fn interleave(coded: &[u8; SYMBOL_COUNT]) -> [u8; SYMBOL_COUNT] {
    let mut out = [0u8; SYMBOL_COUNT];
    let mut k = 0usize;
    for i in 0..=255u8 {
        let j = usize::from(i.reverse_bits());
        if j < SYMBOL_COUNT {
            out[j] = coded[k];
            k += 1;
            if k == SYMBOL_COUNT {
                break;
            }
        }
    }
    out
}

/// Inverts [`interleave`]: maps 162 channel-position values back to
/// sequential coded-bit order. Generic over the element type so the
/// receive path can deinterleave soft metrics, not just hard bits.
///
/// no_std, buffer-free — part of the RX math that works without the
/// `std`-gated engine.
pub fn deinterleave<T: Copy>(channel: &[T; SYMBOL_COUNT], out: &mut [T; SYMBOL_COUNT]) {
    let mut k = 0usize;
    for i in 0..=255u8 {
        let j = usize::from(i.reverse_bits());
        if j < SYMBOL_COUNT {
            out[k] = channel[j];
            k += 1;
            if k == SYMBOL_COUNT {
                break;
            }
        }
    }
}

/// Default hard cap on Fano decoder node visits (forward, backward and
/// threshold-lowering steps all count).
///
/// The sequential decoder's work is data-dependent: on garbage input
/// it would wander the code tree indefinitely, so every visit is
/// counted and the search aborts with [`FanoError::CapExceeded`] once
/// this many have been spent. 400 000 visits bound a decode attempt to
/// a few milliseconds of integer work on a workstation while leaving
/// two orders of magnitude of headroom over a clean decode (≈ a few
/// hundred visits).
pub const FANO_NODE_CAP: u32 = 400_000;

/// Default Fano threshold spacing (Δ), in the units of the caller's
/// per-bit metrics. Matched to the scaling used by the std receive
/// engine (metric ≈ 16 × (log₂(2q) − ½) per coded bit, so a correct
/// branch is worth ≈ +16 at high SNR).
pub const FANO_DELTA: i32 = 32;

/// A sequential-decoder failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FanoError {
    /// The node-visit budget ran out before a full path was found:
    /// the metrics are too noisy (or pure garbage) for the search to
    /// make forward exceed backward motion.
    CapExceeded {
        /// The visit budget that was exhausted.
        cap: u32,
    },
}

impl fmt::Display for FanoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::CapExceeded { cap } => write!(
                f,
                "Fano search exhausted its node-visit cap ({cap}): input too noisy to decode"
            ),
        }
    }
}

impl core::error::Error for FanoError {}

/// Fano sequential decoder for the K=32 rate-1/2 WSPR code.
///
/// Takes per-coded-bit soft metrics in **sequential (deinterleaved)**
/// order: `metrics[i][b]` is the reward for hypothesizing that coded
/// bit `i` equals `b`. The caller must bias the metrics so a correct
/// path drifts upward and a wrong path drifts downward — the classic
/// choice is `scale × (log₂(2·qᵇ) − R)` with `R = ½` and `qᵇ` the
/// per-bit probability, which the std engine uses with `scale = 16`
/// (making [`FANO_DELTA`] the matching threshold spacing).
///
/// This implements the textbook Fano algorithm (threshold `T`, spacing
/// `delta`, move-forward / move-back / lower-threshold rules with
/// first-visit tightening). It owns only
/// stack buffers (≈ 1 KB) and is fully no_std; every step counts
/// against `node_cap` (see [`FANO_NODE_CAP`]) so runtime is hard
/// bounded.
///
/// On success returns the 50 data bits re-packed into 11 bytes — feed
/// them to [`WsprMessage::unpack`].
///
/// # Errors
///
/// [`FanoError::CapExceeded`] when `node_cap` visits were spent
/// without reaching the end of the tree.
pub fn fano_decode(
    metrics: &[[i32; 2]; SYMBOL_COUNT],
    delta: i32,
    node_cap: u32,
) -> Result<[u8; PACKED_LEN], FanoError> {
    const DEPTH: usize = SYMBOL_COUNT / 2; // 81 input bits
    // Branch metric for input bit `u` at depth `k` with register `reg`.
    let branch = |reg: u32, k: usize, u: u32| -> i32 {
        let r = (reg << 1) | u;
        let c0 = ((r & POLY_A).count_ones() & 1) as usize;
        let c1 = ((r & POLY_B).count_ones() & 1) as usize;
        metrics[2 * k][c0] + metrics[2 * k + 1][c1]
    };
    let mut bits = [0u8; DEPTH]; // chosen input bit per depth
    let mut tried = [0u8; DEPTH]; // branch rank taken (0 = best)
    let mut reg = [0u32; DEPTH + 1]; // encoder register entering depth k
    let mut cum = [0i32; DEPTH + 1]; // cumulative metric at depth k
    let mut k = 0usize;
    let mut t: i32 = 0;
    let mut visits: u32 = 0;
    loop {
        visits += 1;
        if visits > node_cap {
            return Err(FanoError::CapExceeded { cap: node_cap });
        }
        // The branch at the current rank, best-first ordering. Tail
        // depths (k >= DATA_BITS) have a single all-zero branch.
        let candidate = if k >= DATA_BITS {
            (tried[k] == 0).then(|| (0u32, branch(reg[k], k, 0)))
        } else {
            let m0 = branch(reg[k], k, 0);
            let m1 = branch(reg[k], k, 1);
            let (best, worst) = if m1 > m0 {
                ((1u32, m1), (0u32, m0))
            } else {
                ((0u32, m0), (1u32, m1))
            };
            match tried[k] {
                0 => Some(best),
                1 => Some(worst),
                _ => None,
            }
        };
        if let Some((u, m)) = candidate
            && cum[k] + m >= t
        {
            // Move forward.
            let new_m = cum[k] + m;
            bits[k] = u as u8;
            reg[k + 1] = (reg[k] << 1) | u;
            cum[k + 1] = new_m;
            // First visit at this threshold: tighten.
            if cum[k] < t + delta {
                while new_m >= t + delta {
                    t += delta;
                }
            }
            k += 1;
            if k == DEPTH {
                let mut packed = [0u8; PACKED_LEN];
                for (i, &bit) in bits.iter().enumerate().take(DATA_BITS) {
                    packed[i / 8] |= bit << (7 - i % 8);
                }
                return Ok(packed);
            }
            tried[k] = 0;
            continue;
        }
        // Cannot move forward at this rank: look back.
        loop {
            visits += 1;
            if visits > node_cap {
                return Err(FanoError::CapExceeded { cap: node_cap });
            }
            let prev = if k == 0 { i32::MIN } else { cum[k - 1] };
            if prev >= t {
                // Move back; advance to the next-worse branch there if
                // one remains, else keep backing up.
                k -= 1;
                if tried[k] == 0 && k < DATA_BITS {
                    tried[k] = 1;
                    break;
                }
            } else {
                // Nowhere to go: lower the threshold and retry the best
                // branch from here.
                t -= delta;
                tried[k] = 0;
                break;
            }
        }
    }
}

/// A validated WSPR audio-synthesis configuration.
///
/// WSPR's canonical timing is 8192 samples per symbol at 12 kHz. Any
/// sample rate that is a multiple of 375 Hz keeps the symbol period
/// exact (`rate × 256 / 375` is then an integer — 12 kHz gives 8192,
/// 48 kHz gives 32768); other rates are rejected rather than timed
/// approximately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WsprConfig {
    base_hz: u32,
    sample_rate: SampleRate,
}

impl WsprConfig {
    /// Creates a configuration for the given base audio frequency
    /// (the symbol-0 tone, conventionally around 1500 Hz) and sample
    /// rate.
    ///
    /// # Errors
    ///
    /// [`WsprError::SampleRateInexact`] when the rate is not a
    /// multiple of 375 Hz; [`WsprError::ToneOutOfRange`] when the base
    /// frequency is zero or the highest tone reaches Nyquist.
    pub const fn new(base_hz: u32, sample_rate: SampleRate) -> Result<Self, WsprError> {
        let sr = sample_rate.hz();
        if !sr.is_multiple_of(375) {
            return Err(WsprError::SampleRateInexact { got: sr });
        }
        // Highest tone: base + 3 * 12000/8192 Hz; require < sr / 2.
        // Scaled by 8192: base * 8192 + 36000 < sr * 4096.
        if base_hz == 0 || (base_hz as u64) * 8_192 + 36_000 >= (sr as u64) * 4_096 {
            return Err(WsprError::ToneOutOfRange {
                base_hz,
                sample_rate: sr,
            });
        }
        Ok(Self {
            base_hz,
            sample_rate,
        })
    }

    /// The base audio frequency (symbol-0 tone) in Hz.
    #[must_use]
    pub const fn base_hz(self) -> u32 {
        self.base_hz
    }

    /// The configured sample rate.
    #[must_use]
    pub const fn sample_rate(self) -> SampleRate {
        self.sample_rate
    }

    /// Samples per channel symbol at this rate (exact by construction):
    /// `rate × 256 / 375` — 8192 at the canonical 12 kHz.
    #[must_use]
    pub const fn samples_per_symbol(self) -> u32 {
        self.sample_rate.hz() / 375 * 256
    }
}

/// Streaming continuous-phase 4-FSK generator for one WSPR transmission.
///
/// Mirrors the crate's AFSK [`Modulator`](crate::Modulator): a single
/// `u32` phase accumulator (full range = one cycle) advanced once per
/// sample by the current symbol's increment and **never reset**, so the
/// waveform is continuous across symbol boundaries. Owns no buffers
/// beyond the 162 symbols; allocation-free.
///
/// Pull samples with [`WsprModulator::next_i16`] /
/// [`WsprModulator::next_f32`], fill caller buffers with
/// [`WsprModulator::fill_i16`], or use the `Iterator<Item = i16>`
/// implementation. The i16 and f32 paths share the one phase
/// accumulator; use one per transmission.
#[derive(Debug, Clone)]
pub struct WsprModulator {
    /// Phase accumulator; full u32 range == one waveform cycle.
    phase: u32,
    /// Per-sample phase increment for each of the four tones.
    inc: [u32; 4],
    /// The 162 channel symbols.
    symbols: [u8; SYMBOL_COUNT],
    /// Index of the symbol currently sounding.
    symbol_idx: usize,
    /// Samples already emitted for the current symbol.
    emitted_in_symbol: u32,
    /// Samples per symbol at the configured rate.
    samples_per_symbol: u32,
}

impl WsprModulator {
    /// Creates a generator for the given channel symbols (see
    /// [`WsprMessage::channel_symbols`]).
    ///
    /// Tone `k` sits at `base + k × 12000/8192` Hz; the phase increment
    /// is `round(f × 2³² / rate)`, computed without rounding the
    /// fractional tone frequency first.
    #[must_use]
    pub fn new(config: WsprConfig, symbols: [u8; SYMBOL_COUNT]) -> Self {
        let sr = u128::from(config.sample_rate().hz());
        let mut inc = [0u32; 4];
        for (k, slot) in inc.iter_mut().enumerate() {
            // f = (base * 8192 + k * 12000) / 8192 Hz, exactly.
            let num = (u128::from(config.base_hz()) * 8_192 + (k as u128) * 12_000) << 32;
            let den = 8_192 * sr;
            *slot = ((num + den / 2) / den) as u32;
        }
        Self {
            phase: 0,
            inc,
            symbols,
            symbol_idx: 0,
            emitted_in_symbol: 0,
            samples_per_symbol: config.samples_per_symbol(),
        }
    }

    /// Creates a generator directly from a validated message.
    #[must_use]
    pub fn for_message(config: WsprConfig, message: &WsprMessage) -> Self {
        Self::new(config, message.channel_symbols())
    }

    /// Total samples this transmission spans: `162 × samples_per_symbol`
    /// (1 327 104 at 12 kHz ≈ 110.6 s).
    #[must_use]
    pub fn total_samples(&self) -> u64 {
        SYMBOL_COUNT as u64 * u64::from(self.samples_per_symbol)
    }

    /// Advances the phase accumulator past the sample just emitted.
    fn advance(&mut self) {
        let inc = self.inc[usize::from(self.symbols[self.symbol_idx] & 3)];
        self.phase = self.phase.wrapping_add(inc);
        self.emitted_in_symbol += 1;
        if self.emitted_in_symbol == self.samples_per_symbol {
            self.emitted_in_symbol = 0;
            self.symbol_idx += 1;
        }
    }

    /// Pulls the next i16 PCM sample, or `None` when the transmission
    /// is complete. Integer-only: table lookup plus a u32 addition.
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

impl Iterator for WsprModulator {
    type Item = i16;

    fn next(&mut self) -> Option<i16> {
        self.next_i16()
    }
}
