//! Local audit log of actions that go beyond reading: network tests and the
//! pods they create and delete, and actions blocked by policy. Stored as JSON
//! lines in the app's data folder, on the user's machine only.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AuditEntry {
    /// Unix seconds.
    pub time: u64,
    pub user: String,
    pub context: String,
    pub environment: String,
    /// e.g. network_test_run, network_test_blocked
    pub action: String,
    pub namespace: Option<String>,
    pub target: Option<String>,
    pub pods_created: Vec<String>,
    pub pods_deleted: Vec<String>,
    pub outcome: String,
}

impl AuditEntry {
    pub fn new(context: &str, environment: &str, action: &str, outcome: &str) -> Self {
        AuditEntry {
            time: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
            user: std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_else(|_| "unknown".into()),
            context: context.into(),
            environment: environment.into(),
            action: action.into(),
            namespace: None,
            target: None,
            pods_created: vec![],
            pods_deleted: vec![],
            outcome: outcome.into(),
        }
    }
}

pub fn append(path: &Path, entry: &AuditEntry) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{}", serde_json::to_string(entry).map_err(std::io::Error::other)?)
}

/// Most recent entries first.
pub fn read(path: &Path, limit: usize) -> Vec<AuditEntry> {
    let Ok(text) = std::fs::read_to_string(path) else { return vec![] };
    let mut v: Vec<AuditEntry> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    v.reverse();
    v.truncate(limit);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let path = std::env::temp_dir().join(format!("tessera-audit-{}.log", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut a = AuditEntry::new("dev-a", "development", "network_test_run", "2 checks failed");
        a.pods_created = vec!["tessera-probe-abc".into()];
        a.pods_deleted = vec!["tessera-probe-abc".into()];
        append(&path, &a).unwrap();
        append(&path, &AuditEntry::new("prod-b", "production", "network_test_blocked", "blocked by policy")).unwrap();
        let r = read(&path, 10);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].context, "prod-b", "newest first");
        assert_eq!(r[1].pods_deleted, vec!["tessera-probe-abc"]);
        std::fs::remove_file(path).ok();
    }
}
