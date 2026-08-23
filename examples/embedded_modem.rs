//! Allocation-free, no_std-style modem use with fixed buffers.
//!
//! * **Scenario** — not an application but an **API demonstration**: the
//!   exact calls an embedded user makes, with every buffer owned by the
//!   caller and no allocation on any warble path.
//! * **Hardware** — written for a `no_std` MCU with no allocator
//!   (ESP32-C3 class, Cortex-M). Runs on a host because the code is the
//!   same either way — that is the point being made.
//! * **Features** — `tnc`. For a whole firmware around this, see
//!   [`balloon_tracker_baremetal.rs`](balloon_tracker_baremetal.rs).
//!
//! Demonstrates the API an embedded user would call: no heap, no
//! growable buffers, no iterator adapters that collect. The transmit
//! side serializes an APRS frame into a caller-provided `[u8; N]` and
//! drains the lazy sample iterator into a fixed `[i16; N]` array (on
//! real hardware each sample would go straight to a DAC); the receive
//! side pushes those samples back one at a time and borrows the decoded
//! frame from the receiver's internal buffer.
//!
//! This is compiled as a normal host binary for convenience, but every
//! `warble` call below is available under `#![no_std]` without `alloc`
//! — the same feature set is cross-built for thumbv7em-none-eabihf and
//! riscv32imac-unknown-none-elf by `scripts/check-embedded.sh`.
//!
//! Run with:
//!
//! ```sh
//! cargo run --example embedded_modem --features tnc
//! ```

use warble::SampleRate;
use warble::aprs::{AprsPacket, Status};
use warble::ax25::Address;
use warble::tnc::{DefaultTncReceiver, MAX_FRAME_BYTES, TncConfig, TncReceiver, TncTransmitter};

/// Enough samples for one short UI frame at 48 kHz / 1200 baud
/// (~40 samples per bit, frame < 2000 bits).
const MAX_SAMPLES: usize = 80_000;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = TncConfig::bell_202(SampleRate::new(48_000)?)?;
    let tx = TncTransmitter::new(cfg);

    // --- Transmit: frame -> i16 samples into a fixed array ---------

    let packet = AprsPacket::Status(Status {
        text: b"embedded warble",
    });

    // All working storage is caller-provided, fixed-size, and could be
    // `static` on a microcontroller.
    // Caller-owned scratch: the library writes into these and never
    // allocates. `MAX_FRAME_BYTES` (330) is the AX.25 worst case, so a
    // frame buffer that size always fits. There is no `INFO_MAX` — an
    // information field can carry a caller-supplied comment of any
    // length — so size that one to your payload; an under-size buffer
    // returns `TncError` with the length needed rather than truncating.
    // Real flash-constrained firmware should shrink both to the traffic
    // it sends; see `examples/balloon_tracker_baremetal.rs`.
    let mut info_buf = [0u8; 64];
    let mut frame_buf = [0u8; MAX_FRAME_BYTES];
    let mut samples = [0i16; MAX_SAMPLES]; // PCM output

    // `transmit_i16` returns a lazy iterator: it computes one sample
    // per `next()`, holding only a phase accumulator — write each
    // sample to a DAC/PWM peripheral, or here into the fixed array.
    let sample_iter = tx.transmit_i16(
        &packet,
        Address::new(b"APRS", 0)?,
        Address::new(b"N0CALL", 7)?,
        &[], // no digipeater path
        &mut info_buf,
        &mut frame_buf,
    )?;
    let mut n = 0;
    for s in sample_iter {
        samples[n] = s; // on hardware: dac.write(s)
        n += 1;
    }
    println!("modulated {n} i16 samples into a fixed [i16; {MAX_SAMPLES}] buffer");

    // --- Receive: samples -> frame, one push at a time -------------

    // The receiver owns a fixed internal frame buffer (const-generic
    // capacity; `DefaultTncReceiver` uses the AX.25 maximum). Each
    // pushed sample may complete an FCS-valid frame, returned borrowed
    // — nothing is allocated on the receive path either.
    let mut rx: DefaultTncReceiver = TncReceiver::new(cfg)?;
    let mut decoded = 0;
    for &s in &samples[..n] {
        if let Some(frame) = rx.push_i16(s) {
            // `frame.info()` borrows from the receiver's buffer.
            println!(
                "decoded frame from {}: {} info bytes",
                core::str::from_utf8(frame.src().callsign.as_bytes())?,
                frame.info().len()
            );
            assert_eq!(frame.info(), b">embedded warble");
            decoded += 1;
        }
    }
    assert_eq!(decoded, 1, "round trip must recover exactly one frame");
    println!("fixed-buffer round trip complete: {decoded} frame recovered");
    Ok(())
}
