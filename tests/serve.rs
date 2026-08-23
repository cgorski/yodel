//! In-process proof of the `warble serve` bridge core.
//!
//! The server core lives in the binary (`src/bin/warble/serve.rs`,
//! `mod serve`) as transport glue with no process-global I/O, so this
//! test includes the SAME sources via `#[path]` (the technique of
//! `tests/app_examples.rs`) and drives it directly:
//!
//! * TCP loopback on an OS-assigned port (`127.0.0.1:0`): RX audio in →
//!   the client receives the correct KISS frame; a client KISS frame in
//!   → the TX audio sink contains a decodable transmission;
//! * two clients: both receive the broadcast of one received frame;
//! * clean shutdown: audio EOF ends the bridge, every join guarded by
//!   a timeout so CI can never hang;
//! * the stdio shape (`run_stream`) on in-memory buffers.
#![cfg(all(
    feature = "cli",
    feature = "std",
    feature = "tnc",
    feature = "micE",
    feature = "kiss",
    feature = "fx25",
    feature = "wav"
))]

// The binary's `serve.rs` reaches its `shared` sibling through
// `crate::shared`, so the include mirrors the binary's module layout
// at this test crate's root: `shared` first, then the serve module.
#[path = "../src/bin/warble/shared.rs"]
#[allow(dead_code, unused_imports)]
mod shared;

#[path = "../src/bin/warble/serve.rs"]
#[allow(dead_code, unused_imports)]
mod warble_bin;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use warble_bin::serve::{PcmSink, SampleSink, ServeStats, kiss_bytes, run_stream, run_tcp};

use warble::SampleRate;
use warble::ax25::{Address, UiFrame};
use warble::kiss::{KissCommand, KissDeframer};
use warble::tnc::{DefaultTncReceiver, TncConfig, TncReceiver, TncTransmitter};

/// Everything in this file must finish well within this bound.
const DEADLINE: Duration = Duration::from_secs(30);

fn addr(call: &[u8], ssid: u8) -> Address {
    Address::new(call, ssid).unwrap()
}

fn config() -> TncConfig {
    TncConfig::bell_202(SampleRate::new(48_000).unwrap()).unwrap()
}

/// Builds the AX.25 frame body (no FCS) for a raw info payload.
fn frame_body(src_ssid: u8, info: &[u8]) -> Vec<u8> {
    let frame = UiFrame::new(addr(b"APRS", 0), addr(b"N0CALL", src_ssid), info);
    let mut buf = [0u8; 330];
    let len = frame.build(&mut buf).unwrap();
    buf[..len].to_vec()
}

/// Renders a frame body to Bell 202 samples (the RX stimulus).
fn frame_samples(body: &[u8]) -> Vec<i16> {
    TncTransmitter::new(config())
        .frame_samples_i16(body)
        .collect()
}

/// Decodes every frame body out of a sample stream.
fn decode_all(samples: &[i16]) -> Vec<Vec<u8>> {
    let mut rx: DefaultTncReceiver = TncReceiver::new(config()).unwrap();
    let mut out = Vec::new();
    for &s in samples {
        if let Some(frame) = rx.push_i16(s) {
            let mut buf = [0u8; 330];
            let len = frame.ui_frame().build(&mut buf).unwrap();
            out.push(buf[..len].to_vec());
        }
    }
    out
}

/// A `SampleSink` collecting into a shared vector the test can poll.
#[derive(Clone, Default)]
struct SharedSink(Arc<Mutex<Vec<i16>>>);

impl SampleSink for SharedSink {
    fn write_samples(&mut self, samples: &[i16]) -> Result<(), String> {
        self.0.lock().unwrap().extend_from_slice(samples);
        Ok(())
    }
    fn finish(&mut self) -> Result<(), String> {
        Ok(())
    }
}

/// Audio input the test paces: bursts pushed through a channel come out
/// as samples; dropping the sender is EOF (the shutdown trigger).
struct PacedAudio {
    rx: mpsc::Receiver<Vec<i16>>,
    current: std::vec::IntoIter<i16>,
}

impl Iterator for PacedAudio {
    type Item = Result<i16, String>;
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(s) = self.current.next() {
                return Some(Ok(s));
            }
            match self.rx.recv() {
                Ok(burst) => self.current = burst.into_iter(),
                Err(_) => return None,
            }
        }
    }
}

