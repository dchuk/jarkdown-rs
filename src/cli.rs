//! CLI argument parsing using clap, matching the Python implementation exactly.

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser, Debug)]
#[command(
    name = "jarkdown-rs",
    about = "Export Jira issues to Markdown with attachments",
    version,
    after_help = r#"Examples:
  jarkdown-rs export PROJ-123
  jarkdown-rs PROJ-123                              # backward-compat form
  jarkdown-rs export PROJ-123 --output ~/Documents/jira-exports
  jarkdown-rs bulk PROJ-1 PROJ-2 PROJ-3
  jarkdown-rs query 'project = FOO AND status = Done'
  jarkdown-rs setup

Environment variables:
  JIRA_DOMAIN     - Your Jira domain (e.g., your-company.atlassian.net)
  JIRA_EMAIL      - Your Jira account email
  JIRA_API_TOKEN  - Your Jira API token"#
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Export a single Jira issue to Markdown
    Export(ExportArgs),

    /// Export multiple Jira issues by key
    Bulk(BulkArgs),

    /// Export Jira issues matching a JQL query
    Query(QueryArgs),

    /// Interactive setup to configure Jira credentials
    Setup,
}

/// Shared flags inherited by all export subcommands.
#[derive(Parser, Debug, Clone)]
pub struct SharedArgs {
    /// Output directory (default: current directory)
    #[arg(short, long)]
    pub output: Option<String>,

    /// Manifest file path for incremental export state
    #[arg(long)]
    pub manifest: Option<String>,

    /// Enable verbose logging
    #[arg(short, long)]
    pub verbose: bool,

    /// Force refresh of cached Jira field metadata
    #[arg(long)]
    pub refresh_fields: bool,

    /// Comma-separated list of custom field names to include
    #[arg(long)]
    pub include_fields: Option<String>,

    /// Comma-separated list of custom field names to exclude
    #[arg(long)]
    pub exclude_fields: Option<String>,

    /// Save the raw Jira API JSON response alongside the Markdown file
    #[arg(long)]
    pub include_json: bool,

    /// Maximum concurrent attachment downloads; 0 downloads serially (default: 4)
    #[arg(long, default_value = "4")]
    pub attachment_concurrency: usize,

    /// Skip attachment binary downloads while preserving attachment metadata
    #[arg(long)]
    pub no_attachments: bool,

    /// Maximum seconds to spend exporting one issue before timing out
    #[arg(long, default_value = "300")]
    pub issue_timeout_seconds: u64,

    /// Only re-export issues that have changed since last export
    #[arg(long)]
    pub incremental: bool,

    /// Force re-export even if issue is unchanged (overrides --incremental)
    #[arg(long)]
    pub force: bool,

    /// Recursively export child issues (subtasks, epic children, JPD delivery items, linked issues)
    #[arg(long)]
    pub hierarchy: bool,

    /// Hierarchy output layout; requires --hierarchy
    #[arg(long, value_enum, requires = "hierarchy")]
    pub hierarchy_layout: Option<HierarchyLayoutArg>,

    /// Maximum depth to recurse into child issues (requires --hierarchy)
    #[arg(long, default_value = "2")]
    pub max_depth: u32,

    /// Maximum total issues to export in hierarchy mode (safety cap, requires --hierarchy)
    #[arg(long, default_value = "200")]
    pub max_issues: u32,

    /// Also export the full paginated changelog (audit trail of field changes)
    /// to a sibling `{KEY}.changelog.md` file.
    #[arg(long)]
    pub include_changelog: bool,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub enum HierarchyLayoutArg {
    Corpus,
    Nested,
}

#[derive(Parser, Debug)]
pub struct ExportArgs {
    /// Jira issue key (e.g., PROJ-123)
    pub issue_key: String,

    #[command(flatten)]
    pub shared: SharedArgs,
}

#[derive(Parser, Debug)]
pub struct BulkArgs {
    /// One or more Jira issue keys (e.g., PROJ-1 PROJ-2 PROJ-3)
    pub issue_keys: Vec<String>,

