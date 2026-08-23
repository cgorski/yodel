//! Permanent decode-performance regression benchmark.
//!
//! Mirrors `scripts/benchmark.sh`: each corpus WAV is decoded through
//! the same pipeline the `warble` CLI uses (`DefaultTncReceiver` fed
//! sample-by-sample; the receiver's multi-chain dedup means one emitted
//! frame per unique decode, exactly what the script's `grep -c '>'`
//! counts). The thresholds below mirror the "current best" recorded in
//! `docs/BENCHMARKS.md` and MUST be raised whenever the record improves
//! so the best counts can never silently regress.
//!
//! The corpus is operator-provided and gitignored, so the tests here are
//! `#[ignore]`d and must behave sanely when their inputs are missing.
//! The rule, shared with `tests/corpus_aprs.rs`, is all-or-nothing:
//! **no input present skips with a message, a partially populated input
//! set fails.** [`availability`] is where that decision lives and why —
//! a subset measured silently is the dangerous state, because it reads
//! as a pass while the ratchet only covered whichever files happened to
//! be on the machine. A contributor with no `corpus/` still gets a green
//! `cargo test`.
//!
//! The synthetic-noise track of the script is generated fresh each run
//! by the reference generator; fixed copies of those WAVs (under
//! `scratch/`, also operator-provided and gitignored) pin the synthetic
//! rows when present.
//!
//! Run with the corpus present:
//!
//! ```text
//! cargo test --all-features --test benchmark -- --ignored
//! ```
#![cfg(feature = "tnc")]

use std::path::{Path, PathBuf};

use warble::tnc::{DefaultTncReceiver, TncConfig};
use warble::{ModemProfile, SampleRate};

/// One pinned corpus track: file name, minimum frame count, and whether
/// the count must match exactly (the clean canary track).
struct Track {
    file: &'static str,
    min_frames: usize,
    exact: bool,
}

/// Pinned thresholds — the "current best" rows of docs/BENCHMARKS.md.
/// Raise these whenever the record improves; never lower them.
const TRACKS: &[Track] = &[
    Track {
        file: "01_40-Mins-Traffic_-on-144.39.wav",
        min_frames: 999,
        exact: false,
    },
    Track {
        file: "02_100-Mic-E-Bursts-DE-emphasized.wav",
        min_frames: 985,
        exact: false,
    },
    Track {
        file: "03_100-Mic-E-Bursts-Flat.wav",
        min_frames: 100,
        exact: true,
    },
    Track {
        file: "04_25-MIns-Drive-Test.wav",
        min_frames: 98,
        exact: false,
    },
];

/// Floor on how many [`TRACKS`] rows a *measuring* run must compare
/// against its threshold: **4, out of the 4 rows in `TRACKS`**.
///
/// A pinned threshold that is never reached is not a ratchet, and the
/// loop below used to `continue` past a missing file — so an empty or
/// half-populated `corpus/` reported success having compared nothing.
/// This floor is the count half of the defence ([`availability`] is the
/// other half): raise it in step with `TRACKS`, never lower it.
///
/// The file pins three synthetic rows besides these four, seven in all.
/// Each of those is a single-file test with no loop to come up short in,
/// so its all-or-nothing gate *is* its row floor: if
/// [`inputs_ready`] returns `true` the fixture is there and the
/// measurement below it cannot be skipped.
const MIN_TRACKS_MEASURED: usize = 4;

/// Deleting a pinned row would silently shrink what
/// [`MIN_TRACKS_MEASURED`] is "out of", so the two are welded together
/// at compile time.
const _: () = assert!(
    TRACKS.len() >= MIN_TRACKS_MEASURED,
    "TRACKS must still hold every row MIN_TRACKS_MEASURED claims to count"
);

/// Which of a suite's required input files are present.
///
/// Kept as a pure function of `(name, exists)` pairs — see
/// [`availability`] — so the three-state decision can be unit-tested
/// with fabricated lists, without a corpus and without going near the
/// 579 MB of operator-provided recordings.
#[derive(Debug, PartialEq, Eq)]
enum Availability<'a> {
    /// Every required input is present: measure and compare.
    AllPresent,
    /// Not one required input is present: skip cleanly, test passes.
    NonePresent,
    /// Some present, some absent: fail.
    Partial {
        present: Vec<&'a str>,
        missing: Vec<&'a str>,
    },
}

