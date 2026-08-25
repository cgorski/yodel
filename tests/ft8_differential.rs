//! FT8 differential against an independent implementation.
//!
//! # Why this exists
//!
//! Before this file, FT8 was validated **entirely by self-consistency**:
//! a closed transmit-to-receive loop, component known-answer tests, and
//! a frozen symbol snapshot that this implementation generated. That is
//! the exact shape of the IL2P defect (`docs/APRS_CONFORMANCE.md` §6.1)
//! — an encoder and a decoder that are mutual inverses stay mutual
//! inverses when a shared constant is wrong, so every round trip passes
//! while the mode cannot exchange a frame with anybody.
//!
//! FT8 was the largest remaining exposure, because it is the most
//! composed codec in the crate: 77-bit source packing, CRC-14,
//! LDPC(174,91), a Gray map and a Costas sync pattern, any one of which
//! can be individually plausible and jointly wrong.
//!
//! # What is compared, and why it is compared stage by stage
//!
//! The reference encoder prints its **intermediate** results, not just
//! its output, so this test compares four values per message rather
//! than one:
//!
//! | stage | what a mismatch means |
//! |---|---|
//! | 77 source-encoded bits | callsign/grid packing, field order, `i3` |
//! | 14-bit CRC | polynomial, bit order, the zero extension |
//! | 83 parity bits | the LDPC generator matrix and its bit order |
//! | 79 channel symbols | the Gray map, Costas pattern and sync placement |
//!
//! Comparing only the symbols would still catch every one of those, but
//! it would not say *which*, and two compensating errors could in
//! principle cancel. Comparing the chain localises the failure to one
//! stage in one line of output — which is what makes this cheap to act
//! on rather than just alarming.
//!
//! The other two legs put real audio across the boundary in each
//! direction, which is what proves the *waveform* rather than the bit
//! stream: tone spacing, GFSK shaping, symbol rate and slot timing.
//!
//! # Running
//!
//! Needs three binaries from an independent FT8 implementation,
//! located through environment variables so that no tracked file
//! hardcodes a path into `reference/` (see CONTRIBUTING.md):
//!
//! ```text
//! YODEL_REF_FT8_ENCODE=/path/to/symbol-printing encoder
//! YODEL_REF_FT8_GEN=/path/to/wav-writing generator
//! YODEL_REF_FT8_DECODE=/path/to/decoder
//!   cargo test --release --all-features --test ft8_differential -- --ignored --nocapture
//! ```
//!
//! Each test skips with a message when its binary is absent, so a
//! contributor without them still gets a fully green `cargo test`.
#![cfg(all(feature = "ft8", feature = "std"))]

use std::path::PathBuf;
use std::process::Command;

use yodel::SampleRate;
use yodel::ft8::{
    CODEWORD_BITS, Ft8Config, Ft8Decoder, Ft8DecoderConfig, Ft8Message, Ft8Modulator, Ft8Tail,
    MESSAGE_BITS, PARITY_BITS, PAYLOAD_BITS, SYMBOL_COUNT, add_crc, ldpc_encode,
};

/// The canonical FT8 working rate; the protocol is defined at it.
const RATE_HZ: u32 = 12_000;
/// An FT8 slot is 15 s; the transmission occupies 12.64 s of it.
const SLOT_SAMPLES: usize = 15 * RATE_HZ as usize;
/// Transmissions nominally start half a second into the slot.
const LEAD_IN_SAMPLES: usize = RATE_HZ as usize / 2;
/// Tone 0 of the transmission, and the centre of both searches.
const BASE_HZ: u32 = 1_500;

/// One case: the message text as both implementations spell it, and
/// the way this crate constructs it.
///
/// Text and construction are given separately on purpose. The
/// reference takes a line of text and does its own parsing; we take
/// typed fields. Deriving one from the other would put this crate's
/// idea of the message on both sides of the comparison, which is the
/// mistake this whole file exists to avoid.
struct Case {
    text: &'static str,
    build: fn() -> Ft8Message,
}

