# warble on ESP32-class RISC-V

Copy-paste APRS **beacon** (modulation), **decoder** (demodulation) and
**digipeater** (store-and-forward relay) examples for riscv32
bare-metal targets: the ESP32-C3, ESP32-C6 and similar dev boards
built around a soft-float RV32IMC/IMAC core.

This is a detached `#![no_std]` library crate (its own `Cargo.toml`
with an empty `[workspace]` table, so the main repository never builds
it implicitly). It depends only on `warble` by path, with
`default-features = false, features = ["tnc", "digipeat"]`. That is
the minimal set: `tnc` pulls in `aprs` (→ `ax25` → `nrzi`), `mod` and
`demod`, while `digipeat` adds the WIDEn-N relay-decision core and
duplicate suppression used by the digipeater example.

## What warble handles vs. what you wire

| warble (this crate) | you (your HAL) |
|---|---|
| APRS payload building/parsing (typed positions, status, messages) | choosing and initializing the board/HAL (`esp-hal`, `riscv-rt`, …) |
| AX.25 UI framing, FCS, HDLC bit stuffing, NRZI | ADC or I2S input at a fixed sample rate (mic / radio audio in) |
| Bell 202 AFSK modulation and demodulation, all integer `i16` DSP | I2S or PWM output (audio to the radio) |
| chunk-boundary-safe streaming decode state | PTT keying GPIO, timing, and RF legality (your license!) |

Both modules end with a **"YOUR HAL HERE"** section containing a
commented, `esp-hal`-flavored `main.rs` sketch. It is comments only,
so this crate compiles with **zero** HAL dependencies and stays
board-agnostic.

## Hardware guide

**Will my chip keep up?** Yes, on the recommended `i16` path: it is
float-free at runtime, so the no-FPU C3/C6/H2 cores run native integer
code. The embedded guide's section
["Will it run on my chip? (ESP32 RISC-V feasibility)"](../../docs/EMBEDDED.md#will-it-run-on-my-chip-esp32-risc-v-feasibility)
has the full compute-budget analysis: measured host throughput per
mode (re-runnable via `examples/throughput.rs` at the repo root),
cycle-budget extrapolations to 96/160/400 MHz rv32 with stated
assumptions, a per-chip guidance table (C3/C6/H2/P4), the RAM
footprint of the receiver structures, and the on-device measurement
method (cycle-counter CSR around the sample-feed loop) to confirm the
estimates on real silicon.

Each row of that guidance table now names a library `DevicePreset`
variant (`Esp32C3`, `Esp32C3FullBank`, `Esp32C6`, `Esp32C6FullBank`,
`Esp32H2`, `Esp32P4`, `Esp32P4G3ruh`): pick your chip's variant and
`DevicePreset::tnc_config()` resolves const-ly to the exact validated
`TncConfig` the table recommends, so there is nothing to tune before
the first build.

Everything below is board-agnostic: the code only ever sees `i16`
slices, so *any* ADC/I2S/PWM/GPIO-capable pins work. On callsigns: the
examples use the `N0CALL` placeholder, so substitute your own.
Transmitting APRS on amateur bands requires a license.

### Receive path: radio → ADC pin

RX audio comes from the radio's **speaker / headphone jack** (or its
data/accessory port if it has one, since data ports usually give a
cleaner, squelch-independent, fixed-level signal). Speaker audio is a
few volts peak-to-peak swinging around 0 V, and the ADC accepts only a
positive range referred to its own ground. So the signal must be
**attenuated** and **DC-biased** into range:

```text
 radio speaker /      C1                          3V3
 headphone jack      100 nF                        │
  (tip) ──[R1 10k]────┤├──────●────────●        [R3 100k]
                              │        │           │
                           [R2 1k]     └───────────●──────► ADC pin
                              │                    │
  (sleeve) ───────●───────────●                 [R4 100k]
                  │           │                    │
                 GND         GND                  GND
              (shared!)
```

