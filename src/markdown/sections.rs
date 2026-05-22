//! Per-section composers for the main `{KEY}.md` artifact.
//!
//! Each `pub(crate) fn` returns the `Vec<String>` of lines for its section in
//! the order it will appear in the final file. [`crate::markdown::compose`]
//! concatenates them with `\n`. Every section function is pure: it borrows
//! [`crate::markdown::RenderContext`] and does not mutate anything.

use std::collections::HashMap;

use serde_json::Value;
use urlencoding::encode as url_encode;

use crate::attachment::DownloadedAttachment;
use crate::custom_field::CustomFieldRenderer;
use crate::issue::{Issue, RichText};
use crate::markdown::adf::{adf_to_plain_text, capitalize, parse_adf_to_markdown};
use crate::markdown::attachments::replace_attachment_links;
use crate::markdown::html::convert_html_to_markdown;
use crate::markdown::RenderContext;

pub(crate) fn frontmatter(ctx: &RenderContext<'_>) -> Vec<String> {
    let metadata = generate_metadata(ctx.issue);
    let yaml_str = serde_yaml::to_string(&metadata).unwrap_or_default();

    let mut lines = Vec::new();
    lines.push("---".into());
    lines.push(yaml_str.trim_end().to_string());
    if let Some(cl) = ctx.changelog_summary {
        lines.push(format!("changelog: {}", cl.file_name));
    }
    lines.push("---".into());
    lines.push(String::new());
    lines
}

pub(crate) fn title(ctx: &RenderContext<'_>) -> Vec<String> {
    let key = if ctx.issue.key.is_empty() {
        "UNKNOWN"
    } else {
        ctx.issue.key.as_str()
    };
    let summary = if ctx.issue.summary.is_empty() {
        "No Summary"
    } else {
        ctx.issue.summary.as_str()
    };
    vec![
        format!("# [{}]({}/browse/{}): {}", key, ctx.base_url, key, summary),
        String::new(),
    ]
}

pub(crate) fn description(ctx: &RenderContext<'_>) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push("## Description".into());
    lines.push(String::new());

    match &ctx.issue.description {
        RichText::Html(html) => {
            let md = convert_html_to_markdown(html);
            let md = replace_attachment_links(&md, ctx.downloaded, ctx.domain);
            lines.push(md);
        }
        RichText::Adf(adf) => {
            let md = parse_adf_to_markdown(adf, &ctx.attachments);
            let md = replace_attachment_links(&md, ctx.downloaded, ctx.domain);
            lines.push(md);
        }
        RichText::Plain(s) => {
            let md = replace_attachment_links(s, ctx.downloaded, ctx.domain);
            lines.push(md);
        }
        RichText::Empty => lines.push("*No description provided*".into()),
    }
    lines.push(String::new());
    lines
}

pub(crate) fn environment(ctx: &RenderContext<'_>) -> Vec<String> {
    let mut lines = vec!["## Environment".into(), String::new()];
    match &ctx.issue.environment {
        RichText::Html(html) => lines.push(convert_html_to_markdown(html)),
        RichText::Adf(adf) => lines.push(parse_adf_to_markdown(adf, &ctx.attachments)),
        RichText::Plain(s) => lines.push(s.clone()),
        RichText::Empty => lines.push("None".into()),
    }
    lines.push(String::new());
    lines
}

