//! Reed-Solomon `RS(255, k)` codec over `GF(256)` for the FX.25 and IL2P
//! FEC layers.
//!
//! # The code family
//!
//! FX.25 protects a byte block with a systematic Reed-Solomon code over the
//! finite field `GF(256)` defined by the field polynomial
//!
//! ```text
//! x^8 + x^4 + x^3 + x^2 + 1        (0x11D)
//! ```
//!
//! with first consecutive root `fcr = 1` and primitive element step
//! `prim = 1`.
//!
//! **These three parameters are not stated in the FX.25 specification.**
//! It names the RS codes by their `(n, k)` shape and cites CCSDS 101.0-B-6
//! in its bibliography, but the parameters above are *not* the CCSDS ones;
//! they are the conventional values of the widely used `init_rs_char`
//! implementation, which the specification's authors used in their own
//! reference encoder. They are therefore a de-facto interoperability
//! requirement rather than a documented one — a peer that reads the
//! specification and picks CCSDS parameters will not interoperate. Treat
//! them as pinned by the `tests/fx25.rs` and differential suites rather
//! than by the document.
//!
//! With those parameters, the generator polynomial for `p` parity symbols is
//!
//! ```text
//! g(x) = (x - a^1)(x - a^2) ... (x - a^p)
//! ```
//!
//! where `a` is the primitive element `x` (numeric value 2). The published
//! FX.25 codes use `p = 16`, `32` or `64` parity bytes ([`RsParity`]; the
//! IL2P layer additionally uses the short `p = 2`, `4`, `6`, `8` members of
//! the same family, with first consecutive root `a^0` instead of `a^1` —
//! an internal seam that leaves every FX.25 code path byte-identical), and
//! *shortened* blocks: fewer than `k = 255 - p` data bytes are treated as if
//! preceded by zero bytes, which are never transmitted.
//!
//! A code with `p` parity symbols corrects up to `t = p / 2` corrupted
//! *symbols* (bytes) anywhere in the block, data or parity alike.
//!
//! # Design
//!
//! [`RsCodec`] is `no_std` and allocation-free: the `GF(256)` log/antilog
//! tables are built at compile time by a `const fn`, the generator
//! polynomial lives inline in the codec value, and both encoder and decoder
//! work in caller-provided slices plus small fixed stack arrays (bounded by
//! the maximum block length 255 and locator degree 64). No input can make
//! it panic; failures surface as [`RsError`].
//!
//! # Beginner: encode and decode a round trip
//!
//! ```
//! use warble::rs::{RsCodec, RsParity};
//!
//! let codec = RsCodec::new(RsParity::Sixteen);
//! let data = *b"hello, fec world";
//!
//! // Build the codeblock: data followed by parity.
//! let mut block = [0u8; 16 + 16];
//! block[..16].copy_from_slice(&data);
//! let (head, parity) = block.split_at_mut(16);
//! codec.encode(head, parity)?;
//!
//! // A clean block decodes with zero corrections.
//! let corrected = codec.decode(&mut block)?;
//! assert_eq!(corrected, 0);
//! assert_eq!(&block[..16], &data);
//! # Ok::<(), warble::rs::RsError>(())
//! ```
//!
//! # Practitioner: shortened block with injected symbol errors
//!
//! Data shorter than `k = 255 - p` shortens the code; the decoder
//! infers the implicit zero prefix from the block length. Here 8 byte
//! errors — the maximum for [`RsParity::Sixteen`] — are corrected:
//!
//! ```
//! use warble::rs::{RsCodec, RsParity};
//!
//! let codec = RsCodec::new(RsParity::Sixteen);
//! let data: [u8; 40] = core::array::from_fn(|i| i as u8 ^ 0x5A);
//!
//! let mut block = [0u8; 40 + 16];
//! block[..40].copy_from_slice(&data);
//! let (head, parity) = block.split_at_mut(40);
//! codec.encode(head, parity)?;
//!
//! // Corrupt t = 8 symbols spread across data and parity.
//! for pos in [0usize, 7, 13, 21, 29, 39, 41, 55] {
//!     block[pos] ^= 0xFF;
//! }
//! assert_eq!(codec.decode(&mut block)?, 8);
//! assert_eq!(&block[..40], &data);
//! # Ok::<(), warble::rs::RsError>(())
//! ```
//!
//! # Expert: the `t = p / 2` bound and failure semantics
//!
//! Beyond `t` symbol errors the received word may fall outside every
//! decoding sphere (reported as [`RsError::Uncorrectable`], block left
//! *unspecified but valid*), or — with probability that shrinks rapidly in
//! the error excess — inside the sphere of a *different* codeword, in which
//! case the decoder "succeeds" with a wrong result. That residual
//! miscorrection risk is inherent to bounded-distance decoding, not an
//! implementation defect; layer a CRC above the RS code when it matters
//! (FX.25 does — the wrapped frame carries its own frame check sequence).
//!
//! ```
//! use warble::rs::{RsCodec, RsParity, RsError};
//!
//! let codec = RsCodec::new(RsParity::Sixteen);
//! let mut block = [0u8; 32];
//! let (head, parity) = block.split_at_mut(16);
//! codec.encode(head, parity)?;
//!
//! // 9 > t = 8 errors: this particular pattern is flagged, not miscorrected.
//! for pos in 0..9 {
//!     block[pos] ^= 0x01;
//! }
//! assert_eq!(codec.decode(&mut block), Err(RsError::Uncorrectable));
//! # Ok::<(), warble::rs::RsError>(())
//! ```

