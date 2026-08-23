//! `warble gen`: deterministic impairment-controlled test-signal
//! generation (seeded noise SNR, amplitude, inter-frame gaps).

use clap::Args;

use warble::SampleRate;
use warble::aprs::{AprsPacket, Status};
use warble::tnc::TncTransmitter;

use crate::shared::{ModemArgs, fx25_samples, il2p_samples, parse_address};

/// Arguments of `warble gen`: a deterministic impairment-controlled
/// test-signal generator.
#[derive(Args)]
pub struct GenArgs {
    /// Output: a WAV file path (16-bit mono integer PCM), or `-` to
    /// stream raw s16le mono PCM to stdout (pipe it straight into
    /// `warble decode --sample-rate <HZ> -`).
    #[arg(long, value_name = "OUTPUT.wav | -")]
    out: String,

    /// How many frames to generate [range: 1..]
    #[arg(long, value_name = "N", default_value_t = 10)]
    count: u32,

    /// Source callsign, `CALL` or `CALL-SSID` [default: a placeholder]
    #[arg(long, value_name = "CALL[-SSID]", default_value = "N0CALL-1")]
    from: String,

    /// Destination callsign, `CALL` or `CALL-SSID`
    #[arg(long, value_name = "CALL[-SSID]", default_value = "APRS")]
    to: String,

    /// Base status text of every frame. The frame counter `[i/N]` is
    /// always appended, so `warble bench` can recover the expected
    /// frame count from a decoded recording.
    #[arg(long, value_name = "TEXT", default_value = "warble test signal")]
    text: String,

    /// Mix in additive white noise at this signal-to-noise ratio in
    /// dB (unit: dB relative to the generated signal's RMS; lower =
    /// noisier; ~20 is mild, ~3 is harsh, 0 means noise as strong as
    /// the signal). Omit the flag for a clean signal. The noise is
    /// drawn from a seeded in-crate PRNG, so output is reproducible.
    #[arg(long, value_name = "DB", allow_hyphen_values = true)]
    snr: Option<f64>,

    /// Signal amplitude as a fraction of full scale [range: >0..=1.0]
    #[arg(long, value_name = "FRACTION", default_value_t = 0.5)]
    level: f64,

    /// Silence between frames in milliseconds
    #[arg(long, value_name = "MS", default_value_t = 150)]
    gap_ms: u32,

    /// Seed of the deterministic noise PRNG: the same flags + seed
    /// always produce byte-identical output
    #[arg(long, value_name = "U64", default_value_t = 1)]
    seed: u64,

    /// Output sample rate in Hz [range: 8000..=48000]
    #[arg(
        long = "sample-rate",
        visible_alias = "rate",
        value_name = "HZ",
        default_value_t = 44_100
    )]
    sample_rate: u32,

    #[command(flatten)]
    modem: ModemArgs,
}

/// Deterministic 64-bit linear congruential generator (the Knuth MMIX
/// multiplier/increment, high bits taken as output) used for the
/// seeded noise of `warble gen`. Hand-rolled on purpose: no dependency,
/// no wall clock, so the same seed and flags always produce
/// byte-identical audio. Statistical quality is plenty for test noise;
/// it is not cryptographic.
struct NoiseRng(u64);

impl NoiseRng {
    /// One warm-up step so nearby seeds diverge immediately.
    fn new(seed: u64) -> Self {
        let mut rng = NoiseRng(seed);
        let _ = rng.next_u64();
        rng
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0
    }

    /// Uniform in [-1.0, 1.0), built from the top 53 bits (the
    /// strongest bits of an LCG).
    fn next_unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 52) as f64 - 1.0
    }
}

/// Rounds and saturates a float sample into the i16 range.
fn clamp_i16(v: f64) -> i16 {
    v.round().clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
}

/// Runs `warble gen`: synthesizes `--count` sequence-numbered status
/// frames, applies the amplitude and seeded-noise impairments, and
/// writes a WAV file or raw s16le PCM to stdout.
pub fn generate(args: &GenArgs) -> Result<(), String> {
    if args.count == 0 {
        return Err("bad --count '0': at least one frame is required".to_owned());
    }
    if !(args.level > 0.0 && args.level <= 1.0) {
        return Err(format!(
            "bad --level '{}': a fraction in (0.0, 1.0] is required",
            args.level
        ));
    }
    if let Some(db) = args.snr
        && !db.is_finite()
    {
        return Err(format!("bad --snr '{db}': a finite dB value is required"));
    }
    let src = parse_address(&args.from)?;
    let dest = parse_address(&args.to)?;
    let rate = SampleRate::new(args.sample_rate)
        .map_err(|e| format!("bad sample rate '{}': {e}", args.sample_rate))?;
    let config = args.modem.config(rate)?;
    let tx = TncTransmitter::new(config);
    let gap = (u64::from(args.gap_ms) * u64::from(rate.hz()) / 1000) as usize;

    // Assemble the clean signal: frames at --level with silent gaps,
    // accumulating the signal power over the frame (non-gap) samples
    // so the SNR is defined against the signal itself, not diluted by
    // the silence between transmissions.
    let mut audio: Vec<i16> = Vec::new();
    let mut power_sum = 0.0f64;
    let mut signal_samples = 0u64;
    for i in 1..=args.count {
        let text = format!("{} [{}/{}]", args.text, i, args.count);
        let packet = AprsPacket::Status(Status {
            text: text.as_bytes(),
        });
        let samples = if args.modem.fx25 {
            fx25_samples(&tx, config, &packet, dest, src, &[])?
        } else if args.modem.il2p {
            il2p_samples(&tx, config, &packet, dest, src, &[])?
        } else {
            tx.transmit_to_vec_i16(&packet, dest, src, &[])
                .map_err(|e| format!("building frame {i}: {e}"))?
        };
        for s in samples {
            let v = f64::from(s) * args.level;
            power_sum += v * v;
            signal_samples += 1;
            audio.push(clamp_i16(v));
        }
        audio.extend(std::iter::repeat_n(0i16, gap));
    }

    // Seeded additive white noise over the WHOLE stream (gaps
    // included, like a real channel): uniform in [-A, A] where
    // A = signal_rms / 10^(snr_dB/20) * sqrt(3), because uniform noise
    // has RMS A/sqrt(3).
    if let Some(db) = args.snr {
        let rms = (power_sum / signal_samples.max(1) as f64).sqrt();
        let amplitude = rms / 10f64.powf(db / 20.0) * 3f64.sqrt();
        let mut rng = NoiseRng::new(args.seed);
        for s in &mut audio {
            *s = clamp_i16(f64::from(*s) + rng.next_unit() * amplitude);
        }
    }

    if args.out == "-" {
        use std::io::Write;
        let stdout = std::io::stdout();
        let mut out = std::io::BufWriter::new(stdout.lock());
        for s in &audio {
            out.write_all(&s.to_le_bytes())
                .map_err(|e| format!("writing stdout: {e}"))?;
        }
        out.flush().map_err(|e| format!("writing stdout: {e}"))?;
        return Ok(());
    }
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: rate.hz(),
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let out = args.out.as_str();
    let mut writer =
        hound::WavWriter::create(out, spec).map_err(|e| format!("creating '{out}': {e}"))?;
    for &s in &audio {
        writer
            .write_sample(s)
            .map_err(|e| format!("writing '{out}': {e}"))?;
    }
    writer
        .finalize()
        .map_err(|e| format!("finalizing '{out}': {e}"))?;
    Ok(())
}
