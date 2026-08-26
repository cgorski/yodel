//! MODULATION: APRS position beacon → Bell 202 PCM samples.
//!
//! # What this file does, start to finish
//!
//! 1. Builds a typed APRS **position report** (latitude, longitude, map
//!    symbol, free-text comment) — no format strings, no manual byte
//!    fiddling; the typed constructors validate everything.
//! 2. Wraps it in an AX.25 UI frame (source callsign → destination
//!    "tocall", optional digipeater path) using small fixed stack
//!    buffers.
//! 3. Modulates the frame into **Bell 202 AFSK** (1200 Bd, 1200/2200 Hz
//!    tones) as `i16` PCM samples, written into a buffer YOU provide —
//!    typically the DMA buffer of your I2S or LEDC-PWM peripheral.
//!
//! Everything is `no_std`, allocation-free, and integer-only ("Why
//! `i16` and not `f32`?" — see the fixed-point note below).
//!
//! # Hardware in one paragraph (transmit side)
//!
//! The samples leave an I2S or PWM pin, pass an **RC low-pass** (e.g.
//! 4.7 kΩ + 10 nF ≈ 3.4 kHz cutoff, only needed for PWM) and a ~÷100
//! divider down to mic level, then AC-couple into the radio's mic
//! input. **PTT** is one plain GPIO: key it, wait the radio's TXDelay
//! — which yodel already models as `TncConfig`'s `preamble_flags`
//! (default 32 flags ≈ 213 ms of flag tone; raise it via
//! `TncConfig::with_flags` for slow radios) — play the audio, unkey.
//! Full schematic, example pin table, shopping list and gotchas: see
//! "Transmit path" under **Hardware guide** in [the sub-crate
//! README](../README.md).
//!
//! # Choosing a sample rate
//!
//! The examples use **48 000 Hz** because:
//!
//! * 48 000 / 1200 Bd = exactly **40 samples per bit** — an integer, so
//!   the bit clock never drifts against the sample clock;
//! * 48 kHz is natively supported by I2S codecs and is a comfortable
//!   rate for the ESP32-C3/C6 I2S and SAR-ADC peripherals.
//!
//! Any rate giving ≥ 2 samples per bit works ([`yodel::SampleRate`]
//! validates this): 9600, 11 025, 22 050, 44 100 Hz are all fine.
//! Rates that divide evenly by 1200 (9600, 12 000, 24 000, 48 000) are
//! the nicest; non-integer ratios (44 100/1200 = 36.75) still work —
//! the modulator's phase accumulator handles fractional samples per
//! bit — but integer ratios make buffer-size math exact.
//!
//! # Buffer sizing math (why `MAX_BEACON_SAMPLES` is what it is)
//!
//! At 1200 Bd, every bit costs `48_000 / 1200 = 40` samples. A frame is:
//!
//! * preamble flags (default 32 flags × 8 bits = 256 bits),
//! * the frame body: addresses (14–70 bytes) + control + PID + APRS
//!   info field + 2-byte FCS, bit-stuffed (worst case ×1.2),
//! * tail flags (default 2 flags × 8 bits = 16 bits).
//!
//! A short position beacon (~40-byte body ⇒ ~336 bits, ~403 stuffed
//! worst-case) totals well under 700 bits ⇒ under 28 000 samples.
//! [`MAX_BEACON_SAMPLES`] (32 768) leaves headroom; at 2 bytes per
//! sample that is a 64 KiB buffer — put it in a `static` (or DMA-capable
//! RAM section) rather than the stack on an ESP32-C3 (400 KiB SRAM).
//! If RAM is tight, don't pre-render at all: `TxI16Samples` is a *lazy*
//! iterator, so you can pull samples one at a time straight into a
//! small ping-pong DMA buffer (see the HAL sketch at the bottom).
//!
//! # Fixed point on a soft-float core (why `i16`, not `f32`)
//!
//! ESP32-C3/C6 (RV32IMC/IMAC) have no FPU: `f32` math is trapped into
//! software emulation, 20–50× slower than integer ops. yodel's
//! `transmit_i16` path is integer-only end to end (sine table + phase
//! accumulator), so this code runs at full native speed. The `i16`
//! range (±32767) is also exactly what I2S codecs expect, so no
//! conversion is needed at the output seam. (The RISC-V ESP32 parts
//! have no internal DAC; audio goes out over I2S or LEDC PWM.)

