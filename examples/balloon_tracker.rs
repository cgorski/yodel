//! Standard-OS balloon tracker: the same flight-computer duties as
//! `examples/balloon_tracker_baremetal.rs`, written the way you would
//! on a **real operating system**.
//!
//! * **Scenario** — the flight computer *inside the payload*. It sends
//!   telemetry beacons down and decodes the audio its receiver hears.
//!   Not the ground station: for the receiving side see
//!   [`examples/decode_to_log.rs`](decode_to_log.rs).
//! * **Hardware** — anything with a full OS and a thread scheduler: a
//!   Raspberry Pi in the payload box, or your Linux/macOS desktop.
//!   Audio would come from a USB sound card or an SDR pipe.
//! * **Features** — `tnc,wav`.
//!
//! Four sibling files run the same duties on other platforms:
//! [`_baremetal`](balloon_tracker_baremetal.rs) (superloop, no
//! executor), [`_embassy`](balloon_tracker_embassy.rs),
//! [`_rtic`](balloon_tracker_rtic.rs) and
//! [`_tokio`](balloon_tracker_tokio.rs).
//!
//! Run it three ways:
//!
//! ```sh
//! # Self-demo (no input needed): synthesizes a beacon WAV in memory,
//! # decodes it, and asserts the frame came back — proof out of the box.
//! cargo run --example balloon_tracker --features tnc,wav
//!
//! # Decode a WAV file (make one with `cargo run --example encode_wav`):
//! cargo run --example balloon_tracker --features tnc,wav -- beacon.wav
//!
//! # Pipe audio on stdin: a WAV stream, or raw 48 kHz s16le mono PCM
//! # (e.g. from arecord/sox/an SDR tool). The intake sniffs which.
//! arecord -f S16_LE -r 48000 -c 1 -t raw | \
//!     cargo run --example balloon_tracker --features tnc,wav -- -
//! ```
//!
//! # Why threads + `std::sync::mpsc`, not async?
//!
//! The crate has an `async` (tokio) feature, and it is the right tool
//! when you already live in an async application. But for a newcomer
//! putting a tracker on a Pi, plain threads are the simpler pick:
//!
//! * **No runtime, no extra feature flag**: `thread::spawn` and
//!   `mpsc::channel` are in `std`. Nothing new to learn or depend on.
//! * **The OS does the sharing**: this is the whole point of the
//!   standard-OS tier. Where the embedded variants hand-schedule
//!   duties off a millis counter, here each duty is simply its own
//!   thread and the kernel preempts fairly. Blocking reads are fine.
//! * **The mental model maps 1:1**: "decoder thread / sensor thread /
//!   TX scheduler thread" is exactly the architecture diagram, with
//!   channels as the arrows. Async buys nothing at three tasks.
//!
//! When the tracker is instead a component of a program that already
//! runs a runtime, [`examples/balloon_tracker_tokio.rs`](balloon_tracker_tokio.rs)
//! is the same three duties written as tokio tasks over the `asynk`
//! stream API.
//!
//! # Structure
//!
//! * **decode thread** — consumes the audio via the crate's stdin/WAV
//!   sniffing intake (`yodel::wav::sniff_pcm` + `decode_sniffed`, the
//!   same path the `yodel` CLI uses) and sends each FCS-valid frame
//!   over a channel as a self-contained `OwnedFrame`.
//! * **sensor thread** — a simulated barometer read every 100 ms
//!   (a real tracker does an I2C transaction here), publishing the
//!   altitude to the TX scheduler over its own channel.
//! * **TX scheduler thread** — every 500 ms synthesizes a telemetry
//!   beacon with the real modulator (`TncTransmitter::transmit_i16`)
//!   and reports the sample count; on a real tracker those samples go
//!   to the sound card / radio (e.g. via `aplay` or cpal — see
//!   `examples/live_capture.rs` for the sound-card side).
//! * **main thread** — prints a log line per decoded frame and
//!   coordinates shutdown.
//!
//! # Timings
//!
//! **Every period in this file is what a real tracker would use**, so
//! they can be copied as they stand: a 45 s beacon, a 10 Hz barometer,
//! and a 5 m/s ascent from 12 km.
//!
//! The beacon period matters more here than for most stations. A
//! 1200-baud APRS frame is roughly half a second of airtime, and every
//! station within radio horizon shares one VHF channel (144.390 MHz in
//! North America). A balloon at 30 km is heard across some 600 km, so an
//! over-eager tracker jams several regions at once — it is the worst
//! possible station to beacon quickly from. At 45 s the transmitter sits
//! near a 1% duty cycle. Real flights often slow at float and speed up
//! near landing, when position matters most for recovery.
//!
//! Only the **clock** is accelerated, by [`TIME_SCALE`], so a
//! three-minute flight takes six seconds to watch. Set it to 1 for real
//! time.
//!
//! # Shutdown
//!
//! Clean ctrl-c handling on std without a dependency means installing
//! a signal handler by hand, and the usual crate for it (`ctrlc`) is
//! not added — no new dependencies. Instead shutdown is a shared
//! `AtomicBool` plus **bounded duration**: the decode thread ends at
//! end-of-input (EOF on stdin/WAV is the natural signal), and the
//! demo/sensor/TX threads run for a fixed flight length and check the
//! flag every tick. Piped-input runs therefore stop when the pipe
//! closes — which is what ctrl-c on the producer side does anyway.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use yodel::SampleRate;
use yodel::aprs::{AprsPacket, Status};
use yodel::ax25::Address;
use yodel::tnc::{MAX_FRAME_BYTES, OwnedFrame, TncConfig, TncTransmitter};
use yodel::wav::{decode_sniffed, sniff_pcm};

