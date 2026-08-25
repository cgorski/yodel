//! WORKSTATION-TIER DIGIPEATER: the observability tier of the digi story.
//!
//! * **Scenario** — an APRS digipeater: hear a frame, decide whether to
//!   relay it, and log every decision. This is the *observability* tier
//!   — verbose tracing, dry-run by default — for understanding and
//!   tuning a relay policy before trusting it on the air.
//! * **Hardware** — a workstation or always-on server/Raspberry Pi. For
//!   the same logic on an MCU see
//!   `examples/esp32-riscv/src/digipeater.rs`.
//! * **Features** — `tnc,digipeat,wav`.
//!
//! # What a digipeater is, in one paragraph
//!
//! An APRS digipeater is a store-and-forward relay. It hears an AX.25
//! UI frame, looks at the frame's digipeater path (hops like
//! `WIDE2-1`), and — when the first unused hop is addressed to it —
//! retransmits the frame with that hop marked used (H bit set) so the
//! same frame is never relayed in circles. The relay rules live in ONE
//! tested place: the library's `digipeat` module (`relay_decision` +
//! `DupeRing`). This example forks NONE of that logic; it only wraps
//! the library core in std-tier observation, policy, and file I/O.
//!
//! # The two tiers, and why this one exists
//!
//! The same relay core drives two digipeaters in this repository:
//!
//! * `examples/esp32-riscv/src/digipeater.rs` — the EMBEDDED tier:
//!   no_std, alloc-free, runs on a dev board wired to a radio.
//! * this file — the WORKSTATION tier: everything std buys you.
//!   Structured `tracing` spans for EVERY decision, stats counters, a
//!   JSON-lines log, per-alias policy flags, and a dry-run default.
//!
//! Because both tiers share `relay_decision`, this is also your
//! DEBUGGING TOOL for the embedded digipeater — run it against a WAV
//! capture to understand what your ESP32 heard: every frame's dupe
//! check, path mutation (before → after), and relay/ignore reason is
//! traced, so a silent embedded relay becomes explainable on your
//! desk with no radio attached.
//!
//! # What this file does, start to finish
//!
//! 1. Parses simple `std::env` flags (see `usage()` below): input is a
//!    WAV path or `-` for raw 48 kHz 16-bit mono little-endian PCM on
//!    stdin; `--mycall`, `--wide-max`, `--no-wide` set the served
//!    alias policy; `--log` appends one JSON object per heard frame;
//!    `--transmit` leaves dry-run mode.
//! 2. Feeds every sample into a `TncReceiver` (Bell 202 demodulator →
//!    HDLC → FCS check → AX.25 parse), exactly like `decode_to_log.rs`.
//! 3. For every FCS-valid frame, inside one `tracing` span per frame:
//!    frame heard (`SRC>DEST`, path with `*` on used hops) →
//!    dupe-check verdict (`DupeRing`, sample-clock milliseconds) →
//!    path decision with the exact mutation (`before → after` hop
//!    lists) → relay or ignore with the library's typed reason.
//! 4. DRY-RUN is the DEFAULT: decisions are logged (`would relay`)
//!    but no output audio is produced — kind to licensing-cautious
//!    users who want to study traffic before keying anything. Pass
//!    `--transmit` to go "live", which here means writing the relay
//!    audio to `--out relay.wav` (WAV input) or to stdout as raw PCM
//!    (stdin input).
//! 5. At end of input it prints a self-report: uptime by sample clock
//!    and wall clock, all counters, and the top 5 talkers.
//!
//! Shutdown is graceful on END OF INPUT: a WAV file ends, or the
//! stdin pipe closes, and the self-report runs. There is no ctrl-c
//! handler — portable signal handling needs a dependency, and for a
//! corpus-replay tool end-of-input covers the real use (ctrl-c still
//! stops the process; you only lose the final report).
//!
//! # Try it
//!
//! ```sh
//! # Make a WAV carrying a WIDE2-2 request — two hops left, so the
//! # relay has something to decrement and can insert itself.
//! cargo run --features cli -- encode --out wide2.wav \
//!     --from N0CALL-7 --to APRS --path WIDE2-2 --sample-rate 48000 \
//!     position --lat 49.0583 --lon -72.0292 --symbol '/>'
//!
//! # Replay it in dry-run: nothing transmitted, every decision told.
//! cargo run --example digipeater_station --features tnc,digipeat,wav -- \
//!     wide2.wav --mycall N0CALL-1 --log digi.jsonl
//!
//! # Go "live": write the relay audio out as a WAV, then read it back
//! # and see the mutated path.
//! cargo run --example digipeater_station --features tnc,digipeat,wav -- \
//!     wide2.wav --mycall N0CALL-1 --transmit --out relay.wav
//! cargo run --example decode_wav --features tnc,wav -- relay.wav
//! #  -> N0CALL-7>APRS,N0CALL-1,WIDE2-1: position lat 49.0583 lon -72.0292
//! ```
//!
//! Use **WIDE2-2**, not the `WIDE1-1` that `encode_wav` emits: with one
//! hop left there is nothing to decrement, so the relay only sets the H
//! bit (`WIDE1-1*`) and the interesting half of the mutation never
//! shows. `--wide-max 1` and `--no-wide` both make the same frame be
//! ignored instead, which is the other half worth watching.
//!
//! All decision/formatting/stats logic below is PURE (no I/O), so the
//! host test suite (`tests/app_examples.rs`) `#[path]`-includes this
//! file and proves a full WAV-style round trip: heard `WIDE2-1` frame
//! (the last-hop case, consumed rather than decremented)
//! → relayed audio decodes with the correct mutation, duplicates are
//! suppressed, counters and JSON fields are exact, and dry-run
//! produces the log but no audio.

