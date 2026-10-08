use spacecraft::daemon::api::Status;
use spacecraft::daemon::client::Client;
use spacecraft::daemon::{Mode, serve_in};
use spacecraft::{Error, Result};
use std::path::PathBuf;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub fn start_daemon(dir: PathBuf) -> JoinHandle<(Result<()>, Vec<u8>)> {
    thread::spawn(move || {
        let mut out = Vec::new();
        (serve_in(&dir, Mode::Foreground, &mut out), out)
    })
}

pub fn wait_for_status(client: &Client) -> spacecraft::Result<Status> {
    let deadline = Instant::now() + Duration::from_secs(5);

    loop {
        match client.status() {
            Ok(status) => return Ok(status),
            Err(Error::DaemonNotRunning) => {
                if Instant::now() > deadline {
                    return Err(Error::DaemonNotRunning);
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(e),
        }
    }
}
