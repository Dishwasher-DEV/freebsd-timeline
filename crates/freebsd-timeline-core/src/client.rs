//! Client side of the freebsd-timelined socket, shared by the CLI and GUI.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use serde::de::DeserializeOwned;

use crate::error::{Error, ErrorKind, Result};
use crate::protocol::{Request, Response};

pub const DEFAULT_SOCKET: &str = "/var/run/freebsd-timelined.sock";

pub fn default_socket() -> PathBuf {
    std::env::var_os("FREEBSD_TIMELINE_SOCKET").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(DEFAULT_SOCKET))
}

#[derive(Debug, Clone)]
pub struct Client {
    pub socket: PathBuf,
}

impl Default for Client {
    fn default() -> Self {
        Client { socket: default_socket() }
    }
}

impl Client {
    pub fn new(socket: PathBuf) -> Self {
        Client { socket }
    }

    pub fn call_raw(&self, req: &Request) -> Result<Response> {
        let mut stream = UnixStream::connect(&self.socket).map_err(|e| {
            Error::new(
                ErrorKind::Io,
                format!(
                    "cannot reach freebsd-timelined at {} ({e}). Is it running? (service freebsd-timelined start)",
                    self.socket.display()
                ),
            )
        })?;
        // Backups and pool scans can take a while.
        stream.set_read_timeout(Some(Duration::from_secs(3600)))?;
        let mut line = serde_json::to_string(req).map_err(|e| Error::new(ErrorKind::Protocol, e.to_string()))?;
        line.push('\n');
        stream.write_all(line.as_bytes())?;
        stream.flush()?;

        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply)?;
        serde_json::from_str(&reply).map_err(|e| Error::new(ErrorKind::Protocol, format!("bad reply from freebsd-timelined: {e}")))
    }

    pub fn call<T: DeserializeOwned>(&self, req: &Request) -> Result<T> {
        let resp = self.call_raw(req)?;
        if !resp.ok {
            return Err(Error::new(ErrorKind::Command, resp.error.unwrap_or_else(|| "unknown error".into())));
        }
        serde_json::from_value(resp.data).map_err(|e| Error::new(ErrorKind::Protocol, format!("bad reply from freebsd-timelined: {e}")))
    }
}
