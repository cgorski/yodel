//! `yodel decode`: WAV or raw-PCM audio in, one line per decoded
//! AX.25/APRS frame on stdout, receive statistics on stderr.
//!
//! Two renderings of that line, selected by `--output-format`: the
//! human `SRC>DEST,PATH: summary` text (the default, unchanged), and
//! JSON Lines (one self-contained JSON object per frame) built by
//! [`crate::json`].

use clap::{Args, ValueEnum};

use yodel::SampleRate;
use yodel::aprs::monitor::MonitorLine;
use yodel::aprs::{
    AprsPacket, Decoded, DecodedKind, MessageContent, NmeaSentence, TelemetryDefinition,
    WeatherReport, decoded_from_ui,
};
use yodel::ax25::UiFrame;
use yodel::demodulator::{AfskDemodulator, DemodulatorConfig};
use yodel::fx25::{Fx25Error, Fx25Receiver};
use yodel::il2p::Il2pReceiver;
use yodel::nrzi::NrziDecoder;
use yodel::tnc::{DefaultTncReceiver, MAX_FRAME_BYTES, TncConfig};

use crate::json::{self, StreamPos};
use crate::shared::{
    IL2P_PARITY, InputFormat, ModemArgs, Output, check_wav_spec, format_address,
    sniff_stdin_samples, wav_samples,
};

#[derive(Args)]
pub struct DecodeArgs {
    /// Input: a WAV file path (16-bit mono integer PCM,
    /// 8000..=48000 Hz), or `-` to read audio from stdin. Stdin
    /// carrying a RIFF/WAV header is decoded as WAV; anything else is
    /// raw PCM (see --format), read continuously until EOF so live
    /// pipes from capture tools work.
    #[arg(value_name = "INPUT.wav | -")]
    input: String,

    /// Sample rate of raw PCM on stdin in Hz [range: 8000..=48000].
    /// Required for raw stdin input (raw PCM carries no rate);
    /// rejected for WAV input, which carries its own.
    #[arg(long = "sample-rate", visible_alias = "rate", value_name = "HZ")]
    sample_rate: Option<u32>,

    /// Raw stdin PCM sample encoding [default: s16le]. Only applies
    /// to `-` input without a WAV header.
    #[arg(long, value_enum, value_name = "FORMAT")]
    format: Option<InputFormat>,

    /// How to render each decoded frame on stdout: `text` is the
    /// human `SRC>DEST,PATH: summary` line; `jsonl` is JSON Lines
    /// (NDJSON) — one self-contained JSON object per frame, one per
    /// line, for `jq`/log pipelines. The schema is in README.md.
    ///
    /// (Spelled `--output-format` rather than `--format` because
    /// `--format` already names the raw-PCM *input* encoding above,
    /// and is documented and tested under that meaning.)
    #[arg(long = "output-format", value_enum, value_name = "FORMAT", default_value_t = OutputFormat::Text)]
    output_format: OutputFormat,

    /// Add a wall-clock `unix_time` (seconds since the Unix epoch, as
    /// a number) to each `jsonl` line. **Off by default**: without it
    /// a decode of a given recording is byte-for-byte reproducible,
    /// which is what makes the output pinnable in a test and diffable
    /// between runs. Turn it on for a live capture, where
    /// when-it-was-heard is real information.
    #[arg(long)]
    wall_clock: bool,

    /// Read the input as **TNC2 monitor text** rather than audio: one
    /// `SRC>DEST,PATH:information` line per packet, which is what an
    /// APRS-IS feed, a TNC in monitor mode and most log files emit.
    /// Use `-` for stdin.
    ///
    /// No modem runs in this mode, so the audio flags do not apply.
    /// Lines beginning with `#` are treated as server comments and
    /// skipped, which is what APRS-IS interleaves with the data.
    #[arg(long)]
    tnc2: bool,

    /// With `--tnc2`, rebuild every packet that decoded to a typed
    /// payload and compare it against the bytes that arrived, adding a
    /// `"rebuild"` field of `exact`, `differs` or `failed`.
    ///
    /// This measures the crate's byte-exactness invariant over real
    /// traffic. It is also the control for a parser relaxation: a packet
    /// that starts decoding but does not rebuild is being read
    /// differently from how its sender wrote it, which is a misreading
    /// rather than a fix.
    #[arg(long, requires = "tnc2")]
    verify_rebuild: bool,