/// The least breadth each leg claims.
///
/// Every assertion in this file lives inside a loop over [`cases`], so
/// an empty case list would make all three tests pass having compared
/// nothing. That is the one failure mode a test suite cannot report on
/// itself, so the count is asserted rather than assumed — and stating
/// it also documents what "13/13" is out of.
const MIN_CASES: usize = 13;

/// Messages chosen to move every field of the source encoding: both
/// callsign slots, the `CQ` token, the acknowledgement flag, every
/// spelling of the 15-bit trailer, and free text.
fn cases() -> Vec<Case> {
    vec![
        Case {
            text: "CQ K1ABC FN42",
            build: || {
                Ft8Message::standard("CQ", "K1ABC", false, Ft8Tail::grid("FN42").expect("grid"))
                    .expect("message")
            },
        },
        Case {
            text: "K1ABC W9XYZ EN37",
            build: || {
                Ft8Message::standard(
                    "K1ABC",
                    "W9XYZ",
                    false,
                    Ft8Tail::grid("EN37").expect("grid"),
                )
                .expect("message")
            },
        },
        Case {
            text: "K1ABC W9XYZ -11",
            build: || {
                Ft8Message::standard("K1ABC", "W9XYZ", false, Ft8Tail::Report(-11))
                    .expect("message")
            },
        },
        Case {
            text: "K1ABC W9XYZ R+03",
            build: || {
                Ft8Message::standard("K1ABC", "W9XYZ", true, Ft8Tail::Report(3)).expect("message")
            },
        },
        Case {
            text: "K1ABC W9XYZ RRR",
            build: || Ft8Message::standard("K1ABC", "W9XYZ", false, Ft8Tail::Rrr).expect("message"),
        },
        Case {
            text: "K1ABC W9XYZ RR73",
            build: || {
                Ft8Message::standard("K1ABC", "W9XYZ", false, Ft8Tail::Rr73).expect("message")
            },
        },
        Case {
            text: "K1ABC W9XYZ 73",
            build: || {
                Ft8Message::standard("K1ABC", "W9XYZ", false, Ft8Tail::Seventy3).expect("message")
            },
        },
        Case {
            text: "K1ABC W9XYZ",
            build: || {
                Ft8Message::standard("K1ABC", "W9XYZ", false, Ft8Tail::None).expect("message")
            },
        },
        // Both extremes of the four-character locator range.
        Case {
            text: "CQ KA1ABC AA00",
            build: || {
                Ft8Message::standard("CQ", "KA1ABC", false, Ft8Tail::grid("AA00").expect("grid"))
                    .expect("message")
            },
        },
        Case {
            text: "CQ VE3XYZ RR99",
            build: || {
                Ft8Message::standard("CQ", "VE3XYZ", false, Ft8Tail::grid("RR99").expect("grid"))
                    .expect("message")
            },
        },
        // A three-character callsign needs the alignment rule; a
        // two-character prefix exercises the other branch.
        Case {
            text: "CQ W1AW FN31",
            build: || {
                Ft8Message::standard("CQ", "W1AW", false, Ft8Tail::grid("FN31").expect("grid"))
                    .expect("message")
            },
        },
        // Free text, the other payload type this crate implements.
        Case {
            text: "TNX BOB 73 GL",
            build: || Ft8Message::free_text("TNX BOB 73 GL").expect("message"),
        },
        Case {
            text: "HELLO WORLD",
            build: || Ft8Message::free_text("HELLO WORLD").expect("message"),
        },
    ]
}