/// Sample rate assumed for raw PCM on stdin (WAV streams carry their
/// own; the demo synthesizes at this rate too).
const RATE_HZ: u32 = 48_000;
/// Barometer read period. 10 Hz is an ordinary rate for a flight
/// computer's pressure sensor, and it costs only a local I2C
/// transaction.
const SENSOR_PERIOD: Duration = Duration::from_millis(100);
/// Telemetry beacon period: 45 s, in the middle of the 30-60 s
/// convention for an ascending balloon. See "Timings" in the header.
const BEACON_PERIOD: Duration = Duration::from_secs(45);
/// Altitude the demo starts from.
const START_ALTITUDE_M: u32 = 12_000;
/// Ascent per barometer read, in decimetres: a sounding balloon climbs
/// at roughly 5 m/s, which at [`SENSOR_PERIOD`] is half a metre per
/// read — so the sensor tracks decimetres and reports metres.
const ASCENT_DM_PER_READ: u32 = 5;
/// Optional `--run-for <SECONDS>` of wall time.
///
/// **The default is to never stop**, because a flight computer has no
/// shutdown path. This flag exists only so the example can be tried, or
/// run in CI, without reaching for Ctrl-C.
fn run_for() -> Option<Duration> {
    let args: Vec<String> = std::env::args().collect();
    let i = args.iter().position(|a| a == "--run-for")?;
    Some(Duration::from_secs_f64(args.get(i + 1)?.parse().ok()?))
}

/// Where to get input audio, printed when none is given.
const INPUT_HELP: &str = "\
input: the receiver's audio. A 16-bit mono WAV, or `-` for raw s16le
mono PCM at 48 kHz on stdin.

no file yet? make one:
  cargo run --example encode_wav --features tnc,wav
  cargo run --features cli -- gen --out test.wav --count 10 --snr 10

