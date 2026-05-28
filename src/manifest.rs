//! Manifest file for incremental/delta exports.
//!
//! Manifest v2 is a graph-ready cache index. This first v2 slice keeps the
//! existing flat incremental workflow working while adding migration,
//! canonical Issue keys, richer per-Issue metadata, external manifest paths,
//! and merge-on-write persistence.

use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, Utc};
use log::warn;
use rand::{distributions::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};

use crate::error::{JarkdownError, Result};
use crate::hierarchy::{HierarchyLayout, IssueNode};
use crate::issue::Issue;

pub const MANIFEST_VERSION: u32 = 2;
pub const MANIFEST_FILENAME: &str = ".jarkdown-manifest.json";

/// Per-artifact path metadata stored in the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactPath {
    /// Path relative to the export output root.
    pub path: String,
    /// Whether this artifact path is active for the Issue.
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyIssueDirectoryWarning {
    pub issue_key: String,
    pub path: PathBuf,
    pub found_name: String,
    pub expected_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyNestedSnapshotWarning {
    pub root_key: String,
    pub path: String,
}

/// Active/evicted state for an Issue cache record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueCacheState {
    Active,
    OrphanedHierarchyMember,
    Evicted,
}

/// Stable coarse reasons for Evicted Issue tombstones in manifest v2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvictionReason {
    NotReturnedByValidationSearch,
    FetchNotFoundOrForbidden,
    ChildFetchOrExportFailed,
    ForceFetchFailed,
    Unknown(String),
}

impl EvictionReason {
    pub const NOT_RETURNED_BY_VALIDATION_SEARCH: &'static str = "not_returned_by_validation_search";
    pub const FETCH_NOT_FOUND_OR_FORBIDDEN: &'static str = "fetch_not_found_or_forbidden";
    pub const CHILD_FETCH_OR_EXPORT_FAILED: &'static str = "child_fetch_or_export_failed";
    pub const FORCE_FETCH_FAILED: &'static str = "force_fetch_failed";

    pub fn as_str(&self) -> &str {
        match self {
            Self::NotReturnedByValidationSearch => Self::NOT_RETURNED_BY_VALIDATION_SEARCH,
            Self::FetchNotFoundOrForbidden => Self::FETCH_NOT_FOUND_OR_FORBIDDEN,
            Self::ChildFetchOrExportFailed => Self::CHILD_FETCH_OR_EXPORT_FAILED,
            Self::ForceFetchFailed => Self::FORCE_FETCH_FAILED,
            Self::Unknown(reason) => reason,
        }
    }
}

impl Serialize for EvictionReason {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for EvictionReason {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let reason = String::deserialize(deserializer)?;
        Ok(match reason.as_str() {
            Self::NOT_RETURNED_BY_VALIDATION_SEARCH => Self::NotReturnedByValidationSearch,
            Self::FETCH_NOT_FOUND_OR_FORBIDDEN => Self::FetchNotFoundOrForbidden,
            Self::CHILD_FETCH_OR_EXPORT_FAILED => Self::ChildFetchOrExportFailed,
            Self::FORCE_FETCH_FAILED => Self::ForceFetchFailed,
            _ => Self::Unknown(reason),
        })
    }
}

/// Per-issue metadata stored in the manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestEntry {
    /// The `fields.updated` timestamp from Jira.
    pub updated: String,
    /// When we last exported this issue.
    pub exported_at: DateTime<Utc>,
    /// Display summary from Jira, used by artifact repair/backfill paths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Display Issue type from Jira.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_type: Option<String>,
    /// Display workflow status from Jira.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Whether this Issue is active or retained as an Evicted Issue tombstone.
    pub state: IssueCacheState,
    /// Coarse eviction reason when `state` is `evicted`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eviction_reason: Option<EvictionReason>,
    /// Artifact directories or files for this Issue, relative to the output root.
    #[serde(default)]
    pub artifact_paths: Vec<ArtifactPath>,
    /// Fingerprint of content-affecting export options used for this entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option_fingerprint: Option<String>,
    /// Requested Roots for which this Issue is directly requested.
    #[serde(default)]
    pub requested_roots: Vec<String>,
    /// Requested Roots whose hierarchy currently includes this Issue.
    #[serde(default)]
    pub hierarchy_roots: Vec<String>,
}

/// Tracks which issues have been exported and when they were last updated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub issues: HashMap<String, ManifestEntry>,
    #[serde(default)]
    pub edges: Vec<HierarchyEdge>,
    #[serde(default)]
    pub root_snapshots: HashMap<String, RootSnapshot>,
    #[serde(skip)]
    touched_issues: HashSet<String>,
    #[serde(skip)]
    touched_graph: bool,
    #[serde(skip)]
    touched_edge_parents: HashSet<String>,
    #[serde(skip)]
    touched_root_snapshots: HashSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HierarchyEdge {
    pub parent: String,
    pub child: String,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootSnapshot {
    pub root_key: String,
    pub layout: String,
    pub path: String,
    pub exported_at: DateTime<Utc>,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub truncated_by_depth: bool,
    #[serde(default)]
    pub truncated_by_issue_count: bool,
    #[serde(default)]
    pub failures: Vec<HierarchyFailureRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HierarchyFailureRecord {
    pub issue_key: String,
    pub reason: String,
}

#[derive(Debug, Deserialize)]
struct V1Manifest {
    issues: HashMap<String, V1ManifestEntry>,
}

#[derive(Debug, Deserialize)]
struct V1ManifestEntry {
    updated: String,
    exported_at: DateTime<Utc>,
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            version: MANIFEST_VERSION,
            issues: HashMap::new(),
            edges: Vec::new(),
            root_snapshots: HashMap::new(),
            touched_issues: HashSet::new(),
            touched_graph: false,
            touched_edge_parents: HashSet::new(),
            touched_root_snapshots: HashSet::new(),
        }
    }
}

impl Manifest {
    /// Load the default manifest from the given output directory. This
    /// compatibility helper preserves the old forgiving behavior for tests and
    /// library callers; CLI paths use [`load_from_path`] so future versions can
    /// fail without being overwritten.
    pub fn load(dir: &Path) -> Self {
        let path = default_manifest_path(dir);
        let manifest = Self::load_from_path(&path).unwrap_or_else(|e| {
            warn!("Failed to load manifest: {}. Starting fresh.", e);
            Self::default()
        });
        manifest.warn_legacy_issue_directories(dir);
        manifest
    }

    /// Load a manifest from an exact path.
    pub fn load_from_path(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let content = match std::fs::read_to_string(path) {
            Ok(content) => content,
            Err(e) => {
                warn!("Failed to read manifest: {}. Starting fresh.", e);
                return Ok(Self::default());
            }
        };

        let value: serde_json::Value = match serde_json::from_str(&content) {
            Ok(value) => value,
            Err(e) => {
                warn!("Failed to parse manifest: {}. Starting fresh.", e);
                return Ok(Self::default());
            }
        };

        let version = value["version"].as_u64().unwrap_or(1);
        if version > MANIFEST_VERSION as u64 {
            return Err(JarkdownError::Unexpected(format!(
                "Unsupported manifest version {} at {:?}; this jarkdown supports manifest version {} and will not overwrite it.",
                version, path, MANIFEST_VERSION
            )));
        }

        if version == 1 {
            return migrate_v1(value);
        }

        let mut manifest: Manifest = serde_json::from_value(value)?;
        manifest.version = MANIFEST_VERSION;
        manifest.touched_issues.clear();
        manifest.touched_graph = false;
        manifest.touched_edge_parents.clear();
        manifest.touched_root_snapshots.clear();
        manifest.normalize_issue_keys();
        manifest.sanitize_artifact_paths();
        manifest.warn_legacy_nested_snapshots();
        Ok(manifest)
    }

    pub fn legacy_issue_directory_warnings(
        &self,
        output_root: &Path,
    ) -> Vec<LegacyIssueDirectoryWarning> {
        let mut warnings = Vec::new();
        let mut checked = HashSet::new();

        for (issue_key, entry) in &self.issues {
            collect_legacy_issue_directory_warning(
                output_root,
                issue_key,
                issue_key,
                &mut checked,
                &mut warnings,
            );
            for artifact in &entry.artifact_paths {
                if let Some(path) = sanitize_artifact_path(&artifact.path) {
                    collect_legacy_artifact_path_warnings(
                        output_root,
                        issue_key,
                        &path,
                        &mut checked,
                        &mut warnings,
                    );
                }
            }
        }

        warnings
    }

