//! Typed Jira issue models, parsed at the HTTP seam.
//!
//! `jira_client` stays pure transport: it performs the HTTP request and hands
//! the raw `serde_json::Value` payload here to be parsed into an [`Issue`] or
//! [`IssueSearchResult`].
//!
//! See ADR-0002 for why [`Issue`] retains its raw payload (`raw`) alongside the
//! typed "spine" fields: `--include-json` serializes `raw` byte-for-byte, and
//! `raw` is the access path for the ~19 display-only standard fields and all
//! custom fields, reached via [`Issue::field`].
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
#[derive(Debug, Clone)]
pub struct Issue {
    /// The untouched payload from `fetch_issue`. See ADR-0002 — do not remove.
    pub raw: Value,
    pub key: String,
    pub summary: String,
    pub updated: String,
    pub issuetype: IssueType,
    pub status: Status,
    pub assignee: Option<User>,
    pub description: RichText,
    pub comments: Vec<Comment>,
    pub attachments: Vec<Value>,
    pub issuelinks: Vec<Value>,
    pub parent: Option<Value>,
    pub subtasks: Vec<Value>,
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
        let updated;
        let issuetype;
        let status;
        let assignee;
        let description;
        let comments;
        let attachments;
        let issuelinks;
        let parent;
        let subtasks;
        {
            let fields = &raw["fields"];
            let rendered = &raw["renderedFields"];
            key = parse_string(&raw["key"], "key")?;
            summary = parse_string(&fields["summary"], "summary")?;
            updated = parse_string(&fields["updated"], "updated")?;
            issuetype = parse_issuetype(&fields["issuetype"], "issuetype")?;
            status = parse_status(&fields["status"], "status")?;
            assignee = parse_user(&fields["assignee"], "assignee")?;
            description = RichText::resolve(&fields["description"], &rendered["description"]);
            comments = parse_comments(fields, rendered)?;
            attachments = parse_array(&fields["attachment"], "attachment")?;
            issuelinks = parse_array(&fields["issuelinks"], "issuelinks")?;
            parent = parse_parent(&fields["parent"])?;
            subtasks = parse_array(&fields["subtasks"], "subtasks")?;
        }
        Ok(Issue {
            raw,
            key,
            summary,
            updated,
            issuetype,
            status,
            assignee,
            description,
            comments,
            attachments,
            issuelinks,
            parent,
            subtasks,
        })
    }

    /// Access an unmodeled standard or custom field by name off `raw["fields"]`.
    /// This is the escape hatch for the ~19 display-only standard fields and
    /// all `customfield_*` values that are deliberately not typed.
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
