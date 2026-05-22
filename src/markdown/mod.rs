//! Pure-function markdown composer for one Jira issue.
//!
//! Public surface:
//!
//! * [`RenderContext`] — borrows everything the composer needs.
//! * [`compose`] — single entry-point pure function: `compose(&ctx) -> String`.
//! * [`AttachmentIndex`] — built once at the [`crate::export`] seam and held
//!   inside [`RenderContext`].
//! * [`CustomFieldMetadata`] — pre-resolved `customfield_*` names and schemas;
//!   the render path never touches the underlying [`crate::field_cache::FieldMetadataCache`].
//!
//! The composer is *pure*: it takes no `&mut`, performs no I/O, and never
//! mutates its inputs. All side-effectful work (HTTP, field-cache writes,
//! filesystem) happens at [`crate::export`] before [`RenderContext`] is built.

use std::collections::HashMap;

use serde_json::Value;

use crate::attachment::DownloadedAttachment;
use crate::changelog::ChangelogSummary;
use crate::config::FieldFilter;
use crate::issue::Issue;

pub mod adf;
pub mod attachments;
pub mod html;
pub mod sections;

pub use attachments::AttachmentIndex;

/// Pre-resolved `customfield_*` display names and schemas for one render.
///
/// Built once in [`crate::export`] (where `&mut FieldMetadataCache` is
/// legal) and borrowed by [`RenderContext`] thereafter, so the markdown
/// layer never needs mutable access to the field cache.
#[derive(Debug, Clone, Default)]
pub struct CustomFieldMetadata {
    /// `customfield_NNNNN` → display name.
    pub names: HashMap<String, String>,
    /// `customfield_NNNNN` → schema `Value` (or `Value::Null` when absent).
    pub schemas: HashMap<String, Value>,
}

impl CustomFieldMetadata {
    pub fn empty() -> Self {
        Self {
            names: HashMap::new(),
            schemas: HashMap::new(),
        }
    }
}

/// Everything [`compose`] needs to render one issue's `{KEY}.md` body.
///
/// Plain `pub` fields so callers literal-construct it at the export seam.
/// All references are borrowed — `RenderContext` owns nothing.
pub struct RenderContext<'a> {
    pub issue: &'a Issue,
    pub downloaded: &'a [DownloadedAttachment],
    pub skipped_attachments: &'a [Value],
    pub attachments: AttachmentIndex<'a>,
    pub field_metadata: &'a CustomFieldMetadata,
    pub field_filter: &'a FieldFilter,
    pub child_issues: &'a [Value],
    pub changelog_summary: Option<&'a ChangelogSummary>,
    pub base_url: &'a str,
    pub domain: &'a str,
}

/// Compose the final markdown file body for one issue.
///
/// Section ordering is load-bearing for `--strict-md` byte-identity against
/// the baseline binary. Each section function returns its lines (including
/// trailing blanks) and the final result is `lines.join("\n")`.
pub fn compose(ctx: &RenderContext<'_>) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.extend(sections::frontmatter(ctx));
    lines.extend(sections::title(ctx));
    lines.extend(sections::description(ctx));
    lines.extend(sections::environment(ctx));
    lines.extend(sections::linked_issues(ctx));
    lines.extend(sections::subtasks(ctx));
    lines.extend(sections::child_issues(ctx));
    lines.extend(sections::worklogs(ctx));
    lines.extend(sections::custom_fields(ctx));
    lines.extend(sections::comments(ctx));
    lines.extend(sections::changelog(ctx));
    lines.extend(sections::attachments(ctx));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue::{IssueType, RichText, Status};
    use serde_json::json;

    /// End-to-end regression: when an issue has *skipped* attachments (e.g.
    /// `--no-attachments`) the Attachments section still references each one
    /// by source URL, and inline `(attachment)` placeholders never leak.
    ///
    /// Inherited from the pre-split `markdown.rs` test; here we drive
    /// [`compose`] directly via a literal [`RenderContext`].
    #[test]
    fn skipped_attachments_render_names_and_source_urls_without_local_placeholders() {
        let issue_data = json!({
            "key": "K1",
            "renderedFields": {},
            "fields": {
                "summary": "Attachment issue",
                "description": {
                    "type": "doc",
                    "content": [
                        {
                            "type": "mediaSingle",
                            "content": [
                                {
                                    "type": "media",
                                    "attrs": {
                                        "id": "10001",
                                        "type": "file",
                                        "alt": "diagram.png"
                                    }
                                }
                            ]
                        }
                    ]
                },
                "issuetype": { "name": "Task" },
                "status": { "name": "Open", "statusCategory": { "name": "To Do" } },
                "priority": { "name": "Medium" },
                "resolution": null,
                "project": { "name": "Project", "key": "PROJ" },
                "assignee": null,
                "reporter": null,
                "creator": null,
                "labels": [],
                "components": [],
                "parent": null,
                "subtasks": [],
                "issuelinks": [],
                "worklog": { "worklogs": [] },
                "comment": { "comments": [] },
                "attachment": [
                    {
                        "id": "10001",
                        "filename": "diagram.png",
                        "content": "https://example.atlassian.net/rest/api/3/attachment/content/10001",
                        "mimeType": "image/png",
                        "size": 1234
                    }
                ]
            }
        });
        let skipped = issue_data["fields"]["attachment"]
            .as_array()
            .unwrap()
            .clone();

        let issue = Issue {
            raw: issue_data,
            key: "K1".to_string(),
            summary: "Attachment issue".to_string(),
            updated: String::new(),
            issuetype: IssueType {
                name: "Task".to_string(),
            },
            status: Status {
                name: "Open".to_string(),
                category: Some("To Do".to_string()),
            },
            assignee: None,
            description: RichText::Empty,
            comments: Vec::new(),
            attachments: skipped.clone(),
            issuelinks: Vec::new(),
            parent: None,
            subtasks: Vec::new(),
        };
        let field_metadata = CustomFieldMetadata::empty();
        let field_filter = FieldFilter::default();
        let ctx = RenderContext {
            issue: &issue,
            downloaded: &[],
            skipped_attachments: &skipped,
            attachments: AttachmentIndex::build(&[], &skipped),
            field_metadata: &field_metadata,
            field_filter: &field_filter,
            child_issues: &[],
            changelog_summary: None,
            base_url: "https://example.atlassian.net",
            domain: "example.atlassian.net",
        };

        let markdown = compose(&ctx);

        assert!(markdown.contains("## Attachments"));
        assert!(markdown.contains(
            "- [diagram.png](https://example.atlassian.net/rest/api/3/attachment/content/10001)"
        ));
        assert!(markdown.contains(
            "[diagram.png](https://example.atlassian.net/rest/api/3/attachment/content/10001)"
        ));
        assert!(!markdown.contains("(attachment)"));
        assert!(!markdown.contains("](diagram.png)"));
    }
}