use std::io::{Read, Write};

use yodel::SampleRate;
use yodel::ax25::{Address, PathHop, UiFrame};
use yodel::digipeat::{
    Alias, DupeRing, ExactAliasAction, Freshness, IgnoreReason, RelayDecision, WideLimit,
    relay_decision,
};
use yodel::tnc::{DefaultTncReceiver, MAX_FRAME_BYTES, TncConfig, TncReceiver, TncTransmitter};

/// Sample rate assumed for raw PCM on stdin (WAV files carry their own).
const STDIN_RATE_HZ: u32 = 48_000;

/// Where to get input audio, printed when the file is missing or
/// unusable. A digipeater wants traffic with an unused WIDEn-N hop;
/// a plain beacon is heard and then correctly ignored, which is
/// undramatic.
const INPUT_HELP: &str = "\
input: a 16-bit mono integer PCM WAV, or `-` for raw 48 kHz s16le PCM
on stdin.

no file yet? make one with a hop left to relay:
  cargo run --features cli -- encode --out wide2.wav \\
      --from N0CALL-7 --to APRS --path WIDE2-2 --sample-rate 48000 \\
      position --lat 49.0583 --lon -72.0292 --symbol '/>'

real off-air traffic works too, converted to mono 16-bit:
  sox recording.wav -c 1 -b 16 -e signed-integer mono.wav";

/// Dupe-ring capacity: plenty for a single-channel monitor window.
const DUPE_SLOTS: usize = 64;

/// The station's relay policy: who we are and what we answer to.
#[derive(Debug, Clone, Copy)]
pub struct Policy {
    /// Our own callsign (exact alias + insertion callsign).
    pub my_call: Address,
    /// Served WIDEn-N ceiling; `None` = serve MYCALL only.
    pub wide_limit: Option<WideLimit>,
    /// `false` = dry-run (default): decide and log, produce no audio.
    pub transmit: bool,
}

impl Policy {
    /// The served alias table this policy expands to, as passed to the
    /// library's `relay_decision` (MYCALL exact + optional WIDEn-N).
    #[must_use]
    pub fn aliases(&self) -> Vec<Alias> {
        let mut aliases = vec![Alias::Exact(self.my_call)];
        if let Some(limit) = self.wide_limit {
            aliases.push(Alias::Wide(limit));
        }
        aliases
    }

