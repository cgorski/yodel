//! FT8 full cycle: encode a CQ call, write it to a WAV, read it back
//! and decode it, printing frequency/time/quality metrics.
//!
//! * **Scenario** — a full FT8 transmit-and-receive cycle in one
//!   process, for checking the mode end to end without a radio.
//! * **Hardware** — any host. FT8 is an HF weak-signal mode: on the air
//!   this audio goes to an SSB transceiver, and decoding needs the
//!   station clock within a second or two of UTC.
//! * **Features** — `ft8,std,wav`.
//!
//! Run with:
//!
//! ```sh
//! cargo run --example ft8_cycle --features ft8,std,wav
//! ```
//!
//! Writes `ft8_cycle.wav` (~15 s of 12 kHz mono PCM) into the current
//! directory, then decodes it with the std-gated receive engine.

use warble::SampleRate;
use warble::ft8::{Ft8Config, Ft8Decoder, Ft8DecoderConfig, Ft8Message, Ft8Modulator, Ft8Tail};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Encode: a CQ call with grid, at 1500 Hz in the audio band.
    let msg = Ft8Message::standard("CQ", "K1ABC", false, Ft8Tail::grid("FN42")?)?;
    let rate = SampleRate::new(12_000)?;
    let config = Ft8Config::new(1_500, rate)?;
    let tx = Ft8Modulator::for_message(config, &msg);

    // 2. WAV: half a second of leading silence (as in a real cycle,
    //    the transmission starts ~0.5 s into the 15 s slot), the
    //    12.64 s transmission, silence to 15 s.
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 12_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let path = "ft8_cycle.wav";
    let mut writer = hound::WavWriter::create(path, spec)?;
    let total = 15 * 12_000usize;
    let mut written = 0usize;
    for _ in 0..6_000 {
        writer.write_sample(0i16)?;
        written += 1;
    }
    for s in tx {
        // Back off to half scale, the polite level.
        writer.write_sample(s / 2)?;
        written += 1;
    }
    while written < total {
        writer.write_sample(0i16)?;
        written += 1;
    }
    writer.finalize()?;
    println!("wrote {path}: {written} samples (~15 s at 12 kHz)");

    // 3. Decode: read the capture back and run the receive engine.
    let mut reader = hound::WavReader::open(path)?;
    let samples: Vec<i16> = reader.samples::<i16>().collect::<Result<_, _>>()?;
    let decoder = Ft8Decoder::new(Ft8DecoderConfig::new(1_500, 300)?);
    let decodes = decoder.decode(&samples)?;
    for d in &decodes {
        println!(
            "decoded: \"{}\" | freq {:.1} Hz | dt {:.2} s | snr {:.0} dB | sync {:.2}",
            d.message, d.freq_hz, d.dt_seconds, d.snr_db, d.sync_score
        );
    }
    assert_eq!(decodes.len(), 1, "expected exactly one decode");
    assert_eq!(decodes[0].message.as_str(), "CQ K1ABC FN42");
    println!("cycle closed: TX output decoded back to the original message");
    Ok(())
}
