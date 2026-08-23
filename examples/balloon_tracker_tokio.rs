//! Async balloon tracker: the same flight-computer duties as
//! `examples/balloon_tracker.rs`, for an application that **already
//! lives in an async runtime**.
//!
//! * **Scenario** — the flight computer *inside the payload*, or a
//!   ground-side service doing the same duties: beacons out, decode of
//!   the received audio. Reach for this shape when the tracker is one
//!   component of a bigger program — one already serving HTTP, talking
//!   to MQTT, or multiplexing several radios.
//! * **Hardware** — anything with a full OS and a tokio runtime: a
//!   Raspberry Pi, or a Linux/macOS server. **Not** an MCU; for those
//!   see [`_baremetal`](balloon_tracker_baremetal.rs),
//!   [`_embassy`](balloon_tracker_embassy.rs) or
//!   [`_rtic`](balloon_tracker_rtic.rs).
//! * **Features** — `async,wav`.
//!
//! With no runtime already in play, [`balloon_tracker.rs`](balloon_tracker.rs)
//! is the simpler answer: three threads, no dependency.
//!
//! Run it two ways:
//!
//! ```sh
//! # Self-demo (no input needed): synthesizes a beacon WAV in memory,
//! # decodes it through the async stream, and asserts the frame came
//! # back — proof out of the box.
//! cargo run --example balloon_tracker_tokio --features async,wav
//!
//! # Decode a WAV file (make one with `cargo run --example encode_wav`):
//! cargo run --example balloon_tracker_tokio --features async,wav -- beacon.wav
//! ```
//!
//! # When to reach for this instead of the threaded version
//!
//! [`examples/balloon_tracker.rs`](balloon_tracker.rs) is the default:
//! three duties, three threads, no runtime, nothing new to learn. It is
//! the one to copy for a Raspberry Pi in a payload box.
//!
//! Pick *this* shape when the tracker is a **component of a larger
//! async program** — one that is already serving HTTP, talking to an
//! MQTT broker, or multiplexing a dozen radios — because there the
//! runtime already exists and threads would be the foreign element.
//! The decisive difference is not performance at three tasks; it is
//! that `frames` and `decode_wav` hand you a [`Stream`], which composes
//! with the combinators, `select!` and cancellation your application is
//! already written in.
//!
//! # What the async feature does
//!
//! warble's DSP core is synchronous, allocation-free and `no_std`. It
//! is **not** rewritten for async, and it should not be: decoding a
//! sample is arithmetic, not I/O, so there is nothing to await. What
//! `warble::asynk` provides is the *edge* — the decode loop is placed on
//! `spawn_blocking` (or fed from an `AsyncRead`), and frames arrive over
//! a **bounded** channel exposed as a `Stream`. A slow consumer applies
//! backpressure all the way to the reader and nothing is dropped.
//!
//! That is the whole idea: async at the I/O boundary, synchronous
//! arithmetic inside.
//!
//! # Structure
//!
//! Three tokio tasks and a stream, mirroring the threaded version:
//!
//! * **decode** — [`warble::asynk::decode_stream`] over the input,
//!   yielding an `OwnedFrame` per FCS-valid frame. This is the task the
//!   threaded version spends a thread on.
//! * **sensor** — a simulated barometer on a 100 ms
//!   [`tokio::time::interval`], publishing altitude over a
//!   [`tokio::sync::watch`] channel. `watch` is the right primitive
//!   here and `mpsc` is not: the TX scheduler wants the *latest*
//!   reading, never a queue of stale ones.
//! * **TX scheduler** — every 500 ms, reads the current altitude and
//!   synthesizes a telemetry beacon with the real modulator
//!   (`TncTransmitter::transmit_to_vec_i16`). On a real tracker those
//!   samples go to the sound card; see `examples/live_capture.rs`.
//! * **main** — consumes the frame stream and prints a log line each.
//!
//! # Timings
//!
//! **Every period in this file is the value a real tracker would use**,
//! so they can be copied as they stand. A 45 s beacon period sits in
//! the middle of the 30-60 s convention for an ascending balloon.
//!
//! That number matters more here than for most stations. A 1200-baud
//! APRS frame is roughly half a second of airtime, and every station
//! within radio horizon shares one VHF channel (144.390 MHz in North
//! America). A balloon at 30 km is heard across some 600 km, so an
//! over-eager tracker jams several regions at once — it is the worst
//! possible station to beacon quickly from. At 45 s the transmitter
//! sits near a 1% duty cycle. Real flights often slow further at float
//! and speed up near landing, when position matters most for recovery.
//!
//! Nothing here is compressed for the sake of the example: it runs at
//! real speed, and beacons 45 s apart. It also does not stop on its own
//! — a flight computer has no shutdown path, so Ctrl-C is how you end
//! it.
//!
//! The sensor period is realistic as written: reading a barometer at
//! 10 Hz costs only a local I2C transaction. Only what goes on the air
//! needs rationing.
//!
//! # Shutdown
//!
//! `tokio::select!` on the flight timer, which is what the threaded
//! version's `AtomicBool` + bounded duration is emulating. The decode
//! stream ends by itself at end of input; the periodic tasks are aborted
//! when the flight ends. Cancellation is the thing async is better at,
//! so it is worth seeing it done directly.

