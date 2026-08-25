# APRS conformance and type design

A gap analysis of `yodel`'s APRS layer against the current protocol
reference, grounded in measurements over the real off-air corpus.

This document is about the *record* level: the strict/lenient contract
and spec coverage. For the component level see the design invariants in
[`../CONTRIBUTING.md`](../CONTRIBUTING.md); for module layout see
[`ARCHITECTURE.md`](ARCHITECTURE.md).

## 1. Which spec are we implementing?

`yodel` cites "APRS 1.01" throughout. That citation is a defect.

`APRS101.pdf` is the 2000 document for protocol version 1.0.1. It is the
only edition the APRS Working Group ever formally approved, and its own
publisher now ships it with a cover note stating that it is
**obsolete**, that it "is missing decades of corrections,
clarifications, and new features", and that implementing from it "is
likely to produce something incompatible with contemporary practices."

The editions, and what standing each of them has:

| Version | Status |
|---|---|
| 1.0.1 (Aug 2000) | The only **approved** document. Obsolete. |
| 1.1 (2004) | Addendum **approved** by the Working Group, never merged into a document. Published only as an errata web page. |
| 1.2 (2004–) | **Proposed draft** addendum. Never approved. Contains the majority of modern practice. |
| `APRS12b.pdf` / `APRS12c.pdf` | **Unofficial compilations** by John Langner (WB2OSZ) merging 1.1 + the implemented parts of 1.2 into the 1.0.1 text. Draft B May 2024, Draft C Nov 2024. |

There is no active standards body. Bob Bruninga (WB4APR) is SK, and the
Working Group is described in the past tense. The de-facto reference is
the unofficial compilation at <https://github.com/wb2osz/aprsspec>, a
**living document with no release cadence**: within six months of
Draft B, Draft C changed a field width, rewrote a whole chapter, and
added four new "reality diverges from the spec" warnings.

**Recommendation.** Cite `APRS12c.pdf` (or later) as the working
reference, keep 1.0.1 named only where we implement the older behavior
on purpose, and record the draft letter we validated against so a reader
can tell how old the citation is. Do not claim conformance to a bare
version number. The claim to make is "targets the 1.2 compilation,
Draft C, with the deviations listed in §3".

**Partially adopted.** `src/aprs/extension.rs` names the *UNOFFICIAL
APRS Protocol Reference, Document Version Draft 1.2 c* (date of issue
November 2024) and says why. The rest of the module still says
"APRS 1.01": `aprs.rs`, `position.rs`, `weather.rs`, `message.rs`,
`mic_e.rs`, `object.rs`, `telemetry.rs`, `nmea.rs`, `ultimeter.rs`. The
modules added since (`status.rs`, `capabilities.rs`, `thirdparty.rs`)
cite a bare chapter number with no edition at all. Both remain defects.

## 2. Measured coverage over real traffic

`tests/benchmark.rs` pins how many AX.25 frames the demodulator recovers
from the corpus, but never asks whether those frames parse as APRS.
`tests/corpus_aprs.rs` closes that loop. Over 2182 frames demodulated
from four off-air VHF recordings:

| Outcome | Frames | Share |
|---|---:|---:|
| The information field alone produced a typed value (`Decoded::decode`) | 1138 | **52.2%** |
| Mic-E, which additionally needs the AX.25 destination (`Decoded::decode_frame`) | 894 | **41.0%** |
| No structured value at all | 150 | **6.9%** |

*(Of the 1138, `AprsPacket::parse` accounts for **763**. The other 375
are the receive-only forms that live on `Decoded` rather than on
`AprsPacket`: 268 raw NMEA, 57 Ultimeter, 50 third-party.)*

*(First measurement was 686 / 868 / 628, or 71.2% structured. The
over-strictness fixes in §2.1 moved it to 74.3%; implementing the
missing data types (§6) moved it to 93.0%, and the two Mic-E and
hemisphere leniency fixes to the **93.1%** above. `MIN_STRUCTURED_PERCENT`
in `tests/corpus_aprs.rs` carries the full ratchet history.)*

**Mic-E needs the AX.25 destination.** It decodes through
`Decoded::decode_frame(dest, info)` and arrives as `DecodedKind::MicE`;
an information field decoded *without* a destination is
`DecodedKind::NeedsDestination { dti }`, which is a true statement about
what is missing. `RxFrame::decoded()`, `OwnedFrame::decoded()` and
`aprs::decoded_from_ui` supply the destination. Damaged reports arrive
as `Malformed { error: MicE(BadLongitudeByte { .. }) }` rather than
filed beside the non-APRS beacons.

**Not an `AprsPacket` variant**, for a transmit-safety reason that
belongs on the record. `AprsPacket::build` writes the *information field
only*, and `build_ui_frame` takes the destination from its caller. An
`AprsPacket::MicE` would therefore let
`build_ui_frame(&MicE(report), some_tocall, …)` compile, return `Ok`,
and transmit a Mic-E information field under a tocall that contradicts
it: a wrong position on the air, from a call that succeeded.
`AprsPacket`'s invariant is that every variant is something the crate
can also *build*. The other half of that split is pinned as a law in
`tests/decoded_laws.rs`: for any information field that is not Mic-E,
`decode_frame(d, info).kind` equals `decode(info).kind` for **every**
`d`, so a destination address can never change the meaning of a
non-Mic-E packet.

**6.9% of correctly demodulated traffic produces nothing typed.**
`Decoded::decode` is total: whatever is left is labelled `Unsupported`
or `Malformed`, and `Decoded::info` always carries the bytes. MEASURED
composition of the surviving 150: **85** non-APRS beacons and
non-graphic leading bytes labelled `Unsupported`, **58** no-fix
`!0000.000/00000.000>` beacons (`BadHemisphere { got: 48 }`), **6**
Mic-E frames carrying a `0xBE` where a longitude byte belongs
(`MicE(BadLongitudeByte { got: 190, position: 1 })`), and **1** frame
corrupted mid-body (`BadDigit`, `0xF3` at offset 9). The table beside
`MIN_STRUCTURED_PERCENT` reads 85 / 58 / 6 / 1. Nearly all of it *ought*
to be rejected.

### 2.1 Where the untyped frames went

Every row below was confirmed against captured payloads; none of them
was inferred from an error name.

| Was | Now | Cause | Verdict |
|---:|---:|---|---|
| 325 | **0** | Unimplemented `$` raw NMEA / Ultimeter | `aprs::nmea` and `aprs::ultimeter`; 268 + 57 frames typed. See §6 |
| 58 | 58 | `!0000.000/00000.000>…` carries a digit where N/S belongs | Malformed, and the "position" is null island. Rejecting is defensible |
| 50 | **0** | Unimplemented `}` third-party | `aprs::thirdparty`; carries a complete nested packet. See §6 |
| 69 | 69 | `W`, `K`, `L`, `U` non-APRS beacons | Not APRS, so still not an `AprsPacket`. Labelled `DecodedKind::Unsupported` with the bytes kept, which is what the spec's "must be able to process them without ill effects" asks for |
| 35 | **0** | Weather-*symbol* position whose comment is not a `DDD/SSS` wind block | The symbol hints at a weather report without guaranteeing one, so the parser falls back to the position |
| 32 | **6** | `` ` `` Mic-E rejected by both layers | Mic-E no longer rejects a whole report over an out-of-spec symbol table byte, recovering 26. The 6 that remain carry a longitude byte outside the 38–127 chapter 10 permits, and declining to report a position report with no valid position is the intended behaviour. They arrive as `Malformed { dti: 0x60, error: MicE(BadLongitudeByte { got: 190, position: 1 }) }` |
| 24 | **0** | `tU2k` read as a `t` temperature tag | That is the Ultimeter-2000 unit code: a report legally ends with a software letter plus a 2–4 char unit code, and the spec permits any code |
| 16 | 16 | 0x0d, 0x20 leading bytes (10 + 6) | Corruption, correctly rejected. The `0xF3` originally counted here is one frame corrupted mid-body, reported as `BadDigit` at offset 9 |
| 8 | **0** | Unimplemented `<` station capabilities | `aprs::capabilities` |
| 8 | **0** | Message id length 6 | A legal five-character reply-ACK id (`MM}AA`) plus a stray trailing CR, now decoded via `MessageContent::reply_ack` |
| 2 | **0** | Lowercase hemisphere `n`/`w` | Accepted case-insensitively on receive. Building still emits upper case |

The four rows that did not close come to 149 of the 150 frames §2 counts
as untyped, and every one of them is rejected by an explicit rule.

Separately, **UI frames with the poll/final bit set (`0x13`)** are
accepted: `0x13` is the same frame type as `0x03` and APRS ignores P/F,
so `src/ax25/frame.rs` compares `control & CONTROL_PF_MASK` against
`CONTROL_UI` rather than testing for equality. Other U-format frames are
still rejected. Rejecting `0x13` cost no frames in *this* corpus, but it
also disabled bit-flip recovery, which is gated on a successful UI
parse.

## 3. Spec gaps in the modules we already have

Ordered by likelihood of silently producing wrong output.

| Area | Gap | Impact |
|---|---|---|
| `position.rs` | `!DAO!` datum/precision extension (1.1) unimplemented | Silently discards ~1 ft precision; a leading `!` may also confuse naive scanning |
| `position.rs` | Base-91 comment telemetry `\|ss1122\|` (1.2) unimplemented | Telemetry invisible |
| ~~`position.rs`~~ | ~~**Position ambiguity is rejected instead of represented.**~~ **FIXED** (`f56e5b4`). `parse_latlon` reads the blanked-digit count, `Position`/`Object`/`Item`/`PositionWeather` carry it, and `coordinates()` masks both axes with `geo::Ambiguity::mask`. MEASURED: 211 packets from 73 senders recovered, 0 corruption cases among the 418 space-bearing fields | was: spec-legal traffic dropped |
| ~~`mic_e.rs`~~ | ~~**Mic-E decodes ambiguity and then every accessor discards it.**~~ **FIXED** in two steps. The count reached `Coordinates` first; then `53c2756` applied it to the *position*, which is the half that mattered: Mic-E cannot blank a longitude digit, so chapter 10 leaves discarding the low-order digits to the receiver, and the crate was publishing a longitude up to 33 km finer than the sender declared. MEASURED: 38 of 280 axis readings were over-precise, now 0 | was: a **wrong** value rather than a missing one, the failure mode the crate's own goals rank worst |
| `telemetry.rs` | Analog range relaxed to 000-999 in 1.2; we clamp at 255. **Now separately observable** for the first time: before `41b3928` every telemetry rejection was the same `Truncated { expected: 34 }` | Valid packets rejected, ~1 700 of them |
| ~~`telemetry.rs`~~ | ~~Fixed 3-digit width~~ **PARTLY FIXED** (`41b3928`). The parser splits on commas, so field width, channel count and a missing digital field no longer reject: 263 packets recovered. Decimals and minus signs still do, and wait on the value type | was: valid packets rejected |
| `weather.rs` | The **raw rain counter `#` is not implemented, and the omission is a decision**: chapter 12 gives it no width, no unit and no scaling, so any reading of it would be invented | `#` stays uninterpreted rather than rejected, which is the safe direction. The tag scan stops at the first byte it does not know and hands the remainder back as `rest`, so an unread tag costs every field behind it too |
| `weather.rs` | The **compressed** and **object-borne** Complete Weather Report layouts are unimplemented; only the four uncompressed spellings (`!`, `=`, `/`, `@`) parse. 0 frames in this corpus | Compressed weather positions decode as a plain compressed position |
| `object.rs` | **Area objects are not implemented at all.** `Object` and `Item` carry `name`, `live`, position, `symbol` and a free-text `comment` and nothing else, so the area-object descriptor (and the 1500-vs-100 scaling factor it turns on) never arises. `Object`/`Item` also do not parse the ordinary 7-byte data extension the spec allows them | Area objects decode as an object with a comment. **Order matters here, and the obvious order is the dangerous one:** the 7-byte extension must not be added before the `\l` area-symbol dispatch, because an area object's `Tyy/Cxx` descriptor is byte-identical to a `ddd/sss` course/speed extension for colours `0`–`9`: `T00/C31` would decode as a course of 0 and a speed of 310 knots. Implementing the cheap half first would convert a missing value into a wrong one |
| `weather.rs` | **A parsed weather report does not rebuild to the bytes it came from.** MEASURED over the corpus, parsing each frame and rebuilding it through `AprsPacket::build`: 651 of the 763 `AprsPacket`-typed frames are byte-exact, and of the 112 that are not, **80 are weather**, or 71% of all rebuild differences. The Complete layout is byte-exact **4 times in 82** (5%). Three separate causes, all confirmed on captured payloads: (1) `build` walks `TAGGED_FIELDS` in a fixed order while real traffic does not, so `…P000b10161h38` comes back `…P000h38b10161`, and chapter 12 explicitly blesses what the sender did: "the remaining parameters may be in a different order (or may not even exist)"; (2) fields that did not exist are re-emitted as dotted placeholders, which are legal on the wire but were not what arrived, so `_225/000t068p000h68b10162` grows to `_225/000g...t068r...p000P...h68b10162`; (3) a field swallowed into `rest` is re-emitted *after* those placeholders, duplicating its tag, so `…p000....h00b10175dU2ks` rebuilds as `…p000P...h..b.........h00b10175dU2ks` with `h` and `b` each appearing twice. Counted over the weather differences: 52 reordered, 30 grew, 30 duplicated a tag | Byte-exactness is a stated crate invariant and it does not hold for the commonest weather layout, so a digipeater, igate or KISS bridge that parses and re-emits **rewrites the packet**. There is a mitigation short of a fix: a caller that only forwards is already served by `Decoded::info`, which carries the received bytes verbatim on every outcome. Closing the defect means recording the received tag order and the absent-field spelling, which points at §4(d)'s raw carriers instead of a bigger `build` |
| all | Length limits are **not** enforced, which is the lenient direction and the right one: `message::TEXT_MAX = 67` is documented as informational and is read by neither `build` nor `parse`, and no 43-character comment limit exists anywhere in the crate | None |