    /// One human-readable row per served alias, for the startup banner.
    #[must_use]
    pub fn table(&self) -> Vec<String> {
        let mut rows = vec![format!(
            "{} (exact, H bit set on match)",
            fmt_addr(&self.my_call)
        )];
        if let Some(limit) = self.wide_limit {
            rows.push(format!(
                "WIDE1-x..WIDE{}-x (decrement + insert {})",
                limit.value(),
                fmt_addr(&self.my_call)
            ));
        }
        rows
    }
}

/// What the station decided about one heard frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Relay with these mutated hops (in dry-run: WOULD relay).
    Relay(Vec<PathHop>),
    /// Heard within the dupe window; suppressed.
    Duplicate,
    /// The library refused, with its typed reason.
    Ignore(IgnoreReason),
}

impl Verdict {
    /// The short decision label used in the JSON log and stats.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Verdict::Relay(_) => "relay",
            Verdict::Duplicate => "duplicate",
            Verdict::Ignore(_) => "ignore",
        }
    }

    /// The reason string used in the JSON log (empty for a relay).
    #[must_use]
    pub fn reason(&self) -> String {
        match self {
            Verdict::Relay(_) => String::new(),
            Verdict::Duplicate => "heard within dupe window".to_string(),
            Verdict::Ignore(reason) => ignore_label(*reason).to_string(),
        }
    }
}

/// A stable machine-readable label per library ignore reason.
#[must_use]
pub fn ignore_label(reason: IgnoreReason) -> &'static str {
    match reason {
        IgnoreReason::AllHopsUsed => "all-hops-used",
        IgnoreReason::NotForUs => "not-for-us",
        IgnoreReason::WideInvalid { .. } => "wide-invalid",
        IgnoreReason::WideAboveLimit { .. } => "wide-above-limit",
        IgnoreReason::PathFull => "path-full",
    }
}

/// The decision core: dupe check, then the LIBRARY's `relay_decision`
/// — no relay logic is forked here. Pure except for `tracing` events
/// (which are data, not I/O: without a subscriber they vanish), so the
/// host tests call this function directly.
///
/// `now_ms` is the sample-clock time in milliseconds (samples ÷ rate);
/// the library has no clock of its own.
pub fn consider(
    dupes: &mut DupeRing<DUPE_SLOTS>,
    policy: &Policy,
    src: Address,
    dest: Address,
    hops: &[PathHop],
    info: &[u8],
    now_ms: u64,
) -> Verdict {
    let fresh = dupes.check_and_insert(&src, &dest, info, now_ms);
    tracing::info!(result = %fresh, "dupe-check");
    if fresh == Freshness::Duplicate {
        return Verdict::Duplicate;
    }
    let before = format_path(hops);
    match relay_decision(
        hops,
        &policy.aliases(),
        policy.my_call,
        ExactAliasAction::Keep,
    ) {
        RelayDecision::Relay(path) => {
            let after = format_path(path.hops());
            tracing::info!(before = %before, after = %after, "path decision: mutate");
            Verdict::Relay(path.hops().to_vec())
        }
        RelayDecision::Ignore(reason) => {
            tracing::info!(before = %before, "path decision: no mutation");
            Verdict::Ignore(reason)
        }
    }
}

/// Saturation-free session counters (u64 on a host is effectively
/// unbounded for this tool's lifetimes).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stats {
    /// FCS-valid frames heard.
    pub heard: u64,
    /// Frames relayed (in dry-run: frames that WOULD have been).
    pub relayed: u64,
    /// Frames suppressed by the dupe window.
    pub duplicates: u64,
    /// Ignored frames, tallied per `ignore_label` reason.
    pub ignored: Vec<(String, u64)>,
    /// Frames heard per source station, insertion-ordered.
    pub per_source: Vec<(String, u64)>,
}

impl Stats {
    /// Total ignored frames across all reasons.
    #[must_use]
    pub fn ignored_total(&self) -> u64 {
        self.ignored.iter().map(|(_, n)| n).sum()
    }

