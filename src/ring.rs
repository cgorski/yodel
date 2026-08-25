//! Allocation-free intake ring buffer for interrupt/DMA-driven sampling.
//!
//! On a bare-metal target the ADC/DMA interrupt produces `i16` PCM
//! samples faster than the main loop can always consume them; the
//! classic pattern is a fixed-capacity FIFO the ISR pushes into and the
//! main loop drains from, feeding chunks to
//! `TncReceiver::push_i16` (`tnc` feature). [`SampleRing`] is that
//! FIFO: const-generic capacity, no heap, no `unsafe`, no dependencies.
//!
//! # Sharing between an ISR and the main loop
//!
//! This crate is `#![forbid(unsafe_code)]`, so [`SampleRing`] cannot be
//! a lock-free split-ownership SPSC queue (those need `unsafe` or an
//! external dependency). Instead, both [`SampleRing::push`] and
//! [`SampleRing::pop_slice`] take `&mut self`, and *the caller* wraps
//! the ring in whatever mutual exclusion the platform provides — a
//! `critical-section` mutex, an RTIC shared resource, an
//! `interrupt::free` block. Every method is O(its data) with no
//! blocking, so the critical sections stay short. Pseudo-code for the
//! common bare-metal shape:
//!
//! ```text
//! static RING: Mutex<RefCell<SampleRing<1024>>> = ...;
//!
//! // ADC/DMA interrupt handler: push the fresh half-buffer.
//! fn on_dma_half(samples: &[i16]) {
//!     critical_section::with(|cs| {
//!         RING.borrow_ref_mut(cs).push_slice(samples);
//!     });
//! }
//!
//! // Main loop: drain a bounded chunk, then decode OUTSIDE the lock.
//! let mut chunk = [0i16; 128];
//! let n = critical_section::with(|cs| {
//!     RING.borrow_ref_mut(cs).pop_slice(&mut chunk)
//! });
//! for &s in &chunk[..n] {
//!     if let Some(frame) = receiver.push_i16(s) {
//!         /* handle the decoded frame */
//!     }
//! }
//! ```
//!
//! # Overrun accounting
//!
//! When the producer outruns the consumer, [`SampleRing`] **drops the
//! newest samples and counts them** ([`SampleRing::overruns`]) rather
//! than silently overwriting the oldest: overwriting mid-drain would
//! hand the decoder a spliced, corrupted stream, while a counted gap is
//! at worst one lost frame and an accurate diagnostic. Poll the counter
//! (and [`SampleRing::take_overruns`] to clear it) to size the ring or
//! the drain cadence.

/// A fixed-capacity FIFO of `i16` PCM samples: the intake buffer
/// between an ISR/DMA producer and a decoding main loop.
///
/// `N` is the capacity in samples (2 bytes each); the ring holds up to
/// `N` samples with no heap allocation and no `unsafe`. See the
/// [module docs](self) for the intended ISR/main-loop usage.
///
/// ```
/// use yodel::ring::SampleRing;
///
/// let mut ring: SampleRing<8> = SampleRing::new();
/// assert_eq!(ring.free(), 8);
///
/// // Producer side (ISR): single samples or slices.
/// assert!(ring.push(1));
/// assert_eq!(ring.push_slice(&[2, 3, 4]), 3);
/// assert_eq!(ring.len(), 4);
///
/// // Consumer side (main loop): drain into a caller slice.
/// let mut chunk = [0i16; 8];
/// let n = ring.pop_slice(&mut chunk);
/// assert_eq!(&chunk[..n], &[1, 2, 3, 4]);
/// assert!(ring.is_empty());
/// assert_eq!(ring.overruns(), 0);
/// ```
#[derive(Debug, Clone)]
pub struct SampleRing<const N: usize> {
    /// Sample storage; only `len` samples starting at `head` are live.
    buf: [i16; N],
    /// Index of the oldest sample (the next to pop).
    head: usize,
    /// Live samples in the ring, `0..=N`.
    len: usize,
    /// Samples dropped because the ring was full (saturating).
    overruns: u32,
}

impl<const N: usize> SampleRing<N> {
    /// Creates an empty ring.
    ///
    /// `const`, so it works in a `static` initializer (the usual home
    /// of an ISR-shared ring).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buf: [0; N],
            head: 0,
            len: 0,
            overruns: 0,
        }
    }

    /// The ring capacity in samples (the const parameter `N`).
    #[must_use]
    pub const fn capacity(&self) -> usize {
        N
    }

    /// Live samples currently buffered.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether the ring holds no samples.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Free space in samples: how many more pushes succeed before
    /// samples start being dropped.
    #[must_use]
    pub const fn free(&self) -> usize {
        N - self.len
    }

    /// Samples dropped so far because the ring was full (saturating).
    ///
    /// A nonzero value means the consumer is not draining fast enough
    /// (or the ring is undersized); the sample stream has gaps at the
    /// drop points.
    #[must_use]
    pub const fn overruns(&self) -> u32 {
        self.overruns
    }

    /// Returns the overrun count and resets it to zero.
    pub const fn take_overruns(&mut self) -> u32 {
        let n = self.overruns;
        self.overruns = 0;
        n
    }

    /// Pushes one sample; returns `false` (and counts an overrun) when
    /// the ring is full. The producer (ISR) side.
    pub const fn push(&mut self, sample: i16) -> bool {
        if self.len >= N {
            self.overruns = self.overruns.saturating_add(1);
            return false;
        }
        let tail = (self.head + self.len) % N;
        self.buf[tail] = sample;
        self.len += 1;
        true
    }

    /// Pushes as many of `samples` as fit, in order; returns how many
    /// were stored. Samples that do not fit are dropped and counted in
    /// [`SampleRing::overruns`] (the newest are lost, never spliced
    /// over older data).
    pub fn push_slice(&mut self, samples: &[i16]) -> usize {
        let mut stored = 0;
        for &s in samples {
            if !self.push(s) {
                // `push` counted this overrun; count the rest of the
                // slice too (the ring stays full for all of them).
                let rest = samples.len() - stored - 1;
                self.overruns = self.overruns.saturating_add(rest as u32);
                break;
            }
            stored += 1;
        }
        stored
    }

    /// Pops the oldest sample, or `None` when empty.
    pub const fn pop(&mut self) -> Option<i16> {
        if self.len == 0 {
            return None;
        }
        let sample = self.buf[self.head];
        self.head = (self.head + 1) % N;
        self.len -= 1;
        Some(sample)
    }

    /// Drains up to `out.len()` of the oldest samples into `out`, in
    /// order; returns how many were written. The consumer (main loop)
    /// side: drain into a small chunk under the lock, decode outside it.
    pub fn pop_slice(&mut self, out: &mut [i16]) -> usize {
        let mut written = 0;
        for slot in out.iter_mut() {
            match self.pop() {
                Some(s) => {
                    *slot = s;
                    written += 1;
                }
                None => break,
            }
        }
        written
    }

    /// Empties the ring (the overrun counter is left untouched).
    pub const fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }
}

