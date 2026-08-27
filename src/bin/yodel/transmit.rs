//! `yodel transmit`: play a WAV to a sound card while keying a
//! transmitter, and do not unkey until the last sample has left.
//!
//! # Why this exists next to `yodel ptt`
//!
//! [`crate::ptt`] keys the line and hands the audio to an external
//! player (`sox`, `aplay`, ...). That composes well and stays out of the
//! way, but it cannot make the one guarantee a transmission needs,
//! because the two halves live in different processes:
//!
//! * A player exiting means it wrote its last sample **to the device**.
//!   It does not mean the device converted that sample to sound.
//! * Worse, a player that tears down its stream at exit can *discard*
//!   whatever the device had not yet converted. Those samples are gone,
//!   not delayed.
//!
//! The second point is the sharp one, and it is why `--tail` cannot
//! paper over this: holding the control line longer transmits nothing
//! if the samples were thrown away. Measured against a USB codec here,
//! ~33 ms vanished exactly that way — which is longer than the 13.3 ms
//! that the library's two framing flags occupy at 1200 baud, so the
//! clipping reached past the tail and into the FCS. Every frame then
//! failed its CRC at the far end while looking perfect at the
//! transmitter: the packet was built correctly, modulated correctly,
//! and the radio keyed correctly.
//!
//! Owning both halves in one process makes the sequence provable:
//!
//! ```text
//! key ─→ --lead ─→ play every sample ─→ --drain ─→ --tail ─→ unkey
//! ```
//!
//! `--drain` is the step an external player cannot be asked for. After
//! the last real sample is handed over, silence keeps being fed for that
//! long, so the device's own buffering is pushed through and the real
//! audio is certainly out before the stream stops. It is silence rather
//! than flags on purpose: this is playback latency, a property of the
//! sound card, not of the packet. The on-air tail belongs in the
//! modulated signal, where `yodel encode --txtail` puts it.
//!
//! That split is the whole design. The WAV ends at its closing flags,
//! the way a TNC's output does and the way an operator expects; the
//! device's slack is handled here, at the device.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use clap::Args;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::ptt::{Keyed, Signal, open_port};

#[derive(Args)]
pub struct TransmitArgs {
    /// WAV file to transmit (16-bit mono integer PCM, as `yodel encode`
    /// writes)
    #[arg(value_name = "INPUT.wav")]
    input: Option<String>,

    /// Output sound device to play through [default: the system default
    /// output]
    ///
    /// Matched case-insensitively on a leading substring, so `--device
    /// USB` finds `USB Audio Device`. A radio interface is rarely the
    /// system default, and playing a transmission out of the laptop
    /// speakers instead is a loud way to find that out.
    #[arg(long, value_name = "NAME")]
    device: Option<String>,

    /// List the output devices this machine can see, and exit
    #[arg(long)]
    list_devices: bool,

    /// Serial port that keys the radio, e.g. `/dev/ttyUSB0`,
    /// `/dev/cu.usbserial-1110` or `COM3`. Omit to play the audio
    /// without keying anything, which is how you check levels safely
    #[arg(long, value_name = "DEVICE")]
    port: Option<String>,

    /// Control line to assert
    #[arg(long, value_enum, default_value_t = Signal::Rts)]
    signal: Signal,

    /// Key on the line being LOW rather than high
    #[arg(long)]
    invert: bool,

    /// Milliseconds to hold the line before the audio starts, so the
    /// transmitter's output stage settles and the far end's squelch
    /// opens before any data arrives.
    ///
    /// The electrical lead-in, not the on-air one: the flags that give a
    /// receiver's clock recovery something to lock onto are inside the
    /// WAV, from `yodel encode --txdelay`.
    #[arg(long, value_name = "MS", default_value_t = 300)]
    lead: u64,

    /// Milliseconds of silence fed to the device AFTER the last sample,
    /// so its buffering is flushed before the stream stops.
    ///
    /// This is what makes the transmission whole. It costs nothing on
    /// air — the carrier is already up and this is the far side of the
    /// data — and it is the difference between the FCS arriving and the
    /// FCS being discarded inside the sound card.
    #[arg(long, value_name = "MS", default_value_t = 250)]
    drain: u64,

    /// Milliseconds to keep the line keyed after the drain completes
    #[arg(long, value_name = "MS", default_value_t = 50)]
    tail: u64,

    /// Hard limit on total key-down time. Exceeding it drops the line
    /// and exits non-zero.
    ///
    /// A safety net, not a schedule: a wedged audio callback would
    /// otherwise hold a transmitter up indefinitely, which jams the
    /// channel for everyone in range and can destroy the radio's output
    /// stage.
    #[arg(long, value_name = "MS", default_value_t = 60_000)]
    max: u64,
}

