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
use crate::exporter::IssueExporter;
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
    exporter: &'a dyn IssueExporter,
    options: HierarchyOptions,
    visited: HashSet<String>,
    issue_count: u32,
}

impl<'a> HierarchyExporter<'a> {
    pub fn new(
        api_client: &'a JiraApiClient,
        exporter: &'a dyn IssueExporter,
        options: HierarchyOptions,
    ) -> Self {
        Self {
            api_client,
            exporter,
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
        self.exporter.export(issue_key, &issue_dir).await?;

        // Fetch the issue for metadata + child discovery
        let issue = self.api_client.fetch_issue(issue_key).await?;
        let summary = issue.summary.clone();
        let issue_type = issue.issuetype.name.clone();

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
        for st in &issue.subtasks {
            if let Some(k) = st["key"].as_str() {
                child_keys.push(k.to_string());
            }
        }

        // 2. Issue links (e.g. "is parent of", "contains", "is implemented by")
        for link in &issue.issuelinks {
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

        let results = self
            .api_client
            .search_jql(&jql, self.options.max_issues)
            .await?;

        Ok(results.into_iter().map(|r| r.key).collect())
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
    use crate::export::ExportWorkflowOptions;
    use crate::exporter::WorkflowIssueExporter;
    use crate::jira_client::JiraApiClient;
    use std::collections::HashMap;
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

        let workflow_options = ExportWorkflowOptions {
            refresh_fields: options.refresh_fields,
            include_fields: options.include_fields.as_deref(),
            exclude_fields: options.exclude_fields.as_deref(),
            include_json: options.include_json,
            attachment_concurrency: options.attachment_concurrency,
            no_attachments: options.no_attachments,
            include_changelog: options.include_changelog,
        };
        let workflow_exporter = WorkflowIssueExporter {
            api_client: &client,
            options: workflow_options,
        };
        let mut exporter = HierarchyExporter::new(&client, &workflow_exporter, options.clone());
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

    // ---------------------------------------------------------------------
    // Fake IssueExporter + scriptable Jira server for hierarchy seam tests.
    // ---------------------------------------------------------------------

    /// Records each issue key the hierarchy traverses, without touching disk
    /// or network for the export step itself.
    #[derive(Default)]
    struct RecordingExporter {
        calls: std::sync::Mutex<Vec<String>>,
    }

    impl IssueExporter for RecordingExporter {
        fn export<'a>(
            &'a self,
            issue_key: &'a str,
            _output_dir: &'a std::path::Path,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = crate::error::Result<()>> + 'a>>
        {
            Box::pin(async move {
                self.calls.lock().unwrap().push(issue_key.to_string());
                Ok(())
            })
        }
    }

    impl RecordingExporter {
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    /// Scriptable test server: maps issue keys to a list of "issue link
    /// outward" child keys, served via the minimal Jira issue shape that
    /// `Issue::from_value` accepts. JQL search always returns no children.
    struct ScriptedJiraServer {
        base_url: String,
    }

    impl ScriptedJiraServer {
        fn start(graph: HashMap<String, Vec<String>>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("test server bind");
            let addr = listener.local_addr().expect("test server addr");
            let base_url = format!("http://{}", addr);
            let graph = Arc::new(graph);

            thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let g = graph.clone();
                    handle_scripted_request(stream, g);
                }
            });

            Self { base_url }
        }
    }

    fn handle_scripted_request(mut stream: TcpStream, graph: Arc<HashMap<String, Vec<String>>>) {
        let mut buffer = [0; 8192];
        let bytes_read = stream.read(&mut buffer).expect("read request");
        let request = String::from_utf8_lossy(&buffer[..bytes_read]);
        let path = request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or("/")
            .to_string();

        let body = if let Some(key) = parse_issue_key(&path) {
            let children = graph.get(&key).cloned().unwrap_or_default();
            scripted_issue_response(&key, &children)
        } else if path.starts_with("/rest/api/3/field") {
            "[]".to_string()
        } else if path.starts_with("/rest/api/3/search/jql") {
            r#"{"issues":[]}"#.to_string()
        } else {
            "{}".to_string()
        };

        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(response.as_bytes())
            .expect("write response");
    }

    /// Extract the issue key from `/rest/api/3/issue/{KEY}?...`. Returns
    /// None for non-issue endpoints (or the changelog sub-resource, which
    /// these tests don't exercise).
    fn parse_issue_key(path: &str) -> Option<String> {
        let rest = path.strip_prefix("/rest/api/3/issue/")?;
        // Trim query string
        let without_query = rest.split('?').next().unwrap_or("");
        // Reject sub-paths like "K1/changelog"
        if without_query.contains('/') {
            return None;
        }
        if without_query.is_empty() {
            return None;
        }
        Some(without_query.to_string())
    }

    /// Minimal issue JSON: the given key plus outward links to each child key
    /// using the JPD "is implemented by" relationship, which
    /// `is_parent_link_type` recognizes as a parent-child edge.
    fn scripted_issue_response(key: &str, children: &[String]) -> String {
        let links: Vec<String> = children
            .iter()
            .map(|child| {
                format!(
                    r#"{{
                    "type": {{ "outward": "is implemented by", "inward": "implements" }},
                    "outwardIssue": {{ "key": "{}" }}
                }}"#,
                    child
                )
            })
            .collect();
        let links_json = links.join(",");

