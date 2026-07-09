//! Internal deterministic planning seams for hierarchy incremental exports.
//!
//! These functions take already-loaded manifest state, validation metadata, CLI
//! freshness options, and artifact locations. They do not fetch from Jira,
//! mutate the manifest, write files, or perform exports.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::freshness::{self, ExportPlan, PlanOptions};
use crate::jira_client::ValidationIssue;
use crate::manifest::{normalize_issue_key, sanitize_artifact_path, Manifest};

/// Deterministic input for choosing the Jira keys to validate for a hierarchy
/// invocation.
pub struct HierarchyValidationKeysInput<'a> {
    /// Current roots requested by this CLI invocation.
    pub requested_roots: &'a [String],
    /// Loaded manifest whose active hierarchy graph supplies cached descendants.
    pub manifest: &'a Manifest,
}

/// Return current requested roots plus their active cached descendants, in
/// request order with normalized duplicates removed.
pub fn hierarchy_validation_keys(input: HierarchyValidationKeysInput<'_>) -> Vec<String> {
    let mut keys = Vec::new();
    for root in input.requested_roots {
        push_normalized_unique(&mut keys, root);
        for key in input.manifest.active_hierarchy_keys(root) {
            push_normalized_unique(&mut keys, &key);
        }
    }
    keys
}

/// Deterministic input for planning warm hierarchy reuse or descendant refresh.
pub struct WarmHierarchyPlanInput<'a> {
    /// Root key requested by this export.
    pub root_key: &'a str,
    /// Output directory used to test expected artifact paths.
    pub output_dir: &'a Path,
    /// Loaded manifest containing the cached hierarchy graph and metadata.
    pub manifest: &'a Manifest,
    /// Validation metadata keyed by normalized issue key.
    pub validation: &'a HashMap<String, ValidationIssue>,
    /// Freshness options shared with flat incremental export planning.
    pub options: PlanOptions<'a>,
}

/// A planned descendant refresh produced without performing side effects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HierarchyRefreshPlan {
    pub key: String,
    pub artifact_paths: Vec<String>,
    pub plan: ExportPlan,
}

/// Deterministic warm hierarchy decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WarmHierarchyPlan {
    /// No safe warm path exists; run the full hierarchy export.
    FullExport,
    /// Missing validation keys should be evicted, then the current cached tree
    /// can be returned.
    UseCached {
        missing_keys: Vec<String>,
        validated_keys: Vec<String>,
    },
    /// Missing validation keys should be evicted and the listed descendants
    /// should be refreshed before returning the cached tree.
    RefreshDescendants {
        missing_keys: Vec<String>,
        validated_keys: Vec<String>,
        refresh_plans: Vec<HierarchyRefreshPlan>,
    },
}

/// Plan a warm incremental hierarchy export for corpus layout.
pub fn plan_warm_corpus_hierarchy(input: WarmHierarchyPlanInput<'_>) -> WarmHierarchyPlan {
    if input
        .manifest
        .cached_hierarchy_tree(input.root_key)
        .is_none()
    {
        return WarmHierarchyPlan::FullExport;
    }
    let keys = input.manifest.active_hierarchy_keys(input.root_key);
    if keys.is_empty() {
        return WarmHierarchyPlan::FullExport;
    }
    let (missing_keys, validated_keys) = partition_validation_keys(keys, input.validation);
    if validated_keys.is_empty() {
        return WarmHierarchyPlan::UseCached {
            missing_keys,
            validated_keys,
        };
    }

    let root_key = normalize_issue_key(input.root_key);
    let mut refresh_plans = Vec::new();
    for key in &validated_keys {
        let Some(issue) = input.validation.get(key) else {
            return WarmHierarchyPlan::FullExport;
        };
        let plan = freshness::plan_metadata(
            key,
            &issue.updated,
            input.manifest,
            input.options,
            &input.output_dir.join(key),
        );
        match plan {
            ExportPlan::Skip => {}
            ExportPlan::Full | ExportPlan::BackfillChangelogOnly
                if normalize_issue_key(key) == root_key =>
            {
                return WarmHierarchyPlan::FullExport;
            }
            ExportPlan::Full | ExportPlan::BackfillChangelogOnly => {
                refresh_plans.push(HierarchyRefreshPlan {
                    key: key.clone(),
                    artifact_paths: vec![key.clone()],
                    plan,
                });
            }
        }
    }

    if refresh_plans.is_empty() {
        WarmHierarchyPlan::UseCached {
            missing_keys,
            validated_keys,
        }
    } else {
        WarmHierarchyPlan::RefreshDescendants {
            missing_keys,
            validated_keys,
            refresh_plans,
        }
    }
}

