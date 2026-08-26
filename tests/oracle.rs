//! Integration tests validating the modem against a reference
//! implementation (an external, independently developed Bell 202 modem).
//!
//! All tests here are `#[ignore]` by default. To run them, set the
//! environment variables `YODEL_REF_GEN` and `YODEL_REF_DECODE` to the
//! absolute paths of the reference WAV generator and decoder binaries,
//! then run `cargo test -- --ignored`.
//!
//! # Unset skips, set-but-wrong fails
//!
//! Every test here resolves its binaries through
//! [`ref_binaries_available`], which implements the rule CONTRIBUTING.md
//! states for the whole tier: an **unset** variable is a legitimate skip
//! (a contributor without the binaries must still get a green
//! `cargo test -- --ignored`), while a variable **set to a path that does
//! not exist** is a hard failure, because somebody meant to run the
//! suite and a single typo would otherwise turn it green while it tested
//! nothing.
//!
//! This file used to get the first half of that wrong: the tests below
//! called [`env_binary`] directly, which panics when a variable is
//! unset, so seventeen of them **failed** rather than skipped for anyone
//! without the reference binaries.

mod common;

use std::path::PathBuf;
use std::process::Command;

use common::*;
use yodel::{AfskDemodulator, Bit, DemodulatorConfig, Modulator, ModulatorConfig, SampleRate};

/// A coordinate magnitude in 1/100 arc-minutes, the unit the fixtures
/// in this file are written in. Storage is finer, so this rounds.
fn hundredths(units: i64) -> i64 {
    let step = yodel::geo::UNITS_PER_HUNDREDTH_MINUTE;
    let half = if units < 0 { -step / 2 } else { step / 2 };
    (units + half) / step
}

/// Resolves one reference-binary variable: `None` when it is unset,
/// which callers turn into a skip, and a hard failure when it is set to
/// something that is not a file.
///
/// Set-but-wrong cannot be allowed to skip. A path typed and mistyped is
/// somebody intending to run this suite, and quietly skipping turns an
/// entire interoperability suite green while it tests nothing at all --
/// the most expensive way a test can fail.
fn ref_binary(var: &str) -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os(var)?);
    assert!(
        path.is_file(),
        "{var}={} does not point to an existing file. Unset the variable \
         to skip this suite deliberately; leaving it set and wrong would \
         pass without testing anything.",
        path.display()
    );
    Some(path)
}

/// True when both reference-binary env vars name real files; otherwise
/// prints a skip notice and returns false.
///
/// Both variables are resolved *before* the skip decision, on purpose:
/// [`ref_binary`] fails on a set-but-wrong path, so a typo in one is
/// reported even when the other is absent. Testing `is_none()` first
/// would let `YODEL_REF_GEN=/typo` with `YODEL_REF_DECODE` unset skip
/// in silence, which is the hole this rule exists to close.
fn ref_binaries_available() -> bool {
    let generator = ref_binary("YODEL_REF_GEN");
    let decoder = ref_binary("YODEL_REF_DECODE");
    if generator.is_none() || decoder.is_none() {
        eprintln!(
            "skipping: set YODEL_REF_GEN and YODEL_REF_DECODE to the \
             reference generator/decoder binaries to run this test"
        );
        return false;
    }
    true
}

/// Fetches a variable known to be set, asserting it names a real file.
///
/// Only ever called behind [`ref_binaries_available`], so the unset arm
/// is unreachable; it panics rather than returning a placeholder so that
/// a future test which forgets the guard fails loudly instead of
/// comparing against nothing.
fn env_binary(var: &str) -> PathBuf {
    let path = std::env::var_os(var).unwrap_or_else(|| {
        panic!(
            "{var} is not set. Set YODEL_REF_GEN and YODEL_REF_DECODE to the \
             absolute paths of the reference implementation's WAV generator and \
             decoder binaries, then run `cargo test -- --ignored`."
        )
    });
    let path = PathBuf::from(path);
    assert!(
        path.is_file(),
        "{var}={} does not point to an existing file",
        path.display()
    );
    path
}

