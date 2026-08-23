//! `warble level`: a live input meter for setting a radio's volume.
//!
//! # Why this exists
//!
//! Setting receive volume for packet is a closed loop done blind. Too
//! quiet and the demodulator has nothing to work with; too loud and the
//! sound card clips, which destroys the tone ratio the discriminator
//! measures. The window between those is wide, but a radio's volume
//! control is coarse near the bottom of its travel and an operator
//! gets no feedback at all from the radio itself.
//!
//! # Two things a single number hides
//!
//! **Clipping.** RMS cannot see it and peak saturates at 100% whether
//! one sample is pinned or ten thousand are. MEASURED while setting up
//! a real interface: a capture read -0.8 dBFS and looked no worse than loud;
//! it was 23% clipped and nothing in it could decode. [`Level`]
//! therefore reports the clipped-sample count beside the two levels,
//! and any clipping at all outranks every other verdict.
//!
//! **Squelch state.** With the squelch CLOSED the audio is silent
//! between signals and a carrier makes it *louder*. With it OPEN there
//! is a constant hiss and a carrier makes it *quieter*, because FM
//! quieting suppresses the noise. A burst detector written for one is
//! exactly backwards for the other. Packet wants the squelch OPEN: it
//! takes tens of milliseconds to lift, which eats a frame's opening
//! flags and turns a decodable packet into an FCS error. So the meter
//! reports which state it is looking at, as advice beside the number.

use std::io::{IsTerminal, Write};

use clap::Args;

use crate::shared::{ModemArgs, sniff_stdin_samples};

/// Any sample at or beyond this magnitude counts as clipped. Not
/// `i16::MAX`: a converter that saturates often lands a count or two
/// short, and treating 32 700 as pinned costs nothing.
const CLIP_THRESHOLD: i16 = 32_700;

/// Bell 202's mark and space tones, the two bins worth watching while
/// a level is being set.
const MARK_HZ: f32 = 1200.0;
const SPACE_HZ: f32 = 2200.0;

/// How a window of audio reads on the meter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Level {
    /// Root-mean-square level in dBFS. `-inf` for pure silence.
    pub rms_dbfs: f32,
    /// Largest absolute sample, as a fraction of full scale.
    pub peak_frac: f32,
    /// Samples at or beyond [`CLIP_THRESHOLD`].
    pub clipped: u32,
    /// The one-word summary.
    pub verdict: Verdict,
}

/// The summary of a window, worst-first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing arriving: squelch closed with no signal, volume at
    /// zero, or the wrong input selected.
    Muted,
    /// Far below what the demodulator wants.
    TooQuiet,
    /// Usable but with little margin.
    Low,
    /// The target.
    Good,
    /// Loud enough that a stronger signal would clip.
    Hot,
    /// Samples are pinned at full scale. Outranks everything else,
    /// because the tone ratio the discriminator measures is already
    /// destroyed whatever the RMS says.
    Clipping,
}

impl Verdict {
    /// The word to print.
    pub const fn label(self) -> &'static str {
        match self {
            Verdict::Muted => "MUTED",
            Verdict::TooQuiet => "TOO QUIET",
            Verdict::Low => "LOW",
            Verdict::Good => "GOOD",
            Verdict::Hot => "HOT",
            Verdict::Clipping => "CLIPPING",
        }
    }

    /// What to do about it.
    pub const fn advice(self) -> &'static str {
        match self {
            Verdict::Muted => "nothing arriving: squelch closed, volume at zero, or wrong input",
            Verdict::TooQuiet => "turn the volume UP",
            Verdict::Low => "turn the volume up slightly",
            Verdict::Good => "leave it here",
            Verdict::Hot => "turn the volume down slightly",
            Verdict::Clipping => "turn the volume DOWN",
        }
    }

    /// Whether this is the level to stop at, for `--until-good`.
    pub const fn is_good(self) -> bool {
        matches!(self, Verdict::Good)
    }
}