live, from a radio on the sound card:
  arecord -f S16_LE -r 48000 -c 1 -t raw | \\
      cargo run --example balloon_tracker --features tnc,wav -- -";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arg = std::env::args().nth(1);
    if arg.is_none() {
        eprintln!("usage: balloon_tracker <input.wav | ->\n\n{INPUT_HELP}");
        std::process::exit(2);
    }
    let rate = SampleRate::new(RATE_HZ)?;
    let cfg = TncConfig::bell_202(rate)?;

    // Shutdown flag shared by the periodic threads.
    let running = Arc::new(AtomicBool::new(true));

    // ---- decode thread: audio in -> OwnedFrame out ------------------
    // The intake is the crate's own sniffing path: WAV files, WAV on
    // stdin, and raw s16le PCM on stdin all funnel through the same
    // two calls the `yodel` CLI uses.
    let (frame_tx, frame_rx) = mpsc::channel::<OwnedFrame>();
    let decoder = std::thread::spawn(move || -> Result<u32, String> {
        let sink = |frame: OwnedFrame| frame_tx.send(frame).is_ok();
        let stats = match arg.as_deref() {
            // Piped audio on stdin: sniffed WAV or raw PCM at RATE_HZ.
            Some("-") => {
                let sniffed =
                    sniff_pcm(std::io::stdin().lock(), Some(rate)).map_err(|e| e.to_string())?;
                decode_sniffed(sniffed, sink).map_err(|e| e.to_string())?
            }
            // A WAV file argument.
            Some(path) => {
                let file = std::fs::File::open(path)
                    .map_err(|e| format!("cannot open {path}: {e}\n\n{INPUT_HELP}"))?;
                let sniffed = sniff_pcm(file, None).map_err(|e| e.to_string())?;
                decode_sniffed(sniffed, sink).map_err(|e| e.to_string())?
            }
            None => unreachable!("checked before the thread was spawned"),
        };
        Ok(stats.frames_ok)
    });

    // ---- sensor thread: simulated barometer -> altitude channel -----
    let (alt_tx, alt_rx) = mpsc::channel::<u32>();
    let sensor_running = Arc::clone(&running);
    let sensor = std::thread::spawn(move || {
        let mut altitude_dm = START_ALTITUDE_M * 10;
        while sensor_running.load(Ordering::Relaxed) {
            std::thread::sleep(SENSOR_PERIOD);
            // A real tracker reads the barometer over I2C here.
            altitude_dm += ASCENT_DM_PER_READ;
            // TX thread gone => flight over.
            let _ = alt_tx.send(altitude_dm / 10);
        }
        altitude_dm / 10
    });

    // ---- TX scheduler thread: periodic beacon synthesis -------------
    let tx_running = Arc::clone(&running);
    let tx_sched = std::thread::spawn(move || -> Result<u32, String> {
        let beacon_tx = TncTransmitter::new(cfg);
        let mut altitude_m = START_ALTITUDE_M;
        let mut beacons = 0u32;
        while tx_running.load(Ordering::Relaxed) {
            std::thread::sleep(BEACON_PERIOD);
            // Latest altitude from the sensor thread (non-blocking).
            while let Ok(alt) = alt_rx.try_recv() {
                altitude_m = alt;
            }
            let text = format!("alt {altitude_m:05} m");
            let mut info = [0u8; 64];
            let mut frame = [0u8; MAX_FRAME_BYTES];
            let samples = beacon_tx
                .transmit_i16(
                    &AprsPacket::Status(Status {
                        text: text.as_bytes(),
                    }),
                    Address::new(b"APRS", 0).map_err(|e| e.to_string())?,
                    Address::new(b"BALLON", 1).map_err(|e| e.to_string())?,
                    &[],
                    &mut info,
                    &mut frame,
                )
                .map_err(|e| e.to_string())?
                .count();
            // On a real tracker these samples feed the sound card /
            // radio; here the run is described instead of keyed.
            println!("[beacon ] keyed \"{text}\" ({samples} samples to the radio)");
            beacons += 1;
        }
        Ok(beacons)
    });

    // ---- main thread: log every decoded frame -----------------------
    // Blocks until the decode thread drops its sender (end of input).
    let mut frames_logged = 0u32;
    for frame in frame_rx {
        println!(
            "[decode ] heard {} > {}: {}",
            core::str::from_utf8(frame.src().callsign.as_bytes())?,
            core::str::from_utf8(frame.dest().callsign.as_bytes())?,
            String::from_utf8_lossy(frame.info())
        );
        frames_logged += 1;
    }

    // The audio ended -- a file ran out, or the pipe closed. On a real
    // flight the receiver never stops, so this is where the program
    // simply keeps going: the sensor and beacon threads run until the
    // process is killed.
    let frames_ok = decoder.join().expect("decode thread panicked")?;
    println!(
        "[intake ] end of audio after {frames_ok} frame(s); \
         beacon and sensor continue (Ctrl-C to stop)"
    );
    let _ = frames_logged;

    // Joining parks this thread forever, which is the right shape: a
    // flight computer has no shutdown path. `running` is how a caller
    // embedding this code stops the threads cleanly -- and how
    // `--run-for` does it here.
    if let Some(limit) = run_for() {
        std::thread::sleep(limit);
        println!("[tracker] --run-for elapsed; a real tracker would keep going");
        running.store(false, Ordering::Relaxed);
    }
    let _ = sensor.join();
    let _ = tx_sched.join();
    Ok(())
}
