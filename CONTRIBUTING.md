# Contributing to yodel

Thanks for your interest! This document describes how the project is
built, tested, and gated. All of it is enforced by CI
(`.github/workflows/ci.yml`), so running the same commands locally
before pushing saves a round trip.

## Scope: what belongs in this crate

Before proposing a new mode, check it against the rules below. They are
binding, and they exist so the crate stays something one person can
audit.

**Open, royalty-free standards only.** Proprietary or patent-encumbered
protocols are excluded permanently. This is not a "not yet" list:

| Excluded | Why |
|---|---|
| **VARA** | Closed; no public specification, and reverse engineering is out of the question here |
| **LoRa APRS** | Chirp spread spectrum is a silicon PHY, not an audio-band sample stream; nothing below the APRS text layer is shared, and that layer is already reusable |
| **AMBE / AMBE+2 vocoders** | Patent-encumbered |
| **DMR / C4FM / D-STAR** | Vocoders plus (for DMR) TDMA timing: layers of machinery this crate has no business hosting |
| **PSK31** | Shares neither the FSK discriminator nor HDLC framing |

**Implement from the specification**, with each constant citing the
document and section it came from. Where a protocol's authors publish
their tables under permissive or public-domain terms, vendor those files
under `third_party/` and check the embedded constants against them in a
test. "The permitted exception" and "Write where a constant came from"
below set out how provenance is recorded here.

**A published specification is not sufficient on its own.** A mode also
has to fit an existing seam or justify a new one. `docs/ARCHITECTURE.md`
is the survey of what fits where, under "Scope: which modes fit these
seams" and "Modes considered and declined".

## Building and testing

```sh
cargo build                     # default features (mod + demod)
cargo test --all-features       # the full suite: 397 unit tests +
                                # 55 integration test files + 171 doctests
                                # (164 run, 7 `rust,ignore` fences).
                                # Re-derive these rather than trusting them;
                                # they drift with every test added.
```

Ten suites carry ignored tests, 55 of them in `tests/` as of this
writing, plus the 7 ignored doctest fences below for 62 in a full run.
Re-derive with the attribute anchored to its own line:

```sh
grep -c '^[[:space:]]*#\[ignore' tests/*.rs | awk -F: '{s+=$2} END {print s}'
```

The anchor matters. An unanchored `grep -c '#\[ignore'` also counts every
prose mention of the attribute in a doc comment, and this file used to
quote its result as "62 in `tests/`" -- a number that was really the
whole-run total, doctests included, matching only because three comment
lines happened to pad the difference. Two of those comments were added
in the same change that noticed.

Nine of the ten suites need an external binary, the operator-provided
audio corpus, or the generated WAVs under `scratch/`, and pass with a
skip message when those are absent: `tests/oracle.rs`,
`tests/differential.rs`,
`tests/aprs_differential.rs`, `tests/il2p_differential.rs`,
`tests/wspr_differential.rs`, `tests/ft8_differential.rs`,
`tests/benchmark.rs`, `tests/corpus_aprs.rs` and `tests/cli.rs`. The
tenth, `tests/ber.rs`, skips its dense sweep purely on cost.

A handful of `rust,ignore` doctest fences are ignored too, reaching
rustdoc through `include_str!` from `README.md` and `docs/EMBEDDED.md`.
Keep that number falling: a `rust,ignore` fence is **never type-checked**,
so a documented example can go stale silently. Two README fences were
found broken that way, calling `WsprMessage::new` and `Ft8Tail::Grid`
with `&str` where both take a `MaidenheadGrid`; they are compiled fences
now. Reach for `rust,ignore` only when the example cannot compile in a
doctest at all, and say why in a comment.

Those counts are load-bearing, so re-derive them rather than trusting
what is written here: they were once stale by 114 tests and five
binaries. Do not hand-edit them from a diff. Run the suite and aggregate
`^test result:` lines, because the doctest and per-binary blocks are
easy to miscount by eye.

## Lint gates

All of these must be clean before any commit:

```sh
cargo fmt --check
cargo clippy --locked --all-features --all-targets -- -D warnings
scripts/check-coordinate-units.sh
shellcheck scripts/*.sh   # if you touched scripts/
actionlint                # if you touched .github/workflows/
```

`--locked` is not decoration. `Cargo.lock` is tracked (see `.gitignore`
for the reasoning), and without the flag the resolver is free to refresh
it in passing, which turns "clippy is clean" into "clippy is clean
against whatever crates.io served this morning" and rewrites a tracked
file while it is at it. Every cargo invocation in CI and in
`scripts/` passes it, so a lockfile that has drifted out of sync with
`Cargo.toml` fails loudly instead of being quietly repaired on each
machine that notices.

The third is a text audit, not a compiler pass, because the property it
checks is invisible to the compiler: `Latitude::new` and
`Longitude::new` count coordinate *storage* units, the wire carries
1/100 arc-minutes, both are `i64`, and a hundredths count handed to
`new` is a legal latitude near the equator. It rejects any construction
whose argument does not name its unit. Reach for
`Latitude::from_hundredths_minute` or `from_degrees_minutes` rather than
scaling by hand.

The last two lint the gates themselves. Six shell scripts in `scripts/`
decide whether CI is green, and for a long time nothing checked them, so
a quoting slip in an audit would have surfaced as the audit passing --
precisely the failure every one of those scripts was written to prevent.
Where shellcheck flags a deliberate idiom (the comma-joined cargo
feature sets in `check-embedded.sh`, the literal markdown backticks in
`check-coverage-citations.sh`) the suppression is written inline with the
reason, never waived wholesale. `actionlint` type-checks the workflows --
misspelled `needs:` entries, invalid contexts, unknown runner labels --
and runs shellcheck over their `run:` blocks; it does not read composite
actions, so `.github/actions/setup` relies on its own inline directives.

## Embedded matrix

