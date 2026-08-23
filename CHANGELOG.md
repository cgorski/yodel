# Changelog

Notable user-facing changes, newest first. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project
uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

This file records **what changed between releases**. Development detail
belongs in the git history and in pull requests, where it is useful and
where it stays accurate: why a given commit was made, what a defect
turned out to be, which approach was tried and abandoned.

## [Unreleased]

### Changed (breaking)

- **Coordinates are stored exactly for every APRS position format.**
  `Latitude` and `Longitude` held signed `i32` in 1/100 arc-minutes,
  18.55 m per unit. That is exact for `DDMM.hh` and Mic-E and too
  coarse for the other three formats: a compressed base-91 position
  carries 0.29 m steps, so 63.5 distinct wire positions collapsed onto
  one stored value and could never be written back.

  They now hold `i64` in units of 1/342 833 400 000 000 of a degree,
  which every format's denominator divides exactly, so no conversion
  rounds at any point.

  Migration, all of it a compile error rather than a silent change of
  meaning:

  | was | now |
  |---|---|
  | `Latitude::new(i32)` | `Latitude::new(i64)`, in the new unit |
  | `Latitude::hundredths_of_minute() -> i32` | `Latitude::units() -> i64` |
  | `Longitude` likewise | |
  | `{Geo,Aprs,MicE,Nmea}Error::Bad{Latitude,Longitude}.got: i32` | `i64` |

  The accessor is **renamed** rather than redefined on purpose: a
  `hundredths_of_minute` that kept its name and changed its unit would
  let every existing call site keep compiling while meaning something
  new. Code holding a coordinate in 1/100 arc-minutes should multiply
  by the new public `geo::UNITS_PER_HUNDREDTH_MINUTE`, or better, move
  to `from_degrees` / `to_degrees`, which are unchanged.

  MEASURED over 64 918 live packets: compressed positions decoded a
  median of 3.84 m and up to 9.30 m away from what the sender
  transmitted, and now decode exactly. Uncompressed positions are
  unaffected, byte for byte.

  Type sizes grew 8 bytes per coordinate pair: `Position` 48 to 56,
  `PositionCs` and `PositionTimestamped` 64 to 72, `MicE` 48 to 56,
  and the enums holding them, `Decoded` 192 to 200, `DecodedKind` and
  `AprsPacket` 176 to 184.

- **The compressed `csT` trailer is built by inverting the parser, not
  the power.** `build` chose the code whose `1.002^n` was numerically
  nearest the altitude it held. The parser truncates that power to
  whole feet, so the nearest code is routinely one below the code that
  reads back as the right number: code 2951 is 363 feet, and the code
  nearest 363.0 is 2950, which reads as 362.

  The bytes emitted for a given `CompressedCs` may therefore differ
  from before. **No value changes**, and values that used to change no
  longer do.

  MEASURED, whole domain: 999 of the 8281 altitude codes decoded to one
  value and rebuilt to another. The loss was not bounded at one foot,
  because igates and digipeaters parse and re-emit: iterating the old
  rule to a fixed point, 417 codes lost more than a foot and code 3131
  walks 520 feet down to 480 over 41 passes. Over 64 918 live packets,
  value-changing rebuilds fell from 302 to 0 and byte-exact rebuilds
  rose from 50 144 to 50 310, with no packet losing byte-exactness.

- **`MicE::coordinates()` reports a position at the precision the
  sender declared.** Mic-E spells position ambiguity in the destination
  address and cannot blank a longitude digit, so the longitude is
  always transmitted at full precision and chapter 10 makes discarding
  the matching low-order digits the receiver's job. The accessor now
  applies the declaration to both axes.

  A caller that wants the transmitted longitude reads the `longitude`
  field, which is unchanged, so `MicE::encode` remains the exact
  inverse of `decode`.

  New: `geo::Ambiguity::mask` and `geo::Ambiguity::step`, the shared
  masking rule for both wire spellings of ambiguity.

  MEASURED over 64 918 live packets: 140 Mic-E reports from 59 senders
  declare ambiguity, and 38 of their 280 axis readings were more
  precise than declared, by a median of 729 m and up to 33 km. Now 0.

