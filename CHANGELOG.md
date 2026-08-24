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

- **Objects and items read chapter 9's compressed position.** They
  called the uncompressed-only parser, so the base-91 form the chapter
  permits in an object or item was refused outright. MEASURED over
  205 635 live packets: 106 objects and 42 items from 26 senders,
  including a Hellenic weather service alert set, whose positions were
  plotted nowhere.

  `Object` and `Item` gain `compressed: bool`, and both now go through
  the position module's own body parser and writer rather than a second
  copy of the base-91 arithmetic. `encoded_len` on both is no longer
  `const fn`, because the length depends on which form the report
  carries. The truncation floor drops from 37 to 31 bytes, since
  demanding the uncompressed length would refuse every compressed
  object before the position parser saw one.

  The three-byte `cs` trailer is not carried, as `Position` does not
  carry it either: 43 of the 148 lose course, speed or altitude there,
  and all 148 gain the position.

- **A direction the wire cannot mean is no longer published.** The
  parser accepted a wind direction outside chapter 12's `000` to `360`
  while `build` enforced it, so `767` reached the typed value, was
  rendered as "wind dir 767 deg", and then could not be written back.
  It is now reported absent, keeping the other eight measurements.

  Mic-E had the same shape of bug, invisible to the rebuild check
  because Mic-E is not buildable from an information field. Chapter 10
  packs the course as `(DC mod 10) * 100 + SE`, which reaches 999, then
  subtracts 400 once, leaving 400..=599 reachable while the field is
  `0..=360`. `MicE::course` documents that range and `MicE::new`
  enforces it, but the decoder built the struct directly. Out-of-range
  courses are now reported as 0, which chapter 10 spells "unknown or
  indefinite". MEASURED: 24 packets across both faults, now 0.

### Added

- **docs.rs builds the whole API.** Without
  `[package.metadata.docs.rs]`, docs.rs builds default features only —
  `mod` and `demod` — which would have published the modulator and the
  demodulator and hidden every module behind `aprs`, `ax25`, `tnc`,
  `kiss`, `digipeat`, `g3ruh`, `fx25`, `il2p`, `wspr`, `ft8` and `m17`.
  Feature badges now render on each gated item.

- **`scripts/check-public-api-exercised.sh`.** CONTRIBUTING.md says "no
  public function should be reachable by users and by nothing else" and
  gives a recipe requiring a CALL, not a mention, splitting each `src/`
  file at its `#[cfg(test)]` boundary so implementation code cannot
  vouch for itself. Run by hand once, it found four; then it stopped
  being run. Automated and wired into CI, it found two more —
  `aprs::decoded_from_ui`, reached only from the CLI binary, and
  `aprs::monitor::is_q_construct`, reached only from its own module.
  Both now have tests in `tests/coverage_fill.rs`.

- **`scripts/check-coverage-citations.sh`.** `docs/COVERAGE.md` cites a
  few hundred tests by name and says they are checked mechanically
  against `--list --include-ignored`. Nothing was doing the checking.
  This does, CI runs it, and a renamed or deleted test can no longer
  leave a citation behind that reads as evidence and is not.

- **CI gates the things that only bite after publication**: `cargo doc`
  under `-D warnings` (which was failing), `cargo publish --dry-run`,
  `cargo-deny` over a dependency tree that `#![forbid(unsafe_code)]`
  says nothing about, the MSRV the manifest actually promises (1.96.0,
  where the toolchain file pins 1.96.1), stable/beta as advisory, and
  `cargo check` on macOS and Windows — `cpal` and `serialport` had only
  ever been built against ALSA. `Cargo.lock` is now tracked, since the
  crate ships a binary.

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

- **`decode --verify-rebuild` re-parses instead of comparing bytes.**
  It compared `build(parse(w))` against `w` and stopped, so `differs`
  covered both a harmless re-spelling and a changed value, and the
  second was invisible. It now re-parses and compares typed values.

  | verdict | meaning |
  |---|---|
  | `exact` | byte for byte |
  | `differs` | bytes changed, the value did not |
  | `value_changed` | does not parse back to the same value |
  | `rejected` | output this crate would itself refuse |
  | `failed` | build undefined where parse succeeded |

  MEASURED over 205 635 packets, 190 804 of them buildable: 11
  `value_changed`, 0 `rejected`, 0 `failed`. The 11 are chapter 6
  ambiguity, where the longitude field holds digits the latitude
  declared ambiguous and `coordinates()` masks both to the same
  position, so no caller sees a moved station. This bumps the JSON Lines
  schema to **2**.

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

