//! Jira API client for handling all communication with Jira Cloud REST API.

use log::info;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use reqwest::{Client, Response, StatusCode};
use serde_json::Value;

use crate::error::{JarkdownError, Result};
use crate::retry::{retry_with_backoff, RetryConfig};

/// Handles all communication with the Jira Cloud REST API.
#[derive(Debug, Clone)]
pub struct JiraApiClient {
    pub domain: String,
    pub base_url: String,
    pub api_base: String,
    client: Client,
}

impl JiraApiClient {
    /// Create a new Jira API client.
    pub fn new(domain: &str, email: &str, api_token: &str) -> Result<Self> {
        let base_url = format!("https://{}", domain);
        let api_base = format!("{}/rest/api/3", base_url);

        let credentials = format!("{}:{}", email, api_token);
        let encoded = base64_encode(&credentials);
        let auth_value = format!("Basic {}", encoded);

        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&auth_value)
                .map_err(|e| JarkdownError::Unexpected(format!("Invalid auth header: {}", e)))?,
        );

        let client = Client::builder()
            .pool_max_idle_per_host(5)
            .timeout(std::time::Duration::from_secs(30))
            .default_headers(headers)
            .build()
            .map_err(|e| {
                JarkdownError::Unexpected(format!("Failed to build HTTP client: {}", e))
            })?;

        Ok(Self {
            domain: domain.to_string(),
            base_url,
            api_base,
            client,
        })
    }

    #[cfg(test)]
    pub fn new_for_test(base_url: &str) -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("test HTTP client");
        let domain = base_url
            .trim_start_matches("http://")
            .trim_start_matches("https://")
            .to_string();
        Self {
            domain,
            base_url: base_url.to_string(),
            api_base: format!("{}/rest/api/3", base_url),
            client,
        }
    }

    /// Fetch issue data from Jira API.
    pub async fn fetch_issue(&self, issue_key: &str) -> Result<Value> {
        let url = format!("{}/issue/{}", self.api_base, issue_key);
        info!("Fetching issue {}...", issue_key);

        let response = self
            .client
            .get(&url)
            .query(&[("fields", "*all"), ("expand", "renderedFields")])
            .send()
            .await?;

        Self::handle_response(response, Some(issue_key)).await
    }

    /// Fetch all field definitions from Jira.
    pub async fn fetch_fields(&self) -> Result<Vec<Value>> {
        let url = format!("{}/field", self.api_base);
        info!("Fetching field metadata...");

        let response = self.client.get(&url).send().await?;
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED {
            return Err(JarkdownError::Authentication(
                "Authentication failed while fetching field metadata.".into(),
            ));
        }
        if !status.is_success() {
            return Err(JarkdownError::JiraApi {
                message: format!("Error fetching field metadata: HTTP {}", status.as_u16()),
                status_code: Some(status.as_u16()),
            });
        }
        Ok(response.json().await?)
    }

    /// Search for issues matching a JQL query, paginating via nextPageToken.
    pub async fn search_jql(&self, jql: &str, max_results: u32) -> Result<Vec<Value>> {
        let url = format!("{}/search/jql", self.api_base);
        let mut issues: Vec<Value> = Vec::new();
        let mut next_page_token: Option<String> = None;
        let page_size = max_results.min(50);
        let config = RetryConfig::default();

        while (issues.len() as u32) < max_results {
            let remaining = max_results - issues.len() as u32;
            let fetch_count = remaining.min(page_size);

            let mut query_params: Vec<(String, String)> = vec![
                ("jql".into(), jql.to_string()),
                ("maxResults".into(), fetch_count.to_string()),
                ("fields".into(), "summary,issuetype,status,assignee".into()),
            ];
            if let Some(ref token) = next_page_token {
                query_params.push(("nextPageToken".into(), token.clone()));
            }

            let client = self.client.clone();
            let url_clone = url.clone();
            let params_clone = query_params.clone();

            let data: Value = retry_with_backoff(
                || {
                    let c = client.clone();
                    let u = url_clone.clone();
                    let p = params_clone.clone();
                    async move {
                        let resp = c.get(&u).query(&p).send().await?;
                        let resp = resp.error_for_status()?;
                        resp.json::<Value>().await
                    }
                },
                &config,
            )
            .await
            .map_err(|e| {
                if e.status() == Some(StatusCode::UNAUTHORIZED) {
                    JarkdownError::Authentication("Authentication failed during JQL search.".into())
                } else {
                    JarkdownError::JiraApi {
                        message: format!("JQL search failed: {}", e),
                        status_code: e.status().map(|s| s.as_u16()),
                    }
                }
            })?;

            let page_issues = data["issues"].as_array().cloned().unwrap_or_default();
            let page_empty = page_issues.is_empty();
            issues.extend(page_issues);
            next_page_token = data["nextPageToken"].as_str().map(|s| s.to_string());
            if next_page_token.is_none() || page_empty {
                break;
            }
        }
        issues.truncate(max_results as usize);
        Ok(issues)
    }

    /// Get the download URL for an attachment.
    pub fn get_attachment_content_url(attachment: &Value) -> String {
        attachment["content"].as_str().unwrap_or("").to_string()
    }

    /// Download an attachment and return the response bytes.
    pub async fn download_attachment(&self, content_url: &str) -> Result<bytes::Bytes> {
        let response = self.client.get(content_url).send().await?;
        let status = response.status();
        if !status.is_success() {
            return Err(JarkdownError::JiraApi {
                message: format!("Error downloading attachment: HTTP {}", status.as_u16()),
                status_code: Some(status.as_u16()),
            });
        }
        Ok(response.bytes().await?)
    }

    async fn handle_response(response: Response, issue_key: Option<&str>) -> Result<Value> {
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED {
            return Err(JarkdownError::Authentication(
                "Authentication failed. Please check your API token and email.".into(),
            ));
        }
        if status == StatusCode::NOT_FOUND {
            return Err(JarkdownError::IssueNotFound(format!(
                "Issue {} not found or not accessible.",
                issue_key.unwrap_or("Unknown")
            )));
        }
        if !status.is_success() {
            return Err(JarkdownError::JiraApi {
                message: format!("HTTP error occurred: {}", status.as_u16()),
                status_code: Some(status.as_u16()),
            });
        }
        Ok(response.json().await?)
    }
}

