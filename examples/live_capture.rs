//! LIVE CAPTURE: decode APRS straight off the default sound-card input.
//!
//! * **Scenario** — a receive-only monitor: decode whatever the radio
//!   plugged into your sound card is hearing, live.
//! * **Hardware** — any host with an audio input: a radio's speaker or
//!   discriminator output into the mic/line-in, or a USB audio adapter.
//!   `cpal` picks the default input device.
//! * **Features** — `tnc,capture`. `capture` pulls in `cpal` and exists
//!   for this example only; it is never a library dependency.
//!
//! ```sh
//! cargo run --example live_capture --features tnc,capture
//! ```
//!
//! The `capture` feature (off by default) pulls in `cpal` for device
//! I/O; nothing in the library depends on it. The flow:
//!
//! 1. Open the default input device via cpal and read its default
//!    input configuration (rate, channel count, sample format).
//! 2. Plan the rate: if the device rate is inside the modem's
//!    8000..=48000 Hz window it is used as-is; if it is an exact
//!    integer multiple of a rate in that window, a simple
//!    keep-every-Nth decimator brings it down (crude but adequate for
//!    Bell 202's ≤ 2.2 kHz tones — proper polyphase resampling is out
//!    of scope here). Anything else is refused with guidance rather
//!    than decoding garbage at a wrong clock.
//! 3. Convert each callback buffer to mono `i16` (average the channels,
//!    scale f32 to i16), decimate, and push every sample into the sync
//!    `TncReceiver`, printing one `decode_to_log`-style line per FCS-valid
//!    frame.
//!
//! The conversion / downmix / decimation / chunk-feed logic is pure
//! (no device, no I/O) and lives in [`plumbing`]; the host test suite
//! (`tests/live_capture.rs`) drives it with a synthesized fake source,
//! so CI proves the plumbing without ever opening an audio device.

/// Device-free plumbing: rate planning, sample conversion, channel
/// downmix, and the chunk-feed into the receiver. Pure so tests can
/// prove it against synthesized audio.
pub mod plumbing {
    use yodel::ax25::{Address, UiFrame};
    use yodel::tnc::DefaultTncReceiver;

    /// The modem's supported rate window (see `SampleRate::new`).
    pub const MIN_RATE_HZ: u32 = 8_000;
    /// Upper edge of the modem's supported rate window.
    pub const MAX_RATE_HZ: u32 = 48_000;

