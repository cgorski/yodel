//! `warble aprsis`: read the live APRS-IS feed and write TNC2 lines.
//!
//! APRS-IS is the internet side of APRS. Igates around the world hear
//! packets on the radio and forward them to a server network, which
//! streams them back out as TNC2 monitor text. That is the same format
//! [`crate::decode`] reads with `--tnc2`, so the two compose:
//!
//! ```text
//! warble aprsis --callsign N0CALL --full-feed --seconds 300 --out capture.txt
//! warble decode --tnc2 --verify-rebuild capture.txt
//!
//! # or as one pipeline, with no file in between
//! warble aprsis --callsign N0CALL --full-feed --seconds 60 | \
//!     warble decode --tnc2 --output-format jsonl -
//! ```
//!
//! # Receive-only by construction
//!
//! The login passcode is the constant [`RECEIVE_ONLY`], `-1`, which
//! every server treats as unverified: such a client may receive and may
//! not send. There is no flag to change it and no code path here that
//! writes anything to the socket except the login line. That is not
//! caution for its own sake. Every packet injected into APRS-IS must be
//! assumed to reach the air, so injecting requires a licensed amateur
//! callsign and a real passcode, and a capture tool has no business
//! holding either.
//!
//! The callsign is required rather than defaulted. It is an identifier
//! that server operators can see and act on, servers refuse the
//! placeholder `N0CALL` outright, and a shared volunteer network is not
//! a place to connect anonymously.
//!
//! # The two feeds are different, and the flags do not mix
//!
//! This trips people up, so the subcommand refuses the ambiguous
//! combinations rather than connecting and delivering nothing.
//!
//! | | port | what it sends |
//! |---|---|---|
//! | filtered | 14580 | **nothing at all** until a filter subscribes you |
//! | full feed | 10152 | everything, and filters are ignored |
//!
//! A `--filter` on the full feed is silently useless, and no filter on
//! 14580 gives a connection that sits there producing keepalives. Both
//! look like a broken program.
//!
//! # This is a shared, volunteer-run network
//!
//! Prefer `--filter` to `--full-feed`, keep one connection rather than
//! several (parallel connections create duplicate loops that make
//! stations jump around on other people's maps), and bound the run with
//! `--seconds` or `--count`. Reconnects use exponential backoff with a
//! fresh DNS lookup, because the rotate addresses load-balance across
//! many different operators' machines and a tight retry loop hammers
//! all of them.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use clap::Args;

use warble::aprs::monitor::LINE_MAX;

/// Receive-only passcode. Anything else requires a licensed callsign,
/// and this tool has no reason to hold one.
const RECEIVE_ONLY: &str = "-1";

/// Tier 2 rotate address, filtered port. Resolves to a different server
/// each time, which is why it is re-resolved on every connection.
const DEFAULT_SERVER: &str = "rotate.aprs2.net:14580";

/// Unfiltered feed. Sample it briefly rather than sitting on it.
const FULL_FEED_SERVER: &str = "rotate.aprs2.net:10152";

/// Read timeout. A server keepalive arrives about every 20 s, so well
/// past that with nothing at all means the connection is dead rather
/// than quiet.
const READ_TIMEOUT: Duration = Duration::from_secs(90);

/// First reconnect delay, doubled on each consecutive failure.
const BACKOFF_START: Duration = Duration::from_secs(5);

/// Cap on the reconnect delay.
const BACKOFF_MAX: Duration = Duration::from_secs(300);

#[derive(Args)]
pub struct AprsIsArgs {
    /// Your callsign, sent as the login identifier. Required: servers
    /// refuse `N0CALL`, and operators can see and act on this.
    #[arg(long)]
    callsign: String,

    /// APRS-IS filter to subscribe to, e.g. `r/39.1/-94.6/250` for a
    /// 250 km radius, `b/N0CALL*` for one station, or `t/poimqstn` by
    /// packet type. Implies the filtered port.
    #[arg(long, conflicts_with = "full_feed")]
    filter: Option<String>,

    /// Take the unfiltered feed: every packet the server sees. Filters
    /// do not apply to it. Bound the run and do not sit on it.
    #[arg(long)]
    full_feed: bool,

    /// Server as `HOST:PORT` [default: rotate.aprs2.net:14580, or
    /// :10152 with --full-feed].
    #[arg(long)]
    server: Option<String>,

    /// Stop after this many seconds.
    #[arg(long, value_name = "SECS")]
    seconds: Option<u64>,

    /// Stop after this many packet lines.
    #[arg(long, value_name = "N")]
    count: Option<u64>,

    /// Write to a file instead of stdout.
    #[arg(long, value_name = "PATH")]
    out: Option<String>,

    /// Keep the server's `#` comment lines (greeting, login response,
    /// keepalives). Dropped by default, because they are not packets
    /// and a downstream TNC2 reader would count them as junk.
    #[arg(long)]
    keep_comments: bool,

    /// Do not reconnect when the server drops the connection.
    #[arg(long)]
    no_reconnect: bool,
}

/// Why a session ended.
enum Done {
    /// A bound was reached, so the run is over.
    Finished,
    /// The server went away and the run may continue.
    Disconnected,
}

/// Running totals, reported to stderr at the end.
#[derive(Default)]
struct Stats {
    lines: u64,
    server_lines: u64,
    sessions: u64,
}

