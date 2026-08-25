//! Host-only decode/modulate throughput benchmark for the `i16` paths.
//!
//! * **Scenario** — a measurement tool, not an application: how many
//!   samples per second the modulator and demodulator sustain, used to
//!   derive the embedded cycle budgets in `docs/BENCHMARKS.md`.
//! * **Hardware** — run it on the host you care about. Numbers are
//!   machine-specific; the MCU figures in the docs are extrapolations
//!   from this, not measurements on silicon.
//! * **Features** — `tnc,g3ruh,fx25`.
//!
//! Synthesizes several seconds of audio per mode (1200-baud Bell 202
//! AFSK at 48 kHz — both the full 11-chain default bank and the
//! single-chain `SpaceGainSweep::UNITY` bank — G3RUH 9600 baud at
//! 48 kHz, and an FX.25-wrapped 1200-baud run), decodes it with the
//! standard receiver configurations, and prints one table row per mode:
//! samples/sec, ns/sample, and the real-time headroom (xRT) at the
//! mode's own sample rate. The modulator side and the per-frame FX.25
//! Reed-Solomon decode are timed too, and `size_of` for the receiver
//! structures is printed for the RAM-footprint note in the README.
//!
//! Re-run with one command (results are meaningless without
//! `--release`):
//!
//! ```sh
//! cargo run --release --example throughput --features tnc,g3ruh,fx25
//! ```
//!
//! ALL numbers are machine-dependent host measurements (a desktop-class
//! x86_64/arm64 host, out-of-order superscalar, large caches): they are
//! inputs to the MHz-extrapolation math in the README's "Will it run on
//! my chip?" section, not embedded results. On-device confirmation
//! needs a cycle counter around the sample-feed loop (see the README).

use std::time::Instant;

use yodel::aprs::{AprsPacket, Status};
use yodel::ax25::Address;
use yodel::demodulator::{AfskDemodulator, DemodulatorConfig};
use yodel::fx25::{Fx25Receiver, WRAP_MAX, byte_bits, stuff_frame, wrap};
use yodel::modulator::{Modulator, ModulatorConfig};
use yodel::nrzi::{self, NrziDecoder};
use yodel::rs::{RsCodec, RsParity};
use yodel::tnc::{
    DefaultTncReceiver, MAX_FRAME_BYTES, SpaceGainSweep, TncConfig, TncReceiver, TncTransmitter,
};
use yodel::{ModemProfile, SampleRate};

