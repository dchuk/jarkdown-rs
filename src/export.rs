//! Shared async export workflow used by both the CLI and BulkExporter.

use std::path::{Path, PathBuf};

use crate::attachment::AttachmentHandler;
use crate::changelog;
use crate::config::ConfigManager;
use crate::error::Result;
use crate::field_cache::FieldMetadataCache;
use crate::jira_client::JiraApiClient;
use crate::markdown::MarkdownConverter;
use log::{info, warn};
use serde_json::Value;

/// Options for the shared export workflow.
#[derive(Debug, Clone, Default)]
pub struct ExportWorkflowOptions<'a> {
    pub refresh_fields: bool,
    pub include_fields: Option<&'a str>,
    pub exclude_fields: Option<&'a str>,
    pub include_json: bool,
    pub attachment_concurrency: usize,
    pub no_attachments: bool,
    pub include_changelog: bool,
}

/// Run the full export workflow for a single Jira issue.
///
/// Fetches issue data, downloads attachments, builds field metadata and
/// config, converts to Markdown, and writes output files.
#[allow(clippy::too_many_arguments)]
pub async fn perform_export(
    api_client: &JiraApiClient,
    issue_key: &str,
    output_path: &Path,
    refresh_fields: bool,
    include_fields: Option<&str>,
    exclude_fields: Option<&str>,
    include_json: bool,
    attachment_concurrency: usize,
) -> Result<PathBuf> {
    perform_export_with_options(
        api_client,
        issue_key,
        output_path,
        ExportWorkflowOptions {
            refresh_fields,
            include_fields,
            exclude_fields,
            include_json,
            attachment_concurrency,
            no_attachments: false,
            include_changelog: false,
        },
    )
    .await
}

