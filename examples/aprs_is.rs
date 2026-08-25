//! Read the live APRS-IS feed from the internet and report on it.
//!
//! APRS-IS is the internet side of APRS: igates around the world hear
//! packets on the radio and forward them to a server network, which
//! streams them back out as text. This example connects to that stream,
//! decodes every packet with the same parsers the radio path uses, and
//! prints running statistics.
//!
//! ```sh
//! # 250 km around Kansas City, for 60 seconds.
//! cargo run --example aprs_is --features std,aprs,micE -- \
//!     --lat 39.1 --lon -94.6 --dist 250 --seconds 60
//!
//! # Every packet, as it arrives.
//! cargo run --example aprs_is --features std,aprs,micE -- --print
//!
//! # A specific station, until you press Ctrl-C.
//! cargo run --example aprs_is --features std,aprs,micE -- --filter 'b/N0CALL*'
//! ```
//!
//! # This connects to a shared, volunteer-run network
//!
//! The defaults here are conservative, and you should keep
//! them that way:
//!
//! * **Receive only.** It logs in with passcode `-1`, which the servers
//!   treat as unverified. An unverified client may receive but may not
//!   send, which is exactly what this example wants. There is no code
//!   path here that transmits, and none should be added: injecting into
//!   APRS-IS requires a licensed amateur callsign, because every packet
//!   on APRS-IS must be assumed to reach the air.
//! * **One connection.** Opening several at once creates a loop and
//!   causes bursts of delayed duplicates that make stations jump around
//!   on other people's maps.
//! * **A narrow filter.** Port 14580 sends nothing until you subscribe.
//!   Ask for a small area. Server operators specifically ask clients not
//!   to request near-full feeds from the Tier 2 network.
//! * **Backoff on reconnect**, with a fresh DNS lookup each time, since
//!   the rotate addresses load-balance across many volunteers' servers.
//!
//! Replace the default `N0CALL` with your own callsign if you have one.
//! The login is an identifier that server operators can see and act on;
//! it is not a throwaway.
//!
//! # What it demonstrates
//!
//! [`MonitorLine`] turns one line of TNC2 monitor text into source,
//! destination, path and information field, and `decoded()` runs the
//! information field through the crate's total decoder. That is the
//! same [`Decoded`] type the audio path produces, so everything
//! downstream of the modem works identically whether a packet arrived
//! over the air or over a socket.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use yodel::aprs::monitor::{LINE_MAX, MonitorLine};
use yodel::aprs::{AprsPacket, Decoded, DecodedKind};

/// Tier 2 rotate address. Resolves to a different server each time,
/// which is why the address is re-resolved on every connection.
const DEFAULT_SERVER: &str = "rotate.aprs2.net:14580";

/// The core round-robin, which is the only tier that carries the
/// unfiltered feed. Sample it briefly and do not point a long-running
/// client at it: the guidance is that GUI clients belong on Tier 2.
const FULL_FEED_SERVER: &str = "rotate.aprs.net:10152";

/// Receive-only passcode. Anything else requires a licensed callsign.
const RECEIVE_ONLY: &str = "-1";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let opts = Options::from_args()?;
    let mut stats = Stats::default();
    let started = Instant::now();
    let mut backoff = Duration::from_secs(5);
    let mut attempt = 0usize;

    loop {
        let before = stats.lines;
        attempt += 1;
        match run_session(&opts, &mut stats, started, attempt) {
            Ok(Done::Finished) => break,
            Ok(Done::Disconnected) => eprintln!("server closed the connection"),
            Err(e) => eprintln!("connection failed: {e}"),
        }
        if let Some(limit) = opts.seconds
            && started.elapsed() >= Duration::from_secs(limit)
        {
            break;
        }
        // A session that delivered traffic was healthy, so start the
        // next backoff from scratch. Without this, a long run that
        // reconnects occasionally would creep up to the cap and stay
        // there.
        if stats.lines > before {
            backoff = Duration::from_secs(5);
        }
        eprintln!("reconnecting in {:.0}s", backoff.as_secs_f32());
        std::thread::sleep(backoff);
        // Exponential backoff, capped. A tight reconnect loop against a
        // rotate address hammers many different operators' servers.
        backoff = (backoff * 2).min(Duration::from_secs(300));
    }

    stats.report(started.elapsed());
    Ok(())
}

