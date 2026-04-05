//! Handler for downloading and managing Jira attachments.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use futures::stream::StreamExt;
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

/// Resolve a unique filename for an attachment, tracking used names to avoid conflicts.
fn resolve_filename(
    attachment: &Value,
    output_dir: &Path,
    used_names: &mut HashSet<String>,
) -> (String, PathBuf) {
    let filename = attachment["filename"]
        .as_str()
        .unwrap_or("unknown")
        .to_string();
    let mut candidate = filename.clone();
    let mut counter = 1u32;
    while used_names.contains(&candidate) {
        let path = std::path::Path::new(&filename);
        let stem = path.file_stem().unwrap_or_default().to_string_lossy();
        let ext = path
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        candidate = format!("{}_{}{}", stem, counter, ext);
        counter += 1;
    }
    used_names.insert(candidate.clone());
    (filename, output_dir.join(&candidate))
}

impl<'a> AttachmentHandler<'a> {
    pub fn new(api_client: &'a JiraApiClient) -> Self {
        Self { api_client }
    }

    /// Download a single attachment (legacy method with filesystem-based conflict resolution).
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

    /// Download an attachment to a pre-resolved path (no conflict logic).
    async fn download_attachment_to(
        &self,
        attachment: &Value,
        file_path: &Path,
        original_filename: &str,
    ) -> Result<DownloadedAttachment> {
        let content_url = attachment["content"].as_str().unwrap_or("").to_string();
        let mime_type = attachment["mimeType"].as_str().unwrap_or("").to_string();
        let size = attachment["size"].as_u64().unwrap_or(0);

        info!(
            "  Downloading {} ({})...",
            original_filename,
            format_size(size)
        );

        let data = self
            .api_client
            .download_attachment(&content_url)
            .await
            .map_err(|e| JarkdownError::AttachmentDownload {
                message: format!("Error downloading {}: {}", original_filename, e),
                filename: Some(original_filename.to_string()),
            })?;

        tokio::fs::write(file_path, &data).await?;

        Ok(DownloadedAttachment {
            attachment_id: attachment["id"].as_str().map(|s| s.to_string()),
            filename: file_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
            original_filename: original_filename.to_string(),
            mime_type,
            path: file_path.to_path_buf(),
        })
    }

    /// Download all attachments for an issue with bounded concurrency.
    ///
    /// Uses a two-phase approach: filenames are resolved synchronously first
    /// to avoid races, then downloads proceed in parallel.
    pub async fn download_all_attachments(
        &self,
        attachments: &[Value],
        output_dir: &Path,
        concurrency: usize,
    ) -> Vec<DownloadedAttachment> {
        if attachments.is_empty() {
            return Vec::new();
        }

        tokio::fs::create_dir_all(output_dir).await.ok();
        info!("Downloading {} attachment(s)...", attachments.len());

        // Phase 1: Resolve all filenames synchronously (no races)
        let mut used_names = HashSet::new();
        let resolved: Vec<(String, PathBuf, &Value)> = attachments
            .iter()
            .map(|att| {
                let (original, path) = resolve_filename(att, output_dir, &mut used_names);
                (original, path, att)
            })
            .collect();

        // Phase 2: Download in parallel with bounded concurrency
        let results: Vec<Option<DownloadedAttachment>> = futures::stream::iter(resolved)
            .map(|(original, path, att)| async move {
                match self.download_attachment_to(att, &path, &original).await {
                    Ok(result) => Some(result),
                    Err(e) => {
                        error!("{}", e);
                        None
                    }
                }
            })
            .buffer_unordered(concurrency)
            .collect()
            .await;

        results.into_iter().flatten().collect()
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