/// Classifies a required-input set into the three states above.
///
/// The middle state is the point. Skipping when nothing is there is
/// correct — the inputs are operator-provided and gitignored, and a
/// contributor without them must still get a green `cargo test`. Doing
/// the same when *some* are there is not: the run prints reassuring
/// per-file lines, compares whichever subset exists, and passes, so a
/// half-restored corpus looks exactly like a full one. That is strictly
/// worse than measuring nothing, because it is indistinguishable from
/// having measured everything.
///
/// An empty list is *not* a clean skip. It is the very hole this
/// function exists to close, so it reads as `AllPresent` and lets
/// [`MIN_TRACKS_MEASURED`] fail the run.
fn availability<'a>(inputs: &[(&'a str, bool)]) -> Availability<'a> {
    let present: Vec<&'a str> = inputs
        .iter()
        .filter(|&&(_, exists)| exists)
        .map(|&(name, _)| name)
        .collect();
    let missing: Vec<&'a str> = inputs
        .iter()
        .filter(|&&(_, exists)| !exists)
        .map(|&(name, _)| name)
        .collect();
    match (present.is_empty(), missing.is_empty()) {
        (true, false) => Availability::NonePresent,
        (false, false) => Availability::Partial { present, missing },
        // All present, or (vacuously) nothing required at all.
        (_, true) => Availability::AllPresent,
    }
}

/// Applies [`availability`]'s rule and reports what it decided.
///
/// Returns `true` when the caller should measure, `false` when it should
/// return early having skipped, and panics (i.e. fails the test) on a
/// partially populated input set.
#[must_use]
fn inputs_ready(what: &str, hint: &str, inputs: &[(&str, bool)]) -> bool {
    match availability(inputs) {
        Availability::AllPresent => true,
        Availability::NonePresent => {
            println!("{what}: no input present; skipping ({hint})");
            false
        }
        Availability::Partial { present, missing } => panic!(
            "{what}: partially populated input set — {} of {} present. \
             Present: {}. Absent: {}. Refusing to measure a subset: it \
             would pass while the ratchet covered only the files that \
             happened to be here. Provide every input or none ({hint}).",
            present.len(),
            present.len() + missing.len(),
            present.join(", "),
            missing.join(", "),
        ),
    }
}

/// Pairs each required file name with whether it exists under `dir`.
/// The only I/O in the presence decision, so [`availability`] stays pure.
fn presence<'a>(dir: &Path, names: impl IntoIterator<Item = &'a str>) -> Vec<(&'a str, bool)> {
    names
        .into_iter()
        .map(|name| (name, dir.join(name).is_file()))
        .collect()
}

