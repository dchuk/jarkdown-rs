# Child-aware incremental manifest and hierarchy layouts

Hierarchical incremental export has to support long-lived caches where the same
Issue may be requested directly, discovered below another Issue, or shared
across multiple hierarchy roots. A single per-Issue `root`/`descendant` role and
one artifact path are not enough to describe that shape.

## Decision: manifest v2 is a graph-backed cache index

Manifest v2 records per-Issue cache facts separately from hierarchy membership.
An Issue can be a Requested Root, a Hierarchy Member below another Requested
Root, or both. Parent-child edges are stored as graph relationships rather than
as an exclusive role on the Issue. Removed edges do not imply the Issue was
deleted; an Issue may become orphaned while its exported files remain.

The manifest also records artifact paths for each Issue. This is required
because jarkdown supports two hierarchy layouts:

- `corpus` layout stores one canonical artifact directory per Issue below the
  output root.
- `nested` layout stores artifacts in tree-shaped directories for human
  browsing, which can create multiple artifact paths for the same Issue.

When `--incremental --hierarchy` is used, `corpus` is the default layout because
it best matches shared-cache semantics. Non-incremental hierarchy export may
continue to default to `nested` for backward compatibility. Users can select the
layout explicitly.

## Decision: validation is chunked and child-aware

Incremental validation builds one invocation-level validation plan from the
current Requested Roots plus their active cached descendants. It validates them
with chunked Jira search requests for `updated` and minimal display metadata,
then exports only Issues whose timestamps advanced or whose artifacts need
repair.

Missing keys in a successful validation response become inactive Evicted Issues.
Whole-query failures do not evict anything.

## Decision: changed descendants refresh only necessary work

A changed leaf descendant is re-exported without re-traversing unchanged
ancestors. A changed descendant that is known to have children is treated as a
subtree root within the current invocation bounds. In `nested` layout, the
changed Issue is written to every active artifact path recorded for it.

Root indexes are traversal snapshots. Descendant-only refresh does not rebuild
ancestor index files.

## Considered alternatives

- **Single `root`/`descendant` role per Issue** — rejected because an Issue can
  be requested directly and also discovered below another Requested Root.
- **Canonical corpus layout only** — rejected because nested hierarchy exports
  are useful for human browsing and existing behavior.
- **Nested layout only** — rejected because overlapping roots create duplicate
  artifacts and undermine shared-cache efficiency unless the manifest tracks all
  paths.
- **Evict missing keys on failed validation requests** — rejected because Jira or
  network failures would look like mass deletion.
