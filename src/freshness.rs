//! The incremental-export freshness decision.
//!
//! ADR-0001 Decision 2 codifies one rule: skip re-exporting an issue whose
//! `updated` timestamp has not moved since the last export — unless
//! `{KEY}.changelog.md` is missing while `--include-changelog` is on, in which
//! case backfill only the changelog (without re-fetching the issue payload).
//!
//! This module is the sole implementation of that decision. The single-export
//! CLI handler and the bulk per-issue loop consult [`plan`] and act on the
//! returned [`ExportPlan`] instead of each re-deriving the rule.

use std::path::Path;

use crate::issue::Issue;
use crate::manifest::Manifest;

/// What an incremental export should do for a single issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportPlan {
    /// Unchanged since the last export and all artifacts present — do nothing.
    Skip,
    /// Unchanged, but `{KEY}.changelog.md` is missing while `--include-changelog`
    /// is on — fetch and write only the changelog.
    BackfillChangelogOnly,
    /// New or changed — perform the full export.
    Full,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PlanOptions<'a> {
    pub include_changelog: bool,
    pub include_json: bool,
    pub option_fingerprint: Option<&'a str>,
    pub option_fingerprint_without_changelog: Option<&'a str>,
    /// JPD archived state observed this run (`None` = not observed). ADR-0005:
    /// drift against the manifest forces a full export even when `updated`
    /// has not moved, so the frontmatter marker never goes stale.
    pub archived: Option<bool>,
}

/// Decide what an incremental export should do for `issue`.
///
/// `issue_dir` is the per-issue output directory — where `{KEY}.changelog.md`
/// would live. See ADR-0001 Decision 2.
pub fn plan(
    issue: &Issue,
    manifest: &Manifest,
    include_changelog: bool,
    issue_dir: &Path,
) -> ExportPlan {
    plan_metadata(
        &issue.key,
        &issue.updated,
        manifest,
        PlanOptions {
            include_changelog,
            include_json: false,
            option_fingerprint: None,
            option_fingerprint_without_changelog: None,
            archived: None,
        },
        issue_dir,
    )
}

