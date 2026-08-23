//! Laws a digipeater must obey, as properties rather than as cases.
//!
//! # Relaying is not canonicalisation, and this file is where that is
//! stated
//!
//! Two different operations get called "forwarding" and the crate
//! treats them very differently.
//!
//! **Canonicalisation** is `build(parse(w))`. It reads the information
//! field into a typed value and writes that value back out. Byte
//! identity is impossible there in general, because several wire
//! spellings can mean one value and only one of them can come back;
//! `tests/rebuild_fidelity.rs` and `tests/common/mod.rs` measure and
//! classify what moves.
//!
//! **Digipeating** is not that. A digipeater's business is the AX.25
//! header: find the first unused hop, decide whether it is addressed to
//! us, mark it repeated, decrement `WIDEn-N`, and re-transmit. The
//! information field is opaque payload that it has no authority over.
//! So the payload is carried by **identity**, not by canonicalisation,
//! and byte fidelity on it is free rather than aspirational.
//!
//! That distinction matters because the argument runs the wrong way
//! otherwise. "A digipeater that parses and re-transmits puts bytes on
//! the air nobody sent" is true, and it is not an argument for making
//! `build` reproduce its input. It is an argument for **not calling
//! `build`**. A relay that re-serialises the payload has made a
//! category error, and no amount of byte-preservation machinery in the
//! builder fixes the category error; it only hides it.
//!
//! `relay_decision` encodes this in its signature: it takes the path
//! and nothing else, so the payload cannot influence the decision and
//! the decision cannot influence the payload. The remaining way to get
//! it wrong is at the rebuild step, by feeding the relay a re-serialised
//! information field instead of the one that arrived. The first law
//! below is what catches that.

#![cfg(feature = "digipeat")]

use warble::ax25::{Address, PathHop, UiFrame};
use warble::digipeat::{Alias, ExactAliasAction, RelayDecision, WideLimit, relay_decision};

fn addr(call: &[u8], ssid: u8) -> Address {
    Address::new(call, ssid).expect("a valid address")
}

/// The station under test, and the aliases it serves.
fn station() -> (Address, [Alias; 2]) {
    let me = addr(b"N0CALL", 1);
    let wide = WideLimit::new(7).expect("WIDE7 is the widest served class");
    (me, [Alias::Exact(me), Alias::Wide(wide)])
}

/// Information fields a relay must carry unchanged.
///
/// Not all valid APRS. A digipeater forwards what it does not
/// understand, and the specification's requirement that a station
/// "must be able to process them without ill effects" applies most to
/// the bytes no parser in this crate claims. If payload transparency
/// held only for packets that parse, a relay would be a filter.
fn payloads() -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = vec![
        // Ordinary APRS.
        b"!4903.50N/07201.75W-hello".to_vec(),
        b"=/5L!!<*e7>{?!".to_vec(),
        b":N0CALL-2 :are you there?{01".to_vec(),
        b"T#046,400,007,00000000".to_vec(),
        // Spellings this crate canonicalises away. A relay must not.
        b"!4903.50n/07201.75w-lower case hemispheres".to_vec(),
        b"!L9Vx*Nj0g&  Gno-data trailer spelled with a G".to_vec(),
        b"_10090556g...t...r...dotted weather placeholders".to_vec(),
        // Things no parser here accepts at all.
        b"".to_vec(),
        b"\x00\x01\x02\x03".to_vec(),
        b"{{ an unsupported data type identifier".to_vec(),
        b"!49  .  N/072  .  W-position ambiguity".to_vec(),
        b"trailing carriage return\r".to_vec(),
        b"\xff\xfe\xfd non-UTF-8".to_vec(),
    ];
    // Every single byte on its own, so no value of the first byte (the
    // data type identifier) can be treated specially by accident.
    for b in 0u8..=255 {
        out.push(vec![b]);
    }
    out
}

/// Paths that produce a relay, one per branch of `relay_decision`.
fn relayable_paths() -> Vec<Vec<PathHop>> {
    vec![
        // Exact match on our own callsign.
        vec![PathHop::unused(addr(b"N0CALL", 1))],
        // WIDEn-1: consumed in place.
        vec![PathHop::unused(addr(b"WIDE1", 1))],
        // WIDEn-N, N > 1: insert us, decrement.
        vec![PathHop::unused(addr(b"WIDE2", 2))],
        vec![PathHop::unused(addr(b"WIDE7", 7))],
        // A used hop skipped to reach ours.
        vec![
            PathHop {
                address: addr(b"OTHER", 3),
                repeated: true,
            },
            PathHop::unused(addr(b"WIDE2", 1)),
        ],
        // Ours first, more behind it.
        vec![
            PathHop::unused(addr(b"WIDE1", 1)),
            PathHop::unused(addr(b"WIDE2", 2)),
        ],
    ]
}