/// Plan a warm incremental hierarchy export for nested layout.
pub fn plan_warm_nested_hierarchy(input: WarmHierarchyPlanInput<'_>) -> WarmHierarchyPlan {
    if input
        .manifest
        .cached_hierarchy_tree(input.root_key)
        .is_none()
    {
        return WarmHierarchyPlan::FullExport;
    }
    let keys = input.manifest.active_hierarchy_keys(input.root_key);
    if keys.is_empty() {
        return WarmHierarchyPlan::FullExport;
    }
    let (missing_keys, validated_keys) = partition_validation_keys(keys, input.validation);
    if validated_keys.is_empty() {
        return WarmHierarchyPlan::UseCached {
            missing_keys,
            validated_keys,
        };
    }

    let root_key = normalize_issue_key(input.root_key);
    let mut refresh_plans = Vec::new();
    for key in &validated_keys {
        let Some(issue) = input.validation.get(key) else {
            return WarmHierarchyPlan::FullExport;
        };
        let paths = input.manifest.active_artifact_paths(key);
        if paths.is_empty() {
            return WarmHierarchyPlan::FullExport;
        }
        let mut planned_paths = paths
            .iter()
            .filter_map(|path| artifact_output_path(input.output_dir, path))
            .map(|path| {
                freshness::plan_metadata(key, &issue.updated, input.manifest, input.options, &path)
            })
            .collect::<Vec<_>>();
        if planned_paths.len() != paths.len() {
            return WarmHierarchyPlan::FullExport;
        }
        if planned_paths.iter().any(|plan| *plan != ExportPlan::Skip) {
            if normalize_issue_key(key) == root_key {
                return WarmHierarchyPlan::FullExport;
            }
            let plan = planned_paths
                .drain(..)
                .find(|plan| *plan == ExportPlan::Full)
                .unwrap_or(ExportPlan::BackfillChangelogOnly);
            refresh_plans.push(HierarchyRefreshPlan {
                key: key.clone(),
                artifact_paths: paths,
                plan,
            });
        }
    }

    if refresh_plans.is_empty() {
        WarmHierarchyPlan::UseCached {
            missing_keys,
            validated_keys,
        }
    } else {
        WarmHierarchyPlan::RefreshDescendants {
            missing_keys,
            validated_keys,
            refresh_plans,
        }
    }
}

fn partition_validation_keys(
    keys: Vec<String>,
    validation: &HashMap<String, ValidationIssue>,
) -> (Vec<String>, Vec<String>) {
    keys.into_iter()
        .partition(|key| !validation.contains_key(key))
}

fn push_normalized_unique(keys: &mut Vec<String>, key: &str) {
    let key = normalize_issue_key(key);
    if !keys.contains(&key) {
        keys.push(key);
    }
}

