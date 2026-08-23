//! Size ratchet for the public APRS data model.
//!
//! Driven by a real user: a bicycle/vehicle tracker on an STM32F1 with
//! 20 KB of RAM. For them a struct's size is a budget line, not an
//! implementation detail, so it must be a **reviewed diff** rather than
//! something discovered on the target.
//!
//! This is not a "must be small" test — it is an equality.
//! Growth is often the right call (a unit-carrying field is wider than
//! the bare integer it replaces, and buys a class of bug), and shrinkage
//! is often real progress (collapsing two enum variants). Either way the
//! number changes in the same commit as the decision, with the reason in
//! the commit message.
//!
//! Measured on a 64-bit host. The 32-bit embedded targets have their own
//! layouts; what proves *those* still build is
//! `scripts/check-embedded.sh`, not this file. Pinning both would mean
//! `cfg(target_pointer_width)` arms that nothing in CI evaluates, so
//! only the host is pinned and the limitation is stated here instead of
//! implied.
//!
//! # This is a size ratchet, not a field-addition detector
//!
//! Stated because the opposite is the natural assumption, and it is
//! wrong. A new field that fits in existing padding moves no number
//! here and this file stays green. VERIFIED: adding
//! `pub freezing_rain: bool` to `WeatherReport` landed in the slack of
//! the 144-byte layout — the struct stayed **144 bytes** and this test
//! passed unchanged. Nor is that a lucky one-off. MEASURED on the host
//! layout: `WeatherReport`'s fields end at offset 138 (`humidity`, two
//! bytes, at 136) inside 144, so **6 bytes of tail slack** are sitting
//! there — the note on `PINNED` explains where they came from, since
//! only 2 of the previous 136 were slack and `luminosity`'s 4 bytes
//! rounded the struct up. Any handful of small fields lands in that gap
//! silently.
//!
//! So this file answers "did the memory budget move?" and nothing else.
//! It cannot answer "is the new field carried through the API, the
//! builders and both CLI writers?" — and that question has already been
//! answered wrong once: `luminosity` and `snowfall` reached the JSON
//! writer and not the text one, and this test stayed green throughout,
//! correctly, because the bytes were pinned and only the projection was
//! short. The companion guard for it is `tests/cli_projection.rs`,
//! which asserts every `WeatherReport` field reaches both the JSON and
//! the text rendering — planned as a later wave of this same effort, so
//! if it is not beside this file yet, that is the gap, not a lookup
//! error. Either way: field completeness is its job, bytes are this
//! file's.
#![cfg(all(feature = "aprs", feature = "micE"))]

use core::mem::size_of;

use warble::aprs::{
    AprsPacket, Decoded, DecodedKind, MicE, Position, PositionCs, PositionTimestamped,
    WeatherReport,
};

/// How many types are pinned.
///
/// Shared by [`PINNED`] and [`measured`] so the two cannot differ in
/// length: the check below `zip`s them, and `zip` stops at the shorter,
/// so a row added to one list and not the other used to be *silently
/// unchecked* rather than a failure. As sized arrays of one constant, a
/// missing row is a compile error instead.
const PINNED_LEN: usize = 8;