use core::fmt;

/// Length of the whole (unshortened) code word in symbols: `n = 255`.
pub const BLOCK_MAX: usize = 255;

/// Largest supported parity length in symbols.
const PARITY_MAX: usize = 64;

/// `GF(256)` field polynomial `x^8 + x^4 + x^3 + x^2 + 1` (the `x^8` bit
/// included), as used by FX.25.
const FIELD_POLY: u16 = 0x11D;

/// First consecutive root exponent of the generator polynomial (`fcr`).
const FCR: usize = 1;

/// Antilog table: `EXP[i] = a^i` for the primitive element `a = 2`.
///
/// Doubled to 510 valid entries so that `EXP[log(a) + log(b)]` never needs a
/// `mod 255` reduction (each log is at most 254).
const EXP: [u8; 512] = gf_tables().0;

/// Log table: `LOG[v]` is the discrete log of `v`; entry 0 is unused (0 has
/// no logarithm) and callers must guard the zero operand explicitly.
const LOG: [u8; 256] = gf_tables().1;

/// Builds the antilog/log tables at compile time by repeated multiplication
/// of the primitive element, reducing by the field polynomial on overflow.
const fn gf_tables() -> ([u8; 512], [u8; 256]) {
    let mut exp = [0u8; 512];
    let mut log = [0u8; 256];
    let mut value: u16 = 1;
    let mut i = 0;
    while i < 255 {
        exp[i] = value as u8;
        log[value as usize] = i as u8;
        value <<= 1;
        if value & 0x100 != 0 {
            value ^= FIELD_POLY;
        }
        i += 1;
    }
    while i < 512 {
        exp[i] = exp[i - 255];
        i += 1;
    }
    (exp, log)
}

/// `GF(256)` multiplication via log/antilog lookup.
#[inline]
const fn gf_mul(a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 {
        0
    } else {
        EXP[LOG[a as usize] as usize + LOG[b as usize] as usize]
    }
}

/// `GF(256)` multiplicative inverse; returns 0 for the (invalid) input 0 so
/// no input can panic — callers must reject the zero case beforehand.
#[inline]
const fn gf_inv(a: u8) -> u8 {
    if a == 0 {
        0
    } else {
        EXP[255 - LOG[a as usize] as usize]
    }
}

/// Number of Reed-Solomon parity symbols per block.
///
/// FX.25 publishes codes with 16, 32 and 64 check bytes; IL2P uses the
/// short 2/4/6/8 (and 16) operating points of the same `RS(255, k)`
/// family. The correction capability is half the parity length
/// (`t = p / 2` symbols).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RsParity {
    /// 2 parity symbols: corrects 1 symbol error (IL2P header/payload).
    Two,
    /// 4 parity symbols: corrects up to 2 symbol errors (IL2P payload).
    Four,
    /// 6 parity symbols: corrects up to 3 symbol errors (IL2P payload).
    Six,
    /// 8 parity symbols: corrects up to 4 symbol errors (IL2P payload).
    Eight,
    /// 16 parity symbols: corrects up to 8 symbol errors.
    Sixteen,
    /// 32 parity symbols: corrects up to 16 symbol errors.
    ThirtyTwo,
    /// 64 parity symbols: corrects up to 32 symbol errors.
    SixtyFour,
}