R1/R2 divide the audio by 11 (1 Vpp in → ~91 mVpp out); C1 blocks DC;
R3/R4 re-center the signal on 3.3 V/2 = **1.65 V**, so the ADC sees
~1.65 V ± 45 mV, with headroom for louder radios. Exact values are
uncritical (see the level note in `demod.rs`: the demodulator compares
tone energies, not absolute levels, and only the DC offset matters).

**Set the ADC attenuation before anything will work.** These ADCs
select their input span per channel, and the default is the narrowest
one. On an ESP32-C3 that default spans roughly 0–750 mV, so a 1.65 V
bias sits above full scale and every sample reads railed. Select the
widest attenuation your HAL offers. ESP-IDF spells it
`ADC_ATTEN_DB_12`, and `ADC_ATTEN_DB_11` before v5.2 renamed it;
`esp-hal` has moved the spelling more than once, so take it from the
version you are building against. The widest setting gives about
0–2500 mV on the C3 and up to roughly 0–3100 mV on some other parts, so
check your chip's table rather than assuming 3.3 V. Note that 1.65 V is not the
centre of a 0–2500 mV span: there is ~850 mV of headroom above the
bias and ~1650 mV below it. That is ample for the ±45 mV this divider
produces, but if you attenuate less, bias lower (R3 150 k with R4
100 k puts it at 1.32 V) so that the swing stays symmetric.

**Cleaner alternative:** an **I2S MEMS microphone board** (held near
the radio speaker, or better, an **I2S codec module** with a line
input wired to the jack). You skip the analog circuit entirely and get
signed samples straight from the peripheral at 48 kHz.

The signal chain, explicitly:

> analog audio → ADC samples at 48 kHz → `i16` slices →
> [`demod.rs`](src/demod.rs) demodulator → AX.25/APRS frames.

### Transmit path: PWM or I2S pin → radio mic input, plus PTT

None of the RISC-V ESP32 parts (C3, C6, H2, P4) has a DAC; that
peripheral exists only on the original ESP32 and the S2. Audio leaves
either an **LEDC PWM** pin followed by the RC low-pass below, or an
**I2S** peripheral driving an external codec. The PWM route is the
cheap one: an RC low-pass strips the carrier and a large divider drops
the level to the few millivolts a mic input expects.

```text
 PWM pin ──────[R1 4.7k]──●──────[R2 100k]──●──────┤├─────► radio
                          │                 │      C2       mic in
                       [C1 10nF]         [R3 1k]  100 nF
                          │                 │
                         GND               GND ───────────► radio
                                        (shared!)           mic gnd
```

R1·C1 gives a cutoff of 1/(2π·4.7k·10n) ≈ **3.4 kHz**, which passes the
1200/2200 Hz tones. R2/R3 divide by ~100 (3.3 Vpp → ~33 mVpp,
mic-level); C2 AC-couples into the radio.

One pole at 3.4 kHz is a gentle filter, so **run the PWM carrier well
above audio**. At a 40 kHz carrier it is only about 20 dB down at the
output; at 200–300 kHz it is 35–39 dB down. LEDC trades resolution for
frequency (from the 80 MHz clock, 300 kHz leaves roughly 8 bits), which
is enough for these two tones. If you would rather keep the resolution,
add a second RC stage instead of lowering the carrier.

If your radio's mic input is low impedance, consider raising C2. Into
2.2 kΩ the 100 nF shown puts the corner near 500 Hz, which tilts the
1200 Hz mark about 0.5 dB against the 2200 Hz space; into 600 Ω the
tilt approaches 1.4 dB. A 1 µF part removes the question.

