//! `warble serve`: a KISS TNC bridging audio I/O to KISS byte streams.
//!
//! This file has two halves: the clap-facing command (`ServeArgs`,
//! [`serve_command`], the audio-edge resolution) and, inside the
//! nested [`serve`] module, the transport-agnostic bridge core.
//! `tests/serve.rs` includes THIS file via `#[path]` (together with
//! `shared.rs`) and drives [`serve::run_tcp`] / [`serve::run_stream`]
//! in-process, so the core must stay clap-free and free of
//! process-global I/O.

use clap::Args;

use warble::SampleRate;

use crate::shared::{ModemArgs, check_wav_spec, sniff_stdin_samples};

/// Arguments of `warble serve`: the KISS TNC bridge.
#[derive(Args)]
pub struct ServeArgs {
    /// Listen for KISS clients on this TCP address, e.g.
    /// `127.0.0.1:8001`. Up to 8 simultaneous clients: every received
    /// frame is broadcast to all of them, and any of them may submit
    /// frames for transmission. Exactly one of --tcp/--stdio is
    /// required.
    #[arg(long, value_name = "ADDR:PORT", conflicts_with = "stdio")]
    tcp: Option<String>,

    /// Speak one KISS stream on stdin/stdout instead of TCP (the
    /// classic direct-attach shape: point a host application straight
    /// at this process). --input/--output cannot be `-` in this mode.
    #[arg(long)]
    stdio: bool,

    /// RX audio: a 16-bit mono PCM WAV file (replay/testing), or `-`
    /// for stdin. Stdin is sniffed exactly like `warble decode -`: a
    /// RIFF/WAV header means WAV (rate from the header), anything else
    /// is raw s16le PCM (requires --sample-rate) — pipe a capture tool
    /// in for live audio (see the README pipe recipes and
    /// examples/live_capture.rs). EOF here is the graceful-shutdown
    /// signal.
    #[arg(long, value_name = "INPUT.wav | -")]
    input: String,

    /// TX audio: a 16-bit mono WAV file (appended to if it exists, so
    /// sessions accumulate), or `-` for raw s16le PCM on stdout — pipe
    /// it into a playback tool for live transmit. The bridge is
    /// half-duplex in spirit: TX audio is never looped back into the
    /// receiver; keying the radio around each burst is the operator's
    /// (or the playback pipeline's) job.
    #[arg(long, value_name = "OUTPUT.wav | -")]
    output: String,

    /// Sample rate in Hz [range: 8000..=48000]. Required for raw PCM
    /// input (`--input -` without a WAV header); for WAV input it
    /// defaults to (and must match) the WAV header's rate.
    #[arg(long = "sample-rate", visible_alias = "rate", value_name = "HZ")]
    sample_rate: Option<u32>,

    #[command(flatten)]
    modem: ModemArgs,
}

/// Transport-agnostic core of `warble serve`, kept clap-free and free
/// of process-global I/O so the whole bridge is testable in-process:
/// `tests/serve.rs` includes this file via `#[path]` and drives
/// [`serve::run_tcp`] on a loopback listener bound to an OS-assigned
/// port and [`serve::run_stream`] on in-memory buffers.
///
/// KISS framing comes from the library's [`warble::kiss`] module
/// (encoder iterator + streaming deframer) — nothing is reimplemented
/// here; this module is sockets, threads and channels only.
///
/// Concurrency shape (std::thread + bounded `sync_channel`s; this
/// bridge stays runtime-free by design — the opt-in `async` feature's
/// tokio adapter lives in `warble::asynk`, see `docs/ARCHITECTURE.md`):
///
/// ```text
/// audio in ─→ decode thread ─→ bounded chan ─→ broadcast thread ─→ every client
/// client N ─→ reader thread ─→ bounded chan ─→ TX writer (caller thread) ─→ audio out
/// ```
///
/// Half duplex: the bridge never mixes RX and TX — TX audio goes to its
/// own sink and is NOT looped back into the receiver; keying a real
/// radio around each burst is the playback pipeline's job. Graceful
/// shutdown: audio-input EOF closes every client socket, drains the
/// channels, joins the threads and returns the session counters.
// The inner module repeats the file name: it survives the bin split
// unchanged so `tests/serve.rs` keeps the same `…::serve::run_tcp`
// paths it used against the old monolith.
#[allow(clippy::module_inception)]
pub mod serve {
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::mpsc::{SyncSender, sync_channel};
    use std::sync::{Arc, Mutex};