/// **D1, payload transparency.** The information field that goes out is
/// the information field that came in, byte for byte, always.
///
/// Driven end to end over the wire rather than by inspecting a struct:
/// heard bytes, parse the frame, take the relay decision, rebuild with
/// the mutated path, and parse the result back. A relay that re-encodes
/// the payload fails here even though every path assertion still passes,
/// which is the mistake this law exists to catch.
///
/// Note what is *not* asserted: that the whole frame is unchanged. The
/// header must change, or nothing was digipeated.
#[test]
fn a_relay_carries_the_payload_byte_for_byte() {
    let (me, served) = station();
    let mut relayed = 0usize;
    for info in payloads() {
        for path in relayable_paths() {
            let heard = UiFrame::with_hops(addr(b"APRS", 0), addr(b"N0CALL", 7), &path, &info)
                .expect("a well-formed frame");
            let mut wire = [0u8; 512];
            let len = heard.build(&mut wire).expect("building the heard frame");
            // From here on, work only from the received bytes.
            let parsed = UiFrame::parse(&wire[..len]).expect("parsing the heard frame");
            let hops: Vec<PathHop> = parsed.hops().collect();
            let RelayDecision::Relay(out) =
                relay_decision(&hops, &served, me, ExactAliasAction::Keep)
            else {
                panic!("expected a relay for {hops:?}");
            };
            let sent = UiFrame::with_hops(parsed.dest, parsed.src, out.hops(), parsed.info)
                .expect("rebuilding with the mutated path");
            let mut out_wire = [0u8; 512];
            let out_len = sent.build(&mut out_wire).expect("building the relay");
            let reheard = UiFrame::parse(&out_wire[..out_len]).expect("parsing the relay");

            assert_eq!(
                reheard.info,
                &info[..],
                "the relay changed the payload: heard {:?}, sent {:?}",
                info,
                reheard.info
            );
            assert_ne!(
                &out_wire[..out_len],
                &wire[..len],
                "the relay changed nothing at all, so no hop was consumed"
            );
            relayed += 1;
        }
    }
    assert!(
        relayed >= 1600,
        "only {relayed} relays were compared; the sweep has narrowed"
    );
}

/// The remaining hop budget of a path: what a flood has left to spend.
///
/// An unused `WIDEn-N` is worth `N` further relays; any other unused hop
/// is worth one, because exactly one station answers to it. A used hop
/// is worth nothing.
fn hop_budget(path: &[PathHop]) -> u32 {
    path.iter()
        .filter(|hop| !hop.repeated)
        .map(|hop| {
            let call = hop.address.callsign.as_bytes();
            let wide = call.len() == 5 && &call[..4] == b"WIDE" && (b'1'..=b'7').contains(&call[4]);
            if wide {
                u32::from(hop.address.ssid.value())
            } else {
                1
            }
        })
        .sum()
}

/// **D3, termination.** Every relay spends exactly one hop of the
/// budget, so a flood is finite and its depth is set by the originating
/// station's requested path.
///
/// This is the property that stops a digipeater network melting down,
/// and it is worth stating as an invariant rather than as a table of
/// worked examples. `relay_decision` has four branches that mutate a
/// path (exact keep, exact substitute, `WIDEn-1` consumed in place,
/// `WIDEn-N` decremented with a callsign inserted) and it is easy to
/// write a fifth that decrements nothing. Termination then depends on
/// the dupe ring alone, which is a time window rather than a bound, and
/// the failure is a network-wide one that no single station's tests
/// would show.
///
/// Exactly one, not just fewer: a relay that spent two hops would
/// still terminate, but it would silently halve the reach every
/// operator configured.
#[test]
fn every_relay_spends_exactly_one_hop_of_the_budget() {
    let (me, served) = station();
    let mut checked = 0usize;
    // Sweep the whole shape space that fits two hops: every WIDEn-N
    // with a plausible N, our own call, a stranger, each used and
    // unused, in either position.
    let mut candidates: Vec<PathHop> = Vec::new();
    for n in 1u8..=7 {
        let call = [b'W', b'I', b'D', b'E', b'0' + n];
        for remaining in 0u8..=n {
            for repeated in [false, true] {
                candidates.push(PathHop {
                    address: addr(&call, remaining),
                    repeated,
                });
            }
        }
    }
    for repeated in [false, true] {
        candidates.push(PathHop {
            address: me,
            repeated,
        });
        candidates.push(PathHop {
            address: addr(b"OTHER", 3),
            repeated,
        });
    }

    for first in &candidates {
        for second in &candidates {
            for path in [vec![*first], vec![*first, *second]] {
                let before = hop_budget(&path);
                match relay_decision(&path, &served, me, ExactAliasAction::Keep) {
                    RelayDecision::Relay(out) => {
                        let after = hop_budget(out.hops());
                        assert_eq!(
                            after,
                            before - 1,
                            "relaying {path:?} moved the hop budget from \
                             {before} to {after}; every relay must spend \
                             exactly one"
                        );
                        checked += 1;
                    }
                    // Declining to relay is always safe: it spends
                    // nothing and cannot extend a flood.
                    RelayDecision::Ignore(_) => {}
                }
            }
        }
    }
    assert!(
        checked >= 500,
        "only {checked} relays were exercised; the sweep has narrowed"
    );
}

/// **D4, local loop freedom.** A station that has already repeated a
/// frame must not repeat it again.
///
/// The H bit carries this, and the law is that relaying is not
/// idempotent but *absorbing*: relay once, and the same station
/// offered the result must decline. Without it two stations in range of
/// each other trade one frame until the band is unusable.
#[test]
fn relaying_our_own_relay_is_always_declined() {
    let (me, served) = station();
    for path in relayable_paths() {
        let RelayDecision::Relay(once) = relay_decision(&path, &served, me, ExactAliasAction::Keep)
        else {
            panic!("expected a relay for {path:?}");
        };
        // Offer our own output straight back to ourselves.
        let again = relay_decision(once.hops(), &served, me, ExactAliasAction::Keep);
        if let RelayDecision::Relay(twice) = again {
            // Relaying again is only legitimate when a *further* hop
            // was requested, and even then it must still spend budget.
            assert!(
                hop_budget(twice.hops()) < hop_budget(once.hops()),
                "relaying our own output for {path:?} spent no budget"
            );
        }
    }
}