    pub fn warn_legacy_issue_directories(&self, output_root: &Path) {
        for warning in self.legacy_issue_directory_warnings(output_root) {
            warn!(
                "Legacy case-mismatched Issue directory for {} found at {:?} (directory entry {:?}, expected {:?}); not migrating automatically.",
                warning.issue_key, warning.path, warning.found_name, warning.expected_name
            );
        }
    }

    pub fn legacy_nested_snapshot_warnings(&self) -> Vec<LegacyNestedSnapshotWarning> {
        let mut warnings = Vec::new();
        for (root_key, snapshot) in &self.root_snapshots {
            if snapshot.layout == "nested" && is_legacy_nested_snapshot_path(&snapshot.path) {
                warnings.push(LegacyNestedSnapshotWarning {
                    root_key: normalize_issue_key(root_key),
                    path: snapshot.path.clone(),
                });
            }
        }
        warnings.sort_by(|a, b| a.root_key.cmp(&b.root_key));
        warnings
    }

    pub fn warn_legacy_nested_snapshots(&self) {
        for warning in self.legacy_nested_snapshot_warnings() {
            warn!(
                "Legacy nested hierarchy snapshot for {} found at {}; keeping it readable but new nested exports write {{ROOT}}.hierarchy.md and do not delete or migrate index.md automatically.",
                warning.root_key, warning.path
            );
        }
    }

    /// Write the default manifest atomically to the given output directory.
    pub fn save(&self, dir: &Path) -> Result<()> {
        let path = default_manifest_path(dir);
        self.save_to_path(&path)
    }

    /// Write the manifest atomically to an exact path.
    ///
    /// If entries have been touched since load, the current on-disk manifest is
    /// reloaded and untouched entries from disk are preserved before saving.
    pub fn save_to_path(&self, path: &Path) -> Result<()> {
        let mut to_write = self.merged_with_disk(path)?;
        to_write.version = MANIFEST_VERSION;
        to_write.touched_issues.clear();
        to_write.touched_graph = false;
        to_write.touched_edge_parents.clear();
        to_write.touched_root_snapshots.clear();

        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }

