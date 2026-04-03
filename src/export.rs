//! Shared async export workflow used by both the CLI and BulkExporter.

use std::path::{Path, PathBuf};

use log::{info, warn};
use crate::attachment::AttachmentHandler;
use crate::config::ConfigManager;
use crate::error::Result;
use crate::field_cache::FieldMetadataCache;
use crate::jira_client::JiraApiClient;
use crate::markdown::MarkdownConverter;

/// Run the full export workflow for a single Jira issue.
///
/// Fetches issue data, downloads attachments, builds field metadata and
/// config, converts to Markdown, and writes output files.
pub async fn perform_export(
    api_client: &JiraApiClient,
    issue_key: &str,
    output_path: &Path,
    refresh_fields: bool,
    include_fields: Option<&str>,
    exclude_fields: Option<&str>,
    include_json: bool,
) -> Result<PathBuf> {
    // Ensure output directory exists
    tokio::fs::create_dir_all(output_path).await?;

    // Fetch issue data
    let issue_data = api_client.fetch_issue(issue_key).await?;

    // Download attachments
    let handler = AttachmentHandler::new(api_client);
    let attachments = issue_data["fields"]["attachment"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let downloaded = handler.download_all_attachments(&attachments, output_path).await;

    // Build field metadata cache
    let mut field_cache = FieldMetadataCache::new(&api_client.domain);
    if refresh_fields || field_cache.is_stale() {
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
    let field_filter = config_manager.get_field_filter(include_fields, exclude_fields);

    // Convert to Markdown
    let mut converter = MarkdownConverter::new(&api_client.base_url, &api_client.domain);
    let mut cache_opt = Some(field_cache);
    let filter_opt = Some(field_filter);
    let markdown_content = converter.compose_markdown(
        &issue_data,
        &downloaded,
        &mut cache_opt,
        &filter_opt,
    );

    // Write raw JSON (opt-in)
    if include_json {
        let json_file = output_path.join(format!("{}.json", issue_key));
        let json_str = serde_json::to_string_pretty(&issue_data)?;
        tokio::fs::write(&json_file, json_str).await?;
    }

    // Write Markdown
    let md_file = output_path.join(format!("{}.md", issue_key));
    tokio::fs::write(&md_file, markdown_content).await?;

    Ok(output_path.to_path_buf())
}
