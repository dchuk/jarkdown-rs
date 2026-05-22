//! Typed Jira issue models, parsed at the HTTP seam.
//!
//! `jira_client` stays pure transport: it performs the HTTP request and hands
//! the raw `serde_json::Value` payload here to be parsed into an [`Issue`] or
//! [`IssueSearchResult`].
//!
//! See ADR-0002 for why [`Issue`] retains its raw payload (`raw`) alongside the
//! typed "spine" fields: `--include-json` serializes `raw` byte-for-byte, and
//! `raw` is the access path for `customfield_*` values, reached via
//! [`Issue::field`].
//!
//! Parsing is strict about *shape*: a field present with an unexpected JSON
//! type is a hard [`JarkdownError::MalformedPayload`]. An absent or `null`
//! field degrades to an empty default — Jira omits fields legitimately
//! (e.g. `updated` on some projections) and that is not schema drift.

use serde_json::Value;

use crate::error::{JarkdownError, Result};

/// Rich-text content for a description or comment body.
///
/// Jira returns these fields in two parallel trees — `fields.*` as ADF (or a
/// plain string) and `renderedFields.*` as HTML. The prefer-HTML precedence is
/// applied once, here at the parse seam, so `renderedFields` never appears in
/// the typed surface. ADF bodies are retained unparsed inside `Adf`.
#[derive(Debug, Clone, PartialEq)]
pub enum RichText {
    Html(String),
    Adf(Value),
    Plain(String),
    Empty,
}

impl RichText {
    /// Resolve rich text from the raw `fields.*` value and its `renderedFields.*`
    /// counterpart, applying prefer-HTML precedence: rendered HTML, then ADF,
    /// then a plain string, else empty.
    fn resolve(raw_field: &Value, rendered: &Value) -> RichText {
        if let Some(html) = rendered.as_str() {
            if !html.is_empty() {
                return RichText::Html(html.to_string());
            }
        }
        if raw_field.is_object() {
            return RichText::Adf(raw_field.clone());
        }
        if let Some(s) = raw_field.as_str() {
            if !s.is_empty() {
                return RichText::Plain(s.to_string());
            }
        }
        RichText::Empty
    }
}

/// The issue type (`fields.issuetype`), projected to the name that drives logic.
#[derive(Debug, Clone, PartialEq)]
pub struct IssueType {
    pub name: String,
}

/// The workflow status (`fields.status`).
#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub name: String,
    pub category: Option<String>,
}

/// A Jira user reference, projected to the display name.
#[derive(Debug, Clone, PartialEq)]
pub struct User {
    pub display_name: String,
}

/// `fields.priority` — only the display name is consumed by the renderer.
#[derive(Debug, Clone, PartialEq)]
pub struct Priority {
    pub name: String,
}

/// `fields.resolution` — only the display name is consumed by the renderer.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolution {
    pub name: String,
}

/// `fields.project` — present unconditionally; absent/null degrades to empty
/// strings (renderer prints `null` in YAML when either is empty).
#[derive(Debug, Clone, PartialEq)]
pub struct Project {
    pub name: String,
    pub key: String,
}

/// A named reference (`fields.components[]`, `fields.versions[]`,
/// `fields.fixVersions[]`) — only `name` is consumed by the renderer.
#[derive(Debug, Clone, PartialEq)]
pub struct NamedRef {
    pub name: String,
}

/// `fields.timetracking` — present unconditionally; each estimate is
/// optional. The renderer prints `null` for absent estimates.
#[derive(Debug, Clone, PartialEq)]
pub struct TimeTracking {
    pub original_estimate: Option<String>,
    pub remaining_estimate: Option<String>,
    pub time_spent: Option<String>,
}

/// `fields.progress` / `fields.aggregateprogress` — present unconditionally.
/// `percent` defaults to `0` when absent (load-bearing: the PSOP-5624
/// baseline payload omits `percent`, and the renderer prints `0`).
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub percent: u64,
}

/// `fields.votes` — present unconditionally; only `votes` is consumed.
#[derive(Debug, Clone, PartialEq)]
pub struct Votes {
    pub votes: u64,
}

/// `fields.watches` — present unconditionally; only `watchCount` is consumed.
#[derive(Debug, Clone, PartialEq)]
pub struct Watches {
    pub watch_count: u64,
}

/// `fields.worklog` — paginated worklog summary. `total` is the server-side
/// count (may exceed `entries.len()`); the renderer warns when truncation
/// has occurred.
#[derive(Debug, Clone, PartialEq)]
pub struct WorklogPage {
    pub total: u64,
    pub entries: Vec<WorklogEntry>,
}

