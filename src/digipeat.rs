//! WIDEn-N digipeater primitives: served aliases, the relay decision, and
//! duplicate suppression.
//!
//! # Where the conventions come from
//!
//! WIDEn-N and the alias-substitution behaviour are **operating
//! convention, not part of the AX.25 specification** — AX.25 2.2 defines
//! only the digipeater path and its H bits ([`crate::ax25`]). The
//! convention is described in:
//!
//! > Bruninga, B. (WB4APR), "Fixing the 144.39 APRS Network — The New
//! > n-N Paradigm", aprs.org, undated (c. 2004).
//! > <https://web.archive.org/web/20220406212642/http://aprs.org/fix14439.html>
//!
//! (Cite the snapshot: the live page was compromised with injected spam
//! some time after April 2022, following the author's death.)
//!
//! A later and more precise treatment, including the decision table this
//! module's [`relay_decision`] follows in spirit:
//!
//! > Langner, J. (WB2OSZ), "APRS Digipeater Algorithm", APRS Foundation,
//! > September 2024 (rev. July 2025), §4.
//!
//! Several mutation conventions exist in the field; the one implemented
//! here is stated explicitly below rather than left implicit.
//!
//! A digipeater is a store-and-forward relay: it hears an AX.25 UI frame,
//! inspects the digipeater path, and — when the *first unused hop*
//! (has-been-repeated bit clear) is addressed to it — retransmits the
//! frame with that hop marked used. This module provides the pure,
//! `no_std`, allocation-free core of that decision so an embedded relay
//! and a workstation relay can share one tested implementation:
//!
//! * [`Alias`] — one served alias: an exact callsign or a WIDEn-N limit;
//! * [`relay_decision`] — path in, [`RelayDecision`] (mutated path or a
//!   typed ignore reason) out;
//! * [`DupeRing`] — a fixed-size fingerprint ring that suppresses
//!   retransmission of recently-heard frames.
//!
//! # The WIDEn-N convention this module implements
//!
//! An APRS station requests relaying with path aliases like `WIDE2-1`:
//! the callsign names the *requested trace class* `n` (`WIDE1`..`WIDE7`,
//! so `n` is `1..=7`) and the SSID carries the *remaining hop count* `N`.
//! Throughout this module `n` is always the class and `N` always the
//! remaining count. Exactly one mutation convention is implemented
//! (there are several in the field; this is the callsign-insertion form
//! used by New-Paradigm fill-in and wide digipeaters):
//!
//! * **Exact alias** (e.g. the station's own call): set the hop's H bit;
//!   with [`ExactAliasAction::Substitute`] also replace the alias with
//!   `my_call` so the trace shows which station repeated the frame.
//! * **`WIDEn-N`, N > 1**: decrement the SSID to `N-1` and insert
//!   `my_call` with its H bit set *before* the WIDE hop, so the path
//!   reads `MYCALL*,WIDEn-(N-1)`.
//! * **`WIDEn-N`, N == 1**: the last requested hop — set the H bit on
//!   the WIDE hop itself (consume it), no insertion.
//! * **`WIDEn-N`, N == 0 or N > n** (e.g. `WIDE1-0`, `WIDE1-2`), or `n`
//!   above the operator's [`WideLimit`]: refused with a typed reason,
//!   never relayed.
//!
//! `WIDEn-N` is the **only** alias pattern this module recognizes, and
//! only in that exact spelling: a five-byte callsign `WIDE1`..`WIDE7`.
//! `TRACEn-N`, bare `WIDE`, `RELAY`, `GATE` and the state/region
//! `SSn-N` aliases are not matched; unless the operator lists one as an
//! [`Alias::Exact`] (which serves it by setting its H bit, with no hop
//! counting), such a hop yields [`IgnoreReason::NotForUs`].
//!
//! Loop prevention is structural: a frame whose hops are all used yields
//! [`IgnoreReason::AllHopsUsed`], and a mutation that would exceed
//! [`MAX_DIGIPEATERS`] hops yields [`IgnoreReason::PathFull`].
//!
//! The crate has no clock: [`DupeRing`] timestamps are **caller-supplied
//! monotonic milliseconds** (any epoch, must not go backwards).

use core::fmt;

use crate::ax25::frame::MAX_DIGIPEATERS;
use crate::ax25::{Address, PathHop};

/// The default duplicate-suppression window, in milliseconds.
pub const DEFAULT_DUPE_WINDOW_MS: u64 = 30_000;

