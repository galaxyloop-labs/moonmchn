//! `docker save` archives on the host, by their absolute path.

use std::path::Path;

use crate::{Error, Result};

/// A `docker save` archive on the host, by its absolute path.
///
/// Other crates get one from [`Image::parse`](super::Image::parse) alone, which checks that the
/// path is absolute. The path is kept exactly as given, and the file need not exist.
///
/// # Examples
///
/// ```
/// use moonmchn::image::Image;
///
/// let Image::Archive(archive) = Image::parse("/images/guest.tar").unwrap() else {
///     unreachable!("an absolute path is an archive");
/// };
/// assert_eq!(archive.as_str(), "/images/guest.tar");
///
/// // The engine would resolve a relative path against the daemon's working directory.
/// assert!(Image::parse("./guest.tar").is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Archive(String);

impl Archive {
    /// Parses the absolute path of a `docker save` archive, kept exactly as given.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidImage`], quoting the path, if it is relative.
    // Crate-private, as `Reference::parse` is, so other crates get an `Archive` from
    // `Image::parse` alone.
    pub(crate) fn parse(image: &str) -> Result<Self> {
        // On Unix, a path is absolute when it starts with `/`. The engine resolves a relative
        // one against the daemon's working directory, wherever `daemon start` ran.
        if !Path::new(image).is_absolute() {
            return Err(Error::invalid_image(
                image,
                "a local image must be an absolute path to a docker save archive",
            ));
        }

        Ok(Self(image.to_string()))
    }

    /// Returns the archive's path as a string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
