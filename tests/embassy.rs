//! `embassy`-feature adapter tests: frames flowing through
//! `yodel::embassy::run_decoder` must match the sync core's decode
//! exactly (the adapter is orchestration only), and the yield/backstop
//! behavior must hold. Host-runnable: `embassy-futures::block_on` is a
//! dependency-free busy-poll executor and `embassy-time`'s `std`
//! dev-feature supplies a host time driver for `TxTicker`.
#![cfg(feature = "embassy")]

use embassy_futures::block_on;
use yodel::SampleRate;
use yodel::aprs::{AprsPacket, Status};
use yodel::ax25::Address;
use yodel::embassy::{SampleSource, TxTicker, run_decoder};
use yodel::ring::SampleRing;
use yodel::tnc::{DefaultTncReceiver, OwnedFrame, TncConfig, TncReceiver, TncTransmitter};

/// Two beacons' worth of PCM, with silence padding between them.
fn test_samples(cfg: TncConfig) -> Vec<i16> {
    let tx = TncTransmitter::new(cfg);
    let mut samples = vec![0i16; 400];
    for text in [&b"hello from the sky"[..], &b"second frame"[..]] {
        let mut info_buf = [0u8; 64];
        let mut frame_buf = [0u8; 128];
        let iter = tx
            .transmit_i16(
                &AprsPacket::Status(Status { text }),
                Address::new(b"APRS", 0).unwrap(),
                Address::new(b"N0CALL", 7).unwrap(),
                &[],
                &mut info_buf,
                &mut frame_buf,
            )
            .unwrap();
        samples.extend(iter);
        samples.extend(std::iter::repeat_n(0i16, 400));
    }
    samples
}

/// Reference decode: the plain sync push loop.
fn sync_decode(cfg: TncConfig, samples: &[i16]) -> Vec<OwnedFrame> {
    let mut rx: DefaultTncReceiver = TncReceiver::new(cfg).unwrap();
    let mut frames = Vec::new();
    for &s in samples {
        if let Some(frame) = rx.push_i16(s) {
            frames.push(OwnedFrame::new(&frame).unwrap());
        }
    }
    frames
}

/// A finite source that refills an intake `SampleRing` in DMA-half-
/// buffer-sized bursts and drains chunks from it — the on-target shape,
/// minus the ISR (single task, so no lock needed).
struct ReplaySource<'a> {
    remaining: &'a [i16],
    ring: SampleRing<256>,
    polls: u32,
}

impl SampleSource for ReplaySource<'_> {
    async fn next_chunk(&mut self, buf: &mut [i16]) -> usize {
        self.polls += 1;
        if self.ring.is_empty() {
            // "DMA half-buffer complete": push the next burst.
            let n = self.remaining.len().min(192);
            assert_eq!(self.ring.push_slice(&self.remaining[..n]), n);
            self.remaining = &self.remaining[n..];
        }
        self.ring.pop_slice(buf)
    }
}

#[test]
fn adapter_decode_matches_sync_core() {
    let cfg = TncConfig::bell_202(SampleRate::new(24_000).unwrap()).unwrap();
    let samples = test_samples(cfg);

    let expected = sync_decode(cfg, &samples);
    assert_eq!(expected.len(), 2, "reference decode must see both frames");

    let mut rx: DefaultTncReceiver = TncReceiver::new(cfg).unwrap();
    let mut source = ReplaySource {
        remaining: &samples,
        ring: SampleRing::new(),
        polls: 0,
    };
    let mut chunk = [0i16; 64];
    let mut got: Vec<OwnedFrame> = Vec::new();
    let total = block_on(run_decoder(&mut source, &mut rx, &mut chunk, |frame| {
        got.push(OwnedFrame::new(frame).unwrap());
    }));

    assert_eq!(total, samples.len() as u64, "every sample must be decoded");
    assert!(source.polls > 1, "decode must proceed in bounded chunks");
    assert_eq!(got.len(), expected.len());
    for (a, b) in got.iter().zip(expected.iter()) {
        assert_eq!(a.src(), b.src());
        assert_eq!(a.dest(), b.dest());
        assert_eq!(a.info(), b.info());
    }
    assert_eq!(rx.stats().frames_ok, 2);
}

#[test]
fn bounded_latency_config_works_through_adapter() {
    // The slice-1 preset composes with the adapter unchanged: same
    // orchestration, different core policy.
    let cfg = TncConfig::bell_202(SampleRate::new(24_000).unwrap())
        .unwrap()
        .bounded_latency();
    let samples = test_samples(cfg);
    let expected = sync_decode(cfg, &samples);

    let mut rx: DefaultTncReceiver = TncReceiver::new(cfg).unwrap();
    let mut source = ReplaySource {
        remaining: &samples,
        ring: SampleRing::new(),
        polls: 0,
    };
    let mut chunk = [0i16; 128];
    let mut got = 0usize;
    block_on(run_decoder(&mut source, &mut rx, &mut chunk, |_| got += 1));
    assert_eq!(got, expected.len());
}

#[test]
fn tx_ticker_fires_on_host_driver() {
    block_on(async {
        let mut tick = TxTicker::every(embassy_time::Duration::from_millis(5));
        let before = embassy_time::Instant::now();
        tick.ready().await;
        tick.ready().await;
        assert!(before.elapsed() >= embassy_time::Duration::from_millis(5));
    });
}
