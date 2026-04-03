//! # Jarkdown
//!
//! Export Jira Cloud issues to Markdown with attachments.
//!
//! This crate can be used as both a CLI tool and as a library in other Rust projects.
//!
//! ## Library Usage
//!
//! ```rust,no_run
//! use jarkdown::{JiraApiClient, export_issue};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let client = JiraApiClient::new(
//!         "company.atlassian.net",
//!         "user@example.com",
//!         "your-api-token",
//!     )?;
//!
//!     let output_path = export_issue(&client, "PROJ-123", None, Default::default()).await?;
//!     println!("Exported to {:?}", output_path);
//!     Ok(())
//! }
//! ```

pub mod attachment;
pub mod bulk;
pub mod cli;
pub mod config;
pub mod custom_field;
pub mod error;
pub mod export;
pub mod field_cache;
pub mod jira_client;
pub mod markdown;
pub mod retry;

// Re-export key types for library consumers
pub use attachment::DownloadedAttachment;
pub use bulk::{BulkExporter, ExportResult};
pub use config::{ConfigManager, FieldFilter};
pub use error::{JarkdownError, Result};
pub use export::perform_export;
pub use field_cache::FieldMetadataCache;
pub use jira_client::JiraApiClient;
pub use markdown::MarkdownConverter;
pub use retry::RetryConfig;

use std::path::{Path, PathBuf};

/// Options for exporting a single issue.
#[derive(Debug, Clone, Default)]
pub struct ExportOptions {
    pub refresh_fields: bool,
    pub include_fields: Option<String>,
    pub exclude_fields: Option<String>,
    pub include_json: bool,
}

/// High-level convenience function to export a single Jira issue.
///
/// This is the primary library entry point for simple use cases.
///
/// # Arguments
/// * `client` - An initialized `JiraApiClient`
/// * `issue_key` - Jira issue key (e.g., "PROJ-123")
/// * `output_dir` - Output directory (None = current directory)
/// * `options` - Export options
///
/// # Returns
/// The path to the directory where files were saved.
pub async fn export_issue(
    client: &JiraApiClient,
    issue_key: &str,
    output_dir: Option<&Path>,
    options: ExportOptions,
) -> Result<PathBuf> {
    let output_path = output_dir
        .map(|d| d.join(issue_key))
        .unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(issue_key)
        });

    perform_export(
        client,
        issue_key,
        &output_path,
        options.refresh_fields,
        options.include_fields.as_deref(),
        options.exclude_fields.as_deref(),
        options.include_json,
    )
    .await
}
