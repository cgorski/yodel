//! Embassy balloon tracker: four cooperative duties on one core,
//! decode as just one of them.
//!
//! * **Scenario** — the flight computer *inside the payload*: telemetry
//!   beacons out, decode of what the receiver hears. Not the ground
//!   station.
//! * **Hardware** — a `no_std` MCU running the **Embassy** async
//!   executor: ESP32-C3/C6 (`esp-hal` + `esp-hal-embassy`), nRF52,
//!   STM32. Still no OS and no allocator; Embassy provides cooperative
//!   tasks and a time driver in place of a hand-rolled superloop.
//! * **Runs here** — on a host: the `embassy-time` dev-dependency's
//!   `std` feature supplies the clock and `embassy-futures` a
//!   dependency-free executor, so the same task bodies self-check
//!   without hardware.
//! * **Features** — `embassy`.
//!
//! Without an executor, see
//! [`_baremetal`](balloon_tracker_baremetal.rs); with a full OS, see
//! [`balloon_tracker.rs`](balloon_tracker.rs).
//!
//! Run on a host (the `embassy-time` dev-dependency's `std` feature
//! supplies the clock; `embassy-futures` supplies a dependency-free
//! executor):
//!
//! ```text
//! cargo run --example balloon_tracker_embassy --features embassy
//! ```
//!
//! The four tasks, exactly the shape a flight computer wants:
//!
//! * **intake** — stands in for the ADC/DMA interrupt: pushes 5 ms
//!   half-buffers of receiver audio into a shared `SampleRing` on a
//!   real-time cadence (on a target this is the DMA half/full-complete
//!   ISR; here a timer task simulates it).
//! * **decode** — `warble::embassy::run_decoder` drains the ring in
//!   128-sample chunks through a `TncReceiver`, yielding between
//!   chunks, and prints every FCS-valid frame heard.
//! * **sensors** — reads the (simulated) barometer/GPS every 200 ms.
//! * **telemetry TX** — a `TxTicker` builds and "keys" a position
//!   beacon every 2 s (prints the sample count it would hand the DAC).
//!
//! # Time budget (24 kHz sample rate, 1200 baud, one core)
//!
//! Steady-state decode costs ~110 ns/sample on a workstation-class
//! core and a low-single-digit-µs bound per sample on a 160 MHz MCU;
//! a 128-sample chunk is therefore well under 1 ms of work between
//! yield points, while real time delivers 128 samples every ~5.3 ms —
//! better than 5x headroom for the sensor and TX duties. The
//! worst-case burst is the push that closes a frame: with
//! `TncConfig::bounded_latency()` (used here) the repair sweep is off,
//! so that burst stays bounded and the sensor cadence is never starved
//! by a corrupted frame. Intake never blocks: a 1024-sample ring holds
//! ~42 ms of audio (8 half-buffers of headroom), and overruns are
//! counted, not spliced.
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
//! timeout 5 cargo run --release --example balloon_tracker_embassy --features embassy
//! ```

use core::cell::{Cell, RefCell};

use embassy_futures::block_on;
use embassy_futures::join::join4;
use embassy_time::{Duration, Ticker, Timer};
use warble::SampleRate;
use warble::aprs::{AprsPacket, Status};
use warble::ax25::Address;
use warble::embassy::{SampleSource, TxTicker, run_decoder};
use warble::ring::SampleRing;
use warble::tnc::{DefaultTncReceiver, TncConfig, TncReceiver, TncTransmitter};

/// How long the simulated flight runs.
/// Telemetry beacon period: 45 s, the middle of the 30-60 s convention
/// for an ascending balloon.
const BEACON_PERIOD: Duration = Duration::from_secs(45);
/// Sensor sweep period.
const SENSOR_PERIOD: Duration = Duration::from_millis(200);
/// Altitude the flight starts from.
const START_ALTITUDE_M: u32 = 12_000;
/// Samples per simulated DMA half-buffer (5 ms at 24 kHz).
const HALF_BUFFER: usize = 120;

/// Drains the shared intake ring; reports end-of-stream once the
/// intake task is done and the ring is empty.
///
/// On a real tracker `intake_done` is never set — the ADC runs until the
/// power does — so the decoder never sees end-of-stream. The flag exists
/// because [`SampleSource`] has to be able to express it: a bench setup
/// feeding a finite recording does end, and the decoder should stop
/// cleanly rather than spin.
struct RingSource<'a> {
    ring: &'a RefCell<SampleRing<1024>>,
    intake_done: &'a Cell<bool>,
}

