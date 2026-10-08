//! Spacecraft daemon API module.

use serde::{Deserialize, Serialize};

/// Request to the daemon.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Request {
    /// Request the status of the daemon.
    Status {},
    /// Request the daemon to shut down.
    Shutdown {},
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_status_and_shutdown_payloads() {
        let status = Request::Status {};
        let shutdown = Request::Shutdown {};

        assert_eq!(serde_json::to_string(&status).unwrap(), r#"{"status":{}}"#);
        assert_eq!(
            serde_json::to_string(&shutdown).unwrap(),
            r#"{"shutdown":{}}"#
        );
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
}