Unimplemented DTIs, all **0** in this corpus: `?` (query), `{`
(user-defined), `%` (Agrelo DF), `,` (test data), `[` (obsolete
Maidenhead beacon), `&` (map feature). `$`, `}`, `<`, `#` and `*` are
implemented. The two Mic-E identifiers `` ` `` and `'` decode through
`Decoded::decode_frame`, which is the only entry point that has the
destination address they need (see §2), and through `mic_e::decode` for
a caller who already has both halves.

Two entries this table used to carry were withdrawn once they were
checked against the code. They are recorded here so that nobody "fixes"
them back into defects:

* **The Mic-E longitude-degrees boundary table.** Encode maps 100–109
  to `deg + 8` (`l`–`u`) and 110–179 to `deg - 72` (`&`–`k`), which
  *is* the 1.1-corrected table, and decode inverts it. Corroborated by
  §5's zero disagreements over 894 Mic-E frames.
* **The barometric field width.** Draft C reads "Barometric pressure
  field should be a total of 6 characters, not 5", but chapter 12's
  `Bytes:` row counts **tag plus digits**: `cccc` is 4 (tag + 3 digits)
  and `hhh` is 3 (tag + 2). So `bbbbbb` = 6 characters = `b` + **5
  digits**, which is what `TAGGED_FIELDS`' `(b'b', 5)` already says.
  Draft C was correcting its own `Bytes:` row, not the field. 130
  corpus barometers cross-checked against an independent decoder give
  **0 disagreements** (`tests/aprs_differential.rs`).

### 3.0 Coordinate storage is too coarse for half the position formats

> **FIXED.** `UNITS_PER_DEGREE` is now 342 833 400 000 000, one unit is
> 0.32 nanometres, and every APRS position format is stored exactly.
> MEASURED against the exact wire rational for all 4 569 compressed
> positions in the capture:
>
> | | median error | max | over 1 m |
> |---|---|---|---|
> | before | 3.843 m | 9.303 m | 78.0% |
> | after | **0.028 m** | **0.056 m** | **0.0%** |
>
> **Zero positions moved farther from the wire**, and uncompressed
> positions are byte-identical before and after. The 0.056 m residual
> is exactly half the JSON output's 1e-6 degree rounding step, so
> storage is exact and only the rendering remains. Before the fix the
> 9.303 m maximum was exactly half the old 18.55 m storage quantum, the
> theoretical bound, which is what confirmed the measurement rather
> than the luck.
>
> The section below is kept for the diagnosis, and three things in it
> were wrong when it was written. **The constant** is not the LCM of the
> six format denominators: NMEA does not fix its decimal count, and
> u-blox receivers emit five places by default, which that LCM does not
> divide. What shipped is `UNITS_PER_DEGREE = 342_833_400_000_000`, one
> unit being 0.32 nm, with 149x of `i64` headroom. **The count of sites
> the compiler could not catch** was nine, not four; the five extra were
> the dangerous kind, including `write_latlon`, where the type mismatch
> the compiler does report has a tempting repair that keeps the wrong
> divisors. **And a naive scaling would have overflowed** `distance_to`.
>
> The constant has one property worth keeping in view, because two
> later features depend on it: it is divisible by 6 000, 60 000 and
> 546 000, so a hundredth of a minute, a `!DAO!` decimal digit and a
> `!DAO!` base-91 step are all exact integers and no reading rounds.

The largest defect in the crate, and the only one in this document that
corrupted a value rather than dropping one. It is listed first because
every other row above is a spelling problem and this one was not.

`Latitude` and `Longitude` **used to** store signed 1/100 arc-minutes,
6000 units per degree, one unit being 18.55 m. That is exactly right for
the oldest and commonest format, `DDMM.hh`, which reports hundredths of
a minute and nothing finer, and the type was designed around it. APRS
has five other position formats and three of them are finer:

| format | smallest step | metres | units per degree |
|---|---|---|---:|
| uncompressed `DDMM.hh` | 1/100 arc-min | 18.553 | 6 000 |
| Mic-E | 1/100 arc-min | 18.553 | 6 000 |
| `!DAO!` decimal | 1/1000 arc-min | 1.852 | 60 000 |
| `!DAO!` base-91 | 1/9100 arc-min | 0.204 | 546 000 |
| **compressed base-91 lat** | 1/380926 deg | **0.292** | 380 926 |
| **compressed base-91 lon** | 1/190463 deg | 0.584 | 190 463 |
| **NMEA `ddmm.mmmm`** | 1/10000 arc-min | **0.185** | 600 000 |

A compressed position is divided into the coarse grid on parse, so 63.5
distinct wire positions collapse onto one stored value. The information
is gone at the moment of storage and can never be written back.

MEASURED over 64 918 packets from 22 879 stations, captured
receive-only from the APRS-IS full feed in three sessions on three
different servers:

| | rebuilds byte-exactly |
|---|---|
| uncompressed positions | 25 259 of 25 278 (99.92%) |
| **compressed positions** | **6 of 4 569 (0.13%)** |

That split is the fingerprint of a storage-unit defect rather than a
parsing one. A worked example, from OM5RW-7 on 2026-08-21: the wire
carries `5:9a`, which is y = 15 280 693, and chapter 9 defines the
latitude as `90 - y/380926`, so the sender meant 49.885403 degrees.
Stored in 1/100 arc-minutes that rounds to 299 312 units, which reads
back as 49.885333: **7.8 metres of invented error on a position the
sender specified to within 29 centimetres**.

#### The storage unit that fixes it

Storing every format exactly requires a unit that each format's
denominator divides. The least common multiple of the seven above is
114 277 800 000 per degree, which needs `i64`.

That figure is **not sufficient**, and the reason is worth recording
because it is invisible from the specification. NMEA 0183 does not fix
the number of decimal places: the field is defined as "a variable
number of digits for decimal-fraction of minutes". Four places is the
figure usually quoted, but u-blox, the dominant GNSS chipset in current
trackers, **emits five by default** and offers seven in its
high-precision mode. Five places needs 6 000 000 units per degree,
which is `2^7 x 3 x 5^6`, and the LCM above carries only `2^6` and
`5^5`. It does not divide.

This is not hypothetical. MEASURED over the same capture: of ten raw
NMEA coordinate fields, **two carry five decimal places**, for example
`$GPRMC,140248.00,A,4536.13196,N,12239.25040,W` from N7QME-3. That
position is 45.60219933 degrees and this crate currently records
45.602167, a **3.6 metre error** from storage alone.

Multiplying the LCM by 3000 gives **342 833 400 000 000 units per
degree**, which stores every format above exactly, plus NMEA at five,
six and seven decimal places, plus whole arc-seconds and 1/16
arc-second. 180 degrees is then 6.17e16, leaving 149x of `i64`
headroom, and one unit is 0.32 nanometres.

