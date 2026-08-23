//! Build an APRS position beacon and write it to a WAV file.
//!
//! * **Scenario** — the transmit side: build a position beacon and
//!   render it to audio you could key into a radio. Pairs with
//!   [`decode_wav.rs`](decode_wav.rs), which reads it back.
//! * **Hardware** — any Linux/macOS/Windows host. Nothing radio-specific;
//!   the output is a plain WAV file.
//! * **Features** — `tnc,wav`. Uses the `hound` crate directly for the
//!   file write, so add `hound = "3"` if you copy that part.
//!
//! Shows the typed constructors (`Latitude`, `Longitude`, `Symbol`,
//! `Position`, `Address`) and the high-level `TncTransmitter`, which
//! turns an APRS packet into Bell 202 AFSK samples in one call. The
//! samples are written to `beacon.wav` (16-bit mono PCM) with `hound`.
//!
//! Run with:
//!
//! ```sh
//! cargo run --example encode_wav --features tnc,wav
//! ```

use warble::SampleRate;
use warble::aprs::{AprsPacket, Latitude, Longitude, Position, Symbol};
use warble::ax25::Address;
use warble::tnc::{MAX_FRAME_BYTES, TncConfig, TncTransmitter};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // A position report: 49° 03.50' N, 072° 01.75' W, drawn as a car.
    // Every constructor validates its input, so an invalid packet
    // cannot be represented.
    let packet = AprsPacket::Position(
        Position::new(
            Latitude::from_degrees(49.0583)?,
            Longitude::from_degrees(-72.0292)?,
            Symbol::CAR,
        )
        .with_comment(b"warble example beacon"),
    );

    // The transmitter composes the whole stack: APRS payload -> AX.25
    // UI frame (addresses, FCS, HDLC) -> NRZI -> continuous-phase AFSK.
    let sample_rate = SampleRate::new(48_000)?;
    let tx = TncTransmitter::new(TncConfig::bell_202(sample_rate)?);

    // The library is allocation-free: builders serialize into
    // caller-provided buffers, and the sample stream is a lazy iterator.
    // Transmit scratch. `MAX_FRAME_BYTES` (330) is the AX.25 worst case
    // -- longest address field, control, PID, a 256-byte information
    // field and the FCS -- so a frame buffer that size always fits and
    // needs no thought. There is no matching `INFO_MAX`, because an
    // information field can embed a caller-supplied comment of any
    // length; 64 is simply comfortable for this beacon.
    //
    // Neither is a guess you can get silently wrong: an under-size
    // buffer makes `build_frame` return `TncError` carrying the length
    // it needed. It never truncates.
    let mut info_buf = [0u8; 64];
    let mut frame_buf = [0u8; MAX_FRAME_BYTES];
    let samples = tx.transmit_i16(
        &packet,
        Address::new(b"APRS", 0)?,   // destination "tocall"
        Address::new(b"N0CALL", 7)?, // source callsign-SSID
        &[Address::new(b"WIDE1", 1)?],
        &mut info_buf,
        &mut frame_buf,
    )?;

    // Drain the iterator straight into a WAV file.
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: sample_rate.hz(),
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create("beacon.wav", spec)?;
    let mut count = 0usize;
    for sample in samples {
        writer.write_sample(sample)?;
        count += 1;
    }
    writer.finalize()?;

    println!(
        "wrote beacon.wav: {count} samples at {} Hz",
        sample_rate.hz()
    );
    println!(
        "decode it back with: cargo run --example decode_wav --features tnc,wav -- beacon.wav"
    );
    Ok(())
}