/// Decide what an incremental export should do from validation metadata.
pub fn plan_metadata(
    issue_key: &str,
    updated: &str,
    manifest: &Manifest,
    options: PlanOptions<'_>,
    issue_dir: &Path,
) -> ExportPlan {
    let entry = manifest.get(issue_key);
    if manifest.is_stale(issue_key, updated) {
        return ExportPlan::Full;
    }
    // ADR-0005 belt-and-braces: archived-state drift forces a full export
    // regardless of `updated`. A stored `None` counts as "not archived" so
    // pre-feature manifest entries don't churn when `Some(false)` is observed.
    if let Some(observed) = options.archived {
        let stored = entry.and_then(|entry| entry.archived).unwrap_or(false);
        if observed != stored {
            return ExportPlan::Full;
        }
    }
    let stored_fingerprint = entry.and_then(|entry| entry.option_fingerprint.as_deref());
    if stored_fingerprint != options.option_fingerprint {
        let changelog_only_delta = options.include_changelog
            && stored_fingerprint == options.option_fingerprint_without_changelog;
        if !changelog_only_delta {
            return ExportPlan::Full;
        }
    }

    let main_markdown = issue_dir.join(format!("{}.md", issue_key));
    if !main_markdown.exists() {
        return ExportPlan::Full;
    }
    if options.include_json {
        let json = issue_dir.join(format!("{}.json", issue_key));
        if !json.exists() {
            return ExportPlan::Full;
        }
    }
    if options.include_changelog {
        let changelog = issue_dir.join(format!("{}.changelog.md", issue_key));
        let changelog_json = issue_dir.join(format!("{}.changelog.json", issue_key));
        if !changelog.exists() || (options.include_json && !changelog_json.exists()) {
            return ExportPlan::BackfillChangelogOnly;
        }
    }
    ExportPlan::Skip
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn issue(key: &str, updated: &str) -> Issue {
        Issue::from_value(json!({
            "key": key,
            "fields": { "summary": "S", "updated": updated }
        }))
        .expect("issue")
    }

    fn temp_dir(label: &str) -> std::path::PathBuf {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let d = std::env::temp_dir().join(format!("jarkdown-freshness-{}-{}", label, n));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    const TS: &str = "2026-01-01T00:00:00.000+0000";

    #[test]
    fn unchanged_with_changelog_present_is_skip() {
        let dir = temp_dir("skip");
        std::fs::write(dir.join("K1.md"), "x").unwrap();
        std::fs::write(dir.join("K1.changelog.md"), "x").unwrap();
        let mut m = Manifest::default();
        m.record("K1", TS);
        assert_eq!(plan(&issue("K1", TS), &m, true, &dir), ExportPlan::Skip);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unchanged_with_changelog_missing_and_flag_is_backfill() {
        let dir = temp_dir("backfill");
        std::fs::write(dir.join("K1.md"), "x").unwrap();
        let mut m = Manifest::default();
        m.record("K1", TS);
        assert_eq!(
            plan(&issue("K1", TS), &m, true, &dir),
            ExportPlan::BackfillChangelogOnly
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unchanged_with_changelog_missing_but_flag_off_is_skip() {
        let dir = temp_dir("flagoff");
        std::fs::write(dir.join("K1.md"), "x").unwrap();
        let mut m = Manifest::default();
        m.record("K1", TS);
        assert_eq!(plan(&issue("K1", TS), &m, false, &dir), ExportPlan::Skip);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_attachment_binaries_alone_are_not_freshness_inputs() {
        let dir = temp_dir("attachments-ignored");
        std::fs::write(dir.join("K1.md"), "x").unwrap();
        let mut m = Manifest::default();
        m.record("K1", TS);
        assert_eq!(plan(&issue("K1", TS), &m, false, &dir), ExportPlan::Skip);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unchanged_with_missing_main_markdown_is_full() {
        let dir = temp_dir("missing-main");
        let mut m = Manifest::default();
        m.record("K1", TS);
        assert_eq!(plan(&issue("K1", TS), &m, false, &dir), ExportPlan::Full);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unchanged_with_missing_json_when_requested_is_full() {
        let dir = temp_dir("missing-json");
        std::fs::write(dir.join("K1.md"), "x").unwrap();
        let mut m = Manifest::default();
        m.record("K1", TS);
        assert_eq!(
            plan_metadata(
                "K1",
                TS,
                &m,
                PlanOptions {
                    include_json: true,
                    ..PlanOptions::default()
                },
                &dir
            ),
            ExportPlan::Full
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Belt-and-braces from ADR-0005: an Idea archived (or restored) since the
    /// last export is re-exported even when `updated` has not moved, so the
    /// frontmatter marker never goes stale. A stored `None` (pre-feature
    /// manifest) counts as "not archived" — observing `Some(false)` must NOT
    /// churn every old entry into a full export.
    #[test]
    fn archived_state_drift_is_full_even_when_updated_is_unchanged() {
        let dir = temp_dir("archived-drift");
        std::fs::write(dir.join("K1.md"), "x").unwrap();
        let mut m = Manifest::default();
        m.record("K1", TS);

        let plan_with = |m: &Manifest, archived: Option<bool>| {
            plan_metadata(
                "K1",
                TS,
                m,
                PlanOptions {
                    archived,
                    ..PlanOptions::default()
                },
                &dir,
            )
        };

        // Stored: not archived (None). Observed archived → Full.
        assert_eq!(plan_with(&m, Some(true)), ExportPlan::Full);
        // Observed live / unobserved → no churn.
        assert_eq!(plan_with(&m, Some(false)), ExportPlan::Skip);
        assert_eq!(plan_with(&m, None), ExportPlan::Skip);

        // Stored: archived. Observed live (restored) → Full; still archived → Skip.
        m.set_archived("K1", Some(true));
        assert_eq!(plan_with(&m, Some(false)), ExportPlan::Full);
        assert_eq!(plan_with(&m, Some(true)), ExportPlan::Skip);
        assert_eq!(plan_with(&m, None), ExportPlan::Skip);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn changed_option_fingerprint_is_full() {
        let dir = temp_dir("fingerprint");
        std::fs::write(dir.join("K1.md"), "x").unwrap();
        let mut m = Manifest::default();
        m.record_metadata_with_fingerprint(
            "K1",
            TS,
            Some("S"),
            Some("Task"),
            Some("Open"),
            "K1",
            Some("include_fields=a"),
        );
        assert_eq!(
            plan_metadata(
                "K1",
                TS,
                &m,
                PlanOptions {
                    option_fingerprint: Some("include_fields=b"),
                    ..PlanOptions::default()
                },
                &dir
            ),
            ExportPlan::Full
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn changed_issue_is_full() {
        let dir = temp_dir("changed");
        let mut m = Manifest::default();
        m.record("K1", TS);
        assert_eq!(
            plan(&issue("K1", "2026-06-01T00:00:00.000+0000"), &m, true, &dir),
            ExportPlan::Full
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn first_export_is_full() {
        let dir = temp_dir("first");
        let m = Manifest::default();
        assert_eq!(plan(&issue("K1", TS), &m, true, &dir), ExportPlan::Full);
        std::fs::remove_dir_all(&dir).ok();
    }
}
