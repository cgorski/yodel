//! RECEIVE → LOG: just listen and see the traffic.
//!
//! * **Scenario** — a monitoring station on the ground: listen to a
//!   channel and write a timestamped log of everything heard. The
//!   receiving counterpart to the balloon-tracker examples.
//! * **Hardware** — any host; a Raspberry Pi with an RTL-SDR or a radio
//!   on the sound card is the usual deployment. Reads a WAV file or a
//!   live 48 kHz PCM pipe on stdin.
//! * **Features** — `tnc,wav`.
//!
//! # What this file does, start to finish
//!
//! This is the first thing to run when you want to see whether decoding
//! works at all: point it at audio, watch human-readable log lines.
//!
//! 1. Opens the input audio — a WAV file path, or `-` for raw 16-bit
//!    little-endian mono PCM on stdin at 48 000 Hz (e.g. piped from a
//!    sound card capture tool).
//! 2. Pushes the samples one at a time into a `TncReceiver` (Bell 202
//!    demodulator → NRZI decoder → HDLC deframer → FCS check → AX.25
//!    UI-frame parse), exactly like `examples/decode_wav.rs`.
//! 3. For every FCS-valid frame, prints ONE structured log line:
//!
//!    ```text
//!    [   12.345s] N0CALL-7>APRS,N1CALL-1*,WIDE2-1: position lat 49.0583 lon -72.0292
//!    ```
//!
//!    * `[...s]` — the **sample-clock** timestamp: the sample position
//!      at which the frame completed, divided by the sample rate. The
//!      library has NO wall clock (it is a no_std DSP crate); relative
//!      time derived from the sample count is the only timestamp it can
//!      give. Wall time exists only at the I/O edge: `main` prints one
//!      wall-clock banner when the session starts, so you can anchor
//!      the relative timestamps if you need to.
//!    * `SRC>DEST` — source and destination (tocall) callsigns.
//!    * the digipeater path, one entry per hop, with a trailing `*` on
//!      every hop whose **H bit** (has-been-repeated) is set — the
//!      common monitor convention for "this digipeater already relayed
//!      the frame". The per-hop flags come from the typed
//!      `UiFrame::hops()` / `PathHop` API.
//!    * the payload kind (`position` / `status` / `message` / `ack` /
//!      `rej` / `mic-e` / `telemetry` / `other`) and a one-line summary:
//!      latitude/longitude for positions, addressee + text for messages.
//!
//! The line formatting is the pure function [`format_frame_line`] over
//! a parsed `UiFrame`, so the host test suite (`tests/app_examples.rs`)
//! asserts EXACT output for synthesized frames — the format you see
//! here is proven, not decorative.
//!
//! # Try it
//!
//! Make a test WAV first with the encode example (or the `warble` CLI's
//! `encode` command), then decode it back:
//!
//! ```sh
//! cargo run --example encode_wav --features tnc,wav
//! cargo run --example decode_to_log --features tnc,wav -- beacon.wav
//! ```

use std::io::Read;

use warble::SampleRate;
use warble::aprs::{AprsPacket, MessageContent};
use warble::ax25::{Address, UiFrame};
use warble::tnc::{DefaultTncReceiver, TncConfig, TncReceiver};

/// Sample rate assumed for raw PCM on stdin (WAV files carry their own).
const STDIN_RATE_HZ: u32 = 48_000;

/// Where to get an input, printed whenever one is missing or unusable.
const INPUT_HELP: &str = "\
input: a 16-bit mono integer PCM WAV (8000-48000 Hz), or `-` for raw
48 kHz 16-bit mono LE PCM on stdin.

no file yet? make one:
  cargo run --example encode_wav --features tnc,wav
      -> beacon.wav, a single clean beacon
  cargo run --features cli -- gen --out test.wav --count 10 --snr 10
      -> a 10-frame test signal with seeded noise (--snr, lower = harsher)

live audio instead:
  arecord -f S16_LE -r 48000 -c 1 -t raw | \\
      cargo run --example decode_to_log --features tnc,wav -- -";

fn main() {
    // Display, not Debug: returning `Result` from `main` would escape
    // the newlines in the help text onto one unreadable line.
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or_else(|| format!("usage: decode_to_log <input.wav | ->\n\n{INPUT_HELP}"))?;

    // Wall time exists ONLY here, at the I/O edge: one banner anchoring
    // the relative sample-clock timestamps that follow. The library
    // itself never sees a clock.
    let wall = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    eprintln!("session start: unix {wall} (log timestamps below are sample-clock relative)");

    if path == "-" {
        // Raw PCM from stdin: 16-bit little-endian mono at 48 kHz.
        let rate = SampleRate::new(STDIN_RATE_HZ)?;
        let mut rx: DefaultTncReceiver = TncReceiver::new(TncConfig::bell_202(rate)?)?;
        let mut sample_pos: u64 = 0;
        let stdin = std::io::stdin();
        let mut bytes = [0u8; 2];
        let mut lock = stdin.lock();
        loop {
            match lock.read_exact(&mut bytes) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e.into()),
            }
            let sample = i16::from_le_bytes(bytes);
            sample_pos += 1;
            if let Some(frame) = rx.push_i16(sample) {
                println!(
                    "{}",
                    format_frame_line(sample_pos, STDIN_RATE_HZ, frame.ui_frame())
                );
            }
        }
        report(&rx);
    } else {
        // A WAV file: must be 16-bit mono integer PCM (see decode_wav).
        let mut reader = hound::WavReader::open(&path).map_err(|e| match e {
            hound::Error::IoError(io) if io.kind() == std::io::ErrorKind::NotFound => {
                format!("cannot open {path}: no such file\n\n{INPUT_HELP}")
            }
            hound::Error::FormatError(_) => {
                format!("{path} is not a WAV file ({e})\n\n{INPUT_HELP}")
            }
            other => format!("cannot open {path}: {other}"),
        })?;
        let spec = reader.spec();
        if spec.channels != 1
            || spec.bits_per_sample != 16
            || spec.sample_format != hound::SampleFormat::Int
        {
            return Err(format!(
                "{path} is {}-channel {}-bit at {} Hz; need 1-channel 16-bit integer\n\n{INPUT_HELP}",
                spec.channels, spec.bits_per_sample, spec.sample_rate
            )
            .into());
        }
        let rate = SampleRate::new(spec.sample_rate)?;
        let mut rx: DefaultTncReceiver = TncReceiver::new(TncConfig::bell_202(rate)?)?;
        let mut sample_pos: u64 = 0;
        for sample in reader.samples::<i16>() {
            sample_pos += 1;
            if let Some(frame) = rx.push_i16(sample?) {
                println!(
                    "{}",
                    format_frame_line(sample_pos, spec.sample_rate, frame.ui_frame())
                );
            }
        }
        report(&rx);
    }
    Ok(())
}