/// The hard ceiling on a WIDEn-N hop request, `n <= 7` (`WIDE7`).
pub const WIDE_N_MAX: u8 = 7;

/// A digipeat policy violation (invalid configuration value).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DigipeatError {
    /// A [`WideLimit`] outside `1..=7`.
    WideLimitOutOfRange {
        /// The rejected limit.
        got: u8,
    },
}

impl fmt::Display for DigipeatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            DigipeatError::WideLimitOutOfRange { got } => write!(
                f,
                "WIDEn-N limit {got} is out of range: must be within 1..=7"
            ),
        }
    }
}

impl core::error::Error for DigipeatError {}

/// A validated maximum served WIDEn-N class, `1..=7`.
///
/// The operator's max-n policy knob: a limit of 2 serves `WIDE1-x` and
/// `WIDE2-x` requests but refuses `WIDE3-x` and up (large-n floods are
/// abusive on shared channels).
///
/// ```
/// use yodel::digipeat::{DigipeatError, WideLimit};
///
/// assert_eq!(WideLimit::new(2)?.value(), 2);
/// assert_eq!(
///     WideLimit::new(0),
///     Err(DigipeatError::WideLimitOutOfRange { got: 0 })
/// );
/// assert_eq!(
///     WideLimit::new(8),
///     Err(DigipeatError::WideLimitOutOfRange { got: 8 })
/// );
/// # Ok::<(), DigipeatError>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WideLimit(u8);

impl WideLimit {
    /// The customary wide-area default: serve up to `WIDE2-x`.
    pub const TWO: Self = Self(2);

    /// Creates a validated limit.
    ///
    /// # Errors
    ///
    /// [`DigipeatError::WideLimitOutOfRange`] when `value` is outside
    /// `1..=7`.
    pub const fn new(value: u8) -> Result<Self, DigipeatError> {
        if value >= 1 && value <= WIDE_N_MAX {
            Ok(Self(value))
        } else {
            Err(DigipeatError::WideLimitOutOfRange { got: value })
        }
    }

    /// The limit value, `1..=7`.
    #[must_use]
    pub const fn value(self) -> u8 {
        self.0
    }
}

/// One served alias: what this digipeater answers to.
///
/// A station typically serves a small fixed set — its own callsign as an
/// exact alias plus one WIDEn-N policy — passed to [`relay_decision`] as
/// a plain slice, so the set is allocation-free:
///
/// ```
/// use yodel::ax25::Address;
/// use yodel::digipeat::{Alias, DigipeatError, WideLimit};
///
/// let my_call = Address::new(b"N0CALL", 1).unwrap();
/// let served = [
///     Alias::Exact(my_call),           // directly addressed hops
///     Alias::Wide(WideLimit::new(2)?), // WIDE1-x / WIDE2-x, refuse WIDE3+
/// ];
/// assert_eq!(served.len(), 2);
/// # Ok::<(), DigipeatError>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alias {
    /// An exact-match callsign+SSID (e.g. the station's own call).
    Exact(Address),
    /// WIDEn-N pattern service up to the given class limit.
    Wide(WideLimit),
}

/// What to do with an exact-alias hop when serving it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExactAliasAction {
    /// Keep the alias in the path and set its H bit.
    Keep,
    /// Replace the alias with `my_call` (H bit set) — callsign
    /// insertion, so the trace records which station repeated the frame.
    Substitute,
}

/// Why a frame was not relayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IgnoreReason {
    /// Every hop already carries its H bit: the path is spent.
    /// Relaying it again would loop the frame.
    AllHopsUsed,
    /// The first unused hop matches none of the served aliases.
    NotForUs,
    /// A WIDEn hop with remaining count 0, or a count above its own
    /// class (e.g. `WIDE1-2`): malformed, never relayed.
    WideInvalid {
        /// The requested class `n` from the callsign.
        n: u8,
        /// The remaining hop count from the SSID.
        remaining: u8,
    },
    /// A WIDEn class above the operator's [`WideLimit`] policy.
    WideAboveLimit {
        /// The requested class `n`.
        requested: u8,
        /// The served maximum.
        max: u8,
    },
    /// The required callsign insertion would exceed
    /// [`MAX_DIGIPEATERS`] hops.
    PathFull,
}