**PTT keying.** Many handhelds key PTT through the mic connector
itself, either as a DC path on the mic line or a separate ring contact,
so check your radio's connector pinout first. Key it with a small NPN
or a logic-level N-MOSFET as a low-side switch, base or gate driven
from the GPIO through a resistor, emitter or source to the shared
ground. **Do not drive the PTT line from a GPIO directly.** An idle PTT
line is usually pulled up inside the radio, often above 3.3 V and on
some handhelds to the battery rail, and clamping that into a GPIO can
exceed the pin's absolute maximum. Measure the line's open-circuit
voltage against audio ground before you connect anything to it.

**PTT timing sequence**:

1. drive the PTT GPIO active (key the transmitter);
2. wait **TXDelay**, the time the radio needs to power up its
   transmitter and the far receiver needs to lock. warble already
   models this inside the audio: `TncConfig`'s **`preamble_flags`**
   field (default 32, settable via `TncConfig::with_flags`) prepends
   32 HDLC flags ≈ 213 ms of flag tone at 1200 Bd, which is the
   classical TNC "TXDelay". If your radio keys up slowly, raise
   `preamble_flags` (or add a plain GPIO delay before starting audio);
3. play the beacon samples at exactly 48 kHz;
4. unkey PTT once the buffer (plus any codec/DMA latency) has drained.

### Example pin assignment (ESP32-C3 dev board)

**This table is only an example.** Any ADC-capable pin for RX and any
PWM/GPIO-capable pins for TX/PTT will do; the code is HAL-agnostic and
never names a pin.

| Signal | Pin (example) | Notes |
|---|---|---|
| RX audio in | GPIO0 (ADC1_CH0) | from the divider/bias circuit above |
| TX audio out | GPIO1 (LEDC PWM) | into the RC low-pass above |
| PTT | GPIO4 | plain GPIO, active per your radio's keying circuit |
| I2S BCLK / WS / DIN | GPIO6 / GPIO7 / GPIO5 | only if using an I2S mic/codec instead of the ADC |
| GND | GND | **shared** with the radio's audio ground |

### Shopping list (generic part classes)

* **ESP32-C3 or ESP32-C6 dev board.** Any board exposing a few GPIOs
  and USB for flashing works; the rv32 core is what matters, not the
  vendor. Memory is not the constraint: `DefaultTncReceiver` measures
  40 848 B, about 10% of the C3's 400 KiB, and the bare
  `AfskDemodulator` is 7832 B. The 320 KiB ESP32-H2 has room as well,
  though at 96 MHz it is the tightest of the four on CPU.
* **A cheap handheld transceiver** with an external mic/speaker
  connector (the common two-plug 3.5 mm + 2.5 mm style, or a
  data/accessory port). Look for: a documented connector pinout and a
  PTT line you can reach.
* **Resistor/capacitor assortment.** A single assortment covers both
  interface circuits above (values in the 1 k–100 k / 10 n–100 n
  decades; nothing is critical).
* *(optional)* **I2S MEMS microphone or I2S codec board**, which
  replaces the RX analog circuit. Look for: 3.3 V supply, standard I2S
  (not PDM-only) output, 48 kHz support.
* Breadboard/jumper wires, and audio plugs matching your radio's
  connector.

### The whole data stream, antenna to callback

```text
RX:  antenna → radio → speaker jack → divider + bias (or I2S mic/codec)
       → ADC/I2S at 48 kHz → i16 DMA buffer (e.g. 512 samples ≈ 10.7 ms
         of audio — small enough for low latency, large enough that the
         per-buffer overhead vanishes; ANY size works, chunk boundaries
         are handled — see demod.rs)
       → AprsDecoder::feed → AX.25 frames → your callback

TX:  fill_position_beacon → i16 PCM buffer (≤ MAX_BEACON_SAMPLES =
       32 768 samples; see the buffer math in beacon.rs)
       → I2S or PWM at 48 kHz → RC low-pass + divider → mic input
       → radio (PTT keyed) → antenna
```

### Gotchas

