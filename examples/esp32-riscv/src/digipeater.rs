//! DIGIPEATER: single-frequency WIDEn-N store-and-forward relay.
//!
//! # What this file does, start to finish
//!
//! 1. Feeds RX audio (`i16` sample chunks, exactly as they arrive from
//!    an ADC/I2S DMA buffer) into the same streaming decoder as
//!    [`demod`](crate::demod).
//! 2. For every FCS-valid UI frame, asks the LIBRARY's digipeater core
//!    — [`warble::digipeat::relay_decision`] — whether the frame's
//!    path addresses us: the served-alias table is our own callsign
//!    (exact match) plus a WIDEn-N policy with a max-n limit
//!    ([`warble::digipeat::WideLimit`], refuse `WIDE3+` floods).
//! 3. Suppresses duplicates with the library's
//!    [`warble::digipeat::DupeRing`]: the same transmission heard
//!    twice within the window (default 30 s) is relayed once. The ring
//!    is timestamped with **caller-supplied monotonic milliseconds** —
//!    see "YOUR TIMER HERE" below.
//! 4. On a `Relay` decision, rebuilds the frame with the mutated path
//!    (H bit set / SSID decremented, exactly what `relay_decision`
//!    returned) and synthesizes Bell 202 TX samples into a
//!    caller-provided buffer — the very same transmitter path
//!    ([`TncTransmitter::frame_samples_i16`]) that `beacon.rs` uses.
//!
//! No relay logic lives in this file: alias matching, the H-bit/SSID
//! mutation, loop prevention, and dupe fingerprinting are all the
//! library's tested `digipeat` module. This file is pure glue:
//! audio → frame → decision → audio.
//!
//! Everything is `no_std`, allocation-free, and integer-only.
//!
//! # The half-duplex transmit sequence (what YOUR firmware loop does)
//!
//! A single-frequency digipeater hears and talks on the SAME channel
//! with ONE radio, so it can never do both at once. When
//! [`Digipeater::feed`] hands you rendered TX samples:
//!
//! 1. **Wait for a clear channel.** Seam note: warble's demodulator
//!    does not expose a carrier-detect (DCD) signal today, so you have
//!    three options, crudest first:
//!      * **Just delay** a short random back-off (e.g. 200–800 ms)
//!        after the decode before keying — the station you just heard
//!        has finished by definition, and the jitter avoids two digis
//!        keying in lock-step. Fine for a first build.
//!      * **Energy threshold:** compute a running mean of `|sample|`
//!        over the last ~50 ms of RX audio; transmit only when it is
//!        near the idle-noise floor. Cheap, catches voice and other
//!        carriers, but needs a squelched radio or a calibrated
//!        threshold.
//!      * **"No decode in progress":** combine the energy gate with
//!        "no frame completed in the last N ms" from your own decode
//!        timestamps. Best of the three without real DCD.
//! 2. **Key PTT** (drive your PTT GPIO).
//! 3. **TXDelay:** the rendered samples already START with the
//!    configured preamble flags (default 32 flags ≈ 213 ms of flag
//!    tone at 1200 Bd) — that IS the TXDelay. Slow radios may need a
//!    little extra plain delay between keying and starting playback.
//! 4. **Play** the buffer at exactly [`SAMPLE_RATE_HZ`].
//! 5. **Unkey** PTT after the buffer (plus codec latency) drains.
//!
//! While transmitting, ignore RX audio (you would only hear
//! yourself); the dupe ring also protects you if your own
//! transmission leaks back in.
//!
//! # YOUR TIMER HERE — the monotonic-milliseconds seam
//!
//! The library has no clock. [`Digipeater::feed`] takes `now_ms`, a
//! monotonic millisecond count that must never go backwards (any
//! epoch: since boot is perfect). On ESP32-C3/C6 this is typically
//! `SystemTimer`/`esp_hal::time::now()` divided down to ms; on any
//! other HAL, a wrapping 64-bit tick counter works. Resolution is
//! uncritical — the dupe window is tens of seconds.

