//! Host-side proof for the application-story examples.
//!
//! `examples/decode_to_log.rs` and `examples/trigger_reply.rs` keep their
//! core logic in pure functions (log-line formatting, the
//! receive→respond decision). To prove that logic — not just its
//! compilability — this test includes the SAME source files via
//! `#[path]` (the technique of `tests/esp32_examples.rs`) and runs them
//! against the main crate's transmit/receive chains:
//!
//! * `format_frame_line` must produce EXACT log lines for synthesized
//!   frames: sample-clock timestamp, `SRC>DEST`, per-hop `*` markers on
//!   used (H-bit set) hops, payload kind, and position/message
//!   summaries;
//! * `decide` must trigger only on text messages addressed to MYCALL,
//!   ack only when a `{id}` is present, and never answer an ack;
//! * the full loop: audio of "message to MYCALL with id" synthesized by
//!   the transmitter, run through the example's receive+decide+build
//!   path, must yield ack and reply frames that decode back correctly.
#![cfg(feature = "tnc")]

#[path = "../examples/decode_many_threads.rs"]
#[allow(dead_code)]
mod decode_many_threads;
#[path = "../examples/decode_to_log.rs"]
#[allow(dead_code)]
mod decode_to_log;
#[cfg(feature = "digipeat")]
#[path = "../examples/digipeater_station.rs"]
#[allow(dead_code)]
mod digipeater_station;
#[path = "../examples/trigger_reply.rs"]
#[allow(dead_code)]
mod trigger_reply;

use warble::SampleRate;
use warble::aprs::{AprsPacket, Latitude, Longitude, Message, MessageContent, Position, Symbol};
use warble::ax25::{Address, PathHop, UiFrame};
use warble::tnc::{DefaultTncReceiver, TncConfig, TncReceiver, TncTransmitter};

fn addr(call: &[u8], ssid: u8) -> Address {
    match Address::new(call, ssid) {
        Ok(a) => a,
        Err(e) => panic!("{e}"),
    }
}

// ---------------------------------------------------------------------
// (a) decode_to_log: exact log lines for synthesized frames.
// ---------------------------------------------------------------------

/// A position frame with a mixed used/unused digipeater path renders
/// the sample-clock timestamp, `SRC>DEST`, a `*` on the used hop only,
/// and the lat/lon summary.
#[test]
fn log_line_position_with_mixed_path() {
    let pos = Position::new(
        Latitude::from_degrees(49.0583).unwrap(),
        Longitude::from_degrees(-72.0292).unwrap(),
        Symbol::CAR,
    );
    let mut info = [0u8; 64];
    let len = AprsPacket::Position(pos).build(&mut info).unwrap();
    let hops = [
        PathHop {
            address: addr(b"N1CALL", 1),
            repeated: true, // this digipeater already relayed the frame
        },
        PathHop::unused(addr(b"WIDE2", 1)),
    ];
    let frame =
        UiFrame::with_hops(addr(b"APRS", 0), addr(b"N0CALL", 7), &hops, &info[..len]).unwrap();
    // 592_800 samples at 48 kHz = 12.35 s of sample clock.
    let line = decode_to_log::format_frame_line(592_800, 48_000, &frame);
    assert_eq!(
        line,
        "[   12.350s] N0CALL-7>APRS,N1CALL-1*,WIDE2-1: position lat 49.0583 lon -72.0292"
    );
}

/// A message with an id renders addressee, text, and the `{id}`.
#[test]
fn log_line_message_with_id() {
    let frame = UiFrame::new(
        addr(b"APRS", 0),
        addr(b"N1CALL", 0),
        b":N0CALL   :Testing{003",
    );
    let line = decode_to_log::format_frame_line(48_000, 48_000, &frame);
    assert_eq!(
        line,
        "[    1.000s] N1CALL>APRS: message N0CALL \"Testing\" {003}"
    );
}