/// Looks up a reference binary, or `None` to skip.
fn ref_binary(var: &str) -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os(var)?);
    // Unset means "this contributor does not have the binaries", which
    // is a legitimate skip. Set-but-wrong means somebody typed a path
    // and meant to run this, so it is a hard failure -- otherwise a
    // single typo turns an entire interoperability suite green while it
    // tests nothing at all, which is the most expensive way a test can
    // fail. `tests/differential.rs` has always asserted this; these
    // suites had drifted from it.
    assert!(
        path.is_file(),
        "{var}={} does not point to an existing file. Unset the variable \
         to skip this suite deliberately; leaving it set and wrong would \
         pass without testing anything.",
        path.display()
    );
    // Absolute, because the tests below run these binaries with a working
    // directory of their own (see `scratch_subdir`). Rust documents a
    // relative program path combined with `current_dir` as platform
    // specific, and where it does not work the failure reads as a missing
    // binary -- indistinguishable from the typo case this function exists
    // to report.
    Some(
        path.canonicalize()
            .unwrap_or_else(|e| panic!("{var}={}: {e}", path.display())),
    )
}

/// Expands packed MSB-first bits into one `0`/`1` byte each, so that a
/// mismatch reports the bit position rather than a hex byte.
fn bits(packed: &[u8], count: usize) -> Vec<u8> {
    (0..count)
        .map(|pos| (packed[pos / 8] >> (7 - pos % 8)) & 1)
        .collect()
}

/// Pulls the first line of `text` after `header` that is `count`
/// characters of the given alphabet, ignoring the reference's column
/// spacing.
fn labelled_run(text: &str, header: &str, count: usize) -> Option<Vec<u8>> {
    let after = text.split(header).nth(1)?;
    after.lines().find_map(|line| {
        let digits: Vec<u8> = line
            .chars()
            .filter(|c| !c.is_whitespace())
            .map(|c| c.to_digit(10).map(|d| d as u8))
            .collect::<Option<Vec<u8>>>()?;
        (digits.len() == count).then_some(digits)
    })
}

/// The reference's dissection of one message.
struct Reference {
    payload: Vec<u8>,
    crc: Vec<u8>,
    parity: Vec<u8>,
    symbols: Vec<u8>,
}

fn run_reference(encoder: &PathBuf, text: &str) -> Reference {
    let output = Command::new(encoder)
        .arg(text)
        .output()
        .expect("run the reference FT8 encoder");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let field = |header: &str, count: usize| {
        labelled_run(&stdout, header, count)
            .unwrap_or_else(|| panic!("no {count}-symbol run after {header:?} in:\n{stdout}"))
    };
    Reference {
        payload: field("77 bits:", PAYLOAD_BITS),
        crc: field("14-bit CRC:", 14),
        parity: field("83 Parity bits:", PARITY_BITS),
        symbols: field("(79 tones):", SYMBOL_COUNT),
    }
}

// ---------------------------------------------------------------------
// The composed encoding, stage by stage
// ---------------------------------------------------------------------

/// Every stage of the FT8 channel encoding must match the reference
/// encoder's, for every message.
///
/// This is the test the crate most needed and did not have. A wrong
/// constant anywhere in the source packing, the CRC polynomial, the
/// LDPC generator, the Gray map or the Costas pattern changes at least
/// one value here, and no amount of internal round-tripping can see
/// any of them.
#[test]
#[ignore = "requires YODEL_REF_FT8_ENCODE"]
fn channel_encoding_matches_the_reference_encoder() {
    let Some(encoder) = ref_binary("YODEL_REF_FT8_ENCODE") else {
        eprintln!("YODEL_REF_FT8_ENCODE not set — skipping");
        return;
    };

    let cases = cases();
    assert!(cases.len() >= MIN_CASES, "case list shrank");
    for case in &cases {
        let message = (case.build)();
        let theirs = run_reference(&encoder, case.text);

        // Stage 1 — the 77 source-encoded bits.
        let payload = message.payload();
        assert_eq!(
            bits(&payload, PAYLOAD_BITS),
            theirs.payload,
            "77-bit source encoding differs for {:?}",
            case.text
        );

        // Stage 2 — the 14-bit CRC, which the reference prints as
        // codeword bits 78..=91, i.e. the tail of the protected
        // message.
        let protected = add_crc(&payload);
        assert_eq!(
            bits(&protected, MESSAGE_BITS)[PAYLOAD_BITS..],
            theirs.crc[..],
            "CRC-14 differs for {:?}",
            case.text
        );

        // Stage 3 — the 83 LDPC parity bits.
        let codeword = ldpc_encode(&protected);
        assert_eq!(
            bits(&codeword, CODEWORD_BITS)[MESSAGE_BITS..],
            theirs.parity[..],
            "LDPC parity differs for {:?}",
            case.text
        );

        // Stage 4 — the 79 channel symbols, sync included.
        let ours = message.channel_symbols();
        assert_eq!(
            ours.as_slice(),
            theirs.symbols.as_slice(),
            "channel symbols differ for {:?}\n ours: {ours:?}\ntheirs: {:?}",
            case.text,
            theirs.symbols
        );
        assert!(ours.iter().all(|&s| s <= 7), "symbol outside 0..=7");
    }

    println!(
        "FT8 channel encoding: {}/{} messages identical at all four stages",
        cases.len(),
        cases.len()
    );
}

