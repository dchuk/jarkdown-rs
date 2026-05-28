//! Bulk export engine for exporting multiple Jira issues concurrently.

use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::Duration;

use chrono::Utc;
use futures::stream::{self, StreamExt};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::time;

use log::info;

use crate::error::Result;
use crate::export::{perform_export_with_options, ExportWorkflowOptions};
use crate::freshness::{self, ExportPlan, PlanOptions};
use crate::jira_client::{JiraApiClient, ValidationIssue};
use crate::manifest::{
    default_manifest_path, export_option_fingerprint, normalize_issue_key, relative_artifact_path,
    EvictionReason, ExportFingerprintOptions, IssueCacheState, Manifest,
};

/// Result of a single issue export attempt.
#[derive(Debug, Clone)]
pub struct ExportResult {
    pub issue_key: String,
    pub success: bool,
    pub output_path: Option<PathBuf>,
    pub error: Option<String>,
}

struct BulkTaskOutcome {
    result: ExportResult,
    manifest_update: Option<ManifestUpdate>,
}

enum ManifestUpdate {
    Validated(ManifestValidationUpdate),
    Evicted {
        issue_key: String,
        reason: EvictionReason,
    },
}

struct ManifestValidationUpdate {
    issue: ValidationIssue,
    artifact_path: String,
    option_fingerprint: Option<String>,
}

