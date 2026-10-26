//! smolmachines backend adapter for Spacecraft.

mod config;
mod engine;
mod image;
mod session;

pub use config::{egress_interceptor, machine_config, mount, port, resources};
pub use engine::{Engine, Machine};
pub use session::session;
