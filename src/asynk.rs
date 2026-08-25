//! Tokio adapters (`async` feature): decoded frames as `Stream`s, plus a
//! one-call async KISS server.
//!
//! The sync, allocation-free `no_std` core is the source of truth; this
//! module is a thin adapter over it. Every function here follows the same
//! plan: async I/O at the edges, the synchronous DSP on
//! [`tokio::task::spawn_blocking`], and a **bounded** channel in between
//! so a slow consumer stalls the decoder instead of growing a queue
//! (frames are never dropped). The bound is [`CHANNEL_CAPACITY`] frames
//! per stream.
//!
//! Frames cross the async boundary as [`OwnedFrame`]s — self-contained
//! copies of the receiver's lending [`crate::tnc::RxFrame`] — so they can
//! outlive the decode loop, move between tasks, and be sent to sinks.
//!
//! Everything here is opt-in: without the `async` cargo feature none of
//! this code (or its tokio/futures dependencies) is compiled.
//!
//! ```no_run
//! use tokio_stream::StreamExt;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let mut frames = std::pin::pin!(yodel::asynk::decode_wav("rx.wav"));
//!     while let Some(frame) = frames.next().await {
//!         println!("{}", String::from_utf8_lossy(frame?.info()));
//!     }
//!     Ok(())
//! }
//! ```
//!
//! # Piped audio
//!
//! Audio arriving on a pipe works the same as a file. Raw s16le PCM
//! from a capture tool on stdin (`your-capture-tool | my-app`) goes
//! through [`frames`] — raw PCM carries no sample rate, so you say it:
//!
//! ```no_run
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! use tokio_stream::StreamExt;
//! use yodel::{SampleRate, tnc::TncConfig};
//!
//! let cfg = TncConfig::bell_202(SampleRate::new(48_000)?)?;
//! let mut frames = std::pin::pin!(yodel::asynk::frames(tokio::io::stdin(), cfg));
//! while let Some(frame) = frames.next().await {
//!     println!("{}", String::from_utf8_lossy(frame?.info()));
//! }
//! # Ok(())
//! # }
//! ```
//!
//! When you do not know whether the pipe carries a WAV or raw PCM,
//! [`decode_stream`] (with the `wav` feature) sniffs the first four
//! bytes and does the right thing either way — the async twin of the
//! CLI's `yodel decode -`:
//!
//! ```no_run
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! use tokio_stream::StreamExt;
//!
//! // WAV takes its rate from the header; 48 kHz applies if raw.
//! let rate = yodel::SampleRate::new(48_000).ok();
//! let mut frames =
//!     std::pin::pin!(yodel::asynk::decode_stream(tokio::io::stdin(), rate));
//! while let Some(frame) = frames.next().await {
//!     println!("{}", String::from_utf8_lossy(frame?.info()));
//! }
//! # Ok(())
//! # }
//! ```
//!
//! No code at all: the same intake is one shell line away —
//! `your-capture-tool | yodel decode - --sample-rate 48000`.

use std::io;

use futures_core::Stream;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;

use crate::kiss::{KissCommand, KissPort, encode_into, encoded_len};
use crate::tnc::{DefaultTncReceiver, MAX_FRAME_BYTES, OwnedFrame, TncConfig};
#[cfg(feature = "wav")]
use crate::types::SampleRate;
#[cfg(feature = "wav")]
use crate::wav::WavError;

/// Bound of every frame channel in this module, in frames.
///
/// When a consumer falls this many frames behind, the decoding side
/// blocks (on its own blocking-pool thread) until the consumer catches
/// up: backpressure, never loss.
pub const CHANNEL_CAPACITY: usize = 64;

/// Read-chunk size of the [`frames`] / [`decode_many`] readers, in bytes.
const CHUNK_BYTES: usize = 8192;

/// Decodes a WAV file into a `Stream` of frames.
///
/// Opens `path` (16-bit mono integer PCM at a supported rate), runs the
/// whole file through a Bell 202 [`DefaultTncReceiver`] on a
/// [`tokio::task::spawn_blocking`] thread, and yields each FCS-valid
/// frame. Errors (opening the file, an unsupported header, a read
/// failure mid-file) arrive as the stream's final item.
///
/// Backpressure: the internal channel holds at most [`CHANNEL_CAPACITY`]
/// frames; a slow consumer stalls the decoder, losing nothing.
///
/// ```no_run
/// # async fn demo() -> Result<(), yodel::wav::WavError> {
/// use tokio_stream::StreamExt;
///
/// let mut frames = std::pin::pin!(yodel::asynk::decode_wav("rx.wav"));
/// while let Some(frame) = frames.next().await {
///     println!("{}", String::from_utf8_lossy(frame?.info()));
/// }
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "wav")]
pub fn decode_wav(
    path: impl AsRef<std::path::Path> + Send + 'static,
) -> impl Stream<Item = Result<OwnedFrame, WavError>> + Send {
    let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
    tokio::task::spawn_blocking(move || {
        let result = crate::wav::decode_frames(path, |frame| tx.blocking_send(Ok(frame)).is_ok());
        if let Err(e) = result {
            // Best effort: the consumer may already be gone.
            let _ = tx.blocking_send(Err(e));
        }
    });
    ReceiverStream::new(rx)
}