/// One worklog entry. `comment` deliberately STAYS `Value` — the renderer
/// passes it straight to `adf_to_plain_text`, and typing it as `RichText`
/// would force an unnecessary HTML/ADF branch here.
#[derive(Debug, Clone, PartialEq)]
pub struct WorklogEntry {
    pub author: String,
    pub time_spent: String,
    pub started: String,
    pub time_spent_seconds: u64,
    pub comment: Value,
}

/// A free-text comment posted on an issue.
#[derive(Debug, Clone, PartialEq)]
pub struct Comment {
    pub id: String,
    pub author: String,
    pub created: String,
    pub body: RichText,
}

/// A single Jira changelog history entry, parsed at the HTTP seam.
///
/// Retains `raw` per ADR-0002 so `{KEY}.changelog.json` is byte-identical to
/// the payload Jira returned. The typed "spine" fields (`id`, `author`,
/// `created`, `items`) cover everything the renderer needs; auxiliary fields
/// like `historyMetadata` live only in `raw`.
#[derive(Debug, Clone)]
pub struct ChangelogEntry {
    /// The untouched history entry payload. See ADR-0002 — do not remove.
    pub raw: Value,
    pub id: String,
    /// `author.displayName`. Empty string when the `author` key is absent or
    /// `null` (some system entries have no author).
    pub author: String,
    /// ISO timestamp exactly as Jira returned it.
    pub created: String,
    pub items: Vec<ChangelogItem>,
}

/// A single field-change inside a [`ChangelogEntry`].
#[derive(Debug, Clone, PartialEq)]
pub struct ChangelogItem {
    pub field: String,
    /// `null` or absent → `None`; the renderer prints this as `∅`.
    pub from_string: Option<String>,
    /// `null` or absent → `None`; the renderer prints this as `∅`.
    pub to_string: Option<String>,
}

/// A fully-fetched Jira issue: the untouched payload (`raw`) plus the typed
/// "spine" fields the rest of the codebase relies on.
///
/// After issue #13, the ~19 display-only standard fields are also typed; the
/// renderer reaches the raw payload only for `customfield_*` and for
/// per-entry walks of `issuelinks`, `subtasks`, and `attachment`. `raw` and
/// [`Issue::field`] remain (ADR-0002) because `--include-json` re-serializes
/// `raw` byte-for-byte and `field()` is the access path for `customfield_*`.
#[derive(Debug, Clone)]
pub struct Issue {
    /// The untouched payload from `fetch_issue`. See ADR-0002 — do not remove.
    pub raw: Value,
    pub key: String,
    pub summary: String,
    pub created: String,
    pub updated: String,
    pub duedate: Option<String>,
    pub resolutiondate: Option<String>,
    pub issuetype: IssueType,
    pub status: Status,
    pub priority: Option<Priority>,
    pub resolution: Option<Resolution>,
    pub project: Project,
    pub assignee: Option<User>,
    pub reporter: Option<User>,
    pub creator: Option<User>,
    pub labels: Vec<String>,
    pub components: Vec<NamedRef>,
    pub versions: Vec<NamedRef>,
    pub fix_versions: Vec<NamedRef>,
    pub timetracking: TimeTracking,
    pub progress: Progress,
    pub aggregateprogress: Progress,
    pub votes: Votes,
    pub watches: Watches,
    pub environment: RichText,
    pub description: RichText,
    pub comments: Vec<Comment>,
    pub attachments: Vec<Value>,
    pub issuelinks: Vec<Value>,
    pub parent: Option<Value>,
    pub subtasks: Vec<Value>,
    pub worklog: WorklogPage,
}

/// The lightweight projection returned by `search_jql` (`key`, `summary`,
/// `issuetype`, `status`, `assignee`). Deliberately *not* merged into [`Issue`]:
/// a single all-`Option` type cannot distinguish "field absent" from "field
/// never fetched".
#[derive(Debug, Clone)]
pub struct IssueSearchResult {
    /// The untouched search-hit payload.
    pub raw: Value,
    pub key: String,
    pub summary: String,
    pub issuetype: IssueType,
    pub status: Status,
    pub assignee: Option<User>,
}

fn malformed(field: &str) -> JarkdownError {
    JarkdownError::MalformedPayload(format!(
        "field `{}` has an unexpected shape",
        field
    ))
}