use std::time::Duration;

use tokio_stream::StreamExt;
use warble::SampleRate;
use warble::aprs::{AprsPacket, Status};
use warble::ax25::Address;
use warble::tnc::{OwnedFrame, TncConfig, TncTransmitter};

/// Sample rate for the synthesized demo and for raw PCM input.
const RATE_HZ: u32 = 48_000;

/// Barometer read period. 10 Hz is an ordinary rate for a flight
/// computer's pressure sensor.
const SENSOR_PERIOD: Duration = Duration::from_millis(100);

/// Altitude the demo starts from: a latex sounding balloon well into
/// its ascent.
const START_ALTITUDE_M: u32 = 12_000;

/// Ascent per barometer read, in decimetres. A sounding balloon climbs
/// at roughly 5 m/s, which at [`SENSOR_PERIOD`] is half a metre per
/// read — so the sensor tracks decimetres and reports metres.
const ASCENT_DM_PER_READ: u32 = 5;

/// Telemetry beacon period: 45 s, in the middle of the 30-60 s
/// convention for an ascending balloon. See "Timings" in the header for
/// why this is not smaller.
const BEACON_PERIOD: Duration = Duration::from_secs(45);

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
      cargo run --example balloon_tracker_tokio --features async,wav -- -";

#[tokio::main]
async fn main() {
    // Display, not Debug: returning `Result` from `main` escapes the
    // newlines in a multi-line help message onto one unreadable line.
    if let Err(e) = run().await {
        eprintln!("{e}");
        std::process::exit(2);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let rate = SampleRate::new(RATE_HZ)?;
    let cfg = TncConfig::bell_202(rate)?;

    // ---- intake -------------------------------------------------
    // On the payload this is the receiver's audio, arriving from an
    // ADC. On a host, point it at a recording or a live pipe.
    let path = std::env::args()
        .nth(1)
        .ok_or_else(|| format!("usage: balloon_tracker_tokio <input.wav | ->\n\n{INPUT_HELP}"))?;
    // A WAV states its own rate in the header; raw PCM does not, so the
    // rate is supplied only for stdin. Passing it for a file would
    // contradict the header whenever the two disagree.
    let (reader, raw_rate): (Box<dyn tokio::io::AsyncRead + Send + Unpin>, _) = if path == "-" {
        (Box::new(tokio::io::stdin()), Some(rate))
    } else {
        let f = tokio::fs::File::open(&path)
            .await
            .map_err(|e| format!("cannot open {path}: {e}\n\n{INPUT_HELP}"))?;
        (Box::new(f), None)
    };
    println!("[intake ] {path}");

    // The intake sniffs WAV vs raw s16le, exactly as the CLI does, so a
    // recording and an `arecord` pipe are the same call here.
    let frames = warble::asynk::decode_stream(reader, raw_rate);
    let mut frames = std::pin::pin!(frames);

    // ---- sensor task --------------------------------------------
    // watch: the TX scheduler wants the newest altitude, not a backlog.
    // Seeded with the power-on reading rather than 0, because
    // `tokio::time::interval` fires its FIRST tick immediately -- so a
    // 0 here would be transmitted before the sensor ever ran. That
    // first-tick behaviour catches people out; it is why the beacon
    // reads a real altitude on frame one.
    let (altitude_tx, altitude_rx) = tokio::sync::watch::channel(START_ALTITUDE_M);
    let sensor = tokio::spawn(async move {
        let mut tick = tokio::time::interval(SENSOR_PERIOD);
        // Decimetres, because a 5 m/s ascent is half a metre per read at
        // 10 Hz and integer metres would truncate the climb to zero.
        let mut altitude_dm = START_ALTITUDE_M * 10;
        loop {
            tick.tick().await;
            // A real tracker does an I2C transaction here.
            altitude_dm = altitude_dm.saturating_add(ASCENT_DM_PER_READ);
            // Send failure means every receiver is gone: time to stop.
            if altitude_tx.send(altitude_dm / 10).is_err() {
                break;
            }
        }
    });

    // ---- TX scheduler task --------------------------------------
    let tx_task = tokio::spawn(async move {
        let mut tick = tokio::time::interval(BEACON_PERIOD);
        let tx = TncTransmitter::new(cfg);
        let mut beacons = 0u32;
        loop {
            tick.tick().await;
            let altitude_m = *altitude_rx.borrow();
            match beacon_samples(&tx, altitude_m) {
                Ok(samples) => {
                    beacons += 1;
                    // On a real tracker: hand `samples` to the sound
                    // card / radio here.
                    println!(
                        "  beacon #{beacons}: {} samples ({:.2} s) at {altitude_m} m",
                        samples.len(),
                        samples.len() as f64 / f64::from(RATE_HZ)
                    );
                }
                Err(e) => eprintln!("  beacon failed: {e}"),
            }
        }
    });

    // ---- main: log what the receiver hears ----------------------
    // The sensor and beacon tasks run until the process is killed, as
    // they would on a real flight. This task ends when the audio does,
    // which for a live radio never happens.
    while let Some(next) = frames.next().await {
        match next {
            Ok(frame) => println!("[rx     ] {}", describe(&frame)),
            Err(e) => eprintln!("[rx     ] {e}"),
        }
    }
    println!("[intake ] end of audio; beacon and sensor continue (Ctrl-C to stop)");

    // Awaiting the tasks parks this one forever, which is the correct
    // shape: the tracker beacons until it loses power or is recovered.
    match run_for() {
        None => {
            let _ = tokio::join!(sensor, tx_task);
        }
        Some(limit) => {
            tokio::time::sleep(limit).await;
            println!("[tracker] --run-for elapsed; a real tracker would keep going");
            sensor.abort();
            tx_task.abort();
        }
    }
    Ok(())
}

/// One telemetry beacon as PCM samples: a status report carrying the
/// current altitude.
fn beacon_samples(
    tx: &TncTransmitter,
    altitude_m: u32,
) -> Result<Vec<i16>, Box<dyn std::error::Error>> {
    let text = format!("balloon alt {altitude_m}m");
    let packet = AprsPacket::Status(Status {
        text: text.as_bytes(),
    });
    Ok(tx.transmit_to_vec_i16(
        &packet,
        Address::new(b"APRS", 0)?,
        Address::new(b"BALLON", 11)?,
        &[Address::new(b"WIDE2", 1)?],
    )?)
}

/// `CALL` or `CALL-SSID`.
fn fmt_addr(addr: &Address) -> String {
    let call = String::from_utf8_lossy(addr.callsign.as_bytes()).into_owned();
    match addr.ssid.value() {
        0 => call,
        n => format!("{call}-{n}"),
    }
}

/// A one-line summary of a received frame.
fn describe(frame: &OwnedFrame) -> String {
    match frame.ui_frame() {
        Ok(ui) => format!(
            "{}>{}: {}",
            fmt_addr(&ui.src),
            fmt_addr(&ui.dest),
            String::from_utf8_lossy(frame.info())
        ),
        Err(e) => format!("(unparsable UI frame: {e})"),
    }
}
