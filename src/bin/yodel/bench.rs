//! `yodel bench`: decode-accuracy measurement over WAV recordings
//! with CI-friendly thresholds.
//!
//! (`bench` was chosen over `accuracy` for brevity and because "decode
//! benchmark" is what the report is; the long help spells out
//! that it measures accuracy, not speed.)

use clap::Args;

use yodel::ax25::UiFrame;
use yodel::demodulator::{AfskDemodulator, DemodulatorConfig};
use yodel::fx25::Fx25Receiver;
use yodel::nrzi::NrziDecoder;
use yodel::tnc::{DefaultTncReceiver, MAX_FRAME_BYTES};

use crate::shared::{ModemArgs, Output, check_wav_spec, wav_samples};

/// Arguments of `yodel bench`: decode-accuracy measurement over WAV
/// recordings with CI-friendly thresholds.
#[derive(Args)]
pub struct BenchArgs {
    /// WAV files and/or directories; each directory contributes the
    /// `.wav` files directly inside it (sorted by name)
    #[arg(value_name = "WAV|DIR", required = true)]
    inputs: Vec<String>,

    /// Expected frame count per file. Without it, the expectation is
    /// recovered from the `[i/N]` counter that `yodel gen` embeds in
    /// its frames (when at least one frame decodes).
    #[arg(long, value_name = "N")]
    expect: Option<u32>,

    /// Aggregate pass threshold: an absolute decoded-frame count
    /// (e.g. `18`) or a percentage of the expected total (e.g. `95%`,
    /// which requires every file to have a known expectation). Below
    /// the threshold the command exits nonzero, so it drops straight
    /// into CI. Without the flag the report is informational only.
    #[arg(long, value_name = "COUNT|PCT%")]
    min: Option<String>,

    /// Emit a machine-readable JSON report on stdout instead of the
    /// human table
    #[arg(long)]
    json: bool,

    #[command(flatten)]
    modem: ModemArgs,
}

/// The decode outcome of one WAV file.
struct FileResult {
    path: String,
    decoded: u32,
    /// Expected frame count: `--expect` if given, otherwise recovered
    /// from the `[i/N]` counter `yodel gen` embeds; `None` if neither.
    expected: Option<u32>,
}

/// The `--min` pass threshold on the aggregate decoded-frame count.
enum Threshold {
    /// An absolute count, e.g. `--min 18`.
    Count(u64),
    /// A percentage of the expected total, e.g. `--min 95%`.
    Percent(f64),
}

/// Parses `--min`: a bare integer is a count, a `%`-suffixed number a
/// percentage of the expected total.
fn parse_min(text: &str) -> Result<Threshold, String> {
    if let Some(pct) = text.strip_suffix('%') {
        let value: f64 = pct
            .parse()
            .map_err(|_| format!("bad --min '{text}': a number before the '%' is required"))?;
        if !(0.0..=100.0).contains(&value) {
            return Err(format!(
                "bad --min '{text}': the range 0%..=100% is required"
            ));
        }
        return Ok(Threshold::Percent(value));
    }
    text.parse()
        .map(Threshold::Count)
        .map_err(|_| format!("bad --min '{text}': a frame count or a percentage like '95%'"))
}

/// Expands the positional inputs: files pass through, directories
/// contribute the `.wav` files directly inside them, sorted by name.
fn expand_inputs(inputs: &[String]) -> Result<Vec<String>, String> {
    let mut files = Vec::new();
    for input in inputs {
        let meta = std::fs::metadata(input).map_err(|e| format!("reading '{input}': {e}"))?;
        if !meta.is_dir() {
            files.push(input.clone());
            continue;
        }
        let mut wavs = Vec::new();
        let entries = std::fs::read_dir(input).map_err(|e| format!("reading '{input}': {e}"))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("reading '{input}': {e}"))?;
            let path = entry.path();
            if path.is_file()
                && path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("wav"))
            {
                wavs.push(path.to_string_lossy().into_owned());
            }
        }
        if wavs.is_empty() {
            return Err(format!("no .wav files in directory '{input}'"));
        }
        wavs.sort();
        files.append(&mut wavs);
    }
    Ok(files)
}

/// Recovers the total `N` from a trailing `[i/N]` counter in a frame's
/// info field (the shape `yodel gen` emits), if present.
fn counter_total(info: &[u8]) -> Option<u32> {
    let text = std::str::from_utf8(info).ok()?.trim_end();
    let body = text.strip_suffix(']')?;
    let open = body.rfind('[')?;
    let (index, total) = body[open + 1..].split_once('/')?;
    let _: u32 = index.parse().ok()?;
    total.parse().ok().filter(|&n| n > 0)
}

