//! End-to-end tests of the `warble` CLI binary.
//!
//! The binary is only compiled with its full feature set (the `cli`
//! aggregate: std + tnc + micE + kiss), so this file is gated on all of
//! them — `env!("CARGO_BIN_EXE_warble")` would otherwise fail.
#![cfg(all(feature = "std", feature = "tnc", feature = "micE", feature = "kiss"))]

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

/// The compiled CLI binary.
const BIN: &str = env!("CARGO_BIN_EXE_warble");

/// A unique scratch WAV path in the system temp directory.
fn scratch(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    std::env::temp_dir().join(format!("warble-cli-{pid}-{n}-{tag}.wav"))
}

/// Runs the binary with `args`, returning (exit ok, stdout, stderr).
fn run(args: &[&str]) -> (bool, String, String) {
    let output = Command::new(BIN)
        .args(args)
        .output()
        .expect("running the warble binary");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Runs the binary with `args`, piping `input` to its stdin.
fn run_with_stdin(args: &[&str], input: &[u8]) -> (bool, String, String) {
    use std::io::Write;
    let mut child = Command::new(BIN)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawning the warble binary");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(input)
        .expect("writing to child stdin");
    let output = child.wait_with_output().expect("waiting for the binary");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Like [`run_with_stdin`], but tolerating the child exiting before it
/// has read everything.
///
/// A subcommand that stops early on purpose closes stdin while the
/// writer is still going, and the writer then gets `BrokenPipe`. That
/// is the correct shape for a pipeline (a live capture tool would take
/// `SIGPIPE` and stop), so it is an expected outcome here rather than
/// a failure. `run_with_stdin` panics on it, which is right for the
/// subcommands that must consume their whole input.
fn run_with_stdin_early_exit(args: &[&str], input: &[u8]) -> (bool, String, String) {
    use std::io::Write;
    let mut child = Command::new(BIN)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawning the warble binary");
    if let Some(mut stdin) = child.stdin.take() {
        match stdin.write_all(input) {
            Ok(()) | Err(_) => {}
        }
    }
    let output = child.wait_with_output().expect("waiting for the binary");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Synthesizes one status-report transmission as i16 samples at `hz`.
fn synthesized_samples(hz: u32) -> Vec<i16> {
    use warble::SampleRate;
    use warble::aprs::{AprsPacket, Status};
    use warble::ax25::Address;
    use warble::tnc::{TncConfig, TncTransmitter};

    let rate = SampleRate::new(hz).expect("rate");
    let config = TncConfig::bell_202(rate).expect("config");
    let tx = TncTransmitter::new(config);
    let packet = AprsPacket::Status(Status {
        text: b"stdin live pipe",
    });
    let dest = Address::new(b"APRS", 0).expect("dest");
    let src = Address::new(b"N3CALL", 5).expect("src");
    tx.transmit_to_vec_i16(&packet, dest, src, &[])
        .expect("samples")
}

/// Writes `samples` as a 16-bit mono WAV at `hz` and returns the path.
fn write_wav(tag: &str, hz: u32, samples: &[i16]) -> PathBuf {
    let wav = scratch(tag);
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: hz,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&wav, spec).expect("create wav");
    for &s in samples {
        writer.write_sample(s).expect("write sample");
    }
    writer.finalize().expect("finalize");
    wav
}

#[test]
fn encode_position_decode_round_trip() {
    let wav = scratch("pos");
    let path = wav.to_string_lossy().into_owned();
    let (ok, _, stderr) = run(&[
        "encode",
        "--out",
        &path,
        "--from",
        "N0CALL-7",
        "--to",
        "APRS",
        "--path",
        "WIDE1-1",
        "position",
        "--lat",
        "40.1234",
        "--lon",
        "-105.5678",
        "--comment",
        "round trip",
    ]);
    assert!(ok, "encode failed: {stderr}");
    let (ok, stdout, stderr) = run(&["decode", &path]);
    assert!(ok, "decode failed: {stderr}");
    assert!(stdout.contains("N0CALL-7>APRS,WIDE1-1"), "got: {stdout}");
    assert!(stdout.contains("lat 40.123"), "got: {stdout}");
    assert!(stdout.contains("lon -105.567"), "got: {stdout}");
    assert!(stdout.contains("round trip"), "got: {stdout}");
    assert!(stderr.contains("frames ok: 1"), "got: {stderr}");
    let _ = std::fs::remove_file(&wav);
}

#[test]
fn encode_message_decode_round_trip() {
    let wav = scratch("msg");
    let path = wav.to_string_lossy().into_owned();
    let (ok, _, stderr) = run(&[
        "encode",
        "--out",
        &path,
        "--from",
        "N0CALL",
        "--to",
        "APRS",
        "message",
        "--to-call",
        "N1CALL",
        "--text",
        "hello there",
        "--id",
        "42",
    ]);
    assert!(ok, "encode failed: {stderr}");
    let (ok, stdout, _) = run(&["decode", &path]);
    assert!(ok);
    assert!(stdout.contains("N0CALL>APRS"), "got: {stdout}");
    assert!(stdout.contains("message to N1CALL"), "got: {stdout}");
    assert!(stdout.contains("hello there"), "got: {stdout}");
    assert!(stdout.contains("id 42"), "got: {stdout}");
    let _ = std::fs::remove_file(&wav);
}

#[test]
fn decode_wav_written_by_library() {
    use warble::SampleRate;
    use warble::aprs::{AprsPacket, Status};
    use warble::ax25::Address;
    use warble::tnc::{TncConfig, TncTransmitter};

    let wav = scratch("lib");
    let rate = SampleRate::new(22_050).expect("rate");
    let config = TncConfig::bell_202(rate).expect("config");
    let tx = TncTransmitter::new(config);
    let packet = AprsPacket::Status(Status {
        text: b"library interop",
    });
    let dest = Address::new(b"APRS", 0).expect("dest");
    let src = Address::new(b"N2CALL", 3).expect("src");
    let samples = tx
        .transmit_to_vec_i16(&packet, dest, src, &[])
        .expect("samples");
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: rate.hz(),
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&wav, spec).expect("create wav");
    for s in samples {
        writer.write_sample(s).expect("write sample");
    }
    writer.finalize().expect("finalize");

    let path = wav.to_string_lossy().into_owned();
    let (ok, stdout, _) = run(&["decode", &path]);
    assert!(ok);
    assert!(stdout.contains("N2CALL-3>APRS"), "got: {stdout}");
    assert!(
        stdout.contains("status \"library interop\""),
        "got: {stdout}"
    );
    let _ = std::fs::remove_file(&wav);
}

#[test]
fn rate_variants_round_trip() {
    for rate in ["8000", "11025", "48000"] {
        let wav = scratch(&format!("rate{rate}"));
        let path = wav.to_string_lossy().into_owned();
        let (ok, _, stderr) = run(&[
            "encode", "--out", &path, "--from", "N0CALL", "--to", "APRS", "--rate", rate,
            "position", "--lat", "-33.9", "--lon", "151.2",
        ]);
        assert!(ok, "encode at {rate} Hz failed: {stderr}");
        let (ok, stdout, _) = run(&["decode", &path]);
        assert!(ok, "decode at {rate} Hz failed");
        assert!(stdout.contains("lat -33.9"), "at {rate} Hz got: {stdout}");
        assert!(stdout.contains("lon 151.2"), "at {rate} Hz got: {stdout}");
        let _ = std::fs::remove_file(&wav);
    }
}

#[test]
fn preset_and_knob_overrides_round_trip() {
    // The benchmark-script shape: encode + decode with --preset hf300.
    let wav = scratch("hf300");
    let path = wav.to_string_lossy().into_owned();
    let (ok, _, stderr) = run(&[
        "encode", "--out", &path, "--from", "N0CALL", "--to", "APRS", "--preset", "hf300",
        "position", "--lat", "12.3", "--lon", "45.6",
    ]);
    assert!(ok, "encode hf300 failed: {stderr}");
    let (ok, stdout, _) = run(&["decode", "--preset", "hf300", &path]);
    assert!(ok);
    assert!(stdout.contains("N0CALL>APRS"), "got: {stdout}");
    assert!(stdout.contains("lat 12.3"), "got: {stdout}");
    let _ = std::fs::remove_file(&wav);

    // Per-knob overrides composing with the preset: Bell 202 tones at
    // 300 baud on both sides.
    let wav = scratch("knobs");
    let path = wav.to_string_lossy().into_owned();
    let (ok, _, stderr) = run(&[
        "encode", "--out", &path, "--from", "N0CALL", "--to", "APRS", "--baud", "300", "--mark",
        "1200", "--space", "2200", "position", "--lat", "-12.3", "--lon", "-45.6",
    ]);
    assert!(ok, "encode with knob overrides failed: {stderr}");
    let (ok, stdout, _) = run(&[
        "decode", "--baud", "300", "--mark", "1200", "--space", "2200", &path,
    ]);
    assert!(ok);
    assert!(stdout.contains("lat -12.3"), "got: {stdout}");

    // A bad --baud is a value error: exit 1, not a usage error.
    let (ok, _, stderr) = run(&["decode", "--baud", "0", &path]);
    assert!(!ok);
    assert!(stderr.contains("--baud"), "got: {stderr}");
    let _ = std::fs::remove_file(&wav);
}

#[test]
fn bad_usage_exits_nonzero() {
    // No arguments at all.
    let (ok, _, stderr) = run(&[]);
    assert!(!ok);
    assert!(stderr.contains("Usage"), "got: {stderr}");

    // Unknown subcommand.
    let (ok, _, stderr) = run(&["transcode"]);
    assert!(!ok);
    assert!(stderr.contains("unrecognized subcommand"), "got: {stderr}");

    // Unknown flag on encode.
    let wav = scratch("badflag");
    let path = wav.to_string_lossy().into_owned();
    let (ok, _, stderr) = run(&[
        "encode", "--out", &path, "--from", "N0CALL", "--to", "APRS", "--loud", "yes", "position",
        "--lat", "1", "--lon", "2",
    ]);
    assert!(!ok);
    assert!(stderr.contains("--loud"), "got: {stderr}");

    // Missing required flag.
    let (ok, _, stderr) = run(&["encode", "--from", "N0CALL", "--to", "APRS", "position"]);
    assert!(!ok);
    assert!(stderr.contains("--out"), "got: {stderr}");

    // Out-of-range latitude.
    let (ok, _, stderr) = run(&[
        "encode", "--out", &path, "--from", "N0CALL", "--to", "APRS", "position", "--lat", "91.5",
        "--lon", "2",
    ]);
    assert!(!ok);
    assert!(stderr.contains("91.5"), "got: {stderr}");
    let _ = std::fs::remove_file(&wav);
}

#[test]
fn decode_bad_inputs_exit_nonzero() {
    // Nonexistent file.
    let (ok, _, stderr) = run(&["decode", "/nonexistent/warble-missing.wav"]);
    assert!(!ok);
    assert!(stderr.contains("warble-missing.wav"), "got: {stderr}");

    // Unsupported WAV format: stereo.
    let wav = scratch("stereo");
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: 44_100,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&wav, spec).expect("create wav");
    for _ in 0..64 {
        writer.write_sample(0i16).expect("write");
    }
    writer.finalize().expect("finalize");
    let path = wav.to_string_lossy().into_owned();
    let (ok, _, stderr) = run(&["decode", &path]);
    assert!(!ok);
    assert!(stderr.contains("got 2 channel(s)"), "got: {stderr}");
    assert!(stderr.contains("16-bit mono"), "got: {stderr}");
    let _ = std::fs::remove_file(&wav);

    // Unsupported sample rate.
    let wav = scratch("lowrate");
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 4_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&wav, spec).expect("create wav");
    for _ in 0..64 {
        writer.write_sample(0i16).expect("write");
    }
    writer.finalize().expect("finalize");
    let path = wav.to_string_lossy().into_owned();
    let (ok, _, stderr) = run(&["decode", &path]);
    assert!(!ok);
    assert!(stderr.contains("got 4000 Hz"), "got: {stderr}");
    assert!(stderr.contains("supported"), "got: {stderr}");
    let _ = std::fs::remove_file(&wav);
}