Two consequences for whoever implements it:

* **The compressed conversion stops rounding.** Because the unit
  divides 380 926 and 190 463 exactly, `90 - y/380926` becomes a
  multiplication by the exact integer 900 000 000 rather than a
  multiply-then-divide. There is then no rounding anywhere on that
  path, so the rounding-asymmetry hazard between the northern and
  southern hemispheres cannot arise. The existing shape,
  `div_round(y * units_per_degree, 380926)`, would overflow `i64` at
  this unit and must go.
* **`distance_to` must divide the cosine out before squaring.** It
  currently keeps the east component at Q15 through the squaring, to
  avoid quantising to a whole 18.55 m unit. At 0.32 nanometres per unit
  that concern is gone, while keeping Q15 would overflow: the sum of
  squares is formed in `i128`, and at this unit the Q15 form reaches
  9.1e41 against `i128::MAX` of 1.7e38. Dividing back to units first
  brings it to 7.6e33, with 22 339x of headroom.

### 3.1 Data extensions

Data extensions are invisible to the coverage percentage: a position
report yields a typed value whether or not its course, speed, wind and
antenna capability were understood. Measured over the same 2182 off-air
frames, of the 516 position reports carrying anything after the symbol:

| content | frames |
|---|---|
| `/A=nnnnnn` altitude | 258 |
| `ddd/sss` course/speed or wind | 253 |
| `PHG` | 139 |

**76% of comment content was structured data being returned as opaque
text.** All of it now decodes, in `src/aprs/extension.rs`: 650 field
values recovered.

Four points, each of them a live bug avoided:

1. **`ddd/sss` is wind, not course/speed, when the symbol is `_`.** The
   bytes are identical. MEASURED: **54 of 253** (21%) are wind, so a
   symbol-blind parser reports a fifth of them as vehicle course and
   speed. `DataExtension::parse` takes the symbol as a required
   argument.
2. **`PHGR` is 9 bytes ending in a mandatory `/`**, not 8 (spec 1.2,
   ch. 7: the example is `PHG72604/`). Both shapes end in a slash: the
   7-byte `PHGabcd/`, by far the commoner form on air, where the `/` is
   only a free-text separator, and the 9-byte `PHGabcdr/`. **Position**
   is therefore the only discriminator. Testing "byte 7 is a digit"
   instead would eat two characters of `PHG5260146.520MHz`.
3. **The rate is `1`–`9` then `A`–`Z`** (A = 10), and rate `0` is a
   *sentinel* meaning the packet was sent outside its normal schedule
   and must be excluded from reliability statistics, rather than "zero
   per hour". Modelled as [`PhgRate::Unscheduled`].
4. **The height code is explicitly allowed above `'9'`.** The spec says
   it "may in fact be any ASCII character 0–9 and above … so that larger
   heights for balloons, aircraft or satellites may be specified", with
   `:` = 10240 ft. This crate ships balloon-tracker examples, so a
   `height > 9` rejection would be self-defeating.

And two boundary decisions:

* **Compressed positions carry no extension.** The 13 compressed bytes
  substitute for the uncompressed position *and its 7-byte extension
  slot* (spec ch. 9 gives the substitution string explicitly, and states
  that the format "does not support PHG"). Parsing one anyway would eat
  the first 7 bytes of every compressed comment. Status reports do not
  carry extensions either. Objects and items *are* allowed one by the
  spec, and this crate does not parse it, which is an open gap rather
  than a decision.