pub(crate) fn linked_issues(ctx: &RenderContext<'_>) -> Vec<String> {
    let mut lines = vec!["## Linked Issues".into(), String::new()];
    let links = &ctx.issue.issuelinks;

    if links.is_empty() {
        lines.push("None".into());
        lines.push(String::new());
        return lines;
    }

    // First-occurrence order of link-type labels keeps the rendered
    // sections deterministic (a HashMap here shuffled them per run).
    let mut groups: Vec<(String, Vec<&Value>)> = Vec::new();
    for link in links {
        let link_type = &link["type"];
        let (label, issue) =
            if link.get("outwardIssue").is_some() && !link["outwardIssue"].is_null() {
                let l = link_type["outward"].as_str().unwrap_or("Related");
                (capitalize(l), &link["outwardIssue"])
            } else if link.get("inwardIssue").is_some() && !link["inwardIssue"].is_null() {
                let l = link_type["inward"].as_str().unwrap_or("Related");
                (capitalize(l), &link["inwardIssue"])
            } else {
                continue;
            };
        if let Some(entry) = groups.iter_mut().find(|(l, _)| l == &label) {
            entry.1.push(issue);
        } else {
            groups.push((label, vec![issue]));
        }
    }

    for (label, issues) in &groups {
        lines.push(format!("### {}", label));
        lines.push(String::new());
        for issue in issues {
            let key = issue["key"].as_str().unwrap_or("UNKNOWN");
            let summary = issue["fields"]["summary"].as_str().unwrap_or("");
            let status = issue["fields"]["status"]["name"].as_str().unwrap_or("");
            lines.push(format!(
                "- [{}]({}/browse/{}): {} ({})",
                key, ctx.base_url, key, summary, status
            ));
        }
        lines.push(String::new());
    }
    lines
}

pub(crate) fn subtasks(ctx: &RenderContext<'_>) -> Vec<String> {
    let mut lines = vec!["## Subtasks".into(), String::new()];
    let subtasks = &ctx.issue.subtasks;
    if subtasks.is_empty() {
        lines.push("None".into());
    } else {
        // Per-entry walks STAY raw — `subtasks: Vec<Value>` per ADR-0002.
        for subtask in subtasks {
            let key = subtask["key"].as_str().unwrap_or("UNKNOWN");
            let summary = subtask["fields"]["summary"].as_str().unwrap_or("");
            let status = subtask["fields"]["status"]["name"].as_str().unwrap_or("");
            let itype = subtask["fields"]["issuetype"]["name"]
                .as_str()
                .unwrap_or("");
            lines.push(format!(
                "- [{}]({}/browse/{}): {} ({}) \u{2014} {}",
                key, ctx.base_url, key, summary, status, itype
            ));
        }
    }
    lines.push(String::new());
    lines
}

pub(crate) fn child_issues(ctx: &RenderContext<'_>) -> Vec<String> {
    if ctx.child_issues.is_empty() {
        return Vec::new();
    }
    let mut lines = vec!["## Child Issues".into(), String::new()];

    lines.push(format!("{} child issue(s)", ctx.child_issues.len()));
    lines.push(String::new());

    // Table
    lines.push("| Key | Type | Summary | Status | Assignee |".into());
    lines.push("|-----|------|---------|--------|----------|".into());

    for issue in ctx.child_issues {
        let key = issue["key"].as_str().unwrap_or("UNKNOWN");
        let summary = issue["fields"]["summary"].as_str().unwrap_or("");
        let status = issue["fields"]["status"]["name"].as_str().unwrap_or("");
        let issue_type = issue["fields"]["issuetype"]["name"].as_str().unwrap_or("");
        let assignee = issue["fields"]["assignee"]["displayName"]
            .as_str()
            .unwrap_or("Unassigned");

        lines.push(format!(
            "| [{}]({}/browse/{}) | {} | {} | {} | {} |",
            key, ctx.base_url, key, issue_type, summary, status, assignee
        ));
    }

    lines.push(String::new());
    lines
}

