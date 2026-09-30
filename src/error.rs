/// Errors produced by this crate.
///
/// No operation fails yet; variants are added alongside fallible APIs.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {}

/// [`std::result::Result`] with this crate's [`Error`] as the default error type.
pub type Result<T, E = Error> = std::result::Result<T, E>;