        let content = serde_json::to_string_pretty(&to_write)?;
        write_manifest_atomically(path, content.as_bytes())?;
        Ok(())
    }

    /// Returns `true` if the issue needs re-exporting: either it is not in the
    /// manifest, is inactive, or its `updated` timestamp has changed.
    pub fn is_stale(&self, issue_key: &str, updated: &str) -> bool {
        match self.issues.get(&normalize_issue_key(issue_key)) {
            Some(entry) if entry.state != IssueCacheState::Active => true,
            Some(entry) => match compare_jira_updated(&entry.updated, updated) {
                UpdatedComparison::IncomingNewer => true,
                UpdatedComparison::Equal => false,
                UpdatedComparison::IncomingOlder => {
                    warn!(
                        "Validation timestamp for {} is older than cached manifest timestamp (cached={}, validation={}); treating as unchanged.",
                        normalize_issue_key(issue_key),
                        entry.updated,
                        updated
                    );
                    false
                }
                UpdatedComparison::Unparseable => entry.updated != updated,
            },
            None => true,
        }
    }

    /// Record minimal export metadata for compatibility with existing tests.
    pub fn record(&mut self, issue_key: &str, updated: &str) {
        let key = normalize_issue_key(issue_key);
        let stored_updated = self.stored_updated_for(&key, updated);
        self.issues.insert(
            key.clone(),
            ManifestEntry {
                updated: stored_updated,
                exported_at: Utc::now(),
                summary: None,
                issue_type: None,
                status: None,
                state: IssueCacheState::Active,
                eviction_reason: None,
                artifact_paths: vec![ArtifactPath {
                    path: key.clone(),
                    active: true,
                }],
                option_fingerprint: None,
                requested_roots: Vec::new(),
                hierarchy_roots: Vec::new(),
            },
        );
        self.touched_issues.insert(key);
    }

    /// Record export metadata from a fully fetched Issue.
    pub fn record_issue(&mut self, issue: &Issue, artifact_path: impl Into<String>) {
        self.record_issue_with_fingerprint(issue, artifact_path, None);
    }

    /// Record export metadata from a fully fetched Issue and option fingerprint.
    pub fn record_issue_with_fingerprint(
        &mut self,
        issue: &Issue,
        artifact_path: impl Into<String>,
        option_fingerprint: Option<&str>,
    ) {
        self.record_metadata_with_fingerprint(
            &issue.key,
            &issue.updated,
            Some(&issue.summary),
            Some(&issue.issuetype.name),
            Some(&issue.status.name),
            artifact_path,
            option_fingerprint,
        );
    }

    /// Record export or validation metadata without requiring a full Issue.
    pub fn record_metadata(
        &mut self,
        issue_key: &str,
        updated: &str,
        summary: Option<&str>,
        issue_type: Option<&str>,
        status: Option<&str>,
        artifact_path: impl Into<String>,
    ) {
        self.record_metadata_with_fingerprint(
            issue_key,
            updated,
            summary,
            issue_type,
            status,
            artifact_path,
            None,
        );
    }

    /// Record export or validation metadata with an option fingerprint.
    pub fn record_metadata_with_fingerprint(
        &mut self,
        issue_key: &str,
        updated: &str,
        summary: Option<&str>,
        issue_type: Option<&str>,
        status: Option<&str>,
        artifact_path: impl Into<String>,
        option_fingerprint: Option<&str>,
    ) {
        let key = normalize_issue_key(issue_key);
        let stored_updated = self.stored_updated_for(&key, updated);
        let artifact_path = normalize_artifact_path(&artifact_path.into());
        let mut existing_artifacts = self
            .issues
            .get(&key)
            .map(|entry| entry.artifact_paths.clone())
            .unwrap_or_default();
        upsert_artifact_path(&mut existing_artifacts, artifact_path, true);
        let (requested_roots, hierarchy_roots) = self
            .issues
            .get(&key)
            .map(|entry| (entry.requested_roots.clone(), entry.hierarchy_roots.clone()))
            .unwrap_or_default();
        self.issues.insert(
            key.clone(),
            ManifestEntry {
                updated: stored_updated,
                exported_at: Utc::now(),
                summary: summary.map(str::to_string),
                issue_type: issue_type.map(str::to_string),
                status: status.map(str::to_string),
                state: IssueCacheState::Active,
                eviction_reason: None,
                artifact_paths: existing_artifacts,
                option_fingerprint: option_fingerprint.map(str::to_string),
                requested_roots,
                hierarchy_roots,
            },
        );
        self.touched_issues.insert(key);
    }

    /// Return the normalized entry for an Issue key.
    pub fn get(&self, issue_key: &str) -> Option<&ManifestEntry> {
        self.issues.get(&normalize_issue_key(issue_key))
    }

    /// Return true when an Issue has an active cache entry.
    pub fn is_active(&self, issue_key: &str) -> bool {
        self.get(issue_key)
            .is_some_and(|entry| entry.state == IssueCacheState::Active)
    }

    /// Mark an active Issue as evicted without deleting files.
    pub fn evict(&mut self, issue_key: &str, reason: EvictionReason) {
        let key = normalize_issue_key(issue_key);
        let Some(entry) = self.issues.get_mut(&key) else {
            return;
        };
        entry.state = IssueCacheState::Evicted;
        entry.eviction_reason = Some(reason);
        for path in &mut entry.artifact_paths {
            path.active = false;
        }
        for edge in &mut self.edges {
            if edge.active && (edge.parent == key || edge.child == key) {
                edge.active = false;
                self.touched_edge_parents.insert(edge.parent.clone());
            }
        }
        self.touched_issues.insert(key);
        self.touched_graph = true;
    }

    pub fn record_hierarchy(
        &mut self,
        root: &IssueNode,
        snapshot_path: impl Into<String>,
        layout: HierarchyLayout,
        option_fingerprint: Option<&str>,
    ) {
        let root_key = normalize_issue_key(&root.key);
        let artifact_path = hierarchy_artifact_path(layout, None, &root_key);
        self.record_hierarchy_node(
            &root_key,
            None,
            root,
            layout,
            &artifact_path,
            option_fingerprint,
        );
        self.root_snapshots.insert(
            root_key.clone(),
            RootSnapshot {
                root_key: root_key.clone(),
                layout: match layout {
                    HierarchyLayout::Corpus => "corpus".to_string(),
                    HierarchyLayout::Nested => "nested".to_string(),
                },
                path: snapshot_path.into(),
                exported_at: Utc::now(),
                truncated: hierarchy_truncated(root),
                truncated_by_depth: hierarchy_truncated_by_depth(root),
                truncated_by_issue_count: hierarchy_truncated_by_issue_count(root),
                failures: hierarchy_failures(root),
            },
        );
        self.touched_root_snapshots.insert(root_key);
        self.recompute_hierarchy_memberships();
        self.touched_graph = true;
    }

    pub fn record_hierarchy_members(
        &mut self,
        root_key: &str,
        subtree: &IssueNode,
        option_fingerprint: Option<&str>,
    ) {
        let root_key = normalize_issue_key(root_key);
        let artifact_path = hierarchy_artifact_path(HierarchyLayout::Corpus, None, &subtree.key);
        self.record_hierarchy_node(
            &root_key,
            None,
            subtree,
            HierarchyLayout::Corpus,
            &artifact_path,
            option_fingerprint,
        );
        self.recompute_hierarchy_memberships();
        self.touched_graph = true;
    }

    pub fn record_hierarchy_members_at_path(
        &mut self,
        root_key: &str,
        subtree: &IssueNode,
        layout: HierarchyLayout,
        artifact_path: impl Into<String>,
        option_fingerprint: Option<&str>,
    ) {
        let root_key = normalize_issue_key(root_key);
        let artifact_path = normalize_artifact_path(&artifact_path.into());
        self.record_hierarchy_node(
            &root_key,
            None,
            subtree,
            layout,
            &artifact_path,
            option_fingerprint,
        );
        self.recompute_hierarchy_memberships();
        self.touched_graph = true;
    }

    pub fn record_hierarchy_metadata_with_fingerprint(
        &mut self,
        issue_key: &str,
        updated: &str,
        summary: Option<&str>,
        issue_type: Option<&str>,
        status: Option<&str>,
        artifact_path: impl Into<String>,
        option_fingerprint: Option<&str>,
    ) {
        let key = normalize_issue_key(issue_key);
        let artifact_path = normalize_artifact_path(&artifact_path.into());
        self.record_hierarchy_metadata_inner(
            &key,
            updated,
            summary,
            issue_type,
            status,
            &artifact_path,
            option_fingerprint,
        );
    }

    pub fn active_artifact_paths(&self, issue_key: &str) -> Vec<String> {
        self.get(issue_key)
            .map(|entry| {
                entry
                    .artifact_paths
                    .iter()
                    .filter(|path| path.active)
                    .filter_map(|path| sanitize_artifact_path(&path.path))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn active_hierarchy_keys(&self, root_key: &str) -> Vec<String> {
        let root_key = normalize_issue_key(root_key);
        if !self.root_snapshots.contains_key(&root_key) {
            return Vec::new();
        }
        let mut out = Vec::new();
        self.collect_hierarchy_keys(&root_key, &mut out, &mut HashSet::new());
        out
    }

    pub fn has_active_children(&self, issue_key: &str) -> bool {
        let issue_key = normalize_issue_key(issue_key);
        self.edges
            .iter()
            .any(|edge| edge.active && edge.parent == issue_key && self.is_active(&edge.child))
    }

    pub fn max_cached_descendant_depth(&self, issue_key: &str) -> u32 {
        let issue_key = normalize_issue_key(issue_key);
        self.max_cached_descendant_depth_inner(&issue_key, &mut HashSet::new())
    }

    pub fn remaining_depth_for_refresh(&self, issue_key: &str, max_depth: u32) -> u32 {
        let issue_key = normalize_issue_key(issue_key);
        if self
            .get(&issue_key)
            .is_some_and(|entry| entry.requested_roots.contains(&issue_key))
        {
            return max_depth;
        }
        let roots = self
            .get(&issue_key)
            .map(|entry| entry.hierarchy_roots.clone())
            .unwrap_or_default();
        let mut best = 0;
        for root in roots {
            for depth in self.active_depths_to(&root, &issue_key, &mut HashSet::new()) {
                if depth <= max_depth {
                    best = best.max(max_depth - depth);
                }
            }
        }
        best
    }

    pub fn is_descendant_of(&self, ancestor: &str, candidate: &str) -> bool {
        let ancestor = normalize_issue_key(ancestor);
        let candidate = normalize_issue_key(candidate);
        self.is_descendant_of_inner(&ancestor, &candidate, &mut HashSet::new())
    }

    pub fn cached_hierarchy_tree(&self, root_key: &str) -> Option<IssueNode> {
        let root_key = normalize_issue_key(root_key);
        if !self.root_snapshots.contains_key(&root_key) {
            return None;
        }
        self.cached_hierarchy_node(&root_key, &mut HashSet::new())
    }

    fn merged_with_disk(&self, path: &Path) -> Result<Self> {
        if (self.touched_issues.is_empty() && !self.touched_graph) || !path.exists() {
            return Ok(self.clone());
        }

        let mut merged = Self::load_from_path(path)?;
        for key in &self.touched_issues {
            if let Some(entry) = self.issues.get(key) {
                merged.issues.insert(key.clone(), entry.clone());
            }
        }
        if self.touched_graph {
            for parent in &self.touched_edge_parents {
                merged.edges.retain(|edge| &edge.parent != parent);
                merged.edges.extend(
                    self.edges
                        .iter()
                        .filter(|edge| &edge.parent == parent)
                        .cloned(),
                );
            }
            for root in &self.touched_root_snapshots {
                if let Some(snapshot) = self.root_snapshots.get(root) {
                    merged.root_snapshots.insert(root.clone(), snapshot.clone());
                } else {
                    merged.root_snapshots.remove(root);
                }
            }
        }
        Ok(merged)
    }

    fn record_hierarchy_node(
        &mut self,
        root_key: &str,
        parent_key: Option<&str>,
        node: &IssueNode,
        layout: HierarchyLayout,
        artifact_path: &str,
        option_fingerprint: Option<&str>,
    ) {
        if node.updated.is_empty() {
            return;
        }

        let key = normalize_issue_key(&node.key);
        self.record_hierarchy_metadata_inner(
            &key,
            &node.updated,
            Some(&node.summary),
            Some(&node.issue_type),
            None,
            artifact_path,
            option_fingerprint,
        );
        if let Some(entry) = self.issues.get_mut(&key) {
            push_unique(&mut entry.hierarchy_roots, root_key.to_string());
            if key == root_key {
                push_unique(&mut entry.requested_roots, root_key.to_string());
            }
        }

        if node.children_discovered && !node.truncated {
            self.replace_active_edges_for_parent(&key, &node.children);
        }
        for failure in &node.failures {
            if failure.reason == EvictionReason::FETCH_NOT_FOUND_OR_FORBIDDEN {
                self.evict(&failure.issue_key, EvictionReason::FetchNotFoundOrForbidden);
            }
        }

        if let Some(parent_key) = parent_key {
            let parent = normalize_issue_key(parent_key);
            self.upsert_active_edge(&parent, &key);
        }

        for child in &node.children {
            let child_path = hierarchy_artifact_path(layout, Some(artifact_path), &child.key);
            self.record_hierarchy_node(
                root_key,
                Some(&key),
                child,
                layout,
                &child_path,
                option_fingerprint,
            );
        }
    }

    fn record_hierarchy_metadata_inner(
        &mut self,
        key: &str,
        updated: &str,
        summary: Option<&str>,
        issue_type: Option<&str>,
        status: Option<&str>,
        artifact_path: &str,
        option_fingerprint: Option<&str>,
    ) {
        let key = normalize_issue_key(key);
        let stored_updated = self.stored_updated_for(&key, updated);
        let mut existing_artifacts = self
            .issues
            .get(&key)
            .map(|entry| entry.artifact_paths.clone())
            .unwrap_or_default();
        upsert_artifact_path(&mut existing_artifacts, artifact_path.to_string(), true);
        let (requested_roots, hierarchy_roots) = self
            .issues
            .get(&key)
            .map(|entry| (entry.requested_roots.clone(), entry.hierarchy_roots.clone()))
            .unwrap_or_default();
        self.issues.insert(
            key.clone(),
            ManifestEntry {
                updated: stored_updated,
                exported_at: Utc::now(),
                summary: summary.map(str::to_string),
                issue_type: issue_type.map(str::to_string),
                status: status.map(str::to_string),
                state: IssueCacheState::Active,
                eviction_reason: None,
                artifact_paths: existing_artifacts,
                option_fingerprint: option_fingerprint.map(str::to_string),
                requested_roots,
                hierarchy_roots,
            },
        );
        self.touched_issues.insert(key);
    }

    fn replace_active_edges_for_parent(&mut self, parent: &str, children: &[IssueNode]) {
        let parent = normalize_issue_key(parent);
        let current_children: HashSet<String> = children
            .iter()
            .filter(|child| !child.updated.is_empty())
            .map(|child| normalize_issue_key(&child.key))
            .collect();
        for edge in self
            .edges
            .iter_mut()
            .filter(|edge| edge.parent == parent && edge.active)
        {
            if !current_children.contains(&edge.child) {
                edge.active = false;
                self.touched_edge_parents.insert(parent.clone());
            }
        }
    }

    fn upsert_active_edge(&mut self, parent: &str, child: &str) {
        let parent = normalize_issue_key(parent);
        let child = normalize_issue_key(child);
        let mut next_edges = Vec::with_capacity(self.edges.len());
        let mut found_pair = false;
        let mut changed = false;

        for mut edge in self.edges.drain(..) {
            if edge.parent == parent && edge.child == child {
                if found_pair {
                    changed = true;
                    continue;
                }
                found_pair = true;
                if !edge.active {
                    edge.active = true;
                    changed = true;
                }
            }
            next_edges.push(edge);
        }

        if !found_pair {
            next_edges.push(HierarchyEdge {
                parent: parent.clone(),
                child,
                active: true,
            });
            changed = true;
        }

        self.edges = next_edges;
        if changed {
            self.touched_edge_parents.insert(parent);
        }
    }

    fn recompute_hierarchy_memberships(&mut self) {
        let previous: HashMap<String, (Vec<String>, IssueCacheState)> = self
            .issues
            .iter()
            .map(|(key, entry)| {
                (
                    key.clone(),
                    (entry.hierarchy_roots.clone(), entry.state.clone()),
                )
            })
            .collect();
        for entry in self.issues.values_mut() {
            entry.hierarchy_roots.clear();
        }
        let mut roots: Vec<String> = self.root_snapshots.keys().cloned().collect();
        roots.sort();
        for root in roots {
            let keys = self.active_hierarchy_keys(&root);
            for key in keys {
                if let Some(entry) = self.issues.get_mut(&key) {
                    push_unique(&mut entry.hierarchy_roots, root.clone());
                }
            }
        }
        let active_children: HashSet<String> = self
            .edges
            .iter()
            .filter(|edge| edge.active)
            .map(|edge| edge.child.clone())
            .collect();
        let known_children: HashSet<String> =
            self.edges.iter().map(|edge| edge.child.clone()).collect();
        for (key, entry) in &mut self.issues {
            if entry.state == IssueCacheState::Evicted {
                continue;
            }
            if !entry.requested_roots.is_empty() || !entry.hierarchy_roots.is_empty() {
                entry.state = IssueCacheState::Active;
                continue;
            }
            if known_children.contains(key) && !active_children.contains(key) {
                entry.state = IssueCacheState::OrphanedHierarchyMember;
            }
        }
        for (key, entry) in &self.issues {
            if previous.get(key).is_some_and(|(roots, state)| {
                roots != &entry.hierarchy_roots || state != &entry.state
            }) {
                self.touched_issues.insert(key.clone());
            }
        }
    }

    fn collect_hierarchy_keys(
        &self,
        key: &str,
        out: &mut Vec<String>,
        visited: &mut HashSet<String>,
    ) {
        let key = normalize_issue_key(key);
        if !visited.insert(key.clone()) {
            return;
        }
        if !self.is_active(&key) {
            return;
        }
        out.push(key.clone());
        for edge in self
            .edges
            .iter()
            .filter(|edge| edge.active && edge.parent == key)
        {
            self.collect_hierarchy_keys(&edge.child, out, visited);
        }
    }

    fn cached_hierarchy_node(&self, key: &str, visited: &mut HashSet<String>) -> Option<IssueNode> {
        let key = normalize_issue_key(key);
        if !visited.insert(key.clone()) {
            return Some(IssueNode {
                key,
                summary: "(already visited)".to_string(),
                issue_type: String::new(),
                updated: String::new(),
                children_discovered: false,
                truncated: false,
                truncated_by_depth: false,
                truncated_by_issue_count: false,
                failures: Vec::new(),
                children: Vec::new(),
            });
        }
        let entry = self.get(&key)?;
        if entry.state != IssueCacheState::Active {
            return None;
        }
        let children = self
            .edges
            .iter()
            .filter(|edge| edge.active && edge.parent == key)
            .filter_map(|edge| self.cached_hierarchy_node(&edge.child, visited))
            .collect();
        Some(IssueNode {
            key,
            summary: entry.summary.clone().unwrap_or_default(),
            issue_type: entry.issue_type.clone().unwrap_or_default(),
            updated: entry.updated.clone(),
            children_discovered: false,
            truncated: false,
            truncated_by_depth: false,
            truncated_by_issue_count: false,
            failures: Vec::new(),
            children,
        })
    }

    fn max_cached_descendant_depth_inner(&self, key: &str, visited: &mut HashSet<String>) -> u32 {
        let key = normalize_issue_key(key);
        if !visited.insert(key.clone()) {
            return 0;
        }
        self.edges
            .iter()
            .filter(|edge| edge.active && edge.parent == key && self.is_active(&edge.child))
            .map(|edge| 1 + self.max_cached_descendant_depth_inner(&edge.child, visited))
            .max()
            .unwrap_or(0)
    }

    fn active_depths_to(
        &self,
        current: &str,
        target: &str,
        visited: &mut HashSet<String>,
    ) -> Vec<u32> {
        let current = normalize_issue_key(current);
        let target = normalize_issue_key(target);
        if !visited.insert(current.clone()) {
            return Vec::new();
        }
        let mut depths = Vec::new();
        if current == target {
            depths.push(0);
        }
        for edge in self
            .edges
            .iter()
            .filter(|edge| edge.active && edge.parent == current && self.is_active(&edge.child))
        {
            let mut child_visited = visited.clone();
            for depth in self.active_depths_to(&edge.child, &target, &mut child_visited) {
                depths.push(depth + 1);
            }
        }
        depths
    }

    fn is_descendant_of_inner(
        &self,
        ancestor: &str,
        candidate: &str,
        visited: &mut HashSet<String>,
    ) -> bool {
        let ancestor = normalize_issue_key(ancestor);
        if !visited.insert(ancestor.clone()) {
            return false;
        }
        for edge in self
            .edges
            .iter()
            .filter(|edge| edge.active && edge.parent == ancestor)
        {
            if edge.child == candidate
                || self.is_descendant_of_inner(&edge.child, candidate, visited)
            {
                return true;
            }
        }
        false
    }

    fn normalize_issue_keys(&mut self) {
        let issues = std::mem::take(&mut self.issues);
        self.issues = issues
            .into_iter()
            .map(|(key, entry)| (normalize_issue_key(&key), entry))
            .collect();
        for edge in &mut self.edges {
            edge.parent = normalize_issue_key(&edge.parent);
            edge.child = normalize_issue_key(&edge.child);
        }
        let snapshots = std::mem::take(&mut self.root_snapshots);
        self.root_snapshots = snapshots
            .into_iter()
            .map(|(key, mut snapshot)| {
                let normalized = normalize_issue_key(&key);
                snapshot.root_key = normalize_issue_key(&snapshot.root_key);
                for failure in &mut snapshot.failures {
                    failure.issue_key = normalize_issue_key(&failure.issue_key);
                }
                (normalized, snapshot)
            })
            .collect();
        for entry in self.issues.values_mut() {
            entry.requested_roots = normalized_unique_keys(&entry.requested_roots);
            entry.hierarchy_roots = normalized_unique_keys(&entry.hierarchy_roots);
        }
    }

    fn sanitize_artifact_paths(&mut self) {
        for (issue_key, entry) in &mut self.issues {
            entry.artifact_paths.retain_mut(|artifact| {
                if let Some(path) = sanitize_artifact_path(&artifact.path) {
                    artifact.path = path;
                    true
                } else {
                    warn!(
                        "Dropping unsafe manifest artifact path for {}: {}",
                        issue_key, artifact.path
                    );
                    false
                }
            });
        }
    }

    fn stored_updated_for(&self, issue_key: &str, incoming: &str) -> String {
        let Some(existing) = self.get(issue_key) else {
            return incoming.to_string();
        };
        match compare_jira_updated(&existing.updated, incoming) {
            UpdatedComparison::IncomingOlder => {
                warn!(
                    "Refusing to regress manifest timestamp for {} from {} to {}.",
                    normalize_issue_key(issue_key),
                    existing.updated,
                    incoming
                );
                existing.updated.clone()
            }
            _ => incoming.to_string(),
        }
    }
}

pub fn default_manifest_path(dir: &Path) -> PathBuf {
    dir.join(MANIFEST_FILENAME)
}

pub fn normalize_issue_key(issue_key: &str) -> String {
    issue_key.trim().to_ascii_uppercase()
}

pub fn relative_artifact_path(output_root: &Path, artifact_path: &Path) -> String {
    artifact_path
        .strip_prefix(output_root)
        .unwrap_or(artifact_path)
        .to_string_lossy()
        .trim_start_matches(std::path::MAIN_SEPARATOR)
        .to_string()
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ExportFingerprintOptions<'a> {
    pub include_fields: Option<&'a str>,
    pub exclude_fields: Option<&'a str>,
    pub no_attachments: bool,
    pub include_json: bool,
    pub include_changelog: bool,
    pub max_depth: Option<u32>,
    pub max_issues: Option<u32>,
}

pub fn export_option_fingerprint(options: ExportFingerprintOptions<'_>) -> Option<String> {
    if options.include_fields.is_none()
        && options.exclude_fields.is_none()
        && !options.no_attachments
        && !options.include_json
        && !options.include_changelog
        && options.max_depth.is_none()
        && options.max_issues.is_none()
    {
        return None;
    }
    Some(format!(
        "v1:exclude_fields={};include_changelog={};include_fields={};include_json={};max_depth={};max_issues={};no_attachments={}",
        canonical_field_list(options.exclude_fields),
        options.include_changelog,
        canonical_field_list(options.include_fields),
        options.include_json,
        options
            .max_depth
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string()),
        options
            .max_issues
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string()),
        options.no_attachments
    ))
}

