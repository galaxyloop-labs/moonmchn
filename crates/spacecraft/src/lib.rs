//! `spacecraft`

pub mod config;
mod error;
mod setup;
mod version;

pub use config::{ByteSize, Config};
pub use error::{Error, Result};
pub use setup::setup;
pub use version::VERSION;