impl SampleSource for RingSource<'_> {
    async fn next_chunk(&mut self, buf: &mut [i16]) -> usize {
        loop {
            let n = self.ring.borrow_mut().pop_slice(buf);
            if n > 0 {
                return n;
            }
            if self.intake_done.get() {
                return 0; // flight over: stop the decoder
            }
            // Nothing buffered yet: sleep one half-buffer period.
            Timer::after_millis(5).await;
        }
    }
}

// The executor never returns: every task loops forever, exactly as it
// would on the target. `main` therefore has no reachable end, which the
// compiler correctly notices.
#[allow(unreachable_code)]
fn main() {
    let cfg = TncConfig::bell_202(SampleRate::new(24_000).unwrap())
        .unwrap()
        .bounded_latency();

    // Pre-render the "audio heard on the downlink": one status frame
    // amid silence, looped by the intake task as the flight's RX feed.
    let tx = TncTransmitter::new(cfg);
    // Sized for THIS payload, not the protocol maximum, to save RAM on
    // a microcontroller. Safe because `transmit_i16` reports the length
    // it needed and never truncates; see the fuller explanation in
    // `balloon_tracker_baremetal.rs`. On a host use `MAX_FRAME_BYTES`.
    let mut info_buf = [0u8; 64];
    let mut frame_buf = [0u8; 128];
    let mut air: Vec<i16> = vec![0; 2400];
    air.extend(
        tx.transmit_i16(
            &AprsPacket::Status(Status {
                text: b"ground station heartbeat",
            }),
            Address::new(b"APRS", 0).unwrap(),
            Address::new(b"GROUND", 0).unwrap(),
            &[],
            &mut info_buf,
            &mut frame_buf,
        )
        .unwrap(),
    );
    air.extend(std::iter::repeat_n(0i16, 2400));

    // Shared state. Everything runs inside one `block_on` on one
    // thread, so plain RefCell/Cell suffice; on a target the ring
    // lives in a critical-section mutex shared with the ISR.
    let ring: RefCell<SampleRing<1024>> = RefCell::new(SampleRing::new());
    let intake_done = Cell::new(false);
    let altitude_m = Cell::new(START_ALTITUDE_M);

    // Task 1: simulated ADC/DMA intake on a real-time cadence.
    let intake = async {
        let mut ticker = Ticker::every(Duration::from_millis(5));
        let mut pos = 0usize;
        loop {
            ticker.next().await;
            let end = (pos + HALF_BUFFER).min(air.len());
            ring.borrow_mut().push_slice(&air[pos..end]);
            pos = if end == air.len() { 0 } else { end };
        }
    };

    // Task 2: decode, one bounded chunk at a time.
    let decode = async {
        let mut rx: DefaultTncReceiver = TncReceiver::new(cfg).unwrap();
        let mut source = RingSource {
            ring: &ring,
            intake_done: &intake_done,
        };
        let mut chunk = [0i16; 128];
        let total = run_decoder(&mut source, &mut rx, &mut chunk, |frame| {
            println!(
                "[decode ] heard {} > {}: {}",
                String::from_utf8_lossy(frame.src().callsign.as_bytes()),
                String::from_utf8_lossy(frame.dest().callsign.as_bytes()),
                String::from_utf8_lossy(frame.info())
            );
        })
        .await;
        println!(
            "[decode ] flight over: {} samples, {} frames ok, {} intake overruns",
            total,
            rx.stats().frames_ok,
            ring.borrow_mut().take_overruns()
        );
    };

    // Task 3: sensor sweep every 200 ms.
    let sensors = async {
        let mut ticker = Ticker::every(SENSOR_PERIOD);
        loop {
            ticker.next().await;
            // A real tracker reads I2C/SPI here (async HAL or quick
            // polled reads). A sounding balloon climbs at about 5 m/s,
            // which is one metre per 200 ms sweep.
            altitude_m.set(altitude_m.get() + 1);
        }
    };

    // Task 4: telemetry beacon every 2 s via the embassy-time ticker.
    let telemetry = async {
        let beacon_tx = TncTransmitter::new(cfg);
        let mut tick = TxTicker::every(BEACON_PERIOD);
        loop {
            tick.ready().await;
            let mut text = *b"alt 00000 m";
            let mut alt = altitude_m.get();
            for slot in text[4..9].iter_mut().rev() {
                *slot = b'0' + (alt % 10) as u8;
                alt /= 10;
            }
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
                .unwrap();
            // On a target these samples feed the DAC/PWM via DMA.
            println!(
                "[beacon ] keyed \"{}\" ({} samples to the DAC)",
                String::from_utf8_lossy(&text),
                samples.count()
            );
        }
    };

    // Never returns: every task loops, as its real counterpart does.
    // A flight computer has no shutdown path; Ctrl-C ends this one.
    block_on(join4(intake, decode, sensors, telemetry));
}
