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
    if manifest.is_stale(&issue.key, &issue.updated) {
        return ExportPlan::Full;
    }
    if include_changelog {
        let changelog = issue_dir.join(format!("{}.changelog.md", issue.key));
        if !changelog.exists() {
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
        std::fs::write(dir.join("K1.changelog.md"), "x").unwrap();
        let mut m = Manifest::default();
        m.record("K1", TS);
        assert_eq!(plan(&issue("K1", TS), &m, true, &dir), ExportPlan::Skip);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unchanged_with_changelog_missing_and_flag_is_backfill() {
        let dir = temp_dir("backfill");
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
        let mut m = Manifest::default();
        m.record("K1", TS);
        assert_eq!(plan(&issue("K1", TS), &m, false, &dir), ExportPlan::Skip);
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