/// A message without an id renders without a `{}` suffix; an ack
/// renders as its own kind.
#[test]
fn log_line_message_without_id_and_ack() {
    let no_id = UiFrame::new(addr(b"APRS", 0), addr(b"N1CALL", 0), b":N0CALL   :hi there");
    assert_eq!(
        decode_to_log::format_frame_line(24_000, 48_000, &no_id),
        "[    0.500s] N1CALL>APRS: message N0CALL \"hi there\""
    );
    let ack = UiFrame::new(addr(b"APRS", 0), addr(b"N0CALL", 0), b":N1CALL   :ack003");
    assert_eq!(
        decode_to_log::format_frame_line(24_000, 48_000, &ack),
        "[    0.500s] N0CALL>APRS: ack N1CALL 003"
    );
}

/// Statuses and unparseable payloads still log (a monitor logs
/// everything it hears), and a Mic-E data-type byte is classified.
#[test]
fn log_line_status_other_and_mic_e() {
    let status = UiFrame::new(addr(b"APRS", 0), addr(b"N0CALL", 7), b">QRV 144.390");
    assert_eq!(
        decode_to_log::format_frame_line(48_000, 48_000, &status),
        "[    1.000s] N0CALL-7>APRS: status \"QRV 144.390\""
    );
    let raw = UiFrame::new(addr(b"APRS", 0), addr(b"N0CALL", 7), b"not aprs at all");
    assert_eq!(
        decode_to_log::format_frame_line(48_000, 48_000, &raw),
        "[    1.000s] N0CALL-7>APRS: other \"not aprs at all\""
    );
    let mic_e = UiFrame::new(addr(b"T7SYWP", 0), addr(b"N0CALL", 7), b"`(_fn\"Oj/");
    assert_eq!(
        decode_to_log::format_frame_line(48_000, 48_000, &mic_e),
        "[    1.000s] N0CALL-7>T7SYWP: mic-e"
    );
}

// ---------------------------------------------------------------------
// (a, continued) trigger_reply: decide() outcomes case by case.
// ---------------------------------------------------------------------

/// A text message to MYCALL carrying `{003}` owes an ack003 + reply.
#[test]
fn decide_acks_message_with_id() {
    let plan = trigger_reply::decide(b"N0CALL", &addr(b"N1CALL", 5), b":N0CALL   :Testing{003")
        .expect("must trigger");
    assert_eq!(plan.to, b"N1CALL-5");
    assert_eq!(plan.ack_id.as_deref(), Some(&b"003"[..]));
    assert_eq!(plan.reply_text, trigger_reply::REPLY_TEXT);
}

/// A message without an id gets the canned reply but NO ack.
#[test]
fn decide_no_ack_without_id() {
    let plan = trigger_reply::decide(b"N0CALL", &addr(b"N1CALL", 0), b":N0CALL   :hi there")
        .expect("must trigger");
    assert_eq!(plan.ack_id, None);
    assert_eq!(plan.to, b"N1CALL");
}

/// Messages for OTHER stations are ignored, whatever they carry.
#[test]
fn decide_ignores_messages_for_others() {
    assert_eq!(
        trigger_reply::decide(b"N0CALL", &addr(b"N1CALL", 0), b":N2CALL   :Testing{003"),
        None
    );
}

/// An ack addressed to us is never answered (no ack-of-ack loops);
/// same for a rej.
#[test]
fn decide_never_acks_an_ack() {
    assert_eq!(
        trigger_reply::decide(b"N0CALL", &addr(b"N1CALL", 0), b":N0CALL   :ack003"),
        None
    );
    assert_eq!(
        trigger_reply::decide(b"N0CALL", &addr(b"N1CALL", 0), b":N0CALL   :rej003"),
        None
    );
}

