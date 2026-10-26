//! Resolution of an image as the user names it: an archive's path becomes absolute.

use std::fs;
use std::path::Path;

use super::{Image, STDIN_IMAGE};
use crate::{Error, Result};

impl Image {
    /// Resolves `image`, as the user passes it to `machine start --image`, to the image to
    /// boot. A registry reference stays as given; an archive path becomes absolute against the
    /// current directory, because the daemon runs in another.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidImage`] for stdin (`-`), a directory, or an image
    /// [`Image::parse`] refuses, and [`Error::Io`] when the archive cannot be reached.
    pub fn resolve(image: &str) -> Result<Self> {
        if image == STDIN_IMAGE {
            return Err(Error::invalid_image(
                image,
                "an image on stdin is not supported; pass the archive's path",
            ));
        }

        if Self::is_local(image) {
            let path =
                fs::canonicalize(image).map_err(|source| Error::io(Path::new(image), source))?;
            // A directory is no archive, and an `Image` cannot hold a path that is not UTF-8.
            let path = path
                .into_os_string()
                .into_string()
                .ok()
                .filter(|path| Path::new(path).is_file())
                .ok_or_else(|| {
                    Error::invalid_image(image, "a local image must be a docker save archive file")
                })?;
            return Self::parse(&path);
        }

        Image::parse(image)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_an_archive_path_to_its_canonical_form() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("guest.tar");
        // Core never reads the archive, so any file will do.
        fs::write(&path, "").unwrap();

        let image = Image::resolve(path.to_str().unwrap()).unwrap();

        // On macOS the temporary directory sits under `/var`, a symlink to `/private/var`,
        // so the image keeps the canonical path.
        let canonical = path.canonicalize().unwrap();
        assert_eq!(image.as_str(), canonical.to_str().unwrap());
    }

    #[test]
    fn a_missing_archive_is_an_io_error() {
        // Cargo runs the tests from the crate's directory, which holds no `guest.tar`.
        let error = Image::resolve("./guest.tar").unwrap_err();

        assert_eq!(error.to_string(), "failed to access ./guest.tar");
        assert!(matches!(error, Error::Io { .. }));
    }

    #[test]
    fn refuses_stdin_and_directories() {
        let tempdir = tempfile::tempdir().unwrap();
        let dir = tempdir.path().to_str().unwrap();

        assert_eq!(
            Image::resolve("-").unwrap_err().to_string(),
            "invalid image \"-\": an image on stdin is not supported; pass the archive's path"
        );
        assert_eq!(
            Image::resolve(dir).unwrap_err().to_string(),
            format!("invalid image \"{dir}\": a local image must be a docker save archive file")
        );
    }

    #[test]
    fn a_registry_reference_stays_as_given() {
        let pinned = format!("ghcr.io/org/name@sha256:{}", "a".repeat(64));
        for reference in ["ghcr.io/org/name:latest", pinned.as_str()] {
            assert_eq!(Image::resolve(reference).unwrap().as_str(), reference);
        }
    }
}