fn scratch_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scratch");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Direction 1: reference generator -> our demodulator -> deframe.
fn oracle_to_us_at_rate(sample_rate: u32) {
    if !ref_binaries_available() {
        return;
    }
    let gen_bin = env_binary("YODEL_REF_GEN");
    let wav_path = scratch_dir().join(format!("oracle_gen_{sample_rate}.wav"));
    // Known test frame in monitoring format: SRC>DEST:info
    let frame_text = "N0CALL-7>APRS:!4237.14NS07120.83W# oracle to us";
    let frame_file = scratch_dir().join(format!("oracle_gen_{sample_rate}.txt"));
    std::fs::write(&frame_file, format!("{frame_text}\n")).unwrap();

    let output = Command::new(&gen_bin)
        .arg("-r")
        .arg(sample_rate.to_string())
        .arg("-o")
        .arg(&wav_path)
        .arg(&frame_file)
        .output()
        .expect("failed to run reference generator");
    assert!(
        output.status.success(),
        "reference generator failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut reader = hound::WavReader::open(&wav_path).unwrap();
    let spec = reader.spec();
    assert_eq!(spec.sample_rate, sample_rate);
    let samples: Vec<i16> = reader.samples::<i16>().map(|s| s.unwrap()).collect();

    let demod = AfskDemodulator::new(
        DemodulatorConfig::bell_202(SampleRate::new(sample_rate).unwrap()).unwrap(),
    )
    .unwrap();
    let line_bits: Vec<Bit> = demod.i16_bits(samples).collect();
    let data_bits = nrzi_decode(&line_bits);
    let frames = hdlc_deframe(&data_bits);
    assert!(
        !frames.is_empty(),
        "no FCS-valid frames recovered from reference audio at {sample_rate} Hz"
    );
    let parsed = frames
        .iter()
        .filter_map(|f| ax25_parse_ui(f))
        .collect::<Vec<_>>();
    assert!(
        parsed.iter().any(|(dest, src, info)| {
            dest == "APRS" && src == "N0CALL-7" && info == b"!4237.14NS07120.83W# oracle to us\n"
        }),
        "expected frame not found; got: {parsed:?}"
    );
}

#[test]
#[ignore = "requires YODEL_REF_GEN / YODEL_REF_DECODE"]
fn oracle_to_us_44100() {
    oracle_to_us_at_rate(44100);
}

#[test]
#[ignore = "requires YODEL_REF_GEN / YODEL_REF_DECODE"]
fn oracle_to_us_48000() {
    oracle_to_us_at_rate(48000);
}

#[test]
#[ignore = "requires YODEL_REF_GEN / YODEL_REF_DECODE"]
fn oracle_to_us_22050() {
    oracle_to_us_at_rate(22050);
}

#[test]
#[ignore = "requires YODEL_REF_GEN / YODEL_REF_DECODE"]
fn oracle_to_us_11025() {
    oracle_to_us_at_rate(11025);
}

#[test]
#[ignore = "requires YODEL_REF_GEN / YODEL_REF_DECODE"]
fn oracle_to_us_8000() {
    oracle_to_us_at_rate(8000);
}

/// Build the full line-coded bit sequence for one UI frame.
fn ui_frame_line_bits(info: &[u8]) -> Vec<Bit> {
    let frame = ax25_ui_frame("APRS", 0, "N0CALL", 7, info);
    let mut with_fcs = frame.clone();
    let fcs = fcs_crc16_x25(&frame);
    with_fcs.push((fcs & 0xFF) as u8);
    with_fcs.push((fcs >> 8) as u8);
    // Generous flag preamble/tail so the decoder's DCD can lock.
    let framed = hdlc_frame(&with_fcs, 45, 5);
    nrzi_encode(&framed)
}

/// Direction 2: our modulator -> WAV -> reference decoder.
fn us_to_oracle_at_rate(sample_rate: u32, use_f32: bool) {
    if !ref_binaries_available() {
        return;
    }
    let decode = env_binary("YODEL_REF_DECODE");
    let line_bits = ui_frame_line_bits(b"us to oracle test");

    let modulator =
        Modulator::new(ModulatorConfig::bell_202(SampleRate::new(sample_rate).unwrap()).unwrap());
    let samples: Vec<i16> = if use_f32 {
        modulator
            .f32_samples(line_bits.into_iter())
            .map(|s| (s * 32767.0).round().clamp(-32768.0, 32767.0) as i16)
            .collect()
    } else {
        modulator.i16_samples(line_bits.into_iter()).collect()
    };

    let tag = if use_f32 { "f32" } else { "i16" };
    let wav_path = scratch_dir().join(format!("us_to_oracle_{sample_rate}_{tag}.wav"));
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&wav_path, spec).unwrap();
    // A little leading/trailing silence so nothing is clipped by the decoder.
    for _ in 0..sample_rate / 10 {
        writer.write_sample(0i16).unwrap();
    }
    for s in &samples {
        writer.write_sample(*s).unwrap();
    }
    for _ in 0..sample_rate / 10 {
        writer.write_sample(0i16).unwrap();
    }
    writer.finalize().unwrap();

    let output = Command::new(&decode)
        .arg(&wav_path)
        .output()
        .expect("failed to run reference decoder");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("N0CALL-7")
            && stdout.contains("APRS")
            && stdout.contains("us to oracle test"),
        "reference decoder did not report our frame at {sample_rate} Hz ({tag}).\nstdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "requires YODEL_REF_GEN / YODEL_REF_DECODE"]
fn us_to_oracle_44100_i16() {
    us_to_oracle_at_rate(44100, false);
}

#[test]
#[ignore = "requires YODEL_REF_GEN / YODEL_REF_DECODE"]
fn us_to_oracle_48000_i16() {
    us_to_oracle_at_rate(48000, false);
}

#[test]
#[ignore = "requires YODEL_REF_GEN / YODEL_REF_DECODE"]
fn us_to_oracle_44100_f32() {
    us_to_oracle_at_rate(44100, true);
}

#[test]
#[ignore = "requires YODEL_REF_GEN / YODEL_REF_DECODE"]
fn us_to_oracle_48000_f32() {
    us_to_oracle_at_rate(48000, true);
}

// ---------------------------------------------------------------------------
// Full protocol stack (APRS -> AX.25 -> NRZI -> AFSK), crate public API only.
// Gated on the `aprs` feature (which implies `ax25`/`nrzi`); run with
// `cargo test --all-features --test oracle -- --ignored`.
// ---------------------------------------------------------------------------
#[cfg(feature = "aprs")]
mod full_stack {
    use super::{PathBuf, env_binary, hundredths, ref_binaries_available, scratch_dir};
    use std::process::Command;

    use yodel::aprs::{
        Addressee, AprsError, AprsPacket, Item, Latitude, Longitude, Message, MessageContent,
        Object, Position, PositionWeather, PositionlessWeather, Symbol, Telemetry, Timestamp,
        WeatherReport, build_ui_frame, packet_from_ui,
    };
    use yodel::ax25::{Address, FrameReceiver, UiFrame, tx_i16};
    use yodel::geo::Ambiguity;
    use yodel::units::{Humidity, Pressure, Rainfall, Speed, Temperature};
    use yodel::{AfskDemodulator, DemodulatorConfig, Modulator, ModulatorConfig, SampleRate};

    /// Writes `samples` (with a little leading/trailing silence) to a 16-bit
    /// mono WAV and returns the path.
    fn write_wav(name: &str, sample_rate: u32, samples: &[i16]) -> PathBuf {
        let wav_path = scratch_dir().join(name);
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&wav_path, spec).unwrap();
        for _ in 0..sample_rate / 10 {
            writer.write_sample(0i16).unwrap();
        }
        for s in samples {
            writer.write_sample(*s).unwrap();
        }
        for _ in 0..sample_rate / 10 {
            writer.write_sample(0i16).unwrap();
        }
        writer.finalize().unwrap();
        wav_path
    }

    /// Runs the reference decoder over a WAV and returns its stdout.
    fn run_ref_decoder(wav_path: &PathBuf) -> String {
        let decode = env_binary("YODEL_REF_DECODE");
        let output = Command::new(&decode)
            .arg(wav_path)
            .output()
            .expect("failed to run reference decoder");
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// Loosely parses the reference decoder's "<n> packets decoded" trailer.
    fn decoded_packet_count(stdout: &str) -> usize {
        let line = stdout
            .lines()
            .find(|l| l.contains("packets decoded"))
            .unwrap_or_else(|| panic!("no 'packets decoded' line in decoder output:\n{stdout}"));
        let head = line.split("packets decoded").next().unwrap();
        // Walk backwards over " " and digits; the line may carry terminal
        // colour escapes before the count.
        let digits: String = head
            .trim_end()
            .chars()
            .rev()
            .take_while(char::is_ascii_digit)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        digits
            .parse()
            .unwrap_or_else(|_| panic!("could not parse packet count from line: {line}"))
    }

    /// Direction A: build an APRS packet with the crate's public `aprs` API,
    /// wrap it in an AX.25 UI frame, run the full public TX pipeline
    /// (HDLC + stuffing -> NRZI -> AFSK) and assert the reference decoder
    /// reports exactly our SRC>DEST header and info field.
    fn full_stack_to_oracle(
        sample_rate: u32,
        packet: &AprsPacket<'_>,
        dest: &str,
        src: &str,
        tag: &str,
    ) {
        if !ref_binaries_available() {
            return;
        }
        let (dest_call, dest_ssid) = split_ssid(dest);
        let (src_call, src_ssid) = split_ssid(src);
        let dest_addr = Address::new(dest_call.as_bytes(), dest_ssid).unwrap();
        let src_addr = Address::new(src_call.as_bytes(), src_ssid).unwrap();

        let mut info_buf = [0u8; 128];
        let mut frame_buf = [0u8; 256];
        let frame_len = build_ui_frame(
            packet,
            dest_addr,
            src_addr,
            &[],
            &mut info_buf,
            &mut frame_buf,
        )
        .unwrap();
        let info_len = packet.build(&mut info_buf).unwrap();
        let info_text = std::str::from_utf8(&info_buf[..info_len])
            .unwrap()
            .to_owned();

        let modulator = Modulator::new(
            ModulatorConfig::bell_202(SampleRate::new(sample_rate).unwrap()).unwrap(),
        );
        let samples: Vec<i16> = tx_i16(&frame_buf[..frame_len], modulator).collect();

        let wav_path = write_wav(
            &format!("full_stack_to_oracle_{tag}_{sample_rate}.wav"),
            sample_rate,
            &samples,
        );
        let stdout = run_ref_decoder(&wav_path);

        // The decoder prints "[chan] SRC>DEST[,PATH]:INFO" per frame.
        let header = format!("{src}>{dest}:{info_text}");
        assert!(
            stdout.lines().any(|l| l.contains(&header)),
            "reference decoder did not report `{header}` at {sample_rate} Hz.\nstdout:\n{stdout}"
        );
        assert_eq!(
            decoded_packet_count(&stdout),
            1,
            "reference decoder packet count mismatch at {sample_rate} Hz.\nstdout:\n{stdout}"
        );
    }

    /// Splits "CALL-N" into ("CALL", N); no dash means SSID 0.
    fn split_ssid(addr: &str) -> (&str, u8) {
        match addr.split_once('-') {
            Some((call, ssid)) => (call, ssid.parse().unwrap()),
            None => (addr, 0),
        }
    }

    /// An uncompressed position report (42°37.14'N 71°20.83'W, `/#`).
    fn oracle_position() -> AprsPacket<'static> {
        AprsPacket::Position(Position {
            ambiguity: Ambiguity::EXACT,
            latitude: Latitude::new(
                (42 * 6000 + 37 * 100 + 14) * yodel::geo::UNITS_PER_HUNDREDTH_MINUTE,
            )
            .unwrap(),
            longitude: Longitude::new(
                -(71 * 6000 + 20 * 100 + 83) * yodel::geo::UNITS_PER_HUNDREDTH_MINUTE,
            )
            .unwrap(),
            symbol: Symbol::DIGI,
            messaging: false,
            compressed: false,
            extension: None,
            comment: b"yodel full stack",
        })
    }

    /// A text message with a message id.
    fn oracle_message() -> AprsPacket<'static> {
        AprsPacket::Message(Message {
            addressee: Addressee::new(b"N0CALL").unwrap(),
            content: MessageContent::Text {
                text: b"full stack oracle",
                id: Some(b"001"),
            },
        })
    }

    #[test]
    #[ignore = "requires YODEL_REF_GEN / YODEL_REF_DECODE"]
    fn full_stack_position_to_oracle_44100() {
        full_stack_to_oracle(44100, &oracle_position(), "APZ001", "N0CALL-1", "pos");
    }

    #[test]
    #[ignore = "requires YODEL_REF_GEN / YODEL_REF_DECODE"]
    fn full_stack_position_to_oracle_48000() {
        full_stack_to_oracle(48000, &oracle_position(), "APZ001", "N0CALL-1", "pos");
    }

    #[test]
    #[ignore = "requires YODEL_REF_GEN / YODEL_REF_DECODE"]
    fn full_stack_message_to_oracle_44100() {
        full_stack_to_oracle(44100, &oracle_message(), "APRS", "N0CALL-1", "msg");
    }

    #[test]
    #[ignore = "requires YODEL_REF_GEN / YODEL_REF_DECODE"]
    fn full_stack_message_to_oracle_48000() {
        full_stack_to_oracle(48000, &oracle_message(), "APRS", "N0CALL-1", "msg");
    }

    /// Runs the reference generator and returns the WAV's samples.
    fn run_ref_generator(args: &[&str], wav_name: &str, sample_rate: u32) -> Vec<i16> {
        let gen_bin = env_binary("YODEL_REF_GEN");
        let wav_path = scratch_dir().join(wav_name);
        let output = Command::new(&gen_bin)
            .arg("-r")
            .arg(sample_rate.to_string())
            .arg("-o")
            .arg(&wav_path)
            .args(args)
            .output()
            .expect("failed to run reference generator");
        assert!(
            output.status.success(),
            "reference generator failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut reader = hound::WavReader::open(&wav_path).unwrap();
        assert_eq!(reader.spec().sample_rate, sample_rate);
        reader.samples::<i16>().map(|s| s.unwrap()).collect()
    }

    /// Runs the full public RX pipeline (demodulator -> NRZI -> HDLC deframer)
    /// over PCM samples and returns the FCS-valid frame bodies.
    fn receive_frames(sample_rate: u32, samples: &[i16]) -> Vec<Vec<u8>> {
        let demod = AfskDemodulator::new(
            DemodulatorConfig::bell_202(SampleRate::new(sample_rate).unwrap()).unwrap(),
        )
        .unwrap();
        let mut receiver: FrameReceiver<512> = FrameReceiver::new(demod);
        let mut frames = Vec::new();
        for &s in samples {
            if let Some(Ok(frame)) = receiver.push_sample_i16(s) {
                frames.push(frame.to_vec());
            }
        }
        frames
    }

    /// Direction B: the reference generator's built-in test frames
    /// (a fixed source address with SSID 15, `DEST TEST`, fox-text info with
    /// packet) through our full public RX pipeline. Only the low-noise packets
    /// are required to survive — the reference decoder itself recovers 2 of 4 —
    /// so the threshold here is: at least packet `0001` (the cleanest) decodes.
    fn oracle_full_stack_to_us(sample_rate: u32) {
        if !ref_binaries_available() {
            return;
        }
        let samples = run_ref_generator(
            &["-n", "4"],
            &format!("oracle_full_stack_{sample_rate}.wav"),
            sample_rate,
        );
        let frames = receive_frames(sample_rate, &samples);
        assert!(
            !frames.is_empty(),
            "no FCS-valid frames recovered from reference audio at {sample_rate} Hz"
        );

        let mut saw_first = false;
        for frame in &frames {
            let ui = UiFrame::parse(frame).unwrap();
            assert_eq!(ui.dest.callsign.as_bytes(), b"TEST");
            let src = ui.src.callsign.as_bytes();
            assert!(
                (1..=6).contains(&src.len())
                    && src
                        .iter()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()),
                "source callsign should parse as a valid amateur callsign: {src:?}"
            );
            assert_eq!(ui.src.ssid.value(), 15);
            assert!(
                ui.info
                    .starts_with(b",The quick brown fox jumps over the lazy dog!"),
                "unexpected info field: {:?}",
                String::from_utf8_lossy(ui.info)
            );
            if ui.info.ends_with(b"0001 of 0004") {
                saw_first = true;
            }
            // The fox text starts with ',' which is not a supported APRS
            // data-type identifier, so the typed error path must trigger.
            assert_eq!(
                packet_from_ui(&ui),
                Err(AprsError::InvalidDataType { got: b',' })
            );
        }
        assert!(
            saw_first,
            "the low-noise packet 0001 was not recovered at {sample_rate} Hz"
        );
    }

    #[test]
    #[ignore = "requires YODEL_REF_GEN / YODEL_REF_DECODE"]
    fn oracle_full_stack_to_us_44100() {
        oracle_full_stack_to_us(44100);
    }

    #[test]
    #[ignore = "requires YODEL_REF_GEN / YODEL_REF_DECODE"]
    fn oracle_full_stack_to_us_48000() {
        oracle_full_stack_to_us(48000);
    }

    /// Direction B with a *real* APRS position: the reference generator accepts
    /// a monitoring-format frame file, so it can emit a valid uncompressed
    /// position report which our full stack must parse all the way up to a
    /// typed [`AprsPacket::Position`].
    fn oracle_aprs_position_to_us(sample_rate: u32) {
        if !ref_binaries_available() {
            return;
        }
        let frame_file = scratch_dir().join(format!("oracle_aprs_pos_{sample_rate}.txt"));
        std::fs::write(
            &frame_file,
            "N0CALL-7>APRS:!4237.14N/07120.83W#yodel oracle\n",
        )
        .unwrap();
        let samples = run_ref_generator(
            &[frame_file.to_str().unwrap()],
            &format!("oracle_aprs_pos_{sample_rate}.wav"),
            sample_rate,
        );
        let frames = receive_frames(sample_rate, &samples);
        assert!(
            !frames.is_empty(),
            "no FCS-valid frames recovered from reference audio at {sample_rate} Hz"
        );

        let ui = UiFrame::parse(&frames[0]).unwrap();
        assert_eq!(ui.dest.callsign.as_bytes(), b"APRS");
        assert_eq!(ui.src.callsign.as_bytes(), b"N0CALL");
        assert_eq!(ui.src.ssid.value(), 7);

        match packet_from_ui(&ui).unwrap() {
            AprsPacket::Position(pos) => {
                assert_eq!(hundredths(pos.latitude.units()), 42 * 6000 + 37 * 100 + 14);
                assert_eq!(
                    hundredths(pos.longitude.units()),
                    -(71 * 6000 + 20 * 100 + 83)
                );
                assert_eq!(pos.symbol.to_wire(), (b'/', b'#'));
                assert!(!pos.messaging);
                assert!(!pos.compressed);
                assert!(pos.comment.starts_with(b"yodel oracle"));
            }
            other => panic!("expected a position report, got {other:?}"),
        }
    }

    #[test]
    #[ignore = "requires YODEL_REF_GEN / YODEL_REF_DECODE"]
    fn oracle_aprs_position_to_us_44100() {
        oracle_aprs_position_to_us(44100);
    }

    #[test]
    #[ignore = "requires YODEL_REF_GEN / YODEL_REF_DECODE"]
    fn oracle_aprs_position_to_us_48000() {
        oracle_aprs_position_to_us(48000);
    }

    // -----------------------------------------------------------------
    // Session-3 payload types: weather, telemetry, object, item, Mic-E.
    // These skip (early return) when the env vars are unset, which is now
    // the convention everywhere in this file: the guard used to live only
    // here, and the older tests above panicked instead. It is
    // `super::ref_binaries_available` rather than a second copy so that
    // the two halves of the file cannot drift apart again.
    // -----------------------------------------------------------------

    /// Renders raw info bytes the way the reference decoder prints them
    /// in its monitor line: printable ASCII verbatim, control bytes as
    /// `<0xNN>` (lowercase hex).
    fn monitor_escape(bytes: &[u8]) -> String {
        let mut out = String::new();
        for &b in bytes {
            if b < 0x20 || b == 0x7f {
                out.push_str(&format!("<0x{b:02x}>"));
            } else {
                out.push(b as char);
            }
        }
        out
    }

    /// Direction A for a raw (dest text, info bytes) payload that the
    /// `AprsPacket` builder cannot produce (Mic-E splits its data across
    /// the destination address). Sends one UI frame through the full
    /// public TX pipeline and asserts the reference decoder's monitor
    /// line reproduces the destination text and raw info bytes exactly.
    fn raw_frame_to_oracle(sample_rate: u32, dest_text: &str, src: &str, info: &[u8], tag: &str) {
        let (src_call, src_ssid) = split_ssid(src);
        let dest_addr = Address::new(dest_text.as_bytes(), 0).unwrap();
        let src_addr = Address::new(src_call.as_bytes(), src_ssid).unwrap();
        let frame = yodel::ax25::UiFrame::new(dest_addr, src_addr, info);
        let mut frame_buf = [0u8; 256];
        let frame_len = frame.build(&mut frame_buf).unwrap();

        let modulator = Modulator::new(
            ModulatorConfig::bell_202(SampleRate::new(sample_rate).unwrap()).unwrap(),
        );
        let samples: Vec<i16> = tx_i16(&frame_buf[..frame_len], modulator).collect();
        let wav_path = write_wav(
            &format!("raw_to_oracle_{tag}_{sample_rate}.wav"),
            sample_rate,
            &samples,
        );
        let stdout = run_ref_decoder(&wav_path);
        let header = format!("{src}>{dest_text}:{}", monitor_escape(info));
        assert!(
            stdout.lines().any(|l| l.contains(&header)),
            "reference decoder did not report `{header}` ({tag}).\nstdout:\n{stdout}"
        );
        assert_eq!(
            decoded_packet_count(&stdout),
            1,
            "reference decoder packet count mismatch ({tag}).\nstdout:\n{stdout}"
        );
    }

    /// Direction B for a hand-written monitor line: the reference
    /// generator synthesizes the AFSK audio, our full public RX
    /// pipeline recovers the frame, and the caller gets the parsed
    /// UI frame handed to `check`.
    fn oracle_line_to_us(sample_rate: u32, line: &[u8], tag: &str, check: &dyn Fn(&UiFrame<'_>)) {
        let frame_file = scratch_dir().join(format!("oracle_{tag}_{sample_rate}.txt"));
        std::fs::write(&frame_file, line).unwrap();
        let samples = run_ref_generator(
            &[frame_file.to_str().unwrap()],
            &format!("oracle_{tag}_{sample_rate}.wav"),
            sample_rate,
        );
        let frames = receive_frames(sample_rate, &samples);
        assert!(
            !frames.is_empty(),
            "no FCS-valid frames recovered from reference audio ({tag})"
        );
        let ui = UiFrame::parse(&frames[0]).unwrap();
        check(&ui);
    }

    /// 49°03.50'N 72°01.75'W in the crate's 1/100 arc-minute fixed point.
    const ORACLE_LAT: i64 = 49 * 6000 + 3 * 100 + 50;
    const ORACLE_LON: i64 = -(72 * 6000 + 100 + 75);

    /// The shared weather body, with the wind speed left to the
    /// caller.
    ///
    /// The two Complete Weather Report layouts spell wind speed in
    /// different units — `sNNN` is miles per hour, the `DDD/SSS` data
    /// extension is knots — so one `Speed` cannot be written into both
    /// and read back equal. Naming the unit at each call site is the
    /// accurate way to say that.
    fn oracle_weather_report(wind_speed: Speed) -> WeatherReport {
        WeatherReport {
            wind_direction: Some(220),
            wind_speed: Some(wind_speed),
            gust: Some(Speed::from_mph(5)),
            temperature: Some(Temperature::from_fahrenheit(-7)),
            rain_1h: Some(Rainfall::from_hundredths_inch(0)),
            rain_24h: Some(Rainfall::from_hundredths_inch(10)),
            rain_midnight: None,
            humidity: Some(Humidity::new(50).expect("in range")),
            barometric_pressure: Some(Pressure::from_tenths_hpa(9900)),
            // Chapter 12's optional "other parameters" reach the wire
            // only when present, in both layouts; this body has neither,
            // so both layouts stop at `b`.
            luminosity: None,
            snowfall: None,
        }
    }

    fn oracle_positionless_weather() -> AprsPacket<'static> {
        AprsPacket::Weather(PositionlessWeather {
            month: 6,
            day: 1,
            hour: 12,
            minute: 34,
            weather: oracle_weather_report(Speed::from_mph(4)),
            rest: b"wx oracle",
        })
    }

    fn oracle_position_weather() -> AprsPacket<'static> {
        AprsPacket::PositionWeather(PositionWeather {
            ambiguity: Ambiguity::EXACT,
            timestamp: None,
            latitude: Latitude::new(ORACLE_LAT).unwrap(),
            longitude: Longitude::new(ORACLE_LON).unwrap(),
            symbol: Symbol::WEATHER_STATION,
            messaging: false,
            weather: oracle_weather_report(Speed::from_knots(4)),
            rest: b" wx trail",
        })
    }

    fn oracle_telemetry() -> AprsPacket<'static> {
        AprsPacket::Telemetry(Telemetry {
            seq: 5,
            analog: Telemetry::integer_channels([199, 0, 255, 73, 123]),
            digital: Some([false, true, true, false, true, false, false, true]),
            rest: b",oracle",
        })
    }

    fn oracle_object() -> AprsPacket<'static> {
        AprsPacket::Object(Object {
            ambiguity: Ambiguity::EXACT,
            name: b"MARKER1",
            live: true,
            timestamp: Timestamp::DhmZulu {
                day: 9,
                hour: 23,
                minute: 45,
            },
            latitude: Latitude::new(ORACLE_LAT).unwrap(),
            longitude: Longitude::new(ORACLE_LON).unwrap(),
            symbol: Symbol::CAR,
            compressed: false,
            comment: b"obj oracle",
        })
    }

    fn oracle_item() -> AprsPacket<'static> {
        AprsPacket::Item(Item {
            ambiguity: Ambiguity::EXACT,
            name: b"AIDPOST",
            live: true,
            latitude: Latitude::new(ORACLE_LAT).unwrap(),
            longitude: Longitude::new(ORACLE_LON).unwrap(),
            symbol: Symbol::DIGI,
            compressed: false,
            comment: b"item oracle",
        })
    }

    #[test]
    #[ignore = "requires YODEL_REF_GEN / YODEL_REF_DECODE"]
    fn full_stack_weather_to_oracle_44100() {
        if !ref_binaries_available() {
            return;
        }
        full_stack_to_oracle(
            44100,
            &oracle_positionless_weather(),
            "APRS",
            "N0CALL-2",
            "wx",
        );
    }

    #[test]
    #[ignore = "requires YODEL_REF_GEN / YODEL_REF_DECODE"]
    fn full_stack_position_weather_to_oracle_44100() {
        if !ref_binaries_available() {
            return;
        }
        full_stack_to_oracle(
            44100,
            &oracle_position_weather(),
            "APRS",
            "N0CALL-2",
            "poswx",
        );
    }

    #[test]
    #[ignore = "requires YODEL_REF_GEN / YODEL_REF_DECODE"]
    fn full_stack_telemetry_to_oracle_44100() {
        if !ref_binaries_available() {
            return;
        }
        full_stack_to_oracle(44100, &oracle_telemetry(), "APRS", "N0CALL-3", "telem");
    }

    #[test]
    #[ignore = "requires YODEL_REF_GEN / YODEL_REF_DECODE"]
    fn full_stack_object_to_oracle_44100() {
        if !ref_binaries_available() {
            return;
        }
        full_stack_to_oracle(44100, &oracle_object(), "APRS", "N0CALL-4", "obj");
    }

    #[test]
    #[ignore = "requires YODEL_REF_GEN / YODEL_REF_DECODE"]
    fn full_stack_item_to_oracle_44100() {
        if !ref_binaries_available() {
            return;
        }
        full_stack_to_oracle(44100, &oracle_item(), "APRS", "N0CALL-5", "item");
    }

    #[test]
    #[ignore = "requires YODEL_REF_GEN / YODEL_REF_DECODE"]
    fn oracle_weather_to_us_44100() {
        if !ref_binaries_available() {
            return;
        }
        // _MMDDHHMM then c/s/g/t/r/p/P/h/b tagged values; P is "not
        // available" (dots) and humidity 50%.
        oracle_line_to_us(
            44100,
            b"N1CALL-1>APRS:_06011234c220s004g005t-07r000p010P...h50b09900wx line\n",
            "wx",
            &|ui| {
                assert_eq!(ui.src.callsign.as_bytes(), b"N1CALL");
                match packet_from_ui(ui).unwrap() {
                    AprsPacket::Weather(wx) => {
                        assert_eq!(wx.month, 6);
                        assert_eq!(wx.day, 1);
                        assert_eq!(wx.hour, 12);
                        assert_eq!(wx.minute, 34);
                        assert_eq!(wx.weather, oracle_weather_report(Speed::from_mph(4)));
                        assert!(wx.rest.starts_with(b"wx line"));
                    }
                    other => panic!("expected positionless weather, got {other:?}"),
                }
            },
        );
    }

    #[test]
    #[ignore = "requires YODEL_REF_GEN / YODEL_REF_DECODE"]
    fn oracle_position_weather_to_us_44100() {
        if !ref_binaries_available() {
            return;
        }
        // Position report with symbol `_` and DDD/SSS + tagged weather.
        oracle_line_to_us(
            44100,
            b"N1CALL-1>APRS:!4903.50N/07201.75W_220/004g005t-07r000p010P...h50b09900 wx trail\n",
            "poswx",
            &|ui| match packet_from_ui(ui).unwrap() {
                AprsPacket::PositionWeather(wx) => {
                    assert_eq!(hundredths(wx.latitude.units()), ORACLE_LAT);
                    assert_eq!(hundredths(wx.longitude.units()), ORACLE_LON);
                    assert_eq!(wx.symbol.to_wire().0, b'/');
                    assert!(!wx.messaging);
                    assert_eq!(wx.weather, oracle_weather_report(Speed::from_knots(4)));
                    assert!(wx.rest.starts_with(b" wx trail"));
                }
                other => panic!("expected position weather, got {other:?}"),
            },
        );
    }

    #[test]
    #[ignore = "requires YODEL_REF_GEN / YODEL_REF_DECODE"]
    fn oracle_telemetry_to_us_44100() {
        if !ref_binaries_available() {
            return;
        }
        oracle_line_to_us(
            44100,
            b"N1CALL-2>APRS:T#005,199,000,255,073,123,01101001,oracle\n",
            "telem",
            &|ui| match packet_from_ui(ui).unwrap() {
                AprsPacket::Telemetry(t) => {
                    assert_eq!(t.seq, 5);
                    assert_eq!(
                        t.analog,
                        Telemetry::integer_channels([199, 0, 255, 73, 123])
                    );
                    assert_eq!(
                        t.digital,
                        Some([false, true, true, false, true, false, false, true])
                    );
                    assert!(t.rest.starts_with(b",oracle"));
                }
                other => panic!("expected telemetry, got {other:?}"),
            },
        );
    }

    #[test]
    #[ignore = "requires YODEL_REF_GEN / YODEL_REF_DECODE"]
    fn oracle_object_to_us_44100() {
        if !ref_binaries_available() {
            return;
        }
        // 9-char space-padded name, live '*', DHM zulu timestamp.
        oracle_line_to_us(
            44100,
            b"N1CALL-3>APRS:;MARKER1  *092345z4903.50N/07201.75W>obj line\n",
            "obj",
            &|ui| match packet_from_ui(ui).unwrap() {
                AprsPacket::Object(o) => {
                    assert_eq!(o.name, b"MARKER1");
                    assert!(o.live);
                    assert_eq!(
                        o.timestamp,
                        Timestamp::DhmZulu {
                            day: 9,
                            hour: 23,
                            minute: 45
                        }
                    );
                    assert_eq!(hundredths(o.latitude.units()), ORACLE_LAT);
                    assert_eq!(hundredths(o.longitude.units()), ORACLE_LON);
                    assert_eq!(o.symbol.to_wire(), (b'/', b'>'));
                    assert!(o.comment.starts_with(b"obj line"));
                }
                other => panic!("expected object, got {other:?}"),
            },
        );
    }

    #[test]
    #[ignore = "requires YODEL_REF_GEN / YODEL_REF_DECODE"]
    fn oracle_item_to_us_44100() {
        if !ref_binaries_available() {
            return;
        }
        oracle_line_to_us(
            44100,
            b"N1CALL-4>APRS:)AIDPOST!4903.50N/07201.75W#item line\n",
            "item",
            &|ui| match packet_from_ui(ui).unwrap() {
                AprsPacket::Item(i) => {
                    assert_eq!(i.name, b"AIDPOST");
                    assert!(i.live);
                    assert_eq!(hundredths(i.latitude.units()), ORACLE_LAT);
                    assert_eq!(hundredths(i.longitude.units()), ORACLE_LON);
                    assert_eq!(i.symbol.to_wire(), (b'/', b'#'));
                    assert!(i.comment.starts_with(b"item line"));
                }
                other => panic!("expected item, got {other:?}"),
            },
        );
    }

    /// Mic-E oracle tests: Mic-E cannot be built via [`AprsPacket`]
    /// (half the data lives in the destination address), so direction A
    /// uses [`raw_frame_to_oracle`] with our `mic_e` encoder's output
    /// and direction B hand-derives destination/info bytes from the
    /// chapter 10 rules and checks our `mic_e::decode` field by field.
    #[cfg(feature = "micE")]
    mod mic_e_oracle {
        use super::*;
        use yodel::aprs::mic_e::{self, MicE, MicEFix, MicEMessage};

        /// Encodes `report` with our encoder and round-trips it through
        /// the reference decoder, asserting the monitor line reproduces
        /// the destination text and info bytes exactly.
        fn mic_e_to_oracle(report: &MicE<'_>, tag: &str) {
            let mut dest = [0u8; 6];
            let mut info = [0u8; 64];
            let info_len = report.encode(&mut dest, &mut info).unwrap();
            let dest_text = std::str::from_utf8(&dest).unwrap().to_owned();
            raw_frame_to_oracle(44100, &dest_text, "N0CALL-6", &info[..info_len], tag);
        }

        #[test]
        #[ignore = "requires YODEL_REF_GEN / YODEL_REF_DECODE"]
        fn mic_e_north_west_to_oracle() {
            if !ref_binaries_available() {
                return;
            }
            // North + west, no longitude offset, standard message set,
            // moving, no altitude, no ambiguity.
            mic_e_to_oracle(
                &MicE {
                    latitude: Latitude::new(ORACLE_LAT).unwrap(),
                    longitude: Longitude::new(ORACLE_LON).unwrap(),
                    speed: 20,
                    course: 251,
                    symbol: Symbol::from_wire(b'/', b'j'),
                    message: MicEMessage::EnRoute,
                    fix: MicEFix::Current,
                    altitude: None,
                    device_prefix: None,
                    ambiguity: 0,
                    status: b" mic-e a1",
                },
                "mice_a1",
            );
        }

        #[test]
        #[ignore = "requires YODEL_REF_GEN / YODEL_REF_DECODE"]
        fn mic_e_south_east_to_oracle() {
            if !ref_binaries_available() {
                return;
            }
            // South + east, longitude over 100 degrees (offset flag),
            // custom message set, old fix, altitude and ambiguity 2.
            mic_e_to_oracle(
                &MicE {
                    latitude: Latitude::new(
                        -(33 * 6000 + 25 * 100) * yodel::geo::UNITS_PER_HUNDREDTH_MINUTE,
                    )
                    .unwrap(),
                    longitude: Longitude::new(
                        (112 * 6000 + 7 * 100 + 74) * yodel::geo::UNITS_PER_HUNDREDTH_MINUTE,
                    )
                    .unwrap(),
                    speed: 105,
                    course: 60,
                    symbol: Symbol::CAR,
                    message: MicEMessage::Custom5,
                    fix: MicEFix::Old,
                    altitude: Some(61),
                    device_prefix: Some(b']'),
                    ambiguity: 2,
                    status: b"mic-e a2",
                },
                "mice_a2",
            );
        }
        /// Reference-encode -> our decode, vector 1: 49°03.50'N
        /// 72°01.75'W (no longitude offset), speed 20 kn, course 251°,
        /// standard message set (M1 en route), current fix, no altitude.
        ///
        /// Destination derivation (chapter 10): latitude digits are
        /// 4 9 0 3 5 0; M1 bits are A=1 B=1 C=0, standard set, so
        /// columns 1-2 use 'P'+digit and column 3 stays a digit:
        /// 'T' 'Y' '0'. Column 4 north => 'P'+3 = 'S'; column 5 has no
        /// +100 offset (72 in 10..=99) => digit '5'; column 6 west =>
        /// 'P'+0 = 'P'. Destination: TY0S5P.
        ///
        /// Info derivation: DTI '`'; lon deg 72 => 72+28 = 100 'd';
        /// lon min 1 (0..=9 shifts +60) => 1+60+28 = 89 'Y'; lon
        /// hundredths 75 => 75+28 = 103 'g'. Speed+800 = 820, course
        /// +400 = 651: SP = 82+28 = 110 'n', DC = 0*10+6+28 = 34 '"',
        /// SE = 51+28 = 79 'O'. Symbol code 'j', table '/'.
        #[test]
        #[ignore = "requires YODEL_REF_GEN / YODEL_REF_DECODE"]
        fn oracle_mic_e_north_west_to_us() {
            if !ref_binaries_available() {
                return;
            }
            oracle_line_to_us(
                44100,
                b"N1CALL-5>TY0S5P:`dYgn\"Oj/ test/a\n",
                "mice_b1",
                &|ui| {
                    let mut dest = [0u8; 6];
                    dest.copy_from_slice(ui.dest.callsign.as_bytes());
                    let report = mic_e::decode(&dest, ui.info).unwrap();
                    assert_eq!(hundredths(report.latitude.units()), ORACLE_LAT);
                    assert_eq!(hundredths(report.longitude.units()), ORACLE_LON);
                    assert_eq!(report.speed, 20);
                    assert_eq!(report.course, 251);
                    assert_eq!(report.symbol.to_wire(), (b'/', b'j'));
                    assert_eq!(report.message, MicEMessage::EnRoute);
                    assert_eq!(report.fix, MicEFix::Current);
                    assert_eq!(report.altitude, None);
                    assert_eq!(report.ambiguity, 0);
                    // The generator carries the file's newline into the
                    // info field, so it ends up in the status text.
                    assert_eq!(report.status, b" test/a\n");
                },
            );
        }

        /// Reference-encode -> our decode, vector 2: 52°09.  'S (two
        /// digits of ambiguity) 0°30.10'E (+100-offset flag set because
        /// 0 < 10), speed 7 kn, course 105°, custom message set (C2),
        /// old fix, altitude 200 m.
        ///
        /// Destination derivation: latitude digits 5 2 0 9 with the
        /// last two blanked. C2 bits are A=1 B=0 C=1 in the custom set
        /// ('A'+digit): 'F' '2' 'A'. Column 4 south => digit '9';
        /// column 5 offset set + blank (standard set) => 'Z'; column 6
        /// east + blank => 'L'. Destination: F2A9ZL.
        ///
        /// Info derivation: DTI '\''; lon deg 0 => 0+118 = 118 'v';
        /// lon min 30 => 30+28 = 58 ':'; hundredths 10 => 10+28 = 38
        /// '&'. Speed+800 = 807, course+400 = 505: SP = 80+28 = 108
        /// 'l', DC = 7*10+5+28 = 103 'g', SE = 5+28 = 33 '!'. Symbol
        /// code '-', table '/'. Altitude 200+10000 = 10200 base-91 =
        /// 1*8281 + 21*91 + 8 => '"' '6' ')' then '}'.
        #[test]
        #[ignore = "requires YODEL_REF_GEN / YODEL_REF_DECODE"]
        fn oracle_mic_e_south_east_to_us() {
            if !ref_binaries_available() {
                return;
            }
            oracle_line_to_us(
                44100,
                b"N1CALL-6>F2A9ZL:'v:&lg!-/\"6)}hello\n",
                "mice_b2",
                &|ui| {
                    let mut dest = [0u8; 6];
                    dest.copy_from_slice(ui.dest.callsign.as_bytes());
                    let report = mic_e::decode(&dest, ui.info).unwrap();
                    // Blanked digits decode as zero: 52 deg 09.00 min S.
                    assert_eq!(hundredths(report.latitude.units()), -(52 * 6000 + 9 * 100));
                    assert_eq!(hundredths(report.longitude.units()), 30 * 100 + 10);
                    assert_eq!(report.speed, 7);
                    assert_eq!(report.course, 105);
                    assert_eq!(report.symbol.to_wire(), (b'/', b'-'));
                    assert_eq!(report.message, MicEMessage::Custom2);
                    assert_eq!(report.fix, MicEFix::Old);
                    assert_eq!(report.altitude, Some(200));
                    assert_eq!(report.ambiguity, 2);
                    assert_eq!(report.status, b"hello\n");
                },
            );
        }
    } // mod mic_e_oracle
} // mod full_stack