- **Telemetry is parsed by splitting on commas.** The parser demanded a
  fixed 34-byte layout, `T#` plus three sequence digits, five `,AAA`
  groups and `,dddddddd`. MEASURED over 64 918 live packets, only 1 401
  of 3 442 `T#` reports matched it.

  Chapter 13's shapes are now accepted: fewer than five analog channels,
  no digital field at all, a sequence of one to five digits, and the
  un-delimited telemetry comment glued to the digital bits.

  The digital field is identified **first**, as the last
  comma-separated field beginning with eight `0`/`1` characters whose
  ninth is not a digit. Assigning analog slots left to right instead
  would read the trailing `00000000` of a two-channel report as a third
  analog value and find no digital field, turning a loudly rejected
  packet into a silently wrong one.

  `Telemetry::seq` widens to `u32` and `build` writes its natural width
  with a minimum of three: 88 captured reports carry a four-digit
  sequence and 16 carry five, and writing 1812 in three digits would
  report 812. `AprsError::TelemetrySequenceOutOfRange.got` widens with
  it.

  263 packets recovered, with **0** telemetry records that already
  decoded changing content and **0** round-trip value failures over the
  1 664 telemetry packets in the capture.

  Known and staged: `[u8; 5]` and `[bool; 8]` cannot express "only two
  channels given" or "no digital field sent", so 262 of the recovered
  reports rebuild with padding that asserts channels and bits the sender
  did not send. The next release note on telemetry removes that.

- **Chapter 6 position ambiguity is honoured.** A station that does not
  wish to publish an exact fix blanks trailing coordinate digits with
  spaces (`4903.__N` is "to the nearest minute"). The parser rejected
  the packet; it now reads the level.

  `Position`, `Object`, `Item` and `PositionWeather` gain a public
  `ambiguity: geo::Ambiguity`, and their `coordinates()` returns a
  position **masked to the declared precision**. Chapter 6 makes the
  latitude authoritative and lets the longitude carry its digits in
  full, so reading the `longitude` field directly can publish a
  position finer than the sender claimed; the accessor cannot.

  Compressed positions are excluded, because a space there is the `cs`
  no-data trailer rather than a blanked digit.

  MEASURED over 64 918 live packets: 221 packets from 73 senders, all
  418 space-bearing fields right-aligned, 211 recovered from rejection
  (123 positions, 80 objects, 8 weather reports) with **0** records
  that already decoded changing in any way.

  Type sizes: `Decoded` 200 to 208, `DecodedKind` and `AprsPacket` 184
  to 192. The four structs are unmoved; `Ambiguity` is one byte and fits
  their existing padding.

- **New: `warble level`, a receive-level meter.** Reads the same stdin
  PCM every other subcommand takes and reports rms, peak, clipped-sample
  count, a verdict and the inferred squelch state, redrawing in place on
  a terminal and one line per window otherwise.

  `--until-good <SECS>`, `--for <SECS>` or `--then-decode`, one
  required so it cannot hang. Both bounds count **audio** time rather
  than wall clock, so a file or a fast pipe behaves like a live capture.

  Clipping is reported separately from peak because rms cannot see it
  and peak saturates whether one sample is pinned or ten thousand. No
  new dependency.