/// A string field: absent/`null` → empty, present-but-not-a-string → error.
fn parse_string(v: &Value, field: &str) -> Result<String> {
    match v {
        Value::Null => Ok(String::new()),
        Value::String(s) => Ok(s.clone()),
        _ => Err(malformed(field)),
    }
}

/// An optional nested string (e.g. `status.statusCategory.name`).
fn parse_opt_string(v: &Value, field: &str) -> Result<Option<String>> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(s.clone())),
        _ => Err(malformed(field)),
    }
}

/// An array field: absent/`null` → empty, present-but-not-an-array → error.
fn parse_array(v: &Value, field: &str) -> Result<Vec<Value>> {
    match v {
        Value::Null => Ok(Vec::new()),
        Value::Array(a) => Ok(a.clone()),
        _ => Err(malformed(field)),
    }
}

fn parse_issuetype(v: &Value, field: &str) -> Result<IssueType> {
    match v {
        Value::Null => Ok(IssueType {
            name: String::new(),
        }),
        Value::Object(_) => Ok(IssueType {
            name: parse_string(&v["name"], "issuetype.name")?,
        }),
        _ => Err(malformed(field)),
    }
}

fn parse_status(v: &Value, field: &str) -> Result<Status> {
    match v {
        Value::Null => Ok(Status {
            name: String::new(),
            category: None,
        }),
        Value::Object(_) => Ok(Status {
            name: parse_string(&v["name"], "status.name")?,
            category: parse_opt_string(&v["statusCategory"]["name"], "status.statusCategory.name")?,
        }),
        _ => Err(malformed(field)),
    }
}

fn parse_user(v: &Value, field: &str) -> Result<Option<User>> {
    match v {
        Value::Null => Ok(None),
        Value::Object(_) => Ok(Some(User {
            display_name: parse_string(&v["displayName"], "user.displayName")?,
        })),
        _ => Err(malformed(field)),
    }
}

fn parse_parent(v: &Value) -> Result<Option<Value>> {
    match v {
        Value::Null => Ok(None),
        Value::Object(_) => Ok(Some(v.clone())),
        _ => Err(malformed("parent")),
    }
}

fn parse_priority(v: &Value) -> Result<Option<Priority>> {
    match v {
        Value::Null => Ok(None),
        Value::Object(_) => Ok(Some(Priority {
            name: parse_string(&v["name"], "priority.name")?,
        })),
        _ => Err(malformed("priority")),
    }
}

fn parse_resolution(v: &Value) -> Result<Option<Resolution>> {
    match v {
        Value::Null => Ok(None),
        Value::Object(_) => Ok(Some(Resolution {
            name: parse_string(&v["name"], "resolution.name")?,
        })),
        _ => Err(malformed("resolution")),
    }
}

fn parse_project(v: &Value) -> Result<Project> {
    match v {
        Value::Null => Ok(Project {
            name: String::new(),
            key: String::new(),
        }),
        Value::Object(_) => Ok(Project {
            name: parse_string(&v["name"], "project.name")?,
            key: parse_string(&v["key"], "project.key")?,
        }),
        _ => Err(malformed("project")),
    }
}

/// An array-of-strings field (e.g. `fields.labels`). Absent/null → empty;
/// any non-string element is hard schema drift.
fn parse_string_array(v: &Value, field: &str) -> Result<Vec<String>> {
    match v {
        Value::Null => Ok(Vec::new()),
        Value::Array(a) => {
            let mut out = Vec::with_capacity(a.len());
            for item in a {
                match item {
                    Value::String(s) => out.push(s.clone()),
                    _ => return Err(malformed(field)),
                }
            }
            Ok(out)
        }
        _ => Err(malformed(field)),
    }
}

/// An array of name-only references (`fields.components`, `fields.versions`,
/// `fields.fixVersions`). Each element must be an object; only `name` is
/// consumed.
fn parse_named_ref_array(v: &Value, field: &str) -> Result<Vec<NamedRef>> {
    match v {
        Value::Null => Ok(Vec::new()),
        Value::Array(a) => {
            let mut out = Vec::with_capacity(a.len());
            for item in a {
                match item {
                    Value::Object(_) => out.push(NamedRef {
                        name: parse_string(&item["name"], "namedRef.name")?,
                    }),
                    _ => return Err(malformed(field)),
                }
            }
            Ok(out)
        }
        _ => Err(malformed(field)),
    }
}