    #[command(flatten)]
    modem: ModemArgs,
}

/// How `yodel decode` renders each frame on stdout.
#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputFormat {
    /// `SRC>DEST,PATH: <summary>`, one line per frame.
    #[value(name = "text")]
    Text,
    /// JSON Lines / NDJSON: one self-contained JSON object per frame.
    #[value(name = "jsonl")]
    Jsonl,
}

/// Renders decoded frames in whichever `--output-format` was asked for.
///
/// Holds the reusable line buffer so JSONL output costs one growing
/// allocation for the whole run rather than one per frame, and holds
/// the sample rate, which only the JSONL path needs (to turn a sample
/// index into seconds).
struct Emitter {
    format: OutputFormat,
    wall_clock: bool,
    rate: SampleRate,
    line: String,
    out: Output,
}

impl Emitter {
    /// Builds an emitter from the parsed arguments.
    fn new(args: &DecodeArgs, rate: SampleRate) -> Self {
        Self {
            format: args.output_format,
            wall_clock: args.wall_clock,
            rate,
            line: String::new(),
            out: Output::new(),
        }
    }

    /// Prints one frame, decoded at sample index `sample`.
    fn emit(&mut self, sample: u64, frame: &UiFrame<'_>) -> Result<(), String> {
        match self.format {
            OutputFormat::Text => self.out.line(format_args!("{}", format_frame(frame))),
            OutputFormat::Jsonl => {
                self.line.clear();
                let at = StreamPos {
                    sample,
                    rate: self.rate,
                    unix_time: self.wall_clock.then(unix_time),
                };
                json::push_frame(&mut self.line, at, frame);
                // `self.line` is borrowed by `format_args!` while
                // `self.out` is borrowed mutably, so split the borrow.
                let Self { out, line, .. } = self;
                out.line(format_args!("{line}"))
            }
        }
    }

    /// Whether the downstream reader has gone away, so the decode loop
    /// can stop instead of grinding through a capture nobody reads.
    fn closed(&self) -> bool {
        self.out.closed()
    }

    /// Flushes the buffered output.
    fn finish(&mut self) -> Result<(), String> {
        self.out.finish()
    }
}

/// Seconds since the Unix epoch, for `--wall-clock`.
///
/// The only clock reading in the whole decode path, and it happens only
/// when the operator asked for it. A clock before the epoch (a
/// misconfigured machine) reads as a negative number rather than
/// panicking.
fn unix_time() -> f64 {
    match std::time::SystemTime::UNIX_EPOCH.elapsed() {
        Ok(d) => d.as_secs_f64(),
        Err(e) => -e.duration().as_secs_f64(),
    }
}

/// Runs `yodel decode`: dispatches on the input kind (WAV file path
/// or `-` for stdin) and feeds the shared sample-decode core.
pub fn decode(args: &DecodeArgs) -> Result<(), String> {
    if args.wall_clock && args.output_format != OutputFormat::Jsonl {
        // The text line has nowhere to put a timestamp, so accepting
        // the flag here would silently do nothing.
        return Err(
            "--wall-clock applies only to --output-format jsonl (the text output has no \
             timestamp field)"
                .to_owned(),
        );
    }
    if args.tnc2 {
        return decode_tnc2(args);
    }
    if args.input == "-" {
        return decode_stdin(args);
    }
    // A WAV file path: the header carries the rate and encoding, so
    // the stdin-only flags are rejected instead of silently ignored.
    if args.sample_rate.is_some() {
        return Err(
            "--sample-rate applies only to raw PCM on stdin ('-'); WAV files carry \
             their own sample rate"
                .to_owned(),
        );
    }
    if args.format.is_some() {
        return Err(
            "--format applies only to raw PCM on stdin ('-'); WAV files carry their \
             own encoding"
                .to_owned(),
        );
    }
    let input = args.input.as_str();
    let mut reader =
        hound::WavReader::open(input).map_err(|e| format!("opening '{input}': {e}"))?;
    let rate = check_wav_spec(&reader.spec(), input)?;
    let config = args.modem.config(rate)?;
    let samples = wav_samples(&mut reader, input);
    decode_samples(args, rate, config, samples)
}