use warble::SampleRate;
use warble::ax25::UiFrame;
use warble::digipeat::{
    Alias, DupeRing, ExactAliasAction, Freshness, RelayDecision, WideLimit, relay_decision,
};
use warble::tnc::{MAX_FRAME_BYTES, RxFrame, TncConfig, TncTransmitter};
use warble::{ConfigError, ax25::Address};

use crate::demod::AprsDecoder;

/// The sample rate used throughout these examples (see `beacon.rs`).
pub const SAMPLE_RATE_HZ: u32 = 48_000;

/// Duplicate-suppression ring capacity: how many distinct recent
/// transmissions are remembered. 16 fingerprints (16 bytes each) is
/// plenty for a channel that carries a few frames per second at most.
pub const DUPE_SLOTS: usize = 16;

/// A comfortable upper bound for one relayed transmission at 48 kHz.
/// Same sizing math as `beacon.rs` (`MAX_BEACON_SAMPLES`), and a relay
/// never grows a frame by more than one 7-byte address.
pub const MAX_RELAY_SAMPLES: usize = 32_768;

/// Relay counters, for a debug console ("is this thing on?").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DigipeaterStats {
    /// FCS-valid frames heard.
    pub heard: u32,
    /// Frames relayed (TX samples rendered).
    pub relayed: u32,
    /// Relayable frames suppressed as duplicates.
    pub dupes: u32,
    /// Frames ignored by the relay decision (not for us, path spent,
    /// WIDE policy refusals, ...).
    pub ignored: u32,
    /// Relays dropped because rendering failed (TX buffer too small
    /// or the mutated frame did not fit — both should be unreachable
    /// with the documented buffer sizes).
    pub errors: u32,
}

/// A single-frequency WIDEn-N digipeater: RX audio chunks in, TX audio
/// buffers out.
///
/// Owns all of its state (no heap): construct it ONCE at startup —
/// like [`AprsDecoder`] it is a few KiB, so prefer a `static` cell over
/// the stack — then call [`Digipeater::feed`] with every DMA buffer
/// your ADC/I2S fills.
pub struct Digipeater {
    decoder: AprsDecoder,
    tx: TncTransmitter,
    dupes: DupeRing<DUPE_SLOTS>,
    my_call: Address,
    aliases: [Alias; 2],
    stats: DigipeaterStats,
}

impl Digipeater {
    /// Builds a digipeater serving `my_call` (exact match, e.g. frames
    /// addressed `via N0CALL-1`) plus `WIDE1-x`/... up to `wide_limit`
    /// (`WideLimit::TWO` is the customary wide-area policy; use
    /// `WideLimit::new(1)` for a home fill-in digi).
    ///
    /// `my_call` must be YOUR callsign — a digipeater transmits, and
    /// transmitting requires an amateur radio license (see the README's
    /// licensing note; N0CALL is a placeholder).
    ///
    /// # Errors
    ///
    /// A [`ConfigError`] only if the constants above were edited into
    /// an invalid combination (fewer than 2 samples per bit).
    pub fn new(my_call: Address, wide_limit: WideLimit) -> Result<Self, ConfigError> {
        let cfg = TncConfig::bell_202(SampleRate::new(SAMPLE_RATE_HZ)?)?;
        Ok(Self {
            decoder: AprsDecoder::new()?,
            tx: TncTransmitter::new(cfg),
            dupes: DupeRing::new(),
            my_call,
            aliases: [Alias::Exact(my_call), Alias::Wide(wide_limit)],
            stats: DigipeaterStats::default(),
        })
    }

