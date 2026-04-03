//! Custom error types for jarkdown.

use thiserror::Error;

/// Base error type for all jarkdown errors.
#[derive(Error, Debug)]
pub enum JarkdownError {
    #[error("Jira API error: {message}")]
    JiraApi {
        message: String,
        status_code: Option<u16>,
    },

    #[error("Authentication failed: {0}")]
    Authentication(String),

    #[error("Issue not found: {0}")]
    IssueNotFound(String),

    #[error("Attachment download error: {message}")]
    AttachmentDownload {
        message: String,
        filename: Option<String>,
    },

    #[error("Configuration error: {0}")]
    Configuration(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Unexpected error: {0}")]
    Unexpected(String),
}

pub type Result<T> = std::result::Result<T, JarkdownError>;
