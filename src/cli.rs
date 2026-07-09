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

    /// Write a machine-readable run summary (reexported/skipped/failed issue
    /// keys) as JSON to this path. Flat bulk/query only.
    #[arg(long)]
    pub summary_json: Option<String>,
}

impl SharedArgs {
    /// Warnings for flags that have no effect given the rest of this invocation.
    ///
    /// Emitting these on stderr prevents a flag from silently no-op'ing (which is
    /// what caused the original misdiagnosis of incremental export). `--manifest`
    /// is intentionally NOT listed: it now persists the manifest on its own.
    ///
    /// `summary_json_support` says whether the invoking command can write a
    /// run summary at all — `SharedArgs` is command-agnostic, but the single
    /// `export` command never writes one.
    pub fn ineffective_flag_warnings(
        &self,
        summary_json_support: SummaryJsonSupport,
    ) -> Vec<String> {
        let mut warnings = Vec::new();
        if self.force && !self.incremental {
            warnings.push(
                "--force has no effect without --incremental (there is nothing to override); ignoring it".to_string(),
            );
        }
        if self.summary_json.is_some() {
            match summary_json_support {
                SummaryJsonSupport::SingleExport => warnings.push(
                    "--summary-json is only written by the bulk and query commands; no summary will be written for a single export".to_string(),
                ),
                SummaryJsonSupport::BulkOrQuery if self.hierarchy => warnings.push(
                    "--summary-json is not supported with --hierarchy; no summary will be written"
                        .to_string(),
                ),
                SummaryJsonSupport::BulkOrQuery => {}
            }
        }
        warnings
    }
}

/// Whether the invoking command can write a `--summary-json` run summary.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SummaryJsonSupport {
    /// `bulk` / `query`: summaries are written on the flat path.
    BulkOrQuery,
    /// Single `export`: never writes a summary.
    SingleExport,
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

    #[test]
    fn force_without_incremental_is_reported_as_ineffective() {
        let args = SharedArgs::try_parse_from(["x", "--force"]).expect("parse");
        let warnings = args.ineffective_flag_warnings(SummaryJsonSupport::BulkOrQuery);
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("--force") && w.contains("--incremental")),
            "expected a warning that --force needs --incremental, got {:?}",
            warnings
        );

        let with_incremental =
            SharedArgs::try_parse_from(["x", "--force", "--incremental"]).expect("parse");
        assert!(
            with_incremental
                .ineffective_flag_warnings(SummaryJsonSupport::BulkOrQuery)
                .is_empty(),
            "no warning expected when --incremental is present"
        );
    }

    #[test]
    fn summary_json_with_hierarchy_is_reported_as_ineffective() {
        let args = SharedArgs::try_parse_from(["x", "--summary-json", "out.json", "--hierarchy"])
            .expect("parse");
        assert!(
            args.ineffective_flag_warnings(SummaryJsonSupport::BulkOrQuery)
                .iter()
                .any(|w| w.contains("--summary-json") && w.contains("--hierarchy")),
            "expected a warning that --summary-json is unsupported with --hierarchy"
        );

        let flat = SharedArgs::try_parse_from(["x", "--summary-json", "out.json"]).expect("parse");
        assert!(
            flat.ineffective_flag_warnings(SummaryJsonSupport::BulkOrQuery)
                .is_empty(),
            "no warning for --summary-json on the flat path"
        );
    }

    /// `--summary-json` on the single `export` command never writes a summary
    /// (flat or hierarchy) and must say so instead of silently no-op'ing —
    /// the same principle as the other ineffective-flag warnings (issue #50).
    #[test]
    fn summary_json_on_single_export_is_reported_as_ineffective() {
        let args = SharedArgs::try_parse_from(["x", "--summary-json", "out.json"]).expect("parse");
        let warnings = args.ineffective_flag_warnings(SummaryJsonSupport::SingleExport);
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("--summary-json") && w.contains("export")),
            "expected a warning that export never writes a summary, got {:?}",
            warnings
        );

        // With --hierarchy on export, the single-export message still applies
        // (one warning, not two).
        let hierarchy =
            SharedArgs::try_parse_from(["x", "--summary-json", "out.json", "--hierarchy"])
                .expect("parse");
        let warnings = hierarchy.ineffective_flag_warnings(SummaryJsonSupport::SingleExport);
        assert_eq!(
            warnings
                .iter()
                .filter(|w| w.contains("--summary-json"))
                .count(),
            1,
            "expected exactly one --summary-json warning, got {:?}",
            warnings
        );

        let without_flag = SharedArgs::try_parse_from(["x"]).expect("parse");
        assert!(
            without_flag
                .ineffective_flag_warnings(SummaryJsonSupport::SingleExport)
                .is_empty(),
            "no warning without --summary-json"
        );
    }
}
