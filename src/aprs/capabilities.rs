//! Station capability reports — the `<` data type identifier.
//!
//! A station announces what it can do as a comma-separated list of
//! tokens, each either a bare flag or a `KEY=VALUE` pair:
//!
//! ```text
//! <IGATE,MSG_CNT=13,LOC_CNT=54
//! ```
//!
//! The specification fixes neither the token set nor their order, and
//! new tokens appear without warning, so this module does not attempt
//! to enumerate them. It gives you the list; you decide what you
//! recognize. That keeps the type accurate as the convention drifts.
//!
//! ```
//! use warble::aprs::capabilities::Capabilities;
//!
//! let cap = Capabilities::parse(b"<IGATE,MSG_CNT=13,LOC_CNT=54")?;
//! assert!(cap.has(b"IGATE"));
//! assert_eq!(cap.value(b"MSG_CNT"), Some(&b"13"[..]));
//! assert_eq!(cap.value(b"MISSING"), None);
//! assert_eq!(cap.tokens().count(), 3);
//! # Ok::<(), warble::aprs::AprsError>(())
//! ```

use super::AprsError;

/// The `<` data type identifier introducing a capability report.
pub const CAPABILITIES_DTI: u8 = b'<';

/// A station capability report (`<`).
///
/// Borrows the information field; [`Capabilities::tokens`] walks the
/// comma-separated list without allocating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities<'a> {
    /// Everything after the `<`, verbatim.
    pub body: &'a [u8],
}

impl<'a> Capabilities<'a> {
    /// Parses a capability report from a complete information field.
    ///
    /// # Errors
    ///
    /// [`AprsError::InvalidDataType`] when `info` does not begin with
    /// `<`, and [`AprsError::Truncated`] when it is empty.
    pub const fn parse(info: &'a [u8]) -> Result<Self, AprsError> {
        match info.split_first() {
            Some((&CAPABILITIES_DTI, body)) => Ok(Self { body }),
            Some((&got, _)) => Err(AprsError::InvalidDataType { got }),
            None => Err(AprsError::Truncated {
                expected: 1,
                got: 0,
            }),
        }
    }

    /// Iterates the comma-separated tokens, empty ones skipped.
    pub fn tokens(&self) -> impl Iterator<Item = &'a [u8]> {
        self.body.split(|&b| b == b',').filter(|t| !t.is_empty())
    }

    /// Whether a bare flag token (no `=`) equal to `name` is present.
    #[must_use]
    pub fn has(&self, name: &[u8]) -> bool {
        self.tokens().any(|t| t == name)
    }

    /// The value of the first `name=value` token, if present.
    ///
    /// Returns `Some(b"")` for a token written as `name=` with nothing
    /// after the separator, which is distinct from `None`.
    #[must_use]
    pub fn value(&self, name: &[u8]) -> Option<&'a [u8]> {
        self.tokens().find_map(|token| {
            let eq = token.iter().position(|&b| b == b'=')?;
            (token.get(..eq)? == name).then(|| token.get(eq + 1..))?
        })
    }

    /// The serialized length in bytes.
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        1 + self.body.len()
    }

    /// Serializes into `buf`, returning the written length.
    ///
    /// # Errors
    ///
    /// [`AprsError::Truncated`] when `buf` is smaller than
    /// [`Capabilities::encoded_len`].
    pub fn build(&self, buf: &mut [u8]) -> Result<usize, AprsError> {
        let need = self.encoded_len();
        if buf.len() < need {
            return Err(AprsError::Truncated {
                expected: need,
                got: buf.len(),
            });
        }
        buf[0] = CAPABILITIES_DTI;
        buf[1..need].copy_from_slice(self.body);
        Ok(need)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_an_igate_report() {
        let cap = Capabilities::parse(b"<IGATE,MSG_CNT=13,LOC_CNT=54").expect("parse");
        assert!(cap.has(b"IGATE"));
        assert!(!cap.has(b"MSG_CNT"), "key=value is not a bare flag");
        assert_eq!(cap.value(b"MSG_CNT"), Some(&b"13"[..]));
        assert_eq!(cap.value(b"LOC_CNT"), Some(&b"54"[..]));
        assert_eq!(cap.value(b"IGATE"), None, "bare flag has no value");
        let tokens: heapless_vec::Collected = cap.tokens().collect();
        assert_eq!(tokens.len(), 3);
    }

    #[test]
    fn empty_body_and_empty_tokens() {
        let cap = Capabilities::parse(b"<").expect("parse");
        assert_eq!(cap.body, b"");
        assert_eq!(cap.tokens().count(), 0);

        let cap = Capabilities::parse(b"<,,A,,").expect("parse");
        assert_eq!(cap.tokens().count(), 1);
        assert!(cap.has(b"A"));
    }

    #[test]
    fn distinguishes_absent_from_empty_value() {
        let cap = Capabilities::parse(b"<K=").expect("parse");
        assert_eq!(cap.value(b"K"), Some(&b""[..]));
        assert_eq!(cap.value(b"J"), None);
    }

    #[test]
    fn rejects_the_wrong_identifier() {
        assert!(matches!(
            Capabilities::parse(b">status"),
            Err(AprsError::InvalidDataType { got: b'>' })
        ));
        assert!(matches!(
            Capabilities::parse(b""),
            Err(AprsError::Truncated { .. })
        ));
    }

    #[test]
    fn round_trips() {
        let original = b"<IGATE,MSG_CNT=13";
        let cap = Capabilities::parse(original).expect("parse");
        let mut buf = [0u8; 32];
        let n = cap.build(&mut buf).expect("build");
        assert_eq!(&buf[..n], original);

        let mut small = [0u8; 4];
        assert!(matches!(
            cap.build(&mut small),
            Err(AprsError::Truncated { .. })
        ));
    }

    /// Minimal fixed-capacity collector so the test needs no `alloc`.
    mod heapless_vec {
        pub struct Collected {
            len: usize,
        }

        impl Collected {
            pub const fn len(&self) -> usize {
                self.len
            }
        }

        impl<'a> FromIterator<&'a [u8]> for Collected {
            fn from_iter<T: IntoIterator<Item = &'a [u8]>>(iter: T) -> Self {
                Self {
                    len: iter.into_iter().count(),
                }
            }
        }
    }
}
