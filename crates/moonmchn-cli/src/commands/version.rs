//! `mchn version`: print the version of this build.

use std::io::Write;

use anyhow::Context;

/// Writes `v{version}` to `out`, where `{version}` is the package version
/// declared in `Cargo.toml`.
///
/// # Errors
///
/// Fails when writing to `out` fails.
pub(crate) fn run(out: &mut impl Write) -> anyhow::Result<()> {
    writeln!(out, "v{}", moonmchn::VERSION).context("failed to write version")
}

#[cfg(test)]
mod tests {
    use super::run;

    #[test]
    fn writes_v_prefixed_package_version() {
        let mut out = Vec::new();

        run(&mut out).unwrap();

        assert_eq!(out, format!("v{}\n", moonmchn::VERSION).as_bytes());
    }
}