/// Every pinned type, as `(name, measured size)`.
///
/// MEASURED at commit `c0124a3`, before the data-model refactor:
/// `Decoded` 96, `DecodedKind` 80, `AprsPacket` 72, `WeatherReport`
/// **40**. The design note that planned the refactor assumed 64 for
/// `WeatherReport`, so its size-growth reasoning was built on a wrong
/// baseline — which is why these numbers are measured here rather than
/// asserted in prose.
///
/// # The one increase that has happened, and why it was worth it
///
/// Typing `WeatherReport`'s nine measurements took it from 40 to 120
/// bytes — a tripling — and
/// carried `AprsPacket` from 72 to 152 and `Decoded` from 96 to 168,
/// since the enum is as large as its largest variant.
///
/// That is a lot, and it is not paying for ergonomics. It is paying
/// for **correctness**. The protocol reference spells wind speed in
/// miles per hour in a positionless report and in knots in a Complete
/// Weather Report, and one integer field cannot mean both: the crate
/// shipped a field documented as mph that held knots for every `!`/`=`
/// weather position, a silent 15% error that no round-trip test could
/// see. `Option<Speed>` makes it unrepresentable. This is the
/// "physical quantities carry their unit in the type" invariant in
/// `CONTRIBUTING.md`.
///
/// For the 20 KB target that motivates this file, 152 bytes is a stack
/// temporary and 0.76% of RAM — one at a time, never an array, and
/// never at all in a build that does not parse weather.
///
/// # The second increase: 16 bytes for `WeatherReport::snowfall`
///
/// `WeatherReport` 120 → **136**, carrying `AprsPacket` 152 → 168,
/// `DecodedKind` 152 → 168 and `Decoded` 168 → 184. One more
/// `Option<Rainfall>`: `i64` has no niche, so the bit of presence costs
/// 8 bytes of padding beside it.
///
/// Bought with it: the `s` tag of a **Complete** Weather Report, which
/// chapter 12 defines as "snowfall (in inches) in the last 24 hours"
/// because that layout's positional `DDD/SSS` extension "replace[s] the
/// cccc and ssss fields". The decoder read it as wind speed instead, so
/// `!4903.50N/07201.75W_220/004…s050` came back as a 50 mph wind —
/// silently overwriting the 4 knots the positional field had already
/// decoded **correctly**. Fixing that without the field would have
/// traded a wrong value for a lost one; with it, the snow is decoded and
/// the rebuild is still byte-exact. See
/// `docs/APRS_CONFORMANCE.md` §3 and
/// `tests/aprs_extras.rs::complete_weather_tagged_s_is_snowfall_not_wind_speed`.
///
/// **If it ever does need to come down**, the lever is the `Option`s,
/// not the units: every quantity is `i64`-backed and `i64` has no
/// niche, so each `Option` costs 16 bytes to carry one bit. Ten bare
/// quantities plus a presence bitmask would be about 88. That means
/// private fields and accessors, which is plan step 7's work anyway,
/// so it should be done there or not at all — not by giving the units
/// back.
// The four weather-driven rows last moved when `WeatherReport` gained
// `luminosity: Option<u16>` so that a `L`/`l` tag mid-block stops
// costing every field after it. `Option<u16>` is 4 bytes (2 payload,
// 1 tag, 1 pad), and only 2 of `WeatherReport`'s 136 were slack, so it
// rounded to 144 — and `WeatherReport` is the size driver for the three
// types that embed it, which is why they all move together and by the
// same 8. `Position`, `PositionCs`, `PositionTimestamped` and `MicE`
// are unmoved: neither the Mic-E prefix fix nor the zero-speed fix
// added a field.
// The three enum rows last moved when chapter 6 position ambiguity
// landed. `Ambiguity` is one byte, and it fitted inside existing
// padding on every struct that took it: `Position`, `Object`, `Item`
// and `PositionWeather` are all unmoved, which is why the four struct
// rows below did not change. The enums grew by one alignment step
// because `PositionWeather` (the size driver, at 144) had no slack
// left, so its variant crossed a boundary and took the three enums
// that hold it with it. Eight bytes on a decoder that already carries
// a 40 KB receiver is the right trade for not silently over-reporting
// the precision of 221 packets per capture.
const PINNED: &[(&str, usize); PINNED_LEN] = &[
    ("Decoded", 208),
    ("DecodedKind", 192),
    ("AprsPacket", 192),
    ("Position", 56),
    ("PositionCs", 72),
    ("PositionTimestamped", 72),
    ("WeatherReport", 144),
    ("MicE", 56),
];

/// The same list, resolved against the real types.
fn measured() -> [(&'static str, usize); PINNED_LEN] {
    [
        ("Decoded", size_of::<Decoded<'_>>()),
        ("DecodedKind", size_of::<DecodedKind<'_>>()),
        ("AprsPacket", size_of::<AprsPacket<'_>>()),
        ("Position", size_of::<Position<'_>>()),
        ("PositionCs", size_of::<PositionCs<'_>>()),
        ("PositionTimestamped", size_of::<PositionTimestamped<'_>>()),
        ("WeatherReport", size_of::<WeatherReport>()),
        ("MicE", size_of::<MicE<'_>>()),
    ]
}

#[test]
fn public_type_sizes_are_pinned() {
    // Only meaningful on the host layout the constants were measured on;
    // see the module docs.
    if size_of::<usize>() != 8 {
        return;
    }

    let mut wrong = Vec::new();
    for (&(name, want), (got_name, got)) in PINNED.iter().zip(measured()) {
        assert_eq!(name, got_name, "PINNED and measured() are out of step");
        if want != got {
            wrong.push(format!("{name}: pinned {want}, measured {got}"));
        }
    }
    assert!(
        wrong.is_empty(),
        "public type sizes moved:\n  {}\n\nThis is not automatically a \
         failure — a wider field or a collapsed enum changes these \
         numbers legitimately. Update PINNED in the SAME commit as the \
         change, and say in the message why the new size is the right \
         trade for a 20 KB target.",
        wrong.join("\n  ")
    );
}