/// Decodes audio arriving on stdin. The first four bytes are sniffed
/// (shared with `yodel serve --input -` via
/// [`crate::shared::sniff_stdin_samples`]): a `RIFF` header means WAV
/// (rate and encoding from the header, checked against any
/// `--sample-rate` also given), anything else is raw PCM
/// (`--sample-rate` required, `--format` selecting the encoding), read
/// continuously until EOF so live pipes work.
/// Decodes TNC2 monitor text: one packet per line, no modem involved.
///
/// The addresses stay as text because APRS-IS traffic is not bound by
/// AX.25 address rules; see [`yodel::aprs::monitor`]. Input is read as
/// bytes rather than as a string, because Mic-E reports are binary and
/// comment fields carry bare Latin-1, so a feed is not valid UTF-8.
fn decode_tnc2(args: &DecodeArgs) -> Result<(), String> {
    use std::io::{BufRead, BufReader, Write};

    let input: Box<dyn BufRead> = if args.input == "-" {
        Box::new(BufReader::new(std::io::stdin()))
    } else {
        Box::new(BufReader::new(
            std::fs::File::open(&args.input)
                .map_err(|e| format!("cannot open {}: {e}", args.input))?,
        ))
    };

    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let mut line = String::new();
    let (mut n, mut decoded, mut skipped) = (0u64, 0u64, 0u64);

    for raw in input.split(b'\n') {
        let mut raw = raw.map_err(|e| format!("read error: {e}"))?;
        while matches!(raw.last(), Some(b'\r')) {
            raw.pop();
        }
        if raw.is_empty() || raw[0] == b'#' {
            continue;
        }
        n += 1;
        let Ok(parsed) = MonitorLine::parse(&raw) else {
            skipped += 1;
            continue;
        };
        decoded += 1;
        match args.output_format {
            OutputFormat::Jsonl => {
                line.clear();
                json::push_monitor_line(&mut line, n, &parsed, args.verify_rebuild);
                writeln!(out, "{line}").map_err(|e| e.to_string())?;
            }
            OutputFormat::Text => {
                let d = parsed.decoded();
                writeln!(
                    out,
                    "{}>{}{}{}: {}",
                    String::from_utf8_lossy(parsed.source),
                    String::from_utf8_lossy(parsed.dest),
                    if parsed.path.is_empty() { "" } else { "," },
                    String::from_utf8_lossy(parsed.path),
                    summarize_kind(&d.kind, d.info),
                )
                .map_err(|e| e.to_string())?;
            }
        }
    }
    out.flush().map_err(|e| e.to_string())?;
    eprintln!("{n} lines, {decoded} parsed, {skipped} unparseable");
    Ok(())
}

fn decode_stdin(args: &DecodeArgs) -> Result<(), String> {
    // `--format` defaults to (and today only offers) s16le.
    let InputFormat::S16le = args.format.unwrap_or(InputFormat::S16le);
    let (rate, samples) = sniff_stdin_samples(std::io::stdin(), args.sample_rate)?;
    let config = args.modem.config(rate)?;
    decode_samples(args, rate, config, samples)
}

/// Decodes an already-sniffed sample stream with the human output
/// format, for `yodel level --then-decode`.
///
/// Lives here rather than in `level` because `DecodeArgs` is private to
/// this module: the alternative is making its fields public, which
/// would let any subcommand assemble a decode configuration that never
/// went through clap's validation.
pub(crate) fn decode_metered(
    modem: &ModemArgs,
    rate: SampleRate,
    samples: impl Iterator<Item = Result<i16, String>>,
) -> Result<(), String> {
    let args = DecodeArgs {
        input: "-".to_string(),
        sample_rate: None,
        format: None,
        output_format: OutputFormat::Text,
        wall_clock: false,
        verify_rebuild: false,
        tnc2: false,
        modem: modem.clone(),
    };
    let config = args.modem.config(rate)?;
    decode_samples(&args, rate, config, samples)
}