impl RsParity {
    /// The parity length in symbols (bytes).
    #[must_use]
    pub const fn len(self) -> usize {
        match self {
            RsParity::Two => 2,
            RsParity::Four => 4,
            RsParity::Six => 6,
            RsParity::Eight => 8,
            RsParity::Sixteen => 16,
            RsParity::ThirtyTwo => 32,
            RsParity::SixtyFour => 64,
        }
    }

    /// Always `false`: every variant carries at least 2 parity symbols.
    /// (Provided because [`Self::len`] exists.)
    #[must_use]
    pub const fn is_empty(self) -> bool {
        false
    }

    /// Maximum number of correctable symbol errors, `t = p / 2`.
    #[must_use]
    pub const fn correctable(self) -> usize {
        self.len() / 2
    }
}

/// Errors reported by [`RsCodec`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RsError {
    /// The data slice passed to [`RsCodec::encode`] exceeds the code's
    /// capacity of `255 - parity` bytes.
    DataTooLong {
        /// Length of the offending data slice.
        got: usize,
        /// Maximum data length for this parity setting.
        max: usize,
    },
    /// The parity slice passed to [`RsCodec::encode`] does not match the
    /// codec's parity length.
    ParityLengthMismatch {
        /// Length of the offending parity slice.
        got: usize,
        /// Required parity length for this codec.
        expected: usize,
    },
    /// The block passed to [`RsCodec::decode`] is too short to contain the
    /// parity plus at least one data byte, or longer than 255 bytes.
    BlockLengthInvalid {
        /// Length of the offending block.
        got: usize,
        /// Smallest valid block length (`parity + 1`).
        min: usize,
        /// Largest valid block length (255).
        max: usize,
    },
    /// The received block contains more symbol errors than the code can
    /// correct; its contents are left unspecified (but initialized).
    Uncorrectable,
}

impl fmt::Display for RsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            RsError::DataTooLong { got, max } => {
                write!(f, "data length {got} exceeds RS capacity {max}")
            }
            RsError::ParityLengthMismatch { got, expected } => {
                write!(f, "parity slice length {got}, codec requires {expected}")
            }
            RsError::BlockLengthInvalid { got, min, max } => {
                write!(f, "block length {got} outside valid range {min}..={max}")
            }
            RsError::Uncorrectable => {
                write!(f, "too many symbol errors: block is uncorrectable")
            }
        }
    }
}

impl core::error::Error for RsError {}

/// A systematic `RS(255, 255 - p)` encoder/decoder over `GF(256)`.
///
/// Construct with [`RsCodec::new`], picking the parity length; see the
/// [module docs](self) for worked examples at three levels.
#[derive(Debug, Clone)]
pub struct RsCodec {
    /// Selected parity length.
    parity: RsParity,
    /// First consecutive root exponent: the generator polynomial roots are
    /// `a^fcr .. a^(fcr + p - 1)`. FX.25 uses 1 ([`FCR`]); IL2P uses 0.
    /// Only the values 0 and 1 are constructible.
    fcr: u8,
    /// Generator polynomial coefficients, lowest degree first;
    /// `generator[parity]` is the (monic) leading coefficient. Slots beyond
    /// `parity` are zero.
    generator: [u8; PARITY_MAX + 1],
}

impl RsCodec {
    /// Creates a codec for the given parity length, computing the generator
    /// polynomial `g(x) = (x - a^1)(x - a^2)...(x - a^p)` (the FX.25
    /// convention, first consecutive root `a^1`).
    #[must_use]
    pub fn new(parity: RsParity) -> Self {
        #[allow(clippy::cast_possible_truncation)] // FCR = 1
        Self::with_fcr(parity, FCR as u8)
    }

    /// [`RsCodec::new`] with an explicit first-consecutive-root exponent:
    /// `g(x) = (x - a^fcr)...(x - a^(fcr + p - 1))`. FX.25 uses `fcr = 1`
    /// ([`RsCodec::new`]); IL2P uses `fcr = 0`. Values above 1 are clamped
    /// to 1 so the decoder's Forney factor stays exact.
    pub(crate) fn with_fcr(parity: RsParity, fcr: u8) -> Self {
        let fcr = if fcr > 1 { 1 } else { fcr };
        let p = parity.len();
        let mut generator = [0u8; PARITY_MAX + 1];
        generator[0] = 1;
        let mut degree = 0;
        // Multiply the running product by (x - a^(fcr + i)); in GF(2^8)
        // subtraction is XOR so the constant term is a^(fcr + i).
        while degree < p {
            let root = EXP[fcr as usize + degree];
            let mut j = degree + 1;
            // new[j] = old[j - 1] + root * old[j], walking downward so the
            // in-place update never reads an already-written slot.
            while j > 0 {
                generator[j] = generator[j - 1] ^ gf_mul(root, generator[j]);
                j -= 1;
            }
            generator[0] = gf_mul(root, generator[0]);
            degree += 1;
        }
        Self {
            parity,
            fcr,
            generator,
        }
    }

