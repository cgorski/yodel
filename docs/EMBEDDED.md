# Embedded use

warble is written for microcontrollers first, and this guide covers the
three questions that follow from that: what the core guarantees, whether
a given chip can keep up, and how to decode continuously while the same
chip does everything else.

It assumes you are comfortable with interrupts, DMA and a fixed memory
budget, but not that you know APRS. For the protocol side, see the
[README](../README.md); for wiring a dev board to a radio, see the
[ESP32 hardware guide](../examples/esp32-riscv/README.md).

## What the core guarantees

Everything except `std`, `wav`, `cli`, `capture` and `async` is
`no_std`, and everything except those and `alloc` is allocation-free:
no heap, no growable buffers, builders write into caller-provided
`&mut [u8]`, parsers borrow from the input, and the transmit path is a
lazy iterator chain. The `i16` PCM path on both the modulator and
demodulator uses integer arithmetic only, so a floating-point unit is
never required.

`alloc` is the one to watch. It is `no_std` but opt-in and off by
default, and it adds heap conveniences such as `AprsPacket::to_vec` and
`TncTransmitter::transmit_to_vec_i16` that allocate and grow. No path in
this guide enables it, `scripts/check-embedded.sh` omits it from every
cross-built feature set, and `tests/no_alloc.rs` proves the core paths
at runtime with a counting global allocator.

`scripts/check-embedded.sh` cross-builds the full no_std feature matrix
for `riscv32imac-unknown-none-elf` and `thumbv7em-none-eabihf`, taking
each feature alone, the pairings that only mean something together such
as `g3ruh,demod` and `tnc,micE`, and then the combined
`mod,demod,nrzi,ax25,aprs,micE,kiss,tnc,g3ruh,fx25,il2p,wspr,ft8,m17,digipeat`
set. Install all three targets with `rustup target add` and run the
script to verify a checkout: `scripts/check-embedded.sh embedded` for
the cross-builds alone, or the default `all`, which adds a second pass
compiling the host test suite per feature set. It also cross-builds the detached ESP32 examples
sub-crate (below) for `riscv32imac-unknown-none-elf` and
`riscv32imc-unknown-none-elf`.

## APRS on ESP32-class RISC-V

[`examples/esp32-riscv/`](../examples/esp32-riscv/) is a detached
`#![no_std]` sub-crate with copy-paste APRS examples for ESP32-C3/C6
class riscv32 dev boards: `beacon.rs` renders a typed APRS position
report into a caller-provided `&mut [i16]` PCM buffer (modulation),
and `demod.rs` streams ADC/I2S-style `i16` sample chunks into the
receive chain and hands back decoded AX.25/APRS frames (demodulation).

The division of labour runs as follows. warble handles the DSP and
protocol work: APRS payloads, AX.25/HDLC framing, and Bell 202
modulation and demodulation, all in integer `i16` fixed point. The C3
and C6 cores have no FPU, so the `f32` paths would crawl on them. The
HAL glue is yours: ADC or I2S input, I2S or PWM output, and PTT
keying, at clearly marked "YOUR HAL HERE" seams with commented
`esp-hal`-flavored sketches. The sub-crate compiles with no HAL
dependencies, cross-builds for both riscv32 targets in CI, and has its
source files executed by the host test suite
(`tests/esp32_examples.rs`), so the code you copy is known to run.
Start with its [README](../examples/esp32-riscv/README.md), which includes
a **Hardware guide** covering interface circuits, PTT keying, example
pins and a shopping list for wiring a dev board to a handheld radio.

## Will it run on my chip? (ESP32 RISC-V feasibility)

Short answer: yes, for 1200-baud AFSK and 9600-baud G3RUH on every
riscv32 ESP32 part listed below. One caveat applies throughout:
**no on-device number is claimed as verified**. The budgets are
extrapolated from a host benchmark you can re-run yourself
([`examples/throughput.rs`](../examples/throughput.rs)):

```sh
cargo run --release --example throughput --features tnc,g3ruh,fx25
```