fn parse_timetracking(v: &Value) -> Result<TimeTracking> {
    match v {
        Value::Null => Ok(TimeTracking {
            original_estimate: None,
            remaining_estimate: None,
            time_spent: None,
        }),
        Value::Object(_) => Ok(TimeTracking {
            original_estimate: parse_opt_string(
                &v["originalEstimate"],
                "timetracking.originalEstimate",
            )?,
            remaining_estimate: parse_opt_string(
                &v["remainingEstimate"],
                "timetracking.remainingEstimate",
            )?,
            time_spent: parse_opt_string(&v["timeSpent"], "timetracking.timeSpent")?,
        }),
        _ => Err(malformed("timetracking")),
    }
}

/// A `u64` field that absent/null → 0; a present non-integer is hard schema
/// drift. The default `0` is load-bearing for `progress.percent`,
/// `votes.votes`, and `watches.watchCount`: the PSOP-5624 baseline omits
/// `percent` and the renderer still prints `0`.
fn parse_u64(v: &Value, field: &str) -> Result<u64> {
    match v {
        Value::Null => Ok(0),
        Value::Number(n) => n.as_u64().ok_or_else(|| malformed(field)),
        _ => Err(malformed(field)),
    }
}

fn parse_progress(v: &Value, field: &str) -> Result<Progress> {
    match v {
        Value::Null => Ok(Progress { percent: 0 }),
        Value::Object(_) => Ok(Progress {
            percent: parse_u64(&v["percent"], "progress.percent")?,
        }),
        _ => Err(malformed(field)),
    }
}

fn parse_votes(v: &Value) -> Result<Votes> {
    match v {
        Value::Null => Ok(Votes { votes: 0 }),
        Value::Object(_) => Ok(Votes {
            votes: parse_u64(&v["votes"], "votes.votes")?,
        }),
        _ => Err(malformed("votes")),
    }
}

fn parse_watches(v: &Value) -> Result<Watches> {
    match v {
        Value::Null => Ok(Watches { watch_count: 0 }),
        Value::Object(_) => Ok(Watches {
            watch_count: parse_u64(&v["watchCount"], "watches.watchCount")?,
        }),
        _ => Err(malformed("watches")),
    }
}

fn parse_worklog_page(v: &Value) -> Result<WorklogPage> {
    match v {
        Value::Null => Ok(WorklogPage {
            total: 0,
            entries: Vec::new(),
        }),
        Value::Object(_) => {
            let total = parse_u64(&v["total"], "worklog.total")?;
            let raw_entries = parse_array(&v["worklogs"], "worklog.worklogs")?;
            let mut entries = Vec::with_capacity(raw_entries.len());
            for e in &raw_entries {
                if !e.is_object() {
                    return Err(malformed("worklog.worklogs[]"));
                }
                entries.push(WorklogEntry {
                    author: parse_string(
                        &e["author"]["displayName"],
                        "worklog.worklogs[].author.displayName",
                    )?,
                    time_spent: parse_string(
                        &e["timeSpent"],
                        "worklog.worklogs[].timeSpent",
                    )?,
                    started: parse_string(
                        &e["started"],
                        "worklog.worklogs[].started",
                    )?,
                    time_spent_seconds: parse_u64(
                        &e["timeSpentSeconds"],
                        "worklog.worklogs[].timeSpentSeconds",
                    )?,
                    // `comment` stays Value — the renderer hands it straight
                    // to `adf_to_plain_text`.
                    comment: e["comment"].clone(),
                });
            }
            Ok(WorklogPage { total, entries })
        }
        _ => Err(malformed("worklog")),
    }
}

fn parse_comments(fields: &Value, rendered_fields: &Value) -> Result<Vec<Comment>> {
    let raw_comments = match &fields["comment"]["comments"] {
        Value::Null => return Ok(Vec::new()),
        Value::Array(a) => a,
        _ => return Err(malformed("comment.comments")),
    };
    let rendered: Vec<&Value> = rendered_fields["comment"]["comments"]
        .as_array()
        .map(|a| a.iter().collect())
        .unwrap_or_default();

    let mut out = Vec::with_capacity(raw_comments.len());
    for c in raw_comments {
        // Prefer-HTML precedence for the body: `renderedBody` on the comment,
        // then the matching entry in `renderedFields.comment.comments`, then
        // the raw ADF/string `body`.
        let rendered_body = if c["renderedBody"].is_string() {
            c["renderedBody"].clone()
        } else {
            rendered
                .iter()
                .find(|rc| rc["id"] == c["id"])
                .map(|rc| rc["body"].clone())
                .unwrap_or(Value::Null)
        };
        out.push(Comment {
            id: parse_string(&c["id"], "comment.id")?,
            author: parse_string(&c["author"]["displayName"], "comment.author.displayName")?,
            created: parse_string(&c["created"], "comment.created")?,
            body: RichText::resolve(&c["body"], &rendered_body),
        });
    }
    Ok(out)
}

