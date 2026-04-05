//! Jarkdown CLI entry point.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process;

use clap::Parser;
use log::info;

use jarkdown::bulk::BulkExporter;
use jarkdown::cli::{self, Cli, Command};
use jarkdown::export::perform_export;
use jarkdown::hierarchy::{HierarchyExporter, HierarchyOptions};
use jarkdown::jira_client::JiraApiClient;
use jarkdown::manifest::Manifest;

/// Load and validate Jira credentials from environment variables.
fn load_credentials() -> (String, String, String) {
    dotenvy::dotenv().ok();

    let domain = std::env::var("JIRA_DOMAIN").ok();
    let email = std::env::var("JIRA_EMAIL").ok();
    let api_token = std::env::var("JIRA_API_TOKEN").ok();

    // Check if .env file exists and no environment variables are set
    let env_path = std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".env");

    if !env_path.exists() && (domain.is_none() || email.is_none() || api_token.is_none()) {
        eprintln!("Error: Configuration file '.env' not found.");
        eprintln!();
        eprintln!("To set up your configuration, run: jarkdown setup");
        eprintln!("Or create a .env file manually with:");
        eprintln!("  JIRA_DOMAIN=your-company.atlassian.net");
        eprintln!("  JIRA_EMAIL=your-email@example.com");
        eprintln!("  JIRA_API_TOKEN=your-api-token");
        process::exit(1);
    }

    let mut missing = Vec::new();
    if domain.is_none() {
        missing.push("JIRA_DOMAIN");
    }
    if email.is_none() {
        missing.push("JIRA_EMAIL");
    }
    if api_token.is_none() {
        missing.push("JIRA_API_TOKEN");
    }

    if !missing.is_empty() {
        eprintln!(
            "Error: Missing required environment variables: {}",
            missing.join(", ")
        );
        eprintln!();
        eprintln!("To set up your configuration, run: jarkdown setup");
        eprintln!("Or add the missing variables to your .env file.");
        process::exit(1);
    }

    (
        domain.unwrap(),
        email.unwrap(),
        api_token.unwrap(),
    )
}

/// Interactive setup to create .env file with Jira credentials.
fn setup_configuration() {
    use std::io::{self, Write};

    println!();
    println!("=== Jarkdown Configuration Setup ===");
    println!();
    println!("This will help you create a .env file with your Jira credentials.");
    println!();
    println!("You'll need:");
    println!("1. Your Jira domain (e.g., company.atlassian.net)");
    println!("2. Your Jira email address");
    println!("3. A Jira API token");
    println!();
    println!("To create an API token:");
    println!("1. Go to https://id.atlassian.com/manage-profile/security/api-tokens");
    println!("2. Click 'Create API token'");
    println!("3. Give it a name and copy the token");
    println!();

    let env_path = std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".env");

    if env_path.exists() {
        print!(".env file already exists. Overwrite? (y/N): ");
        io::stdout().flush().ok();
        let mut response = String::new();
        io::stdin().read_line(&mut response).ok();
        if response.trim().to_lowercase() != "y" {
            println!("Setup cancelled.");
            return;
        }
    }

    // Collect information
    print!("\nJira domain (e.g., company.atlassian.net): ");
    io::stdout().flush().ok();
    let mut domain = String::new();
    io::stdin().read_line(&mut domain).ok();
    let mut domain = domain.trim().to_string();

    if domain.is_empty() {
        eprintln!("Error: Domain is required");
        process::exit(1);
    }

    // Remove protocol prefix if provided
    if domain.starts_with("https://") {
        domain = domain[8..].to_string();
    } else if domain.starts_with("http://") {
        domain = domain[7..].to_string();
    }

    print!("Jira email address: ");
    io::stdout().flush().ok();
    let mut email = String::new();
    io::stdin().read_line(&mut email).ok();
    let email = email.trim().to_string();

    if email.is_empty() {
        eprintln!("Error: Email is required");
        process::exit(1);
    }

    // Use rpassword for hidden input
    let api_token = rpassword::prompt_password("Jira API token (hidden): ")
        .unwrap_or_default()
        .trim()
        .to_string();

    if api_token.is_empty() {
        eprintln!("Error: API token is required");
        process::exit(1);
    }

    // Write .env file
    let content = format!(
        "JIRA_DOMAIN={}\nJIRA_EMAIL={}\nJIRA_API_TOKEN={}\n",
        domain, email, api_token
    );

    match std::fs::write(&env_path, content) {
        Ok(_) => {
            println!();
            println!("Configuration saved to {:?}", env_path);
            println!("You can now run: jarkdown export ISSUE-KEY");
        }
        Err(e) => {
            eprintln!("Error writing .env file: {}", e);
            process::exit(1);
        }
    }
}

