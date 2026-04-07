[![Crates.io](https://img.shields.io/crates/v/jarkdown.svg)](https://crates.io/crates/jarkdown)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

# Jarkdown (Rust)

A fast, full-featured Rust CLI and library for exporting Jira Cloud issues to Markdown with attachments. Rust port of [jarkdown](https://github.com/dchuk/jarkdown).

This crate provides both a **CLI tool** (`jarkdown-rs`) and an **importable library** for use in other Rust projects.

> **Note:** The CLI binary is named `jarkdown-rs` to avoid conflicts with the Python [jarkdown](https://github.com/dchuk/jarkdown) package.

## Installation

### Homebrew (macOS)

```bash
brew install dchuk/tap/jarkdown-rs
```

### Shell installer (macOS / Linux)

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/dchuk/jarkdown-rs/releases/latest/download/jarkdown-rs-installer.sh | sh
```

### Prebuilt binaries

Download the latest binary for your platform from the [releases page](https://github.com/dchuk/jarkdown-rs/releases) and place it somewhere on your `PATH`.

| Platform | Archive |
|----------|---------|
| macOS (Apple Silicon) | `jarkdown-rs-aarch64-apple-darwin.tar.xz` |
| macOS (Intel) | `jarkdown-rs-x86_64-apple-darwin.tar.xz` |
| Linux (x86_64) | `jarkdown-rs-x86_64-unknown-linux-gnu.tar.xz` |
| Linux (ARM64) | `jarkdown-rs-aarch64-unknown-linux-gnu.tar.xz` |
| Windows (x86_64) | `jarkdown-rs-x86_64-pc-windows-msvc.zip` |

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
jarkdown = { git = "https://github.com/dchuk/jarkdown-rs" }
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

# Field filtering
jarkdown-rs export PROJ-123 --include-fields "Story Points,Sprint"
jarkdown-rs export PROJ-123 --exclude-fields "Internal Notes"

# Parallel attachment downloads
jarkdown-rs export PROJ-123 --attachment-concurrency 8

# Incremental export (skip unchanged issues)
jarkdown-rs bulk PROJ-1 PROJ-2 PROJ-3 --incremental
jarkdown-rs bulk PROJ-1 PROJ-2 PROJ-3 --incremental --force  # override skip

# Hierarchical export (epics, JPD ideas, and children — works with any command)
jarkdown-rs export EPIC-123 --hierarchy
jarkdown-rs export EPIC-123 --hierarchy --max-depth 3 --max-issues 500
jarkdown-rs bulk EPIC-1 EPIC-2 --hierarchy
jarkdown-rs query 'type = Epic AND project = FOO' --hierarchy

# JPD Idea → delivery items (follows "is implemented by" links)
jarkdown-rs export IDEA-42 --hierarchy --max-depth 3

# Verbose logging
jarkdown-rs export PROJ-123 --verbose
```

## CLI Defaults Reference

| Flag | Applies To | Default |
|------|-----------|---------|
| `--output` | all | current directory |
| `--verbose` | all | off |
| `--refresh-fields` | all | off |
| `--include-fields` | all | none (all fields) |
| `--exclude-fields` | all | none |
| `--include-json` | all | off |
| `--concurrency` | bulk, query | 3 |
| `--max-results` | query | 50 |
| `--batch-name` | bulk, query | none |
| `--attachment-concurrency` | all | 4 |
| `--incremental` | all | off |
| `--force` | all | off |
| `--hierarchy` | all | off |
| `--max-depth` | all (with `--hierarchy`) | 2 |
| `--max-issues` | all (with `--hierarchy`) | 200 |

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

### Bulk / Query Export

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
            ..Default::default()
        },
    ).await?;

    Ok(())
}
```

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
    );

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

In `--hierarchy` mode, jarkdown-rs follows the full chain: Idea → Epics → Stories/Tasks → Subtasks, producing a nested directory tree with an index.

## Requirements

- **Rust 2021 edition** (for building from source)
- **Jira Cloud** instance (Server/Data Center not supported)
- **Jira API token** — [create one here](https://id.atlassian.com/manage-profile/security/api-tokens)

## Limitations

- **Jira Cloud only** — Server and Data Center instances are not supported
- Attachment downloads are sequential by default (use `--attachment-concurrency` to parallelize)
- No webhook/real-time sync — exports are point-in-time snapshots

## Roadmap

- [x] Parallel attachment downloads (`--attachment-concurrency`)
- [x] Incremental/delta export (`--incremental`)
- [ ] Alternative output formats (PDF, HTML, Confluence wiki)
- [x] Hierarchical export — epics and JPD ideas with child issues (`--hierarchy` flag)

## Contributing

```bash
git clone https://github.com/dchuk/jarkdown-rs.git
cd jarkdown-rs
cargo build
cargo test
cargo clippy -- -D warnings
```

PRs welcome! Please ensure `cargo clippy` and `cargo test` pass before submitting.

## License

MIT
