//! WAV input helpers (`wav` feature): header validation, byte-stream
//! sniffing, and a whole-file sync decode.
//!
//! This is the library-side home of the WAV plumbing the `yodel` CLI
//! uses: [`check_spec`] validates a header against what the modem
//! accepts, [`sniff_pcm`] tells a WAV byte stream from raw s16le PCM by
//! its first four bytes (the `yodel decode -` behavior), and — with
//! the `tnc` feature — [`decode_frames`] / [`decode_sniffed`] run the
//! audio through a Bell 202 receiver, handing each decoded frame to a
//! caller-supplied sink. The `asynk` adapter layer
//! ([`crate::asynk::decode_wav`], [`crate::asynk::decode_stream`],
//! `async` feature) drives the same functions from a blocking-pool
//! thread, so the sync and async paths cannot drift apart.

use core::fmt;
use std::io::Read;
use std::path::Path;

#[cfg(feature = "tnc")]
use crate::tnc::{DefaultTncReceiver, OwnedFrame, TncConfig, TncStats};
use crate::types::{SAMPLE_RATE_MAX, SAMPLE_RATE_MIN, SampleRate};

/// A WAV input failure: an unsupported header, a codec/IO error from
/// `hound`, or an invalid modem configuration derived from the header.
#[derive(Debug)]
pub enum WavError {
    /// The WAV is not 16-bit mono integer PCM.
    UnsupportedFormat {
        /// Channel count found in the header.
        channels: u16,
        /// Bits per sample found in the header.
        bits_per_sample: u16,
        /// Whether the samples are floating point.
        float: bool,
    },
    /// The WAV sample rate is outside the supported range.
    UnsupportedRate {
        /// The rejected rate in Hz.
        hz: u32,
    },
    /// Reading or parsing the WAV failed.
    Wav(hound::Error),
    /// The modem configuration derived from the header was invalid.
    Config(crate::ConfigError),
    /// A raw PCM stream (no RIFF header) arrived without a sample
    /// rate: raw s16le carries no rate of its own, so the caller must
    /// supply one.
    RateRequired,
    /// The caller supplied a sample rate that contradicts the rate in
    /// the stream's own WAV header.
    RateContradiction {
        /// The rate the WAV header declares, in Hz.
        header_hz: u32,
        /// The rate the caller supplied, in Hz.
        given_hz: u32,
    },
}

impl fmt::Display for WavError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            WavError::UnsupportedFormat {
                channels,
                bits_per_sample,
                float,
            } => write!(
                f,
                "got {channels} channel(s), {bits_per_sample} bits, {} samples; \
                 16-bit mono integer PCM is required",
                if float { "float" } else { "integer" }
            ),
            WavError::UnsupportedRate { hz } => write!(
                f,
                "got {hz} Hz, supported: {SAMPLE_RATE_MIN}..={SAMPLE_RATE_MAX} Hz"
            ),
            WavError::Wav(ref e) => write!(f, "WAV codec: {e}"),
            WavError::Config(ref e) => write!(f, "configuration: {e}"),
            WavError::RateRequired => write!(
                f,
                "raw PCM carries no sample-rate header; a sample rate is required"
            ),
            WavError::RateContradiction {
                header_hz,
                given_hz,
            } => write!(
                f,
                "the given sample rate ({given_hz} Hz) contradicts the WAV header \
                 ({header_hz} Hz)"
            ),
        }
    }
}

impl std::error::Error for WavError {}

impl From<hound::Error> for WavError {
    fn from(e: hound::Error) -> Self {
        WavError::Wav(e)
    }
}

impl From<crate::ConfigError> for WavError {
    fn from(e: crate::ConfigError) -> Self {
        WavError::Config(e)
    }
}