> * **Sample-rate mismatch** is the #1 silent failure: if the ADC or
>   the output peripheral runs at 44.1 kHz while the code assumes
>   48 kHz, nothing decodes. Verify the real rate, or build the config
>   with your rate.
> * **ADC attenuation** is the #2 silent failure: at the default
>   setting a 1.65 V bias reads railed on every sample. Select the
>   widest attenuation, as described in the receive-path section.
> * **ADC DC offset**: subtract the mid-scale value before feeding the
>   demodulator (see the centering snippet in `demod.rs`). A constant
>   offset degrades the discriminator.
> * **Clipping / levels**: too hot on RX flat-tops the tones; too hot
>   on TX overdrives the mic and distorts. Start quiet, raise slowly.
> * **Speaker-jack squelch tails**: the squelch burst at the end of a
>   received transmission is loud garbage; expect FCS errors in
>   `stats()`, not decoded frames. A data port avoids this.
> * **Shared ground**: the radio's audio ground and the dev board's
>   GND must be connected, or you decode hum instead of packets.
> * **Licensing**: transmitting on amateur bands requires an amateur
>   radio license and your own callsign; rules vary by country. RX-only
>   is generally fine anywhere. `N0CALL` is a placeholder, never
>   transmit it.

## How digipeating works (start here if "WIDE2-1" means nothing to you)

APRS covers a wide area with *digipeaters*: relay stations that hear a
packet and retransmit it, so a 5 W tracker in a valley can still reach
the whole region. [`src/digipeater.rs`](src/digipeater.rs) turns a dev
board plus a radio into one. The concepts, from the ground up:

### The WIDEn-N paradigm

Every AX.25 frame carries a small **digipeater path**: a list of up to
8 "via" addresses between the source and the payload. Instead of
naming specific relay stations, an APRS sender usually writes a
*generic request* like `WIDE2-2`, which reads as: "any wide-area
digipeater, please relay this, and I'd like **2** hops in total." The
callsign part (`WIDE2`) names the *class* of service requested; the
SSID part (`-2`) is the *remaining hop count*, which digis count down:

```text
sent by tracker:   K1ABC-9 > APRS, WIDE2-2      (2 hops still wanted)
after 1st digi:    K1ABC-9 > APRS, N0CALL-1*, WIDE2-1
after 2nd digi:    K1ABC-9 > APRS, N0CALL-1*, WIDE2-1*   (path spent)
```

Each relaying digi *decrements* the count and (while hops remain)
*inserts its own callsign* into the path, so the received packet
records the route it took. When the count reaches its last hop the
WIDE alias itself is marked used, and no further station will touch
the frame. A digi also answers to its **own callsign** as an exact
alias (`via N0CALL-1`), and applies a **max-n policy**: `WIDE1-x` and
`WIDE2-x` are normal; `WIDE3-x` and up are flood-abusive on busy
channels, and a well-configured digi refuses them
(`warble::digipeat::WideLimit` is that knob).

### H-bits: why packets don't loop forever

The `*` in the traces above is the **H bit** ("has been repeated"): a
single bit carried inside each path address on the wire. A digipeater
only ever considers the **first unused hop**, meaning the first
address whose H bit is clear, and it sets that bit when it relays.
Once every hop in the path is marked used, the frame is structurally
dead: *no* conforming digipeater will relay it again, no matter how
many hear it. That is the loop protection. Without H bits, two digis
in range of each other would bounce every packet back and forth
forever. The library's `relay_decision` implements exactly this (and
never relays a fully-used path); the example only feeds it frames and
transmits what it returns.

### Dupe windows: why the same packet isn't relayed twice