impl Issue {
    /// Parse a `fetch_issue` payload into a typed [`Issue`], retaining `raw`.
    pub fn from_value(raw: Value) -> Result<Issue> {
        let key;
        let summary;
        let created;
        let updated;
        let duedate;
        let resolutiondate;
        let issuetype;
        let status;
        let priority;
        let resolution;
        let project;
        let assignee;
        let reporter;
        let creator;
        let labels;
        let components;
        let versions;
        let fix_versions;
        let timetracking;
        let progress;
        let aggregateprogress;
        let votes;
        let watches;
        let environment;
        let description;
        let comments;
        let attachments;
        let issuelinks;
        let parent;
        let subtasks;
        let worklog;
        {
            let fields = &raw["fields"];
            let rendered = &raw["renderedFields"];
            key = parse_string(&raw["key"], "key")?;
            summary = parse_string(&fields["summary"], "summary")?;
            created = parse_string(&fields["created"], "created")?;
            updated = parse_string(&fields["updated"], "updated")?;
            duedate = parse_opt_string(&fields["duedate"], "duedate")?;
            resolutiondate = parse_opt_string(&fields["resolutiondate"], "resolutiondate")?;
            issuetype = parse_issuetype(&fields["issuetype"], "issuetype")?;
            status = parse_status(&fields["status"], "status")?;
            priority = parse_priority(&fields["priority"])?;
            resolution = parse_resolution(&fields["resolution"])?;
            project = parse_project(&fields["project"])?;
            assignee = parse_user(&fields["assignee"], "assignee")?;
            reporter = parse_user(&fields["reporter"], "reporter")?;
            creator = parse_user(&fields["creator"], "creator")?;
            labels = parse_string_array(&fields["labels"], "labels")?;
            components = parse_named_ref_array(&fields["components"], "components")?;
            versions = parse_named_ref_array(&fields["versions"], "versions")?;
            fix_versions = parse_named_ref_array(&fields["fixVersions"], "fixVersions")?;
            timetracking = parse_timetracking(&fields["timetracking"])?;
            progress = parse_progress(&fields["progress"], "progress")?;
            aggregateprogress = parse_progress(&fields["aggregateprogress"], "aggregateprogress")?;
            votes = parse_votes(&fields["votes"])?;
            watches = parse_watches(&fields["watches"])?;
            environment = RichText::resolve(&fields["environment"], &rendered["environment"]);
            description = RichText::resolve(&fields["description"], &rendered["description"]);
            comments = parse_comments(fields, rendered)?;
            attachments = parse_array(&fields["attachment"], "attachment")?;
            issuelinks = parse_array(&fields["issuelinks"], "issuelinks")?;
            parent = parse_parent(&fields["parent"])?;
            subtasks = parse_array(&fields["subtasks"], "subtasks")?;
            worklog = parse_worklog_page(&fields["worklog"])?;
        }
        Ok(Issue {
            raw,
            key,
            summary,
            created,
            updated,
            duedate,
            resolutiondate,
            issuetype,
            status,
            priority,
            resolution,
            project,
            assignee,
            reporter,
            creator,
            labels,
            components,
            versions,
            fix_versions,
            timetracking,
            progress,
            aggregateprogress,
            votes,
            watches,
            environment,
            description,
            comments,
            attachments,
            issuelinks,
            parent,
            subtasks,
            worklog,
        })
    }

    /// Access an unmodeled field by name off `raw["fields"]`.
    ///
    /// After issue #13, every previously-display-only standard field has a
    /// typed home on [`Issue`]; this method is the escape hatch only for
    /// `customfield_*` values, which are deliberately not modeled.
    pub fn field(&self, name: &str) -> Option<&Value> {
        self.raw
            .get("fields")
            .and_then(|f| f.get(name))
            .filter(|v| !v.is_null())
    }
}