/// Non-message payloads (position, status, garbage) never trigger.
#[test]
fn decide_ignores_non_messages() {
    let src = addr(b"N1CALL", 0);
    assert_eq!(
        trigger_reply::decide(b"N0CALL", &src, b"!4903.50N/07201.75W>"),
        None
    );
    assert_eq!(
        trigger_reply::decide(b"N0CALL", &src, b">just a status"),
        None
    );
    assert_eq!(trigger_reply::decide(b"N0CALL", &src, b"garbage"), None);
}

/// The built responses follow src/aprs/message.rs byte-for-byte:
/// `ack003` to the sender first, then the unnumbered canned reply.
#[test]
fn build_responses_matches_message_semantics() {
    let plan = trigger_reply::decide(b"N0CALL", &addr(b"N1CALL", 5), b":N0CALL   :Testing{003")
        .expect("must trigger");
    let frames = trigger_reply::build_responses(&plan).expect("must build");
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0], b":N1CALL-5 :ack003");
    let mut expected_reply = b":N1CALL-5 :".to_vec();
    expected_reply.extend_from_slice(trigger_reply::REPLY_TEXT);
    assert_eq!(frames[1], expected_reply);
}

// ---------------------------------------------------------------------
// (b) Full-loop proof: audio in, correct ack + reply audio out.
// ---------------------------------------------------------------------

/// Renders one UI frame (raw info payload) to Bell 202 samples.
fn synthesize(dest: Address, src: Address, info: &[u8]) -> Vec<i16> {
    let cfg = TncConfig::bell_202(SampleRate::new(48_000).unwrap()).unwrap();
    let tx = TncTransmitter::new(cfg);
    let mut frame_buf = [0u8; 330];
    let len = tx
        .build_frame_raw(dest, src, &[], info, &mut frame_buf)
        .expect("frame must build");
    tx.frame_samples_i16(&frame_buf[..len]).collect()
}

/// Decodes all frames from a sample stream into owned (src, info) pairs.
fn decode_all(samples: &[i16]) -> Vec<(Address, Vec<u8>)> {
    let cfg = TncConfig::bell_202(SampleRate::new(48_000).unwrap()).unwrap();
    let mut rx: DefaultTncReceiver = TncReceiver::new(cfg).unwrap();
    let mut out = Vec::new();
    for &s in samples {
        if let Some(frame) = rx.push_i16(s) {
            out.push((frame.src(), frame.info().to_vec()));
        }
    }
    out
}

/// The whole trigger_reply story end to end: synthesize "message to
/// MYCALL with id" AUDIO, run the example's receive+decide+build+
/// modulate path, and decode the produced audio back — the ack and the
/// canned reply must both arrive intact and correctly addressed.
#[test]
fn trigger_full_loop_round_trips_through_audio() {
    // 1. The triggering transmission, as audio.
    let stimulus = synthesize(
        addr(b"APRS", 0),
        addr(b"N1CALL", 5),
        b":N0CALL   :Testing{003",
    );

    // 2. The example's receive loop: decode frames, collect plans.
    let mut plans = Vec::new();
    for (src, info) in decode_all(&stimulus) {
        if let Some(plan) = trigger_reply::decide(trigger_reply::MYCALL, &src, &info) {
            plans.push(plan);
        }
    }
    assert_eq!(plans.len(), 1, "exactly one frame must trigger");

    // 3. The example's transmit side: response payloads → audio.
    let src = addr(trigger_reply::MYCALL, trigger_reply::MYCALL_SSID);
    let dest = addr(trigger_reply::TOCALL, 0);
    let mut reply_audio = Vec::new();
    for info in trigger_reply::build_responses(&plans[0]).expect("must build") {
        reply_audio.extend(synthesize(dest, src, &info));
    }

    // 4. Decode the response audio back: ack003 first, then the reply,
    //    both from MYCALL and addressed to the original sender.
    let heard = decode_all(&reply_audio);
    assert_eq!(heard.len(), 2, "ack + reply must both decode");
    for (src, _) in &heard {
        assert_eq!(src.callsign.as_bytes(), trigger_reply::MYCALL);
    }
    let ack = Message::parse(&heard[0].1).expect("ack must parse");
    assert_eq!(ack.addressee.as_bytes(), b"N1CALL-5");
    assert_eq!(ack.content, MessageContent::Ack { id: b"003" });
    let reply = Message::parse(&heard[1].1).expect("reply must parse");
    assert_eq!(reply.addressee.as_bytes(), b"N1CALL-5");
    assert_eq!(
        reply.content,
        MessageContent::Text {
            text: trigger_reply::REPLY_TEXT,
            id: None,
        }
    );
}