    /// Pushes one chunk of RX samples; for every frame that should be
    /// relayed, renders the retransmission into `tx_buf` and hands the
    /// filled prefix to `on_relay`. Returns how many relays were
    /// rendered within this chunk.
    ///
    /// * `chunk` — RX audio, any length, straight from your ADC/I2S
    ///   DMA buffer (chunk boundaries are irrelevant, exactly as in
    ///   [`AprsDecoder::feed`]).
    /// * `now_ms` — YOUR monotonic millisecond clock (see the file
    ///   header); it timestamps the dupe ring.
    /// * `tx_buf` — the TX sample scratch buffer, ≥
    ///   [`MAX_RELAY_SAMPLES`]; on hardware, DMA-capable RAM.
    /// * `on_relay` — receives the rendered samples. This is where
    ///   your firmware runs the half-duplex sequence from the file
    ///   header: wait for clear channel, key PTT, play at
    ///   [`SAMPLE_RATE_HZ`], unkey. (Or copy the slice out and do that
    ///   from your main loop.)
    ///
    /// The relay decision and the dupe check are entirely the
    /// library's: [`relay_decision`] picks the first unused hop and
    /// returns the mutated path (H bit set; for `WIDEn-N` with N > 1,
    /// our callsign inserted used and the SSID decremented), and
    /// [`DupeRing::check_and_insert`] fingerprints source +
    /// destination + payload so the same transmission heard again
    /// within the window is not relayed twice.
    pub fn feed(
        &mut self,
        chunk: &[i16],
        now_ms: u64,
        tx_buf: &mut [i16],
        mut on_relay: impl FnMut(&[i16]),
    ) -> usize {
        // Split borrows up front so the decode callback can use the
        // rest of `self` while `decoder` is mutably borrowed.
        let tx = &self.tx;
        let dupes = &mut self.dupes;
        let stats = &mut self.stats;
        let my_call = self.my_call;
        let aliases = &self.aliases;

        let mut relays = 0;
        self.decoder.feed(chunk, |frame| {
            stats.heard = stats.heard.saturating_add(1);
            match relay_one(frame, tx, dupes, my_call, aliases, now_ms, tx_buf) {
                Outcome::Relayed(n) => {
                    stats.relayed = stats.relayed.saturating_add(1);
                    relays += 1;
                    if let Some(samples) = tx_buf.get(..n) {
                        on_relay(samples);
                    }
                }
                Outcome::Duplicate => stats.dupes = stats.dupes.saturating_add(1),
                Outcome::Ignored => stats.ignored = stats.ignored.saturating_add(1),
                Outcome::RenderFailed => stats.errors = stats.errors.saturating_add(1),
            }
        });
        relays
    }

    /// Relay counters (heard / relayed / dupes / ignored / errors).
    #[must_use]
    pub fn stats(&self) -> DigipeaterStats {
        self.stats
    }

    /// Receive-side statistics from the underlying decoder.
    #[must_use]
    pub fn rx_stats(&self) -> warble::tnc::TncStats {
        self.decoder.stats()
    }
}

/// Per-frame outcome, internal to [`Digipeater::feed`].
enum Outcome {
    /// Rendered this many TX samples into the buffer.
    Relayed(usize),
    Duplicate,
    Ignored,
    RenderFailed,
}

/// Decision + dupe check + rebuild + synthesis for ONE heard frame.
/// All policy comes from the library; this only wires the pieces.
fn relay_one(
    frame: &RxFrame<'_>,
    tx: &TncTransmitter,
    dupes: &mut DupeRing<DUPE_SLOTS>,
    my_call: Address,
    aliases: &[Alias],
    now_ms: u64,
    tx_buf: &mut [i16],
) -> Outcome {
    // --- 1. The library's relay decision -----------------------------
    // Collect the received per-hop H bits into a fixed array (the path
    // is at most MAX_DIGIPEATERS hops; `hops()` cannot yield more).
    use warble::ax25::frame::MAX_DIGIPEATERS;
    let mut heard = [warble::ax25::PathHop::unused(my_call); MAX_DIGIPEATERS];
    let mut hop_count = 0usize;
    for hop in frame.ui_frame().hops() {
        if let Some(slot) = heard.get_mut(hop_count) {
            *slot = hop;
            hop_count += 1;
        }
    }
    let path = heard.get(..hop_count).unwrap_or(&[]);

    // `ExactAliasAction::Keep`: a hop addressed literally to us keeps
    // our callsign in the path with the H bit set (no substitution
    // needed — it already names us).
    let decision = relay_decision(path, aliases, my_call, ExactAliasAction::Keep);
    let mutated = match decision {
        RelayDecision::Relay(p) => p,
        RelayDecision::Ignore(_) => return Outcome::Ignored,
    };

    // --- 2. The library's dupe check ----------------------------------
    // Fingerprint is src + dest + info (NOT the path: the same
    // transmission heard again via another digi is still a dupe).
    // Checked only after a positive decision so the ring holds only
    // transmissions we would relay.
    let src = frame.src();
    let dest = frame.dest();
    if dupes.check_and_insert(&src, &dest, frame.info(), now_ms) == Freshness::Duplicate {
        return Outcome::Duplicate;
    }

    // --- 3. Rebuild the frame with the mutated path -------------------
    // Same payload, same src/dest, new hop list — `with_hops` carries
    // the H bits onto the wire.
    let rebuilt = match UiFrame::with_hops(dest, src, mutated.hops(), frame.info()) {
        Ok(f) => f,
        Err(_) => return Outcome::RenderFailed,
    };
    let mut frame_buf = [0u8; MAX_FRAME_BYTES];
    let len = match rebuilt.build(&mut frame_buf) {
        Ok(len) => len,
        Err(_) => return Outcome::RenderFailed,
    };
    let Some(body) = frame_buf.get(..len) else {
        return Outcome::RenderFailed;
    };

    // --- 4. Synthesize TX samples -------------------------------------
    // The exact transmitter path beacon.rs uses: HDLC bits → NRZI →
    // Bell 202 AFSK, preamble/tail flags included, lazily drained into
    // the caller's buffer.
    let mut n = 0;
    for s in tx.frame_samples_i16(body) {
        match tx_buf.get_mut(n) {
            Some(slot) => *slot = s,
            None => return Outcome::RenderFailed,
        }
        n += 1;
    }
    Outcome::Relayed(n)
}

