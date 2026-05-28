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
- whether the Issue is currently active, evicted, or an orphaned hierarchy
  member
- artifact paths where this Issue is written, each with active/inactive state
- an option fingerprint for content-visible export settings
- Requested Roots and hierarchy roots associated with the Issue

An Evicted Issue stays in the manifest as an inactive tombstone. Its files are
not deleted. Artifact paths for evicted Issues are marked inactive so later
refreshes do not write through stale paths.

`eviction_reason` is a stable string vocabulary:

- `not_returned_by_validation_search`: a successful incremental validation
  search omitted an active cached Issue.
- `fetch_not_found_or_forbidden`: hierarchy traversal could not fetch a child
  because Jira returned not found or forbidden.
- `child_fetch_or_export_failed`: hierarchy traversal could not fetch or export
  a child for another reason.
- `force_fetch_failed`: `--force` targeted an evicted root, but the full
  fetch/export failed, so the root remains evicted.

Unknown legacy reason strings are accepted when loading older manifests and are
written back unchanged. New jarkdown producers use the typed vocabulary above.

## Option Fingerprint

The option fingerprint is versioned. The current format starts with `v1:` and
serializes sorted, normalized key/value pairs. Code that changes the format must
use a new prefix so older cached entries safely invalidate.

Content-visible options participate in the fingerprint:

- `include_json`
- `include_changelog`
- `include_fields`
- `exclude_fields`
- `no_attachments`
- hierarchy bounds when hierarchy output is involved: `max_depth` and
  `max_issues`

Options that affect only logging, concurrency, credentials, or destination
selection do not belong in the fingerprint.

If the only fingerprint delta is enabling `include_changelog`, and the main
Markdown artifact is already fresh, jarkdown uses the changelog-only backfill
path from ADR-0001 rather than rewriting the main Markdown.

## Graph

Hierarchy membership is stored as graph relationships, not as a single role on
an Issue. An Issue may be a Requested Root, a Hierarchy Member below another
Requested Root, or both.

Edges can be removed without removing the Issue entry. If an Issue loses its
last known parent and is not a Requested Root, it becomes an Orphaned Hierarchy
Member. Its files may remain on disk.

Root snapshots record the last hierarchy snapshot for each Requested Root:

- root key
- layout (`corpus` or `nested`)
- snapshot path (`{ROOT}.hierarchy.md` for new corpus and nested exports)
- export timestamp
- `truncated`, the compatibility boolean for any traversal truncation
- `truncated_by_depth`, true when `max_depth` stopped traversal
- `truncated_by_issue_count`, true when `max_issues` stopped traversal
- child-fetch failures recorded during traversal

Snapshots are traversal snapshots, not live views. Descendant-only refreshes can
update changed artifacts without rebuilding unchanged ancestor snapshots.
Older manifests that only contain `truncated` load with both cause fields
defaulting to `false`; jarkdown does not infer causes for historical snapshots.
Older nested exports that recorded `index.md` remain readable. When such a
manifest is loaded, jarkdown warns and preserves the legacy path without
deleting or migrating the file.

## Layouts

`corpus` layout writes one canonical uppercase artifact directory per Issue
below the output root. Producers record manifest `artifact_paths` using the
same byte-for-byte directory casing that new exports create on disk. When a
manifest is loaded for an output root, jarkdown scans directory entry names and
warns if it finds an older case-mismatched Issue directory such as `proj-1/`
for canonical key `PROJ-1`; it does not rename or migrate that directory.

`nested` layout writes tree-shaped artifact directories for browsing. Because
the same Issue may appear in multiple trees, manifest v2 records every active
artifact path for each Issue. New nested root snapshots use
`{ROOT}.hierarchy.md`, so multiple Requested Roots in one output directory have
distinct snapshot files and distinct manifest paths.

`--incremental --hierarchy` defaults to `corpus`. Non-incremental hierarchy
export may keep the legacy `nested` default for compatibility.

Changed shared Issues in `nested` layout are refreshed to every active artifact
path recorded for that Issue. Shared non-cyclic children are recorded under all
active parent edges and nested paths without duplicate child discovery.

## Validation

Incremental validation is planned once per invocation from the current Requested
Roots plus their active cached descendants. Jira lookups are chunked and
paginated. Validation requests fetch `updated` plus minimal display metadata.

Keys missing from a successful validation response become Evicted Issues. If a
validation request fails, no keys from that request are evicted.

Freshness uses parsed Jira/RFC3339 `updated` timestamps where possible:

- incoming newer timestamp: full export
- equal parsed timestamp: unchanged
- incoming older timestamp: warn and keep the newer cached timestamp
- unparseable values: fall back to conservative string comparison

Missing main Markdown or requested JSON artifacts force a full export. Missing
changelog artifacts under `--include-changelog` use the changelog-only backfill
path when the Issue itself is unchanged.

## Path Safety

Artifact paths in the manifest are always treated as paths relative to the
export output root. Unsafe paths are dropped at load time and rejected at use
sites. Rejected forms include:

- parent traversal components such as `..`
- absolute paths
- root or prefix components
- Windows-drive-style prefixes such as `C:`
- empty paths

This prevents a tampered manifest from causing artifact repair or hierarchy
refresh to write outside the output root.

## Writes

Manifest saves merge touched in-memory records with the current on-disk
manifest so sequential invocations preserve unrelated entries, inactive paths,
edges, and snapshots. The current invocation wins for records and graph parents
it touched.

Saves are atomic at the file level: jarkdown writes a randomized `create_new`
temporary file beside the manifest, syncs it, and renames it over the target.
The randomized exclusive temp path avoids following a pre-existing deterministic
temp-file symlink.

## Migration

Manifest v1 entries are migrated in memory to v2. Each v1 Issue becomes an
active Issue with no known graph edges and one inferred artifact path. Saving
writes v2.

## Non-Goals

Manifest v2 does not provide cross-process locking. It also does not delete
exported files when Issues are unlinked, orphaned, or evicted. It is a cache
index, not an append-only audit log.
