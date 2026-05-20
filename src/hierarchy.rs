//! Hierarchical export of epics, JPD ideas, and their child issues.
//!
//! Fetches a root issue, discovers children via JQL, issue links, and
//! JPD delivery links ("is implemented by"), and exports everything
//! into a tree-structured directory.

use std::collections::HashSet;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use log::{info, warn};

use crate::error::Result;
use crate::export::{perform_export_with_options, ExportWorkflowOptions};
use crate::field_cache::FieldMetadataCache;
use crate::jira_client::JiraApiClient;

/// A node in the issue hierarchy tree.
#[derive(Debug, Clone)]
pub struct IssueNode {
    pub key: String,
    pub summary: String,
    pub issue_type: String,
    pub children: Vec<IssueNode>,
}

/// Configuration for hierarchical export.
#[derive(Debug, Clone)]
pub struct HierarchyOptions {
    pub max_depth: u32,
    pub max_issues: u32,
    pub refresh_fields: bool,
    pub include_fields: Option<String>,
    pub exclude_fields: Option<String>,
    pub include_json: bool,
    pub attachment_concurrency: usize,
    pub no_attachments: bool,
    pub include_changelog: bool,
}

/// Orchestrates discovery and export of an issue hierarchy.
pub struct HierarchyExporter<'a> {
    api_client: &'a JiraApiClient,
    options: HierarchyOptions,
    visited: HashSet<String>,
    issue_count: u32,
}

impl<'a> HierarchyExporter<'a> {
    pub fn new(api_client: &'a JiraApiClient, options: HierarchyOptions) -> Self {
        Self {
            api_client,
            options,
            visited: HashSet::new(),
            issue_count: 0,
        }
    }

    /// Export an issue and its entire hierarchy to the given output directory.
    pub async fn export_hierarchy(
        &mut self,
        root_key: &str,
        output_dir: &Path,
    ) -> Result<IssueNode> {
        tokio::fs::create_dir_all(output_dir).await?;

        // Look up "Epic Link" field ID for this Jira instance
        let epic_link_field = self.resolve_epic_link_field().await;

        let tree = self
            .build_tree(root_key, output_dir, 0, epic_link_field.as_deref())
            .await?;

        // Write index.md with tree visualization
        let index_content = self.render_index(&tree, root_key);
        let index_path = output_dir.join("index.md");
        tokio::fs::write(&index_path, index_content).await?;
        info!("Wrote hierarchy index to {:?}", index_path);

        Ok(tree)
    }