/// Validates a WAV header (16-bit mono integer PCM at a supported rate)
/// and returns the validated sample rate.
///
/// # Errors
///
/// [`WavError::UnsupportedFormat`] for anything but 16-bit mono integer
/// PCM; [`WavError::UnsupportedRate`] when the rate is outside
/// [`SAMPLE_RATE_MIN`]`..=`[`SAMPLE_RATE_MAX`].
pub fn check_spec(spec: &hound::WavSpec) -> Result<SampleRate, WavError> {
    if spec.channels != 1
        || spec.bits_per_sample != 16
        || spec.sample_format != hound::SampleFormat::Int
    {
        return Err(WavError::UnsupportedFormat {
            channels: spec.channels,
            bits_per_sample: spec.bits_per_sample,
            float: spec.sample_format == hound::SampleFormat::Float,
        });
    }
    SampleRate::new(spec.sample_rate).map_err(|_| WavError::UnsupportedRate {
        hz: spec.sample_rate,
    })
}

/// Decodes a whole 16-bit mono PCM WAV through a Bell 202 receiver,
/// calling `sink` with each FCS-valid frame.
///
/// The receiver is a [`DefaultTncReceiver`] built with
/// [`TncConfig::bell_202`] at the file's sample rate. `sink` returns
/// whether decoding should continue: return `false` to stop early (the
/// remaining samples are skipped). On success the receiver's final
/// statistics are returned.
///
/// # Errors
///
/// [`WavError::Wav`] when opening or reading the file fails;
/// [`WavError::UnsupportedFormat`] / [`WavError::UnsupportedRate`] for a
/// header the modem cannot accept; [`WavError::Config`] when no valid
/// Bell 202 configuration exists at the file's rate.
#[cfg(feature = "tnc")]
pub fn decode_frames<P, F>(path: P, sink: F) -> Result<TncStats, WavError>
where
    P: AsRef<Path>,
    F: FnMut(OwnedFrame) -> bool,
{
    let mut reader = hound::WavReader::open(path)?;
    let rate = check_spec(&reader.spec())?;
    run_receiver(
        rate,
        reader.samples::<i16>().map(|s| s.map_err(WavError::from)),
        sink,
    )
}

/// A byte stream with its sniffed four-byte prefix replayed in front,
/// so downstream readers see the stream from its first byte.
pub type Replayed<R> = std::io::Chain<std::io::Cursor<Vec<u8>>, R>;

/// A PCM byte stream after [`sniff_pcm`] classified it.
pub enum SniffedPcm<R: Read> {
    /// The stream begins with a RIFF header: a validated WAV
    /// (16-bit mono integer PCM at a supported rate).
    Wav {
        /// The sample rate from the WAV header.
        rate: SampleRate,
        /// The WAV reader, positioned at the first sample.
        reader: hound::WavReader<Replayed<R>>,
    },
    /// No RIFF header: raw signed 16-bit little-endian mono PCM at the
    /// caller-supplied rate.
    Raw {
        /// The caller-supplied sample rate.
        rate: SampleRate,
        /// The raw byte stream, sniffed prefix included.
        reader: Replayed<R>,
    },
}

impl<R: Read> SniffedPcm<R> {
    /// The validated sample rate, whichever shape the stream took.
    #[must_use]
    pub fn rate(&self) -> SampleRate {
        match *self {
            SniffedPcm::Wav { rate, .. } | SniffedPcm::Raw { rate, .. } => rate,
        }
    }
}