/// Handles the test drives a running TCP bridge by: port, audio
/// feeder, shared TX sink, and the bridge's result channel.
type BridgeHandles = (
    u16,
    mpsc::SyncSender<Vec<i16>>,
    SharedSink,
    mpsc::Receiver<Result<ServeStats, String>>,
);

/// Starts the TCP bridge on an OS-assigned loopback port. Returns the
/// port, the audio feeder, the shared TX sink, and the result channel.
fn start_tcp_bridge() -> BridgeHandles {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (audio_tx, audio_rx) = mpsc::sync_channel::<Vec<i16>>(16);
    let sink = SharedSink::default();
    let (done_tx, done_rx) = mpsc::channel();
    let mut bridge_sink = sink.clone();
    std::thread::spawn(move || {
        let rx_audio = PacedAudio {
            rx: audio_rx,
            current: Vec::new().into_iter(),
        };
        let result = run_tcp(listener, config(), false, rx_audio, &mut bridge_sink);
        let _ = done_tx.send(result);
    });
    (port, audio_tx, sink, done_rx)
}

/// Connects a client and waits until the bridge has admitted it (the
/// accept loop polls, so a fresh connection needs a beat before it is
/// on the broadcast list).
fn connect(port: u16) -> TcpStream {
    let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(DEADLINE)).unwrap();
    // The accept loop polls every ~25 ms; give it time to register the
    // client before any frame is broadcast.
    std::thread::sleep(Duration::from_millis(200));
    stream
}

/// Reads one complete KISS data frame from a stream (with the read
/// timeout as the hang guard) and returns its payload.
fn read_kiss_frame(stream: &mut TcpStream) -> Vec<u8> {
    let mut deframer = KissDeframer::<400>::new();
    let mut byte = [0u8; 1];
    let start = Instant::now();
    loop {
        assert!(start.elapsed() < DEADLINE, "timed out reading a KISS frame");
        match stream.read(&mut byte) {
            Ok(0) => panic!("socket closed before a KISS frame arrived"),
            Ok(_) => {
                if let Some(result) = deframer.push(byte[0]) {
                    let frame = result.expect("well-formed KISS frame");
                    assert_eq!(frame.command(), KissCommand::Data);
                    return frame.payload().to_vec();
                }
            }
            Err(e) => panic!("reading the client socket: {e}"),
        }
    }
}

