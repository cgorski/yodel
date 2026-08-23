//! N-concurrent-feeds decode: many WAV inputs in parallel on a bounded
//! worker pool, frames flowing through bounded channels into a sink.
//!
//! * **Scenario** — a multi-receiver site: several radios or recordings
//!   decoded at once into one log. The runtime-free counterpart to
//!   [`decode_many_tokio.rs`](decode_many_tokio.rs).
//! * **Hardware** — any multi-core host. At ~110 ns/sample one core
//!   handles hundreds of real-time feeds, so this is bounded by I/O
//!   rather than by the DSP.
//! * **Features** — `tnc,wav`. No tokio anywhere in `cargo tree`.
//!
//! ```text
//! cargo run --example decode_many_threads --features tnc,wav -- out.jsonl a.wav b.wav ...
//! ```
//!
//! This is the crate's documented runtime-free concurrency idiom (std
//! threads + bounded `sync_channel`s), recorded alongside the async
//! discussion in `docs/ARCHITECTURE.md`: warble's DSP core is
//! synchronous and allocation-free, so concurrency belongs at the I/O
//! edges of the *application*, not inside the library. At ~110
//! ns/sample one core decodes hundreds of real-time feeds, so a small
//! worker pool is the whole story.
//!
//! The moving parts:
//!
//! * a fixed pool of [`WORKERS`] decode threads pulling file paths from
//!   a shared work queue (a `Mutex<VecDeque>` — the pool is bounded by
//!   construction, never one-thread-per-file);
//! * each decoded frame crosses ONE bounded `sync_channel` of depth
//!   [`CHANNEL_DEPTH`] into the sink thread;
//! * the [`Sink`] trait receives the frames; two implementations ship —
//!   [`MemorySink`] (counts/stores, with an optional artificial delay)
//!   and [`JsonlSink`] (one JSON object per frame into a file).
//!
//! # Backpressure, shown
//!
//! `sync_channel(CHANNEL_DEPTH)` is the backpressure valve: when the
//! sink is slower than the decoders, the channel fills and the NEXT
//! `send` in a decode worker BLOCKS until the sink drains a slot. The
//! decode workers therefore pace themselves to the sink automatically —
//! memory in flight is bounded by `CHANNEL_DEPTH` frames plus one
//! per worker, no matter how many files or how slow the sink. The test
//! in `tests/app_examples.rs` proves it with an artificially slow
//! [`MemorySink`]: every frame still arrives, and the channel can never
//! hold more than its capacity by construction.
//!
//! # Using warble from async (tokio)
//!
//! **The crate ships the adapter: `--features async`, module
//! `warble::asynk`.** None of the glue below is something you have to
//! write. The async analogue of this example's decode side is two
//! lines — a bounded `Stream` of frames, each tagged with the feed it
//! came from, a slow consumer stalling the decoders exactly as the slow
//! sink stalls them here:
//!
//! ```text
//! // N feeds decoded concurrently, frames tagged with the feed index:
//! let mut frames = std::pin::pin!(warble::asynk::decode_many(feeds, cfg));
//! while let Some((feed, frame)) = frames.next().await { … }
//! ```
//!
//! `decode_many`'s feeds are raw s16le PCM readers, the one difference
//! from this file's WAV inputs; for those there is `decode_wav(path)`
//! per file, or `decode_stream(reader, rate)`, which sniffs a WAV header
//! off a pipe and falls back to raw PCM. Also `frames(reader, cfg)` for
//! a single raw feed and `serve_kiss` for KISS-over-TCP. Inside every
//! one of them the synchronous DSP runs on `spawn_blocking` and each
//! channel is bounded — the same triangle as this file, assembled for
//! you.
//!
//! ## So why does this example still exist?
//!
//! Because a runtime is a dependency, and this shape needs none: std
//! threads, `std::sync::mpsc`, no tokio in `cargo tree`. If that is
//! what you want — or you already own a thread pool, or you are
//! curious what the adapter does — this is the worked answer, and the
//! "why threads" reasoning below is unchanged by the feature existing.
//! The historical record of the decision (a session-8 NO, overridden in
//! session 9) is `docs/ARCHITECTURE.md` §"The serve shape and the async
//! verdict"; what it says about the *core* still holds, which is why
//! the async code lives in an off-by-default adapter and the DSP itself
//! is still synchronous and allocation-free.
//!
//! If you are already on a runtime,
//! [`examples/decode_many_tokio.rs`](decode_many_tokio.rs) is this same job built
//! on `warble::asynk` instead of hand-rolled — `decode_many` merges N
//! feeds into one stream, tagging each frame with its feed index, and
//! its `--slow` flag makes the bounded-channel backpressure visible.
//!
//! If you would rather hand-roll it anyway, the tokio shape of THIS
//! example is a mechanical transpose:
//!
//! ```text
//! // async edges, sync DSP core, bounded channel — the same triangle:
//! let (tx, mut rx) = tokio::sync::mpsc::channel::<DecodedFrame>(64); // bounded
//! for path in paths {
//!     let tx = tx.clone();
//!     tokio::task::spawn_blocking(move || {          // sync core off the runtime
//!         let mut rx_modem = DefaultTncReceiver::new(cfg)?;
//!         for sample in wav_samples(&path) {
//!             if let Some(frame) = rx_modem.push_i16(sample?) {
//!                 // Copy out of the lending borrow, then a BLOCKING
//!                 // send: tokio's bounded mpsc gives the same
//!                 // backpressure this example gets from sync_channel.
//!                 tx.blocking_send(to_owned(frame))?;
//!             }
//!         }
//!         Ok::<_, Error>(())
//!     });
//! }
//! drop(tx);
//! while let Some(frame) = rx.recv().await {          // async sink edge
//!     db.insert(frame).await?;                       // e.g. a DB write
//! }
//! ```
//!
//! `spawn_blocking` hosts the synchronous decode loop, `blocking_send`
//! / `recv().await` bridge the sync→async boundary, and the bounded
//! channel is the backpressure — a slow database stalls the decoders
//! exactly as the slow sink stalls them here. Nothing else changes;
//! `warble::asynk` is that ~20-line pattern, written once, with the
//! lending-borrow copy (`OwnedFrame`) and the channel bounds already
//! decided.

