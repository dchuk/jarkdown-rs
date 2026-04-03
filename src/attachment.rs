//! Handler for downloading and managing Jira attachments.

use std::path::{Path, PathBuf};

use log::{error, info};
use serde_json::Value;

use crate::error::{JarkdownError, Result};
use crate::jira_client::JiraApiClient;

/// Information about a downloaded attachment.
#[derive(Debug, Clone)]
pub struct DownloadedAttachment {
    pub attachment_id: Option<String>,
    pub filename: String,
    pub original_filename: String,
    pub mime_type: String,
    pub path: PathBuf,
}

/// Manages downloading and saving of issue attachments.
pub struct AttachmentHandler<'a> {
    api_client: &'a JiraApiClient,
}

impl<'a> AttachmentHandler<'a> {
    pub fn new(api_client: &'a JiraApiClient) -> Self {
        Self { api_client }
    }

    /// Download a single attachment.
    pub async fn download_attachment(
        &self,
        attachment: &Value,
        output_dir: &Path,
    ) -> Result<DownloadedAttachment> {
        let filename = attachment["filename"].as_str().unwrap_or("unknown").to_string();
        let content_url = attachment["content"].as_str().unwrap_or("").to_string();
        let mime_type = attachment["mimeType"].as_str().unwrap_or("").to_string();
        let size = attachment["size"].as_u64().unwrap_or(0);

        let mut file_path = output_dir.join(&filename);

        // Handle filename conflicts
        let mut counter = 1u32;
        let original_path = file_path.clone();
        while file_path.exists() {
            let stem = original_path.file_stem().unwrap_or_default().to_string_lossy();
            let ext = original_path
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy()))
                .unwrap_or_default();
            file_path = original_path.with_file_name(format!("{}_{}{}", stem, counter, ext));
            counter += 1;
        }

        info!("  Downloading {} ({})...", filename, format_size(size));

        let data = self
            .api_client
            .download_attachment(&content_url)
            .await
            .map_err(|e| JarkdownError::AttachmentDownload {
                message: format!("Error downloading {}: {}", filename, e),
                filename: Some(filename.clone()),
            })?;

        tokio::fs::write(&file_path, &data).await?;

        Ok(DownloadedAttachment {
            attachment_id: attachment["id"].as_str().map(|s| s.to_string()),
            filename: file_path.file_name().unwrap_or_default().to_string_lossy().to_string(),
            original_filename: filename,
            mime_type,
            path: file_path,
        })
    }

    /// Download all attachments for an issue (sequential).
    pub async fn download_all_attachments(
        &self,
        attachments: &[Value],
        output_dir: &Path,
    ) -> Vec<DownloadedAttachment> {
        if attachments.is_empty() {
            return Vec::new();
        }

        tokio::fs::create_dir_all(output_dir).await.ok();
        info!("Downloading {} attachment(s)...", attachments.len());

        let mut downloaded = Vec::new();
        for attachment in attachments {
            match self.download_attachment(attachment, output_dir).await {
                Ok(result) => downloaded.push(result),
                Err(e) => error!("{}", e),
            }
        }
        downloaded
    }
}

fn format_size(size: u64) -> String {
    let mut s = size as f64;
    for unit in &["B", "KB", "MB", "GB"] {
        if s < 1024.0 {
            return format!("{:.1} {}", s, unit);
        }
        s /= 1024.0;
    }
    format!("{:.1} TB", s)
}
