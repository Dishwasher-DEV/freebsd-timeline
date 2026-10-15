//! The `Backend` trait is everything freebsd-timeline does to the system. `ZfsBackend`
//! runs the real tools; `mock::MockBackend` fakes them for development.
//!
//! Commands are always spawned directly (never through a shell) and every
//! name is validated before it gets here.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::error::{Error, ErrorKind, Result};
use crate::model::{BootEnv, Dataset, DiffEntry, PoolHealth, Snapshot};
use crate::{names, parse};

pub trait Backend: Send + Sync {
    fn name(&self) -> &'static str;

    fn list_snapshots(&self, dataset: Option<&str>) -> Result<Vec<Snapshot>>;
    fn create_snapshot(&self, dataset: &str, name: &str, recursive: bool) -> Result<()>;
    fn destroy_snapshot(&self, full: &str, recursive: bool) -> Result<()>;
    /// Without `destroy_newer`, ZFS only allows rolling back to the newest snapshot.
    fn rollback(&self, full: &str, destroy_newer: bool) -> Result<()>;
    fn diff(&self, from: &str, to: Option<&str>) -> Result<Vec<DiffEntry>>;

    fn list_datasets(&self) -> Result<Vec<Dataset>>;
    fn unmount(&self, dataset: &str) -> Result<()>;
    fn unload_key(&self, dataset: &str) -> Result<()>;
    fn load_key(&self, dataset: &str, passphrase: &str) -> Result<()>;
    fn mount_all(&self) -> Result<()>;

    fn boot_envs_supported(&self) -> bool;
    fn list_boot_envs(&self) -> Result<Vec<BootEnv>>;
    fn create_boot_env(&self, name: &str) -> Result<()>;
    fn activate_boot_env(&self, name: &str, temporary: bool) -> Result<()>;
    fn destroy_boot_env(&self, name: &str) -> Result<()>;

    fn pool_health(&self) -> Result<Vec<PoolHealth>>;

    /// Snapshot names (the part after `@`) that exist on the backup target.
    fn remote_snapshots(&self, target: &str, dest: &str) -> Result<Vec<String>>;
    /// Raw (`-w`) send, so encrypted datasets stay encrypted on the target.
    fn send_snapshot(&self, full: &str, base: Option<&str>, target: &str, dest: &str) -> Result<()>;
}

pub struct ZfsBackend;

fn command_error(prog: &str, args: &[&str], stderr: &[u8]) -> Error {
    Error::new(
        ErrorKind::Command,
        format!("{prog} {}: {}", args.join(" "), String::from_utf8_lossy(stderr).trim()),
    )
}

