//! IL2P differential against an independent implementation.
//!
//! # Why this exists
//!
//! IL2P is the mode this crate has already shipped broken once. Through
//! v0.4 it could not exchange a single frame with any other station
//! while every one of its round-trip tests passed, because an encoder
//! and decoder that are mutual inverses stay mutual inverses when a
//! shared constant is wrong (`docs/APRS_CONFORMANCE.md` §6.1). Session
//! 17 corrected the constants against the specification's own example
//! packets — but example packets are byte vectors, and nothing had ever
//! put IL2P **on the air** against another implementation.
//!
//! Doing so immediately found a defect that byte vectors structurally
//! cannot catch, because it lives below them: the crate was applying
//! NRZI to IL2P. Specification v0.6, "Interface to Physical Layer",
//! says of both the AFSK and the FSK symbol maps: *"Differential
//! encoding is not used."* NRZI **is** differential encoding. Applying
//! it on transmit and undoing it on receive is invisible to every
//! internal test and fatal to interoperability. The receive direction
//! went from **0 to 4 frames** on reference audio when it was removed.
//!
//! # Running
//!
//! ```text
//! YODEL_REF_GEN=/path/to/packet generator
//! YODEL_REF_DECODE=/path/to/audio decoder
//!   cargo test --release --all-features --test il2p_differential -- --ignored --nocapture
//! ```
//!
//! The same two binaries `tests/differential.rs` uses; the generator
//! needs an IL2P transmit option and the decoder an IL2P receive path.
#![cfg(all(feature = "il2p", feature = "demod", feature = "std"))]

use std::path::PathBuf;
use std::process::Command;

use yodel::demodulator::{AfskDemodulator, DemodulatorConfig};
use yodel::il2p::{Il2pParity, Il2pReceiver};
use yodel::{Bit, SampleRate};

/// Frames the reference generator is asked to produce.
const FRAME_COUNT: usize = 5;

/// Floor on frames recovered from the reference's IL2P audio.
///
/// A ratchet, and the number is not arbitrary: the reference's **own**
/// decoder recovers 4 of the 5 frames from this recording, losing the
/// first to its generator's lead-in. Matching it exactly is the target;
/// dropping below it is a regression.
///
/// MEASURED: 0 before the NRZI defect was fixed, **4** after.
const MIN_FRAMES_FROM_REFERENCE: usize = 4;

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
    Some(path)
}