    use warble::demodulator::{AfskDemodulator, DemodulatorConfig};
    use warble::fx25::{Fx25Receiver, WRAP_MAX, byte_bits, stuff_frame, wrap};
    use warble::kiss::{KissCommand, KissDeframer, KissPort, frame_iter};
    use warble::modulator::{Modulator, ModulatorConfig};
    use warble::nrzi::{self, NrziDecoder};
    use warble::tnc::{DefaultTncReceiver, MAX_FRAME_BYTES, TncConfig, TncTransmitter};

    /// At most this many simultaneous KISS clients; connections beyond
    /// the cap are closed immediately (documented limit — it keeps the
    /// broadcast list small and the thread count bounded).
    pub const MAX_CLIENTS: usize = 8;

    /// Depth of the bounded channels between the audio threads and the
    /// socket threads: deep enough to ride out bursts, small enough
    /// that a stalled peer exerts backpressure instead of growing
    /// memory without bound.
    pub const CHANNEL_DEPTH: usize = 32;

    /// KISS deframer capacity: command byte + the largest AX.25 frame
    /// body the modem accepts.
    const KISS_CAP: usize = MAX_FRAME_BYTES + 1;

    /// How often the accept loop polls for shutdown between connections.
    const ACCEPT_POLL: std::time::Duration = std::time::Duration::from_millis(25);