// ---------------------------------------------------------------------
// (c) digipeater_station: pure decision/formatting/stats logic plus a
//     corpus-style WAV round trip through the station itself.
// ---------------------------------------------------------------------

#[cfg(feature = "digipeat")]
mod digi {
    use super::addr;
    use crate::digipeater_station::{
        FrameReport, Policy, Station, Verdict, json_line, parse_args, parse_callsign, stats_report,
    };
    use warble::SampleRate;
    use warble::ax25::PathHop;
    use warble::digipeat::WideLimit;
    use warble::tnc::{DefaultTncReceiver, TncConfig, TncReceiver};

    fn policy(transmit: bool) -> Policy {
        Policy {
            my_call: addr(b"N0CALL", 1),
            wide_limit: Some(WideLimit::TWO),
            transmit,
        }
    }

    /// Runs samples through a station, collecting every frame report.
    fn run(station: &mut Station, samples: &[i16]) -> Vec<FrameReport> {
        samples.iter().filter_map(|&s| station.push(s)).collect()
    }

    /// The corpus-style round trip: a heard `WIDE2-1` frame is relayed
    /// (live mode), the produced AUDIO decodes back, and the relayed
    /// path shows the exact mutation — the WIDE hop consumed in place
    /// with its H bit set. Semantics come solely from the library's
    /// `relay_decision`; this test proves the station glue around it.
    #[test]
    fn wav_round_trip_relays_with_correct_mutation() {
        let mut station = Station::new(48_000, policy(true)).unwrap();
        // Stimulus: N1CALL-5 > APRS via WIDE2-1 (last hop: consume).
        let stimulus = synthesize_with_path(b"N1CALL", 5, &[(b"WIDE2", 1, false)], b">hello digi");
        let reports = run(&mut station, &stimulus);
        assert_eq!(reports.len(), 1, "exactly one frame must be heard");
        let report = &reports[0];
        assert!(!report.tx_audio.is_empty(), "live relay must produce audio");

        // Decode the relayed audio back and check the exact mutation.
        let cfg = TncConfig::bell_202(SampleRate::new(48_000).unwrap()).unwrap();
        let mut rx: DefaultTncReceiver = TncReceiver::new(cfg).unwrap();
        let mut heard = 0;
        for &s in &report.tx_audio {
            if let Some(frame) = rx.push_i16(s) {
                heard += 1;
                let ui = frame.ui_frame();
                assert_eq!(ui.src, addr(b"N1CALL", 5));
                assert_eq!(ui.dest, addr(b"APRS", 0));
                let hops: Vec<PathHop> = ui.hops().collect();
                assert_eq!(
                    hops,
                    vec![PathHop {
                        address: addr(b"WIDE2", 1),
                        repeated: true, // consumed in place, H bit set
                    }]
                );
                assert_eq!(ui.info, b">hello digi");
            }
        }
        assert_eq!(heard, 1, "the relayed audio must decode");

        // Stats: one heard, one relayed, nothing else.
        assert_eq!(station.stats.heard, 1);
        assert_eq!(station.stats.relayed, 1);
        assert_eq!(station.stats.duplicates, 0);
        assert_eq!(station.stats.ignored_total(), 0);
    }