- **New: `warble ptt`, serial push-to-talk.** Keys a transmitter on RTS
  or DTR while a player sends the audio, so the control line is held
  for exactly the player's lifetime. That shape is forced rather than
  chosen: PTT must be asserted before the first sample reaches the air
  and released after the last, and a process writing PCM into a pipe
  knows neither moment because the player downstream buffers.

  `--hold <MS>` keys for a fixed time instead, for checking an
  interface before trusting it. `--list` enumerates ports. `--signal`,
  `--invert`, `--lead` and `--tail` cover the wiring and timing
  variations.

  Every exit path releases the line, including errors and panics, and
  `--max` (60 s default) kills a hung player rather than leave a
  transmitter keyed. Note that some USB-serial drivers assert RTS at
  open, keying a wired radio before any program logic runs; this drops
  both lines immediately after opening.

  Behind a new `ptt` feature, included in the `cli` aggregate. It adds
  `serialport` with default features off, so no library build and no
  embedded feature set sees it, and Linux does not need libudev.

- **A telemetry channel holds the decimal the sender wrote.** Chapter
  13 gives an analog channel three digits and the range `000..=255`.
  Real senders exceed both, so `[u8; 5]` rejected **2 574** reports in
  a 95 219-packet capture for carrying an ordinary reading.

  | was | now |
  |---|---|
  | `Telemetry::analog: [u8; 5]` | `[Option<TelemetryValue>; 5]` |
  | `Telemetry::digital: [bool; 8]` | `Option<[bool; 8]>` |
  | `AprsError::BadAnalogValue { got: i32 }` | `{ position: usize }` |

  `TelemetryValue` is `{ mantissa: i64, decimals: u8 }`, the value
  being `mantissa` scaled by ten to the minus `decimals`.
  `Telemetry::integer_channels([i64; 5])` builds the all-integer case
  that a transmitting station usually wants.

  Fixed-point milliunits were the obvious alternative and would have
  corrupted values. MEASURED: the widest field carries **13** decimal
  places and the largest magnitude is **32 767 646**, so `i32`
  milliunits truncate the first and overflow on nine fields. The
  rebuild check cannot see that, because `build` writes back whatever
  was stored and a shortened value rebuilds byte-exactly against its
  own shortened self.

  A value above 255 is no longer an error. `BadAnalogValue` now means a
  number the value type cannot hold, and carries the offset of the
  field rather than a value that by definition did not fit. The new
  `AprsError::TelemetryDecimalsOutOfRange` rejects a fraction wider
  than an `i64` mantissa can pair with an integer digit.

  The `Option`s remove an assertion: `T#477,114,087,040,255` used to
  rebuild as `T#477,114,087,040,255,000,00000000`, stating a fifth
  reading and eight clear bits the sender never sent.

- **`coordinates()` is no longer `const fn`**, on `Position`,
  `PositionCs`, `PositionTimestamped` and `MicE`. It now also applies a
  `!DAO!` refinement found in the comment, which means scanning a byte
  slice. The alternative was a second, refined accessor beside the
  unrefined one, and that is the trap this crate has already fallen
  into twice: every renderer read the raw coordinate fields instead of
  the masking accessor. One accessor that is always right beats two
  where the caller picks.

### Added

- **Chapter 13 telemetry definition messages are typed.** `PARM.`,
  `UNIT.`, `EQNS.` and `BITS.` say what a station's channels measure,
  in what units, scaled by what coefficients, with which digital
  senses. MEASURED, **5 805** of 95 219 packets carry one, from 1 279
  senders, and 99.90% of them type.

  `Message::telemetry_definition` is a **view** over the message text
  rather than a `MessageContent` variant, so `build` is untouched, the
  text still rebuilds byte for byte, and a form this crate cannot type
  returns `None` with the text still in hand.

  Bind these on the **sender**, not the addressee. A definition
  describes the station that sent it and usually addresses itself, but
  MEASURED **277 of 5 805** address a different callsign, so keying on
  the addressee never binds and never errors. Applying the `EQNS`
  coefficients is out of scope: the result carries a unit that only the
  matching `UNIT.` message names.