    /// Counters returned by a completed bridge run.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct ServeStats {
        /// Frames decoded from the audio input and offered to clients.
        pub rx_frames: u64,
        /// Client KISS data frames modulated into the audio output.
        pub tx_frames: u64,
    }

    /// Where TX audio goes (WAV file, raw PCM pipe, or a test buffer).
    pub trait SampleSink {
        /// Appends one burst of samples (one modulated frame).
        fn write_samples(&mut self, samples: &[i16]) -> Result<(), String>;
        /// Flushes/finalizes the sink at clean shutdown.
        fn finish(&mut self) -> Result<(), String>;
    }

    /// Raw s16le PCM sink over any writer (`--output -` = stdout).
    pub struct PcmSink<W: Write> {
        /// The underlying byte writer.
        pub out: W,
    }

    impl<W: Write> SampleSink for PcmSink<W> {
        fn write_samples(&mut self, samples: &[i16]) -> Result<(), String> {
            for s in samples {
                self.out
                    .write_all(&s.to_le_bytes())
                    .map_err(|e| format!("writing TX audio: {e}"))?;
            }
            self.out
                .flush()
                .map_err(|e| format!("writing TX audio: {e}"))
        }

        fn finish(&mut self) -> Result<(), String> {
            self.out
                .flush()
                .map_err(|e| format!("flushing TX audio: {e}"))
        }
    }

    /// 16-bit mono WAV file sink; appends to an existing file (so
    /// repeated sessions accumulate) or creates it at `rate`.
    pub struct WavSink {
        writer: Option<hound::WavWriter<std::io::BufWriter<std::fs::File>>>,
        path: String,
    }

    impl WavSink {
        /// Opens `path` for appending, creating it when absent.
        pub fn open(path: &str, rate: u32) -> Result<Self, String> {
            let writer = if std::path::Path::new(path).exists() {
                hound::WavWriter::append(path).map_err(|e| format!("appending '{path}': {e}"))?
            } else {
                let spec = hound::WavSpec {
                    channels: 1,
                    sample_rate: rate,
                    bits_per_sample: 16,
                    sample_format: hound::SampleFormat::Int,
                };
                hound::WavWriter::create(path, spec)
                    .map_err(|e| format!("creating '{path}': {e}"))?
            };
            Ok(Self {
                writer: Some(writer),
                path: path.to_owned(),
            })
        }
    }

    impl SampleSink for WavSink {
        fn write_samples(&mut self, samples: &[i16]) -> Result<(), String> {
            let path = self.path.clone();
            let writer = self
                .writer
                .as_mut()
                .ok_or_else(|| format!("'{path}': already finalized"))?;
            for &s in samples {
                writer
                    .write_sample(s)
                    .map_err(|e| format!("writing '{path}': {e}"))?;
            }
            Ok(())
        }

        fn finish(&mut self) -> Result<(), String> {
            if let Some(writer) = self.writer.take() {
                writer
                    .finalize()
                    .map_err(|e| format!("finalizing '{}': {e}", self.path))?;
            }
            Ok(())
        }
    }

    /// Sample-in, AX.25-frame-out receive front end: the plain
    /// multi-chain receiver, or the FX.25-aware tag hunter with `--fx25`.
    pub enum FrameDecoder {
        /// The default receive path.
        Plain(Box<DefaultTncReceiver>),
        /// The FX.25-aware path (also decodes plain AX.25 frames).
        Fx25 {
            /// Tone demodulator (FX.25 is tone-AFSK only).
            demod: Box<AfskDemodulator>,
            /// NRZI line decoder between demodulator and tag hunter.
            nrzi: NrziDecoder,
            /// The correlation-tag hunter with plain-HDLC fallback.
            rx: Box<Fx25Receiver<MAX_FRAME_BYTES>>,
        },
    }

    impl FrameDecoder {
        /// Builds the decoder for `config`, FX.25-aware when `fx25`.
        pub fn new(config: TncConfig, fx25: bool) -> Result<Self, String> {
            if fx25 {
                let demod_config =
                    DemodulatorConfig::new(config.sample_rate(), config.baud(), config.tones())
                        .map_err(|e| format!("receiver setup: {e}"))?;
                let demod = AfskDemodulator::new(demod_config)
                    .map_err(|e| format!("receiver setup: {e}"))?;
                Ok(FrameDecoder::Fx25 {
                    demod: Box::new(demod),
                    nrzi: NrziDecoder::default(),
                    rx: Box::new(Fx25Receiver::new()),
                })
            } else {
                Ok(FrameDecoder::Plain(Box::new(
                    DefaultTncReceiver::new(config).map_err(|e| format!("receiver setup: {e}"))?,
                )))
            }
        }

        /// Pushes one sample; returns a completed AX.25 frame body
        /// (without FCS — the KISS payload convention) when one decodes.
        pub fn push(&mut self, sample: i16) -> Option<Vec<u8>> {
            let mut buf = [0u8; MAX_FRAME_BYTES];
            match self {
                FrameDecoder::Plain(rx) => {
                    let frame = rx.push_i16(sample)?;
                    let len = frame.ui_frame().build(&mut buf).ok()?;
                    Some(buf.get(..len)?.to_vec())
                }
                FrameDecoder::Fx25 { demod, nrzi, rx } => {
                    let line = demod.push_sample_i16(sample)?;
                    let frame = match rx.push(nrzi.decode(line)) {
                        Some(Ok(frame)) => frame.to_vec(),
                        _ => return None,
                    };
                    let ui = warble::ax25::UiFrame::parse(&frame).ok()?;
                    let len = ui.build(&mut buf).ok()?;
                    Some(buf.get(..len)?.to_vec())
                }
            }
        }
    }

    /// Wraps an AX.25 frame body as one KISS data frame (port 0),
    /// using the library's lazy encoder.
    #[must_use]
    pub fn kiss_bytes(frame: &[u8]) -> Vec<u8> {
        frame_iter(KissPort::default(), KissCommand::Data, frame).collect()
    }

    /// Streaming KISS-side receive: wire bytes in, data payloads out.
    ///
    /// Non-data commands (TxDelay, Persistence, …) are accepted and
    /// ignored — this bridge has no radio timing hardware to configure
    /// — and malformed frames are dropped with re-sync, both per the
    /// KISS protocol's tolerance expectations.
    #[derive(Default)]
    pub struct KissExtractor {
        deframer: KissDeframer<KISS_CAP>,
    }

    impl KissExtractor {
        /// Pushes one wire byte; returns the payload of a completed
        /// KISS data frame.
        pub fn push(&mut self, byte: u8) -> Option<Vec<u8>> {
            match self.deframer.push(byte)? {
                Ok(frame) if frame.command() == KissCommand::Data => Some(frame.payload().to_vec()),
                _ => None,
            }
        }
    }

    /// Modulates an AX.25 frame body (KISS data payload, no FCS) into
    /// TX audio at the configured preset, FX.25-wrapped when `fx25`.
    pub fn frame_audio(config: TncConfig, fx25: bool, frame: &[u8]) -> Result<Vec<i16>, String> {
        if frame.is_empty() {
            return Err("empty KISS data frame".to_owned());
        }
        let tx = TncTransmitter::new(config);
        if !fx25 {
            return Ok(tx.frame_samples_i16(frame).collect());
        }
        let mut stuffed = [0u8; 2 * MAX_FRAME_BYTES];
        let stuffed_len =
            stuff_frame(frame, &mut stuffed).map_err(|e| format!("FX.25 framing: {e}"))?;
        let mut wrapped = [0u8; WRAP_MAX];
        let block = wrap(&stuffed[..stuffed_len], &mut wrapped)
            .map_err(|e| format!("FX.25 framing: {e}"))?;
        let mut bytes = vec![0x7Eu8; config.preamble_flags()];
        bytes.extend_from_slice(&wrapped[..block.len()]);
        bytes.extend(std::iter::repeat_n(0x7Eu8, config.tail_flags().max(2)));
        let modulator_config =
            ModulatorConfig::new(config.sample_rate(), config.baud(), config.tones())
                .map_err(|e| format!("transmitter setup: {e}"))?;
        Ok(Modulator::new(modulator_config)
            .i16_samples(nrzi::encode_iter(byte_bits(&bytes)))
            .collect())
    }

    /// Runs the TCP bridge on an already-bound listener until the
    /// audio input reaches EOF (or errors), then closes every client
    /// and returns the session counters.
    ///
    /// Taking a bound [`TcpListener`] (rather than an address string)
    /// lets tests bind `127.0.0.1:0` themselves and learn the
    /// OS-assigned port before starting the bridge.
    pub fn run_tcp<I, S>(
        listener: TcpListener,
        config: TncConfig,
        fx25: bool,
        rx_audio: I,
        tx_sink: &mut S,
    ) -> Result<ServeStats, String>
    where
        I: Iterator<Item = Result<i16, String>> + Send + 'static,
        S: SampleSink,
    {
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("listener setup: {e}"))?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let clients: Arc<Mutex<Vec<TcpStream>>> = Arc::new(Mutex::new(Vec::new()));
        let rx_frames = Arc::new(AtomicU64::new(0));
        let tx_frames = Arc::new(AtomicU64::new(0));

        // Received frames flow decode → broadcast over a BOUNDED
        // channel: a wedged client stalls the decode thread instead of
        // growing an unbounded queue.
        let (bc_tx, bc_rx) = sync_channel::<Vec<u8>>(CHANNEL_DEPTH);
        // Client TX audio flows reader → caller thread, also bounded.
        let (audio_tx, audio_rx) = sync_channel::<Vec<i16>>(CHANNEL_DEPTH);

        // Decode thread: audio samples → AX.25 frames → KISS bytes.
        let decode = {
            let shutdown = Arc::clone(&shutdown);
            let clients = Arc::clone(&clients);
            let rx_frames = Arc::clone(&rx_frames);
            std::thread::spawn(move || -> Result<(), String> {
                let mut decoder = FrameDecoder::new(config, fx25)?;
                let mut result = Ok(());
                for sample in rx_audio {
                    let sample = match sample {
                        Ok(s) => s,
                        Err(e) => {
                            result = Err(format!("audio input: {e}"));
                            break;
                        }
                    };
                    if let Some(frame) = decoder.push(sample) {
                        rx_frames.fetch_add(1, Ordering::Relaxed);
                        if bc_tx.send(kiss_bytes(&frame)).is_err() {
                            break; // broadcaster gone: shutting down
                        }
                    }
                }
                // Audio EOF is the graceful-shutdown trigger: stop the
                // accept loop and unblock every client reader.
                shutdown.store(true, Ordering::SeqCst);
                if let Ok(list) = clients.lock() {
                    for stream in list.iter() {
                        let _ = stream.shutdown(Shutdown::Both);
                    }
                }
                result
            })
        };

        // Broadcast thread: fans each encoded frame out to every
        // client, dropping clients whose sockets fail.
        let broadcaster = {
            let clients = Arc::clone(&clients);
            std::thread::spawn(move || {
                for bytes in bc_rx {
                    if let Ok(mut list) = clients.lock() {
                        list.retain_mut(|stream| {
                            stream
                                .write_all(&bytes)
                                .and_then(|()| stream.flush())
                                .is_ok()
                        });
                    }
                }
            })
        };

        // Accept thread: admits clients up to MAX_CLIENTS and spawns
        // one reader thread per client. Readers hold clones of
        // `audio_tx`; when the last clone drops, the TX loop below
        // sees the channel close.
        let acceptor = {
            let shutdown = Arc::clone(&shutdown);
            let clients = Arc::clone(&clients);
            let tx_frames = Arc::clone(&tx_frames);
            let audio_tx = audio_tx.clone();
            std::thread::spawn(move || {
                let mut readers = Vec::new();
                while !shutdown.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let admitted = {
                                let Ok(mut list) = clients.lock() else { break };
                                if list.len() >= MAX_CLIENTS {
                                    false
                                } else if let Ok(writer) = stream.try_clone() {
                                    list.push(writer);
                                    true
                                } else {
                                    false
                                }
                            };
                            if !admitted {
                                continue; // over the cap (or clone failed): drop
                            }
                            let _ = stream.set_nonblocking(false);
                            let audio_tx = audio_tx.clone();
                            let tx_frames = Arc::clone(&tx_frames);
                            readers.push(std::thread::spawn(move || {
                                client_reader(stream, config, fx25, &audio_tx, &tx_frames);
                            }));
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(ACCEPT_POLL);
                        }
                        Err(_) => break,
                    }
                }
                for reader in readers {
                    let _ = reader.join();
                }
            })
        };
        // Only reader threads (via the acceptor) hold senders now.
        drop(audio_tx);

        // TX loop on the caller's thread: single owner of the sink.
        let mut tx_result = Ok(());
        for samples in &audio_rx {
            if let Err(e) = tx_sink.write_samples(&samples) {
                tx_result = Err(e);
                shutdown.store(true, Ordering::SeqCst);
                break;
            }
        }
        let decode_result = decode
            .join()
            .map_err(|_| "decode thread panicked".to_owned())?;
        // Drain any TX bursts that raced the shutdown.
        for samples in audio_rx {
            if tx_result.is_ok()
                && let Err(e) = tx_sink.write_samples(&samples)
            {
                tx_result = Err(e);
            }
        }
        acceptor
            .join()
            .map_err(|_| "accept thread panicked".to_owned())?;
        broadcaster
            .join()
            .map_err(|_| "broadcast thread panicked".to_owned())?;
        tx_sink.finish()?;
        decode_result?;
        tx_result?;
        Ok(ServeStats {
            rx_frames: rx_frames.load(Ordering::Relaxed),
            tx_frames: tx_frames.load(Ordering::Relaxed),
        })
    }

    /// One client's reader loop: KISS bytes in, modulated TX audio
    /// bursts out through the bounded channel. Ends at socket
    /// EOF/error (which a shutdown of the socket also produces).
    fn client_reader(
        mut stream: TcpStream,
        config: TncConfig,
        fx25: bool,
        audio_tx: &SyncSender<Vec<i16>>,
        tx_frames: &AtomicU64,
    ) {
        let mut extractor = KissExtractor::default();
        let mut buf = [0u8; 1024];
        loop {
            let n = match stream.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            for &byte in buf.get(..n).unwrap_or(&[]) {
                if let Some(payload) = extractor.push(byte) {
                    // A payload the modem rejects is dropped; the
                    // bridge keeps serving.
                    if let Ok(samples) = frame_audio(config, fx25, &payload) {
                        tx_frames.fetch_add(1, Ordering::Relaxed);
                        if audio_tx.send(samples).is_err() {
                            return; // TX side gone: shutting down
                        }
                    }
                }
            }
        }
    }

    /// Runs the single-stream bridge (`--stdio`): one KISS stream
    /// in/out over any reader/writer pair.
    ///
    /// A decode thread turns audio into KISS frames on `kiss_out`; the
    /// caller's thread turns KISS data frames from `kiss_in` into TX
    /// audio. Returns once both the KISS input and the audio input
    /// reach EOF.
    pub fn run_stream<R, W, I, S>(
        mut kiss_in: R,
        mut kiss_out: W,
        config: TncConfig,
        fx25: bool,
        rx_audio: I,
        tx_sink: &mut S,
    ) -> Result<ServeStats, String>
    where
        R: Read,
        W: Write + Send + 'static,
        I: Iterator<Item = Result<i16, String>> + Send + 'static,
        S: SampleSink,
    {
        let decode = std::thread::spawn(move || -> Result<u64, String> {
            let mut decoder = FrameDecoder::new(config, fx25)?;
            let mut rx_frames = 0u64;
            for sample in rx_audio {
                let sample = sample.map_err(|e| format!("audio input: {e}"))?;
                if let Some(frame) = decoder.push(sample) {
                    rx_frames += 1;
                    kiss_out
                        .write_all(&kiss_bytes(&frame))
                        .and_then(|()| kiss_out.flush())
                        .map_err(|e| format!("writing the KISS stream: {e}"))?;
                }
            }
            Ok(rx_frames)
        });

        let mut extractor = KissExtractor::default();
        let mut tx_frames = 0u64;
        let mut buf = [0u8; 1024];
        let mut tx_result = Ok(());
        'kiss: loop {
            let n = match kiss_in.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    tx_result = Err(format!("reading the KISS stream: {e}"));
                    break;
                }
            };
            for &byte in buf.get(..n).unwrap_or(&[]) {
                if let Some(payload) = extractor.push(byte)
                    && let Ok(samples) = frame_audio(config, fx25, &payload)
                {
                    tx_frames += 1;
                    if let Err(e) = tx_sink.write_samples(&samples) {
                        tx_result = Err(e);
                        break 'kiss;
                    }
                }
            }
        }
        let rx_frames = decode
            .join()
            .map_err(|_| "decode thread panicked".to_owned())??;
        tx_sink.finish()?;
        tx_result?;
        Ok(ServeStats {
            rx_frames,
            tx_frames,
        })
    }
}

