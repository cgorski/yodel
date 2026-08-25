//! IL2P end-to-end round trip: encode an AX.25 UI frame as IL2P,
//! modulate it as 1200-baud Bell 202 audio, corrupt a few payload
//! bytes, receive it back through the bit-level [`Il2pReceiver`], and
//! print the corrected-symbol statistics.
//!
//! * **Scenario** — an IL2P link demonstrated end to end, including
//!   injected corruption, to show the forward error correction
//!   recovering bytes rather than just being present.
//! * **Hardware** — any host. On the air IL2P needs IL2P at both ends
//!   (a NinoTNC, typically); it is not readable by a plain AX.25
//!   receiver.
//! * **Features** — `il2p,mod,demod`.
//!
//! Run with:
//!
//! ```sh
//! cargo run --example il2p_roundtrip --features il2p,mod,demod
//! ```

use yodel::SampleRate;
use yodel::ax25::{Address, UiFrame};
use yodel::demodulator::{AfskDemodulator, DemodulatorConfig};
use yodel::il2p::{
    self, ENCODED_MAX, HEADER_LEN, HEADER_PARITY_LEN, Il2pParity, Il2pReceiver, SYNC_LEN,
};
use yodel::modulator::{Modulator, ModulatorConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let rate = SampleRate::new(48_000)?;
    let parity = Il2pParity::Sixteen;

    // 1. Build the frame and encode it as IL2P (sync word, translated
    //    header + header FEC, scrambled payload block + block FEC).
    let frame = UiFrame::new(
        Address::new(b"APRS", 0)?,
        Address::new(b"N0CALL", 7)?,
        b">IL2P round trip demo",
    );
    let mut encoded = [0u8; ENCODED_MAX];
    let len = il2p::encode_ui_frame(&frame, parity, &mut encoded)?;
    println!(
        "encoded frame: {len} bytes (payload {} bytes)",
        frame.info.len()
    );

    // 2. Corrupt a few payload bytes, well within the block's
    //    correction capacity t = parity/2 = 8 symbols.
    let payload_at = SYNC_LEN + HEADER_LEN + HEADER_PARITY_LEN;
    for k in 0..3 {
        encoded[payload_at + 5 * k] ^= 0xA5;
    }
    println!(
        "injected 3 corrupted payload bytes (t = {})",
        parity.correctable()
    );

    // 3. Modulate: MSB-first bits with a 0x55 preamble, NRZI, Bell 202.
    let audio: Vec<i16> = Modulator::new(ModulatorConfig::bell_202(rate)?)
        .i16_samples(il2p::tx_bits(&encoded[..len], 16, 2))
        .collect();
    println!("modulated: {} samples at 48 kHz", audio.len());

    // 4. Receive: demodulate → NRZI decode → IL2P sync hunt + decode.
    let mut demod = AfskDemodulator::new(DemodulatorConfig::bell_202(rate)?)?;
    let mut rx = Il2pReceiver::new(parity);
    let mut recovered = false;
    for &s in &audio {
        let Some(line) = demod.push_sample_i16(s) else {
            continue;
        };
        if let Some(Ok(rxf)) = rx.push(line) {
            let ui = rxf.ui_frame()?;
            println!(
                "recovered: {}>{} \"{}\" (header corrected {}, payload corrected {})",
                core::str::from_utf8(ui.src.callsign.as_bytes())?,
                core::str::from_utf8(ui.dest.callsign.as_bytes())?,
                String::from_utf8_lossy(ui.info),
                rxf.decoded.header_corrected,
                rxf.decoded.payload_corrected,
            );
            assert_eq!(ui, frame);
            recovered = true;
        }
    }
    assert!(recovered, "the frame must decode");
    println!("round trip OK");
    Ok(())
}