use std::collections::VecDeque;
use std::io::Write;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};

use warble::SampleRate;
use warble::tnc::{DefaultTncReceiver, OwnedFrame, TncConfig};

/// Size of the decode worker pool: bounded however many files arrive.
pub const WORKERS: usize = 4;

/// Depth of the bounded frame channel between workers and the sink:
/// the backpressure valve (see the module header).
pub const CHANNEL_DEPTH: usize = 8;

/// One decoded frame crossing the channel: the library's owned copy
/// of the frame ([`OwnedFrame`] — the receiver's lending `RxFrame`
/// borrow cannot leave the decode loop) plus which input it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedFrame {
    /// Which input file the frame came from.
    pub source: String,
    /// The owned frame (addresses, path with H bits, info field).
    pub frame: OwnedFrame,
}

impl DecodedFrame {
    /// `CALL[-SSID]` of the sender.
    #[must_use]
    pub fn sender(&self) -> String {
        let src = self.frame.src();
        let call = String::from_utf8_lossy(src.callsign.as_bytes())
            .trim_end()
            .to_owned();
        match src.ssid.value() {
            0 => call,
            n => format!("{call}-{n}"),
        }
    }
}

/// Where decoded frames end up. Implementations may be arbitrarily
/// slow; the bounded channel turns their slowness into decoder
/// backpressure instead of memory growth.
pub trait Sink {
    /// Consumes one decoded frame.
    fn accept(&mut self, frame: DecodedFrame) -> Result<(), String>;
}

/// In-memory sink: counts and stores every frame. `delay` simulates a
/// slow consumer (e.g. a saturated database) for the backpressure test.
#[derive(Default)]
pub struct MemorySink {
    /// Every frame accepted, in arrival order.
    pub frames: Vec<DecodedFrame>,
    /// Artificial per-frame processing delay.
    pub delay: Option<std::time::Duration>,
}

impl Sink for MemorySink {
    fn accept(&mut self, frame: DecodedFrame) -> Result<(), String> {
        if let Some(delay) = self.delay {
            std::thread::sleep(delay);
        }
        self.frames.push(frame);
        Ok(())
    }
}

/// JSON-lines file sink: one `{"source":…,"from":…,"info":…}` object
/// per frame (info as lossy text with non-printables replaced).
pub struct JsonlSink<W: Write> {
    /// The underlying line writer.
    pub out: W,
}