/// As [`presence`], for a suite whose required input is a single file.
fn presence_of<'a>(label: &'a str, path: &Path) -> [(&'a str, bool); 1] {
    [(label, path.is_file())]
}

/// Absolute path to an operator-provided input, relative to the crate root.
fn fixture(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
}

/// Decodes one WAV through the CLI pipeline and returns the number of
/// unique frames emitted (post-dedup, same as `warble decode | grep -c '>'`).
fn decode_count(path: &Path) -> Result<usize, String> {
    decode_count_with(path, ModemProfile::BELL_202)
}

/// As [`decode_count`], for a non-default modem profile (the 300-baud
/// row uses `--preset hf300`, which is a different tone pair and baud).
fn decode_count_with(path: &Path, profile: ModemProfile) -> Result<usize, String> {
    let mut reader =
        hound::WavReader::open(path).map_err(|e| format!("opening {}: {e}", path.display()))?;
    let spec = reader.spec();
    if spec.channels != 1
        || spec.bits_per_sample != 16
        || spec.sample_format != hound::SampleFormat::Int
    {
        return Err(format!(
            "{}: 16-bit mono integer PCM is required, got {} ch / {} bits / {:?}",
            path.display(),
            spec.channels,
            spec.bits_per_sample,
            spec.sample_format
        ));
    }
    let rate = SampleRate::new(spec.sample_rate)
        .map_err(|e| format!("{}: sample rate: {e}", path.display()))?;
    let config = TncConfig::from_profile(rate, profile)
        .map_err(|e| format!("{}: config: {e}", path.display()))?;
    let mut rx = DefaultTncReceiver::new(config)
        .map_err(|e| format!("{}: receiver setup: {e}", path.display()))?;
    let mut frames = 0usize;
    for sample in reader.samples::<i16>() {
        let sample = sample.map_err(|e| format!("reading {}: {e}", path.display()))?;
        if rx.push_i16(sample).is_some() {
            frames += 1;
        }
    }
    Ok(frames)
}

/// Decodes one WAV through the FX.25-aware receive path, mirroring
/// `warble decode --fx25`: a bare demodulator feeding the correlation-tag
/// hunter, with a parallel plain-HDLC path.
#[cfg(feature = "fx25")]
fn decode_count_fx25(path: &Path) -> Result<usize, String> {
    use warble::ax25::UiFrame;
    use warble::demodulator::{AfskDemodulator, DemodulatorConfig};
    use warble::fx25::Fx25Receiver;
    use warble::nrzi::NrziDecoder;

    let mut reader =
        hound::WavReader::open(path).map_err(|e| format!("opening {}: {e}", path.display()))?;
    let rate = SampleRate::new(reader.spec().sample_rate)
        .map_err(|e| format!("{}: sample rate: {e}", path.display()))?;
    let profile = ModemProfile::BELL_202;
    let cfg = DemodulatorConfig::new(rate, profile.baud(), profile.tones())
        .map_err(|e| format!("{}: config: {e}", path.display()))?;
    let mut demod =
        AfskDemodulator::new(cfg).map_err(|e| format!("{}: receiver: {e}", path.display()))?;
    let mut nrzi = NrziDecoder::default();
    let mut rx = Fx25Receiver::<330>::new();
    let mut frames = 0usize;
    for sample in reader.samples::<i16>() {
        let sample = sample.map_err(|e| format!("reading {}: {e}", path.display()))?;
        if let Some(line) = demod.push_sample_i16(sample)
            && let Some(Ok(frame)) = rx.push(nrzi.decode(line))
            && UiFrame::parse(frame).is_ok()
        {
            frames += 1;
        }
    }
    Ok(frames)
}

/// Asserts the decoded-frame count of every corpus track never drops
/// below the pinned record.
///
/// Skips cleanly when no corpus track is present and **fails** when only
/// some are: see [`availability`]. The [`MIN_TRACKS_MEASURED`] floor at
/// the end is the second lock — no arrangement of missing files or
/// mid-file read errors can let this pass having compared fewer rows
/// than it claims to.
#[test]
#[ignore = "needs the operator-provided corpus/ WAVs; run with -- --ignored"]
fn corpus_decode_counts_never_regress() {
    let corpus = fixture("corpus");
    let inputs = presence(&corpus, TRACKS.iter().map(|track| track.file));
    if !inputs_ready("corpus/", "operator-provided, gitignored", &inputs) {
        return;
    }
    let mut failures = Vec::new();
    let mut measured = 0usize;
    for track in TRACKS {
        let count = match decode_count(&corpus.join(track.file)) {
            Ok(n) => n,
            Err(e) => {
                failures.push(e);
                continue;
            }
        };
        measured += 1;
        let ok = if track.exact {
            count == track.min_frames
        } else {
            count >= track.min_frames
        };
        let op = if track.exact { "==" } else { ">=" };
        println!(
            "{}: {count} frames (required {op} {}) {}",
            track.file,
            track.min_frames,
            if ok { "OK" } else { "REGRESSION" }
        );
        if !ok {
            failures.push(format!(
                "{}: {count} frames, required {op} {}",
                track.file, track.min_frames
            ));
        }
    }
    println!(
        "measured {measured} of {} pinned tracks (required >= {MIN_TRACKS_MEASURED})",
        TRACKS.len()
    );
    if measured < MIN_TRACKS_MEASURED {
        failures.push(format!(
            "measured only {measured} of the {} pinned tracks, required >= \
             {MIN_TRACKS_MEASURED}: the thresholds above did not all run",
            TRACKS.len()
        ));
    }
    assert!(
        failures.is_empty(),
        "benchmark regression(s): {}",
        failures.join("; ")
    );
}

/// Pins the synthetic-noise row of `docs/BENCHMARKS.md` at its current
/// value (74 frames, measured on the operator-provided fixed noise WAV):
/// the count must never drop below the record. Additive only — the four
/// real-world pins above are untouched. Skips cleanly when the WAV is
/// absent (it is operator-provided and gitignored, like the corpus);
/// one row out of one, so the gate is also the row floor.
#[test]
#[ignore = "needs the operator-provided scratch/bench_noise.wav; run with -- --ignored"]
fn synthetic_noise_row_never_regresses() {
    const SYNTHETIC_MIN_FRAMES: usize = 74;
    let path = fixture("scratch/bench_noise.wav");
    if !inputs_ready(
        "synthetic-noise-100",
        "operator-provided, gitignored",
        &presence_of("scratch/bench_noise.wav", &path),
    ) {
        return;
    }
    let count = decode_count(&path).expect("decoding the synthetic-noise WAV");
    println!("synthetic-noise-100: {count} frames (required >= {SYNTHETIC_MIN_FRAMES})");
    assert!(
        count >= SYNTHETIC_MIN_FRAMES,
        "synthetic-noise regression: {count} frames, required >= {SYNTHETIC_MIN_FRAMES}"
    );
}

/// Pins the FX.25 synthetic-noise row.
///
/// The FX.25 receive path runs a bare [`warble::AfskDemodulator`] rather
/// than the diversity bank, so it was the crate's largest deficit (60
/// against the reference's 82) until the discriminator's decision
/// statistic was given the same envelope smoothing the `TncReceiver`
/// chains already used. It now leads both the reference (82) and its
/// best tuned profile (91). See the tone-envelope smoothing discussion
/// on `QuadratureCorrelator::push`.
///
/// Skips cleanly when the WAV is absent (operator-provided and
/// gitignored, like the corpus); one row out of one, so the gate is also
/// the row floor.
#[test]
// `decode_count_fx25` is `fx25`-gated, so this row must be too: without
// the gate the file failed to COMPILE for any feature set that has
// `tnc` but not `fx25` (e.g. `--no-default-features --features tnc`).
#[cfg(feature = "fx25")]
#[ignore = "needs the operator-provided scratch/bench_noise_fx25.wav; run with -- --ignored"]
fn synthetic_noise_fx25_row_never_regresses() {
    const SYNTHETIC_FX25_MIN_FRAMES: usize = 92;
    let path = fixture("scratch/bench_noise_fx25.wav");
    if !inputs_ready(
        "synthetic-noise-100-fx25",
        "operator-provided, gitignored",
        &presence_of("scratch/bench_noise_fx25.wav", &path),
    ) {
        return;
    }
    let count = decode_count_fx25(&path).expect("decoding the FX.25 synthetic-noise WAV");
    println!("synthetic-noise-100-fx25: {count} frames (required >= {SYNTHETIC_FX25_MIN_FRAMES})");
    assert!(
        count >= SYNTHETIC_FX25_MIN_FRAMES,
        "FX.25 synthetic-noise regression: {count} frames, \
         required >= {SYNTHETIC_FX25_MIN_FRAMES}"
    );
}

/// Pins the 300-baud synthetic-noise row. This profile ran a single
/// balanced chain against Bell 202's eleven and was the crate's largest
/// deficit (58 against the reference's 70) until the correlator
/// observation window was widened to the orthogonal 1.5 bits; it now
/// leads both the reference (70) and its best tuned profile (72). See
/// the tone-orthogonality section of `src/discriminator.rs`.
///
/// Skips cleanly when the WAV is absent (operator-provided and
/// gitignored, like the corpus); one row out of one, so the gate is also
/// the row floor.
#[test]
#[ignore = "needs the operator-provided scratch/bench_noise_300.wav; run with -- --ignored"]
fn synthetic_noise_300_baud_row_never_regresses() {
    const SYNTHETIC_300_MIN_FRAMES: usize = 74;
    let path = fixture("scratch/bench_noise_300.wav");
    if !inputs_ready(
        "synthetic-noise-100-300bd",
        "operator-provided, gitignored",
        &presence_of("scratch/bench_noise_300.wav", &path),
    ) {
        return;
    }
    let count = decode_count_with(&path, ModemProfile::HF_APRS_300)
        .expect("decoding the 300-baud synthetic-noise WAV");
    println!("synthetic-noise-100-300bd: {count} frames (required >= {SYNTHETIC_300_MIN_FRAMES})");
    assert!(
        count >= SYNTHETIC_300_MIN_FRAMES,
        "300-baud synthetic-noise regression: {count} frames, \
         required >= {SYNTHETIC_300_MIN_FRAMES}"
    );
}

/// Tier-1 cover for the presence decision itself.
///
/// The measuring tests above are `#[ignore]`d and need 579 MB of
/// operator-provided audio, so the logic that decides whether they
/// measure at all would otherwise be the least-tested code in the file —
/// which is how it came to pass vacuously in the first place. These run
/// on every `cargo test`, use fabricated path lists, touch no disk, and
/// never look at `corpus/`.
mod presence_rules {
    use super::{Availability, MIN_TRACKS_MEASURED, TRACKS, availability};

    #[test]
    fn every_input_present_measures() {
        assert_eq!(
            availability(&[("a.wav", true), ("b.wav", true), ("c.wav", true)]),
            Availability::AllPresent
        );
    }

    #[test]
    fn no_input_present_skips() {
        assert_eq!(
            availability(&[("a.wav", false), ("b.wav", false), ("c.wav", false)]),
            Availability::NonePresent
        );
    }

    #[test]
    fn a_partial_input_set_is_never_a_skip() {
        for inputs in [
            [("a.wav", true), ("b.wav", false)],
            [("a.wav", false), ("b.wav", true)],
        ] {
            match availability(&inputs) {
                Availability::Partial { present, missing } => {
                    assert_eq!(present.len(), 1, "{inputs:?}");
                    assert_eq!(missing.len(), 1, "{inputs:?}");
                }
                other => panic!("{inputs:?} must not read as {other:?}"),
            }
        }
    }

    /// Exhaustive over the real table: all 16 present/absent patterns of
    /// the four pinned tracks land in exactly the intended state, so no
    /// arrangement of a half-restored `corpus/` can measure a subset.
    #[test]
    fn the_pinned_track_table_is_three_state_for_every_pattern() {
        let names: Vec<&str> = TRACKS.iter().map(|track| track.file).collect();
        let mut seen = (0usize, 0usize, 0usize);
        for mask in 0u32..(1u32 << TRACKS.len()) {
            let inputs: Vec<(&str, bool)> = names
                .iter()
                .enumerate()
                .map(|(i, &name)| (name, (mask & (1 << i)) != 0))
                .collect();
            let present = mask.count_ones() as usize;
            match availability(&inputs) {
                Availability::AllPresent => {
                    assert_eq!(present, names.len(), "mask {mask:#06b}");
                    seen.0 += 1;
                }
                Availability::NonePresent => {
                    assert_eq!(present, 0, "mask {mask:#06b}");
                    seen.1 += 1;
                }
                Availability::Partial {
                    present: p,
                    missing: m,
                } => {
                    assert_eq!(p.len(), present, "mask {mask:#06b}");
                    assert_eq!(m.len(), names.len() - present, "mask {mask:#06b}");
                    assert!(present > 0 && present < names.len(), "mask {mask:#06b}");
                    seen.2 += 1;
                }
            }
        }
        assert_eq!(
            seen,
            (1, 1, 14),
            "of 16 patterns exactly one is all-present, one is none-present, \
             and the remaining 14 must fail rather than measure a subset"
        );
    }

    /// An empty required-input list must not read as "nothing to do".
    #[test]
    fn an_empty_input_list_is_not_a_clean_skip() {
        assert_eq!(availability(&[]), Availability::AllPresent);
    }

    /// Documents what [`MIN_TRACKS_MEASURED`] is out of, at runtime as
    /// well as in the `const` assertion beside it.
    #[test]
    fn the_measured_floor_covers_every_pinned_track() {
        assert_eq!(
            MIN_TRACKS_MEASURED,
            TRACKS.len(),
            "the floor must count every pinned track; raise it with the table"
        );
    }
}