    /// The parity length in symbols.
    #[must_use]
    pub const fn parity_len(&self) -> usize {
        self.parity.len()
    }

    /// Maximum data length: `k = 255 - parity`. Shorter data uses the
    /// shortened code (implicit zero prefix).
    #[must_use]
    pub const fn data_capacity(&self) -> usize {
        BLOCK_MAX - self.parity.len()
    }

    /// Maximum number of correctable symbol errors, `t = parity / 2`.
    #[must_use]
    pub const fn correctable(&self) -> usize {
        self.parity.correctable()
    }

    /// Computes the parity symbols for `data` into `parity`.
    ///
    /// Systematic encoding: the transmitted block is `data` followed by
    /// `parity`, so `data` itself is never modified. `data` may be any
    /// length up to [`Self::data_capacity`] (shortened code); `parity` must
    /// be exactly [`Self::parity_len`] bytes.
    ///
    /// # Errors
    ///
    /// [`RsError::DataTooLong`] or [`RsError::ParityLengthMismatch`] on
    /// slice-length violations; never fails otherwise.
    pub fn encode(&self, data: &[u8], parity: &mut [u8]) -> Result<(), RsError> {
        let p = self.parity.len();
        if data.len() > self.data_capacity() {
            return Err(RsError::DataTooLong {
                got: data.len(),
                max: self.data_capacity(),
            });
        }
        if parity.len() != p {
            return Err(RsError::ParityLengthMismatch {
                got: parity.len(),
                expected: p,
            });
        }
        // Polynomial long division of data(x) * x^p by g(x) realized as an
        // LFSR: the register holds the running remainder, highest degree at
        // index 0. Implicit leading zeros of a shortened block feed zero
        // into the division and leave the register untouched, so they need
        // no explicit processing.
        let mut reg = [0u8; PARITY_MAX];
        for &byte in data {
            let feedback = byte ^ reg[0];
            let mut i = 0;
            while i + 1 < p {
                reg[i] = reg[i + 1] ^ gf_mul(feedback, self.generator[p - 1 - i]);
                i += 1;
            }
            reg[p - 1] = gf_mul(feedback, self.generator[0]);
        }
        parity.copy_from_slice(&reg[..p]);
        Ok(())
    }