pub fn legacy_export_option_fingerprint(
    include_fields: Option<&str>,
    exclude_fields: Option<&str>,
    no_attachments: bool,
) -> Option<String> {
    export_option_fingerprint(ExportFingerprintOptions {
        include_fields,
        exclude_fields,
        no_attachments,
        ..ExportFingerprintOptions::default()
    })
}

pub fn sanitize_artifact_path(path: &str) -> Option<String> {
    let normalized = path.replace('\\', "/");
    if normalized.is_empty() || normalized.starts_with('/') {
        return None;
    }
    let first_segment = normalized.split('/').next().unwrap_or_default();
    if first_segment.len() == 2
        && first_segment.ends_with(':')
        && first_segment.as_bytes()[0].is_ascii_alphabetic()
    {
        return None;
    }

    let mut parts = Vec::new();
    for component in Path::new(&normalized).components() {
        match component {
            Component::Normal(part) => {
                let part = part.to_string_lossy();
                if part.is_empty() || part == "." || part == ".." {
                    return None;
                }
                parts.push(part.to_string());
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.contains(&value) {
        values.push(value);
    }
}

fn collect_legacy_artifact_path_warnings(
    output_root: &Path,
    issue_key: &str,
    artifact_path: &str,
    checked: &mut HashSet<(PathBuf, String)>,
    warnings: &mut Vec<LegacyIssueDirectoryWarning>,
) {
    let mut parent = output_root.to_path_buf();
    for segment in artifact_path
        .split('/')
        .filter(|segment| !segment.is_empty())
    {
        if normalize_issue_key(segment) == normalize_issue_key(issue_key) {
            collect_legacy_issue_directory_warning(&parent, issue_key, segment, checked, warnings);
        }
        parent.push(segment);
    }
}

fn collect_legacy_issue_directory_warning(
    parent: &Path,
    issue_key: &str,
    expected_name: &str,
    checked: &mut HashSet<(PathBuf, String)>,
    warnings: &mut Vec<LegacyIssueDirectoryWarning>,
) {
    let canonical_key = normalize_issue_key(issue_key);
    let expected_name = normalize_issue_key(expected_name);
    if !checked.insert((parent.to_path_buf(), expected_name.clone())) {
        return;
    }

    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let found_name = entry.file_name().to_string_lossy().to_string();
        if found_name.eq_ignore_ascii_case(&expected_name) && found_name != expected_name {
            warnings.push(LegacyIssueDirectoryWarning {
                issue_key: canonical_key.clone(),
                path: entry.path(),
                found_name,
                expected_name: expected_name.clone(),
            });
        }
    }
}

fn upsert_artifact_path(paths: &mut Vec<ArtifactPath>, path: String, active: bool) {
    if let Some(existing) = paths.iter_mut().find(|artifact| artifact.path == path) {
        existing.active = active;
        return;
    }
    paths.push(ArtifactPath { path, active });
}

fn hierarchy_artifact_path(
    layout: HierarchyLayout,
    parent_artifact_path: Option<&str>,
    issue_key: &str,
) -> String {
    let key = normalize_issue_key(issue_key);
    match (layout, parent_artifact_path) {
        (HierarchyLayout::Corpus, _) | (HierarchyLayout::Nested, None) => key,
        (HierarchyLayout::Nested, Some(parent)) => {
            normalize_artifact_path(&format!("{}/{}", parent, key))
        }
    }
}

fn normalize_artifact_path(path: &str) -> String {
    sanitize_artifact_path(path).unwrap_or_else(|| {
        warn!("Rejected unsafe artifact path while normalizing: {}", path);
        "UNSAFE_ARTIFACT_PATH".to_string()
    })
}

fn is_legacy_nested_snapshot_path(path: &str) -> bool {
    path.replace('\\', "/").trim_start_matches("./") == "index.md"
}

fn hierarchy_truncated(node: &IssueNode) -> bool {
    node.truncated || node.children.iter().any(hierarchy_truncated)
}

fn hierarchy_truncated_by_depth(node: &IssueNode) -> bool {
    node.truncated_by_depth || node.children.iter().any(hierarchy_truncated_by_depth)
}

fn hierarchy_truncated_by_issue_count(node: &IssueNode) -> bool {
    node.truncated_by_issue_count || node.children.iter().any(hierarchy_truncated_by_issue_count)
}

fn hierarchy_failures(node: &IssueNode) -> Vec<HierarchyFailureRecord> {
    let mut out: Vec<HierarchyFailureRecord> = node
        .failures
        .iter()
        .map(|failure| HierarchyFailureRecord {
            issue_key: normalize_issue_key(&failure.issue_key),
            reason: failure.reason.clone(),
        })
        .collect();
    for child in &node.children {
        out.extend(hierarchy_failures(child));
    }
    out
}

fn canonical_field_list(fields: Option<&str>) -> String {
    let mut fields: Vec<String> = fields
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|field| !field.is_empty())
        .map(str::to_string)
        .collect();
    fields.sort();
    fields.join(",")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpdatedComparison {
    IncomingNewer,
    Equal,
    IncomingOlder,
    Unparseable,
}

fn compare_jira_updated(cached: &str, incoming: &str) -> UpdatedComparison {
    let Some(cached) = parse_jira_updated(cached) else {
        return UpdatedComparison::Unparseable;
    };
    let Some(incoming) = parse_jira_updated(incoming) else {
        return UpdatedComparison::Unparseable;
    };
    match incoming.cmp(&cached) {
        std::cmp::Ordering::Greater => UpdatedComparison::IncomingNewer,
        std::cmp::Ordering::Equal => UpdatedComparison::Equal,
        std::cmp::Ordering::Less => UpdatedComparison::IncomingOlder,
    }
}

fn parse_jira_updated(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .or_else(|_| DateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f%z"))
        .ok()
        .map(|datetime| datetime.with_timezone(&Utc))
}

fn normalized_unique_keys(keys: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for key in keys {
        let key = normalize_issue_key(key);
        if !key.is_empty() && !out.contains(&key) {
            out.push(key);
        }
    }
    out
}

fn migrate_v1(value: serde_json::Value) -> Result<Manifest> {
    let v1: V1Manifest = serde_json::from_value(value)?;
    let mut manifest = Manifest::default();
    manifest.issues = v1
        .issues
        .into_iter()
        .map(|(key, entry)| {
            let normalized = normalize_issue_key(&key);
            (
                normalized.clone(),
                ManifestEntry {
                    updated: entry.updated,
                    exported_at: entry.exported_at,
                    summary: None,
                    issue_type: None,
                    status: None,
                    state: IssueCacheState::Active,
                    eviction_reason: None,
                    artifact_paths: vec![ArtifactPath {
                        path: normalized,
                        active: true,
                    }],
                    option_fingerprint: None,
                    requested_roots: Vec::new(),
                    hierarchy_roots: Vec::new(),
                },
            )
        })
        .collect();
    Ok(manifest)
}

fn write_manifest_atomically(path: &Path, content: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| MANIFEST_FILENAME.to_string());
    let mut last_error = None;
    for _ in 0..16 {
        let random: String = rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(12)
            .map(char::from)
            .collect();
        let tmp_path = parent.join(format!(".{}.{}.tmp", file_name, random));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
        {
            Ok(mut file) => {
                if let Err(e) = file.write_all(content).and_then(|_| file.sync_all()) {
                    let _ = std::fs::remove_file(&tmp_path);
                    return Err(e.into());
                }
                std::fs::rename(&tmp_path, path)?;
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                last_error = Some(e);
            }
            Err(e) => return Err(e.into()),
        }
    }
    Err(last_error
        .unwrap_or_else(|| std::io::Error::new(std::io::ErrorKind::AlreadyExists, "temp path"))
        .into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let d = std::env::temp_dir().join(format!("jarkdown-manifest-{}-{}", label, n));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn eviction_reason_serializes_stable_manifest_strings() {
        let cases = [
            (
                EvictionReason::NotReturnedByValidationSearch,
                EvictionReason::NOT_RETURNED_BY_VALIDATION_SEARCH,
            ),
            (
                EvictionReason::FetchNotFoundOrForbidden,
                EvictionReason::FETCH_NOT_FOUND_OR_FORBIDDEN,
            ),
            (
                EvictionReason::ChildFetchOrExportFailed,
                EvictionReason::CHILD_FETCH_OR_EXPORT_FAILED,
            ),
            (
                EvictionReason::ForceFetchFailed,
                EvictionReason::FORCE_FETCH_FAILED,
            ),
        ];

        for (reason, expected) in cases {
            let encoded = serde_json::to_string(&reason).unwrap();
            assert_eq!(encoded, format!(r#""{}""#, expected));
            let decoded: EvictionReason = serde_json::from_str(&encoded).unwrap();
            assert_eq!(decoded, reason);
        }
    }

    #[test]
    fn unknown_legacy_eviction_reason_loads_and_round_trips() {
        let encoded = r#""legacy_custom_reason""#;
        let decoded: EvictionReason = serde_json::from_str(encoded).unwrap();
        assert_eq!(
            decoded,
            EvictionReason::Unknown("legacy_custom_reason".to_string())
        );
        assert_eq!(serde_json::to_string(&decoded).unwrap(), encoded);
    }

    #[test]
    fn legacy_nested_index_snapshot_loads_and_reports_compatibility_warning() {
        let dir = temp_dir("legacy-nested-index-snapshot");
        let path = default_manifest_path(&dir);
        std::fs::write(
            &path,
            r#"{
                "version": 2,
                "issues": {},
                "edges": [],
                "root_snapshots": {
                    "a": {
                        "root_key": "a",
                        "layout": "nested",
                        "path": "index.md",
                        "exported_at": "2026-01-01T00:00:00Z",
                        "truncated": false,
                        "failures": []
                    }
                }
            }"#,
        )
        .unwrap();

        let manifest = Manifest::load_from_path(&path).unwrap();

        assert_eq!(manifest.root_snapshots["A"].path, "index.md");
        assert_eq!(
            manifest.legacy_nested_snapshot_warnings(),
            vec![LegacyNestedSnapshotWarning {
                root_key: "A".to_string(),
                path: "index.md".to_string(),
            }]
        );
        assert!(
            !dir.join("A.hierarchy.md").exists(),
            "loading a legacy nested snapshot must not migrate files"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn v1_manifest_loads_and_saves_back_as_v2() {
        let dir = temp_dir("v1");
        let path = default_manifest_path(&dir);
        std::fs::write(
            &path,
            r#"{"version":1,"issues":{"proj-1":{"updated":"2026-01-01T00:00:00.000+0000","exported_at":"2026-01-02T00:00:00Z"}}}"#,
        )
        .unwrap();

        let manifest = Manifest::load_from_path(&path).unwrap();
        assert_eq!(manifest.version, MANIFEST_VERSION);
        assert!(manifest.get("PROJ-1").is_some());

        manifest.save_to_path(&path).unwrap();
        let saved: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved["version"], json!(2));
        assert!(saved["issues"]["PROJ-1"].is_object());
        assert_eq!(saved["issues"]["PROJ-1"]["state"], json!("active"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unsupported_future_version_fails_and_save_does_not_overwrite() {
        let dir = temp_dir("future");
        let path = default_manifest_path(&dir);
        let original = r#"{"version":99,"issues":{}}"#;
        std::fs::write(&path, original).unwrap();

        let err = Manifest::load_from_path(&path).unwrap_err();
        assert!(err.to_string().contains("Unsupported manifest version 99"));

        let mut manifest = Manifest::default();
        manifest.record("PROJ-1", "2026-01-01T00:00:00.000+0000");
        let err = manifest.save_to_path(&path).unwrap_err();
        assert!(err.to_string().contains("Unsupported manifest version 99"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn requested_keys_are_normalized() {
        let mut manifest = Manifest::default();
        manifest.record("proj-1", "one");
        manifest.record("PROJ-1", "two");

        assert_eq!(manifest.issues.len(), 1);
        assert_eq!(manifest.get("ProJ-1").unwrap().updated, "two");
    }

    #[test]
    fn legacy_issue_directory_warning_uses_actual_directory_entry_casing() {
        let dir = temp_dir("legacy-issue-dir-casing");
        std::fs::create_dir_all(dir.join("proj-1")).unwrap();

        let mut manifest = Manifest::default();
        manifest.record("PROJ-1", "2026-01-01T00:00:00.000+0000");

        let warnings = manifest.legacy_issue_directory_warnings(&dir);
        assert_eq!(warnings.len(), 1, "warnings: {:?}", warnings);
        assert_eq!(warnings[0].issue_key, "PROJ-1");
        assert_eq!(warnings[0].found_name, "proj-1");
        assert_eq!(warnings[0].expected_name, "PROJ-1");
        assert_eq!(warnings[0].path, dir.join("proj-1"));

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn merge_on_write_preserves_untouched_disk_entries() {
        let dir = temp_dir("merge");
        let path = default_manifest_path(&dir);

        let mut first = Manifest::default();
        first.record("PROJ-1", "one");
        first.save_to_path(&path).unwrap();

        let mut loaded = Manifest::load_from_path(&path).unwrap();
        let mut other_invocation = Manifest::load_from_path(&path).unwrap();
        other_invocation.record("PROJ-2", "two");
        other_invocation.save_to_path(&path).unwrap();

        loaded.record("PROJ-1", "one-new");
        loaded.save_to_path(&path).unwrap();

        let saved = Manifest::load_from_path(&path).unwrap();
        assert_eq!(saved.get("PROJ-1").unwrap().updated, "one-new");
        assert_eq!(saved.get("PROJ-2").unwrap().updated, "two");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn evict_marks_entry_inactive_without_removing_paths() {
        let mut manifest = Manifest::default();
        manifest.record("PROJ-1", "one");

        manifest.evict("proj-1", EvictionReason::NotReturnedByValidationSearch);

        let entry = manifest.get("PROJ-1").unwrap();
        assert_eq!(entry.state, IssueCacheState::Evicted);
        assert_eq!(
            entry.eviction_reason,
            Some(EvictionReason::NotReturnedByValidationSearch)
        );
        assert_eq!(entry.artifact_paths.len(), 1);
        assert!(!entry.artifact_paths[0].active);
    }

    #[test]
    fn stale_comparison_parses_timestamps_with_string_fallback() {
        let mut manifest = Manifest::default();
        manifest.record("PROJ-1", "2026-01-01T00:00:00.000+0000");

        assert!(!manifest.is_stale("PROJ-1", "2026-01-01T00:00:00Z"));
        assert!(manifest.is_stale("PROJ-1", "2026-01-01T00:00:01Z"));
        assert!(!manifest.is_stale("PROJ-1", "2025-12-31T23:59:59Z"));
        assert!(manifest.is_stale("PROJ-1", "not-a-timestamp"));

        manifest.record_metadata(
            "PROJ-1",
            "2025-12-31T23:59:59Z",
            Some("Older"),
            Some("Task"),
            Some("Open"),
            "PROJ-1",
        );
        assert_eq!(
            manifest.get("PROJ-1").unwrap().updated,
            "2026-01-01T00:00:00.000+0000"
        );
    }

    #[test]
    fn option_fingerprint_is_versioned_and_includes_content_visible_inputs() {
        let base = export_option_fingerprint(ExportFingerprintOptions {
            no_attachments: true,
            ..ExportFingerprintOptions::default()
        })
        .unwrap();

        assert!(base.starts_with("v1:"));
        assert_ne!(
            base,
            export_option_fingerprint(ExportFingerprintOptions {
                no_attachments: true,
                include_json: true,
                ..ExportFingerprintOptions::default()
            })
            .unwrap()
        );
        assert_ne!(
            base,
            export_option_fingerprint(ExportFingerprintOptions {
                no_attachments: true,
                include_changelog: true,
                ..ExportFingerprintOptions::default()
            })
            .unwrap()
        );
        assert_ne!(
            base,
            export_option_fingerprint(ExportFingerprintOptions {
                no_attachments: true,
                max_depth: Some(3),
                ..ExportFingerprintOptions::default()
            })
            .unwrap()
        );
        assert_ne!(
            base,
            export_option_fingerprint(ExportFingerprintOptions {
                no_attachments: true,
                max_issues: Some(10),
                ..ExportFingerprintOptions::default()
            })
            .unwrap()
        );
    }

    #[test]
    fn unsafe_artifact_paths_are_dropped_on_load() {
        let dir = temp_dir("unsafe-path");
        let path = default_manifest_path(&dir);
        std::fs::write(
            &path,
            r#"{
                "version": 2,
                "issues": {
                    "PROJ-1": {
                        "updated": "2026-01-01T00:00:00.000+0000",
                        "exported_at": "2026-01-02T00:00:00Z",
                        "state": "active",
                        "artifact_paths": [
                            {"path": "../escape", "active": true},
                            {"path": "/absolute", "active": true},
                            {"path": "C:\\escape", "active": true},
                            {"path": "PROJ-1", "active": true}
                        ]
                    }
                }
            }"#,
        )
        .unwrap();

        let manifest = Manifest::load_from_path(&path).unwrap();
        assert_eq!(
            manifest.active_artifact_paths("PROJ-1"),
            vec!["PROJ-1".to_string()]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn randomized_manifest_save_does_not_follow_legacy_temp_symlink() {
        let dir = temp_dir("temp-symlink");
        let path = dir.join("state.json");
        let legacy_tmp = dir.join("state.json.tmp");
        let target = dir.join("target.txt");
        std::fs::write(&target, "do not overwrite").unwrap();
        std::os::unix::fs::symlink(&target, &legacy_tmp).unwrap();

        let mut manifest = Manifest::default();
        manifest.record("PROJ-1", "2026-01-01T00:00:00.000+0000");
        manifest.save_to_path(&path).unwrap();

        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "do not overwrite"
        );
        assert!(path.exists());
        assert!(legacy_tmp.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn completed_child_discovery_removes_only_relevant_edge_and_orphans_child() {
        let mut manifest = Manifest::default();
        manifest.record_hierarchy(
            &hierarchy_node(
                "A",
                vec![hierarchy_node("B", vec![]), hierarchy_node("C", vec![])],
            ),
            "A.hierarchy.md",
            HierarchyLayout::Corpus,
            None,
        );

        manifest.record_hierarchy(
            &hierarchy_node("A", vec![hierarchy_node("B", vec![])]),
            "A.hierarchy.md",
            HierarchyLayout::Corpus,
            None,
        );

        assert!(manifest
            .edges
            .iter()
            .any(|edge| edge.parent == "A" && edge.child == "B" && edge.active));
        assert!(manifest
            .edges
            .iter()
            .any(|edge| edge.parent == "A" && edge.child == "C" && !edge.active));
        let orphan = manifest.get("C").unwrap();
        assert_eq!(orphan.state, IssueCacheState::OrphanedHierarchyMember);
        assert!(
            orphan.artifact_paths.iter().any(|path| path.active),
            "orphaned hierarchy members keep their files"
        );
    }

    #[test]
    fn relinking_hierarchy_edge_reactivates_existing_edge_without_duplicates() {
        let mut manifest = Manifest::default();
        manifest.record_hierarchy(
            &hierarchy_node("A", vec![hierarchy_node("B", vec![])]),
            "A.hierarchy.md",
            HierarchyLayout::Corpus,
            None,
        );

        for _ in 0..2 {
            manifest.record_hierarchy(
                &hierarchy_node("A", vec![]),
                "A.hierarchy.md",
                HierarchyLayout::Corpus,
                None,
            );
            manifest.record_hierarchy(
                &hierarchy_node("A", vec![hierarchy_node("B", vec![])]),
                "A.hierarchy.md",
                HierarchyLayout::Corpus,
                None,
            );
        }

        let matching_edges: Vec<_> = manifest
            .edges
            .iter()
            .filter(|edge| edge.parent == "A" && edge.child == "B")
            .collect();
        assert_eq!(matching_edges.len(), 1);
        assert!(matching_edges[0].active);
    }

    #[test]
    fn relinking_hierarchy_edge_collapses_touched_legacy_duplicates() {
        let mut manifest = Manifest::default();
        manifest.edges.push(HierarchyEdge {
            parent: "A".to_string(),
            child: "B".to_string(),
            active: false,
        });
        manifest.edges.push(HierarchyEdge {
            parent: "A".to_string(),
            child: "B".to_string(),
            active: false,
        });
        manifest.edges.push(HierarchyEdge {
            parent: "A".to_string(),
            child: "C".to_string(),
            active: true,
        });

        manifest.record_hierarchy(
            &hierarchy_node("A", vec![hierarchy_node("B", vec![])]),
            "A.hierarchy.md",
            HierarchyLayout::Corpus,
            None,
        );

        let ab_edges: Vec<_> = manifest
            .edges
            .iter()
            .filter(|edge| edge.parent == "A" && edge.child == "B")
            .collect();
        assert_eq!(ab_edges.len(), 1);
        assert!(ab_edges[0].active);
        assert!(manifest
            .edges
            .iter()
            .any(|edge| edge.parent == "A" && edge.child == "C" && !edge.active));
    }

    #[test]
    fn truncated_hierarchy_record_preserves_unvisited_old_edges() {
        let mut manifest = Manifest::default();
        manifest.record_hierarchy(
            &hierarchy_node(
                "A",
                vec![hierarchy_node("B", vec![]), hierarchy_node("C", vec![])],
            ),
            "A.hierarchy.md",
            HierarchyLayout::Corpus,
            None,
        );

        let mut truncated = hierarchy_node("A", vec![hierarchy_node("B", vec![])]);
        truncated.truncated = true;
        manifest.record_hierarchy(&truncated, "A.hierarchy.md", HierarchyLayout::Corpus, None);

        assert!(manifest
            .edges
            .iter()
            .any(|edge| edge.parent == "A" && edge.child == "C" && edge.active));
        assert!(manifest.root_snapshots["A"].truncated);
    }

    #[test]
    fn root_snapshot_records_depth_truncation_cause() {
        let mut manifest = Manifest::default();
        let mut capped = hierarchy_node("C", vec![]);
        capped.truncated = true;
        capped.truncated_by_depth = true;

        manifest.record_hierarchy(
            &hierarchy_node("A", vec![hierarchy_node("B", vec![capped])]),
            "A.hierarchy.md",
            HierarchyLayout::Corpus,
            None,
        );

        let snapshot = &manifest.root_snapshots["A"];
        assert!(snapshot.truncated);
        assert!(snapshot.truncated_by_depth);
        assert!(!snapshot.truncated_by_issue_count);
    }

    #[test]
    fn root_snapshot_records_issue_count_truncation_cause() {
        let mut manifest = Manifest::default();
        let mut capped = hierarchy_node("A", vec![hierarchy_node("B", vec![])]);
        capped.truncated = true;
        capped.truncated_by_issue_count = true;

        manifest.record_hierarchy(&capped, "A.hierarchy.md", HierarchyLayout::Corpus, None);

        let snapshot = &manifest.root_snapshots["A"];
        assert!(snapshot.truncated);
        assert!(!snapshot.truncated_by_depth);
        assert!(snapshot.truncated_by_issue_count);
    }

    #[test]
    fn old_root_snapshot_defaults_missing_truncation_causes_without_inference() {
        let dir = temp_dir("old-root-truncation");
        let path = default_manifest_path(&dir);
        std::fs::write(
            &path,
            r#"{
                "version": 2,
                "issues": {},
                "root_snapshots": {
                    "A": {
                        "root_key": "A",
                        "layout": "corpus",
                        "path": "A.hierarchy.md",
                        "exported_at": "2026-01-02T00:00:00Z",
                        "truncated": true
                    }
                }
            }"#,
        )
        .unwrap();

        let manifest = Manifest::load_from_path(&path).unwrap();
        let snapshot = &manifest.root_snapshots["A"];
        assert!(snapshot.truncated);
        assert!(!snapshot.truncated_by_depth);
        assert!(!snapshot.truncated_by_issue_count);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn remaining_depth_for_refresh_uses_requested_root_path_depths() {
        let mut manifest = Manifest::default();
        manifest.record_hierarchy(
            &hierarchy_node(
                "A",
                vec![hierarchy_node(
                    "B",
                    vec![hierarchy_node("C", vec![hierarchy_node("D", vec![])])],
                )],
            ),
            "A.hierarchy.md",
            HierarchyLayout::Corpus,
            None,
        );

        assert_eq!(manifest.remaining_depth_for_refresh("C", 3), 1);
        assert_eq!(manifest.remaining_depth_for_refresh("A", 3), 3);
    }

    #[test]
    fn remaining_depth_for_refresh_uses_max_across_active_paths() {
        let mut manifest = Manifest::default();
        manifest.record_hierarchy(
            &hierarchy_node(
                "A",
                vec![hierarchy_node("X", vec![hierarchy_node("C", vec![])])],
            ),
            "A.hierarchy.md",
            HierarchyLayout::Corpus,
            None,
        );
        manifest.record_hierarchy(
            &hierarchy_node("B", vec![hierarchy_node("C", vec![])]),
            "B.hierarchy.md",
            HierarchyLayout::Corpus,
            None,
        );

        assert_eq!(manifest.remaining_depth_for_refresh("C", 3), 2);
    }

    #[test]
    fn inaccessible_child_failure_evicts_child_and_records_snapshot_failure() {
        let mut manifest = Manifest::default();
        manifest.record_hierarchy(
            &hierarchy_node("A", vec![hierarchy_node("B", vec![])]),
            "A.hierarchy.md",
            HierarchyLayout::Corpus,
            None,
        );

        let mut root = hierarchy_node("A", vec![]);
        root.failures.push(crate::hierarchy::HierarchyFailure {
            issue_key: "B".to_string(),
            reason: "fetch_not_found_or_forbidden".to_string(),
        });
        manifest.record_hierarchy(&root, "A.hierarchy.md", HierarchyLayout::Corpus, None);

        assert_eq!(manifest.get("B").unwrap().state, IssueCacheState::Evicted);
        assert!(manifest
            .edges
            .iter()
            .any(|edge| edge.parent == "A" && edge.child == "B" && !edge.active));
        assert_eq!(manifest.root_snapshots["A"].failures.len(), 1);
        assert_eq!(
            manifest.root_snapshots["A"].failures[0].reason,
            "fetch_not_found_or_forbidden"
        );
    }

    #[test]
    fn nested_hierarchy_records_multiple_active_artifact_paths_for_shared_issue() {
        let mut manifest = Manifest::default();
        manifest.record_hierarchy(
            &hierarchy_node("A", vec![hierarchy_node("C", vec![])]),
            "index.md",
            HierarchyLayout::Nested,
            None,
        );
        manifest.record_hierarchy(
            &hierarchy_node("B", vec![hierarchy_node("C", vec![])]),
            "index.md",
            HierarchyLayout::Nested,
            None,
        );

        let mut paths = manifest.active_artifact_paths("C");
        paths.sort();
        assert_eq!(paths, vec!["A/C".to_string(), "B/C".to_string()]);
    }

    #[test]
    fn evict_marks_all_nested_artifact_paths_inactive() {
        let mut manifest = Manifest::default();
        manifest.record_hierarchy(
            &hierarchy_node("A", vec![hierarchy_node("C", vec![])]),
            "index.md",
            HierarchyLayout::Nested,
            None,
        );
        manifest.record_hierarchy(
            &hierarchy_node("B", vec![hierarchy_node("C", vec![])]),
            "index.md",
            HierarchyLayout::Nested,
            None,
        );

        manifest.evict("C", EvictionReason::NotReturnedByValidationSearch);

        assert!(manifest.active_artifact_paths("C").is_empty());
        assert!(manifest
            .get("C")
            .unwrap()
            .artifact_paths
            .iter()
            .all(|path| !path.active));
    }

    #[test]
    fn merge_on_write_preserves_unrelated_graph_state_added_after_load() {
        let dir = temp_dir("graph-merge");
        let path = default_manifest_path(&dir);

        let mut first = Manifest::default();
        first.record_hierarchy(
            &hierarchy_node("A", vec![hierarchy_node("C", vec![])]),
            "A.hierarchy.md",
            HierarchyLayout::Corpus,
            None,
        );
        first.save_to_path(&path).unwrap();

        let mut current_invocation = Manifest::load_from_path(&path).unwrap();
        let mut other_invocation = Manifest::load_from_path(&path).unwrap();
        other_invocation.record_hierarchy(
            &hierarchy_node("B", vec![hierarchy_node("D", vec![])]),
            "B.hierarchy.md",
            HierarchyLayout::Corpus,
            None,
        );
        other_invocation.save_to_path(&path).unwrap();

        current_invocation.record_hierarchy(
            &hierarchy_node("A", vec![hierarchy_node("E", vec![])]),
            "A.hierarchy.md",
            HierarchyLayout::Corpus,
            None,
        );
        current_invocation.save_to_path(&path).unwrap();

        let saved = Manifest::load_from_path(&path).unwrap();
        assert!(saved
            .edges
            .iter()
            .any(|edge| edge.parent == "B" && edge.child == "D" && edge.active));
        assert!(saved.root_snapshots.contains_key("B"));
        assert!(saved
            .edges
            .iter()
            .any(|edge| edge.parent == "A" && edge.child == "C" && !edge.active));
        assert!(saved
            .edges
            .iter()
            .any(|edge| edge.parent == "A" && edge.child == "E" && edge.active));
        assert_eq!(
            saved.get("C").unwrap().state,
            IssueCacheState::OrphanedHierarchyMember
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn external_manifest_merge_preserves_evictions_and_inactive_nested_paths() {
        let dir = temp_dir("external-graph-merge");
        let path = dir.join("cache").join("state.json");

        let mut first = Manifest::default();
        first.record_hierarchy(
            &hierarchy_node("A", vec![hierarchy_node("C", vec![])]),
            "index.md",
            HierarchyLayout::Nested,
            None,
        );
        first.record_hierarchy(
            &hierarchy_node("B", vec![hierarchy_node("C", vec![])]),
            "index.md",
            HierarchyLayout::Nested,
            None,
        );
        first.save_to_path(&path).unwrap();

        let mut current_invocation = Manifest::load_from_path(&path).unwrap();
        let mut other_invocation = Manifest::load_from_path(&path).unwrap();
        other_invocation.evict("C", EvictionReason::NotReturnedByValidationSearch);
        other_invocation.save_to_path(&path).unwrap();

        current_invocation.record_hierarchy(
            &hierarchy_node("D", vec![hierarchy_node("E", vec![])]),
            "D.hierarchy.md",
            HierarchyLayout::Corpus,
            None,
        );
        current_invocation.save_to_path(&path).unwrap();

        let saved = Manifest::load_from_path(&path).unwrap();
        let c = saved.get("C").unwrap();
        assert_eq!(c.state, IssueCacheState::Evicted);
        assert!(c.artifact_paths.iter().all(|path| !path.active));
        assert!(saved
            .edges
            .iter()
            .any(|edge| edge.parent == "D" && edge.child == "E" && edge.active));
        std::fs::remove_dir_all(&dir).ok();
    }

    fn hierarchy_node(key: &str, children: Vec<IssueNode>) -> IssueNode {
        IssueNode {
            key: key.to_string(),
            summary: format!("Issue {}", key),
            issue_type: "Task".to_string(),
            updated: "2026-01-01T00:00:00.000+0000".to_string(),
            children_discovered: true,
            truncated: false,
            truncated_by_depth: false,
            truncated_by_issue_count: false,
            failures: Vec::new(),
            children,
        }
    }
}
