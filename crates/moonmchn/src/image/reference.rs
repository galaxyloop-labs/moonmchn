//! Registry references, split into host, path, tag and digest.

use super::Digest;
use crate::{Error, Result};

/// Ends the registry host: everything before the first one is the host.
const HOST_SEPARATOR: char = '/';

/// A registry host holds one of these: a `.` in a domain name or an IP address, or a `:`
/// before a port.
const HOST_MARKERS: [char; 2] = ['.', ':'];

/// The one registry host that needs no marker.
const LOCALHOST: &str = "localhost";

/// Starts the digest, after the path and the tag.
const DIGEST_SEPARATOR: char = '@';

/// Starts the tag: the last one after the host, once the digest is split off.
const TAG_SEPARATOR: char = ':';

/// The tag of a registry reference that names none.
const DEFAULT_TAG: &str = "latest";

/// The registry host of the image a machine boots by default.
const DEFAULT_IMAGE_HOST: &str = "ghcr.io";

/// The repository of the image a machine boots by default: the one that holds every Moon MCHN
/// image.
const DEFAULT_IMAGE_PATH: &str = "galaxyloop-labs/moonmchn-images";

/// The tag of the image a machine boots by default.
const DEFAULT_IMAGE_TAG: &str = "default";

/// A registry reference in the form `host[:port]/path[:tag][@sha256:hex]`, split into its
/// parts.
///
/// The host must hold a `.` or a `:`, or be `localhost`, so a bare name such as `alpine` is
/// refused. A reference without a tag gets `latest`.
///
/// Other crates get one from [`Image::parse`](super::Image::parse), for an image that
/// [`Image::is_local`](super::Image::is_local) reads as a registry reference, or from `Default`,
/// the reference of the image a machine boots by default, so every `Reference` holds a valid
/// reference. The reference is kept exactly as given: the parts fill in what it leaves out, such
/// as the tag, and `as_str` returns it unchanged.
///
/// # Examples
///
/// ```
/// use moonmchn::image::Image;
///
/// let Image::Registry(reference) = Image::parse("localhost:5000/org/name").unwrap() else {
///     unreachable!("localhost:5000 is a registry host");
/// };
/// assert_eq!(reference.host(), "localhost:5000");
/// assert_eq!(reference.path(), "org/name");
/// assert_eq!(reference.tag(), "latest");
/// assert_eq!(reference.digest(), None);
/// assert_eq!(reference.as_str(), "localhost:5000/org/name");
///
/// assert!(Image::parse("alpine").is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    /// The reference exactly as given.
    image: String,
    /// The registry host, including its port when present.
    host: String,
    /// The repository path within the registry, without a leading slash.
    path: String,
    /// The image tag, defaulting to `latest` when omitted.
    tag: String,
    /// The optional content digest, including its `sha256:` prefix.
    digest: Option<Digest>,
}

impl Reference {
    /// Splits a registry reference into its host, path, tag, and optional digest.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidImage`] if the registry host is missing, the path
    /// or tag is empty, or the digest is not `sha256:` followed by 64 hex digits.
    // Crate-private, so other crates get a `Reference` from `Image::parse` alone, and its kind
    // agrees with `Image::is_local`: this check passes `./x`, whose `.` reads as a host, and
    // `ghcr.io/org/x.tar`, both of which `is_local` calls local.
    pub(crate) fn parse(image: &str) -> Result<Self> {
        // Without a separator, the host and the path stay empty, and the check below refuses
        // them.
        let (host, rest) = image.split_once(HOST_SEPARATOR).unwrap_or_default();
        // Split the digest before the tag so its `sha256:` colon stays intact.
        let (repository, digest) = match rest.split_once(DIGEST_SEPARATOR) {
            Some((repository, digest)) => (repository, Some(digest)),
            None => (rest, None),
        };
        let (path, tag) = repository
            .rsplit_once(TAG_SEPARATOR)
            .unwrap_or((repository, DEFAULT_TAG));

        // The engine's default registry and Docker Hub disagree on what a bare `alpine` means,
        // so a reference must name its host. The same reason covers an empty path or tag.
        if !is_registry_host(host) || path.is_empty() || tag.is_empty() {
            return Err(Error::invalid_image(
                image,
                "a registry reference must start with its registry host",
            ));
        }
        // A malformed digest is reported with the whole reference.
        let digest = if let Some(digest) = digest {
            Some(Digest::parse_within(image, digest)?)
        } else {
            None
        };

        Ok(Self {
            image: image.to_string(),
            host: host.to_string(),
            path: path.to_string(),
            tag: tag.to_string(),
            digest,
        })
    }