- **Base-91 comment telemetry and `!DAO!` are read from position
  comments.** Both are views, following `/A=` altitude: the bytes stay
  in the comment, so rebuilds do not move. MEASURED over 95 219
  packets, **1 262** base-91 blocks and **773** `!DAO!` fields,
  refining **767** positions, in uncompressed, compressed and Mic-E
  reports alike.

  Base-91 telemetry carries two bytes per value, so a channel is
  `0..=8280`. Its digital word runs the **opposite way** from the `T#`
  form, LSB first, and is unambiguous only when all five analog
  channels precede it.

  `!DAO!` refines a `DDMM.hh` position to about a foot. The addend is
  exactly `v / 91 x 0.01` minutes; the spec's instruction to scale "by
  1.10" is an approximation of `100/91`, and the exact form is free
  because `UNITS_PER_DEGREE` divides by 546 000. It is always under a
  hundredth of a minute, so it cannot carry into the printed field.

  Recognising telemetry **before** `!DAO!` is required, and enforced
  rather than documented: a telemetry payload is arbitrary base-91
  bytes and produces `!x??!` sequences. MEASURED, scanning without the
  exclusion yields 51 false positives, three inside the telemetry of a
  compressed position where a bogus refinement would move the position
  it claims to refine.

- **Plain-text AX.25 beacons are classified instead of mislabelled.**
  Not every frame on 144.39 MHz is APRS. Stations beacon readable text:
  a TNC's station identification (conventionally addressed to `ID`), its
  beacon banner (`BEACON`), a digipeater's firmware version (`UIDIGI`),
  and human-written weather bulletins. These carry **no data type
  identifier**, and the crate was reporting the first letter of
  `WA6TK/R RELAY/D` as a data type identifier of `W`.

  `DecodedKind::Text { text }` names them. The discriminator is chapter
  5's own table, which marks `A`-`S`, `U`-`Z`, `a`-`z`, `0`-`9`, `|` and
  `~` as "[Do not use]", with `T` (telemetry) carved out: a field
  opening with one of those is not an APRS packet by the specification's
  account. Identifiers the spec assigns or reserves, such as `?`, `{`
  and `,`, stay `Unsupported` and keep naming the byte, so the
  diagnostic still says which format is missing.

  MEASURED: 75 of 2182 off-air frames and 749 of 95 219 live packets,
  with zero packets newly rejected.

  `Decoded::is_typed` answers **`false`** for `Text`, so the
  structured-coverage figure does not move by fiat. The new
  `Decoded::is_aprs` answers `false`, which is what makes a correct
  denominator possible: `tests/corpus_aprs.rs` now reports coverage of
  APRS frames (**96.4%**) beside coverage of every frame heard (93.1%),
  with a floor on each and a ceiling on how many frames may be set aside
  as non-APRS, so the new figure cannot be flattered by shrinking its
  denominator.

- **`warble aprsis`** reads the live APRS-IS feed and writes TNC2
  monitor lines, the format `decode --tnc2` already takes, so the two
  compose into a pipeline. `--filter` subscribes to a slice of the
  traffic on port 14580, which sends nothing without one; `--full-feed`
  takes port 10152, which sends everything and ignores filters. The
  combinations that would connect and deliver nothing are refused
  before any socket opens.

  Receive-only by construction: the passcode is the constant `-1`,
  there is no flag to change it, and nothing is written to the socket
  but the login line. The callsign is required, because servers refuse
  the placeholder `N0CALL` and a shared volunteer network is not
  somewhere to connect anonymously.

### Fixed

- **A `{` or an `ack` prefix in message text is no longer a rejection.**
  Chapter 14 puts the message identifier at the end of the text and caps
  it at five characters, so a longer run is not an identifier and the
  brace belongs to the text. The parser errored on its length instead,
  and the `ack`/`rej` arm had the same defect one step earlier.

  MEASURED: 203 packets from 24 senders recovered, taking every message
  in the capture to a successful parse. None of the recovered packets
  claims an identifier, and none claims `Ack` or `Reject`: a message
  with no valid identifier is one chapter 14 says must not be
  acknowledged. `build` still rejects an over-long id, because there a
  caller asked for something that cannot be spelled.