/// Decodes a piped audio stream — WAV or raw s16le PCM, told apart by
/// sniffing the first four bytes — into a `Stream` of frames.
///
/// This is the async twin of `yodel decode -`: point it at any
/// [`AsyncRead`] (tokio stdin, a socket, a file) and the intake
/// classifies the bytes exactly the way the CLI does, via the shared
/// [`crate::wav::sniff_pcm`]. A stream opening with `RIFF` is parsed
/// as WAV — the sample rate comes from the header, validated by
/// [`crate::wav::check_spec`] — and anything else is raw signed 16-bit
/// little-endian mono PCM at the rate in `rate`.
///
/// `rate` is required for raw streams (raw PCM carries no rate) and
/// optional for WAV. Contradiction check: when `rate` is given AND the
/// stream has a WAV header, the two must agree, or the stream's only
/// item is a [`WavError::RateContradiction`]. A raw stream without
/// `rate` yields a single [`WavError::RateRequired`].
///
/// Frames decode with a Bell 202 receiver at the resolved rate. As
/// everywhere in this module, the DSP runs on
/// [`tokio::task::spawn_blocking`] and the channels are bounded
/// ([`CHANNEL_CAPACITY`] frames): a slow consumer stalls the decoder,
/// losing nothing.
///
/// ```no_run
/// # async fn demo() -> Result<(), yodel::wav::WavError> {
/// use tokio_stream::StreamExt;
///
/// // WAV or raw PCM piped to stdin; 48 kHz applies if it is raw.
/// let rate = yodel::SampleRate::new(48_000).ok();
/// let mut frames = std::pin::pin!(yodel::asynk::decode_stream(
///     tokio::io::stdin(),
///     rate,
/// ));
/// while let Some(frame) = frames.next().await {
///     println!("{}", String::from_utf8_lossy(frame?.info()));
/// }
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "wav")]
pub fn decode_stream<R>(
    reader: R,
    rate: Option<SampleRate>,
) -> impl Stream<Item = Result<OwnedFrame, WavError>> + Send
where
    R: AsyncRead + Send + Unpin + 'static,
{
    let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
    let (chunk_tx, chunk_rx) = mpsc::channel::<io::Result<Vec<u8>>>(4);
    // Async edge: byte chunks off the reader into a bounded channel.
    tokio::spawn(pump_bytes(reader, chunk_tx));
    // Blocking side: the shared sync sniff + decode over those chunks.
    tokio::task::spawn_blocking(move || {
        let reader = ChunkReader {
            chunks: chunk_rx,
            buffer: Vec::new(),
            pos: 0,
        };
        let result = crate::wav::sniff_pcm(reader, rate).and_then(|sniffed| {
            crate::wav::decode_sniffed(sniffed, |frame| tx.blocking_send(Ok(frame)).is_ok())
        });
        if let Err(e) = result {
            // Best effort: the consumer may already be gone.
            let _ = tx.blocking_send(Err(e));
        }
    });
    ReceiverStream::new(rx)
}

/// Async edge of [`decode_stream`]: reads chunks and forwards them
/// (or the read error) into the bounded chunk channel.
#[cfg(feature = "wav")]
async fn pump_bytes<R>(mut reader: R, chunk_tx: mpsc::Sender<io::Result<Vec<u8>>>)
where
    R: AsyncRead + Send + Unpin + 'static,
{
    let mut buf = vec![0u8; CHUNK_BYTES];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => return,
            Ok(n) => {
                if chunk_tx.send(Ok(buf[..n].to_vec())).await.is_err() {
                    return;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => {
                let _ = chunk_tx.send(Err(e)).await;
                return;
            }
        }
    }
}

/// Blocking-side [`io::Read`] over the chunk channel of
/// [`decode_stream`]: hands the shared sync sniff/decode code a plain
/// reader, with the channel bound providing the backpressure.
#[cfg(feature = "wav")]
struct ChunkReader {
    chunks: mpsc::Receiver<io::Result<Vec<u8>>>,
    buffer: Vec<u8>,
    pos: usize,
}

