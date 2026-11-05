//! smolmachines backend adapter for Spacecraft.

mod config;
mod engine;
mod gateway;
mod image;
mod session;

pub use config::{egress_endpoint, machine_config, mount, port, resources};
pub use engine::{Engine, Machine};
pub use gateway::{Gateway, PreambleError};
pub use session::session;
