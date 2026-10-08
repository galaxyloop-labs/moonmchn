//! `spacecraft`

#![cfg_attr(
    not(test),
    warn(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::print_stdout,
        clippy::print_stderr
    )
)]

pub mod config;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod daemon;
mod error;
mod setup;
mod version;

pub use config::{ByteSize, Config};
pub use error::{Error, Result};
pub use setup::setup;
pub use version::VERSION;