    /// Recursively build the issue tree.
    fn build_tree<'b>(
        &'b mut self,
        issue_key: &'b str,
        output_dir: &'b Path,
        depth: u32,
        epic_link_field: Option<&'b str>,
    ) -> Pin<Box<dyn Future<Output = Result<IssueNode>> + 'b>> {
        Box::pin(self.build_tree_inner(issue_key, output_dir, depth, epic_link_field))
    }

    async fn build_tree_inner(
        &mut self,
        issue_key: &str,
        output_dir: &Path,
        depth: u32,
        epic_link_field: Option<&str>,
    ) -> Result<IssueNode> {
        // Cycle detection
        if self.visited.contains(issue_key) {
            return Ok(IssueNode {
                key: issue_key.to_string(),
                summary: "(already visited)".to_string(),
                issue_type: String::new(),
                children: Vec::new(),
            });
        }
        self.visited.insert(issue_key.to_string());
        self.issue_count += 1;

        // Export this issue
        let issue_dir = output_dir.join(issue_key);
        perform_export_with_options(
            self.api_client,
            issue_key,
            &issue_dir,
            ExportWorkflowOptions {
                refresh_fields: self.options.refresh_fields,
                include_fields: self.options.include_fields.as_deref(),
                exclude_fields: self.options.exclude_fields.as_deref(),
                include_json: self.options.include_json,
                attachment_concurrency: self.options.attachment_concurrency,
                no_attachments: self.options.no_attachments,
                include_changelog: self.options.include_changelog,
            },
        )
        .await?;

        // Fetch issue data for metadata + child discovery
        let issue_data = self.api_client.fetch_issue(issue_key).await?;
        let fields = &issue_data["fields"];
        let summary = fields["summary"].as_str().unwrap_or("").to_string();
        let issue_type = fields["issuetype"]["name"]
            .as_str()
            .unwrap_or("")
            .to_string();

        let mut children = Vec::new();

        // Stop recursing if we've hit max depth or max issues
        if depth >= self.options.max_depth || self.issue_count >= self.options.max_issues {
            return Ok(IssueNode {
                key: issue_key.to_string(),
                summary,
                issue_type,
                children,
            });
        }

        // Discover children from multiple sources
        let mut child_keys: Vec<String> = Vec::new();

        // 1. Subtasks from issue data
        if let Some(subtasks) = fields["subtasks"].as_array() {
            for st in subtasks {
                if let Some(k) = st["key"].as_str() {
                    child_keys.push(k.to_string());
                }
            }
        }

        // 2. Issue links (e.g. "is parent of", "contains", "is implemented by")
        if let Some(links) = fields["issuelinks"].as_array() {
            for link in links {
                // Outward links where this issue is the parent
                if let Some(outward) = link["outwardIssue"]["key"].as_str() {
                    let link_type = link["type"]["outward"].as_str().unwrap_or("");
                    if is_parent_link_type(link_type) {
                        child_keys.push(outward.to_string());
                    }
                }
                // Inward links where this issue is the parent
                if let Some(inward) = link["inwardIssue"]["key"].as_str() {
                    let link_type = link["type"]["inward"].as_str().unwrap_or("");
                    if is_parent_link_type(link_type) {
                        child_keys.push(inward.to_string());
                    }
                }
            }
        }

        // 3. JQL search for children (parent = KEY or "Epic Link" = KEY)
        let jql_children = self
            .search_children(issue_key, epic_link_field)
            .await
            .unwrap_or_default();
        child_keys.extend(jql_children);

        // Deduplicate while preserving order
        let mut seen = HashSet::new();
        child_keys.retain(|k| seen.insert(k.clone()));

        // Recursively process children
        for child_key in &child_keys {
            if self.issue_count >= self.options.max_issues {
                warn!(
                    "Reached max issue limit ({}). Stopping hierarchy traversal.",
                    self.options.max_issues
                );
                break;
            }
            match self
                .build_tree(
                    child_key,
                    &output_dir.join(issue_key),
                    depth + 1,
                    epic_link_field,
                )
                .await
            {
                Ok(child_node) => children.push(child_node),
                Err(e) => warn!("Failed to export {}: {}", child_key, e),
            }
        }

        Ok(IssueNode {
            key: issue_key.to_string(),
            summary,
            issue_type,
            children,
        })
    }

    /// Resolve the custom field ID for "Epic Link" by reverse-looking up the field name.
    async fn resolve_epic_link_field(&self) -> Option<String> {
        let mut cache = FieldMetadataCache::new(&self.api_client.domain);
        if cache.is_stale() {
            if let Ok(fields) = self.api_client.fetch_fields().await {
                cache.save(&fields);
            }
        }
        cache.get_field_id_by_name("Epic Link")
    }

    /// Search for child issues via JQL.
    async fn search_children(
        &self,
        parent_key: &str,
        epic_link_field: Option<&str>,
    ) -> Result<Vec<String>> {
        let mut clauses = vec![format!("parent = {}", parent_key)];
        if let Some(field_id) = epic_link_field {
            clauses.push(format!("\"{}\" = {}", field_id, parent_key));
        }
        let jql = clauses.join(" OR ");

        let issues = self
            .api_client
            .search_jql(&jql, self.options.max_issues)
            .await?;

        Ok(issues
            .iter()
            .filter_map(|i| i["key"].as_str().map(|s| s.to_string()))
            .collect())
    }

    /// Render the tree as a Markdown index file.
    fn render_index(&self, root: &IssueNode, root_key: &str) -> String {
        let mut lines = vec![
            format!("# Hierarchy: {}", root_key),
            String::new(),
            format!(
                "Exported {} issues with max depth {}.",
                self.visited.len(),
                self.options.max_depth
            ),
            String::new(),
            "## Issue Tree".to_string(),
            String::new(),
            "```".to_string(),
        ];

        render_tree_node(root, "", true, &mut lines);

        lines.push("```".to_string());
        lines.push(String::new());

        // Add a linked list for easy navigation
        lines.push("## Issues".to_string());
        lines.push(String::new());
        render_issue_list(root, &mut lines);
        lines.push(String::new());

        lines.join("\n")
    }
}

fn render_tree_node(node: &IssueNode, prefix: &str, is_last: bool, lines: &mut Vec<String>) {
    let connector = if prefix.is_empty() {
        ""
    } else if is_last {
        "└── "
    } else {
        "├── "
    };

    let type_label = if node.issue_type.is_empty() {
        String::new()
    } else {
        format!(" [{}]", node.issue_type)
    };

    lines.push(format!(
        "{}{}{}{} — {}",
        prefix, connector, node.key, type_label, node.summary
    ));

    let child_prefix = if prefix.is_empty() {
        String::new()
    } else if is_last {
        format!("{}    ", prefix)
    } else {
        format!("{}│   ", prefix)
    };

    for (i, child) in node.children.iter().enumerate() {
        let last = i == node.children.len() - 1;
        render_tree_node(child, &child_prefix, last, lines);
    }
}

fn render_issue_list(node: &IssueNode, lines: &mut Vec<String>) {
    lines.push(format!(
        "- [{}]({}/{}.md) — {}",
        node.key, node.key, node.key, node.summary
    ));
    for child in &node.children {
        render_issue_list(child, lines);
    }
}

