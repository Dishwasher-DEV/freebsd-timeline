//! freebsd-timeline core: everything that talks to ZFS, decides what to keep, and
//! speaks the daemon protocol. The daemon, CLI and GUI are thin layers on top.

pub mod backend;
pub mod backup;
#[cfg(unix)]
pub mod client;
pub mod config;
pub mod error;
pub mod mock;
pub mod model;
pub mod names;
pub mod parse;
pub mod protocol;
pub mod retention;
pub mod service;
pub mod time;
pub mod util;

pub use error::{Error, ErrorKind, Result};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