#[test]
fn decode_stdin_raw_pcm_matches_wav_decode() {
    // The same synthesized audio decoded via the WAV path and via raw
    // s16le PCM on stdin must produce identical frame lines.
    let hz = 48_000;
    let samples = synthesized_samples(hz);
    let wav = write_wav("stdinref", hz, &samples);
    let path = wav.to_string_lossy().into_owned();
    let (ok, wav_stdout, _) = run(&["decode", &path]);
    assert!(ok);
    assert!(wav_stdout.contains("N3CALL-5>APRS"), "got: {wav_stdout}");
    let _ = std::fs::remove_file(&wav);

    let mut pcm = Vec::with_capacity(samples.len() * 2);
    for s in &samples {
        pcm.extend_from_slice(&s.to_le_bytes());
    }
    let (ok, stdout, stderr) = run_with_stdin(&["decode", "--sample-rate", "48000", "-"], &pcm);
    assert!(ok, "stdin decode failed: {stderr}");
    assert_eq!(
        stdout, wav_stdout,
        "stdin PCM decode differs from WAV decode"
    );
    assert!(stderr.contains("frames ok: 1"), "got: {stderr}");

    // The explicit --format s16le spelling decodes identically too.
    let (ok, stdout, _) = run_with_stdin(
        &["decode", "--sample-rate", "48000", "--format", "s16le", "-"],
        &pcm,
    );
    assert!(ok);
    assert_eq!(stdout, wav_stdout);
}

#[test]
fn decode_stdin_wav_sniffed_by_riff_header() {
    // A whole WAV file piped to stdin decodes like the file path does
    // (the RIFF header is sniffed; no --sample-rate needed).
    let hz = 22_050;
    let samples = synthesized_samples(hz);
    let wav = write_wav("stdinwav", hz, &samples);
    let path = wav.to_string_lossy().into_owned();
    let (ok, wav_stdout, _) = run(&["decode", &path]);
    assert!(ok);
    let bytes = std::fs::read(&wav).expect("reading wav bytes");
    let _ = std::fs::remove_file(&wav);

    let (ok, stdout, stderr) = run_with_stdin(&["decode", "-"], &bytes);
    assert!(ok, "WAV-on-stdin decode failed: {stderr}");
    assert_eq!(
        stdout, wav_stdout,
        "WAV-on-stdin differs from WAV-path decode"
    );
}

#[test]
fn decode_stdin_flag_validation() {
    // Raw PCM on stdin without --sample-rate is an error explaining
    // the flag.
    let (ok, _, stderr) = run_with_stdin(&["decode", "-"], &[0u8; 64]);
    assert!(!ok);
    assert!(stderr.contains("--sample-rate"), "got: {stderr}");

    // --sample-rate disagreeing with the WAV header on stdin is a
    // contradiction error, not a silent pick of either rate.
    let wav = write_wav("stdinclash", 48_000, &[0i16; 64]);
    let bytes = std::fs::read(&wav).expect("reading wav bytes");
    let _ = std::fs::remove_file(&wav);
    let (ok, _, stderr) = run_with_stdin(&["decode", "--sample-rate", "44100", "-"], &bytes);
    assert!(!ok);
    assert!(stderr.contains("contradicts"), "got: {stderr}");
    assert!(stderr.contains("48000"), "got: {stderr}");

    // Out-of-range --sample-rate is a value error.
    let (ok, _, stderr) = run_with_stdin(&["decode", "--sample-rate", "4000", "-"], &[0u8; 64]);
    assert!(!ok);
    assert!(stderr.contains("4000"), "got: {stderr}");

    // The stdin-only flags are rejected for WAV file paths.
    let hz = 48_000;
    let wav = write_wav("flagclash", hz, &[0i16; 64]);
    let path = wav.to_string_lossy().into_owned();
    let (ok, _, stderr) = run(&["decode", "--sample-rate", "48000", &path]);
    assert!(!ok);
    assert!(stderr.contains("--sample-rate"), "got: {stderr}");
    let (ok, _, stderr) = run(&["decode", "--format", "s16le", &path]);
    assert!(!ok);
    assert!(stderr.contains("--format"), "got: {stderr}");
    let _ = std::fs::remove_file(&wav);
}

// ---------------------------------------------------------------------
// `decode --output-format jsonl`: JSON Lines / NDJSON output.
//
// Four things are worth proving and are proved separately below:
//
//   1. the exact bytes of the output for a fixed input (a real ratchet,
//      possible only because a frame is identified by its sample offset
//      rather than by the wall clock);
//   2. that every line is well-formed JSON -- *proved*, with the
//      minimal parser in `tiny_json` below, not assumed from the fact
//      that it looks like JSON;
//   3. that the escaping and the `info_hex` sibling rule hold for the
//      bytes real traffic carries;
//   4. one line per frame, agreeing with the text mode's frame count.
//
// The crate has no JSON dependency and is not getting one, so the
// parser is written here, in the test, where a bug in it fails loudly.
// ---------------------------------------------------------------------

/// A minimal recursive-descent JSON parser: objects, arrays, strings
/// with escapes, numbers, and the three literals.
///
/// It **validates only** — no value tree — because that is all the
/// well-formedness claim needs, and it is strict where strictness is
/// the point: a raw control byte inside a string is rejected, which is
/// what makes it able to catch an escaping bug in the writer.
/// `tiny_json_accepts_and_rejects` pins that it is not vacuous.
mod tiny_json {
    /// Parses `text` as exactly one JSON value, requiring the whole
    /// input to be consumed.
    pub fn parse_document(text: &str) -> Result<(), String> {
        let mut p = Parser {
            b: text.as_bytes(),
            i: 0,
        };
        p.ws();
        p.value()?;
        p.ws();
        if p.i != p.b.len() {
            return Err(format!("trailing input at byte {}", p.i));
        }
        Ok(())
    }

    struct Parser<'a> {
        b: &'a [u8],
        i: usize,
    }

    impl Parser<'_> {
        fn peek(&self) -> Option<u8> {
            self.b.get(self.i).copied()
        }

        fn bump(&mut self) -> Option<u8> {
            let c = self.peek();
            if c.is_some() {
                self.i += 1;
            }
            c
        }

        fn ws(&mut self) {
            while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
                self.i += 1;
            }
        }

        fn expect(&mut self, c: u8) -> Result<(), String> {
            if self.bump() == Some(c) {
                Ok(())
            } else {
                Err(format!("expected '{}' at byte {}", c as char, self.i))
            }
        }

        fn literal(&mut self, word: &str) -> Result<(), String> {
            if self.b[self.i..].starts_with(word.as_bytes()) {
                self.i += word.len();
                Ok(())
            } else {
                Err(format!("bad literal at byte {}", self.i))
            }
        }

        fn value(&mut self) -> Result<(), String> {
            match self.peek() {
                Some(b'{') => self.object(),
                Some(b'[') => self.array(),
                Some(b'"') => self.string(),
                Some(b't') => self.literal("true"),
                Some(b'f') => self.literal("false"),
                Some(b'n') => self.literal("null"),
                Some(c) if c == b'-' || c.is_ascii_digit() => self.number(),
                _ => Err(format!("no value at byte {}", self.i)),
            }
        }

        fn object(&mut self) -> Result<(), String> {
            self.expect(b'{')?;
            self.ws();
            if self.peek() == Some(b'}') {
                self.i += 1;
                return Ok(());
            }
            loop {
                self.ws();
                self.string()?;
                self.ws();
                self.expect(b':')?;
                self.ws();
                self.value()?;
                self.ws();
                match self.bump() {
                    Some(b',') => {}
                    Some(b'}') => return Ok(()),
                    _ => return Err(format!("expected ',' or '}}' at byte {}", self.i)),
                }
            }
        }

        fn array(&mut self) -> Result<(), String> {
            self.expect(b'[')?;
            self.ws();
            if self.peek() == Some(b']') {
                self.i += 1;
                return Ok(());
            }
            loop {
                self.ws();
                self.value()?;
                self.ws();
                match self.bump() {
                    Some(b',') => {}
                    Some(b']') => return Ok(()),
                    _ => return Err(format!("expected ',' or ']' at byte {}", self.i)),
                }
            }
        }

        fn string(&mut self) -> Result<(), String> {
            self.expect(b'"')?;
            loop {
                match self.bump() {
                    None => return Err("unterminated string".to_owned()),
                    Some(b'"') => return Ok(()),
                    Some(b'\\') => match self.bump() {
                        Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => {}
                        Some(b'u') => {
                            for _ in 0..4 {
                                match self.bump() {
                                    Some(c) if c.is_ascii_hexdigit() => {}
                                    _ => {
                                        return Err(format!("bad \\u escape at byte {}", self.i));
                                    }
                                }
                            }
                        }
                        _ => return Err(format!("bad escape at byte {}", self.i)),
                    },
                    // The whole point of the strictness: an unescaped
                    // control byte is illegal in a JSON string, so a
                    // writer that forgets to escape one fails here.
                    Some(c) if c < 0x20 => {
                        return Err(format!("raw control byte {c:#04x} at byte {}", self.i));
                    }
                    Some(_) => {}
                }
            }
        }

        fn number(&mut self) -> Result<(), String> {
            if self.peek() == Some(b'-') {
                self.i += 1;
            }
            match self.bump() {
                Some(b'0') => {}
                Some(c) if c.is_ascii_digit() => {
                    self.digits();
                }
                _ => return Err(format!("bad number at byte {}", self.i)),
            }
            if self.peek() == Some(b'.') {
                self.i += 1;
                if !self.digits() {
                    return Err(format!("bad fraction at byte {}", self.i));
                }
            }
            if matches!(self.peek(), Some(b'e' | b'E')) {
                self.i += 1;
                if matches!(self.peek(), Some(b'+' | b'-')) {
                    self.i += 1;
                }
                if !self.digits() {
                    return Err(format!("bad exponent at byte {}", self.i));
                }
            }
            Ok(())
        }

        /// Consumes a run of digits; `false` if there were none.
        fn digits(&mut self) -> bool {
            let start = self.i;
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.i += 1;
            }
            self.i > start
        }
    }
}

/// Asserts every line of `stdout` is a complete, well-formed JSON
/// object, and returns the lines.
fn parsed_jsonl_lines(stdout: &str) -> Vec<&str> {
    let lines: Vec<&str> = stdout.lines().collect();
    for (n, line) in lines.iter().enumerate() {
        if let Err(e) = tiny_json::parse_document(line) {
            panic!("line {} is not well-formed JSON ({e}): {line}", n + 1);
        }
        assert!(
            line.starts_with("{\"v\":2,\"sample\":"),
            "line {} does not start with the schema version and sample offset: {line}",
            n + 1
        );
    }
    lines
}

/// The top-level `"kind"` of a JSONL line.
///
/// The first `"kind":"` in the line is always the top-level one: the
/// writer emits it before the typed object, and the only other `kind`
/// is inside a nested third-party payload, which comes later.
fn line_kind(line: &str) -> &str {
    let at = line.find("\"kind\":\"").expect("a kind discriminant");
    let rest = &line[at + 8..];
    let end = rest.find('"').expect("a closed kind string");
    &rest[..end]
}

