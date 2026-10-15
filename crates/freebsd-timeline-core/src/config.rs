use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorKind, Result};
use crate::names;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// One `[[dataset]]` block per dataset freebsd-timeline should snapshot automatically.
    #[serde(default)]
    pub dataset: Vec<DatasetPolicy>,
    pub backup: Option<BackupConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetPolicy {
    pub name: String,
    /// Also snapshot child datasets (same snapshot name, atomically).
    #[serde(default)]
    pub recursive: bool,
    /// How often to take an automatic snapshot: `15m`, `1h`, `1d`, `1w`.
    #[serde(default = "default_every")]
    pub every: String,
    #[serde(default)]
    pub keep: Keep,
}

fn default_every() -> String {
    "1h".to_string()
}

/// How many automatic snapshots to keep per period. Manual and pre-upgrade
/// snapshots are never pruned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Keep {
    pub hourly: u32,
    pub daily: u32,
    pub weekly: u32,
    pub monthly: u32,
    pub yearly: u32,
}

impl Default for Keep {
    fn default() -> Self {
        Keep { hourly: 24, daily: 7, weekly: 4, monthly: 6, yearly: 0 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupConfig {
    /// `host` or `user@host`; uses your normal SSH keys, no accounts anywhere.
    pub target: String,
    /// Parent dataset on the target, e.g. `tank/backups/laptop`.
    pub dest: String,
    /// Datasets to back up. Empty = every watched dataset.
    #[serde(default)]
    pub datasets: Vec<String>,
    /// Send automatically after each scheduled snapshot.
    #[serde(default)]
    pub auto: bool,
}

impl BackupConfig {
    /// `zroot/home` -> `<dest>/zroot_home`
    pub fn remote_dest(&self, dataset: &str) -> String {
        format!("{}/{}", self.dest, dataset.replace('/', "_"))
    }
}

/// `15m`, `1h`, `2d`, `1w` -> seconds.
pub fn parse_duration(s: &str) -> Result<i64> {
    let bad = || Error::new(ErrorKind::Config, format!("invalid duration {s:?} (try 15m, 1h, 1d)"));
    let s = s.trim();
    let split = s.find(|c: char| !c.is_ascii_digit()).ok_or_else(bad)?;
    let (n, unit) = s.split_at(split);
    let n: i64 = n.parse().map_err(|_| bad())?;
    let mult = match unit {
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        "w" => 7 * 86400,
        _ => return Err(bad()),
    };
    Ok(n * mult)
}

impl Config {
    pub fn default_path() -> &'static str {
        if cfg!(target_os = "freebsd") {
            "/usr/local/etc/freebsd-timeline.toml"
        } else {
            "/etc/freebsd-timeline.toml"
        }
    }

    pub fn parse(text: &str) -> Result<Config> {
        let cfg: Config = toml::from_str(text)
            .map_err(|e| Error::new(ErrorKind::Config, format!("config: {e}")))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// A missing file is not an error: freebsd-timeline still works, just without schedules.
    pub fn load(path: &Path) -> Result<Config> {
        match std::fs::read_to_string(path) {
            Ok(text) => Config::parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn validate(&self) -> Result<()> {
        for ds in &self.dataset {
            names::validate_dataset(&ds.name)?;
            if parse_duration(&ds.every)? < 300 {
                return Err(Error::new(
                    ErrorKind::Config,
                    format!("{}: snapshots more often than every 5m are not supported", ds.name),
                ));
            }
        }
        if let Some(b) = &self.backup {
            names::validate_ssh_target(&b.target)?;
            names::validate_dataset(&b.dest)?;
            for ds in &b.datasets {
                names::validate_dataset(ds)?;
            }
        }
        Ok(())
    }

    pub fn policy(&self, dataset: &str) -> Option<&DatasetPolicy> {
        self.dataset.iter().find(|d| d.name == dataset)
    }

    pub fn backup_datasets(&self) -> Vec<String> {
        match &self.backup {
            Some(b) if !b.datasets.is_empty() => b.datasets.clone(),
            Some(_) => self.dataset.iter().map(|d| d.name.clone()).collect(),
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("15m").unwrap(), 900);
        assert_eq!(parse_duration("1h").unwrap(), 3600);
        assert_eq!(parse_duration("2d").unwrap(), 172800);
        assert!(parse_duration("h").is_err());
        assert!(parse_duration("10").is_err());
        assert!(parse_duration("5y").is_err());
    }

    #[test]
    fn parses_example() {
        let cfg = Config::parse(include_str!("../../../examples/freebsd-timeline.toml")).unwrap();
        assert!(!cfg.dataset.is_empty());
        assert_eq!(cfg.dataset[0].keep.hourly, 24);
    }

    #[test]
    fn rejects_bad_names() {
        assert!(Config::parse("[[dataset]]\nname = \"-R\"\n").is_err());
        assert!(Config::parse("[[dataset]]\nname = \"zroot\"\nevery = \"1m\"\n").is_err());
        assert!(Config::parse("[backup]\ntarget = \"-oProxyCommand=x\"\ndest = \"tank\"\n").is_err());
    }

    #[test]
    fn remote_dest() {
        let b = BackupConfig { target: "backuphost".into(), dest: "tank/bk".into(), datasets: vec![], auto: false };
        assert_eq!(b.remote_dest("zroot/home"), "tank/bk/zroot_home");
    }
}