impl From<ExportResult> for BulkTaskOutcome {
    fn from(result: ExportResult) -> Self {
        Self {
            result,
            manifest_update: None,
        }
    }
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
    no_attachments: bool,
    issue_timeout: Duration,
    incremental: bool,
    force: bool,
    include_changelog: bool,
    manifest_path: Option<PathBuf>,
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
            no_attachments: false,
            issue_timeout: Duration::from_secs(300),
            incremental,
            force,
            include_changelog: false,
            manifest_path: None,
        }
    }

    pub fn with_manifest_path(mut self, manifest_path: Option<&str>) -> Self {
        self.manifest_path = manifest_path.map(PathBuf::from);
        self
    }

    pub fn with_include_changelog(mut self, include_changelog: bool) -> Self {
        self.include_changelog = include_changelog;
        self
    }

    pub fn with_no_attachments(mut self, no_attachments: bool) -> Self {
        self.no_attachments = no_attachments;
        self
    }

    pub fn with_issue_timeout_seconds(mut self, seconds: u64) -> Self {
        self.issue_timeout = Duration::from_secs(seconds);
        self
    }

    /// Export multiple issues concurrently with semaphore-limited concurrency.
    pub async fn export_bulk(
        &self,
        issue_keys: &[String],
    ) -> (Vec<ExportResult>, Vec<ExportResult>) {
        let total = issue_keys.len();
        if total == 0 {
            return (Vec::new(), Vec::new());
        }
        let stderr_is_terminal = std::io::stderr().is_terminal();

        // Load manifest for incremental support
        let manifest_path = self
            .manifest_path
            .clone()
            .unwrap_or_else(|| default_manifest_path(&self.output_dir));
        let manifest = if self.incremental {
            match Manifest::load_from_path(&manifest_path) {
                Ok(manifest) => {
                    manifest.warn_legacy_issue_directories(&self.output_dir);
                    Some(manifest)
                }
                Err(e) => {
                    let failures = issue_keys
                        .iter()
                        .map(|key| ExportResult {
                            issue_key: key.clone(),
                            success: false,
                            output_path: None,
                            error: Some(e.to_string()),
                        })
                        .collect();
                    return (Vec::new(), failures);
                }
            }
        } else {
            None
        };
        let (validation_succeeded, validation) = if self.incremental && !self.force {
            match self.api_client.validate_issue_keys(issue_keys).await {
                Ok(results) => (
                    true,
                    results
                        .into_iter()
                        .map(|issue| (normalize_issue_key(&issue.key), issue))
                        .collect::<HashMap<_, _>>(),
                ),
                Err(e) => {
                    eprintln!(
                        "Warning: Failed to validate incremental manifest through Jira search: {}",
                        e
                    );
                    (false, HashMap::new())
                }
            }
        } else {
            (false, HashMap::new())
        };
        let validation = Arc::new(validation);

        let outcomes: Vec<BulkTaskOutcome> = stream::iter(issue_keys.iter().enumerate())
            .map(|(i, key)| {
                let sem = self.semaphore.clone();
                let client = self.api_client.clone();
                let output_dir = self.output_dir.clone();
                let refresh = self.refresh_fields;
                let inc = self.include_fields.clone();
                let exc = self.exclude_fields.clone();
                let json = self.include_json;
                let att_concurrency = self.attachment_concurrency;
                let no_attachments = self.no_attachments;
                let issue_timeout = self.issue_timeout;
                let key = normalize_issue_key(key);
                let manifest_ref = manifest.clone();
                let force = self.force;
                let include_changelog = self.include_changelog;
                let option_fingerprint = export_option_fingerprint(ExportFingerprintOptions {
                    include_fields: inc.as_deref(),
                    exclude_fields: exc.as_deref(),
                    no_attachments,
                    include_json: json,
                    include_changelog,
                    ..ExportFingerprintOptions::default()
                });
                let option_fingerprint_without_changelog =
                    export_option_fingerprint(ExportFingerprintOptions {
                        include_fields: inc.as_deref(),
                        exclude_fields: exc.as_deref(),
                        no_attachments,
                        include_json: json,
                        include_changelog: false,
                        ..ExportFingerprintOptions::default()
                    });
                let validation = validation.clone();
                let validation_succeeded = validation_succeeded;

                async move {
                    let _permit = sem.acquire().await.unwrap();
                    emit_progress(
                        stderr_is_terminal,
                        &format!("Exporting {}/{}... ({})", i + 1, total, key),
                    );

                    let key_for_timeout = key.clone();
                    let export = async {
                        // Incremental check: validate metadata once per invocation before
                        // deciding whether this Issue needs a full export.
                        if let Some(ref m) = manifest_ref {
                            if !force {
                                let normalized_key = normalize_issue_key(&key);
                                if let Some(validated) = validation.get(&normalized_key) {
                                    let path = output_dir.join(&normalized_key);
                                    let unchanged = ExportResult {
                                        issue_key: normalized_key.clone(),
                                        success: true,
                                        output_path: Some(path.clone()),
                                        error: None,
                                    };
                                    match freshness::plan_metadata(
                                        &normalized_key,
                                        &validated.updated,
                                        m,
                                        PlanOptions {
                                            include_changelog,
                                            include_json: json,
                                            option_fingerprint: option_fingerprint.as_deref(),
                                            option_fingerprint_without_changelog:
                                                option_fingerprint_without_changelog.as_deref(),
                                        },
                                        &path,
                                    ) {
                                        ExportPlan::Skip => {
                                            info!("Skipping {} (unchanged)", key);
                                            return BulkTaskOutcome {
                                                result: unchanged,
                                                manifest_update: Some(ManifestUpdate::Validated(
                                                    ManifestValidationUpdate {
                                                        issue: validated.clone(),
                                                        artifact_path: relative_artifact_path(
                                                            &output_dir,
                                                            &path,
                                                        ),
                                                        option_fingerprint: option_fingerprint
                                                            .clone(),
                                                    },
                                                )),
                                            };
                                        }
                                        ExportPlan::BackfillChangelogOnly => {
                                            // Changelog opt-in is on but the file is
                                            // missing (e.g. the flag was just enabled);
                                            // write it without re-fetching the payload.
                                            let summary = validated
                                                .summary
                                                .as_deref()
                                                .or_else(|| {
                                                    m.get(&normalized_key)
                                                        .and_then(|entry| entry.summary.as_deref())
                                                })
                                                .unwrap_or(&validated.key);
                                            backfill_changelog_with_summary(
                                                &client,
                                                &normalized_key,
                                                summary,
                                                &path,
                                                json,
                                            )
                                            .await;
                                            return BulkTaskOutcome {
                                                result: unchanged,
                                                manifest_update: Some(ManifestUpdate::Validated(
                                                    ManifestValidationUpdate {
                                                        issue: validated.clone(),
                                                        artifact_path: relative_artifact_path(
                                                            &output_dir,
                                                            &path,
                                                        ),
                                                        option_fingerprint: option_fingerprint
                                                            .clone(),
                                                    },
                                                )),
                                            };
                                        }
                                        ExportPlan::Full => {}
                                    }
                                } else if validation_succeeded && m.is_active(&normalized_key) {
                                    info!("Evicting {} (not returned by validation search)", key);
                                    return BulkTaskOutcome {
                                        result: ExportResult {
                                            issue_key: normalized_key.clone(),
                                            success: true,
                                            output_path: Some(output_dir.join(&normalized_key)),
                                            error: None,
                                        },
                                        manifest_update: Some(ManifestUpdate::Evicted {
                                            issue_key: normalized_key,
                                            reason: EvictionReason::NotReturnedByValidationSearch,
                                        }),
                                    };
                                }
                            }
                        }

                        let forced_evicted_entry = force
                            && manifest_ref.as_ref().is_some_and(|m| {
                                m.get(&key)
                                    .is_some_and(|entry| entry.state == IssueCacheState::Evicted)
                            });
                        let output_path = output_dir.join(&key);
                        match perform_export_with_options(
                            &client,
                            &key,
                            &output_path,
                            ExportWorkflowOptions {
                                refresh_fields: refresh,
                                include_fields: inc.as_deref(),
                                exclude_fields: exc.as_deref(),
                                include_json: json,
                                attachment_concurrency: att_concurrency,
                                no_attachments,
                                include_changelog,
                            },
                        )
                        .await
                        {
                            Ok(path) => ExportResult {
                                issue_key: key,
                                success: true,
                                output_path: Some(path),
                                error: None,
                            }
                            .into(),
                            Err(e) if forced_evicted_entry => BulkTaskOutcome {
                                result: ExportResult {
                                    issue_key: key.clone(),
                                    success: false,
                                    output_path: None,
                                    error: Some(format!(
                                        "{}: {}",
                                        EvictionReason::FORCE_FETCH_FAILED,
                                        e
                                    )),
                                },
                                manifest_update: Some(ManifestUpdate::Evicted {
                                    issue_key: key,
                                    reason: EvictionReason::ForceFetchFailed,
                                }),
                            },
                            Err(e) => ExportResult {
                                issue_key: key,
                                success: false,
                                output_path: None,
                                error: Some(e.to_string()),
                            }
                            .into(),
                        }
                    };

                    let result = timeout_export(&key_for_timeout, issue_timeout, export).await;
                    emit_progress(
                        stderr_is_terminal,
                        &finish_message(i + 1, total, &result.result),
                    );
                    result
                }
            })
            .buffer_unordered(total)
            .collect()
            .await;

        if stderr_is_terminal {
            eprintln!(); // newline after progress
        }

        // Update manifest with successful exports
        if self.incremental {
            let mut manifest = manifest.unwrap_or_default();
            for outcome in &outcomes {
                if let Some(update) = &outcome.manifest_update {
                    match update {
                        ManifestUpdate::Validated(update) => {
                            manifest.record_metadata_with_fingerprint(
                                &update.issue.key,
                                &update.issue.updated,
                                update.issue.summary.as_deref(),
                                update.issue.issue_type.as_deref(),
                                update.issue.status.as_deref(),
                                update.artifact_path.clone(),
                                update.option_fingerprint.as_deref(),
                            );
                        }
                        ManifestUpdate::Evicted { issue_key, reason } => {
                            manifest.evict(issue_key, reason.clone());
                        }
                    }
                    continue;
                }
                let r = &outcome.result;
                if r.success {
                    match self.api_client.fetch_issue(&r.issue_key).await {
                        Ok(issue) => {
                            let artifact_path = r
                                .output_path
                                .as_ref()
                                .map(|path| relative_artifact_path(&self.output_dir, path))
                                .unwrap_or_else(|| r.issue_key.clone());
                            let option_fingerprint =
                                export_option_fingerprint(ExportFingerprintOptions {
                                    include_fields: self.include_fields.as_deref(),
                                    exclude_fields: self.exclude_fields.as_deref(),
                                    no_attachments: self.no_attachments,
                                    include_json: self.include_json,
                                    include_changelog: self.include_changelog,
                                    ..ExportFingerprintOptions::default()
                                });
                            manifest.record_issue_with_fingerprint(
                                &issue,
                                artifact_path,
                                option_fingerprint.as_deref(),
                            );
                        }
                        Err(e) => {
                            eprintln!(
                                "Warning: Failed to refresh manifest metadata for {}: {}",
                                r.issue_key, e
                            );
                        }
                    }
                }
            }
            if let Err(e) = manifest.save_to_path(&manifest_path) {
                eprintln!("Warning: Failed to save manifest: {}", e);
            }
        }

        let mut successes = Vec::new();
        let mut failures = Vec::new();
        for outcome in outcomes {
            let r = outcome.result;
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

            let summary = fields.and_then(|f| f["summary"].as_str()).unwrap_or("-");
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

async fn backfill_changelog_with_summary(
    client: &JiraApiClient,
    issue_key: &str,
    summary: &str,
    output_path: &std::path::Path,
    include_json: bool,
) {
    use crate::changelog;
    let entries = match client.fetch_changelog(issue_key).await {
        Ok(e) => e,
        Err(e) => {
            log::warn!(
                "Backfill: failed to fetch changelog for {}: {}",
                issue_key,
                e
            );
            return;
        }
    };
    if let Err(e) = tokio::fs::create_dir_all(output_path).await {
        log::warn!("Backfill: failed to create {:?}: {}", output_path, e);
        return;
    }
    match changelog::write_artifacts(issue_key, summary, &entries, output_path, include_json).await
    {
        Ok(summary) => info!(
            "Backfilled changelog for {} ({} rows)",
            issue_key, summary.entry_count
        ),
        Err(e) => log::warn!(
            "Backfill: failed to write changelog for {}: {}",
            issue_key,
            e
        ),
    }
}

async fn timeout_export<F>(issue_key: &str, timeout: Duration, export: F) -> BulkTaskOutcome
where
    F: std::future::Future<Output = BulkTaskOutcome>,
{
    match time::timeout(timeout, export).await {
        Ok(result) => result,
        Err(_) => BulkTaskOutcome {
            result: ExportResult {
                issue_key: issue_key.to_string(),
                success: false,
                output_path: None,
                error: Some(format!("timed out after {}s", timeout.as_secs())),
            },
            manifest_update: None,
        },
    }
}

fn emit_progress(stderr_is_terminal: bool, message: &str) {
    if stderr_is_terminal {
        eprint!("\r{}", message);
    } else {
        eprintln!("{}", message);
    }
}

fn finish_message(position: usize, total: usize, result: &ExportResult) -> String {
    if result.success {
        format!("Finished {}/{}... ({})", position, total, result.issue_key)
    } else {
        format!(
            "Failed {}/{}... ({}): {}",
            position,
            total,
            result.issue_key,
            result.error.as_deref().unwrap_or("Unknown error")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;
    use std::io::{Read as _, Write as _};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[tokio::test]
    async fn bulk_export_backfills_missing_changelog_md_on_unchanged_issue_under_incremental() {
        let updated_ts = "2024-05-01T12:00:00.000+0000";
        let server = BackfillServer::start(updated_ts);
        let client = JiraApiClient::new_for_test(&server.base_url);
        let output_dir = unique_temp_dir("jarkdown-changelog-backfill");

        // Seed the manifest so K1 appears "unchanged" relative to what the server returns
        let issue_dir = output_dir.join("K1");
        std::fs::create_dir_all(&issue_dir).expect("mkdir issue");
        let mut manifest = Manifest::default();
        manifest.record("K1", updated_ts);
        manifest.save(&output_dir).expect("save manifest");
        // Pre-existing main markdown (proves we won't rewrite it)
        let main_md = issue_dir.join("K1.md");
        std::fs::write(&main_md, "PRE-EXISTING").expect("seed main md");
        let main_mtime_before = std::fs::metadata(&main_md).unwrap().modified().unwrap();

        let mut exporter = BulkExporter::new(
            client,
            /* concurrency */ 1,
            Some(output_dir.to_str().unwrap()),
            None,
            false,
            None,
            None,
            false,
            0,
            /* incremental */ true,
            /* force */ false,
        );
        exporter = exporter.with_include_changelog(true);

        let (successes, failures) = exporter.export_bulk(&["K1".to_string()]).await;
        assert_eq!(failures.len(), 0, "no failures expected");
        assert_eq!(successes.len(), 1);

        let changelog_path = issue_dir.join("K1.changelog.md");
        assert!(
            changelog_path.exists(),
            "changelog must be backfilled even when issue is unchanged"
        );
        let body = std::fs::read_to_string(&changelog_path).unwrap();
        assert!(
            body.contains("**status**: To Do → In Progress"),
            "expected rendered bullet; got:\n{}",
            body
        );

        // Main md should NOT have been re-written
        let main_after = std::fs::read_to_string(&main_md).unwrap();
        assert_eq!(main_after, "PRE-EXISTING", "main .md must be untouched");
        let main_mtime_after = std::fs::metadata(&main_md).unwrap().modified().unwrap();
        assert_eq!(
            main_mtime_before, main_mtime_after,
            "main .md mtime must not change"
        );
        assert!(
            !server.saw_full_issue_fetch("K1"),
            "unchanged warm incremental export must not fetch the full Issue; observed paths: {:?}",
            server.observed_paths()
        );

        std::fs::remove_dir_all(&output_dir).ok();
    }

    #[tokio::test]
    async fn bulk_export_writes_external_manifest_without_moving_artifacts() {
        let updated_ts = "2024-05-01T12:00:00.000+0000";
        let server = BackfillServer::start(updated_ts);
        let client = JiraApiClient::new_for_test(&server.base_url);
        let output_dir = unique_temp_dir("jarkdown-external-manifest");
        let manifest_path = output_dir.join("cache").join("state.json");

        let exporter = BulkExporter::new(
            client,
            /* concurrency */ 1,
            Some(output_dir.to_str().unwrap()),
            None,
            false,
            None,
            None,
            false,
            0,
            /* incremental */ true,
            /* force */ false,
        )
        .with_manifest_path(Some(manifest_path.to_str().unwrap()))
        .with_no_attachments(true);

        let (successes, failures) = exporter.export_bulk(&["K1".to_string()]).await;
        assert_eq!(failures.len(), 0, "no failures expected");
        assert_eq!(successes.len(), 1);

        assert!(output_dir.join("K1").join("K1.md").exists());
        assert!(!output_dir.join(".jarkdown-manifest.json").exists());
        assert!(manifest_path.exists());

        let manifest = Manifest::load_from_path(&manifest_path).expect("manifest");
        let entry = manifest.get("K1").expect("K1 manifest entry");
        assert_eq!(entry.summary.as_deref(), Some("Backfill me"));
        assert_eq!(entry.issue_type.as_deref(), Some("Task"));
        assert_eq!(entry.status.as_deref(), Some("Open"));
        assert_eq!(entry.artifact_paths[0].path, "K1");

        std::fs::remove_dir_all(&output_dir).ok();
    }

    #[tokio::test]
    async fn bulk_export_canonicalizes_lowercase_requested_issue_directory_and_manifest_path() {
        let updated_ts = "2024-05-01T12:00:00.000+0000";
        let server = BackfillServer::start(updated_ts);
        let client = JiraApiClient::new_for_test(&server.base_url);
        let output_dir = unique_temp_dir("jarkdown-canonical-dir");

        let exporter = BulkExporter::new(
            client,
            /* concurrency */ 1,
            Some(output_dir.to_str().unwrap()),
            None,
            false,
            None,
            None,
            false,
            0,
            /* incremental */ true,
            /* force */ false,
        )
        .with_no_attachments(true);

        let (successes, failures) = exporter.export_bulk(&["k1".to_string()]).await;
        assert_eq!(failures.len(), 0, "no failures expected");
        assert_eq!(successes.len(), 1);
        assert_eq!(successes[0].issue_key, "K1");
        let expected_output_path = output_dir.join("K1");
        assert_eq!(
            successes[0].output_path.as_ref(),
            Some(&expected_output_path)
        );

        let dir_names: Vec<String> = std::fs::read_dir(&output_dir)
            .expect("read output dir")
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|ty| ty.is_dir()))
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();
        assert!(
            dir_names.iter().any(|name| name == "K1"),
            "expected byte-for-byte K1 directory entry, got {:?}",
            dir_names
        );
        assert!(
            !dir_names.iter().any(|name| name == "k1"),
            "lowercase issue directory should not be created; got {:?}",
            dir_names
        );
        assert!(output_dir.join("K1").join("K1.md").exists());

        let manifest = Manifest::load(&output_dir);
        let entry = manifest.get("K1").expect("K1 manifest entry");
        assert_eq!(entry.artifact_paths[0].path, "K1");
        assert!(output_dir
            .join(&entry.artifact_paths[0].path)
            .join("K1.md")
            .exists());

        std::fs::remove_dir_all(&output_dir).ok();
    }

    #[tokio::test]
    async fn successful_validation_omission_evicts_active_issue_without_deleting_files() {
        let updated_ts = "2024-05-01T12:00:00.000+0000";
        let server = BackfillServer::start_with_validation(updated_ts, ValidationMode::Empty);
        let client = JiraApiClient::new_for_test(&server.base_url);
        let output_dir = unique_temp_dir("jarkdown-evict-missing");
        let issue_dir = output_dir.join("K1");
        std::fs::create_dir_all(&issue_dir).expect("mkdir issue");
        std::fs::write(issue_dir.join("K1.md"), "KEEP").expect("seed md");
        let mut manifest = Manifest::default();
        manifest.record("K1", updated_ts);
        manifest.save(&output_dir).expect("save manifest");

        let exporter = BulkExporter::new(
            client,
            /* concurrency */ 1,
            Some(output_dir.to_str().unwrap()),
            None,
            false,
            None,
            None,
            false,
            0,
            /* incremental */ true,
            /* force */ false,
        )
        .with_no_attachments(true);

        let (successes, failures) = exporter.export_bulk(&["K1".to_string()]).await;
        assert_eq!(
            failures.len(),
            0,
            "eviction should not be an export failure"
        );
        assert_eq!(successes.len(), 1);
        assert!(
            issue_dir.join("K1.md").exists(),
            "eviction must not delete files"
        );
        assert!(
            !server.saw_full_issue_fetch("K1"),
            "successful validation omission should evict without full fetch; observed paths: {:?}",
            server.observed_paths()
        );

        let manifest = Manifest::load(&output_dir);
        let entry = manifest.get("K1").expect("evicted entry remains");
        assert_eq!(entry.state, crate::manifest::IssueCacheState::Evicted);
        assert_eq!(
            entry.eviction_reason,
            Some(EvictionReason::NotReturnedByValidationSearch)
        );
        assert!(!entry.artifact_paths[0].active);

        std::fs::remove_dir_all(&output_dir).ok();
    }

    #[tokio::test]
    async fn failed_validation_does_not_evict_active_issue() {
        let updated_ts = "2024-05-01T12:00:00.000+0000";
        let server = BackfillServer::start_with_validation(updated_ts, ValidationMode::Fails);
        let client = JiraApiClient::new_for_test(&server.base_url);
        let output_dir = unique_temp_dir("jarkdown-validation-fails");
        let mut manifest = Manifest::default();
        manifest.record("K1", updated_ts);
        manifest.save(&output_dir).expect("save manifest");

        let exporter = BulkExporter::new(
            client,
            /* concurrency */ 1,
            Some(output_dir.to_str().unwrap()),
            None,
            false,
            None,
            None,
            false,
            0,
            /* incremental */ true,
            /* force */ false,
        )
        .with_no_attachments(true);

        let (_successes, failures) = exporter.export_bulk(&["K1".to_string()]).await;
        assert_eq!(failures.len(), 0, "fallback full export should succeed");
        assert!(
            server.saw_full_issue_fetch("K1"),
            "failed validation may fall back to full fetch but must not evict"
        );

        let manifest = Manifest::load(&output_dir);
        let entry = manifest.get("K1").expect("active entry remains");
        assert_eq!(entry.state, crate::manifest::IssueCacheState::Active);
        assert_eq!(entry.eviction_reason, None);

        std::fs::remove_dir_all(&output_dir).ok();
    }

    #[tokio::test]
    async fn force_incremental_reactivates_evicted_manifest_entry() {
        let updated_ts = "2024-05-01T12:00:00.000+0000";
        let server = BackfillServer::start_with_validation(updated_ts, ValidationMode::Empty);
        let client = JiraApiClient::new_for_test(&server.base_url);
        let output_dir = unique_temp_dir("jarkdown-force-reactivates");
        let mut manifest = Manifest::default();
        manifest.record("K1", "old");
        manifest.evict("K1", EvictionReason::NotReturnedByValidationSearch);
        manifest.save(&output_dir).expect("save manifest");

        let exporter = BulkExporter::new(
            client,
            /* concurrency */ 1,
            Some(output_dir.to_str().unwrap()),
            None,
            false,
            None,
            None,
            false,
            0,
            /* incremental */ true,
            /* force */ true,
        )
        .with_no_attachments(true);

        let (_successes, failures) = exporter.export_bulk(&["K1".to_string()]).await;
        assert_eq!(failures.len(), 0, "force export should succeed");
        assert!(
            server.saw_full_issue_fetch("K1"),
            "force incremental should bypass validation/skip planning"
        );

        let manifest = Manifest::load(&output_dir);
        let entry = manifest.get("K1").expect("reactivated entry");
        assert_eq!(entry.state, crate::manifest::IssueCacheState::Active);
        assert_eq!(entry.eviction_reason, None);
        assert!(entry.artifact_paths[0].active);

        std::fs::remove_dir_all(&output_dir).ok();
    }

    #[tokio::test]
    async fn force_incremental_failed_fetch_keeps_evicted_entry_with_force_reason() {
        let updated_ts = "2024-05-01T12:00:00.000+0000";
        let server =
            BackfillServer::start_with_validation(updated_ts, ValidationMode::FullIssueFails);
        let client = JiraApiClient::new_for_test(&server.base_url);
        let output_dir = unique_temp_dir("jarkdown-force-fetch-failed");
        let mut manifest = Manifest::default();
        manifest.record("K1", "old");
        manifest.evict("K1", EvictionReason::NotReturnedByValidationSearch);
        manifest.save(&output_dir).expect("save manifest");

        let exporter = BulkExporter::new(
            client,
            /* concurrency */ 1,
            Some(output_dir.to_str().unwrap()),
            None,
            false,
            None,
            None,
            false,
            0,
            /* incremental */ true,
            /* force */ true,
        )
        .with_no_attachments(true);

        let (successes, failures) = exporter.export_bulk(&["K1".to_string()]).await;
        assert_eq!(successes.len(), 0, "force fetch should fail");
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].issue_key, "K1");
        assert!(
            failures[0]
                .error
                .as_deref()
                .is_some_and(|error| error.contains(EvictionReason::FORCE_FETCH_FAILED)),
            "failure summary should include force_fetch_failed: {:?}",
            failures[0].error
        );
        assert!(
            server.saw_full_issue_fetch("K1"),
            "force incremental should attempt a full Issue fetch"
        );
        let summary = exporter.generate_index_md(&failures, &HashMap::new());
        assert!(
            summary.contains(EvictionReason::FORCE_FETCH_FAILED),
            "generated summary should include force_fetch_failed: {}",
            summary
        );

        let manifest = Manifest::load(&output_dir);
        let entry = manifest.get("K1").expect("evicted entry remains");
        assert_eq!(entry.state, IssueCacheState::Evicted);
        assert_eq!(
            entry.eviction_reason,
            Some(EvictionReason::ForceFetchFailed)
        );
        assert!(!entry.artifact_paths[0].active);

        std::fs::remove_dir_all(&output_dir).ok();
    }

    #[tokio::test]
    async fn force_without_incremental_does_not_write_manifest() {
        let updated_ts = "2024-05-01T12:00:00.000+0000";
        let server = BackfillServer::start(updated_ts);
        let client = JiraApiClient::new_for_test(&server.base_url);
        let output_dir = unique_temp_dir("jarkdown-force-nonincremental");

        let exporter = BulkExporter::new(
            client,
            /* concurrency */ 1,
            Some(output_dir.to_str().unwrap()),
            None,
            false,
            None,
            None,
            false,
            0,
            /* incremental */ false,
            /* force */ true,
        )
        .with_no_attachments(true);

        let (_successes, failures) = exporter.export_bulk(&["K1".to_string()]).await;
        assert_eq!(failures.len(), 0, "force export should succeed");
        assert!(!default_manifest_path(&output_dir).exists());

        std::fs::remove_dir_all(&output_dir).ok();
    }

    struct BackfillServer {
        base_url: String,
        requests: Arc<Mutex<Vec<String>>>,
    }

    #[derive(Clone, Copy)]
    enum ValidationMode {
        ReturnsIssue,
        Empty,
        Fails,
        FullIssueFails,
    }

    impl BackfillServer {
        fn start(updated_ts: &'static str) -> Self {
            Self::start_with_validation(updated_ts, ValidationMode::ReturnsIssue)
        }

        fn start_with_validation(
            updated_ts: &'static str,
            validation_mode: ValidationMode,
        ) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let addr = listener.local_addr().expect("addr");
            let base_url = format!("http://{}", addr);
            let ts: &'static str = updated_ts;
            let requests = Arc::new(Mutex::new(Vec::new()));
            let requests_for_thread = requests.clone();

            thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    handle_backfill(stream, ts, validation_mode, &requests_for_thread);
                }
            });

            Self { base_url, requests }
        }

        fn observed_paths(&self) -> Vec<String> {
            self.requests.lock().unwrap().clone()
        }

        fn saw_full_issue_fetch(&self, issue_key: &str) -> bool {
            let issue_path = format!("/rest/api/3/issue/{}", issue_key);
            self.observed_paths()
                .iter()
                .any(|path| path.starts_with(&issue_path) && !path.contains("/changelog"))
        }
    }

    fn handle_backfill(
        mut stream: TcpStream,
        updated_ts: &str,
        validation_mode: ValidationMode,
        requests: &Arc<Mutex<Vec<String>>>,
    ) {
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).expect("read");
        let req = String::from_utf8_lossy(&buf[..n]);
        let path = req
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or("/")
            .to_string();
        requests.lock().unwrap().push(path.clone());

        if path.starts_with("/rest/api/3/search/jql")
            && matches!(validation_mode, ValidationMode::Fails)
        {
            let resp = "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}";
            let _ = stream.write_all(resp.as_bytes());
            return;
        }

        if path.starts_with("/rest/api/3/issue/K1")
            && !path.contains("/changelog")
            && matches!(validation_mode, ValidationMode::FullIssueFails)
        {
            let body = r#"{"errorMessages":["missing"]}"#;
            let resp = format!(
                "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
            return;
        }

        let body = if path.starts_with("/rest/api/3/search/jql") {
            match validation_mode {
                ValidationMode::ReturnsIssue => format!(
                    r#"{{
                    "issues":[{{
                        "key":"K1",
                        "fields":{{
                            "summary":"Backfill me",
                            "updated":"{}",
                            "issuetype":{{"name":"Task"}},
                            "status":{{"name":"Open"}}
                        }}
                    }}]
                }}"#,
                    updated_ts
                ),
                ValidationMode::Empty | ValidationMode::Fails | ValidationMode::FullIssueFails => {
                    r#"{"issues":[]}"#.to_string()
                }
            }
        } else if path.starts_with("/rest/api/3/issue/K1/changelog") {
            r#"{"startAt":0,"maxResults":100,"total":1,"isLast":true,"values":[
                {
                    "id":"1",
                    "author":{"displayName":"Jane Smith"},
                    "created":"2024-01-20T14:32:17.000+0000",
                    "items":[{"field":"status","fromString":"To Do","toString":"In Progress"}]
                }
            ]}"#
            .to_string()
        } else if path.starts_with("/rest/api/3/issue/K1") {
            format!(
                r#"{{
                    "key":"K1",
                    "renderedFields":{{}},
                    "fields":{{
                        "summary":"Backfill me",
                        "updated":"{}",
                        "description":null,
                        "issuetype":{{"name":"Task"}},
                        "status":{{"name":"Open","statusCategory":{{"name":"To Do"}}}},
                        "priority":{{"name":"Medium"}},
                        "resolution":null,
                        "project":{{"name":"P","key":"PROJ"}},
                        "assignee":null,"reporter":null,"creator":null,
                        "labels":[],"components":[],"parent":null,"subtasks":[],
                        "issuelinks":[],"worklog":{{"worklogs":[]}},
                        "comment":{{"comments":[]}},"attachment":[]
                    }}
                }}"#,
                updated_ts
            )
        } else {
            "{}".to_string()
        };
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(resp.as_bytes());
    }

    fn unique_temp_dir(prefix: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        std::env::temp_dir().join(format!("{}-{}", prefix, nanos))
    }

    #[tokio::test]
    async fn timeout_export_fails_issue_without_waiting_for_future() {
        let result = timeout_export("K1", Duration::from_millis(5), async {
            time::sleep(Duration::from_secs(60)).await;
            ExportResult {
                issue_key: "K1".to_string(),
                success: true,
                output_path: None,
                error: None,
            }
            .into()
        })
        .await;

        assert_eq!(result.result.issue_key, "K1");
        assert!(!result.result.success);
        assert_eq!(result.result.error.as_deref(), Some("timed out after 0s"));
    }

    #[test]
    fn finish_message_includes_success_and_failure_states() {
        let success = ExportResult {
            issue_key: "K1".to_string(),
            success: true,
            output_path: None,
            error: None,
        };
        let failure = ExportResult {
            issue_key: "K2".to_string(),
            success: false,
            output_path: None,
            error: Some("boom".to_string()),
        };

        assert_eq!(finish_message(1, 2, &success), "Finished 1/2... (K1)");
        assert_eq!(finish_message(2, 2, &failure), "Failed 2/2... (K2): boom");
    }
}