impl<const N: usize> Default for SampleRing<N> {
    /// Same as [`SampleRing::new`].
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_ring_is_empty_with_full_capacity() {
        let ring: SampleRing<4> = SampleRing::new();
        assert!(ring.is_empty());
        assert_eq!(ring.len(), 0);
        assert_eq!(ring.free(), 4);
        assert_eq!(ring.capacity(), 4);
        assert_eq!(ring.overruns(), 0);
    }

    #[test]
    fn push_pop_fifo_order() {
        let mut ring: SampleRing<4> = SampleRing::new();
        assert!(ring.push(10));
        assert!(ring.push(-20));
        assert!(ring.push(30));
        assert_eq!(ring.pop(), Some(10));
        assert_eq!(ring.pop(), Some(-20));
        assert_eq!(ring.pop(), Some(30));
        assert_eq!(ring.pop(), None);
    }

    #[test]
    fn wraparound_preserves_order_across_many_cycles() {
        let mut ring: SampleRing<5> = SampleRing::new();
        // Interleave pushes and pops so head/tail lap the buffer many
        // times; the FIFO order must survive every wrap.
        let mut next_in: i16 = 0;
        let mut next_out: i16 = 0;
        for _ in 0..40 {
            assert_eq!(ring.push_slice(&[next_in, next_in + 1, next_in + 2]), 3);
            next_in += 3;
            let mut out = [0i16; 3];
            assert_eq!(ring.pop_slice(&mut out), 3);
            assert_eq!(out, [next_out, next_out + 1, next_out + 2]);
            next_out += 3;
        }
        assert!(ring.is_empty());
        assert_eq!(ring.overruns(), 0);
    }

    #[test]
    fn overrun_drops_newest_and_counts() {
        let mut ring: SampleRing<3> = SampleRing::new();
        assert_eq!(ring.push_slice(&[1, 2, 3]), 3);
        // Full: single push fails and counts.
        assert!(!ring.push(4));
        assert_eq!(ring.overruns(), 1);
        // Slice push stores nothing, counts every dropped sample.
        assert_eq!(ring.push_slice(&[5, 6]), 0);
        assert_eq!(ring.overruns(), 3);
        // The buffered (oldest) data is intact — never spliced over.
        let mut out = [0i16; 3];
        assert_eq!(ring.pop_slice(&mut out), 3);
        assert_eq!(out, [1, 2, 3]);
        // Counter reads out and clears.
        assert_eq!(ring.take_overruns(), 3);
        assert_eq!(ring.overruns(), 0);
    }

    #[test]
    fn partial_slice_push_stores_prefix() {
        let mut ring: SampleRing<4> = SampleRing::new();
        assert!(ring.push(9));
        // Only 3 of 5 fit; the stored prefix is in order, the 2 dropped
        // are counted.
        assert_eq!(ring.push_slice(&[1, 2, 3, 4, 5]), 3);
        assert_eq!(ring.overruns(), 2);
        let mut out = [0i16; 8];
        assert_eq!(ring.pop_slice(&mut out), 4);
        assert_eq!(&out[..4], &[9, 1, 2, 3]);
    }

    #[test]
    fn pop_slice_with_short_output_leaves_remainder() {
        let mut ring: SampleRing<8> = SampleRing::new();
        assert_eq!(ring.push_slice(&[1, 2, 3, 4, 5]), 5);
        let mut out = [0i16; 2];
        assert_eq!(ring.pop_slice(&mut out), 2);
        assert_eq!(out, [1, 2]);
        assert_eq!(ring.len(), 3);
        assert_eq!(ring.pop(), Some(3));
    }

    #[test]
    fn clear_empties_but_keeps_overruns() {
        let mut ring: SampleRing<2> = SampleRing::new();
        assert_eq!(ring.push_slice(&[1, 2, 3]), 2);
        assert_eq!(ring.overruns(), 1);
        ring.clear();
        assert!(ring.is_empty());
        assert_eq!(ring.free(), 2);
        assert_eq!(ring.overruns(), 1);
        assert!(ring.push(7));
        assert_eq!(ring.pop(), Some(7));
    }

    #[test]
    fn const_constructible_in_static_position() {
        // The usual embedded home: a static (here just a const to prove
        // const-ness without globals in tests).
        const RING: SampleRing<16> = SampleRing::new();
        assert_eq!(RING.capacity(), 16);
        assert!(RING.is_empty());
    }
}