impl fmt::Display for IgnoreReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            IgnoreReason::AllHopsUsed => {
                write!(f, "every path hop is already used (H bit set)")
            }
            IgnoreReason::NotForUs => {
                write!(f, "the first unused hop matches no served alias")
            }
            IgnoreReason::WideInvalid { n, remaining } => write!(
                f,
                "WIDE{n}-{remaining} is malformed: the remaining count must be within 1..={n}"
            ),
            IgnoreReason::WideAboveLimit { requested, max } => {
                write!(f, "WIDE{requested} exceeds the served maximum of WIDE{max}")
            }
            IgnoreReason::PathFull => write!(
                f,
                "callsign insertion would exceed {MAX_DIGIPEATERS} path hops"
            ),
        }
    }
}

/// The mutated digipeater path of a frame that should be relayed.
///
/// A fixed-capacity ([`MAX_DIGIPEATERS`]) hop list; read it with
/// [`RelayPath::hops`] and feed it to
/// [`UiFrame::with_hops`](crate::ax25::UiFrame::with_hops).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelayPath {
    hops: [PathHop; MAX_DIGIPEATERS],
    len: usize,
}

impl RelayPath {
    /// The mutated hops, ready to rebuild the frame with.
    #[must_use]
    pub fn hops(&self) -> &[PathHop] {
        self.hops
            .get(..self.len.min(MAX_DIGIPEATERS))
            .unwrap_or(&[])
    }
}

/// The outcome of [`relay_decision`]: relay with a mutated path, or
/// ignore for a stated reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayDecision {
    /// Retransmit the frame with this mutated path.
    Relay(RelayPath),
    /// Do not retransmit; the reason says why.
    Ignore(IgnoreReason),
}

/// Parses a WIDEn-N hop: callsign `WIDE1`..`WIDE7`, SSID = remaining
/// count. Returns `(n, remaining)`; `None` when the callsign is not a
/// WIDEn form at all.
///
/// The match is exact and narrow — five bytes, `WIDE` followed by one
/// digit `1..=7`. `TRACEn-N`, bare `WIDE`, `RELAY` and the state/region
/// `SSn-N` aliases all return `None` and end up as
/// [`IgnoreReason::NotForUs`] unless served as an [`Alias::Exact`].
fn parse_wide(address: &Address) -> Option<(u8, u8)> {
    let call = address.callsign.as_bytes();
    if call.len() != 5 || &call[..4] != b"WIDE" {
        return None;
    }
    let digit = call[4].wrapping_sub(b'0');
    if (1..=WIDE_N_MAX).contains(&digit) {
        Some((digit, address.ssid.value()))
    } else {
        None
    }
}

