//! Runtime proof of allocation-freedom for the core paths.
//!
//! `warble` claims its core is allocation-free; until now that claim
//! rested on build-only evidence (the embedded cross-build matrix links
//! without an allocator). This test makes it a runtime theorem: a
//! counting `#[global_allocator]` wraps the system allocator, and the
//! full core paths run inside a measured window during which the
//! allocation counter must not move:
//!
//! * AX.25 UI frame build → TNC modulate to `i16` samples in fixed
//!   buffers → demodulate → recover the frame byte-exact;
//! * FX.25: bit-stuff → RS-wrap → bit-level receive → recover;
//! * KISS: encode into a fixed buffer → deframe → recover.
//!
//! All setup, buffer creation and assertion I/O happen OUTSIDE the
//! measured window; inside it only the library calls under test run.
//! The receivers are boxed *before* the window so their (large,
//! fixed-size) state does not live on the test thread's stack.
#![cfg(all(feature = "tnc", feature = "fx25", feature = "kiss"))]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

/// System allocator wrapper counting every allocation/reallocation
/// made by the CURRENT thread (tests run in parallel threads, so a
/// process-global counter would pick up other tests' setup noise).
struct CountingAlloc;

std::thread_local! {
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

// SAFETY: delegates directly to `System`; the counter is a const-
// initialized thread-local `Cell` whose access never allocates.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|c| c.set(c.get() + 1));
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|c| c.set(c.get() + 1));
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

/// Runs `f` and returns the number of heap allocations the current
/// thread performed inside it.
fn allocations_during<R>(f: impl FnOnce() -> R) -> (usize, R) {
    let before = ALLOCATIONS.with(Cell::get);
    let result = f();
    let after = ALLOCATIONS.with(Cell::get);
    (after - before, result)
}

use warble::SampleRate;
use warble::ax25::{Address, UiFrame};
use warble::fx25::{Fx25Receiver, WRAP_MAX, byte_bits, stuff_frame, wrap};
use warble::kiss::{KissCommand, KissDeframer, KissPort, encode_into};
use warble::tnc::{DefaultTncReceiver, TncConfig, TncTransmitter};

/// Full modem path — build AX.25 UI frame, modulate to i16 samples,
/// demodulate, recover the frame — performs ZERO heap allocations.
#[test]
fn core_mod_demod_frame_path_is_alloc_free() {
    // Setup (outside the measured window): config, transmitter, boxed
    // receiver, frame buffers.
    let config = TncConfig::bell_202(SampleRate::new(48_000).unwrap()).unwrap();
    let tx = TncTransmitter::new(config);
    let mut rx = Box::new(DefaultTncReceiver::new(config).unwrap());
    let dest = Address::new(b"APRS", 0).unwrap();
    let src = Address::new(b"N0CALL", 7).unwrap();
    let info = b"!4903.50N/07201.75W-no alloc proof";
    let mut frame_buf = [0u8; 330];
    let mut recovered = [0u8; 330];
    let mut recovered_len = 0usize;
    let mut frames = 0usize;

    // Measured window: frame build + modulate + demodulate + recover.
    let (allocs, ()) = allocations_during(|| {
        let frame_len = tx
            .build_frame_raw(dest, src, &[], info, &mut frame_buf)
            .unwrap();
        for sample in tx.frame_samples_i16(&frame_buf[..frame_len]) {
            if let Some(frame) = rx.push_i16(sample) {
                let bytes = frame.info();
                recovered[..bytes.len()].copy_from_slice(bytes);
                recovered_len = bytes.len();
                frames += 1;
            }
        }
    });

    // Assertions (outside the window).
    assert_eq!(allocs, 0, "core mod→demod→frame path allocated");
    assert_eq!(frames, 1, "frame not recovered");
    assert_eq!(
        &recovered[..recovered_len],
        info,
        "info field not recovered byte-exact"
    );
}

/// FX.25 encode (stuff + RS wrap) and bit-level decode perform ZERO
/// heap allocations.
#[test]
fn fx25_encode_decode_is_alloc_free() {
    // Setup outside the window.
    let frame = UiFrame::new(
        Address::new(b"APRS", 0).unwrap(),
        Address::new(b"N0CALL", 7).unwrap(),
        b">fx.25 no alloc",
    );
    let mut body = [0u8; 330];
    let body_len = frame.build(&mut body).unwrap();
    let mut stuffed = [0u8; 512];
    let mut out = [0u8; WRAP_MAX];
    let mut rx = Box::new(Fx25Receiver::<330>::new());
    let mut recovered = [0u8; 330];
    let mut recovered_len = 0usize;

    let (allocs, ()) = allocations_during(|| {
        let stuffed_len = stuff_frame(&body[..body_len], &mut stuffed).unwrap();
        let wrapped = wrap(&stuffed[..stuffed_len], &mut out).unwrap();
        for bit in byte_bits(&out[..wrapped.len()]) {
            if let Some(Ok(frame)) = rx.push(bit) {
                recovered[..frame.len()].copy_from_slice(frame);
                recovered_len = frame.len();
            }
        }
    });

    assert_eq!(allocs, 0, "FX.25 encode/decode path allocated");
    assert_eq!(&recovered[..recovered_len], &body[..body_len]);
}

/// KISS encode into a fixed buffer and streaming deframe perform ZERO
/// heap allocations.
#[test]
fn kiss_round_trip_is_alloc_free() {
    // Setup outside the window. Payload includes FEND/FESC to force the
    // escaping paths.
    let payload = [0xC0u8, 0xDB, 0x01, 0x7E, 0xFF, 0x00, 0xDC, 0xDD];
    let mut wire = [0u8; 64];
    let mut deframer = KissDeframer::<64>::new();
    let mut recovered = [0u8; 64];
    let mut recovered_len = 0usize;

    let (allocs, ()) = allocations_during(|| {
        let wire_len = encode_into(
            KissPort::new(0).unwrap(),
            KissCommand::Data,
            &payload,
            &mut wire,
        )
        .unwrap();
        for &byte in wire.iter().take(wire_len) {
            if let Some(Ok(frame)) = deframer.push(byte) {
                let bytes = frame.payload();
                recovered[..bytes.len()].copy_from_slice(bytes);
                recovered_len = bytes.len();
            }
        }
    });

    assert_eq!(allocs, 0, "KISS round trip allocated");
    assert_eq!(&recovered[..recovered_len], &payload);
}
