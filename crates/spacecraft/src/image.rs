//! Guest images: what a machine boots, and the content that image resolved to.
//!
//! - [`Image`] is what a machine boots, as the user named it: an [`Archive`] or a registry
//!   [`Reference`]. Code that holds one matches on its kind.
//! - [`Archive`] is the absolute path of a `docker save` archive on the host.
//! - [`Digest`] is the content the image resolved to at `machine start`: `sha256:` followed by
//!   64 hex digits. A machine records it next to the image, so it boots exactly that content,
//!   and can boot it again.
//! - [`Reference`] is a registry reference that starts with its registry host, split into
//!   host, path, tag and digest: the parts a registry client needs, and the way to pin a
//!   reference to its digest.
//!
//! [`Image`] and [`Digest`] hold only valid values: `parse` checks them, and serde runs the
//! same check on every request and marker. [`Image::default`] is the image a machine boots
//! when `machine start` names none.
//!
//! # Examples
//!
//! ```
//! use spacecraft::image::{Digest, Image};
//!
//! // What a machine boots when `machine start` names none, and the content it resolved to.
//! let image = Image::default();
//! assert_eq!(image.as_str(), "ghcr.io/galaxyloop-labs/spacecraft-images:default");
//! let digest = Digest::parse(&format!("sha256:{}", "a".repeat(64))).unwrap();
//!
//! // The parts a registry client needs, from the reference the image holds.
//! let Image::Registry(reference) = &image else {
//!     unreachable!("the default image is a registry reference");
//! };
//! assert_eq!(reference.host(), "ghcr.io");
//! assert_eq!(reference.tag(), "default");
//!
//! // What the engine pulls: the image pinned to its digest, so a tag that moves later
//! // changes nothing for this machine.
//! assert_eq!(
//!     image.pinned(&digest),
//!     format!("ghcr.io/galaxyloop-labs/spacecraft-images@{digest}")
//! );
//! ```

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::{Error, Result};

mod archive;
mod digest;
mod reference;
mod resolve;
pub use archive::Archive;
pub use digest::Digest;
pub use reference::Reference;

/// The image that makes the engine read a `docker save` archive from stdin.
const STDIN_IMAGE: &str = "-";

/// The prefixes that make an image a path on the host.
const LOCAL_PATH_PREFIXES: [&str; 3] = ["/", "./", "../"];

/// The suffixes that make an image a `docker save` archive on the host, plain or gzipped.
const ARCHIVE_SUFFIXES: [&str; 3] = [".tar", ".tar.gz", ".tgz"];

/// An image a machine can boot: a `docker save` archive on the host, or an image in a
/// registry.
///
/// Every `Image` holds a valid value, of the kind [`Image::is_local`] picks: `parse` checks
/// it, serde runs `parse` through `try_from`, and other crates get an [`Archive`] or a
/// [`Reference`] from `parse` alone. The image is kept exactly as given.
///
/// # Examples
///
/// ```
/// use spacecraft::image::Image;
///
/// let archive = Image::parse("/images/guest.tar").unwrap();
/// assert!(matches!(archive, Image::Archive(_)));
/// assert_eq!(archive.as_str(), "/images/guest.tar");
///
/// // The engine would resolve a relative path against the daemon's working directory.
/// let error = Image::parse("./guest.tar").unwrap_err();
/// assert!(error.to_string().contains("must be an absolute path"));
///
/// // A bare name means one thing to the engine's default registry and another to Docker Hub.
/// let error = Image::parse("alpine").unwrap_err();
/// assert!(error.to_string().contains("must start with its registry host"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
// `Image` stays exhaustive. `#[non_exhaustive]`, which `Error` carries, would force a `_` arm
// in other crates, and the compiler would stop checking that a `match` there, such as the
// engine adapter's, handles every kind.
pub enum Image {
    /// A `docker save` archive on the host, by its absolute path.
    Archive(Archive),
    /// An image in a registry, by a reference that starts with its registry host.
    Registry(Reference),
}

impl From<Image> for String {
    fn from(value: Image) -> Self {
        value.as_str().to_string()
    }
}