/// The audio-input side of `warble serve`, resolved from `--input`:
/// the validated sample rate plus a `Send + 'static` sample iterator.
type ServeAudio = (
    SampleRate,
    Box<dyn Iterator<Item = Result<i16, String>> + Send>,
);

fn serve_rx_audio(args: &ServeArgs) -> Result<ServeAudio, String> {
    if args.input == "-" {
        // Stdin is sniffed WAV-vs-raw exactly like `warble decode -`
        // (rate from a WAV header, contradiction check against
        // --sample-rate, raw s16le otherwise).
        return sniff_stdin_samples(std::io::stdin(), args.sample_rate);
    }
    let input = args.input.clone();
    let reader = hound::WavReader::open(&input).map_err(|e| format!("opening '{input}': {e}"))?;
    let rate = check_wav_spec(&reader.spec(), &input)?;
    if let Some(hz) = args.sample_rate
        && hz != rate.hz()
    {
        return Err(format!(
            "--sample-rate {hz} contradicts the WAV header of '{input}' ({} Hz); drop \
             the flag for WAV input",
            rate.hz()
        ));
    }
    let samples = reader
        .into_samples::<i16>()
        .map(move |s| s.map_err(|e| format!("reading '{input}': {e}")));
    Ok((rate, Box::new(samples)))
}