/// A private, empty scratch directory for one test's generated audio,
/// kept out of `scratch/` proper so the benchmark fixtures there are
/// never disturbed.
fn scratch_subdir(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("scratch")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Decodes IL2P frames from a recording, returning each UI frame's
/// rendered `SRC>DEST` and information field.
///
/// No NRZI stage — see the module docs.
fn decode_il2p(path: &PathBuf) -> Vec<String> {
    let mut reader = hound::WavReader::open(path).expect("open wav");
    let rate = reader.spec().sample_rate;
    let samples: Vec<i16> = reader
        .samples::<i16>()
        .map(|s| s.expect("sample"))
        .collect();

    let config = DemodulatorConfig::bell_202(SampleRate::new(rate).expect("rate")).expect("config");
    let mut demod = AfskDemodulator::new(config).expect("demod");
    let mut rx = Il2pReceiver::new(Il2pParity::Sixteen);
    let mut out = Vec::new();
    for &sample in &samples {
        let Some(bit) = demod.push_sample_i16(sample) else {
            continue;
        };
        if let Some(Ok(frame)) = rx.push(bit)
            && let Ok(ui) = frame.ui_frame()
        {
            out.push(format!(
                "{}>{}:{}",
                core::str::from_utf8(ui.src.callsign.as_bytes()).unwrap_or("?"),
                core::str::from_utf8(ui.dest.callsign.as_bytes()).unwrap_or("?"),
                String::from_utf8_lossy(ui.info),
            ));
        }
    }
    let _ = Bit::One; // keep the import meaningful across cfgs
    out
}

/// We must recover the reference implementation's IL2P transmissions.
///
/// This is the test that caught the NRZI defect, and the one that would
/// catch its return.
#[test]
#[ignore = "requires YODEL_REF_GEN"]
fn we_decode_the_reference_il2p_transmission() {
    let Some(generator) = ref_binary("YODEL_REF_GEN") else {
        eprintln!("YODEL_REF_GEN not set — skipping");
        return;
    };
    let dir = scratch_subdir("il2p_ref_gen");
    let path = dir.join("il2p_reference.wav");

    // The generator's IL2P transmit option; n=1 selects its stronger FEC.
    let count = FRAME_COUNT.to_string();
    let args = ["-I", "1", "-n", &count, "-o"];
    let output = Command::new(&generator)
        .args(args)
        .arg(&path)
        .output()
        .expect("run the reference generator");
    // argv, status and both streams; see tests/wspr_differential.rs for
    // why stderr alone is not enough.
    assert!(
        output.status.success(),
        "reference generator failed ({})\n  binary: {}\n  args:   {:?} {}\n\
         --- stdout ---\n{}\n  --- stderr ---\n{}",
        output.status,
        generator.display(),
        args,
        path.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let frames = decode_il2p(&path);
    println!("recovered {} IL2P frames from the reference:", frames.len());
    for frame in &frames {
        println!("  {frame}");
    }
    assert!(
        frames.len() >= MIN_FRAMES_FROM_REFERENCE,
        "recovered {} IL2P frames from the reference's audio, floor is \
         {MIN_FRAMES_FROM_REFERENCE}. Zero means the physical-layer \
         coding has diverged again — IL2P is NOT differentially encoded \
         (spec v0.6, \"Interface to Physical Layer\"), so no NRZI stage \
         belongs anywhere in this path.",
        frames.len()
    );
    // Content, not just a count: the reference's built-in test message.
    assert!(
        frames.iter().all(|f| f.contains("quick brown fox")),
        "recovered frames do not carry the reference's test message: {frames:?}"
    );

    // ...and again through the CLI, which is the path a user runs. The
    // check above exercises `Il2pReceiver` directly, so it cannot see a
    // line-coding stage wrongly reintroduced in the binary's glue --
    // which is exactly where the NRZI defect lived.
    // Verified by mutation: re-adding NRZI to the CLI's IL2P decode
    // escapes the library-level check and is caught here.
    let cli = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/release/yodel");
    if cli.is_file() {
        let output = Command::new(&cli)
            .args(["decode", "--il2p"])
            .arg(&path)
            .output()
            .expect("run our CLI decoder");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let via_cli = stdout.matches("quick brown fox").count();
        println!("the CLI recovered {via_cli} IL2P frames from the reference");
        assert!(
            via_cli >= MIN_FRAMES_FROM_REFERENCE,
            "the CLI recovered {via_cli} IL2P frames, floor is \
             {MIN_FRAMES_FROM_REFERENCE}.\n{stdout}"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// The reference implementation must recover **our** IL2P
/// transmissions.
///
/// This direction was broken for a second, independent reason, and it
/// is the more interesting of the two defects because the crate was
/// **conforming to the specification** while being undecodable by
/// everyone.
///
/// Header byte 0 bit 7 is the v0.4 "FEC level": set means a constant 16
/// parity symbols per payload block, clear means the variable
/// 2/4/6/8-symbol baseline scheme, which also splits blocks
/// differently. Draft v0.6 deleted baseline FEC, mandated 16 symbols
/// everywhere, and redefined the bit as RESERVED — so a strict v0.6
/// encoder clears it.
///
/// Deployed receivers did not follow. They still read the bit and use
/// it to compute **how many bytes to take off the air** for the
/// payload. Clearing it while sending 16-symbol parity told the
/// reference to collect 61 bytes where we had sent 75; its RS decode
/// then failed and the frame was discarded. Everything else — sync
/// word, scrambler, header RS, payload blocks — was already byte
/// exact, which is precisely why no vector-based test could see it.
///
/// The fix derives the header bit from the same [`Il2pParity`] the
/// payload is encoded with, so the two cannot disagree.
///
/// One difference remains: the Command/Response bit of the UI control
/// subfield. IL2P carries it as a single bit copied from the AX.25
/// destination address's C bit, which this crate's `Address` does not
/// currently model, so we always emit "response" where the reference
/// emits "command". It is never validated on receive and does not
/// affect decodability — see `docs/APRS_CONFORMANCE.md` §6.2.
#[test]
#[ignore = "requires YODEL_REF_GEN / YODEL_REF_DECODE"]
fn the_reference_decodes_our_il2p_transmission() {
    let (Some(generator), Some(decoder)) =
        (ref_binary("YODEL_REF_GEN"), ref_binary("YODEL_REF_DECODE"))
    else {
        eprintln!("YODEL_REF_GEN / YODEL_REF_DECODE not set — skipping");
        return;
    };
    let _ = generator;

    // Built by the CLI, which is the only path that assembles a full
    // IL2P transmission today.
    let dir = scratch_subdir("il2p_ours");
    let path = dir.join("il2p_ours.wav");
    let cli = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/release/yodel");
    if !cli.is_file() {
        eprintln!("{} not built — skipping", cli.display());
        return;
    }
    let built = Command::new(&cli)
        .args(["gen", "--il2p", "--count", &FRAME_COUNT.to_string()])
        .arg("--out")
        .arg(&path)
        .output()
        .expect("run our generator");
    assert!(built.status.success(), "our generator failed");

    let output = Command::new(&decoder)
        .arg(&path)
        .output()
        .expect("run the reference decoder");
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Match on CONTENT, not on line position. The reference colourises
    // its output, so every decoded line begins with an ANSI escape
    // sequence rather than the "[0]" it appears to start with -- a
    // positional match silently counts zero while the frames are all
    // there. Checking for each frame's own sequence counter is both
    // immune to that and a stronger assertion: it proves all five
    // distinct frames arrived intact, not just that five lines were
    // printed.
    let recovered = (1..=FRAME_COUNT)
        .filter(|i| stdout.contains(&format!("[{i}/{FRAME_COUNT}]")))
        .count();
    println!("the reference recovered {recovered}/{FRAME_COUNT} of our IL2P frames");
    assert_eq!(
        recovered, FRAME_COUNT,
        "the reference decoder recovered {recovered} of our {FRAME_COUNT} IL2P \
         frames.\n{stdout}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
