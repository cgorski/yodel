//! WSPR beacon round trip: generate one transmission as a WAV, decode
//! it back with the receive engine, and print the quality metrics.
//!
//! * **Scenario** — a WSPR propagation beacon generated and then decoded
//!   back, for checking the mode without a radio.
//! * **Hardware** — any host. On the air WSPR is an HF beacon mode: one
//!   ~110.6 s transmission per even minute into an SSB transceiver, and
//!   it needs the station clock accurate to about a second.
//! * **Features** — `wspr,wav`.
//!
//! Run with:
//!
//! ```sh
//! cargo run --example wspr_beacon --features wspr,wav
//! ```
//!
//! The generated `wspr_beacon.wav` is a spec-conformant ~110.6 s WSPR
//! transmission at 12 kHz — playable into any WSPR receive setup. The
//! decode leg exercises `WsprDecoder`, whose measured sensitivity on
//! our own signals is pinned at −22 dB SNR (2500 Hz reference
//! bandwidth) by tests/wspr_rx.rs; the often-quoted −31 dB belongs to
//! the reference implementation's decoder, not this one.

use warble::wspr::{WsprConfig, WsprDecoder, WsprDecoderConfig, WsprMessage, WsprModulator};
use warble::{MaidenheadGrid, SampleRate};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. TX: message -> 162 channel symbols -> 4-FSK samples. The
    //    locator is parsed once, at the boundary, into the shared
    //    `MaidenheadGrid` type.
    let message = WsprMessage::new("K1ABC", MaidenheadGrid::new("FN42")?, 37)?;
    let config = WsprConfig::new(1_500, SampleRate::new(12_000)?)?;
    let tx = WsprModulator::for_message(config, &message);

    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 12_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut samples: Vec<i16> = Vec::new();
    let mut writer = hound::WavWriter::create("wspr_beacon.wav", spec)?;
    for s in tx {
        writer.write_sample(s)?;
        samples.push(s);
    }
    writer.finalize()?;
    println!(
        "wrote wspr_beacon.wav: {} samples (~{:.1} s) at 12 kHz",
        samples.len(),
        samples.len() as f32 / 12_000.0
    );

    // 2. RX: pad to a full ~114 s capture window and decode it back.
    samples.resize(114 * 12_000, 0);
    let decoder = WsprDecoder::new(WsprDecoderConfig::new(1_500, 100)?);
    let decodes = decoder.decode(&samples)?;
    for d in &decodes {
        println!(
            "decoded: {} {} {} dBm | freq {:.1} Hz | dt {:.2} s | snr {:.0} dB | sync {:.2}",
            String::from_utf8_lossy(d.message.callsign()).trim(),
            d.message.grid(),
            d.message.power_dbm(),
            d.freq_hz,
            d.dt_seconds,
            d.snr_db,
            d.sync_score
        );
    }
    assert_eq!(decodes.len(), 1, "the beacon should decode exactly once");
    assert_eq!(decodes[0].message, message, "round trip must be exact");
    println!("round trip OK");
    Ok(())
}
