//! Tests of the `async` feature's tokio adapter layer (`yodel::asynk`):
//! stream decode of a synthesized signal, sync/async equivalence,
//! many-feeds attribution, the KISS server over a loopback connection,
//! and backpressure with a slow consumer. No devices, no fixed ports:
//! all I/O is in-process (`tokio::io::duplex`, `127.0.0.1:0`).
#![cfg(feature = "async")]

use tokio::io::AsyncReadExt;
use tokio_stream::StreamExt;

use yodel::SampleRate;
use yodel::aprs::{AprsPacket, Status};
use yodel::ax25::Address;
use yodel::kiss::KissDeframer;
use yodel::tnc::{DefaultTncReceiver, OwnedFrame, TncConfig, TncTransmitter};

/// The shared Bell 202 configuration at 48 kHz.
fn config() -> TncConfig {
    TncConfig::bell_202(SampleRate::new(48_000).unwrap()).unwrap()
}

/// Synthesizes one status-frame transmission from `src` as i16 samples.
fn transmission(src: &str, text: &[u8]) -> Vec<i16> {
    let tx = TncTransmitter::new(config());
    tx.transmit_to_vec_i16(
        &AprsPacket::Status(Status { text }),
        Address::new(b"APRS", 0).unwrap(),
        Address::new(src.as_bytes(), 0).unwrap(),
        &[],
    )
    .unwrap()
}

/// The samples as little-endian PCM bytes.
fn pcm_bytes(samples: &[i16]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

/// Decodes samples on the sync path, collecting owned frames.
fn sync_decode(samples: &[i16]) -> Vec<OwnedFrame> {
    let mut rx = DefaultTncReceiver::new(config()).unwrap();
    let mut frames = Vec::new();
    for &s in samples {
        if let Some(frame) = rx.push_i16(s) {
            frames.push(OwnedFrame::new(&frame).unwrap());
        }
    }
    frames
}

/// A synthesized signal fed through `asynk::frames` decodes to the
/// expected frame.
#[tokio::test]
async fn frames_decodes_synthesized_signal() {
    let samples = transmission("N0CALL", b"QRV async");
    let (mut a, b) = tokio::io::duplex(4096);
    let writer = tokio::spawn(async move {
        tokio::io::AsyncWriteExt::write_all(&mut a, &pcm_bytes(&samples))
            .await
            .unwrap();
        // Dropping `a` closes the pipe (EOF for the decoder).
    });
    let mut frames = std::pin::pin!(yodel::asynk::frames(b, config()));
    let frame = frames.next().await.expect("one frame").expect("no error");
    assert_eq!(frame.src().callsign.as_bytes(), b"N0CALL");
    assert_eq!(frame.info(), b">QRV async");
    assert!(frames.next().await.is_none(), "stream ends at EOF");
    writer.await.unwrap();
}

/// The async path yields byte-identical frames to the sync path on the
/// same input.
#[tokio::test]
async fn async_decode_matches_sync_decode() {
    // Several transmissions back to back, with silence gaps.
    let mut samples = Vec::new();
    for (i, text) in [&b"first"[..], b"second one", b"third frame"]
        .iter()
        .enumerate()
    {
        samples.extend(transmission(&format!("CALL{i}"), text));
        samples.extend(std::iter::repeat_n(0i16, 4_000));
    }
    let expected = sync_decode(&samples);
    assert_eq!(expected.len(), 3, "sync path decodes all three");

    let (mut a, b) = tokio::io::duplex(1024);
    let bytes = pcm_bytes(&samples);
    let writer = tokio::spawn(async move {
        tokio::io::AsyncWriteExt::write_all(&mut a, &bytes)
            .await
            .unwrap();
    });
    let got: Vec<OwnedFrame> = yodel::asynk::frames(b, config())
        .map(|r| r.expect("no error"))
        .collect()
        .await;
    writer.await.unwrap();

    assert_eq!(got.len(), expected.len());
    for (a, b) in got.iter().zip(expected.iter()) {
        assert_eq!(a, b, "async frame differs from sync frame");
        assert_eq!(a.info(), b.info());
        assert_eq!(a.src(), b.src());
        assert_eq!(a.dest(), b.dest());
        assert_eq!(a.hops(), b.hops());
    }
}

/// `decode_wav` yields byte-identical frames to the sync path on the
/// same WAV file.
#[cfg(feature = "wav")]
#[tokio::test]
async fn decode_wav_matches_sync_decode() {
    let mut samples = transmission("W1AW", b"wav equivalence");
    samples.extend(std::iter::repeat_n(0i16, 2_000));
    samples.extend(transmission("K2XYZ", b"second"));
    let expected = sync_decode(&samples);
    assert_eq!(expected.len(), 2);

    let path = std::env::temp_dir().join(format!("yodel-asynk-{}.wav", std::process::id()));
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 48_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&path, spec).unwrap();
    for &s in &samples {
        writer.write_sample(s).unwrap();
    }
    writer.finalize().unwrap();

    let got: Vec<OwnedFrame> = yodel::asynk::decode_wav(path.clone())
        .map(|r| r.expect("no error"))
        .collect()
        .await;
    let _ = std::fs::remove_file(&path);

    assert_eq!(got.len(), expected.len());
    for (a, b) in got.iter().zip(expected.iter()) {
        assert_eq!(a, b, "async WAV frame differs from sync frame");
    }
}