        format!(
            r#"{{
            "key": "{}",
            "renderedFields": {{}},
            "fields": {{
                "summary": "Issue {}",
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
                "issuelinks": [{}],
                "worklog": {{ "worklogs": [] }},
                "comment": {{ "comments": [] }},
                "attachment": []
            }}
        }}"#,
            key, key, links_json
        )
    }

    fn default_hierarchy_options(max_depth: u32, max_issues: u32) -> HierarchyOptions {
        HierarchyOptions {
            max_depth,
            max_issues,
            refresh_fields: false,
            include_fields: None,
            exclude_fields: None,
            include_json: false,
            attachment_concurrency: 1,
            no_attachments: true,
            include_changelog: false,
        }
    }

    #[tokio::test]
    async fn hierarchy_skips_already_visited_issue_via_fake_exporter() {
        // A -> B and B -> A; the cycle should be detected on the second visit.
        let mut graph = HashMap::new();
        graph.insert("A".to_string(), vec!["B".to_string()]);
        graph.insert("B".to_string(), vec!["A".to_string()]);

        let server = ScriptedJiraServer::start(graph);
        let client = JiraApiClient::new_for_test(&server.base_url);
        let output_dir = unique_temp_dir("jarkdown-cycle");
        let options = default_hierarchy_options(5, 10);
        let fake = RecordingExporter::default();

        let mut exporter = HierarchyExporter::new(&client, &fake, options);
        let tree = exporter
            .export_hierarchy("A", &output_dir)
            .await
            .expect("hierarchy export");

        // Each unique key is exported exactly once.
        let mut calls = fake.calls();
        calls.sort();
        assert_eq!(calls, vec!["A".to_string(), "B".to_string()]);

        // Root is A with B as a child; B's child A is the "already visited"
        // sentinel node.
        assert_eq!(tree.key, "A");
        assert_eq!(tree.children.len(), 1);
        let b = &tree.children[0];
        assert_eq!(b.key, "B");
        assert_eq!(b.children.len(), 1);
        let cycle_back = &b.children[0];
        assert_eq!(cycle_back.key, "A");
        assert_eq!(cycle_back.summary, "(already visited)");
        assert_eq!(cycle_back.issue_type, "");

        std::fs::remove_dir_all(output_dir).ok();
    }

    #[tokio::test]
    async fn hierarchy_respects_max_depth_cap_via_fake_exporter() {
        // Chain A -> B -> C -> D with max_depth = 2. The exporter is called
        // for A (depth 0), B (depth 1), and C (depth 2); D is not exported
        // because traversal stops once depth >= max_depth.
        let mut graph = HashMap::new();
        graph.insert("A".to_string(), vec!["B".to_string()]);
        graph.insert("B".to_string(), vec!["C".to_string()]);
        graph.insert("C".to_string(), vec!["D".to_string()]);
        graph.insert("D".to_string(), vec![]);

        let server = ScriptedJiraServer::start(graph);
        let client = JiraApiClient::new_for_test(&server.base_url);
        let output_dir = unique_temp_dir("jarkdown-maxdepth");
        let options = default_hierarchy_options(2, 100);
        let fake = RecordingExporter::default();

        let mut exporter = HierarchyExporter::new(&client, &fake, options);
        let _tree = exporter
            .export_hierarchy("A", &output_dir)
            .await
            .expect("hierarchy export");

        assert_eq!(
            fake.calls(),
            vec!["A".to_string(), "B".to_string(), "C".to_string()],
            "D must not be exported once max_depth is reached"
        );

        std::fs::remove_dir_all(output_dir).ok();
    }

    #[tokio::test]
    async fn hierarchy_respects_max_issues_cap_via_fake_exporter() {
        // A fans out to B, C, D, E. With max_issues = 3 only A and the first
        // two siblings (B, C) should be exported; the "Reached max issue
        // limit" guard at line 190 short-circuits the remaining iterations.
        let mut graph = HashMap::new();
        graph.insert(
            "A".to_string(),
            vec![
                "B".to_string(),
                "C".to_string(),
                "D".to_string(),
                "E".to_string(),
            ],
        );
        graph.insert("B".to_string(), vec![]);
        graph.insert("C".to_string(), vec![]);
        graph.insert("D".to_string(), vec![]);
        graph.insert("E".to_string(), vec![]);

        let server = ScriptedJiraServer::start(graph);
        let client = JiraApiClient::new_for_test(&server.base_url);
        let output_dir = unique_temp_dir("jarkdown-maxissues");
        let options = default_hierarchy_options(5, 3);
        let fake = RecordingExporter::default();

        let mut exporter = HierarchyExporter::new(&client, &fake, options);
        let _tree = exporter
            .export_hierarchy("A", &output_dir)
            .await
            .expect("hierarchy export");

        let calls = fake.calls();
        assert_eq!(calls.len(), 3, "exporter should run exactly max_issues times");
        assert_eq!(calls[0], "A");
        // Children are appended in discovery order; the first two siblings win.
        let exported_children: std::collections::HashSet<_> = calls[1..].iter().cloned().collect();
        assert!(exported_children.contains("B"));
        assert!(exported_children.contains("C"));
        assert!(!exported_children.contains("D"));
        assert!(!exported_children.contains("E"));

        std::fs::remove_dir_all(output_dir).ok();
    }
}