/// Decides whether — and how — to relay a heard frame: **the** shared
/// digipeater core.
///
/// Scans `path` for the first hop with a clear H bit and applies the
/// module-level WIDEn-N convention against the served `aliases`;
/// `my_call` is the relaying station's own address, used for callsign
/// insertion (WIDEn-N decrement) and for [`ExactAliasAction::Substitute`].
/// The function is pure: it never transmits, it only returns the mutated
/// hop list (or a typed reason not to).
///
/// A fully-used path is **never** relayed, and a mutation that would
/// exceed [`MAX_DIGIPEATERS`] hops is refused — both are loop/flood
/// protections, not errors.
///
/// ```
/// use yodel::ax25::{Address, PathHop};
/// use yodel::digipeat::{
///     Alias, ExactAliasAction, RelayDecision, WideLimit, relay_decision,
/// };
///
/// let my_call = Address::new(b"N0CALL", 1).unwrap();
/// let served = [Alias::Exact(my_call), Alias::Wide(WideLimit::TWO)];
///
/// // A fresh WIDE2-2 request ...
/// let heard = [PathHop::unused(Address::new(b"WIDE2", 2).unwrap())];
/// let decision = relay_decision(&heard, &served, my_call, ExactAliasAction::Keep);
///
/// // ... becomes N0CALL-1* , WIDE2-1: insert ourselves used, decrement.
/// let RelayDecision::Relay(path) = decision else {
///     panic!("expected a relay");
/// };
/// assert_eq!(
///     path.hops(),
///     &[
///         PathHop { address: my_call, repeated: true },
///         PathHop::unused(Address::new(b"WIDE2", 1).unwrap()),
///     ]
/// );
/// ```
#[must_use]
pub fn relay_decision(
    path: &[PathHop],
    aliases: &[Alias],
    my_call: Address,
    exact_action: ExactAliasAction,
) -> RelayDecision {
    // Never mutate a path we could not rebuild.
    if path.len() > MAX_DIGIPEATERS {
        return RelayDecision::Ignore(IgnoreReason::PathFull);
    }
    let Some(first_unused) = path.iter().position(|hop| !hop.repeated) else {
        return RelayDecision::Ignore(IgnoreReason::AllHopsUsed);
    };
    let hop = match path.get(first_unused) {
        Some(h) => *h,
        // Unreachable: `position` returned an in-bounds index.
        None => return RelayDecision::Ignore(IgnoreReason::NotForUs),
    };

    let mut out = RelayPath {
        hops: [PathHop::unused(my_call); MAX_DIGIPEATERS],
        len: path.len(),
    };
    for (slot, src) in out.hops.iter_mut().zip(path.iter()) {
        *slot = *src;
    }

    // Exact aliases take precedence over pattern matching.
    let exact = aliases
        .iter()
        .any(|alias| matches!(alias, Alias::Exact(a) if *a == hop.address));
    if exact {
        let served = PathHop {
            address: match exact_action {
                ExactAliasAction::Keep => hop.address,
                ExactAliasAction::Substitute => my_call,
            },
            repeated: true,
        };
        if let Some(slot) = out.hops.get_mut(first_unused) {
            *slot = served;
        }
        return RelayDecision::Relay(out);
    }

    let wide_limit = aliases.iter().find_map(|alias| match alias {
        Alias::Wide(limit) => Some(*limit),
        Alias::Exact(_) => None,
    });
    if let (Some(limit), Some((n, remaining))) = (wide_limit, parse_wide(&hop.address)) {
        if n > limit.value() {
            return RelayDecision::Ignore(IgnoreReason::WideAboveLimit {
                requested: n,
                max: limit.value(),
            });
        }
        if remaining == 0 || remaining > n {
            return RelayDecision::Ignore(IgnoreReason::WideInvalid { n, remaining });
        }
        if remaining == 1 {
            // Last requested hop: consume the WIDE hop in place.
            if let Some(slot) = out.hops.get_mut(first_unused) {
                slot.repeated = true;
            }
            return RelayDecision::Relay(out);
        }
        // remaining > 1: insert my_call used, decrement the WIDE SSID.
        if path.len() + 1 > MAX_DIGIPEATERS {
            return RelayDecision::Ignore(IgnoreReason::PathFull);
        }
        out.len = path.len() + 1;
        // Shift the WIDE hop (and everything after it) right by one.
        let mut i = out.len - 1;
        while i > first_unused {
            out.hops[i] = out.hops[i - 1];
            i -= 1;
        }
        out.hops[first_unused] = PathHop {
            address: my_call,
            repeated: true,
        };
        let decremented = match Address::new(hop.address.callsign.as_bytes(), remaining - 1) {
            Ok(a) => a,
            // Unreachable: the callsign came from a valid Address and
            // remaining-1 <= 6 fits any SSID.
            Err(_) => return RelayDecision::Ignore(IgnoreReason::NotForUs),
        };
        if let Some(slot) = out.hops.get_mut(first_unused + 1) {
            *slot = PathHop::unused(decremented);
        }
        return RelayDecision::Relay(out);
    }

    RelayDecision::Ignore(IgnoreReason::NotForUs)
}

/// The verdict of a [`DupeRing`] check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// Not heard within the window; the fingerprint is now recorded.
    Fresh,
    /// Heard within the window; do not relay again.
    Duplicate,
}

impl fmt::Display for Freshness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Freshness::Fresh => write!(f, "fresh"),
            Freshness::Duplicate => write!(f, "duplicate"),
        }
    }
}

/// FNV-1a over the identity of a transmission: source, destination,
/// and information field. The digipeater path is *not* hashed — the
/// same transmission heard again with more H bits set is still the
/// same transmission.
fn fingerprint(src: &Address, dest: &Address, info: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    let mut eat = |byte: u8| {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(PRIME);
    };
    for &b in src.callsign.as_bytes() {
        eat(b);
    }
    eat(src.ssid.value());
    for &b in dest.callsign.as_bytes() {
        eat(b);
    }
    eat(dest.ssid.value());
    for &b in info {
        eat(b);
    }
    hash
}