    /// Returns the reference exactly as given.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.image
    }

    /// Returns the registry host, including its port when present.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Returns the repository path within the registry, without a leading slash.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns the image tag: `latest` when the reference names none.
    #[must_use]
    pub fn tag(&self) -> &str {
        &self.tag
    }

    /// Returns the digest the reference pins, including its `sha256:` prefix, or `None` when it
    /// pins none.
    #[must_use]
    pub fn digest(&self) -> Option<&Digest> {
        self.digest.as_ref()
    }

    /// Returns a pinned reference in the form `host/path@sha256:hex`.
    #[must_use]
    pub fn pinned(&self, digest: &Digest) -> String {
        let Self { host, path, .. } = self;
        format!("{host}{HOST_SEPARATOR}{path}{DIGEST_SEPARATOR}{digest}")
    }
}

impl Default for Reference {
    /// The reference of the image a machine boots when `machine start` names none.
    fn default() -> Self {
        // Built from its parts: `parse` returns a `Result`, and the library never panics. The
        // `image` module's example checks the outcome against the reference written out.
        let (host, path, tag) = (DEFAULT_IMAGE_HOST, DEFAULT_IMAGE_PATH, DEFAULT_IMAGE_TAG);
        Self {
            image: format!("{host}{HOST_SEPARATOR}{path}{TAG_SEPARATOR}{tag}"),
            host: host.to_string(),
            path: path.to_string(),
            tag: tag.to_string(),
            digest: None,
        }
    }
}

/// Whether `host` names a registry: it holds a `.` or a `:`, or it is `localhost`.
fn is_registry_host(host: &str) -> bool {
    host.contains(HOST_MARKERS) || host == LOCALHOST
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_references() {
        // Every hex digit, in both cases: the digest check accepts either.
        let hex: String = "0123456789abcdef".chars().cycle().take(64).collect();
        let lower = format!("sha256:{hex}");
        let upper = format!("sha256:{}", hex.to_uppercase());
        let cases = [
            (
                "ghcr.io/galaxyloop-labs/moonmchn-images:default".to_string(),
                "ghcr.io",
                "galaxyloop-labs/moonmchn-images",
                "default",
                None,
            ),
            (
                "localhost:5000/org/name".to_string(),
                "localhost:5000",
                "org/name",
                "latest",
                None,
            ),
            (
                "127.0.0.1:5000/org/name:v2".to_string(),
                "127.0.0.1:5000",
                "org/name",
                "v2",
                None,
            ),
            (
                format!("ghcr.io/org/name@{lower}"),
                "ghcr.io",
                "org/name",
                "latest",
                Some(Digest(lower.clone())),
            ),
            (
                format!("ghcr.io/org/name:v2@{lower}"),
                "ghcr.io",
                "org/name",
                "v2",
                Some(Digest(lower)),
            ),
            (
                "localhost/name".to_string(),
                "localhost",
                "name",
                "latest",
                None,
            ),
            (
                format!("localhost:5000/org/nested/name:v2@{upper}"),
                "localhost:5000",
                "org/nested/name",
                "v2",
                Some(Digest(upper)),
            ),
        ];

        for (input, host, path, tag, digest) in cases {
            let reference = Reference::parse(&input).unwrap();

            // The parts fill in what the reference leaves out, and `as_str` keeps it as given:
            // `localhost:5000/org/name` reads `latest` from `tag()`, with no `:latest` added.
            assert_eq!(reference.as_str(), input);
            assert_eq!(reference.host(), host, "{input}");
            assert_eq!(reference.path(), path, "{input}");
            assert_eq!(reference.tag(), tag, "{input}");
            assert_eq!(reference.digest(), digest.as_ref(), "{input}");
        }
    }

    #[test]
    fn refuses_malformed_references() {
        for input in [
            "",
            "alpine",
            "library/alpine",
            "/org/name",
            "ghcr.io",
            "ghcr.io/",
            "ghcr.io/:latest",
            "ghcr.io/org/name:",
        ] {
            let error = Reference::parse(input).unwrap_err();
            assert!(
                matches!(error, Error::InvalidImage { image, reason }
                    if image == input
                        && reason == "a registry reference must start with its registry host"),
                "{input}"
            );
        }
    }

    #[test]
    fn refuses_malformed_digests() {
        // Inside a reference, the error quotes the whole reference.
        let input = "ghcr.io/org/name@sha256:short";
        let error = Reference::parse(input).unwrap_err();

        assert!(
            matches!(error, Error::InvalidImage { image, reason }
                if image == input
                    && reason == "the digest must be sha256: followed by 64 hex digits"),
            "{input}"
        );
    }
}