/// Check if a link type name indicates a parent-child relationship.
/// Includes JPD Polaris links ("is implemented by") for Idea → delivery item traversal.
fn is_parent_link_type(link_type: &str) -> bool {
    let lower = link_type.to_lowercase();
    lower.contains("parent of")
        || lower.contains("contains")
        || lower.contains("is epic of")
        || lower.contains("is parent of")
        || lower.contains("is implemented by")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jira_client::JiraApiClient;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[tokio::test]
    async fn hierarchy_export_with_no_attachments_writes_markdown_and_json_without_downloading_binaries(
    ) {
        let server = TestJiraServer::start();
        let client = JiraApiClient::new_for_test(&server.base_url);
        let output_dir = unique_temp_dir("jarkdown-no-attachments");
        let options = HierarchyOptions {
            max_depth: 3,
            max_issues: 10,
            refresh_fields: false,
            include_fields: None,
            exclude_fields: None,
            include_json: true,
            attachment_concurrency: 4,
            no_attachments: true,
            include_changelog: false,
        };

        let mut exporter = HierarchyExporter::new(&client, options);
        let tree = exporter
            .export_hierarchy("K1", &output_dir)
            .await
            .expect("hierarchy export");

        assert_eq!(tree.key, "K1");
        let issue_dir = output_dir.join("K1");
        let markdown = tokio::fs::read_to_string(issue_dir.join("K1.md"))
            .await
            .expect("markdown file");
        let json: serde_json::Value = serde_json::from_str(
            &tokio::fs::read_to_string(issue_dir.join("K1.json"))
                .await
                .expect("json file"),
        )
        .expect("issue json");

        assert!(markdown.contains("## Attachments"));
        assert!(markdown.contains("[diagram.png]"));
        assert_eq!(
            json["fields"]["attachment"][0]["filename"].as_str(),
            Some("diagram.png")
        );
        assert!(!issue_dir.join("diagram.png").exists());
        assert!(
            !server.requested_path_containing("/attachment/content/10001"),
            "attachment content endpoint must not be requested"
        );
        std::fs::remove_dir_all(output_dir).ok();
    }

    fn unique_temp_dir(prefix: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        std::env::temp_dir().join(format!("{}-{}", prefix, nanos))
    }

    struct TestJiraServer {
        base_url: String,
        requests: Arc<Mutex<Vec<String>>>,
    }

    impl TestJiraServer {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("test server bind");
            let addr = listener.local_addr().expect("test server addr");
            let base_url = format!("http://{}", addr);
            let requests = Arc::new(Mutex::new(Vec::new()));
            let thread_requests = requests.clone();
            let thread_base_url = base_url.clone();

            thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    handle_request(stream, &thread_requests, &thread_base_url);
                }
            });

            Self { base_url, requests }
        }

        fn requested_path_containing(&self, needle: &str) -> bool {
            self.requests
                .lock()
                .expect("request log")
                .iter()
                .any(|path| path.contains(needle))
        }
    }

    fn handle_request(mut stream: TcpStream, requests: &Arc<Mutex<Vec<String>>>, base_url: &str) {
        let mut buffer = [0; 8192];
        let bytes_read = stream.read(&mut buffer).expect("read request");
        let request = String::from_utf8_lossy(&buffer[..bytes_read]);
        let path = request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or("/")
            .to_string();
        requests.lock().expect("request log").push(path.clone());

        let body = if path.starts_with("/rest/api/3/issue/K1") {
            issue_response(base_url)
        } else if path.starts_with("/rest/api/3/field") {
            "[]".to_string()
        } else if path.starts_with("/rest/api/3/search/jql") {
            r#"{"issues":[]}"#.to_string()
        } else if path.starts_with("/rest/api/3/attachment/content/10001") {
            "unexpected binary download".to_string()
        } else {
            "{}".to_string()
        };
        let status = if path.starts_with("/rest/api/3/attachment/content/10001") {
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
        stream
            .write_all(response.as_bytes())
            .expect("write response");
    }

    fn issue_response(base_url: &str) -> String {
        format!(
            r#"{{
            "key": "K1",
            "renderedFields": {{}},
            "fields": {{
                "summary": "Attachment issue",
                "description": {{
                    "type": "doc",
                    "content": [
                        {{
                            "type": "mediaSingle",
                            "content": [
                                {{
                                    "type": "media",
                                    "attrs": {{
                                        "id": "10001",
                                        "type": "file",
                                        "alt": "diagram.png"
                                    }}
                                }}
                            ]
                        }}
                    ]
                }},
                "issuetype": {{ "name": "Task" }},
                "status": {{ "name": "Open", "statusCategory": {{ "name": "To Do" }} }},
                "priority": {{ "name": "Medium" }},
                "resolution": null,
                "project": {{ "name": "Project", "key": "PROJ" }},
                "assignee": null,
                "reporter": null,
                "creator": null,
                "labels": [],
                "components": [],
                "parent": null,
                "subtasks": [],
                "issuelinks": [],
                "worklog": {{ "worklogs": [] }},
                "comment": {{ "comments": [] }},
                "attachment": [
                    {{
                        "id": "10001",
                        "filename": "diagram.png",
                        "content": "{}/rest/api/3/attachment/content/10001",
                        "mimeType": "image/png",
                        "size": 1234
                    }}
                ]
            }}
        }}"#,
            base_url
        )
    }
}