enum Done {
    Finished,
    Disconnected,
}

fn run_session(
    opts: &Options,
    stats: &mut Stats,
    started: Instant,
    attempt: usize,
) -> std::io::Result<Done> {
    // Re-resolve every time so the load balancer can do its job, and
    // pick a different entry on each attempt. Always taking the first
    // address defeats the rotation just as thoroughly as caching it
    // would, and pins a retry loop to whichever server just failed.
    let addrs: Vec<_> = opts.server.to_socket_addrs()?.collect();
    let addr = *addrs
        .get(attempt % addrs.len().max(1))
        .ok_or_else(|| std::io::Error::other("no address for server"))?;
    let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(15))?;
    stream.set_nodelay(true)?;
    // A server keepalive arrives about every 20 s; well past that with
    // nothing at all means the connection is dead.
    stream.set_read_timeout(Some(Duration::from_secs(90)))?;
    eprintln!("connected to {addr}");

    let mut out = stream.try_clone()?;
    // With no filter the login ends after the version. On port 14580
    // that subscribes to almost nothing; on a full-feed port it is the
    // only correct form, because the feed is not filterable.
    let login = if opts.filter.is_empty() {
        format!(
            "user {} pass {RECEIVE_ONLY} vers yodel-example {}\r\n",
            opts.callsign,
            env!("CARGO_PKG_VERSION"),
        )
    } else {
        format!(
            "user {} pass {RECEIVE_ONLY} vers yodel-example {} filter {}\r\n",
            opts.callsign,
            env!("CARGO_PKG_VERSION"),
            opts.filter,
        )
    };
    out.write_all(login.as_bytes())?;
    out.flush()?;

    let mut reader = BufReader::new(stream);
    let mut raw = Vec::with_capacity(LINE_MAX);

    loop {
        if let Some(limit) = opts.seconds
            && started.elapsed() >= Duration::from_secs(limit)
        {
            return Ok(Done::Finished);
        }

        raw.clear();
        // Read bytes, never a String: Mic-E is binary and comment
        // fields carry bare Latin-1, so the stream is not valid UTF-8.
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
        // a source callsign cannot start with '#'.
        if raw[0] == b'#' {
            stats.server_lines += 1;
            let text = String::from_utf8_lossy(&raw);
            if text.contains("logresp") {
                eprintln!("{}", text.trim());
                if text.contains("unverified") {
                    eprintln!("(receive-only, as intended)");
                }
            }
            continue;
        }

        stats.lines += 1;
        match MonitorLine::parse(&raw) {
            Ok(line) => {
                let decoded = line.decoded();
                if opts.print {
                    print_packet(&line, &decoded);
                }
                stats.record(&line, &decoded);
            }
            Err(_) => stats.unparseable += 1,
        }
    }
}

fn print_packet(line: &MonitorLine<'_>, decoded: &Decoded<'_>) {
    let src = String::from_utf8_lossy(line.source);
    let via = if line.is_from_rf() { "RF " } else { "net" };
    let what = describe(decoded);
    match position_of(decoded) {
        Some((lat, lon)) => println!("{via} {src:<9} {what:<12} {lat:9.4},{lon:10.4}"),
        None => println!("{via} {src:<9} {what:<12} {}", summary(decoded)),
    }
}