    /// The same frame heard twice within the dupe window: the second
    /// hearing is suppressed and counted as a duplicate.
    #[test]
    fn duplicate_suppressed_and_counted() {
        let mut station = Station::new(48_000, policy(true)).unwrap();
        let stimulus = synthesize_with_path(b"N1CALL", 5, &[(b"WIDE2", 1, false)], b">dupe me");
        let first = run(&mut station, &stimulus);
        let second = run(&mut station, &stimulus);
        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        assert!(!first[0].tx_audio.is_empty());
        assert!(second[0].tx_audio.is_empty(), "duplicate must not relay");
        assert!(second[0].json.contains("\"decision\":\"duplicate\""));
        assert_eq!(station.stats.heard, 2);
        assert_eq!(station.stats.relayed, 1);
        assert_eq!(station.stats.duplicates, 1);
    }

    /// Dry-run (the default) makes the SAME decision and produces the
    /// SAME JSON log line as live mode — but no output audio.
    #[test]
    fn dry_run_logs_but_produces_no_audio() {
        let stimulus = synthesize_with_path(b"N1CALL", 5, &[(b"WIDE2", 1, false)], b">dry run");
        let mut dry = Station::new(48_000, policy(false)).unwrap();
        let mut live = Station::new(48_000, policy(true)).unwrap();
        let dry_reports = run(&mut dry, &stimulus);
        let live_reports = run(&mut live, &stimulus);
        assert_eq!(dry_reports.len(), 1);
        assert_eq!(live_reports.len(), 1);
        // Decision equivalence: identical JSON record either way.
        assert_eq!(dry_reports[0].json, live_reports[0].json);
        assert!(dry_reports[0].tx_audio.is_empty(), "dry-run: no audio");
        assert!(!live_reports[0].tx_audio.is_empty(), "live: audio");
        // Both count the frame as relayed (dry-run means WOULD relay).
        assert_eq!(dry.stats.relayed, 1);
        assert_eq!(live.stats.relayed, 1);
    }

    /// The JSON-lines record has EXACT fields: sample-clock time,
    /// src, dst, path before/after, decision, reason.
    #[test]
    fn json_line_fields_exact() {
        let relayed = Verdict::Relay(vec![
            PathHop {
                address: addr(b"N0CALL", 1),
                repeated: true,
            },
            PathHop::unused(addr(b"WIDE2", 1)),
        ]);
        assert_eq!(
            json_line(12_350, "N1CALL-5", "APRS", "WIDE2-2", &relayed),
            "{\"t_s\":12.350,\"src\":\"N1CALL-5\",\"dst\":\"APRS\",\"path_before\":\"WIDE2-2\",\"path_after\":\"N0CALL-1*,WIDE2-1\",\"decision\":\"relay\",\"reason\":\"\"}"
        );
        assert_eq!(
            json_line(500, "N1CALL", "APRS", "WIDE2-1*", &Verdict::Duplicate),
            "{\"t_s\":0.500,\"src\":\"N1CALL\",\"dst\":\"APRS\",\"path_before\":\"WIDE2-1*\",\"path_after\":\"\",\"decision\":\"duplicate\",\"reason\":\"heard within dupe window\"}"
        );
    }

    /// A frame not addressed to any served alias is ignored, counted
    /// per reason, and its JSON carries the typed reason label.
    #[test]
    fn not_for_us_ignored_with_reason() {
        let mut station = Station::new(48_000, policy(true)).unwrap();
        let stimulus = synthesize_with_path(b"N1CALL", 5, &[(b"K1ABC", 0, false)], b">not ours");
        let reports = run(&mut station, &stimulus);
        assert_eq!(reports.len(), 1);
        assert!(reports[0].tx_audio.is_empty());
        assert!(reports[0].json.contains("\"decision\":\"ignore\""));
        assert!(reports[0].json.contains("\"reason\":\"not-for-us\""));
        assert_eq!(station.stats.ignored, vec![("not-for-us".to_string(), 1)]);
    }