use yodel::SampleRate;
use yodel::aprs::{AprsPacket, Latitude, Longitude, Position, Symbol};
use yodel::ax25::Address;
use yodel::tnc::{MAX_FRAME_BYTES, TncConfig, TncError, TncTransmitter};

/// The sample rate used throughout these examples: 48 kHz. See the file
/// header for why (exactly 40 samples per bit at 1200 Bd).
pub const SAMPLE_RATE_HZ: u32 = 48_000;

/// Samples per bit at 48 kHz / 1200 Bd (exact).
pub const SAMPLES_PER_BIT: usize = (SAMPLE_RATE_HZ / 1_200) as usize;

/// A comfortable upper bound for one short position beacon at 48 kHz
/// (see the buffer sizing math in the file header). 64 KiB as `i16`s.
pub const MAX_BEACON_SAMPLES: usize = 32_768;

/// Errors from [`fill_position_beacon`]: either the modem layers
/// rejected the inputs, or the output buffer was too small.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeaconError {
    /// A yodel layer (APRS / AX.25 / DSP config) rejected the inputs.
    Tnc(TncError),
    /// `pcm_out` filled up before the transmission ended; the required
    /// capacity is at most [`MAX_BEACON_SAMPLES`] for short beacons.
    BufferTooSmall,
}

impl From<TncError> for BeaconError {
    fn from(e: TncError) -> Self {
        BeaconError::Tnc(e)
    }
}

/// Builds an APRS position beacon and renders it as Bell 202 PCM into
/// `pcm_out`, returning the number of samples written.
///
/// * `src` — your station callsign + SSID, e.g.
///   `Address::new(b"N0CALL", 9)` (N0CALL is a placeholder; use YOUR
///   callsign — transmitting requires an amateur radio license).
/// * `lat_hundredths_min` / `lon_hundredths_min` — position in signed
///   1/100 arc-minutes (north/east positive). This integer encoding
///   avoids all floating point: `degrees × 6000`. Example: 49.0583° N
///   is `49.0583 * 6000 ≈ 294_350`.
/// * `comment` — free text appended to the report (keep it short).
/// * `pcm_out` — your output buffer; on hardware, a DMA-capable buffer.
///
/// The samples are full-scale ±32767 `i16` at [`SAMPLE_RATE_HZ`]. Play
/// them out at exactly that rate and any APRS receiver in earshot will
/// decode the beacon.
///
/// # Errors
///
/// [`BeaconError::Tnc`] when a coordinate is out of range or the frame
/// does not fit its internal buffers; [`BeaconError::BufferTooSmall`]
/// when `pcm_out` cannot hold the whole transmission.
pub fn fill_position_beacon(
    src: Address,
    lat_hundredths_min: i64,
    lon_hundredths_min: i64,
    comment: &[u8],
    pcm_out: &mut [i16],
) -> Result<usize, BeaconError> {
    // --- 1. DSP configuration -------------------------------------
    // The Bell 202 preset: 1200 Bd, mark 1200 Hz / space 2200 Hz.
    // `SampleRate::new` rejects rates below 2 samples per bit, so both
    // constructors are infallible for 48 kHz — but stay explicit.
    let rate = SampleRate::new(SAMPLE_RATE_HZ).map_err(TncError::Config)?;
    let cfg = TncConfig::bell_202(rate).map_err(TncError::Config)?;
    let tx = TncTransmitter::new(cfg);

    // --- 2. The typed APRS payload --------------------------------
    // `Latitude`/`Longitude` validate the ±90°/±180° ranges at
    // construction; `Symbol::CAR` is the `/>` map glyph (pick any of
    // the named constants, e.g. Symbol::HOUSE, Symbol::JOGGER, ...).
    //
    // `from_hundredths_minute`, NOT `Latitude::new`: `new` counts
    // coordinate storage units, of which there are 57 138 900 000 to
    // the hundredth of an arc-minute. Handing it a hundredths count is
    // off by that factor and is still a legal latitude -- 294 350 units
    // is 8.6 micro-degrees, not 49.0583 degrees -- so nothing rejects
    // it and the beacon keys up from 0000.00N/00000.00W. This example
    // did exactly that. The constructor below takes the unit this
    // function documents, and it range-checks instead of overflowing.
    let packet = AprsPacket::Position(
        Position::new(
            Latitude::from_hundredths_minute(lat_hundredths_min)
                .map_err(|e| TncError::Aprs(e.into()))?,
            Longitude::from_hundredths_minute(lon_hundredths_min)
                .map_err(|e| TncError::Aprs(e.into()))?,
            Symbol::CAR,
        )
        .with_comment(comment),
    );

    // --- 3. Fixed working buffers ----------------------------------
    // The info field and full UI frame serialize into these stack
    // arrays. `MAX_FRAME_BYTES` is the AX.25 maximum, so any legal
    // frame fits. On a microcontroller these could be `static mut`-free
    // statics via a cell, but 660 stack bytes is fine.
    let mut info_buf = [0u8; MAX_FRAME_BYTES];
    let mut frame_buf = [0u8; MAX_FRAME_BYTES];

    // --- 4. Frame + modulate ---------------------------------------
    // `transmit_i16` serializes the packet (APRS info → UI frame →
    // HDLC bits → NRZI → AFSK) and returns a LAZY iterator: each
    // `next()` computes one sample from a phase accumulator. Nothing
    // is buffered internally — we drain it into the caller's buffer.
    let samples = tx.transmit_i16(
        &packet,
        // Destination "tocall": APRS product identifier, not a station.
        Address::new(b"APRS", 0).map_err(TncError::Ax25)?,
        src,
        // Digipeater path: empty here. For RF use you would typically
        // pass &[Address::new(b"WIDE1", 1)?] or similar.
        &[],
        &mut info_buf,
        &mut frame_buf,
    )?;

    // --- 5. Drain into the caller's buffer -------------------------
    let mut n = 0;
    for s in samples {
        match pcm_out.get_mut(n) {
            Some(slot) => *slot = s,
            None => return Err(BeaconError::BufferTooSmall),
        }
        n += 1;
    }
    Ok(n)
}