The crate core is `#![no_std]`, allocation-free, and
`#![forbid(unsafe_code)]`; `src/lib.rs` also carries
`#![deny(missing_docs)]`. To prove the no_std claim,
`scripts/check-embedded.sh` cross-builds **every** no_std feature set
(each feature alone plus the combined
`mod,demod,nrzi,ax25,aprs,micE,kiss,tnc,g3ruh,fx25,il2p,wspr,ft8,m17,digipeat`
set) with `--no-default-features` for two bare-metal targets:

```sh
rustup target add riscv32imac-unknown-none-elf riscv32imc-unknown-none-elf \
    thumbv7em-none-eabihf
scripts/check-embedded.sh
```

The third target is not optional: the script also cross-builds the
detached `examples/esp32-riscv` sub-crate for both `imac` and `imc`.

Any change to the feature graph or to a `no_std` module must keep this
matrix green.

### The matrix proves the library builds, not that the tests do

`scripts/check-embedded.sh embedded` builds the **library** for each
feature set. Nothing used to compile the **test suite** for a partial
feature set, and CI only ever ran `--all-features`, so the gap stayed
invisible while 22 of 31 feature sets failed to compile their tests.
The causes were mundane and would each have been caught the day they
landed: `tests/{noise,oracle,roundtrip}.rs` reaching for
`yodel::modulator` / `yodel::demodulator` without declaring them, and
`src/scrambler.rs`'s own unit tests using `Vec` where there is no
`alloc`.

```sh
scripts/check-embedded.sh tests     # host-side, cargo test --no-run per set
scripts/check-embedded.sh           # both passes
```

The test sweep runs for the **host** target, because bare metal cannot
host a test harness, and uses `--no-run`, which catches every error of
this class in about a minute. Both passes gate CI as separate jobs.

When a test file needs a feature its `cfg` gate does not guarantee, add
a `[[test]]` block with `required-features` in `Cargo.toml` (there are
ten, each with a comment naming the symbol that forced it) so smaller
feature sets **skip** the file instead of failing to compile it. Prefer
a per-test `#[cfg(feature = "…")]` when only one test is responsible:
`required-features` is the blunter tool and silently costs coverage in
every set that falls short of it.

## Feature-flag expectations

Each Cargo feature gates exactly one layer, and the dependency chain
must match what those layers require of each other (see the commented
`[features]` table in `Cargo.toml` and the feature matrix in
`README.md`):

- everything except `std`, `wav`, `cli`, `capture` and `async` is
  `no_std`, and everything except those and `alloc` is allocation-free.
  `alloc` is `no_std` but opt-in and off by default; it adds heap
  conveniences such as `AprsPacket::to_vec` that allocate and grow;
- `std` implies `alloc` and pulls in no dependency; WAV I/O (the
  `hound` codec) lives behind the separate `wav` feature;
- `cli` is the aggregate for the binary (`wav`, `tnc`, `micE`,
  `kiss`, `fx25`, `il2p`, `wspr`, `ft8`, `m17`, plus `clap`);
- no protocol feature is in the default set (`default = ["mod",
  "demod"]`).

New functionality that adds a dependency between layers must be
reflected in the feature graph, the README table, and
`scripts/check-embedded.sh`.

## Pinned benchmark rows

`tests/benchmark.rs` pins the receiver's decode performance on the
operator-provided corpus and on fixed synthetic vectors. The pinned
rows are **regression floors**: they must never be lowered. Raise them
whenever the record improves; a change that reduces any count is
rejected.

| Row | Floor | Where |
|---|---|---|
| corpus track 01 | 999 | `TRACKS` |
| corpus track 02 | 985 | `TRACKS` |
| corpus track 03 | **100, exact** | `TRACKS` (the clean canary) |
| corpus track 04 | 98 | `TRACKS` |
| synthetic 1200 baud | 74 | `SYNTHETIC_MIN_FRAMES` |
| synthetic 300 baud | 74 | `SYNTHETIC_300_MIN_FRAMES` |
| synthetic FX.25 | 92 | `SYNTHETIC_FX25_MIN_FRAMES` |

The APRS layer has its own floors. The frame counts above cannot see
them, because a frame decodes whether or not its fields were understood:

| Metric | Floor | Where |
|---|---|---|
| structured coverage | 93.0% (measured 93.1%) | `tests/corpus_aprs.rs::MIN_STRUCTURED_PERCENT` |
| total frames | 2100 (measured 2182) | `MIN_FRAMES` |
| per-field decode counts | `/A=` altitude 250, course/speed 190, PHG 130, wind 78, Mic-E altitude 800, Mic-E altitude behind a device prefix 590, status timestamp 75 | `MIN_FIELDS` |
| fields cross-checked against an independent decoder | one row per field | `tests/aprs_differential.rs::MIN_COMPARED` |
| coverage gap vs an independent decoder | one row per field | `MAX_GAP` |

`tests/aprs_differential.rs` compares **every** field it can, not just
the position: course, speed (in both units the reference prints),
altitude, radio range, `PHG`, all nine weather measurements, and the
symbol. Full-field comparison is a requirement here. Comparing positions
alone left 650 data-extension values in the corpus ratchets that nothing
outside this repository had ever looked at, and the first run that
compared them found two value defects and three missing wire formats. A
count of recovered fields says nothing about whether the values are
right.

The symbol is compared as a *relation* rather than a value, because
neither implementation's symbol chart can be derived from the other's:
one wire pair must always draw one description out of the reference,
and one description must always come from one wire pair. Reading the
symbol from the wrong offset in any single format breaks that
immediately.

### The corpus only contains what was on the air that afternoon

A second test in the same file, `synthetic_formats_agree_with_reference_decoder`,
needs **no corpus at all**: the reference decoder reads monitor text on
standard input, so a field comparison needs only our own builders. It
exists because the corpus is a sample, not a specification, and the
sample has holes:

| Format | Corpus frames |
|---|---:|
| uncompressed position | 462 |
| **base-91 compressed position** | **0** |
| **`RNG` radio range** | **0** |
| **`DFS` direction finding** | **0** |

So the compressed family had no independent verification of any kind,
despite being a first-class APRS format whose speed is an exponent of
1.08 and whose altitude is an exponent of 1.002. `tests/differential.rs`
looks like it covers this and does not: it builds a compressed position,
parses it with **our own** decoder, and checks the reference recovers
the same *bytes* off the air. That proves the modem. Nobody was asking
the reference what latitude it read.

Two rules for adding cases there, both learned by getting them wrong:

1. **Compare the re-parsed bytes, not the packet you asked for.** The
   compressed encodings discard precision; 500 knots is not
   representable and the nearest wire value means 508.6. Comparing the
   request against the reference's reading of the bytes produces
   disagreements that are nobody's bug.
2. **Sweep to the top of the range.** A 0.1% error in the altitude base
   is 20% wrong at 100 000 ft and 2× wrong at a million (verified by
   mutation), but invisible at 100.

Still carrying no independent verification, and worth closing the same
way: status report text (126 corpus frames, never compared), station
capabilities (8), Ultimeter records (57), and timestamps as values.
`DFS` cannot be done this way, because the reference does not decode it.

## Three ways a test suite lies to you

All three were found in this suite.

### A set-but-wrong `YODEL_REF_*` path used to pass

The tier-4 suites skip when their binary is absent, which is right: a
contributor without it must still get a green `cargo test`. They used to
skip just as quietly when the variable was **set to a path that does not
exist**, so a single typo turned an entire interoperability suite green
while testing nothing.

The rule now, in every suite: **unset skips, set-but-wrong fails.** If
you typed a path you meant to run the test. `tests/differential.rs` had
always asserted this; the others had drifted and have since been
converged on it.

"Every suite" was itself an overclaim the first time it was written
here, which is worth recording because it is the same failure one level
up. `tests/oracle.rs`, the largest tier-4 suite, had not been converged
at all: with the variables unset it **failed** 17 of its 31 ignored
tests, so `cargo test -- --ignored` was red for any contributor without
the binaries. There was also a two-variable hole in both `oracle.rs` and
`differential.rs`: because `is_none()` was tested before the path was
validated, `YODEL_REF_GEN=/typo` with `YODEL_REF_DECODE` unset skipped
in silence. Resolve every variable *before* deciding to skip.

This has already cost something. Two suites that looked green had
**never executed a single comparison**: the WSPR differential passed the
wrong flag to the reference encoder and then looked for 162 symbols on
one line when the encoder wraps them, so a panic was its only reachable
outcome; and the FT8 "our audio → their decoder" leg omitted the
decoder's required positional arguments. A decoder handed a bare
filename prints usage and exits **zero**, which is indistinguishable
from "ran and heard nothing". Assert the exit status of every external
binary you shell out to.

### Assertions inside a loop over an empty list pass

Every assertion in the WSPR and FT8 differentials lives inside a loop
over a case list. An empty list would make all six tests pass having
compared nothing, and no amount of reading the output would show it.
Each now asserts a `MIN_CASES` floor, which doubles as documentation of
what "13/13" is out of.

The general shape to watch for: *if the input set were empty, would this
test still pass?* Ratchet floors (`MIN_COMPARED`) are the same defence
at the other end of the file.

### A suite CI compiles but never runs rots at the fixtures

CI compiles every `#[ignore]`d suite -- `--all-features` builds them,
and `scripts/check-embedded.sh tests` builds them once per feature set --
so an `#[ignore]`d suite is protected against the errors a compiler can
see and against nothing else. `tests/differential.rs` and
`tests/aprs_differential.rs` had **fixtures on the old coordinate unit**:
`Latitude::new(49 * 6000 + 350)` and a `rand_lat` composing
`deg * 6000 + min * 100 + hundredths`, handed straight to a constructor
that counts storage units. Both still compiled. Both are the exact
hazard `tests/coordinate_paths.rs` was written to prevent, right down to
the literal in its own header comment.

The cost was total: every one of the 320 differential cases collapsed to
0000.00N/00000.00W, and the very first case failed its own
encode-decode identity, so the whole suite panicked during corpus
generation before it ever reached the reference binaries. Any run would
have said so immediately -- nobody ran one.

Two lessons, and the second is the one that generalises:

1. **Run the ignored suites after touching anything they name.** A unit
   change, a constructor rename, a wire-format tweak: `cargo test
   --release -- --ignored` with the `YODEL_REF_*` variables set, before
   the commit that claims a tier-4 number.
2. **A fixture written in a bare literal outlives the unit it was
   written in.** `tests/coordinate_paths.rs` says this in its header and
   the differentials violated it anyway, because they are the files
   nobody re-reads. Compose fixtures through the named constant
   (`UNITS_PER_HUNDREDTH_MINUTE`) or a physical-quantity constructor, so
   the compiler or the value moves with the unit.

### Checking that every public function is exercised

No public function should be reachable by users and by nothing else.
The check is mechanical, so it is worth re-running after adding API. It
found eleven unexercised functions, two of them added the same day:

```sh
# For each `pub fn` in src/ (excluding the binary), is its name
# mentioned anywhere in tests/, examples/, a doc comment, an in-module
# test, or the README?
```

**That recipe has since saturated: run today it reports zero, and it is
no longer measuring anything.** Names are matched bare, so a generic
one (`new`, `parse`, `build`, `len`, `push`) is cleared by any single
hit among dozens of definitions; 146 of 632 `pub fn`s share a name
with another and can never be attributed. Prose in a doc comment counts
too, so an intra-doc link like ``[`Demodulator::with_discriminator`]``
clears the very function it is documenting.

The replacement requires a **call**, not a mention, and splits each
`src/` file at its `#[cfg(test)]` boundary so implementation code cannot
vouch for itself:

```text
# For each `pub fn` in src/ (excluding src/bin/ and in-module test
# bodies), does `name(` appear in: tests/, examples/, README.md, a
# ///-or-//! doc comment, or an in-module #[cfg(test)] body?
# Report anything with no call site. 632 definitions, 370 distinct
# names; the answer should be zero.
```

Run that way it immediately found four more: `wav::check_spec`,
`wav::decode_frames`, `ft8::llrs_from_energies` and
`ax25::fcs::locate_single_bit_error`. All four are public and all four
are reached only from other `src/` code, so their error paths and edge
behaviour were unverified. It is also worth listing the functions
called *only* from an in-module `#[cfg(test)]` block (28 of them): that
is legitimate coverage, but it means no integration test, example or
doctest demonstrates the function to a user.

`tests/coverage_fill.rs` is where the answers go. Note that a builder
being *called* is not enough: those tests go builder → build → parse →
assert equal, because a builder that writes the wrong field still runs
fine. The same applies to an accessor, so assert the value. Where the
function is a seam (`Demodulator::with_discriminator` takes a
caller-supplied `Discriminator`), implement the trait in the test and
add a negative control, so the test fails if the caller's object is
ignored.

## Invariants

- The library core stays `#![no_std]` with **no heap allocation**:
  builders write into caller-provided buffers, parsers borrow from
  the input, transmit paths are lazy iterators. No type in the core
  may own a growable buffer or return a collection. This applies to
  the crate's own unit tests too: they compile `no_std` under a
  feature set without `alloc`, so reaching for a `Vec` in a
  `#[cfg(test)]` module breaks the feature matrix.
- `#![forbid(unsafe_code)]` in `src/lib.rs`: no unsafe in the library,
  ever. The attribute is crate-local and does **not** reach integration
  tests; there is exactly one exception, the `GlobalAlloc` impl in
  `tests/no_alloc.rs`, which is the allocation guard itself and cannot
  be written in safe Rust. Adding a second one needs a reason this
  good.
- Determinism: no wall clock or unseeded randomness in tests; every
  failure must reproduce exactly.

### Design invariants that are load-bearing

These are requirements, not style preferences. Each exists because
breaking it makes a wrong program *compile and pass tests*, which is the
only failure mode this crate cannot test its way out of.

- **Validate on transmit, preserve on receive.** A builder rejects what
  it cannot represent; a parser never discards bytes it did not
  understand. Every receive outcome keeps the received bytes reachable
  on `Decoded::info`, malformed and unsupported ones included, so a
  caller that only wants to forward never has to parse at all.
  Preservation lives there and nowhere else; the next two invariants are
  why it must not live in `build`.

- **`build` must factor through the typed value.** The bytes a builder
  writes are a function of the parsed value and of nothing else. A
  builder is never handed the wire it came from, and no type keeps a
  borrowed slice of its input beside the parsed value to re-emit later.
  Those "raw carriers" are rejected, and the reason is not taste.

  Write parse as a partial function `p : W -> V` and build as
  `b : V -> W`. The rebuild-fidelity check is the predicate
  `[b(p(w)) = w]`, and **all of its diagnostic power comes from `b` not
  having seen `w`**. Hand `b` the input and its signature becomes
  `b' : V x W -> W`; then `b'(p(w), w) = w` is available for free and
  the predicate is a tautology that reports nothing, on any defect.

  This is not hypothetical. The telemetry parser once clamped analog
  values above 255. The wire said 510, `b` wrote 255 from the value it
  held, and the mismatch is what exposed it. Under `b'` the rebuild
  would have reproduced 510 and the defect would have shipped.

  A corollary worth internalising: two wire spellings of one value
  cannot both be returned, so byte-exact rebuild is a **measurement,
  not a target**, and the question is never whether a rebuild moved a
  packet but which way it moved. `docs/APRS_CONFORMANCE.md` section 4
  carries the F1 to F5 vocabulary for answering that;
  `tests/rebuild_fidelity.rs` and the per-kind floors in
  `tests/corpus_aprs.rs` are where the measurement lives. Two shipped
  decisions follow directly from this rule: the weather builder omits an
  absent field rather than dotting it, and the compressed `csT` builder
  inverts the *parser* rather than the underlying power.

- **Relaying is not canonicalising.** A digipeater's authority is the
  AX.25 header: find the first unused hop, decide whether it is
  addressed here, set the H bit, decrement `WIDEn-N`, re-transmit. The
  information field is opaque to it. Split a frame as `w = (h, i)` and
  the two operations are different maps:

  ```text
  canonicalise   k(h, i) = (h, b(p(i)))          reads the payload
  digipeat       D(h, i) = (b_h(t(p_h(h))), i)   does not
  ```

  So on a relay path the payload is carried by **identity**, and byte
  fidelity on it is free and total, including for payloads no parser
  here accepts. Never build a forwarding path out of parse-then-build in
  order to "preserve" bytes: `k` is partial and is undefined on exactly
  the frames a relay is obliged to forward.

  The seam is enforced by the signatures, and it must stay that way.
  `relay_decision` takes the path and nothing else, so the payload
  cannot reach the decision; `UiFrame::with_hops` borrows the
  information field rather than rebuilding it, so the decision cannot
  reach the payload. `tests/digipeat_laws.rs` sweeps the three laws that
  follow: payload transparency, hop-budget termination (every relay
  spends exactly one hop of the budget, so a flood is finite), and local
  loop freedom.
- **Every `AprsPacket` variant must be buildable from its information
  field alone.** `AprsPacket::build` writes the information field, and
  `build_ui_frame` takes the destination from the caller. A variant whose
  meaning depends on the destination (Mic-E is the only one) would make
  `build_ui_frame(&MicE(report), some_tocall, ..)` compile, return `Ok`,
  and put a **wrong position on the air**. Frame-level and receive-only
  formats belong on `Decoded`, not `AprsPacket`; that split is a
  transmit-safety invariant, not a taxonomy.
- **No tuple conversions for coordinate pairs.** The crate provides no
  `Coordinates::to_degrees() -> (f64, f64)` and no
  `From<(Latitude, Longitude)>`, because a transposed destructuring
  compiles silently and puts a station in the wrong hemisphere. The
  per-axis `Latitude::to_degrees()` / `Longitude::to_degrees()` are the
  form that cannot be transposed. `src/geo.rs` carries the full note.
  Generalise this whenever two same-typed quantities travel together:
  prefer distinct types over positional pairs.
- **Physical quantities carry their unit in the type.** A bare integer
  once let a gust value reach a wind-speed field. That was a silent 15%
  error no round-trip test could see, because it survives the round
  trip. `Option<Speed>` makes it unrepresentable. See `src/units.rs`.
- **The `Symbol` pattern for new wire types**: a validated typed core,
  named constants for the common values, and an infallible raw hatch so
  that any bytes seen on air round-trip exactly. Follow it; do not
  invent a fifth shape.

## Documentation standards

- `#![deny(missing_docs)]` is on: every public item needs a doc
  comment, and the comment should explain units, wire layouts, and
  invariants, not just restate the name.
- Public API examples belong in doctests so they compile and run
  under `cargo test`; README code fences are doctests too (via
  feature-gated `fn main` wrappers) and must stay green.
- **A `rust,ignore` fence is not a doctest in any useful sense.**
  Measured: rustdoc *parses* it to build a harness entry and stops
  there, never type-checking, compiling or running the body, in either
  run mode. A fence referencing a type that does not exist passes; a
  fence with a stray token fails, but only under `--ignored`. So the
  ten `rust,ignore` fences in `README.md` cannot detect an API rename.
  What protects them from drift is live coverage of the same API in
  `tests/` and `examples/` (mapped fence-by-fence in
  `docs/COVERAGE.md`). Reach for `ignore` only when the code is meant
  to be wrong or cannot be run, and prefer `text` when it is prose
  rather than code. An `ignore` fence whose body was not valid Rust
  made `cargo test -- --ignored` fail for everyone until it was found.

## Examples are reference implementations, not demos

An example in `examples/` should read as **the program someone would
deploy**. A reader has to be able to open it, understand it, and copy
it. Every line that exists only to make the example demonstrate itself
is a line they have to identify and delete first, and worse, a line that
teaches them the wrong thing.

So the rule is: **write it the way the deployed program would be
written.** If a reader wants it to terminate early, print more, or run
faster, they can add that themselves; those changes are easy and are
theirs to choose.

Concretely, do not add:

- **Accelerated or scaled clocks.** No `TIME_SCALE`, no periods divided
  by a demo factor. Use the real interval. If that means the example
  prints something every 45 seconds, it prints something every 45
  seconds. That *is* the behaviour being documented.
- **Artificial run limits baked into the logic.** A station beacons until
  it is switched off, so its loop is `loop { }` and the reader stops it
  with Ctrl-C. A `FLIGHT_MS` constant implies the firmware knows how long
  the flight is, which no tracker does; it invents a domain concept to
  serve the example.

  An **explicit, opt-in** bound is a different thing and is welcome *in
  a host example*: `--run-for <SECONDS>`, defaulting to never, lets
  someone try it or run it in CI without hunting for Ctrl-C. The test is
  whether the default path is the real one, so say in the doc header that
  the default is infinite.

  **Not in a firmware-shaped example.** A Cortex-M or ESP32 target has no
  command line, no `std::env::args`, and no process to pass a flag to, so
  an argument-parsing call in the middle of a superloop misrepresents
  the target. That is the kind of host-only scaffolding this section
  exists to keep out. Bound those runs from the shell, where the concern
  belongs:

  ```sh
  timeout 5 cargo run --release --example balloon_tracker_rtic --features tnc
  ```

  The general form: **ask what the target is capable of.** If the real
  hardware could not run the line you are writing, it does not belong in
  the example, however convenient it is.
- **`--demo`, `--slow`, `--fast` style flags.** A flag earns its place
  only if a reader would want it in their own program. A flag that
  exists to show off a property is scaffolding: demonstrate the property
  in the normal path instead, so it is seen without being asked for.
- **Assertions and self-checks for the example's own benefit.** Invariants
  that real firmware would check, such as "the ring never overran",
  belong inline where the real thing would check them. A summary block of
  `assert!`s at the end is a test wearing an example's clothes; if the
  behaviour needs testing, test it in `tests/`.
- **Synthesized input where real input is the point.** Prefer taking a
  file or a stream, as the real program would, and document how to
  produce one (`encode_wav`, `yodel gen`). A decoder that invents its
  own signal teaches nothing about wiring up a decoder.

What is legitimately not real, and how to handle it:

- **Hardware you cannot have on a host.** Stub the single call that
  needs it (the I2C barometer read, the DAC write) and say in a comment
  what goes there instead. Keep the surrounding structure exactly as it
  would be, so the shape is still correct.
- **A framework that needs a real target.** `examples/balloon_tracker_rtic.rs`
  keeps the RTIC `#[app]` skeleton in comments because it needs an
  interrupt controller, while the task bodies stay real code.

Every example opens with a **Scenario / Hardware / Features** block, so
a reader knows within three lines whether it is for them: what job it
does and on which side of the radio link, what class of machine it
targets (full OS, `no_std` MCU with an executor, `no_std` MCU without
one), and the exact feature list, which must match its
`required-features` in `Cargo.toml`.

There are two genres, and the failure mode is drifting between them.
**Concept examples** teach one API: one entry point, no flags, short
enough to read in one screen. **Application examples** show a realistic
program and may be much larger, with their pure logic `#[path]`-included
by `tests/app_examples.rs` so that it is under assertion. Either genre
is fine; a concept example built like an application one is not.

## The `reference/` directory

`reference/` (gitignored, operator-provided) holds external material
used **for study only**. One subdirectory per source, e.g.:

```text
reference/
  <implementation>/   a GPL-licensed C TNC implementation (git checkout)
  <spec>/             the APRS protocol reference PDFs (git checkout)
  <weak-signal>/      a GPL-licensed weak-signal suite, for the WSPR
                      encoder/generator/decoder binaries (git checkout)
```

Rules, in order of importance:

- concepts, protocol behavior and **testing approach** may be studied
  and reimplemented from first principles;
- GPL **code, tables, and test fixtures must never be copied** into
  this MIT/Apache-2.0 crate, not even "just the constants";

### The tempting exception, and why it is still forbidden

This one recurs, so it is written down rather than re-litigated.

The case below is about vectors that reach you **inside a copyleft
repository**. Vectors published by a protocol's own authors, in a
standalone specification that licenses them for implementers, are a
different thing and are allowed; "The permitted exception" below covers
them. `tests/il2p.rs` uses IL2P v0.6's "Example Encoded Packets" on
exactly that basis, because the spec contributes them "for use as
verification samples to help individuals implementing their own IL2P
encoders and decoders".

Several of these projects ship **published test vectors** in a manual
page, a README or a specification document: a message together with its
intermediate encodings. They are exactly what a tier-2 test wants, they
would remove a whole mode's dependence on an external binary, and the
argument that "the output of a deterministic algorithm is a fact, and
facts are not copyrightable" is a respectable one.

**Do not use them anyway.** Three reasons, in order:

1. The rule above says "test fixtures", and a published vector is a
   test fixture. It also says "not even just the constants". There is
   no reading under which a pasted bit string is outside it.
2. The licences are worse than they look. Implementation repositories
   are usually GPL-2.0 or GPL-3.0, which is expected. Protocol
   **documents** in the same repositories are often **GFDL**, which is
   share-alike for documentation and no more compatible with
   MIT/Apache-2.0 than the GPL is. "It's only the spec" is not a
   defence when the spec carries a copyleft licence of its own. Check
   the document's licence separately from the code's.
3. The boundary is drawn where it needs no judgement call. A rule you
   have to reason about at 2 a.m. is a rule you will eventually reason
   your way around.

**What to do instead.** Run the other implementation as an external
black box behind a `YODEL_REF_*` variable, at test time, and compare.
GPLv2 §0 and GPLv3 §2 both state that running a program is unrestricted
and that its output is not a derived work. That settled reading is the
basis of the tier-4 suites. The cost is that the test is `#[ignore]`d
and cannot gate CI, which is why tiers 1–2 must carry their own weight
(see "Test tiers").

Where a mode's composed encoding therefore has **no** tier-2 known
answer, say so in that module's test file rather than quietly relying
on tier 4. `tests/ft8.rs` does.

### The permitted exception: data the authors put in the public domain

The rule above is about **copyleft** material. It is not a rule against
protocol constants as such, and reading it that way has a cost: it pushes
you toward transcribing a table "from spec knowledge" and asserting
clean-room provenance, when a citable public-domain source existed all
along. That is strictly worse: the same bytes with a weaker paper trail.

So: **where the protocol's own authors have published the tables under
permissive or public-domain terms, use those, vendor them, and cite
them.** The test is the licence on the artifact you are copying from, not
whether the artifact is a table.

The worked example is FT8. Its LDPC matrices live in a GPLv3 source file
*and* in `ft4_ft8_protocols.tgz`, the authors' own resource package,
which §9 of the QEX paper places in the public domain and explicitly
carves out of the GPL. This crate takes the second route:
`third_party/ft4_ft8_public/` vendors `generator.dat` and `parity.dat`
with the dedication text, and two tier-1 tests check the embedded
constants against them.

Three obligations come with taking this route:

1. **Vendor the source file**, do not just cite a URL. Primary URLs die;
   the FT8 package's Princeton link already 403s and had to be recovered
   from a web archive. A 7 KB file in `third_party/` is cheaper than
   re-establishing provenance in five years.
2. **Make the check executable.** A comment claiming provenance is worth
   little. `src/ft8.rs` carried one that was *false*: it asserted the
   generator matrix had not been copied from any GPL source file while
   storing it in that file's exact 23-hex-character row format. No
   reviewer caught it because there was nothing to run.
3. **Do not copy the other implementation's representation.** Values may
   be public domain while the *encoding* is a formatting choice belonging
   to a particular source file. 91 bits is not a whole number of hex
   digits, so a 23-character row is a fingerprint of the file that chose
   that padding. Pick your own encoding and derive it from the
   public-domain form in a test.

Read the licence of the specific artifact. A public-domain dedication may
also carry **conditions**. FT8's does, and they bind any implementation
that uses the name (see the "Protocol licence and conditions" section of
the `src/ft8.rs` module docs).
- a **study reference's name must not be written** in any tracked file
  when the context is comparison, benchmarking or behaviour; call it
  "the reference". (The spec PDFs are a different matter: they are a
  published protocol document and may be cited by name and version. See
  `docs/APRS_CONFORMANCE.md`.)

### The naming rule stops at attribution

The rule above exists so that benchmark rows, decision logs and
behavioural notes do not read as a running comparison against a named
project, and so nothing tracked depends on the study checkout. It is
**not** a rule against attribution, and applying it there does damage.

**Where naming an upstream project is what makes a licensing statement
true, name it.** "These tables are carved out of the reference's
copyleft licence" states nothing checkable; "§9 of the QEX paper places
them in the public domain and excludes them from WSJT-X's GPLv3" is a
fact a reader can verify and a court could rely on. Declining to name the
project whose authors' grant this crate depends on would be a discourtesy
to them and a disservice to anyone auditing us.

So, narrowly: an upstream implementation, and the specific file within
it, may be named in tracked files **when the purpose is attribution,
licensing provenance, or recording a correction to a provenance claim**.
`src/ft8.rs` and `third_party/ft4_ft8_public/README.md` do exactly that.
Everywhere else the rule stands unchanged.

Naming a project in an attribution note does not license copying from it.
The two rules are independent.

### Write where a constant came from, never where it didn't

This crate used to sprinkle assertions like "no GPL source was consulted
or copied" and "clean-room implementation" through its module docs. They
have all been removed, and new ones should not be added.

They are the wrong tool in three ways:

1. **They cannot be checked.** A reader has no way to verify a negative
   about your process, so the sentence carries no information. A citation
   can be followed: document, version, section. A vendored file with a
   test against it can be *run*.
2. **They can be wrong without anyone noticing.** `src/ft8.rs` asserted
   "NOT copied from any GPL source file" directly above a table in
   another project's exact row format, and it survived review because
   there was nothing to check it against.
3. **They invite the suspicion they are meant to deflect.** A codebase
   that repeatedly volunteers its innocence reads worse than one that
   cites its sources, in the same way that unprompted denials generally
   do.

So: **state the source.** "Implemented from the M17 specification
(spec.m17project.org), constants cited per spec part" does the whole job.
If a table has a citable published origin, cite it; if that origin is
vendorable, vendor it and add the test. If a value is self-derived,
label it self-derived and show the derivation. `tests/edge_cases.rs`'s
G3RUH PN-sequence comment derives all 48 bits by hand, and a reader can
check that derivation line by line.

The substantive rule is unchanged and is not a documentation matter: do
not copy from copyleft sources. It is satisfied by conduct, and removing
these assertions does not relax it.

Because a study reference's name may not appear in tracked files for the
purposes above, nothing tracked may hardcode a path into `reference/`.
The oracle and differential test suites, and `scripts/benchmark.sh`, all
locate the binaries through the `YODEL_REF_GEN` / `YODEL_REF_DECODE`
environment variables and treat them purely as external black boxes:

```sh
export YODEL_REF_GEN=/path/to/reference-generator
export YODEL_REF_DECODE=/path/to/reference-decoder
cargo test --all-features -- --ignored
```

#### Name the interface, because the name is not available

A reference project may not be named in a tracked file, which means a
variable cannot be documented as "point this at *X*". Describe the
**interface** instead. It is not a courtesy: two binaries shipped by the
same project, with the same stem, can differ entirely, and "wav-writing
generator" does not choose between them. That cost a debugging session
here -- the WSPR generator variable was pointed at a sibling that writes
a `.c2` file, the run failed with an empty message, and nothing said
what had been expected.

| Variable | Interface it must satisfy |
|---|---|
| `YODEL_REF_GEN` | AX.25 generator. `-n <count> -o <file.wav>`, plus `-B <baud>` and `-X 1` (FX.25) and `-I 1` (IL2P). Writes a WAV. |
| `YODEL_REF_DECODE` | AX.25 decoder. Takes a WAV path, accepts `-B <baud>` and `-P <profile>`, prints one monitor line per frame and a `<n> packets decoded` trailer. |
| `YODEL_REF_APRS` | APRS **text** decoder. Reads TNC2 monitor lines on stdin, prints a human-readable dissection. Not the audio decoder above. |
| `YODEL_REF_WSPR_ENCODE` | Prints WSPR channel symbols for a message (a `-c`-style flag). |
| `YODEL_REF_WSPR_GEN` | `"message" f0 DT fspread delay nwav nfiles snr`, positional, writes a **WAV**. Distinct from the encoder above even where the two share a name. |
| `YODEL_REF_WSPR_DECODE` | Takes a WSPR WAV or `.c2` and prints decoded messages. |
| `YODEL_REF_FT8_ENCODE` | Prints FT8 **intermediates**: the 77 source bits, the CRC, the parity bits and the 79 symbols. |
| `YODEL_REF_FT8_GEN` | `"message" f0 DT fdop delay nfiles snr`, positional, writes a WAV. |
| `YODEL_REF_FT8_DECODE` | `<MaxIt> <Norder> <file.wav>`, prints decoded messages. |

Keep the paths in a gitignored file rather than retyping them --
`scratch/` is already ignored, so `scratch/ref-env.sh` full of `export`
lines works and never reaches the tree.

Every suite asserts the exit status of what it runs and prints **argv,
the status and both streams** on failure. Keep it that way: a CLI that
rejects its arguments prints usage to *stdout*, so a message carrying
only stderr reports a failure and no reason.

**On macOS**, a Homebrew upgrade can leave a reference binary linked
against a library version that no longer exists, and it will then die in
the dynamic loader before `main`. Setting `DYLD_LIBRARY_PATH` is not a
reliable fix, because macOS strips `DYLD_*` from the environment of
SIP-protected shells, so it may not survive to the binary. Relink the
binary once instead:

```sh
install_name_tool -change /old/path/libfoo.dylib /actual/path/libfoo.dylib BINARY
codesign -f -s - BINARY
```

### WSPR interoperability

`tests/wspr_differential.rs` is the same idea one layer up. It exists
because WSPR was previously validated **entirely by self-consistency**:
a closed transmit-to-receive loop with no external audio and no
independent implementation anywhere in the loop. That is the exact
shape of the IL2P defect (`docs/APRS_CONFORMANCE.md` §6.1), where every
round-trip test passed while the mode could not exchange a frame with
anybody.

It needs three binaries from an independent WSPR implementation, again
as black boxes behind environment variables:

```sh
export YODEL_REF_WSPR_ENCODE=/path/to/symbol-printing encoder
export YODEL_REF_WSPR_GEN=/path/to/wav-writing generator
export YODEL_REF_WSPR_DECODE=/path/to/decoder
cargo test --release --all-features --test wspr_differential -- --ignored --nocapture
```

The three tests are, in increasing scope: our 162 channel symbols
against theirs (the cheapest and strongest, because it compares the
whole composed encoding, which is where a wrong constant hides); our
audio through their decoder; and their audio through ours. Each skips
with a message when its binary is absent.

### FT8 interoperability

`tests/ft8_differential.rs` is the same three legs again, and needs
three more binaries from an independent FT8 implementation:

```sh
export YODEL_REF_FT8_ENCODE=/path/to/symbol-printing encoder
export YODEL_REF_FT8_GEN=/path/to/wav-writing generator
export YODEL_REF_FT8_DECODE=/path/to/decoder
cargo test --release --all-features --test ft8_differential -- --ignored --nocapture
```

Its encoder leg compares **four** values per message rather than one:
the 77 source-encoded bits, the 14-bit CRC, the 83 LDPC parity bits and
the 79 channel symbols. The reference encoder prints its intermediates,
so a failure names the stage it occurred in instead of reporting only
that something differs.

**Do not skip the encoder leg on the grounds that the audio legs are
stronger.** They are not. Both defects this suite found (see
`docs/APRS_CONFORMANCE.md` §6.3) leave the transmission perfectly
intelligible to the other implementation, and the audio legs were
measured passing 13/13 with both defects present. "The other end
understood me" is a weaker claim than "I sent what everyone else
sends", and only the composed-encoding comparison tells them apart.

Caveat on "13/13 in both directions", which this file used to claim: the
encoder leg and the *their audio → our decoder* leg do pass 13/13. The
third leg, **our audio → their decoder, has never run**: its 13/13 was
the vacuous pass described above. With the invocation corrected the
reference decoder dies with SIGBUS on WAVs written by its own sibling
generator, so the leg now fails loudly and says the failure is not
evidence about our transmission. Treat that direction as **unverified**,
not as passing.

M17 still has **no** equivalent leg and is validated only by
self-consistency plus component known-answer tests. The gap is known and
recorded.

## The `corpus/` directory

`corpus/` (gitignored, operator-provided) holds real off-air recordings:
several hundred MB of 16-bit mono WAV captured from live VHF APRS
traffic, plus the disc image they came from. They are the ground truth
for `tests/benchmark.rs` (frame recovery counts), `tests/corpus_aprs.rs`
(APRS-layer structured coverage and content integrity), and
`scripts/benchmark.sh`.

**Licensing: use freely, never redistribute.** The recordings are a
published amateur-radio test disc, downloadable from its author's site,
whose stated purpose is exactly this: "intended to be played back
directly into TNCs to compare the performance of various TNCs." Using
them to test a decoder is the use they exist for. But the disc carries a
bare copyright notice and **grants no redistribution rights**, so:

- do **not** commit them, mirror them, or attach them to a release;
- do **not** commit derived audio either (excerpts, resamples, or
  transcodes are still derivative works);
- **do** commit derived *measurements*: frame counts, coverage
  percentages, decoded-field expectations. Facts about how our decoder
  performs are ours.

Each contributor obtains their own copy from the original source. Both
corpus suites are `#[ignore]`d and skip with a message when `corpus/` is
absent, so a contributor without it still gets a fully green
`cargo test`. Numbers derived from the corpus are pinned in
`docs/BENCHMARKS.md` and in the tests, so results stay reviewable
without the audio.

## Test tiers, and staying self-sufficient

The external material above is a convenience, not a dependency. Tests
are layered so the crate stands on its own:

| Tier | Needs | Gates CI | What it proves |
|---|---|---|---|
| 1 | nothing — in-repo, hermetic | yes | Correctness, round-trips, sensitivity (`tests/noise.rs`), **specificity** (`tests/false_positives.rs`) |
| 2 | nothing — in-repo vectors | yes | Conformance to the published spec, byte-for-byte |
| 3 | `corpus/` | no (`#[ignore]`) | Behavior on real traffic; regression floors |
| 4 | `reference/` binaries | no (`#[ignore]`) | Interoperability with an independent implementation (AX.25 *and* WSPR) |

Tier 1 and 2 must always be sufficient to catch a real regression. If a
bug can *only* be caught by tier 3 or 4, that is a gap in tiers 1–2;
close it by adding a synthetic or vector-based test that reproduces it.
The long-term goal is for `reference/` to be a cross-check we could
delete without losing confidence.

## Before a release

CI runs tiers 1 and 2 on every push. Tiers 3 and 4 run nowhere but on a
maintainer's machine, and the numbers this project advertises come from
them, so they are a release gate rather than a nicety.

Every claim below was found stale or unverifiable at least once, in each
case because the suite behind it had not been run since the change that
broke it.

```sh
# 1. Everything CI runs, locally.
cargo fmt --check
cargo clippy --locked --all-features --all-targets -- -D warnings
cargo test --locked --all-features --no-fail-fast
RUSTDOCFLAGS='-D warnings' cargo doc --locked --all-features --no-deps
shellcheck scripts/*.sh
actionlint
scripts/check-public-api-exercised.sh
scripts/check-coordinate-units.sh
scripts/check-coverage-citations.sh
scripts/check-embedded.sh
cargo publish --dry-run --locked --all-features

# 2. The corpus and reference tiers. See "The `reference/` directory"
#    for the variables; keep them in a gitignored scratch/ref-env.sh.
. scratch/ref-env.sh
scripts/gen-bench-inputs.sh
cargo test --release --all-features --no-fail-fast -- --ignored
```

`--no-fail-fast` is not optional: cargo stops at the first failing test
*binary*, and the ignored tier spans a dozen of them, so without it a
single early failure hides the state of everything after it
alphabetically.

The second command must report **every** ignored test passing, not
"passing or skipped". A skip means a variable is unset or an input is
missing, and a release measured with half the tier skipped is exactly
the situation this checklist exists to prevent. `61 passed` with one
skipped is a red result.

Then re-derive anything the release notes assert -- frame counts,
coverage percentages, `320/320` -- from that run's output rather than
from the previous release's notes.

## Commit hygiene

Group work into small, logical commits with imperative subjects, and
run all the gates above before each commit.