    /// A workable plan for a device rate: decode at `decode_hz`,
    /// keeping one sample in every `keep_every` from the device.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct RatePlan {
        /// The rate the decoder runs at (inside 8000..=48000 Hz).
        pub decode_hz: u32,
        /// Keep one device sample in every this-many (1 = no decimation).
        pub keep_every: u32,
    }

    /// Plans how to feed a device running at `device_hz` into the
    /// modem: direct if the rate is in-window, an integer-ratio
    /// decimation if an exact divisor lands in-window, otherwise an
    /// error telling the user what to do (proper fractional resampling
    /// is out of scope for this example).
    pub fn plan_rate(device_hz: u32) -> Result<RatePlan, String> {
        if (MIN_RATE_HZ..=MAX_RATE_HZ).contains(&device_hz) {
            return Ok(RatePlan {
                decode_hz: device_hz,
                keep_every: 1,
            });
        }
        if device_hz > MAX_RATE_HZ {
            // Prefer the smallest factor: the highest in-window rate.
            for factor in 2..=(device_hz / MIN_RATE_HZ).max(2) {
                if device_hz.is_multiple_of(factor) {
                    let down = device_hz / factor;
                    if (MIN_RATE_HZ..=MAX_RATE_HZ).contains(&down) {
                        return Ok(RatePlan {
                            decode_hz: down,
                            keep_every: factor,
                        });
                    }
                }
            }
        }
        Err(format!(
            "device sample rate {device_hz} Hz is outside the modem's \
             {MIN_RATE_HZ}..={MAX_RATE_HZ} Hz window and no integer-ratio decimation \
             reaches it; configure the device (or your OS sound settings) for a rate \
             in that window — 48000 Hz is the safe choice — or record with a capture \
             tool that resamples and pipe it into `yodel decode -`"
        ))
    }

    /// Scales one f32 sample (nominal -1.0..=1.0) to i16, saturating
    /// out-of-range input instead of wrapping.
    #[must_use]
    pub fn f32_to_i16(sample: f32) -> i16 {
        let scaled = sample * 32767.0;
        if scaled >= 32767.0 {
            32767
        } else if scaled <= -32768.0 {
            -32768
        } else {
            // In range by the checks above; truncation toward zero.
            #[allow(clippy::cast_possible_truncation)]
            {
                scaled as i16
            }
        }
    }

    /// Averages one interleaved frame (`channels` samples) down to a
    /// single mono i16 sample.
    #[must_use]
    pub fn downmix_frame_i16(frame: &[i16]) -> i16 {
        if frame.is_empty() {
            return 0;
        }
        let sum: i64 = frame.iter().map(|&s| i64::from(s)).sum();
        #[allow(clippy::cast_possible_truncation)]
        {
            (sum / frame.len() as i64) as i16
        }
    }

    /// Streaming feed: downmixes interleaved device chunks to mono,
    /// applies the planned decimation, pushes each sample into the
    /// receiver, and collects one formatted line per decoded frame.
    /// State (decimation phase, sample clock) persists across chunks so
    /// callback-sized buffers behave identically to one long slice.
    pub struct ChunkFeed {
        channels: usize,
        keep_every: u32,
        phase: u32,
        decode_hz: u32,
        sample_pos: u64,
    }

    impl ChunkFeed {
        /// A feed for `channels`-channel interleaved input under `plan`.
        #[must_use]
        pub fn new(channels: usize, plan: RatePlan) -> Self {
            Self {
                channels: channels.max(1),
                keep_every: plan.keep_every.max(1),
                phase: 0,
                decode_hz: plan.decode_hz,
                sample_pos: 0,
            }
        }

        /// Feeds one interleaved i16 chunk; returns a log line per
        /// frame that completed inside it.
        pub fn push_i16(
            &mut self,
            interleaved: &[i16],
            rx: &mut DefaultTncReceiver,
        ) -> Vec<String> {
            let mut lines = Vec::new();
            for frame in interleaved.chunks(self.channels) {
                let mono = downmix_frame_i16(frame);
                if self.phase == 0 {
                    self.sample_pos += 1;
                    if let Some(rx_frame) = rx.push_i16(mono) {
                        lines.push(format_line(
                            self.sample_pos,
                            self.decode_hz,
                            rx_frame.ui_frame(),
                        ));
                    }
                }
                self.phase = (self.phase + 1) % self.keep_every;
            }
            lines
        }

        /// Feeds one interleaved f32 chunk (scaled to i16 first).
        pub fn push_f32(
            &mut self,
            interleaved: &[f32],
            rx: &mut DefaultTncReceiver,
        ) -> Vec<String> {
            let converted: Vec<i16> = interleaved.iter().map(|&s| f32_to_i16(s)).collect();
            self.push_i16(&converted, rx)
        }
    }

    /// One monitor line in the `decode_to_log` style: relative
    /// sample-clock timestamp, `SRC>DEST[,PATH*...]`, raw info text.
    #[must_use]
    pub fn format_line(sample_pos: u64, rate_hz: u32, frame: &UiFrame<'_>) -> String {
        #[allow(clippy::cast_precision_loss)] // display only
        let secs = sample_pos as f64 / f64::from(rate_hz.max(1));
        let mut line = format!(
            "[{secs:9.3}s] {}>{}",
            fmt_addr(&frame.src),
            fmt_addr(&frame.dest)
        );
        for hop in frame.hops() {
            line.push(',');
            line.push_str(&fmt_addr(&hop.address));
            if hop.repeated {
                line.push('*');
            }
        }
        line.push_str(": ");
        line.extend(frame.info.iter().map(|&b| {
            if (b' '..=b'~').contains(&b) {
                b as char
            } else {
                '.'
            }
        }));
        line
    }

    /// Formats an address as `CALL` or `CALL-SSID`.
    fn fmt_addr(addr: &Address) -> String {
        let call = String::from_utf8_lossy(addr.callsign.as_bytes()).into_owned();
        match addr.ssid.value() {
            0 => call,
            n => format!("{call}-{n}"),
        }
    }
}