/// Decodes one WAV file with the shared modem settings and counts the
/// frames, inferring the expected count from `[i/N]` counters.
fn bench_file(path: &str, modem: &ModemArgs, expect: Option<u32>) -> Result<FileResult, String> {
    let mut reader = hound::WavReader::open(path).map_err(|e| format!("opening '{path}': {e}"))?;
    let rate = check_wav_spec(&reader.spec(), path)?;
    let config = modem.config(rate)?;
    let mut decoded = 0u32;
    let mut inferred: Option<u32> = None;
    let samples = wav_samples(&mut reader, path);
    if modem.fx25 {
        let demod_config = DemodulatorConfig::new(rate, config.baud(), config.tones())
            .map_err(|e| format!("receiver setup: {e}"))?;
        let mut demod =
            AfskDemodulator::new(demod_config).map_err(|e| format!("receiver setup: {e}"))?;
        let mut nrzi = NrziDecoder::default();
        let mut rx = Fx25Receiver::<MAX_FRAME_BYTES>::new();
        for sample in samples {
            let Some(line) = demod.push_sample_i16(sample?) else {
                continue;
            };
            if let Some(Ok(frame)) = rx.push(nrzi.decode(line)) {
                let frame = frame.to_vec();
                if let Ok(ui) = UiFrame::parse(&frame) {
                    decoded += 1;
                    if inferred.is_none() {
                        inferred = counter_total(ui.info);
                    }
                }
            }
        }
    } else {
        let mut rx = DefaultTncReceiver::new(config).map_err(|e| format!("receiver setup: {e}"))?;
        for sample in samples {
            if let Some(frame) = rx.push_i16(sample?) {
                decoded += 1;
                if inferred.is_none() {
                    inferred = counter_total(frame.ui_frame().info);
                }
            }
        }
    }
    Ok(FileResult {
        path: path.to_owned(),
        decoded,
        expected: expect.or(inferred),
    })
}

/// Escapes a string as a JSON string literal.
///
/// Delegates to the shared writer in [`crate::json`] so the binary has
/// one escaping rule rather than two that can drift apart.
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    crate::json::push_quoted(&mut out, s);
    out
}

/// Runs `yodel bench`: decodes every input, prints the per-file and
/// aggregate report, and fails (exit 1) when the aggregate decoded
/// count falls below `--min`.
pub fn bench(args: &BenchArgs) -> Result<(), String> {
    args.modem.reject_il2p("bench")?;
    let threshold = args.min.as_deref().map(parse_min).transpose()?;
    let files = expand_inputs(&args.inputs)?;
    let mut results = Vec::with_capacity(files.len());
    for path in &files {
        results.push(bench_file(path, &args.modem, args.expect)?);
    }
    let total_decoded: u64 = results.iter().map(|r| u64::from(r.decoded)).sum();
    // The expected total is only known when every file has an
    // expectation (from --expect or an embedded counter).
    let total_expected: Option<u64> = results
        .iter()
        .map(|r| r.expected.map(u64::from))
        .sum::<Option<u64>>();
    let pass = match threshold {
        None => None,
        Some(Threshold::Count(n)) => Some(total_decoded >= n),
        Some(Threshold::Percent(pct)) => {
            let total = total_expected.ok_or(
                "--min with a percentage needs an expected frame count for every file: \
                 pass --expect <N>, or bench recordings made by `yodel gen` (whose \
                 frames carry their own counter)",
            )?;
            Some(total == 0 || total_decoded as f64 * 100.0 >= pct * total as f64)
        }
    };

    let mut out = Output::new();
    if args.json {
        let mut json = String::from("{\"files\":[");
        for (i, r) in results.iter().enumerate() {
            if i > 0 {
                json.push(',');
            }
            let expected = match r.expected {
                Some(n) => n.to_string(),
                None => "null".to_owned(),
            };
            json.push_str(&format!(
                "{{\"path\":{},\"decoded\":{},\"expected\":{expected}}}",
                json_string(&r.path),
                r.decoded
            ));
        }
        let expected = match total_expected {
            Some(n) => n.to_string(),
            None => "null".to_owned(),
        };
        let (min, pass_text) = match (&args.min, pass) {
            (Some(min), Some(ok)) => (json_string(min), ok.to_string()),
            _ => ("null".to_owned(), "null".to_owned()),
        };
        json.push_str(&format!(
            "],\"decoded\":{total_decoded},\"expected\":{expected},\"min\":{min},\"pass\":{pass_text}}}"
        ));
        out.line(format_args!("{json}"))?;
    } else {
        let width = results
            .iter()
            .map(|r| r.path.len())
            .max()
            .unwrap_or(0)
            .max("total".len());
        out.line(format_args!(
            "{:<width$}  {:>7}  {:>8}",
            "file", "decoded", "expected"
        ))?;
        for r in &results {
            let expected = match r.expected {
                Some(n) => n.to_string(),
                None => "?".to_owned(),
            };
            out.line(format_args!(
                "{:<width$}  {:>7}  {:>8}",
                r.path, r.decoded, expected
            ))?;
        }
        let expected = match total_expected {
            Some(n) => n.to_string(),
            None => "?".to_owned(),
        };
        out.line(format_args!(
            "{:<width$}  {total_decoded:>7}  {expected:>8}",
            "total"
        ))?;
        if let (Some(min), Some(ok)) = (&args.min, pass) {
            out.line(format_args!(
                "threshold --min {min}: {}",
                if ok { "PASS" } else { "FAIL" }
            ))?;
        }
    }
    out.finish()?;

    if pass == Some(false) {
        return Err(format!(
            "decoded {total_decoded} frame(s), below the --min {} threshold",
            args.min.as_deref().unwrap_or("")
        ));
    }
    Ok(())
}