/// Target duration of synthesized audio per mode, in seconds of the
/// mode's own sample rate. Long enough that per-run overhead vanishes,
/// short enough that the whole benchmark finishes in seconds.
const SECONDS: u32 = 10;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let rate = SampleRate::new(48_000)?;
    let dest = Address::new(b"APRS", 0)?;
    let src = Address::new(b"N0CALL", 7)?;
    let packet = AprsPacket::Status(Status {
        text: b"throughput benchmark payload",
    });

    println!("yodel i16-path throughput (HOST measurement; machine-dependent)");
    println!("build: --release required; see the header for the exact command");
    println!();
    println!(
        "{:<28} {:>14} {:>10} {:>10}",
        "mode", "samples/sec", "ns/sample", "xRT@rate"
    );

    // ---- 1200-baud Bell 202 AFSK at 48 kHz --------------------------
    let bell = TncConfig::bell_202(rate)?;
    let (bell_audio, bell_mod_ns) = synthesize(&bell, &packet, dest, src)?;
    report_mod("1200 AFSK modulate", 48_000, bell_audio.len(), bell_mod_ns);
    {
        // Untimed warm-up pass: without it the first timed row absorbs
        // cold caches and CPU frequency ramp, which on this author's
        // machine inflated it by ~1.9x and made the headline row the
        // least trustworthy one in the table.
        let mut warm: DefaultTncReceiver = TncReceiver::new(bell)?;
        for &s in &bell_audio {
            let _ = warm.push_i16(s);
        }
        let mut rx: DefaultTncReceiver = TncReceiver::new(bell)?;
        let mut frames = 0u32;
        let t = Instant::now();
        for &s in &bell_audio {
            if rx.push_i16(s).is_some() {
                frames += 1;
            }
        }
        report("1200 AFSK decode", 48_000, bell_audio.len(), t.elapsed());
        assert!(frames > 0, "1200 AFSK run must decode frames");
    }
    // The single-chain receiver, measured as a `TncReceiver` rather than
    // inferred from the bare-demodulator rows below. `SpaceGainSweep::UNITY`
    // drops the bank to one decision chain, but the sample-rate front end
    // (three correlator banks + envelope tap + band-pass + pre-emphasis)
    // runs regardless, so this is NOT the bare-demodulator cost — which is
    // exactly why it needs its own row: `DevicePreset`'s conservative
    // variants resolve to this configuration.
    {
        let unity = TncConfig::bell_202(rate)?.with_space_gain_sweep(SpaceGainSweep::UNITY);
        let mut rx: DefaultTncReceiver = TncReceiver::new(unity)?;
        let mut frames = 0u32;
        let t = Instant::now();
        for &s in &bell_audio {
            if rx.push_i16(s).is_some() {
                frames += 1;
            }
        }
        report(
            "1200 AFSK decode (1 chain)",
            48_000,
            bell_audio.len(),
            t.elapsed(),
        );
        assert!(frames > 0, "1200 AFSK UNITY run must decode frames");
    }

    // ---- G3RUH 9600 baud at 48 kHz (a tested rate) -------------------
    let g3ruh = TncConfig::from_profile(rate, ModemProfile::G3RUH_9600)?;
    let (g3ruh_audio, g3ruh_mod_ns) = synthesize(&g3ruh, &packet, dest, src)?;
    report_mod(
        "9600 G3RUH modulate",
        48_000,
        g3ruh_audio.len(),
        g3ruh_mod_ns,
    );
    {
        let mut rx: DefaultTncReceiver = TncReceiver::new(g3ruh)?;
        let mut frames = 0u32;
        let t = Instant::now();
        for &s in &g3ruh_audio {
            if rx.push_i16(s).is_some() {
                frames += 1;
            }
        }
        report("9600 G3RUH decode", 48_000, g3ruh_audio.len(), t.elapsed());
        assert!(frames > 0, "G3RUH run must decode frames");
    }

    // ---- FX.25-wrapped 1200-baud run ---------------------------------
    let (fx25_audio, fx25_frames_in) = synthesize_fx25(bell, &packet, dest, src)?;
    {
        let demod_config = DemodulatorConfig::new(rate, bell.baud(), bell.tones())?;
        let mut demod = AfskDemodulator::new(demod_config)?;
        let mut nrzi_rx = NrziDecoder::default();
        let mut rx = Fx25Receiver::<MAX_FRAME_BYTES>::new();
        let mut frames = 0u32;
        let t = Instant::now();
        for &s in &fx25_audio {
            if let Some(line) = demod.push_sample_i16(s)
                && let Some(Ok(_)) = rx.push(nrzi_rx.decode(line))
            {
                frames += 1;
            }
        }
        report(
            "FX.25 1200 decode (RS incl.)",
            48_000,
            fx25_audio.len(),
            t.elapsed(),
        );
        assert!(frames > 0, "FX.25 run must decode frames");
        let _ = fx25_frames_in;
    }

    // ---- Per-frame FX.25 RS(255,239) decode cost ---------------------
    {
        let codec = RsCodec::new(RsParity::Sixteen);
        let mut block = [0u8; 255];
        for (i, b) in block.iter_mut().enumerate().take(codec.data_capacity()) {
            *b = (i as u8).wrapping_mul(37).wrapping_add(11);
        }
        let (data, parity) = block.split_at_mut(codec.data_capacity());
        codec.encode(data, parity)?;
        let reference = block;
        const ITERS: u32 = 2_000;
        // Clean-block decode (syndromes all zero: the common case).
        let t = Instant::now();
        for _ in 0..ITERS {
            let mut b = reference;
            let corrected = codec.decode(&mut b)?;
            assert_eq!(corrected, 0);
        }
        let clean_ns = t.elapsed().as_nanos() / u128::from(ITERS);
        // Worst-case decode: the maximum 8 correctable byte errors.
        let t = Instant::now();
        for _ in 0..ITERS {
            let mut b = reference;
            for slot in 0..8 {
                b[slot * 29] ^= 0x5A;
            }
            let corrected = codec.decode(&mut b)?;
            assert_eq!(corrected, 8);
        }
        let worst_ns = t.elapsed().as_nanos() / u128::from(ITERS);
        println!();
        println!(
            "FX.25 RS(255,239) decode per frame: {clean_ns} ns clean, \
             {worst_ns} ns at max 8 byte errors (HOST; per-frame spike, \
             not per-sample)"
        );
    }

    // ---- RAM footprint of the receiver structures --------------------
    println!();
    println!("receiver struct sizes (size_of, MEASURED, this build):");
    println!(
        "  DefaultTncReceiver (1200/9600): {} bytes",
        size_of::<DefaultTncReceiver>()
    );
    println!(
        "  Fx25Receiver<MAX_FRAME_BYTES>:  {} bytes",
        size_of::<Fx25Receiver<MAX_FRAME_BYTES>>()
    );
    println!(
        "  AfskDemodulator:                {} bytes",
        size_of::<AfskDemodulator>()
    );
    Ok(())
}

