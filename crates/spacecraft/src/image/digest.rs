//! The content an image resolved to, as a `sha256:` digest.

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::{Error, Result};

/// Starts a digest: SHA-256 is the one algorithm Spacecraft pins images with.
const SHA256_PREFIX: &str = "sha256:";

/// The number of hex digits in a SHA-256 hash.
const SHA256_HEX_LEN: usize = 64;

/// The content an image resolved to: `sha256:` followed by 64 hex digits.
///
/// The only ways in are `parse` and serde, which runs `parse` through `try_from`, so every
/// `Digest` holds a valid value. The digest is kept exactly as given.
///
/// # Examples
///
/// ```
/// use spacecraft::image::Digest;
///
/// let digest = Digest::parse(&format!("sha256:{}", "a".repeat(64))).unwrap();
/// assert_eq!(digest.to_string(), format!("sha256:{}", "a".repeat(64)));
///
/// let error = Digest::parse("sha256:abc").unwrap_err();
/// assert!(error.to_string().contains("64 hex digits"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
// The field is visible to the rest of `image`, so the tests in `reference.rs` can write an
// expected digest without `parse`. Outside `image`, `parse` and serde stay the only ways in.
pub struct Digest(pub(super) String);

impl From<Digest> for String {
    fn from(value: Digest) -> Self {
        value.0
    }
}

impl TryFrom<String> for Digest {
    type Error = Error;

    fn try_from(value: String) -> Result<Self> {
        Self::parse(&value)
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Digest {
    /// Parses a digest: `sha256:` followed by 64 hex digits, kept exactly as given.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidImage`], quoting the digest, if it is not `sha256:` followed by
    /// 64 hex digits.
    pub fn parse(digest: &str) -> Result<Self> {
        Self::parse_within(digest, digest)
    }

    /// Parses a digest found within `image`, and names `image` in the error, so a reference
    /// with a malformed digest is reported whole.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidImage`], quoting `image`, if the digest is not `sha256:`
    /// followed by 64 hex digits.
    pub(super) fn parse_within(image: &str, digest: &str) -> Result<Self> {
        if !is_digest(digest) {
            return Err(Error::invalid_image(
                image,
                "the digest must be sha256: followed by 64 hex digits",
            ));
        }

        Ok(Self(digest.to_string()))
    }

    /// Parses a digest from its bare SHA-256 hex, with `sha256:` added in front.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidImage`], quoting the digest, if `hex` is not 64 hex digits.
    pub fn from_hex(hex: &str) -> Result<Self> {
        Self::parse(&format!("{SHA256_PREFIX}{hex}"))
    }
}

/// Returns `true` when `digest` is `sha256:` followed by 64 hex digits.
fn is_digest(digest: &str) -> bool {
    digest.strip_prefix(SHA256_PREFIX).is_some_and(|hex| {
        hex.len() == SHA256_HEX_LEN && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_digests() {
        let digest = format!("sha256:{}", "a".repeat(64));

        let parsed = Digest::parse(&digest).unwrap();

        assert_eq!(parsed.to_string(), digest);
    }

    #[test]
    fn refuses_malformed_digests() {
        for digest in [
            String::new(),
            "sha256:".to_string(),
            "sha256:short".to_string(),
            format!("md5:{}", "a".repeat(64)),
            format!("sha256:{}", "a".repeat(63)),
            format!("sha256:{}", "a".repeat(65)),
            format!("sha256:{}", "g".repeat(64)),
            format!("sha256:{}@sha256:{}", "a".repeat(64), "b".repeat(64)),
        ] {
            // On its own, the error quotes the digest.
            assert_eq!(
                Digest::parse(&digest).unwrap_err().to_string(),
                format!(
                    "invalid image \"{digest}\": the digest must be sha256: followed by 64 hex digits"
                ),
                "{digest:?}"
            );
        }
    }

    #[test]
    fn serde_runs_the_same_checks() {
        // Requests and markers carry the digest as a plain string. Reading one runs the same
        // check as `parse`, so an invalid digest cannot come in.
        let error = serde_json::from_str::<Digest>(r#""sha256:short""#).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("the digest must be sha256: followed by 64 hex digits"),
            "{error}"
        );
    }
}
