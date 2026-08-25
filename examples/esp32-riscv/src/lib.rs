//! # yodel on ESP32-class RISC-V: copy-paste APRS examples
//!
//! Two pure, HAL-agnostic modules you can lift straight into an
//! ESP32-C3 / ESP32-C6 (riscv32imc/imac) firmware project:
//!
//! * [`beacon`] — **modulation**: build an APRS position report and fill
//!   a caller-provided `&mut [i16]` buffer with Bell 202 AFSK PCM, ready
//!   for an I2S or LEDC-PWM output peripheral.
//! * [`demod`] — **demodulation**: feed `i16` sample chunks (exactly as
//!   they arrive from an ADC / I2S DMA buffer) into a decoder and get
//!   FCS-valid AX.25/APRS frames back through a callback.
//! * [`digipeater`] — **store-and-forward relay**: the two halves glued
//!   to yodel's `digipeat` core — RX audio chunks in, WIDEn-N relay
//!   decision + dupe suppression, retransmission audio out into a
//!   caller buffer.
//!
//! ## Why this crate is a `lib`, not a `main.rs`
//!
//! A bare-metal *binary* needs a panic handler, an entry-point/runtime
//! crate (e.g. `riscv-rt` or `esp-hal`'s `#[main]`), and a linker script
//! — all of which are owned by YOUR HAL choice, not by yodel. Keeping
//! this crate a plain `#![no_std]` library means:
//!
//! * it cross-compiles cleanly for `riscv32imac-unknown-none-elf` and
//!   `riscv32imc-unknown-none-elf` with no board assumptions (this is
//!   verified in CI by `scripts/check-embedded.sh`);
//! * the exact same source files are compiled and executed by the host
//!   test suite (`tests/esp32_examples.rs` in the repository root), so
//!   the DSP logic you copy is *proven*, not decorative.
//!
//! To make firmware out of it: create your own binary crate with
//! `esp-hal` (or any HAL), copy `beacon.rs` / `demod.rs` in (or depend
//! on this crate by path), and wire the buffers to your peripherals at
//! the marked "YOUR HAL HERE" seams. Each module's file header
//! walks through every step; commented `esp-hal`-flavored sketches show
//! what the glue typically looks like.
//!
//! ## No floats, no heap
//!
//! ESP32-C3/C6 cores have **no FPU**: every `f32` operation would be
//! emulated in software, dozens of times slower than an integer op.
//! yodel's `i16` PCM paths (used exclusively here) are fixed-point
//! integer arithmetic end to end, so the modem runs at full speed on a
//! soft-float core. Nothing here allocates either — all buffers are
//! caller-provided or fixed-size const-generic, so the examples work
//! without `alloc` and without a heap.
#![no_std]
#![forbid(unsafe_code)]

pub mod beacon;
pub mod demod;
pub mod digipeater;