fn print_summary(successes: &[jarkdown::ExportResult], failures: &[jarkdown::ExportResult]) {
    let total = successes.len() + failures.len();
    eprintln!(
        "\nExport complete: {}/{} succeeded, {} failed.",
        successes.len(),
        total,
        failures.len()
    );
    if !failures.is_empty() {
        eprintln!("\nFailed issues:");
        for result in failures {
            eprintln!(
                "  {}: {}",
                result.issue_key,
                result.error.as_deref().unwrap_or("Unknown error")
            );
        }
    }
}

fn init_logging(verbose: bool) {
    env_logger::Builder::new()
        .filter_level(if verbose {
            log::LevelFilter::Debug
        } else {
            log::LevelFilter::Info
        })
        .format_target(false)
        .format_timestamp(None)
        .init();
}

async fn handle_export(args: jarkdown::cli::ExportArgs) {
    init_logging(args.shared.verbose);

    let (domain, email, api_token) = load_credentials();
    let client = match JiraApiClient::new(&domain, &email, &api_token) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: {}", e);
            process::exit(1);
        }
    };

    // Hierarchy mode: delegate to hierarchy exporter
    if args.shared.hierarchy {
        let output_dir = args
            .shared
            .output
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        run_hierarchy_export(&client, &args.issue_key, &output_dir, &args.shared).await;
        return;
    }

    let output_path = args
        .shared
        .output
        .as_ref()
        .map(|o| PathBuf::from(o).join(&args.issue_key))
        .unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(&args.issue_key)
        });

    // Incremental check for single export
    if args.shared.incremental && !args.shared.force {
        let parent_dir = output_path.parent().unwrap_or(std::path::Path::new("."));
        let manifest = Manifest::load(parent_dir);
        if let Ok(issue_data) = client.fetch_issue(&args.issue_key).await {
            let updated = issue_data["fields"]["updated"].as_str().unwrap_or("");
            if !manifest.is_stale(&args.issue_key, updated) {
                info!("Skipping {} (unchanged since last export)", args.issue_key);
                return;
            }
        }
    }

    match perform_export(
        &client,
        &args.issue_key,
        &output_path,
        args.shared.refresh_fields,
        args.shared.include_fields.as_deref(),
        args.shared.exclude_fields.as_deref(),
        args.shared.include_json,
        args.shared.attachment_concurrency,
    )
    .await
    {
        Ok(path) => {
            // Update manifest for incremental support
            if args.shared.incremental {
                let parent_dir = path.parent().unwrap_or(std::path::Path::new("."));
                let mut manifest = Manifest::load(parent_dir);
                if let Ok(issue_data) = client.fetch_issue(&args.issue_key).await {
                    let updated = issue_data["fields"]["updated"].as_str().unwrap_or("");
                    manifest.record(&args.issue_key, updated);
                    if let Err(e) = manifest.save(parent_dir) {
                        eprintln!("Warning: Failed to save manifest: {}", e);
                    }
                }
            }

            info!("\nSuccessfully exported {} to {:?}", args.issue_key, path);
            if args.shared.include_json {
                info!("  - Raw JSON: {:?}", path.join(format!("{}.json", args.issue_key)));
            }
            info!(
                "  - Markdown file: {:?}",
                path.join(format!("{}.md", args.issue_key))
            );
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            process::exit(1);
        }
    }
}

async fn handle_bulk(args: jarkdown::cli::BulkArgs) {
    init_logging(args.shared.verbose);

    let (domain, email, api_token) = load_credentials();
    let client = match JiraApiClient::new(&domain, &email, &api_token) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: {}", e);
            process::exit(1);
        }
    };

    // Hierarchy mode: run hierarchy export for each issue key
    if args.shared.hierarchy {
        let output_dir = args
            .shared
            .output
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        for key in &args.issue_keys {
            run_hierarchy_export(&client, key, &output_dir, &args.shared).await;
        }
        return;
    }

    let exporter = BulkExporter::new(
        client,
        args.concurrency,
        args.shared.output.as_deref(),
        args.batch_name.as_deref(),
        args.shared.refresh_fields,
        args.shared.include_fields.as_deref(),
        args.shared.exclude_fields.as_deref(),
        args.shared.include_json,
        args.shared.attachment_concurrency,
        args.shared.incremental,
        args.shared.force,
    );

    let (successes, failures) = exporter.export_bulk(&args.issue_keys).await;
    let all_results: Vec<_> = successes.iter().chain(failures.iter()).cloned().collect();
    if let Err(e) = exporter.write_index_md(&all_results, &HashMap::new()).await {
        eprintln!("Warning: Failed to write index.md: {}", e);
    }
    print_summary(&successes, &failures);
    if !failures.is_empty() {
        process::exit(1);
    }
}