/// A short label for what the packet turned out to be.
fn describe(decoded: &Decoded<'_>) -> &'static str {
    match &decoded.kind {
        DecodedKind::Packet(p) => match p {
            AprsPacket::Position(_)
            | AprsPacket::PositionCs(_)
            | AprsPacket::PositionTimestamped(_) => "position",
            AprsPacket::PositionWeather(_) | AprsPacket::Weather(_) => "weather",
            AprsPacket::Status(_) => "status",
            AprsPacket::Message(_) => "message",
            AprsPacket::Telemetry(_) => "telemetry",
            AprsPacket::Object(_) => "object",
            AprsPacket::Item(_) => "item",
            AprsPacket::Capabilities(_) => "capabilities",
            _ => "other",
        },
        #[cfg(feature = "micE")]
        DecodedKind::MicE(_) => "mic-e",
        DecodedKind::Nmea(_) => "nmea",
        DecodedKind::Ultimeter(_) => "ultimeter",
        DecodedKind::ThirdParty(_) => "third-party",
        DecodedKind::NeedsDestination { .. } => "needs-dest",
        DecodedKind::Unsupported { .. } => "unsupported",
        DecodedKind::Malformed { .. } => "malformed",
        _ => "other",
    }
}

/// Latitude and longitude in degrees, for the kinds that carry one.
fn position_of(decoded: &Decoded<'_>) -> Option<(f64, f64)> {
    match &decoded.kind {
        DecodedKind::Packet(AprsPacket::Position(p)) => {
            Some((p.latitude.to_degrees(), p.longitude.to_degrees()))
        }
        DecodedKind::Packet(AprsPacket::PositionCs(p)) => Some((
            p.position.latitude.to_degrees(),
            p.position.longitude.to_degrees(),
        )),
        DecodedKind::Packet(AprsPacket::PositionTimestamped(p)) => Some((
            p.position.latitude.to_degrees(),
            p.position.longitude.to_degrees(),
        )),
        DecodedKind::Packet(AprsPacket::PositionWeather(w)) => {
            Some((w.latitude.to_degrees(), w.longitude.to_degrees()))
        }
        DecodedKind::Packet(AprsPacket::Object(o)) => {
            Some((o.latitude.to_degrees(), o.longitude.to_degrees()))
        }
        DecodedKind::Packet(AprsPacket::Item(i)) => {
            Some((i.latitude.to_degrees(), i.longitude.to_degrees()))
        }
        #[cfg(feature = "micE")]
        DecodedKind::MicE(m) => {
            let c = m.coordinates();
            Some((c.latitude.to_degrees(), c.longitude.to_degrees()))
        }
        _ => None,
    }
}

fn summary(decoded: &Decoded<'_>) -> String {
    // The information field is arbitrary bytes, so render it lossily
    // and only for display.
    let text = String::from_utf8_lossy(decoded.info);
    text.chars().take(48).collect()
}

#[derive(Default)]
struct Stats {
    lines: u64,
    server_lines: u64,
    unparseable: u64,
    kinds: HashMap<&'static str, u64>,
    stations: HashMap<String, u64>,
    igates: HashMap<String, u64>,
    from_rf: u64,
    from_net: u64,
    positions: u64,
    lat: (f64, f64),
    lon: (f64, f64),
}

impl Stats {
    fn record(&mut self, line: &MonitorLine<'_>, decoded: &Decoded<'_>) {
        *self.kinds.entry(describe(decoded)).or_default() += 1;
        *self
            .stations
            .entry(String::from_utf8_lossy(line.source).into_owned())
            .or_default() += 1;
        if line.is_from_rf() {
            self.from_rf += 1;
            if let Some(gate) = line.igate() {
                *self
                    .igates
                    .entry(String::from_utf8_lossy(gate).into_owned())
                    .or_default() += 1;
            }
        } else {
            self.from_net += 1;
        }
        if let Some((lat, lon)) = position_of(decoded) {
            if self.positions == 0 {
                self.lat = (lat, lat);
                self.lon = (lon, lon);
            } else {
                self.lat = (self.lat.0.min(lat), self.lat.1.max(lat));
                self.lon = (self.lon.0.min(lon), self.lon.1.max(lon));
            }
            self.positions += 1;
        }
    }

    fn report(&self, elapsed: Duration) {
        let secs = elapsed.as_secs_f64().max(1.0);
        println!("\n─── {:.0}s of APRS-IS ───", elapsed.as_secs_f64());
        println!(
            "{} packets ({:.1}/s), {} server lines, {} unparseable",
            self.lines,
            self.lines as f64 / secs,
            self.server_lines,
            self.unparseable
        );
        if self.lines == 0 {
            println!("nothing received; try a wider filter or a longer run");
            return;
        }
        let pct = |n: u64| 100.0 * n as f64 / self.lines as f64;
        println!(
            "heard on radio {} ({:.0}%), injected on the internet {} ({:.0}%)",
            self.from_rf,
            pct(self.from_rf),
            self.from_net,
            pct(self.from_net)
        );

        println!("\nby payload:");
        for (kind, n) in sorted(&self.kinds) {
            println!("  {kind:<14} {n:>6}  {:>5.1}%", pct(n));
        }

        println!("\nmost active stations:");
        for (call, n) in sorted(&self.stations).into_iter().take(10) {
            println!("  {call:<12} {n:>6}");
        }

        if !self.igates.is_empty() {
            println!("\nbusiest igates (packets each gated from RF):");
            for (call, n) in sorted(&self.igates).into_iter().take(10) {
                println!("  {call:<12} {n:>6}");
            }
        }

        println!(
            "\n{} stations seen, {} positions decoded",
            self.stations.len(),
            self.positions
        );
        if self.positions > 0 {
            println!(
                "bounding box  lat {:.4}..{:.4}  lon {:.4}..{:.4}",
                self.lat.0, self.lat.1, self.lon.0, self.lon.1
            );
        }
    }
}

fn sorted<K: Clone + Ord>(map: &HashMap<K, u64>) -> Vec<(K, u64)> {
    let mut v: Vec<(K, u64)> = map.iter().map(|(k, n)| (k.clone(), *n)).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v
}

struct Options {
    server: String,
    callsign: String,
    filter: String,
    seconds: Option<u64>,
    print: bool,
}

impl Options {
    fn from_args() -> Result<Self, Box<dyn std::error::Error>> {
        let mut o = Options {
            server: DEFAULT_SERVER.to_string(),
            callsign: "N0CALL".to_string(),
            filter: String::new(),
            seconds: Some(30),
            print: false,
        };
        let (mut lat, mut lon, mut dist) = (39.1_f64, -94.6_f64, 250_u32);
        let mut explicit_filter = None;

        let mut args = std::env::args().skip(1);
        while let Some(a) = args.next() {
            let mut val = || args.next().ok_or_else(|| format!("{a} needs a value"));
            match a.as_str() {
                "--server" => o.server = val()?,
                "--callsign" => o.callsign = val()?,
                "--filter" => explicit_filter = Some(val()?),
                "--lat" => lat = val()?.parse()?,
                "--lon" => lon = val()?.parse()?,
                "--dist" => dist = val()?.parse()?,
                "--seconds" => o.seconds = Some(val()?.parse()?),
                "--forever" => o.seconds = None,
                "--no-filter" => explicit_filter = Some(String::new()),
                "--full-feed" => {
                    // The unfiltered firehose lives on a different port,
                    // and only the core servers carry it.
                    o.server = FULL_FEED_SERVER.to_string();
                    explicit_filter = Some(String::new());
                }
                "--print" => o.print = true,
                "-h" | "--help" => {
                    println!("{}", HELP);
                    std::process::exit(0);
                }
                other => return Err(format!("unknown argument {other}\n\n{HELP}").into()),
            }
        }
        o.filter = explicit_filter.unwrap_or_else(|| format!("r/{lat}/{lon}/{dist}"));
        Ok(o)
    }
}

const HELP: &str = "\
Read the live APRS-IS feed and report statistics. Receive only.

  --lat <DEG> --lon <DEG> --dist <KM>   area to subscribe to (default 39.1/-94.6/250)
  --filter <SPEC>                       raw APRS-IS filter, replaces the area
  --callsign <CALL>                     login callsign (default N0CALL)
  --server <HOST:PORT>                  default rotate.aprs2.net:14580
  --seconds <N>                         run for N seconds (default 30)
  --forever                             run until interrupted
  --print                               print every packet as it arrives
  --no-filter                           subscribe to nothing (see below)
  --full-feed                           sample the unfiltered core feed, briefly

Note that --no-filter and --full-feed are not the same thing. Port 14580
is an additive subscription: with no filter it sends you almost nothing.
The unfiltered firehose is a separate port on the core servers, which is
what --full-feed uses. Sample it briefly; it is not for long-running
clients, and the volume will outrun most applications.

Filters are additive subscriptions; port 14580 sends almost nothing
until you ask. Keep the area small: the Tier 2 servers are run by
volunteers and their operators ask clients not to request near-full
feeds. Common filters:

  r/LAT/LON/KM     circle          b/CALL1/CALL2   these stations
  p/PREFIX         callsign prefix t/pomw          types (position/object/message/weather)
  a/N/W/S/E        bounding box    e/IGATE         gated by this igate
";
