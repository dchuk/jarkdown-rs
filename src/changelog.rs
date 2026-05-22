//! Render Jira changelog (audit trail of field changes) to Markdown.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::error::Result;
use crate::issue::ChangelogEntry;

/// Lightweight reference passed into the main markdown composer so the main
/// `{KEY}.md` file can cross-link to the sibling changelog file.
#[derive(Debug, Clone)]
pub struct ChangelogSummary {
    pub file_name: String,
    pub entry_count: usize,
}

/// Count the number of bullet rows that would be rendered (one per field
/// change across all entries). Useful for populating [`ChangelogSummary`].
pub fn row_count(entries: &[ChangelogEntry]) -> usize {
    entries.iter().map(|e| e.items.len()).sum()
}

/// Render the full `{KEY}.changelog.md` file body.
///
/// Returns YAML frontmatter (`key`, `summary`, `issue_file`, `entries`, `generated`),
/// a `# {KEY} Changelog` heading, and one bullet row per field change, oldest-first.
pub fn render_changelog_file(
    issue_key: &str,
    summary: &str,
    entries: &[ChangelogEntry],
    generated_at: DateTime<Utc>,
) -> String {
    let mut sorted: Vec<&ChangelogEntry> = entries.iter().collect();
    sorted.sort_by_key(|e| parse_created(e.created.as_str()));

    let row_count: usize = row_count(entries);

    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("key: {}\n", issue_key));
    out.push_str(&format!("summary: {}\n", yaml_scalar(summary)));
    out.push_str(&format!("issue_file: {}.md\n", issue_key));
    out.push_str(&format!("entries: {}\n", row_count));
    out.push_str(&format!(
        "generated: {}\n",
        generated_at.format("%Y-%m-%dT%H:%M:%SZ")
    ));
    out.push_str("---\n\n");
    out.push_str(&format!("# {} Changelog\n\n", issue_key));

    for entry in sorted {
        let timestamp = normalize_timestamp(entry.created.as_str());
        let author = entry.author.as_str();
        for item in entry.items.iter() {
            let field = item.field.as_str();
            let from = display_value(item.from_string.as_deref());
            let to = display_value(item.to_string.as_deref());
            out.push_str(&format!(
                "- {} — {} — **{}**: {} → {}\n",
                timestamp, author, field, from, to
            ));
        }
    }
    out
}

/// Render and write the on-disk changelog artifacts for an issue:
/// `{KEY}.changelog.md` and, when `include_json` is set, `{KEY}.changelog.json`.
///
/// This is the single owner of the changelog *write* sequence — both the
/// standard single-export path and the incremental-backfill path call it, so
/// the artifact shape and the JSON-write error handling can no longer drift.
/// Fetching the entries stays with the caller: the two paths have
/// intentionally different fetch-failure policies (a full export treats a
/// fetch failure as an empty changelog, whereas backfill skips entirely so a
/// transient failure does not write an empty file and suppress a later
/// backfill).
pub async fn write_artifacts(
    issue_key: &str,
    summary: &str,
    entries: &[ChangelogEntry],
    output_dir: &Path,
    include_json: bool,
) -> Result<ChangelogSummary> {
    let body = render_changelog_file(issue_key, summary, entries, Utc::now());
    let md_path = output_dir.join(format!("{}.changelog.md", issue_key));
    tokio::fs::write(&md_path, body).await?;

    if include_json {
        let json_path = output_dir.join(format!("{}.changelog.json", issue_key));
        // ADR-0002: serialize the retained raw payloads so the `.changelog.json`
        // artifact is byte-identical to what Jira returned. Serializing the
        // typed struct would drop auxiliary keys (e.g. `historyMetadata`,
        // `fieldId`, `fieldtype`, `from`, `to`) and reorder fields.
        let raws: Vec<&Value> = entries.iter().map(|e| &e.raw).collect();
        let json_str = serde_json::to_string_pretty(&raws)?;
        tokio::fs::write(&json_path, json_str).await?;
    }

    Ok(ChangelogSummary {
        file_name: format!("{}.changelog.md", issue_key),
        entry_count: row_count(entries),
    })
}