/// One frame of the JSONL fixture recording.
struct FixtureFrame {
    dest: &'static str,
    src: &'static str,
    ssid: u8,
    /// Digipeater path as (callsign, SSID, has-been-repeated).
    hops: &'static [(&'static str, u8, bool)],
    info: &'static [u8],
}

/// The four AX.25 UI frames of the JSONL fixture.
///
/// Awkward payloads: every escape the writer implements, a real off-air
/// Mic-E report, and a real off-air Mic-E report that does *not* decode.
/// The last two are transcribed byte-for-byte from the corpus
/// recordings, so the fixture exercises what the air carries rather than
/// what is convenient to type.
const JSONL_FIXTURE: &[FixtureFrame] = &[
    // A status report carrying every escape the writer implements:
    // quote, backslash, backspace, tab, newline, form feed, carriage
    // return, a bare C0 control, and DEL. Also the only frame with a
    // path, and its first hop has the H bit set.
    FixtureFrame {
        dest: "APRS",
        src: "N0CALL",
        ssid: 7,
        hops: &[("WIDE1", 1, true), ("WIDE2", 2, false)],
        info: b">q\"s b\\s\x08\t\n\x0c\r\x01\x7fend",
    },
    // An uncompressed position with a course/speed extension and a
    // `/A=` altitude in the comment.
    FixtureFrame {
        dest: "APRS",
        src: "N0CALL",
        ssid: 0,
        hops: &[],
        info: b"!4007.40N/10534.07W>088/036 /A=005280 pin",
    },
    // A real Mic-E position report (corpus track 03).
    FixtureFrame {
        dest: "STPYXT",
        src: "WA8LMF",
        ssid: 0,
        hops: &[("WIDE2", 2, false)],
        info: b"'._\x1el _>/]\"7<}\r",
    },
    // A real Mic-E report that does not decode: an FCS-valid frame
    // carrying 0xBE where a longitude byte belongs (corpus track 01).
    // Not valid UTF-8, so it is also the `info_hex` case, and it
    // carries a DEL (0x7f) besides.
    FixtureFrame {
        dest: "S4PXYX",
        src: "AC6VV",
        ssid: 9,
        hops: &[],
        info: b"\x60\xbe\x5f\x7f\x6c\x23\x35\x3e\x2f\x5d\x22\x36\x6e\x7d\x0d",
    },
];

/// Writes the JSONL fixture recording: the frames above, modulated back
/// to back at `hz` with a fixed silent gap between them.
///
/// Every step is a pure function of the constants here — the modulator
/// has no randomness and the gap is fixed — so the sample offset at
/// which each frame completes is a constant, which is what makes the
/// exact-output pin below possible.
fn write_jsonl_fixture_wav(tag: &str, hz: u32) -> PathBuf {
    use warble::SampleRate;
    use warble::ax25::{Address, PathHop, UiFrame};
    use warble::tnc::{TncConfig, TncTransmitter};

    /// Silence between frames, in samples. A round number so the
    /// arithmetic behind the pinned offsets is inspectable.
    const GAP: usize = 4800;

    let rate = SampleRate::new(hz).expect("rate");
    let tx = TncTransmitter::new(TncConfig::bell_202(rate).expect("config"));
    let mut samples: Vec<i16> = vec![0; GAP];
    for spec in JSONL_FIXTURE {
        let hops: Vec<PathHop> = spec
            .hops
            .iter()
            .map(|&(call, ssid, repeated)| PathHop {
                address: Address::new(call.as_bytes(), ssid).expect("hop"),
                repeated,
            })
            .collect();
        let frame = UiFrame::with_hops(
            Address::new(spec.dest.as_bytes(), 0).expect("dest"),
            Address::new(spec.src.as_bytes(), spec.ssid).expect("src"),
            &hops,
            spec.info,
        )
        .expect("frame");
        let mut body = [0u8; 512];
        let len = frame.build(&mut body).expect("build");
        samples.extend(tx.frame_samples_i16(&body[..len]));
        samples.extend(std::iter::repeat_n(0i16, GAP));
    }
    write_wav(tag, hz, &samples)
}

#[test]
fn tiny_json_accepts_and_rejects() {
    // The well-formedness proof below is only worth anything if the
    // parser can fail, so pin both directions.
    for good in [
        "{}",
        "[]",
        "{\"a\":1}",
        "{\"a\":[1,-2.5,3e10,true,false,null]}",
        "{\"a\":{\"b\":\"\\u0001\\\"\\\\\\n\"}}",
        " { \"a\" : [ 1 , 2 ] } ",
        "\"caf\u{e9} \u{1f600}\"",
    ] {
        assert!(
            tiny_json::parse_document(good).is_ok(),
            "should parse: {good}"
        );
    }
    for bad in [
        "",
        "{",
        "{}{}",
        "{\"a\":}",
        "{\"a\" 1}",
        "{a:1}",
        "{\"a\":01}",
        "{\"a\":1,}",
        "[1,]",
        "[1 2]",
        "\"unterminated",
        "\"bad \\x escape\"",
        "\"short \\u00 escape\"",
        // The escaping check: a raw tab inside a string.
        "\"raw\ttab\"",
        "tru",
        "{\"a\":1} trailing",
    ] {
        assert!(
            tiny_json::parse_document(bad).is_err(),
            "should not parse: {bad:?}"
        );
    }
}

// The pinned output, one `const` per line so each stays readable.
// Written with raw strings, so what appears below is byte-for-byte what
// the decoder emits rather than a Rust escaping of it.

/// Status report: two hops with the H bit set on the first, and every
/// escape the writer implements.
const PIN_STATUS: &str = concat!(
    r#"{"v":2,"sample":31492,"t":0.656083,"src":"N0CALL-7","dst":"APRS","#,
    r#""path":[{"call":"WIDE1-1","repeated":true},{"call":"WIDE2-2","repeated":false}],"#,
    r#""kind":"status","status":{"text":"q\"s b\\s\b\t\n\f\r\u0001\u007fend","#,
    r#""message":"q\"s b\\s\b\t\n\f\r\u0001\u007fend"},"#,
    r#""info":">q\"s b\\s\b\t\n\f\r\u0001\u007fend"}"#,
);

/// Uncompressed position with a course/speed extension and a `/A=`
/// altitude found inside the comment.
const PIN_POSITION: &str = concat!(
    r#"{"v":2,"sample":66132,"t":1.377750,"src":"N0CALL","dst":"APRS","path":[],"#,
    r#""kind":"position","position":{"lat_deg":40.123333,"lon_deg":-105.567833,"#,
    r#""symbol":"/>","messaging":false,"compressed":false,"altitude_ft":5280,"#,
    r#""extension":{"type":"course_speed","course_deg":88,"speed_kt":36},"#,
    r#""comment":" /A=005280 pin"},"info":"!4007.40N/10534.07W>088/036 /A=005280 pin"}"#,
);

/// A real off-air Mic-E report: half its position comes from the
/// destination callsign, which is why the projection is frame-level.
const PIN_MIC_E: &str = concat!(
    r#"{"v":2,"sample":94812,"t":1.975250,"src":"WA8LMF","dst":"STPYXT","#,
    r#""path":[{"call":"WIDE2-2","repeated":false}],"kind":"mic_e","#,
    r#""mic_e":{"lat_deg":34.164000,"lon_deg":-118.117000,"speed_kt":0,"course_deg":67,"#,
    r#""symbol":"/>","message":"off_duty","fix":"old","altitude_m":310,"#,
    r#""device_prefix":"]","ambiguity_digits":0,"status":"\r"},"#,
    r#""info":"'._\u001el _>/]\"7<}\r"}"#,
);

/// A real off-air Mic-E report that does not decode: one line all the
/// same, labelled `malformed`, carrying the parser's own message and
/// — because 0xBE is not valid UTF-8 — an `info_hex` sibling.
const PIN_MALFORMED: &str = concat!(
    r#"{"v":2,"sample":121252,"t":2.526083,"src":"AC6VV-9","dst":"S4PXYX","path":[],"#,
    r#""kind":"malformed","malformed":{"dti":96,"dti_char":"`"},"#,
    r#""error":"Mic-E report: longitude byte 0xBE at offset 1 decodes outside its legal range","#,
    // U+FFFD is what the lossy conversion leaves where the 0xBE was;
    // spelled as an escape here rather than pasted, so the source stays
    // readable. `info_hex` is the byte-exact copy.
    "\"info\":\"`\u{fffd}_",
    r#"\u007fl#5>/]\"6n}\r","info_hex":"60be5f7f6c23353e2f5d22366e7d0d"}"#,
);

#[test]
fn decode_jsonl_exact_output_pin() {
    // The ratchet. Nothing in the decode path reads a clock or a random
    // number, so a fixed recording has exactly one correct rendering,
    // byte for byte -- including the `sample` offsets, which are a
    // function of the fixture's frame lengths and gaps and of the
    // receiver's own latency.
    //
    // If this fails after a demodulator change, the offsets are what
    // moved; check the rest of each line before re-pinning them.
    let wav = write_jsonl_fixture_wav("jsonlpin", 48_000);
    let path = wav.to_string_lossy().into_owned();
    let (ok, stdout, stderr) = run(&["decode", "--output-format", "jsonl", &path]);
    assert!(ok, "decode failed: {stderr}");
    let _ = std::fs::remove_file(&wav);

    let expected = format!("{PIN_STATUS}\n{PIN_POSITION}\n{PIN_MIC_E}\n{PIN_MALFORMED}\n");
    assert_eq!(stdout, expected, "actual output was:\n{stdout}");

    // Same input, twice: the whole point of a sample offset rather than
    // a wall clock.
    let wav = write_jsonl_fixture_wav("jsonlpin2", 48_000);
    let path = wav.to_string_lossy().into_owned();
    let (ok, again, _) = run(&["decode", "--output-format", "jsonl", &path]);
    assert!(ok);
    let _ = std::fs::remove_file(&wav);
    assert_eq!(stdout, again, "jsonl output is not reproducible");
}

#[test]
fn decode_jsonl_every_line_is_well_formed_and_escaped() {
    // Well-formedness *proved* with the parser above, not assumed, and
    // over the payloads that break naive writers.
    let wav = write_jsonl_fixture_wav("jsonlwf", 48_000);
    let path = wav.to_string_lossy().into_owned();
    let (ok, stdout, stderr) = run(&["decode", "--output-format", "jsonl", &path]);
    assert!(ok, "decode failed: {stderr}");
    let _ = std::fs::remove_file(&wav);

    let lines = parsed_jsonl_lines(&stdout);
    assert_eq!(lines.len(), JSONL_FIXTURE.len());

    // `line[line.kind]` is an object on every line, whatever the kind.
    for line in &lines {
        let kind = line_kind(line);
        assert!(
            line.contains(&format!("\"{kind}\":{{")),
            "kind {kind} has no object of its own: {line}"
        );
    }
    let kinds: Vec<&str> = lines.iter().map(|l| line_kind(l)).collect();
    assert_eq!(kinds, ["status", "position", "mic_e", "malformed"]);

    // Escaping: quote, backslash, the five short forms, a bare C0
    // control and DEL. The parser above already rejected any raw
    // control byte, so this pins the *spelling* as well.
    let status = lines[0];
    for escape in [
        "\\\"", "\\\\", "\\b", "\\t", "\\n", "\\f", "\\r", "\\u0001", "\\u007f",
    ] {
        assert!(status.contains(escape), "missing {escape} in: {status}");
    }

    // A valid-UTF-8 information field gets no `info_hex`; an invalid
    // one gets it, byte-exact, and the line still parses.
    assert!(!lines[2].contains("info_hex"), "got: {}", lines[2]);
    assert!(
        lines[3].contains("\"info_hex\":\"60be5f7f6c23353e2f5d22366e7d0d\""),
        "got: {}",
        lines[3]
    );
}