pub(crate) fn worklogs(ctx: &RenderContext<'_>) -> Vec<String> {
    let worklog = &ctx.issue.worklog;
    let wl = &worklog.entries;
    let total = worklog.total;

    let mut lines = vec!["## Worklogs".into(), String::new()];

    if wl.is_empty() {
        lines.push("None".into());
        lines.push(String::new());
        return lines;
    }

    let total_seconds: u64 = wl.iter().map(|e| e.time_spent_seconds).sum();
    lines.push(format!(
        "**Total Time Logged:** {}",
        format_time(total_seconds)
    ));
    lines.push(String::new());

    if total > wl.len() as u64 {
        lines.push(format!(
            "> **Note:** Showing {} of {} worklogs. Additional worklogs may exist.",
            wl.len(),
            total
        ));
        lines.push(String::new());
    }

    lines.push("| Author | Time Spent | Date | Comment |".into());
    lines.push("|--------|-----------|------|---------|".into());

    for entry in wl {
        // Preserve the "Unknown" fallback when an author's display name is
        // missing — some legacy worklogs lack `author.displayName`.
        let author = if entry.author.is_empty() {
            "Unknown"
        } else {
            entry.author.as_str()
        };
        let started = entry.started.as_str();
        let date = if started.len() >= 10 {
            &started[..10]
        } else {
            started
        };
        // `entry.comment` STAYS `Value` — handed straight to `adf_to_plain_text`.
        let comment = adf_to_plain_text(&entry.comment).replace('|', "\\|");
        lines.push(format!(
            "| {} | {} | {} | {} |",
            author, entry.time_spent, date, comment
        ));
    }
    lines.push(String::new());
    lines
}

pub(crate) fn custom_fields(ctx: &RenderContext<'_>) -> Vec<String> {
    let issue_data = &ctx.issue.raw;
    let fields = match issue_data["fields"].as_object() {
        Some(f) => f,
        None => return Vec::new(),
    };

    let mut custom_fields: Vec<(String, String, Value)> = Vec::new();

    for (key, value) in fields {
        if !key.starts_with("customfield_") || value.is_null() {
            continue;
        }

        let display_name = ctx
            .field_metadata
            .names
            .get(key)
            .cloned()
            .unwrap_or_else(|| key.clone());

        // Apply field filter
        if ctx.field_filter.exclude.contains(&display_name) {
            continue;
        }
        if let Some(ref include) = ctx.field_filter.include {
            if !include.contains(&display_name) {
                continue;
            }
        }

        custom_fields.push((display_name, key.clone(), value.clone()));
    }

    if custom_fields.is_empty() {
        return Vec::new();
    }

    custom_fields.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));

    let attachments_ref = &ctx.attachments;
    let renderer =
        CustomFieldRenderer::new(|v: &Value| parse_adf_to_markdown(v, attachments_ref));

    let mut lines = vec!["## Custom Fields".into(), String::new()];

    for (display_name, field_id, value) in &custom_fields {
        let schema = ctx
            .field_metadata
            .schemas
            .get(field_id)
            .cloned()
            .unwrap_or(Value::Null);

        let rendered = match renderer.render_value(value, &schema) {
            Some(r) => r,
            None => continue,
        };

        if rendered.contains('\n') {
            lines.push(format!("### {}", display_name));
            lines.push(String::new());
            lines.push(rendered);
            lines.push(String::new());
        } else {
            lines.push(format!("- **{}:** {}", display_name, rendered));
        }
    }
    lines.push(String::new());
    lines
}

pub(crate) fn comments(ctx: &RenderContext<'_>) -> Vec<String> {
    let comments = &ctx.issue.comments;
    if comments.is_empty() {
        return Vec::new();
    }

    let mut lines = vec!["## Comments".into(), String::new()];

    for (i, comment) in comments.iter().enumerate() {
        let formatted_date = format_jira_date(&comment.created);
        lines.push(format!("**{}** - _{}_", comment.author, formatted_date));
        lines.push(String::new());

        let body_md = match &comment.body {
            RichText::Html(html) => convert_html_to_markdown(html),
            RichText::Adf(adf) => parse_adf_to_markdown(adf, &ctx.attachments),
            RichText::Plain(s) => s.clone(),
            RichText::Empty => "*No comment body*".to_string(),
        };

        let body_md = replace_attachment_links(&body_md, ctx.downloaded, ctx.domain);
        lines.push(body_md);

        if i < comments.len() - 1 {
            lines.push(String::new());
            lines.push("---".into());
            lines.push(String::new());
        }
    }
    lines.push(String::new());
    lines
}

