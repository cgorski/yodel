//! Decoding many feeds concurrently on a tokio runtime, using the
//! `asynk` stream API (`--features async,wav`).
//!
//! * **Scenario** — the same multi-receiver site as
//!   [`decode_many_threads.rs`](decode_many_threads.rs), for a program
//!   that already runs a tokio runtime.
//! * **Hardware** — any host with a full OS. Not an MCU.
//! * **Features** — `async`. Note there is no `wav` here: the feeds are
//!   raw PCM, which is the whole point of `decode_many`.
//!
//! ```sh
//! # One feed per receiver, raw s16le mono PCM at 48 kHz.
//! cargo run --example decode_many_tokio --features async -- a.s16 b.s16
//! ```
//!
//! Make a couple with the CLI, or convert recordings with `sox`:
//!
//! ```sh
//! cargo run --features cli -- gen --out - --sample-rate 48000 \
//!     --count 5 --from N0CALL-1 > a.s16
//! sox recording.wav -t raw -r 48000 -c 1 -b 16 -e signed-integer b.s16
//! ```
//!
//! # Which of the four entry points to use
//!
//! `yodel::asynk` offers four ways in, and picking the right one is
//! most of the work:
//!
//! | You have | Use | Notes |
//! |---|---|---|
//! | a path to a WAV file | [`decode_wav`](yodel::asynk::decode_wav) | reads on a blocking thread; nothing is buffered up front |
//! | one `AsyncRead` (socket, stdin, pipe) | [`decode_stream`](yodel::asynk::decode_stream) | sniffs WAV vs raw s16le; pass the rate for raw |
//! | one `AsyncRead` of raw PCM at a known config | [`frames`](yodel::asynk::frames) | no sniffing, no `wav` feature needed |
//! | *N* readers at once | [`decode_many`](yodel::asynk::decode_many) | one merged stream, each item tagged with its feed index |
//!
//! This example uses `decode_many`, because the interesting question
//! with more than one radio is not how to decode — it is how to know
//! *which* radio a frame came from, and how to stop a fast feed from
//! starving a slow one.
//!
//! # Backpressure is the point
//!
//! Every one of these returns a stream backed by a **bounded** channel.
//! That matters more than it looks:
//!
//! * The decoders cannot run ahead of the consumer. If the sink here
//!   were writing to a slow disk or a network socket, the channel would
//!   fill, the feed tasks would park on `send`, and the readers would
//!   stop pulling bytes. Nothing is dropped and nothing grows without
//!   bound — the pressure reaches all the way back to the input.
//! * That is the opposite of `tokio::spawn`-per-frame, which looks
//!   simpler and quietly turns a slow consumer into unbounded memory
//!   growth.
//!
//! Nothing here arranges that: it is a property of the bounded channel
//! `decode_many` returns, and you inherit it by using the stream. Give
//! the loop below a slow sink -- a database insert, a remote POST -- and
//! the feeds throttle to match, with no frames dropped and no queue
//! growth.
//!
//! # Why the DSP core is not async
//!
//! Decoding a sample is arithmetic, not I/O: there is nothing to await,
//! and making it `async` would add a state machine to a hot loop for no
//! benefit. So the core stays synchronous, allocation-free and
//! `no_std`, and `asynk` puts the *edges* on the runtime. Async at the
//! boundary, arithmetic inside.
//!
//! For the same job without a runtime at all, see
//! [`examples/decode_many_threads.rs`](decode_many_threads.rs) — std
//! threads and `sync_channel`, no tokio in `cargo tree`.

use std::time::Instant;

use tokio_stream::StreamExt;
use yodel::SampleRate;
use yodel::ax25::Address;
use yodel::tnc::{OwnedFrame, TncConfig};

const RATE_HZ: u32 = 48_000;

/// Where to get inputs, printed when none are given.
const INPUT_HELP: &str = "\
input: one or more feeds of RAW s16le mono PCM at 48000 Hz, one per
receiver. Not WAV: `decode_many` takes the rate from the TncConfig, so
the stream carries no header (see the entry-point table in the header
docs). A WAV passed here decodes its own header as audio and yields
nothing.

no feeds yet? make some:
  cargo run --features cli -- gen --out - --sample-rate 48000 \\
      --count 5 --from N0CALL-1 > a.s16
  cargo run --features cli -- gen --out - --sample-rate 48000 \\
      --count 5 --from N0CALL-2 > b.s16

convert a WAV you already have:
  sox in.wav -t raw -r 48000 -c 1 -b 16 -e signed-integer out.s16

each feed is any `AsyncRead`, so a TcpStream or a pipe drops in where
the files are opened below.";

#[tokio::main]
async fn main() {
    // Display, not Debug: returning `Result` from `main` escapes the
    // newlines in a multi-line help message onto one unreadable line.
    if let Err(e) = run().await {
        eprintln!("{e}");
        std::process::exit(2);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let rate = SampleRate::new(RATE_HZ)?;
    let cfg = TncConfig::bell_202(rate)?;

    let labels: Vec<String> = std::env::args().skip(1).collect();
    if labels.is_empty() {
        return Err(format!("usage: decode_many_tokio <a.s16> <b.s16> ...\n\n{INPUT_HELP}").into());
    }

    // Each feed is anything implementing tokio's `AsyncRead`: a file
    // here, a `TcpStream` from a networked receiver just as easily.
    let mut feeds = Vec::with_capacity(labels.len());
    for path in &labels {
        feeds.push(
            tokio::fs::File::open(path)
                .await
                .map_err(|e| format!("cannot open {path}: {e}\n\n{INPUT_HELP}"))?,
        );
    }

    for (i, label) in labels.iter().enumerate() {
        println!("feed {i}: {label}");
    }
    println!();

    // One merged stream over every feed. Items are `(feed_index, result)`,
    // which is how a frame keeps its provenance without a channel per radio.
    let started = Instant::now();
    let mut stream = std::pin::pin!(yodel::asynk::decode_many(feeds, cfg));

    let mut total = 0u32;
    let mut per_feed = vec![0u32; labels.len()];
    while let Some((feed, result)) = stream.next().await {
        match result {
            Ok(frame) => {
                total += 1;
                if let Some(slot) = per_feed.get_mut(feed) {
                    *slot += 1;
                }
                let label = labels.get(feed).map_or("?", String::as_str);
                println!(
                    "[{:>6.2}s] {label}: {}",
                    started.elapsed().as_secs_f64(),
                    describe(&frame)
                );
            }
            Err(e) => eprintln!("feed {feed}: {e}"),
        }
    }

    println!(
        "\ndecoded {total} frame(s) in {:.2}s",
        started.elapsed().as_secs_f64()
    );
    for (i, n) in per_feed.iter().enumerate() {
        println!(
            "  feed {i} ({}): {n}",
            labels.get(i).map_or("?", String::as_str)
        );
    }
    Ok(())
}

/// `CALL` or `CALL-SSID`.
fn fmt_addr(addr: &Address) -> String {
    let call = String::from_utf8_lossy(addr.callsign.as_bytes()).into_owned();
    match addr.ssid.value() {
        0 => call,
        n => format!("{call}-{n}"),
    }
}

/// A one-line summary of a received frame.
fn describe(frame: &OwnedFrame) -> String {
    match frame.ui_frame() {
        Ok(ui) => format!(
            "{}>{}: {}",
            fmt_addr(&ui.src),
            fmt_addr(&ui.dest),
            String::from_utf8_lossy(frame.info())
        ),
        Err(e) => format!("(unparsable UI frame: {e})"),
    }
}