#[cfg(feature = "wav")]
impl io::Read for ChunkReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.pos >= self.buffer.len() {
            match self.chunks.blocking_recv() {
                Some(Ok(chunk)) => {
                    self.buffer = chunk;
                    self.pos = 0;
                }
                Some(Err(e)) => return Err(e),
                None => return Ok(0),
            }
        }
        let n = out.len().min(self.buffer.len() - self.pos);
        out[..n].copy_from_slice(&self.buffer[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// Decodes raw signed 16-bit little-endian mono PCM from any async
/// reader into a `Stream` of frames.
///
/// Byte chunks are read on the async side; the DSP runs on a
/// [`tokio::task::spawn_blocking`] thread with a Bell 202-style receiver
/// built from `config`. Each FCS-valid frame is yielded as it completes;
/// a read error ends the stream with that error as its final item. A
/// trailing odd byte at EOF is reported as an
/// [`io::ErrorKind::UnexpectedEof`] error rather than silently dropped.
///
/// Backpressure: bounded channels ([`CHANNEL_CAPACITY`] frames) on both
/// the chunk and the frame side; a slow consumer stalls the reader.
///
/// ```no_run
/// # async fn demo() -> std::io::Result<()> {
/// use tokio_stream::StreamExt;
/// use yodel::SampleRate;
/// use yodel::tnc::TncConfig;
///
/// let cfg = TncConfig::bell_202(SampleRate::new(48_000).unwrap()).unwrap();
/// let pcm = tokio::fs::File::open("rx.s16le").await?;
/// let mut frames = std::pin::pin!(yodel::asynk::frames(pcm, cfg));
/// while let Some(frame) = frames.next().await {
///     println!("{}", String::from_utf8_lossy(frame?.info()));
/// }
/// # Ok(())
/// # }
/// ```
pub fn frames<R>(
    reader: R,
    config: TncConfig,
) -> impl Stream<Item = Result<OwnedFrame, io::Error>> + Send
where
    R: AsyncRead + Send + Unpin + 'static,
{
    let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
    tokio::spawn(pump_feed(reader, config, tx, ()));
    ReceiverStream::new(rx).map(|((), item)| item)
}

/// Decodes many raw-PCM feeds concurrently into one merged `Stream`.
///
/// Each feed is an independent async reader of signed 16-bit
/// little-endian mono PCM, all decoded with the same `config`. Every
/// feed gets its own receiver on its own blocking-pool thread; the
/// decoded frames merge into a single stream of `(feed_index, result)`
/// pairs, where `feed_index` is the feed's position in `feeds` (order
/// within one feed is preserved; interleaving between feeds is
/// arbitrary). A feed's read error is its final item; other feeds keep
/// going.
///
/// Backpressure: one shared bounded channel ([`CHANNEL_CAPACITY`]
/// frames); a slow consumer stalls every feed, losing nothing.
///
/// ```no_run
/// # async fn demo() -> std::io::Result<()> {
/// use tokio_stream::StreamExt;
/// use yodel::SampleRate;
/// use yodel::tnc::TncConfig;
///
/// let cfg = TncConfig::bell_202(SampleRate::new(48_000).unwrap()).unwrap();
/// let a = tokio::fs::File::open("feed-a.s16le").await?;
/// let b = tokio::fs::File::open("feed-b.s16le").await?;
/// let mut frames = std::pin::pin!(yodel::asynk::decode_many([a, b], cfg));
/// while let Some((feed, frame)) = frames.next().await {
///     println!("feed {feed}: {}", String::from_utf8_lossy(frame?.info()));
/// }
/// # Ok(())
/// # }
/// ```
pub fn decode_many<I, R>(
    feeds: I,
    config: TncConfig,
) -> impl Stream<Item = (usize, Result<OwnedFrame, io::Error>)> + Send
where
    I: IntoIterator<Item = R>,
    R: AsyncRead + Send + Unpin + 'static,
{
    let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
    for (index, feed) in feeds.into_iter().enumerate() {
        tokio::spawn(pump_feed(feed, config, tx.clone(), index));
    }
    ReceiverStream::new(rx)
}

/// Shared engine of [`frames`] and [`decode_many`]: reads byte chunks
/// from one async reader and runs the receiver on the blocking pool,
/// tagging every emitted item with `tag`.
async fn pump_feed<R, T>(
    mut reader: R,
    config: TncConfig,
    out: mpsc::Sender<(T, Result<OwnedFrame, io::Error>)>,
    tag: T,
) where
    R: AsyncRead + Send + Unpin + 'static,
    T: Copy + Send + 'static,
{
    let (chunk_tx, mut chunk_rx) = mpsc::channel::<Vec<u8>>(4);
    let worker_out = out.clone();
    let worker = tokio::task::spawn_blocking(move || {
        let out = worker_out;
        let mut rx = match DefaultTncReceiver::new(config) {
            Ok(rx) => rx,
            // TncConfig is validated at construction; defensive only.
            Err(_) => return,
        };
        // Carry for a sample split across two chunks.
        let mut carry: Option<u8> = None;
        while let Some(chunk) = chunk_rx.blocking_recv() {
            let mut bytes = chunk.iter().copied();
            if let Some(lo) = carry.take()
                && let Some(hi) = bytes.next()
                && !push_sample(&mut rx, [lo, hi], &out, tag)
            {
                return;
            }
            while let Some(lo) = bytes.next() {
                match bytes.next() {
                    Some(hi) => {
                        if !push_sample(&mut rx, [lo, hi], &out, tag) {
                            return;
                        }
                    }
                    None => {
                        carry = Some(lo);
                        break;
                    }
                }
            }
        }
        if carry.is_some() {
            let e = io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated sample (odd byte count) at EOF",
            );
            let _ = out.blocking_send((tag, Err(e)));
        }
    });
    let mut buf = vec![0u8; CHUNK_BYTES];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                if chunk_tx.send(buf[..n].to_vec()).await.is_err() {
                    break;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => {
                // Let the worker drain what it already has, then
                // report the read error as the feed's final item.
                drop(chunk_tx);
                let _ = worker.await;
                let _ = out.send((tag, Err(e))).await;
                return;
            }
        }
    }
    drop(chunk_tx);
    let _ = worker.await;
}

