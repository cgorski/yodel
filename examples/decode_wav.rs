//! Decode APRS frames from a WAV file and print them.
//!
//! * **Scenario** — the receive side, offline: a recording in, decoded
//!   frames out. The simplest possible use of the crate.
//! * **Hardware** — any Linux/macOS/Windows host. For live audio instead
//!   see [`live_capture.rs`](live_capture.rs) (sound card) or
//!   [`decode_pcm_tokio.rs`](decode_pcm_tokio.rs) (network/pipe).
//! * **Features** — `tnc,wav`. Uses `hound` directly for the file read,
//!   so add `hound = "3"` if you copy that part.
//!
//! Shows the full receive chain: 16-bit mono PCM samples pushed one at
//! a time into a `TncReceiver` (demodulator -> NRZI decoder -> HDLC
//! deframer -> FCS check -> AX.25 UI-frame parse), with each FCS-valid
//! frame printed as a human-readable `SRC>DEST,PATH: payload` line and
//! receive statistics at the end.
//!
//! ```sh
//! cargo run --example decode_wav --features tnc,wav -- beacon.wav
//! ```
//!
//! # Getting an input file
//!
//! Any **16-bit mono integer PCM WAV** at 8000-48000 Hz. Three ways to
//! get one without a radio:
//!
//! ```sh
//! # 1. One clean beacon -> beacon.wav
//! cargo run --example encode_wav --features tnc,wav
//!
//! # 2. A multi-frame test signal, with impairments you control. This
//! #    is the one to use for experiments: --snr adds seeded noise
//! #    (~20 dB mild, ~3 dB harsh), so runs are reproducible.
//! cargo run --features cli -- gen --out test.wav --count 10 --snr 10
//!
//! # 3. Off-air audio you recorded yourself, resampled to mono 16-bit:
//! sox recording.wav -c 1 -b 16 -e signed-integer mono.wav
//! ```
//!
//! Stereo, 8-bit, 24-bit and float WAVs are rejected with a message
//! rather than decoded as noise — see [`open_pcm_wav`].
//!
//! # A file is the easy case
//!
//! This example is blocking on purpose. A file has an end, and waiting
//! for a disk read is cheap, so a runtime would add machinery and buy
//! nothing. Decoding a **live** source — a socket or a pipe, arriving in
//! real time with no end in sight — is the case that wants async: see
//! [`examples/decode_pcm_tokio.rs`](decode_pcm_tokio.rs).

use yodel::SampleRate;
use yodel::aprs::AprsPacket;
use yodel::ax25::Address;
use yodel::tnc::{DefaultTncReceiver, RxFrame, TncConfig, TncReceiver};

/// How to produce an input file, printed whenever one is missing or
/// unusable. An example that only says "usage: <input.wav>" leaves a
/// newcomer to go and find a WAV from somewhere; this says where.
const INPUT_HELP: &str = "\
input: a 16-bit mono integer PCM WAV, 8000-48000 Hz.

no file yet? make one:
  cargo run --example encode_wav --features tnc,wav
      -> beacon.wav, a single clean beacon
  cargo run --features cli -- gen --out test.wav --count 10 --snr 10
      -> a 10-frame test signal with seeded noise (--snr, lower = harsher)

have a recording in the wrong shape? convert it:
  sox in.wav -c 1 -b 16 -e signed-integer out.wav";

/// Opens a WAV and checks it is the shape the receiver can read,
/// failing with an actionable message rather than a raw decoder error.
fn open_pcm_wav(path: &str) -> Result<hound::WavReader<std::io::BufReader<std::fs::File>>, String> {
    let reader = hound::WavReader::open(path).map_err(|e| match e {
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
        let fmt = match spec.sample_format {
            hound::SampleFormat::Int => "integer",
            hound::SampleFormat::Float => "float",
        };
        return Err(format!(
            "{path} is {}-channel {}-bit {fmt} at {} Hz; need 1-channel 16-bit integer\n\n{INPUT_HELP}",
            spec.channels, spec.bits_per_sample, spec.sample_rate
        ));
    }
    Ok(reader)
}

fn main() {
    // Print failures with Display, not Debug: returning `Result` from
    // `main` escapes the newlines in a multi-line help message and
    // renders it as one unreadable line.
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or_else(|| format!("usage: decode_wav <input.wav>\n\n{INPUT_HELP}"))?;

    let mut reader = open_pcm_wav(&path)?;
    let spec = reader.spec();
    let rate = SampleRate::new(spec.sample_rate)?;

    // A Bell 202 receiver at the file's sample rate. `DefaultTncReceiver`
    // sizes the internal frame buffer for the AX.25 maximum.
    let mut rx: DefaultTncReceiver = TncReceiver::new(TncConfig::bell_202(rate)?)?;

    // Push samples one at a time; every push may complete a frame.
    for sample in reader.samples::<i16>() {
        if let Some(frame) = rx.push_i16(sample?) {
            print_frame(&frame);
        }
    }

    // The receiver keeps saturating counters of what it saw.
    let stats = rx.stats();
    eprintln!(
        "frames ok: {}, fcs errors: {}",
        stats.frames_ok, stats.fcs_errors
    );
    Ok(())
}

/// Prints one frame as `SRC>DEST,PATH: <decoded payload>`.
fn print_frame(frame: &RxFrame<'_>) {
    let mut head = format!("{}>{}", fmt_addr(&frame.src()), fmt_addr(&frame.dest()));
    for digi in frame.path() {
        head.push(',');
        head.push_str(&fmt_addr(digi));
    }
    // `aprs()` lazily parses the information field; fall back to the
    // raw bytes when the payload is not (parseable) APRS.
    match frame.aprs() {
        Ok(AprsPacket::Position(p)) => println!(
            "{head}: position lat {:.4} lon {:.4}",
            p.latitude.to_degrees(),
            p.longitude.to_degrees()
        ),
        Ok(AprsPacket::Status(s)) => {
            println!("{head}: status \"{}\"", String::from_utf8_lossy(s.text));
        }
        Ok(other) => println!("{head}: {other:?}"),
        Err(_) => println!("{head}: raw \"{}\"", String::from_utf8_lossy(frame.info())),
    }
}

/// Formats an address as `CALL` or `CALL-SSID`.
fn fmt_addr(addr: &Address) -> String {
    let call = String::from_utf8_lossy(addr.callsign.as_bytes()).into_owned();
    match addr.ssid.value() {
        0 => call,
        n => format!("{call}-{n}"),
    }
}
