# warble

An amateur radio digital stack in Rust, from PCM samples up to decoded
APRS, with the whole core `#![no_std]`, `#![forbid(unsafe_code)]`,
allocation-free and free of runtime dependencies.

The starting point is a Bell 202 AFSK software modem: 1200 baud,
1200 Hz mark / 2200 Hz space, the physical layer of amateur packet radio
and of the telephone modems it was designed for. On top of that sits the
rest of the stack, and beside it several other modes that share the same
seams.

**One crate, sample to packet and back.**

| layer | what is implemented |
|---|---|
| modem | Bell 202 AFSK 1200 baud, G3RUH-scrambled 9600 baud, both directions, `i16` and `f32` PCM paths |
| line coding | NRZI, HDLC bit stuffing and framing, CRC-16/X.25 |
| frame | AX.25 UI frames, KISS TNC framing, FX.25 forward error correction, IL2P |
| packet | APRS: positions, weather, telemetry, objects, items, messages, status, capabilities |
| relay | WIDEn-N digipeater primitives with duplicate suppression |

The APRS layer reads what is on the air rather than what is convenient:
all four uncompressed position forms, base-91 compressed positions, every
`csT` variant, Mic-E, `!DAO!` datum and precision, base-91 comment
telemetry, the chapter 13 telemetry definition messages, the 7-byte data
extensions, and receive-only raw NMEA 0183, Peet Bros Ultimeter and
third-party encapsulation.

Three weak-signal and digital-voice-adjacent modes share the same
building blocks: **WSPR** (beacon plus a `no_std` receive path), **FT8**
(transmit plus decode), and **M17** packet mode.

**It runs where you need it.** The same library compiles for a
microcontroller and for a workstation:

* **Embedded first.** No allocation, no `unsafe`, no dependencies in the
  core. Worked examples for bare metal, [embassy](examples/balloon_tracker_embassy.rs),
  [RTIC](examples/balloon_tracker_rtic.rs), and a real
  [ESP32-C3 RISC-V board](examples/esp32-riscv/) with a hardware guide.
  `scripts/check-embedded.sh` cross-builds every `no_std` feature set.
* **Desktop too.** A `warble` command-line tool decodes and encodes WAV
  files or live audio pipes, runs as a KISS TNC over TCP or stdio, keys a
  transmitter over a serial line, meters your receive level, reads the
  live APRS-IS feed, and generates seeded test signals for benchmarking.
* **Async when wanted.** Optional tokio adapters, with the runtime-free
  path staying the default.

**Measured, not asserted.** Correctness claims here come with numbers and
the method that produced them:

* **2182 real off-air frames** from a published test recording are
  demodulated and decoded on every run, behind ratchet floors that fail
  the build if coverage regresses. **96.4% of the APRS frames yield a
  typed value.** The 3.6% that do not are traffic that should be
  refused: a tracker with no GPS fix beaconing zeros where the
  hemisphere belongs, and frames whose payload is visibly corrupted.
  Counting every frame heard on the channel, including the plain-text
  station identifications and beacon banners that are not APRS at all,
  it is 93.1%.
* **95 219 live APRS-IS packets** across two captures are used to check
  that a decoded packet re-serializes to the bytes that arrived. 1.14%
  are rejected as malformed, and zero re-serialize to a different value.
* **A differential harness** checks the stack in both directions against
  an independently developed reference modem, 320 cases over 16 packet
  kinds, currently 320/320 in both directions.
* **Seeded noise ladders and fuzzing**: frame recovery is pinned at
  fixed SNRs, and every parser is driven with hundreds of thousands of
  corrupted inputs. No panics, only typed errors.

What "correct" means for a decoder is itself worked out, in
[docs/APRS_CONFORMANCE.md](docs/APRS_CONFORMANCE.md) section 4: parse and
build as partial maps, five properties that separate a rebuild that lost
information from one that chose a different legal spelling, and a written
account of what each measurement cannot see.

```sh
cargo add warble --features tnc          # library: samples to packets
cargo install warble --features cli      # the command-line tool
```

```sh
# Decode a recording, one line per frame.
warble decode traffic.wav

# Build a position beacon and write it as audio.
warble encode --from N0CALL-9 --lat 39.1 --lon -94.6 --out beacon.wav
```

## Design

[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) is the starting point for
contributors. It has the layer diagram, a per-file module map, an
account of the PHY seam, and the feature-flag rationale table.

The API is streaming in both directions; no type in the core owns a growable
buffer or returns a collection.

* **Modulator** (bit in, sample out): continuous-phase FSK. A single 32-bit
  phase accumulator runs across bit boundaries, so switching tones only swaps
  the per-sample phase increment and the waveform never has a discontinuity.
  Fractional samples-per-bit ratios (e.g. 36.75 at 44 100 Hz) are handled
  with an integer remainder accumulator, so the sample count never drifts.
* **Demodulator** (sample in, bit out): composes two stages.
  1. A dual-tone **quadrature correlator** discriminator turns each PCM
     sample into a signed soft metric (positive = mark, negative = space).
     The front end is pluggable through the `Discriminator` trait;
     `QuadratureCorrelator` is the default.
  2. A **PLL bit slicer** recovers the bit clock from metric zero
     crossings and emits one raw tone decision per bit cell. The loop
     gain is lock-adaptive: 1/2 while searching, so an alternating
     preamble acquires within a few transitions, then 1/8 once
     transitions land consistently near the expected phase, so the
     clock coasts through fades. No NRZI or other line decoding is
     applied.
* Both **`i16`** and **`f32`** PCM paths exist on each side; the `i16` path
  uses integer arithmetic only.
* Configuration types (`SampleRate`, `BaudRate`, `TonePair`,
  `ModulatorConfig`, `DemodulatorConfig`) are built through validated
  constructors returning `Result<_, ConfigError>`, so an invalid
  configuration cannot be represented.

## Features

| Feature | Enables                                                                 | Requires            | `no_std` | Default |
|---------|-------------------------------------------------------------------------|---------------------|----------|---------|
| `mod`   | The modulator (`Modulator`, `ModulatorConfig`)                          | —                   | yes      | yes     |
| `demod` | The demodulator, discriminator, and slicer                              | —                   | yes      | yes     |
| `alloc` | Heap-backed conveniences (e.g. `TncTransmitter::transmit_to_vec_i16`)   | —                   | `alloc`  | no      |
| `std`   | std conveniences (no dependencies)                                      | `alloc`             | no       | no      |
| `wav`   | WAV I/O via `hound`: the `wav` module + the CLI's WAV edges             | `std`               | no       | no      |
| `nrzi`  | NRZI differential line coding (streaming encoder/decoder)               | —                   | yes      | no      |
| `ax25`  | AX.25 UI frames: addresses, CRC-16/X.25 FCS, HDLC framing               | `nrzi`              | yes      | no      |
| `aprs`  | APRS payloads over AX.25: position (uncompressed, base-91 compressed with all csT variants, timestamped `/`/`@` forms, and the 7-byte data extension — course/speed, wind, `PHG`/`PHGR`, `RNG`, `DFS` — plus `/A=` altitude), status, message, weather, telemetry, object, item; and receive-only NMEA 0183, Peet Bros Ultimeter, third-party encapsulation and station capabilities via the total `Decoded` entry point | `ax25` | yes    | no      |
| `micE`  | Mic-E compressed position reports (encode + decode)                     | `aprs`              | yes      | no      |
| `digipeat` | WIDEn-N digipeater primitives: served aliases, the pure `relay_decision` core, `DupeRing` duplicate suppression | `ax25` | yes | no |
| `kiss`  | KISS TNC framing: escaping encoder, streaming deframer, command bytes   | — (standalone)      | yes      | no      |
| `g3ruh` | G3RUH 9600-baud LFSR scrambler/descrambler (x¹⁷ + x¹² + 1); with `mod` / `demod` also the scrambled-baseband modem front end | — (standalone) | yes | no |
| `fx25`  | FX.25 FEC layer: RS(255,k) codec over GF(256) + correlation-tag framing (the tag-hunting receiver additionally needs `ax25`) | — (standalone) | yes | no |
| `il2p`  | IL2P frame codec: sync word + 13-byte header codec + x⁹ + x⁴ + 1 scrambler + per-block RS(255,k) FEC | `ax25` | yes | no |
| `wspr`  | WSPR beacon: type-1 message encoding → 162 channel symbols → continuous-phase 4-FSK audio; no_std RX math (deinterleave, capped Fano decoder, unpack); with `std` also the buffered `WsprDecoder` receive engine | — (standalone; RX engine needs `std`) | TX + decode math | no |
| `ft8`   | FT8: documented message subset (standard `i3=1` + free text) → CRC-14 → LDPC(174,91) → 79 Gray/Costas channel symbols → GFSK-shaped continuous-phase 8-FSK audio; no_std RX math (Gray-demap LLRs, hard-capped LDPC min-sum decoder, CRC verify, unpack); with `std` also the buffered `Ft8Decoder` receive engine | — (standalone; RX engine needs `std`) | TX + decode math | no      |
| `m17`   | M17 **packet mode**: base-40 callsign addressing, Link Setup Frame + packet frames (CRC-16 0x5935), K=5 r=1/2 convolutional FEC with P1/P3 puncturing, QPP interleaver, decorrelator, Golay(24,12), and a 4-level RRC-shaped 4800 sym/s baseband modem (TX + RX). Voice (Codec2) is out of scope | — (standalone) | yes | no |
| `tnc`   | High-level TNC pipeline: `AprsPacket` ⇄ PCM samples in one type each way | `aprs`, `mod`, `demod` | yes  | no      |
| `ptt`   | Serial PTT for `warble ptt` — assert RTS or DTR to key a transmitter. The one feature that can put a signal on the air by itself, so its failure mode is deassert | `std` | no | no |
| `cli`   | The `warble` command-line binary (encode/decode WAV files)              | `wav`, `tnc`, `micE`, `kiss`, `fx25`, `il2p`, `wspr`, `ft8`, `m17`, `ptt` | no | no    |
| `capture` | Sound-card input via `cpal` for `examples/live_capture.rs` only — never a library dependency | `std` | no | no |
| `async` | Tokio adapters (`asynk`): frame `Stream`s, one-call KISS server, concurrent many-feeds decoder | `std`, `tnc`, `kiss` | no | no |
| `embassy` | Embassy adapters (`embassy`): an async chunk-drain decode loop over `SampleRing` + `TncReceiver`, and a periodic-TX ticker. Pulls only `embassy-time` | `tnc` | yes | no |

Everything except `std`, `wav`, `cli`, `capture` and `async` is `no_std`
and allocation-free like the core, and no protocol feature is in the
default set. `embassy` is `no_std` as well; it sits outside the
cross-build matrix below only because `embassy-time` needs a platform
time driver at link time.

`scripts/check-embedded.sh` cross-builds every no_std feature against
`riscv32imac-unknown-none-elf` and `thumbv7em-none-eabihf` with
`--no-default-features`. That covers `micE`, `kiss`, `g3ruh`, `fx25`,
`il2p`, `wspr`, `ft8`, `m17`, `tnc` and `digipeat` individually, the
combined
`mod,demod,nrzi,ax25,aprs,micE,kiss,tnc,g3ruh,fx25,il2p,wspr,ft8,m17,digipeat`
set, and the detached `examples/esp32-riscv` sub-crate for both
`riscv32imac` and `riscv32imc`.

## Terms

The protocol sections below assume the amateur packet-radio
vocabulary. If you came for the modem and the embedded work rather than
for the radio side, this is the whole of it.