* **Altitude stays in `comment`, exposed as a view.** Unlike an
  extension it has no fixed offset ("the altitude may appear anywhere
  in the comment"), so promoting it to a field would mean either losing
  byte-exact round-tripping or storing an offset beside it. The negative
  form `/A=-ddddd` is accepted; the spec notes it is not official but
  widely recognised, and below-sea-level stations are real.

Pinned by per-field floors in `tests/corpus_aprs.rs` (`MIN_FIELDS`),
because the coverage percentage cannot catch a regression here.
Mutation-tested: breaking the weather-symbol comparison drops the wind
count to 0 and fails with the field named.

Still unimplemented in this area, ranked by value-to-effort: extensions
on objects and items, base-91 comment telemetry (`|…|`), `!DAO!`
precision, the 8-byte `/BRG/NRQ` DF follow-on (gate it on the DF
symbol), and frequency/tone/offset fields. MEASURED: base-91 telemetry
and `!DAO!` are both **0** frames in this 2005 corpus and expected
common in current traffic.

The Mic-E device-type **prefix** is recognised unconditionally:
`MicE::device_prefix` holds the byte so `MicE::altitude` can see past it
while `encode` stays byte-exact. Prefix and altitude are found
independently, with a fallback so that an *un*prefixed altitude whose
leading base-91 digit happens to be a prefix byte still reads as an
altitude; without that fallback the crate's own encoder stops being
invertible above 39 km. MEASURED: 641 corpus frames carry a prefix and
all **641** are recognised.

The trailing 1–2 byte device **suffix** is not implemented, because
there are **0** suffixes in this corpus to validate an implementation
against. All 35 prefixed frames with no altitude behind them are `]`
followed by ordinary status text, not a prefix/suffix pair, and
`AE6GR-7` settles it by transmitting *both* spellings from one radio. A
scanner that eats one or two bytes off the end of every status text on a
wrong hypothesis is the silent value error §3 ranks first. The *rule* is
cheap, but the device-identity table is large and the spec recommends
loading it at runtime, which suits a `no_std` crate poorly.

## 4. The type-design tension

Two requirements pull against each other:

* **Read everything.** A receiver must extract value from malformed,
  truncated, obsolete and vendor-quirked traffic. The spec's own guidance
  is that programs "must be able to process [non-conforming packets]
  without ill effects", and it concedes elsewhere that "receiving parser
  action is undefined".
* **Be confident in what you hold.** A `Position` should not be able to
  contain a latitude of 91°, and downstream code should not need to
  re-validate.

### How we resolve it today: the labelled tier, and nothing above it

One tier of the lenient gradient is built: `AprsPacket::parse` returns
`Result<_, AprsError>`, but `Decoded::decode` is total, and
`DecodedKind::{Unsupported, Malformed}` is the raw-carrying variant;
see (a) below. What is missing is the **"parsed with warnings"** tier:
there is no diagnostics channel, and no per-field raw carrier, so
anything a parser cannot canonicalize is still all-or-nothing at the
record level. That is where the loss above comes from (now 6.9%, and
labelled rather than lost). Three structural problems remain:

**1. Component types are validated; record types are not.** `Latitude`,
`SymbolCode` and `Addressee` do make illegal states unrepresentable.
Every packet struct, though, is a `pub`-field wire record whose
invariants live in `build()`, so
`Timestamp::DhmZulu { day: 0, hour: 99, .. }` and
`Telemetry { seq: 65535, .. }` still construct fine and fail late
(verified: `BadTimestamp { field: b'D', got: 0 }` and
`TelemetrySequenceOutOfRange { got: 65535 }`, both only on `build`).
`WeatherReport { humidity: Some(200), .. }` no longer compiles: typing
the nine measurements moved that one into `Humidity::new`, applied to
one struct out of eleven. These fields do carry cross-field invariants,
in at least four places (`PositionCs::cs` is silently voided when
`compressed` is false, verified: a `CourseSpeed` trailer builds as
`!0000.00N/00000.00E>` with the course and speed gone; GGA +
`CourseSpeed` is rejected only at build time, as `NmeaSourceConflict`).

**2. One type serves both "received" and "about to transmit".** A
received packet may legally contain things a builder should never emit,
and the crate has no way to say so. Consequences, all verified:

* `Symbol::from_wire` is an RX hatch reachable from TX, so the crate
  will build `!0000.00N~00000.00E$`, a packet **its own parser
  rejects**. The doctest teaches this as a feature.
* `MessageContent::Text { text: b"ack1" }` builds a packet that
  re-parses as an `Ack`.
* `MicE::status` beginning `xxx}` re-decodes as an altitude.
* A `Position` with a weather symbol and a wx-shaped comment re-parses
  as `PositionWeather`, so the variant is not stable across a round trip.

**3. Build is not wire-faithful**, in three ways.
`WeatherReport::write_tagged` iterates the whole `TAGGED_FIELDS` array
unconditionally, so build always emits every tag with dot placeholders
and any parse-then-forward path (digipeater, igate, KISS bridge)
rewrites every weather packet; the `weather.rs` rebuild row in §3
carries the measurements and the other two causes. The
`csT` trailer is discarded when `c == ' '`, because a `CompressedCs::NoData`
trailer collapses the packet to `AprsPacket::Position`, which has no
`compression_type` field to keep the `T` byte in. And `CompressionType`
drops base-91 bit 6, because bits 7–6 are read as "unused" while a
base-91 digit runs to 90: wire `'a'` (value 64) comes back out as `'!'`,
and `'{'` (value 90) as `';'`. `compression_type_byte_round_trips` in
`tests/compressed.rs` round-trips only the *typed* field combinations, so
nothing pins this loss either way, and this document is the only record
of it.

### The shape of a better answer

Four changes, in dependency order. None requires `alloc` or `unsafe`.

**(a) Make the parse total. Done, on a separate type.** The shape
recommended here was raw-carrying variants on `AprsPacket` itself:

```rust
#[non_exhaustive]
pub enum AprsPacket<'a> {
    Position(Position<'a>),
    // …
    MicE(MicE<'a>),                                   // REJECTED — see (b)
    /// DTI recognized, body did not conform. Bytes preserved.
    Malformed { dti: u8, body: &'a [u8], error: AprsError },
    /// DTI not implemented, or not an APRS packet at all.
    Unsupported { dti: u8, body: &'a [u8] },
}
```

What shipped puts the two raw variants on a *new* total type instead, so
`AprsPacket` keeps meaning "validated, and re-transmittable":

```rust
pub struct Decoded<'a> { pub info: &'a [u8], pub kind: DecodedKind<'a> }

#[non_exhaustive]
pub enum DecodedKind<'a> {
    Packet(AprsPacket<'a>),                          // parse *and* build
    MicE(MicE<'a>),                                  // frame-level; see (b)
    Nmea(NmeaSentence<'a>),                          // receive-only
    Ultimeter(UltimeterRecord<'a>),                  // receive-only
    ThirdParty(ThirdParty<'a>),                      // receive-only
    NeedsDestination { dti: u8 },                    // Mic-E, no dest given
    Unsupported { dti: u8 },
    Malformed { dti: u8, error: AprsError },
}
```

The bytes live on `Decoded::info` rather than in each variant, so they
are reachable from **every** outcome instead of only the two raw ones.
This is the `SymbolRepr::{Valid, Raw}` pattern lifted to the record
level. It costs no allocation (the variants borrow), and it converted
"28.8% lost" into "6.9% explicitly labelled and recoverable".
`#[non_exhaustive]` is on `AprsPacket`, `AprsError`, `DecodedKind` **and
`MicEError`**, matching `wspr`/`ft8`/`m17`/`units`/`geo`.

**(b) Fold Mic-E in by taking the destination address. Done, on
`Decoded`,** with `DecodedKind::MicE` for the answer and
`DecodedKind::NeedsDestination` for the refusal. It is not folded into
`AprsPacket` and will not be: §2 gives the transmit-safety reason, and
`CONTRIBUTING.md` records it as a design invariant.

**(c) Split the received view from the transmit builder.** The four
impersonation bugs are all one root cause. Two viable encodings:

* *Typestate*: `Position<'a, Rx>` / `Position<'a, Tx>` with a sealed
  `Role` trait and `PhantomData`. Zero-sized, `Copy`, no unsafe. Raw
  escapes are constructible only in `Rx`. Cost: a second type parameter
  on ~8 types, mitigable with `type RxPosition<'a> = Position<'a, Rx>`.
* *View/builder split*: parsed types become read-only accessors over
  borrowed bytes with no public constructor; building goes through
  separate types that accept only validated components. Bigger change,
  but it also fixes wire-fidelity for free, because a view keeps the
  original bytes rather than re-serializing.

The view/builder split is the better long-term shape; typestate is the
cheaper increment.

**(d) Model ambiguity and rawness in the data, not as errors.** Because
no-alloc forbids normalizing on parse (`Cow` is unavailable), anything
we cannot canonicalize must be representable:

```rust
pub enum Coordinate {
    Precise(Latitude),
    Ambiguous { digits: [u8; 8], places: u8 },   // spec levels 1..=4
}
```

#### A vocabulary for round trips

This section kept reaching for phrases like "round-trips" and
"byte-exact" to mean four different things, and the argument about raw
carriers cannot be settled in that vocabulary. What follows is small
enough to hold in the head and precise enough to decide the question.

Write `W` for wire byte strings and `V` for the values this crate's
types can hold. Parsing and building are **partial** functions:

```text
p : W ⇀ V        parse      defined on W_ok, the wire forms that parse
b : V ⇀ W        build      defined on V_ok, the values that can be written
κ = b ∘ p        the canonicalisation map, W_ok → W
```

`κ` is the thing every rebuild measurement computes. It takes
the bytes that arrived and returns the bytes this crate would have sent
for the same meaning.

**The fibre** of a value `v` is `p⁻¹(v)`, every wire spelling that means
`v`. Informally: its synonyms. `κ` picks one representative from each
fibre, so:

> `κ = id` **is impossible** wherever a fibre has more than one member.
> Two spellings cannot both be returned.

That single line disposes of "make the rebuild rate 100%" as a goal.
The question is never whether `κ` moves a packet, it is **which way**,
and five properties tell those apart. Let `L ⊆ W` be the spec-legal
forms.

| | property | statement | verdict |
|---|---|---|---|
| **F1** | byte fidelity | `κ(w) = w` | impossible in general |
| **F2** | legality preservation | `w ∈ L ∩ W_ok ⟹ κ(w) ∈ L` | **required** |
| **F3** | semantic idempotence | `p(κ(w)) = p(w)`, equivalently `κ∘κ = κ` | **required** |
| **F4** | normalisation | `w ∉ L ⟹ κ(w) ∈ L` | desirable |
| **F5** | legal-spelling preservation | `w ∈ L ⟹ κ(w) = w` | optional, and costly |

The six outcomes `Asymmetry` in `tests/common/mod.rs` classifies are
exactly the failure modes of those five properties:

| variant | in the vocabulary | verdict |
|---|---|---|
| `Exact` | F1 holds at `w` | correct |
| `NormalisedTerminator` | F4 doing its job: `w ∉ L`, `κ(w) ∈ L` | correct |
| `NormalisedCase` | F4, the same | correct |
| `Rewritten` | F5 fails while F2 and F3 hold | cosmetic |
| `ValueChanged` | **F3 fails** | information lost |
| `BuildFailed` | `b` is undefined at `p(w)`, so `κ` is narrower than `p` | a gap between two partial maps |

`Asymmetry::is_acceptable` returns true for exactly the first three, and
that boundary is what the ratchet floors in `tests/corpus_aprs.rs` are
written against.

Two edges of that table are worth stating, because both have been
misread. **F2 failure has no variant of its own.** A rebuild that no
longer parses cannot be compared against the value it came from, so the
caller passes `reparses_equal = false` and it scores as `ValueChanged`.
That is the right severity: an output this crate would itself reject is
worse than one it spells differently. **`BuildFailed` is not by itself a
defect.** `p` and `b` are separately partial, and a receive-only format
such as raw NMEA is supposed to land there.

#### Which property to argue from

The five are not equally cheap to satisfy, and the order you reach for
them decides what a fix costs.

> **Look for the F2 argument first. It is usually there, and it decides
> without costing anything.**

The weather absent-field spelling established this. Chapter 12 permits
the parameters in any order, so choosing between two orderings is F5,
optional, and exactly the kind of thing a raw carrier gets proposed for.
What settled it was F2. The placeholder run was written *before* `rest`,
so on any report whose tag scan stopped early, `build` inserted
synthetic bytes into the middle of content the sender did send:
`_11230221c298s000g000t-103r000p000P000h10b10163wRSW` went from 53 bytes
to 74, with five tags appearing twice. That output is not in `L`.
Writing an absent field by omitting it cannot lengthen a packet, so it
cannot do that, and the repair needed no new mechanism. MEASURED on the
off-air corpus after the change: `Weather` rebuilds **28 of 28**
correct, and `PositionWeather` moved to **39.0%**, as a side effect of
an argument that was never about byte counts.

#### Why build must not see the wire

A raw carrier changes build's signature from `b : V ⇀ W` to
`b′ : V × W ⇀ W`. Any function that receives `w` may return it, so
`κ′(w) = b′(p(w), w) = w` and F1 holds everywhere by construction.

That looks like a win and is the trap. The rebuild check is the
predicate `[κ(w) = w]`, and its diagnostic power comes entirely from
`b` **not** having seen `w`: when the telemetry parser clamped values
above 255, the wire said 510, `b` wrote 255 from the value it held, and
the mismatch is what exposed the defect. Under `b′` the check is a
tautology and reports nothing, on that defect or any other.

So the design rule, stated once:

> **`build` must factor through `V`.** Whatever else changes, the
> written bytes are a function of the typed value alone. F1 is then a
> measurement, not a target, and F2 and F3 are the properties to hold.

F5 is the only property a carrier buys, it is optional, and it costs
the diagnostic. Where F5 is wanted for forwarding, `Decoded::info`
already provides it, verbatim, on every outcome including malformed
ones, without touching `b`.

#### What each instrument cannot see

Every number in this document comes from one of three instruments, and
each has a blind spot that has hidden a real defect. This is the most
load-bearing paragraph here, because three of the defects fixed in this
crate were invisible to the instrument that should have found them.

| instrument | reaches | structurally cannot see |
|---|---|---|
| the off-air WAV corpus (2182 frames, reproducible, so the ratchet floors live here) | genuine RF: trailing CRs, lower-case hemispheres, real demodulation | **base-91 compressed positions: it contains none.** Being 2005-era, `!DAO!` and base-91 comment telemetry are 0 frames each |
| live APRS-IS captures (95 219 packets over two sessions, not reproducible, so their numbers never become floors) | volume, diversity, current sender behaviour, modern formats | the trailing CR, which igates strip, so it shows only on RF. Third-party frames are near-absent, and it carries information fields longer than any RF frame can hold |
| the rebuild check `[κ(w) = w]` | any disagreement between what was stored and what the wire said | **Mic-E**, which is not buildable from an information field, and **precision lost at parse** |

The third row is the subtle one and it has cost the most. `build` writes
back whatever was stored, so a value quantised on the way in rebuilds
byte-exactly and scores as `Exact`. Precision loss at parse is invisible
to a rebuild comparison **by construction**, and the only way to see it
is to compare against the wire's own exact value.

That blind spot hid the coordinate defect in §3.0. It would equally have
hidden a fixed-point telemetry value type: `9.2362515628338` stored as
`9.236` rebuilds as `9.236` against itself and reports no fault, which
is why the value type is a decimal mantissa and digit count rather than
milliunits. The Mic-E row is the same shape one level up: 3 284 packets,
5.1% of the live capture and roughly 41% of RF traffic, sit outside the
instrument that found and measured every other defect here. "Zero
value-changed rebuilds" is a true statement about the formats the check
can reach, and says nothing about the ones it cannot.

> **Before trusting any number here, ask what the measurement
> structurally cannot reach.** Three times out of three, that is where
> the defect was.

#### What this vocabulary does not promise

The five properties are a decision procedure, not a proof system. They
say which way a change moves a packet and let two people disagree about
a tradeoff in the same terms. They do not promise that a decoder can be
total, and on this protocol it cannot be, for reasons worth naming
rather than rediscovering one at a time.

**Parts of the spec define nothing.** Chapter 12 gives the raw rain
counter `#` no width, no unit and no scaling, so any reading this crate
produced would be invented; the field is left unparsed on purpose. The
same goes for chapter 18's user-defined format, whose content is by
definition private to the sender.

**Senders violate the spec, and the violations are not random.** They
are the fixed output of particular firmware, so they arrive in
thousands rather than ones. A latitude of 88 minutes, an object name not
padded to nine characters, `HHMMSS` wrongly suffixed `z`: each is one
implementation repeated across every station running it. Accepting them
is not always the generous choice. Reading `123456z` as a
day-hour-minute stamp reports an hour the sender did not mean, so those
rejections stay rejections. **Refusing to guess is a decision this
vocabulary supports:** F4 asks that `κ(w) ∈ L`, and says nothing about
`κ` being defined everywhere.

**Some rules are empirical, and their status belongs next to them.** The
telemetry digital-field anchor is the clearest case. Its safety argument
is that the scan cannot pick the wrong field, and the evidence is that
**zero** reports offer two candidates across 95 219 packets in two
independent captures two days apart. That is a property of observed
traffic and not a theorem; the source comment says so, and the census is
re-run against every fresh capture. A rule of that kind is sound to ship
and unsound to assume, and the difference is whether the doubt is
recorded beside it.

So the practical target is not "parse everything". It is:

* every packet yields either a typed value or a typed error, and
  `Decoded` makes that total at the record level;
* nothing is reported that the sender did not send, which is F3, and it
  is the row that must stay at zero;
* what is refused is refused for a stated reason, and the count is
  tracked rather than rounded away.

1 086 packets in 95 219, or 1.14%, stay rejected under that standard.
That number is a result, not a defect budget, and §7 lists it by cause.

#### Relaying is a different map, and it is the one forwarding wants

The argument for F5 is nearly always some version of "a digipeater or
igate that parses and re-transmits puts bytes on the air that nobody
sent". That is true, and it is an argument for **not applying `κ`**,
not for making `κ` the identity.

Split a frame into its header and its payload, `w = (h, i)`. The two
operations are:

```
canonicalise   κ(h, i) = (h, b(p(i)))          reads the payload
digipeat       D(h, i) = (b_h(t(p_h(h))), i)   does not
```

A digipeater's authority is the AX.25 header: find the first unused
hop, decide whether it is addressed here, set the H bit, decrement
`WIDEn-N`, re-transmit. The information field is opaque to it. So on
the relay path the payload is carried by **identity**, and byte
fidelity on it is free, total, and not contingent on the payload being
something this crate can parse.

That last clause is the part a carrier cannot match. A relay must
forward what it does not understand; `κ` is partial and is undefined on
exactly those frames. `D` is total in `i` because it never looks at it.

The crate already has this shape. `relay_decision` takes the path and
nothing else, so the payload cannot influence the decision and the
decision cannot influence the payload, and `UiFrame::with_hops` borrows
the information field rather than rebuilding it. `tests/digipeat_laws.rs`
states the three properties as swept laws: payload transparency,
termination (every relay spends exactly one hop of the budget), and
local loop freedom.

So the tension the raw carrier was invented to resolve does not exist:

| you are | use | F1 on the payload |
|---|---|---|
| relaying, gating, forwarding | the header-only path | **free and total** |
| re-originating, bridging, displaying | `κ` | impossible in general, and not wanted |

The carrier tries to make one map serve both. It cannot: serving the
first destroys the diagnostic that makes the second measurable, and the
first is already served better by a map that never reads the payload.

This also settles the remaining F5 items. Weather tag order and the
compressed `cs` no-data trailer are re-spellings that only a
canonicalising path produces, and no correctly built relay takes that
path. They are cosmetic, and there is no user standing behind them
asking for the bytes back.

#### A worked example: the compressed `csT` altitude

The clearest demonstration the crate has of what the vocabulary buys,
because every property in the table is exercised by one three-byte
field and they do not all point the same way.

Chapter 9 encodes altitude on an exponential scale, `1.002^n` feet for
a code `n` in `0..=8280`, and the parser truncates that power to whole
feet, as the chapter's own worked example does. So

```text
p(n) = floor(1.002^n)
```

is the decoder. Two facts about it decide everything else:

* **`p` is not injective.** Only 5669 of the 8281 codes name a distinct
  altitude, and below about 500 feet the code step is under a foot, so
  many codes share a value. The fibre `p^-1(v)` has up to 347 members.
  **F1 is therefore impossible here by counting**, not by
  implementation quality: the builder is handed a foot count and cannot
  know which of 347 codes produced it.
* **`p` is not surjective.** Above 5000 feet the step exceeds 10 feet,
  so most whole-foot values are not on the scale at all.

Build used to pick the code minimising `|1.002^n - v|`, which inverts
the **power**. The parser does not report the power, it reports
`p(n)`, and truncation makes those disagree: `1.002^2951 = 363.6187`
reports 363 feet, and the code nearest 363.0 is 2950, which reports
362. That is **F3 failing**, on 999 of the 8281 codes.

F3 is the property worth the emphasis, because the failure compounds.
`k = b . p` was not idempotent, and igates and digipeaters parse and
re-emit, so the cycle runs more than once. Iterating the old rule to a
fixed point across the whole domain: 417 codes lost more than one foot,
and code 3131 walks 520 feet down to 480 over 41 passes. A quantity
that decays every time it is relayed is a different class of defect
from one that is imprecise once.

The repair is to invert `p` rather than the power: search the decoded
values for a code that reads back as the value in hand. Where several
do, pick the one nearest the power, clamped into the fibre. Then

* **F3 holds everywhere**, by construction: the code emitted is one
  that decodes to the value held, whenever the scale has one.
* **F1 improves as a side effect**, 5303 to 5669 of the 8281 codes, and
  on the live feed 166 packets moved from `differs` to `exact` with
  **none** moving the other way.
* **F1 remains unreachable** for the 2612 codes that share a value, and
  no further work changes that. Chasing it would mean re-emitting the
  received bytes, which section "Why build must not see the wire" rules
  out.

The lesson generalises past this field: **when a decoder quantises,
the encoder must invert the decoder and not the underlying function.**
Speed and range sit on the same kind of scale but round rather than
truncate, and were F3-stable already; they go through the same
inversion so that one rule covers all three, which costs nothing
because the clamp leaves a correct choice alone.

One caution the example also supplies. The rebuild check **cannot see**
precision lost at parse. Build faithfully writes back whatever was
stored, so a value quantised on the way in rebuilds byte-exactly and
scores as a success. That is how the coordinate defect in section 3.0
stayed invisible, and it is why a telemetry value type that cannot hold
13 decimal places would pass this measurement while corrupting the
value. Byte fidelity is a statement about spelling; it says nothing
about what was understood.

#### What the vocabulary settles about weather

The weather block fails F5 for two independent reasons, and they have
different fixes:

1. **A cardinality mismatch.** Each tagged field has **three** states
   on the wire (a value, a dotted "no data", or absent entirely) and
   `Option<T>` has **two**. So `p` is non-injective on that field by
   construction, its fibre contains both `t...` and no `t` at all, and
   no build strategy recovers the distinction. Only a three-state type
   can. MEASURED over 5 517 weather packets: 1 520 (27.5%) spell at
   least one field with dots and the other 72.5% omit.
2. **Ordering.** Chapter 12 permits the parameters in any order, so the
   fibre also contains every permutation. Recovering that needs the
   order stored, which is F5 again and carries the cost above.

Only the first breaks **F2**, and that is what makes it the one to fix.
Emitting a dotted placeholder for an absent field appends bytes, and
when a tag has been swallowed into `rest` the placeholder run is
written *before* it, so the tag appears twice and the output is
malformed. Choosing "omit" as the canonical spelling of absence cannot
do that: it never lengthens a packet and never duplicates a tag.
Ordering, by contrast, breaks only F5, which is optional.

#### Keeping a raw carrier sound when the fields are public

The rule usually given for a raw carrier is "emit the raw bytes when
the typed values have not been modified, and clear the carrier in every
setter". **That rule is unsound in this crate**, because the packet
structs are `pub`-field wire records with no setters. A caller writes
`wx.temperature = Some(t)` directly, nothing can intercept it, and the
carrier then re-emits bytes that contradict the value the caller set.
That is a worse defect than the reordering it was introduced to fix.

The sound form with public fields is **re-parse and compare**: keep the
carrier private, and emit it only when parsing it again yields a value
equal to the one being built. It is an equality test rather than an
attempt to diff bytes against fields, so it cannot be fooled: if any
field was edited, the comparison fails and canonical output is used. If
a field was edited and then edited back, the comparison succeeds, and
emitting the sender's spelling is then correct. The cost is one extra
parse per `build`, over a few dozen bytes, on a path that runs once per
frame rather than once per sample.

Two requirements come with it, and both are easy to get wrong:

* **`PartialEq` must ignore the carrier.** Otherwise a parsed packet
  never equals a hand-built one with identical fields, which breaks the
  crate's own round-trip tests and is surprising besides. Ignoring it
  also makes the comparison above test exactly the right thing, namely
  the typed values alone.
* **The re-parse needs the same context as the original parse.** The
  weather block is the example: `s` is wind speed in the positionless
  layout and snowfall in the Complete one, so re-parsing a carrier
  under the wrong layout could compare unequal, or worse, equal by
  accident.

#### What a carrier must not be used for

A carrier makes `build` echo its input, and that has a consequence
nobody should discover by accident: **it makes the rebuild check
vacuous**. The control that underpins every relaxation in this document
is "a packet you understood correctly serializes back to the bytes it
came from". If `build` returns the received bytes whenever they parse
to the current value, then `to_vec() == info` holds for every parsed
packet, whether or not a single field was read correctly. The clamp
defect that this crate's own tooling caught, where telemetry values
above 255 were clamped and 114 packets recorded 255 where the wire said
510, would rebuild byte-exactly under a whole-packet carrier and look
like a clean recovery.

So the boundary is:

* **Carriers are for spelling, where one value has several legal
  encodings and the crate must otherwise pick one.** Weather tag order,
  the compressed `cs` no-data trailer (`"  G"` against `" sT"`), a
  lower-case hemisphere letter, a trailing CR, telemetry's `005`
  against `5`. No improvement to the types removes these, because the
  wire carries information the value does not.
* **Carriers are not for values.** Compressed coordinate precision is a
  storage defect and is fixed by §3.0. An out-of-range wind direction
  is a range defect. A four-character temperature is a width defect.
  Papering over any of these with raw bytes would hide a wrong value
  behind a correct-looking rebuild.

And because even the narrow use blinds the control for the fields it
covers, `build` needs a canonical mode that ignores the carrier, so the
control keeps measuring the typed values. Two figures are then worth
tracking, and they answer different questions: **carrier rebuild**,
which should reach 100% and measures whether the plumbing preserves
what arrived, and **canonical rebuild**, which will not reach 100% and
measures whether the values are right and the canonical spelling agrees
with the sender's.

The crate took a different and cheaper shape for the ambiguity half:
`geo::Ambiguity` is a validated `0..=4` count carried as a third field
on `Coordinates` rather than a variant wrapping `Latitude`, so a
coordinate is never *un*-readable. But **no parser sets it**: `position.rs`
still rejects a space in a digit position with `BadDigit`, so the type
exists and the gap does not close. Same pattern still wanted for
`Timestamp::Raw([u8; 7])`, `TelemetrySeq::{Num, Raw}`, and a
`CsTrailer::Raw([u8; 3])` that would fix the verified round-trip loss.
All `Copy`, fixed-size, allocation-free.

**Optionally (e): a bounded diagnostics channel.** For issues that are
recoverable but worth surfacing (over-length comment, unknown weather
unit code, non-canonical trailer) return the value plus
`Diagnostics<const N: usize>` (a fixed array plus an overflow counter).
This is the "parsed with warnings" tier that turns over-strictness into
information rather than failure.

### Techniques evaluated and *not* recommended

* **GATs**: no benefit; `&'a [u8]` sharing means a plain
  `Iterator<Item = AprsPacket<'a>>` suffices.
* **`Box<dyn AprsPayload>`**: forbidden by no-alloc, and the reason the
  enum is closed. A sealed trait with `const DTI: u8` gives static
  dispatch for build helpers without it.
* **`NonZeroU8` niches**: real but marginal at these sizes. Note
  `Symbol` already spends 3 bytes encoding 2 bytes of information,
  since `SymbolRepr`'s `Valid`/`Raw` discriminant is derivable rather
  than worth storing.

## 5. Measured accuracy vs. an independent decoder

`tests/aprs_differential.rs` renders every corpus frame as a monitor line,
pipes the identical lines through the reference implementation's APRS
decoder, and compares the decoded fields: position, course, speed in
**both** units the reference prints, altitude, radio range, all four
`PHG` quantities, all nine weather measurements, and the symbol as a
one-to-one *relation* rather than a value. That separates two things
that are easy to conflate:

| Metric | Result |
|---|---|
| Frames where both decoders produced a position | 1724 |
| — **agreeing within 0.0001°** | **1724 (100%)** |
| — disagreeing | **0** |
| Positions only yodel found | 55 |
| Positions only the reference found | 2 |

**Accuracy is not the problem.** Every position we decode matches an
independent implementation exactly, including all 894 Mic-E frames.
Mic-E is the format with the most errata, and the one where a 1.0.1-era
longitude table would show up as boundary errors; we are not carrying
that bug. The other fields agree as well: **0 disagreements in every
field above**, over 1096 altitudes, 1204 courses, 1279 speeds (compared
in both mph and km/h, so a knots/mph mix-up cannot hide behind a loose
tolerance), 120 `PHG` readings, 1743 symbols across 32 distinct wire
pairs, and 1238 weather measurements.

A second test in the same file drives 64 synthetic frames through the
same oracle: the whole compressed position family, `RNG`, `PHG`, and the
four weather layouts. The corpus is a sample, and it does not contain
one compressed `csT` trailer.

**Coverage is what is left**, and it is now small. Per-field frames the
reference decodes and we do not:

| Missing | Field | Cause |
|---:|---|---|
| **28** | symbol | 26 Mic-E frames whose symbol table byte is outside the spec, where we decline to name a symbol and the reference falls back to the primary table (a difference of policy), plus the 2 nested `}` below. It was 296 before the address-borne symbols of chapter 20 were read (§6) |
| **10** | speed | 6 `` ` `` with a corrupt longitude, 2 `}`, and 2 real `000/000`, which should not close. It was 26 before the `000/000` sentinel was read as chapter 7 states it, for the **pair**: 16 were a real course beside a real speed of *zero* knots (`315/000`) |
| 12 | course | 6 `/`, real `000/sss` and right to keep since chapter 7 gives the course domain as `001-360`, plus the same 6 `` ` `` |
| 6 | altitude | 6 `` ` ``: Mic-E frames whose longitude byte is outside chapter 10's range, so we decline the whole report |
| 2 | position | 2 `}`: a Mic-E payload inside a third-party wrapper. `ThirdParty` is built not to nest, so the *harness* cannot reach the inner destination. This is a property of the test and not of the parser |
| 0 | range, all nine weather fields, `PHG` | no gap |

Against that, 55 positions only *we* find. Two of the gaps are intended
and should not be closed: `000/000` is "unknown" per chapter 7, so we
report `None` where the reference reports zero; and a Mic-E longitude
byte outside chapter 10's range is rejected rather than half-salvaged.

## 6. Where the missing formats are specified

Research notes for the DTIs that were unimplemented when this document
was written, with licensing, because the sources differ sharply in how
freely they can be used. The formats below have since been implemented;
the notes are kept because they record *why* each was built the way it
was, and the licensing constraints still bind.

### `$`: raw NMEA and Ultimeter (213 frames, the biggest single win)

Implemented in `src/aprs/nmea.rs` and `src/aprs/ultimeter.rs`, reached
through `DecodedKind::Nmea` / `DecodedKind::Ultimeter`. Both are
receive-only, so `AprsPacket::build` never grows an arm that could only
fail. MEASURED: 268 + 57 corpus frames, up from 0.

Two unrelated formats share this DTI. Dispatch on the body: `ULTW`
prefix → Ultimeter, otherwise a 5-character `ccXXX` NMEA tag. Ultimeter
claims four identifiers, not one: `$ULTW` packet mode, `!!` data-logger
mode, and `*` / `#` for the older Ultimeter II.

