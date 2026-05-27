# Jarkdown

A Rust CLI and library that exports Jira Cloud issues to local Markdown files, preserving content, attachments, and metadata for offline use.

For how the code is layered (with MermaidJS diagrams of every key flow), see
[`docs/architecture.md`](docs/architecture.md). For non-obvious design
decisions, see [`docs/adr/`](docs/adr/).

## Language

**Issue**:
A single Jira ticket, identified by its key (e.g. `PROJ-123`).
_Avoid_: Ticket, story (use these only when referring to a specific Jira issue type).

**Child Issue**:
An **Issue** directly related beneath another **Issue** in an exported hierarchy.
Child Issues may come from Jira sub-tasks, epic membership, JPD delivery links,
or parent-child issue links.
_Avoid_: Child ticket.

**Descendant Issue**:
An **Issue** reachable below another **Issue** through one or more Child Issue
relationships.
_Avoid_: Descendant ticket.

**Delivery Issue**:
An **Issue** linked from a JPD Idea through the delivery/implementation
relationship.
_Avoid_: Delivery ticket.

**Delivery Link**:
A Jira/JPD relationship connecting a JPD Idea to a Delivery Issue.
_Avoid_: Delivery ticket link.

**Requested Root**:
An **Issue** explicitly named by the user or selected by a query as the starting
point for an export.
_Avoid_: Root role.

**Hierarchy Member**:
An **Issue** that belongs to an exported hierarchy, whether it was requested
directly or discovered as a Child Issue or Descendant Issue.
_Avoid_: Cached ticket.

**Orphaned Hierarchy Member**:
A **Hierarchy Member** that is no longer connected to any known parent and is
not currently a Requested Root.
_Avoid_: Deleted issue.

**Evicted Issue**:
An **Issue** kept in the local record as inactive because Jira no longer
returned it during freshness validation. An Evicted Issue may have been deleted,
moved out of permission scope, or otherwise hidden from the current user.
_Avoid_: Deleted issue, permission-lost issue.

**Changelog**:
Jira's audit trail of field changes on an **Issue** — every status transition, assignee change, sprint move, etc. Each entry has author, timestamp, and one or more (field, from, to) tuples. Fetched from `/rest/api/3/issue/{key}/changelog` (paginated).
_Avoid_: History, audit log, activity (informal aliases — always write "changelog" in code, docs, and flag names).

**Comment**:
A free-text user comment posted on an **Issue**. Distinct from a **Changelog** entry — comments are content, changelog entries are field-change events.
_Avoid_: Note.

**Worklog**:
A time-tracking entry logged against an **Issue** (author, time spent, date, optional comment). Exported as part of the Issue's content, but out of scope for the **Changelog** feature — a worklog records logged time, a changelog entry records a field change.

## Relationships

- An **Issue** has zero or more **Changelog** entries, zero or more **Comments**, and zero or more **Worklog** entries.
- A **Changelog** entry represents one user save action and may contain multiple field changes.
- A **Child Issue** is directly below one parent **Issue** within a hierarchy export.
- A **Descendant Issue** may be nested several levels below the hierarchy root.
- A **Delivery Issue** is a kind of Child Issue when the parent is a JPD Idea
  connected by a **Delivery Link**.
- A **Requested Root** can also be a **Hierarchy Member** below another Requested Root.
- A **Hierarchy Member** can belong to more than one exported hierarchy.
- An **Orphaned Hierarchy Member** may keep its exported files even after its
  last known hierarchy edge is removed.
- An **Evicted Issue** is inactive until it is requested directly or rediscovered
  through hierarchy traversal.

## Flagged ambiguities

- "history" was used colloquially to mean **Changelog**. Resolved: the user-facing term remains "history" in casual speech, but the canonical term in code, flags, and output sections is **changelog**.
