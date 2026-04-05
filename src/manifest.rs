//! Manifest file for incremental/delta exports.
//!
//! Tracks per-issue `updated` timestamps so unchanged issues can be skipped
//! on subsequent exports.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use log::warn;
use serde::{Deserialize, Serialize};

const MANIFEST_FILENAME: &str = ".jarkdown-manifest.json";

/// Per-issue metadata stored in the manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestEntry {
    /// The `fields.updated` timestamp from Jira.
    pub updated: String,
    /// When we last exported this issue.
    pub exported_at: DateTime<Utc>,
}

/// Tracks which issues have been exported and when they were last updated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub issues: HashMap<String, ManifestEntry>,
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            version: 1,
            issues: HashMap::new(),
        }
    }
}

impl Manifest {
    /// Load a manifest from the given directory. Returns a default manifest if
    /// the file doesn't exist or can't be parsed.
    pub fn load(dir: &Path) -> Self {
        let path = manifest_path(dir);
        if !path.exists() {
            return Self::default();
        }
        match std::fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_else(|e| {
                warn!("Failed to parse manifest: {}. Starting fresh.", e);
                Self::default()
            }),
            Err(e) => {
                warn!("Failed to read manifest: {}. Starting fresh.", e);
                Self::default()
            }
        }
    }

    /// Write the manifest atomically to the given directory.
    pub fn save(&self, dir: &Path) -> crate::Result<()> {
        let path = manifest_path(dir);
        let tmp_path = path.with_extension("json.tmp");
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(&tmp_path, &content)?;
        std::fs::rename(&tmp_path, &path)?;
        Ok(())
    }

    /// Returns `true` if the issue needs re-exporting — either it's not in the
    /// manifest or its `updated` timestamp has changed.
    pub fn is_stale(&self, issue_key: &str, updated: &str) -> bool {
        match self.issues.get(issue_key) {
            Some(entry) => entry.updated != updated,
            None => true,
        }
    }

    /// Record that an issue was exported with the given `updated` timestamp.
    pub fn record(&mut self, issue_key: &str, updated: &str) {
        self.issues.insert(
            issue_key.to_string(),
            ManifestEntry {
                updated: updated.to_string(),
                exported_at: Utc::now(),
            },
        );
    }
}

fn manifest_path(dir: &Path) -> PathBuf {
    dir.join(MANIFEST_FILENAME)
}