    /// The exit self-report renders counters and top talkers exactly.
    #[test]
    fn stats_report_exact() {
        let mut station = Station::new(48_000, policy(false)).unwrap();
        let stimulus = synthesize_with_path(b"N1CALL", 5, &[(b"WIDE2", 1, false)], b">r1");
        run(&mut station, &stimulus);
        let report = stats_report(&station.stats, 12_350, Some(3));
        assert_eq!(
            report,
            "digipeater session report\n  uptime: 12.350s sample clock, 3s wall clock\n  heard: 1  relayed: 1  duplicate: 0  ignored: 0\n  top talkers:\n    N1CALL-5: 1 frame(s)"
        );
    }

    /// The per-alias policy flags parse into the expected table.
    #[test]
    fn cli_policy_flags_parse() {
        let args: Vec<String> = ["in.wav", "--mycall", "N2CALL-7", "--wide-max", "1"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let cli = parse_args(&args).unwrap();
        assert_eq!(cli.policy.my_call, addr(b"N2CALL", 7));
        assert_eq!(cli.policy.wide_limit.unwrap().value(), 1);
        assert!(!cli.policy.transmit, "dry-run must be the default");

        let args: Vec<String> = ["-", "--no-wide", "--transmit", "--log", "x.jsonl"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let cli = parse_args(&args).unwrap();
        assert_eq!(cli.policy.wide_limit, None);
        assert!(cli.policy.transmit);
        assert_eq!(cli.log.as_deref(), Some("x.jsonl"));

        assert!(parse_callsign("N0CALL-16").is_err());
        assert!(parse_args(&["--bogus".to_string()]).is_err());
    }

    /// Synthesizes a frame with an explicit hop list as Bell 202 audio
    /// (the plain `synthesize` helper has no digipeater path).
    fn synthesize_with_path(
        src_call: &[u8],
        src_ssid: u8,
        hops: &[(&[u8], u8, bool)],
        info: &[u8],
    ) -> Vec<i16> {
        use warble::ax25::UiFrame;
        use warble::tnc::TncTransmitter;
        let hop_list: Vec<PathHop> = hops
            .iter()
            .map(|&(call, ssid, repeated)| PathHop {
                address: addr(call, ssid),
                repeated,
            })
            .collect();
        let frame = UiFrame::with_hops(addr(b"APRS", 0), addr(src_call, src_ssid), &hop_list, info)
            .unwrap();
        let mut buf = [0u8; 330];
        let len = frame.build(&mut buf).unwrap();
        let cfg = TncConfig::bell_202(SampleRate::new(48_000).unwrap()).unwrap();
        TncTransmitter::new(cfg)
            .frame_samples_i16(&buf[..len])
            .collect()
    }
}

// ---------------------------------------------------------------------
// (d) decode_many_threads: bounded worker pool + bounded channel + sinks.
// ---------------------------------------------------------------------

mod concurrent {
    use super::{addr, synthesize};
    use crate::decode_many_threads::{
        CHANNEL_DEPTH, DecodedFrame, JsonlSink, MemorySink, Sink, WORKERS, decode_pool,
    };
    use std::sync::atomic::{AtomicU32, Ordering};
    use warble::SampleRate;
    use warble::aprs::{AprsPacket, Status};
    use warble::ax25::Address;
    use warble::tnc::{DefaultTncReceiver, TncConfig, TncTransmitter};

    /// Writes `count` status frames (each naming `tag` and its index)
    /// as a 48 kHz WAV in the temp directory, returning the path.
    fn wav_fixture(tag: &str, count: u32) -> String {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "warble-concurrent-{}-{n}-{tag}.wav",
            std::process::id()
        ));
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        for i in 0..count {
            let info = format!(">{tag} frame {i}");
            let audio = synthesize(addr(b"APRS", 0), addr(b"N0CALL", 1), info.as_bytes());
            for s in audio {
                writer.write_sample(s).unwrap();
            }
            for _ in 0..4800 {
                writer.write_sample(0i16).unwrap(); // 100 ms gap
            }
        }
        writer.finalize().unwrap();
        path.to_string_lossy().into_owned()
    }