/// Run the full export workflow with explicit options.
pub async fn perform_export_with_options(
    api_client: &JiraApiClient,
    issue_key: &str,
    output_path: &Path,
    options: ExportWorkflowOptions<'_>,
) -> Result<PathBuf> {
    // Ensure output directory exists
    tokio::fs::create_dir_all(output_path).await?;

    // Fetch the issue
    let issue = api_client.fetch_issue(issue_key).await?;

    // Download attachments
    let handler = AttachmentHandler::new(api_client);
    let attachments = issue.attachments.clone();
    let downloaded = if options.no_attachments {
        Vec::new()
    } else {
        handler
            .download_all_attachments(&attachments, output_path, options.attachment_concurrency)
            .await
    };

    // Build field metadata cache
    let mut field_cache = FieldMetadataCache::new(&api_client.domain);
    if options.refresh_fields || field_cache.is_stale() {
        match api_client.fetch_fields().await {
            Ok(fields) => {
                field_cache.save(&fields);
                info!("Field metadata cached ({} fields)", fields.len());
            }
            Err(e) => {
                warn!("Failed to refresh field metadata: {}", e);
            }
        }
    }

    // Build field filter
    let config_manager = ConfigManager::new(None);
    let field_filter =
        config_manager.get_field_filter(options.include_fields, options.exclude_fields);

    // Discover child issues for Epics (via JQL) and Ideas (via issue links).
    // `compose_markdown`'s child-issue section still reads raw `Value`s, so
    // search hits are unwrapped back to their `raw` payloads here.
    let child_issues: Vec<Value> = {
        if issue.issuetype.name == "Epic" {
            // Look up the "Epic Link" field ID for JQL, and also query by parent
            let epic_link_field = field_cache.get_field_id_by_name("Epic Link");
            let mut clauses = vec![format!("parent = {}", issue_key)];
            if let Some(ref field_id) = epic_link_field {
                clauses.push(format!("\"{}\" = {}", field_id, issue_key));
            }
            let jql = clauses.join(" OR ");
            match api_client.search_jql(&jql, 200).await {
                Ok(results) => results.into_iter().map(|r| r.raw).collect(),
                Err(e) => {
                    warn!("Failed to fetch child issues for epic: {}", e);
                    Vec::new()
                }
            }
        } else {
            extract_delivery_children(&issue.raw)
        }
    };

    // Fetch + write the changelog artifacts up-front (opt-in); the returned
    // summary lets the main markdown cross-reference the sibling changelog
    // file. A fetch failure here degrades to an empty changelog.
    let changelog_summary: Option<changelog::ChangelogSummary> = if options.include_changelog {
        let entries = match api_client.fetch_changelog(issue_key).await {
            Ok(e) => e,
            Err(e) => {
                warn!("Failed to fetch changelog for {}: {}", issue_key, e);
                Vec::new()
            }
        };
        Some(
            changelog::write_artifacts(
                issue_key,
                &issue.summary,
                &entries,
                output_path,
                options.include_json,
            )
            .await?,
        )
    } else {
        None
    };

    // Convert to Markdown
    let mut converter = MarkdownConverter::new(&api_client.base_url, &api_client.domain);
    let mut cache_opt = Some(field_cache);
    let filter_opt = Some(field_filter);
    let markdown_content = converter.compose_markdown(
        &issue,
        &downloaded,
        if options.no_attachments {
            &attachments
        } else {
            &[]
        },
        &mut cache_opt,
        &filter_opt,
        &child_issues,
        changelog_summary.as_ref(),
    );

    // Write raw JSON (opt-in)
    if options.include_json {
        let json_file = output_path.join(format!("{}.json", issue_key));
        let json_str = serde_json::to_string_pretty(&issue.raw)?;
        tokio::fs::write(&json_file, json_str).await?;
    }

    // Write Markdown
    let md_file = output_path.join(format!("{}.md", issue_key));
    tokio::fs::write(&md_file, markdown_content).await?;

    Ok(output_path.to_path_buf())
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;
    use std::io::{Read as _, Write as _};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[tokio::test]
    async fn export_with_include_changelog_cross_references_changelog_from_main_md() {
        let server = ExportTestServer::start();
        let client = JiraApiClient::new_for_test(&server.base_url);
        let output_dir = unique_temp_dir("jarkdown-changelog-xref");
        let issue_dir = output_dir.join("K1");

        perform_export_with_options(
            &client,
            "K1",
            &issue_dir,
            ExportWorkflowOptions {
                refresh_fields: false,
                include_fields: None,
                exclude_fields: None,
                include_json: false,
                attachment_concurrency: 0,
                no_attachments: true,
                include_changelog: true,
            },
        )
        .await
        .expect("export should succeed");

        let main_md = tokio::fs::read_to_string(issue_dir.join("K1.md"))
            .await
            .expect("read K1.md");

        assert!(
            main_md.contains("changelog: K1.changelog.md"),
            "main .md must reference changelog file in frontmatter; got:\n{}",
            main_md
        );
        assert!(
            main_md.contains("## Changelog"),
            "main .md must include ## Changelog section; got:\n{}",
            main_md
        );
        assert!(
            main_md.contains("[K1.changelog.md](K1.changelog.md)"),
            "section must link to the changelog file; got:\n{}",
            main_md
        );
        assert!(
            main_md.contains("1 entr"),
            "section must mention entry count; got:\n{}",
            main_md
        );

        std::fs::remove_dir_all(&output_dir).ok();
    }

    #[tokio::test]
    async fn export_without_include_changelog_writes_no_file_and_skips_changelog_endpoint() {
        let server = ObservingServer::start();
        let client = JiraApiClient::new_for_test(&server.base_url);
        let output_dir = unique_temp_dir("jarkdown-changelog-off");
        let issue_dir = output_dir.join("K1");

        perform_export_with_options(
            &client,
            "K1",
            &issue_dir,
            ExportWorkflowOptions {
                refresh_fields: false,
                include_fields: None,
                exclude_fields: None,
                include_json: false,
                attachment_concurrency: 0,
                no_attachments: true,
                include_changelog: false,
            },
        )
        .await
        .expect("export should succeed");

        assert!(
            !issue_dir.join("K1.changelog.md").exists(),
            "K1.changelog.md must not exist when flag is off"
        );
        assert!(
            !server.saw_changelog_request(),
            "changelog endpoint must not be hit; observed: {:?}",
            server.observed()
        );

        std::fs::remove_dir_all(&output_dir).ok();
    }

    struct ObservingServer {
        base_url: String,
        requests: Arc<Mutex<Vec<String>>>,
    }

    impl ObservingServer {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let addr = listener.local_addr().expect("addr");
            let base_url = format!("http://{}", addr);
            let requests = Arc::new(Mutex::new(Vec::new()));
            let t = requests.clone();

            thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    handle_observing(stream, &t);
                }
            });

            Self { base_url, requests }
        }

        fn saw_changelog_request(&self) -> bool {
            self.requests
                .lock()
                .unwrap()
                .iter()
                .any(|p| p.contains("/changelog"))
        }

        fn observed(&self) -> Vec<String> {
            self.requests.lock().unwrap().clone()
        }
    }

    fn handle_observing(mut stream: TcpStream, requests: &Arc<Mutex<Vec<String>>>) {
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

        let body = if path.starts_with("/rest/api/3/issue/K1") && !path.contains("/changelog") {
            issue_response()
        } else if path.starts_with("/rest/api/3/field") {
            "[]".to_string()
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

    #[tokio::test]
    async fn export_with_include_changelog_writes_empty_changelog_file_when_no_entries() {
        let server = EmptyChangelogServer::start();
        let client = JiraApiClient::new_for_test(&server.base_url);
        let output_dir = unique_temp_dir("jarkdown-changelog-empty");
        let issue_dir = output_dir.join("K1");

        perform_export_with_options(
            &client,
            "K1",
            &issue_dir,
            ExportWorkflowOptions {
                refresh_fields: false,
                include_fields: None,
                exclude_fields: None,
                include_json: false,
                attachment_concurrency: 0,
                no_attachments: true,
                include_changelog: true,
            },
        )
        .await
        .expect("export should succeed");

        let body = tokio::fs::read_to_string(issue_dir.join("K1.changelog.md"))
            .await
            .expect("read changelog");
        assert!(body.contains("entries: 0"), "expected entries: 0; got:\n{}", body);
        assert!(body.contains("# K1 Changelog"), "expected heading; got:\n{}", body);
        let bullet_lines = body.lines().filter(|l| l.starts_with("- ")).count();
        assert_eq!(bullet_lines, 0, "expected no bullets; got:\n{}", body);

        std::fs::remove_dir_all(&output_dir).ok();
    }

    struct EmptyChangelogServer {
        base_url: String,
    }

    impl EmptyChangelogServer {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let addr = listener.local_addr().expect("addr");
            let base_url = format!("http://{}", addr);

            thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    handle_empty(stream);
                }
            });

            Self { base_url }
        }
    }

    fn handle_empty(mut stream: TcpStream) {
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
            r#"{"startAt":0,"maxResults":100,"total":0,"isLast":true,"values":[]}"#.to_string()
        } else if path.starts_with("/rest/api/3/issue/K1") {
            issue_response()
        } else if path.starts_with("/rest/api/3/field") {
            "[]".to_string()
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

    #[tokio::test]
    async fn export_with_include_changelog_writes_changelog_md_with_frontmatter_and_rows() {
        let server = ExportTestServer::start();
        let client = JiraApiClient::new_for_test(&server.base_url);
        let output_dir = unique_temp_dir("jarkdown-changelog-export");
        let issue_dir = output_dir.join("K1");

        perform_export_with_options(
            &client,
            "K1",
            &issue_dir,
            ExportWorkflowOptions {
                refresh_fields: false,
                include_fields: None,
                exclude_fields: None,
                include_json: false,
                attachment_concurrency: 0,
                no_attachments: true,
                include_changelog: true,
            },
        )
        .await
        .expect("export should succeed");

        let changelog_path = issue_dir.join("K1.changelog.md");
        assert!(
            changelog_path.exists(),
            "expected {} to exist",
            changelog_path.display()
        );
        let body = tokio::fs::read_to_string(&changelog_path)
            .await
            .expect("read changelog file");

        assert!(body.starts_with("---\n"), "missing YAML frontmatter:\n{}", body);
        assert!(body.contains("key: K1"), "missing key in frontmatter:\n{}", body);
        assert!(
            body.contains("summary: Implement auth"),
            "missing summary in frontmatter:\n{}",
            body
        );
        assert!(
            body.contains("issue_file: K1.md"),
            "missing issue_file in frontmatter:\n{}",
            body
        );
        assert!(
            body.contains("entries: 1"),
            "missing entries count in frontmatter:\n{}",
            body
        );
        assert!(
            body.contains("# K1 Changelog"),
            "missing heading:\n{}",
            body
        );
        assert!(
            body.contains("- 2024-01-20T14:32:17Z — Jane Smith — **status**: To Do → In Progress"),
            "missing bullet row:\n{}",
            body
        );

        std::fs::remove_dir_all(&output_dir).ok();
    }

    fn unique_temp_dir(prefix: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        std::env::temp_dir().join(format!("{}-{}", prefix, nanos))
    }

    struct ExportTestServer {
        base_url: String,
    }

    impl ExportTestServer {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let addr = listener.local_addr().expect("addr");
            let base_url = format!("http://{}", addr);

            thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    handle(stream);
                }
            });

            Self { base_url }
        }
    }

    fn handle(mut stream: TcpStream) {
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
            issue_response()
        } else if path.starts_with("/rest/api/3/field") {
            "[]".to_string()
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

    fn issue_response() -> String {
        r#"{
            "key":"K1",
            "renderedFields":{},
            "fields":{
                "summary":"Implement auth",
                "description":null,
                "issuetype":{"name":"Task"},
                "status":{"name":"Open","statusCategory":{"name":"To Do"}},
                "priority":{"name":"Medium"},
                "resolution":null,
                "project":{"name":"Project","key":"PROJ"},
                "assignee":null,
                "reporter":null,
                "creator":null,
                "labels":[],
                "components":[],
                "parent":null,
                "subtasks":[],
                "issuelinks":[],
                "worklog":{"worklogs":[]},
                "comment":{"comments":[]},
                "attachment":[]
            }
        }"#
        .to_string()
    }
}

/// Extract child issues from "is implemented by" issue links (JPD Polaris links).
///
/// JPD Ideas link to delivery items (Epics, Stories, Tasks) via these links.
/// The delivery item appears as an `inwardIssue` with `type.inward` = "is implemented by".
fn extract_delivery_children(issue_data: &Value) -> Vec<Value> {
    let links = match issue_data["fields"]["issuelinks"].as_array() {
        Some(l) => l,
        None => return Vec::new(),
    };

    links
        .iter()
        .filter(|link| {
            let link_type = link["type"]["inward"].as_str().unwrap_or("");
            link_type.to_lowercase().contains("is implemented by")
        })
        .filter_map(|link| {
            let inward = &link["inwardIssue"];
            if inward.is_null() {
                None
            } else {
                Some(inward.clone())
            }
        })
        .collect()
}
