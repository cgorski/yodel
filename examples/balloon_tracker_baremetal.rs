//! Bare-metal balloon tracker: one superloop, no executor, no
//! interrupts framework — the simplest way to share an MCU with
//! `yodel`.
//!
//! * **Scenario** — the flight computer *inside the payload*. It sends
//!   telemetry beacons down and decodes what its receiver hears (the
//!   simulated RX audio here is a ground-station uplink). Not the
//!   ground station.
//! * **Hardware** — a `no_std` MCU with **no operating system**:
//!   ESP32-C3/C6 class RISC-V, or a Cortex-M4/M7. No allocator, no
//!   threads, no executor — just `main` and a millisecond counter.
//!   Audio arrives from an ADC via DMA and leaves through a DAC/PWM.
//! * **Runs here** — on a host, with the ADC/DMA/DAC simulated so the
//!   loop self-checks. Only the scaffolding (`println!`, `Instant`) is
//!   host-only; the yodel call paths are the `no_std` ones.
//! * **Features** — `tnc`.
//!
//! If your MCU already runs an executor, prefer
//! [`_embassy`](balloon_tracker_embassy.rs) or
//! [`_rtic`](balloon_tracker_rtic.rs); with a full OS, prefer
//! [`balloon_tracker.rs`](balloon_tracker.rs).
//!
//! Run on a host:
//!
//! ```sh
//! cargo run --release --example balloon_tracker_baremetal --features tnc
//! ```
//!
//!
//! It never returns — firmware does not — so **Ctrl-C stops it**. There
//! is no `--run-for` flag: this file is shaped like code for a chip with
//! no command line, no `std::env::args`, and no process to pass a flag
//! to, and a host-only knob in the middle of it would be a lie about the
//! target. To bound a run, bound it from the shell, where that concern
//! belongs:
//!
//! ```sh
//! timeout 5 cargo run --release --example balloon_tracker_baremetal --features tnc
//! ```
//! **Use `--release`.** This example reports the decode duty cycle, and
//! an unoptimized build makes that figure roughly nine times too large
//! — measuring the debug build instead of the modem.
//!
//! The shape is exactly what a poll-loop flight computer does, with
//! the hardware simulated so it runs (and self-checks) on a host:
//!
//! * **intake** — stands in for the ADC/DMA half-complete interrupt:
//!   every 5 simulated milliseconds a 120-sample half-buffer of
//!   receiver audio (a real, pre-modulated APRS frame amid silence)
//!   is pushed into a `SampleRing` — on a target this happens in the
//!   ISR under a critical section, here at the top of the loop body.
//! * **decode** — the superloop drains the ring in bounded 128-sample
//!   chunks into a `TncConfig::bounded_latency()` `TncReceiver` via
//!   `push_i16`, so the worst-case time spent in the decode duty per
//!   lap is small and known.
//! * **sensors** — a barometer read stub every 200 ms.
//! * **logging** — a housekeeping/log stub every 1000 ms.
//! * **telemetry TX** — every 2000 ms (by the millis counter) the
//!   existing modulator synthesizes a position beacon; the samples
//!   would feed a DAC, here they are counted.
//!
//! Everything is fixed-size (`[i16; N]` buffers, a const-generic
//! ring, no heap on any yodel call path): the same code structure
//! compiles under `#![no_std]` without `alloc` — only the simulation
//! scaffolding (`println!`, `Instant` for the duty-cycle report) is
//! host-only.
//!
//!
//! # Timings
//!
//! **Every period here is a real one** — a 45 s beacon, a 5 Hz
//! barometer, a 5 m/s ascent from 12 km — and none of it is compressed,
//! because the flight clock is simulated: three minutes of flight costs
//! a few seconds of CPU whatever the periods are.
//!
//! The beacon period matters more for a balloon than for most stations.
//! A 1200-baud APRS frame is roughly half a second of airtime, and every
//! station within radio horizon shares one VHF channel (144.390 MHz in
//! North America). At 30 km a balloon is heard across some 600 km, so an
//! over-eager tracker jams several regions at once. At 45 s the
//! transmitter sits near a 1% duty cycle; real flights often slow at
//! float and speed up near landing, when position matters most for
//! recovery.
//! # Time budget (24 kHz sample rate, 1200 baud, one core)
//!
//! Real time delivers 120 samples every 5 ms; the loop drains up to
//! 128 per lap, so the ring (1024 samples ≈ 42 ms of audio) never
//! grows. Steady-state decode costs ~110 ns/sample on a
//! workstation-class core and a low-single-digit-µs bound per sample
//! on a 160 MHz RV32IMC — a 128-sample chunk is well under 1 ms of
//! work against a 5.3 ms real-time budget, leaving >5x headroom for
//! the sensor/log/TX duties. `bounded_latency()` turns the
//! FCS-failure repair sweep off, so the frame-close spike is bounded
//! and a corrupted frame can never starve the loop. Measured duty
//! cycles go out with the periodic housekeeping report, so the budget
//! claim is checked rather than asserted.
//!
//! The loop does not end. A flight computer runs until it loses power
//! or is recovered; Ctrl-C is how you stop this one.