// ---------------------------------------------------------------------
// Audio, in both directions
// ---------------------------------------------------------------------

/// A private, empty scratch directory for one test's generated audio.
///
/// Each test gets its own subdirectory rather than sharing `scratch/`,
/// because the reference binaries write working files of their own
/// there. Those landing beside the benchmark fixtures is how a stale
/// file quietly starts influencing a later run.
fn scratch_subdir(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("scratch")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Our modulator's output, padded into a full 15-second slot.
fn our_slot_samples(message: &Ft8Message) -> Vec<i16> {
    let config =
        Ft8Config::new(BASE_HZ, SampleRate::new(RATE_HZ).expect("valid rate")).expect("config");
    let mut samples = vec![0i16; LEAD_IN_SAMPLES];
    samples.extend(Ft8Modulator::for_message(config, message));
    samples.resize(SLOT_SAMPLES, 0);
    samples
}

fn write_wav(path: &PathBuf, samples: &[i16]) {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: RATE_HZ,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).expect("create wav");
    for &s in samples {
        writer.write_sample(s).expect("write sample");
    }
    writer.finalize().expect("finalize wav");
}

fn read_wav(path: &PathBuf) -> Vec<i16> {
    let mut reader = hound::WavReader::open(path).expect("open wav");
    assert_eq!(reader.spec().sample_rate, RATE_HZ, "reference wav rate");
    assert_eq!(reader.spec().channels, 1, "reference wav channels");
    reader
        .samples::<i16>()
        .map(|s| s.expect("sample"))
        .collect()
}

/// The decoder's two required arguments, which precede the file name:
/// its iteration limit and the depth of its ordered-statistics stage.
/// Both are the reference's own documented example values, and they
/// govern how hard it tries rather than what counts as a decode.
///
/// They are not optional, and omitting them is not a loud failure. A
/// decoder handed a bare file name prints its usage text and exits
/// **zero** -- which is indistinguishable from a decoder that ran and
/// heard nothing, so this leg reported that our transmission was
/// unrecoverable while the decoder had never looked at it.
const DECODER_ARGS: [&str; 2] = ["40", "2"];