/// Measures one window of audio.
///
/// Pure: no I/O and no state, so every boundary below is pinned by a
/// test over synthesised buffers rather than needing a sound card.
///
/// The thresholds are judgement rather than physics, which is exactly
/// why they are written down in one place and asserted. `Good` spans
/// -28 to -12 dBFS: the demodulator normalises, so it tolerates far
/// more than this, and the band is narrowed on purpose to leave
/// headroom for a signal stronger than whatever is being used to set
/// the level.
#[must_use]
pub fn measure(window: &[i16]) -> Level {
    if window.is_empty() {
        return Level {
            rms_dbfs: f32::NEG_INFINITY,
            peak_frac: 0.0,
            clipped: 0,
            verdict: Verdict::Muted,
        };
    }
    let mut sum = 0.0f64;
    let mut peak = 0i32;
    let mut clipped = 0u32;
    for &s in window {
        let v = f64::from(s);
        sum += v * v;
        let a = i32::from(s).abs();
        if a > peak {
            peak = a;
        }
        if a >= i32::from(CLIP_THRESHOLD) {
            clipped += 1;
        }
    }
    #[allow(clippy::cast_possible_truncation)]
    let rms = (sum / window.len() as f64).sqrt() as f32;
    let full = f32::from(i16::MAX);
    let rms_dbfs = if rms <= 0.0 {
        f32::NEG_INFINITY
    } else {
        20.0 * (rms / full).log10()
    };
    #[allow(clippy::cast_precision_loss)]
    let peak_frac = peak as f32 / full;

    // Clipping first: it outranks the level, because a pinned sample
    // has already destroyed the tone ratio whatever the RMS reads.
    let verdict = if clipped > 0 {
        Verdict::Clipping
    } else if rms_dbfs < -60.0 {
        Verdict::Muted
    } else if rms_dbfs < -35.0 {
        Verdict::TooQuiet
    } else if rms_dbfs < -28.0 {
        Verdict::Low
    } else if rms_dbfs <= -12.0 {
        Verdict::Good
    } else {
        Verdict::Hot
    };
    Level {
        rms_dbfs,
        peak_frac,
        clipped,
        verdict,
    }
}

/// Whether the radio's squelch appears to be open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Squelch {
    /// Not enough windows yet to say.
    Unknown,
    /// A continuous noise floor: what packet wants.
    Open,
    /// Silence between signals. The squelch's opening delay eats a
    /// frame's leading flags, so packets arrive as FCS errors.
    Closed,
}

impl Squelch {
    /// The word to print.
    pub const fn label(self) -> &'static str {
        match self {
            Squelch::Unknown => "squelch ?",
            Squelch::Open => "squelch OPEN",
            Squelch::Closed => "squelch CLOSED",
        }
    }
}

/// Infers squelch state from the quietest level seen so far.
///
/// A closed squelch mutes hard, so the floor sits near digital
/// silence. An open one leaves the receiver's hiss, which is far above
/// that even on a quiet channel. Five windows, a second at the default
/// size, is enough for the floor to mean something.
#[must_use]
pub fn squelch_from_floor(floor_dbfs: f32, windows: u32) -> Squelch {
    if windows < 5 {
        Squelch::Unknown
    } else if floor_dbfs < -70.0 {
        Squelch::Closed
    } else {
        Squelch::Open
    }
}

