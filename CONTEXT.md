# Jarkdown

A Rust CLI and library that exports Jira Cloud issues to local Markdown files, preserving content, attachments, and metadata for offline use.

## Language

**Issue**:
A single Jira ticket, identified by its key (e.g. `PROJ-123`).
_Avoid_: Ticket, story (use these only when referring to a specific Jira issue type).

**Changelog**:
Jira's audit trail of field changes on an **Issue** — every status transition, assignee change, sprint move, etc. Each entry has author, timestamp, and one or more (field, from, to) tuples. Fetched from `/rest/api/3/issue/{key}/changelog` (paginated).
_Avoid_: History, audit log, activity (informal aliases — always write "changelog" in code, docs, and flag names).

**Comment**:
A free-text user comment posted on an **Issue**. Distinct from a **Changelog** entry — comments are content, changelog entries are field-change events.
_Avoid_: Note.

**Worklog**:
A time-tracking entry logged against an **Issue**. Not currently exported; explicitly out of scope for the changelog feature.

## Relationships

- An **Issue** has zero or more **Changelog** entries, zero or more **Comments**, and zero or more **Worklog** entries.
- A **Changelog** entry represents one user save action and may contain multiple field changes.

## Flagged ambiguities

- "history" was used colloquially to mean **Changelog**. Resolved: the user-facing term remains "history" in casual speech, but the canonical term in code, flags, and output sections is **changelog**.