| Term | Meaning |
|---|---|
| **AX.25** | The amateur packet-radio link layer: addressed, CRC-checked frames sent over a shared channel. |
| **UI frame** | Unnumbered Information, the connectionless AX.25 frame that APRS uses. No handshake, no acknowledgement, no retries at this layer. |
| **APRS** | Automatic Packet Reporting System, the application layer riding on those frames: positions, weather, telemetry, short messages. |
| **TNC** | Terminal Node Controller. Historically a hardware box between radio and computer that turns audio into frames and back; here it is the `tnc` feature. |
| **Callsign, SSID** | A station is identified by callsign, such as `N0CALL`. The `-7` in `N0CALL-7` is an SSID, a 0..=15 suffix separating one operator's stations; `-7` conventionally means a handheld. |
| **tocall** | The AX.25 destination field. APRS does not route with it, so it carries a device or software identifier instead. |
| **Digipeater, WIDEn-N** | A station that re-transmits what it hears, extending range. A path of `WIDE1-1` asks for one hop; the trailing digit counts down as each digipeater relays. |
| **Mic-E** | A compressed position format that splits one report across *both* AX.25 address fields. |

## Protocol stack

The optional protocol features layer a complete APRS transmit/receive
stack on top of the AFSK physical layer. Position reports come in every
form: plain uncompressed lat/lon, base-91 **compressed** positions with
a typed compression-type (T) byte and every cs-field variant
(`CompressedCs`: no data, course/speed, pre-calculated radio range, and
altitude-on-GGA), and **timestamped** positions (`/` and `@` data type
identifiers, DHM zulu/local and HMS timestamps) wrapping either an
uncompressed or a compressed body. Uncompressed reports also carry the
7-byte **data extension** (`DataExtension`: course/speed, wind, `PHG`,
`PHGR`, `RNG`, `DFS`) and any `/A=` altitude in the comment.

A note on spec provenance. The only formally approved edition is 1.0.1
(2000), and its publisher now distributes that edition as a one-page
notice declaring it obsolete. Where the two differ, this crate follows
the **UNOFFICIAL APRS Protocol Reference Draft 1.2 c**. See
[docs/APRS_CONFORMANCE.md](docs/APRS_CONFORMANCE.md) §1.

```text
APRS payload            position / status / message / weather / telemetry
                        / object / item / Mic-E information field
  -> AX.25 UI frame     addresses, control 0x03, PID 0xF0,
                        CRC-16/X.25 FCS appended little-endian,
                        HDLC 0x7E flags + zero-bit stuffing, LSB-first
  -> NRZI               line coding: a 0 toggles the tone, a 1 holds it
  -> Bell 202 AFSK      1200/2200 Hz continuous-phase samples
```

Each layer is independently usable. With `aprs` plus the `mod` / `demod`
DSP features, glue helpers wire the whole stack together:
`warble::aprs::build_ui_frame` plus `warble::ax25::tx_i16` on the way
down, and `warble::ax25::FrameReceiver` plus
`warble::aprs::packet_from_ui` on the way up. All of it stays `no_std`
and allocation-free: builders serialize into caller-provided buffers,
parsers borrow from the input, and the transmit path is a lazy iterator
chain. The `tnc` feature packages both directions into two types,
`TncTransmitter` and `TncReceiver`; the examples below use those.

## Transmit: APRS packet → PCM samples

Build an APRS position report, wrap it in an AX.25 UI frame, and
generate `i16` PCM samples (requires the `tnc` feature):

```rust
# #[cfg(feature = "tnc")]
# fn main() -> Result<(), Box<dyn std::error::Error>> {
use warble::SampleRate;
use warble::aprs::{AprsPacket, Latitude, Longitude, Position, Symbol};
use warble::ax25::Address;
use warble::tnc::{TncConfig, TncTransmitter};

// 49° 03.50' N, 072° 01.75' W, shown as a car on the map.
let packet = AprsPacket::Position(
    Position::new(
        Latitude::from_degrees(49.0583)?,
        Longitude::from_degrees(-72.0292)?,
        Symbol::CAR,
    )
    .with_comment(b"warble"),
);

let tx = TncTransmitter::new(TncConfig::bell_202(SampleRate::new(48_000)?)?);
let mut info_buf = [0u8; 64];
let mut frame_buf = [0u8; 330];
let samples = tx.transmit_i16(
    &packet,
    Address::new(b"APRS", 0)?,   // destination "tocall"
    Address::new(b"N0CALL", 7)?, // source callsign-SSID
    &[Address::new(b"WIDE1", 1)?],
    &mut info_buf,
    &mut frame_buf,
)?;
assert!(samples.count() > 0); // lazy iterator: write each i16 to a DAC
# Ok(())
# }
# #[cfg(not(feature = "tnc"))]
# fn main() {}
```

`Symbol` carries the two-byte APRS symbol (table + code). It provides
named constants (`Symbol::CAR`, `Symbol::WEATHER_STATION`, …), typed
construction (`Symbol::new`, with overlays via `OverlayId`) and a
`describe()` lookup, and `Position::new(lat, lon, symbol)` builds a
report that is valid by construction. Exact wire values, including
out-of-spec bytes seen on air, go through
`Symbol::from_wire(table, code)`, which is infallible and round-trips
any pair. Coordinates are an `i64` count of `geo::UNITS_PER_DEGREE`
(1/342 833 400 000 000 of a degree), the unit chosen so that every APRS
position format's denominator divides it exactly; `Latitude::new` takes
that count and `Latitude::units()` reads it back, while
`from_degrees_minutes` and `from_degrees` build on the wire grid and
from a decimal:

```rust
# fn main() -> Result<(), Box<dyn std::error::Error>> {
# #[cfg(feature = "aprs")] {
use warble::aprs::{Latitude, Longitude, Position, Symbol};

// Out-of-spec symbol bytes seen on air still round-trip exactly.
let odd = Position::new(Latitude::new(0)?, Longitude::new(0)?, Symbol::from_wire(0x01, 0xFF));
assert_eq!(odd.symbol.to_wire(), (0x01, 0xFF)); // held verbatim, never rejected
let car = Position::new(Latitude::new(0)?, Longitude::new(0)?, Symbol::CAR);
let mut buf = [0u8; 32];
let len = car.build(&mut buf)?;
assert_eq!(&buf[..len], b"!0000.00N/00000.00E>");
# }
# Ok(())
# }
```

Without the `tnc` glue, the same stack is available layer by layer:
`warble::aprs::build_ui_frame` plus `warble::ax25::tx_i16` compose the
identical transmit chain (requires `aprs` and `mod`):

```rust
# #[cfg(all(feature = "aprs", feature = "mod"))]
# fn main() -> Result<(), Box<dyn std::error::Error>> {
use warble::aprs::{AprsPacket, Latitude, Longitude, Position, Symbol, build_ui_frame};
use warble::ax25::{Address, tx_i16};
use warble::{Modulator, ModulatorConfig, SampleRate};

// 49° 03.50' N, 072° 01.75' W, shown as a car on the map.
let packet = AprsPacket::Position(
    Position::new(
        Latitude::from_degrees(49.0583)?,
        Longitude::from_degrees(-72.0292)?,
        Symbol::CAR,
    )
    .with_comment(b"warble"),
);

let mut info_buf = [0u8; 64];
let mut frame_buf = [0u8; 330];
let len = build_ui_frame(
    &packet,
    Address::new(b"APRS", 0)?,   // destination "tocall"
    Address::new(b"N0CALL", 7)?, // source callsign-SSID
    &[Address::new(b"WIDE1", 1)?],
    &mut info_buf,
    &mut frame_buf,
)?;

let modulator = Modulator::new(ModulatorConfig::bell_202(SampleRate::new(48_000)?)?);
let samples: Vec<i16> = tx_i16(&frame_buf[..len], modulator).collect();
assert!(!samples.is_empty());
# Ok(())
# }
# #[cfg(not(all(feature = "aprs", feature = "mod")))]
# fn main() {}
```

## Receive: PCM samples → APRS packet

Push PCM samples through a `TncReceiver` (demodulator, NRZI decoder,
HDLC deframer, FCS check, UI-frame parse) and decode the APRS packet
(requires `tnc`; the sample source below reuses the transmit path):

```rust
# #[cfg(feature = "tnc")]
# fn main() -> Result<(), Box<dyn std::error::Error>> {
use warble::SampleRate;
use warble::aprs::{AprsPacket, Status};
use warble::ax25::Address;
use warble::tnc::{DefaultTncReceiver, TncConfig, TncReceiver, TncTransmitter};

let cfg = TncConfig::bell_202(SampleRate::new(48_000)?)?;
let tx = TncTransmitter::new(cfg);

// A minimal on-air signal: a status-report UI frame, modulated.
let packet = AprsPacket::Status(Status {
    text: b"warble on the air",
});
let mut info_buf = [0u8; 64];
let mut frame_buf = [0u8; 330];
let samples = tx.transmit_i16(
    &packet,
    Address::new(b"APRS", 0)?,
    Address::new(b"N0CALL", 7)?,
    &[],
    &mut info_buf,
    &mut frame_buf,
)?;

// Receive: every pushed sample may complete an FCS-valid frame.
let mut rx: DefaultTncReceiver = TncReceiver::new(cfg)?;
let mut packets = 0;
for sample in samples {
    if let Some(frame) = rx.push_i16(sample) {
        assert_eq!(frame.src().callsign.as_bytes(), b"N0CALL");
        match frame.aprs()? {
            AprsPacket::Status(s) => assert_eq!(s.text, b"warble on the air"),
            _ => panic!("expected a status report"),
        }
        packets += 1;
    }
}
assert_eq!(packets, 1);
# Ok(())
# }
# #[cfg(not(feature = "tnc"))]
# fn main() {}
```

## Mic-E

The `micE` feature builds and decodes compressed Mic-E position
reports. Mic-E splits a report across *both* AX.25 address fields, with
the destination callsign carrying the latitude digits and the flag
bits. Frames are therefore built from the encoded destination via
`build_frame_raw` and decoded with `RxFrame::mic_e`:

```rust
# #[cfg(all(feature = "tnc", feature = "micE"))]
# fn main() -> Result<(), Box<dyn std::error::Error>> {
use warble::SampleRate;
use warble::aprs::{
    Latitude, LatitudeHemisphere, Longitude, LongitudeHemisphere, MicE, MicEFix, MicEMessage,
    Symbol,
};
use warble::ax25::Address;
use warble::tnc::{DefaultTncReceiver, TncConfig, TncReceiver, TncTransmitter};

let report = MicE::new(
    // Mic-E carries hundredths of an arc-minute, so the position is
    // built on that grid and the round trip below is exact.
    Latitude::from_degrees_minutes(33, 2564, LatitudeHemisphere::North)?, // 33° 25.64' N
    Longitude::from_degrees_minutes(112, 700, LongitudeHemisphere::West)?, // 112° 07.00' W
    20,  // knots
    251, // degrees
    Symbol::from_wire(b'/', b'j'), // jeep
    MicEMessage::InService,
)?
.with_fix(MicEFix::Current)
.with_altitude(Some(61)) // meters
.with_status(b"hello");
let mut dest_text = [0u8; 6];
let mut info = [0u8; 64];
let info_len = report.encode(&mut dest_text, &mut info)?;

let cfg = TncConfig::bell_202(SampleRate::new(48_000)?)?;
let tx = TncTransmitter::new(cfg);
let mut frame_buf = [0u8; 330];
let len = tx.build_frame_raw(
    Address::new(&dest_text, 0)?,
    Address::new(b"N0CALL", 9)?,
    &[],
    &info[..info_len],
    &mut frame_buf,
)?;

let mut rx: DefaultTncReceiver = TncReceiver::new(cfg)?;
for sample in tx.frame_samples_i16(&frame_buf[..len]) {
    if let Some(frame) = rx.push_i16(sample) {
        assert_eq!(frame.mic_e()?, report);
    }
}
# Ok(())
# }
# #[cfg(not(all(feature = "tnc", feature = "micE")))]
# fn main() {}
```