/// Prints the receiver's saturating counters at end of input.
fn report(rx: &DefaultTncReceiver) {
    let stats = rx.stats();
    eprintln!(
        "frames ok: {}, fcs errors: {}",
        stats.frames_ok, stats.fcs_errors
    );
}

/// Formats one parsed frame as a structured monitor log line — a PURE
/// function (no I/O, no clock), so tests can assert exact output.
///
/// `sample_pos` is the sample count at which the frame completed and
/// `sample_rate_hz` the stream's rate; together they give the relative
/// sample-clock timestamp. The rest of the line is
/// `SRC>DEST,HOP1*,HOP2: kind summary`, where a trailing `*` marks a
/// hop whose has-been-repeated (H) bit is set.
#[must_use]
pub fn format_frame_line(sample_pos: u64, sample_rate_hz: u32, frame: &UiFrame<'_>) -> String {
    // Relative sample-clock time in seconds, millisecond resolution.
    #[allow(clippy::cast_precision_loss)] // display only
    let secs = sample_pos as f64 / f64::from(sample_rate_hz.max(1));
    let mut line = format!(
        "[{secs:9.3}s] {}>{}",
        fmt_addr(&frame.src),
        fmt_addr(&frame.dest)
    );
    // The typed per-hop path: address + H bit. Used hops get a '*'.
    for hop in frame.hops() {
        line.push(',');
        line.push_str(&fmt_addr(&hop.address));
        if hop.repeated {
            line.push('*');
        }
    }
    line.push_str(": ");
    line.push_str(&summarize(frame.info));
    line
}

/// The payload kind + one-line summary for an information field — also
/// pure. Unparseable payloads become `other "<lossy text>"` instead of
/// an error: a monitor should log everything it hears.
fn summarize(info: &[u8]) -> String {
    // Mic-E reports (data type '`' or '\'') hide half the position in
    // the DESTINATION callsign, so `AprsPacket::parse` cannot decode
    // them from the info field alone; classify by data-type identifier
    // (decode them with `RxFrame::mic_e` under the `micE` feature).
    if matches!(info.first(), Some(b'`' | b'\'')) {
        return "mic-e".to_string();
    }
    match AprsPacket::parse(info) {
        Ok(AprsPacket::Position(p)) => position_summary(p.latitude, p.longitude),
        Ok(AprsPacket::PositionCs(p)) => {
            position_summary(p.position.latitude, p.position.longitude)
        }
        Ok(AprsPacket::PositionTimestamped(p)) => {
            position_summary(p.position.latitude, p.position.longitude)
        }
        Ok(AprsPacket::Message(m)) => {
            let to = String::from_utf8_lossy(m.addressee.as_bytes()).into_owned();
            match m.content {
                MessageContent::Text { text, id: Some(id) } => format!(
                    "message {to} \"{}\" {{{}}}",
                    String::from_utf8_lossy(text),
                    String::from_utf8_lossy(id)
                ),
                MessageContent::Text { text, id: None } => {
                    format!("message {to} \"{}\"", String::from_utf8_lossy(text))
                }
                MessageContent::Ack { id } => {
                    format!("ack {to} {}", String::from_utf8_lossy(id))
                }
                MessageContent::Reject { id } => {
                    format!("rej {to} {}", String::from_utf8_lossy(id))
                }
            }
        }
        Ok(AprsPacket::Status(s)) => {
            format!("status \"{}\"", String::from_utf8_lossy(s.text))
        }
        Ok(AprsPacket::Telemetry(_)) => "telemetry".to_string(),
        Ok(_) | Err(_) => format!("other \"{}\"", String::from_utf8_lossy(info)),
    }
}

/// `position lat L lon L` with four decimals (≈ 11 m resolution).
fn position_summary(lat: warble::aprs::Latitude, lon: warble::aprs::Longitude) -> String {
    format!(
        "position lat {:.4} lon {:.4}",
        lat.to_degrees(),
        lon.to_degrees()
    )
}

/// Formats an address as `CALL` or `CALL-SSID`.
fn fmt_addr(addr: &Address) -> String {
    let call = String::from_utf8_lossy(addr.callsign.as_bytes()).into_owned();
    match addr.ssid.value() {
        0 => call,
        n => format!("{call}-{n}"),
    }
}