- **An absent weather field is omitted, not spelled with dots.**
  `write_fields` emitted every standard tag unconditionally. Chapter 12
  permits both spellings, so the choice was free until the placeholder
  run turned out to be written *before* the unparsed remainder: on any
  report whose tag scan stopped early, the builder inserted synthetic
  bytes into the middle of content the sender did send. On one real
  report a four-character temperature stopped the scan and 53 bytes
  became 74, with five tags appearing twice. That output is malformed;
  omission cannot lengthen a packet, so it cannot do that.

  MEASURED: 1 308 live weather reports have both a non-empty remainder
  and at least one absent standard field. Byte-exact rebuilds went from
  49 770 to 50 144, trading 786 reports that omit against 413 that
  spelled absence with dots; every one of the 413 was checked and has
  dotted fields on the wire.

- **`AprsPacket::to_vec` no longer refuses information fields longer
  than 256 bytes.** It built into a fixed stack array sized from the
  AX.25 frame budget, which is the right ceiling for radio and the
  wrong one for a function whose job is to allocate. It now starts
  there and grows once from the length the error reports.

  No signature change, and no caller can be harmed: inputs that used to
  return `BufferTooSmall` now succeed. Only a caller depending on that
  error for packets over 256 bytes is affected.

  APRS-IS imposes no frame size, so this is reachable in practice:
  MEASURED over 30 051 live packets, 115 exceed 256 bytes and every one
  of them that had a builder failed to re-serialize.

## [0.1.0] - unreleased

First release. Everything below is new.

### The modem

Bell 202 AFSK (1200 baud, 1200/2200 Hz) software modem, plus Bell 103 and
300-baud HF profiles, and arbitrary baud/tone pairs through validated
constructors.

- **Modulator**: continuous-phase FSK driven by a single `u32` phase
  accumulator. An integer remainder accumulator keeps fractional
  samples-per-bit ratios (36.75 at 44.1 kHz) from drifting.
- **Demodulator**: a pluggable `Discriminator` front end feeding a
  digital-PLL bit slicer with a lock-adaptive loop gain. The default
  front end is a dual-tone quadrature correlator whose observation
  window stretches to the tone-orthogonality point where that measurably
  helps.
- **`TncReceiver`**: a multi-chain diversity receiver. Parallel decision
  chains run over raw, band-passed and pre-emphasized copies of the audio
  at swept space-tone gains, with cross-chain bit voting, single-bit FCS
  repair, and content de-duplication.
- Both `i16` and `f32` sample paths; the `i16` path is integer-only.

### Protocol stack

NRZI line coding, AX.25 UI frames (addresses, CRC-16/X.25 FCS, HDLC
framing), and APRS payloads: position in every form (uncompressed,
base-91 compressed, timestamped, with the 7-byte data extension), status,
message, weather, telemetry, object, item, Mic-E, and receive-only NMEA
0183, Peet Bros Ultimeter, third-party encapsulation and station
capabilities.

Also: KISS TNC framing, WIDEn-N digipeater primitives, G3RUH 9600-baud
scrambled baseband, FX.25 and IL2P forward error correction, and the
WSPR, FT8 and M17 (packet data) modes.

### Design

- `#![no_std]` and `#![forbid(unsafe_code)]` in the core, with **no heap
  allocation**: builders write into caller-provided buffers, parsers
  borrow from the input, transmit paths are lazy iterators.
- No runtime dependencies in the default feature set. Every protocol
  layer is independently usable and off by default.
- Cross-built for `riscv32imac` and `thumbv7em` on every `no_std` feature
  combination.

### Tooling

- `warble` CLI: encode, decode (with JSON Lines output), generate test
  signals, benchmark, and serve as a KISS TNC over TCP.
- Optional tokio (`async`) and Embassy (`embassy`) adapters.

[Unreleased]: https://github.com/cgorski/warble/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/cgorski/warble/releases/tag/v0.1.0
