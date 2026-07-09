# Changelog

## Unreleased

JPD archived-state support (#52, ADR-0005): archived ideas are mirrored and
marked, never filtered.

### Added

- Frontmatter marks JPD Archived Ideas: `archived: true` plus `archived_on` /
  `archived_by` when the site's "Idea archived" field is `Yes`. Live ideas and
  non-JPD issues emit none of the keys — absence means "not archived". The
  fields are resolved by display name (per-site `customfield_*` ids).
- Incremental runs re-export an idea whose archived state drifted from the
  manifest even when `updated` has not moved, so the frontmatter marker never
  goes stale. Validation observes the state when the field id is known from
  the field cache; pre-existing manifests load unchanged.
- README: archived-ideas section explaining the API-vs-view count gap
  (archived ideas are returned by JQL/REST by default but hidden in JPD
  views), the sync-all-and-filter-on-frontmatter recipe, and the
  `"Idea archived" IS EMPTY` / `= Yes` JQL clauses.

### Changed

- The three "Idea archived*" fields no longer render in `## Custom Fields`
  (they are frontmatter now).

## 1.8.0 - 2026-06-01

Incremental-export ergonomics for flat `bulk`/`query` (#48).

### Added

- `--manifest <PATH>` now persists the manifest on its own, without requiring
  `--incremental`, so a plain `bulk KEY --manifest m.json` primes the cache for
  a later incremental pull. Plain exports with neither flag still write nothing.
- `--summary-json <PATH>` writes a machine-readable run summary
  (`{"reexported": [...], "skipped": [...], "failed": [...]}`) for flat
  `bulk`/`query`, letting programmatic consumers re-pull only what changed.
- Stderr warnings for flags that have no effect in the current invocation
  (`--force` without `--incremental`; `--summary-json` with `--hierarchy`),
  emitted by `export`, `bulk`, and `query` — no more silent no-ops.

### Fixed

- Unchanged issues whose only work was a changelog backfill are now reported as
  `skipped` rather than `reexported` in the run summary.
- `--summary-json` creates missing parent directories before writing, matching
  the index and manifest writers.

## 1.7.2 - 2026-05-27

Cache-correctness patch release.

### Added

- Typed `EvictionReason` vocabulary
  (`not_returned_by_validation_search`, `fetch_not_found_or_forbidden`,
  `child_fetch_or_export_failed`, `force_fetch_failed`) with stable
  serialization and round-trip support for unknown legacy strings.
- `truncated_by_depth` / `truncated_by_issue_count` cause flags on root
  snapshots, in addition to the existing compatibility `truncated` boolean.
- New internal `planner` module with deterministic warm-hierarchy planning
  for corpus and nested layouts, covered by direct unit tests.
- Validation pagination guard that errors after a bounded number of pages
  per chunk so a misbehaving Jira response cannot loop forever.

### Changed

- Requested Issue keys are canonicalized to uppercase before writing
  artifacts, so `proj-1` and `PROJ-1` both land in the same `PROJ-1/`
  directory and manifest entry.
- Nested hierarchy root snapshots now write to `{ROOT}.hierarchy.md` (same
  as corpus), so multiple Requested Roots in one output directory do not
  collide on a shared `index.md`.
- Hierarchy edge upserts replace duplicate legacy edges with a single
  active edge and only mark touched parents for merge-on-write.
- `--force --incremental` failures on a previously evicted entry now
  preserve the eviction with a `force_fetch_failed` reason instead of
  reviving the entry.

### Fixed

- Manifest loads warn when they find legacy case-mismatched Issue
  directories (e.g. `proj-1/` for canonical key `PROJ-1`) or legacy
  nested `index.md` snapshots, without renaming or deleting either.
- Validation pagination correctly terminates on empty `nextPageToken`
  and when all requested keys in a chunk have already been seen.

### Documentation

- Refreshed `docs/architecture.md` and `docs/manifest-v2.md` for the
  new eviction vocabulary, truncation cause metadata, canonical Issue
  directories, planner extraction, and nested snapshot disambiguation.
- README updates for canonical Issue directory normalization and the
  new nested `{ROOT}.hierarchy.md` snapshot path.

## 1.7.1 - 2026-05-27

Docs-only patch release.

- Rewrote the architecture guide for the 1.7 child-aware incremental cache:
  validation-first planning, `plan_metadata`, corpus/nested hierarchy layouts,
  shared-child traversal, child-aware warm planning, and freshness rules.
- Expanded the manifest v2 diagnostic reference with root snapshots, timestamp
  semantics, artifact path safety, atomic writes, and merge-on-write behavior.
- Hardened README guidance around package vs binary names, crates.io dependency
  usage, conservative incremental validation, and hierarchy layout defaults.
- Removed a real-looking Jira key from internal comments. No runtime behavior
  changes.

## 1.7.0 - 2026-05-27

### Added

- Manifest v2 incremental cache with graph-backed hierarchy state, v1
  migration, external `--manifest <path>` support, merge-on-write persistence,
  and unsupported future-version protection.
- Child-aware incremental hierarchy export for corpus and nested layouts,
  including warm skips, bounded descendant refresh, multi-root validation
  planning, orphaned hierarchy members, evicted Issue tombstones, and snapshot
  metadata.
- `--hierarchy-layout corpus|nested`; incremental hierarchy defaults to
  `corpus`, while non-incremental hierarchy keeps the legacy nested layout.
- Content-visible option fingerprints covering JSON, changelog, field filters,
  `--no-attachments`, hierarchy depth, and hierarchy issue caps.

### Changed

- Incremental flat, bulk, query, and hierarchy exports validate cached Issues
  through Jira search metadata before deciding whether to skip, repair missing
  artifacts, backfill changelogs, evict inaccessible cache entries, or perform
  a full export.
- Changelog-only incremental repairs now fetch and write only the changelog
  sidecars instead of rewriting the main Markdown when the Issue itself is
  unchanged.
- Hierarchy traversal now records all parent edges and artifact paths for
  shared non-cyclic children while avoiding duplicate child fetches.
- Jira `updated` freshness comparison now parses Jira/RFC3339 timestamps and
  uses conservative string fallback only when parsing fails.

### Security

- Manifest artifact paths are sanitized at load and write/join use sites,
  rejecting parent traversal, absolute paths, root/prefix components, and
  Windows-drive-style paths.
- Manifest saves now use randomized `create_new` temp files beside the target,
  sync them, and rename atomically, avoiding deterministic temp-file symlink
  attacks.

### Documentation

- Added manifest v2 format documentation.
- Updated README installation names for the generated Homebrew formula,
  installer, and release archive names.
- Documented `--manifest`, `--hierarchy-layout`, corpus hierarchy output, and
  nested incremental shared-path behavior.

## 1.6.1 - 2026-05-26

Docs-only patch release.

- Added architecture diagrams and source maps in `docs/architecture.md`.
- Documented `--include-changelog` in README usage, defaults, output
  structure, and library escape hatch examples.
- Linked architecture and ADR docs from the project context.