## KISS framing

The standalone `kiss` feature implements the KISS TNC serial protocol:
a zero-allocation frame encoder (buffer-based or a lazy byte iterator)
and a streaming deframer with typed errors:

```rust
# #[cfg(feature = "kiss")]
# fn main() -> Result<(), Box<dyn std::error::Error>> {
use warble::kiss::{KissCommand, KissDeframer, KissPort, encode_into};

// Encode an AX.25 frame body as a KISS data frame on port 0.
let payload = [0x82, 0xC0, 0x7E]; // 0xC0 (FEND) gets escaped
let mut wire = [0u8; 16];
let len = encode_into(KissPort::new(0)?, KissCommand::Data, &payload, &mut wire)?;

// Decode: push received bytes one at a time.
let mut deframer: KissDeframer<330> = KissDeframer::new();
let mut frames = 0;
for &byte in &wire[..len] {
    if let Some(frame) = deframer.push(byte) {
        let frame = frame?;
        assert_eq!(frame.command(), KissCommand::Data);
        assert_eq!(frame.payload(), payload);
        frames += 1;
    }
}
assert_eq!(frames, 1);
# Ok(())
# }
# #[cfg(not(feature = "kiss"))]
# fn main() {}
```

## Command-line tool

The `cli` feature builds the `warble` binary, which encodes APRS
packets into 16-bit mono PCM WAV files and decodes them back:

```sh
# Encode a position report to a WAV file.
cargo run --features cli -- encode --out beacon.wav \
    --from N0CALL-7 --to APRS --path WIDE1-1 --sample-rate 48000 \
    position --lat 49.0583 --lon -72.0292 --symbol '/>' --comment 'warble'

# Encode a directed message.
cargo run --features cli -- encode --out msg.wav \
    --from N0CALL-7 --to APRS \
    message --to-call N1CALL --text 'hello' --id 42

# Decode a WAV: one line per AX.25/APRS frame on stdout,
# receive statistics on stderr.
cargo run --features cli -- decode beacon.wav
```

The parser is clap-based: every subcommand has a detailed
`--help` (e.g. `warble encode --help`) listing value ranges and
defaults. Optional modem knobs on both `encode` and `decode`:

- `--preset <bell202|hf300|bell103|bell103-answer|g3ruh>` selects the
  base baud-rate and mark/space tone profile (default `bell202`). The
  `g3ruh` value (alias `g3ruh-9600`) selects 9600-baud G3RUH scrambled
  baseband, which carries no audio tones, so `--baud`, `--mark` and
  `--space` are rejected alongside it. Build with
  `--features cli,g3ruh`.
- `--baud <BPS>`, `--mark <HZ>` and `--space <HZ>` override individual
  fields. The preset supplies the base profile and each override
  replaces one field of it.
- `--fx25` turns on FX.25 forward error correction (tone-AFSK presets
  only). On `encode` each frame is wrapped in a correlation tag and a
  Reed-Solomon codeblock before modulation, and legacy receivers still
  decode the embedded AX.25 frame. On `decode` the FX.25-aware receive
  path corrects codeblock errors and continues to decode plain AX.25
  frames.
