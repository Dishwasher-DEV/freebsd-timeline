use serde::{Deserialize, Serialize};

use crate::names;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub dataset: String,
    pub name: String,
    /// Creation time, epoch seconds.
    pub created: i64,
    /// Space unique to this snapshot (freed if it is destroyed).
    pub used: u64,
    pub referenced: u64,
}

impl Snapshot {
    pub fn full_name(&self) -> String {
        format!("{}@{}", self.dataset, self.name)
    }

    pub fn is_managed(&self) -> bool {
        names::is_managed(&self.name)
    }

    pub fn is_auto(&self) -> bool {
        names::is_auto(&self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dataset {
    pub name: String,
    pub used: u64,
    pub avail: u64,
    pub mountpoint: Option<String>,
    pub encrypted: bool,
    /// The dataset whose key unlocks this one, if encrypted.
    pub encryption_root: Option<String>,
    pub key_loaded: bool,
    pub mounted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootEnv {
    pub name: String,
    pub active_now: bool,
    pub active_on_reboot: bool,
    pub mountpoint: Option<String>,
    pub space: String,
    pub created: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolHealth {
    pub name: String,
    pub health: String,
    pub size: u64,
    pub alloc: u64,
    pub free: u64,
    pub scan: Option<String>,
    pub errors: Option<String>,
}

impl PoolHealth {
    pub fn is_healthy(&self) -> bool {
        self.health == "ONLINE"
            && self
                .errors
                .as_deref()
                .map_or(true, |e| e.starts_with("No known data errors"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    Added,
    Removed,
    Modified,
    Renamed,
}

impl Change {
    pub fn symbol(self) -> char {
        match self {
            Change::Added => '+',
            Change::Removed => '-',
            Change::Modified => 'M',
            Change::Renamed => 'R',
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffEntry {
    pub change: Change,
    pub path: String,
    pub new_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub version: String,
    pub backend: String,
    pub watched: Vec<String>,
    pub snapshots: usize,
    pub managed: usize,
    pub last_snapshot: Option<i64>,
    pub pools: Vec<PoolHealth>,
    pub boot_envs_supported: bool,
    pub backup_target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionResult {
    pub target: String,
    pub ok: bool,
    pub message: String,
}
