//! `moonmchn`

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::print_stdout,
        clippy::print_stderr,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::arithmetic_side_effects,
        reason = "Tests may panic and print diagnostics"
    )
)]

pub mod config;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod daemon;
pub(crate) mod dirs;
mod error;
pub mod image;
pub mod machine;
mod setup;
mod version;

pub use config::{ByteSize, Config};
pub use error::{Error, Result};
pub use setup::setup;
pub use version::VERSION;