    /// Decodes a received block (`data` followed by `parity`) in place,
    /// correcting up to `t = parity / 2` symbol errors.
    ///
    /// Returns the number of symbols corrected (0 for a clean block). On
    /// success the first `block.len() - parity` bytes are the corrected
    /// data.
    ///
    /// # Errors
    ///
    /// * [`RsError::BlockLengthInvalid`] unless
    ///   `parity < block.len() <= 255`.
    /// * [`RsError::Uncorrectable`] when the error pattern exceeds the
    ///   code's correction capability (the block contents are then
    ///   unspecified but never uninitialized — some corrections may have
    ///   been applied and rolled into the failed verification).
    pub fn decode(&self, block: &mut [u8]) -> Result<usize, RsError> {
        let p = self.parity.len();
        let n = block.len();
        if n <= p || n > BLOCK_MAX {
            return Err(RsError::BlockLengthInvalid {
                got: n,
                min: p + 1,
                max: BLOCK_MAX,
            });
        }

        // Syndromes: S_j = r(a^(fcr + j)) for j in 0..p, where the received
        // polynomial r(x) has block[0] as its highest-degree coefficient.
        let mut syn = [0u8; PARITY_MAX];
        let mut clean = true;
        for j in 0..p {
            let x = EXP[self.fcr as usize + j];
            let mut acc = 0u8;
            for &byte in block.iter() {
                acc = gf_mul(acc, x) ^ byte;
            }
            syn[j] = acc;
            clean &= acc == 0;
        }
        if clean {
            return Ok(0);
        }

        // Berlekamp-Massey: find the minimal LFSR (error locator polynomial
        // lambda, lowest degree first) generating the syndrome sequence.
        let mut lambda = [0u8; PARITY_MAX + 1];
        let mut prev = [0u8; PARITY_MAX + 1];
        lambda[0] = 1;
        prev[0] = 1;
        let mut errors = 0usize; // current LFSR length L
        let mut shift = 1usize; // x^shift multiplier pending on `prev`
        let mut prev_disc = 1u8; // discrepancy at the last length change
        for step in 0..p {
            let mut disc = syn[step];
            let mut i = 1;
            while i <= errors && i <= step {
                disc ^= gf_mul(lambda[i], syn[step - i]);
                i += 1;
            }
            if disc == 0 {
                shift += 1;
            } else {
                let coef = gf_mul(disc, gf_inv(prev_disc));
                if 2 * errors <= step {
                    let snapshot = lambda;
                    let mut i = 0;
                    while i + shift <= PARITY_MAX {
                        lambda[i + shift] ^= gf_mul(coef, prev[i]);
                        i += 1;
                    }
                    prev = snapshot;
                    prev_disc = disc;
                    errors = step + 1 - errors;
                    shift = 1;
                } else {
                    let mut i = 0;
                    while i + shift <= PARITY_MAX {
                        lambda[i + shift] ^= gf_mul(coef, prev[i]);
                        i += 1;
                    }
                    shift += 1;
                }
            }
        }
        if errors > self.parity.correctable() {
            return Err(RsError::Uncorrectable);
        }

        // Chien search: an error at block index i (degree n-1-i, i.e.
        // locator power j = n-1-i) makes a^-j a root of lambda. Roots at
        // powers >= n would land in the implicit zero prefix of a shortened
        // block, so scanning j in 0..n and demanding exactly `errors` roots
        // also rejects those.
        let mut positions = [0usize; PARITY_MAX / 2];
        let mut found = 0usize;
        for j in 0..n {
            let x_inv = if j == 0 { 1 } else { EXP[255 - j] };
            let mut acc = 0u8;
            let mut i = errors + 1;
            while i > 0 {
                i -= 1;
                acc = gf_mul(acc, x_inv) ^ lambda[i];
            }
            if acc == 0 {
                if found == positions.len() {
                    return Err(RsError::Uncorrectable);
                }
                positions[found] = j;
                found += 1;
            }
        }
        if found != errors || found == 0 {
            return Err(RsError::Uncorrectable);
        }

        // Forney: error evaluator omega(x) = S(x) * lambda(x) mod x^p; the
        // magnitude at locator power j is
        // X_j^(1-fcr) * omega(a^-j) / lambda'(a^-j) with X_j = a^j — the
        // leading factor is 1 for fcr = 1 (FX.25) and a^j for fcr = 0
        // (IL2P).
        let mut omega = [0u8; PARITY_MAX];
        for (i, slot) in omega.iter_mut().enumerate().take(p) {
            let mut acc = 0u8;
            let mut k = 0;
            while k <= i && k <= errors {
                acc ^= gf_mul(lambda[k], syn[i - k]);
                k += 1;
            }
            *slot = acc;
        }
        for &j in positions.iter().take(found) {
            let x_inv = if j == 0 { 1 } else { EXP[255 - j] };
            // omega(x_inv) by Horner from the top coefficient down.
            let mut num = 0u8;
            let mut i = p;
            while i > 0 {
                i -= 1;
                num = gf_mul(num, x_inv) ^ omega[i];
            }
            // Formal derivative: lambda'(x) = sum over odd i of
            // lambda_i * x^(i-1) (even terms vanish in characteristic 2).
            let x_inv_sq = gf_mul(x_inv, x_inv);
            let mut den = 0u8;
            let mut power = 1u8; // x_inv^(i-1) for the current odd i
            let mut i = 1;
            while i <= errors {
                den ^= gf_mul(lambda[i], power);
                power = gf_mul(power, x_inv_sq);
                i += 2;
            }
            if den == 0 {
                return Err(RsError::Uncorrectable);
            }
            // X_j^(1-fcr): the fcr = 0 codes need one extra a^j factor.
            if self.fcr == 0 {
                num = gf_mul(num, EXP[j]);
            }
            let magnitude = gf_mul(num, gf_inv(den));
            block[n - 1 - j] ^= magnitude;
        }

        // Verify: all syndromes of the corrected block must vanish;
        // otherwise the pattern exceeded the code's capability.
        for j in 0..p {
            let x = EXP[self.fcr as usize + j];
            let mut acc = 0u8;
            for &byte in block.iter() {
                acc = gf_mul(acc, x) ^ byte;
            }
            if acc != 0 {
                return Err(RsError::Uncorrectable);
            }
        }
        Ok(found)
    }
}