pub fn aprsis(args: &AprsIsArgs) -> Result<(), String> {
    if args.callsign.trim().is_empty() {
        return Err("--callsign must not be empty".to_string());
    }
    // Refuse the two combinations that connect and then deliver
    // nothing, rather than letting them look like a broken program.
    if args.filter.is_none() && !args.full_feed && args.server.is_none() {
        return Err(
            "port 14580 sends nothing until a filter subscribes you: pass --filter \
             (for example --filter r/39.1/-94.6/250), or --full-feed for the \
             unfiltered feed"
                .to_string(),
        );
    }
    if args.seconds.is_none() && args.count.is_none() {
        eprintln!(
            "warble aprsis: no --seconds or --count, so this runs until interrupted; \
             one bounded connection is the courteous shape"
        );
    }

    let server = match (&args.server, args.full_feed) {
        (Some(s), _) => s.clone(),
        (None, true) => FULL_FEED_SERVER.to_string(),
        (None, false) => DEFAULT_SERVER.to_string(),
    };

    // Open the sink before connecting, so a bad path fails before the
    // tool takes a slot on someone's server.
    let mut sink: Box<dyn Write> = match &args.out {
        Some(path) => Box::new(std::io::BufWriter::new(
            std::fs::File::create(path).map_err(|e| format!("{path}: {e}"))?,
        )),
        None => Box::new(std::io::BufWriter::new(std::io::stdout())),
    };

    let mut stats = Stats::default();
    let started = Instant::now();
    let mut backoff = BACKOFF_START;
    let mut attempt = 0usize;

    loop {
        let before = stats.lines;
        attempt += 1;
        stats.sessions += 1;
        match run_session(args, &server, &mut sink, &mut stats, started, attempt) {
            Ok(Done::Finished) => break,
            Ok(Done::Disconnected) => eprintln!("server closed the connection"),
            Err(e) => eprintln!("connection failed: {e}"),
        }
        if args.no_reconnect || reached_bound(args, &stats, started) {
            break;
        }
        // A session that delivered traffic was healthy, so the next
        // backoff starts from scratch. Without this, a long run that
        // reconnects occasionally creeps up to the cap and stays there.
        if stats.lines > before {
            backoff = BACKOFF_START;
        }
        eprintln!("reconnecting in {:.0}s", backoff.as_secs_f32());
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }

    sink.flush().map_err(|e| format!("writing output: {e}"))?;
    let elapsed = started.elapsed().as_secs_f64().max(f64::MIN_POSITIVE);
    eprintln!(
        "{} packets in {:.1} s ({:.1}/s), {} server lines, {} session(s)",
        stats.lines,
        elapsed,
        stats.lines as f64 / elapsed,
        stats.server_lines,
        stats.sessions,
    );
    Ok(())
}

/// Whether a `--seconds` or `--count` bound has been reached.
fn reached_bound(args: &AprsIsArgs, stats: &Stats, started: Instant) -> bool {
    if let Some(limit) = args.seconds
        && started.elapsed() >= Duration::from_secs(limit)
    {
        return true;
    }
    if let Some(limit) = args.count
        && stats.lines >= limit
    {
        return true;
    }
    false
}

fn run_session(
    args: &AprsIsArgs,
    server: &str,
    sink: &mut dyn Write,
    stats: &mut Stats,
    started: Instant,
    attempt: usize,
) -> std::io::Result<Done> {
    // Re-resolve every time so the load balancer can do its job, and
    // step through the addresses so a retry loop does not pin itself to
    // whichever server just failed.
    let addrs: Vec<_> = server.to_socket_addrs()?.collect();
    let addr = *addrs
        .get(attempt % addrs.len().max(1))
        .ok_or_else(|| std::io::Error::other(format!("no address for {server}")))?;
    let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(15))?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    eprintln!("connected to {addr}");

    let mut out = stream.try_clone()?;
    let version = env!("CARGO_PKG_VERSION");
    let login = match &args.filter {
        Some(filter) => format!(
            "user {} pass {RECEIVE_ONLY} vers warble {version} filter {filter}\r\n",
            args.callsign,
        ),
        None => format!(
            "user {} pass {RECEIVE_ONLY} vers warble {version}\r\n",
            args.callsign,
        ),
    };
    out.write_all(login.as_bytes())?;
    out.flush()?;

    let mut reader = BufReader::new(stream);
    let mut raw = Vec::with_capacity(LINE_MAX);

    loop {
        if reached_bound(args, stats, started) {
            return Ok(Done::Finished);
        }

        raw.clear();
        // Read bytes, never a String. Mic-E is binary and comment
        // fields carry bare Latin-1, so the stream is not valid UTF-8
        // and decoding it here would corrupt packets before they are
        // written.
        match reader.read_until(b'\n', &mut raw) {
            Ok(0) => return Ok(Done::Disconnected),
            Ok(_) => {}
            Err(e) => return Err(e),
        }
        while matches!(raw.last(), Some(b'\r' | b'\n')) {
            raw.pop();
        }
        if raw.is_empty() {
            continue;
        }

        // A line beginning with '#' is a server comment: the greeting,
        // the login response, or a keepalive. It is never data, because
        // a source callsign cannot begin with '#'.
        if raw[0] == b'#' {
            stats.server_lines += 1;
            let text = String::from_utf8_lossy(&raw);
            if text.contains("logresp") {
                eprintln!("{}", text.trim());
                if text.contains("unverified") {
                    eprintln!("(receive-only, as intended)");
                }
            }
            if !args.keep_comments {
                continue;
            }
        } else {
            stats.lines += 1;
        }

        // CRLF, matching what the servers send and what a TNC2 capture
        // file is expected to contain.
        sink.write_all(&raw)?;
        sink.write_all(b"\r\n")?;
    }
}