/// A fixed-size duplicate-suppression ring of frame fingerprints.
///
/// Holds up to `N` FNV-1a fingerprints (over source + destination +
/// info bytes) with their last-heard timestamps. The crate has no
/// clock: `now_ms` is **caller-supplied monotonic milliseconds** (any
/// epoch; must never decrease between calls). When the ring is full the
/// oldest entry is evicted.
///
/// ```
/// use yodel::ax25::Address;
/// use yodel::digipeat::{DupeRing, Freshness};
///
/// let src = Address::new(b"N0CALL", 1).unwrap();
/// let dest = Address::new(b"APRS", 0).unwrap();
/// let mut ring: DupeRing<8> = DupeRing::new(); // 30 s default window
///
/// // First hearing is fresh; the same frame 5 s later is a duplicate.
/// assert_eq!(ring.check_and_insert(&src, &dest, b">hi", 1_000), Freshness::Fresh);
/// assert_eq!(ring.check_and_insert(&src, &dest, b">hi", 6_000), Freshness::Duplicate);
///
/// // After the window expires it is fresh again.
/// assert_eq!(ring.check_and_insert(&src, &dest, b">hi", 40_000), Freshness::Fresh);
/// ```
#[derive(Debug, Clone)]
pub struct DupeRing<const N: usize> {
    /// `(fingerprint, last-heard ms)` per slot; `None` = never used.
    entries: [Option<(u64, u64)>; N],
    /// Next slot to overwrite: plain round-robin, *not* least-recently
    /// heard. New fingerprints are inserted in monotonic time order,
    /// but an already-present fingerprint whose window has expired is
    /// re-armed in place with the fresh timestamp (see
    /// [`DupeRing::check_and_insert`]), so slot ages are not monotonic
    /// around the ring and eviction can discard a fresher fingerprint
    /// than one it keeps. On a full ring that only weakens the
    /// duplicate window — a fingerprint still in the ring is always
    /// found, because the lookup scans every slot.
    cursor: usize,
    /// Suppression window in milliseconds.
    window_ms: u64,
}

impl<const N: usize> DupeRing<N> {
    /// An empty ring with the default window
    /// ([`DEFAULT_DUPE_WINDOW_MS`], 30 s).
    #[must_use]
    pub const fn new() -> Self {
        Self::with_window(DEFAULT_DUPE_WINDOW_MS)
    }

    /// An empty ring with a custom suppression window in milliseconds.
    #[must_use]
    pub const fn with_window(window_ms: u64) -> Self {
        Self {
            entries: [None; N],
            cursor: 0,
            window_ms,
        }
    }

    /// Checks whether the transmission identified by `src` + `dest` +
    /// `info` was heard within the window; records it either way.
    ///
    /// `now_ms` is the caller's monotonic clock in milliseconds. A
    /// duplicate does **not** refresh the stored timestamp, so a frame
    /// repeated every few seconds becomes relayable again one window
    /// after its first hearing rather than being suppressed forever.
    pub fn check_and_insert(
        &mut self,
        src: &Address,
        dest: &Address,
        info: &[u8],
        now_ms: u64,
    ) -> Freshness {
        let fp = fingerprint(src, dest, info);
        for entry in self.entries.iter_mut().flatten() {
            if entry.0 == fp {
                if now_ms.saturating_sub(entry.1) < self.window_ms {
                    return Freshness::Duplicate;
                }
                // Expired: re-arm this slot with the new hearing.
                entry.1 = now_ms;
                return Freshness::Fresh;
            }
        }
        if let Some(slot) = self.entries.get_mut(self.cursor) {
            *slot = Some((fp, now_ms));
        }
        self.cursor = if N == 0 { 0 } else { (self.cursor + 1) % N };
        Freshness::Fresh
    }
}