impl TryFrom<String> for Image {
    type Error = Error;

    fn try_from(value: String) -> Result<Self> {
        Self::parse(&value)
    }
}

impl fmt::Display for Image {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl Default for Image {
    /// The image a machine boots when `machine start` names none: the `default` tag of the
    /// repository that holds every Spacecraft image.
    fn default() -> Self {
        Self::Registry(Reference::default())
    }
}

impl Image {
    /// Parses an image: the absolute path of a `docker save` archive, or a registry reference
    /// that starts with its registry host. [`Image::is_local`] picks the kind.
    ///
    /// The image is kept exactly as given.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidImage`] if a local image is a relative path, or a registry
    /// reference has no registry host, an empty path or tag, or a malformed digest.
    pub fn parse(image: &str) -> Result<Self> {
        if Self::is_local(image) {
            Archive::parse(image).map(Self::Archive)
        } else {
            Reference::parse(image).map(Self::Registry)
        }
    }

    /// Returns `true` when the engine reads `image` from the host, and `false` when it pulls it
    /// from a registry.
    ///
    /// An image is local when it starts with `/`, `./` or `../`, is `-`, or ends in `.tar`,
    /// `.tar.gz` or `.tgz`. Only the string counts: the path need not exist.
    ///
    /// The rule mirrors the one `smolvm --help` states in smolvm 1.29.1, so Spacecraft and the
    /// engine agree on which images are archives. A suffix that a later smolvm adds stays a
    /// registry reference until it is added here.
    ///
    /// ponytail: Spacecraft copies one engine's rule, as `REMOTE_SCHEMES` does for mounts. If a
    /// second engine ever arrives, each engine answers for its own images through the `Engine`
    /// trait.
    ///
    /// # Examples
    ///
    /// ```
    /// use spacecraft::image::Image;
    ///
    /// assert!(Image::is_local("/images/guest.tar"));
    /// assert!(Image::is_local("./guest.tar.gz"));
    /// assert!(Image::is_local("guest.tgz"));
    /// assert!(Image::is_local("-"));
    /// assert!(!Image::is_local("ghcr.io/org/name:latest"));
    /// ```
    #[must_use]
    pub fn is_local(image: &str) -> bool {
        image == STDIN_IMAGE
            || LOCAL_PATH_PREFIXES
                .iter()
                .any(|prefix| image.starts_with(prefix))
            || ARCHIVE_SUFFIXES
                .iter()
                .any(|suffix| image.ends_with(suffix))
    }

