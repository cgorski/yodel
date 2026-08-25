//! `yodel wspr`: WSPR beacon generation and capture decoding.
//!
//! Two subcommands mirroring the library split: `gen` runs the TX
//! pipeline (message → 162 symbols → continuous-phase 4-FSK WAV) and
//! `decode` runs the std receive engine over a 12 kHz capture.

use clap::{Args, Subcommand};

use yodel::wspr::{WsprConfig, WsprDecoder, WsprDecoderConfig, WsprMessage, WsprModulator};
use yodel::{MaidenheadGrid, SampleRate};

use crate::shared::{Output, check_wav_spec};

/// Arguments of `yodel wspr`: WSPR beacon TX and capture RX.
#[derive(Args)]
pub struct WsprArgs {
    #[command(subcommand)]
    command: WsprCommand,
}

#[derive(Subcommand)]
enum WsprCommand {
    /// Generate one WSPR transmission (~110.6 s) as a 16-bit mono WAV
    /// at 12 kHz.
    Gen {
        /// Callsign (1..=6 chars, standard type-1 shape; no `/`)
        #[arg(long, value_name = "CALL")]
        callsign: String,

        /// 4-character Maidenhead grid locator, e.g. FN42
        #[arg(long, value_name = "GRID")]
        grid: String,

        /// Power in dBm [range: 0..=60, ending in 0, 3 or 7]
        #[arg(long, value_name = "DBM")]
        power: u8,

        /// Output WAV file (16-bit mono integer PCM at 12 kHz)
        #[arg(long = "out", short = 'o', value_name = "OUTPUT.wav")]
        out: String,

        /// Tone-0 audio frequency in Hz (the WSPR sub-band convention
        /// is 1400..=1600)
        #[arg(long = "offset-hz", value_name = "HZ", default_value_t = 1_500)]
        offset_hz: u32,
    },
    /// Decode WSPR transmissions from a 12 kHz 16-bit mono WAV capture
    /// (at least ~110.6 s): one line per decode on stdout.
    Decode {
        /// Input WAV file (16-bit mono integer PCM; 12 kHz only — the
        /// engine is fixed-rate, resample externally if needed)
        #[arg(value_name = "INPUT.wav")]
        input: String,

        /// Search half-width around 1500 Hz, in Hz [range: 10..=100]
        #[arg(long, value_name = "HZ", default_value_t = 100)]
        window: u32,

        /// Maximum decodes to attempt per capture [range: 1..=16]
        #[arg(long, value_name = "N", default_value_t = 3)]
        max_candidates: usize,
    },
}

/// Runs `yodel wspr`.
pub fn wspr(args: &WsprArgs) -> Result<(), String> {
    match &args.command {
        WsprCommand::Gen {
            callsign,
            grid,
            power,
            out,
            offset_hz,
        } => generate(callsign, grid, *power, out, *offset_hz),
        WsprCommand::Decode {
            input,
            window,
            max_candidates,
        } => decode(input, *window, *max_candidates),
    }
}

/// WSPR's fixed capture/synthesis rate.
const RATE_HZ: u32 = 12_000;

fn generate(
    callsign: &str,
    grid: &str,
    power: u8,
    out: &str,
    offset_hz: u32,
) -> Result<(), String> {
    // Text at the boundary, a validated locator everywhere inside: the
    // typed grid is also what makes `--callsign`/`--grid` impossible to
    // hand to `WsprMessage::new` in the wrong order.
    let locator = MaidenheadGrid::new(grid).map_err(|e| format!("bad --grid '{grid}': {e}"))?;
    let msg =
        WsprMessage::new(callsign, locator, power).map_err(|e| format!("bad message: {e}"))?;
    let rate = SampleRate::new(RATE_HZ).expect("12 kHz is in range");
    let config = WsprConfig::new(offset_hz, rate)
        .map_err(|e| format!("bad --offset-hz '{offset_hz}': {e}"))?;
    let tx = WsprModulator::for_message(config, &msg);
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
            "unsupported WAV sample rate in '{input}': got {} Hz, the WSPR decoder is \
             fixed at {RATE_HZ} Hz (resample the capture externally)",
            rate.hz()
        ));
    }
    let samples: Vec<i16> = reader
        .samples::<i16>()
        .collect::<Result<_, _>>()
        .map_err(|e| format!("reading '{input}': {e}"))?;
    let config = WsprDecoderConfig::new(1_500, window)
        .map_err(|e| format!("bad --window '{window}': {e}"))?
        .max_candidates(max_candidates)
        .map_err(|e| format!("bad --max-candidates '{max_candidates}': {e}"))?;
    let decoder = WsprDecoder::new(config);
    let decodes = decoder
        .decode(&samples)
        .map_err(|e| format!("decoding '{input}': {e}"))?;
    let mut out = Output::new();
    for d in &decodes {
        let call = String::from_utf8_lossy(d.message.callsign())
            .trim()
            .to_owned();
        let grid = d.message.grid();
        out.line(format_args!(
            "{call} {grid} {} dBm | freq {:.1} Hz | dt {:.2} s | snr {:.0} dB | sync {:.2}",
            d.message.power_dbm(),
            d.freq_hz,
            d.dt_seconds,
            d.snr_db,
            d.sync_score
        ))?;
    }
    out.finish()?;
    eprintln!("{} decode(s)", decodes.len());
    Ok(())
}
