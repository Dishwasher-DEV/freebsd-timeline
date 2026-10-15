//! Wire protocol between freebsd-timelined and its clients: one JSON request line in,
//! one JSON response line out, over a local Unix socket. Nothing listens on
//! the network.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Requests larger than this are rejected.
pub const MAX_REQUEST: u64 = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Status,
    ListSnapshots {
        dataset: Option<String>,
    },
    CreateSnapshot {
        dataset: String,
        label: Option<String>,
        #[serde(default)]
        recursive: bool,
    },
    DestroySnapshot {
        snapshot: String,
        #[serde(default)]
        recursive: bool,
    },
    Rollback {
        snapshot: String,
        #[serde(default)]
        destroy_newer: bool,
    },
    Diff {
        from: String,
        to: Option<String>,
    },
    ListDatasets,
    ListBootEnvs,
    CreateBootEnv {
        name: Option<String>,
        label: Option<String>,
    },
    ActivateBootEnv {
        name: String,
        #[serde(default)]
        temporary: bool,
    },
    DestroyBootEnv {
        name: String,
    },
    PoolHealth,
    PrunePlan,
    Prune,
    Backup {
        dataset: Option<String>,
    },
    LockAll,
    Unlock {
        dataset: String,
        passphrase: String,
    },
}

impl Request {
    /// Requests that only read state.
    pub fn is_read_only(&self) -> bool {
        matches!(
            self,
            Request::Status
                | Request::ListSnapshots { .. }
                | Request::Diff { .. }
                | Request::ListDatasets
                | Request::ListBootEnvs
                | Request::PoolHealth
                | Request::PrunePlan
        )
    }

    /// Short description for the daemon log. Never includes secrets.
    pub fn describe(&self) -> String {
        match self {
            Request::Unlock { dataset, .. } => format!("unlock {dataset}"),
            other => {
                let v = serde_json::to_value(other).unwrap_or(Value::Null);
                v.to_string()
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(default)]
    pub data: Value,
    pub error: Option<String>,
}

impl Response {
    pub fn ok(data: impl Serialize) -> Self {
        Response { ok: true, data: serde_json::to_value(data).unwrap_or(Value::Null), error: None }
    }

    pub fn err(message: impl Into<String>) -> Self {
        Response { ok: false, data: Value::Null, error: Some(message.into()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let r: Request = serde_json::from_str(r#"{"op":"create_snapshot","dataset":"zroot/home"}"#).unwrap();
        assert!(matches!(r, Request::CreateSnapshot { recursive: false, label: None, .. }));
        assert!(serde_json::from_str::<Request>(r#"{"op":"rm_rf"}"#).is_err());
    }

    #[test]
    fn describe_hides_passphrase() {
        let r = Request::Unlock { dataset: "zroot/home".into(), passphrase: "hunter2".into() };
        assert!(!r.describe().contains("hunter2"));
    }
}
