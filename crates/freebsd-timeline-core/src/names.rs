//! Strict validation for every name that ends up on a command line.
//!
//! freebsd-timeline never goes through a shell, but names still become arguments to a
//! root-run `zfs`, so anything starting with `-` (option injection) or
//! containing characters ZFS itself would reject is refused up front.

use crate::error::{Error, ErrorKind, Result};
use crate::time::Utc;

/// Every snapshot freebsd-timeline creates starts with this. Snapshots without it are
/// never touched by pruning.
pub const PREFIX: &str = "freebsd-timeline-";
/// Label used by the scheduler. Only `freebsd-timeline-auto-*` snapshots are pruned.
pub const AUTO_LABEL: &str = "auto";

const MAX_NAME: usize = 255;

fn invalid(what: &str, name: &str, why: &str) -> Error {
    Error::new(ErrorKind::InvalidName, format!("invalid {what} {name:?}: {why}"))
}

fn component_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ':' | '.')
}

fn check_component(what: &str, full: &str, comp: &str) -> Result<()> {
    if comp.is_empty() {
        return Err(invalid(what, full, "empty component"));
    }
    if comp == "." || comp == ".." {
        return Err(invalid(what, full, "'.' and '..' are not allowed"));
    }
    if let Some(c) = comp.chars().find(|c| !component_char(*c)) {
        return Err(invalid(what, full, &format!("character {c:?} is not allowed")));
    }
    Ok(())
}

pub fn validate_dataset(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > MAX_NAME {
        return Err(invalid("dataset", name, "length must be 1-255"));
    }
    if name.starts_with('-') {
        return Err(invalid("dataset", name, "must not start with '-'"));
    }
    for comp in name.split('/') {
        check_component("dataset", name, comp)?;
    }
    Ok(())
}

/// The part after `@`.
pub fn validate_snapshot_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > MAX_NAME {
        return Err(invalid("snapshot", name, "length must be 1-255"));
    }
    if name.starts_with('-') {
        return Err(invalid("snapshot", name, "must not start with '-'"));
    }
    check_component("snapshot", name, name)
}

/// `pool/dataset@name` -> (`pool/dataset`, `name`)
pub fn split_snapshot(full: &str) -> Result<(&str, &str)> {
    let (ds, snap) = full
        .split_once('@')
        .ok_or_else(|| invalid("snapshot", full, "expected dataset@name"))?;
    validate_dataset(ds)?;
    validate_snapshot_name(snap)?;
    Ok((ds, snap))
}

pub fn validate_label(label: &str) -> Result<()> {
    if label.is_empty() || label.len() > 32 {
        return Err(invalid("label", label, "length must be 1-32"));
    }
    if label.starts_with('-') {
        return Err(invalid("label", label, "must not start with '-'"));
    }
    if let Some(c) = label
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-')))
    {
        return Err(invalid("label", label, &format!("character {c:?} is not allowed")));
    }
    Ok(())
}

pub fn validate_boot_env(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 128 {
        return Err(invalid("boot environment", name, "length must be 1-128"));
    }
    if name.starts_with('-') {
        return Err(invalid("boot environment", name, "must not start with '-'"));
    }
    if let Some(c) = name
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')))
    {
        return Err(invalid("boot environment", name, &format!("character {c:?} is not allowed")));
    }
    Ok(())
}

/// `host` or `user@host`. Passed to ssh after `--`, but validated anyway.
pub fn validate_ssh_target(target: &str) -> Result<()> {
    let ok_part = |s: &str| {
        !s.is_empty()
            && !s.starts_with('-')
            && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    let valid = match target.split_once('@') {
        Some((user, host)) => ok_part(user) && ok_part(host),
        None => ok_part(target),
    };
    if valid {
        Ok(())
    } else {
        Err(invalid("ssh target", target, "expected host or user@host"))
    }
}

/// `freebsd-timeline-<label>-<YYYYMMDD-HHMMSS>`
pub fn managed_name(label: &str, t: i64) -> String {
    format!("{PREFIX}{label}-{}", Utc::from_epoch(t).compact())
}

pub fn is_managed(snapshot_name: &str) -> bool {
    snapshot_name.starts_with(PREFIX)
}

pub fn is_auto(snapshot_name: &str) -> bool {
    snapshot_name.starts_with(&format!("{PREFIX}{AUTO_LABEL}-"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datasets() {
        assert!(validate_dataset("zroot/home").is_ok());
        assert!(validate_dataset("zroot/ROOT/default").is_ok());
        assert!(validate_dataset("-o").is_err());
        assert!(validate_dataset("zroot//home").is_err());
        assert!(validate_dataset("zroot/home; rm -rf /").is_err());
        assert!(validate_dataset("zroot/../etc").is_err());
        assert!(validate_dataset("").is_err());
    }

    #[test]
    fn snapshots() {
        assert_eq!(split_snapshot("zroot/home@x").unwrap(), ("zroot/home", "x"));
        assert!(split_snapshot("zroot/home").is_err());
        assert!(split_snapshot("zroot/home@-R").is_err());
        assert!(split_snapshot("zroot/home@a@b").is_err());
    }

    #[test]
    fn targets() {
        assert!(validate_ssh_target("backuphost").is_ok());
        assert!(validate_ssh_target("backup@backuphost.lan").is_ok());
        assert!(validate_ssh_target("-oProxyCommand=x").is_err());
        assert!(validate_ssh_target("a@").is_err());
        assert!(validate_ssh_target("a b").is_err());
    }

    #[test]
    fn managed() {
        let n = managed_name("auto", 0);
        assert_eq!(n, "freebsd-timeline-auto-19700101-000000");
        assert!(is_managed(&n) && is_auto(&n));
        assert!(!is_auto("freebsd-timeline-manual-19700101-000000"));
        assert!(!is_managed("zfs-auto-snap_hourly"));
    }
}