/// Energy at the Bell 202 mark and space tones, normalised to full
/// scale.
///
/// Two Goertzel bins rather than a transform: it costs one multiply
/// and two adds per sample per tone, allocates nothing, and answers
/// the only question being asked while a level is set, which is
/// whether the thing arriving is a packet or hiss.
#[must_use]
pub fn tone_energy(window: &[i16], rate: u32) -> (f32, f32) {
    let one = |hz: f32| -> f32 {
        if window.is_empty() || rate == 0 {
            return 0.0;
        }
        #[allow(clippy::cast_precision_loss)]
        let k = 2.0 * core::f32::consts::PI * hz / rate as f32;
        let coeff = 2.0 * k.cos();
        let (mut s1, mut s2) = (0.0f32, 0.0f32);
        for &x in window {
            let s0 = f32::from(x) + coeff * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        #[allow(clippy::cast_precision_loss)]
        let n = window.len() as f32;
        let mag = (s1 * s1 + s2 * s2 - coeff * s1 * s2).max(0.0).sqrt();
        mag / n / f32::from(i16::MAX)
    };
    (one(MARK_HZ), one(SPACE_HZ))
}

/// Arguments of `warble level`.
#[derive(Args)]
pub struct LevelArgs {
    /// Input: `-` to read audio from stdin, WAV or raw s16le PCM. The
    /// same stdin shape every other subcommand takes, so the usual
    /// capture pipe works unchanged.
    #[arg(value_name = "-")]
    input: String,

    /// Sample rate of raw PCM on stdin in Hz [range: 8000..=48000].
    /// Required for raw stdin input; WAV carries its own.
    #[arg(long, value_name = "HZ", alias = "rate")]
    sample_rate: Option<u32>,

    /// Measurement window in milliseconds.
    #[arg(long, value_name = "MS", default_value_t = 200)]
    window: u64,

    /// Meter for this many seconds, then stop.
    #[arg(long, value_name = "SECS")]
    r#for: Option<u64>,

    /// Stop once the level has been in range **continuously** for this
    /// many seconds.
    ///
    /// Usually what you want: turn the knob, let it settle, and the
    /// command finishes by itself. Unlike waiting for a keypress it
    /// behaves the same way in a script, so nothing has to detect
    /// whether a terminal is attached.
    #[arg(long, value_name = "SECS")]
    until_good: Option<u64>,

    /// Keep metering and decode the same stream, printing frames on
    /// stdout underneath the meter.
    #[arg(long)]
    then_decode: bool,

    /// Modem preset and per-field overrides, as `warble decode` takes
    /// them. Only consulted with `--then-decode`.
    #[command(flatten)]
    modem: ModemArgs,
}

/// Runs `warble level`.
///
/// # Errors
///
/// No terminating condition, an input that is not `-`, a bad sample
/// rate, an I/O failure, or `--until-good` never being reached inside
/// `--for`.
pub fn level(args: &LevelArgs) -> Result<(), String> {
    if args.input != "-" {
        return Err(format!(
            "level reads audio from stdin: pass '-', not '{}'. Pipe a capture \
             tool into it, e.g. `... | warble level --rate 44100 -`",
            args.input
        ));
    }
    if args.r#for.is_none() && args.until_good.is_none() && !args.then_decode {
        return Err(
            "no terminating condition: give --for <SECS>, --until-good <SECS>, \
                    or --then-decode"
                .to_string(),
        );
    }
    if args.window == 0 {
        return Err("--window must be at least 1 ms".to_string());
    }

    let (rate, samples) = sniff_stdin_samples(std::io::stdin(), args.sample_rate)?;
    if args.then_decode {
        return meter_and_decode(args, rate, samples);
    }
    #[allow(clippy::cast_possible_truncation)]
    let per_window = ((u64::from(rate.hz()) * args.window) / 1000).max(1) as usize;

    // Both bounds are measured in AUDIO time, not wall clock: windows
    // seen times the window length. For a live capture the two are the
    // same, but a file or a fast pipe delivers minutes of audio in
    // milliseconds, and a wall clock would then never reach a hold it
    // has already heard. Counting windows is also deterministic, which
    // is what lets the exit conditions be tested without a sound card.
    let limit_windows = args.r#for.map(|s| (s * 1000).div_ceil(args.window).max(1));
    let hold_windows = args
        .until_good
        .map(|s| (s * 1000).div_ceil(args.window).max(1));
    let redraw = std::io::stderr().is_terminal();

    let mut buf: Vec<i16> = Vec::with_capacity(per_window);
    let mut floor = f32::INFINITY;
    let mut windows = 0u32;
    let mut good_run = 0u64;
    let mut last = Level {
        rms_dbfs: f32::NEG_INFINITY,
        peak_frac: 0.0,
        clipped: 0,
        verdict: Verdict::Muted,
    };

    for sample in samples {
        buf.push(sample?);
        if buf.len() < per_window {
            continue;
        }
        let level = measure(&buf);
        let (mark, space) = tone_energy(&buf, rate.hz());
        buf.clear();
        windows += 1;
        if level.rms_dbfs.is_finite() && level.rms_dbfs < floor {
            floor = level.rms_dbfs;
        }
        last = level;
        let sq = squelch_from_floor(floor, windows);
        render(&mut std::io::stderr(), level, sq, mark, space, redraw);

        if let Some(hold) = hold_windows {
            good_run = if level.verdict.is_good() {
                good_run + 1
            } else {
                0
            };
            if good_run >= hold {
                finish(redraw);
                eprintln!(
                    "level held in range for {} ms of audio: {:.1} dBFS, peak {:.0}%",
                    good_run * args.window,
                    level.rms_dbfs,
                    level.peak_frac * 100.0
                );
                return Ok(());
            }
        }
        if limit_windows.is_some_and(|n| u64::from(windows) >= n) {
            break;
        }
    }
    finish(redraw);

    if hold_windows.is_some() {
        return Err(format!(
            "level never held in range: last reading {:.1} dBFS ({}), {}",
            last.rms_dbfs,
            last.verdict.label(),
            last.verdict.advice()
        ));
    }
    eprintln!("{}: {}", last.verdict.label(), last.verdict.advice());
    Ok(())
}