    /// Returns the image exactly as given.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Archive(archive) => archive.as_str(),
            Self::Registry(reference) => reference.as_str(),
        }
    }

    /// Returns the image to boot: a registry reference pinned to `digest`, in the form
    /// `host/path@sha256:hex`, or an archive's path unchanged, since an archive has no digest
    /// syntax.
    ///
    /// # Examples
    ///
    /// ```
    /// use spacecraft::image::{Digest, Image};
    ///
    /// let digest = Digest::parse(&format!("sha256:{}", "a".repeat(64))).unwrap();
    ///
    /// let registry = Image::parse("ghcr.io/org/name:latest").unwrap();
    /// assert_eq!(registry.pinned(&digest), format!("ghcr.io/org/name@{digest}"));
    ///
    /// let archive = Image::parse("/images/guest.tar").unwrap();
    /// assert_eq!(archive.pinned(&digest), "/images/guest.tar");
    /// ```
    #[must_use]
    pub fn pinned(&self, digest: &Digest) -> String {
        match self {
            Self::Archive(archive) => archive.as_str().to_string(),
            Self::Registry(reference) => reference.pinned(digest),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_local_copies_the_engine_rule() {
        // A prefix, `-` or an archive suffix makes an image local, whatever the path holds in
        // between: the rule reads the string alone.
        let local_images = [
            "/abs/x.tar",
            "./x.tar",
            "../x.tar",
            "-",
            "x.tar",
            "x.tar.gz",
            "x.tgz",
            "/dir",
            "/images/../dir",
            "/images//./dir",
            "./images/../dir",
            "../images//./dir",
            "images/../x.tar",
            "images//x.tar.gz",
            "images/./x.tgz",
            "images/../nested//./x.tar.gz",
        ];
        for image in local_images {
            assert!(Image::is_local(image), "local image: {image:?}");
        }

        // Everything else is a registry reference: names, tags and hosts; a suffix the engine
        // does not list, `.gz` alone included, or one that ends a directory's name; uppercase
        // suffixes, since the engine lists them in lowercase; and `.` and `..`, which lack the
        // `./` and `../` prefixes.
        let references = [
            "alpine",
            "alpine:latest",
            "ghcr.io/org/name:latest",
            "localhost:5000/x",
            "tarball",
            "",
            "x.zip",
            "xtar",
            "xtgz",
            "xgz",
            "x.gz",
            "xtar.gz",
            "x.gz.bak",
            "x.tar.bak",
            "x.tar.gz.bak",
            "x.tgz.bak",
            "images/../nested//./x.gz",
            "images.tar/alpine:latest",
            "alpine:tar",
            "x.TAR",
            "x.TAR.GZ",
            "x.TGZ",
            "x.GZ",
            "images/../x.gZ",
            "images/../x.TaR.gZ",
            ".",
            "..",
        ];
        for image in references {
            assert!(!Image::is_local(image), "registry reference: {image:?}");
        }
    }

    #[test]
    fn parses_images() {
        // Both kinds of image: an archive's absolute path, and registry references with a tag,
        // or with a port and a digest. Each comes back as its kind, exactly as given.
        let digest = format!("sha256:{}", "a".repeat(64));
        for (image, is_archive) in [
            ("/abs/guest.tar".to_string(), true),
            ("ghcr.io/org/name:latest".to_string(), false),
            (format!("localhost:5000/org/name@{digest}"), false),
        ] {
            let parsed = Image::parse(&image).unwrap();
            if is_archive {
                assert!(matches!(parsed, Image::Archive(_)), "{image}");
            } else {
                assert!(matches!(parsed, Image::Registry(_)), "{image}");
            }
            assert_eq!(parsed.as_str(), image);
            assert_eq!(parsed.to_string(), image);
        }
    }

    #[test]
    fn refuses_malformed_images() {
        // One image of each kind, each breaking its kind's rule: a relative archive path, and a
        // bare name with no registry host. The message quotes the image and names the rule.
        let cases = [
            (
                "./guest.tar",
                "a local image must be an absolute path to a docker save archive",
            ),
            (
                "alpine",
                "a registry reference must start with its registry host",
            ),
        ];
        for (image, reason) in cases {
            assert_eq!(
                Image::parse(image).unwrap_err().to_string(),
                format!("invalid image \"{image}\": {reason}"),
                "{image:?}"
            );
        }
    }

    #[test]
    fn pins_an_image() {
        let hex = "a".repeat(64);
        let digest = Digest::parse(&format!("sha256:{hex}")).unwrap();

        // A registry image gets the digest in place of its tag. An archive has no digest
        // syntax, so its path stays as it is.
        let registry = Image::parse("ghcr.io/org/name:latest").unwrap();
        assert_eq!(
            registry.pinned(&digest),
            format!("ghcr.io/org/name@sha256:{hex}")
        );
        let archive = Image::parse("/abs/guest.tar").unwrap();
        assert_eq!(archive.pinned(&digest), "/abs/guest.tar");
    }

    #[test]
    fn serde_runs_the_same_checks() {
        // Requests and markers carry the image as a plain string. Reading one runs the same
        // check as `parse`, so an invalid image cannot come in.
        let error = serde_json::from_str::<Image>(r#""./guest.tar""#).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("a local image must be an absolute path to a docker save archive"),
            "{error}"
        );

        // A valid value is written as the plain string and reads back equal.
        let image = Image::parse("/abs/guest.tar").unwrap();
        let json = serde_json::to_string(&image).unwrap();
        assert_eq!(json, r#""/abs/guest.tar""#);
        assert_eq!(serde_json::from_str::<Image>(&json).unwrap(), image);
    }
}