    /// Maximum number of issues to export
    #[arg(long)]
    pub max_results: Option<u32>,

    /// Optional name for output batch directory wrapper
    #[arg(long)]
    pub batch_name: Option<String>,

    /// Maximum concurrent exports (default: 3)
    #[arg(long, default_value = "3")]
    pub concurrency: usize,

    #[command(flatten)]
    pub shared: SharedArgs,
}

#[derive(Parser, Debug)]
pub struct QueryArgs {
    /// JQL query string (e.g., 'project = FOO AND status = Done')
    pub jql: String,

    /// Maximum number of issues to export (default: 50)
    #[arg(long, alias = "limit", default_value = "50")]
    pub max_results: u32,

    /// Optional name for output batch directory wrapper
    #[arg(long)]
    pub batch_name: Option<String>,

    /// Maximum concurrent exports (default: 3)
    #[arg(long, default_value = "3")]
    pub concurrency: usize,

    /// Print matching issue keys and skip file export
    #[arg(long)]
    pub keys_only: bool,

    #[command(flatten)]
    pub shared: SharedArgs,
}

/// Backward-compat shim: if argv[1] looks like an issue key (e.g. PROJ-123),
/// inject "export" so that `jarkdown PROJ-123` works the same as `jarkdown export PROJ-123`.
pub fn preprocess_args() -> Vec<String> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        let re = regex::Regex::new(r"^[A-Z]+-\d+$").unwrap();
        if re.is_match(&args[1]) {
            let mut new_args = vec![args[0].clone(), "export".to_string()];
            new_args.extend(args[1..].iter().cloned());
            return new_args;
        }
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn manifest_flag_parses_for_export_bulk_and_query() {
        let export = Cli::parse_from([
            "jarkdown-rs",
            "export",
            "PROJ-1",
            "--manifest",
            "cache/state.json",
        ]);
        match export.command.unwrap() {
            Command::Export(args) => {
                assert_eq!(args.shared.manifest.as_deref(), Some("cache/state.json"))
            }
            _ => panic!("expected export command"),
        }

        let bulk = Cli::parse_from([
            "jarkdown-rs",
            "bulk",
            "PROJ-1",
            "--manifest",
            "cache/state.json",
        ]);
        match bulk.command.unwrap() {
            Command::Bulk(args) => {
                assert_eq!(args.shared.manifest.as_deref(), Some("cache/state.json"))
            }
            _ => panic!("expected bulk command"),
        }

        let query = Cli::parse_from([
            "jarkdown-rs",
            "query",
            "project = PROJ",
            "--manifest",
            "cache/state.json",
        ]);
        match query.command.unwrap() {
            Command::Query(args) => {
                assert_eq!(args.shared.manifest.as_deref(), Some("cache/state.json"))
            }
            _ => panic!("expected query command"),
        }
    }

    #[test]
    fn hierarchy_layout_requires_hierarchy_and_accepts_corpus_or_nested() {
        let err = Cli::try_parse_from([
            "jarkdown-rs",
            "export",
            "PROJ-1",
            "--hierarchy-layout",
            "corpus",
        ])
        .expect_err("layout without hierarchy should fail");
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);

        let export = Cli::parse_from([
            "jarkdown-rs",
            "export",
            "PROJ-1",
            "--hierarchy",
            "--hierarchy-layout",
            "corpus",
        ]);
        match export.command.unwrap() {
            Command::Export(args) => {
                assert_eq!(
                    args.shared.hierarchy_layout,
                    Some(HierarchyLayoutArg::Corpus)
                )
            }
            _ => panic!("expected export command"),
        }

        let bulk = Cli::parse_from([
            "jarkdown-rs",
            "bulk",
            "PROJ-1",
            "--hierarchy",
            "--hierarchy-layout",
            "nested",
        ]);
        match bulk.command.unwrap() {
            Command::Bulk(args) => {
                assert_eq!(
                    args.shared.hierarchy_layout,
                    Some(HierarchyLayoutArg::Nested)
                )
            }
            _ => panic!("expected bulk command"),
        }
    }
}