pub(crate) fn changelog(ctx: &RenderContext<'_>) -> Vec<String> {
    let cl = match ctx.changelog_summary {
        Some(c) => c,
        None => return Vec::new(),
    };
    let plural = if cl.entry_count == 1 { "entry" } else { "entries" };
    vec![
        "## Changelog".into(),
        String::new(),
        format!(
            "See [{}]({}) ({} {}).",
            cl.file_name, cl.file_name, cl.entry_count, plural
        ),
        String::new(),
    ]
}

pub(crate) fn attachments(ctx: &RenderContext<'_>) -> Vec<String> {
    let downloaded = ctx.downloaded;
    let skipped_attachments = ctx.skipped_attachments;

    if !downloaded.is_empty() {
        let mut lines = Vec::new();
        lines.push("## Attachments".into());
        lines.push(String::new());
        // Render in Jira's attachment-array order: `downloaded` arrives in
        // concurrent-download completion order, which varies per run.
        // Per-entry walk of `attachment` STAYS raw — `Issue.attachments:
        // Vec<Value>` per ADR-0002.
        let jira_order: HashMap<&str, usize> = ctx
            .issue
            .attachments
            .iter()
            .enumerate()
            .filter_map(|(i, a)| a["id"].as_str().map(|id| (id, i)))
            .collect();
        let mut ordered: Vec<&DownloadedAttachment> = downloaded.iter().collect();
        ordered.sort_by(|a, b| {
            let rank = |att: &DownloadedAttachment| {
                att.attachment_id
                    .as_deref()
                    .and_then(|id| jira_order.get(id))
                    .copied()
                    .unwrap_or(usize::MAX)
            };
            rank(a).cmp(&rank(b)).then_with(|| a.filename.cmp(&b.filename))
        });
        for att in ordered {
            let encoded = url_encode(&att.filename);
            if att.mime_type.starts_with("image/") {
                lines.push(format!("- ![{}]({})", att.filename, encoded));
            } else {
                lines.push(format!("- [{}]({})", att.filename, encoded));
            }
        }
        lines.push(String::new());
        lines
    } else if !skipped_attachments.is_empty() {
        let mut lines = Vec::new();
        lines.push("## Attachments".into());
        lines.push(String::new());
        for attachment in skipped_attachments {
            let filename = attachment["filename"].as_str().unwrap_or("unknown");
            if let Some(url) = attachment["content"].as_str() {
                lines.push(format!("- [{}]({})", filename, url));
            } else {
                lines.push(format!("- {}", filename));
            }
        }
        lines.push(String::new());
        lines
    } else {
        Vec::new()
    }
}

// -----------------------------------------------------------------------------
// helpers — kept private to this module
// -----------------------------------------------------------------------------

