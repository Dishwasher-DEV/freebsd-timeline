//! Parsers for the scripted (`-H`, tab-separated) output of zfs, zpool and bectl.

use crate::error::{Error, ErrorKind, Result};
use crate::model::{BootEnv, Change, Dataset, DiffEntry, PoolHealth, Snapshot};

fn parse_err(what: &str, line: &str) -> Error {
    Error::new(
        ErrorKind::Parse,
        format!("unexpected {what} output: {line:?}"),
    )
}

fn lines(out: &str) -> impl Iterator<Item = &str> {
    out.lines().filter(|l| !l.trim().is_empty())
}

fn opt(field: &str) -> Option<String> {
    match field {
        "-" | "none" | "legacy" | "" => None,
        s => Some(s.to_string()),
    }
}

fn num(what: &str, line: &str, field: &str) -> Result<u64> {
    if field == "-" {
        return Ok(0);
    }
    field.parse().map_err(|_| parse_err(what, line))
}

/// `zfs list -H -p -t snapshot -o name,creation,used,referenced`
pub fn snapshots(out: &str) -> Result<Vec<Snapshot>> {
    lines(out)
        .map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() != 4 {
                return Err(parse_err("zfs list", line));
            }
            let (dataset, name) = f[0]
                .split_once('@')
                .ok_or_else(|| parse_err("zfs list", line))?;
            Ok(Snapshot {
                dataset: dataset.to_string(),
                name: name.to_string(),
                created: f[1].parse().map_err(|_| parse_err("zfs list", line))?,
                used: num("zfs list", line, f[2])?,
                referenced: num("zfs list", line, f[3])?,
            })
        })
        .collect()
}

/// `zfs list -H -p -t filesystem -o name,used,avail,mountpoint,encryption,encryptionroot,keystatus,mounted`
pub fn datasets(out: &str) -> Result<Vec<Dataset>> {
    lines(out)
        .map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() != 8 {
                return Err(parse_err("zfs list", line));
            }
            Ok(Dataset {
                name: f[0].to_string(),
                used: num("zfs list", line, f[1])?,
                avail: num("zfs list", line, f[2])?,
                mountpoint: opt(f[3]),
                encrypted: f[4] != "off" && f[4] != "-",
                encryption_root: opt(f[5]),
                key_loaded: f[6] == "available",
                mounted: f[7] == "yes",
            })
        })
        .collect()
}

/// `zpool list -H -p -o name,size,alloc,free,health`
pub fn pools(out: &str) -> Result<Vec<PoolHealth>> {
    lines(out)
        .map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() != 5 {
                return Err(parse_err("zpool list", line));
            }
            Ok(PoolHealth {
                name: f[0].to_string(),
                size: num("zpool list", line, f[1])?,
                alloc: num("zpool list", line, f[2])?,
                free: num("zpool list", line, f[3])?,
                health: f[4].to_string(),
                scan: None,
                errors: None,
            })
        })
        .collect()
}

/// Pulls the `scan:` and `errors:` lines out of `zpool status <pool>`.
pub fn pool_status(out: &str) -> (Option<String>, Option<String>) {
    let field = |key: &str| {
        out.lines()
            .map(str::trim)
            .find_map(|l| l.strip_prefix(key))
            .map(|v| v.trim().to_string())
    };
    (field("scan:"), field("errors:"))
}

/// `bectl list -H`: name, active flags, mountpoint, space, created.
/// Active flags: `N` = active now, `R` = active on reboot, `T` = temporary.
pub fn boot_envs(out: &str) -> Result<Vec<BootEnv>> {
    lines(out)
        .map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() < 5 {
                return Err(parse_err("bectl list", line));
            }
            Ok(BootEnv {
                name: f[0].to_string(),
                active_now: f[1].contains('N'),
                active_on_reboot: f[1].contains('R'),
                mountpoint: opt(f[2]),
                space: f[3].to_string(),
                created: f[4].to_string(),
            })
        })
        .collect()
}

/// `zfs diff -H a [b]`: `M\t/path`, `R\t/old\t/new`, ...
pub fn diff(out: &str) -> Result<Vec<DiffEntry>> {
    lines(out)
        .map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            let change = match f.first().copied() {
                Some("+") => Change::Added,
                Some("-") => Change::Removed,
                Some("M") => Change::Modified,
                Some("R") => Change::Renamed,
                _ => return Err(parse_err("zfs diff", line)),
            };
            let path = f
                .get(1)
                .ok_or_else(|| parse_err("zfs diff", line))?
                .to_string();
            let new_path = if change == Change::Renamed {
                Some(
                    f.get(2)
                        .ok_or_else(|| parse_err("zfs diff", line))?
                        .to_string(),
                )
            } else {
                None
            };
            Ok(DiffEntry {
                change,
                path,
                new_path,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_snapshots() {
        let out = "zroot/home@freebsd-timeline-auto-20261014-120000\t1792000000\t4096\t1048576\n\
                   zroot@base\t1700000000\t0\t-\n";
        let s = snapshots(out).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].dataset, "zroot/home");
        assert!(s[0].is_auto());
        assert_eq!(s[1].referenced, 0);
        assert!(snapshots("garbage").is_err());
    }

    #[test]
    fn parses_datasets() {
        let out = "zroot/home\t100\t200\t/home\taes-256-gcm\tzroot/home\tavailable\tyes\n\
                   zroot\t1\t2\tnone\toff\t-\t-\tno\n";
        let d = datasets(out).unwrap();
        assert!(d[0].encrypted && d[0].key_loaded && d[0].mounted);
        assert_eq!(d[0].encryption_root.as_deref(), Some("zroot/home"));
        assert_eq!(d[1].mountpoint, None);
        assert!(!d[1].encrypted);
    }

    #[test]
    fn parses_status() {
        let out = "  pool: zroot\n state: ONLINE\n  scan: scrub repaired 0B in 00:01:02 with 0 errors on Sun Oct 12 2026\nconfig:\n\nerrors: No known data errors\n";
        let (scan, errors) = pool_status(out);
        assert!(scan.unwrap().starts_with("scrub repaired"));
        assert_eq!(errors.as_deref(), Some("No known data errors"));
    }

    #[test]
    fn parses_bectl() {
        let out = "default\tNR\t/\t2.1G\t2026-09-01 10:00\nold\t-\t-\t300M\t2026-08-01 09:00\n";
        let b = boot_envs(out).unwrap();
        assert!(b[0].active_now && b[0].active_on_reboot);
        assert!(!b[1].active_now);
        assert_eq!(b[1].mountpoint, None);
    }

    #[test]
    fn parses_diff() {
        let out = "M\t/home/u/a.txt\nR\t/home/u/b\t/home/u/c\n+\t/home/u/new\n";
        let d = diff(out).unwrap();
        assert_eq!(d[0].change, Change::Modified);
        assert_eq!(d[1].new_path.as_deref(), Some("/home/u/c"));
        assert_eq!(d[2].change, Change::Added);
    }
}
