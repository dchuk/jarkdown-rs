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
            .map_err(|e| JarkdownError::Unexpected(format!("Failed to build HTTP client: {}", e)))?;

        Ok(Self { domain: domain.to_string(), base_url, api_base, client })
    }

    /// Fetch issue data from Jira API.
    pub async fn fetch_issue(&self, issue_key: &str) -> Result<Value> {
        let url = format!("{}/issue/{}", self.api_base, issue_key);
        info!("Fetching issue {}...", issue_key);

        let response = self.client.get(&url)
            .query(&[("fields", "*all"), ("expand", "renderedFields")])
            .send().await?;

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

fn base64_encode(input: &str) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = input.as_bytes();
    let mut result = String::with_capacity((bytes.len() + 2) / 3 * 4);
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