/// The shared decode core: pushes samples through the receiver, prints
/// one line per frame on stdout and the statistics on stderr. The
/// output is identical for every input kind.
///
/// Each path enumerates its input samples so a frame can be identified
/// by *where in the stream* it completed rather than by the time of
/// day; see [`crate::json`]. The index advances once per input sample
/// on every path, including the samples the demodulator swallows
/// without producing a bit, so it means the same thing whichever
/// receive chain is in use.
fn decode_samples(
    args: &DecodeArgs,
    rate: SampleRate,
    config: TncConfig,
    samples: impl Iterator<Item = Result<i16, String>>,
) -> Result<(), String> {
    let mut out = Emitter::new(args, rate);
    if args.modem.fx25 {
        return decode_fx25(&mut out, rate, config, samples);
    }
    if args.modem.il2p {
        return decode_il2p(&mut out, rate, config, samples);
    }
    let mut rx = DefaultTncReceiver::new(config).map_err(|e| format!("receiver setup: {e}"))?;
    for (at, sample) in samples.enumerate() {
        if let Some(frame) = rx.push_i16(sample?) {
            out.emit(at as u64, frame.ui_frame())?;
            if out.closed() {
                break;
            }
        }
    }
    out.finish()?;
    let stats = rx.stats();
    eprintln!(
        "frames ok: {}, fcs errors: {}",
        stats.frames_ok, stats.fcs_errors
    );
    Ok(())
}

/// The FX.25-aware decode path: demodulator → NRZI → tag hunter with a
/// parallel plain-HDLC path, so both FX.25 and plain AX.25 frames on
/// the same recording decode.
fn decode_fx25(
    out: &mut Emitter,
    rate: SampleRate,
    config: TncConfig,
    samples: impl Iterator<Item = Result<i16, String>>,
) -> Result<(), String> {
    let demod_config = DemodulatorConfig::new(rate, config.baud(), config.tones())
        .map_err(|e| format!("receiver setup: {e}"))?;
    let mut demod =
        AfskDemodulator::new(demod_config).map_err(|e| format!("receiver setup: {e}"))?;
    let mut nrzi = NrziDecoder::default();
    let mut rx = Fx25Receiver::<MAX_FRAME_BYTES>::new();
    let mut frames_ok = 0u32;
    let mut fcs_errors = 0u32;
    for (at, sample) in samples.enumerate() {
        let Some(line) = demod.push_sample_i16(sample?) else {
            continue;
        };
        match rx.push(nrzi.decode(line)) {
            Some(Ok(frame)) => {
                let frame = frame.to_vec();
                if let Ok(ui) = UiFrame::parse(&frame) {
                    frames_ok += 1;
                    out.emit(at as u64, &ui)?;
                    if out.closed() {
                        break;
                    }
                }
            }
            Some(Err(Fx25Error::Ax25(yodel::ax25::Ax25Error::FcsMismatch { .. }))) => {
                fcs_errors += 1;
            }
            _ => {}
        }
    }
    out.finish()?;
    eprintln!("frames ok: {frames_ok}, fcs errors: {fcs_errors}");
    Ok(())
}

/// The IL2P decode path: demodulator → NRZI → sync-word receiver at
/// the CLI's fixed 16-parity operating point. IL2P is not
/// AX.25-compatible on the air, so this path decodes only IL2P
/// transmissions (use the default or --fx25 paths for HDLC traffic).
fn decode_il2p(
    out: &mut Emitter,
    rate: SampleRate,
    config: TncConfig,
    samples: impl Iterator<Item = Result<i16, String>>,
) -> Result<(), String> {
    let demod_config = DemodulatorConfig::new(rate, config.baud(), config.tones())
        .map_err(|e| format!("receiver setup: {e}"))?;
    let mut demod =
        AfskDemodulator::new(demod_config).map_err(|e| format!("receiver setup: {e}"))?;
    // No NRZI: IL2P is not differentially encoded (spec v0.6,
    // "Interface to Physical Layer"). The demodulated bits feed the
    // receiver directly.
    let mut rx = Box::new(Il2pReceiver::new(IL2P_PARITY));
    let mut frames_ok = 0u32;
    let mut uncorrectable = 0u32;
    let mut corrected = 0u64;
    for (at, sample) in samples.enumerate() {
        let Some(line) = demod.push_sample_i16(sample?) else {
            continue;
        };
        match rx.push(line) {
            Some(Ok(frame)) => {
                corrected += frame.corrected() as u64;
                if let Ok(ui) = frame.ui_frame() {
                    frames_ok += 1;
                    out.emit(at as u64, &ui)?;
                    if out.closed() {
                        break;
                    }
                }
            }
            Some(Err(_)) => uncorrectable += 1,
            None => {}
        }
    }
    out.finish()?;
    eprintln!(
        "frames ok: {frames_ok}, uncorrectable: {uncorrectable}, symbols corrected: {corrected}"
    );
    Ok(())
}

