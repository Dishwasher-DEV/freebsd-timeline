//! Incremental, raw ZFS replication to a machine you own, over SSH.
//!
//! Raw sends (`zfs send -w`) ship encrypted datasets *still encrypted*: the
//! backup host stores ciphertext and never sees your key. No cloud, no
//! account, nothing to sign up for.

use serde::{Deserialize, Serialize};

use crate::backend::Backend;
use crate::config::BackupConfig;
use crate::error::{Error, ErrorKind, Result};
use crate::names;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupReport {
    pub dataset: String,
    pub snapshot: String,
    pub destination: String,
    /// `None` = full send (first backup), otherwise the incremental base.
    pub base: Option<String>,
    pub up_to_date: bool,
}

pub fn backup_dataset(
    backend: &dyn Backend,
    cfg: &BackupConfig,
    dataset: &str,
) -> Result<BackupReport> {
    names::validate_dataset(dataset)?;
    let dest = cfg.remote_dest(dataset);

    let mut local: Vec<_> = backend
        .list_snapshots(Some(dataset))?
        .into_iter()
        .filter(|s| s.dataset == dataset && s.is_managed())
        .collect();
    local.sort_by_key(|s| s.created);
    let latest = local.last().cloned().ok_or_else(|| {
        Error::new(
            ErrorKind::NotFound,
            format!("{dataset} has no freebsd-timeline snapshots yet; take one first"),
        )
    })?;

    let remote = backend.remote_snapshots(&cfg.target, &dest)?;
    let mut report = BackupReport {
        dataset: dataset.to_string(),
        snapshot: latest.name.clone(),
        destination: format!("{}:{dest}", cfg.target),
        base: None,
        up_to_date: false,
    };
    if remote.contains(&latest.name) {
        report.up_to_date = true;
        return Ok(report);
    }

    // Newest snapshot both sides have = incremental base.
    let base = local.iter().rev().find(|s| remote.contains(&s.name));
    if base.is_none() && !remote.is_empty() {
        return Err(Error::new(
            ErrorKind::Refused,
            format!("{dest} on {} has snapshots but none in common with {dataset}; refusing to overwrite", cfg.target),
        ));
    }
    let base_full = base.map(|s| s.full_name());
    backend.send_snapshot(
        &latest.full_name(),
        base_full.as_deref(),
        &cfg.target,
        &dest,
    )?;
    report.base = base.map(|s| s.name.clone());
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockBackend;

    #[test]
    fn full_then_incremental_then_noop() {
        let m = MockBackend::new();
        let cfg = BackupConfig {
            target: "backuphost".into(),
            dest: "tank/bk".into(),
            datasets: vec![],
            auto: false,
        };

        let r1 = backup_dataset(&m, &cfg, "zroot/home").unwrap();
        assert!(r1.base.is_none() && !r1.up_to_date);

        let r2 = backup_dataset(&m, &cfg, "zroot/home").unwrap();
        assert!(r2.up_to_date);

        m.create_snapshot(
            "zroot/home",
            &names::managed_name("manual", crate::time::now() + 5),
            false,
        )
        .unwrap();
        let r3 = backup_dataset(&m, &cfg, "zroot/home").unwrap();
        assert_eq!(r3.base.as_deref(), Some(r1.snapshot.as_str()));
    }
}