* **NMEA 0183.** The standard itself is **paywalled** (USD 1,150–10,000
  depending on tier) and its publisher asserts copyright aggressively.
  The *formats* are documented in free secondary sources that are
  explicitly not derivative of the standard. Implement from those, cite
  them, and do not describe the crate as "NMEA compliant" (that is a
  paid certification).
* **Two traps.** The talker ID is **not** always `GP`, since modern
  multi-constellation receivers emit `GN`, `GL`, `GA`, `BD`, so match
  the 3-character *formatter* (`RMC`, `GGA`, `GLL`, `VTG`, `WPL`)
  instead of the 5-character tag. And sentence field counts **grew**
  across NMEA versions, so a parser must require a minimum count and
  ignore trailing extras rather than asserting an exact count.
* **Ultimeter (Peet Bros)** is the good case: the vendor publishes the
  complete serial-data specification openly, with an explicit invitation
  to write software against it. Fixed-width 4-hex-digit fields,
  big-endian, two's complement, `----` for absent sensors. Note the `#`
  vs `*` DTIs differ only in wind-speed units (km/h vs mph), which is a
  1.6× error if conflated.
* `$ULTI` does not exist; it is a mis-recollection of `$ULTW`.

### `}`: third-party (48 frames)

Implemented in `src/aprs/thirdparty.rs`, reached through
`DecodedKind::ThirdParty`. MEASURED: 50 corpus frames.

