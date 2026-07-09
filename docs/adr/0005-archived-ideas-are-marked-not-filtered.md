# Archived Ideas are marked in frontmatter, not filtered out of exports

JPD archiving is a locked custom field ("Idea archived", empty or `Yes`), not a
Jira status: JPD views in the UI always hide Archived Ideas, but JQL/REST
search returns them by default. That mismatch makes naive project-wide exports
appear to contain "extra" issues (e.g. the API returns 75 ideas while the JPD
view shows 50), and there is no supported API for querying a JPD view's own
membership.

## Decision: mirror everything, mark archived state, filter downstream

jarkdown never rewrites user JQL and adds no archive-related fetch flags.
Instead:

- Exported frontmatter carries `archived: true` plus `archived_on` /
  `archived_by` when an Issue's "Idea archived" field is `Yes`, and omits all
  three otherwise (matching the existing omit-when-absent frontmatter
  convention). The field is resolved by display name via `/rest/api/3/field`
  because its `customfield_*` id differs per site.
- The documented JPD recipe is to sync **without** an archived exclusion and
  let consumers filter on the `archived` frontmatter key. Archiving an Idea
  changes the Issue, so incremental runs re-export it with the marker — the
  local mirror stays truthful over time.
- Users who want UI-equivalent result sets can still write
  `AND "Idea archived" IS EMPTY` themselves; the README documents the clause.

## Considered alternatives

- **Exclude archived by default (match the JPD UI)** — rejected because it
  silently rewrites user JQL, changes existing users' results on upgrade, and
  behaves differently per site depending on whether the field exists.
- **An `--exclude-archived` convenience flag** — rejected as unnecessary
  surface area once frontmatter carries the state; the JQL clause is a
  one-liner and the flag would invite the stale-mirror problem below.
- **Exclude archived in the sync JQL** — rejected because an Idea archived
  after its first export drops out of the result set, is never revalidated
  (incremental validation only covers keys in the current run's scope), and
  its file lingers looking like a live idea — the mirror silently lies.
- **Manifest membership diffing (evict keys that leave the query result)** —
  rejected as a much larger manifest change solving the general
  "left the JQL" problem; unnecessary once archived Ideas stay in the synced
  set and carry their own marker. Note that an Archived Idea is deliberately
  **not** an Evicted Issue: Jira still returns it.

## Consequence to verify during implementation

Incremental re-export of a newly archived Idea relies on archiving bumping the
Issue's `updated` timestamp. Belt-and-braces: include the "Idea archived"
field in the freshness-validation fetch and compare against the manifest, so
re-export does not depend on `updated` moving.
