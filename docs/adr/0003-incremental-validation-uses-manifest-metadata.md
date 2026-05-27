# Incremental validation uses manifest metadata for unchanged artifacts

When incremental export validates cached Issues through a batched Jira search,
unchanged Issues no longer have their full Issue payload fetched. That creates a
non-obvious interaction with `--include-changelog`: ADR-0001 requires jarkdown
to backfill a missing changelog sidecar even when the Issue itself is unchanged,
but the changelog writer needs a display title.

## Decision: store display metadata in the manifest

Manifest v2 stores the last exported display metadata needed to maintain
sidecars for an unchanged Issue, including at least the Issue summary. When an
unchanged Issue is missing a changelog sidecar, jarkdown fetches only the
changelog endpoint and renders the sidecar using the manifest summary. If the
manifest has no summary, it falls back to the Issue key.

The full Issue payload is fetched only when the Issue is selected for full
export, when it is newly discovered during hierarchy traversal, or when the user
bypasses incremental behavior with `--force`.

## Considered alternatives

- **Fetch the full Issue for changelog backfill** — rejected because it
  reintroduces the per-Issue network round-trip that batched validation is meant
  to remove.
- **Skip changelog backfill for unchanged Issues** — rejected because it
  contradicts ADR-0001 and produces incomplete exports when users enable
  `--include-changelog` after an initial run.
- **Render changelog sidecars without a title** — rejected because the manifest
  already has a natural place to retain the last exported summary, and falling
  back to the key covers old manifests without sacrificing output quality for
  current ones.