No external standard needed: this is APRS-IS encapsulation, documented
freely at `aprs-is.net`, and the format is already in the 1.2 reference
we have. Strip `}`, parse the inner `FROM>TO,path:` header, and tolerate
`TCPIP`, `TCPXX`, `NOGATE`, `RFONLY` and `qXX` tokens in the inner path
rather than rejecting them. Unbounded nesting is malformed on the air
and a cheap denial-of-service vector; rather than capping recursion at a
constant, `ThirdParty` borrows `payload` as bytes and does **not** nest,
so the caller decides whether to descend. Depth is bounded by
construction, which is also what makes the type work without allocation.

Frames arriving via APRS-IS may also carry **alphanumeric two-character
SSIDs**, which an AX.25-only 0..=15 parser rejects. The strict rule
stays for RF-sourced frames: `CALLSIGN_MAX = 9`, raw slices, with
`source_address()` / `dest_address()` offering the AX.25 conversion only
where it is legal.

### Registries: device IDs and symbols

The 1.2 reference says device identifiers should be read at runtime
rather than hardcoded. Both relevant datasets are **CC BY-SA**, which
has no path to MIT/Apache-2.0 for a derived table:

| Data | License | Size | Churn | Recommendation |
|---|---|---|---|---|
| Device ID / TOCALL registry | CC BY-SA 2.0 | ~44 KB, ~400 entries | ~50–100 new/year, entries occasionally retyped | **Do not embed.** A lookup trait in the core crate plus an optional companion crate carrying the data keeps our licensing uniform. |
| Symbol description index | CC BY-SA 4.0 | ~11 KB | **Upstream frozen since 2021** | Embedding is defensible on the merits, since the keyspace is closed and the deployed network cannot absorb new symbols, but the table needs its own licence header and attribution. |

Neither has been done, and the current state is a third option not
listed above: **no** device-ID lookup exists at all (not even a trait),
and `Symbol::describe` returns short *original* wordings for **37**
well-known glyphs (33 primary, 4 alternate) with
`SymbolDescription::Unknown` for everything else. That sidesteps the
CC BY-SA question at the cost of coverage, and it is why
`tests/aprs_differential.rs` compares symbols as a one-to-one relation
rather than as strings: neither implementation's chart can be derived
from the other's, so what is checkable is that one wire pair always
draws one description and one description always comes from one wire
pair.

Mic-E *type-byte* semantics (`>`, `]`, `` ` ``, `'`, and the original
space) are protocol, not registry data, and are hardcoded in
`is_device_prefix` in `src/aprs/mic_e.rs`. The registry does not cover
every documented type byte, so driving Mic-E identification purely from
it would silently miss some devices.