#[test]
fn decode_jsonl_line_count_matches_text_mode() {
    // One line per frame, and the same frames as the human output.
    let wav = write_jsonl_fixture_wav("jsonlcount", 48_000);
    let path = wav.to_string_lossy().into_owned();
    let (ok, text, text_err) = run(&["decode", &path]);
    assert!(ok, "text decode failed: {text_err}");
    let (ok, jsonl, jsonl_err) = run(&["decode", "--output-format", "jsonl", &path]);
    assert!(ok, "jsonl decode failed: {jsonl_err}");
    let _ = std::fs::remove_file(&wav);

    assert_eq!(
        text.lines().count(),
        jsonl.lines().count(),
        "text and jsonl disagree on the frame count"
    );
    assert_eq!(jsonl.lines().count(), JSONL_FIXTURE.len());
    // Both modes report the same receive statistics on stderr, which
    // is where the frame count is authoritative.
    assert!(text_err.contains("frames ok: 4"), "got: {text_err}");
    assert!(jsonl_err.contains("frames ok: 4"), "got: {jsonl_err}");
    // Every jsonl line ends with a newline (NDJSON, not a JSON array).
    assert!(jsonl.ends_with('\n'));
    assert!(!jsonl.contains("[\n"));
}

#[test]
fn decode_output_format_defaults_to_text_and_wall_clock_is_opt_in() {
    let wav = write_jsonl_fixture_wav("jsonlflags", 48_000);
    let path = wav.to_string_lossy().into_owned();

    // Default: nothing changes for existing users.
    let (ok, implicit, _) = run(&["decode", &path]);
    assert!(ok);
    let (ok, explicit, _) = run(&["decode", "--output-format", "text", &path]);
    assert!(ok);
    assert_eq!(implicit, explicit);
    assert!(
        implicit.starts_with("N0CALL-7>APRS,WIDE1-1,WIDE2-2: "),
        "got: {implicit}"
    );

    // No wall clock by default — the reason the pin above can exist.
    let (ok, plain, _) = run(&["decode", "--output-format", "jsonl", &path]);
    assert!(ok);
    assert!(!plain.contains("unix_time"), "got: {plain}");

    // Opt in and it appears, still parsing, on every line.
    let (ok, timed, stderr) = run(&["decode", "--output-format", "jsonl", "--wall-clock", &path]);
    assert!(ok, "wall-clock decode failed: {stderr}");
    for line in parsed_jsonl_lines(&timed) {
        assert!(line.contains("\"unix_time\":"), "got: {line}");
    }

    // Asking for a wall clock the text output cannot carry is refused
    // rather than silently ignored.
    let (ok, _, stderr) = run(&["decode", "--wall-clock", &path]);
    assert!(!ok);
    assert!(stderr.contains("--wall-clock"), "got: {stderr}");

    // An unknown format is a usage error from clap.
    let (ok, _, stderr) = run(&["decode", "--output-format", "yaml", &path]);
    assert!(!ok);
    assert!(stderr.contains("jsonl"), "got: {stderr}");

    let _ = std::fs::remove_file(&wav);
}

/// Corpus tracks, mirroring `tests/benchmark.rs` and
/// `tests/corpus_aprs.rs`.
const CORPUS_FILES: &[&str] = &[
    "01_40-Mins-Traffic_-on-144.39.wav",
    "02_100-Mic-E-Bursts-DE-emphasized.wav",
    "03_100-Mic-E-Bursts-Flat.wav",
    "04_25-MIns-Drive-Test.wav",
];

/// Floor on the number of corpus lines checked, so the loop below
/// cannot pass having validated nothing.
///
/// MEASURED at 2182 frames over the four tracks (the same total
/// `tests/corpus_aprs.rs` pins); set a little under so ordinary
/// demodulator jitter cannot fail the build.
const MIN_CORPUS_LINES: usize = 2100;

#[test]
#[ignore = "requires the operator-provided corpus/ recordings"]
fn decode_jsonl_corpus_lines_are_well_formed() {
    // Tier 3: every frame of real off-air VHF traffic rendered as JSON
    // and parsed back. The fixture above is hand-picked; this is the
    // channel as it is -- Mic-E, weather, third-party
    // encapsulation, NMEA, non-APRS beacons and damaged frames.
    let dir = std::path::Path::new("corpus");
    if !dir.is_dir() {
        eprintln!("corpus/ absent — skipping JSONL well-formedness test");
        return;
    }
    let mut total = 0usize;
    let mut kinds: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut lossy = 0usize;
    for name in CORPUS_FILES {
        let path = dir.join(name);
        if !path.is_file() {
            eprintln!("corpus/{name} absent — skipping JSONL well-formedness test");
            return;
        }
        let arg = path.to_string_lossy().into_owned();
        let (ok, stdout, stderr) = run(&["decode", "--output-format", "jsonl", &arg]);
        assert!(ok, "decoding {name} failed: {stderr}");
        for line in parsed_jsonl_lines(&stdout) {
            let kind = line_kind(line);
            *kinds.entry(kind.to_owned()).or_default() += 1;
            if line.contains("\"info_hex\":") {
                lossy += 1;
            }
            assert!(
                line.contains(&format!("\"{kind}\":{{")),
                "kind {kind} has no object of its own: {line}"
            );
            assert!(line.contains("\"info\":"), "no info field: {line}");
            total += 1;
        }
    }
    eprintln!("corpus JSONL: {total} lines, kinds {kinds:?}, {lossy} with info_hex");
    assert!(
        total >= MIN_CORPUS_LINES,
        "only {total} corpus lines checked, expected at least {MIN_CORPUS_LINES}"
    );
    // The corpus is known to carry frames whose information field is
    // not valid UTF-8 (there is one with a 0xBE); if none showed up,
    // the `_hex` sibling rule went untested here.
    assert!(lossy > 0, "no corpus frame exercised the info_hex path");
}

#[test]
fn serve_stdin_sniffs_wav_like_decode() {
    // `serve --input -` shares the decode sniff: a whole WAV piped to
    // stdin needs no --sample-rate (the header carries it), and the
    // frame decodes. Audio EOF is the graceful shutdown, so the run
    // terminates on its own.
    let hz = 22_050;
    let samples = synthesized_samples(hz);
    let wav = write_wav("servewav", hz, &samples);
    let bytes = std::fs::read(&wav).expect("reading wav bytes");
    let _ = std::fs::remove_file(&wav);
    let out = scratch("serveout");
    let out_path = out.to_string_lossy().into_owned();

    let (ok, _, stderr) = run_with_stdin(
        &[
            "serve",
            "--tcp",
            "127.0.0.1:0",
            "--input",
            "-",
            "--output",
            &out_path,
        ],
        &bytes,
    );
    assert!(ok, "serve WAV-on-stdin failed: {stderr}");
    assert!(stderr.contains("1 frame(s) received"), "got: {stderr}");
    let _ = std::fs::remove_file(&out);

    // The contradiction check also applies here.
    let clash = write_wav("serveclash", 48_000, &[0i16; 64]);
    let clash_bytes = std::fs::read(&clash).expect("reading wav bytes");
    let _ = std::fs::remove_file(&clash);
    let out = scratch("serveout2");
    let out_path = out.to_string_lossy().into_owned();
    let (ok, _, stderr) = run_with_stdin(
        &[
            "serve",
            "--tcp",
            "127.0.0.1:0",
            "--input",
            "-",
            "--sample-rate",
            "44100",
            "--output",
            &out_path,
        ],
        &clash_bytes,
    );
    assert!(!ok);
    assert!(stderr.contains("contradicts"), "got: {stderr}");
    let _ = std::fs::remove_file(&out);

    // Raw PCM on stdin still requires --sample-rate.
    let out = scratch("serveout3");
    let out_path = out.to_string_lossy().into_owned();
    let (ok, _, stderr) = run_with_stdin(
        &[
            "serve",
            "--tcp",
            "127.0.0.1:0",
            "--input",
            "-",
            "--output",
            &out_path,
        ],
        &[0u8; 64],
    );
    assert!(!ok);
    assert!(stderr.contains("--sample-rate"), "got: {stderr}");
    let _ = std::fs::remove_file(&out);
}

#[test]
fn gen_is_deterministic_per_seed() {
    // Same flags + seed => byte-identical WAV; a different seed with
    // noise enabled => different bytes.
    let args = |out: &str, seed: &str| {
        vec![
            "gen".to_owned(),
            "--out".to_owned(),
            out.to_owned(),
            "--count".to_owned(),
            "3".to_owned(),
            "--snr".to_owned(),
            "12".to_owned(),
            "--seed".to_owned(),
            seed.to_owned(),
            "--sample-rate".to_owned(),
            "22050".to_owned(),
        ]
    };
    let run_gen = |tag: &str, seed: &str| {
        let wav = scratch(tag);
        let path = wav.to_string_lossy().into_owned();
        let argv = args(&path, seed);
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        let (ok, _, stderr) = run(&argv);
        assert!(ok, "gen failed: {stderr}");
        let bytes = std::fs::read(&wav).expect("reading gen output");
        let _ = std::fs::remove_file(&wav);
        bytes
    };
    let a = run_gen("gen-a", "99");
    let b = run_gen("gen-b", "99");
    let c = run_gen("gen-c", "100");
    assert_eq!(a, b, "same seed must produce identical bytes");
    assert_ne!(a, c, "different seeds must produce different bytes");
}

#[test]
fn gen_clean_round_trip_decodes_every_frame() {
    let wav = scratch("gen-clean");
    let path = wav.to_string_lossy().into_owned();
    let (ok, _, stderr) = run(&["gen", "--out", &path, "--count", "4"]);
    assert!(ok, "gen failed: {stderr}");
    let (ok, stdout, stderr) = run(&["decode", &path]);
    assert!(ok, "decode failed: {stderr}");
    assert!(stderr.contains("frames ok: 4"), "got: {stderr}");
    assert!(stdout.contains("N0CALL-1>APRS"), "got: {stdout}");
    assert!(stdout.contains("[1/4]"), "got: {stdout}");
    assert!(stdout.contains("[4/4]"), "got: {stdout}");
    let _ = std::fs::remove_file(&wav);
}

#[test]
fn gen_il2p_round_trip_decodes_every_frame() {
    // IL2P TX (`gen --il2p`) → IL2P RX (`decode --il2p`), and the
    // non-compatibility check: the plain decode path sees nothing
    // (IL2P replaces the HDLC framing wholesale).
    let wav = scratch("gen-il2p");
    let path = wav.to_string_lossy().into_owned();
    let (ok, _, stderr) = run(&["gen", "--out", &path, "--count", "3", "--il2p"]);
    assert!(ok, "gen --il2p failed: {stderr}");
    let (ok, stdout, stderr) = run(&["decode", "--il2p", &path]);
    assert!(ok, "decode --il2p failed: {stderr}");
    assert!(stderr.contains("frames ok: 3"), "got: {stderr}");
    assert!(stdout.contains("N0CALL-1>APRS"), "got: {stdout}");
    assert!(stdout.contains("[3/3]"), "got: {stdout}");
    let (ok, _, stderr) = run(&["decode", &path]);
    assert!(ok, "plain decode failed: {stderr}");
    assert!(stderr.contains("frames ok: 0"), "got: {stderr}");
    let _ = std::fs::remove_file(&wav);
}

#[test]
fn il2p_and_fx25_flags_conflict() {
    let (ok, _, stderr) = run(&["decode", "--il2p", "--fx25", "nonexistent.wav"]);
    assert!(!ok);
    assert!(
        stderr.contains("--fx25") && stderr.contains("--il2p"),
        "got: {stderr}"
    );
}