fn artifact_output_path(output_dir: &Path, artifact_path: &str) -> Option<PathBuf> {
    sanitize_artifact_path(artifact_path).map(|path| output_dir.join(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hierarchy::{HierarchyLayout, IssueNode};
    use std::time::{SystemTime, UNIX_EPOCH};

    const TS: &str = "2026-01-01T00:00:00.000+0000";
    const NEWER_TS: &str = "2026-01-02T00:00:00.000+0000";

    #[test]
    fn hierarchy_validation_keys_deduplicate_shared_cached_descendants() {
        let mut manifest = Manifest::default();
        manifest.record_hierarchy(
            &node("A", vec![node("C", vec![])]),
            "A.hierarchy.md",
            HierarchyLayout::Corpus,
            None,
        );
        manifest.record_hierarchy(
            &node("B", vec![node("C", vec![])]),
            "B.hierarchy.md",
            HierarchyLayout::Corpus,
            None,
        );

        let keys = hierarchy_validation_keys(HierarchyValidationKeysInput {
            requested_roots: &["a".to_string(), "b".to_string()],
            manifest: &manifest,
        });

        assert_eq!(keys, vec!["A", "C", "B"]);
    }

    #[test]
    fn hierarchy_validation_keys_scope_to_current_roots() {
        let mut manifest = Manifest::default();
        manifest.record_hierarchy(
            &node("A", vec![node("C", vec![])]),
            "A.hierarchy.md",
            HierarchyLayout::Corpus,
            None,
        );
        manifest.record_hierarchy(
            &node("B", vec![node("D", vec![])]),
            "B.hierarchy.md",
            HierarchyLayout::Corpus,
            None,
        );

        let keys = hierarchy_validation_keys(HierarchyValidationKeysInput {
            requested_roots: &["A".to_string()],
            manifest: &manifest,
        });

        assert_eq!(keys, vec!["A", "C"]);
    }

    #[test]
    fn warm_corpus_all_skip_returns_cached_plan() {
        let output_dir = temp_dir("corpus-skip");
        write_issue_artifacts(&output_dir.join("A"), "A", false);
        write_issue_artifacts(&output_dir.join("C"), "C", false);

        let mut manifest = Manifest::default();
        manifest.record_hierarchy(
            &node("A", vec![node("C", vec![])]),
            "A.hierarchy.md",
            HierarchyLayout::Corpus,
            None,
        );
        manifest.record_metadata_with_fingerprint("A", TS, None, None, None, "A", None);
        manifest.record_metadata_with_fingerprint("C", TS, None, None, None, "C", None);

        let plan = plan_warm_corpus_hierarchy(WarmHierarchyPlanInput {
            root_key: "A",
            output_dir: &output_dir,
            manifest: &manifest,
            validation: &validation(&[("A", TS), ("C", TS)]),
            options: PlanOptions::default(),
        });

        assert_eq!(
            plan,
            WarmHierarchyPlan::UseCached {
                missing_keys: Vec::new(),
                validated_keys: vec!["A".to_string(), "C".to_string()],
            }
        );
    }

    #[test]
    fn warm_nested_changed_shared_issue_refreshes_each_active_path() {
        let output_dir = temp_dir("nested-shared");
        write_issue_artifacts(&output_dir.join("A"), "A", false);
        write_issue_artifacts(&output_dir.join("A").join("C"), "C", false);
        write_issue_artifacts(&output_dir.join("B"), "B", false);
        write_issue_artifacts(&output_dir.join("B").join("C"), "C", false);

        let mut manifest = Manifest::default();
        manifest.record_hierarchy(
            &node("A", vec![node("C", vec![])]),
            "A.hierarchy.md",
            HierarchyLayout::Nested,
            None,
        );
        manifest.record_hierarchy(
            &node("B", vec![node("C", vec![])]),
            "B.hierarchy.md",
            HierarchyLayout::Nested,
            None,
        );
        manifest.record_metadata_with_fingerprint("A", TS, None, None, None, "A", None);
        manifest.record_metadata_with_fingerprint("B", TS, None, None, None, "B", None);
        manifest.record_metadata_with_fingerprint("C", TS, None, None, None, "A/C", None);
        manifest.record_metadata_with_fingerprint("C", TS, None, None, None, "B/C", None);

        let plan = plan_warm_nested_hierarchy(WarmHierarchyPlanInput {
            root_key: "A",
            output_dir: &output_dir,
            manifest: &manifest,
            validation: &validation(&[("A", TS), ("C", NEWER_TS)]),
            options: PlanOptions::default(),
        });

        assert_eq!(
            plan,
            WarmHierarchyPlan::RefreshDescendants {
                missing_keys: Vec::new(),
                validated_keys: vec!["A".to_string(), "C".to_string()],
                refresh_plans: vec![HierarchyRefreshPlan {
                    key: "C".to_string(),
                    artifact_paths: vec!["A/C".to_string(), "B/C".to_string()],
                    plan: ExportPlan::Full,
                }],
            }
        );
    }

    fn validation(entries: &[(&str, &str)]) -> HashMap<String, ValidationIssue> {
        entries
            .iter()
            .map(|(key, updated)| {
                (
                    normalize_issue_key(key),
                    ValidationIssue {
                        key: key.to_string(),
                        updated: updated.to_string(),
                        summary: None,
                        issue_type: None,
                        status: None,
                        archived: None,
                    },
                )
            })
            .collect()
    }

    fn node(key: &str, children: Vec<IssueNode>) -> IssueNode {
        IssueNode {
            key: key.to_string(),
            summary: key.to_string(),
            issue_type: "Task".to_string(),
            updated: TS.to_string(),
            children_discovered: true,
            children,
            truncated: false,
            truncated_by_depth: false,
            truncated_by_issue_count: false,
            failures: Vec::new(),
        }
    }

    fn write_issue_artifacts(dir: &Path, key: &str, include_json: bool) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(format!("{}.md", key)), "# issue").unwrap();
        if include_json {
            std::fs::write(dir.join(format!("{}.json", key)), "{}").unwrap();
        }
    }

    fn temp_dir(label: &str) -> PathBuf {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("jarkdown-planner-{}-{}", label, n));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