/// Renders one decoded frame as `SRC>DEST,PATH: <summary>`.
fn format_frame(frame: &UiFrame<'_>) -> String {
    let mut head = format!(
        "{}>{}",
        format_address(&frame.src),
        format_address(&frame.dest)
    );
    for digi in frame.path() {
        head.push(',');
        head.push_str(&format_address(digi));
    }
    format!("{head}: {}", summarize(frame))
}

/// Pretty-prints the frame payload.
///
/// Frame-level rather than field-level, because Mic-E is: the
/// destination callsign carries half its position, so `decoded_from_ui`
/// is the only decode that can see it.
fn summarize(frame: &UiFrame<'_>) -> String {
    summarize_kind(&decoded_from_ui(frame).kind, frame.info)
}

/// Pretty-prints one APRS information field, with no frame around it.
///
/// Split out from [`summarize`] so a third-party packet can render its
/// encapsulated payload with the same code. Only ever recurses one
/// level: the inner call sees a payload whose own third-party arm would
/// need a *second* `}`, which real traffic does not carry.
fn summarize_info(info: &[u8]) -> String {
    summarize_kind(&Decoded::decode(info).kind, info)
}

/// Renders one decode outcome; the shared body of [`summarize`] and
/// [`summarize_info`].
///
/// The total decode: Mic-E, raw NMEA, Ultimeter weather and third-party
/// traffic are all real APRS payloads that `AprsPacket` does not model,
/// so summarize them here rather than falling through to `raw`.
fn summarize_kind(kind: &DecodedKind<'_>, info: &[u8]) -> String {
    match *kind {
        DecodedKind::Packet(ref packet) => summarize_packet(packet),
        // `coordinates()` rather than the fields: Mic-E sends the
        // longitude at full precision whatever ambiguity the
        // destination declares, and applying that declaration is the
        // receiver's job (chapter 10).
        DecodedKind::MicE(ref m) => format!(
            "Mic-E position lat {:.4} lon {:.4} speed {} kn course {} deg status \"{}\"",
            m.coordinates().latitude.to_degrees(),
            m.coordinates().longitude.to_degrees(),
            m.speed,
            m.course,
            printable(m.status)
        ),
        DecodedKind::Nmea(ref sentence) => summarize_nmea(sentence),
        DecodedKind::Ultimeter(ref record) => format!(
            "ultimeter {:?} {}",
            record.format(),
            summarize_weather(&record.to_weather_report())
        ),
        DecodedKind::ThirdParty(ref tp) => format!(
            "third-party {}>{}{}{}: {}",
            printable(tp.source),
            printable(tp.dest),
            if tp.path.is_empty() { "" } else { "," },
            printable(tp.path),
            summarize_info(tp.payload)
        ),
        // Not APRS: a station identification, a beacon banner or a
        // human-written bulletin. Named as text rather than reported
        // with a data type identifier it does not have.
        DecodedKind::Text { text } => format!("text \"{}\"", printable(text)),
        // `DecodedKind` is `#[non_exhaustive]`, and anything we cannot
        // name is still printable as its original bytes.
        _ => format!("raw \"{}\"", printable(info)),
    }
}

/// Replaces non-printable bytes so raw payloads are safe to print.
fn printable(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| {
            if (b' '..=b'~').contains(&b) {
                b as char
            } else {
                '.'
            }
        })
        .collect()
}