// ====================================================================
// YOUR HAL HERE — the full half-duplex station loop
// ====================================================================
//
// Everything above is pure DSP + the library's relay policy: `&[i16]`
// chunks in, `&[i16]` transmissions out. Typical esp-hal-flavored glue
// (COMMENTED ONLY — this crate compiles with no HAL dependency):
//
// ```ignore
// // main.rs of your esp-hal binary crate (ESP32-C3/C6):
// #![no_std]
// #![no_main]
//
// use esp_hal::main;
// use warble::ax25::Address;
// use warble::digipeat::WideLimit;
// use warble_esp32_riscv_examples::digipeater::{Digipeater, MAX_RELAY_SAMPLES};
//
// // TX buffer in a static: 64 KiB is too big for the default stack.
// static mut TX_PCM: [i16; MAX_RELAY_SAMPLES] = [0; MAX_RELAY_SAMPLES];
//
// #[main]
// fn main() -> ! {
//     let p = esp_hal::init(esp_hal::Config::default());
//     // let mut ptt = Output::new(p.GPIO4, Level::Low, ...);
//
//     let mut digi = Digipeater::new(
//         Address::new(b"N0CALL", 1).unwrap(), // YOUR callsign here
//         WideLimit::TWO,
//     ).unwrap();
//
//     let mut dma_buf = [0i16; 512];
//     loop {
//         // Block until the ADC/I2S peripheral filled the RX buffer:
//         // i2s_rx.read_words(&mut dma_buf).unwrap();
//
//         // YOUR TIMER HERE: monotonic ms since boot, e.g.
//         // let now_ms = esp_hal::time::now().duration_since_epoch().to_millis();
//         let now_ms: u64 = 0;
//
//         digi.feed(&dma_buf, now_ms, unsafe { &mut TX_PCM }, |samples| {
//             // Half-duplex sequence (see the file header):
//             // 1. wait for clear channel (delay / energy gate — the
//             //    demodulator exposes no DCD today);
//             // 2. ptt.set_high();  (+ extra delay for slow radios —
//             //    the samples already begin with the preamble flags);
//             // 3. play `samples` at exactly 48 kHz:
//             //    i2s_tx.write_words(samples).unwrap();
//             // 4. ptt.set_low();
//             let _ = samples;
//         });
//     }
// }
// ```
//
// Timing budget: decoding costs the same as demod.rs (~20.8 µs per
// sample at 48 kHz, comfortably met); the relay decision and dupe
// check are a few hundred integer ops per FRAME — free. Rendering a
// relay is the beacon.rs cost, and it happens while you are about to
// transmit anyway.
