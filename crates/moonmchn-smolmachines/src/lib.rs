//! smolmachines backend adapter for Moon MCHN.

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

mod config;
mod engine;
mod gateway;
mod image;
mod session;

pub use config::{egress_endpoint, machine_config, mount, port, resources};
pub use engine::{Engine, Machine};
pub use gateway::{Gateway, PreambleError};
pub use session::session;