/// One-line summary of a parsed APRS packet.
/// Positions go through `coordinates()`, never the fields: it masks
/// both axes to the chapter 6 precision the sender declared, and
/// chapter 6 lets the longitude carry its digits in full beside a
/// blanked latitude.
fn summarize_packet(packet: &AprsPacket<'_>) -> String {
    match *packet {
        AprsPacket::Position(ref p) => format!(
            "position lat {:.4} lon {:.4} symbol {}{} comment \"{}\"",
            p.coordinates().latitude.to_degrees(),
            p.coordinates().longitude.to_degrees(),
            p.symbol.to_wire().0 as char,
            p.symbol.to_wire().1 as char,
            printable(p.comment)
        ),
        AprsPacket::PositionCs(ref p) => format!(
            "position lat {:.4} lon {:.4} symbol {}{} comment \"{}\"",
            p.coordinates().latitude.to_degrees(),
            p.coordinates().longitude.to_degrees(),
            p.position.symbol.to_wire().0 as char,
            p.position.symbol.to_wire().1 as char,
            printable(p.position.comment)
        ),
        AprsPacket::PositionTimestamped(ref p) => format!(
            "timestamped position lat {:.4} lon {:.4} symbol {}{} comment \"{}\"",
            p.coordinates().latitude.to_degrees(),
            p.coordinates().longitude.to_degrees(),
            p.position.symbol.to_wire().0 as char,
            p.position.symbol.to_wire().1 as char,
            printable(p.position.comment)
        ),
        AprsPacket::PositionWeather(ref w) => format!(
            "weather at lat {:.4} lon {:.4} {}",
            w.coordinates().latitude.to_degrees(),
            w.coordinates().longitude.to_degrees(),
            summarize_weather(&w.weather)
        ),
        AprsPacket::Weather(ref w) => format!(
            "weather {:02}-{:02} {:02}:{:02}z {}",
            w.month,
            w.day,
            w.hour,
            w.minute,
            summarize_weather(&w.weather)
        ),
        AprsPacket::Telemetry(ref t) => {
            use std::fmt::Write as _;

            // A channel the sender did not send prints as `-`, not as
            // `0`, and a report with no digital field prints `none`
            // rather than eight clear bits it never asserted.
            let mut analog = String::new();
            for (index, value) in t.analog.iter().enumerate() {
                if index > 0 {
                    analog.push_str(", ");
                }
                match *value {
                    Some(value) => {
                        let _ = write!(analog, "{value}");
                    }
                    None => analog.push('-'),
                }
            }
            let digital: String = match t.digital {
                Some(bits) => bits.iter().map(|&b| if b { '1' } else { '0' }).collect(),
                None => "none".to_string(),
            };
            format!(
                "telemetry seq {} analog [{analog}] digital {digital}",
                t.seq
            )
        }
        AprsPacket::Object(ref o) => format!(
            "object \"{}\" ({}) lat {:.4} lon {:.4} comment \"{}\"",
            printable(o.name),
            if o.live { "live" } else { "killed" },
            o.coordinates().latitude.to_degrees(),
            o.coordinates().longitude.to_degrees(),
            printable(o.comment)
        ),
        AprsPacket::Item(ref i) => format!(
            "item \"{}\" ({}) lat {:.4} lon {:.4} comment \"{}\"",
            printable(i.name),
            if i.live { "live" } else { "killed" },
            i.coordinates().latitude.to_degrees(),
            i.coordinates().longitude.to_degrees(),
            printable(i.comment)
        ),
        AprsPacket::Status(ref s) => format!("status \"{}\"", printable(s.text)),
        AprsPacket::Capabilities(ref c) => {
            format!("capabilities \"{}\"", printable(c.body))
        }
        AprsPacket::Message(ref m) => {
            /// Which of chapter 13's four definition messages this is.
            fn definition_kind(d: &TelemetryDefinition<'_>) -> &'static str {
                match *d {
                    TelemetryDefinition::Parameters(_) => "PARM",
                    TelemetryDefinition::Units(_) => "UNIT",
                    TelemetryDefinition::Equations(_) => "EQNS",
                    TelemetryDefinition::BitSense(_) => "BITS",
                }
            }

            /// The typed content, one line.
            fn summarize_definition(d: &TelemetryDefinition<'_>) -> String {
                use std::fmt::Write as _;

                match *d {
                    TelemetryDefinition::Parameters(ref l) | TelemetryDefinition::Units(ref l) => {
                        let mut out = String::new();
                        for label in l.analog.iter().chain(l.digital.iter()) {
                            if !out.is_empty() {
                                out.push_str(", ");
                            }
                            match *label {
                                Some(label) => {
                                    let _ = write!(out, "\"{}\"", printable(label));
                                }
                                None => out.push('-'),
                            }
                        }
                        out
                    }
                    TelemetryDefinition::Equations(ref e) => {
                        let mut out = String::new();
                        for channel in 0..5 {
                            if let Some((a, b, c)) = e.channel(channel) {
                                if !out.is_empty() {
                                    out.push_str(", ");
                                }
                                let _ = write!(out, "a{channel} {a}x^2+{b}x+{c}");
                            }
                        }
                        if out.is_empty() {
                            out.push_str("no complete channel");
                        }
                        out
                    }
                    TelemetryDefinition::BitSense(ref b) => {
                        let sense: String = match b.sense {
                            Some(s) => s.iter().map(|&x| if x { '1' } else { '0' }).collect(),
                            None => "none".to_string(),
                        };
                        format!("sense {sense} project \"{}\"", printable(b.title))
                    }
                }
            }

            let to = printable(m.addressee.as_bytes());
            // A chapter 13 definition is a view over the text, so it is
            // named before the text rather than instead of it.
            if let Some(definition) = m.telemetry_definition() {
                return format!(
                    "telemetry {} for the SENDER (addressed to {to}) {}",
                    definition_kind(&definition),
                    summarize_definition(&definition)
                );
            }
            match m.content {
                MessageContent::Text { text, id } => match id {
                    Some(id) => format!(
                        "message to {to} \"{}\" id {}",
                        printable(text),
                        printable(id)
                    ),
                    None => format!("message to {to} \"{}\"", printable(text)),
                },
                MessageContent::Ack { id } => {
                    format!("ack to {to} id {}", printable(id))
                }
                MessageContent::Reject { id } => {
                    format!("rej to {to} id {}", printable(id))
                }
            }
        }
        // `AprsPacket` is `#[non_exhaustive]`; a data type added later
        // still prints usefully rather than failing to compile here.
        _ => "unrecognized packet".to_string(),
    }
}

/// One-line summary of a raw NMEA sentence.
fn summarize_nmea(sentence: &NmeaSentence<'_>) -> String {
    let talker = printable(&sentence.talker.as_bytes());
    let formatter = printable(&sentence.formatter().as_bytes());
    match sentence.position() {
        Some(at) => format!(
            "NMEA {talker}{formatter} lat {:.4} lon {:.4}",
            at.latitude.to_degrees(),
            at.longitude.to_degrees()
        ),
        None => format!("NMEA {talker}{formatter} (no position)"),
    }
}

/// Summarizes the present weather fields as `name value` pairs.
///
/// The quantities carry no unit-less `Display` by design, so every
/// line below names the unit it asked for. Both systems are printed
/// where they differ, since a monitor line is read by people in
/// countries that disagree about degrees.
///
/// **Every measurement the JSON writer emits is rendered here too, in
/// the same wire order** — see [`crate::json::push_weather_fields`].
/// The two writers are the same projection of one struct, so a field
/// present in only one of them is a bug, not a style choice: this
/// function rendered nine of eleven for a while, and the two it
/// dropped (`luminosity`, `snowfall`) are exactly the two most
/// recently added. A real off-air frame carrying `L050`
/// (`src/aprs/weather.rs`) therefore decoded correctly, counted as
/// structured, and then printed as if the station had never sent it —
/// text mode losing a measurement JSON mode had. Add a field to
/// `WeatherReport` and it goes in both writers or neither.
fn summarize_weather(w: &WeatherReport) -> String {
    let mut parts = Vec::new();
    if let Some(v) = w.wind_direction {
        parts.push(format!("wind dir {v} deg"));
    }
    if let Some(v) = w.wind_speed {
        parts.push(format!("wind {} mph ({} km/h)", v.mph(), v.kmh()));
    }
    if let Some(v) = w.gust {
        parts.push(format!("gust {} mph ({} km/h)", v.mph(), v.kmh()));
    }
    if let Some(v) = w.temperature {
        parts.push(format!("temp {} F ({} C)", v.fahrenheit(), v.celsius()));
    }
    if let Some(v) = w.rain_1h {
        parts.push(format!("rain 1h {} mm", v.millimeters()));
    }
    if let Some(v) = w.rain_24h {
        parts.push(format!("rain 24h {} mm", v.millimeters()));
    }
    if let Some(v) = w.rain_midnight {
        parts.push(format!("rain mn {} mm", v.millimeters()));
    }
    if let Some(v) = w.humidity {
        parts.push(format!("humidity {}%", v.percent()));
    }
    if let Some(v) = w.barometric_pressure {
        parts.push(format!("pressure {} hPa", v.hpa()));
    }
    if let Some(v) = w.luminosity {
        parts.push(format!("luminosity {v} W/m2"));
    }
    if let Some(v) = w.snowfall {
        parts.push(format!("snow 24h {} mm", v.millimeters()));
    }
    if parts.is_empty() {
        "no data".to_owned()
    } else {
        parts.join(", ")
    }
}