/// Runs `yodel transmit`.
///
/// # Errors
///
/// No input, a WAV that is not 16-bit mono PCM, no matching output
/// device, a device that cannot run at the file's sample rate, a serial
/// port that cannot be opened, or a transmission that runs past `--max`.
pub fn transmit(args: &TransmitArgs) -> Result<(), String> {
    if args.list_devices {
        return list_devices();
    }
    let input = args
        .input
        .as_deref()
        .ok_or("no input: pass a WAV file, or --list-devices to see the output devices")?;

    let (samples, rate) = read_wav(input)?;
    if samples.is_empty() {
        return Err(format!("'{input}' contains no samples"));
    }

    let device = pick_device(args.device.as_deref())?;
    let name = device_name(&device);
    let config = pick_config(&device, rate)?;
    let channels = config.channels as usize;

    // Key BEFORE the lead, and hold the guard for the whole
    // transmission: every exit path from here, including `?` and a
    // panic in the audio plumbing, releases the line on the way out.
    let keyed = match args.port.as_deref() {
        Some(path) => {
            let port = open_port(path, args.invert)?;
            eprintln!("PTT on  ({} on {path})", args.signal.describe(args.invert));
            Some(Keyed::assert(port, args.signal, args.invert))
        }
        None => {
            eprintln!("no --port: playing without keying anything");
            None
        }
    };
    let started = Instant::now();
    std::thread::sleep(Duration::from_millis(args.lead));

    let drain_frames = (u64::from(rate) * args.drain / 1000) as usize;
    let total = samples.len() + drain_frames;
    let data = Arc::new(samples);
    let position = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicBool::new(false));
    let failed = Arc::new(AtomicBool::new(false));

    let stream = build_stream(
        &device,
        &config,
        channels,
        Arc::clone(&data),
        Arc::clone(&position),
        Arc::clone(&done),
        Arc::clone(&failed),
        total,
    )?;
    stream
        .play()
        .map_err(|e| format!("cannot start playback on '{name}': {e}"))?;

    // Poll rather than block on a condvar: the point of `--max` is that
    // a stalled callback still releases the transmitter.
    while !done.load(Ordering::Acquire) {
        if started.elapsed() > Duration::from_millis(args.max) {
            drop(stream);
            drop(keyed);
            return Err(format!(
                "--max ({} ms) exceeded with the audio still playing; line released",
                args.max
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(stream);

    if failed.load(Ordering::Acquire) {
        drop(keyed);
        return Err(format!(
            "the audio device '{name}' reported an error mid-transmission"
        ));
    }

    std::thread::sleep(Duration::from_millis(args.tail));
    let held = started.elapsed().as_millis();
    drop(keyed);
    eprintln!("PTT off ({held} ms keyed, {} samples played)", data.len());
    Ok(())
}

/// Builds the output stream, converting to whatever the device wants.
#[allow(clippy::too_many_arguments)]
fn build_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    data: Arc<Vec<i16>>,
    position: Arc<AtomicUsize>,
    done: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    total: usize,
) -> Result<cpal::Stream, String> {
    let err_done = Arc::clone(&done);
    let err_failed = Arc::clone(&failed);
    let on_error = move |e| {
        eprintln!("audio device error: {e}");
        err_failed.store(true, Ordering::Release);
        err_done.store(true, Ordering::Release);
    };

    // One mono sample is written to every channel of a frame: a radio
    // interface is mono, and splitting it across a stereo device would
    // halve the level on one side and lose the other.
    let fill = move |out: &mut [f32]| {
        let frames = out.len() / channels.max(1);
        let start = position.fetch_add(frames, Ordering::AcqRel);
        for f in 0..frames {
            let idx = start + f;
            let v = data.get(idx).map_or(0.0, |s| f32::from(*s) / 32768.0);
            for c in 0..channels {
                out[f * channels + c] = v;
            }
        }
        if start + frames >= total {
            done.store(true, Ordering::Release);
        }
    };

    let stream = device
        // cpal 0.18 takes the config BY VALUE (`StreamConfig` is `Copy`).
        .build_output_stream(
            *config,
            move |out: &mut [f32], _: &cpal::OutputCallbackInfo| fill(out),
            on_error,
            None,
        )
        .map_err(|e| format!("cannot open an f32 output stream: {e}"))?;
    Ok(stream)
}

/// Reads a 16-bit mono PCM WAV, returning its samples and rate.
fn read_wav(path: &str) -> Result<(Vec<i16>, u32), String> {
    let reader = hound::WavReader::open(path).map_err(|e| format!("cannot open '{path}': {e}"))?;
    let spec = reader.spec();
    if spec.channels != 1 {
        return Err(format!(
            "'{path}' has {} channels; this takes the 16-bit MONO PCM that `yodel encode` \
             writes (convert with: sox in.wav -c 1 mono.wav)",
            spec.channels
        ));
    }
    if spec.bits_per_sample != 16 || spec.sample_format != hound::SampleFormat::Int {
        return Err(format!(
            "'{path}' is not 16-bit integer PCM; this takes what `yodel encode` writes"
        ));
    }
    let samples: Vec<i16> = reader
        .into_samples::<i16>()
        .collect::<Result<_, _>>()
        .map_err(|e| format!("reading '{path}': {e}"))?;
    Ok((samples, spec.sample_rate))
}

/// Finds the output device, by leading-substring match or the default.
/// The device's name, or a placeholder when the backend will not say.
///
/// cpal 0.18 removed `DeviceTrait::name()`. `Device` does implement
/// `Display`, which looks like the obvious replacement and is a trap: that
/// impl propagates a failed `description()` as `fmt::Error`, and
/// `to_string()` PANICS on a `Display` that errors. Panicking part-way
/// through enumerating audio devices is not acceptable in a tool that can
/// key a transmitter, so go through `description()`, which keeps the
/// failure a value -- exactly what the old `name()` did.
fn device_name(device: &cpal::Device) -> String {
    device
        .description()
        .map_or_else(|_| "<unnamed>".to_owned(), |d| d.name().to_owned())
}

fn pick_device(want: Option<&str>) -> Result<cpal::Device, String> {
    let host = cpal::default_host();
    let Some(want) = want else {
        return host
            .default_output_device()
            .ok_or_else(|| "no default output device; try --list-devices".to_owned());
    };
    let needle = want.to_lowercase();
    let devices = host
        .output_devices()
        .map_err(|e| format!("cannot enumerate output devices: {e}"))?;
    for d in devices {
        if let Ok(desc) = d.description()
            && desc.name().to_lowercase().starts_with(&needle)
        {
            return Ok(d);
        }
    }
    Err(format!(
        "no output device whose name starts with '{want}'; run --list-devices to see them"
    ))
}

/// Picks an f32 output config running at exactly `rate`.
///
/// Refused rather than resampled: a wrong clock stretches every symbol,
/// and a modem that quietly transmits at the wrong baud rate is worse
/// than one that declines to transmit at all.
fn pick_config(device: &cpal::Device, rate: u32) -> Result<cpal::StreamConfig, String> {
    let name = device_name(device);
    let supported = device
        .supported_output_configs()
        .map_err(|e| format!("cannot query '{name}': {e}"))?;
    let mut seen = Vec::new();
    for c in supported {
        if c.sample_format() != cpal::SampleFormat::F32 {
            continue;
        }
        seen.push(format!(
            "{}-{} Hz",
            c.min_sample_rate(),
            c.max_sample_rate()
        ));
        if c.min_sample_rate() <= rate && rate <= c.max_sample_rate() {
            return Ok(c.with_sample_rate(rate).into());
        }
    }
    Err(format!(
        "'{name}' cannot run at the file's {rate} Hz (f32 ranges offered: {}). Re-encode at a \
         rate it supports, e.g. `yodel encode --rate 48000 ...`",
        if seen.is_empty() {
            "none".to_owned()
        } else {
            seen.join(", ")
        }
    ))
}

/// Lists the output devices, marking the default.
fn list_devices() -> Result<(), String> {
    let host = cpal::default_host();
    // Deliberately NOT `device_name`: this value is only ever compared
    // against the names below to place the `[default]` marker. Falling
    // back to "<unnamed>" here would make every unnamed device match
    // every other one; an empty string matches nothing, which is right.
    let default = host
        .default_output_device()
        .and_then(|d| d.description().ok())
        .map(|d| d.name().to_owned())
        .unwrap_or_default();
    let devices = host
        .output_devices()
        .map_err(|e| format!("cannot enumerate output devices: {e}"))?;
    for d in devices {
        let name = device_name(&d);
        let mark = if name == default { "  [default]" } else { "" };
        let rates = d.supported_output_configs().map_or_else(
            |_| String::new(),
            |cs| {
                let mut v: Vec<String> = cs
                    .filter(|c| c.sample_format() == cpal::SampleFormat::F32)
                    .map(|c| format!("{}-{}", c.min_sample_rate(), c.max_sample_rate()))
                    .collect();
                v.dedup();
                if v.is_empty() {
                    String::new()
                } else {
                    format!("  f32 {} Hz", v.join(", "))
                }
            },
        );
        println!("{name}{rates}{mark}");
    }
    Ok(())
}
