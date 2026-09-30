//! `spacecraft` library.
//!
//! The command-line interface in this package is a thin wrapper over this
//! crate. Depend on it with `default-features = false` to skip the CLI-only
//! dependencies.
//!
//! # Examples
//!
//! ```
//! assert!(!spacecraft::VERSION.is_empty());
//! ```

mod error;

pub use error::{Error, Result};

/// Version of this crate, as declared in `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    #[test]
    fn version_matches_package_version() {
        assert_eq!(crate::VERSION, env!("CARGO_PKG_VERSION"));
    }
}
