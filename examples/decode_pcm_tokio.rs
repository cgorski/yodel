//! Decode a **live PCM stream** on tokio (`--features async`).
//!
//! ```sh
//! # Runs three short demonstrations against a local paced "radio".
//! cargo run --example decode_pcm_tokio --features async
//!
//! # Real use: raw s16le mono PCM at 48 kHz on stdin.
//! arecord -f S16_LE -r 48000 -c 1 -t raw | \
//!     cargo run --example decode_pcm_tokio --features async -- --stdin
//! ```
//!
//! # Why a stream is the async case, and a file is not
//!
//! For a WAV on disk, use [`examples/decode_wav.rs`](decode_wav.rs): the
//! file ends by itself, waiting on a disk read is cheap, and a runtime
//! would add machinery for nothing.
//!
//! A live stream changes that. It has **no end you know in advance**, it
//! **arrives in real time** so the reader spends its life waiting on
//! bytes that do not exist yet, and it is usually **one of several**
//! alongside a web UI or an uplink. Those are the three things a runtime
//! is for.
//!
//! The whole decode is this:
//!
//! ```no_run
//! # async fn demo(reader: tokio::net::TcpStream, cfg: warble::tnc::TncConfig) {
//! use tokio_stream::StreamExt;
//! let mut frames = std::pin::pin!(warble::asynk::frames(reader, cfg));
//! while let Some(Ok(frame)) = frames.next().await {
//!     println!("{}", String::from_utf8_lossy(frame.info()));
//! }
//! # }
//! ```
//!
//! Everything below that is this file demonstrating three properties of
//! that stream. They are what justify the runtime, so they are shown
//! unconditionally rather than hidden behind flags.
//!
//! # Input format
//!
//! Raw **s16le mono PCM** at [`RATE_HZ`] — no header, no framing. That
//! is what `arecord -f S16_LE -r 48000 -c 1 -t raw` emits and what most
//! SDR tools pipe out. [`warble::asynk::frames`] takes the rate from the
//! [`TncConfig`], so the stream carries no metadata that could disagree
//! with you; it also means this example needs no `wav` feature.
//!
//! For a *WAV* stream, whose header states its own rate, use
//! [`warble::asynk::decode_stream`] instead — same shape, plus sniffing.
//! To turn a WAV file into a stream of the right shape:
//!
//! ```sh
//! sox beacon.wav -t raw -r 48000 -c 1 -b 16 -e signed-integer - | \
//!     cargo run --example decode_pcm_tokio --features async -- --stdin
//! ```

use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncWriteExt};
use tokio_stream::StreamExt;
use warble::SampleRate;
use warble::aprs::{AprsPacket, Status};
use warble::ax25::Address;
use warble::tnc::{TncConfig, TncTransmitter};

/// Sample rate of the stream. Raw PCM has no header, so both ends must
/// agree out of band; this is the agreement.
const RATE_HZ: u32 = 48_000;

/// Beacons the demo radio sends, spaced by [`GAP`].
const BEACONS: usize = 4;

/// Idle gap the demo radio leaves between transmissions.
const GAP: Duration = Duration::from_millis(150);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = TncConfig::bell_202(SampleRate::new(RATE_HZ)?)?;

    if std::env::args().any(|a| a == "--stdin") {
        println!("decoding raw s16le PCM from stdin at {RATE_HZ} Hz");
        let n = decode(tokio::io::stdin(), cfg, None, None).await?;
        println!("decoded {n} frame(s)");
        return Ok(());
    }

    // 1. A live feed decodes as it arrives. The gaps in the output are
    //    the radio's idle time, not ours: this is the wait that would
    //    otherwise occupy a thread.
    println!("-- 1. live feed --");
    let n = decode(demo_radio(cfg).await?, cfg, None, None).await?;
    println!("   {n} frame(s)\n");

    // 2. Backpressure. A slow consumer stops the reader, the socket
    //    receive window closes, and the *sender* blocks -- across a
    //    network boundary, with no flow control written by hand. Frame
    //    spacing stretches to the sink's rate and nothing is dropped.
    println!("-- 2. slow consumer: spacing follows the sink, no loss --");
    let n = decode(
        demo_radio(cfg).await?,
        cfg,
        None,
        Some(Duration::from_millis(400)),
    )
    .await?;
    println!("   {n} frame(s), still all of them\n");

    // 3. Cancellation is dropping the stream. No shutdown handshake, no
    //    flag for the reader to poll: the channel closes and its task
    //    stops.
    println!("-- 3. deadline at 250 ms --");
    let n = decode(
        demo_radio(cfg).await?,
        cfg,
        Some(Duration::from_millis(250)),
        None,
    )
    .await?;
    println!("   {n} frame(s) before the deadline cut it off");
    Ok(())
}

/// Decodes every frame from `reader`, printing each with the gap since
/// the previous one so the stream's timing is visible.
///
/// `budget` is a **total** deadline, not a per-frame one, so a
/// long-running feed cannot creep past it a frame at a time.
/// `sink_delay` simulates a slow consumer.
async fn decode(
    reader: impl AsyncRead + Send + Unpin + 'static,
    cfg: TncConfig,
    budget: Option<Duration>,
    sink_delay: Option<Duration>,
) -> Result<usize, Box<dyn std::error::Error>> {
    let deadline = budget.map(|b| tokio::time::Instant::now() + b);
    let mut frames = std::pin::pin!(warble::asynk::frames(reader, cfg));
    let mut n = 0;
    let mut last = Instant::now();
    loop {
        let next = match deadline {
            Some(at) => match tokio::time::timeout_at(at, frames.next()).await {
                Ok(item) => item,
                Err(_) => break,
            },
            None => frames.next().await,
        };
        let Some(item) = next else { break };
        let frame = item?;
        n += 1;
        println!(
            "   (+{:>4.0} ms) {}",
            last.elapsed().as_secs_f64() * 1000.0,
            String::from_utf8_lossy(frame.info())
        );
        last = Instant::now();
        if let Some(d) = sink_delay {
            tokio::time::sleep(d).await;
        }
    }
    Ok(n)
}

/// A radio on the network: serves [`BEACONS`] transmissions as raw PCM,
/// paced [`GAP`] apart so the example behaves like a live feed rather
/// than a file that arrives all at once. Returns a connected socket.
async fn demo_radio(cfg: TncConfig) -> Result<tokio::net::TcpStream, Box<dyn std::error::Error>> {
    let tx = TncTransmitter::new(cfg);
    let mut bursts = Vec::new();
    for seq in 0..BEACONS {
        let text = format!("live demo {seq}");
        let packet = AprsPacket::Status(Status {
            text: text.as_bytes(),
        });
        let pcm = tx.transmit_to_vec_i16(
            &packet,
            Address::new(b"APRS", 0)?,
            Address::new(b"N0CALL", 9)?,
            &[],
        )?;
        bursts.push(
            pcm.iter()
                .flat_map(|s| s.to_le_bytes())
                .collect::<Vec<u8>>(),
        );
    }

    // Port 0: the OS picks, so parallel runs never collide.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        for burst in bursts {
            // A failed write means the decoder went away -- a normal
            // end here, not an error.
            if socket.write_all(&burst).await.is_err() {
                return;
            }
            tokio::time::sleep(GAP).await;
        }
        let _ = socket.shutdown().await;
    });
    Ok(tokio::net::TcpStream::connect(addr).await?)
}