/// Polls the shared TX sink until `pred` holds (or the deadline hits).
fn wait_for_sink(sink: &SharedSink, pred: impl Fn(&[i16]) -> bool) -> Vec<i16> {
    let start = Instant::now();
    loop {
        {
            let samples = sink.0.lock().unwrap();
            if pred(&samples) {
                return samples.clone();
            }
        }
        assert!(start.elapsed() < DEADLINE, "timed out waiting for TX audio");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The full TCP loop: RX audio → client KISS frame, client KISS frame
/// → TX audio that decodes back — plus clean shutdown at audio EOF.
#[test]
fn tcp_bridge_round_trips_both_directions() {
    let (port, audio_tx, sink, done_rx) = start_tcp_bridge();
    let mut client = connect(port);

    // RX direction: audio in, KISS frame out to the client.
    let rx_body = frame_body(1, b">rx via radio");
    audio_tx.send(frame_samples(&rx_body)).unwrap();
    assert_eq!(read_kiss_frame(&mut client), rx_body);

    // TX direction: KISS frame in, modulated audio in the sink.
    let tx_body = frame_body(2, b">tx via client");
    client.write_all(&kiss_bytes(&tx_body)).unwrap();
    client.flush().unwrap();
    let samples = wait_for_sink(&sink, |s| decode_all(s).len() == 1);
    assert_eq!(decode_all(&samples), vec![tx_body]);

    // Clean shutdown: audio EOF ends the bridge and closes the client.
    drop(audio_tx);
    let stats = done_rx
        .recv_timeout(DEADLINE)
        .expect("bridge must shut down at audio EOF")
        .expect("bridge must exit cleanly");
    assert_eq!(
        stats,
        ServeStats {
            rx_frames: 1,
            tx_frames: 1
        }
    );
    // The socket is closed: the next read reaches EOF.
    let mut rest = Vec::new();
    assert_eq!(client.read_to_end(&mut rest).unwrap_or(0), rest.len());
}

/// Two connected clients BOTH receive the KISS frame of one received
/// transmission (the broadcast path), and either may transmit.
#[test]
fn tcp_bridge_broadcasts_to_every_client() {
    let (port, audio_tx, sink, done_rx) = start_tcp_bridge();
    let mut first = connect(port);
    let mut second = connect(port);

    let rx_body = frame_body(3, b">to everyone");
    audio_tx.send(frame_samples(&rx_body)).unwrap();
    assert_eq!(read_kiss_frame(&mut first), rx_body);
    assert_eq!(read_kiss_frame(&mut second), rx_body);

    // The second client transmits; the frame reaches the audio sink.
    let tx_body = frame_body(4, b">second speaks");
    second.write_all(&kiss_bytes(&tx_body)).unwrap();
    second.flush().unwrap();
    let samples = wait_for_sink(&sink, |s| decode_all(s).len() == 1);
    assert_eq!(decode_all(&samples), vec![tx_body]);

    drop(audio_tx);
    let stats = done_rx
        .recv_timeout(DEADLINE)
        .expect("bridge must shut down")
        .expect("bridge must exit cleanly");
    assert_eq!(
        stats,
        ServeStats {
            rx_frames: 1,
            tx_frames: 1
        }
    );
}

/// A client disconnecting mid-session never wedges the bridge: the
/// remaining client still receives frames and shutdown stays clean.
#[test]
fn tcp_bridge_survives_client_disconnect() {
    let (port, audio_tx, _sink, done_rx) = start_tcp_bridge();
    let leaver = connect(port);
    let mut stayer = connect(port);
    drop(leaver);

    let rx_body = frame_body(5, b">still here");
    audio_tx.send(frame_samples(&rx_body)).unwrap();
    assert_eq!(read_kiss_frame(&mut stayer), rx_body);

    drop(audio_tx);
    assert!(
        done_rx
            .recv_timeout(DEADLINE)
            .expect("bridge must shut down")
            .is_ok()
    );
}

/// Shutdown with no clients at all: EOF on an idle bridge returns
/// promptly (the accept loop's poll notices the flag).
#[test]
fn tcp_bridge_shuts_down_without_clients() {
    let (_port, audio_tx, _sink, done_rx) = start_tcp_bridge();
    drop(audio_tx);
    match done_rx.recv_timeout(DEADLINE) {
        Ok(result) => assert_eq!(result.unwrap(), ServeStats::default()),
        Err(RecvTimeoutError::Timeout) => panic!("idle bridge failed to shut down"),
        Err(e) => panic!("bridge thread lost: {e}"),
    }
}

/// The stdio shape on in-memory buffers: KISS in on a byte slice, KISS
/// out into a shared vector, audio both ways — both directions round-trip
/// and the run terminates at EOF on both inputs.
#[test]
fn stream_bridge_round_trips_in_memory() {
    /// `Write` into a shared vector (the KISS output side).
    #[derive(Clone, Default)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);
    impl Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let rx_body = frame_body(6, b">over the air");
    let tx_body = frame_body(7, b">from the host");
    let kiss_in = kiss_bytes(&tx_body);
    let kiss_out = SharedWriter::default();
    let mut tx_audio: Vec<u8> = Vec::new();
    let rx_audio = frame_samples(&rx_body).into_iter().map(Ok);

    let stats = {
        let mut sink = PcmSink { out: &mut tx_audio };
        run_stream(
            &kiss_in[..],
            kiss_out.clone(),
            config(),
            false,
            rx_audio,
            &mut sink,
        )
        .expect("the stream bridge must run to EOF")
    };
    assert_eq!(
        stats,
        ServeStats {
            rx_frames: 1,
            tx_frames: 1
        }
    );

    // KISS output carries the received frame.
    let mut deframer = KissDeframer::<400>::new();
    let mut heard = Vec::new();
    for &byte in kiss_out.0.lock().unwrap().iter() {
        if let Some(Ok(frame)) = deframer.push(byte) {
            assert_eq!(frame.command(), KissCommand::Data);
            heard.push(frame.payload().to_vec());
        }
    }
    assert_eq!(heard, vec![rx_body]);

    // TX audio (raw s16le) decodes back to the host's frame.
    let samples: Vec<i16> = tx_audio
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();
    assert_eq!(decode_all(&samples), vec![tx_body]);
}
