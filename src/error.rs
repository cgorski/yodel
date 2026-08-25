//! Error types for `yodel`.
//!
//! All fallible constructors in this crate return [`ConfigError`]. Each
//! variant records the offending value alongside the rule it violated so
//! that the rendered message is self-explanatory.

use core::fmt;

/// An invalid modem configuration value.
///
/// Returned by every validated constructor in the crate
/// ([`crate::SampleRate::new`], [`crate::BaudRate::new`],
/// [`crate::TonePair::new`] and the config types built from them), so an
/// invalid configuration cannot be represented.
///
/// # Typed variant + self-explanatory `Display`
///
/// Each variant records the offending value alongside the violated rule,
/// and the rendered message repeats both:
///
/// ```
/// use yodel::{ConfigError, SampleRate};
///
/// // 7000 Hz is below the supported 8000..=48000 Hz range.
/// let err = SampleRate::new(7_000).unwrap_err();
/// assert_eq!(
///     err,
///     ConfigError::SampleRateOutOfRange {
///         got: 7_000,
///         min: 8_000,
///         max: 48_000,
///     }
/// );
/// assert_eq!(
///     err.to_string(),
///     "sample rate 7000 Hz is out of range: must be within 8000..=48000 Hz"
/// );
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigError {
    /// The sample rate lies outside the supported range.
    SampleRateOutOfRange {
        /// The rejected sample rate in Hz.
        got: u32,
        /// The lowest supported sample rate in Hz.
        min: u32,
        /// The highest supported sample rate in Hz.
        max: u32,
    },
    /// The baud rate lies outside the supported range.
    BaudRateInvalid {
        /// The rejected baud rate in bits per second.
        got: u32,
        /// The lowest supported baud rate.
        min: u32,
        /// The highest supported baud rate.
        max: u32,
    },
    /// A tone frequency is zero or too high for the sample rate.
    ToneOutOfRange {
        /// The rejected tone frequency in Hz.
        got: u32,
        /// The exclusive upper bound (the Nyquist frequency) in Hz.
        nyquist: u32,
    },
    /// The baud rate exceeds the sample rate, so a bit would span less
    /// than one sample.
    BaudExceedsSampleRate {
        /// The rejected baud rate in bits per second.
        baud: u32,
        /// The configured sample rate in Hz.
        sample_rate: u32,
    },
    /// A slicer-bank space-gain sweep had an invalid chain count.
    SweepLenInvalid {
        /// The rejected number of gains.
        got: usize,
        /// The largest supported number of gains.
        max: usize,
    },
    /// A slicer-bank space-gain sweep contained a zero gain.
    SweepGainZero {
        /// The index of the zero gain within the sweep.
        index: usize,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            ConfigError::SampleRateOutOfRange { got, min, max } => write!(
                f,
                "sample rate {got} Hz is out of range: must be within {min}..={max} Hz"
            ),
            ConfigError::BaudRateInvalid { got, min, max } => write!(
                f,
                "baud rate {got} is invalid: must be within {min}..={max} bit/s"
            ),
            ConfigError::ToneOutOfRange { got, nyquist } => write!(
                f,
                "tone {got} Hz is out of range: must be nonzero and below the Nyquist frequency {nyquist} Hz"
            ),
            ConfigError::BaudExceedsSampleRate { baud, sample_rate } => write!(
                f,
                "baud rate {baud} exceeds sample rate {sample_rate} Hz: each bit needs at least one sample"
            ),
            ConfigError::SweepLenInvalid { got, max } => write!(
                f,
                "space-gain sweep length {got} is invalid: must be within 1..={max}"
            ),
            ConfigError::SweepGainZero { index } => write!(
                f,
                "space-gain sweep entry {index} is zero: gains must be positive Q8 values"
            ),
        }
    }
}

impl core::error::Error for ConfigError {}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::string::ToString;

    #[test]
    fn display_sample_rate_out_of_range() {
        let e = ConfigError::SampleRateOutOfRange {
            got: 7_000,
            min: 8_000,
            max: 48_000,
        };
        assert_eq!(
            e.to_string(),
            "sample rate 7000 Hz is out of range: must be within 8000..=48000 Hz"
        );
    }

    #[test]
    fn display_baud_rate_invalid() {
        let e = ConfigError::BaudRateInvalid {
            got: 0,
            min: 1,
            max: 9_600,
        };
        assert_eq!(
            e.to_string(),
            "baud rate 0 is invalid: must be within 1..=9600 bit/s"
        );
    }

    #[test]
    fn display_tone_out_of_range() {
        let e = ConfigError::ToneOutOfRange {
            got: 5_000,
            nyquist: 4_000,
        };
        assert_eq!(
            e.to_string(),
            "tone 5000 Hz is out of range: must be nonzero and below the Nyquist frequency 4000 Hz"
        );
    }

    #[test]
    fn display_baud_exceeds_sample_rate() {
        let e = ConfigError::BaudExceedsSampleRate {
            baud: 9_600,
            sample_rate: 8_000,
        };
        assert_eq!(
            e.to_string(),
            "baud rate 9600 exceeds sample rate 8000 Hz: each bit needs at least one sample"
        );
    }

    #[test]
    fn error_trait_object() {
        let e: &dyn core::error::Error = &ConfigError::BaudRateInvalid {
            got: 0,
            min: 1,
            max: 9_600,
        };
        assert!(e.source().is_none());
    }

    #[test]
    fn error_is_copy_and_eq() {
        let e = ConfigError::ToneOutOfRange { got: 1, nyquist: 2 };
        let e2 = e;
        assert_eq!(e, e2);
    }
}