fn run(prog: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| Error::new(ErrorKind::Command, format!("failed to run {prog}: {e}")))?;
    if !out.status.success() {
        return Err(command_error(prog, args, &out.stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn run_with_stdin(prog: &str, args: &[&str], input: &str) -> Result<()> {
    let mut child = Command::new(prog)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::new(ErrorKind::Command, format!("failed to run {prog}: {e}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input.as_bytes())?;
        stdin.write_all(b"\n")?;
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(command_error(prog, args, &out.stderr));
    }
    Ok(())
}

const SSH_OPTS: [&str; 4] = ["-o", "BatchMode=yes", "-o", "ConnectTimeout=15"];

impl Backend for ZfsBackend {
    fn name(&self) -> &'static str {
        "zfs"
    }

    fn list_snapshots(&self, dataset: Option<&str>) -> Result<Vec<Snapshot>> {
        let mut args = vec!["list", "-H", "-p", "-t", "snapshot", "-o", "name,creation,used,referenced"];
        if let Some(ds) = dataset {
            names::validate_dataset(ds)?;
            args.extend(["-r", ds]);
        }
        let mut snaps = parse::snapshots(&run("zfs", &args)?)?;
        snaps.sort_by_key(|s| s.created);
        Ok(snaps)
    }

    fn create_snapshot(&self, dataset: &str, name: &str, recursive: bool) -> Result<()> {
        names::validate_dataset(dataset)?;
        names::validate_snapshot_name(name)?;
        let full = format!("{dataset}@{name}");
        let mut args = vec!["snapshot"];
        if recursive {
            args.push("-r");
        }
        args.push(&full);
        run("zfs", &args).map(drop)
    }

    fn destroy_snapshot(&self, full: &str, recursive: bool) -> Result<()> {
        names::split_snapshot(full)?;
        let mut args = vec!["destroy"];
        if recursive {
            args.push("-r");
        }
        args.push(full);
        run("zfs", &args).map(drop)
    }

    fn rollback(&self, full: &str, destroy_newer: bool) -> Result<()> {
        names::split_snapshot(full)?;
        let mut args = vec!["rollback"];
        if destroy_newer {
            args.push("-r");
        }
        args.push(full);
        run("zfs", &args).map(drop)
    }

    fn diff(&self, from: &str, to: Option<&str>) -> Result<Vec<DiffEntry>> {
        names::split_snapshot(from)?;
        let mut args = vec!["diff", "-H", from];
        if let Some(to) = to {
            // Either another snapshot or the live dataset.
            if to.contains('@') {
                names::split_snapshot(to)?;
            } else {
                names::validate_dataset(to)?;
            }
            args.push(to);
        }
        parse::diff(&run("zfs", &args)?)
    }

    fn list_datasets(&self) -> Result<Vec<Dataset>> {
        parse::datasets(&run(
            "zfs",
            &[
                "list", "-H", "-p", "-t", "filesystem", "-o",
                "name,used,avail,mountpoint,encryption,encryptionroot,keystatus,mounted",
            ],
        )?)
    }

    fn unmount(&self, dataset: &str) -> Result<()> {
        names::validate_dataset(dataset)?;
        run("zfs", &["unmount", dataset]).map(drop)
    }

    fn unload_key(&self, dataset: &str) -> Result<()> {
        names::validate_dataset(dataset)?;
        run("zfs", &["unload-key", dataset]).map(drop)
    }

    fn load_key(&self, dataset: &str, passphrase: &str) -> Result<()> {
        names::validate_dataset(dataset)?;
        run_with_stdin("zfs", &["load-key", dataset], passphrase)
    }

    fn mount_all(&self) -> Result<()> {
        run("zfs", &["mount", "-a"]).map(drop)
    }

    fn boot_envs_supported(&self) -> bool {
        Path::new("/sbin/bectl").exists()
    }

    fn list_boot_envs(&self) -> Result<Vec<BootEnv>> {
        if !self.boot_envs_supported() {
            return Err(Error::new(ErrorKind::Unsupported, "boot environments need bectl (FreeBSD)"));
        }
        parse::boot_envs(&run("bectl", &["list", "-H"])?)
    }

    fn create_boot_env(&self, name: &str) -> Result<()> {
        names::validate_boot_env(name)?;
        run("bectl", &["create", name]).map(drop)
    }

    fn activate_boot_env(&self, name: &str, temporary: bool) -> Result<()> {
        names::validate_boot_env(name)?;
        let mut args = vec!["activate"];
        if temporary {
            args.push("-t");
        }
        args.push(name);
        run("bectl", &args).map(drop)
    }

    fn destroy_boot_env(&self, name: &str) -> Result<()> {
        names::validate_boot_env(name)?;
        run("bectl", &["destroy", name]).map(drop)
    }

    fn pool_health(&self) -> Result<Vec<PoolHealth>> {
        let mut pools = parse::pools(&run("zpool", &["list", "-H", "-p", "-o", "name,size,alloc,free,health"])?)?;
        for pool in &mut pools {
            if let Ok(out) = run("zpool", &["status", &pool.name]) {
                let (scan, errors) = parse::pool_status(&out);
                pool.scan = scan;
                pool.errors = errors;
            }
        }
        Ok(pools)
    }

    fn remote_snapshots(&self, target: &str, dest: &str) -> Result<Vec<String>> {
        names::validate_ssh_target(target)?;
        names::validate_dataset(dest)?;
        let mut args: Vec<&str> = SSH_OPTS.to_vec();
        args.extend(["--", target, "zfs", "list", "-H", "-o", "name", "-t", "snapshot", "-d", "1", dest]);
        match run("ssh", &args) {
            Ok(out) => Ok(out
                .lines()
                .filter_map(|l| l.split_once('@').map(|(_, s)| s.to_string()))
                .collect()),
            // First backup: the destination doesn't exist yet.
            Err(e) if e.message.contains("does not exist") => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    fn send_snapshot(&self, full: &str, base: Option<&str>, target: &str, dest: &str) -> Result<()> {
        names::split_snapshot(full)?;
        names::validate_ssh_target(target)?;
        names::validate_dataset(dest)?;
        let mut send_args = vec!["send", "-w"];
        if let Some(base) = base {
            names::split_snapshot(base)?;
            send_args.extend(["-i", base]);
        }
        send_args.push(full);

        let mut send = Command::new("zfs")
            .args(&send_args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error::new(ErrorKind::Command, format!("failed to run zfs send: {e}")))?;
        let stream = send.stdout.take().expect("piped stdout");

        let mut recv_args: Vec<&str> = SSH_OPTS.to_vec();
        recv_args.extend(["--", target, "zfs", "recv", "-u", dest]);
        let recv = Command::new("ssh")
            .args(&recv_args)
            .stdin(Stdio::from(stream))
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error::new(ErrorKind::Command, format!("failed to run ssh: {e}")))?;

        let recv_out = recv.wait_with_output()?;
        let send_out = send.wait_with_output()?;
        if !send_out.status.success() {
            return Err(command_error("zfs", &send_args, &send_out.stderr));
        }
        if !recv_out.status.success() {
            return Err(command_error("ssh", &recv_args, &recv_out.stderr));
        }
        Ok(())
    }
}
