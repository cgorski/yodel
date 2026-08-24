//! `warble m17`: M17 packet-mode transmission generation and capture
//! decoding.
//!
//! Two subcommands mirroring the `warble wspr` / `warble ft8` shape:
//! `gen` runs the TX pipeline (LSF + packet superframe → RRC-shaped
//! 4-level PAM WAV at 48 kHz) and `decode` runs the streaming receiver
//! over a 48 kHz capture, printing the decoded LSF addresses, the
//! packet payload and per-frame FEC statistics.

use clap::{Args, Subcommand};

use warble::SampleRate;
use warble::m17::{Address, Lsf, M17FrameEvent, M17PacketTx, M17Receiver, PacketAssembler};

use crate::shared::{Output, check_wav_spec};

/// Arguments of `warble m17`: M17 packet-mode TX and capture RX.
#[derive(Args)]
pub struct M17Args {
    #[command(subcommand)]
    command: M17Command,
}

#[derive(Subcommand)]
enum M17Command {
    /// Generate one M17 packet-mode transmission (preamble + LSF +
    /// packet frames + EOT) as a 16-bit mono WAV at 48 kHz.
    Gen {
        /// Source callsign (base-40 alphabet, 1..=9 chars)
        #[arg(long, value_name = "CALL")]
        src: String,

        /// Destination callsign, or BROADCAST
        #[arg(long, value_name = "CALL|BROADCAST")]
        dst: String,

        /// Packet payload text (at most 823 bytes)
        #[arg(long, value_name = "PAYLOAD")]
        text: String,

        /// Channel access number [range: 0..=15]
        #[arg(long, value_name = "CAN", default_value_t = 0)]
        can: u8,

        /// Output WAV file (16-bit mono integer PCM at 48 kHz)
        #[arg(long = "out", short = 'o', value_name = "OUTPUT.wav")]
        out: String,
    },
    /// Decode M17 packet-mode transmissions from a 48 kHz 16-bit mono
    /// WAV capture: decoded LSF addresses and payload on stdout, FEC
    /// statistics on stderr.
    Decode {
        /// Input WAV file (16-bit mono integer PCM; 48 kHz only — the
        /// receiver wants an integer number of samples per 4800 Hz
        /// symbol, resample externally if needed)
        #[arg(value_name = "INPUT.wav")]
        input: String,
    },
}

/// Runs `warble m17`.
pub fn m17(args: &M17Args) -> Result<(), String> {
    match &args.command {
        M17Command::Gen {
            src,
            dst,
            text,
            can,
            out,
        } => generate(src, dst, text, *can, out),
        M17Command::Decode { input } => decode(input),
    }
}

/// The CLI's fixed capture/synthesis rate (the M17 spec's reference
/// baseband rate; 10 samples per 4800 Hz symbol).
const RATE_HZ: u32 = 48_000;

/// Parses a `--src`/`--dst` value: the literal `BROADCAST` or a
/// base-40 callsign.
fn parse_address(value: &str, flag: &str) -> Result<Address, String> {
    if value == "BROADCAST" {
        return Ok(Address::broadcast());
    }
    Address::from_callsign(value).map_err(|e| format!("bad {flag} '{value}': {e}"))
}

fn generate(src: &str, dst: &str, text: &str, can: u8, out: &str) -> Result<(), String> {
    let src = parse_address(src, "--src")?;
    if src.is_broadcast() {
        return Err("bad --src 'BROADCAST': the source must be a callsign".to_owned());
    }
    let dst = parse_address(dst, "--dst")?;
    if can > 15 {
        return Err(format!("bad --can '{can}': must be 0..=15"));
    }
    let lsf = Lsf::packet_data(dst, src, can);
    let rate = SampleRate::new(RATE_HZ).expect("48 kHz is in range");
    let mut tx = M17PacketTx::new(rate, lsf, text.as_bytes())
        .map_err(|e| format!("bad --text ({} bytes): {e}", text.len()))?;
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: RATE_HZ,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer =
        hound::WavWriter::create(out, spec).map_err(|e| format!("creating '{out}': {e}"))?;
    while let Some(s) = tx.next_i16() {
        writer
            .write_sample(s)
            .map_err(|e| format!("writing '{out}': {e}"))?;
    }
    writer
        .finalize()
        .map_err(|e| format!("finalizing '{out}': {e}"))?;
    Ok(())
}

fn decode(input: &str) -> Result<(), String> {
    let mut reader =
        hound::WavReader::open(input).map_err(|e| format!("opening '{input}': {e}"))?;
    let spec = reader.spec();
    let rate = check_wav_spec(&spec, input)?;
    if rate.hz() != RATE_HZ {
        return Err(format!(
            "unsupported WAV sample rate in '{input}': got {} Hz, the M17 receiver is \
             fixed at {RATE_HZ} Hz (resample the capture externally)",
            rate.hz()
        ));
    }
    let rate = SampleRate::new(RATE_HZ).expect("48 kHz is in range");
    let mut rx = M17Receiver::new(rate).map_err(|e| format!("building the receiver: {e}"))?;
    let mut assembler = PacketAssembler::new();
    let mut lsf_count = 0usize;
    let mut frame_count = 0usize;
    let mut packet_count = 0usize;
    let mut out = Output::new();
    for sample in reader.samples::<i16>() {
        let sample = sample.map_err(|e| format!("reading '{input}': {e}"))?;
        match rx.push_i16(sample) {
            Some(M17FrameEvent::Lsf(lsf)) => {
                lsf_count += 1;
                let mut sbuf = [0u8; 9];
                let mut dbuf = [0u8; 9];
                out.line(format_args!(
                    "LSF: {} -> {} | type {:#06x} | CAN {}",
                    lsf.src.callsign(&mut sbuf),
                    lsf.dst.callsign(&mut dbuf),
                    lsf.lsf_type,
                    (lsf.lsf_type >> 7) & 0xF
                ))?;
                assembler.start(lsf);
            }
            Some(M17FrameEvent::PacketFrame(frame)) => {
                frame_count += 1;
                if let Some(payload) = assembler.feed(&frame) {
                    packet_count += 1;
                    out.line(format_args!(
                        "payload: {}",
                        String::from_utf8_lossy(payload)
                    ))?;
                }
            }
            None => {}
        }
    }
    out.finish()?;
    eprintln!(
        "{packet_count} packet(s) | {lsf_count} LSF(s) | {frame_count} packet frame(s) \
         passed FEC"
    );
    if packet_count == 0 {
        return Err(format!(
            "no complete M17 packet decoded from '{input}' \
             ({lsf_count} LSF(s), {frame_count} packet frame(s) passed FEC)"
        ));
    }
    Ok(())
}
