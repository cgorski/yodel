//! DEMODULATION: ADC/I2S `i16` sample chunks → decoded AX.25/APRS frames.
//!
//! # What this file does, start to finish
//!
//! 1. Wraps warble's [`TncReceiver`] — the full Bell 202 receive chain
//!    (tone discriminator → parallel slicer bank → NRZI decode → HDLC
//!    deframe → FCS check) — in a small [`AprsDecoder`] struct sized for
//!    a microcontroller.
//! 2. Accepts incoming samples in **chunks of any size**, exactly as
//!    they arrive from an ADC or I2S DMA buffer. Chunk boundaries are
//!    irrelevant: the receiver is a push-one-sample state machine, so a
//!    frame spanning two (or ten) DMA buffers decodes identically.
//! 3. Hands every FCS-valid frame to your callback as source/destination
//!    callsigns plus the raw APRS information field, and offers
//!    [`parse_aprs`] to lift that payload into warble's typed
//!    [`AprsPacket`] representation.
//!
//! Everything is `no_std`, allocation-free, and integer-only.
//!
//! # Hardware in one paragraph (receive side)
//!
//! RX audio comes from the radio's speaker/headphone jack (or data
//! port), **attenuated** through a divider (e.g. 10 kΩ / 1 kΩ),
//! **AC-coupled** through a ~100 nF cap, and **re-biased** to 3.3 V/2
//! by two equal resistors so it sits mid-range on the ADC pin; an I2S
//! MEMS mic or codec module is the cleaner, circuit-free alternative.
//! Either way, remove the ADC's DC offset before feeding samples in
//! (see the centering snippet below) — the demodulator cares about the
//! offset, not the absolute level. Full schematic, example pin table,
//! shopping list and gotchas: see "Receive path" under **Hardware
//! guide** in [the sub-crate README](../README.md).
//!
//! # Sample rate and input scaling
//!
//! The decoder is built for **48 000 Hz** to match `beacon.rs` (exactly
//! 40 samples per bit at 1200 Bd; any rate with ≥ 2 samples per bit
//! works — just build the config with your rate and resample nothing).
//! Input samples are `i16`. If your ADC gives unsigned 12-bit values
//! (ESP32-C3 SAR ADC: 0..=4095), center and scale them first:
//!
//! ```ignore
//! let centered = (raw as i32 - 2048) << 4; // 0..4095 -> roughly ±32768
//! let sample = centered.clamp(-32768, 32767) as i16;
//! ```
//!
//! Exact scaling is uncritical — the discriminator compares tone
//! energies against each other, not against an absolute level — but
//! removing the DC offset matters, and more bits of signal means more
//! noise margin.
//!
//! # Memory: the const-generic frame buffer
//!
//! [`TncReceiver<N>`] holds one `N`-byte frame buffer **per decision
//! chain** (the parallel bank that lets the decoder handle
//! pre-/de-emphasized audio). [`AprsDecoder`] uses [`RX_FRAME_BYTES`]
//! (330, the AX.25 maximum: 10 addresses × 7 bytes + control + PID +
//! 256-byte info field + FCS slack), which costs ~3.6 KiB total —
//! nothing next to the C3's 400 KiB SRAM. If you only expect short
//! beacons you can shrink `N`, or swap the receiver's sweep for
//! `SpaceGainSweep::UNITY` (a single chain) to cut both RAM and CPU;
//! see `warble::tnc::TncConfig::with_space_gain_sweep`.
//!
//! # Fixed point on a soft-float core
//!
//! Same story as `beacon.rs`: the C3/C6 have no FPU, so this module
//! only uses `push_i16` — warble's integer receive path (fixed-point
//! correlators and one-pole filters). Never route your samples through
//! `f32` on these cores.

use warble::aprs::{AprsError, AprsPacket};
use warble::tnc::{RxFrame, TncConfig, TncReceiver};
use warble::{ConfigError, SampleRate};

/// The sample rate the decoder is configured for (matches `beacon.rs`).
pub const SAMPLE_RATE_HZ: u32 = 48_000;

/// Receive frame-buffer capacity in bytes: the AX.25 maximum
/// (`warble::tnc::MAX_FRAME_BYTES`). See the memory note in the file
/// header for shrinking it.
pub const RX_FRAME_BYTES: usize = 330;

/// A streaming APRS decoder: feed it `i16` sample chunks, get frames.
///
/// Owns all of its state (no heap): construct it ONCE at startup — it
/// is a few KiB, so prefer a `static` cell over the stack — then call
/// [`AprsDecoder::feed`] with every DMA buffer your ADC/I2S fills.
pub struct AprsDecoder {
    rx: TncReceiver<RX_FRAME_BYTES>,
}