/// `decode_wav` surfaces a missing file as the stream's error item.
#[cfg(feature = "wav")]
#[tokio::test]
async fn decode_wav_reports_open_error() {
    let path = std::env::temp_dir().join("yodel-asynk-does-not-exist.wav");
    let mut frames = std::pin::pin!(yodel::asynk::decode_wav(path));
    let item = frames.next().await.expect("an error item");
    assert!(item.is_err());
}

/// Several feeds decode concurrently; every frame arrives with the
/// right feed index.
#[tokio::test]
async fn decode_many_attributes_frames_to_feeds() {
    let calls = ["FEED0", "FEED1", "FEED2", "FEED3"];
    let mut writers = Vec::new();
    let mut readers = Vec::new();
    for call in calls {
        let (mut a, b) = tokio::io::duplex(1024);
        let bytes = pcm_bytes(&transmission(call, b"hello"));
        writers.push(tokio::spawn(async move {
            tokio::io::AsyncWriteExt::write_all(&mut a, &bytes)
                .await
                .unwrap();
        }));
        readers.push(b);
    }
    let got: Vec<(usize, OwnedFrame)> = yodel::asynk::decode_many(readers, config())
        .map(|(feed, r)| (feed, r.expect("no error")))
        .collect()
        .await;
    for w in writers {
        w.await.unwrap();
    }
    assert_eq!(got.len(), calls.len(), "every feed's frame arrives");
    let mut seen = [false; 4];
    for (feed, frame) in got {
        assert_eq!(
            frame.src().callsign.as_bytes(),
            calls[feed].as_bytes(),
            "frame attributed to the wrong feed"
        );
        assert!(!seen[feed], "duplicate frame from feed {feed}");
        seen[feed] = true;
    }
}

/// The KISS server broadcasts decoded frames to a connected client as
/// well-formed KISS data frames.
#[tokio::test]
async fn serve_kiss_delivers_frames_to_client() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let expected = sync_decode(&transmission("N0CALL", b"kiss me"));
    assert_eq!(expected.len(), 1);
    let expected_body = {
        let mut buf = [0u8; 512];
        let len = expected[0].ui_frame().unwrap().build(&mut buf).unwrap();
        buf[..len].to_vec()
    };

    // Frames arrive only after the client connects, so the connection
    // cannot miss the broadcast.
    let (feed_tx, feed_rx) = tokio::sync::mpsc::channel::<OwnedFrame>(4);
    let feed = tokio_stream::wrappers::ReceiverStream::new(feed_rx);
    let server = tokio::spawn(yodel::asynk::serve_kiss(listener, feed));

    let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
    // Client is connected; release the frame. The accept race is real:
    // give the server a beat to accept before the stream ends.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    feed_tx.send(expected[0].clone()).await.unwrap();
    drop(feed_tx); // ends the feed, so the server returns

    let mut deframer = KissDeframer::<512>::new();
    let mut byte = [0u8; 1];
    let payload = loop {
        client.read_exact(&mut byte).await.unwrap();
        if let Some(result) = deframer.push(byte[0]) {
            break result.unwrap().payload().to_vec();
        }
    };
    assert_eq!(payload, expected_body, "KISS payload is the AX.25 frame");
    server.await.unwrap().unwrap();
}

/// A slow consumer stalls the decoder but loses nothing: every frame
/// still arrives, in order.
#[tokio::test]
async fn backpressure_slow_consumer_loses_nothing() {
    const FRAMES: usize = 12;
    let mut samples = Vec::new();
    for i in 0..FRAMES {
        samples.extend(transmission("N0CALL", format!("frame {i:02}").as_bytes()));
        samples.extend(std::iter::repeat_n(0i16, 2_000));
    }
    let (mut a, b) = tokio::io::duplex(512);
    let bytes = pcm_bytes(&samples);
    let writer = tokio::spawn(async move {
        tokio::io::AsyncWriteExt::write_all(&mut a, &bytes)
            .await
            .unwrap();
    });
    let mut frames = std::pin::pin!(yodel::asynk::frames(b, config()));
    let mut got = Vec::new();
    while let Some(frame) = frames.next().await {
        // The slow sink: yield repeatedly so the decoder runs far ahead
        // and hits the channel bound.
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }
        got.push(frame.expect("no error"));
    }
    writer.await.unwrap();
    assert_eq!(got.len(), FRAMES, "slow consumer dropped frames");
    for (i, frame) in got.iter().enumerate() {
        assert_eq!(frame.info(), format!(">frame {i:02}").as_bytes());
    }
}