- `--il2p` selects IL2P framing (tone-AFSK presets only; conflicts
  with `--fx25`). On `gen` each frame is emitted as an IL2P
  transmission (sync word, translated header, Reed-Solomon-protected
  payload blocks) in place of HDLC, and on `decode` the IL2P sync-word
  receive path is used. Unlike FX.25, IL2P is **not** AX.25-compatible
  on the air, so both ends must speak it; see the [IL2P](#il2p)
  section.
- `encode` also takes `--path` (digipeater list) and `--sample-rate`
  (alias `--rate`; default 44100 Hz, range 8000..=48000).
- `decode` also takes `--output-format <text|jsonl>` (default `text`);
  see [JSON Lines output](#json-lines-output-decode---output-format-jsonl)
  below.

Usage errors (unknown flag, missing value) exit with code 2; bad
values (out-of-range coordinate, unreadable WAV) exit with code 1.

### JSON Lines output (`decode --output-format jsonl`)

`warble decode` prints a human monitor line by default. Pass
`--output-format jsonl` and it prints **JSON Lines** (NDJSON) instead:
one self-contained JSON object per decoded frame, one per line, no
enclosing array, so the stream pipes straight into `jq`, a log shipper
or a database `COPY`. The feature adds no dependency: the writer lives
in the binary (`src/bin/warble/json.rs`) and the library core remains
zero-dependency and `no_std`.

```sh
# One JSON object per frame on stdout; statistics still on stderr.
warble decode --output-format jsonl traffic.wav

# Every station that reported a position, with its coordinates.
warble decode --output-format jsonl traffic.wav \
  | jq -r 'select(.kind=="position" or .kind=="mic_e")
           | [.src, .[.kind].lat_deg, .[.kind].lon_deg] | @tsv'

# Which data types are on the channel, most common first.
warble decode --output-format jsonl traffic.wav \
  | jq -r .kind | sort | uniq -c | sort -rn

# Frames this crate could not parse, with the reason.
warble decode --output-format jsonl traffic.wav \
  | jq -r 'select(.error) | [.src, .error, .info] | @tsv'

# Rebuild the TNC2 monitor path from the structured one.
warble decode --output-format jsonl traffic.wav \
  | jq -r '[.src, ">", .dst] + [.path[] | "," + .call + (if .repeated then "*" else "" end)]
           | add'
```

Three real lines from an off-air recording (wrapped here; each is one
line in reality):

```json
{"v":2,"sample":94812,"t":1.975250,"src":"WA8LMF","dst":"STPYXT","path":[{"call":"WIDE2-2","repeated":false}],"kind":"mic_e","mic_e":{"lat_deg":34.164000,"lon_deg":-118.117000,"speed_kt":0,"course_deg":67,"symbol":"/>","message":"off_duty","fix":"old","altitude_m":310,"device_prefix":"]","ambiguity_digits":0,"status":"\r"},"info":"'._\u001el _>/]\"7<}\r"}
{"v":2,"sample":1229456,"t":27.878821,"src":"N6EX-3","dst":"APJI23","path":[{"call":"N6EX-4","repeated":false},{"call":"SOCAL1-1","repeated":false}],"kind":"third_party","third_party":{"src":"W6AHM","dst":"APRS","path":"TCPIP,N6EX-3*","payload":{"kind":"weather","weather":{"lat_deg":33.838000,"lon_deg":-118.314167,"symbol":"/_","messaging":true,"timestamp":{"form":"dhm_zulu","day":23,"hour":1,"minute":35},"wind_dir_deg":269,"wind_speed_mph":12,"gust_mph":10,"temperature_f":65,"rain_1h_hundredths_inch":0,"rain_24h_hundredths_inch":0,"rain_midnight_hundredths_inch":0,"humidity_pct":64,"pressure_tenths_hpa":10155,"rest":"v6"},"info":"@230135z3350.28N/11818.85W_269/010g010t065r000P000p000h64b10155v6"}},"info":"}W6AHM>APRS,TCPIP,N6EX-3*:@230135z3350.28N/11818.85W_269/010g010t065r000P000p000h64b10155v6"}
{"v":2,"sample":49762572,"t":1128.402993,"src":"AC6VV-9","dst":"S4PXYX","path":[{"call":"WIDE1-1","repeated":false}],"kind":"malformed","malformed":{"dti":96,"dti_char":"`"},"error":"Mic-E report: longitude byte 0xBE at offset 1 decodes outside its legal range","info":"`�_\u007fl#5>/]\"6n}\r","info_hex":"60be5f7f6c23353e2f5d22366e7d0d"}
```

#### Five rules the schema follows

**1. Every frame produces one line, including frames that do not
parse.** The last example above is an FCS-valid frame carrying `0xBE`
where a Mic-E longitude byte belongs. Rather than drop it or coerce it
silently, the decoder emits `"kind":"malformed"` with the parser's own
message in `"error"` and the bytes intact. That carries the `Decoded`
contract, which labels what it cannot parse instead of discarding it,
into the output format. A frame that yields no typed payload still
carries its addresses and its `info`.

**2. `line[line.kind]` is always an object.** The `"kind"` discriminant
is one of `position`, `mic_e`, `message`, `weather`, `status`,
`object`, `item`, `telemetry`, `capabilities`, `nmea`, `ultimeter`,
`third_party`, `unsupported`, `needs_destination` or `malformed`, and
the key of the same name holds that kind's typed fields. `jq '.[.kind]'`
therefore reaches the payload without a `case` statement, and two kinds
cannot collide over a field name that means something different in
each.

**3. `info` is lossy, and `info_hex` says so.** APRS information fields
are arbitrary bytes, while JSON strings are UTF-8. `"info"` is always
present as a UTF-8-lossy string (invalid sequences become U+FFFD),
which keeps it readable, greppable and `jq`-able. `"info_hex"` appears
only when the field is not valid UTF-8, carrying the exact bytes as
lowercase hex. Its presence is the machine-readable signal that
`"info"` lost something, which makes the line byte-lossless without
doubling the size of every other line. (MEASURED: 17 of 2182 off-air
frames need it, so 99.2% pay nothing.) The same `_hex` sibling rule
covers every byte-slice field: `comment`/`comment_hex`,
`status`/`status_hex`, `text`/`text_hex`, `name`/`name_hex` and
`symbol`/`symbol_hex`.

> A `\u00XX` Latin-1 escaping convention was considered and rejected.
> It looks lossless, but `\u00BE` is U+00BE, so any consumer that
> re-encodes the string as UTF-8 gets two bytes where the air carried
> one. A `_hex` field beside a lossy string describes the situation
> accurately; a Latin-1 escape misdescribes it.

**4. No wall clock by default.** A frame is identified by where it
landed in the input stream rather than by the time of day. `"sample"`
is the sample index at which the frame completed, and `"t"` is the same
position in seconds. Both are functions of the input alone, so decoding
a recording twice produces byte-identical output, and the output can be
pinned in a test (`tests/cli.rs::decode_jsonl_exact_output_pin`). A
live capture is the case where the time of reception is itself
information; there, `--wall-clock` adds a `"unix_time"` field in
seconds since the Unix epoch. That flag is opt-in and will remain so.

**5. Every quantity key names its unit.** `altitude_ft`, `speed_kt`,
`course_deg`, `temperature_f`, `pressure_tenths_hpa`,
`rain_1h_hundredths_inch`. There is no bare `altitude`, and there will
not be one: the crate's `units` module exists because a single integer
cannot mean two things, and a log line is where that ambiguity does the
most damage. Each key uses the unit of the wire field it came from, so
the value is exact rather than converted and rounded. Downstream
consumers can convert from a number whose unit is stated.

#### Schema, version 1

The envelope, on **every** line, in this order:

| key | type | meaning |
|---|---|---|
| `v` | number | Schema version, currently `1`. Bumped only for a breaking change; adding a key is not breaking. |
| `sample` | number | Index of the input sample at which the frame completed. |
| `t` | number | `sample / sample_rate`, in **seconds**, to 6 decimal places. |
| `unix_time` | number | Seconds since the Unix epoch. **Only with `--wall-clock`.** |
| `src` | string | Source address, `CALL` or `CALL-SSID`. |
| `dst` | string | Destination address (the APRS tocall). |
| `path` | array | Digipeater path: `[{"call":"WIDE1-1","repeated":true}, …]`. |
| `kind` | string | The discriminant; see rule 2. |
| *`kind`* | object | The typed fields of that kind (the tables below). |
| `error` | string | Why the parse failed. **Only when `kind` is `malformed`.** |
| `info` | string | The information field, UTF-8 lossy. Always present. |
| `info_hex` | string | The information field, exact, lowercase hex. **Only when not valid UTF-8.** |

`path` is structured rather than written in the TNC2 monitor form
`WIDE1-1*`. Both were considered; the structured form won because a
`*` suffix makes one string carry two pieces of information, the same
ambiguity `units` exists to prevent. The `jq` recipe above
reconstructs the monitor form in one line.

The typed objects, by kind:

| kind | keys |
|---|---|
| `position` | `lat_deg`, `lon_deg`, `symbol`, `messaging`, `compressed`, `comment`; optional `timestamp`, `altitude_ft` (from a `/A=` in the comment), `extension`, `cs` |
| `mic_e` | `lat_deg`, `lon_deg`, `speed_kt`, `course_deg`, `symbol`, `message` (`off_duty`/`en_route`/`in_service`/`returning`/`committed`/`special`/`priority`/`emergency`/`custom0`…`custom6`), `fix` (`current`/`old`), `ambiguity_digits`, `status`; optional `altitude_m`, `device_prefix` |
| `message` | `to`, `type` (`text`/`ack`/`rej`), `text` (for `text`), optional `id` |
| `weather` | either `lat_deg`+`lon_deg`+`symbol`+`messaging`+optional `timestamp` (Complete Weather Report), or `month`+`day`+`hour`+`minute` (positionless); then the measurement keys below; then `rest` |
| `status` | `text`, `message` (the text with any timestamp/beam stripped); optional `timestamp`, `grid`, `beam_heading_deg`, `beam_erp_w` |
| `object` | `name`, `live`, `timestamp`, `lat_deg`, `lon_deg`, `symbol`, `comment` |
| `item` | `name`, `live`, `lat_deg`, `lon_deg`, `symbol`, `comment` |
| `telemetry` | `seq`, `analog` (5 numbers), `digital` (8 booleans), `rest` |
| `capabilities` | `body` |
| `nmea` | `talker`, `formatter`, `checksum` (`valid`/`invalid`/`absent`); optional `fix` (`valid`/`degraded`/`invalid`), `lat_deg`, `lon_deg`, `course_deg`, `speed_kt`, `altitude_m` |
| `ultimeter` | `format` (`packet`/`data_logger`/`ultimeter_two`), optional `wire_wind_unit`; then the measurement keys below |
| `third_party` | `src`, `dst`, `path` (all *text* off the wire, not validated addresses — a gateway writes `qAC`, `TCPIP*`), and `payload`: the encapsulated frame decoded **one level deep**, with the same `kind` / *`kind`* / `info` / `info_hex` shape |
| `unsupported` | `dti`, `dti_char` — a data type identifier this crate does not implement, or a non-APRS beacon |
| `needs_destination` | `dti` — a Mic-E information field decoded without its frame (cannot occur at the top level, only inside a `third_party` payload) |
| `malformed` | `dti`, `dti_char`; the reason is the top-level `error` |

Measurement keys, shared by `weather` and `ultimeter`, all optional:
`wind_dir_deg`, `wind_speed_mph`, `gust_mph`, `temperature_f`,
`rain_1h_hundredths_inch`, `rain_24h_hundredths_inch`,
`rain_midnight_hundredths_inch`, `humidity_pct`,
`pressure_tenths_hpa`, `luminosity_wm2`, `snowfall_hundredths_inch`.

Sub-objects: `timestamp` is `{"form":"dhm_zulu"|"dhm_local","day",
"hour","minute"}` or `{"form":"hms","hour","minute","second"}`;
`extension` is `{"type":"course_speed"|"wind"|"phg"|"range"|"dfs", …}`
with unit-named keys (`course_deg`, `speed_kt`, `wind_dir_deg`,
`wind_speed_kt`, `power_w`, `height_ft`, `gain_dbi`, `gain_db`,
`directivity_deg`, `rate_per_hour`, `strength_s_points`, `range_mi`);
`cs` (the compressed-position trailer) is
`{"type":"course_speed"|"radio_range"|"altitude", …}` and is omitted
entirely when it carries no data.

String escaping: `"` and `\`; the short forms `\b \t \n \f \r`; every
other C0 control and DEL (0x7f) as `\u00xx`. Everything else, including
non-ASCII, is emitted verbatim as UTF-8.

`warble serve` has no `--output-format`. In `--stdio` mode its stdout
already carries the binary KISS frame stream, and in TCP mode frames go
to the sockets, so interleaving NDJSON would corrupt the channel. Use
`decode` for a readable stream and `serve` for a TNC.

### Test signals and decode accuracy (`gen` and `bench`)

Two subcommands close the loop without a radio: `gen` synthesizes a
multi-frame recording with controlled, reproducible impairments, and
`bench` measures how much of it (or of your own recordings) the
decoder recovers, with thresholds fit for CI.

```sh
# Ten sequence-numbered status frames, clean, to a WAV file.
cargo run --features cli -- gen --out clean.wav --count 10

# The same, but harsher: 6 dB SNR seeded noise at 30% amplitude.
cargo run --features cli -- gen --out noisy.wav --count 10 \
    --snr 6 --level 0.3 --seed 42

# Score the decoder on both; fail (exit 1) below 90% recovery.
cargo run --features cli -- bench clean.wav noisy.wav --min 90%

# Or stream raw PCM straight into the decoder — no files at all.
cargo run --features cli -- gen --out - --count 5 --sample-rate 48000 --snr 10 | \
    cargo run --features cli -- decode --sample-rate 48000 -
```

`gen` writes `--count` APRS status frames (`--from`/`--to`/`--text`
override the placeholder content) separated by `--gap-ms` of silence,
at `--level` amplitude (fraction of full scale, default 0.5), through
the same `--preset`/`--baud`/`--mark`/`--space`/`--fx25`/`--il2p`
modem knobs as `encode` (`--il2p` on `gen` only). `--snr <DB>` mixes in additive white noise at that
signal-to-noise ratio in dB (measured against the generated signal's
RMS; ~20 dB is mild, 0 dB means noise as strong as the signal). The
noise comes from a small in-crate seeded PRNG: the same flags and
`--seed` always produce byte-identical output, so a generated corpus
is a stable regression fixture. Each frame's text ends in an `[i/N]`
counter, which is how `bench` later knows what a recording *should*
contain.

`bench` decodes each WAV (files, or directories of `.wav` files) with
the shared modem flags and prints a per-file and aggregate table of
decoded vs expected frames. The expectation comes from `--expect N`,
or is recovered from `gen`'s embedded `[i/N]` counters. `--min`
sets the aggregate pass threshold as an absolute count (`--min 18`)
or a percentage of the expected total (`--min 95%`); below it the
command exits with code 1, so a `gen` fixture plus `bench --min` is a
one-line decode-accuracy gate in CI. `--json` swaps the table for a
single machine-readable JSON object
(`{"files":[{"path":…,"decoded":…,"expected":…}],"decoded":…,"expected":…,"min":…,"pass":…}`).

## APRS from the internet, and APRS without a radio

APRS also travels as text, and the same decoders read it. A line of
TNC2 monitor format is what APRS-IS streams, what most TNCs print, and
what sits inside a third-party frame:

```text
N0CALL-7>APRS,WIDE1-1,qAR,IGATE-1:!4903.50N/07201.75W-hi
└─src──┘ └dst┘ └───── path ─────┘ └────── information ──┘
```

`warble::aprs::monitor::MonitorLine` parses that line, and `decoded()`
hands the information field to the same total decoder the audio path
uses. Addresses stay as text, because APRS-IS is not bound by AX.25
rules and forcing them through `Address` would reject the traffic worth
reading. The parser is `no_std` and allocation-free, like the rest of
the APRS layer.

```rust
# #[cfg(feature = "aprs")] {
use warble::aprs::monitor::MonitorLine;

let line = MonitorLine::parse(b"N0CALL-7>APRS,WIDE1-1,qAR,IGATE-1:>hello")?;
assert_eq!(line.source, b"N0CALL-7");
assert_eq!(line.info, b">hello");
assert_eq!(line.q_construct(), Some(&b"qAR"[..]));  // how it entered APRS-IS
assert_eq!(line.igate(), Some(&b"IGATE-1"[..]));    // which station gated it
assert!(line.is_from_rf());                          // qAR means heard on a radio
# }
# Ok::<(), warble::aprs::AprsError>(())
```

Two examples build on this.
[`examples/aprs_is.rs`](examples/aprs_is.rs) connects to the APRS-IS
network and reports live statistics; it logs in receive-only and cannot
transmit. [`examples/aprs_offline.rs`](examples/aprs_offline.rs) builds
and decodes packets with no radio, sound card or network involved.

### Reading the live feed (`aprsis`)

`warble aprsis` is the same connection as a subcommand, writing raw
TNC2 lines rather than statistics, so it composes with `decode --tnc2`:

```sh
# A slice of the traffic: 250 km around Kansas City, 12 packets.
warble aprsis --callsign N0CALL --filter 'r/39.1/-94.6/250' --count 12

# The unfiltered feed for five minutes, into a capture file.
warble aprsis --callsign N0CALL --full-feed --seconds 300 --out capture.txt
warble decode --tnc2 --verify-rebuild capture.txt

# Or as one pipeline, with no file in between.
warble aprsis --callsign N0CALL --full-feed --count 200 | \
    warble decode --tnc2 --output-format jsonl -
```

The login passcode is the constant `-1`, which every server treats as
unverified: such a client may receive and may not send. There is no
flag to change it and no code path that writes to the socket except the
login line. Injecting into APRS-IS must be assumed to reach the air, so
it requires a licensed callsign and a real passcode, and a capture tool
has no business holding either. The callsign is required rather than
defaulted, because servers refuse the placeholder `N0CALL` and a shared
volunteer network is not somewhere to connect anonymously.

The two feeds behave differently and the subcommand refuses the
combinations that would connect and then deliver nothing:

| | port | sends |
|---|---|---|
| `--filter <SPEC>` | 14580 | nothing at all until a filter subscribes you |
| `--full-feed` | 10152 | everything, and filters are ignored |

Prefer a filter, keep one connection rather than several (parallel
connections create duplicate loops that make stations jump around on
other people's maps), and bound the run with `--seconds` or `--count`.
Reconnects back off exponentially with a fresh DNS lookup, because the
rotate addresses load-balance across many volunteers' servers.

### Setting the receive level (`level`)

A radio's volume knob is the only receive-level control most interfaces
have, and it gives no feedback. `warble level` reads the same stdin PCM
every other subcommand takes and reports what the modem will see:

```sh
ffmpeg -f avfoundation -i ":2" -ar 44100 -f s16le - \
  | warble level --rate 44100 --until-good 3 -
```

```text
rms -19.7 dBFS  peak  31%  clip 0  [.....|=========.....]  GOOD  squelch OPEN  1200/2200 --
```

`--until-good <SECS>` exits once the level has held in range, `--for
<SECS>` after a fixed time, and `--then-decode` keeps metering while
decoding the same stream so frames print underneath. One is required,
so it can never hang; both bounds are counted in **audio** time, which
means a file or a fast pipe behaves the same as a live capture.

Two things it reports that a single number hides:

* **clipped samples**, separately from peak. RMS cannot see clipping and
  peak saturates at 100% whether one sample is pinned or ten thousand.
  A real capture once read -0.8 dBFS, looked no worse than loud, and was 23%
  clipped with nothing in it decodable.
* **squelch state**. Packet wants the squelch OPEN: it takes tens of
  milliseconds to lift, which eats a frame's opening flags and turns a
  decodable packet into an FCS error.

### Keying a transmitter (`ptt`)

Everything above writes audio to a file or a pipe, because this crate
does protocol and DSP and leaves audio to your operating system. Push
to talk is the one thing that cannot follow that pattern on its own: it
has to be asserted before the first sample reaches the air and released
after the last one, and a process writing PCM into a pipe knows
neither moment, because the player downstream buffers.

So `warble ptt` runs the player and holds the line for exactly its
lifetime:

```sh
# Build a packet, then transmit it: key, play, unkey.
warble encode --out beacon.wav --from N0CALL --to APRS \
  position --lat 43.632334 --lon -70.230565 --comment "beacon"

warble ptt --port /dev/ttyUSB0 -- sox beacon.wav -t alsa default

# Check the interface keys at all, before trusting it with audio.
warble ptt --port /dev/ttyUSB0 --hold 2000

# Which ports can this machine see?
warble ptt --list
```

`--signal dtr` uses DTR instead of RTS, `--invert` keys on the line
being low, and `--lead` / `--tail` (300 ms / 150 ms by default) pad the
key-down so the transmitter settles before data and a buffered tail is
not cut off.

**It fails toward not transmitting.** The line is released on every
exit path, including an error or a panic, and `--max` (60 s by default)
kills a hung player and drops the line rather than let a stuck
transmitter jam a shared channel. One hazard worth knowing about, since
it bites silently: **some USB-serial drivers assert RTS the moment the
port is opened**, which keys a wired-up radio before any program logic
runs. `warble ptt` drops both control lines immediately after opening;
other tools may not.

## Live decode from your sound card

`warble decode -` reads audio from stdin instead of a WAV file, so any
capture tool that can write raw PCM to a pipe becomes a live front
end. Two stdin forms are accepted:

- **Raw PCM**: signed 16-bit little-endian mono (`--format s16le`, the
  default and currently the only encoding; the flag exists so that
  more can be added). Raw PCM has no header, so `--sample-rate` is
  required and must equal the rate the capture tool records at. Input
  is read continuously until EOF, so a live pipe works as it stands.
- **WAV**: a stream starting with a `RIFF` header is decoded as a WAV
  file, taking rate and format from the header with no flags needed,
  as in `warble decode - < beacon.wav`.

Pipe recipes, all capturing s16le mono at 48 kHz from the default
input device (build the binary once with
`cargo build --release --features cli`):

```sh
# ALSA capture (Linux): -t raw -f S16_LE -c 1, rate 48000.
arecord -t raw -f S16_LE -c 1 -r 48000 | warble decode --sample-rate 48000 -

# sox: record from the default device, convert to s16le mono on the fly.
rec -t raw -b 16 -e signed-integer -L -c 1 -r 48000 - | \
    warble decode --sample-rate 48000 -

# ffmpeg: any input it can open (here an ALSA device), downmixed to mono.
ffmpeg -f alsa -i default -f s16le -ac 1 -ar 48000 - | \
    warble decode --sample-rate 48000 -
```

**Sample-rate matching.** The audio on a radio's speaker or line
output is analog and has no inherent sample rate. The rate of the
stream is whatever rate your capture tool digitizes at, and
`--sample-rate` must match that value. If it does not, the decoder's
bit clock runs at the wrong speed and nothing decodes. The modem
accepts 8000..=48000 Hz, and **48000 Hz is the safe default**: every
sound card supports it, and higher rates give the demodulator more
samples per bit. If your device only does 96 kHz or 192 kHz, let the
capture tool resample (`-r 48000` and `-ar 48000` above do that).

**Levels and clipping.** Aim for a healthy but unclipped level. AFSK
survives quiet audio far better than clipped audio, which flattens the
tones into square waves and shifts their spectra. Set the radio's
volume, or the OS input gain, so that peaks stay well below full
scale; around half scale is plenty. Turn off any mic boost, AGC or
noise suppression the OS offers, since all of them mangle modem tones.

**Getting radio audio into the computer**, from simplest to nicest:

- **Line-in or mic jack**: a 3.5 mm cable from the radio's speaker or
  data jack into the computer's line input. This is the cheapest
  option. Use a *line*-level input if you have one, because mic inputs
  expect millivolts and clip easily; keep the radio volume low if
  mic-in is all you have.
- **USB audio dongle**: a $10 USB sound adapter gives any machine an
  isolated input, including a machine with no line-in or a Raspberry
  Pi, and keeps radio hum away from the motherboard's grounds.
- **Soundcard-interface unit**: a purpose-built radio-to-USB interface,
  of the kind sold for digital modes, adds transformer isolation and
  PTT keying. Choose this once you also want to transmit.

Whichever route you take, the modem wants **mono** input. If your
capture path is stereo, downmix it with the `-c 1` or `-ac 1` flags
above rather than sending one silent channel. The interfacing
electronics (attenuation and biasing, isolation, PTT) pose the same
problem on a desktop as on an embedded board, and the **Hardware
guide** in
[examples/esp32-riscv/README.md](examples/esp32-riscv/README.md) works
through those circuits in detail. Its advice applies unchanged to a
desktop sound card.

To skip the pipe entirely, `examples/live_capture.rs` opens the default
input device through `cpal`, behind the non-default `capture` feature:

```sh
cargo run --example live_capture --features tnc,capture
```

It downmixes to mono `i16`, checks the device rate against the modem's
window, and prints one monitor line per decoded frame. Devices running
at 96 or 192 kHz get a simple integer-ratio decimation; any other
mismatch is refused with guidance, since full resampling is out of
scope. The conversion, downmix and feed plumbing are pure functions,
exercised without a device in `tests/cli.rs`.

## KISS TNC server (`serve`)

[KISS](https://en.wikipedia.org/wiki/KISS_(amateur_radio_protocol))
is the small serial protocol that host applications use to talk to a
TNC. Each AX.25 frame is wrapped between `0xC0` delimiter bytes with a
one-byte command header and two escape sequences, and the specification
goes little further than that. The TNC does the modem work and the host
does everything above it. KISS is the common language of APRS clients such as Xastir,
YAAC and APRSdroid, so speaking it makes `warble` a drop-in modem for
any of them. `warble serve` binds the crate's `kiss` framing layer to a
transport and bridges it to audio:

```sh
# TCP: serve KISS on a local port; decode replayed (or piped-in live)
# audio to every connected client, modulate client frames to TX audio.
warble serve --tcp 127.0.0.1:8001 --input rx.wav --output tx.wav

# Live RX audio via a pipe (same recipes as `decode -`), TX as raw
# PCM on stdout into a playback tool. Stdin is sniffed exactly like
# `decode -`: a WAV header sets the rate itself (no --sample-rate
# needed); raw s16le PCM requires it.
arecord -t raw -f S16_LE -c 1 -r 48000 | \
    warble serve --tcp 127.0.0.1:8001 --sample-rate 48000 --input - --output - | \
    aplay -t raw -f S16_LE -c 1 -r 48000

# stdio: one KISS stream on stdin/stdout (the classic direct-attach
# shape — point a host application straight at the process). The
# audio edges must be files in this mode.
warble serve --stdio --input rx.wav --output tx.wav
```

To connect an APRS application, configure it for a "network KISS
TNC" / "KISS over TCP" interface at the address you passed to
`--tcp`. Received frames are broadcast to **every** connected client
(up to 8; later connections are dropped), and any client may submit
KISS data frames for transmit. Non-data KISS commands such as TXDELAY
are accepted and ignored, since there is no radio-keying hardware here
to configure. The modem settings are the shared `--preset`, `--baud`,
`--mark`, `--space` and `--fx25` flags.

Half-duplex expectations: the bridge writes TX audio to its own output
and does not loop it back into the receiver. Muting the RX path during
transmission, and keying PTT, belong to whatever surrounds the audio
pipes. `--output` appends to an existing WAV so that repeated sessions
accumulate, or streams raw s16le PCM when given `-`.

Exit codes: 0 after a clean shutdown, meaning the audio input reached
EOF at the end of the WAV or the capture pipe closed; 1 on an I/O or
setup failure; 2 for usage errors. The bridge is plain `std::net` and
`std::thread` with bounded channels, and uses no async runtime. The
next section covers the async option.

### Using warble from async (tokio)

Enable the `async` feature and the plumbing is done for you:

```toml
[dependencies]
warble = { version = "0.1", features = ["async", "wav"] }
tokio-stream = "0.1"
```

```rust,ignore
use tokio_stream::StreamExt;

let mut frames = std::pin::pin!(warble::asynk::decode_wav("rx.wav"));
while let Some(frame) = frames.next().await {
    println!("{}", String::from_utf8_lossy(frame?.info()));
}
```

Many feeds at once: dozens of PCM streams decoded concurrently, each
frame tagged with the feed it came from:

```rust,ignore
let cfg = TncConfig::bell_202(SampleRate::new(48_000)?)?;
let mut frames = std::pin::pin!(warble::asynk::decode_many(feeds, cfg));
while let Some((feed, frame)) = frames.next().await {
    database.insert(feed, frame?).await?; // slow sink stalls the decoders
}
```

`warble::asynk` also has `frames(reader, cfg)` for a single
`AsyncRead` of raw s16le PCM and `serve_kiss(listener, frames)`, a
one-call KISS-over-TCP broadcast server. Inside each adapter the DSP
runs on `spawn_blocking` rather than on the reactor, and every channel
is bounded, so a slow consumer applies backpressure and no frame is
dropped.

#### Piped audio

Pipes are first-class inputs. Raw s16le PCM on stdin, which is what
most capture tools emit, decodes with `asynk::frames` over
`tokio::io::stdin()`. Raw PCM carries no sample rate, so you pass one:

```rust,ignore
let cfg = TncConfig::bell_202(SampleRate::new(48_000)?)?;
let mut frames = std::pin::pin!(warble::asynk::frames(tokio::io::stdin(), cfg));
while let Some(frame) = frames.next().await {
    println!("{}", String::from_utf8_lossy(frame?.info()));
}
```

When the pipe might carry a WAV instead, because someone `cat`s a
recording into it, `asynk::decode_stream` sniffs the first four bytes
and handles both forms. A WAV takes its rate from the header, and
anything else is treated as raw PCM at the rate you supply. A rate that
contradicts a WAV header raises an error rather than a silent guess:

```rust,ignore
let rate = SampleRate::new(48_000).ok(); // applies only if raw
let mut frames = std::pin::pin!(warble::asynk::decode_stream(
    tokio::io::stdin(),
    rate,
));
while let Some(frame) = frames.next().await {
    println!("{}", String::from_utf8_lossy(frame?.info()));
}
```

And without writing any code, the CLI does the same intake:

```sh
your-capture-tool | warble decode - --sample-rate 48000
```

One crate therefore covers the whole stack, from a bare-metal `no_std`
microcontroller up to a multicore server. The `async` feature is off by
default and no other feature turns it on, so the synchronous,
allocation-free, dependency-free core is unaffected for anyone who does
not want a runtime. To own the glue yourself, or to use std threads
instead,
[`examples/decode_many_threads.rs`](examples/decode_many_threads.rs)
works the same pattern with std threads. At roughly 88 ns per sample,
one blocking thread decodes hundreds of real-time feeds, so the pool
stays small.

## Other modes / presets

Bell 202 is the default, but baud rate and tone pair are first-class
configuration: `ModemProfile` bundles a validated `BaudRate` +
`TonePair`, and `TncConfig::from_profile` (plus the modulator and
demodulator `new` constructors) accepts any validated combination.
Named presets:

| preset | baud | mark/space | use |
|---|---|---|---|
| `ModemProfile::BELL_202` | 1200 | 1200/2200 Hz | VHF APRS (default) |
| `ModemProfile::HF_APRS_300` | 300 | 1600/1800 Hz | HF APRS (10.147 MHz) |
| `ModemProfile::BELL_103` / `_ORIGINATE` | 300 | 1270/1070 Hz | Bell 103 originate |
| `ModemProfile::BELL_103_ANSWER` | 300 | 2225/2025 Hz | Bell 103 answer |
| `ModemProfile::G3RUH_9600` (`g3ruh` feature) | 9600 | scrambled baseband (no tones) | 9600-baud packet |

```rust
use warble::{ModemProfile, SampleRate};
let rate = SampleRate::new(48_000)?;
let profile = ModemProfile::HF_APRS_300;
assert_eq!(profile.baud().bps(), 300);
# #[cfg(feature = "tnc")]
# { let _ = warble::tnc::TncConfig::from_profile(rate, profile)?; }
# Ok::<(), warble::ConfigError>(())
```

The CLI mirrors this with `--preset bell202|hf300|bell103|bell103-answer|g3ruh`
on `decode`, `encode`, `gen`, `bench` and `serve` (default
`bell202`). Non-Bell-202 profiles use a single balanced receiver
chain; the multi-chain emphasis-compensating bank is Bell-202-tuned
(see `docs/BENCHMARKS.md` for measured 300-baud numbers).

> **`--preset g3ruh` needs the `g3ruh` feature**, which the `cli`
> aggregate does *not* include. A `--features cli` build stops the
> preset list at `bell103-answer` and rejects `--preset g3ruh` as an
> invalid value; use `--all-features` (or `--features cli,g3ruh`) for
> the G3RUH preset.

## IL2P

The `il2p` feature (off by default, with no new dependencies)
implements the Improved Layer 2 Protocol of Nino Carrillo, KK4HEJ, an
alternative framing for AX.25 traffic that replaces HDLC entirely.
FX.25 wraps a standard HDLC frame in FEC so that legacy receivers still
decode it. IL2P instead re-encodes the frame as a `0xF15E48` sync word
after a `0x55` preamble, a 13-byte translated header with its own
Reed-Solomon parity, and scrambled payload blocks each protected by
RS(255,k). There are consequently no flags and no bit stuffing, and FEC
covers the header as well. The trade-off is compatibility: an IL2P
transmission is opaque to a plain AX.25 receiver, so **both ends must
speak IL2P**. Choose FX.25 to stay interoperable with legacy stations,
and IL2P when both ends are yours and you want stronger, more uniform
error protection at lower overhead.

This implements **IL2P Specification Draft v0.6** (16 March 2024). The
wire constants a peer must agree on, namely the scrambler preset, the
PID code table, the UI control subfield and the payload block divisor,
are pinned by the specification's own "Example Encoded Packets"
verification vectors, which our encoder reproduces byte for byte.

**Interoperability is verified on the air**, in both directions,
against an independent implementation in `tests/il2p_differential.rs`
(tier 4). We transmit at 16 parity symbols per block, the level current
stations use, and receive either level. The header's FEC-level bit says
which one is in use, and the legacy 2/4/6/8-symbol scheme derives its
symbol count from the block size rather than carrying it.

One caveat remains: the **optional** Trailing CRC is not implemented
(v0.6 says its use "must be coordinated between participating
stations", and it is not a default).

> This took three attempts to get right. Through v0.4 the crate
> implemented the earlier draft and could not exchange a frame with
> anybody, even though every round-trip test passed: an encoder and
> decoder that are mutual inverses remain mutual inverses when a shared
> constant is wrong. Spec vectors fixed that. The vectors then passed
> while the crate was still undecodable, for two reasons that neither
> vectors nor round trips can detect. It was applying NRZI, which the
> specification forbids ("Differential encoding is not used"), and it
> cleared the header's FEC-level bit as v0.6 instructs while sending
> 16-parity payloads, which tells a deployed receiver to collect the
> wrong number of bytes. Only putting audio in front of another
> implementation found either fault. See
> [docs/APRS_CONFORMANCE.md](docs/APRS_CONFORMANCE.md) §6.1 and §6.2.

On the CLI (which transmits 16 parity symbols per block):

```sh
# Generate three IL2P frames as 1200-baud Bell 202 audio…
cargo run --features cli -- gen --out il2p.wav --count 3 --il2p

# …and decode them back (a plain `decode` sees nothing here).
cargo run --features cli -- decode --il2p il2p.wav
```

> **`--il2p` is implemented by `gen` and `decode` only.** The flag is
> shared plumbing, so `encode`, `bench` and `serve` parse it as well,
> but they refuse it with an explanatory error rather than quietly
> producing or expecting plain AX.25.

In the library, transmit is `il2p::encode_ui_frame` (or `encode` /
`encode_raw`) plus the `il2p::tx_bits` MSB-first bit iterator feeding
the usual NRZI → modulator chain. Receive is a demodulator → NRZI →
`Il2pReceiver` chain: a parallel bit consumer with its own sync-word
correlator, tolerating one bit error. It is kept separate from
`TncReceiver` because IL2P frames never resemble HDLC:

```rust
# #[cfg(all(feature = "il2p", feature = "mod", feature = "demod"))] {
use warble::SampleRate;
use warble::ax25::{Address, UiFrame};
use warble::demodulator::{AfskDemodulator, DemodulatorConfig};
use warble::il2p::{self, ENCODED_MAX, Il2pParity, Il2pReceiver};
use warble::modulator::{Modulator, ModulatorConfig};
use warble::nrzi::{self, NrziDecoder};

let rate = SampleRate::new(48_000)?;
let frame = UiFrame::new(
    Address::new(b"APRS", 0)?,
    Address::new(b"N0CALL", 7)?,
    b">IL2P demo",
);
let mut encoded = [0u8; ENCODED_MAX];
let len = il2p::encode_ui_frame(&frame, Il2pParity::Sixteen, &mut encoded)?;
let audio: Vec<i16> = Modulator::new(ModulatorConfig::bell_202(rate)?)
    .i16_samples(nrzi::encode_iter(il2p::tx_bits(&encoded[..len], 16, 2)))
    .collect();

let mut demod = AfskDemodulator::new(DemodulatorConfig::bell_202(rate)?)?;
let mut nrzi_rx = NrziDecoder::default();
let mut rx = Il2pReceiver::new(Il2pParity::Sixteen);
let mut got = false;
for &s in &audio {
    if let Some(line) = demod.push_sample_i16(s)
        && let Some(Ok(rxf)) = rx.push(nrzi_rx.decode(line))
    {
        assert_eq!(rxf.ui_frame()?, frame);
        got = true;
    }
}
assert!(got);
# }
# Ok::<(), Box<dyn std::error::Error>>(())
```

Like everything else in the crate the codec and receiver are
`no_std`, allocation-free and integer-only. See
`examples/il2p_roundtrip.rs` for the same round trip with injected
byte corruption and corrected-symbol statistics, and
`tests/il2p_audio.rs` for the audio-level proofs (multi-frame,
corruption within/beyond the correction radius, sync-word bit-error
tolerance, coexistence with plain HDLC traffic on the same audio).

## WSPR

WSPR (Weak Signal Propagation Reporter, the `wspr` feature) is a
beacon mode for probing radio propagation: a station transmits its
callsign, 4-character Maidenhead grid square and power level in a
~110.6 s burst of 4-tone FSK with tones only 12000/8192 ≈ 1.4648 Hz
apart, and stations around the world report what they heard. The
heavy FEC (a K=32 rate-1/2 convolutional code) and very long symbols
buy extraordinary sensitivity at 50 bits per two minutes.

Generate a beacon WAV and decode it back with the CLI (built with
`--features cli`):

```sh
# One transmission: K1ABC in FN42 at 37 dBm (5 W), tone 0 at 1500 Hz,
# written as a ~110.6 s 16-bit mono WAV at 12 kHz.
warble wspr gen --callsign K1ABC --grid FN42 --power 37 -o beacon.wav

# Decode a 12 kHz capture (≥ ~110.6 s): one line per decoded signal
# with frequency, time offset and quality metrics.
warble wspr decode beacon.wav
# K1ABC FN42 37 dBm | freq 1500.0 Hz | dt 0.00 s | snr 12 dB | sync 1.00
```

`--offset-hz` moves the tone-0 frequency (the sub-band convention is
1400–1600 Hz), and `--window` narrows the decoder's search around
1500 Hz. The decoder is fixed at 12 kHz; it reports an error on other
rates rather than resampling without being asked.

The same round trip from the library (`wspr` + `std` for the receive
engine):

```rust
# #[cfg(all(feature = "wspr", feature = "std"))]
# fn main() -> Result<(), Box<dyn std::error::Error>> {
use warble::SampleRate;
use warble::geo::MaidenheadGrid;
use warble::wspr::{
    WsprConfig, WsprDecoder, WsprDecoderConfig, WsprMessage, WsprModulator,
};

let msg = WsprMessage::new("K1ABC", MaidenheadGrid::new("FN42")?, 37)?;
let config = WsprConfig::new(1_500, SampleRate::new(12_000)?)?;
let mut samples: Vec<i16> = WsprModulator::for_message(config, &msg).collect();
samples.resize(114 * 12_000, 0); // pad to a full capture window

let decoder = WsprDecoder::new(WsprDecoderConfig::new(1_500, 100)?);
for decode in decoder.decode(&samples)? {
    println!("{:?} at {:.1} Hz, ~{:.0} dB SNR", decode.message, decode.freq_hz, decode.snr_db);
}
# Ok(())
# }
# #[cfg(not(all(feature = "wspr", feature = "std")))]
# fn main() {}
```

`examples/wspr_beacon.rs` is the same loop with quality metrics
printed.

**Embedded feasibility.** The TX side embeds without difficulty: the
modulator is `no_std`, allocation-free and integer-only, like the
crate's AFSK modulator. Receive is another matter, because the engine
buffers a whole capture and is memory-hungry. MEASURED peak heap for
one 114 s capture at 12 kHz is **≈ 14.9 MiB**, with four buffers live
at once inside the decimator: a padded `i16` copy of the capture at
≈ 2.8 MB, the mixed complex-f32 signal *at the input rate* at
≈ 11.1 MB, then ≈ 1.4 MB and ≈ 346 KB for the two FIR stages. Only
≈ 342 KB persists, the surviving 375 Hz complex-f32 baseband.
`WsprDecoder` is therefore std-gated, and only the buffer-free decode
math stays `no_std`: deinterleave, the Fano sequential decoder
(hard-capped at 400 000 node visits, which bounds decode time), and
message unpack.

Sensitivity needs the same care. The widely quoted **−31 dB** SNR in
2500 Hz belongs to the reference implementation's decoder, which stacks
noncoherent averaging techniques that this single-pass engine does not
attempt. Ours decodes its own transmissions down to a measured,
test-pinned **−22 dB** (see `tests/wspr_rx.rs`) and fails cleanly below
that. Treat anything between −22 and −31 dB as a signal this decoder
will miss but a fully equipped WSPR station would copy.

## FT8

FT8 (the `ft8` feature) is the weak-signal QSO mode of the modern
weak-signal family: stations exchange short structured messages in strictly timed
**15-second cycles**. A 77-bit payload is protected by a CRC-14 and
an LDPC(174,91) code, mapped onto 58 Gray-coded 8-FSK data symbols,
and framed by three 7×7 Costas sync arrays into 79 channel symbols.
Those go out as GFSK-shaped continuous-phase 8-tone FSK with 6.25 Hz
tone spacing and 0.16 s symbols, giving about 12.64 s of audio inside
the 15 s slot.

Implemented from the published protocol definition: the
Franke/Somerville/Taylor QEX paper, plus the authors' own resource
package `ft4_ft8_protocols.tgz` (reference [14] of that paper), which
§9 of the paper places in the **public domain** and explicitly carves
out of WSJT-X's GPLv3. The two LDPC matrices are embedded from that
package, vendored at `third_party/ft4_ft8_public/`, and checked against
it by the test suite on every CI run.

**Protocol licence conditions.** The public-domain dedication is
conditional, and using the name "FT8" accepts those conditions. Two of
them matter here; `src/ft8.rs` documents all five against what this
crate does.

- *Unassigned message types must not be assigned.* Honoured: everything
  outside the supported subset is rejected rather than repurposed.
- *"Robotic or unattended QSOs must be explicitly disallowed."* They
  are disallowed here. This crate is a modem and holds no QSO state, so
  it cannot complete a QSO on its own, but **using it to conduct
  robotic or unattended FT8 QSOs is not a supported use and is contrary
  to the protocol licence this implementation relies on.** An operator
  must be present for each exchange. Unattended *reception*, such as a
  decode logger or a propagation monitor, is not a QSO and is
  unaffected.

**Supported message subset.** Standard `i3 = 1` messages: two standard
callsigns (or `CQ`, `QRZ` or `DE` first), the `R` flag, and a trailer
that is a grid, a signal report, `RRR`, `RR73` or `73`. Free text under
`i3.n3 = 0.0` (13 characters) is supported as well. Everything else,
including directed CQ, compound and hashed callsigns, and the contest
and telemetry types, is rejected with a specific error on both the TX
and RX sides rather than silently mangled.

Generate a transmission WAV and decode it back with the CLI (built
with `--features cli`):

```sh
# One transmission: "CQ K1ABC FN42", tone 0 at 1500 Hz, written as a
# ~12.64 s 16-bit mono WAV at 12 kHz.
warble ft8 gen --message "CQ K1ABC FN42" -o cq.wav

# Free text instead of a standard exchange:
warble ft8 gen --message "TNX BOB 73 GL" --free-text -o tnx.wav

# Decode a 12 kHz capture (≥ ~12.64 s): one line per decoded signal
# with frequency, time offset and quality metrics.
warble ft8 decode cq.wav
# CQ K1ABC FN42 | freq 1500.0 Hz | dt 0.00 s | snr 21 dB | sync 0.87
```

`--offset-hz` moves the tone-0 frequency, and `--window` narrows the
decoder's search around 1500 Hz (default ±300 Hz, that is 1200–1800 Hz).
The decoder is fixed at 12 kHz and rejects other rates with an error
rather than resampling on your behalf.

The same round trip from the library (`ft8` + `std` for the receive
engine):

```rust
# #[cfg(all(feature = "ft8", feature = "std"))]
# fn main() -> Result<(), Box<dyn std::error::Error>> {
use warble::SampleRate;
use warble::ft8::{
    Ft8Config, Ft8Decoder, Ft8DecoderConfig, Ft8Message, Ft8Modulator, Ft8Tail,
};

let msg = Ft8Message::standard("CQ", "K1ABC", false, Ft8Tail::grid("FN42")?)?;
let config = Ft8Config::new(1_500, SampleRate::new(12_000)?)?;
let mut samples: Vec<i16> = Ft8Modulator::for_message(config, &msg).collect();
samples.resize(15 * 12_000, 0); // pad to a full 15 s cycle

let decoder = Ft8Decoder::new(Ft8DecoderConfig::new(1_500, 300)?);
for decode in decoder.decode(&samples)? {
    println!("{} at {:.1} Hz, ~{:.0} dB SNR", decode.message, decode.freq_hz, decode.snr_db);
}
# Ok(())
# }
# #[cfg(not(all(feature = "ft8", feature = "std")))]
# fn main() {}
```

`examples/ft8_cycle.rs` is the full encode → WAV → decode cycle with
the metrics printed.

**Embedded feasibility.** TX embeds without difficulty: `no_std` and
allocation-free, though the GFSK pulse evaluates an `f64` erf per
sample, which means soft float on an MCU. `Ft8Modulator` documents that
cost.

Receive splits in two. The **decode math is `no_std`**. The
LDPC(174,91) min-sum decoder works in about 3.7 KB of stack (174×2 f32
LLR/posterior arrays plus 83×7 f32 check messages), with iterations
hard-capped at `LDPC_MAX_ITERS = 40` and an early exit
on H·ĉ = 0; the Gray-demap LLR builder, CRC-14 verify and message
unpack join it there. The **capture engine is std-gated**, because
`Ft8Decoder` buffers a 15 s capture with a MEASURED peak heap of
≈ 2.13 MiB: ≈ 1.5 MB at the input rate during decimation, ≈ 294 KB at
the 2400 Hz intermediate rate, and ≈ 98 KB of persistent 800 Hz complex
baseband. FT8's capture runs 12.64 s against WSPR's 114 s, so it needs
about a seventh of WSPR's peak.

Sensitivity carries the same caveat as WSPR. The widely quoted
**−21 dB** SNR in 2500 Hz belongs to the reference implementation's
decoder, with its a-priori message hypotheses and subtraction passes.
Ours decodes its own transmissions down to a measured, test-pinned
**−14 dB**, with 10 of 10 seeds still passing at −16 dB (see
`tests/ft8_rx.rs`), and fails cleanly below that. Treat anything
between −14 and −21 dB as a signal this decoder may miss but a fully
equipped FT8 station would copy.

## M17 (data)

[M17](https://spec.m17project.org/) is an open, royalty-free digital
radio protocol: 4-level FSK at 4800 symbols/s carrying voice (Codec2),
data, or both. The `m17` feature (off by default, no dependencies,
fully `no_std` and allocation-free) ships **packet-mode data**,
implemented from the published spec. That covers base-40 callsign
addressing, the Link Setup Frame, packet superframes with CRC-16
(0x5935), the K=5 rate-1/2 convolutional FEC with P1/P3 puncturing, the
QPP interleaver and randomizer, the published sync bursts, a
Golay(24,12) codec (the building block of stream mode's LICH), and a
4-level RRC-shaped (α = 0.5) baseband PAM modem at 48 kHz, in both
directions. **Voice is not shipped**: Codec2 is an external LGPL
dependency pending operator approval, and the proposal lives in
`docs/ARCHITECTURE.md`, "Codec2 voice for M17 stream mode".

Note that this is a *baseband* modem. The waveform below is what an FM
exciter's modulator input accepts and what a discriminator output
yields; the RF 4FSK itself happens inside the radio.

```rust
# #[cfg(feature = "m17")] {
use warble::SampleRate;
use warble::m17::{Address, Lsf, M17FrameEvent, M17PacketTx, M17Receiver, PacketAssembler};

let lsf = Lsf::packet_data(Address::broadcast(), Address::from_callsign("N0CALL")?, 0);
let sr = SampleRate::new(48_000)?;
let mut tx = M17PacketTx::new(sr, lsf, b"Hello, M17!")?;

let mut rx = M17Receiver::new(sr)?;
let mut asm = PacketAssembler::new();
while let Some(sample) = tx.next_i16() {
    match rx.push_i16(sample) {
        Some(M17FrameEvent::Lsf(l)) => asm.start(l),
        Some(M17FrameEvent::PacketFrame(f)) => {
            if let Some(payload) = asm.feed(&f) {
                println!("{}", String::from_utf8_lossy(payload));
            }
        }
        None => {}
    }
}
# }
# Ok::<(), Box<dyn std::error::Error>>(())
```

`examples/m17_packet.rs` is the same round trip with the LSF fields
printed.

The same round trip from the command line (the binary always has M17
support; `m17` rides the `cli` aggregate feature like `wspr`/`ft8`):

```sh
# One packet transmission (preamble + LSF + frames + EOT), 48 kHz WAV:
warble m17 gen --src N0CALL --dst BROADCAST --text "Hello, M17!" -o m17.wav

# Decode a 48 kHz capture: LSF addresses + payload on stdout, FEC
# statistics (LSF / packet-frame counts) on stderr.
warble m17 decode m17.wav
# LSF: N0CALL -> @ALL | type 0x0002 | CAN 0
# payload: Hello, M17!
```

`--dst` takes a callsign or the literal `BROADCAST`, and `--can` sets
the channel access number (0..=15). The decoder is fixed at 48 kHz, or
10 samples per 4800 Hz symbol; resample other captures externally.
## Embedded use

Every feature except `std` and `cli` holds the guarantees from the top
of this file. Builders write into caller-provided `&mut [u8]`, parsers
borrow from the input, the transmit path is a lazy iterator chain, and
the `i16` PCM path is integer arithmetic throughout, so no
floating-point unit is required.

**[docs/EMBEDDED.md](docs/EMBEDDED.md) is the guide for microcontroller
work.** It covers which chips can keep up and at what cost, the
`DevicePreset` enum that resolves a validated configuration per chip,
the bounded-latency contract that makes decoding real-time-safe, and
the four ways to decode continuously while one core also reads sensors,
logs and beacons (std threads, a bare-metal superloop, embassy, RTIC).

For wiring a dev board to a handheld radio, including interface
circuits and PTT keying, see the
[ESP32 hardware guide](examples/esp32-riscv/README.md).

## Examples

Runnable, commented examples live in [`examples/`](examples/). Besides
the eleven below, `throughput.rs`, `live_capture.rs` and the five
`balloon_tracker*` variants (std threads, bare-metal poll loop,
embassy, RTIC and tokio) are covered in their own sections above.

```sh
# Build an APRS position beacon and write Bell 202 samples to beacon.wav.
cargo run --example encode_wav --features tnc,wav

# Decode a WAV back into human-readable APRS frames.
cargo run --example decode_wav --features tnc,wav -- beacon.wav

# Allocation-free fixed-buffer round trip (the API an embedded user calls).
cargo run --example embedded_modem --features tnc

# Monitor: decode audio into structured log lines (sample-clock timestamp,
# SRC>DEST, digipeater path with used hops marked '*', payload summary).
cargo run --example decode_to_log --features tnc,wav -- beacon.wav

# Workstation digipeater: WAV/stdio in, per-frame tracing of every relay
# decision (dupe check, exact path mutation, typed ignore reasons),
# JSON-lines log, per-alias policy flags, dry-run by default.
cargo run --example digipeater_station --features tnc,digipeat,wav -- beacon.wav

# Receive -> decide -> respond: ack + canned reply for APRS messages
# addressed to MYCALL, rendered to reply.wav (spec-correct ack{n} semantics).
cargo run --example trigger_reply --features tnc,wav -- input.wav

# N WAV feeds decoded in parallel on a bounded worker pool, frames
# flowing through a bounded channel into a JSON-lines sink (the
# runtime-free concurrency idiom: std threads, no tokio in cargo tree).
cargo run --example decode_many_threads --features tnc,wav -- out.jsonl a.wav b.wav

# The same job on tokio, via the `asynk` stream API: decode_many merges
# N raw-PCM feeds into one stream tagged by feed index, with the bounded
# channel throttling the decoders to whatever the sink can take.
cargo run --example decode_many_tokio --features async -- a.s16 b.s16

# Decode a LIVE PCM stream (TCP or stdin) rather than a file — the case
# that wants a runtime. Self-demo spawns a local paced "radio";
# --timeout shows that cancelling is just dropping the stream.
cargo run --example decode_pcm_tokio --features async

# Async balloon tracker: decode stream + sensor + beacon scheduler as
# tokio tasks, cancelled by a flight timer. Self-demo needs no input.
cargo run --example balloon_tracker_tokio --features async,wav

# IL2P encode → corrupt → modulate → demodulate → decode round trip,
# printing per-stage corrected-symbol statistics.
cargo run --example il2p_roundtrip --features il2p,mod,demod

# Read the live APRS-IS feed from the internet and report statistics:
# packet kinds, busiest stations and igates, RF vs internet, bounding box.
cargo run --example aprs_is --features std,aprs,micE -- --lat 39.1 --lon -94.6

# Encode and decode APRS with no radio, sound card or network.
cargo run --example aprs_offline --features std,aprs,micE
```

**Copying one into your own crate?** The examples that read or write
`.wav` files use the [`hound`](https://crates.io/crates/hound) crate
directly for the file I/O. warble's own `wav` feature covers
`warble::wav::{decode_frames, sniff_pcm, ...}`, but it does not
re-export a WAV writer, so add `hound = "3"` alongside `warble` if you
lift that part. Everything touching the modem itself needs only the
features named in each command above, and the examples that work on raw
PCM (`decode_pcm_tokio` and `embedded_modem`) have no such dependency.

The application-story examples keep their core logic in pure functions.
The host test suite (`tests/app_examples.rs`) `#[path]`-includes those
functions and checks them against the real transmit and receive chains,
covering exact log lines, ack semantics and a full audio round trip.

### The workstation digipeater

The digipeater story ships at TWO tiers sharing ONE relay core, the
library's `digipeat` module (`relay_decision` + `DupeRing`), so no
forked relay logic exists anywhere:

- **embedded tier** (`examples/esp32-riscv/src/digipeater.rs`):
  no_std, alloc-free, for a dev board wired to a radio;
- **workstation tier** (`examples/digipeater_station.rs`): the
  observability tier, with structured `tracing` spans for every
  decision (frame heard → dupe check → exact path mutation, before →
  after → relay/ignore with a typed reason), stats counters with an
  exit self-report, a JSON-lines decision log, per-alias policy flags
  (`--mycall`, `--wide-max`, `--no-wide`), and a **dry-run default**
  (pass `--transmit` to write relay audio).

Because both tiers decide identically, the workstation example doubles
as a **debugging tool for the embedded digipeater**. Run it against a
WAV capture to see what your ESP32 heard: every decision the board made
silently is traced and explained at your desk, with no radio involved.

## Usage

Modulating bits into PCM samples:

```rust
use warble::{Bit, Modulator, ModulatorConfig, SampleRate};

let config = ModulatorConfig::bell_202(SampleRate::new(48_000)?)?;
let bits = [Bit::One, Bit::Zero, Bit::One];
let samples: Vec<i16> = Modulator::new(config)
    .i16_samples(bits.into_iter())
    .collect();
assert_eq!(samples.len(), 3 * 40); // 48000 / 1200 = 40 samples per bit
# Ok::<(), warble::ConfigError>(())
```

Demodulating PCM samples back into bits, one sample at a time:

```rust
use warble::{AfskDemodulator, Bit, DemodulatorConfig, Modulator,
             ModulatorConfig, SampleRate};

let sr = SampleRate::new(48_000)?;

// Transmit a 32-bit alternating preamble, the payload, then two trailing
// bits so the final payload bit cell completes inside the sample stream.
let payload = [Bit::One, Bit::One, Bit::Zero, Bit::One];
let bits = (0..32)
    .map(|i| if i % 2 == 0 { Bit::One } else { Bit::Zero })
    .chain(payload.iter().copied())
    .chain([Bit::Zero, Bit::Zero]);
let samples: Vec<i16> = Modulator::new(ModulatorConfig::bell_202(sr)?)
    .i16_samples(bits)
    .collect();

// Receive: push samples; each completed bit cell yields Some(Bit).
let mut demod = AfskDemodulator::new(DemodulatorConfig::bell_202(sr)?)?;
let mut recovered = Vec::new();
for s in samples {
    if let Some(bit) = demod.push_sample_i16(s) {
        recovered.push(bit);
    }
}
// The preamble region is settling time; the payload follows it exactly.
assert!(recovered.windows(payload.len()).any(|w| w == payload));
# Ok::<(), Box<dyn std::error::Error>>(())
```

On a target without `alloc`, use the same `feed`/`next_i16` and
`push_sample_i16` calls directly and write each sample or bit into a
fixed-capacity buffer or straight to a peripheral; the iterator adapters are
convenience only.

## Sample rates and tuning

`SampleRate::new` accepts 8 000 to 48 000 Hz. The tested set, exercised by
every round-trip and noise suite, is 8000, 11025, 22050, 44100, and
48000 Hz.

Tuning notes:

* The slicer's PLL switches loop gain on a lock detector. While
  searching it corrects half the phase error per zero crossing; after
  seven consecutive crossings land within a quarter bit period it drops
  to an eighth, which keeps a noisy or fading tail from pulling the
  sampling instant around. A 32-bit alternating preamble (`1 0 1 0 …`)
  is enough for the discriminator window to fill and the PLL to lock;
  treat the demodulated preamble region as settling time.
* Measured noise behavior (pinned by `tests/noise.rs` with seeded,
  reproducible noise): at 20 dB SNR and again at 10 dB SNR the modem
  recovers 100 % of payloads across the seeded cases at all five sample
  rates. At 0 dB SNR (noise as strong as the signal) recovery still
  succeeds in the majority of cases at 48 kHz; the suite pins a floor of at
  least 120 of 200 cases rather than promising perfection.

## Testing & verification

* **Coverage matrix**: [docs/COVERAGE.md](docs/COVERAGE.md) tracks a
  layer-by-layer matrix across five categories (encode-KAT /
  decode-KAT / roundtrip / edge / reject) from the AFSK modulator up
  through NRZI, HDLC, AX.25, every APRS payload kind (including
  compressed and timestamped positions), Mic-E, KISS, the G3RUH
  scrambler, the FX.25/RS(255,k) FEC layer, the TNC pipeline and the
  CLI. Every cell cites at least one passing test.
* **Differential harness**: `tests/differential.rs` checks the stack
  against an independent reference implementation (external oracle,
  `#[ignore]`d unless the reference binaries are configured) over a
  seeded 320-case corpus spanning 16 packet kinds. Agreement is
  100 % (320/320) in both directions: our transmit against the
  reference decoder, and the reference generator against our receiver.
  An SNR shootout decodes the *same* noisy WAV with both decoders and
  asserts ours recovers at least as many frames as the reference at
  every asserted level (we tie 50/50 from clean down to 1.5 dB).
* **Fuzz robustness**: `tests/fuzz_decode.rs` drives every decoder and
  parser with hundreds of thousands of seeded random, truncated and
  corrupted inputs, covering bytes, bits, frames and raw PCM (including
  NaN and rail values). It found zero panics; every failure is a typed
  error or a silently discarded non-frame.
* **Pinned SNR ladder**: `tests/snr.rs` pins measured frame recovery
  under seeded noise: 30/30 frames at 20, 10, and 5 dB SNR, and 24/30
  at 0 dB. Deterministic seeds make the counts exact and reproducible.

The measured numbers behind all four bullets are recorded in
[docs/COVERAGE.md](docs/COVERAGE.md).

**What "correct" means for a decoder.** A packet that decodes is not the
same as a packet that was understood, and "round-trips" turns out to
mean four different things. Section 4 of
[docs/APRS_CONFORMANCE.md](docs/APRS_CONFORMANCE.md) sets out the
vocabulary the crate reasons in: parse and build as partial maps, the
canonicalisation `build ∘ parse`, and five properties (byte fidelity,
legality preservation, semantic idempotence, normalisation, legal-
spelling preservation) that separate a rebuild which lost information
from one that chose a different legal spelling. It also records what
that vocabulary does **not** promise, since a protocol this old cannot
be decoded totally: which parts of the spec define nothing, why some
rejections are the correct answer, and which of the crate's rules are
empirical properties of measured traffic rather than theorems. The
classification is implemented in `tests/common/mod.rs` and is what the
ratchet floors are written against.

## Validation

* **Round trips**: `tests/roundtrip.rs` drives the modulator into the
  demodulator at every supported sample rate on both PCM paths and requires
  exact payload recovery.
* **Seeded noise**: `tests/noise.rs` mixes deterministic, seeded uniform
  noise at controlled SNRs (no wall clock anywhere), so every failure
  reproduces exactly.
* **Reference oracle**: `tests/oracle.rs` validates the modem in both
  directions against a reference implementation, an external and
  independently developed Bell 202 modem. With the protocol features
  enabled it validates the full APRS/AX.25/NRZI stack the same way. The
  reference WAV generator feeds our receive pipeline, and our transmit
  pipeline feeds its decoder. These tests are `#[ignore]`d by default
  because they need external binaries. To run them, set the environment
  variables `WARBLE_REF_GEN` and `WARBLE_REF_DECODE` to the absolute
  paths of the reference generator and decoder, then run:

  ```sh
  WARBLE_REF_GEN=/path/to/generator \
  WARBLE_REF_DECODE=/path/to/decoder \
  cargo test -- --ignored
  ```

## License

Licensed under either of

* MIT license ([LICENSE-MIT](LICENSE-MIT))
* Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))