    /// Records one verdict for one heard frame from `src`.
    pub fn record(&mut self, src: &str, verdict: &Verdict) {
        self.heard += 1;
        bump(&mut self.per_source, src);
        match verdict {
            Verdict::Relay(_) => self.relayed += 1,
            Verdict::Duplicate => self.duplicates += 1,
            Verdict::Ignore(reason) => bump(&mut self.ignored, ignore_label(*reason)),
        }
    }

    /// The busiest sources, descending, at most `n` (ties keep
    /// first-heard order — deterministic for tests).
    #[must_use]
    pub fn top_talkers(&self, n: usize) -> Vec<(String, u64)> {
        let mut sorted = self.per_source.clone();
        sorted.sort_by_key(|entry| std::cmp::Reverse(entry.1));
        sorted.truncate(n);
        sorted
    }
}

/// Increments `key` in an insertion-ordered tally list.
fn bump(tallies: &mut Vec<(String, u64)>, key: &str) {
    if let Some(entry) = tallies.iter_mut().find(|(k, _)| k == key) {
        entry.1 += 1;
    } else {
        tallies.push((key.to_string(), 1));
    }
}

/// Formats a hop list the monitor way: `HOP1*,HOP2` with a `*` on
/// every used (H bit set) hop. Pure; tests assert exact output.
#[must_use]
pub fn format_path(hops: &[PathHop]) -> String {
    let mut out = String::new();
    for (i, hop) in hops.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&fmt_addr(&hop.address));
        if hop.repeated {
            out.push('*');
        }
    }
    out
}

/// Formats an address as `CALL` or `CALL-SSID`.
#[must_use]
pub fn fmt_addr(addr: &Address) -> String {
    let call = String::from_utf8_lossy(addr.callsign.as_bytes()).into_owned();
    match addr.ssid.value() {
        0 => call,
        n => format!("{call}-{n}"),
    }
}

/// Minimal JSON string escaping (quotes, backslashes, control bytes).
/// The fields are plain strings and numbers, so this hand-rolled
/// escaper keeps the crate free of a serde dev-dependency.
#[must_use]
pub fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// One JSON-lines log record for one heard frame — pure, hand-rolled
/// (see `json_escape`), so tests assert the exact line.
///
/// `t_ms` is the sample-clock time in milliseconds; `path_after` is
/// empty unless the verdict was a relay. The decision/reason fields
/// are IDENTICAL in dry-run and live modes: dry-run changes only
/// whether audio is produced, never what is decided.
#[must_use]
pub fn json_line(t_ms: u64, src: &str, dst: &str, path_before: &str, verdict: &Verdict) -> String {
    let path_after = match verdict {
        Verdict::Relay(hops) => format_path(hops),
        _ => String::new(),
    };
    format!(
        "{{\"t_s\":{}.{:03},\"src\":\"{}\",\"dst\":\"{}\",\"path_before\":\"{}\",\"path_after\":\"{}\",\"decision\":\"{}\",\"reason\":\"{}\"}}",
        t_ms / 1000,
        t_ms % 1000,
        json_escape(src),
        json_escape(dst),
        json_escape(path_before),
        json_escape(&path_after),
        verdict.label(),
        json_escape(&verdict.reason()),
    )
}

/// The exit self-report, pure: uptime by sample clock (and wall clock
/// when known), counters, top 5 talkers.
#[must_use]
pub fn stats_report(stats: &Stats, sample_ms: u64, wall_secs: Option<u64>) -> String {
    let mut out = format!(
        "digipeater session report\n  uptime: {}.{:03}s sample clock",
        sample_ms / 1000,
        sample_ms % 1000
    );
    if let Some(w) = wall_secs {
        out.push_str(&format!(", {w}s wall clock"));
    }
    out.push_str(&format!(
        "\n  heard: {}  relayed: {}  duplicate: {}  ignored: {}",
        stats.heard,
        stats.relayed,
        stats.duplicates,
        stats.ignored_total()
    ));
    for (reason, n) in &stats.ignored {
        out.push_str(&format!("\n    ignored/{reason}: {n}"));
    }
    out.push_str("\n  top talkers:");
    if stats.per_source.is_empty() {
        out.push_str(" (none)");
    }
    for (call, n) in stats.top_talkers(5) {
        out.push_str(&format!("\n    {call}: {n} frame(s)"));
    }
    out
}

