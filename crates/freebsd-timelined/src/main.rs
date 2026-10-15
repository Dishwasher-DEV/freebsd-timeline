//! freebsd-timelined: the only part of freebsd-timeline that runs as root.
//!
//! It listens on a local Unix socket (never the network), runs the snapshot
//! schedule, and does what clients ask after validating every name. Access is
//! controlled by the socket's permissions: root and members of the `freebsd-timeline`
//! group.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime};

use freebsd_timeline_core::backend::{Backend, ZfsBackend};
use freebsd_timeline_core::client::default_socket;
use freebsd_timeline_core::config::Config;
use freebsd_timeline_core::mock::MockBackend;
use freebsd_timeline_core::protocol::{Request, Response, MAX_REQUEST};
use freebsd_timeline_core::service::Service;
use freebsd_timeline_core::time;

type BoxResult<T> = Result<T, Box<dyn std::error::Error>>;

const GROUP: &str = "freebsd-timeline";

fn log(msg: &str) {
    eprintln!("{} freebsd-timelined: {msg}", time::Utc::from_epoch(time::now()).human());
}

fn usage() {
    println!(
        "usage: freebsd-timelined [--check-config] [--version]

Environment:
  FREEBSD_TIMELINE_CONFIG   config file (default {})
  FREEBSD_TIMELINE_SOCKET   control socket (default /var/run/freebsd-timelined.sock)
  FREEBSD_TIMELINE_BACKEND  'zfs' (default) or 'mock' for development
  FREEBSD_TIMELINE_TICK     scheduler interval in seconds (default 60)",
        Config::default_path()
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let config_path = std::env::var_os("FREEBSD_TIMELINE_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(Config::default_path()));

    match args.first().map(String::as_str) {
        None => {}
        Some("--version" | "-V") => {
            println!("freebsd-timelined {}", freebsd_timeline_core::VERSION);
            return;
        }
        Some("--check-config") => match Config::load(&config_path) {
            Ok(cfg) => {
                println!("{}: ok, {} dataset(s)", config_path.display(), cfg.dataset.len());
                return;
            }
            Err(e) => {
                eprintln!("{}: {e}", config_path.display());
                std::process::exit(1);
            }
        },
        Some(_) => {
            usage();
            std::process::exit(2);
        }
    }

    if let Err(e) = run(&config_path) {
        log(&format!("fatal: {e}"));
        std::process::exit(1);
    }
}

fn run(config_path: &Path) -> BoxResult<()> {
    let cfg = Config::load(config_path)?;
    let backend: Box<dyn Backend> = match std::env::var("FREEBSD_TIMELINE_BACKEND").as_deref() {
        Ok("mock") => Box::new(MockBackend::new()),
        Ok("zfs") | Err(_) => Box::new(ZfsBackend),
        Ok(other) => return Err(format!("unknown FREEBSD_TIMELINE_BACKEND {other:?}").into()),
    };
    let service = Arc::new(Service::new(backend, cfg));

    let socket = default_socket();
    let listener = bind(&socket)?;
    log(&format!(
        "listening on {} (backend {}, config {})",
        socket.display(),
        service.backend().name(),
        config_path.display()
    ));

    let tick = std::env::var("FREEBSD_TIMELINE_TICK").ok().and_then(|v| v.parse().ok()).unwrap_or(60u64).max(5);
    {
        let service = Arc::clone(&service);
        let config_path = config_path.to_path_buf();
        thread::spawn(move || scheduler(&service, &config_path, Duration::from_secs(tick)));
    }

    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
                let service = Arc::clone(&service);
                thread::spawn(move || {
                    if let Err(e) = handle(&service, stream) {
                        log(&format!("client error: {e}"));
                    }
                });
            }
            Err(e) => log(&format!("accept failed: {e}")),
        }
    }
    Ok(())
}

/// Runs the schedule forever and picks up config edits without a restart.
fn scheduler(service: &Service, config_path: &Path, every: Duration) {
    let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let mut seen: Option<SystemTime> = mtime(config_path);
    loop {
        let current = mtime(config_path);
        if current != seen {
            seen = current;
            match Config::load(config_path) {
                Ok(cfg) => {
                    log(&format!("reloaded config ({} dataset(s))", cfg.dataset.len()));
                    service.set_config(cfg);
                }
                Err(e) => log(&format!("config change ignored: {e}")),
            }
        }
        for line in service.tick(time::now()) {
            log(&line);
        }
        thread::sleep(every);
    }
}

fn group_id(name: &str) -> Option<u32> {
    let groups = std::fs::read_to_string("/etc/group").ok()?;
    groups.lines().find_map(|line| {
        let mut f = line.split(':');
        (f.next()? == name).then_some(())?;
        f.next()?;
        f.next()?.parse().ok()
    })
}

fn bind(path: &Path) -> BoxResult<UnixListener> {
    if path.exists() {
        if UnixStream::connect(path).is_ok() {
            return Err(format!("another freebsd-timelined is already listening on {}", path.display()).into());
        }
        std::fs::remove_file(path)?; // stale socket from a crash
    }
    let listener = UnixListener::bind(path)?;

    // Owner-only until we know the freebsd-timeline group can be applied.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    match group_id(GROUP) {
        Some(gid) => match std::os::unix::fs::chown(path, None, Some(gid)) {
            Ok(()) => {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))?;
                log(&format!("socket open to group '{GROUP}'"));
            }
            Err(e) => log(&format!("could not hand socket to group '{GROUP}' ({e}); owner-only access")),
        },
        None => log(&format!("no '{GROUP}' group; socket is owner-only (pw groupadd {GROUP} to share it)")),
    }
    Ok(listener)
}

fn handle(service: &Service, stream: UnixStream) -> BoxResult<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut line = String::new();
    BufReader::new((&stream).take(MAX_REQUEST)).read_line(&mut line)?;
    if line.trim().is_empty() {
        return Ok(());
    }

    let response = match serde_json::from_str::<Request>(&line) {
        Ok(req) => {
            if !req.is_read_only() {
                log(&format!("request: {}", req.describe()));
            }
            service.handle(req)
        }
        Err(e) => Response::err(format!("bad request: {e}")),
    };

    let mut out = serde_json::to_string(&response)?;
    out.push('\n');
    (&stream).write_all(out.as_bytes())?;
    Ok(())
}