/// A trailing odd byte is an error item, not a silent drop.
#[tokio::test]
async fn odd_trailing_byte_is_reported() {
    let (mut a, b) = tokio::io::duplex(64);
    tokio::io::AsyncWriteExt::write_all(&mut a, &[0u8; 7])
        .await
        .unwrap();
    drop(a);
    let mut frames = std::pin::pin!(yodel::asynk::frames(b, config()));
    let item = frames.next().await.expect("an error item");
    assert!(item.is_err(), "odd byte count must surface as an error");
}

/// The samples as in-memory WAV bytes at 48 kHz.
#[cfg(feature = "wav")]
fn wav_bytes(samples: &[i16]) -> Vec<u8> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 48_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = std::io::Cursor::new(Vec::new());
    let mut writer = hound::WavWriter::new(&mut cursor, spec).unwrap();
    for &s in samples {
        writer.write_sample(s).unwrap();
    }
    writer.finalize().unwrap();
    cursor.into_inner()
}

/// Feeds `bytes` through `decode_stream` over an in-memory pipe.
#[cfg(feature = "wav")]
async fn stream_decode(
    bytes: Vec<u8>,
    rate: Option<yodel::SampleRate>,
) -> Vec<Result<yodel::tnc::OwnedFrame, yodel::wav::WavError>> {
    let (mut a, b) = tokio::io::duplex(1024);
    let writer = tokio::spawn(async move {
        // A write error is fine: an early error item (bad rate, raw
        // without a rate) hangs up on the writer mid-stream.
        let _ = tokio::io::AsyncWriteExt::write_all(&mut a, &bytes).await;
    });
    let got = yodel::asynk::decode_stream(b, rate).collect().await;
    writer.await.unwrap();
    got
}

/// The same WAV bytes decode identically through the sync path and
/// through the WAV-on-a-stream async intake.
#[cfg(feature = "wav")]
#[tokio::test]
async fn decode_stream_wav_matches_sync_decode() {
    let mut samples = transmission("N7XYZ", b"stream one");
    samples.extend(std::iter::repeat_n(0i16, 2_000));
    samples.extend(transmission("K9ABC", b"stream two"));
    let expected = sync_decode(&samples);
    assert_eq!(expected.len(), 2, "sync path decodes both");

    let got: Vec<OwnedFrame> = stream_decode(wav_bytes(&samples), None)
        .await
        .into_iter()
        .map(|r| r.expect("no error"))
        .collect();
    assert_eq!(got.len(), expected.len());
    for (a, b) in got.iter().zip(expected.iter()) {
        assert_eq!(a, b, "stream WAV frame differs from sync frame");
    }
}

/// The sniff takes the rate from the WAV header (no hint needed) and
/// accepts an agreeing hint.
#[cfg(feature = "wav")]
#[tokio::test]
async fn decode_stream_wav_header_sets_the_rate() {
    let samples = transmission("N0CALL", b"header rate");
    let bytes = wav_bytes(&samples);
    let no_hint = stream_decode(bytes.clone(), None).await;
    assert_eq!(no_hint.len(), 1);
    assert!(no_hint[0].is_ok(), "WAV without a hint decodes");

    let agreeing = yodel::SampleRate::new(48_000).ok();
    let hinted = stream_decode(bytes, agreeing).await;
    assert_eq!(hinted.len(), 1);
    assert!(hinted[0].is_ok(), "an agreeing hint is accepted");
}

/// Raw PCM plus an explicit rate decodes; the sniff classifies
/// headerless bytes as raw.
#[cfg(feature = "wav")]
#[tokio::test]
async fn decode_stream_raw_with_rate_decodes() {
    let samples = transmission("N0CALL", b"raw shape");
    let rate = yodel::SampleRate::new(48_000).ok();
    let got = stream_decode(pcm_bytes(&samples), rate).await;
    assert_eq!(got.len(), 1);
    let frame = got.into_iter().next().unwrap().expect("no error");
    assert_eq!(frame.info(), b">raw shape");
}

/// A raw stream without a rate is a single RateRequired error item.
#[cfg(feature = "wav")]
#[tokio::test]
async fn decode_stream_raw_without_rate_errors() {
    let got = stream_decode(vec![0u8; 64], None).await;
    assert_eq!(got.len(), 1);
    assert!(matches!(got[0], Err(yodel::wav::WavError::RateRequired)));
}

/// A rate hint contradicting the WAV header is a single
/// RateContradiction error item.
#[cfg(feature = "wav")]
#[tokio::test]
async fn decode_stream_rate_contradiction_errors() {
    let bytes = wav_bytes(&transmission("N0CALL", b"clash"));
    let wrong = yodel::SampleRate::new(44_100).ok();
    let got = stream_decode(bytes, wrong).await;
    assert_eq!(got.len(), 1);
    assert!(matches!(
        got[0],
        Err(yodel::wav::WavError::RateContradiction {
            header_hz: 48_000,
            given_hz: 44_100,
        })
    ));
}
