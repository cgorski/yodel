//! Send a short text packet over M17 baseband audio and decode it.
//!
//! * **Scenario** — an M17 packet-mode link: address, encode, modulate,
//!   demodulate and read the payload back.
//! * **Hardware** — any host. This is *baseband* audio at 48 kHz: on the
//!   air it feeds an FM exciter's modulator input directly, not a
//!   microphone input. Voice is not implemented.
//! * **Features** — `m17`.
//!
//! Run with:
//!
//! ```sh
//! cargo run --example m17_packet --features m17
//! ```
//!
//! The transmit side assembles preamble + LSF + packet frames + EOT as
//! RRC-shaped 4-level PAM at 48 kHz; the receive side runs the matched
//! filter, sync hunt, 4-level slicer, Viterbi FEC and superframe CRC —
//! all through the public `yodel::m17` API. This is baseband audio:
//! on a real link this waveform drives (and comes back from) an FM
//! radio's modulator/discriminator.

use yodel::SampleRate;
use yodel::m17::{Address, Lsf, M17FrameEvent, M17PacketTx, M17Receiver, PacketAssembler};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dst = Address::broadcast();
    let src = Address::from_callsign("N0CALL")?;
    let lsf = Lsf::packet_data(dst, src, 0);
    let message = b"Greetings from yodel over M17 packet mode!";

    let sr = SampleRate::new(48_000)?;
    let mut tx = M17PacketTx::new(sr, lsf, message)?;

    let mut rx = M17Receiver::new(sr)?;
    let mut assembler = PacketAssembler::new();
    let mut samples = 0usize;
    let mut decoded: Option<Vec<u8>> = None;

    while let Some(sample) = tx.next_i16() {
        samples += 1;
        match rx.push_i16(sample) {
            Some(M17FrameEvent::Lsf(l)) => {
                let mut dbuf = [0u8; 9];
                let mut sbuf = [0u8; 9];
                println!(
                    "LSF: {} -> {} (type {:#06x})",
                    l.src.callsign(&mut sbuf),
                    l.dst.callsign(&mut dbuf),
                    l.lsf_type
                );
                assembler.start(l);
            }
            Some(M17FrameEvent::PacketFrame(f)) => {
                if let Some(payload) = assembler.feed(&f) {
                    decoded = Some(payload.to_vec());
                }
            }
            None => {}
        }
    }

    println!(
        "generated {samples} samples ({:.1} ms of 48 kHz baseband audio)",
        samples as f64 / 48.0
    );
    let payload = decoded.ok_or("packet did not decode")?;
    println!("decoded packet: {}", String::from_utf8_lossy(&payload));
    assert_eq!(payload, message);
    Ok(())
}