/// The reference decoder must recover our message from our audio.
///
/// This is what the symbol comparison cannot reach: the symbols could
/// be perfect while the tone spacing, the GFSK shaping or the slot
/// timing put the energy somewhere the other implementation does not
/// look.
#[test]
#[ignore = "requires YODEL_REF_FT8_DECODE"]
fn our_transmission_decodes_in_the_reference_decoder() {
    let Some(decoder) = ref_binary("YODEL_REF_FT8_DECODE") else {
        eprintln!("YODEL_REF_FT8_DECODE not set — skipping");
        return;
    };
    let dir = scratch_subdir("ft8_ref_decode");

    let cases = cases();
    assert!(cases.len() >= MIN_CASES, "case list shrank");
    let mut decoded = 0usize;
    for (index, case) in cases.iter().enumerate() {
        let message = (case.build)();
        // The reference derives a timestamp from the file name, so it
        // must look like one of its captures.
        let path = dir.join(format!("250101_{:06}.wav", 100 + index));
        write_wav(&path, &our_slot_samples(&message));

        let output = Command::new(&decoder)
            .current_dir(&dir)
            .args(DECODER_ARGS)
            .arg(&path)
            .output()
            .expect("run the reference decoder");
        let stdout = String::from_utf8_lossy(&output.stdout);
        // A decoder that crashed, or refused its arguments, has not
        // disagreed with us about anything. Distinguishing that from a
        // real miss matters: charged to the wrong account it reads as
        // this crate's transmitter failing interoperability.
        assert!(
            output.status.success(),
            "the reference decoder exited unsuccessfully ({}) instead of \
             decoding {:?}. That is a fault in the decoder or in the \
             arguments it was given, not evidence about our \
             transmission.\nstdout:\n{stdout}\nstderr:\n{}",
            output.status,
            case.text,
            String::from_utf8_lossy(&output.stderr)
        );
        // Compare on whitespace-normalised lines: the reference pads
        // its columns and we do not care where.
        let want = case.text.split_whitespace().collect::<Vec<_>>().join(" ");
        let found = stdout
            .lines()
            .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
            .any(|line| line.ends_with(&want));
        assert!(
            found,
            "the reference decoder did not recover {:?} from our audio:\n{stdout}",
            case.text
        );
        let _ = std::fs::remove_file(&path);
        decoded += 1;
    }
    let _ = std::fs::remove_dir_all(&dir);
    println!(
        "our audio, their decoder: {decoded}/{} recovered",
        cases.len()
    );
}

/// Our receive engine must recover the reference's message from the
/// reference's audio.
#[test]
#[ignore = "requires YODEL_REF_FT8_GEN"]
fn we_decode_the_reference_transmission() {
    let Some(generator) = ref_binary("YODEL_REF_FT8_GEN") else {
        eprintln!("YODEL_REF_FT8_GEN not set — skipping");
        return;
    };
    let dir = scratch_subdir("ft8_ref_gen");

    let decoder = Ft8Decoder::new(Ft8DecoderConfig::new(BASE_HZ, 100).expect("config"));
    let cases = cases();
    assert!(cases.len() >= MIN_CASES, "case list shrank");
    let mut decoded = 0usize;
    for case in &cases {
        for entry in std::fs::read_dir(&dir).expect("read gen dir").flatten() {
            let _ = std::fs::remove_file(entry.path());
        }
        // args: message f0 DT fdop delay nfiles snr
        // fdop/delay 0 keep the channel clean; a high SNR disables the
        // noise generator, so this measures conformance and not
        // sensitivity (tests/ft8_rx.rs pins that).
        let status = Command::new(&generator)
            .current_dir(&dir)
            .args([
                case.text,
                &BASE_HZ.to_string(),
                "0.0",
                "0.0",
                "0.0",
                "1",
                "20",
            ])
            .output()
            .expect("run the reference generator");
        assert!(
            status.status.success(),
            "reference generator failed: {}",
            String::from_utf8_lossy(&status.stderr)
        );

        let wav = std::fs::read_dir(&dir)
            .expect("read gen dir")
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|e| e == "wav"))
            .expect("the reference generator wrote no wav");

        let samples = read_wav(&wav);
        assert_eq!(samples.len(), SLOT_SAMPLES, "a full 15-second slot");

        let decodes = decoder.decode(&samples).expect("decode");
        let want = case.text.split_whitespace().collect::<Vec<_>>().join(" ");
        let found = decodes.iter().any(|d| d.message.as_str() == want);
        assert!(
            found,
            "we did not recover {:?} from the reference's audio; got {:?}",
            case.text,
            decodes
                .iter()
                .map(|d| d.message.as_str())
                .collect::<Vec<_>>()
        );
        decoded += 1;
    }
    let _ = std::fs::remove_dir_all(&dir);
    println!(
        "their audio, our decoder: {decoded}/{} recovered",
        cases.len()
    );
}