fn yaml_scalar(s: &str) -> String {
    if s.contains(':') || s.contains('#') || s.contains('"') || s.contains('\'') || s.contains('\n') {
        let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{}\"", escaped)
    } else {
        s.to_string()
    }
}

/// Map an `Option<&str>` field value (from `ChangelogItem::from_string` /
/// `to_string`) to the renderer's display form: `None` or empty string → `∅`,
/// otherwise the string itself.
fn display_value(s: Option<&str>) -> &str {
    match s {
        Some(v) if !v.is_empty() => v,
        _ => "∅",
    }
}

fn parse_created(raw: &str) -> DateTime<Utc> {
    DateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S%.3f%z")
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| DateTime::<Utc>::MIN_UTC)
}

fn normalize_timestamp(raw: &str) -> String {
    match DateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S%.3f%z") {
        Ok(dt) => dt
            .with_timezone(&Utc)
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string(),
        Err(_) => raw.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue::ChangelogItem;
    use serde_json::json;

    fn fixed_generated_at() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-05-20T19:32:17Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn renders_null_or_empty_field_strings_as_empty_set_symbol() {
        let entries = vec![
            ChangelogEntry {
                raw: json!({
                    "id": "1",
                    "author": { "displayName": "U" },
                    "created": "2024-01-01T00:00:00.000+0000",
                    "items": [
                        { "field": "assignee", "fromString": null, "toString": "Jane" }
                    ]
                }),
                id: "1".into(),
                author: "U".into(),
                created: "2024-01-01T00:00:00.000+0000".into(),
                items: vec![ChangelogItem {
                    field: "assignee".into(),
                    from_string: None,
                    to_string: Some("Jane".into()),
                }],
            },
            ChangelogEntry {
                raw: json!({
                    "id": "2",
                    "author": { "displayName": "U" },
                    "created": "2024-01-02T00:00:00.000+0000",
                    "items": [
                        { "field": "assignee", "fromString": "Jane", "toString": null }
                    ]
                }),
                id: "2".into(),
                author: "U".into(),
                created: "2024-01-02T00:00:00.000+0000".into(),
                items: vec![ChangelogItem {
                    field: "assignee".into(),
                    from_string: Some("Jane".into()),
                    to_string: None,
                }],
            },
            ChangelogEntry {
                raw: json!({
                    "id": "3",
                    "author": { "displayName": "U" },
                    "created": "2024-01-03T00:00:00.000+0000",
                    "items": [
                        { "field": "labels", "fromString": "", "toString": "" }
                    ]
                }),
                id: "3".into(),
                author: "U".into(),
                created: "2024-01-03T00:00:00.000+0000".into(),
                items: vec![ChangelogItem {
                    field: "labels".into(),
                    from_string: Some("".into()),
                    to_string: Some("".into()),
                }],
            },
        ];

        let output = render_changelog_file("K", "S", &entries, fixed_generated_at());

        assert!(
            output.contains("**assignee**: ∅ → Jane"),
            "null fromString should render as ∅; got:\n{}",
            output
        );
        assert!(
            output.contains("**assignee**: Jane → ∅"),
            "null toString should render as ∅; got:\n{}",
            output
        );
        assert!(
            output.contains("**labels**: ∅ → ∅"),
            "empty strings should render as ∅; got:\n{}",
            output
        );
    }

    #[test]
    fn sorts_entries_oldest_first_regardless_of_input_order() {
        let entries = vec![
            ChangelogEntry {
                raw: json!({
                    "id": "2",
                    "author": { "displayName": "Later User" },
                    "created": "2024-03-01T00:00:00.000+0000",
                    "items": [ { "field": "status", "fromString": "A", "toString": "B" } ]
                }),
                id: "2".into(),
                author: "Later User".into(),
                created: "2024-03-01T00:00:00.000+0000".into(),
                items: vec![ChangelogItem {
                    field: "status".into(),
                    from_string: Some("A".into()),
                    to_string: Some("B".into()),
                }],
            },
            ChangelogEntry {
                raw: json!({
                    "id": "1",
                    "author": { "displayName": "Earlier User" },
                    "created": "2024-01-01T00:00:00.000+0000",
                    "items": [ { "field": "status", "fromString": "X", "toString": "Y" } ]
                }),
                id: "1".into(),
                author: "Earlier User".into(),
                created: "2024-01-01T00:00:00.000+0000".into(),
                items: vec![ChangelogItem {
                    field: "status".into(),
                    from_string: Some("X".into()),
                    to_string: Some("Y".into()),
                }],
            },
        ];

        let output = render_changelog_file("K", "S", &entries, fixed_generated_at());
        let lines: Vec<&str> = output.lines().filter(|l| l.starts_with("- ")).collect();

        let earlier_pos = lines
            .iter()
            .position(|l| l.contains("Earlier User"))
            .expect("Earlier User row present");
        let later_pos = lines
            .iter()
            .position(|l| l.contains("Later User"))
            .expect("Later User row present");
        assert!(
            earlier_pos < later_pos,
            "Earlier should precede Later; lines={:?}",
            lines
        );
    }

    #[test]
    fn flattens_multi_item_entry_to_one_row_per_field_change() {
        let entries = vec![ChangelogEntry {
            raw: json!({
                "id": "2",
                "author": { "displayName": "Bob Jones" },
                "created": "2024-02-01T09:00:00.000+0000",
                "items": [
                    { "field": "status", "fromString": "To Do", "toString": "In Progress" },
                    { "field": "assignee", "fromString": null, "toString": "Bob Jones" }
                ]
            }),
            id: "2".into(),
            author: "Bob Jones".into(),
            created: "2024-02-01T09:00:00.000+0000".into(),
            items: vec![
                ChangelogItem {
                    field: "status".into(),
                    from_string: Some("To Do".into()),
                    to_string: Some("In Progress".into()),
                },
                ChangelogItem {
                    field: "assignee".into(),
                    from_string: None,
                    to_string: Some("Bob Jones".into()),
                },
            ],
        }];

        let output =
            render_changelog_file("PROJ-1", "Multi-field save", &entries, fixed_generated_at());

        let lines: Vec<&str> = output.lines().filter(|l| l.starts_with("- ")).collect();
        assert_eq!(
            lines.len(),
            2,
            "expected 2 rows from a 2-item entry; got:\n{}",
            output
        );
        assert!(lines[0]
            .contains("2024-02-01T09:00:00Z — Bob Jones — **status**: To Do → In Progress"));
        assert!(lines[1].contains("2024-02-01T09:00:00Z — Bob Jones — **assignee**:"));
    }

    #[test]
    fn renders_single_field_change_as_compact_bullet_line() {
        let entries = vec![ChangelogEntry {
            raw: json!({
                "id": "1",
                "author": { "displayName": "Jane Smith" },
                "created": "2024-01-20T14:32:17.000+0000",
                "items": [
                    { "field": "status", "fromString": "To Do", "toString": "In Progress" }
                ]
            }),
            id: "1".into(),
            author: "Jane Smith".into(),
            created: "2024-01-20T14:32:17.000+0000".into(),
            items: vec![ChangelogItem {
                field: "status".into(),
                from_string: Some("To Do".into()),
                to_string: Some("In Progress".into()),
            }],
        }];

        let output =
            render_changelog_file("PROJ-123", "Implement auth", &entries, fixed_generated_at());

        assert!(
            output.contains("- 2024-01-20T14:32:17Z — Jane Smith — **status**: To Do → In Progress"),
            "missing expected bullet line; got:\n{}",
            output
        );
    }
}