/// Everything the station reports about one heard frame: the JSON log
/// line, the tracing already emitted, and — when relaying live — the
/// transmit audio. Owned, so it outlives the receiver borrow.
#[derive(Debug, Clone)]
pub struct FrameReport {
    /// The JSON-lines record for `--log`.
    pub json: String,
    /// The relay transmission as PCM samples; EMPTY in dry-run or when
    /// the verdict was not a relay.
    pub tx_audio: Vec<i16>,
}

/// The whole station: receiver, dupe ring, policy, stats. Push samples
/// in; get a `FrameReport` whenever a frame completes. Pure with
/// respect to I/O (tracing events aside), so tests drive it directly —
/// this IS the code path `main` runs.
pub struct Station {
    rx: DefaultTncReceiver,
    tx: TncTransmitter,
    dupes: DupeRing<DUPE_SLOTS>,
    /// The relay policy in force.
    pub policy: Policy,
    /// Session counters, updated per heard frame.
    pub stats: Stats,
    rate_hz: u32,
    sample_pos: u64,
}

impl Station {
    /// A fresh station decoding at `rate_hz`.
    ///
    /// # Errors
    ///
    /// When `rate_hz` is not a valid Bell 202 sample rate.
    pub fn new(rate_hz: u32, policy: Policy) -> Result<Self, Box<dyn std::error::Error>> {
        let cfg = TncConfig::bell_202(SampleRate::new(rate_hz)?)?;
        Ok(Self {
            rx: TncReceiver::new(cfg)?,
            tx: TncTransmitter::new(cfg),
            dupes: DupeRing::new(),
            policy,
            stats: Stats::default(),
            rate_hz,
            sample_pos: 0,
        })
    }

    /// Sample-clock time in milliseconds at the current position.
    #[must_use]
    pub fn sample_ms(&self) -> u64 {
        self.sample_pos * 1000 / u64::from(self.rate_hz.max(1))
    }

    /// Pushes one PCM sample; when a frame completes, decides on it
    /// (tracing every step) and returns the report.
    pub fn push(&mut self, sample: i16) -> Option<FrameReport> {
        self.sample_pos += 1;
        let now_ms = self.sample_ms();
        let frame = self.rx.push_i16(sample)?;

        // Own the frame fields: the borrow of `rx` must end before we
        // rebuild the relay transmission.
        let ui = frame.ui_frame();
        let src = ui.src;
        let dest = ui.dest;
        let hops: Vec<PathHop> = ui.hops().collect();
        let info: Vec<u8> = ui.info.to_vec();

        let src_s = fmt_addr(&src);
        let dst_s = fmt_addr(&dest);
        let before = format_path(&hops);

        // One span per frame: everything below hangs off it.
        let span = tracing::info_span!("frame", src = %src_s, dst = %dst_s, t_ms = now_ms);
        let _guard = span.enter();
        tracing::info!(path = %before, "heard");

        let verdict = consider(
            &mut self.dupes,
            &self.policy,
            src,
            dest,
            &hops,
            &info,
            now_ms,
        );
        match &verdict {
            Verdict::Relay(_) if self.policy.transmit => tracing::info!("relay"),
            Verdict::Relay(_) => tracing::info!("relay (dry-run: no audio produced)"),
            Verdict::Duplicate => {
                tracing::info!(reason = "heard within dupe window", "ignore");
            }
            Verdict::Ignore(reason) => tracing::info!(reason = %reason, "ignore"),
        }

        self.stats.record(&src_s, &verdict);
        let json = json_line(now_ms, &src_s, &dst_s, &before, &verdict);

        // Dry-run (the default) stops HERE: the decision above is
        // identical, only the audio is withheld.
        let tx_audio = match &verdict {
            Verdict::Relay(new_hops) if self.policy.transmit => {
                self.render_relay(dest, src, new_hops, &info)
            }
            _ => Vec::new(),
        };
        Some(FrameReport { json, tx_audio })
    }

