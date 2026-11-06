//! `spacecraft setup`: create the initial user configuration.

use std::io::Write;

use anyhow::Context;

/// Creates the configuration, then writes its path to `out`.
///
/// # Errors
///
/// Fails when setup fails or writing to `out` fails.
pub(crate) fn run(force: bool, out: &mut impl Write) -> anyhow::Result<()> {
    let path = spacecraft::setup(force)?;
    writeln!(out, "Created {}", path.display()).context("failed to write setup result")
}