H bits stop *loops*, but not *echoes*: when three digis all hear the
same tracker, all three would legitimately relay the same packet
(each copy's path still has an unused hop). A digi therefore keeps a
short memory of recently-heard transmissions and refuses to relay
anything it has already relayed within a **dupe window**, customarily
~30 seconds. What it stores is a fingerprint of
**source + destination + payload**, and the path is left out of it:
the same beacon arriving via a different digi is still the same
beacon. `warble::digipeat::DupeRing` is that memory: a fixed-size ring
of fingerprints, no heap, timestamped with a monotonic millisecond
clock **you** supply (the library has no clock; see "YOUR TIMER HERE"
in `digipeater.rs`).

### Why single-frequency store-and-forward suits half-duplex hardware

APRS runs on **one shared frequency** per region, and a cheap handheld
is **half-duplex**: it can listen or talk, never both. A digipeater
built from one radio therefore *must* work store-and-forward: decode
the whole packet into memory first, wait until the channel is free,
then key up and retransmit it. That is exactly the shape of
`Digipeater::feed`: RX samples in, a fully rendered TX buffer out, and
*your* firmware decides when to play it. The half-duplex sequence is:

1. **wait for a clear channel**. warble's demodulator does not expose
   a carrier-detect (DCD) signal today, so gate on "no decode in
   progress + RX energy near the noise floor", or apply a short random
   delay after the decode (the station you just heard has, by
   definition, stopped talking). `digipeater.rs` documents all three
   options;
2. **key PTT**;
3. **TXDelay**, already inside the rendered audio as the preamble
   flags (~213 ms of flag tone by default);
4. **play** the buffer at exactly 48 kHz;
5. **unkey**.

### Licensing note (read this one twice)

A digipeater **transmits automatically, without a human at the key**.
Before putting one on the air you need an amateur radio license, your
own callsign in place of the `N0CALL` placeholder, and, in many
countries, to satisfy specific rules about **unattended/automatic
operation** (some licenses require a separate notice or permit for
unattended stations, restrict where they may operate, or require a
remote shutdown capability). Check your local regulations before
leaving a digipeater running. Also be a good neighbor: a redundant
digi on a busy channel makes the network *worse*, so serve `WIDE1-x`
only (fill-in) unless your site needs wide coverage.

## Copy-paste instructions

1. Create your firmware crate as usual (e.g. `esp-generate` /
   `cargo generate esp-rs/esp-template` for `esp-hal`).
2. Add warble to its `Cargo.toml`:

   ```toml
   warble = { version = "0.1", default-features = false, features = ["tnc", "digipeat"] }
   ```

3. Copy [`src/beacon.rs`](src/beacon.rs), [`src/demod.rs`](src/demod.rs)
   and/or [`src/digipeater.rs`](src/digipeater.rs) (the digipeater
   needs `demod.rs` and the `digipeat` feature) into your `src/`,
   declare them (`mod beacon;` / `mod demod;` / `mod digipeater;`),
   and follow the commented `main.rs` sketch at the bottom of each
   file to wire your peripherals in.
4. Cross-check anytime without hardware:

   ```sh
   cd examples/esp32-riscv
   cargo build --target riscv32imc-unknown-none-elf   # ESP32-C3/C6 class
   cargo build --target riscv32imac-unknown-none-elf
   ```

   Both targets build cleanly today (warble's core is atomics-free, so
   the `imc` target's lack of the A extension is not a problem); CI
   verifies both via `scripts/check-embedded.sh`.

## Tested against the host suite

The exact files above are also compiled into the main crate's host test
suite (`tests/esp32_examples.rs` at the repository root): the beacon's
samples are demodulated back to the expected APRS frame, the decoder is
fed transmitter-synthesized samples in odd-sized DMA-like chunks to
prove chunk-boundary correctness, and the digipeater is proven with
audio end to end. A `WIDE2-1` frame's samples go in and exactly one
relayed transmission comes out, its decoded path carrying the
documented H-bit/SSID mutation, with dupes suppressed inside the
window, fully-used paths ignored, and relays resuming after the window
expires.

## Why a library, not a binary

A bare-metal binary needs a panic handler, an entry point and a linker
script, all of which are owned by your HAL choice. Keeping this crate
a plain library means it cross-compiles with no board assumptions and
the same code runs under the host tests. See `src/lib.rs` for the full
note.
