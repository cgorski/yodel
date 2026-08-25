//! `yodel ft8`: FT8 transmission generation and capture decoding.
//!
//! Two subcommands mirroring the library split (and the `yodel wspr`
//! shape): `gen` runs the TX pipeline (message → 79 symbols →
//! GFSK-shaped 8-FSK WAV) and `decode` runs the std receive engine
//! over a ~15 s 12 kHz capture.

use clap::{Args, Subcommand};

use yodel::SampleRate;
use yodel::ft8::{Ft8Config, Ft8Decoder, Ft8DecoderConfig, Ft8Message, Ft8Modulator, Ft8Tail};

use crate::shared::{Output, check_wav_spec};

/// Arguments of `yodel ft8`: FT8 TX and capture RX.
#[derive(Args)]
pub struct Ft8Args {
    #[command(subcommand)]
    command: Ft8Command,
}

#[derive(Subcommand)]
enum Ft8Command {
    /// Generate one FT8 transmission (~12.64 s) as a 16-bit mono WAV
    /// at 12 kHz.
    Gen {
        /// The message: either a standard exchange ("CQ K1ABC FN42",
        /// "K1ABC W9XYZ R-08", "K1ABC W9XYZ RR73", ...) or free text
        /// (up to 13 chars of the FT8 alphabet) with --free-text
        #[arg(long, value_name = "TEXT")]
        message: String,

        /// Treat the message as free text (i3.n3 = 0.0) instead of
        /// parsing it as a standard exchange
        #[arg(long)]
        free_text: bool,

        /// Output WAV file (16-bit mono integer PCM at 12 kHz)
        #[arg(long = "out", short = 'o', value_name = "OUTPUT.wav")]
        out: String,

        /// Tone-0 audio frequency in Hz (the audio-passband
        /// convention is roughly 200..=3000)
        #[arg(long = "offset-hz", value_name = "HZ", default_value_t = 1_500)]
        offset_hz: u32,
    },
    /// Decode FT8 transmissions from a 12 kHz 16-bit mono WAV capture
    /// (at least ~12.64 s): one line per decode on stdout.
    Decode {
        /// Input WAV file (16-bit mono integer PCM; 12 kHz only — the
        /// engine is fixed-rate, resample externally if needed)
        #[arg(value_name = "INPUT.wav")]
        input: String,

        /// Search half-width around 1500 Hz, in Hz [range: 50..=300]
        #[arg(long, value_name = "HZ", default_value_t = 300)]
        window: u32,

        /// Maximum decodes to attempt per capture [range: 1..=32]
        #[arg(long, value_name = "N", default_value_t = 6)]
        max_candidates: usize,
    },
}

/// Runs `yodel ft8`.
pub fn ft8(args: &Ft8Args) -> Result<(), String> {
    match &args.command {
        Ft8Command::Gen {
            message,
            free_text,
            out,
            offset_hz,
        } => generate(message, *free_text, out, *offset_hz),
        Ft8Command::Decode {
            input,
            window,
            max_candidates,
        } => decode(input, *window, *max_candidates),
    }
}

/// FT8's fixed capture/synthesis rate.
const RATE_HZ: u32 = 12_000;

/// Parses a standard-exchange message string into an [`Ft8Message`]:
/// `CALL_A CALL_B [R] TAIL` where TAIL is a grid, `+NN`/`-NN` report,
/// `RRR`, `RR73`, `73`, or absent.
fn parse_standard(message: &str) -> Result<Ft8Message, String> {
    let fields: Vec<&str> = message.split_whitespace().collect();
    let (a, b, rest) = match fields.as_slice() {
        [a, b, rest @ ..] if rest.len() <= 2 => (*a, *b, rest),
        _ => {
            return Err(format!(
                "cannot parse '{message}' as a standard exchange: expected \
                 'CALL_A CALL_B [R] [grid|report|RRR|RR73|73]' (use --free-text for free text)"
            ));
        }
    };
    let (r, tail_str) = match rest {
        ["R", t] => (true, Some(*t)),
        [t] if t.len() >= 2 && t.starts_with('R') && (t[1..2] == *"-" || t[1..2] == *"+") => {
            (true, Some(&t[1..]))
        }
        [t] => (false, Some(*t)),
        [] => (false, None),
        _ => return Err(format!("cannot parse the trailer of '{message}'")),
    };
    let tail = match tail_str {
        None => Ft8Tail::None,
        Some("RRR") => Ft8Tail::Rrr,
        Some("RR73") => Ft8Tail::Rr73,
        Some("73") => Ft8Tail::Seventy3,
        Some(t) if t.starts_with('+') || t.starts_with('-') => {
            let v: i8 = t
                .parse()
                .map_err(|_| format!("cannot parse report '{t}'"))?;
            Ft8Tail::Report(v)
        }
        Some(t) => Ft8Tail::grid(t).map_err(|e| format!("bad grid '{t}': {e}"))?,
    };
    Ft8Message::standard(a, b, r, tail).map_err(|e| format!("bad message '{message}': {e}"))
}

fn generate(message: &str, free_text: bool, out: &str, offset_hz: u32) -> Result<(), String> {
    let msg = if free_text {
        Ft8Message::free_text(message).map_err(|e| format!("bad free text '{message}': {e}"))?
    } else {
        parse_standard(message)?
    };
    let rate = SampleRate::new(RATE_HZ).expect("12 kHz is in range");
    let config = Ft8Config::new(offset_hz, rate)
        .map_err(|e| format!("bad --offset-hz '{offset_hz}': {e}"))?;
    let tx = Ft8Modulator::for_message(config, &msg);
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: RATE_HZ,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer =
        hound::WavWriter::create(out, spec).map_err(|e| format!("creating '{out}': {e}"))?;
    for s in tx {
        writer
            .write_sample(s)
            .map_err(|e| format!("writing '{out}': {e}"))?;
    }
    writer
        .finalize()
        .map_err(|e| format!("finalizing '{out}': {e}"))?;
    Ok(())
}

fn decode(input: &str, window: u32, max_candidates: usize) -> Result<(), String> {
    let mut reader =
        hound::WavReader::open(input).map_err(|e| format!("opening '{input}': {e}"))?;
    let spec = reader.spec();
    let rate = check_wav_spec(&spec, input)?;
    if rate.hz() != RATE_HZ {
        return Err(format!(
            "unsupported WAV sample rate in '{input}': got {} Hz, the FT8 decoder is \
             fixed at {RATE_HZ} Hz (resample the capture externally)",
            rate.hz()
        ));
    }
    let samples: Vec<i16> = reader
        .samples::<i16>()
        .collect::<Result<_, _>>()
        .map_err(|e| format!("reading '{input}': {e}"))?;
    let config = Ft8DecoderConfig::new(1_500, window)
        .map_err(|e| format!("bad --window '{window}': {e}"))?
        .max_candidates(max_candidates)
        .map_err(|e| format!("bad --max-candidates '{max_candidates}': {e}"))?;
    let decoder = Ft8Decoder::new(config);
    let decodes = decoder
        .decode(&samples)
        .map_err(|e| format!("decoding '{input}': {e}"))?;
    let mut out = Output::new();
    for d in &decodes {
        out.line(format_args!(
            "{} | freq {:.1} Hz | dt {:.2} s | snr {:.0} dB | sync {:.2}",
            d.message, d.freq_hz, d.dt_seconds, d.snr_db, d.sync_score
        ))?;
    }
    out.finish()?;
    eprintln!("{} decode(s)", decodes.len());
    Ok(())
}