impl ChangelogEntry {
    /// Parse a single changelog history entry into a typed
    /// [`ChangelogEntry`], retaining `raw` for byte-identical `.changelog.json`
    /// serialization.
    ///
    /// Shape rules mirror [`Issue::from_value`]: absent/`null` fields degrade
    /// to empty defaults (`author` legitimately absent on some system
    /// entries), but a present-but-wrong-typed field is a hard
    /// [`JarkdownError::MalformedPayload`].
    pub fn from_value(raw: Value) -> Result<ChangelogEntry> {
        if !raw.is_object() {
            return Err(malformed("changelog[]"));
        }

        let id;
        let author;
        let created;
        let items;
        {
            id = parse_string(&raw["id"], "changelog[].id")?;
            // `author` may be absent entirely; indexing a missing key yields
            // `Value::Null`, which `parse_string` collapses to "".
            author = parse_string(
                &raw["author"]["displayName"],
                "changelog[].author.displayName",
            )?;
            created = parse_string(&raw["created"], "changelog[].created")?;

            let raw_items = parse_array(&raw["items"], "changelog[].items")?;
            let mut parsed_items = Vec::with_capacity(raw_items.len());
            for item in &raw_items {
                if !item.is_object() && !item.is_null() {
                    return Err(malformed("changelog[].items[]"));
                }
                parsed_items.push(ChangelogItem {
                    field: parse_string(&item["field"], "changelog[].items[].field")?,
                    from_string: parse_opt_string(
                        &item["fromString"],
                        "changelog[].items[].fromString",
                    )?,
                    to_string: parse_opt_string(
                        &item["toString"],
                        "changelog[].items[].toString",
                    )?,
                });
            }
            items = parsed_items;
        }

        Ok(ChangelogEntry {
            raw,
            id,
            author,
            created,
            items,
        })
    }
}