/// Pushes one little-endian sample; returns `false` when the consumer
/// hung up (the stream was dropped) and decoding should stop.
fn push_sample<T: Copy>(
    rx: &mut DefaultTncReceiver,
    bytes: [u8; 2],
    out: &mpsc::Sender<(T, Result<OwnedFrame, io::Error>)>,
    tag: T,
) -> bool {
    if let Some(frame) = rx.push_i16(i16::from_le_bytes(bytes)) {
        let Ok(owned) = OwnedFrame::new(&frame) else {
            return true;
        };
        return out.blocking_send((tag, Ok(owned))).is_ok();
    }
    true
}

/// Serves decoded frames to KISS-over-TCP clients, one call.
///
/// Accepts connections on `listener` and broadcasts every frame from
/// `frames`, KISS-encoded as a port-0 data frame (the wire format APRS
/// applications speak as a "network KISS TNC"), to every connected
/// client. A client that stops reading or disconnects is dropped;
/// everyone else keeps receiving. Returns once `frames` ends.
///
/// Bind the listener yourself — an ephemeral `127.0.0.1:0` in tests, a
/// real address in production:
///
/// ```no_run
/// # async fn demo() -> std::io::Result<()> {
/// let listener = tokio::net::TcpListener::bind("127.0.0.1:8001").await?;
/// let frames = yodel::asynk::decode_wav("rx.wav")
///     .filter_map(|r| r.ok());
/// yodel::asynk::serve_kiss(listener, frames).await?;
/// # Ok(())
/// # }
/// # use tokio_stream::StreamExt;
/// ```
///
/// # Errors
///
/// An [`io::Error`] from accepting a connection. Per-client write
/// failures are not errors: the failing client is dropped.
pub async fn serve_kiss(
    listener: TcpListener,
    frames: impl Stream<Item = OwnedFrame>,
) -> io::Result<()> {
    let mut frames = std::pin::pin!(frames);
    let mut clients: Vec<tokio::net::TcpStream> = Vec::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                clients.push(stream);
            }
            frame = frames.next() => {
                let Some(frame) = frame else { return Ok(()) };
                let Some(bytes) = kiss_bytes(&frame) else { continue };
                let mut keep = Vec::with_capacity(clients.len());
                for mut client in clients.drain(..) {
                    if client.write_all(&bytes).await.is_ok() {
                        keep.push(client);
                    }
                }
                clients = keep;
            }
        }
    }
}

/// Serializes a frame as one KISS port-0 data frame; `None` when the
/// frame cannot be rebuilt (not reachable for frames this module
/// decodes).
fn kiss_bytes(frame: &OwnedFrame) -> Option<Vec<u8>> {
    // The AX.25 frame body: addresses (up to 72 bytes) + control + PID
    // + info (capped at MAX_FRAME_BYTES).
    let mut body = [0u8; MAX_FRAME_BYTES + 80];
    let len = frame.ui_frame().ok()?.build(&mut body).ok()?;
    let body = body.get(..len)?;
    let port = KissPort::new(0).ok()?;
    let mut out = vec![0u8; encoded_len(port, KissCommand::Data, body)];
    let written = encode_into(port, KissCommand::Data, body, &mut out).ok()?;
    out.truncate(written);
    Some(out)
}