The derivation is in
[docs/BENCHMARKS.md](BENCHMARKS.md#embedded-cost-throughput-cycle-budget-and-ram):
measured host throughput, the rv32 cycle arithmetic, chunk-size budgets
and RAM footprints.
See also the [ESP32 hardware guide](../examples/esp32-riscv/README.md).

### Guidance table

Each row names the recommended [`DevicePreset`] variant or variants.
`DevicePreset` is a public `no_std` enum whose `tnc_config()` resolves
at compile time to the configuration described; see the example below
the table.

| chip | spec (grounded) | verdict per mode | recommended preset |
|---|---|---|---|
| ESP32-C3 | single-core RV32IMC, 160 MHz, no FPU | 1200 AFSK: **comfortable single-chain, workable full-bank**. A single chain (`SpaceGainSweep::UNITY`) is ESTIMATED at ~12% of the core, the full 11-chain default at ~40%. 9600 G3RUH: **comfortable** (~4% ESTIMATED). +FX.25: fine, though mind the per-frame RS spike. All **unconfirmed without on-device measurement**. | `DevicePreset::Esp32C3` (conservative single-chain) when you need the headroom; `DevicePreset::Esp32C3FullBank` for the full diversity bank, at ~3.4× the CPU |
| ESP32-C6 | 160 MHz, no FPU | Same as C3. | `DevicePreset::Esp32C6` / `DevicePreset::Esp32C6FullBank` |
| ESP32-H2 | 96 MHz, no FPU | Tighter, and this is the chip where the choice matters most: single-chain 1200 AFSK is ESTIMATED at ~20% of the core, the full 11-chain bank at ~66%. The conservative preset is cheap here; the full bank is the one to budget carefully, or drop the sample rate to 24 kHz, which halves both. 9600 G3RUH is easy (~6% ESTIMATED). Unconfirmed without on-device measurement. | `DevicePreset::Esp32H2` (conservative-only: no full-bank variant exists for this chip) |
| ESP32-P4 | dual-core, 400 MHz, with FPU | **Everything comfortable** (8333 cycles/sample available; single chain ~5% and even the full bank ~16% of one core, ESTIMATED), and the FPU makes the `_f32` twins viable too. | `DevicePreset::Esp32P4` (full bank); `DevicePreset::Esp32P4G3ruh` for 9600 G3RUH (`g3ruh` feature) |
| classic ESP32 / S2 / S3 | Xtensa, not RISC-V | Out of scope of the riscv32 examples, but the portable `no_std` integer core applies unchanged; budgets need their own measurement. | none; measure first, then set the parameters by hand via `TncConfig` |

### Device presets: one enum to a running decoder

Pick your chip's variant and `tnc_config()` returns the validated
configuration behind its table row: mode, chain bank, 48 kHz rate and
the fixed-point `i16` path. It is a `const fn`, works in `no_std`, and
allocates nothing. `description()` and `expected_cpu()` return the same
text as the table, with the MEASURED and ESTIMATED labels intact.
Wiring on hardware is covered in the
[ESP32 hardware guide](../examples/esp32-riscv/README.md).

```rust
# #[cfg(feature = "tnc")]
# fn main() -> Result<(), warble::ConfigError> {
use warble::DevicePreset;
use warble::tnc::DefaultTncReceiver;

// ESP32-C3, conservative: 1200-baud AFSK, single balanced chain.
// Expected CPU: ~390 ESTIMATED rv32 cycles/sample — ~12% of the 48 kHz
// budget at 160 MHz (unconfirmed without on-device measurement).
let config = DevicePreset::Esp32C3.tnc_config()?;
let mut rx = DefaultTncReceiver::new(config).unwrap();
// On hardware: feed ADC/radio i16 samples; frames pop out decoded.
// for sample in adc_samples {
//     if let Some(frame) = rx.push_i16(sample) { /* frame.aprs() … */ }
// }
# let _ = &mut rx;
# Ok(())
# }
# #[cfg(not(feature = "tnc"))]
# fn main() {}
```

Swap the variant to move along the table. `Esp32C3FullBank` and
`Esp32C6FullBank` trade about **3.4×** the CPU for the full 11-chain
diversity bank (~40% ESTIMATED at 160 MHz, against ~12% for the
conservative variant). `Esp32H2` stays conservative-only at 96 MHz
(~20% ESTIMATED). `Esp32P4` affords the full bank (~16% of one 400 MHz
core, ESTIMATED), and `Esp32P4G3ruh`, with the `g3ruh` feature, runs
9600 G3RUH on the P4's budget. Every variant documents its
expected-CPU note, quoting the figures above with their MEASURED and
ESTIMATED labels.

**The two variants now differ enough to be worth choosing between.**
Earlier revisions of this section advised the full bank everywhere,
because 11 chains cost only about 1.4× one: the receiver built all
three correlator banks whatever the sweep length and discarded the
unused ones, so a single chain paid for the whole front end regardless.
The receiver now skips banks that no active chain reads, which made the
conservative variants **2.5× cheaper** and moved the ratio to about
3.4×. Take the full bank when you want its emphasis and tilt diversity
and can afford ~40% of a 160 MHz core; take the conservative variant
when you want the core back for other work. On the 96 MHz H2 the
conservative variant now costs ~20% rather than the ~49% it used to.

**On-device confirmation method.** Do this before trusting any
ESTIMATED row. Wrap the sample-feed loop in the RISC-V cycle-counter
CSR (`mcycle`, or `esp-hal`'s `SystemTimer`): feed a buffer of N
synthesized samples through `push_i16`, read the counter before and
after, and divide. Alternatively, toggle a GPIO around the loop and
measure the high time on a scope. Compare the result against the
per-sample budget in
[docs/BENCHMARKS.md](BENCHMARKS.md#embedded-cost-throughput-cycle-budget-and-ram).

## Sharing the MCU

The feasibility numbers say the decode fits. This section addresses the
harder beginner question: **how do I decode continuously while my one
small chip also does everything else?**

Start with the case where the question does not arise. **On an
operating system you do not have this problem.** If your "tracker
computer" is a Raspberry Pi or any Linux box, you never share the MCU
by hand, because the kernel preempts threads for you. Give each duty
its own `std::thread`, wire them together with `std::sync::mpsc`
channels, and blocking reads are fine:

```rust,ignore
use warble::wav::{decode_sniffed, sniff_pcm};

// Decode thread: the CLI's own stdin/WAV sniffing intake, frames out
// over a plain channel. Sensor and TX-scheduler threads run alongside;
// the OS schedules all three.
std::thread::spawn(move || {
    let sniffed = sniff_pcm(std::io::stdin().lock(), Some(rate))?;
    decode_sniffed(sniffed, |frame| frame_tx.send(frame).is_ok())
});
```

[`examples/balloon_tracker.rs`](../examples/balloon_tracker.rs) is that
path in full: a decode thread, a simulated sensor thread, a periodic
beacon-TX thread, and a self-demo mode that synthesizes and decodes its
own beacon so it runs with no input at all
(`cargo run --example balloon_tracker --features tnc,wav`). Its header
explains why threads and channels suit a std newcomer better than
async.

The rest of this section is for **when the modem shares one chip**: no
OS, one core, and every duty hand-scheduled. Two user stories follow,
walked end to end, then the three embedded concurrency styles that wrap
the same core.

The underlying pattern does not change, because
`TncReceiver::push_i16` is a pure per-sample state machine with no
allocation, no blocking and no callbacks. *You* decide when decoding
runs and how much of it runs:

1. **Interrupt side**: the ADC/DMA interrupt fires with a fresh buffer
   of `i16` samples. It does one cheap thing: push them into a
   [`SampleRing`](../src/ring.rs) (a const-generic, allocation-free
   FIFO). The crate is `forbid(unsafe_code)`, so the ring takes
   `&mut self` on both sides and you wrap it in your platform's
   critical-section mutex. The [`SampleRing`] rustdoc shows that
   arrangement.
2. **Main side**: whenever it gets a turn, the main loop or task pops a
   *bounded* chunk (say 128 samples) from the ring and feeds it through
   `push_i16`. A bounded chunk in means a bounded time out, and the CPU
   is then yours again for sensors, logging and TX.
3. If the ring overflows, the newest samples are dropped and
   **counted** (`SampleRing::take_overruns`) rather than spliced, so
   you can detect an undersized ring instead of decoding garbage.

### The bounded-latency contract

One configuration detail matters before any of this is real-time-safe.
The **default** receiver config trades latency for sensitivity. When a
*corrupted* frame closes, an FCS-failure repair sweep re-destuffs the
raw bit window once per candidate flipped bit, which is
O(content_bits²) over a window of up to 4096 bits, and it can run once
per active chain plus once more in the cross-chain voting path. Worst
case for a single frame-close event is on the order of **1.5–2.5 G rv32
cycles ≈ 10–15 seconds at 160 MHz (ESTIMATED)**, absorbed inside the one
`push_i16` call that delivers the closing flag. That is tolerable on a
workstation and unacceptable on an MCU, where it means a duty missed by
seconds.

`TncConfig::bounded_latency()` removes the sweep entirely by setting
`RecoveryPolicy::None` and `ChainVoting::Off`, so **no repair path can
run** and the worst case of a single push is small and known. The cost
is that corrupted frames are rejected, and counted in
`TncStats::fcs_errors`, instead of possibly repaired; clean-signal
decode is unaffected. FX.25, if enabled, still bursts at frame close
for its Reed-Solomon decode, at ~0.13–0.3 M rv32 cycles ≈ **0.8–1.9 ms
at 160 MHz (ESTIMATED)**, which a few ms of ring headroom absorbs.
Every embedded example below uses `bounded_latency()`.

### Two shapes, and the examples that run them

Both common questions have the same answer, and each has a runnable
example behind it:

* *"Decode continuously while also reading sensors, logging, and
  beaconing."* This is the superloop below.
  [`examples/balloon_tracker_baremetal.rs`](../examples/balloon_tracker_baremetal.rs)
  is the complete version, with simulated DMA intake, a real modulator
  synthesizing the beacon, and a measured duty-cycle report
  (`cargo run --example balloon_tracker_baremetal --features tnc`).
* *"Digipeat on the same chip that is decoding."* This is the same loop
  with a relay decision in the frame handler.
  [`examples/digipeater_station.rs`](../examples/digipeater_station.rs) and
  [`examples/esp32-riscv/src/digipeater.rs`](../examples/esp32-riscv/) are
  the runnable versions.

The shape is the same either way. The ISR fills a ring, and the loop
drains **one bounded chunk** per pass and decodes outside the lock:

```rust,ignore
use warble::ring::SampleRing;
use warble::tnc::{DefaultTncReceiver, TncConfig, TncReceiver};

let cfg = TncConfig::bell_202(SampleRate::new(24_000)?)?.bounded_latency();
let mut rx: DefaultTncReceiver = TncReceiver::new(cfg)?;
// Shared with the ADC/DMA ISR behind a critical-section mutex:
// static RING: Mutex<RefCell<SampleRing<1024>>> = ...;

loop {
    // ISR side (not shown): ring.push_slice(dma_half_buffer);

    // Decode duty: drain ONE bounded chunk; decode outside the lock.
    let mut chunk = [0i16; 128];
    let n = critical_section::with(|cs| RING.borrow_ref_mut(cs).pop_slice(&mut chunk));
    for &s in &chunk[..n] {
        if let Some(frame) = rx.push_i16(s) {
            // frame.aprs(), frame.info(), ... — handle and move on.
        }
    }

    // The other duties, scheduled off a millis counter:
    // read sensors / write log / key the beacon via transmit_i16.
}
```

At 24 kHz, real time delivers 128 samples every ~5.3 ms, and decoding
them costs a small fraction of that on a 160 MHz rv32 core. The
chunk-size budget in
[docs/BENCHMARKS.md](BENCHMARKS.md#embedded-cost-throughput-cycle-budget-and-ram)
gives the ESTIMATED headroom per chip and per chain count.

### Path (a): the bare-metal superloop

No dependencies and no frameworks: the snippet above *is* the whole
architecture. One free-running loop, a millis counter for scheduling,
and the ISR/ring handoff for intake. It is the simplest thing that
works, and by the chunk-size budget in
[docs/BENCHMARKS.md](BENCHMARKS.md#embedded-cost-throughput-cycle-budget-and-ram)
it has ample headroom for this workload. Full example:
[`examples/balloon_tracker_baremetal.rs`](../examples/balloon_tracker_baremetal.rs).

### Path (b): embassy tasks (`--features embassy`)

The opt-in `embassy` feature (off by default, still `no_std`, only
dependency `embassy-time`) ships the async glue in
[`src/embassy.rs`](../src/embassy.rs) so each duty becomes its own task:
`run_decoder` drains your `SampleSource` (the seam to the DMA/ADC-fed
ring) through the receiver in bounded chunks, yielding between chunks
so sibling tasks always get a turn; `TxTicker` schedules the beacon.

```rust,ignore
use warble::embassy::{SampleSource, TxTicker, run_decoder};

// Decode task: bounded chunks, cooperative yield between them.
let mut chunk = [0i16; 128];
run_decoder(&mut source, &mut rx, &mut chunk, |frame| {
    // handle each FCS-valid frame
}).await;

// Telemetry task:
let mut tick = TxTicker::every(embassy_time::Duration::from_secs(30));
loop {
    tick.ready().await;
    // build + key the beacon via transmit_i16
}
```

Full example (intake + decode + sensors + TX as four tasks, runnable
on a host):
[`examples/balloon_tracker_embassy.rs`](../examples/balloon_tracker_embassy.rs)
(`cargo run --example balloon_tracker_embassy --features embassy`).

### Path (c): RTIC tasks (example-only)

RTIC composes with the core with **zero glue code**. Its scheduler,
resource locks and priorities all come from the `#[rtic::app]` macro,
and warble's role reduces to types placed in resources. There is
therefore no `rtic` cargo feature; the reasoning is in
[docs/ARCHITECTURE.md](ARCHITECTURE.md) §"The rtic verdict".
The shape, with explicit priorities:

```text
#[shared] ring: SampleRing<1024>,     // ISR -> decode handoff
#[local]  rx: DefaultTncReceiver,     // decode task owns it

#[task(binds = DMA_CH0, priority = 3, shared = [ring])]  // intake: highest
#[task(priority = 2, shared = [ring], local = [rx])]     // decode: bounded chunks
#[task(priority = 1, ...)]                               // beacon TX
// sensors & housekeeping run in idle
```

[`examples/balloon_tracker_rtic.rs`](../examples/balloon_tracker_rtic.rs)
shows the full RTIC 2 app skeleton in its header and compiles/runs the
actual task bodies under a priority-ordered mock dispatcher on the
host, with a worst-case-latency table
(`cargo run --example balloon_tracker_rtic --features tnc`).

### Choosing between them

The decode core is **identical** in all four: the same bounded
`push_i16` chunks and the same `bounded_latency()` contract. The choice
is about the scaffolding around the modem.

If you have an operating system, use it. **std threads** on a Pi make
the question disappear, and the kernel's scheduler outperforms anything
below it here.

On one bare chip, the **superloop** has no dependencies and is the
simplest to reason about; given this workload's headroom (see the
chunk-size table), it is a sound choice rather than a compromise.
**embassy** offers the best ergonomics, since each duty reads as
straight-line async code, and it brings the `embassy-time` timer
ecosystem; it is also mainstream on ESP32 through `esp-hal`. **RTIC**
gives the strongest real-time guarantees, through priority-based
preemption with static analysis of resource contention, which suits a
system where some ISR has a hard deadline that must preempt decoding.

Start with the superloop. Move to embassy when the task count makes the
superloop unwieldy, and to RTIC when a deadline has to be provable
rather than observed to hold.
