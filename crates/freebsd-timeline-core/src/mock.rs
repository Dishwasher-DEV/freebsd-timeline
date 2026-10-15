//! An in-memory fake of a small FreeBSD laptop, so the daemon, CLI and GUI
//! can be developed anywhere (`FREEBSD_TIMELINE_BACKEND=mock`). It enforces the same
//! rules the real tools do, e.g. rollback past newer snapshots needs -r.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::backend::Backend;
use crate::error::{Error, ErrorKind, Result};
use crate::model::{BootEnv, Change, Dataset, DiffEntry, PoolHealth, Snapshot};
use crate::{names, time};

struct State {
    datasets: Vec<Dataset>,
    snapshots: Vec<Snapshot>,
    boot_envs: Vec<BootEnv>,
    remote: HashMap<String, Vec<String>>,
}

pub struct MockBackend {
    state: Mutex<State>,
}

fn ds(name: &str, mount: Option<&str>, used: u64, enc_root: Option<&str>) -> Dataset {
    Dataset {
        name: name.into(),
        used,
        avail: 180 << 30,
        mountpoint: mount.map(Into::into),
        encrypted: enc_root.is_some(),
        encryption_root: enc_root.map(Into::into),
        key_loaded: true,
        mounted: mount.is_some(),
    }
}

