//! Embassy adapters (`embassy` feature): async chunk-drain decoding over
//! the sync core, plus a periodic-TX ticker helper.
//!
//! The sync, allocation-free `no_std` core is the source of truth; this
//! module contains **no decode logic** — only orchestration. The shape is
//! the same bare-metal pattern the [`crate::ring`] docs describe,
//! transposed to embassy tasks:
//!
//! * a DMA/ADC interrupt (or an embassy intake task) pushes `i16` PCM
//!   samples into a [`SampleRing`];
//! * an embassy decode task awaits chunks from a [`SampleSource`] and
//!   drains them through a caller-provided [`TncReceiver`] via bounded
//!   `push_i16` calls, with a **yield point between chunks** so
//!   same-priority tasks (sensors, housekeeping) get the core;
//! * decoded frames are delivered to a callback while still borrowed
//!   (the receiver's [`RxFrame`] is lending — copy what you keep);
//! * transmit scheduling runs on an [`embassy_time::Ticker`] via
//!   [`TxTicker`].
//!
//! # Dependencies, justified
//!
//! The only library dependency this feature adds is `embassy-time`
//! (no_std, alloc-free), pulled in solely for [`TxTicker`]'s periodic
//! scheduling; the platform HAL supplies the underlying time driver at
//! link time. The decode path needs **no** embassy crate at all: it is
//! plain `async` Rust, so it runs on the embassy executor — or any
//! other — without further glue. No executor crate is a library
//! dependency; host tests and the worked example use a dev-dependency
//! executor.
//!
//! # Usage sketch
//!
//! ```no_run
//! use core::cell::RefCell;
//! use yodel::SampleRing;
//! use yodel::embassy::{SampleSource, TxTicker, run_decoder};
//! use yodel::tnc::{DefaultTncReceiver, TncConfig, TncReceiver};
//!
//! /// Drains a task-shared ring; on a real target the ISR is the
//! /// producer and the shared cell is a critical-section mutex.
//! struct RingSource<'a, const N: usize> {
//!     ring: &'a RefCell<SampleRing<N>>,
//! }
//!
//! impl<const N: usize> SampleSource for RingSource<'_, N> {
//!     async fn next_chunk(&mut self, buf: &mut [i16]) -> usize {
//!         loop {
//!             let n = self.ring.borrow_mut().pop_slice(buf);
//!             if n > 0 {
//!                 return n;
//!             }
//!             // Nothing buffered: sleep one DMA half-buffer period.
//!             embassy_time::Timer::after_millis(5).await;
//!         }
//!     }
//! }
//!
//! async fn decode_task(ring: &RefCell<SampleRing<1024>>, cfg: TncConfig) {
//!     let mut rx: DefaultTncReceiver = TncReceiver::new(cfg).unwrap();
//!     let mut source = RingSource { ring };
//!     let mut chunk = [0i16; 128];
//!     run_decoder(&mut source, &mut rx, &mut chunk, |frame| {
//!         let _ = frame.info(); // copy out what you keep
//!     })
//!     .await;
//! }
//!
//! async fn beacon_task() {
//!     let mut tick = TxTicker::every(embassy_time::Duration::from_secs(30));
//!     loop {
//!         tick.ready().await;
//!         // build the packet with TncTransmitter, hand samples to the DAC
//!     }
//! }
//! ```

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

use crate::tnc::{RxFrame, TncReceiver};

#[cfg(doc)]
use crate::ring::SampleRing;

/// An async producer of PCM sample chunks: the seam between the
/// platform's intake (DMA ring, ADC ISR, channel) and [`run_decoder`].
///
/// `next_chunk` fills a prefix of `buf` with the oldest pending samples
/// and returns how many it wrote. Returning `0` means *end of stream*
/// and stops the decoder — a live radio source never returns `0` (await
/// until samples arrive instead); finite sources (tests, replays)
/// return `0` when exhausted.
pub trait SampleSource {
    /// Awaits the next batch of samples, writes them into a prefix of
    /// `buf`, and returns the count (`0` = end of stream).
    fn next_chunk(&mut self, buf: &mut [i16]) -> impl Future<Output = usize>;
}

/// Drains `source` through `receiver` in bounded chunks, yielding to
/// the executor between chunks, until the source reports end of stream.
///
/// Each chunk costs at most `chunk.len()` constant-cost
/// [`TncReceiver::push_i16`] calls, so `chunk.len()` is the decode
/// task's latency knob: smaller chunks yield more often. Every decoded
/// frame is handed to `on_frame` while still borrowed from the
/// receiver; copy out (e.g. via [`crate::tnc::OwnedFrame`]) anything
/// that must outlive the callback. All decode semantics — recovery
/// policy, chain voting, stats — are exactly the sync core's; this
/// function only moves samples.
///
/// Returns the total number of samples decoded.
pub async fn run_decoder<const N: usize, S, F>(
    source: &mut S,
    receiver: &mut TncReceiver<N>,
    chunk: &mut [i16],
    mut on_frame: F,
) -> u64
where
    S: SampleSource,
    F: FnMut(&RxFrame<'_>),
{
    debug_assert!(!chunk.is_empty(), "zero-length chunk cannot make progress");
    let mut total: u64 = 0;
    loop {
        let n = source.next_chunk(chunk).await;
        if n == 0 {
            return total;
        }
        for &s in &chunk[..n] {
            if let Some(frame) = receiver.push_i16(s) {
                on_frame(&frame);
            }
        }
        total += n as u64;
        // Cooperative yield: give same-priority tasks the core between
        // chunks even when the source always has data ready.
        yield_now().await;
    }
}

/// Periodic transmit scheduling on `embassy-time`: a thin wrapper over
/// [`embassy_time::Ticker`] that keeps the beacon cadence steady
/// (missed deadlines are skipped, not bunched).
///
/// (No `Debug` impl: the wrapped `embassy_time::Ticker` has none.)
pub struct TxTicker {
    ticker: embassy_time::Ticker,
}

impl TxTicker {
    /// A ticker firing every `period`, first tick one period from now.
    #[must_use]
    pub fn every(period: embassy_time::Duration) -> Self {
        Self {
            ticker: embassy_time::Ticker::every(period),
        }
    }

    /// Waits for the next tick (the moment to build and key a frame).
    pub async fn ready(&mut self) {
        self.ticker.next().await;
    }
}

/// Yields to the executor once: returns `Pending` on the first poll
/// (after scheduling a wake) and `Ready` on the next.
fn yield_now() -> YieldNow {
    YieldNow { yielded: false }
}

struct YieldNow {
    yielded: bool,
}

impl Future for YieldNow {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.yielded {
            Poll::Ready(())
        } else {
            self.yielded = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}