/// Writes one meter reading.
///
/// Redraws in place on a terminal and prints one line per window
/// otherwise, so piping the meter to a file yields a log rather than a
/// smear of escape codes.
fn render<W: Write>(out: &mut W, l: Level, sq: Squelch, mark: f32, space: f32, redraw: bool) {
    // A 24-cell scale from -60 to 0 dBFS, with the target band marked.
    let cells = 24usize;
    let pos = if l.rms_dbfs.is_finite() {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let p = (((l.rms_dbfs + 60.0) / 60.0).clamp(0.0, 1.0) * cells as f32) as usize;
        p.min(cells)
    } else {
        0
    };
    let lo = cells * 32 / 60; // -28 dBFS
    let hi = cells * 48 / 60; // -12 dBFS
    let mut bar = String::with_capacity(cells + 2);
    for i in 0..cells {
        bar.push(if i < pos {
            '='
        } else if i == lo || i == hi {
            '|'
        } else {
            '.'
        });
    }
    let tone = if mark.max(space) > 0.01 { "##" } else { "--" };
    let line = format!(
        "rms {:6.1} dBFS  peak {:3.0}%  clip {:<5} [{bar}]  {:<9} {:<14} 1200/2200 {tone}",
        l.rms_dbfs,
        l.peak_frac * 100.0,
        l.clipped,
        l.verdict.label(),
        sq.label(),
    );
    if redraw {
        let _ = write!(out, "\r\x1b[2K{line}");
    } else {
        let _ = writeln!(out, "{line}");
    }
    let _ = out.flush();
}

/// Ends the in-place redraw so following output starts on a fresh line.
fn finish(redraw: bool) {
    if redraw {
        let _ = writeln!(std::io::stderr());
    }
}

/// `--then-decode`: meter every window on stderr while the same
/// samples go to the decoder, frames on stdout.
///
/// The two cannot be separate processes without splitting the stream,
/// and the whole point of the mode is to watch the level of the audio
/// that is being decoded rather than of a second capture.
fn meter_and_decode(
    args: &LevelArgs,
    rate: warble::SampleRate,
    samples: Box<dyn Iterator<Item = Result<i16, String>> + Send>,
) -> Result<(), String> {
    #[allow(clippy::cast_possible_truncation)]
    let per_window = ((u64::from(rate.hz()) * args.window) / 1000).max(1) as usize;
    let redraw = std::io::stderr().is_terminal();
    let mut buf: Vec<i16> = Vec::with_capacity(per_window);
    let mut floor = f32::INFINITY;
    let mut windows = 0u32;

    // Tee: measure a window, then pass it on. `inspect` would run per
    // sample; a window is the unit both halves want.
    let metered = samples.map(move |s| {
        let s = s?;
        buf.push(s);
        if buf.len() >= per_window {
            let level = measure(&buf);
            let (mark, space) = tone_energy(&buf, rate.hz());
            buf.clear();
            windows += 1;
            if level.rms_dbfs.is_finite() && level.rms_dbfs < floor {
                floor = level.rms_dbfs;
            }
            render(
                &mut std::io::stderr(),
                level,
                squelch_from_floor(floor, windows),
                mark,
                space,
                redraw,
            );
        }
        Ok(s)
    });
    let result = crate::decode::decode_metered(&args.modem, rate, metered);
    finish(redraw);
    result
}
