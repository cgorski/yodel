//! RTIC balloon tracker: the exact task/resource/priority structure of
//! an RTIC 2 flight computer.
//!
//! * **Scenario** — the flight computer *inside the payload*: telemetry
//!   beacons out, decode of what the receiver hears. Not the ground
//!   station.
//! * **Hardware** — a `no_std` MCU running **RTIC 2**, which needs
//!   hardware interrupt priorities: Cortex-M (nRF52, STM32, RP2040).
//!   RTIC schedules preemptively off the NVIC rather than
//!   cooperatively, which is the reason to choose it over Embassy — a
//!   high-priority ADC task cannot be delayed by a slow logging task.
//! * **Runs here** — on a host: the RTIC `#[app]` skeleton is in
//!   comments (it needs a real interrupt controller) while the task
//!   *bodies* and the shared-resource discipline are real code that
//!   self-checks.
//! * **Features** — `tnc`.
//!
//! For cooperative scheduling instead, see
//! [`_embassy`](balloon_tracker_embassy.rs); for neither, see
//! [`_baremetal`](balloon_tracker_baremetal.rs).
//!
//! Run on a host:
//!
//! ```sh
//! cargo run --example balloon_tracker_rtic --features tnc
//! ```
//!
//! # What is mocked, and why
//!
//! A real RTIC app is macro-generated around a device PAC
//! (`#[rtic::app(device = esp32c3, ...)]`) and its hardware interrupt
//! vectors: it cannot compile as a host example, and pinning one
//! vendor's PAC into this crate's example set would tie the example to
//! one chip. So this file is a **structural mock**: the RTIC app
//! skeleton is shown verbatim in the comment block below, while the
//! *logic each task would run* — the shared-resource shapes, the ISR
//! push, the bounded chunk drain, the idle duties — is expressed with
//! plain types and plain functions so it compiles, runs, and
//! self-checks on the host. Nothing warble-related is mocked: every
//! `SampleRing`/`TncReceiver`/`TncTransmitter` call is the real one.
//! Only RTIC's scheduler is simulated (a loop that dispatches
//! "interrupts" in priority order), and the resource lock is `&mut`
//! borrows — which is precisely what `rtic::Mutex::lock` hands the
//! closure on a target.
//!
//! # The real RTIC 2 app this mocks (esp32c3 flavour, abridged)
//!
//! ```text
//! #[rtic::app(device = esp32c3, dispatchers = [FROM_CPU_INTR0, FROM_CPU_INTR1])]
//! mod app {
//!     use warble::ring::SampleRing;
//!     use warble::tnc::{DefaultTncReceiver, TncReceiver, TncTransmitter, TncConfig};
//!
//!     #[shared]
//!     struct Shared {
//!         ring: SampleRing<1024>,          // ISR -> decode handoff
//!         altitude_m: u32,                 // sensors -> beacon
//!     }
//!
//!     #[local]
//!     struct Local {
//!         rx: DefaultTncReceiver,          // decode task owns it
//!         beacon_tx: TncTransmitter,       // beacon task owns it
//!     }
//!
//!     /// ADC/DMA half-complete interrupt — HIGHEST priority (3).
//!     /// Push the fresh half-buffer; O(len), never blocks.
//!     #[task(binds = DMA_CH0, priority = 3, shared = [ring])]
//!     fn adc_dma(mut cx: adc_dma::Context) {
//!         let half: &[i16] = /* the DMA half-buffer */;
//!         cx.shared.ring.lock(|ring| { ring.push_slice(half); });
//!     }
//!
//!     /// Decode software task — MID priority (2). Drains ONE bounded
//!     /// chunk under the lock, decodes OUTSIDE it, then re-spawns
//!     /// itself while samples remain.
//!     #[task(priority = 2, shared = [ring], local = [rx])]
//!     async fn decode(mut cx: decode::Context) {
//!         let mut chunk = [0i16; 128];
//!         let n = cx.shared.ring.lock(|ring| ring.pop_slice(&mut chunk));
//!         for &s in &chunk[..n] {
//!             if let Some(frame) = cx.local.rx.push_i16(s) { /* handle */ }
//!         }
//!     }
//!
//!     /// Telemetry beacon — LOW priority (1), monotonic-scheduled
//!     /// every 2 s; preempted freely by adc_dma and decode.
//!     #[task(priority = 1, shared = [altitude_m], local = [beacon_tx])]
//!     async fn beacon(mut cx: beacon::Context) { /* transmit_i16 -> DAC DMA */ }
//!
//!     /// Sensor housekeeping in idle — effectively priority 0.
//!     #[idle(shared = [altitude_m])]
//!     fn idle(mut cx: idle::Context) -> ! { /* polled I2C reads */ }
//! }
//! ```
//!
//! Note there is **zero warble-specific glue** in that skeleton: RTIC
//! itself provides the locking, priorities, and scheduling, and the
//! crate's sync types drop straight into `#[shared]`/`#[local]`
//! resources. That is why warble ships no `rtic` cargo feature — see
//! "The rtic verdict" in docs/ARCHITECTURE.md.
//!
//! # Worst-case-latency budget (24 kHz, 1200 baud, 160 MHz RV32IMC)
//!
//! | duty                    | priority | worst case per activation                        |
//! |-------------------------|----------|--------------------------------------------------|
//! | `adc_dma` ISR push      | 3 (high) | `push_slice(120)` is O(120) copies ≈ a few µs; no decode work ever runs at this priority |
//! | `decode` chunk          | 2 (mid)  | 128 × push_i16 at a low-single-digit-µs/sample bound ≈ well under 1 ms steady-state; frame-close spike with `bounded_latency()` (repair sweep off) stays bounded — FX.25 RS(255,k), if used, adds ≈ 1.5–3.75 ms once per frame |
//! | `beacon` TX render      | 1 (low)  | one `transmit_i16` frame ≈ tens of ms of lazy per-sample synthesis, freely preempted |
//! | `idle` sensors          | 0        | whatever is left — real time delivers 128 samples every ~5.3 ms, so decode uses <20% of the core |
//!
//! The critical section held by the ISR is only the ring push (and by
//! the decode task only the `pop_slice`), so the maximum time priority
//! 3 blocks anything is microseconds; decode latency is bounded by
//! `TncConfig::bounded_latency()` by construction.
//!
//! # Timings, and what a real flight would use
//!
//! The 2 s beacon period here is **compressed so the example finishes
//! quickly**. Do not copy it onto the air.
//!
//! A 1200-baud APRS frame is roughly half a second of airtime, and every
//! station within radio horizon shares one VHF channel (144.390 MHz in
//! North America). A balloon is the worst possible station to
//! over-beacon from: at 30 km its horizon is some 600 km. The convention
//! is **30-60 s between beacons** while ascending -- a 1-2% duty cycle --
//! often slower at float and faster near landing, when position matters
//! most for recovery.
//!
//! The sensor and logging periods are realistic as written; only what
//! goes on the air needs rationing.
//!
//! It never returns — firmware does not — so **Ctrl-C stops it**. There
//! is no `--run-for` flag: this file is shaped like code for a chip with
//! no command line, no `std::env::args`, and no process to pass a flag
//! to, and a host-only knob in the middle of it would be a lie about the
//! target. To bound a run, bound it from the shell, where that concern
//! belongs:
//!
//! ```sh
//! timeout 5 cargo run --release --example balloon_tracker_rtic --features tnc
//! ```

