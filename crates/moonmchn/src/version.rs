//! `moonmchn` version.
//!
//! # Examples
//!
//! ```
//! assert!(!moonmchn::VERSION.is_empty());
//! ```

/// Version of this crate, as declared in `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    #[test]
    fn version_matches_package_version() {
        assert_eq!(crate::VERSION, env!("CARGO_PKG_VERSION"));
    }
}
