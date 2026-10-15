//! The brain of freebsd-timelined: turns protocol requests into backend calls and runs
//! the snapshot schedule. Lives in the core crate so it can be tested without
//! a socket.

use std::sync::RwLock;

use serde::Serialize;
use serde_json::Value;

use crate::backend::Backend;
use crate::backup;
use crate::config::{parse_duration, Config};
use crate::error::{Error, ErrorKind, Result};
use crate::model::{ActionResult, Status};
use crate::names;
use crate::protocol::{Request, Response};
use crate::retention::{self, PrunePlan};
use crate::time;

pub struct Service {
    backend: Box<dyn Backend>,
    config: RwLock<Config>,
}

fn json(v: impl Serialize) -> Result<Value> {
    serde_json::to_value(v).map_err(|e| Error::new(ErrorKind::Protocol, e.to_string()))
}

fn action(target: &str, r: Result<String>) -> ActionResult {
    match r {
        Ok(message) => ActionResult {
            target: target.into(),
            ok: true,
            message,
        },
        Err(e) => ActionResult {
            target: target.into(),
            ok: false,
            message: e.message,
        },
    }
}

impl Service {
    pub fn new(backend: Box<dyn Backend>, config: Config) -> Self {
        Service {
            backend,
            config: RwLock::new(config),
        }
    }

    pub fn backend(&self) -> &dyn Backend {
        self.backend.as_ref()
    }