/// `--il2p` is shared plumbing, so every subcommand parses it, but only
/// `gen` and `decode` implement it. The three that do not must refuse:
/// accepting and ignoring the flag wrote a plain AX.25 WAV with no
/// warning, so the operator got a file they believed was IL2P and found
/// out on the air.
#[test]
fn il2p_is_refused_by_subcommands_that_do_not_implement_it() {
    let wav = scratch("il2p-refused");
    let path = wav.to_string_lossy().to_string();
    let cases: [(&str, Vec<&str>); 3] = [
        (
            "encode",
            vec![
                "encode",
                "--out",
                &path,
                "--from",
                "N0CALL-1",
                "--to",
                "APRS",
                "--il2p",
                "message",
                "--to-call",
                "N1CALL",
                "--text",
                "hi",
            ],
        ),
        ("bench", vec!["bench", "--il2p", &path]),
        (
            "serve",
            vec!["serve", "--stdio", "--il2p", "--input", &path],
        ),
    ];
    for (name, argv) in cases {
        let (ok, _, stderr) = run(&argv);
        assert!(!ok, "`{name} --il2p` should fail, but it succeeded");
        assert!(
            stderr.contains("--il2p") && stderr.contains(name),
            "`{name} --il2p` must say which subcommand refused and why; got: {stderr}"
        );
    }
    let _ = std::fs::remove_file(&wav);
}

#[test]
fn gen_stdout_pcm_pipes_into_decode() {
    // `gen --out -` streams raw s16le PCM; feed it back through
    // `decode -` and every frame comes out.
    let output = Command::new(BIN)
        .args([
            "gen",
            "--out",
            "-",
            "--count",
            "2",
            "--sample-rate",
            "48000",
        ])
        .output()
        .expect("running gen");
    assert!(output.status.success());
    assert!(!output.stdout.is_empty());
    let (ok, stdout, stderr) =
        run_with_stdin(&["decode", "--sample-rate", "48000", "-"], &output.stdout);
    assert!(ok, "decode failed: {stderr}");
    assert!(stderr.contains("frames ok: 2"), "got: {stderr}");
    assert!(stdout.contains("[2/2]"), "got: {stdout}");
}

#[test]
fn gen_moderate_noise_still_mostly_decodes() {
    // 15 dB SNR is a mild impairment: most of 6 frames must survive
    // (loose bound — the exact count is receiver tuning, not contract).
    let wav = scratch("gen-noisy");
    let path = wav.to_string_lossy().into_owned();
    let (ok, _, stderr) = run(&[
        "gen", "--out", &path, "--count", "6", "--snr", "15", "--seed", "3",
    ]);
    assert!(ok, "gen failed: {stderr}");
    let (ok, stdout, stderr) = run(&["bench", &path, "--min", "4", "--json"]);
    assert!(ok, "bench failed: {stdout} {stderr}");
    let _ = std::fs::remove_file(&wav);
}

#[test]
fn bench_threshold_pass_and_fail_exit_codes() {
    let wav = scratch("bench-thr");
    let path = wav.to_string_lossy().into_owned();
    let (ok, _, stderr) = run(&["gen", "--out", &path, "--count", "3"]);
    assert!(ok, "gen failed: {stderr}");

    // Absolute count: 3 decoded >= 3 passes, >= 4 fails (exit 1).
    let (ok, stdout, _) = run(&["bench", &path, "--min", "3"]);
    assert!(ok, "got: {stdout}");
    assert!(stdout.contains("PASS"), "got: {stdout}");
    let (ok, stdout, stderr) = run(&["bench", &path, "--min", "4"]);
    assert!(!ok, "a below-threshold bench must exit nonzero");
    assert!(stdout.contains("FAIL"), "got: {stdout}");
    assert!(stderr.contains("below"), "got: {stderr}");

    // Percentage against the embedded [i/N] counter: 100% passes.
    let (ok, stdout, _) = run(&["bench", &path, "--min", "100%"]);
    assert!(ok, "got: {stdout}");

    // Percentage with an explicit --expect above reality: fails.
    let (ok, _, _) = run(&["bench", &path, "--expect", "5", "--min", "80%"]);
    assert!(!ok);
    let _ = std::fs::remove_file(&wav);
}