impl JiraApiClient {
    /// Fetch the full changelog (audit trail of field changes) for an issue.
    ///
    /// Paginates `/rest/api/3/issue/{key}/changelog` via `startAt`/`maxResults`
    /// until `isLast` is true (or the accumulated count reaches `total`),
    /// returning every history entry concatenated in the order Jira returned them.
    pub async fn fetch_changelog(&self, issue_key: &str) -> Result<Vec<Value>> {
        let url = format!("{}/issue/{}/changelog", self.api_base, issue_key);
        let page_size: u32 = 100;
        let mut start_at: u32 = 0;
        let mut out: Vec<Value> = Vec::new();

        loop {
            let response = self
                .client
                .get(&url)
                .query(&[
                    ("startAt", start_at.to_string()),
                    ("maxResults", page_size.to_string()),
                ])
                .send()
                .await?;
            let data = Self::handle_response(response, Some(issue_key)).await?;

            if let Some(values) = data["values"].as_array() {
                out.extend(values.iter().cloned());
            }

            let is_last = data["isLast"].as_bool().unwrap_or(true);
            let total = data["total"].as_u64().unwrap_or(out.len() as u64);
            if is_last || (out.len() as u64) >= total {
                break;
            }
            start_at = out.len() as u32;
        }
        Ok(out)
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;
    use std::io::{Read as _, Write as _};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};
    use std::thread;

    #[tokio::test]
    async fn fetch_changelog_concatenates_paginated_pages_in_order() {
        let server = PaginatedChangelogServer::start();
        let client = JiraApiClient::new_for_test(&server.base_url);

        let entries = client
            .fetch_changelog("PROJ-1")
            .await
            .expect("fetch_changelog");

        let ids: Vec<&str> = entries
            .iter()
            .map(|e| e["id"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(
            ids,
            vec!["1", "2", "3", "4", "5"],
            "expected concatenated ids across both pages"
        );
        assert!(server.saw_startat(0), "first page request missing");
        assert!(
            server.saw_startat(3),
            "second page request (startAt=3) missing; observed paths: {:?}",
            server.observed_paths()
        );
    }

    struct PaginatedChangelogServer {
        base_url: String,
        requests: Arc<Mutex<Vec<String>>>,
    }

    impl PaginatedChangelogServer {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
            let addr = listener.local_addr().expect("addr");
            let base_url = format!("http://{}", addr);
            let requests = Arc::new(Mutex::new(Vec::new()));
            let t_requests = requests.clone();

            thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    handle(stream, &t_requests);
                }
            });

            Self { base_url, requests }
        }

        fn saw_startat(&self, n: u32) -> bool {
            let needle = format!("startAt={}", n);
            self.requests
                .lock()
                .unwrap()
                .iter()
                .any(|p| p.contains(&needle))
        }

        fn observed_paths(&self) -> Vec<String> {
            self.requests.lock().unwrap().clone()
        }
    }

    fn handle(mut stream: TcpStream, requests: &Arc<Mutex<Vec<String>>>) {
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

        let body = if path.contains("/changelog") {
            if path.contains("startAt=0") {
                r#"{"startAt":0,"maxResults":3,"total":5,"isLast":false,"values":[
                    {"id":"1"},{"id":"2"},{"id":"3"}
                ]}"#
                .to_string()
            } else if path.contains("startAt=3") {
                r#"{"startAt":3,"maxResults":3,"total":5,"isLast":true,"values":[
                    {"id":"4"},{"id":"5"}
                ]}"#
                .to_string()
            } else {
                r#"{"startAt":0,"maxResults":0,"total":0,"isLast":true,"values":[]}"#.to_string()
            }
        } else {
            "{}".to_string()
        };
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(resp.as_bytes()).expect("write");
    }
}

fn base64_encode(input: &str) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = input.as_bytes();
    let mut result = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}