    pub fn config(&self) -> Config {
        self.config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn set_config(&self, cfg: Config) {
        *self.config.write().unwrap_or_else(|e| e.into_inner()) = cfg;
    }

    pub fn handle(&self, req: Request) -> Response {
        match self.dispatch(req) {
            Ok(v) => Response {
                ok: true,
                data: v,
                error: None,
            },
            Err(e) => Response::err(e.message),
        }
    }

    fn dispatch(&self, req: Request) -> Result<Value> {
        let b = self.backend();
        let now = time::now();
        match req {
            Request::Status => json(self.status()?),
            Request::ListSnapshots { dataset } => json(b.list_snapshots(dataset.as_deref())?),
            Request::CreateSnapshot {
                dataset,
                label,
                recursive,
            } => {
                let label = label.unwrap_or_else(|| "manual".into());
                names::validate_label(&label)?;
                if label == names::AUTO_LABEL {
                    return Err(Error::new(
                        ErrorKind::Refused,
                        "the label 'auto' is reserved for scheduled snapshots",
                    ));
                }
                let name = names::managed_name(&label, now);
                b.create_snapshot(&dataset, &name, recursive)?;
                json(format!("{dataset}@{name}"))
            }
            Request::DestroySnapshot {
                snapshot,
                recursive,
            } => {
                b.destroy_snapshot(&snapshot, recursive)?;
                json(snapshot)
            }
            Request::Rollback {
                snapshot,
                destroy_newer,
            } => {
                b.rollback(&snapshot, destroy_newer)?;
                json(snapshot)
            }
            Request::Diff { from, to } => json(b.diff(&from, to.as_deref())?),
            Request::ListDatasets => json(b.list_datasets()?),
            Request::ListBootEnvs => json(b.list_boot_envs()?),
            Request::CreateBootEnv { name, label } => {
                let name = match name {
                    Some(n) => n,
                    None => {
                        let label = label.unwrap_or_else(|| "freebsd-timeline".into());
                        names::validate_label(&label)?;
                        format!("{label}-{}", time::Utc::from_epoch(now).compact())
                    }
                };
                b.create_boot_env(&name)?;
                json(name)
            }
            Request::ActivateBootEnv { name, temporary } => {
                b.activate_boot_env(&name, temporary)?;
                json(name)
            }
            Request::DestroyBootEnv { name } => {
                b.destroy_boot_env(&name)?;
                json(name)
            }
            Request::PoolHealth => json(b.pool_health()?),
            Request::PrunePlan => json(self.prune_plan(now)?),
            Request::Prune => json(self.prune(now)?),
            Request::Backup { dataset } => json(self.backup(dataset)?),
            Request::LockAll => json(self.lock_all()?),
            Request::Unlock {
                dataset,
                passphrase,
            } => {
                names::validate_dataset(&dataset)?;
                b.load_key(&dataset, &passphrase)?;
                b.mount_all()?;
                json(dataset)
            }
        }
    }

    fn status(&self) -> Result<Status> {
        let b = self.backend();
        let cfg = self.config();
        let snaps = b.list_snapshots(None)?;
        Ok(Status {
            version: crate::VERSION.into(),
            backend: b.name().into(),
            watched: cfg.dataset.iter().map(|d| d.name.clone()).collect(),
            snapshots: snaps.len(),
            managed: snaps.iter().filter(|s| s.is_managed()).count(),
            last_snapshot: snaps
                .iter()
                .filter(|s| s.is_managed())
                .map(|s| s.created)
                .max(),
            pools: b.pool_health().unwrap_or_default(),
            boot_envs_supported: b.boot_envs_supported(),
            backup_target: cfg
                .backup
                .as_ref()
                .map(|bk| format!("{}:{}", bk.target, bk.dest)),
        })
    }

    pub fn prune_plan(&self, now: i64) -> Result<PrunePlan> {
        let cfg = self.config();
        let mut total = PrunePlan::default();
        for policy in &cfg.dataset {
            let snaps: Vec<_> = self
                .backend()
                .list_snapshots(Some(&policy.name))?
                .into_iter()
                .filter(|s| s.dataset == policy.name)
                .collect();
            let p = retention::plan(&snaps, &policy.keep, now);
            total.keep.extend(p.keep);
            total.destroy.extend(p.destroy);
        }
        Ok(total)
    }

    pub fn prune(&self, now: i64) -> Result<Vec<ActionResult>> {
        let cfg = self.config();
        let plan = self.prune_plan(now)?;
        Ok(plan
            .destroy
            .iter()
            .map(|s| {
                let recursive = cfg.policy(&s.dataset).is_some_and(|p| p.recursive);
                let full = s.full_name();
                action(
                    &full,
                    self.backend()
                        .destroy_snapshot(&full, recursive)
                        .map(|_| "destroyed".into()),
                )
            })
            .collect())
    }

    pub fn backup(&self, dataset: Option<String>) -> Result<Vec<ActionResult>> {
        let cfg = self.config();
        let bk = cfg
            .backup
            .as_ref()
            .ok_or_else(|| Error::new(ErrorKind::Config, "no [backup] section in the config"))?;
        let datasets = match dataset {
            Some(d) => vec![d],
            None => cfg.backup_datasets(),
        };
        Ok(datasets
            .iter()
            .map(|ds| {
                let r = backup::backup_dataset(self.backend(), bk, ds).map(|rep| {
                    if rep.up_to_date {
                        format!("already up to date ({})", rep.snapshot)
                    } else if let Some(base) = rep.base {
                        format!(
                            "sent {} incrementally from {base} to {}",
                            rep.snapshot, rep.destination
                        )
                    } else {
                        format!("sent {} (full) to {}", rep.snapshot, rep.destination)
                    }
                });
                action(ds, r)
            })
            .collect())
    }

    /// Panic lock: unmount and unload the key of every encrypted dataset that
    /// isn't holding up the running system.
    pub fn lock_all(&self) -> Result<Vec<ActionResult>> {
        let b = self.backend();
        let datasets = b.list_datasets()?;
        let roots: Vec<String> = datasets
            .iter()
            .filter(|d| {
                d.encrypted && d.key_loaded && d.encryption_root.as_deref() == Some(d.name.as_str())
            })
            .map(|d| d.name.clone())
            .collect();

        let mut results = Vec::new();
        for root in roots {
            let mut members: Vec<_> = datasets
                .iter()
                .filter(|d| d.encryption_root.as_deref() == Some(root.as_str()))
                .collect();
            if members.iter().any(|d| d.mountpoint.as_deref() == Some("/")) {
                results.push(action(
                    &root,
                    Err(Error::new(
                        ErrorKind::Refused,
                        "holds the root filesystem, skipped",
                    )),
                ));
                continue;
            }
            members.sort_by_key(|d| std::cmp::Reverse(d.name.len())); // children first
            let r = (|| -> Result<String> {
                for d in members.iter().filter(|d| d.mounted) {
                    b.unmount(&d.name)?;
                }
                b.unload_key(&root)?;
                Ok("locked".to_string())
            })();
            results.push(action(&root, r));
        }
        Ok(results)
    }

    /// One scheduler pass: take due snapshots, prune, and back up if enabled.
    /// Returns log lines.
    pub fn tick(&self, now: i64) -> Vec<String> {
        let cfg = self.config();
        let b = self.backend();
        let backup_set = cfg.backup_datasets();
        let mut log = Vec::new();

        for policy in &cfg.dataset {
            let every = match parse_duration(&policy.every) {
                Ok(s) => s,
                Err(e) => {
                    log.push(format!("{}: {e}", policy.name));
                    continue;
                }
            };
            let last = match b.list_snapshots(Some(&policy.name)) {
                Ok(s) => s
                    .iter()
                    .filter(|s| s.dataset == policy.name && s.is_auto())
                    .map(|s| s.created)
                    .max(),
                Err(e) => {
                    log.push(format!("{}: {e}", policy.name));
                    continue;
                }
            };
            if last.map_or(true, |l| now - l >= every) {
                let name = names::managed_name(names::AUTO_LABEL, now);
                match b.create_snapshot(&policy.name, &name, policy.recursive) {
                    Ok(()) => log.push(format!("snapshot {}@{name}", policy.name)),
                    Err(e) => log.push(format!("snapshot {} failed: {e}", policy.name)),
                }
                if let (Some(bk), true) = (&cfg.backup, backup_set.contains(&policy.name)) {
                    if bk.auto {
                        match backup::backup_dataset(b, bk, &policy.name) {
                            Ok(r) => {
                                log.push(format!("backup {} -> {} ok", policy.name, r.destination))
                            }
                            Err(e) => log.push(format!("backup {} failed: {e}", policy.name)),
                        }
                    }
                }
            }
        }

        match self.prune(now) {
            Ok(results) => {
                for r in results {
                    log.push(format!("prune {}: {}", r.target, r.message));
                }
            }
            Err(e) => log.push(format!("prune failed: {e}")),
        }
        log
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DatasetPolicy, Keep};
    use crate::mock::MockBackend;
    use crate::model::Snapshot;

    fn service() -> Service {
        let cfg = Config {
            dataset: vec![DatasetPolicy {
                name: "zroot/home".into(),
                recursive: true,
                every: "1h".into(),
                keep: Keep {
                    hourly: 2,
                    daily: 1,
                    weekly: 0,
                    monthly: 0,
                    yearly: 0,
                },
            }],
            backup: None,
        };
        Service::new(Box::new(MockBackend::new()), cfg)
    }

    #[test]
    fn tick_snapshots_and_prunes() {
        let s = service();
        let log = s.tick(time::now());
        assert!(
            log.iter()
                .any(|l| l.starts_with("snapshot zroot/home@freebsd-timeline-auto-")),
            "{log:?}"
        );
        assert!(log.iter().any(|l| l.starts_with("prune ")), "{log:?}");
        // A second tick right away takes no new snapshot.
        let log2 = s.tick(time::now());
        assert!(!log2.iter().any(|l| l.starts_with("snapshot ")), "{log2:?}");
    }

    #[test]
    fn auto_label_reserved() {
        let s = service();
        let r = s.handle(Request::CreateSnapshot {
            dataset: "zroot/home".into(),
            label: Some("auto".into()),
            recursive: false,
        });
        assert!(!r.ok);
        let r = s.handle(Request::CreateSnapshot {
            dataset: "zroot/home".into(),
            label: None,
            recursive: false,
        });
        assert!(r.ok, "{:?}", r.error);
    }

    #[test]
    fn lock_all_skips_root_and_locks_home() {
        let s = service();
        let r = s.lock_all().unwrap();
        assert_eq!(r.len(), 1);
        assert!(r[0].ok, "{:?}", r[0]);
        let ds = s.backend().list_datasets().unwrap();
        assert!(ds
            .iter()
            .filter(|d| d.encrypted)
            .all(|d| !d.key_loaded && !d.mounted));
        let bad = s.handle(Request::Unlock {
            dataset: "zroot/home".into(),
            passphrase: "nope".into(),
        });
        assert!(!bad.ok);
        let good = s.handle(Request::Unlock {
            dataset: "zroot/home".into(),
            passphrase: "freebsd-timeline".into(),
        });
        assert!(good.ok);
    }

    #[test]
    fn status_reports() {
        let s = service();
        let r = s.handle(Request::Status);
        assert!(r.ok);
        let st: Status = serde_json::from_value(r.data).unwrap();
        assert_eq!(st.backend, "mock");
        assert!(st.managed > 0);
        let _: Vec<Snapshot> =
            serde_json::from_value(s.handle(Request::ListSnapshots { dataset: None }).data)
                .unwrap();
    }
}
