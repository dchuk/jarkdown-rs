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
use crate::issue::Issue;
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
    no_attachments: bool,
    issue_timeout: Duration,
    incremental: bool,
    force: bool,
    include_changelog: bool,
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
        }
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
                let no_attachments = self.no_attachments;
                let issue_timeout = self.issue_timeout;
                let key = key.clone();
                let manifest_ref = manifest.clone();
                let force = self.force;
                let include_changelog = self.include_changelog;

                async move {
                    let _permit = sem.acquire().await.unwrap();
                    emit_progress(
                        stderr_is_terminal,
                        &format!("Exporting {}/{}... ({})", i + 1, total, key),
                    );

                    let key_for_timeout = key.clone();
                    let export = async {
                        // Incremental check: fetch issue metadata to compare timestamps
                        if let Some(ref m) = manifest_ref {
                            if !force {
                                if let Ok(issue) = client.fetch_issue(&key).await {
                                    if !m.is_stale(&key, &issue.updated) {
                                        let path = output_dir.join(&key);
                                        // Backfill: if changelog opt-in is on but the file
                                        // is missing (e.g. user just enabled the flag),
                                        // fetch and write the changelog without re-fetching
                                        // or rewriting the main issue payload.
                                        let changelog_path = path
                                            .join(format!("{}.changelog.md", key));
                                        if include_changelog && !changelog_path.exists() {
                                            backfill_changelog(
                                                &client,
                                                &key,
                                                &path,
                                                &issue,
                                                json,
                                            )
                                            .await;
                                        } else {
                                            info!("Skipping {} (unchanged)", key);
                                        }
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
                            },
                            Err(e) => ExportResult {
                                issue_key: key,
                                success: false,
                                output_path: None,
                                error: Some(e.to_string()),
                            },
                        }
                    };

                    let result = timeout_export(&key_for_timeout, issue_timeout, export).await;
                    emit_progress(stderr_is_terminal, &finish_message(i + 1, total, &result));
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

async fn backfill_changelog(
    client: &JiraApiClient,
    issue_key: &str,
    output_path: &std::path::Path,
    issue: &Issue,
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
    let body = changelog::render_changelog_file(issue_key, &issue.summary, &entries, Utc::now());
    let md_path = output_path.join(format!("{}.changelog.md", issue_key));
    if let Err(e) = tokio::fs::write(&md_path, body).await {
        log::warn!("Backfill: failed to write {:?}: {}", md_path, e);
        return;
    }
    info!(
        "Backfilled changelog for {} ({} rows)",
        issue_key,
        changelog::row_count(&entries)
    );
    if include_json {
        let json_path = output_path.join(format!("{}.changelog.json", issue_key));
        if let Ok(s) = serde_json::to_string_pretty(&entries) {
            let _ = tokio::fs::write(&json_path, s).await;
        }
    }
}

async fn timeout_export<F>(issue_key: &str, timeout: Duration, export: F) -> ExportResult
where
    F: std::future::Future<Output = ExportResult>,
{
    match time::timeout(timeout, export).await {
        Ok(result) => result,
        Err(_) => ExportResult {
            issue_key: issue_key.to_string(),
            success: false,
            output_path: None,
            error: Some(format!("timed out after {}s", timeout.as_secs())),
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

        std::fs::remove_dir_all(&output_dir).ok();
    }

    struct BackfillServer {
        base_url: String,
    }

    impl BackfillServer {
        fn start(updated_ts: &'static str) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let addr = listener.local_addr().expect("addr");
            let base_url = format!("http://{}", addr);
            let ts: &'static str = updated_ts;

            thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    handle_backfill(stream, ts);
                }
            });

            Self { base_url }
        }
    }

    fn handle_backfill(mut stream: TcpStream, updated_ts: &str) {
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).expect("read");
        let req = String::from_utf8_lossy(&buf[..n]);
        let path = req
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or("/")
            .to_string();

        let body = if path.starts_with("/rest/api/3/issue/K1/changelog") {
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
        })
        .await;

        assert_eq!(result.issue_key, "K1");
        assert!(!result.success);
        assert_eq!(result.error.as_deref(), Some("timed out after 0s"));
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