/// Runs `warble serve`: resolves the audio edges and dispatches to the
/// TCP or stdio bridge core. Exit codes follow the binary's
/// convention: 0 at clean audio EOF, 1 on any I/O or setup failure,
/// 2 for usage errors (via clap).
pub fn serve_command(args: &ServeArgs) -> Result<(), String> {
    if args.tcp.is_none() && !args.stdio {
        return Err("pick a transport: --tcp <ADDR:PORT> or --stdio".to_owned());
    }
    if args.stdio && (args.input == "-" || args.output == "-") {
        return Err(
            "--stdio uses stdin/stdout for the KISS stream, so --input/--output \
             cannot be '-' in this mode"
                .to_owned(),
        );
    }
    args.modem.reject_il2p("serve")?;
    let (rate, rx_audio) = serve_rx_audio(args)?;
    let config = args.modem.config(rate)?;
    let stats = if let Some(addr) = args.tcp.as_deref() {
        let listener =
            std::net::TcpListener::bind(addr).map_err(|e| format!("binding '{addr}': {e}"))?;
        if let Ok(local) = listener.local_addr() {
            eprintln!(
                "serving KISS on {local} (up to {} clients)",
                serve::MAX_CLIENTS
            );
        }
        if args.output == "-" {
            let mut sink = serve::PcmSink {
                out: std::io::BufWriter::new(std::io::stdout()),
            };
            serve::run_tcp(listener, config, args.modem.fx25, rx_audio, &mut sink)?
        } else {
            let mut sink = serve::WavSink::open(&args.output, rate.hz())?;
            serve::run_tcp(listener, config, args.modem.fx25, rx_audio, &mut sink)?
        }
    } else {
        let mut sink = serve::WavSink::open(&args.output, rate.hz())?;
        serve::run_stream(
            std::io::stdin().lock(),
            std::io::stdout(),
            config,
            args.modem.fx25,
            rx_audio,
            &mut sink,
        )?
    };
    eprintln!(
        "serve done: {} frame(s) received, {} frame(s) transmitted",
        stats.rx_frames, stats.tx_frames
    );
    Ok(())
}