/// Modulates repeated copies of one frame until `SECONDS` of audio at
/// the config's sample rate exist; returns the audio and the modulate
/// time in nanoseconds (the synthesis loop is itself the modulator
/// benchmark: samples are produced lazily, one per `next()`).
fn synthesize(
    config: &TncConfig,
    packet: &AprsPacket<'_>,
    dest: Address,
    src: Address,
) -> Result<(Vec<i16>, u128), Box<dyn std::error::Error>> {
    let tx = TncTransmitter::new(*config);
    let mut info_buf = [0u8; MAX_FRAME_BYTES];
    let mut frame_buf = [0u8; MAX_FRAME_BYTES];
    let len = tx.build_frame(packet, dest, src, &[], &mut info_buf, &mut frame_buf)?;
    let target = (config.sample_rate().hz() * SECONDS) as usize;
    let mut audio = Vec::with_capacity(target + 16_384);
    let t = Instant::now();
    while audio.len() < target {
        audio.extend(tx.frame_samples_i16(&frame_buf[..len]));
    }
    Ok((audio, t.elapsed().as_nanos()))
}

/// Modulates repeated FX.25-wrapped copies of one frame (correlation
/// tag + RS codeblock around the stuffed HDLC frame, flanked by HDLC
/// flags) until `SECONDS` of audio exist.
fn synthesize_fx25(
    config: TncConfig,
    packet: &AprsPacket<'_>,
    dest: Address,
    src: Address,
) -> Result<(Vec<i16>, u32), Box<dyn std::error::Error>> {
    let tx = TncTransmitter::new(config);
    let mut info_buf = [0u8; MAX_FRAME_BYTES];
    let mut frame_buf = [0u8; MAX_FRAME_BYTES];
    let len = tx.build_frame(packet, dest, src, &[], &mut info_buf, &mut frame_buf)?;
    let mut stuffed = [0u8; 2 * MAX_FRAME_BYTES];
    let stuffed_len = stuff_frame(&frame_buf[..len], &mut stuffed)?;
    let mut wrapped = [0u8; WRAP_MAX];
    let frame = wrap(&stuffed[..stuffed_len], &mut wrapped)?;
    let mut bytes = vec![0x7Eu8; config.preamble_flags()];
    bytes.extend_from_slice(&wrapped[..frame.len()]);
    bytes.extend(std::iter::repeat_n(0x7Eu8, config.tail_flags().max(2)));
    let modulator_config =
        ModulatorConfig::new(config.sample_rate(), config.baud(), config.tones())?;
    let one: Vec<i16> = Modulator::new(modulator_config)
        .i16_samples(nrzi::encode_iter(byte_bits(&bytes)))
        .collect();
    let target = (config.sample_rate().hz() * SECONDS) as usize;
    let mut audio = Vec::with_capacity(target + one.len());
    let mut frames = 0u32;
    while audio.len() < target {
        audio.extend_from_slice(&one);
        frames += 1;
    }
    Ok((audio, frames))
}

/// Prints one decode table row.
fn report(mode: &str, rate_hz: u32, samples: usize, elapsed: std::time::Duration) {
    let ns = elapsed.as_nanos();
    let ns_per_sample = ns as f64 / samples as f64;
    let samples_per_sec = samples as f64 / elapsed.as_secs_f64();
    let x_rt = samples_per_sec / f64::from(rate_hz);
    println!("{mode:<28} {samples_per_sec:>14.0} {ns_per_sample:>10.1} {x_rt:>9.0}x");
}

/// Prints one modulator table row (from a pre-measured duration).
fn report_mod(mode: &str, rate_hz: u32, samples: usize, elapsed_ns: u128) {
    let ns_per_sample = elapsed_ns as f64 / samples as f64;
    let samples_per_sec = samples as f64 / (elapsed_ns as f64 / 1e9);
    let x_rt = samples_per_sec / f64::from(rate_hz);
    println!("{mode:<28} {samples_per_sec:>14.0} {ns_per_sample:>10.1} {x_rt:>9.0}x");
}