/// Generate YAML metadata mapping from the typed [`Issue`].
///
/// After issue #13, every standard field consumed here is read from the typed
/// spine — `Issue::raw` is no longer touched for frontmatter. The
/// `Option<String>` / empty-string distinction encodes the YAML
/// `null`-vs-string output: `parse_string` collapses absent/null to `""`,
/// which we surface as YAML `null`; `parse_opt_string` preserves the absent
/// case directly.
fn generate_metadata(issue: &Issue) -> serde_yaml::Value {
    use serde_yaml::Value as Y;

    let mut map = serde_yaml::Mapping::new();

    // `set_str` emits YAML `null` for `None` and a quoted/unquoted string
    // otherwise — mirrors `set_str(Option<&str>)` exactly.
    let set_str = |map: &mut serde_yaml::Mapping, key: &str, val: Option<&str>| {
        map.insert(
            Y::String(key.into()),
            match val {
                Some(s) => Y::String(s.to_string()),
                None => Y::Null,
            },
        );
    };

    // A required string field (`key`, `summary`, etc.): empty string ⇒ YAML
    // `null`, matching the pre-#13 `as_str()` projection on a missing field.
    fn some_if_not_empty(s: &str) -> Option<&str> {
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }

    set_str(&mut map, "key", some_if_not_empty(&issue.key));
    set_str(&mut map, "summary", some_if_not_empty(&issue.summary));
    set_str(&mut map, "type", some_if_not_empty(&issue.issuetype.name));
    set_str(&mut map, "status", some_if_not_empty(&issue.status.name));
    set_str(
        &mut map,
        "status_category",
        issue.status.category.as_deref(),
    );
    set_str(
        &mut map,
        "priority",
        issue.priority.as_ref().map(|p| p.name.as_str()),
    );
    set_str(
        &mut map,
        "resolution",
        issue.resolution.as_ref().map(|r| r.name.as_str()),
    );
    set_str(&mut map, "project", some_if_not_empty(&issue.project.name));
    set_str(&mut map, "project_key", some_if_not_empty(&issue.project.key));
    set_str(
        &mut map,
        "assignee",
        issue.assignee.as_ref().map(|u| u.display_name.as_str()),
    );
    set_str(
        &mut map,
        "reporter",
        issue.reporter.as_ref().map(|u| u.display_name.as_str()),
    );
    set_str(
        &mut map,
        "creator",
        issue.creator.as_ref().map(|u| u.display_name.as_str()),
    );

    let labels: Vec<serde_yaml::Value> = issue
        .labels
        .iter()
        .map(|s| Y::String(s.clone()))
        .collect();
    map.insert(Y::String("labels".into()), Y::Sequence(labels));

    let components: Vec<serde_yaml::Value> = issue
        .components
        .iter()
        .map(|c| Y::String(c.name.clone()))
        .collect();
    map.insert(Y::String("components".into()), Y::Sequence(components));

    // `parent` STAYS `Option<Value>` per ADR-0002 — the renderer walks it
    // raw here for `key` / `fields.summary`.
    set_str(
        &mut map,
        "parent_key",
        issue
            .parent
            .as_ref()
            .and_then(|p| p["key"].as_str()),
    );
    set_str(
        &mut map,
        "parent_summary",
        issue
            .parent
            .as_ref()
            .and_then(|p| p["fields"]["summary"].as_str()),
    );

    let affects: Vec<serde_yaml::Value> = issue
        .versions
        .iter()
        .map(|v| Y::String(v.name.clone()))
        .collect();
    map.insert(Y::String("affects_versions".into()), Y::Sequence(affects));

    let fix_ver: Vec<serde_yaml::Value> = issue
        .fix_versions
        .iter()
        .map(|v| Y::String(v.name.clone()))
        .collect();
    map.insert(Y::String("fix_versions".into()), Y::Sequence(fix_ver));

    set_str(&mut map, "created_at", some_if_not_empty(&issue.created));
    set_str(&mut map, "updated_at", some_if_not_empty(&issue.updated));
    set_str(&mut map, "resolved_at", issue.resolutiondate.as_deref());
    set_str(&mut map, "duedate", issue.duedate.as_deref());

    let tt = &issue.timetracking;
    set_str(&mut map, "original_estimate", tt.original_estimate.as_deref());
    set_str(&mut map, "time_spent", tt.time_spent.as_deref());
    set_str(&mut map, "remaining_estimate", tt.remaining_estimate.as_deref());

    map.insert(
        Y::String("progress".into()),
        Y::Number(issue.progress.percent.into()),
    );
    map.insert(
        Y::String("aggregate_progress".into()),
        Y::Number(issue.aggregateprogress.percent.into()),
    );

    map.insert(
        Y::String("votes".into()),
        Y::Number(issue.votes.votes.into()),
    );
    map.insert(
        Y::String("watches".into()),
        Y::Number(issue.watches.watch_count.into()),
    );

    Y::Mapping(map)
}