impl<W: Write> Sink for JsonlSink<W> {
    fn accept(&mut self, frame: DecodedFrame) -> Result<(), String> {
        let info: String = frame
            .frame
            .info()
            .iter()
            .map(|&b| {
                if (b' '..=b'~').contains(&b) {
                    b as char
                } else {
                    '.'
                }
            })
            .collect();
        writeln!(
            self.out,
            "{{\"source\":{},\"from\":{},\"info\":{}}}",
            json_string(&frame.source),
            json_string(&frame.sender()),
            json_string(&info)
        )
        .map_err(|e| format!("writing the log: {e}"))
    }
}

/// Escapes a string as a JSON string literal.
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Decodes one WAV file, sending each frame through the bounded
/// channel. A full channel makes `send` block — that block IS the
/// backpressure that paces this worker to the sink.
fn decode_file(path: &str, frames: &SyncSender<DecodedFrame>) -> Result<u32, String> {
    let mut reader = hound::WavReader::open(path).map_err(|e| format!("opening '{path}': {e}"))?;
    let spec = reader.spec();
    if spec.channels != 1 || spec.bits_per_sample != 16 {
        return Err(format!("'{path}': 16-bit mono PCM WAV required"));
    }
    let rate = SampleRate::new(spec.sample_rate)
        .map_err(|e| format!("'{path}': unsupported rate: {e}"))?;
    let config = TncConfig::bell_202(rate).map_err(|e| format!("'{path}': {e}"))?;
    let mut rx = DefaultTncReceiver::new(config).map_err(|e| format!("'{path}': {e}"))?;
    let mut decoded = 0u32;
    for sample in reader.samples::<i16>() {
        let sample = sample.map_err(|e| format!("reading '{path}': {e}"))?;
        if let Some(frame) = rx.push_i16(sample) {
            // Copy out of the lending borrow (RxFrame borrows the
            // receiver's buffer until the next push) via the library's
            // OwnedFrame, then block on the bounded channel if the
            // sink is behind.
            let owned = DecodedFrame {
                source: path.to_owned(),
                frame: OwnedFrame::new(&frame).map_err(|e| format!("'{path}': {e}"))?,
            };
            decoded += 1;
            frames.send(owned).map_err(|_| "sink hung up".to_owned())?;
        }
    }
    Ok(decoded)
}

/// Runs the whole pipeline: `paths` decoded on a [`WORKERS`]-thread
/// pool, all frames funneled through one bounded channel into `sink`
/// on the calling thread. Returns the total number of decoded frames.
pub fn decode_pool(paths: &[String], sink: &mut dyn Sink) -> Result<u32, String> {
    let queue: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(paths.iter().cloned().collect()));
    let (frame_tx, frame_rx): (SyncSender<DecodedFrame>, Receiver<DecodedFrame>) =
        sync_channel(CHANNEL_DEPTH);

    let workers: Vec<_> = (0..WORKERS.min(paths.len().max(1)))
        .map(|_| {
            let queue = Arc::clone(&queue);
            let frame_tx = frame_tx.clone();
            std::thread::spawn(move || -> Result<u32, String> {
                let mut decoded = 0u32;
                loop {
                    let Some(path) = queue.lock().map_err(|_| "queue poisoned")?.pop_front() else {
                        return Ok(decoded);
                    };
                    decoded += decode_file(&path, &frame_tx)?;
                }
            })
        })
        .collect();
    // The sink loop below must see the channel close when the last
    // worker finishes, so the main thread's sender goes first.
    drop(frame_tx);

    // Sink loop on the calling thread: the single consumer. When it is
    // slow, the channel fills and the workers block in `send` — total
    // in-flight frames never exceed CHANNEL_DEPTH + one per worker.
    let mut sink_result = Ok(());
    for frame in frame_rx {
        if let Err(e) = sink.accept(frame) {
            sink_result = Err(e);
            break;
        }
    }

    let mut total = 0u32;
    for worker in workers {
        total += worker
            .join()
            .map_err(|_| "decode worker panicked".to_owned())??;
    }
    sink_result?;
    Ok(total)
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((log_path, paths)) = args.split_first() else {
        eprintln!("usage: decode_many_threads <out.jsonl> <input.wav>...");
        return std::process::ExitCode::FAILURE;
    };
    if paths.is_empty() {
        eprintln!("usage: decode_many_threads <out.jsonl> <input.wav>...");
        return std::process::ExitCode::FAILURE;
    }
    let file = match std::fs::File::create(log_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: creating '{log_path}': {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let mut sink = JsonlSink {
        out: std::io::BufWriter::new(file),
    };
    match decode_pool(paths, &mut sink) {
        Ok(total) => {
            eprintln!(
                "{total} frame(s) from {} file(s) -> {log_path}",
                paths.len()
            );
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