at your option.

### Dependency licences

The default build and every `no_std` feature set have **no runtime
dependencies at all**, so nothing below applies unless you opt in.

| dependency | licence | pulled in by |
|---|---|---|
| `hound` | Apache-2.0 only | `wav`, and so `cli` |
| `clap` | MIT OR Apache-2.0 | `cli` |
| `serialport` | **MPL-2.0** | `ptt`, and so `cli` |
| `cpal` | Apache-2.0 only | `capture` (never enabled by another feature) |
| `tokio`, `tokio-stream` | MIT | `async` |

Two of these are worth knowing about. `serialport` is MPL-2.0, a
file-level copyleft: linking it does not affect warble's own grant, but
if you distribute a statically linked binary with the `ptt` feature on,
MPL-2.0 section 3.2 asks you to make that dependency's source available.
`hound` and `cpal` are Apache-2.0 only rather than dual-licensed, so
picking the MIT branch for warble does not avoid Apache-2.0 terms if you
enable `wav` or `capture`.

### Third-party material

`third_party/ft4_ft8_public/` vendors four FT4/FT8 protocol tables that
section 9 of the defining QEX paper places in the **public domain** and
explicitly carves out of WSJT-X's GPLv3. Its README records the
provenance chain, the published checksums, and the conditions the
dedication attaches to use of the mode names. Four tests read those files
directly, so the provenance is checked on every run rather than asserted.