fn format_time(seconds: u64) -> String {
    let days = seconds / 28800; // 8h workday
    let remaining = seconds % 28800;
    let hours = remaining / 3600;
    let remaining = remaining % 3600;
    let minutes = remaining / 60;

    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{}d", days));
    }
    if hours > 0 {
        parts.push(format!("{}h", hours));
    }
    if minutes > 0 {
        parts.push(format!("{}m", minutes));
    }
    if parts.is_empty() {
        "0m".to_string()
    } else {
        parts.join(" ")
    }
}

fn format_jira_date(created: &str) -> String {
    if created.is_empty() {
        return "Unknown date".to_string();
    }
    // Try parsing ISO 8601
    let normalized = if created.ends_with('Z') {
        created.replace('Z', "+00:00")
    } else if created.contains('+') && !created.ends_with("+00:00") {
        // Replace +0000 with +00:00
        let re = regex::Regex::new(r"\+(\d{2})(\d{2})$").unwrap();
        re.replace(created, "+$1:$2").to_string()
    } else {
        created.to_string()
    };

    match chrono::DateTime::parse_from_rfc3339(&normalized) {
        Ok(dt) => dt.format("%Y-%m-%d %I:%M %p").to_string(),
        Err(_) => match chrono::NaiveDateTime::parse_from_str(
            &created[..19.min(created.len())],
            "%Y-%m-%dT%H:%M:%S",
        ) {
            Ok(dt) => dt.format("%Y-%m-%d %I:%M %p").to_string(),
            Err(_) => created.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FieldFilter;
    use crate::issue::{
        Comment, Issue, IssueType, NamedRef, Priority, Progress, Project, Resolution, RichText,
        Status, TimeTracking, User, Votes, Watches, WorklogPage,
    };
    use crate::markdown::{AttachmentIndex, CustomFieldMetadata, RenderContext};

    /// AC#7: a section composer built from a *literal* `RenderContext` (no
    /// `json!()` fixture) renders the exact expected byte sequence. Asserting
    /// `assert_eq!` on the joined lines guards the prefer-HTML precedence and
    /// the `_2024-01-15 10:30 AM_` `format_jira_date` shape.
    #[test]
    fn comments_section_renders_single_comment_verbatim() {
        let issue = Issue {
            raw: serde_json::Value::Null,
            key: "K1".into(),
            summary: "S".into(),
            created: String::new(),
            updated: String::new(),
            duedate: None,
            resolutiondate: None,
            issuetype: IssueType {
                name: "Task".into(),
            },
            status: Status {
                name: "Open".into(),
                category: None,
            },
            priority: None,
            resolution: None,
            project: Project {
                name: String::new(),
                key: String::new(),
            },
            assignee: None,
            reporter: None,
            creator: None,
            labels: Vec::new(),
            components: Vec::new(),
            versions: Vec::new(),
            fix_versions: Vec::new(),
            timetracking: TimeTracking {
                original_estimate: None,
                remaining_estimate: None,
                time_spent: None,
            },
            progress: Progress { percent: 0 },
            aggregateprogress: Progress { percent: 0 },
            votes: Votes { votes: 0 },
            watches: Watches { watch_count: 0 },
            environment: RichText::Empty,
            description: RichText::Empty,
            comments: vec![Comment {
                id: "1".into(),
                author: "Alice".into(),
                created: "2024-01-15T10:30:00.000+0000".into(),
                body: RichText::Plain("hello world".into()),
            }],
            attachments: Vec::new(),
            issuelinks: Vec::new(),
            parent: None,
            subtasks: Vec::new(),
            worklog: WorklogPage {
                total: 0,
                entries: Vec::new(),
            },
        };
        let field_metadata = CustomFieldMetadata::empty();
        let field_filter = FieldFilter::default();
        let ctx = RenderContext {
            issue: &issue,
            downloaded: &[],
            skipped_attachments: &[],
            attachments: AttachmentIndex::empty(),
            field_metadata: &field_metadata,
            field_filter: &field_filter,
            child_issues: &[],
            changelog_summary: None,
            base_url: "https://example.atlassian.net",
            domain: "example.atlassian.net",
        };

        let rendered = comments(&ctx).join("\n");
        let expected =
            "## Comments\n\n**Alice** - _2024-01-15 10:30 AM_\n\nhello world\n";
        assert_eq!(rendered, expected);
    }

    /// AC#5: the frontmatter section, built from a *literal* `Issue` (no
    /// `json!()` fixture, `raw: Value::Null`), renders every newly-typed
    /// display field. Asserting `contains` on each key/value pair guards the
    /// `Issue::field`/`raw` removal: if any standard field were still being
    /// fetched off `raw`, this test would fail because `raw` is `Null`.
    #[test]
    fn frontmatter_section_renders_typed_display_fields() {
        let issue = Issue {
            raw: serde_json::Value::Null,
            key: "PROJ-1".into(),
            summary: "Typed frontmatter".into(),
            created: "2026-05-01T08:00:00.000+0000".into(),
            updated: "2026-05-22T10:00:00.000+0000".into(),
            duedate: Some("2026-06-30".into()),
            resolutiondate: None,
            issuetype: IssueType {
                name: "Bug".into(),
            },
            status: Status {
                name: "Open".into(),
                category: Some("To Do".into()),
            },
            priority: Some(Priority {
                name: "High".into(),
            }),
            resolution: None::<Resolution>,
            project: Project {
                name: "Project X".into(),
                key: "PROJ".into(),
            },
            assignee: Some(User {
                display_name: "Jane".into(),
            }),
            reporter: Some(User {
                display_name: "Bob".into(),
            }),
            creator: Some(User {
                display_name: "Bot".into(),
            }),
            labels: vec!["a".into(), "b".into()],
            components: vec![NamedRef { name: "UI".into() }],
            versions: Vec::new(),
            fix_versions: vec![NamedRef {
                name: "v1.0".into(),
            }],
            timetracking: TimeTracking {
                original_estimate: None,
                remaining_estimate: None,
                time_spent: None,
            },
            progress: Progress { percent: 0 },
            aggregateprogress: Progress { percent: 0 },
            votes: Votes { votes: 3 },
            watches: Watches { watch_count: 8 },
            environment: RichText::Empty,
            description: RichText::Empty,
            comments: Vec::new(),
            attachments: Vec::new(),
            issuelinks: Vec::new(),
            parent: None,
            subtasks: Vec::new(),
            worklog: WorklogPage {
                total: 0,
                entries: Vec::new(),
            },
        };
        let field_metadata = CustomFieldMetadata::empty();
        let field_filter = FieldFilter::default();
        let ctx = RenderContext {
            issue: &issue,
            downloaded: &[],
            skipped_attachments: &[],
            attachments: AttachmentIndex::empty(),
            field_metadata: &field_metadata,
            field_filter: &field_filter,
            child_issues: &[],
            changelog_summary: None,
            base_url: "https://example.atlassian.net",
            domain: "example.atlassian.net",
        };
        let rendered = frontmatter(&ctx).join("\n");
        assert!(rendered.contains("priority: High"));
        assert!(rendered.contains("project: Project X"));
        assert!(rendered.contains("project_key: PROJ"));
        assert!(rendered.contains("reporter: Bob"));
        assert!(rendered.contains("creator: Bot"));
        assert!(rendered.contains("votes: 3"));
        assert!(rendered.contains("watches: 8"));
        assert!(rendered.contains("- a")); // labels
        assert!(rendered.contains("- UI")); // components
    }
}
