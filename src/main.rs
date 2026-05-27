//! Jarkdown CLI entry point.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process;
use std::time::Duration;

use clap::Parser;
use log::info;
use tokio::time;

use jarkdown::bulk::BulkExporter;
use jarkdown::cli::{self, Cli, Command};
use jarkdown::export::{perform_export_with_options, ExportWorkflowOptions};
use jarkdown::freshness::{self, ExportPlan, PlanOptions};
use jarkdown::hierarchy::{HierarchyExporter, HierarchyLayout, HierarchyOptions};
use jarkdown::issue::IssueSearchResult;
use jarkdown::jira_client::{JiraApiClient, ValidationIssue};
use jarkdown::manifest::{
    default_manifest_path, export_option_fingerprint, relative_artifact_path, Manifest,
};

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
        eprintln!("To set up your configuration, run: jarkdown-rs setup");
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
        eprintln!("To set up your configuration, run: jarkdown-rs setup");
        eprintln!("Or add the missing variables to your .env file.");
        process::exit(1);
    }

    (domain.unwrap(), email.unwrap(), api_token.unwrap())
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
            println!("You can now run: jarkdown-rs export ISSUE-KEY");
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
        let result =
            run_hierarchy_export(&client, &args.issue_key, &output_dir, &args.shared).await;
        if !result.success {
            process::exit(1);
        }
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
    let output_root = output_path.parent().unwrap_or(std::path::Path::new("."));
    let manifest_path = manifest_path_for_output(output_root, &args.shared);
    let option_fingerprint = export_option_fingerprint(
        args.shared.include_fields.as_deref(),
        args.shared.exclude_fields.as_deref(),
        args.shared.no_attachments,
    );
    let mut manifest = if args.shared.incremental {
        match Manifest::load_from_path(&manifest_path) {
            Ok(manifest) => Some(manifest),
            Err(e) => {
                eprintln!("Error: {}", e);
                process::exit(1);
            }
        }
    } else {
        None
    };

    // Incremental check for single export
    if args.shared.incremental && !args.shared.force {
        let (validation_succeeded, validation) = match client
            .validate_issue_keys(std::slice::from_ref(&args.issue_key))
            .await
        {
            Ok(results) => (
                true,
                results
                    .into_iter()
                    .map(|issue| (issue.key.clone(), issue))
                    .collect::<HashMap<_, _>>(),
            ),
            Err(e) => {
                eprintln!(
                    "Warning: Failed to validate incremental manifest through Jira search: {}",
                    e
                );
                (false, HashMap::new())
            }
        };

        let normalized_issue_key = args.issue_key.trim().to_ascii_uppercase();
        if let Some(issue) = validation.get(&normalized_issue_key) {
            match freshness::plan_metadata(
                &args.issue_key,
                &issue.updated,
                manifest.as_ref().expect("manifest loaded for incremental"),
                PlanOptions {
                    include_changelog: args.shared.include_changelog,
                    include_json: args.shared.include_json,
                    option_fingerprint: option_fingerprint.as_deref(),
                },
                &output_path,
            ) {
                ExportPlan::Skip => {
                    info!("Skipping {} (unchanged since last export)", args.issue_key);
                    if let Some(manifest) = manifest.as_mut() {
                        manifest.record_metadata_with_fingerprint(
                            &issue.key,
                            &issue.updated,
                            issue.summary.as_deref(),
                            issue.issue_type.as_deref(),
                            issue.status.as_deref(),
                            relative_artifact_path(output_root, &output_path),
                            option_fingerprint.as_deref(),
                        );
                        if let Err(e) = manifest.save_to_path(&manifest_path) {
                            eprintln!("Warning: Failed to save manifest: {}", e);
                        }
                    }
                    return;
                }
                ExportPlan::BackfillChangelogOnly => {
                    info!(
                        "Backfilling changelog for {} (issue unchanged)",
                        args.issue_key
                    );
                    match client.fetch_changelog(&args.issue_key).await {
                        Ok(entries) => {
                            let summary = issue
                                .summary
                                .as_deref()
                                .or_else(|| {
                                    manifest
                                        .as_ref()
                                        .and_then(|m| m.get(&issue.key))
                                        .and_then(|entry| entry.summary.as_deref())
                                })
                                .unwrap_or(&issue.key);
                            if let Err(e) = jarkdown::changelog::write_artifacts(
                                &args.issue_key,
                                summary,
                                &entries,
                                &output_path,
                                args.shared.include_json,
                            )
                            .await
                            {
                                eprintln!("Warning: changelog backfill failed: {}", e);
                            }
                        }
                        Err(e) => {
                            eprintln!("Warning: changelog backfill fetch failed: {}", e)
                        }
                    }
                    if let Some(manifest) = manifest.as_mut() {
                        manifest.record_metadata_with_fingerprint(
                            &issue.key,
                            &issue.updated,
                            issue.summary.as_deref(),
                            issue.issue_type.as_deref(),
                            issue.status.as_deref(),
                            relative_artifact_path(output_root, &output_path),
                            option_fingerprint.as_deref(),
                        );
                        if let Err(e) = manifest.save_to_path(&manifest_path) {
                            eprintln!("Warning: Failed to save manifest: {}", e);
                        }
                    }
                    return;
                }
                ExportPlan::Full => {}
            }
        } else if validation_succeeded
            && manifest
                .as_ref()
                .is_some_and(|manifest| manifest.is_active(&args.issue_key))
        {
            info!(
                "Evicting {} (not returned by validation search)",
                args.issue_key
            );
            if let Some(manifest) = manifest.as_mut() {
                manifest.evict(&args.issue_key, "not_returned_by_validation_search");
                if let Err(e) = manifest.save_to_path(&manifest_path) {
                    eprintln!("Warning: Failed to save manifest: {}", e);
                }
            }
            return;
        }
    }

    let export = perform_export_with_options(
        &client,
        &args.issue_key,
        &output_path,
        ExportWorkflowOptions {
            refresh_fields: args.shared.refresh_fields,
            include_fields: args.shared.include_fields.as_deref(),
            exclude_fields: args.shared.exclude_fields.as_deref(),
            include_json: args.shared.include_json,
            attachment_concurrency: args.shared.attachment_concurrency,
            no_attachments: args.shared.no_attachments,
            include_changelog: args.shared.include_changelog,
        },
    );

    match time::timeout(
        Duration::from_secs(args.shared.issue_timeout_seconds),
        export,
    )
    .await
    {
        Err(_) => {
            eprintln!(
                "Error: {} timed out after {}s",
                args.issue_key, args.shared.issue_timeout_seconds
            );
            process::exit(1);
        }
        Ok(Ok(path)) => {
            // Update manifest for incremental support
            if args.shared.incremental {
                if let Ok(issue) = client.fetch_issue(&args.issue_key).await {
                    if let Some(manifest) = manifest.as_mut() {
                        manifest.record_issue_with_fingerprint(
                            &issue,
                            relative_artifact_path(output_root, &path),
                            option_fingerprint.as_deref(),
                        );
                        if let Err(e) = manifest.save_to_path(&manifest_path) {
                            eprintln!("Warning: Failed to save manifest: {}", e);
                        }
                    }
                }
            }

            info!("\nSuccessfully exported {} to {:?}", args.issue_key, path);
            if args.shared.include_json {
                info!(
                    "  - Raw JSON: {:?}",
                    path.join(format!("{}.json", args.issue_key))
                );
            }
            info!(
                "  - Markdown file: {:?}",
                path.join(format!("{}.md", args.issue_key))
            );
        }
        Ok(Err(e)) => {
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
        let mut successes = Vec::new();
        let mut failures = Vec::new();
        let validation_plan =
            build_hierarchy_validation_plan(&client, &args.issue_keys, &output_dir, &args.shared)
                .await;
        for key in &args.issue_keys {
            let result = run_hierarchy_export_with_validation(
                &client,
                key,
                &output_dir,
                &args.shared,
                validation_plan.as_ref(),
            )
            .await;
            if result.success {
                successes.push(result);
            } else {
                failures.push(result);
            }
        }
        print_summary(&successes, &failures);
        if !failures.is_empty() {
            process::exit(1);
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
    )
    .with_manifest_path(args.shared.manifest.as_deref())
    .with_no_attachments(args.shared.no_attachments)
    .with_issue_timeout_seconds(args.shared.issue_timeout_seconds)
    .with_include_changelog(args.shared.include_changelog);

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

    let issue_keys = issue_keys_from_search_results(&issues);

    if args.keys_only {
        for key in &issue_keys {
            println!("{}", key);
        }
        return;
    }

    eprintln!("Found {} issues.", issue_keys.len());

    // Hierarchy mode: run hierarchy export for each matched issue
    if args.shared.hierarchy {
        let output_dir = args
            .shared
            .output
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        let mut successes = Vec::new();
        let mut failures = Vec::new();
        let validation_plan =
            build_hierarchy_validation_plan(&client, &issue_keys, &output_dir, &args.shared).await;
        for key in &issue_keys {
            let result = run_hierarchy_export_with_validation(
                &client,
                key,
                &output_dir,
                &args.shared,
                validation_plan.as_ref(),
            )
            .await;
            if result.success {
                successes.push(result);
            } else {
                failures.push(result);
            }
        }
        print_summary(&successes, &failures);
        if !failures.is_empty() {
            process::exit(1);
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
    )
    .with_manifest_path(args.shared.manifest.as_deref())
    .with_no_attachments(args.shared.no_attachments)
    .with_issue_timeout_seconds(args.shared.issue_timeout_seconds)
    .with_include_changelog(args.shared.include_changelog);

    let (successes, failures) = exporter.export_bulk(&issue_keys).await;
    let all_results: Vec<_> = successes.iter().chain(failures.iter()).cloned().collect();

    let issues_data: HashMap<String, serde_json::Value> = issues
        .into_iter()
        .filter(|r| !r.key.is_empty())
        .map(|r| (r.key.clone(), r.raw))
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
) -> jarkdown::ExportResult {
    run_hierarchy_export_with_validation(client, issue_key, output_dir, shared, None).await
}

async fn run_hierarchy_export_with_validation(
    client: &JiraApiClient,
    issue_key: &str,
    output_dir: &std::path::Path,
    shared: &jarkdown::cli::SharedArgs,
    validation_plan: Option<&HashMap<String, ValidationIssue>>,
) -> jarkdown::ExportResult {
    let options = HierarchyOptions {
        max_depth: shared.max_depth,
        max_issues: shared.max_issues,
        refresh_fields: shared.refresh_fields,
        include_fields: shared.include_fields.clone(),
        exclude_fields: shared.exclude_fields.clone(),
        include_json: shared.include_json,
        attachment_concurrency: shared.attachment_concurrency,
        no_attachments: shared.no_attachments,
        include_changelog: shared.include_changelog,
        layout: hierarchy_layout(shared),
    };

    let manifest_path = manifest_path_for_output(output_dir, shared);
    let option_fingerprint = export_option_fingerprint(
        shared.include_fields.as_deref(),
        shared.exclude_fields.as_deref(),
        shared.no_attachments,
    );
    if shared.incremental && !shared.force {
        let warm_result = match options.layout {
            HierarchyLayout::Corpus => {
                try_warm_corpus_hierarchy_skip(
                    client,
                    issue_key,
                    output_dir,
                    shared,
                    &manifest_path,
                    option_fingerprint.as_deref(),
                    validation_plan,
                )
                .await
            }
            HierarchyLayout::Nested => {
                try_warm_nested_hierarchy_skip(
                    client,
                    issue_key,
                    output_dir,
                    shared,
                    &manifest_path,
                    option_fingerprint.as_deref(),
                    validation_plan,
                )
                .await
            }
        };
        match warm_result {
            Ok(Some(tree)) => {
                eprintln!(
                    "Exported hierarchy for {} ({} issues)",
                    issue_key,
                    count_nodes(&tree)
                );
                return jarkdown::ExportResult {
                    issue_key: issue_key.to_string(),
                    success: true,
                    output_path: Some(output_dir.join(issue_key)),
                    error: None,
                };
            }
            Ok(None) => {}
            Err(e) => eprintln!("Warning: hierarchy incremental validation failed: {}", e),
        }
    }

    let workflow_options = ExportWorkflowOptions {
        refresh_fields: options.refresh_fields,
        include_fields: options.include_fields.as_deref(),
        exclude_fields: options.exclude_fields.as_deref(),
        include_json: options.include_json,
        attachment_concurrency: options.attachment_concurrency,
        no_attachments: options.no_attachments,
        include_changelog: options.include_changelog,
    };
    let workflow_exporter = jarkdown::exporter::WorkflowIssueExporter {
        api_client: client,
        options: workflow_options,
    };
    let mut exporter = HierarchyExporter::new(client, &workflow_exporter, options.clone());
    let export = exporter.export_hierarchy(issue_key, output_dir);
    match time::timeout(Duration::from_secs(shared.issue_timeout_seconds), export).await {
        Err(_) => {
            let error = format!("timed out after {}s", shared.issue_timeout_seconds);
            eprintln!("Error exporting hierarchy for {}: {}", issue_key, error);
            jarkdown::ExportResult {
                issue_key: issue_key.to_string(),
                success: false,
                output_path: None,
                error: Some(error),
            }
        }
        Ok(Ok(tree)) => {
            if shared.incremental {
                match Manifest::load_from_path(&manifest_path) {
                    Ok(mut manifest) => {
                        let snapshot_path = match options.layout {
                            HierarchyLayout::Corpus => {
                                output_dir.join(format!("{}.hierarchy.md", issue_key))
                            }
                            HierarchyLayout::Nested => output_dir.join("index.md"),
                        };
                        manifest.record_hierarchy(
                            &tree,
                            relative_artifact_path(output_dir, &snapshot_path),
                            options.layout,
                            option_fingerprint.as_deref(),
                        );
                        if let Err(e) = manifest.save_to_path(&manifest_path) {
                            eprintln!("Warning: Failed to save manifest: {}", e);
                        }
                    }
                    Err(e) => eprintln!("Warning: Failed to load manifest: {}", e),
                }
            }
            eprintln!(
                "Exported hierarchy for {} ({} issues)",
                issue_key,
                count_nodes(&tree)
            );
            jarkdown::ExportResult {
                issue_key: issue_key.to_string(),
                success: true,
                output_path: Some(output_dir.join(issue_key)),
                error: None,
            }
        }
        Ok(Err(e)) => {
            eprintln!("Error exporting hierarchy for {}: {}", issue_key, e);
            jarkdown::ExportResult {
                issue_key: issue_key.to_string(),
                success: false,
                output_path: None,
                error: Some(e.to_string()),
            }
        }
    }
}

async fn try_warm_corpus_hierarchy_skip(
    client: &JiraApiClient,
    issue_key: &str,
    output_dir: &std::path::Path,
    shared: &jarkdown::cli::SharedArgs,
    manifest_path: &std::path::Path,
    option_fingerprint: Option<&str>,
    validation_plan: Option<&HashMap<String, ValidationIssue>>,
) -> jarkdown::Result<Option<jarkdown::IssueNode>> {
    let mut manifest = Manifest::load_from_path(manifest_path)?;
    let Some(tree) = manifest.cached_hierarchy_tree(issue_key) else {
        return Ok(None);
    };
    let keys = manifest.active_hierarchy_keys(issue_key);
    if keys.is_empty() {
        return Ok(None);
    }

    let validation = match validation_plan {
        Some(plan) => plan.clone(),
        None => client
            .validate_issue_keys(&keys)
            .await?
            .into_iter()
            .map(|issue| (normalize_issue_key_local(&issue.key), issue))
            .collect::<HashMap<_, _>>(),
    };
    if keys.iter().any(|key| !validation.contains_key(key)) {
        return Ok(None);
    }

    let mut refresh_keys = Vec::new();
    for key in &keys {
        let Some(issue) = validation.get(key) else {
            return Ok(None);
        };
        let plan = freshness::plan_metadata(
            key,
            &issue.updated,
            &manifest,
            PlanOptions {
                include_changelog: shared.include_changelog,
                include_json: shared.include_json,
                option_fingerprint,
            },
            &output_dir.join(key),
        );
        match plan {
            ExportPlan::Skip => {}
            ExportPlan::Full
                if normalize_issue_key_local(key) != normalize_issue_key_local(issue_key) =>
            {
                refresh_keys.push(key.clone());
            }
            ExportPlan::BackfillChangelogOnly
                if normalize_issue_key_local(key) != normalize_issue_key_local(issue_key) =>
            {
                refresh_keys.push(key.clone());
            }
            ExportPlan::Full | ExportPlan::BackfillChangelogOnly => return Ok(None),
        }
    }

    if !refresh_keys.is_empty() {
        refresh_changed_corpus_descendants(
            client,
            issue_key,
            output_dir,
            shared,
            &mut manifest,
            &validation,
            &refresh_keys,
            option_fingerprint,
        )
        .await?;
        manifest.save_to_path(manifest_path)?;
        return Ok(manifest.cached_hierarchy_tree(issue_key));
    }

    for key in &keys {
        let issue = validation.get(key).expect("validated key");
        manifest.record_metadata_with_fingerprint(
            &issue.key,
            &issue.updated,
            issue.summary.as_deref(),
            issue.issue_type.as_deref(),
            issue.status.as_deref(),
            key,
            option_fingerprint,
        );
    }
    manifest.save_to_path(manifest_path)?;
    Ok(Some(tree))
}

async fn refresh_changed_corpus_descendants(
    client: &JiraApiClient,
    root_key: &str,
    output_dir: &std::path::Path,
    shared: &jarkdown::cli::SharedArgs,
    manifest: &mut Manifest,
    validation: &HashMap<String, jarkdown::jira_client::ValidationIssue>,
    refresh_keys: &[String],
    option_fingerprint: Option<&str>,
) -> jarkdown::Result<()> {
    let workflow_options = ExportWorkflowOptions {
        refresh_fields: shared.refresh_fields,
        include_fields: shared.include_fields.as_deref(),
        exclude_fields: shared.exclude_fields.as_deref(),
        include_json: shared.include_json,
        attachment_concurrency: shared.attachment_concurrency,
        no_attachments: shared.no_attachments,
        include_changelog: shared.include_changelog,
    };
    let workflow_exporter = jarkdown::exporter::WorkflowIssueExporter {
        api_client: client,
        options: workflow_options.clone(),
    };

    let refresh_set: std::collections::HashSet<_> = refresh_keys
        .iter()
        .map(|key| normalize_issue_key_local(key))
        .collect();
    for key in refresh_keys {
        let normalized_key = normalize_issue_key_local(key);
        if refresh_set.iter().any(|other| {
            other != &normalized_key && manifest.is_descendant_of(other, &normalized_key)
        }) {
            continue;
        }

        if manifest.has_active_children(&normalized_key) {
            let mut options = HierarchyOptions {
                max_depth: shared
                    .max_depth
                    .min(manifest.max_cached_descendant_depth(&normalized_key)),
                max_issues: shared.max_issues,
                refresh_fields: shared.refresh_fields,
                include_fields: shared.include_fields.clone(),
                exclude_fields: shared.exclude_fields.clone(),
                include_json: shared.include_json,
                attachment_concurrency: shared.attachment_concurrency,
                no_attachments: shared.no_attachments,
                include_changelog: shared.include_changelog,
                layout: HierarchyLayout::Corpus,
            };
            if options.max_depth == 0 {
                options.max_depth = 1;
            }
            let mut exporter = HierarchyExporter::new(client, &workflow_exporter, options);
            let subtree = exporter.export_subtree(&normalized_key, output_dir).await?;
            manifest.record_hierarchy_members(root_key, &subtree, option_fingerprint);
        } else {
            perform_export_with_options(
                client,
                &normalized_key,
                &output_dir.join(&normalized_key),
                workflow_options.clone(),
            )
            .await?;
            if let Some(issue) = validation.get(&normalized_key) {
                manifest.record_metadata_with_fingerprint(
                    &issue.key,
                    &issue.updated,
                    issue.summary.as_deref(),
                    issue.issue_type.as_deref(),
                    issue.status.as_deref(),
                    &normalized_key,
                    option_fingerprint,
                );
            }
        }
    }
    Ok(())
}

async fn try_warm_nested_hierarchy_skip(
    client: &JiraApiClient,
    issue_key: &str,
    output_dir: &std::path::Path,
    shared: &jarkdown::cli::SharedArgs,
    manifest_path: &std::path::Path,
    option_fingerprint: Option<&str>,
    validation_plan: Option<&HashMap<String, ValidationIssue>>,
) -> jarkdown::Result<Option<jarkdown::IssueNode>> {
    let mut manifest = Manifest::load_from_path(manifest_path)?;
    let Some(tree) = manifest.cached_hierarchy_tree(issue_key) else {
        return Ok(None);
    };
    let keys = manifest.active_hierarchy_keys(issue_key);
    if keys.is_empty() {
        return Ok(None);
    }

    let validation = match validation_plan {
        Some(plan) => plan.clone(),
        None => client
            .validate_issue_keys(&keys)
            .await?
            .into_iter()
            .map(|issue| (normalize_issue_key_local(&issue.key), issue))
            .collect::<HashMap<_, _>>(),
    };
    if keys.iter().any(|key| !validation.contains_key(key)) {
        return Ok(None);
    }

    let mut refresh_keys = Vec::new();
    for key in &keys {
        let Some(issue) = validation.get(key) else {
            return Ok(None);
        };
        let paths = manifest.active_artifact_paths(key);
        if paths.is_empty() {
            return Ok(None);
        }
        let needs_refresh = paths.iter().any(|path| {
            freshness::plan_metadata(
                key,
                &issue.updated,
                &manifest,
                PlanOptions {
                    include_changelog: shared.include_changelog,
                    include_json: shared.include_json,
                    option_fingerprint,
                },
                &output_dir.join(path),
            ) != ExportPlan::Skip
        });
        if needs_refresh {
            if normalize_issue_key_local(key) == normalize_issue_key_local(issue_key) {
                return Ok(None);
            }
            refresh_keys.push(key.clone());
        }
    }

    if !refresh_keys.is_empty() {
        refresh_changed_nested_descendants(
            client,
            issue_key,
            output_dir,
            shared,
            &mut manifest,
            &validation,
            &refresh_keys,
            option_fingerprint,
        )
        .await?;
        manifest.save_to_path(manifest_path)?;
        return Ok(manifest.cached_hierarchy_tree(issue_key));
    }

    manifest.save_to_path(manifest_path)?;
    Ok(Some(tree))
}

async fn refresh_changed_nested_descendants(
    client: &JiraApiClient,
    root_key: &str,
    output_dir: &std::path::Path,
    shared: &jarkdown::cli::SharedArgs,
    manifest: &mut Manifest,
    validation: &HashMap<String, jarkdown::jira_client::ValidationIssue>,
    refresh_keys: &[String],
    option_fingerprint: Option<&str>,
) -> jarkdown::Result<()> {
    let workflow_options = ExportWorkflowOptions {
        refresh_fields: shared.refresh_fields,
        include_fields: shared.include_fields.as_deref(),
        exclude_fields: shared.exclude_fields.as_deref(),
        include_json: shared.include_json,
        attachment_concurrency: shared.attachment_concurrency,
        no_attachments: shared.no_attachments,
        include_changelog: shared.include_changelog,
    };
    let workflow_exporter = jarkdown::exporter::WorkflowIssueExporter {
        api_client: client,
        options: workflow_options.clone(),
    };

    let refresh_set: std::collections::HashSet<_> = refresh_keys
        .iter()
        .map(|key| normalize_issue_key_local(key))
        .collect();
    for key in refresh_keys {
        let normalized_key = normalize_issue_key_local(key);
        if refresh_set.iter().any(|other| {
            other != &normalized_key && manifest.is_descendant_of(other, &normalized_key)
        }) {
            continue;
        }

        let paths = manifest.active_artifact_paths(&normalized_key);
        if manifest.has_active_children(&normalized_key) {
            let mut options = HierarchyOptions {
                max_depth: shared
                    .max_depth
                    .min(manifest.max_cached_descendant_depth(&normalized_key)),
                max_issues: shared.max_issues,
                refresh_fields: shared.refresh_fields,
                include_fields: shared.include_fields.clone(),
                exclude_fields: shared.exclude_fields.clone(),
                include_json: shared.include_json,
                attachment_concurrency: shared.attachment_concurrency,
                no_attachments: shared.no_attachments,
                include_changelog: shared.include_changelog,
                layout: HierarchyLayout::Nested,
            };
            if options.max_depth == 0 {
                options.max_depth = 1;
            }
            for path in &paths {
                let base = nested_refresh_base(output_dir, path, &normalized_key);
                let mut exporter =
                    HierarchyExporter::new(client, &workflow_exporter, options.clone());
                let subtree = exporter.export_subtree(&normalized_key, &base).await?;
                manifest.record_hierarchy_members_at_path(
                    root_key,
                    &subtree,
                    HierarchyLayout::Nested,
                    path,
                    option_fingerprint,
                );
            }
        } else if let Some(issue) = validation.get(&normalized_key) {
            for path in &paths {
                perform_export_with_options(
                    client,
                    &normalized_key,
                    &output_dir.join(path),
                    workflow_options.clone(),
                )
                .await?;
                manifest.record_hierarchy_metadata_with_fingerprint(
                    &issue.key,
                    &issue.updated,
                    issue.summary.as_deref(),
                    issue.issue_type.as_deref(),
                    issue.status.as_deref(),
                    path,
                    option_fingerprint,
                );
            }
        }
    }
    Ok(())
}

fn nested_refresh_base(
    output_dir: &std::path::Path,
    artifact_path: &str,
    issue_key: &str,
) -> PathBuf {
    let normalized_key = normalize_issue_key_local(issue_key);
    let mut parts: Vec<&str> = artifact_path
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    if parts
        .last()
        .is_some_and(|last| normalize_issue_key_local(last) == normalized_key)
    {
        parts.pop();
    }
    parts
        .into_iter()
        .fold(output_dir.to_path_buf(), |path, part| path.join(part))
}

async fn build_hierarchy_validation_plan(
    client: &JiraApiClient,
    requested_roots: &[String],
    output_dir: &std::path::Path,
    shared: &jarkdown::cli::SharedArgs,
) -> Option<HashMap<String, ValidationIssue>> {
    if !shared.incremental || shared.force {
        return None;
    }
    let manifest_path = manifest_path_for_output(output_dir, shared);
    let manifest = match Manifest::load_from_path(&manifest_path) {
        Ok(manifest) => manifest,
        Err(e) => {
            eprintln!(
                "Warning: Failed to load hierarchy manifest for validation: {}",
                e
            );
            return None;
        }
    };
    let mut keys = Vec::new();
    for root in requested_roots {
        push_normalized_unique(&mut keys, root);
        for key in manifest.active_hierarchy_keys(root) {
            push_normalized_unique(&mut keys, &key);
        }
    }
    if keys.is_empty() {
        return None;
    }
    match client.validate_issue_keys(&keys).await {
        Ok(results) => Some(
            results
                .into_iter()
                .map(|issue| (normalize_issue_key_local(&issue.key), issue))
                .collect(),
        ),
        Err(e) => {
            eprintln!(
                "Warning: Failed to validate hierarchy incremental manifest through Jira search: {}",
                e
            );
            None
        }
    }
}

fn push_normalized_unique(keys: &mut Vec<String>, key: &str) {
    let key = normalize_issue_key_local(key);
    if !keys.contains(&key) {
        keys.push(key);
    }
}

fn normalize_issue_key_local(key: &str) -> String {
    key.trim().to_ascii_uppercase()
}

fn hierarchy_layout(shared: &jarkdown::cli::SharedArgs) -> HierarchyLayout {
    match shared.hierarchy_layout {
        Some(jarkdown::cli::HierarchyLayoutArg::Corpus) => HierarchyLayout::Corpus,
        Some(jarkdown::cli::HierarchyLayoutArg::Nested) => HierarchyLayout::Nested,
        None if shared.incremental => HierarchyLayout::Corpus,
        None => HierarchyLayout::Nested,
    }
}

fn count_nodes(node: &jarkdown::IssueNode) -> usize {
    1 + node.children.iter().map(count_nodes).sum::<usize>()
}

fn issue_keys_from_search_results(results: &[IssueSearchResult]) -> Vec<String> {
    results
        .iter()
        .filter(|r| !r.key.is_empty())
        .map(|r| r.key.clone())
        .collect()
}

fn manifest_path_for_output(
    output_root: &std::path::Path,
    shared: &jarkdown::cli::SharedArgs,
) -> PathBuf {
    shared
        .manifest
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| default_manifest_path(output_root))
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{Read as _, Write as _};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};
    use std::thread;

    #[test]
    fn issue_keys_from_search_results_extracts_one_key_per_matching_issue() {
        let issues = vec![
            IssueSearchResult::from_value(json!({"key": "K1"})).unwrap(),
            IssueSearchResult::from_value(json!({"fields": {"summary": "missing key"}})).unwrap(),
            IssueSearchResult::from_value(json!({"key": "K2"})).unwrap(),
        ];

        assert_eq!(issue_keys_from_search_results(&issues), vec!["K1", "K2"]);
    }

    #[test]
    fn hierarchy_layout_default_preserves_nested_unless_incremental() {
        let mut shared = shared_args();
        shared.hierarchy = true;
        assert_eq!(hierarchy_layout(&shared), HierarchyLayout::Nested);

        shared.incremental = true;
        assert_eq!(hierarchy_layout(&shared), HierarchyLayout::Corpus);

        shared.hierarchy_layout = Some(jarkdown::cli::HierarchyLayoutArg::Nested);
        assert_eq!(hierarchy_layout(&shared), HierarchyLayout::Nested);
    }

    #[tokio::test]
    async fn warm_corpus_hierarchy_skip_validates_cached_tree_without_full_fetches() {
        let server = WarmHierarchyServer::start();
        let client = JiraApiClient::new_for_test(&server.base_url);
        let output_dir = std::env::temp_dir().join(format!(
            "jarkdown-warm-hierarchy-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(output_dir.join("A")).unwrap();
        std::fs::create_dir_all(output_dir.join("B")).unwrap();
        std::fs::write(output_dir.join("A").join("A.md"), "A").unwrap();
        std::fs::write(output_dir.join("B").join("B.md"), "B").unwrap();
        std::fs::write(output_dir.join("A.hierarchy.md"), "snapshot").unwrap();

        let tree = jarkdown::IssueNode {
            key: "A".to_string(),
            summary: "Root A".to_string(),
            issue_type: "Task".to_string(),
            updated: "2026-01-01T00:00:00.000+0000".to_string(),
            children_discovered: true,
            truncated: false,
            failures: Vec::new(),
            children: vec![jarkdown::IssueNode {
                key: "B".to_string(),
                summary: "Child B".to_string(),
                issue_type: "Task".to_string(),
                updated: "2026-01-01T00:00:00.000+0000".to_string(),
                children_discovered: true,
                truncated: false,
                failures: Vec::new(),
                children: Vec::new(),
            }],
        };
        let mut manifest = Manifest::default();
        let fingerprint = export_option_fingerprint(None, None, true);
        manifest.record_hierarchy(
            &tree,
            "A.hierarchy.md",
            HierarchyLayout::Corpus,
            fingerprint.as_deref(),
        );
        manifest.save(&output_dir).unwrap();
        let loaded = Manifest::load(&output_dir);
        assert_eq!(
            loaded.active_hierarchy_keys("A"),
            vec!["A".to_string(), "B".to_string()]
        );
        assert!(loaded.cached_hierarchy_tree("A").is_some());

        let mut shared = shared_args();
        shared.hierarchy = true;
        shared.incremental = true;
        shared.no_attachments = true;
        let result = run_hierarchy_export(&client, "A", &output_dir, &shared).await;

        assert!(result.success, "warm hierarchy skip should succeed");
        let paths = server.observed_paths();
        assert!(
            paths
                .iter()
                .any(|path| path.contains("/rest/api/3/search/jql")),
            "expected validation search request, observed: {:?}",
            paths
        );
        assert!(
            !paths.iter().any(|path| path.contains("/rest/api/3/issue/")),
            "warm skip must not fetch full Issues or Changelogs, observed: {:?}",
            paths
        );

        std::fs::remove_dir_all(output_dir).ok();
    }

    #[tokio::test]
    async fn changed_leaf_descendant_refreshes_only_that_issue_and_preserves_snapshot() {
        let server = ChangedHierarchyServer::start(ChangedHierarchyShape::Leaf);
        let client = JiraApiClient::new_for_test(&server.base_url);
        let output_dir = seeded_hierarchy_dir("jarkdown-changed-leaf");
        std::fs::write(output_dir.join("A.hierarchy.md"), "SNAPSHOT").unwrap();
        seed_manifest_tree(
            &output_dir,
            jarkdown::IssueNode {
                key: "A".to_string(),
                summary: "Root A".to_string(),
                issue_type: "Task".to_string(),
                updated: OLD_TS.to_string(),
                children_discovered: true,
                truncated: false,
                failures: Vec::new(),
                children: vec![jarkdown::IssueNode {
                    key: "B".to_string(),
                    summary: "Child B".to_string(),
                    issue_type: "Task".to_string(),
                    updated: OLD_TS.to_string(),
                    children_discovered: true,
                    truncated: false,
                    failures: Vec::new(),
                    children: Vec::new(),
                }],
            },
        );

        let mut shared = shared_args();
        shared.hierarchy = true;
        shared.incremental = true;
        shared.no_attachments = true;
        let result = run_hierarchy_export(&client, "A", &output_dir, &shared).await;

        assert!(result.success, "changed leaf refresh should succeed");
        let paths = server.observed_paths();
        assert!(paths
            .iter()
            .any(|path| path.contains("/rest/api/3/issue/B")));
        assert!(
            !paths
                .iter()
                .any(|path| path.contains("/rest/api/3/issue/A")),
            "unchanged ancestor must not be fully fetched: {:?}",
            paths
        );
        assert_eq!(
            std::fs::read_to_string(output_dir.join("A.hierarchy.md")).unwrap(),
            "SNAPSHOT"
        );
        assert_eq!(
            Manifest::load(&output_dir).get("B").unwrap().updated,
            NEW_TS
        );

        std::fs::remove_dir_all(output_dir).ok();
    }

    #[tokio::test]
    async fn changed_intermediate_descendant_refreshes_bounded_subtree_not_ancestor() {
        let server = ChangedHierarchyServer::start(ChangedHierarchyShape::Intermediate);
        let client = JiraApiClient::new_for_test(&server.base_url);
        let output_dir = seeded_hierarchy_dir("jarkdown-changed-intermediate");
        std::fs::write(output_dir.join("A.hierarchy.md"), "SNAPSHOT").unwrap();
        seed_manifest_tree(
            &output_dir,
            jarkdown::IssueNode {
                key: "A".to_string(),
                summary: "Root A".to_string(),
                issue_type: "Task".to_string(),
                updated: OLD_TS.to_string(),
                children_discovered: true,
                truncated: false,
                failures: Vec::new(),
                children: vec![jarkdown::IssueNode {
                    key: "B".to_string(),
                    summary: "Child B".to_string(),
                    issue_type: "Task".to_string(),
                    updated: OLD_TS.to_string(),
                    children_discovered: true,
                    truncated: false,
                    failures: Vec::new(),
                    children: vec![jarkdown::IssueNode {
                        key: "C".to_string(),
                        summary: "Grandchild C".to_string(),
                        issue_type: "Task".to_string(),
                        updated: OLD_TS.to_string(),
                        children_discovered: true,
                        truncated: false,
                        failures: Vec::new(),
                        children: Vec::new(),
                    }],
                }],
            },
        );

        let mut shared = shared_args();
        shared.hierarchy = true;
        shared.incremental = true;
        shared.no_attachments = true;
        shared.max_depth = 1;
        let result = run_hierarchy_export(&client, "A", &output_dir, &shared).await;

        assert!(
            result.success,
            "changed intermediate refresh should succeed"
        );
        let paths = server.observed_paths();
        assert!(paths
            .iter()
            .any(|path| path.contains("/rest/api/3/issue/B")));
        assert!(paths
            .iter()
            .any(|path| path.contains("/rest/api/3/issue/C")));
        assert!(
            !paths
                .iter()
                .any(|path| path.contains("/rest/api/3/issue/A")),
            "unchanged ancestor must not be fully fetched: {:?}",
            paths
        );
        assert_eq!(
            std::fs::read_to_string(output_dir.join("A.hierarchy.md")).unwrap(),
            "SNAPSHOT"
        );
        let manifest = Manifest::load(&output_dir);
        assert_eq!(manifest.get("B").unwrap().updated, NEW_TS);
        assert_eq!(manifest.get("C").unwrap().updated, OLD_TS);

        std::fs::remove_dir_all(output_dir).ok();
    }

    #[tokio::test]
    async fn nested_incremental_refreshes_changed_shared_issue_to_all_active_paths() {
        let server = ChangedHierarchyServer::start(ChangedHierarchyShape::SharedNested);
        let client = JiraApiClient::new_for_test(&server.base_url);
        let output_dir = seeded_nested_shared_dir("jarkdown-nested-shared");
        let fingerprint = export_option_fingerprint(None, None, true);
        let mut manifest = Manifest::default();
        manifest.record_hierarchy(
            &jarkdown::IssueNode {
                key: "A".to_string(),
                summary: "Root A".to_string(),
                issue_type: "Task".to_string(),
                updated: OLD_TS.to_string(),
                children_discovered: true,
                truncated: false,
                failures: Vec::new(),
                children: vec![jarkdown::IssueNode {
                    key: "C".to_string(),
                    summary: "Shared C".to_string(),
                    issue_type: "Task".to_string(),
                    updated: OLD_TS.to_string(),
                    children_discovered: true,
                    truncated: false,
                    failures: Vec::new(),
                    children: Vec::new(),
                }],
            },
            "index.md",
            HierarchyLayout::Nested,
            fingerprint.as_deref(),
        );
        manifest.record_hierarchy(
            &jarkdown::IssueNode {
                key: "B".to_string(),
                summary: "Root B".to_string(),
                issue_type: "Task".to_string(),
                updated: OLD_TS.to_string(),
                children_discovered: true,
                truncated: false,
                failures: Vec::new(),
                children: vec![jarkdown::IssueNode {
                    key: "C".to_string(),
                    summary: "Shared C".to_string(),
                    issue_type: "Task".to_string(),
                    updated: OLD_TS.to_string(),
                    children_discovered: true,
                    truncated: false,
                    failures: Vec::new(),
                    children: Vec::new(),
                }],
            },
            "index.md",
            HierarchyLayout::Nested,
            fingerprint.as_deref(),
        );
        manifest.save(&output_dir).unwrap();
        std::fs::remove_dir_all(output_dir.join("B").join("C")).unwrap();

        let mut shared = shared_args();
        shared.hierarchy = true;
        shared.incremental = true;
        shared.no_attachments = true;
        shared.hierarchy_layout = Some(jarkdown::cli::HierarchyLayoutArg::Nested);
        let result = run_hierarchy_export(&client, "A", &output_dir, &shared).await;

        assert!(result.success, "nested shared refresh should succeed");
        let paths = server.observed_paths();
        let c_fetches = paths
            .iter()
            .filter(|path| path.contains("/rest/api/3/issue/C"))
            .count();
        assert_eq!(
            c_fetches, 2,
            "changed shared issue should refresh every active nested path: {:?}",
            paths
        );
        assert!(
            !paths
                .iter()
                .any(|path| path.contains("/rest/api/3/issue/A")),
            "unchanged root must not be fully fetched: {:?}",
            paths
        );
        assert!(
            std::fs::read_to_string(output_dir.join("A").join("C").join("C.md"))
                .unwrap()
                .contains("Issue C")
        );
        assert!(
            std::fs::read_to_string(output_dir.join("B").join("C").join("C.md"))
                .unwrap()
                .contains("Issue C")
        );

        let manifest = Manifest::load(&output_dir);
        assert_eq!(manifest.get("C").unwrap().updated, NEW_TS);
        let mut active_paths = manifest.active_artifact_paths("C");
        active_paths.sort();
        assert_eq!(active_paths, vec!["A/C".to_string(), "B/C".to_string()]);

        std::fs::remove_dir_all(output_dir).ok();
    }

    #[tokio::test]
    async fn hierarchy_invocation_validation_plan_is_shared_across_overlapping_roots() {
        let server = PlanValidationServer::start();
        let client = JiraApiClient::new_for_test(&server.base_url);
        let output_dir = seeded_hierarchy_dir("jarkdown-shared-plan");
        std::fs::write(output_dir.join("A.hierarchy.md"), "A snapshot").unwrap();
        std::fs::write(output_dir.join("B.hierarchy.md"), "B snapshot").unwrap();
        let fingerprint = export_option_fingerprint(None, None, true);
        let mut manifest = Manifest::default();
        manifest.record_hierarchy(
            &jarkdown::IssueNode {
                key: "A".to_string(),
                summary: "Root A".to_string(),
                issue_type: "Task".to_string(),
                updated: OLD_TS.to_string(),
                children_discovered: true,
                truncated: false,
                failures: Vec::new(),
                children: vec![jarkdown::IssueNode {
                    key: "C".to_string(),
                    summary: "Shared C".to_string(),
                    issue_type: "Task".to_string(),
                    updated: OLD_TS.to_string(),
                    children_discovered: true,
                    truncated: false,
                    failures: Vec::new(),
                    children: Vec::new(),
                }],
            },
            "A.hierarchy.md",
            HierarchyLayout::Corpus,
            fingerprint.as_deref(),
        );
        manifest.record_hierarchy(
            &jarkdown::IssueNode {
                key: "B".to_string(),
                summary: "Root B".to_string(),
                issue_type: "Task".to_string(),
                updated: OLD_TS.to_string(),
                children_discovered: true,
                truncated: false,
                failures: Vec::new(),
                children: vec![jarkdown::IssueNode {
                    key: "C".to_string(),
                    summary: "Shared C".to_string(),
                    issue_type: "Task".to_string(),
                    updated: OLD_TS.to_string(),
                    children_discovered: true,
                    truncated: false,
                    failures: Vec::new(),
                    children: Vec::new(),
                }],
            },
            "B.hierarchy.md",
            HierarchyLayout::Corpus,
            fingerprint.as_deref(),
        );
        manifest.save(&output_dir).unwrap();

        let mut shared = shared_args();
        shared.hierarchy = true;
        shared.incremental = true;
        shared.no_attachments = true;
        let roots = vec!["A".to_string(), "B".to_string()];
        let plan = build_hierarchy_validation_plan(&client, &roots, &output_dir, &shared)
            .await
            .expect("validation plan");

        assert_eq!(plan.len(), 3);
        for root in &roots {
            let result = run_hierarchy_export_with_validation(
                &client,
                root,
                &output_dir,
                &shared,
                Some(&plan),
            )
            .await;
            assert!(result.success, "{root} should warm skip");
        }

        let paths = server.observed_paths();
        let validation_requests = paths
            .iter()
            .filter(|path| path.contains("/rest/api/3/search/jql"))
            .count();
        assert_eq!(
            validation_requests, 1,
            "overlapping roots should share one validation request: {:?}",
            paths
        );
        assert!(
            !paths.iter().any(|path| path.contains("/rest/api/3/issue/")),
            "warm skips with a shared plan must not fetch full Issues: {:?}",
            paths
        );

        std::fs::remove_dir_all(output_dir).ok();
    }

    #[tokio::test]
    async fn hierarchy_validation_plan_uses_current_invocation_roots_not_prior_roots() {
        let server = PlanValidationServer::start();
        let client = JiraApiClient::new_for_test(&server.base_url);
        let output_dir = seeded_hierarchy_dir("jarkdown-scoped-plan");
        let mut manifest = Manifest::default();
        manifest.record_hierarchy(
            &jarkdown::IssueNode {
                key: "A".to_string(),
                summary: "Root A".to_string(),
                issue_type: "Task".to_string(),
                updated: OLD_TS.to_string(),
                children_discovered: true,
                truncated: false,
                failures: Vec::new(),
                children: vec![jarkdown::IssueNode {
                    key: "C".to_string(),
                    summary: "Shared C".to_string(),
                    issue_type: "Task".to_string(),
                    updated: OLD_TS.to_string(),
                    children_discovered: true,
                    truncated: false,
                    failures: Vec::new(),
                    children: Vec::new(),
                }],
            },
            "A.hierarchy.md",
            HierarchyLayout::Corpus,
            None,
        );
        manifest.record_hierarchy(
            &jarkdown::IssueNode {
                key: "B".to_string(),
                summary: "Prior Root B".to_string(),
                issue_type: "Task".to_string(),
                updated: OLD_TS.to_string(),
                children_discovered: true,
                truncated: false,
                failures: Vec::new(),
                children: Vec::new(),
            },
            "B.hierarchy.md",
            HierarchyLayout::Corpus,
            None,
        );
        manifest.save(&output_dir).unwrap();

        let mut shared = shared_args();
        shared.hierarchy = true;
        shared.incremental = true;
        let roots = vec!["A".to_string()];
        let _plan = build_hierarchy_validation_plan(&client, &roots, &output_dir, &shared)
            .await
            .expect("validation plan");

        let paths = server.observed_paths();
        assert_eq!(paths.len(), 1);
        assert!(
            paths[0].contains("%22A%22") && paths[0].contains("%22C%22"),
            "current root and active descendant should be validated: {:?}",
            paths
        );
        assert!(
            !paths[0].contains("%22B%22"),
            "prior root absent from the current invocation must not be validated: {:?}",
            paths
        );

        std::fs::remove_dir_all(output_dir).ok();
    }

    fn shared_args() -> jarkdown::cli::SharedArgs {
        jarkdown::cli::SharedArgs {
            output: None,
            manifest: None,
            verbose: false,
            refresh_fields: false,
            include_fields: None,
            exclude_fields: None,
            include_json: false,
            attachment_concurrency: 4,
            no_attachments: false,
            issue_timeout_seconds: 300,
            incremental: false,
            force: false,
            hierarchy: false,
            hierarchy_layout: None,
            max_depth: 2,
            max_issues: 200,
            include_changelog: false,
        }
    }

    const OLD_TS: &str = "2026-01-01T00:00:00.000+0000";
    const NEW_TS: &str = "2026-02-01T00:00:00.000+0000";

    fn seeded_hierarchy_dir(prefix: &str) -> std::path::PathBuf {
        let output_dir = std::env::temp_dir().join(format!(
            "{}-{}",
            prefix,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        for key in ["A", "B", "C"] {
            std::fs::create_dir_all(output_dir.join(key)).unwrap();
            std::fs::write(output_dir.join(key).join(format!("{}.md", key)), key).unwrap();
        }
        output_dir
    }

    fn seeded_nested_shared_dir(prefix: &str) -> std::path::PathBuf {
        let output_dir = std::env::temp_dir().join(format!(
            "{}-{}",
            prefix,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        for path in [
            output_dir.join("A"),
            output_dir.join("A").join("C"),
            output_dir.join("B"),
            output_dir.join("B").join("C"),
        ] {
            std::fs::create_dir_all(path).unwrap();
        }
        std::fs::write(output_dir.join("A").join("A.md"), "A").unwrap();
        std::fs::write(output_dir.join("A").join("C").join("C.md"), "old C").unwrap();
        std::fs::write(output_dir.join("B").join("B.md"), "B").unwrap();
        std::fs::write(output_dir.join("B").join("C").join("C.md"), "old C").unwrap();
        output_dir
    }

    fn seed_manifest_tree(output_dir: &std::path::Path, tree: jarkdown::IssueNode) {
        let mut manifest = Manifest::default();
        let fingerprint = export_option_fingerprint(None, None, true);
        manifest.record_hierarchy(
            &tree,
            "A.hierarchy.md",
            HierarchyLayout::Corpus,
            fingerprint.as_deref(),
        );
        manifest.save(output_dir).unwrap();
    }

    struct WarmHierarchyServer {
        base_url: String,
        requests: Arc<Mutex<Vec<String>>>,
    }

    #[derive(Clone, Copy)]
    enum ChangedHierarchyShape {
        Leaf,
        Intermediate,
        SharedNested,
    }

    struct ChangedHierarchyServer {
        base_url: String,
        requests: Arc<Mutex<Vec<String>>>,
    }

    struct PlanValidationServer {
        base_url: String,
        requests: Arc<Mutex<Vec<String>>>,
    }

    impl ChangedHierarchyServer {
        fn start(shape: ChangedHierarchyShape) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let addr = listener.local_addr().expect("addr");
            let base_url = format!("http://{}", addr);
            let requests = Arc::new(Mutex::new(Vec::new()));
            let thread_requests = requests.clone();
            thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    handle_changed_hierarchy_request(stream, &thread_requests, shape);
                }
            });
            Self { base_url, requests }
        }

        fn observed_paths(&self) -> Vec<String> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl WarmHierarchyServer {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let addr = listener.local_addr().expect("addr");
            let base_url = format!("http://{}", addr);
            let requests = Arc::new(Mutex::new(Vec::new()));
            let thread_requests = requests.clone();
            thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    handle_warm_hierarchy_request(stream, &thread_requests);
                }
            });
            Self { base_url, requests }
        }

        fn observed_paths(&self) -> Vec<String> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl PlanValidationServer {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let addr = listener.local_addr().expect("addr");
            let base_url = format!("http://{}", addr);
            let requests = Arc::new(Mutex::new(Vec::new()));
            let thread_requests = requests.clone();
            thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    handle_plan_validation_request(stream, &thread_requests);
                }
            });
            Self { base_url, requests }
        }

        fn observed_paths(&self) -> Vec<String> {
            self.requests.lock().unwrap().clone()
        }
    }

    fn handle_warm_hierarchy_request(mut stream: TcpStream, requests: &Arc<Mutex<Vec<String>>>) {
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).expect("read");
        let req = String::from_utf8_lossy(&buf[..n]);
        let path = req
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or("/")
            .to_string();
        requests.lock().unwrap().push(path.clone());

        let (status, body) = if path.starts_with("/rest/api/3/search/jql") {
            (
                "200 OK",
                r#"{"issues":[
                    {"key":"A","fields":{"updated":"2026-01-01T00:00:00.000+0000","summary":"Root A","issuetype":{"name":"Task"},"status":{"name":"Open"}}},
                    {"key":"B","fields":{"updated":"2026-01-01T00:00:00.000+0000","summary":"Child B","issuetype":{"name":"Task"},"status":{"name":"Open"}}}
                ]}"#
                .to_string(),
            )
        } else {
            ("500 Internal Server Error", "{}".to_string())
        };
        let response = format!(
            "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            status,
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).expect("write");
    }

    fn handle_changed_hierarchy_request(
        mut stream: TcpStream,
        requests: &Arc<Mutex<Vec<String>>>,
        shape: ChangedHierarchyShape,
    ) {
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).expect("read");
        let req = String::from_utf8_lossy(&buf[..n]);
        let path = req
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or("/")
            .to_string();
        requests.lock().unwrap().push(path.clone());

        let body = if path.starts_with("/rest/api/3/search/jql")
            && (path.contains("fields=updated") || path.contains("updated%2Csummary"))
        {
            changed_validation_body(shape)
        } else if path.starts_with("/rest/api/3/search/jql") {
            r#"{"issues":[]}"#.to_string()
        } else if path.starts_with("/rest/api/3/field") {
            "[]".to_string()
        } else if path.starts_with("/rest/api/3/issue/B") {
            changed_issue_body(
                "B",
                NEW_TS,
                matches!(shape, ChangedHierarchyShape::Intermediate),
            )
        } else if path.starts_with("/rest/api/3/issue/C") {
            let updated = if matches!(shape, ChangedHierarchyShape::SharedNested) {
                NEW_TS
            } else {
                OLD_TS
            };
            changed_issue_body("C", updated, false)
        } else if path.starts_with("/rest/api/3/issue/A") {
            r#"{"error":"A should not be fetched"}"#.to_string()
        } else {
            "{}".to_string()
        };
        let status = if path.starts_with("/rest/api/3/issue/A") {
            "500 Internal Server Error"
        } else {
            "200 OK"
        };
        let response = format!(
            "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            status,
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).expect("write");
    }

    fn handle_plan_validation_request(mut stream: TcpStream, requests: &Arc<Mutex<Vec<String>>>) {
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).expect("read");
        let req = String::from_utf8_lossy(&buf[..n]);
        let path = req
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or("/")
            .to_string();
        requests.lock().unwrap().push(path.clone());

        let (status, body) = if path.starts_with("/rest/api/3/search/jql") {
            (
                "200 OK",
                format!(
                    r#"{{"issues":[
                        {{"key":"A","fields":{{"updated":"{}","summary":"Root A","issuetype":{{"name":"Task"}},"status":{{"name":"Open"}}}}}},
                        {{"key":"B","fields":{{"updated":"{}","summary":"Root B","issuetype":{{"name":"Task"}},"status":{{"name":"Open"}}}}}},
                        {{"key":"C","fields":{{"updated":"{}","summary":"Shared C","issuetype":{{"name":"Task"}},"status":{{"name":"Open"}}}}}}
                    ]}}"#,
                    OLD_TS, OLD_TS, OLD_TS
                ),
            )
        } else {
            ("500 Internal Server Error", "{}".to_string())
        };
        let response = format!(
            "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            status,
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).expect("write");
    }

    fn changed_validation_body(shape: ChangedHierarchyShape) -> String {
        if matches!(shape, ChangedHierarchyShape::SharedNested) {
            return format!(
                r#"{{"issues":[
                    {{"key":"A","fields":{{"updated":"{}","summary":"Root A","issuetype":{{"name":"Task"}},"status":{{"name":"Open"}}}}}},
                    {{"key":"C","fields":{{"updated":"{}","summary":"Shared C","issuetype":{{"name":"Task"}},"status":{{"name":"Open"}}}}}}
                ]}}"#,
                OLD_TS, NEW_TS
            );
        }
        let extra = if matches!(shape, ChangedHierarchyShape::Intermediate) {
            format!(
                r#",{{"key":"C","fields":{{"updated":"{}","summary":"Grandchild C","issuetype":{{"name":"Task"}},"status":{{"name":"Open"}}}}}}"#,
                OLD_TS
            )
        } else {
            String::new()
        };
        format!(
            r#"{{"issues":[
                {{"key":"A","fields":{{"updated":"{}","summary":"Root A","issuetype":{{"name":"Task"}},"status":{{"name":"Open"}}}}}},
                {{"key":"B","fields":{{"updated":"{}","summary":"Child B","issuetype":{{"name":"Task"}},"status":{{"name":"Open"}}}}}}{}
            ]}}"#,
            OLD_TS, NEW_TS, extra
        )
    }

    fn changed_issue_body(key: &str, updated: &str, child_c: bool) -> String {
        let links = if child_c {
            r#"{"type":{"outward":"is implemented by","inward":"implements"},"outwardIssue":{"key":"C"}}"#
        } else {
            ""
        };
        format!(
            r#"{{
                "key":"{}",
                "renderedFields":{{}},
                "fields":{{
                    "summary":"Issue {}",
                    "updated":"{}",
                    "description":null,
                    "issuetype":{{"name":"Task"}},
                    "status":{{"name":"Open","statusCategory":{{"name":"To Do"}}}},
                    "priority":{{"name":"Medium"}},
                    "resolution":null,
                    "project":{{"name":"P","key":"PROJ"}},
                    "assignee":null,"reporter":null,"creator":null,
                    "labels":[],"components":[],"parent":null,"subtasks":[],
                    "issuelinks":[{}],"worklog":{{"worklogs":[]}},
                    "comment":{{"comments":[]}},"attachment":[]
                }}
            }}"#,
            key, key, updated, links
        )
    }
}
