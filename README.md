# Jarkdown (Rust)

A Rust port of [jarkdown](https://github.com/dchuk/jarkdown) — export Jira Cloud issues to Markdown with attachments.

This crate provides both a **CLI tool** and an **importable library** for use in other Rust projects.

## Installation

### From source

```bash
cargo install --path .
```

### As a dependency in your Rust project

```toml
[dependencies]
jarkdown = { git = "https://github.com/dchuk/jarkdown-rs" }
```

## CLI Usage

The CLI interface is identical to the Python version:

```bash
# Interactive setup
jarkdown setup

# Export a single issue
jarkdown export PROJ-123
jarkdown PROJ-123                              # backward-compat shorthand

# Export to a specific directory
jarkdown export PROJ-123 --output ~/exports

# Bulk export
jarkdown bulk PROJ-1 PROJ-2 PROJ-3 --concurrency 5

# JQL query export
jarkdown query 'project = FOO AND status = Done' --limit 100

# Other flags
jarkdown export PROJ-123 --include-json --verbose
jarkdown export PROJ-123 --include-fields "Story Points,Sprint"
jarkdown export PROJ-123 --exclude-fields "Internal Notes"
```

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

## Configuration

Same as the Python version — create a `.env` file or run `jarkdown setup`:

```
JIRA_DOMAIN=your-company.atlassian.net
JIRA_EMAIL=your-email@example.com
JIRA_API_TOKEN=your-api-token
```

Optional `.jarkdown.toml` for field filtering:

```toml
[fields]
exclude = ["Internal Notes", "Dev Notes"]
```

## Crate Dependencies

| Purpose | Crate |
|---------|-------|
| Async runtime | `tokio` |
| HTTP client | `reqwest` |
| CLI parsing | `clap` (derive) |
| HTML → Markdown | `html2md` |
| JSON | `serde_json` |
| YAML frontmatter | `serde_yaml` |
| .env files | `dotenvy` |
| TOML config | `toml` |
| XDG directories | `dirs` |
| Regex | `regex` |
| URL encoding | `urlencoding` |
| Error handling | `thiserror` |
| Logging | `log` + `env_logger` |
| Date/time | `chrono` |
| Password input | `rpassword` |
| Retry/backoff | `rand` (jitter) |

## License

MIT
