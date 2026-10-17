//! smolmachines backend adapter for Spacecraft.

mod config;
mod engine;

pub use config::{egress_interceptor, machine_config, mount, port, resources};
pub use engine::{Engine, Machine};