fn not_found(what: &str) -> Error {
    Error::new(ErrorKind::NotFound, format!("{what} does not exist"))
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MockBackend {
    pub fn new() -> Self {
        let now = time::now();
        let datasets = vec![
            ds("zroot", None, 60 << 30, None),
            ds("zroot/ROOT", None, 20 << 30, None),
            ds("zroot/ROOT/default", Some("/"), 18 << 30, None),
            ds("zroot/home", Some("/home"), 34 << 30, Some("zroot/home")),
            ds(
                "zroot/home/tom",
                Some("/home/tom"),
                33 << 30,
                Some("zroot/home"),
            ),
            ds("zroot/var/log", Some("/var/log"), 200 << 20, None),
        ];

        let mut snapshots = Vec::new();
        let mut add = |dataset: &str, label: &str, t: i64, used: u64| {
            snapshots.push(Snapshot {
                dataset: dataset.into(),
                name: names::managed_name(label, t),
                created: t,
                used,
                referenced: 30 << 30,
            });
        };
        // Three days of 6-hourly auto snapshots, a few manual ones.
        for i in (1..=12).rev() {
            let t = now - i * 6 * 3600;
            add("zroot/home", "auto", t, (i as u64 % 5 + 1) * (40 << 20));
            add("zroot/home/tom", "auto", t, (i as u64 % 3 + 1) * (90 << 20));
        }
        add(
            "zroot/ROOT/default",
            "pre-upgrade",
            now - 5 * 86400,
            1200 << 20,
        );
        add("zroot/home/tom", "manual", now - 2 * 86400 - 1800, 15 << 20);
        add("zroot/ROOT/default", "manual", now - 3600, 2 << 20);
        snapshots.sort_by_key(|s| s.created);

        let boot_envs = vec![
            BootEnv {
                name: "default".into(),
                active_now: true,
                active_on_reboot: true,
                mountpoint: Some("/".into()),
                space: "18.2G".into(),
                created: time::Utc::from_epoch(now - 40 * 86400).human(),
            },
            BootEnv {
                name: "15.0-RELEASE-p2".into(),
                active_now: false,
                active_on_reboot: false,
                mountpoint: None,
                space: "1.1G".into(),
                created: time::Utc::from_epoch(now - 5 * 86400).human(),
            },
        ];

        MockBackend {
            state: Mutex::new(State {
                datasets,
                snapshots,
                boot_envs,
                remote: HashMap::new(),
            }),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Backend for MockBackend {
    fn name(&self) -> &'static str {
        "mock"
    }

    fn list_snapshots(&self, dataset: Option<&str>) -> Result<Vec<Snapshot>> {
        let st = self.state();
        if let Some(d) = dataset {
            names::validate_dataset(d)?;
            if !st.datasets.iter().any(|x| x.name == d) {
                return Err(not_found(d));
            }
        }
        Ok(st
            .snapshots
            .iter()
            .filter(|s| {
                dataset.map_or(true, |d| {
                    s.dataset == d || s.dataset.starts_with(&format!("{d}/"))
                })
            })
            .cloned()
            .collect())
    }

    fn create_snapshot(&self, dataset: &str, name: &str, recursive: bool) -> Result<()> {
        names::validate_dataset(dataset)?;
        names::validate_snapshot_name(name)?;
        let mut st = self.state();
        let targets: Vec<String> = st
            .datasets
            .iter()
            .filter(|d| {
                d.name == dataset || (recursive && d.name.starts_with(&format!("{dataset}/")))
            })
            .map(|d| d.name.clone())
            .collect();
        if targets.is_empty() {
            return Err(not_found(dataset));
        }
        if st
            .snapshots
            .iter()
            .any(|s| targets.contains(&s.dataset) && s.name == name)
        {
            return Err(Error::new(
                ErrorKind::Command,
                format!("{dataset}@{name}: dataset already exists"),
            ));
        }
        let now = time::now();
        for t in targets {
            st.snapshots.push(Snapshot {
                dataset: t,
                name: name.into(),
                created: now,
                used: 0,
                referenced: 30 << 30,
            });
        }
        Ok(())
    }

    fn destroy_snapshot(&self, full: &str, recursive: bool) -> Result<()> {
        let (dataset, name) = names::split_snapshot(full)?;
        let mut st = self.state();
        let before = st.snapshots.len();
        st.snapshots.retain(|s| {
            let matches_ds = s.dataset == dataset
                || (recursive && s.dataset.starts_with(&format!("{dataset}/")));
            !(matches_ds && s.name == name)
        });
        if st.snapshots.len() == before {
            return Err(not_found(full));
        }
        Ok(())
    }

    fn rollback(&self, full: &str, destroy_newer: bool) -> Result<()> {
        let (dataset, name) = names::split_snapshot(full)?;
        let mut st = self.state();
        let created = st
            .snapshots
            .iter()
            .find(|s| s.dataset == dataset && s.name == name)
            .map(|s| s.created)
            .ok_or_else(|| not_found(full))?;
        let newer = st
            .snapshots
            .iter()
            .filter(|s| s.dataset == dataset && s.created > created)
            .count();
        if newer > 0 && !destroy_newer {
            return Err(Error::new(
                ErrorKind::Refused,
                format!("cannot rollback to {full}: {newer} more recent snapshot(s) exist (use destroy_newer)"),
            ));
        }
        st.snapshots
            .retain(|s| !(s.dataset == dataset && s.created > created));
        Ok(())
    }

    fn diff(&self, from: &str, _to: Option<&str>) -> Result<Vec<DiffEntry>> {
        let (dataset, _) = names::split_snapshot(from)?;
        let st = self.state();
        let mount = st
            .datasets
            .iter()
            .find(|d| d.name == dataset)
            .and_then(|d| d.mountpoint.clone())
            .unwrap_or_else(|| format!("/{dataset}"));
        let p = |rel: &str| format!("{mount}/{rel}");
        Ok(vec![
            DiffEntry {
                change: Change::Modified,
                path: p(".config/hypr/hyprland.conf"),
                new_path: None,
            },
            DiffEntry {
                change: Change::Added,
                path: p("Pictures/rice-v3.png"),
                new_path: None,
            },
            DiffEntry {
                change: Change::Removed,
                path: p("notes/old-todo.md"),
                new_path: None,
            },
            DiffEntry {
                change: Change::Renamed,
                path: p("projects/freebsd-timeline-draft"),
                new_path: Some(p("projects/freebsd-timeline")),
            },
        ])
    }

    fn list_datasets(&self) -> Result<Vec<Dataset>> {
        Ok(self.state().datasets.clone())
    }

    fn unmount(&self, dataset: &str) -> Result<()> {
        let mut st = self.state();
        let d = st
            .datasets
            .iter_mut()
            .find(|d| d.name == dataset)
            .ok_or_else(|| not_found(dataset))?;
        d.mounted = false;
        Ok(())
    }

    fn unload_key(&self, dataset: &str) -> Result<()> {
        let mut st = self.state();
        if st
            .datasets
            .iter()
            .any(|d| d.encryption_root.as_deref() == Some(dataset) && d.mounted)
        {
            return Err(Error::new(
                ErrorKind::Command,
                format!("{dataset}: datasets are still mounted"),
            ));
        }
        for d in st
            .datasets
            .iter_mut()
            .filter(|d| d.encryption_root.as_deref() == Some(dataset))
        {
            d.key_loaded = false;
        }
        Ok(())
    }

    fn load_key(&self, dataset: &str, passphrase: &str) -> Result<()> {
        if passphrase != "freebsd-timeline" {
            return Err(Error::new(
                ErrorKind::Command,
                "incorrect key provided (mock passphrase is \"freebsd-timeline\")",
            ));
        }
        let mut st = self.state();
        for d in st
            .datasets
            .iter_mut()
            .filter(|d| d.encryption_root.as_deref() == Some(dataset))
        {
            d.key_loaded = true;
        }
        Ok(())
    }

    fn mount_all(&self) -> Result<()> {
        let mut st = self.state();
        for d in st.datasets.iter_mut() {
            if d.mountpoint.is_some() && (!d.encrypted || d.key_loaded) {
                d.mounted = true;
            }
        }
        Ok(())
    }

    fn boot_envs_supported(&self) -> bool {
        true
    }

    fn list_boot_envs(&self) -> Result<Vec<BootEnv>> {
        Ok(self.state().boot_envs.clone())
    }

    fn create_boot_env(&self, name: &str) -> Result<()> {
        names::validate_boot_env(name)?;
        let mut st = self.state();
        if st.boot_envs.iter().any(|b| b.name == name) {
            return Err(Error::new(
                ErrorKind::Command,
                format!("boot environment {name} already exists"),
            ));
        }
        st.boot_envs.push(BootEnv {
            name: name.into(),
            active_now: false,
            active_on_reboot: false,
            mountpoint: None,
            space: "8K".into(),
            created: time::Utc::from_epoch(time::now()).human(),
        });
        Ok(())
    }

    fn activate_boot_env(&self, name: &str, _temporary: bool) -> Result<()> {
        let mut st = self.state();
        if !st.boot_envs.iter().any(|b| b.name == name) {
            return Err(not_found(name));
        }
        for b in st.boot_envs.iter_mut() {
            b.active_on_reboot = b.name == name;
        }
        Ok(())
    }

    fn destroy_boot_env(&self, name: &str) -> Result<()> {
        let mut st = self.state();
        let be = st
            .boot_envs
            .iter()
            .find(|b| b.name == name)
            .ok_or_else(|| not_found(name))?;
        if be.active_now || be.active_on_reboot {
            return Err(Error::new(
                ErrorKind::Refused,
                format!("{name} is active and cannot be destroyed"),
            ));
        }
        st.boot_envs.retain(|b| b.name != name);
        Ok(())
    }

    fn pool_health(&self) -> Result<Vec<PoolHealth>> {
        Ok(vec![PoolHealth {
            name: "zroot".into(),
            health: "ONLINE".into(),
            size: 238 << 30,
            alloc: 61 << 30,
            free: 177 << 30,
            scan: Some("scrub repaired 0B in 00:03:12 with 0 errors".into()),
            errors: Some("No known data errors".into()),
        }])
    }

    fn remote_snapshots(&self, target: &str, dest: &str) -> Result<Vec<String>> {
        names::validate_ssh_target(target)?;
        Ok(self.state().remote.get(dest).cloned().unwrap_or_default())
    }

    fn send_snapshot(
        &self,
        full: &str,
        base: Option<&str>,
        target: &str,
        dest: &str,
    ) -> Result<()> {
        names::validate_ssh_target(target)?;
        let (_, name) = names::split_snapshot(full)?;
        let mut st = self.state();
        let remote = st.remote.entry(dest.to_string()).or_default();
        if let Some(base) = base {
            let (_, base_name) = names::split_snapshot(base)?;
            if !remote.iter().any(|r| r == base_name) {
                return Err(Error::new(
                    ErrorKind::Command,
                    "incremental source does not exist on target",
                ));
            }
        }
        remote.push(name.to_string());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rollback_needs_destroy_newer() {
        let m = MockBackend::new();
        let snaps = m.list_snapshots(Some("zroot/home")).unwrap();
        let oldest = snaps
            .iter()
            .find(|s| s.dataset == "zroot/home")
            .unwrap()
            .full_name();
        assert!(m.rollback(&oldest, false).is_err());
        m.rollback(&oldest, true).unwrap();
        let after = m.list_snapshots(Some("zroot/home")).unwrap();
        assert_eq!(
            after.iter().filter(|s| s.dataset == "zroot/home").count(),
            1
        );
    }

    #[test]
    fn recursive_snapshot() {
        let m = MockBackend::new();
        m.create_snapshot("zroot/home", "x", true).unwrap();
        let s = m.list_snapshots(Some("zroot/home")).unwrap();
        assert_eq!(s.iter().filter(|s| s.name == "x").count(), 2);
        m.destroy_snapshot("zroot/home@x", true).unwrap();
        assert!(m.destroy_snapshot("zroot/home@x", true).is_err());
    }
}