The **address-borne symbol mnemonics of chapter 20** (`GPSxy`, `SPCxy`,
`SYMxy`, `GPSCnn`/`GPSEnn`, the overlay `z` slot and the 16-row
source-SSID fallback) look like a third registry and are not one: they
are in the specification's own Appendix 2, and they need no table at
all. Chapter 8 says why they exist at all ("symbols had to go in the
destination field using names like `GPSxxx`"). The 94 mnemonics per
table decompose into **seven contiguous runs** with disjoint leading
letters, so `aprs::symbol::from_destination` decodes them
arithmetically, which closed 268 frames of the §5 symbol gap without
touching the licensing question. MEASURED: 211 of those frames name the
symbol in the destination and 57 in the source SSID, and reading both
introduced **0** new disagreements. One run endpoint off by one
would draw a *plausible wrong icon* rather than erroring, so
`tests/symbol_from_address.rs` transcribes Appendix 2 row by row as an
independent oracle and sweeps both directions; see `docs/COVERAGE.md`.

### Specs we already implement: staleness check

* **IL2P**: we had implemented v0.4 and now target v0.6, with the
  spec's own verification vectors as known-answer tests. See §6.1.
* **FX.25**: the 2006 specification is **withdrawn but recoverable**, and
  is a *draft*: Jim McGuire (KB3MPL), "FX.25 FEC Extension to AX.25 Link
  Protocol for Amateur Packet Radio", Stensat Group LLC, document version
  **0.01.06 DRAFT**. There is no later edition; "v1.0" in the wild refers
  to this same document. Stensat pulled it from `stensat.org/docs/` some
  time after September 2022, but web-archive captures and third-party
  mirrors survive and are byte-identical to the 2006 original (SHA-256
  `8e7d1e6f5b727b9427de4cc3a6e97eb9dbe19693762cea02211bf2eba2e2b85f`).
  **Copyright © 2006 Stensat Group LLC with no licence or redistribution
  grant of any kind**. The document is silent on reuse, so cite and
  implement, do not vendor. Stensat Group LLC still trades and is
  contactable, so an explicit grant could be requested if vendoring ever
  became worthwhile.

  Vendoring is not needed for provenance: the spec publishes the
  Gold-code *construction* of the correlation tags, so
  `tests/fx25.rs::tags_regenerate_from_the_published_gold_code` rebuilds
  all eleven constants from the published polynomials, with no copy of
  the document in the loop.

  Three things this crate implements are **not** in the specification and
  are documented as conventions rather than requirements: the RS field
  parameters (`0x11D`, `fcr = 1`, `prim = 1`, which are *not* the CCSDS
  parameters its own bibliography cites), the zero-**suffix** shortening
  convention, and any tag-matching tolerance.
* **AX.25 2.2**: still freely downloadable, but its original publisher
  dropped it and the working group is defunct. No open licence grant:
  cite, implement, do not vendor the document.

### 6.1 IL2P v0.6 audit: four interop-breaking defects

The current spec is **v0.6, 16 March 2024** (changelog entry dated
12 Feb 2024). Its own changelog confirms the suspicion that prompted
the audit: *"Added Trailing CRC description. Removed Weak Signal
Extensions. Corrected description of block scrambling. Removed
reference to Baseline FEC level."*

| # | Item | Spec v0.6 | Was | Impact if wrong |
|---|---|---|---|---|
| 1 | Scrambler initial state | all-ones (`0x1FF`) | `0x1F0` | **every frame, both directions** |
| 2 | PID code for "no layer 3" | `0xF` | `0xD` | every APRS UI frame |
| 3 | UI control subfield | `0x28` (opcode `101`) | hard-wired `0`, and rejected on receive | every translated UI frame |
| 4 | Payload block divisor | 239 | 205 | payload lengths 240–478, 479–717, … |

All four are corrected, and the spec's "Example Encoded Packets" are
now known-answer tests (`spec_v06_*` in `tests/il2p.rs`): our encoder
reproduces the U-frame header codeblock byte for byte, and the S-frame
vector pins the scrambler independently of our header translation. Each
defect was re-introduced and confirmed to break at least one vector.

On receive we accept any P/F and C bits and require only the UI opcode,
so a peer's command/response framing does not cause a rejection. The C
bit *is* preserved onto `Il2pHeader::Translated::command`; P/F is not,
and an AX.25 UI frame recovered through `to_ui_frame` comes back with
both clear, which is lossless for APRS and for nothing else this crate
builds.

What v0.6 "corrected" about scrambling is a **presentation** change, not
an algorithm change: it draws the LFSR in Galois form with an explicit
5-bit pipeline delay and an end-of-block flush, and working the delay
out of the schematic gives back our recurrence exactly. Only the
*effective preset* differed. One wrong constant there breaks every byte
while frames still sync on the unscrambled sync word, so the failure
presents as "header uncorrectable" or nonsense callsigns.

Confirmed correct and not to be touched: the polynomial (`x⁹+x⁴+1`),
MSB-first ordering, per-block reset, scramble-before-RS ordering,
unscrambled parity, sync word `0xF15E48` and its 1-bit tolerance, the
13-byte header bit map, SIXBIT packing, the RS field and its
first-consecutive-root convention (`fcr = 0`), max payload 1023,
balanced short final blocks with large blocks first, and the absence of
any parity-length signalling.

Two corrections to what we previously believed:

- **The trailing CRC is optional, not the interop default.** The spec
  says its use "must be coordinated between participating stations";
  the word "default" does not appear, and the reference implementation
  has no IL2P CRC at all. Implement it behind an opt-in flag. (For the
  record: AX.25 FCS over the *pre-conversion* AX.25 frame, four nibbles
  high-first, each in a Hamming(7,4) codeword, appended **outside** the
  RS-protected region.)
- **The 2/4/6/8-parity operating points are v0.4 "Baseline FEC", which
  v0.6 deleted.** Since parity length is not signalled on the wire,
  anything other than 16 is un-negotiable and cannot interoperate.

Why our own tests did not catch any of this: `tests/il2p.rs` proves the
encoder and decoder are mutual inverses, which is exactly the property
a wrong-but-consistent scrambler seed preserves. This is the
tier-1/tier-2 gap `CONTRIBUTING.md` warns about: a round-trip test
cannot establish conformance, only self-consistency. The fix is
known-answer vectors, and the spec has them.

### 6.2 IL2P on the air: what the spec vectors could not see

MEASURED: we recover **4** IL2P frames from the reference's recording
(matching its own decoder, which also loses the first to its generator's
lead-in), and the reference recovers **5 of 5** of ours.
`tests/il2p_differential.rs`, tier 4.

§6.1's spec vectors are byte vectors, and all they verified was the byte
vectors. Nothing had put IL2P **on the air** against another
implementation, and doing so found two defects that live one layer below
where a byte vector can look.

#### The defect: IL2P was being differentially encoded

The crate passed IL2P through NRZI on transmit and undid it on receive.
Specification v0.6, "Interface to Physical Layer", says of the AFSK
symbol map:

> A '1' bit is sent as a Bell 202 "mark" tone (1200 Hz), while a '0'
> bit is sent as a Bell 202 "space" tone (2200 Hz). **Differential
> encoding is not used.**

and repeats the sentence for the FSK map. NRZI *is* differential
encoding. IL2P's transition density comes from the scrambler, which is
the job NRZI plus bit stuffing does in HDLC, so the stage is redundant
and the spec forbids it.

Applying it on both sides is invisible to every internal test, since the
two cancel. It is fatal on the air. MEASURED on a reference-generated
recording: **0 frames before, 4 after**. A corroborating clue nobody had
read: the reference generator offers a *polarity* option at all, and
NRZI is polarity-insensitive by construction, so a polarity flag can
only exist for a non-differential line code.

#### The second defect: conforming to v0.6 made us undecodable

This one is the more instructive of the two: the crate was *following
the specification*, and that is what broke it.

Header byte 0 bit 7 is the v0.4 **FEC level**: set means a constant 16
parity symbols per payload block, clear means the variable
2/4/6/8-symbol "baseline" scheme, which also splits blocks on a
different ceiling (247 data bytes rather than 239). Draft v0.6 deleted
baseline FEC, mandated 16 symbols everywhere, and redefined the bit as
RESERVED. A strict v0.6 encoder therefore clears it, and we did.

**Deployed receivers did not follow.** They still read the bit, and use
it to compute *how many bytes to take off the air* for the payload, a
length committed the moment the header decodes with no delimiter and no
resynchronisation behind it. Clearing the bit while sending 16-symbol
parity told the reference to collect **61** bytes where we had sent
**75**. Its payload RS decode then failed, and one failed block discards
the entire frame.

Everything else was already byte-exact: sync word, scrambler and its
seed, header RS parity, payload blocking. That is why no vector-based
test could see it, since the specification's own example packets are
*also* generated with the bit clear. v0.6 is internally inconsistent
here: its example 3 carries a 9-byte payload block with 16 parity
symbols while its header declares the baseline plan.

The header bit is now derived from the same `Il2pParity` the payload is
encoded with, so the two cannot disagree. `tests/il2p.rs` keeps the
specification's published header bytes as a known-answer test (encoded
at a baseline parity, which is what reproduces them) *and* separately
pins that a 16-parity transmission sets the bit. Both are true, and
neither implies the other. Patching the bit afterwards is not an option:
the scrambler is multiplicative, so flipping that one plaintext bit
rewrites twelve of the fifteen wire bytes.

#### The Command/Response bit

IL2P carries AX.25's command/response indication as a single bit in the
UI control subfield, copied from the **destination** address's C bit.
This crate used to always emit "response" (control subfield `0x28`)
where the reference emits "command" (`0x2C`). No implementation examined
validates it on receive and it does not affect decodability, which is
why the interoperability tests passed with it wrong.

`ax25::Address` still does not model the C bit; `Il2pHeader::Translated`
carries an explicit `command: bool` instead. `UiFrame::build` always
writes the destination C bit set and the source's clear, which is the
AX.25 command encoding, so every frame reachable through
`encode_ui_frame` passes `command: true`, while a frame decoded off the
air may be either and needs a field to say which. A response is still
expressible by building the header directly. Transmit and receive use
separate constants, because `CONTROL_UI` had served as both:
`il2p::CONTROL_UI_OPCODE` (`0b010_1000`, what receive compares through
`CONTROL_UI_OPCODE_MASK`) and `il2p::CONTROL_UI_COMMAND` (`0b010_1100`,
what transmit writes).

The mapping remains inherently lossy: four AX.25 C-bit combinations
collapse onto one IL2P bit, so the two legacy "both bits equal" cases
cannot round-trip, and a strict AX.25-in-equals-AX.25-out assertion must
allow for that. `UiFrame` does not carry the bit either, so an
IL2P → AX.25 conversion drops it.

Note the asymmetry that hid both defects: our receive ignores the P/F
and C bits by choice and tolerates the FEC-level bit, which is why we
decoded the reference happily while emitting something it would not
accept. We were lenient on receive and wrong on transmit, and only an
on-air differential can separate the two.

### 6.3 FT8 on the air, and the limits of an on-air differential

FT8 is the most composed codec in the crate (77-bit source packing,
CRC-14, LDPC(174,91), a Gray map, a Costas sync pattern), and it was
for a long time validated only by a closed transmit-to-receive loop and
a symbol snapshot this implementation generated.

`tests/ft8_differential.rs` closes that in three legs, mirroring the WSPR
suite: the composed encoding against an independent encoder, our audio
through their decoder, and their audio through ours. All three are
green, 13/13 messages each, covering both callsign slots, the `CQ`
token, the acknowledgement flag, every spelling of the 15-bit trailer,
both extremes of the locator range, and free text.

It found **two defects**, both in what went on the air:

1. **`RR73` was encoded as the reserved token, not as a grid square.**
   The 15-bit trailer reserves values above `MAXGRID4` for the special
   messages, and `MAXGRID4 + 3` is `RR73`. But the reference's packer
   asks "is the last word a valid four-character locator?" *before* it
   consults the token list, so what goes on the air is the grid index
   32 373. The overload is safe because RR73 is a square in the Arctic
   Ocean north of Siberia (83.5 N, 175 E, computed and not assumed), and
   the same implementation's decoder special-cases the string where it
   would otherwise treat a grid as a grid.

2. **Free text was left-justified; the network right-justifies.** The
   13 characters are packed as a single base-42 integer, so which end
   the padding goes on is a multiplication by a power of 42 and changes
   every bit of the payload.

#### What the audio legs could not see

With both defects restored and the encoder comparison removed, **the two
audio legs still pass 13/13 in both directions**. That was measured, not
assumed. Their decoder maps the reserved token back to `RR73` and trims
free-text padding, so both wrong encodings are *intelligible* without
being the same transmission.

This sharpens the lesson of §6.2. There, a closed loop was insufficient
and an on-air differential was decisive. Here, an on-air differential
in **both directions against an independent implementation** was still
insufficient, because "the other end understood me" is a weaker claim
than "I sent what everyone else sends". Only comparing the composed
encoding, stage by stage, distinguishes them, which is why
`CONTRIBUTING.md` orders symbols first and audio second.

#### Remaining FT8 gap

The supported payload subset is still `i3 = 1` (standard) and
`i3 = 0, n3 = 0` (free text). The contest, DXpedition, telemetry and
nonstandard-callsign types are unimplemented, and the `/R` and `/P`
rover suffixes are rejected rather than carried. M17 remains without
any interoperability leg at all.

## 7. Remaining work

Ordered by value per unit of work.

**This ranking was re-derived against the live feed and changed.** The
older ordering put weather first, on a corpus measurement that the
2005-era recordings support and current traffic contradicts. The
corpus contains **no compressed positions at all**, so the largest
defect in the crate was invisible to every measurement in this document
until 64 918 packets were captured from APRS-IS. A corpus is a sample,
and this is what it means to over-read one.

Populations below are from two receive-only captures totalling **95 219
packets from 30 233 stations**: 64 918 on 2026-08-21 across three
servers, and 30 301 on 2026-08-23 from a full feed. The second was
taken after the design settled and is used to check the first rather
than to extend it.

**DONE since this list was written**, in order: coordinate storage
precision (§3.0); the weather absent-field spelling; the compressed
`csT` altitude, which was the last case where a well-formed packet
rebuilt to a *different value*; Mic-E position ambiguity; message
identifiers; chapter 6 position ambiguity; telemetry comma splitting;
the telemetry value type; the definition messages; and the two comment
views.

Re-measured over the **combined 95 219-packet corpus**, both captures
through the same binary, before being the state the last three items
started from:

| | before | now |
|---|---:|---:|
| rejected as malformed | 3 660 (3.84%) | **1 086 (1.14%)** |
| buildable packets | 91 559 | 94 133 |
| recovered from malformed | | **2 574** |
| newly malformed | | **0** |
| value-changed rebuilds | | **0** |
| telemetry reports decoding | 2 473 | **5 047** |

Counting from the start of this effort the rejection rate went from
4.86% to 1.14%, and value-changed rebuilds from 302 to 0. The second
capture was taken two days after the first and held out from every
design decision; measured alone it reads 3.90% before and 1.10% after,
so the figure is not an artefact of the one sample the work was built
on.

The remaining 1 086 are the population listed below as unrecoverable.
**79** of them are telemetry: reports carrying more than chapter 13's
five analog channels, and corrupt ones.

1. **Telemetry value type: DONE.** `TelemetryValue` is
   `{ mantissa: i64, decimals: u8 }`, and the analog and digital fields
   are `[Option<TelemetryValue>; 5]` and `Option<[bool; 8]>`. MEASURED
   over the live capture, it recovered **1 724** reports that rejected
   on the 1.0.1 `0..=255` cap or on a decimal point, with **0** newly
   malformed and **0** records that already decoded changing content.

   **Fixed-point milliunits were considered and refused.** MEASURED
   over 3 442 `T#` reports: the widest value carries **13 decimal
   places** (`T#296,9.2362515628338,2000`), the largest magnitude is
   **32 767 646**, and nine fields overflow `i32` milliunits. Storing
   them that way would quantise at parse, and **the rebuild check
   cannot see that**, because build writes back whatever was stored;
   see "What each instrument cannot see" in §4. A decimal mantissa and
   digit count holds every observed value exactly, with about 99 860x
   of `i64` headroom.

   It also removed the assertion comma splitting left behind. `[u8; 5]`
   and `[bool; 8]` could not express "only two channels given" or "no
   digital field sent", so `T#477,114,087,040,255` rebuilt as
   `T#477,114,087,040,255,000,00000000`, stating a fifth reading and
   eight clear bits the sender never sent. `build` now writes chapter
   13's three digits and widens only when the value needs it, which is
   what the sequence counter already did.

2. **Telemetry definition messages: DONE.** `PARM.`, `UNIT.`, `EQNS.`
   and `BITS.` are typed by `Message::telemetry_definition`, a **view**
   over the message text rather than a `MessageContent` variant, so
   `build` is untouched and typing them could not reject a packet that
   used to decode. Verified: 0 records changed verdict or kind.
   MEASURED over 95 219 packets, 5 799 of 5 805 type (99.90%); the 6
   that do not keep their text, 3 carrying 17 coefficients where
   chapter 13 has 15 and 3 holding corrupt bytes.

   The metadata is keyed on the **sender**. MEASURED, **277 of 5 805**
   address a different callsign: an EchoLink and SvxLink family sending
   from `KJ6ZD` addressed to `EL-KJ6ZD`, another prefixing `ER-`, and
   the rest unrelated. Binding on the addressee never binds, and never
   errors.
   Applying the coefficients is out of scope: the result carries a unit
   only the matching `UNIT.` message names.

3. **`!DAO!` and base-91 comment telemetry: DONE.** Both are views of
   the comment, following `/A=` altitude, so the bytes stay where they
   are and rebuilds do not move. MEASURED over 95 219 packets:
   **1 262** base-91 blocks and **773** `!DAO!` fields, refining **767**
   positions, against **0** frames of either in the 2005 corpus, so
   they are pinned by tier-2 vectors in `tests/rebuild_fidelity.rs`
   rather than by a ratchet. Both appear in Mic-E as well as the
   uncompressed and compressed forms, which is why the accessors are on
   both types.

   The order inside the scan is load-bearing and is now enforced rather
   than documented: `dao` locates the telemetry block and refuses to
   look inside it. MEASURED, scanning without that exclusion produces
   **51** false positives, three inside the telemetry of a compressed
   position, where a bogus refinement would move the position it claims
   to refine. Two independent checks that the survivors are real: all
   1 262 accepted payloads are even-length 4 to 14 with no odd-length
   candidate anywhere, and all 773 surviving matches carry a datum
   letter chapter 5 assigns (710 `w`, 63 `W`) with **zero** unassigned.
   A false positive is exactly what would not do that.

   The refinement is bounded by construction and by measurement: the
   widest addend is under a hundredth of a minute, so it cannot carry
   into the printed field, and the worst movement observed is 0.009900
   minutes, **18.37 m**, against an 18.55 m bound.

   Two spec defects are pinned by test rather than followed. Chapter
   5's `!w:\!` example claims `:` adds "27", where `:` is 58 and 58 -
   33 is **25**. And its instruction to scale a base-91 addend "by
   1.10" is an approximation of 100/91; the exact `v/91 x 0.01` minutes
   costs nothing here, because `UNITS_PER_DEGREE` divides by 546 000.

   The refinement is applied in `coordinates()`, the accessor that also
   masks ambiguity, and there is no second unrefined accessor. Every
   renderer in this project has at some point read the raw fields
   instead, twice, and a second accessor is the same trap with a
   friendlier name.

4. **Extensions on objects and items**, but only *after* `\l` area
   dispatch, for the `Tyy/Cxx` reason in §3.

5. **The RX/TX split** (§4(2), §4(c)). The four impersonation bugs are
   all still reproducible.

**Position ambiguity is done, both spellings.** Mic-E reported a
longitude up to 33 km finer than declared; the uncompressed parser
rejected 211 packets outright. `geo::Ambiguity::mask` is the one shared
rule, called by both, and the field-versus-accessor question the Mic-E
fix opened is settled: **a field holds its own wire slot, an accessor
applies declarations that live in other slots.** Both cases are guarded
by a `tests/cli.rs` test that drives the built binary, because the
renderers read the fields the first two times and a doc comment did not
stop them.

**Removed from this list: raw carriers.** They were item 6, for
timestamps, the telemetry sequence and the `csT` trailer, and they are
not wanted. §4(d) explains why the diagnostic cost is not repayable,
and the paragraph after it explains why the forwarding use case that
motivated them is served by a map that never reads the payload at all.
The two remaining F5 items, weather tag order and the compressed `cs`
no-data trailer, are cosmetic with no user standing behind them.

Still open and still correct as written: the trailing-CR message row
and the lower-case hemisphere row are **not** defects. Chapter 14 says
not to send the terminator and notes that igates strip it; chapter 6
specifies the upper-case hemisphere letter. Both are F4 normalisation
and the classification in `tests/common/mod.rs` scores them as correct.

Left off the list on purpose: the raw rain counter `#` (the spec
gives it no width, unit or scaling) and the Mic-E device *suffix* (0
frames here, so nothing can validate it).

**A standing note on measurement discipline.** The item that used to
head this list was "fold Mic-E into `AprsPacket` via a
destination-taking entry point", billed as the largest single item at
41% of traffic. It was done, on `Decoded` rather than on `AprsPacket`
(§2 gives the transmit-safety reason), and **MEASURED, it moved
structured coverage by 0.00 points**, because those frames were already
decoded via `RxFrame::mic_e` and already counted. What it bought was a
correct label, one call per frame, and six duplicated pad sites
collapsed. That estimate was carried through three audits before anyone
measured it. The retraction is recorded here rather than quietly
rewritten.

The coverage measurements in §2 and §5 remain the ratchet: raise the
floors in `tests/corpus_aprs.rs` (`MIN_STRUCTURED_PERCENT`, which sits
at 93.0, and `MIN_FIELDS` beside it, because the percentage is
structurally blind to field-level loss) and `tests/aprs_differential.rs`
after each step.