impl AprsDecoder {
    /// Builds a decoder for Bell 202 at [`SAMPLE_RATE_HZ`].
    ///
    /// # Errors
    ///
    /// A [`ConfigError`] only if the constants above were edited into
    /// an invalid combination (fewer than 2 samples per bit).
    pub fn new() -> Result<Self, ConfigError> {
        let cfg = TncConfig::bell_202(SampleRate::new(SAMPLE_RATE_HZ)?)?;
        Ok(Self {
            rx: TncReceiver::new(cfg)?,
        })
    }

    /// Pushes one chunk of samples; invokes `on_frame` once per decoded,
    /// FCS-valid AX.25 UI frame. Returns how many frames completed
    /// within this chunk.
    ///
    /// The chunk can be ANY length — one sample, a 512-sample DMA
    /// half-buffer, a whole recording. The receiver keeps its bit-clock
    /// and HDLC state across calls, so frames that straddle chunk
    /// boundaries decode exactly like contiguous ones (the host test
    /// suite proves this by feeding odd-sized chunks).
    ///
    /// The [`RxFrame`] passed to the callback BORROWS the receiver's
    /// internal buffer: copy out whatever you need inside the callback
    /// (callsigns are small `Address` values; `frame.info()` is the
    /// raw APRS payload slice).
    pub fn feed(&mut self, chunk: &[i16], mut on_frame: impl FnMut(&RxFrame<'_>)) -> usize {
        let mut frames = 0;
        for &sample in chunk {
            // One sample in, at most one completed frame out. Bad
            // frames (FCS mismatch, oversize, unparseable) never panic
            // and never error out here — they are tallied in
            // `self.stats()` and yield no frame.
            if let Some(frame) = self.rx.push_i16(sample) {
                frames += 1;
                on_frame(&frame);
            }
        }
        frames
    }

    /// Receive statistics: accepted frames, FCS errors, oversizes.
    /// Useful for a debug console ("am I hearing anything at all?").
    #[must_use]
    pub fn stats(&self) -> warble::tnc::TncStats {
        self.rx.stats()
    }
}

/// Parses a decoded frame's information field as a typed APRS packet
/// (position, status, message, ...). Small helper so the callback side
/// stays one line.
///
/// # Errors
///
/// The [`AprsError`] variants of [`AprsPacket::parse`] when the payload
/// is not well-formed APRS — common on real RF, so treat a parse error
/// as "show the raw bytes instead", never as fatal.
pub fn parse_aprs<'a>(frame: &RxFrame<'a>) -> Result<AprsPacket<'a>, AprsError> {
    frame.aprs()
}

// ====================================================================
// YOUR HAL HERE — input seam
// ====================================================================
//
// Everything above is pure DSP: `&[i16]` chunks in, frames out. Getting
// audio into those chunks is your HAL's business. Typical
// esp-hal-flavored glue (COMMENTED ONLY — this crate compiles with no
// HAL dependency):
//
// ```ignore
// // main.rs of your esp-hal binary crate (ESP32-C3/C6):
// #![no_std]
// #![no_main]
//
// use esp_hal::main;
// use warble_esp32_riscv_examples::demod::{AprsDecoder, parse_aprs};
//
// #[main]
// fn main() -> ! {
//     let p = esp_hal::init(esp_hal::Config::default());
//
//     // Audio in, two common options on C3/C6:
//     //  * I2S master RX from a digital MEMS mic or codec at 48 kHz —
//     //    cleanest; you already get signed 16/24-bit samples.
//     //  * SAR ADC + timer at 48 kHz sampling a biased (VCC/2) and
//     //    AC-coupled radio speaker output — center & scale as in the
//     //    file header. 48 kHz continuous ADC is achievable via the
//     //    DMA-driven `adc` continuous mode.
//
//     let mut decoder = AprsDecoder::new().unwrap();
//     let mut dma_buf = [0i16; 512]; // one DMA half-buffer of audio
//
//     loop {
//         // Block until the peripheral filled the buffer:
//         // i2s_rx.read_words(&mut dma_buf).unwrap();
//
//         decoder.feed(&dma_buf, |frame| {
//             // Copy out what you need INSIDE the callback — the frame
//             // borrows the decoder's buffer until the next sample.
//             // defmt::info!("from {}", frame.src().callsign.as_bytes());
//             match parse_aprs(frame) {
//                 Ok(_packet) => { /* typed position/status/... */ }
//                 Err(_) => { /* non-APRS payload: log frame.info() */ }
//             }
//         });
//     }
// }
// ```
//
// Timing budget: at 48 kHz the decoder must average under ~20.8 µs per
// sample. warble's integer chain runs each correlator once per sample
// plus a handful of multiply-adds per decision chain — comfortably
// inside budget on a 160 MHz C3/C6, and you can halve the work again
// with `SpaceGainSweep::UNITY` if needed.
