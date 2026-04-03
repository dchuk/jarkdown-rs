# Jarkdown (Rust)

A Rust port of [jarkdown](https://github.com/dchuk/jarkdown) — export Jira Cloud issues to Markdown with attachments.

This crate provides both a **CLI tool** and an **importable library** for use in other Rust projects.

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
jarkdown = { git = "https://github.com/dchuk/jarkdown-rs" }
```

## Setup

Jarkdown needs three pieces of information to connect to your Jira instance:

1. **Jira domain** (e.g. `your-company.atlassian.net`)
2. **Jira email** (the email you log in with)
3. **Jira API token** — create one at [id.atlassian.com/manage-profile/security/api-tokens](https://id.atlassian.com/manage-profile/security/api-tokens)

### Interactive setup

```bash
jarkdown setup
```

This walks you through creating a `.env` file in the current directory.

### Manual setup

Create a `.env` file in the directory you'll run `jarkdown` from:

```
JIRA_DOMAIN=your-company.atlassian.net
JIRA_EMAIL=your-email@example.com
JIRA_API_TOKEN=your-api-token
```

Alternatively, set these as environment variables directly (e.g. in your shell profile) — no `.env` file needed.

## CLI Usage

```bash
# Export a single issue
jarkdown export PROJ-123
jarkdown PROJ-123                              # backward-compat shorthand

# Export to a specific directory
jarkdown export PROJ-123 --output ~/exports

# Bulk export
jarkdown bulk PROJ-1 PROJ-2 PROJ-3 --concurrency 5

# JQL query export
jarkdown query 'project = FOO AND status = Done' --limit 100

# Include raw JSON alongside Markdown
jarkdown export PROJ-123 --include-json

# Field filtering
jarkdown export PROJ-123 --include-fields "Story Points,Sprint"
jarkdown export PROJ-123 --exclude-fields "Internal Notes"

# Verbose logging
jarkdown export PROJ-123 --verbose
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
    );

    let keys = vec!["PROJ-1".into(), "PROJ-2".into(), "PROJ-3".into()];
    let (successes, failures) = exporter.export_bulk(&keys).await;

    println!("{} succeeded, {} failed", successes.len(), failures.len());
    Ok(())
}
```

## License

MIT