    /// N files decoded in parallel: every frame of every file reaches
    /// the sink, tagged with its source file.
    #[test]
    fn every_frame_of_every_file_reaches_the_sink() {
        let files: Vec<String> = (0..6)
            .map(|i| wav_fixture(&format!("feed{i}"), 2))
            .collect();
        let mut sink = MemorySink::default();
        let total = decode_pool(&files, &mut sink).expect("pool must run");
        assert_eq!(total, 12, "6 files x 2 frames each");
        assert_eq!(sink.frames.len(), 12);
        for (i, path) in files.iter().enumerate() {
            for frame_no in 0..2 {
                let info = format!(">feed{i} frame {frame_no}").into_bytes();
                assert!(
                    sink.frames.iter().any(|f| f.source == *path
                        && f.frame.info() == info
                        && f.sender() == "N0CALL-1"),
                    "frame {frame_no} of {path} must reach the sink"
                );
            }
        }
        for path in files {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Backpressure: an artificially slow sink still receives EVERY
    /// frame, and in-flight memory is bounded by construction — the
    /// channel is a `sync_channel(CHANNEL_DEPTH)`, so it can never hold
    /// more than CHANNEL_DEPTH frames; a full channel BLOCKS the decode
    /// workers (at most one un-sent frame each) instead of queueing.
    /// The run completing with all frames present is the observable
    /// proof that blocked senders resume when the slow sink drains.
    #[test]
    fn slow_sink_backpressure_loses_nothing_and_completes() {
        // More frames than the channel holds, so the workers MUST
        // block on the full channel at least once.
        let frames_per_file = 4u32;
        let files: Vec<String> = (0..4)
            .map(|i| wav_fixture(&format!("slow{i}"), frames_per_file))
            .collect();
        let total_expected = 4 * frames_per_file;
        assert!(
            total_expected as usize > CHANNEL_DEPTH + WORKERS,
            "the fixture must overfill the channel to exercise blocking"
        );
        let mut sink = MemorySink {
            delay: Some(std::time::Duration::from_millis(30)),
            ..MemorySink::default()
        };
        let total =
            decode_pool(&files, &mut sink).expect("pool must complete despite the slow sink");
        assert_eq!(total, total_expected);
        assert_eq!(sink.frames.len(), total_expected as usize);
        for path in files {
            let _ = std::fs::remove_file(path);
        }
    }

    /// The JSON-lines sink writes one exact object per frame.
    #[test]
    fn jsonl_sink_writes_one_exact_line_per_frame() {
        // Build a real OwnedFrame by decoding one transmission (the
        // type is a faithful copy of a received frame, so it has no
        // free-form constructor).
        let cfg = TncConfig::bell_202(SampleRate::new(44_100).unwrap()).unwrap();
        let tx = TncTransmitter::new(cfg);
        let mut rx = DefaultTncReceiver::new(cfg).unwrap();
        let samples = tx
            .transmit_to_vec_i16(
                &AprsPacket::Status(Status {
                    text: b"hello \x01world",
                }),
                Address::new(b"APRS", 0).unwrap(),
                Address::new(b"N0CALL", 1).unwrap(),
                &[],
            )
            .unwrap();
        let mut owned = None;
        for s in samples {
            if let Some(frame) = rx.push_i16(s) {
                owned = Some(warble::tnc::OwnedFrame::new(&frame).unwrap());
            }
        }
        let mut out = Vec::new();
        {
            let mut sink = JsonlSink { out: &mut out };
            sink.accept(DecodedFrame {
                source: "a.wav".to_owned(),
                frame: owned.expect("one frame decodes"),
            })
            .unwrap();
        }
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "{\"source\":\"a.wav\",\"from\":\"N0CALL-1\",\"info\":\">hello .world\"}\n"
        );
    }
}