/// Tells a WAV byte stream from raw s16le PCM by its first four bytes.
///
/// This is the intake behind `yodel decode -` and
/// `yodel serve --input -`, available to library users (and the
/// `asynk` layer) so nobody re-implements the sniff: a stream opening
/// with `RIFF` is parsed as WAV — the sample rate comes from the
/// header, validated by [`check_spec`] — and anything else is treated
/// as raw signed 16-bit little-endian mono PCM at the rate in `rate`.
///
/// The `rate` argument is required for raw streams (raw PCM has no
/// rate of its own) and optional for WAV; when it is given AND the
/// stream has a WAV header, the two must agree.
///
/// # Errors
///
/// [`WavError::RateRequired`] for a raw stream without `rate`;
/// [`WavError::RateContradiction`] when `rate` disagrees with the WAV
/// header; [`WavError::UnsupportedFormat`] /
/// [`WavError::UnsupportedRate`] for a WAV header the modem cannot
/// accept; [`WavError::Wav`] for read or parse failures.
pub fn sniff_pcm<R: Read>(
    mut reader: R,
    rate: Option<SampleRate>,
) -> Result<SniffedPcm<R>, WavError> {
    let mut head = [0u8; 4];
    let mut got = 0usize;
    while got < head.len() {
        match reader.read(&mut head[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(WavError::Wav(e.into())),
        }
    }
    let replay = std::io::Cursor::new(head[..got].to_vec()).chain(reader);
    if got == head.len() && head == *b"RIFF" {
        let wav = hound::WavReader::new(replay)?;
        let header = check_spec(&wav.spec())?;
        if let Some(given) = rate
            && given.hz() != header.hz()
        {
            return Err(WavError::RateContradiction {
                header_hz: header.hz(),
                given_hz: given.hz(),
            });
        }
        return Ok(SniffedPcm::Wav {
            rate: header,
            reader: wav,
        });
    }
    let rate = rate.ok_or(WavError::RateRequired)?;
    Ok(SniffedPcm::Raw {
        rate,
        reader: replay,
    })
}

/// Decodes a sniffed PCM stream (see [`sniff_pcm`]) through a Bell 202
/// receiver, calling `sink` with each FCS-valid frame.
///
/// The WAV and raw shapes decode identically once the samples are out
/// of the bytes; `sink` returns whether decoding should continue
/// (return `false` to stop early). On success the receiver's final
/// statistics are returned. In the raw shape, a trailing odd byte at
/// EOF is an error, not a silent drop.
///
/// # Errors
///
/// [`WavError::Wav`] when reading fails; [`WavError::Config`] when no
/// valid Bell 202 configuration exists at the stream's rate.
#[cfg(feature = "tnc")]
pub fn decode_sniffed<R, F>(input: SniffedPcm<R>, sink: F) -> Result<TncStats, WavError>
where
    R: Read,
    F: FnMut(OwnedFrame) -> bool,
{
    match input {
        SniffedPcm::Wav { rate, mut reader } => run_receiver(
            rate,
            reader.samples::<i16>().map(|s| s.map_err(WavError::from)),
            sink,
        ),
        SniffedPcm::Raw { rate, mut reader } => {
            run_receiver(rate, raw_s16le_samples(&mut reader), sink)
        }
    }
}

/// The shared receive loop of [`decode_frames`] and [`decode_sniffed`]:
/// a Bell 202 receiver at `rate` over any fallible sample iterator.
#[cfg(feature = "tnc")]
fn run_receiver<F>(
    rate: SampleRate,
    samples: impl Iterator<Item = Result<i16, WavError>>,
    mut sink: F,
) -> Result<TncStats, WavError>
where
    F: FnMut(OwnedFrame) -> bool,
{
    let config = TncConfig::bell_202(rate)?;
    let mut rx = DefaultTncReceiver::new(config)?;
    for sample in samples {
        if let Some(frame) = rx.push_i16(sample?) {
            // A frame from a DefaultTncReceiver always fits in an
            // OwnedFrame (same capacity), so this cannot fail.
            let Ok(owned) = OwnedFrame::new(&frame) else {
                continue;
            };
            if !sink(owned) {
                break;
            }
        }
    }
    Ok(rx.stats())
}

/// Signed 16-bit little-endian samples out of a raw byte reader, until
/// EOF; a trailing odd byte is an error, not a silent drop.
#[cfg(feature = "tnc")]
fn raw_s16le_samples<R: Read>(reader: &mut R) -> impl Iterator<Item = Result<i16, WavError>> + '_ {
    let mut done = false;
    std::iter::from_fn(move || {
        if done {
            return None;
        }
        let mut bytes = [0u8; 2];
        let mut filled = 0usize;
        while filled < bytes.len() {
            match reader.read(&mut bytes[filled..]) {
                Ok(0) if filled == 0 => {
                    done = true;
                    return None;
                }
                Ok(0) => {
                    done = true;
                    let e = std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "truncated sample (odd byte count) at EOF",
                    );
                    return Some(Err(WavError::Wav(e.into())));
                }
                Ok(n) => filled += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => {
                    done = true;
                    return Some(Err(WavError::Wav(e.into())));
                }
            }
        }
        Some(Ok(i16::from_le_bytes(bytes)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WAV bytes (16-bit mono integer PCM at `hz`) in memory.
    fn wav_bytes(hz: u32, samples: &[i16]) -> Vec<u8> {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: hz,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut cursor = std::io::Cursor::new(Vec::new());
        let mut writer = hound::WavWriter::new(&mut cursor, spec).unwrap();
        for &s in samples {
            writer.write_sample(s).unwrap();
        }
        writer.finalize().unwrap();
        cursor.into_inner()
    }

    #[test]
    fn sniff_honors_wav_header() {
        let bytes = wav_bytes(44_100, &[1, -2, 3]);
        let sniffed = sniff_pcm(std::io::Cursor::new(bytes), None).unwrap();
        assert_eq!(sniffed.rate().hz(), 44_100);
        let SniffedPcm::Wav { mut reader, .. } = sniffed else {
            panic!("WAV bytes classified as raw");
        };
        let samples: Vec<i16> = reader.samples::<i16>().map(|s| s.unwrap()).collect();
        assert_eq!(samples, [1, -2, 3], "sniff must not eat header bytes");
    }

    #[test]
    fn sniff_matching_rate_hint_is_accepted() {
        let bytes = wav_bytes(44_100, &[0; 4]);
        let hint = SampleRate::new(44_100).unwrap();
        let sniffed = sniff_pcm(std::io::Cursor::new(bytes), Some(hint)).unwrap();
        assert!(matches!(sniffed, SniffedPcm::Wav { .. }));
    }

    #[test]
    fn sniff_rejects_contradicting_rate() {
        let bytes = wav_bytes(44_100, &[0; 4]);
        let hint = SampleRate::new(48_000).unwrap();
        let err = match sniff_pcm(std::io::Cursor::new(bytes), Some(hint)) {
            Err(e) => e,
            Ok(_) => panic!("contradicting rate hint accepted"),
        };
        assert!(matches!(
            err,
            WavError::RateContradiction {
                header_hz: 44_100,
                given_hz: 48_000,
            }
        ));
    }

    #[test]
    fn sniff_raw_needs_a_rate() {
        let err = match sniff_pcm(std::io::Cursor::new(vec![0u8; 64]), None) {
            Err(e) => e,
            Ok(_) => panic!("raw stream without a rate accepted"),
        };
        assert!(matches!(err, WavError::RateRequired));
    }

    #[test]
    fn sniff_raw_with_rate_replays_the_prefix() {
        let rate = SampleRate::new(48_000).unwrap();
        let bytes = vec![1, 0, 2, 0, 3, 0];
        let sniffed = sniff_pcm(std::io::Cursor::new(bytes.clone()), Some(rate)).unwrap();
        let SniffedPcm::Raw { mut reader, rate } = sniffed else {
            panic!("raw bytes classified as WAV");
        };
        assert_eq!(rate.hz(), 48_000);
        let mut back = Vec::new();
        reader.read_to_end(&mut back).unwrap();
        assert_eq!(back, bytes, "the sniffed prefix must be replayed");
    }

    #[test]
    fn sniff_short_stream_is_raw() {
        // Fewer than four bytes cannot be a WAV; with a rate they are
        // (truncated) raw PCM, replayed intact.
        let rate = SampleRate::new(48_000).unwrap();
        let sniffed = sniff_pcm(std::io::Cursor::new(vec![7u8, 0]), Some(rate)).unwrap();
        let SniffedPcm::Raw { mut reader, .. } = sniffed else {
            panic!("short stream classified as WAV");
        };
        let mut back = Vec::new();
        reader.read_to_end(&mut back).unwrap();
        assert_eq!(back, [7, 0]);
    }
}