use warble::SampleRate;
use warble::aprs::{AprsPacket, Status};
use warble::ax25::Address;
use warble::ring::SampleRing;
use warble::tnc::{DefaultTncReceiver, TncConfig, TncReceiver, TncTransmitter};

/// Samples per simulated DMA half-buffer (5 ms at 24 kHz).
const HALF_BUFFER: usize = 120;
/// Bounded decode chunk drained per `decode` activation.
const CHUNK: usize = 128;
/// Telemetry beacon period, in simulated milliseconds: 45 s, the middle
/// of the 30-60 s convention for an ascending balloon.
const BEACON_PERIOD_MS: u32 = 45_000;
/// Housekeeping report period, in simulated milliseconds.
const STATUS_PERIOD_MS: u32 = 60_000;
/// Altitude the flight starts from.
const START_ALTITUDE_M: u32 = 12_000;

/// The `#[shared]` resources: on a target RTIC wraps each in a
/// priority-ceiling lock; here exclusive `&mut` access models exactly
/// what `lock` hands its closure.
struct Shared {
    ring: SampleRing<1024>,
    altitude_m: u32,
}

/// The `#[local]` resources of the `decode` task.
struct DecodeLocal {
    rx: DefaultTncReceiver,
    frames_decoded: u32,
}

/// Priority 3: the ADC/DMA half-complete ISR body. Only the push —
/// O(half-buffer), microseconds, nothing else at this priority.
fn adc_dma(shared_ring: &mut SampleRing<1024>, half: &[i16]) {
    shared_ring.push_slice(half);
}

/// Priority 2: one activation of the `decode` software task. Drain a
/// bounded chunk "under the lock", decode "outside" it. Returns how
/// many samples were consumed (a target re-spawns while > 0).
fn decode(shared_ring: &mut SampleRing<1024>, local: &mut DecodeLocal) -> usize {
    let mut chunk = [0i16; CHUNK];
    let n = shared_ring.pop_slice(&mut chunk); // the lock ends here
    for &s in &chunk[..n] {
        if let Some(frame) = local.rx.push_i16(s) {
            println!(
                "[decode  p2] heard {} > {}: {}",
                String::from_utf8_lossy(frame.src().callsign.as_bytes()),
                String::from_utf8_lossy(frame.dest().callsign.as_bytes()),
                String::from_utf8_lossy(frame.info())
            );

            local.frames_decoded += 1;
        }
    }
    n
}