impl<const N: usize> Default for DupeRing<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(call: &[u8], ssid: u8) -> Address {
        match Address::new(call, ssid) {
            Ok(a) => a,
            Err(e) => panic!("{e}"),
        }
    }

    fn served() -> [Alias; 2] {
        [
            Alias::Exact(addr(b"N0CALL", 1)),
            Alias::Wide(WideLimit::TWO),
        ]
    }

    fn relay(decision: RelayDecision) -> RelayPath {
        match decision {
            RelayDecision::Relay(path) => path,
            RelayDecision::Ignore(reason) => panic!("expected relay, got ignore: {reason}"),
        }
    }

    #[test]
    fn exact_alias_sets_h_bit() {
        let my = addr(b"N0CALL", 1);
        let path = [PathHop::unused(my), PathHop::unused(addr(b"WIDE2", 1))];
        let out = relay(relay_decision(&path, &served(), my, ExactAliasAction::Keep));
        assert_eq!(
            out.hops(),
            &[
                PathHop {
                    address: my,
                    repeated: true
                },
                PathHop::unused(addr(b"WIDE2", 1)),
            ]
        );
    }

    #[test]
    fn exact_alias_substitution_inserts_my_call() {
        let my = addr(b"N0CALL", 1);
        let path = [PathHop::unused(addr(b"N0CALL", 2))];
        let aliases = [Alias::Exact(addr(b"N0CALL", 2))];
        let out = relay(relay_decision(
            &path,
            &aliases,
            my,
            ExactAliasAction::Substitute,
        ));
        assert_eq!(
            out.hops(),
            &[PathHop {
                address: my,
                repeated: true
            }]
        );
    }

    #[test]
    fn wide2_1_consumed_in_place() {
        let my = addr(b"N0CALL", 1);
        let path = [PathHop::unused(addr(b"WIDE2", 1))];
        let out = relay(relay_decision(&path, &served(), my, ExactAliasAction::Keep));
        assert_eq!(
            out.hops(),
            &[PathHop {
                address: addr(b"WIDE2", 1),
                repeated: true
            }]
        );
    }

    #[test]
    fn wide2_2_decrements_and_inserts() {
        let my = addr(b"N0CALL", 1);
        let path = [PathHop::unused(addr(b"WIDE2", 2))];
        let out = relay(relay_decision(&path, &served(), my, ExactAliasAction::Keep));
        assert_eq!(
            out.hops(),
            &[
                PathHop {
                    address: my,
                    repeated: true
                },
                PathHop::unused(addr(b"WIDE2", 1)),
            ]
        );
    }

    #[test]
    fn wide1_1_consumed() {
        let my = addr(b"N0CALL", 1);
        let path = [PathHop::unused(addr(b"WIDE1", 1))];
        let out = relay(relay_decision(&path, &served(), my, ExactAliasAction::Keep));
        assert_eq!(
            out.hops(),
            &[PathHop {
                address: addr(b"WIDE1", 1),
                repeated: true
            }]
        );
    }

    #[test]
    fn skips_used_hops_to_first_unused() {
        let my = addr(b"N0CALL", 1);
        let path = [
            PathHop {
                address: addr(b"K1ABC", 0),
                repeated: true,
            },
            PathHop::unused(addr(b"WIDE2", 1)),
        ];
        let out = relay(relay_decision(&path, &served(), my, ExactAliasAction::Keep));
        assert!(out.hops().iter().all(|h| h.repeated));
        assert_eq!(out.hops()[0].address, addr(b"K1ABC", 0));
    }

    #[test]
    fn fully_used_path_never_relayed() {
        let my = addr(b"N0CALL", 1);
        let path = [
            PathHop {
                address: my,
                repeated: true,
            },
            PathHop {
                address: addr(b"WIDE2", 1),
                repeated: true,
            },
        ];
        assert_eq!(
            relay_decision(&path, &served(), my, ExactAliasAction::Keep),
            RelayDecision::Ignore(IgnoreReason::AllHopsUsed)
        );
        // Empty path counts as fully used, too.
        assert_eq!(
            relay_decision(&[], &served(), my, ExactAliasAction::Keep),
            RelayDecision::Ignore(IgnoreReason::AllHopsUsed)
        );
    }

    #[test]
    fn non_matching_first_hop_ignored() {
        let my = addr(b"N0CALL", 1);
        // First unused hop is someone else's call, even though a WIDE
        // hop follows: only the first unused hop is consulted.
        let path = [
            PathHop::unused(addr(b"K1ABC", 0)),
            PathHop::unused(addr(b"WIDE2", 1)),
        ];
        assert_eq!(
            relay_decision(&path, &served(), my, ExactAliasAction::Keep),
            RelayDecision::Ignore(IgnoreReason::NotForUs)
        );
    }

    #[test]
    fn wide_n_zero_refused() {
        let my = addr(b"N0CALL", 1);
        let path = [PathHop::unused(addr(b"WIDE2", 0))];
        assert_eq!(
            relay_decision(&path, &served(), my, ExactAliasAction::Keep),
            RelayDecision::Ignore(IgnoreReason::WideInvalid { n: 2, remaining: 0 })
        );
    }

    #[test]
    fn wide_remaining_above_class_refused() {
        let my = addr(b"N0CALL", 1);
        let path = [PathHop::unused(addr(b"WIDE1", 2))];
        assert_eq!(
            relay_decision(&path, &served(), my, ExactAliasAction::Keep),
            RelayDecision::Ignore(IgnoreReason::WideInvalid { n: 1, remaining: 2 })
        );
    }

    #[test]
    fn wide_above_limit_refused() {
        let my = addr(b"N0CALL", 1);
        let path = [PathHop::unused(addr(b"WIDE3", 3))];
        assert_eq!(
            relay_decision(&path, &served(), my, ExactAliasAction::Keep),
            RelayDecision::Ignore(IgnoreReason::WideAboveLimit {
                requested: 3,
                max: 2
            })
        );
    }

    #[test]
    fn insertion_refused_when_path_full() {
        let my = addr(b"N0CALL", 1);
        let mut path = [PathHop {
            address: addr(b"K1ABC", 0),
            repeated: true,
        }; MAX_DIGIPEATERS];
        path[MAX_DIGIPEATERS - 1] = PathHop::unused(addr(b"WIDE2", 2));
        assert_eq!(
            relay_decision(&path, &served(), my, ExactAliasAction::Keep),
            RelayDecision::Ignore(IgnoreReason::PathFull)
        );
    }

    #[test]
    fn non_wide_callsigns_are_not_pattern_matched() {
        let my = addr(b"N0CALL", 1);
        for call in [&b"WIDE"[..], b"WIDE8", b"WIDE0", b"WIDER1", b"WIDES"] {
            if let Ok(a) = Address::new(call, 1) {
                assert_eq!(
                    parse_wide(&a),
                    None,
                    "{}",
                    core::str::from_utf8(call).unwrap()
                );
                let path = [PathHop::unused(a)];
                assert_eq!(
                    relay_decision(&path, &served(), my, ExactAliasAction::Keep),
                    RelayDecision::Ignore(IgnoreReason::NotForUs)
                );
            }
        }
    }

    #[test]
    fn dupe_ring_suppresses_within_window() {
        let src = addr(b"N0CALL", 1);
        let dest = addr(b"APRS", 0);
        let mut ring: DupeRing<4> = DupeRing::new();
        assert_eq!(
            ring.check_and_insert(&src, &dest, b">a", 0),
            Freshness::Fresh
        );
        assert_eq!(
            ring.check_and_insert(&src, &dest, b">a", 29_999),
            Freshness::Duplicate
        );
        // Different info bytes are a different transmission.
        assert_eq!(
            ring.check_and_insert(&src, &dest, b">b", 1),
            Freshness::Fresh
        );
    }

    #[test]
    fn dupe_ring_admits_after_expiry() {
        let src = addr(b"N0CALL", 1);
        let dest = addr(b"APRS", 0);
        let mut ring: DupeRing<4> = DupeRing::with_window(10_000);
        assert_eq!(
            ring.check_and_insert(&src, &dest, b">a", 0),
            Freshness::Fresh
        );
        assert_eq!(
            ring.check_and_insert(&src, &dest, b">a", 10_000),
            Freshness::Fresh
        );
        assert_eq!(
            ring.check_and_insert(&src, &dest, b">a", 15_000),
            Freshness::Duplicate
        );
    }

    #[test]
    fn dupe_ring_evicts_oldest_at_capacity() {
        let dest = addr(b"APRS", 0);
        let mut ring: DupeRing<2> = DupeRing::new();
        let a = addr(b"N0CALL", 1);
        let b = addr(b"N1CALL", 1);
        let c = addr(b"N2CALL", 1);
        assert_eq!(ring.check_and_insert(&a, &dest, b">x", 0), Freshness::Fresh);
        assert_eq!(ring.check_and_insert(&b, &dest, b">x", 1), Freshness::Fresh);
        // Capacity 2: inserting a third evicts the oldest (a).
        assert_eq!(ring.check_and_insert(&c, &dest, b">x", 2), Freshness::Fresh);
        assert_eq!(ring.check_and_insert(&a, &dest, b">x", 3), Freshness::Fresh);
        // b was evicted by re-inserting a.
        assert_eq!(
            ring.check_and_insert(&c, &dest, b">x", 4),
            Freshness::Duplicate
        );
    }
}