    /// Rebuilds the frame with the mutated hops and modulates it.
    fn render_relay(&self, dest: Address, src: Address, hops: &[PathHop], info: &[u8]) -> Vec<i16> {
        let mut buf = [0u8; MAX_FRAME_BYTES];
        let Ok(frame) = UiFrame::with_hops(dest, src, hops, info) else {
            return Vec::new(); // unreachable: hops came from relay_decision
        };
        let Ok(len) = frame.build(&mut buf) else {
            return Vec::new(); // unreachable: same size class as the heard frame
        };
        self.tx.frame_samples_i16(&buf[..len]).collect()
    }
}

/// Parsed command line for `main` — separate from `Policy` so the
/// policy stays a pure value tests can build directly.
#[derive(Debug, Clone)]
pub struct CliArgs {
    /// Input WAV path, or `-` for raw PCM on stdin.
    pub input: String,
    /// Relay-audio WAV path (WAV input + `--transmit` only).
    pub out: Option<String>,
    /// JSON-lines log path.
    pub log: Option<String>,
    /// The relay policy the flags expand to.
    pub policy: Policy,
}

/// Usage text (kept as a function so the parser stays pure).
#[must_use]
pub fn usage() -> String {
    "usage: digipeater_station <input.wav | -> [--mycall CALL[-SSID]] \
     [--wide-max N] [--no-wide] [--log file.jsonl] [--transmit] [--out relay.wav]\n\
     '-' reads 48 kHz 16-bit mono LE PCM from stdin; with --transmit the relay\n\
     audio goes to --out (WAV input) or stdout as raw PCM (stdin input).\n\
     Default policy: MYCALL N0CALL-1 exact + WIDE1/WIDE2 (max-n 2), DRY-RUN."
        .to_string()
}

/// Parses argv (after the program name) into `CliArgs` — pure, so the
/// per-alias policy flags are host-testable.
///
/// # Errors
///
/// A human-readable message for unknown flags, bad values, or a
/// missing input path.
pub fn parse_args(args: &[String]) -> Result<CliArgs, String> {
    let mut input = None;
    let mut out = None;
    let mut log = None;
    let mut my_call = Address::new(b"N0CALL", 1).map_err(|e| e.to_string())?;
    let mut wide_limit = Some(WideLimit::TWO);
    let mut transmit = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--mycall" => {
                let v = it.next().ok_or("--mycall needs a value")?;
                my_call = parse_callsign(v)?;
            }
            "--wide-max" => {
                let v = it.next().ok_or("--wide-max needs a value")?;
                let n: u8 = v.parse().map_err(|_| format!("bad --wide-max {v}"))?;
                wide_limit = Some(WideLimit::new(n).map_err(|e| e.to_string())?);
            }
            "--no-wide" => wide_limit = None,
            "--log" => log = Some(it.next().ok_or("--log needs a path")?.clone()),
            "--out" => out = Some(it.next().ok_or("--out needs a path")?.clone()),
            "--transmit" => transmit = true,
            other if input.is_none() && !other.starts_with("--") => {
                input = Some(other.to_string());
            }
            other => return Err(format!("unknown argument {other}\n{}", usage())),
        }
    }
    Ok(CliArgs {
        input: input.ok_or_else(usage)?,
        out,
        log,
        policy: Policy {
            my_call,
            wide_limit,
            transmit,
        },
    })
}

/// Parses `CALL` or `CALL-SSID` into a typed address — pure.
///
/// # Errors
///
/// A message when the callsign or SSID is invalid.
pub fn parse_callsign(s: &str) -> Result<Address, String> {
    let (call, ssid) = match s.split_once('-') {
        Some((c, n)) => (c, n.parse::<u8>().map_err(|_| format!("bad SSID in {s}"))?),
        None => (s, 0),
    };
    Address::new(call.as_bytes(), ssid).map_err(|e| e.to_string())
}

