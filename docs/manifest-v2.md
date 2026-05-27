# Manifest v2

Manifest v2 is jarkdown's internal cache index for incremental exports. It is
stable enough for diagnostics, but it is not yet a public integration API for
external tools.

## Location

By default, the manifest lives at `<output>/.jarkdown-manifest.json`.
`--manifest <path>` changes only the manifest file location. Artifact paths
inside the manifest are resolved relative to the export output root, not
relative to the manifest file.

## Keys

Issue keys are stored using Jira's canonical uppercase form. Requested keys are
normalized before validation and planning so `proj-1` and `PROJ-1` do not create
separate cache entries.

## Issue Entries

Each Issue entry records cache facts for one Issue:

- last validated/exported `updated` timestamp
- `exported_at`
- display metadata such as `summary` and `issue_type`
- whether the Issue is currently active or evicted
- artifact paths where this Issue is written

An Evicted Issue stays in the manifest as an inactive tombstone. Its files are
not deleted.

## Graph

Hierarchy membership is stored as graph relationships, not as a single role on
an Issue. An Issue may be a Requested Root, a Hierarchy Member below another
Requested Root, or both.

Edges can be removed without removing the Issue entry. If an Issue loses its
last known parent and is not a Requested Root, it becomes an Orphaned Hierarchy
Member. Its files may remain on disk.

## Layouts

`corpus` layout writes one canonical artifact directory per Issue below the
output root.

`nested` layout writes tree-shaped artifact directories for browsing. Because
the same Issue may appear in multiple trees, manifest v2 records every active
artifact path for each Issue.

`--incremental --hierarchy` defaults to `corpus`. Non-incremental hierarchy
export may keep the legacy `nested` default for compatibility.

## Validation

Incremental validation is planned once per invocation from the current Requested
Roots plus their active cached descendants. Jira lookups are chunked and
paginated. Validation requests fetch `updated` plus minimal display metadata.

Keys missing from a successful validation response become Evicted Issues. If a
validation request fails, no keys from that request are evicted.

## Migration

Manifest v1 entries are migrated in memory to v2. Each v1 Issue becomes an
active Issue with no known graph edges and one inferred artifact path. Saving
writes v2.

## Non-Goals

Manifest v2 does not provide cross-process locking. It also does not delete
exported files when Issues are unlinked, orphaned, or evicted.
