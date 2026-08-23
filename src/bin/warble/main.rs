//! `warble` command-line tool: encode APRS packets to WAV files and
//! decode APRS WAV files, on top of the library's TNC pipeline.
//!
//! Built only with the `cli` aggregate feature (`wav`, `tnc`, `micE`,
//! `kiss`); see `[[bin]]` in `Cargo.toml`. Argument parsing is
//! clap-based; clap is an optional dependency activated only by the
//! `cli` feature, so `no_std` library builds never pull it in.
//!
//! Layout: this file holds the clap command tree and the dispatch;
//! each subcommand lives in its own module (`decode`, `encode`,
//! `gen.rs`, `bench`, `serve`, `wspr`, `ft8`, `m17`), with the shared modem-flag plumbing
//! (presets, per-knob overrides, WAV/PCM input helpers) in `shared`
//! and the dependency-free JSON writer behind `decode --output-format
//! jsonl` (and `bench --json`) in `json`.

use clap::{Parser, Subcommand};
use std::process::ExitCode;

mod aprsis;
mod bench;
mod decode;
mod encode;
mod ft8;
mod json;
mod level;
mod m17;
#[cfg(feature = "ptt")]
mod ptt;
// `gen` is a reserved keyword in edition 2024, so the module keeps the
// subcommand's file name but a different module name.
#[path = "gen.rs"]
mod generate;
mod serve;
mod shared;
mod wspr;

use aprsis::AprsIsArgs;
use bench::BenchArgs;
use decode::DecodeArgs;
use encode::EncodeArgs;
use ft8::Ft8Args;
use generate::GenArgs;
use m17::M17Args;
use serve::ServeArgs;
use wspr::WsprArgs;

/// AFSK (Bell 202 and friends) <-> APRS WAV tool.
///
/// Decode 16-bit mono PCM WAV recordings into AX.25/APRS frames, or
/// build an APRS packet and modulate it into a WAV file.
#[derive(Parser)]
#[command(name = "warble", version, about, max_term_width = 100)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Decode audio — a 16-bit mono PCM WAV file, or `-` for stdin
    /// (WAV, or raw s16le PCM with --sample-rate, read until EOF so
    /// live pipes work): one line per AX.25/APRS frame on stdout,
    /// receive statistics on stderr. `--output-format jsonl` swaps the
    /// human line for one JSON object per frame (see README.md).
    Decode(DecodeArgs),
    /// Build an APRS packet, modulate it and write a 16-bit mono WAV.
    Encode(EncodeArgs),
    /// Generate a deterministic multi-frame test signal with seeded
    /// impairments (noise SNR, amplitude, inter-frame gaps): a WAV
    /// file, or raw s16le PCM on stdout with `--out -`. Feeds decoders
    /// and benchmarks without a radio; pairs with `warble bench`.
    Gen(GenArgs),
    /// Measure decode accuracy over one or more WAV recordings (or
    /// directories of them): per-file and aggregate frame counts, with
    /// CI-friendly pass/fail thresholds (nonzero exit below `--min`).
    Bench(BenchArgs),
    /// Run a KISS TNC bridging audio to KISS clients: received frames
    /// go out as KISS data frames (to every TCP client, or on stdout in
    /// --stdio mode), and incoming KISS data frames are modulated into
    /// the TX audio output. Exits 0 at audio EOF, 1 on I/O failure.
    Serve(ServeArgs),
    /// WSPR beacon tools: `gen` writes one ~110.6 s transmission as a
    /// 12 kHz WAV; `decode` searches a 12 kHz capture and prints every
    /// decoded callsign/grid/power with quality metrics.
    Wspr(WsprArgs),
    /// FT8 tools: `gen` writes one ~12.64 s transmission as a 12 kHz
    /// WAV; `decode` searches a 12 kHz capture and prints every
    /// decoded message with quality metrics.
    Ft8(Ft8Args),
    /// M17 packet-mode tools: `gen` writes one packet transmission
    /// (preamble + LSF + packet frames + EOT) as a 48 kHz baseband
    /// WAV; `decode` runs the streaming receiver over a 48 kHz capture
    /// and prints the LSF addresses, payload and FEC statistics.
    M17(M17Args),
    /// Read the live APRS-IS feed from the internet and write TNC2
    /// monitor lines, the format `decode --tnc2` reads. `--filter`
    /// subscribes to a slice of the traffic (port 14580, which sends
    /// nothing without one); `--full-feed` takes everything (port
    /// 10152, which ignores filters). Login is receive-only and cannot
    /// transmit. Bound the run with `--seconds` or `--count`.
    Aprsis(AprsIsArgs),
    /// Live input meter for setting a radio's receive volume: reads the
    /// same stdin PCM every other subcommand takes and redraws rms,
    /// peak, clipped-sample count and a verdict. `--until-good <SECS>`
    /// stops once the level has held in range, `--for <SECS>` after a
    /// fixed time, `--then-decode` keeps metering and decodes the same
    /// stream. Clipping is reported separately because rms hides it.
    Level(level::LevelArgs),
    /// Key a transmitter over a serial control line (RTS or DTR) while
    /// a player sends the audio: `warble ptt --port /dev/ttyUSB0 --
    /// sox packet.wav -t alsa default`. The player owns playback, so
    /// the line is held for exactly its lifetime; `--hold <MS>` keys
    /// for a fixed time instead, to check an interface before trusting
    /// it. Every exit path releases the line, and `--max` bounds a
    /// hung player.
    #[cfg(feature = "ptt")]
    Ptt(ptt::PttArgs),
}

fn main() -> ExitCode {
    // Usage errors exit 2 (and --help/--version exit 0) via clap;
    // well-formed commands that fail (bad value, I/O, or a `bench`
    // result below its threshold) exit 1.
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => e.exit(),
    };
    let result = match cli.command {
        Command::Decode(args) => decode::decode(&args),
        Command::Encode(args) => encode::encode(&args),
        Command::Gen(args) => generate::generate(&args),
        Command::Bench(args) => bench::bench(&args),
        Command::Serve(args) => serve::serve_command(&args),
        Command::Wspr(args) => wspr::wspr(&args),
        Command::Ft8(args) => ft8::ft8(&args),
        Command::M17(args) => m17::m17(&args),
        Command::Aprsis(args) => aprsis::aprsis(&args),
        Command::Level(args) => level::level(&args),
        #[cfg(feature = "ptt")]
        Command::Ptt(args) => ptt::ptt(&args),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("error: {msg}");
            ExitCode::FAILURE
        }
    }
}