// ====================================================================
// YOUR HAL HERE — output seam
// ====================================================================
//
// Everything above is pure DSP: `&mut [i16]` in, sample count out.
// What you do with the samples is 100% your HAL's business. Typical
// esp-hal-flavored glue (COMMENTED ONLY — this crate compiles with no
// HAL dependency) looks like:
//
// ```ignore
// // main.rs of your esp-hal binary crate (ESP32-C3/C6):
// #![no_std]
// #![no_main]
//
// use esp_hal::{main, i2s::master::{I2s, Standard, DataFormat}};
// use yodel_esp32_riscv_examples::beacon;
//
// // 64 KiB PCM buffer in a static: too big for the default stack.
// static mut PCM: [i16; beacon::MAX_BEACON_SAMPLES] =
//     [0; beacon::MAX_BEACON_SAMPLES];
//
// #[main]
// fn main() -> ! {
//     let p = esp_hal::init(esp_hal::Config::default());
//
//     // 1. Key the transmitter: drive your PTT GPIO high and wait the
//     //    radio's TX-delay (often ~100–300 ms) before audio starts.
//     // let mut ptt = Output::new(p.GPIO4, Level::Low, ...);
//     // ptt.set_high(); delay.delay_millis(200);
//
//     // 2. Render the beacon (pure computation, no peripherals):
//     let n = beacon::fill_position_beacon(
//         yodel::ax25::Address::new(b"N0CALL", 9).unwrap(),
//         294_350,  //  49.0583° N in 1/100 arc-minutes
//         -432_175, // -72.0292° E (i.e. 72.0292° W)
//         b"yodel on ESP32-C6",
//         unsafe { &mut PCM },
//     ).unwrap();
//
//     // 3. Play the samples at EXACTLY 48 kHz. Options on C3/C6:
//     //    * I2S master TX to an external DAC/codec (best quality);
//     //    * LEDC PWM at a high carrier, updating the duty from a
//     //      48 kHz timer interrupt (crudest, needs an RC low-pass);
//     //    * boards without an external codec go the PWM route.
//     // i2s_tx.write_words(&PCM[..n]).unwrap();
//
//     // 4. Drop PTT after the buffer (plus codec latency) has drained.
//     // ptt.set_low();
//     loop {}
// }
// ```
//
// RAM-tight alternative: skip the big buffer entirely. Build the frame
// with `TncTransmitter::transmit_i16` yourself (the body of
// `fill_position_beacon` shows how) and pull the lazy iterator from a
// 48 kHz timer/DMA-refill interrupt, ~a few hundred samples at a time,
// into a small ping-pong buffer. The iterator holds only a phase
// accumulator, so per-sample cost is a table lookup and an add.