#[test]
fn bench_json_shape_and_directory_input() {
    // Two gen WAVs in a scratch directory, benched by directory path;
    // the JSON report carries per-file and aggregate fields.
    let dir = std::env::temp_dir().join(format!("warble-bench-dir-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    for (name, count) in [("one.wav", "2"), ("two.wav", "3")] {
        let path = dir.join(name).to_string_lossy().into_owned();
        let (ok, _, stderr) = run(&["gen", "--out", &path, "--count", count]);
        assert!(ok, "gen failed: {stderr}");
    }
    let dir_arg = dir.to_string_lossy().into_owned();
    let (ok, stdout, stderr) = run(&["bench", &dir_arg, "--min", "100%", "--json"]);
    assert!(ok, "bench failed: {stderr}");
    let json = stdout.trim();
    assert!(json.starts_with('{') && json.ends_with('}'), "got: {json}");
    assert!(json.contains("\"files\":["), "got: {json}");
    assert!(json.contains("one.wav"), "got: {json}");
    assert!(json.contains("two.wav"), "got: {json}");
    assert!(json.contains("\"decoded\":5"), "got: {json}");
    assert!(json.contains("\"expected\":5"), "got: {json}");
    assert!(json.contains("\"min\":\"100%\""), "got: {json}");
    assert!(json.contains("\"pass\":true"), "got: {json}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn gen_bad_values_exit_nonzero() {
    let (ok, _, stderr) = run(&["gen", "--out", "/tmp/x.wav", "--count", "0"]);
    assert!(!ok);
    assert!(stderr.contains("--count"), "got: {stderr}");
    let (ok, _, stderr) = run(&["gen", "--out", "/tmp/x.wav", "--level", "1.5"]);
    assert!(!ok);
    assert!(stderr.contains("--level"), "got: {stderr}");
    let (ok, _, stderr) = run(&["bench", "/nonexistent/warble-bench-missing.wav"]);
    assert!(!ok);
    assert!(stderr.contains("warble-bench-missing"), "got: {stderr}");
    let (ok, _, stderr) = run(&["bench", "/tmp", "--min", "5x"]);
    assert!(!ok);
    assert!(stderr.contains("--min"), "got: {stderr}");
}

// ---------------------------------------------------------------------
// Device-free proof of the `live_capture` example's plumbing.
//
// The example keeps its sample conversion / channel downmix /
// decimation / chunk-feed logic in the pure `plumbing` module; the
// tests below `#[path]`-include the SAME source file (the technique of
// `tests/app_examples.rs`) and drive it with a synthesized fake source:
// audio produced by the crate's own transmitter, sliced into
// callback-sized chunks, interleaved into fake stereo, doubled into a
// fake 96 kHz device rate. NO audio device is ever opened, so the suite
// stays green on any CI runner with or without the `capture` feature.
// ---------------------------------------------------------------------

#[path = "../examples/live_capture.rs"]
#[allow(dead_code)]
mod live_capture;

/// `wspr gen` → `wspr decode` round trip: the beacon WAV decodes back
/// to the exact message with plausible quality metrics.
#[test]
fn wspr_gen_decode_round_trip() {
    let wav = scratch("wspr");
    let path = wav.to_string_lossy().into_owned();
    let (ok, _, stderr) = run(&[
        "wspr",
        "gen",
        "--callsign",
        "K1ABC",
        "--grid",
        "FN42",
        "--power",
        "37",
        "-o",
        &path,
    ]);
    assert!(ok, "wspr gen failed: {stderr}");
    let (ok, stdout, stderr) = run(&["wspr", "decode", &path]);
    assert!(ok, "wspr decode failed: {stderr}");
    assert!(stdout.contains("K1ABC FN42 37 dBm"), "got: {stdout}");
    assert!(stdout.contains("freq 1500."), "got: {stdout}");
    let _ = std::fs::remove_file(&wav);
}

/// An off-center `--offset-hz` still decodes inside the window.
#[test]
fn wspr_offset_round_trip() {
    let wav = scratch("wspr-off");
    let path = wav.to_string_lossy().into_owned();
    let (ok, _, stderr) = run(&[
        "wspr",
        "gen",
        "--callsign",
        "G4JNT",
        "--grid",
        "IO90",
        "--power",
        "30",
        "--offset-hz",
        "1430",
        "-o",
        &path,
    ]);
    assert!(ok, "wspr gen failed: {stderr}");
    let (ok, stdout, stderr) = run(&["wspr", "decode", "--window", "90", &path]);
    assert!(ok, "wspr decode failed: {stderr}");
    assert!(stdout.contains("G4JNT IO90 30 dBm"), "got: {stdout}");
    let _ = std::fs::remove_file(&wav);
}

/// Bad WSPR values and unsupported captures exit nonzero with
/// explanatory messages.
#[test]
fn wspr_bad_inputs_exit_nonzero() {
    // Compound callsign rejected at message validation.
    let (ok, _, stderr) = run(&[
        "wspr",
        "gen",
        "--callsign",
        "K1ABC/P",
        "--grid",
        "FN42",
        "--power",
        "37",
        "-o",
        "x.wav",
    ]);
    assert!(!ok);
    assert!(stderr.contains("compound"), "got: {stderr}");

    // Nonstandard power.
    let (ok, _, stderr) = run(&[
        "wspr",
        "gen",
        "--callsign",
        "K1ABC",
        "--grid",
        "FN42",
        "--power",
        "38",
        "-o",
        "x.wav",
    ]);
    assert!(!ok);
    assert!(stderr.contains("0, 3 or 7"), "got: {stderr}");

    // Malformed locator: rejected while parsing --grid, naming the flag.
    let (ok, _, stderr) = run(&[
        "wspr",
        "gen",
        "--callsign",
        "K1ABC",
        "--grid",
        "SN42",
        "--power",
        "37",
        "-o",
        "x.wav",
    ]);
    assert!(!ok);
    assert!(stderr.contains("bad --grid 'SN42'"), "got: {stderr}");

    // A well-formed subsquare is a valid locator but does not fit a
    // type-1 message: rejected, never truncated to FN42.
    let (ok, _, stderr) = run(&[
        "wspr",
        "gen",
        "--callsign",
        "K1ABC",
        "--grid",
        "FN42ab",
        "--power",
        "37",
        "-o",
        "x.wav",
    ]);
    assert!(!ok);
    assert!(
        stderr.contains("must be exactly 4 characters"),
        "got: {stderr}"
    );

    // A 44.1 kHz WAV is rejected outright (no resampler).
    let wav = write_wav("wspr-rate", 44_100, &[0i16; 1024]);
    let path = wav.to_string_lossy().into_owned();
    let (ok, _, stderr) = run(&["wspr", "decode", &path]);
    assert!(!ok);
    assert!(stderr.contains("fixed at 12000 Hz"), "got: {stderr}");
    let _ = std::fs::remove_file(&wav);

    // A too-short 12 kHz capture is rejected with the sample math.
    let wav = write_wav("wspr-short", 12_000, &[0i16; 12_000]);
    let path = wav.to_string_lossy().into_owned();
    let (ok, _, stderr) = run(&["wspr", "decode", &path]);
    assert!(!ok);
    assert!(stderr.contains("too short"), "got: {stderr}");
    let _ = std::fs::remove_file(&wav);

    // Window validation.
    let (ok, _, stderr) = run(&["wspr", "decode", "--window", "5", "nonexistent.wav"]);
    assert!(!ok, "got: {stderr}");
}

/// `ft8 gen` → `ft8 decode` round trip: a standard CQ message decodes
/// back verbatim with plausible quality metrics.
#[test]
fn ft8_gen_decode_round_trip() {
    let wav = scratch("ft8");
    let path = wav.to_string_lossy().into_owned();
    let (ok, _, stderr) = run(&["ft8", "gen", "--message", "CQ K1ABC FN42", "-o", &path]);
    assert!(ok, "ft8 gen failed: {stderr}");
    let (ok, stdout, stderr) = run(&["ft8", "decode", &path]);
    assert!(ok, "ft8 decode failed: {stderr}");
    assert!(stdout.contains("CQ K1ABC FN42"), "got: {stdout}");
    assert!(stdout.contains("freq 1500."), "got: {stdout}");
    let _ = std::fs::remove_file(&wav);
}

/// An off-center `--offset-hz` free-text message still decodes inside
/// the window, with a narrower `--window`.
#[test]
fn ft8_offset_free_text_round_trip() {
    let wav = scratch("ft8-off");
    let path = wav.to_string_lossy().into_owned();
    let (ok, _, stderr) = run(&[
        "ft8",
        "gen",
        "--message",
        "TNX 73 GL",
        "--free-text",
        "--offset-hz",
        "1420",
        "-o",
        &path,
    ]);
    assert!(ok, "ft8 gen failed: {stderr}");
    let (ok, stdout, stderr) = run(&["ft8", "decode", "--window", "120", &path]);
    assert!(ok, "ft8 decode failed: {stderr}");
    assert!(stdout.contains("TNX 73 GL"), "got: {stdout}");
    assert!(stdout.contains("freq 1420."), "got: {stdout}");
    let _ = std::fs::remove_file(&wav);
}

/// A standard exchange with R flag and report survives the CLI parser
/// and the round trip.
#[test]
fn ft8_report_round_trip() {
    let wav = scratch("ft8-rpt");
    let path = wav.to_string_lossy().into_owned();
    let (ok, _, stderr) = run(&["ft8", "gen", "--message", "K1ABC W9XYZ R-08", "-o", &path]);
    assert!(ok, "ft8 gen failed: {stderr}");
    let (ok, stdout, stderr) = run(&["ft8", "decode", &path]);
    assert!(ok, "ft8 decode failed: {stderr}");
    assert!(stdout.contains("K1ABC W9XYZ R-08"), "got: {stdout}");
    let _ = std::fs::remove_file(&wav);
}

/// Bad FT8 values and unsupported captures exit nonzero with
/// explanatory messages.
#[test]
fn ft8_bad_inputs_exit_nonzero() {
    // Compound callsign rejected at message validation.
    let (ok, _, stderr) = run(&["ft8", "gen", "--message", "CQ K1ABC/P FN42", "-o", "x.wav"]);
    assert!(!ok);
    assert!(stderr.contains("compound"), "got: {stderr}");

    // A malformed locator in the trailer is named as such.
    let (ok, _, stderr) = run(&["ft8", "gen", "--message", "CQ K1ABC SN42", "-o", "x.wav"]);
    assert!(!ok);
    assert!(stderr.contains("bad grid 'SN42'"), "got: {stderr}");

    // A subsquare parses but does not fit `g15`: rejected, not
    // truncated.
    let (ok, _, stderr) = run(&["ft8", "gen", "--message", "CQ K1ABC FN42ab", "-o", "x.wav"]);
    assert!(!ok);
    assert!(
        stderr.contains("must be exactly 4 characters"),
        "got: {stderr}"
    );

    // Directed CQ is outside the supported subset.
    let (ok, _, stderr) = run(&["ft8", "gen", "--message", "CQ DX K1ABC FN42", "-o", "x.wav"]);
    assert!(!ok);
    assert!(
        stderr.contains("subset") || stderr.contains("parse"),
        "got: {stderr}"
    );

    // Free text too long.
    let (ok, _, stderr) = run(&[
        "ft8",
        "gen",
        "--message",
        "FOURTEEN CHARS",
        "--free-text",
        "-o",
        "x.wav",
    ]);
    assert!(!ok);
    assert!(stderr.contains("13"), "got: {stderr}");

    // A 44.1 kHz WAV is rejected outright (no resampler).
    let wav = write_wav("ft8-rate", 44_100, &[0i16; 1024]);
    let path = wav.to_string_lossy().into_owned();
    let (ok, _, stderr) = run(&["ft8", "decode", &path]);
    assert!(!ok);
    assert!(stderr.contains("fixed at 12000 Hz"), "got: {stderr}");
    let _ = std::fs::remove_file(&wav);

    // A too-short 12 kHz capture is rejected with the sample math.
    let wav = write_wav("ft8-short", 12_000, &[0i16; 12_000]);
    let path = wav.to_string_lossy().into_owned();
    let (ok, _, stderr) = run(&["ft8", "decode", &path]);
    assert!(!ok);
    assert!(stderr.contains("too short"), "got: {stderr}");
    let _ = std::fs::remove_file(&wav);

    // Window validation.
    let (ok, _, stderr) = run(&["ft8", "decode", "--window", "20", "nonexistent.wav"]);
    assert!(!ok, "got: {stderr}");
}

/// `m17 gen` → `m17 decode` round trip: LSF addresses and the packet
/// payload come back from the 48 kHz WAV.
#[test]
fn m17_gen_decode_round_trip() {
    let wav = scratch("m17");
    let path = wav.to_string_lossy().into_owned();
    let (ok, _, stderr) = run(&[
        "m17",
        "gen",
        "--src",
        "N0CALL",
        "--dst",
        "BROADCAST",
        "--text",
        "Greetings from the warble CLI over M17!",
        "-o",
        &path,
    ]);
    assert!(ok, "m17 gen failed: {stderr}");
    let (ok, stdout, stderr) = run(&["m17", "decode", &path]);
    assert!(ok, "m17 decode failed: {stderr}");
    assert!(
        stdout.contains("N0CALL") && stdout.contains("@ALL"),
        "got: {stdout}"
    );
    assert!(
        stdout.contains("payload: Greetings from the warble CLI over M17!"),
        "got: {stdout}"
    );
    assert!(stderr.contains("1 packet(s)"), "got: {stderr}");
    let _ = std::fs::remove_file(&wav);
}

/// `m17 gen` with a directed destination and nonzero CAN round trips.
#[test]
fn m17_directed_can_round_trip() {
    let wav = scratch("m17-can");
    let path = wav.to_string_lossy().into_owned();
    let (ok, _, stderr) = run(&[
        "m17", "gen", "--src", "AB1CDE", "--dst", "XY9ZZZ", "--can", "7", "--text", "ping", "-o",
        &path,
    ]);
    assert!(ok, "m17 gen failed: {stderr}");
    let (ok, stdout, stderr) = run(&["m17", "decode", &path]);
    assert!(ok, "m17 decode failed: {stderr}");
    assert!(
        stdout.contains("AB1CDE") && stdout.contains("XY9ZZZ") && stdout.contains("CAN 7"),
        "got: {stdout}"
    );
    assert!(stdout.contains("payload: ping"), "got: {stdout}");
    let _ = std::fs::remove_file(&wav);
}

/// `m17` bad inputs are rejected with a nonzero exit and a pointed
/// message.
#[test]
fn m17_bad_inputs_exit_nonzero() {
    // Callsign outside the base-40 alphabet.
    let (ok, _, stderr) = run(&[
        "m17",
        "gen",
        "--src",
        "bad_call!",
        "--dst",
        "BROADCAST",
        "--text",
        "x",
        "-o",
        "x.wav",
    ]);
    assert!(!ok);
    assert!(stderr.contains("bad --src"), "got: {stderr}");

    // A broadcast source is meaningless.
    let (ok, _, stderr) = run(&[
        "m17",
        "gen",
        "--src",
        "BROADCAST",
        "--dst",
        "N0CALL",
        "--text",
        "x",
        "-o",
        "x.wav",
    ]);
    assert!(!ok);
    assert!(
        stderr.contains("source must be a callsign"),
        "got: {stderr}"
    );

    // CAN out of range.
    let (ok, _, stderr) = run(&[
        "m17",
        "gen",
        "--src",
        "N0CALL",
        "--dst",
        "BROADCAST",
        "--can",
        "16",
        "--text",
        "x",
        "-o",
        "x.wav",
    ]);
    assert!(!ok);
    assert!(stderr.contains("bad --can"), "got: {stderr}");

    // A 44.1 kHz WAV is rejected outright (no resampler).
    let wav = write_wav("m17-rate", 44_100, &[0i16; 1024]);
    let path = wav.to_string_lossy().into_owned();
    let (ok, _, stderr) = run(&["m17", "decode", &path]);
    assert!(!ok);
    assert!(stderr.contains("fixed at 48000 Hz"), "got: {stderr}");
    let _ = std::fs::remove_file(&wav);

    // Silence decodes nothing and exits nonzero.
    let wav = write_wav("m17-silence", 48_000, &[0i16; 48_000]);
    let path = wav.to_string_lossy().into_owned();
    let (ok, _, stderr) = run(&["m17", "decode", &path]);
    assert!(!ok);
    assert!(stderr.contains("no complete M17 packet"), "got: {stderr}");
    let _ = std::fs::remove_file(&wav);
}

mod live_capture_plumbing {
    use super::live_capture::plumbing::{
        ChunkFeed, RatePlan, downmix_frame_i16, f32_to_i16, plan_rate,
    };
    use warble::SampleRate;
    use warble::aprs::{AprsPacket, Status};
    use warble::ax25::Address;
    use warble::tnc::{DefaultTncReceiver, TncConfig, TncReceiver, TncTransmitter};

    /// Synthesized transmission: a status report as i16 samples at `hz`.
    fn fake_source(hz: u32) -> Vec<i16> {
        let rate = SampleRate::new(hz).expect("rate");
        let tx = TncTransmitter::new(TncConfig::bell_202(rate).expect("config"));
        let packet = AprsPacket::Status(Status {
            text: b"live capture plumbing",
        });
        tx.transmit_to_vec_i16(
            &packet,
            Address::new(b"APRS", 0).expect("dest"),
            Address::new(b"N4CALL", 2).expect("src"),
            &[],
        )
        .expect("samples")
    }

    fn receiver(hz: u32) -> DefaultTncReceiver {
        let rate = SampleRate::new(hz).expect("rate");
        TncReceiver::new(TncConfig::bell_202(rate).expect("config")).expect("receiver")
    }

    #[test]
    fn rate_plan_in_window_is_direct() {
        for hz in [8_000, 22_050, 44_100, 48_000] {
            assert_eq!(
                plan_rate(hz).expect("plan"),
                RatePlan {
                    decode_hz: hz,
                    keep_every: 1
                }
            );
        }
    }

    #[test]
    fn rate_plan_decimates_integer_multiples() {
        // 96/192 kHz are common device defaults: 2:1 and 4:1 down to
        // 48 kHz; 88.2 kHz lands on 44.1 kHz.
        for (device, decode, keep) in [
            (96_000, 48_000, 2),
            (192_000, 48_000, 4),
            (88_200, 44_100, 2),
        ] {
            assert_eq!(
                plan_rate(device).expect("plan"),
                RatePlan {
                    decode_hz: decode,
                    keep_every: keep
                }
            );
        }
    }

    #[test]
    fn rate_plan_refuses_unworkable_rates_with_guidance() {
        // Below the window, and a rate with no in-window integer
        // divisor above it.
        for hz in [4_000, 96_001] {
            let err = plan_rate(hz).expect_err("must refuse");
            assert!(err.contains("48000"), "guidance missing: {err}");
            assert!(err.contains("warble decode -"), "guidance missing: {err}");
        }
    }

    #[test]
    fn f32_conversion_scales_and_saturates() {
        assert_eq!(f32_to_i16(0.0), 0);
        assert_eq!(f32_to_i16(1.0), 32767);
        assert_eq!(f32_to_i16(-1.0), -32767);
        assert_eq!(f32_to_i16(2.5), 32767); // clipped, not wrapped
        assert_eq!(f32_to_i16(-2.5), -32768);
    }

    #[test]
    fn downmix_averages_channels() {
        assert_eq!(downmix_frame_i16(&[100, 300]), 200);
        assert_eq!(downmix_frame_i16(&[-100, 100]), 0);
        assert_eq!(downmix_frame_i16(&[7]), 7);
        assert_eq!(downmix_frame_i16(&[]), 0);
    }

    /// Mono audio fed in callback-sized chunks decodes the frame; the
    /// chunk boundaries must not matter.
    #[test]
    fn chunk_feed_mono_decodes_across_chunk_boundaries() {
        let hz = 48_000;
        let samples = fake_source(hz);
        let mut rx = receiver(hz);
        let mut feed = ChunkFeed::new(
            1,
            RatePlan {
                decode_hz: hz,
                keep_every: 1,
            },
        );
        let mut lines = Vec::new();
        // 480-sample chunks: a typical 10 ms device callback.
        for chunk in samples.chunks(480) {
            lines.extend(feed.push_i16(chunk, &mut rx));
        }
        assert_eq!(lines.len(), 1, "got: {lines:?}");
        assert!(lines[0].contains("N4CALL-2>APRS"), "got: {}", lines[0]);
        assert!(
            lines[0].contains("live capture plumbing"),
            "got: {}",
            lines[0]
        );
    }

    /// Fake stereo (the mono signal duplicated into two interleaved
    /// channels) downmixes and decodes identically.
    #[test]
    fn chunk_feed_stereo_downmix_decodes() {
        let hz = 48_000;
        let samples = fake_source(hz);
        let stereo: Vec<i16> = samples.iter().flat_map(|&s| [s, s]).collect();
        let mut rx = receiver(hz);
        let mut feed = ChunkFeed::new(
            2,
            RatePlan {
                decode_hz: hz,
                keep_every: 1,
            },
        );
        let mut lines = Vec::new();
        for chunk in stereo.chunks(512) {
            lines.extend(feed.push_i16(chunk, &mut rx));
        }
        assert_eq!(lines.len(), 1, "got: {lines:?}");
        assert!(lines[0].contains("N4CALL-2>APRS"), "got: {}", lines[0]);
    }

    /// A fake 96 kHz device (each 48 kHz sample doubled) decodes
    /// through the 2:1 decimation plan.
    #[test]
    fn chunk_feed_decimates_fake_96k_device() {
        let samples = fake_source(48_000);
        let device: Vec<i16> = samples.iter().flat_map(|&s| [s, s]).collect();
        let plan = plan_rate(96_000).expect("plan");
        let mut rx = receiver(plan.decode_hz);
        let mut feed = ChunkFeed::new(1, plan);
        let mut lines = Vec::new();
        for chunk in device.chunks(960) {
            lines.extend(feed.push_i16(chunk, &mut rx));
        }
        assert_eq!(lines.len(), 1, "got: {lines:?}");
        assert!(lines[0].contains("N4CALL-2>APRS"), "got: {}", lines[0]);
    }

    /// The f32 path (scaled copies of the same signal) decodes too.
    #[test]
    fn chunk_feed_f32_path_decodes() {
        let hz = 48_000;
        let samples = fake_source(hz);
        let as_f32: Vec<f32> = samples.iter().map(|&s| f32::from(s) / 32767.0).collect();
        let mut rx = receiver(hz);
        let mut feed = ChunkFeed::new(
            1,
            RatePlan {
                decode_hz: hz,
                keep_every: 1,
            },
        );
        let mut lines = Vec::new();
        for chunk in as_f32.chunks(480) {
            lines.extend(feed.push_f32(chunk, &mut rx));
        }
        assert_eq!(lines.len(), 1, "got: {lines:?}");
        assert!(lines[0].contains("N4CALL-2>APRS"), "got: {}", lines[0]);
    }
}

/// The renderers must publish a Mic-E position at the precision the
/// sender declared, not the precision the wire happens to carry.
///
/// This is a guard against a specific, already-made mistake. Mic-E
/// spells position ambiguity in the destination address and always
/// transmits the longitude at full precision; chapter 10 makes
/// discarding the matching low-order digits the receiver's job.
/// `MicE::coordinates()` does that. Both CLI renderers read
/// `report.latitude` and `report.longitude` directly instead, so the
/// library was corrected and the tool went on publishing a longitude up
/// to 33 km more precise than the station claimed.
///
/// A doc comment on the fields would not have caught that, because the
/// author of the renderer was the same project that wrote the doc
/// comment. Driving the built binary does.
///
/// The vector is chapter 10's own worked example: destination `T4SQZZ`
/// declares two ambiguous digits, and the longitude bytes `(_f` are
/// 112 degrees 7.74 minutes, which the spec says to report as 112
/// degrees 7 minutes.
#[test]
fn mic_e_ambiguity_is_honoured_by_both_renderers() {
    let line = "N0CALL>T4SQZZ,TCPIP*,qAC,TEST:'(_f \u{1c}>/]\n";

    let (ok, out, err) = run_with_stdin(
        &["decode", "--tnc2", "--output-format", "jsonl", "-"],
        line.as_bytes(),
    );
    assert!(ok, "jsonl decode failed: {err}");
    assert!(
        out.contains("\"ambiguity_digits\":2"),
        "the destination declares two ambiguous digits: {out}"
    );
    // 112 deg 7 min = 112.116667. The unmasked wire value is
    // 112.129000, which is what this used to print.
    assert!(
        out.contains("-112.116667"),
        "chapter 10 says to report 112 deg 7 min; got {out}"
    );
    assert!(
        !out.contains("-112.129"),
        "the full-precision longitude must not reach the output: {out}"
    );

    let (ok, out, err) = run_with_stdin(&["decode", "--tnc2", "-"], line.as_bytes());
    assert!(ok, "text decode failed: {err}");
    assert!(
        out.contains("-112.1167"),
        "the text renderer must mask too; got {out}"
    );
}

/// Chapter 6 ambiguity must reach both renderers, on both axes.
///
/// The companion to `mic_e_ambiguity_is_honoured_by_both_renderers`,
/// and it exists because the same mistake was made twice: the library
/// masks in `coordinates()`, and every renderer read the `latitude` and
/// `longitude` fields instead. For a position report the latitude
/// arrives already blanked, so only the longitude reveals it, which is
/// what makes the bug quiet.
///
/// `4314.  N` with `07742.60W` is the case that matters: the sender
/// declared precision to the whole minute and spelled the longitude in
/// full, and chapter 6 says the level "will automatically apply to the
/// longitude as well". The reported longitude must be 77 deg 42.00 min,
/// not 77 deg 42.60 min, a difference of about 830 m.
#[test]
fn position_ambiguity_is_honoured_by_both_renderers() {
    let line = "AC2GW>APWW11,TCPIP*,qAC,T2CAEAST:@140221h4314.  NI07742.60W#APRS-IS for Win32\n";

    let (ok, out, err) = run_with_stdin(
        &["decode", "--tnc2", "--output-format", "jsonl", "-"],
        line.as_bytes(),
    );
    assert!(ok, "jsonl decode failed: {err}");
    assert!(
        out.contains("\"ambiguity_digits\":2"),
        "the latitude blanks two digits: {out}"
    );
    assert!(
        out.contains("-77.7"),
        "77 deg 42.00 min is -77.700000; got {out}"
    );
    assert!(
        !out.contains("-77.71"),
        "the full-precision longitude must not reach the output: {out}"
    );

    let (ok, out, err) = run_with_stdin(&["decode", "--tnc2", "-"], line.as_bytes());
    assert!(ok, "text decode failed: {err}");
    assert!(
        out.contains("-77.7000"),
        "the text renderer must mask too; got {out}"
    );
}

// ----------------------------------------------------------------- ptt

/// `warble ptt --list` enumerates without touching a radio.
///
/// Exits zero whether or not any port exists, because "no serial ports
/// found" is a fact about the machine, not a failure of the command.
#[cfg(feature = "ptt")]
#[test]
fn ptt_list_runs_without_hardware() {
    let (ok, out, err) = run(&["ptt", "--list"]);
    assert!(ok, "ptt --list failed: {err}");
    assert!(!out.is_empty(), "expected a port list or a plain statement");
}

/// Every way of asking for a transmission that could not happen is
/// refused **before** the port is opened.
///
/// This is the one subcommand that can put a signal on the air, so an
/// argument error must not be discovered halfway through a key-down.
/// Each case below is checked with a port that does not exist: if any
/// of them reached the open, the error text would say so instead.
#[cfg(feature = "ptt")]
#[test]
fn ptt_refuses_impossible_transmissions_before_opening_the_port() {
    // Neither a duration nor a player: nothing to hold the line for.
    let (ok, _, err) = run(&["ptt", "--port", "/nonexistent/tty"]);
    assert!(!ok);
    assert!(
        err.contains("nothing to do"),
        "expected the nothing-to-do error, got {err}"
    );

    // The fixed overheads already consume the safety bound, so the
    // transmission could never run inside it.
    let (ok, _, err) = run(&[
        "ptt",
        "--port",
        "/nonexistent/tty",
        "--lead",
        "500",
        "--tail",
        "200",
        "--max",
        "600",
        "--hold",
        "10",
    ]);
    assert!(!ok);
    assert!(
        err.contains("could never run"),
        "expected the lead+tail vs max error, got {err}"
    );

    // A port that cannot be opened is reported as such, not as a
    // successful silent no-op.
    let (ok, _, err) = run(&["ptt", "--port", "/nonexistent/tty", "--hold", "10"]);
    assert!(!ok);
    assert!(
        err.contains("cannot open serial port"),
        "expected an open failure, got {err}"
    );
}

/// `--hold` and a player are mutually exclusive, and clap enforces it.
#[cfg(feature = "ptt")]
#[test]
fn ptt_hold_and_a_player_are_mutually_exclusive() {
    let (ok, _, err) = run(&[
        "ptt",
        "--port",
        "/nonexistent/tty",
        "--hold",
        "10",
        "--",
        "true",
    ]);
    assert!(!ok);
    assert!(
        err.contains("cannot be used with"),
        "expected clap's conflict error, got {err}"
    );
}

// --------------------------------------------------------------- level

/// Raw s16le mono PCM of a sine at `peak_dbfs`, for driving the meter
/// without a sound card.
fn pcm_tone(peak_dbfs: f64, secs: f64, hz: f64, rate: u32) -> Vec<u8> {
    let amp = 10f64.powf(peak_dbfs / 20.0) * 32767.0;
    let n = (f64::from(rate) * secs) as usize;
    let mut out = Vec::with_capacity(n * 2);
    for i in 0..n {
        let t = 2.0 * std::f64::consts::PI * hz * i as f64 / f64::from(rate);
        out.extend_from_slice(&((amp * t.sin()) as i16).to_le_bytes());
    }
    out
}

/// The verdict boundaries, driven end to end through the binary.
///
/// The thresholds are judgement rather than physics, which is exactly
/// why each one is pinned: moving a boundary should be a visible
/// decision, not a drift.
#[test]
fn level_reports_the_verdict_for_each_band() {
    let rate = 44100;
    let cases: [(&str, Vec<u8>); 4] = [
        ("MUTED", vec![0u8; rate as usize * 2]),
        ("TOO QUIET", pcm_tone(-45.0, 1.0, 1200.0, rate)),
        // A sine's rms sits 3 dB below its peak, so -20 dBFS peak is
        // -23 dBFS rms: inside the -28..-12 target band.
        ("GOOD", pcm_tone(-20.0, 1.0, 1200.0, rate)),
        ("HOT", pcm_tone(-6.0, 1.0, 1200.0, rate)),
    ];
    for (want, pcm) in cases {
        let (ok, _, err) = run_with_stdin(&["level", "--rate", "44100", "--for", "1", "-"], &pcm);
        assert!(ok, "level exited non-zero for {want}: {err}");
        assert!(
            err.contains(want),
            "expected verdict {want}, got: {}",
            err.lines().last().unwrap_or("")
        );
    }
}

/// A single pinned sample is CLIPPING, even though the rms is quiet.
///
/// This is the case the meter exists for. RMS cannot see clipping at
/// all and peak saturates at 100% whether one sample is pinned or ten
/// thousand, so a reading can look just loud while the tone ratio the
/// discriminator measures is already destroyed. MEASURED on a real
/// interface: a capture read -0.8 dBFS, looked loud, and was 23%
/// clipped with nothing in it decodable.
#[test]
fn level_reports_clipping_that_rms_hides() {
    let rate = 44100;
    let mut pcm = pcm_tone(-30.0, 1.0, 1200.0, rate);
    // One sample pinned, in an otherwise quiet buffer.
    pcm[0..2].copy_from_slice(&i16::MAX.to_le_bytes());
    let (ok, _, err) = run_with_stdin(&["level", "--rate", "44100", "--for", "1", "-"], &pcm);
    assert!(ok, "{err}");
    assert!(
        err.contains("CLIPPING"),
        "one pinned sample must outrank the rms band: {}",
        err.lines().next().unwrap_or("")
    );
    assert!(
        err.contains("clip 1"),
        "the clipped-sample count is the point: {}",
        err.lines().next().unwrap_or("")
    );
}

/// `--until-good` stops on a good level and fails on one that never
/// arrives.
///
/// The hold is counted in **audio** time, not wall clock. A file or a
/// fast pipe delivers seconds of audio in milliseconds, so a wall clock
/// would never reach a hold it has already heard; that bug was real and
/// this is what caught it.
#[test]
fn level_until_good_measures_audio_time_not_wall_clock() {
    let rate = 44100;
    let good = pcm_tone(-20.0, 2.0, 1200.0, rate);
    let (ok, _, err) = run_with_stdin_early_exit(
        &[
            "level",
            "--rate",
            "44100",
            "--until-good",
            "1",
            "--for",
            "30",
            "-",
        ],
        &good,
    );
    assert!(ok, "a good level must satisfy --until-good: {err}");
    assert!(err.contains("held in range"), "{err}");

    // Silence never satisfies it, and the failure names the reading.
    let (ok, _, err) = run_with_stdin(
        &[
            "level",
            "--rate",
            "44100",
            "--until-good",
            "1",
            "--for",
            "1",
            "-",
        ],
        &vec![0u8; rate as usize * 2],
    );
    assert!(!ok, "silence must not satisfy --until-good");
    assert!(err.contains("never held in range"), "{err}");
    assert!(
        err.contains("MUTED"),
        "the failure must name the reading: {err}"
    );
}

/// Every way of asking for a meter that could never finish is refused.
#[test]
fn level_requires_a_terminating_condition() {
    let (ok, _, err) = run_with_stdin(&["level", "--rate", "44100", "-"], &[0u8; 64]);
    assert!(!ok);
    assert!(err.contains("no terminating condition"), "{err}");

    let (ok, _, err) = run(&["level", "--rate", "44100", "--for", "1", "recording.wav"]);
    assert!(!ok);
    assert!(err.contains("reads audio from stdin"), "{err}");
}

// ---------------------------------------------------------------------
// Comment views: !DAO! and base-91 telemetry
// ---------------------------------------------------------------------

/// The built binary must report the position `!DAO!` refines it to.
///
/// This is the third field-versus-accessor case, after Mic-E ambiguity
/// and uncompressed ambiguity, and the first two were both shipped
/// broken because every renderer read the raw fields instead of
/// `coordinates()`. A doc comment did not prevent it and cannot, so the
/// guard is a test that drives the binary and compares a rendered
/// number against one that visibly differs.
#[test]
fn dao_refinement_reaches_the_rendered_position() {
    // Without the refinement this is 49.058333; `!w12!` adds 16/91 of
    // a hundredth of a minute, which shows in the sixth decimal place.
    let line = b"K1ABC>APRS:!4903.50N/07201.75W-Test!w12!\r\n";
    let (ok, out, _) = run_with_stdin(&["decode", "--tnc2", "--output-format", "jsonl", "-"], line);
    assert!(ok, "{out}");
    assert!(
        out.contains("49.058363"),
        "the rendered latitude must carry the DAO refinement: {out}"
    );
    assert!(
        !out.contains("49.058333"),
        "the unrefined field value must not be what is reported: {out}"
    );
    assert!(out.contains("\"datum\":\"w\""), "{out}");
}

/// Telemetry bytes must not be read as a `!DAO!` that moves a position.
///
/// MEASURED over a 64 918-packet capture, scanning for `!DAO!` without
/// excluding the base-91 telemetry block yields 51 false positives,
/// three inside the telemetry of a compressed position. This is one of
/// those packets: its payload contains the literal bytes `!X!Y!`.
#[test]
fn telemetry_payload_does_not_move_the_position() {
    let line = b"K3ABC>APRS:!4903.50N/07201.75W-APRS Digi|$d!X!Y!U&!!(|\r\n";
    let (ok, out, _) = run_with_stdin(&["decode", "--tnc2", "--output-format", "jsonl", "-"], line);
    assert!(ok, "{out}");
    assert!(
        out.contains("49.058333"),
        "a telemetry payload must not refine the position: {out}"
    );
    assert!(
        !out.contains("\"dao\""),
        "and must not be reported as a datum option: {out}"
    );
    assert!(
        out.contains("comment_telemetry"),
        "it is telemetry, and should be read as telemetry: {out}"
    );
}

/// A comment view does not disturb the bytes it reads.
#[test]
fn comment_views_leave_the_rebuild_exact() {
    let line = b"K2XYZ>APRS:!4903.50N/07201.75W-hi|ss1122334455!\"|!w12!\r\n";
    let (ok, out, _) = run_with_stdin(
        &[
            "decode",
            "--tnc2",
            "--verify-rebuild",
            "--output-format",
            "jsonl",
            "-",
        ],
        line,
    );
    assert!(ok, "{out}");
    assert!(
        out.contains("\"rebuild\":\"exact\""),
        "reading the comment must not rewrite it: {out}"
    );
    // Both views present at once, and the spec's own sequence value.
    assert!(out.contains("\"seq\":7544"), "{out}");
    assert!(out.contains("\"datum\":\"w\""), "{out}");
}

// ---------------------------------------------------------------------
// Telemetry definition messages
// ---------------------------------------------------------------------

/// The renderer must key a definition on the sender, not the addressee.
///
/// A `PARM.`/`UNIT.`/`EQNS.`/`BITS.` message describes the telemetry of
/// the station that **sent** it, and usually addresses itself, which
/// makes the addressee look like the right key. MEASURED over 95 219
/// packets, 277 of 5 805 address someone else: an EchoLink and SvxLink
/// family sending from `KJ6ZD` addressed to `EL-KJ6ZD`, another using
/// `ER-`, and 91 unrelated. Binding on the addressee never binds and
/// never errors, so the failure is silent and this drives the built
/// binary to catch it, the way the ambiguity renderers are caught.
#[test]
fn telemetry_definition_names_the_sender_not_the_addressee() {
    let line = b"KJ6ZD>APSVX1,TCPIP*,qAS,KJ6ZD::EL-KJ6ZD :UNIT.erlang,erlang,receptions\r\n";
    let (ok, out, _) = run_with_stdin(&["decode", "--tnc2", "-"], line);
    assert!(ok, "{out}");
    assert!(out.contains("UNIT"), "the kind must be named: {out}");
    assert!(
        out.contains("SENDER"),
        "the output must say the metadata belongs to the sender: {out}"
    );
    assert!(
        out.contains("EL-KJ6ZD"),
        "the differing addressee must still be shown: {out}"
    );
    assert!(
        out.contains("erlang"),
        "the typed labels must appear: {out}"
    );
}

/// A definition message still carries its text, and still rebuilds.
///
/// The typed reading is a view, so nothing about the message record may
/// move: if this ever fails, typing has stopped being free.
#[test]
fn telemetry_definition_is_a_view_and_does_not_disturb_the_message() {
    let line = b"N0QBF-11>APRS::N0QBF-11 :BITS.10110000,Big Balloon\r\n";
    let (ok, out, _) = run_with_stdin(
        &[
            "decode",
            "--tnc2",
            "--verify-rebuild",
            "--output-format",
            "jsonl",
            "-",
        ],
        line,
    );
    assert!(ok, "{out}");
    assert!(
        out.contains("\"rebuild\":\"exact\""),
        "typing a definition must not disturb the rebuild: {out}"
    );
    assert!(
        out.contains("BITS.10110000,Big Balloon"),
        "the message text must survive verbatim: {out}"
    );
    assert!(
        out.contains("telemetry_definition"),
        "and the typed view must sit beside it: {out}"
    );
    assert!(out.contains("Big Balloon"), "{out}");
}

// ---------------------------------------------------------------------
// aprsis
//
// No test here opens a socket. APRS-IS is a shared volunteer network
// and a test suite has no business connecting to it, so what is pinned
// is the argument handling that happens before any connection: the
// combinations that would connect and then deliver nothing.
// ---------------------------------------------------------------------

/// Asking for the filtered port with no filter is refused up front.
///
/// Port 14580 sends nothing at all until a filter subscribes you, so a
/// client that connects without one sits there producing keepalives and
/// looks broken. The error has to name the fix, because the symptom
/// gives no clue.
#[test]
fn aprsis_refuses_a_subscription_that_would_deliver_nothing() {
    let (ok, _, err) = run(&["aprsis", "--callsign", "N0CALL", "--seconds", "1"]);
    assert!(!ok, "a filterless 14580 connection must be refused");
    assert!(err.contains("sends nothing"), "{err}");
    assert!(
        err.contains("--filter") && err.contains("--full-feed"),
        "the error must name both ways out: {err}"
    );
}

/// A filter and the unfiltered feed are mutually exclusive, because the
/// full-feed port ignores filters and would silently oversubscribe.
#[test]
fn aprsis_refuses_a_filter_on_the_unfiltered_feed() {
    let (ok, _, err) = run(&[
        "aprsis",
        "--callsign",
        "N0CALL",
        "--full-feed",
        "--filter",
        "r/39.1/-94.6/250",
        "--count",
        "1",
    ]);
    assert!(!ok, "--filter with --full-feed must be refused");
    assert!(
        err.contains("cannot be used with"),
        "clap should report the conflict: {err}"
    );
}

/// The callsign is required, and an empty one is not a way around it.
///
/// It is the identifier server operators can see and act on, so there
/// is no anonymous path onto the network from this tool.
#[test]
fn aprsis_requires_a_callsign() {
    let (ok, _, err) = run(&["aprsis", "--full-feed", "--count", "1"]);
    assert!(!ok, "--callsign must be required");
    assert!(err.contains("--callsign"), "{err}");

    let (ok, _, err) = run(&["aprsis", "--callsign", "   ", "--full-feed", "--count", "1"]);
    assert!(!ok, "a blank callsign must be refused");
    assert!(err.contains("must not be empty"), "{err}");
}
