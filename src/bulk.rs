//! Bulk export engine for exporting multiple Jira issues concurrently.

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::Utc;
use futures::stream::{self, StreamExt};
use serde_json::Value;
use tokio::sync::Semaphore;
use std::sync::Arc;

use log::info;

use crate::error::Result;
use crate::export::perform_export;
use crate::jira_client::JiraApiClient;
use crate::manifest::Manifest;

/// Result of a single issue export attempt.
#[derive(Debug, Clone)]
pub struct ExportResult {
    pub issue_key: String,
    pub success: bool,
    pub output_path: Option<PathBuf>,
    pub error: Option<String>,
}

/// Orchestrates concurrent export of multiple Jira issues.
pub struct BulkExporter {
    api_client: JiraApiClient,
    semaphore: Arc<Semaphore>,
    pub output_dir: PathBuf,
    refresh_fields: bool,
    include_fields: Option<String>,
    exclude_fields: Option<String>,
    include_json: bool,
    attachment_concurrency: usize,
    incremental: bool,
    force: bool,
}

impl BulkExporter {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        api_client: JiraApiClient,
        concurrency: usize,
        output_dir: Option<&str>,
        batch_name: Option<&str>,
        refresh_fields: bool,
        include_fields: Option<&str>,
        exclude_fields: Option<&str>,
        include_json: bool,
        attachment_concurrency: usize,
        incremental: bool,
        force: bool,
    ) -> Self {
        let mut dir = output_dir
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        if let Some(name) = batch_name {
            dir = dir.join(name);
        }

        Self {
            api_client,
            semaphore: Arc::new(Semaphore::new(concurrency)),
            output_dir: dir,
            refresh_fields,
            include_fields: include_fields.map(|s| s.to_string()),
            exclude_fields: exclude_fields.map(|s| s.to_string()),
            include_json,
            attachment_concurrency,
            incremental,
            force,
        }
    }

    /// Export multiple issues concurrently with semaphore-limited concurrency.
    pub async fn export_bulk(
        &self,
        issue_keys: &[String],
    ) -> (Vec<ExportResult>, Vec<ExportResult>) {
        let total = issue_keys.len();

        // Load manifest for incremental support
        let manifest = if self.incremental {
            Some(Manifest::load(&self.output_dir))
        } else {
            None
        };

        let results: Vec<ExportResult> = stream::iter(issue_keys.iter().enumerate())
            .map(|(i, key)| {
                let sem = self.semaphore.clone();
                let client = self.api_client.clone();
                let output_dir = self.output_dir.clone();
                let refresh = self.refresh_fields;
                let inc = self.include_fields.clone();
                let exc = self.exclude_fields.clone();
                let json = self.include_json;
                let att_concurrency = self.attachment_concurrency;
                let key = key.clone();
                let manifest_ref = manifest.clone();
                let force = self.force;

                async move {
                    let _permit = sem.acquire().await.unwrap();
                    eprint!("\rExporting {}/{}... ({})", i + 1, total, key);

                    // Incremental check: fetch issue metadata to compare timestamps
                    if let Some(ref m) = manifest_ref {
                        if !force {
                            if let Ok(issue_data) = client.fetch_issue(&key).await {
                                let updated = issue_data["fields"]["updated"]
                                    .as_str()
                                    .unwrap_or("");
                                if !m.is_stale(&key, updated) {
                                    info!("Skipping {} (unchanged)", key);
                                    let path = output_dir.join(&key);
                                    return ExportResult {
                                        issue_key: key,
                                        success: true,
                                        output_path: Some(path),
                                        error: None,
                                    };
                                }
                            }
                        }
                    }

                    let output_path = output_dir.join(&key);
                    match perform_export(
                        &client,
                        &key,
                        &output_path,
                        refresh,
                        inc.as_deref(),
                        exc.as_deref(),
                        json,
                        att_concurrency,
                    )
                    .await
                    {
                        Ok(path) => ExportResult {
                            issue_key: key,
                            success: true,
                            output_path: Some(path),
                            error: None,
                        },
                        Err(e) => ExportResult {
                            issue_key: key,
                            success: false,
                            output_path: None,
                            error: Some(e.to_string()),
                        },
                    }
                }
            })
            .buffer_unordered(total)
            .collect()
            .await;

        eprintln!(); // newline after progress

        // Update manifest with successful exports
        if self.incremental {
            let mut manifest = manifest.unwrap_or_default();
            for r in &results {
                if r.success {
                    // We record the current time as a proxy; the actual `updated`
                    // field will be compared on the next run.
                    manifest.record(&r.issue_key, &Utc::now().to_rfc3339());
                }
            }
            if let Err(e) = manifest.save(&self.output_dir) {
                eprintln!("Warning: Failed to save manifest: {}", e);
            }
        }

        let mut successes = Vec::new();
        let mut failures = Vec::new();
        for r in results {
            if r.success {
                successes.push(r);
            } else {
                failures.push(r);
            }
        }
        (successes, failures)
    }

    /// Generate index.md content as a Markdown summary table.
    pub fn generate_index_md(
        &self,
        results: &[ExportResult],
        all_issues_data: &HashMap<String, Value>,
    ) -> String {
        let total = results.len();
        let succeeded = results.iter().filter(|r| r.success).count();
        let failed = total - succeeded;
        let today = Utc::now().format("%Y-%m-%d").to_string();

        let mut lines = vec![
            "# Export Summary".to_string(),
            String::new(),
            format!(
                "Exported: {} of {} issues | Date: {} | Failed: {}",
                succeeded, total, today, failed
            ),
            String::new(),
            "| Key | Summary | Status | Type | Assignee | Result |".to_string(),
            "|-----|---------|--------|------|----------|--------|".to_string(),
        ];

        let mut sorted_results: Vec<&ExportResult> = results.iter().collect();
        sorted_results.sort_by(|a, b| a.issue_key.cmp(&b.issue_key));

        for result in sorted_results {
            let issue_data = all_issues_data.get(&result.issue_key);
            let fields = issue_data.map(|d| &d["fields"]);

            let summary = fields
                .and_then(|f| f["summary"].as_str())
                .unwrap_or("-");
            let status = fields
                .and_then(|f| f["status"]["name"].as_str())
                .unwrap_or("-");
            let issue_type = fields
                .and_then(|f| f["issuetype"]["name"].as_str())
                .unwrap_or("-");
            let assignee = fields
                .and_then(|f| f["assignee"]["displayName"].as_str())
                .unwrap_or("-");

            let (key_link, result_col) = if result.success {
                (
                    format!(
                        "[{}]({}/{}.md)",
                        result.issue_key, result.issue_key, result.issue_key
                    ),
                    "\u{2713}".to_string(),
                )
            } else {
                (
                    format!("[{}](#)", result.issue_key),
                    format!(
                        "\u{2717} {}",
                        result.error.as_deref().unwrap_or("Unknown error")
                    ),
                )
            };

            lines.push(format!(
                "| {} | {} | {} | {} | {} | {} |",
                key_link, summary, status, issue_type, assignee, result_col
            ));
        }

        lines.join("\n") + "\n"
    }

    /// Write index.md to the output directory.
    pub async fn write_index_md(
        &self,
        results: &[ExportResult],
        issues_data: &HashMap<String, Value>,
    ) -> Result<()> {
        tokio::fs::create_dir_all(&self.output_dir).await?;
        let content = self.generate_index_md(results, issues_data);
        let index_path = self.output_dir.join("index.md");
        tokio::fs::write(&index_path, content).await?;
        Ok(())
    }
}
