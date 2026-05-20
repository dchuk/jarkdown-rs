# Changelog export shape and incremental backfill

When adding `--include-changelog`, two non-obvious choices were made.

## Decision 1: Separate `{KEY}.changelog.md` file, not inline in `{KEY}.md`

A ticket's changelog routinely runs into hundreds of entries on long-lived issues. Inlining it under the description would drown the actual ticket content — exactly the signal most consumers (humans skimming, RAG indexers, AI agents) come for. Splitting the changelog into its own file keeps the main `.md` focused on description + comments + current state, and lets downstream tooling opt in to or skip the changelog without parsing it out.

The main `.md` still cross-references the changelog file via a `changelog:` frontmatter key and a one-line `## Changelog` body section, so discoverability is preserved.

When `--include-json` is also set, the changelog lands in a parallel `{KEY}.changelog.json` rather than being merged into `{KEY}.json`. This is because the changelog is fetched from a separate paginated endpoint (`/rest/api/3/issue/{key}/changelog`), not from `?expand=changelog` — merging would synthesize a payload shape Jira would never actually return at this scale (the inline expansion caps at ~100 entries), which would mislead anyone debugging against the real API.

## Decision 2: Backfill missing `.changelog.md` even under `--incremental`

`--incremental` skips re-exporting issues whose `updated` timestamp hasn't moved. That logic was designed when the only artifact was the issue payload. With a second artifact in play, a naive implementation produces a surprising failure mode: a user runs an incremental export, adds `--include-changelog` on a later run, and gets a half-populated export where unchanged issues have no `.changelog.md` and nothing warns them.

The fix: when `--include-changelog` is on, fetch the changelog if **either** the issue was re-fetched **or** `.changelog.md` is missing. The issue payload itself is not re-fetched in the missing-artifact case — only the changelog. Once the file exists, subsequent incremental runs skip cheaply. This is a one-time backfill cost in exchange for not requiring users to discover and apply `--force` to make the new flag take effect.

## Considered alternatives

- **Inline changelog section in `{KEY}.md`** — rejected because long-lived tickets would dwarf the description, hurting the main artifact's signal-to-noise.
- **Merging changelog into `{KEY}.json`** — rejected because it would fabricate a Jira API shape that doesn't exist at full-changelog scale.
- **Documenting "use `--force` after enabling `--include-changelog`"** — rejected because the missing-artifact signal is unambiguous and the code can act on it correctly without burdening the user.
- **Always re-fetching changelog regardless of `--incremental`** — rejected because it negates the perf benefit of incremental for the most common combination (`--incremental --include-changelog`).