use std::time::{Duration, Instant};

use yodel::SampleRate;
use yodel::aprs::{AprsPacket, Status};
use yodel::ax25::Address;
use yodel::ring::SampleRing;
use yodel::tnc::{DefaultTncReceiver, TncConfig, TncReceiver, TncTransmitter};

/// Samples per simulated DMA half-buffer (5 ms at 24 kHz).
const HALF_BUFFER: usize = 120;
/// Bounded decode chunk drained from the ring per superloop lap.
const CHUNK: usize = 128;
/// Fixed storage for the pre-rendered "on-air" RX audio.
const AIR_CAP: usize = 32_768;
/// Simulated flight length in milliseconds.
/// Housekeeping report period, in simulated milliseconds.
const STATUS_PERIOD_MS: u32 = 60_000;

/// Telemetry beacon period, in simulated milliseconds: 45 s, the middle
/// of the 30-60 s convention for an ascending balloon. Real value, not
/// a demo one — the clock here is simulated, so a realistic period
/// costs nothing to run.
const BEACON_PERIOD_MS: u32 = 45_000;
/// Barometer read period, in simulated milliseconds.
const SENSOR_PERIOD_MS: u32 = 200;
/// Altitude the flight starts from.
const START_ALTITUDE_M: u32 = 12_000;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = TncConfig::bell_202(SampleRate::new(24_000)?)?.bounded_latency();

    // ---- Pre-render the downlink audio into a FIXED buffer --------
    // One real status frame amid silence, looped by the intake as the
    // flight's RX feed (on a target this is what the antenna hears).
    let tx = TncTransmitter::new(cfg);
    // Transmit scratch, sized for THIS payload rather than for the
    // protocol maximum. A 24-character status is ~25 information bytes
    // and a ~43-byte frame, so 64/128 has ample headroom while
    // `tnc::MAX_FRAME_BYTES` (330, the AX.25 worst case) would cost
    // 2.5x the RAM for traffic this firmware never sends.
    //
    // That trade is safe to make because it cannot fail silently:
    // `transmit_i16` returns `TncError` carrying the length it needed
    // and never truncates. Sizing down is an embedded choice; on a
    // host, use `MAX_FRAME_BYTES` and stop thinking about it.
    let mut info_buf = [0u8; 64];
    let mut frame_buf = [0u8; 128];
    let mut air = [0i16; AIR_CAP];
    let mut air_len = 2_400; // 100 ms of leading silence
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
    air_len += 2_400; // 100 ms of trailing silence (already zeroed)
    assert!(air_len <= AIR_CAP);

    // ---- Fixed state: ring, receiver, duty bookkeeping -------------
    // On a target the ring is a `static` behind a critical-section
    // mutex shared with the DMA ISR; the superloop owns everything
    // else exclusively.
    let mut ring: SampleRing<1024> = SampleRing::new();
    let mut rx: DefaultTncReceiver = TncReceiver::new(cfg)?;
    let beacon_tx = TncTransmitter::new(cfg);

    let mut air_pos = 0usize; // intake cursor into the looped RX feed
    let mut altitude_m = START_ALTITUDE_M;
    let mut samples_processed = 0u64;
    let mut frames_decoded = 0u32;
    let mut beacons_keyed = 0u32;
    let mut sensor_reads = 0u32;
    let mut log_lines = 0u32;

    // Host-only duty-cycle measurement (a target would use a cycle
    // counter, or nothing at all).
    let mut spent_decode = Duration::ZERO;
    let mut spent_duties = Duration::ZERO;

    // ---- The superloop ---------------------------------------------
    // One lap per simulated millisecond: on hardware the loop is
    // free-running and `millis` comes from a SysTick counter. Note the
    // shape — a flight computer never leaves this loop; it runs until
    // the battery dies. The only exit here is the demo bound at the
    // bottom.
    let mut millis = 0u32;
    loop {
        // "DMA half-complete ISR": every 5 ms a half-buffer arrives.
        // On a target this block IS the ISR body (under the lock);
        // the superloop never touches the ADC directly.
        if millis.is_multiple_of(5) {
            let end = (air_pos + HALF_BUFFER).min(air_len);
            ring.push_slice(&air[air_pos..end]);
            air_pos = if end == air_len { 0 } else { end };
        }

        // Decode duty: drain ONE bounded chunk, decode outside the
        // (simulated) lock. Bounded work per lap by construction.
        let t = Instant::now();
        let mut chunk = [0i16; CHUNK];
        let n = ring.pop_slice(&mut chunk); // on a target: under the lock
        for &s in &chunk[..n] {
            if let Some(frame) = rx.push_i16(s) {
                println!(
                    "[decode ] t={millis:>4} ms heard {} > {}: {}",
                    core::str::from_utf8(frame.src().callsign.as_bytes())?,
                    core::str::from_utf8(frame.dest().callsign.as_bytes())?,
                    String::from_utf8_lossy(frame.info())
                );
                frames_decoded += 1;
            }
        }
        samples_processed += n as u64;
        spent_decode += t.elapsed();

        // Interleaved duties, scheduled off the millis counter.
        let t = Instant::now();
        if millis.is_multiple_of(SENSOR_PERIOD_MS) {
            // Sensor read stub: a real tracker does a quick polled
            // I2C/SPI transaction here. A sounding balloon climbs at
            // about 5 m/s, so one metre per 200 ms read.
            altitude_m += 1;
            sensor_reads += 1;
        }
        if millis.is_multiple_of(1_000) {
            // Logging stub: a real tracker writes a flash/SD record.
            log_lines += 1;
        }
        if millis.is_multiple_of(BEACON_PERIOD_MS) {
            // TX schedule: synthesize a beacon with the modulator.
            // On a target each sample feeds the DAC/PWM via DMA.
            let mut text = *b"alt 00000 m";
            let mut alt = altitude_m;
            for slot in text[4..9].iter_mut().rev() {
                *slot = b'0' + (alt % 10) as u8;
                alt /= 10;
            }
            // Same sizing rationale as the pre-render above. On a real
            // target these would be `static mut` or task-local rather
            // than stack, to keep the superloop's frame size flat.
            let mut tx_info = [0u8; 64];
            let mut tx_frame = [0u8; 128];
            let samples = beacon_tx
                .transmit_i16(
                    &AprsPacket::Status(Status { text: &text }),
                    Address::new(b"APRS", 0)?,
                    Address::new(b"BALLON", 1)?,
                    &[],
                    &mut tx_info,
                    &mut tx_frame,
                )?
                .count();
            println!(
                "[beacon ] t={millis:>4} ms keyed \"{}\" ({samples} samples to the DAC)",
                String::from_utf8_lossy(&text),
            );
            beacons_keyed += 1;
        }
        // Housekeeping: every simulated minute, report the time budget.
        // A real tracker logs this to flash, or drops it into the
        // telemetry beacon, rather than printing it.
        if millis.is_multiple_of(STATUS_PERIOD_MS) && millis > 0 {
            let elapsed = Duration::from_millis(u64::from(millis));
            println!(
                "[budget ] t={:>3}s alt {altitude_m} m | {frames_decoded} frames, \
                 {beacons_keyed} beacons, {sensor_reads} reads, {log_lines} logs",
                millis / 1_000
            );
            println!(
                "[budget ] decode duty {:.2}% of elapsed time, other duties {:.2}% \
                 ({samples_processed} samples, {} overruns)",
                100.0 * spent_decode.as_secs_f64() / elapsed.as_secs_f64(),
                100.0 * spent_duties.as_secs_f64() / elapsed.as_secs_f64(),
                ring.overruns()
            );
        }
        spent_duties += t.elapsed();

        // The invariant that matters, checked every lap rather than
        // once at the end: if the loop ever falls behind the intake,
        // audio is lost and the decode is silently wrong. On hardware
        // this is where a watchdog or an error counter goes.
        assert_eq!(
            ring.overruns(),
            0,
            "superloop fell behind the intake at t={millis} ms"
        );

        // Advance the simulated clock. On hardware this value comes
        // from a SysTick counter.
        millis = millis.wrapping_add(1);
    }
}
