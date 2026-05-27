# Changelog

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