impl IssueSearchResult {
    /// Parse a single `search_jql` hit into a typed [`IssueSearchResult`].
    pub fn from_value(raw: Value) -> Result<IssueSearchResult> {
        let key;
        let summary;
        let issuetype;
        let status;
        let assignee;
        {
            let fields = &raw["fields"];
            key = parse_string(&raw["key"], "key")?;
            summary = parse_string(&fields["summary"], "summary")?;
            issuetype = parse_issuetype(&fields["issuetype"], "issuetype")?;
            status = parse_status(&fields["status"], "status")?;
            assignee = parse_user(&fields["assignee"], "assignee")?;
        }
        Ok(IssueSearchResult {
            raw,
            key,
            summary,
            issuetype,
            status,
            assignee,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn from_value_parses_spine_fields_and_retains_raw() {
        let raw = json!({
            "key": "PROJ-1",
            "renderedFields": { "description": "<p>Hello</p>" },
            "fields": {
                "summary": "A typed issue",
                "updated": "2026-05-22T10:00:00.000+0000",
                "issuetype": { "name": "Task" },
                "status": { "name": "Open", "statusCategory": { "name": "To Do" } },
                "assignee": { "displayName": "Jane Smith" },
                "description": { "type": "doc", "content": [] },
                "comment": { "comments": [] },
                "attachment": [],
                "issuelinks": [],
                "parent": null,
                "subtasks": []
            }
        });
        let issue = Issue::from_value(raw.clone()).expect("parse");
        assert_eq!(issue.key, "PROJ-1");
        assert_eq!(issue.summary, "A typed issue");
        assert_eq!(issue.updated, "2026-05-22T10:00:00.000+0000");
        assert_eq!(issue.issuetype.name, "Task");
        assert_eq!(issue.status.name, "Open");
        assert_eq!(issue.status.category.as_deref(), Some("To Do"));
        assert_eq!(
            issue.assignee.as_ref().map(|u| u.display_name.as_str()),
            Some("Jane Smith")
        );
        // Prefer-HTML precedence: renderedFields.description wins over ADF.
        assert_eq!(issue.description, RichText::Html("<p>Hello</p>".to_string()));
        assert_eq!(issue.raw, raw, "raw payload retained byte-for-byte");
    }

    #[test]
    fn from_value_tolerates_absent_fields_but_rejects_wrong_types() {
        // Absent `updated` and arrays degrade to empty (not every projection
        // includes every field).
        let ok = Issue::from_value(json!({
            "key": "K1",
            "fields": { "summary": "S", "issuetype": { "name": "Bug" }, "status": { "name": "Done" } }
        }))
        .expect("absent fields tolerated");
        assert_eq!(ok.updated, "");
        assert!(ok.comments.is_empty());
        assert_eq!(ok.description, RichText::Empty);

        // A present-but-wrong-typed field is hard schema drift.
        let err = Issue::from_value(json!({
            "key": "K1",
            "fields": { "summary": { "not": "a string" } }
        }));
        assert!(matches!(err, Err(JarkdownError::MalformedPayload(_))));
    }

    #[test]
    fn comment_body_prefers_rendered_html() {
        let issue = Issue::from_value(json!({
            "key": "K1",
            "fields": {
                "summary": "S",
                "comment": { "comments": [
                    { "id": "100", "author": { "displayName": "Bob" },
                      "created": "2026-01-01T00:00:00.000+0000",
                      "body": { "type": "doc" }, "renderedBody": "<p>hi</p>" }
                ] }
            }
        }))
        .expect("parse");
        assert_eq!(issue.comments.len(), 1);
        assert_eq!(issue.comments[0].author, "Bob");
        assert_eq!(issue.comments[0].body, RichText::Html("<p>hi</p>".to_string()));
    }

    #[test]
    fn changelog_entry_from_value_parses_typed_fields_and_retains_raw() {
        let raw = json!({
            "id": "9827921",
            "author": { "displayName": "Jane Smith" },
            "created": "2026-05-06T15:52:53.582-0700",
            "items": [
                {
                    "field": "assignee",
                    "fieldId": "assignee",
                    "fieldtype": "jira",
                    "from": null,
                    "fromString": "Old User",
                    "to": null,
                    "toString": null
                },
                {
                    "field": "status",
                    "fieldId": "status",
                    "fieldtype": "jira",
                    "from": "1",
                    "fromString": null,
                    "to": "3",
                    "toString": "In Progress"
                }
            ],
            "historyMetadata": { "type": "myType" }
        });

        let entry = ChangelogEntry::from_value(raw.clone()).expect("parse");
        assert_eq!(entry.id, "9827921");
        assert_eq!(entry.author, "Jane Smith");
        assert_eq!(entry.created, "2026-05-06T15:52:53.582-0700");
        assert_eq!(entry.items.len(), 2);
        assert_eq!(entry.items[0].field, "assignee");
        assert_eq!(entry.items[0].from_string.as_deref(), Some("Old User"));
        assert_eq!(entry.items[0].to_string, None);
        assert_eq!(entry.items[1].field, "status");
        assert_eq!(entry.items[1].from_string, None);
        assert_eq!(entry.items[1].to_string.as_deref(), Some("In Progress"));
        // ADR-0002: the raw payload is retained byte-for-byte so that the
        // sibling `.changelog.json` artifact can be re-serialized losslessly.
        assert_eq!(entry.raw, raw);
    }

    #[test]
    fn changelog_entry_from_value_tolerates_absent_author_but_rejects_wrong_types() {
        // (a) Some system-generated entries omit `author` entirely (mirrors
        // PSOP-5624 entry id=9827921 in the baseline).
        let ok = ChangelogEntry::from_value(json!({
            "id": "9827921",
            "created": "2026-05-06T15:52:53.582-0700",
            "items": [
                { "field": "Insights", "fromString": null, "toString": "0" }
            ]
        }))
        .expect("absent author tolerated");
        assert_eq!(ok.author, "");
        assert_eq!(ok.id, "9827921");
        assert_eq!(ok.items.len(), 1);

        // (b) `created` present as an integer is hard schema drift.
        let bad_created = ChangelogEntry::from_value(json!({
            "id": "x",
            "created": 12345,
            "items": []
        }));
        assert!(matches!(
            bad_created,
            Err(JarkdownError::MalformedPayload(_))
        ));

        // (c) `items` present as an object instead of an array is hard
        // schema drift.
        let bad_items = ChangelogEntry::from_value(json!({
            "id": "x",
            "created": "2026-01-01T00:00:00.000+0000",
            "items": { "not": "an array" }
        }));
        assert!(matches!(
            bad_items,
            Err(JarkdownError::MalformedPayload(_))
        ));
    }

    #[test]
    fn from_value_types_display_only_fields() {
        // AC#1: the ~19 display-only standard fields now have typed parsers.
        let raw = json!({
            "key": "K1",
            "fields": {
                "summary": "S",
                "created": "2026-05-01T00:00:00.000+0000",
                "updated": "2026-05-22T10:00:00.000+0000",
                "duedate": "2026-06-30",
                "resolutiondate": null,
                "issuetype": { "name": "Bug" },
                "status": { "name": "Open" },
                "priority": { "name": "High" },
                "resolution": null,
                "project": { "name": "Proj X", "key": "PX" },
                "assignee": null,
                "reporter": { "displayName": "Bob" },
                "creator": { "displayName": "Bot" },
                "labels": ["a", "b"],
                "components": [{ "name": "UI" }],
                "versions": [],
                "fixVersions": [{ "name": "v1.0" }],
                "timetracking": { "originalEstimate": "1d", "remainingEstimate": null, "timeSpent": "2h" },
                // `progress` deliberately omits `percent` to match the baseline
                // payload — typed default `0` is load-bearing.
                "progress": {},
                "aggregateprogress": { "percent": 50 },
                "votes": { "votes": 3 },
                "watches": { "watchCount": 8 },
                "environment": null,
                "worklog": {
                    "total": 1,
                    "worklogs": [
                        {
                            "author": { "displayName": "Alice" },
                            "timeSpent": "30m",
                            "started": "2026-05-10T12:00:00.000+0000",
                            "timeSpentSeconds": 1800,
                            "comment": { "type": "doc", "content": [] }
                        }
                    ]
                }
            }
        });
        let issue = Issue::from_value(raw).expect("parse");
        assert_eq!(issue.created, "2026-05-01T00:00:00.000+0000");
        assert_eq!(issue.duedate.as_deref(), Some("2026-06-30"));
        assert_eq!(issue.resolutiondate, None);
        assert_eq!(issue.priority.as_ref().map(|p| p.name.as_str()), Some("High"));
        assert!(issue.resolution.is_none());
        assert_eq!(issue.project.name, "Proj X");
        assert_eq!(issue.project.key, "PX");
        assert_eq!(
            issue.reporter.as_ref().map(|u| u.display_name.as_str()),
            Some("Bob")
        );
        assert_eq!(
            issue.creator.as_ref().map(|u| u.display_name.as_str()),
            Some("Bot")
        );
        assert_eq!(issue.labels, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(issue.components.len(), 1);
        assert_eq!(issue.components[0].name, "UI");
        assert!(issue.versions.is_empty());
        assert_eq!(issue.fix_versions.len(), 1);
        assert_eq!(issue.fix_versions[0].name, "v1.0");
        assert_eq!(issue.timetracking.original_estimate.as_deref(), Some("1d"));
        assert_eq!(issue.timetracking.remaining_estimate, None);
        assert_eq!(issue.timetracking.time_spent.as_deref(), Some("2h"));
        assert_eq!(issue.progress.percent, 0);
        assert_eq!(issue.aggregateprogress.percent, 50);
        assert_eq!(issue.votes.votes, 3);
        assert_eq!(issue.watches.watch_count, 8);
        assert_eq!(issue.environment, RichText::Empty);
        assert_eq!(issue.worklog.total, 1);
        assert_eq!(issue.worklog.entries.len(), 1);
        assert_eq!(issue.worklog.entries[0].author, "Alice");
        assert_eq!(issue.worklog.entries[0].time_spent, "30m");
        assert_eq!(issue.worklog.entries[0].started, "2026-05-10T12:00:00.000+0000");
        assert_eq!(issue.worklog.entries[0].time_spent_seconds, 1800);
        // `comment` STAYS Value — the renderer hands it to `adf_to_plain_text`.
        assert!(issue.worklog.entries[0].comment.is_object());
    }

    #[test]
    fn from_value_rejects_wrong_typed_display_fields() {
        // labels-as-object is hard schema drift.
        let err = Issue::from_value(json!({
            "key": "K1",
            "fields": { "summary": "S", "labels": { "not": "an array" } }
        }));
        assert!(matches!(err, Err(JarkdownError::MalformedPayload(_))));

        // priority-as-string is hard schema drift.
        let err = Issue::from_value(json!({
            "key": "K1",
            "fields": { "summary": "S", "priority": "High" }
        }));
        assert!(matches!(err, Err(JarkdownError::MalformedPayload(_))));

        // votes.votes-as-string is hard schema drift.
        let err = Issue::from_value(json!({
            "key": "K1",
            "fields": { "summary": "S", "votes": { "votes": "many" } }
        }));
        assert!(matches!(err, Err(JarkdownError::MalformedPayload(_))));
    }

    #[test]
    fn search_result_parses_projection() {
        let r = IssueSearchResult::from_value(json!({
            "key": "K2",
            "fields": {
                "summary": "Search hit",
                "issuetype": { "name": "Story" },
                "status": { "name": "In Progress" },
                "assignee": null
            }
        }))
        .expect("parse");
        assert_eq!(r.key, "K2");
        assert_eq!(r.summary, "Search hit");
        assert_eq!(r.issuetype.name, "Story");
        assert!(r.assignee.is_none());
    }
}
