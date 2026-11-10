//! Entry points for the fuzz targets under `fuzz/`, which `cargo fuzz` builds with
//! `--cfg fuzzing`. They reach the decider's private parsing, so they are no part of the
//! library's API, and no other build has them.

/// Returns the name a flow's opening bytes claim, as the decider reads it: the SNI of the TLS
/// `ClientHello` they hold, before normalization.
#[must_use]
pub fn claimed_name(opening: &[u8]) -> Option<String> {
    super::decider::claimed_name(opening)
}