async fn handle_query(args: jarkdown::cli::QueryArgs) {
    init_logging(args.shared.verbose);

    let (domain, email, api_token) = load_credentials();
    let client = match JiraApiClient::new(&domain, &email, &api_token) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: {}", e);
            process::exit(1);
        }
    };

    eprintln!("Searching: {}", args.jql);
    let issues = match client.search_jql(&args.jql, args.max_results).await {
        Ok(i) => i,
        Err(e) => {
            eprintln!("Error: {}", e);
            process::exit(1);
        }
    };

    if issues.is_empty() {
        eprintln!("No issues found.");
        return;
    }

    let issue_keys: Vec<String> = issues
        .iter()
        .filter_map(|i| i["key"].as_str().map(|s| s.to_string()))
        .collect();
    eprintln!("Found {} issues.", issue_keys.len());

    // Hierarchy mode: run hierarchy export for each matched issue
    if args.shared.hierarchy {
        let output_dir = args
            .shared
            .output
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        for key in &issue_keys {
            run_hierarchy_export(&client, key, &output_dir, &args.shared).await;
        }
        return;
    }

    let exporter = BulkExporter::new(
        client,
        args.concurrency,
        args.shared.output.as_deref(),
        args.batch_name.as_deref(),
        false,
        None,
        None,
        args.shared.include_json,
        args.shared.attachment_concurrency,
        args.shared.incremental,
        args.shared.force,
    );

    let (successes, failures) = exporter.export_bulk(&issue_keys).await;
    let all_results: Vec<_> = successes.iter().chain(failures.iter()).cloned().collect();

    let issues_data: HashMap<String, serde_json::Value> = issues
        .into_iter()
        .filter_map(|i| {
            i["key"]
                .as_str()
                .map(|k| (k.to_string(), i.clone()))
        })
        .collect();

    if let Err(e) = exporter.write_index_md(&all_results, &issues_data).await {
        eprintln!("Warning: Failed to write index.md: {}", e);
    }
    print_summary(&successes, &failures);
    if !failures.is_empty() {
        process::exit(1);
    }
}

/// Run hierarchical export for a single issue key.
async fn run_hierarchy_export(
    client: &JiraApiClient,
    issue_key: &str,
    output_dir: &std::path::Path,
    shared: &jarkdown::cli::SharedArgs,
) {
    let options = HierarchyOptions {
        max_depth: shared.max_depth,
        max_issues: shared.max_issues,
        refresh_fields: shared.refresh_fields,
        include_fields: shared.include_fields.clone(),
        exclude_fields: shared.exclude_fields.clone(),
        include_json: shared.include_json,
        attachment_concurrency: shared.attachment_concurrency,
    };

    let mut exporter = HierarchyExporter::new(client, options);
    match exporter.export_hierarchy(issue_key, output_dir).await {
        Ok(tree) => {
            eprintln!(
                "Exported hierarchy for {} ({} issues)",
                issue_key,
                count_nodes(&tree)
            );
        }
        Err(e) => {
            eprintln!("Error exporting hierarchy for {}: {}", issue_key, e);
        }
    }
}

fn count_nodes(node: &jarkdown::IssueNode) -> usize {
    1 + node.children.iter().map(count_nodes).sum::<usize>()
}

#[tokio::main]
async fn main() {
    // Apply backward-compat shim for bare issue keys
    let args = cli::preprocess_args();
    let cli = Cli::parse_from(args);

    match cli.command {
        Some(Command::Export(args)) => handle_export(args).await,
        Some(Command::Bulk(args)) => handle_bulk(args).await,
        Some(Command::Query(args)) => handle_query(args).await,
        Some(Command::Setup) => {
            setup_configuration();
            process::exit(0);
        }
        None => {
            // Print help
            use clap::CommandFactory;
            Cli::command().print_help().ok();
            println!();
            process::exit(1);
        }
    }
}
