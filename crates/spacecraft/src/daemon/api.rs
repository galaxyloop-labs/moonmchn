//! Spacecraft daemon API module.

use crate::machine;
use serde::{Deserialize, Serialize};

/// Request to the daemon.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Request {
    /// Request the status of the daemon.
    Status {},
    /// Request the daemon to shut down.
    Shutdown {
        /// Whether to force the shutdown.
        #[serde(default)]
        force: bool,
    },
    /// Request the start of a machine.
    MachineStart {
        /// The name of the machine to start.
        name: String,
        /// The configuration for the machine.
        config: machine::Config,
    },
    /// Request the stop of a machine.
    MachineStop {
        /// The name of the machine to stop.
        name: String,
    },
    /// Request the status of a machine.
    MachineStatus {
        /// The name of the machine to get the status of.
        name: String,
    },
    /// Request the list of machines.
    MachineList {},
    /// Request the resolution of a machine.
    MachineResolve {
        /// The name of the machine to resolve.
        name: String,
    },
}

/// Reply variant from the daemon.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reply<T> {
    /// Reply indicating the daemon is done processing the request.
    Done(T),
    /// Reply indicating an error occurred while processing the request.
    Error {
        /// The error message.
        message: String,
    },
}

/// Status of the daemon.
#[derive(Debug, Serialize, Deserialize)]
pub struct Status {
    /// The process ID of the daemon.
    pub pid: u32,
    /// The version of the daemon.
    pub version: String,
}

/// Machine start reply from the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineStart {
    /// The machine was started successfully.
    Started(machine::Status),
    /// The machine was already running.
    AlreadyRunning(machine::Status),
}

/// Machine stop reply from the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineStop {
    /// The machine was stopped successfully.
    Stopped,
    /// The machine was not running.
    NotRunning,
}

/// Machine status reply from the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineStatus {
    /// The status of the machine.
    pub status: machine::Status,
    /// The record of the machine, if available.
    pub record: Option<machine::Record>,
}

#[cfg(test)]
mod tests {
    use crate::config::Resources;
    use crate::image::{Digest, Image};
    use crate::machine::State::Running;

    use super::*;

    #[test]
    fn request_status_and_shutdown_payloads() {
        let status = Request::Status {};
        let shutdown_force_false = Request::Shutdown { force: false };
        let shutdown_force_true = Request::Shutdown { force: true };

        assert_eq!(serde_json::to_string(&status).unwrap(), r#"{"status":{}}"#);
        assert_eq!(
            serde_json::to_string(&shutdown_force_false).unwrap(),
            r#"{"shutdown":{"force":false}}"#
        );
        assert_eq!(
            serde_json::to_string(&shutdown_force_true).unwrap(),
            r#"{"shutdown":{"force":true}}"#
        );
    }

    #[test]
    fn shutdown_without_force_still_parses() {
        let shutdown: Request = serde_json::from_str(r#"{"shutdown":{}}"#).unwrap();
        assert_eq!(shutdown, Request::Shutdown { force: false });

        let forced: Request = serde_json::from_str(r#"{"shutdown":{"force":true}}"#).unwrap();
        assert_eq!(forced, Request::Shutdown { force: true });
    }

    #[test]
    fn reply_done_and_error_payloads() {
        let done = Reply::Done(Status {
            pid: 1234,
            version: "0.1.0".to_string(),
        });
        let error: Reply<Status> = Reply::Error {
            message: "boom".to_string(),
        };

        assert_eq!(
            serde_json::to_string(&done).unwrap(),
            r#"{"done":{"pid":1234,"version":"0.1.0"}}"#
        );
        assert_eq!(
            serde_json::to_string(&error).unwrap(),
            r#"{"error":{"message":"boom"}}"#
        );
    }

    #[test]
    fn machine_requests_serialize_to_the_documented_lines() {
        let machine_status = Request::MachineStatus {
            name: "x".to_string(),
        };

        assert_eq!(
            serde_json::to_string(&machine_status).unwrap(),
            r#"{"machine_status":{"name":"x"}}"#
        );

        let machine_start = Request::MachineStart {
            name: "x".to_string(),
            config: machine::Config {
                image: Image::parse("ghcr.io/org/name:latest").unwrap(),
                mounts: Vec::new(),
                resources: Resources::default(),
            },
        };
        let line = serde_json::to_string(&machine_start).unwrap();

        assert!(line.starts_with(r#"{"machine_start":{"name":"x","config":{"#));
        assert_eq!(
            serde_json::from_str::<Request>(&line).unwrap(),
            machine_start
        );

        let machine_stop = Request::MachineStop {
            name: "x".to_string(),
        };
        let line = serde_json::to_string(&machine_stop).unwrap();

        assert_eq!(line, r#"{"machine_stop":{"name":"x"}}"#);
        assert_eq!(
            serde_json::from_str::<Request>(&line).unwrap(),
            machine_stop
        );

        let machine_list = Request::MachineList {};
        let line = serde_json::to_string(&machine_list).unwrap();

        assert_eq!(line, r#"{"machine_list":{}}"#);
        assert_eq!(
            serde_json::from_str::<Request>(&line).unwrap(),
            machine_list
        );

        let machine_resolve = Request::MachineResolve {
            name: "x".to_string(),
        };
        let line = serde_json::to_string(&machine_resolve).unwrap();

        assert_eq!(line, r#"{"machine_resolve":{"name":"x"}}"#);
        assert_eq!(
            serde_json::from_str::<Request>(&line).unwrap(),
            machine_resolve
        );
    }

    #[test]
    fn machine_stop_outcomes_serialize_as_words() {
        let stopped = Reply::Done(MachineStop::Stopped);
        assert_eq!(
            serde_json::to_string(&stopped).unwrap(),
            r#"{"done":"stopped"}"#
        );

        let not_running = Reply::Done(MachineStop::NotRunning);
        assert_eq!(
            serde_json::to_string(&not_running).unwrap(),
            r#"{"done":"not_running"}"#
        );
    }

    #[test]
    fn machine_start_outcomes_serialize_to_the_documented_shapes() {
        let status = machine::Status {
            name: "x".to_string(),
            state: Running,
            pid: Some(4242),
        };

        let done = Reply::Done(MachineStart::Started(status.clone()));
        assert_eq!(
            serde_json::to_string(&done).unwrap(),
            r#"{"done":{"started":{"name":"x","state":"running","pid":4242}}}"#
        );

        let done = Reply::Done(MachineStart::AlreadyRunning(status));
        assert_eq!(
            serde_json::to_string(&done).unwrap(),
            r#"{"done":{"already_running":{"name":"x","state":"running","pid":4242}}}"#
        );
    }

    #[test]
    fn machine_status_reply_carries_the_record() {
        let status = machine::Status {
            name: "x".to_string(),
            state: Running,
            pid: Some(4242),
        };

        let done = Reply::Done(MachineStatus {
            status: status.clone(),
            record: None,
        });
        assert_eq!(
            serde_json::to_string(&done).unwrap(),
            r#"{"done":{"status":{"name":"x","state":"running","pid":4242},"record":null}}"#
        );

        let done = Reply::Done(MachineStatus {
            status,
            record: Some(machine::Record {
                digest: Digest::parse(&format!("sha256:{}", "a".repeat(64))).unwrap(),
                config: machine::Config {
                    image: Image::parse("ghcr.io/org/name:latest").unwrap(),
                    mounts: Vec::new(),
                    resources: Resources::default(),
                },
            }),
        });
        let line = serde_json::to_string(&done).unwrap();

        assert!(line.contains(r#""record":{"digest":"#), "{line}");
    }
}