- **A modem config re-checks its tones against its own sample rate.**
  `TonePair::new` takes a `SampleRate`, checks Nyquist and then discards
  it, so the pair carries no memory of what cleared it. A pair validated
  at 48 kHz could be handed to a `ModulatorConfig` or `DemodulatorConfig`
  at 8 kHz, where it aliases — a hole in the claim that `types` makes
  "illegal modem configurations unrepresentable". Both constructors now
  return `ConfigError::ToneOutOfRange` for a tone that does not fit.

  The `TonePair` constants are plain `const`s that no rate has ever
  cleared; they pass only because every one of their tones sits below
  the Nyquist of `SAMPLE_RATE_MIN`, which is now pinned so a future
  constant cannot quietly break it.

- **FT8 refuses a non-finite channel LLR instead of fabricating a
  decode.** The LDPC hard decision is `posterior < 0.0`, and `NaN`
  compares false against everything, so a single poisoned LLR read as a
  confident bit 0 and `ldpc_decode` returned a codeword assembled from
  nothing. `llrs_from_energies` divides by a mean floored at
  `f32::MIN_POSITIVE`, so one infinite energy anywhere in the symbol
  window was enough to reach it. CRC-14 caught most of it downstream,
  which is a 1-in-16384 gate standing in for a check the decoder could
  make directly. New `Ft8Error::LlrNotFinite` (the enum is
  `#[non_exhaustive]`, so this is additive).

- **The lazy iterator adapters are `#[must_use]`.** `Iterator`'s own
  `#[must_use]` covers `-> impl Iterator`, not the named structs these
  return, so dropping the result of `Modulator::i16_samples`,
  `Demodulator::i16_bits`, the baseband pair, `nrzi::encode_iter` or
  `scrambler::scramble_iter` was a silent no-op.

- **`warble decode ... | head` exits quietly instead of panicking.**
  Rust ignores `SIGPIPE`, so the `print!` family turns a closed stdout
  into `failed printing to stdout: Broken pipe` and exit 101. Every
  subcommand that writes to stdout now goes through a buffered writer
  that treats a closed downstream as "stop, successfully", which is what
  every other filter in a Unix pipeline does. `decode` also stops
  decoding at that point rather than grinding through a capture nobody
  is reading.

- **`warble aprsis ... | head` no longer reconnects to a volunteer
  server.** The sink write shared an `io::Result` with the socket reads,
  so a closed stdout was reported as "connection failed" and sent the
  retry loop back to Tier 2 on a doubling backoff. A broken pipe
  downstream now ends the run: there is nothing to reconnect *for*.

- **`warble aprsis` bounds the line it will read.** `read_until` has no
  upper limit, so a server that streamed bytes without a terminator grew
  the buffer until the process died. Lines are now capped at the
  512-byte APRS-IS maximum, which the specification already says a
  reader should treat as a protocol violation rather than growing to
  fit; an overlong line is dropped whole and the reader resynchronises
  on the next one.

- **`warble serve` survives a client that stops reading.** The broadcast
  loop held the client-list mutex across a blocking `write_all`, so one
  wedged peer could stall the accept loop and the shutdown sweep, which
  need the same mutex. The list is now snapshotted under the lock and
  written outside it, and admitted sockets carry a write timeout.

- **`warble serve` gives a disconnected client its slot back.** Clients
  were removed only by a *failed broadcast write*, so on a quiet band
  with no traffic eight connect/disconnect cycles — an ordinary TNC
  reconnect loop over a few days — left eight dead sockets holding every
  slot and the server refused new clients until restarted.

- **`warble serve` shuts down on a failing TX sink.** The decode loop
  read its audio source to exhaustion and never consulted the shutdown
  flag. A sound card or a piped PCM stream never reaches EOF, so a sink
  failure (full disk, closed pipe) left the run blocked on a thread join
  forever.

- **`Coordinates::bearing_to` returns the nearest whole degree.** The
  360-candidate search used the truncating sine lookup, so each
  candidate sat up to 0.088 degrees below its nominal angle and every
  half-degree decision boundary moved with it. MEASURED over 3240
  directions, 28 came back as the neighbouring degree — and unlike the
  documented `cos_q15` tilt, this happened at the equator too.

- **The integer cosine no longer leans one way.** Its interpolation
  arithmetic-shifted the product, which floors, and the delta keeps one
  sign across a quarter turn — so the term cost up to 1 LSB
  one-sidedly *down* and every east-west distance read short. MEASURED:
  mean error −0.49 LSB, now centred; the east/north residual window
  narrows from a swept [−2.314, +0.280] to [−0.838, +0.877].

  This moves `distance_to`'s accuracy table both ways, because the old
  bias had been cancelling part of the equirectangular projection error,
  which over-estimates east-west distance. Short paths improve
  (0.00554% → 0.00385% to 100 km below 45 degrees) and 300 km paths give
  up to 0.002 percentage points back (0.01282% → 0.01496%). The
  cancellation was luck rather than design, and it was worth least
  exactly where this crate operates.

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