// ---------------------------------------------------------------------
// Device I/O edge: everything below needs the `capture` feature (cpal)
// and never runs in tests.
// ---------------------------------------------------------------------

#[cfg(feature = "capture")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use plumbing::{ChunkFeed, plan_rate};
    use yodel::SampleRate;
    use yodel::tnc::{DefaultTncReceiver, TncConfig, TncReceiver};

    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or("no default input device; check your OS sound settings")?;
    let config = device.default_input_config()?;
    // cpal 0.18: `SampleRate` is a plain `u32` alias, not a newtype.
    let device_hz = config.sample_rate();
    let channels = usize::from(config.channels());
    let plan = plan_rate(device_hz)?;
    eprintln!(
        "input: '{}' at {device_hz} Hz, {channels} channel(s); decoding at {} Hz \
         (keeping 1 in {})",
        // cpal 0.18 removed `name()`. `Device: Display` looks like the
        // replacement but panics via `to_string()` when `description()`
        // fails, so go through `description()` and keep the fallback.
        device
            .description()
            .map_or_else(|_| "<unnamed>".to_owned(), |d| d.name().to_owned()),
        plan.decode_hz,
        plan.keep_every
    );

    let rate = SampleRate::new(plan.decode_hz)?;
    let mut rx: DefaultTncReceiver = TncReceiver::new(TncConfig::bell_202(rate)?)?;
    let mut feed = ChunkFeed::new(channels, plan);

    // The cpal callback runs on an audio thread; ship raw chunks over
    // a channel and do all DSP on the main thread so the callback
    // stays allocation-light and never blocks on stdout.
    let (tx_chunks, rx_chunks) = std::sync::mpsc::channel::<Vec<i16>>();
    let err_fn = |e| eprintln!("stream error: {e}");
    let stream = match config.sample_format() {
        // cpal 0.18 takes the config by value (`StreamConfig` is `Copy`).
        cpal::SampleFormat::I16 => device.build_input_stream(
            config.into(),
            move |data: &[i16], _| {
                let _ = tx_chunks.send(data.to_vec());
            },
            err_fn,
            None,
        )?,
        cpal::SampleFormat::F32 => device.build_input_stream(
            config.into(),
            move |data: &[f32], _| {
                let _ = tx_chunks.send(data.iter().map(|&s| plumbing::f32_to_i16(s)).collect());
            },
            err_fn,
            None,
        )?,
        other => {
            return Err(format!(
                "unsupported device sample format {other:?}; i16 and f32 inputs are handled"
            )
            .into());
        }
    };
    stream.play()?;
    eprintln!("listening; Ctrl-C to stop");

    // Chunks arrive already converted to i16 (interleaved).
    for chunk in rx_chunks {
        for line in feed.push_i16(&chunk, &mut rx) {
            println!("{line}");
        }
    }
    Ok(())
}

/// Without the `capture` feature there is no device backend; explain
/// how to build the real thing. (Also keeps `#[path]` inclusion of the
/// pure plumbing above compiling in the device-free test suite.)
#[cfg(not(feature = "capture"))]
fn main() {
    eprintln!("rebuild with: cargo run --example live_capture --features tnc,capture");
}