/// Priority 1: one activation of the monotonic-scheduled `beacon`
/// task. Renders a real frame with the modulator; a target streams
/// the samples to the DAC via DMA while being preempted freely.
fn beacon(altitude_m: u32, beacon_tx: &TncTransmitter) -> usize {
    let mut text = *b"alt 00000 m";
    let mut alt = altitude_m;
    for slot in text[4..9].iter_mut().rev() {
        *slot = b'0' + (alt % 10) as u8;
        alt /= 10;
    }
    // Sized for THIS payload, not the protocol maximum, to save RAM on
    // a microcontroller. Safe because `transmit_i16` reports the length
    // it needed and never truncates; see the fuller explanation in
    // `balloon_tracker_baremetal.rs`. On a host use `MAX_FRAME_BYTES`.
    let mut info_buf = [0u8; 64];
    let mut frame_buf = [0u8; 128];
    let samples = beacon_tx
        .transmit_i16(
            &AprsPacket::Status(Status { text: &text }),
            Address::new(b"APRS", 0).unwrap(),
            Address::new(b"BALLON", 1).unwrap(),
            &[],
            &mut info_buf,
            &mut frame_buf,
        )
        .unwrap()
        .count();
    println!(
        "[beacon  p1] keyed \"{}\" ({samples} samples to the DAC)",
        String::from_utf8_lossy(&text)
    );
    samples
}

/// Priority 0 (idle): the polled sensor sweep. A real build does an
/// I2C transaction here; a sounding balloon climbs at about 5 m/s,
/// which is one metre per 200 ms sweep.
fn idle_sensors(altitude_m: &mut u32) {
    *altitude_m += 1;
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = TncConfig::bell_202(SampleRate::new(24_000)?)?.bounded_latency();

    // Pre-render the "on-air" RX audio the DMA delivers: one real
    // status frame amid silence, looped for the whole flight.
    let tx = TncTransmitter::new(cfg);
    let mut info_buf = [0u8; 64];
    let mut frame_buf = [0u8; 128];
    let mut air = [0i16; 32_768];
    let mut air_len = 2_400;
    for s in tx.transmit_i16(
        &AprsPacket::Status(Status {
            text: b"ground station heartbeat",
        }),
        Address::new(b"APRS", 0)?,
        Address::new(b"GROUND", 0)?,
        &[],
        &mut info_buf,
        &mut frame_buf,
    )? {
        air[air_len] = s;
        air_len += 1;
    }
    air_len += 2_400;

    let mut shared = Shared {
        ring: SampleRing::new(),
        altitude_m: START_ALTITUDE_M,
    };
    let mut decode_local = DecodeLocal {
        rx: TncReceiver::new(cfg)?,
        frames_decoded: 0,
    };
    let beacon_tx = TncTransmitter::new(cfg);
    let mut beacons_keyed = 0u32;
    let mut air_pos = 0usize;

    // The mock scheduler: one lap per millisecond, dispatching each
    // "interrupt" in DESCENDING priority order — exactly the order a
    // single-core RTIC kernel would run ready tasks. Like the kernel it
    // stands in for, it never returns.
    let mut millis = 0u32;
    loop {
        // p3: DMA half-complete fires every 5 ms.
        if millis.is_multiple_of(5) {
            let end = (air_pos + HALF_BUFFER).min(air_len);
            adc_dma(&mut shared.ring, &air[air_pos..end]);
            air_pos = if end == air_len { 0 } else { end };
        }
        // p2: decode runs while samples remain (re-spawn semantics).
        while decode(&mut shared.ring, &mut decode_local) > 0 {}
        // p1: monotonic beacon every 2 s.
        if millis.is_multiple_of(BEACON_PERIOD_MS) {
            beacon(shared.altitude_m, &beacon_tx);
            beacons_keyed += 1;
        }
        // p0: idle gets the rest — sensor sweep every 200 ms.
        if millis.is_multiple_of(200) {
            idle_sensors(&mut shared.altitude_m);
        }

        // Housekeeping, as a low-priority task would emit it.
        if millis.is_multiple_of(STATUS_PERIOD_MS) && millis > 0 {
            println!(
                "[status ] t={:>3}s alt {} m | {} frames, {beacons_keyed} beacons, \
                 {} intake overruns",
                millis / 1_000,
                shared.altitude_m,
                decode_local.frames_decoded,
                shared.ring.overruns()
            );
        }

        // The invariant a real build would guard with a watchdog: if
        // the decode task ever falls behind the DMA, audio is lost and
        // the decode is silently wrong.
        assert_eq!(
            shared.ring.overruns(),
            0,
            "decode task fell behind the DMA at t={millis} ms"
        );

        millis = millis.wrapping_add(1);
    }
}