fn main() {
    // Display, not Debug: returning `Result` from `main` escapes the
    // newlines in the usage text onto one unreadable line.
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{}", usage());
        return Ok(());
    }
    let cli = parse_args(&args)?;

    // What std buys here: structured tracing of every decision. Spans
    // and events land on stderr so stdout stays clean for PCM output.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();

    tracing::info!(
        mode = if cli.policy.transmit {
            "TRANSMIT"
        } else {
            "DRY-RUN"
        },
        "digipeater starting"
    );
    for row in cli.policy.table() {
        tracing::info!(alias = %row, "serving");
    }

    let wall_start = std::time::Instant::now();
    let mut log_file = match &cli.log {
        Some(path) => Some(std::fs::File::create(path)?),
        None => None,
    };

    let mut wav_out: Vec<i16> = Vec::new();
    let station_rate;

    if cli.input == "-" {
        // Raw PCM from stdin; relay audio (if any) to stdout.
        station_rate = STDIN_RATE_HZ;
        let mut station = Station::new(STDIN_RATE_HZ, cli.policy)?;
        let stdin = std::io::stdin();
        let mut lock = stdin.lock();
        let stdout = std::io::stdout();
        let mut out_lock = stdout.lock();
        let mut bytes = [0u8; 2];
        loop {
            match lock.read_exact(&mut bytes) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e.into()),
            }
            if let Some(report) = station.push(i16::from_le_bytes(bytes)) {
                if let Some(f) = log_file.as_mut() {
                    writeln!(f, "{}", report.json)?;
                }
                for s in report.tx_audio {
                    out_lock.write_all(&s.to_le_bytes())?;
                }
            }
        }
        finish(&station, station_rate, wall_start);
    } else {
        let path = &cli.input;
        let mut reader = hound::WavReader::open(path).map_err(|e| match e {
            hound::Error::IoError(io) if io.kind() == std::io::ErrorKind::NotFound => {
                format!("cannot open {path}: no such file\n\n{INPUT_HELP}")
            }
            hound::Error::FormatError(_) => {
                format!("{path} is not a WAV file ({e})\n\n{INPUT_HELP}")
            }
            other => format!("cannot open {path}: {other}"),
        })?;
        let spec = reader.spec();
        if spec.channels != 1
            || spec.bits_per_sample != 16
            || spec.sample_format != hound::SampleFormat::Int
        {
            return Err(format!(
                "{path} is {}-channel {}-bit at {} Hz; need 1-channel 16-bit integer\n\n{INPUT_HELP}",
                spec.channels, spec.bits_per_sample, spec.sample_rate
            )
            .into());
        }
        station_rate = spec.sample_rate;
        let mut station = Station::new(spec.sample_rate, cli.policy)?;
        for sample in reader.samples::<i16>() {
            if let Some(report) = station.push(sample?) {
                if let Some(f) = log_file.as_mut() {
                    writeln!(f, "{}", report.json)?;
                }
                wav_out.extend(report.tx_audio);
            }
        }
        // Live mode writes the relay WAV; dry-run writes NOTHING.
        if cli.policy.transmit && !wav_out.is_empty() {
            let out_path = cli.out.as_deref().unwrap_or("relay.wav");
            let mut writer = hound::WavWriter::create(out_path, spec)?;
            for s in &wav_out {
                writer.write_sample(*s)?;
            }
            writer.finalize()?;
            tracing::info!(
                path = out_path,
                samples = wav_out.len(),
                "relay audio written"
            );
        }
        finish(&station, station_rate, wall_start);
    }
    Ok(())
}

/// Prints the exit self-report (graceful end-of-input shutdown).
fn finish(station: &Station, _rate_hz: u32, wall_start: std::time::Instant) {
    eprintln!(
        "{}",
        stats_report(
            &station.stats,
            station.sample_ms(),
            Some(wall_start.elapsed().as_secs()),
        )
    );
}
