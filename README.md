[![Crates.io](https://img.shields.io/crates/v/jarkdown.svg)](https://crates.io/crates/jarkdown)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

# Jarkdown (Rust)

A fast, full-featured Rust CLI and library for exporting Jira Cloud issues to Markdown with attachments. Rust port of [jarkdown](https://github.com/dchuk/jarkdown).

This crate provides both a **CLI tool** (`jarkdown-rs`) and an **importable library** for use in other Rust projects.

> **Note:** The CLI binary is named `jarkdown-rs` to avoid conflicts with the Python [jarkdown](https://github.com/dchuk/jarkdown) package.
> Homebrew and crates.io package names are `jarkdown`, but the installed
> executable is still `jarkdown-rs`.

## Installation

### Homebrew (macOS)

```bash
brew install dchuk/tap/jarkdown
```

### Shell installer (macOS / Linux)

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/dchuk/jarkdown-rs/releases/latest/download/jarkdown-installer.sh | sh
```

### Prebuilt binaries

Download the latest binary for your platform from the [releases page](https://github.com/dchuk/jarkdown-rs/releases) and place it somewhere on your `PATH`.

| Platform | Archive |
|----------|---------|
| macOS (Apple Silicon) | `jarkdown-aarch64-apple-darwin.tar.xz` |
| macOS (Intel) | `jarkdown-x86_64-apple-darwin.tar.xz` |
| Linux (x86_64) | `jarkdown-x86_64-unknown-linux-gnu.tar.xz` |
| Linux (ARM64) | `jarkdown-aarch64-unknown-linux-gnu.tar.xz` |
| Windows (x86_64) | `jarkdown-x86_64-pc-windows-msvc.zip` |

### From crates.io

```bash
cargo install jarkdown
```

### From source

```bash
git clone https://github.com/dchuk/jarkdown-rs.git
cd jarkdown-rs
cargo install --path .
```

### As a library dependency

```toml
[dependencies]
jarkdown = "1.7"
```

## Setup

Jarkdown needs three pieces of information to connect to your Jira instance:

1. **Jira domain** (e.g. `your-company.atlassian.net`)
2. **Jira email** (the email you log in with)
3. **Jira API token** — create one at [id.atlassian.com/manage-profile/security/api-tokens](https://id.atlassian.com/manage-profile/security/api-tokens)

### Interactive setup

```bash
jarkdown-rs setup
```

This walks you through creating a `.env` file in the current directory.

### Manual setup

Create a `.env` file in the directory you'll run `jarkdown-rs` from:

```
JIRA_DOMAIN=your-company.atlassian.net
JIRA_EMAIL=your-email@example.com
JIRA_API_TOKEN=your-api-token
```

Alternatively, set these as environment variables directly (e.g. in your shell profile) — no `.env` file needed.

## CLI Usage

```bash
# Export a single issue
jarkdown-rs export PROJ-123
jarkdown-rs PROJ-123                              # backward-compat shorthand

# Export to a specific directory
jarkdown-rs export PROJ-123 --output ~/exports

# Bulk export
jarkdown-rs bulk PROJ-1 PROJ-2 PROJ-3 --concurrency 5

# JQL query export
jarkdown-rs query 'project = FOO AND status = Done' --limit 100

# Include raw JSON alongside Markdown
jarkdown-rs export PROJ-123 --include-json

# Preserve attachment metadata but skip binary downloads
jarkdown-rs export PROJ-123 --no-attachments --include-json

# Field filtering
jarkdown-rs export PROJ-123 --include-fields "Story Points,Sprint"
jarkdown-rs export PROJ-123 --exclude-fields "Internal Notes"

# Parallel attachment downloads
jarkdown-rs export PROJ-123 --attachment-concurrency 8
jarkdown-rs export PROJ-123 --attachment-concurrency 0  # serial downloads

# Incremental export (skip unchanged issues)
jarkdown-rs bulk PROJ-1 PROJ-2 PROJ-3 --incremental
jarkdown-rs bulk PROJ-1 PROJ-2 PROJ-3 --incremental --force  # override skip
jarkdown-rs bulk PROJ-1 PROJ-2 PROJ-3 --incremental --manifest ./cache/jarkdown.json

# Hierarchical export (epics, JPD ideas, and children — works with any command)
jarkdown-rs export EPIC-123 --hierarchy
jarkdown-rs export EPIC-123 --hierarchy --max-depth 3 --max-issues 500
jarkdown-rs export EPIC-123 --hierarchy --hierarchy-layout nested
jarkdown-rs bulk EPIC-1 EPIC-2 --hierarchy
jarkdown-rs query 'type = Epic AND project = FOO' --hierarchy

# Export the full Jira changelog (audit trail of field changes) as a sibling file
jarkdown-rs export PROJ-123 --include-changelog
jarkdown-rs bulk PROJ-1 PROJ-2 PROJ-3 --include-changelog --incremental

# Print matching keys without exporting files
jarkdown-rs query 'project = FOO AND status = Done' --keys-only

# Bound a slow issue during bulk-style exports
jarkdown-rs bulk PROJ-1 PROJ-2 PROJ-3 --issue-timeout-seconds 300

# JPD Idea → delivery items (follows "is implemented by" links)
jarkdown-rs export IDEA-42 --hierarchy --max-depth 3

# Verbose logging
jarkdown-rs export PROJ-123 --verbose
```

## CLI Defaults Reference

| Flag | Applies To | Default |
|------|-----------|---------|
| `--output` | all | current directory |
| `--manifest` | all | `<output>/.jarkdown-manifest.json` |
| `--verbose` | all | off |
| `--refresh-fields` | all | off |
| `--include-fields` | all | none (all fields) |
| `--exclude-fields` | all | none |
| `--include-json` | all | off |
| `--no-attachments` | all | off |
| `--issue-timeout-seconds` | all | 300 |
| `--concurrency` | bulk, query | 3 |
| `--max-results` | query | 50 |
| `--batch-name` | bulk, query | none |
| `--attachment-concurrency` | all | 4 (`0` means serial; use `--no-attachments` to skip downloads) |
| `--incremental` | all | off |
| `--force` | all | off |
| `--hierarchy` | all | off |
| `--hierarchy-layout` | all (with `--hierarchy`) | `corpus` for `--incremental --hierarchy`; legacy `nested` otherwise |
| `--max-depth` | all (with `--hierarchy`) | 2 |
| `--max-issues` | all (with `--hierarchy`) | 200 |
| `--include-changelog` | all | off (writes `{KEY}.changelog.md`; see [ADR-0001](docs/adr/0001-changelog-export.md)) |
| `--keys-only` | query | off |

## Output Structure

### Single Issue

```
PROJ-123/
├── PROJ-123.md
├── screenshot.png
└── design-doc.pdf
```

### With `--include-json`

```
PROJ-123/
├── PROJ-123.md
├── PROJ-123.json
├── screenshot.png
└── design-doc.pdf
```

### With `--include-changelog`

The changelog (Jira's audit trail of field changes) is exported as a sibling
file rather than inlined, so long-lived issues don't drown out the description.
The main `.md` cross-references it via a `changelog:` frontmatter key and a
`## Changelog` section. See [ADR-0001](docs/adr/0001-changelog-export.md) for
the rationale and the one-time backfill behaviour under `--incremental`.

```
PROJ-123/
├── PROJ-123.md
├── PROJ-123.changelog.md
└── screenshot.png
```

With `--include-json` set as well, the changelog also lands in a parallel
`PROJ-123.changelog.json` rather than being merged into `PROJ-123.json`.

### Bulk / Query Export

Requested Issue keys are normalized before writing artifacts, so `proj-1` and
`PROJ-1` both write to the canonical `PROJ-1/` directory.

```
output/
├── index.md
├── PROJ-1/
│   ├── PROJ-1.md
│   └── attachment.png
├── PROJ-2/
│   └── PROJ-2.md
└── PROJ-3/
    ├── PROJ-3.md
    └── spec.pdf
```

### Incremental Manifest

`--incremental` writes a manifest v2 cache index at
`<output>/.jarkdown-manifest.json` by default. Use `--manifest <path>` when
the cache state should live somewhere else. Artifact paths inside the manifest
remain relative to the export output root, not to the manifest file.
When loading a manifest, jarkdown warns if it sees an older case-mismatched
Issue directory such as `proj-1/`; it leaves legacy directories in place.

Manifest v2 tracks validated Jira `updated` timestamps, content-visible option
fingerprints, evicted Issue tombstones, hierarchy graph edges, requested roots,
and every active artifact path used by nested hierarchy exports. This lets
unchanged exports skip full Issue fetches while still repairing missing JSON or
changelog sidecars. See [`docs/manifest-v2.md`](docs/manifest-v2.md) for the
cache format and safety rules.

Incremental validation is conservative:

- successful Jira validation omissions mark active cached Issues as evicted
  without deleting their files;
- failed validation requests do not evict anything;
- parsed Jira `updated` timestamps drive freshness, with string comparison used
  only when parsing fails;
- changing content-visible options such as `--include-json`,
  `--include-changelog`, field filters, `--no-attachments`, `--max-depth`, or
  `--max-issues` invalidates stale cached artifacts.

### Hierarchy Layouts

Incremental hierarchy export defaults to `corpus`, which writes one canonical
directory per Issue plus a `{ROOT}.hierarchy.md` snapshot:

```
output/
├── EPIC-123/
│   └── EPIC-123.md
├── STORY-1/
│   └── STORY-1.md
└── EPIC-123.hierarchy.md
```

Use `--hierarchy-layout nested` when you want the older browsable tree layout:

```
output/
├── EPIC-123.hierarchy.md
└── EPIC-123/
    ├── EPIC-123.md
    └── STORY-1/
        └── STORY-1.md
```

Nested incremental exports record every active path for shared Issues, so a
changed shared Issue can be refreshed everywhere it appears.
Older nested exports wrote a shared `index.md` root snapshot; jarkdown warns
when a manifest still references that legacy snapshot and leaves it in place.

## Markdown Format

Each exported issue produces a Markdown file with YAML frontmatter:

```markdown
---
key: PROJ-123
summary: Implement user authentication
status: In Progress
type: Story
priority: High
assignee: Jane Smith
created: 2024-01-15
updated: 2024-01-20
---

# PROJ-123: Implement user authentication

**Status:** In Progress | **Type:** Story | **Priority:** High

## Description

The rendered description content goes here...

## Comments

### Jane Smith — 2024-01-16

Comment content here...

## Attachments

- [screenshot.png](screenshot.png) (245.3 KB)
- [design-doc.pdf](design-doc.pdf) (1.2 MB)

## Custom Fields

| Field | Value |
|-------|-------|
| Story Points | 5 |
| Sprint | Sprint 23 |
```

## Configuration

Optional `.jarkdown.toml` in the working directory for persistent field filtering:

```toml
[fields]
include = ["Story Points", "Sprint"]  # only export these custom fields
exclude = ["Internal Notes", "Dev Notes"]  # or exclude specific fields
```

CLI flags (`--include-fields`, `--exclude-fields`) override the config file.

The CLI field filters are comma-separated. Field names that contain commas cannot
be represented as a single CLI value today, so export all fields or use
`.jarkdown.toml` array values when you need exact names such as
`Plus, Enterprise, or Both? (G)`.

## Library Usage

```rust
use jarkdown::{JiraApiClient, export_issue, ExportOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = JiraApiClient::new(
        "company.atlassian.net",
        "user@example.com",
        "your-api-token",
    )?;

    // Simple single-issue export
    let output = export_issue(
        &client,
        "PROJ-123",
        None,  // uses current directory
        ExportOptions::default(),
    ).await?;
    println!("Exported to {:?}", output);

    // With options
    let output = export_issue(
        &client,
        "PROJ-456",
        Some(std::path::Path::new("./exports")),
        ExportOptions {
            include_json: true,
            refresh_fields: true,
            no_attachments: true,
            ..Default::default()
        },
    ).await?;

    Ok(())
}
```

### Library access to `--include-changelog`

The high-level `ExportOptions` deliberately stays minimal. To opt into the
changelog from library code, drop one level down to
`perform_export_with_options` and pass `include_changelog: true`:

```rust
use jarkdown::{JiraApiClient, perform_export_with_options, ExportWorkflowOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = JiraApiClient::new("company.atlassian.net", "user@example.com", "token")?;

    perform_export_with_options(
        &client,
        "PROJ-123",
        std::path::Path::new("./exports/PROJ-123"),
        ExportWorkflowOptions {
            include_changelog: true,
            include_json: true, // also writes PROJ-123.changelog.json
            ..Default::default()
        },
    )
    .await?;
    Ok(())
}
```

`BulkExporter` exposes the same flag via the `.with_include_changelog(true)`
builder method (see the bulk example below).

### Bulk export via library

```rust
use std::collections::HashMap;
use jarkdown::{JiraApiClient, BulkExporter};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = JiraApiClient::new("company.atlassian.net", "user@example.com", "token")?;

    let exporter = BulkExporter::new(
        client.clone(),
        /* concurrency */ 5,
        Some("./exports"),
        None,   // batch_name
        false,  // refresh_fields
        None,   // include_fields
        None,   // exclude_fields
        false,  // include_json
        4,      // attachment_concurrency
        false,  // incremental
        false,  // force
    )
    .with_no_attachments(true)
    .with_issue_timeout_seconds(300);

    let keys = vec!["PROJ-1".into(), "PROJ-2".into(), "PROJ-3".into()];
    let (successes, failures) = exporter.export_bulk(&keys).await;

    println!("{} succeeded, {} failed", successes.len(), failures.len());
    Ok(())
}
```

## Jira Product Discovery (JPD) Support

Jarkdown supports exporting JPD Idea tickets and their delivery hierarchy. JPD links Ideas to delivery items (Epics, Stories, Tasks) via "implements" / "is implemented by" Polaris links.

```bash
# Export an Idea and all its delivery items
jarkdown-rs export IDEA-42 --hierarchy --max-depth 3

# Single export shows delivery items in the Child Issues section
jarkdown-rs export IDEA-42
```

In `--hierarchy` mode, jarkdown-rs follows the full chain: Idea → Epics →
Stories/Tasks → Subtasks. Non-incremental hierarchy exports default to the
legacy nested tree; nested and corpus layouts both write root snapshots as
`{ROOT}.hierarchy.md`. Incremental hierarchy exports default to the corpus
layout for better shared-cache behavior. Use `--hierarchy-layout nested` or
`--hierarchy-layout corpus` to choose explicitly.

### Archived ideas

JPD archiving is a locked custom field ("Idea archived": empty or `Yes`), not
a Jira status. JPD views in the UI always hide archived ideas, but JQL/REST
search returns them by default — so a project-wide query can return **more**
issues than the JPD view shows (e.g. the API returns 75 ideas while the view
shows 50). That gap is archived ideas.

jarkdown never rewrites your JQL. Instead, exported frontmatter marks each
archived idea:

```yaml
archived: true
archived_on: 2026-06-12
archived_by: Priya Patel
```

Live ideas (and non-JPD issues) carry none of these keys — absence means "not
archived". The recommended sync recipe is therefore to query the **whole
project** and let consumers filter on the `archived` frontmatter key:

```bash
# Living mirror: archived ideas stay in the folder, marked in frontmatter.
# Archiving an idea re-exports it with the marker on the next incremental run.
jarkdown-rs query "project = PSOP" --incremental --manifest .jarkdown-manifest.json
```

Excluding archived ideas in JQL also works when you want UI-equivalent result
sets, at a cost: an idea archived *after* its first export drops out of the
query and its exported file silently goes stale.

```bash
# Match what the JPD view shows (archived hidden)
jarkdown-rs query 'project = PSOP AND "Idea archived" IS EMPTY'

# Only archived ideas
jarkdown-rs query 'project = PSOP AND "Idea archived" = Yes'
```

The archiving fields' `customfield_*` ids differ per site; jarkdown resolves
them by display name from the cached field metadata (refresh with
`--refresh-fields` if archived state is missing from exports). See
`docs/adr/0005-archived-ideas-are-marked-not-filtered.md` for the full design.

## Requirements

- **Rust 2021 edition** (for building from source)
- **Jira Cloud** instance (Server/Data Center not supported)
- **Jira API token** — [create one here](https://id.atlassian.com/manage-profile/security/api-tokens)

## Limitations

- **Jira Cloud only** — Server and Data Center instances are not supported
- Attachment downloads are bounded by `--attachment-concurrency`; `0` means serial downloads, and `--no-attachments` skips binary downloads entirely.
- CLI field filters split on commas, so use `.jarkdown.toml` array values or downstream JSON filtering for field names that contain commas.
- No webhook/real-time sync — exports are point-in-time snapshots

## Roadmap

- [x] Parallel attachment downloads (`--attachment-concurrency`)
- [x] Incremental/delta export (`--incremental`)
- [x] Hierarchical export — epics and JPD ideas with child issues (`--hierarchy` flag)
- [x] Full changelog export — audit trail of field changes (`--include-changelog` flag)
- [ ] Alternative output formats (PDF, HTML, Confluence wiki)

## Contributing

```bash
git clone https://github.com/dchuk/jarkdown-rs.git
cd jarkdown-rs
cargo build
cargo test
cargo clippy -- -D warnings
```

PRs welcome! Please ensure `cargo clippy` and `cargo test` pass before submitting.

### Internal docs

- [`docs/architecture.md`](docs/architecture.md) — layered architecture with
  MermaidJS diagrams for the overall system, single-issue export flow, bulk
  concurrency, hierarchy traversal, child-aware incremental validation, the
  pure render pipeline, the typed/raw `Issue` duality, and the attachment
  pipeline.
- [`CONTEXT.md`](CONTEXT.md) — domain glossary (Issue, Changelog, Comment,
  Worklog) and the terms code/docs should standardize on.
- [`CHANGELOG.md`](CHANGELOG.md) — release notes for users installing from
  crates.io, Homebrew, shell installers, or GitHub Releases.
- [`docs/adr/`](docs/adr/) — Architecture Decision Records covering
  non-obvious design choices including changelog export shape, typed `Issue`
  parsing, validation metadata, and child-aware incremental hierarchy caching.

## License

MIT
