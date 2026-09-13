use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub kind: String,
    pub dev: u64,
    pub ino: u64,
    pub mtime_ns: i64,
    pub ctime_ns: i64,
    pub size: u64,
    pub mode: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScanError {
    pub path: String,
    pub error: String,
    pub errno: Option<i32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScanResult {
    pub scope: String,
    pub entries: Vec<Entry>,
    pub directories: Vec<String>,
    pub errors: Vec<ScanError>,
    pub renamed: u64,
    /// Stable identity of one bounded directory observation, when chunked.
    #[serde(default)]
    pub scan_id: Option<String>,
    /// False means this job must yield and resume without negative inference.
    #[serde(default = "scan_complete")]
    pub complete: bool,
    /// Policy refused the requested scope; this proves no filesystem absence.
    #[serde(default)]
    pub traversal_skipped: bool,
}

fn scan_complete() -> bool {
    true
}

impl Default for ScanResult {
    fn default() -> Self {
        Self {
            scope: String::new(),
            entries: Vec::new(),
            directories: Vec::new(),
            errors: Vec::new(),
            renamed: 0,
            scan_id: None,
            complete: true,
            traversal_skipped: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    pub path: String,
    pub flags: u32,
    pub id: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Volume {
    pub key: String,
    pub uuid: String,
    pub device: u64,
    pub mount: String,
    pub roots: Vec<String>,
}

#[derive(Debug)]
pub struct PendingRecoveryError(pub String);
impl std::fmt::Display for PendingRecoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for PendingRecoveryError {}

pub trait Reconciler {
    fn policy(&self) -> &crate::policy::Policy;
    fn set_policy(&mut self, _policy: crate::policy::Policy) {}
    /// Requested job paths owning incomplete observations, before filesystem
    /// spelling resolution. Each request owns its own stable scan identity.
    fn active_scans(&self) -> Vec<String> {
        Vec::new()
    }
    fn retain_scans(&mut self, _paths: &[String]) {}
    fn reconcile(&mut self, path: &str, recursive: bool) -> anyhow::Result<ScanResult>;
    fn reconcile_step(&mut self, path: &str, recursive: bool) -> anyhow::Result<ScanResult> {
        self.reconcile(path, recursive)
    }
    fn retry_paths(&mut self, now: f64) -> anyhow::Result<Vec<String>>;
}

pub fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
