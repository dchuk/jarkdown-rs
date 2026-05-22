//! One-method seam between hierarchy traversal and the concrete export workflow.
//!
//! The `IssueExporter` trait lets `HierarchyExporter` delegate per-issue export
//! to either the production workflow (`WorkflowIssueExporter`) or to a test
//! fake. The production impl simply delegates to
//! `perform_export_with_options`.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use crate::error::Result;
use crate::export::{perform_export_with_options, ExportWorkflowOptions};
use crate::jira_client::JiraApiClient;

/// One-method seam between hierarchy traversal and the concrete export
/// workflow. A test fake can record keys; the production impl delegates to
/// `perform_export_with_options`.
///
/// The returned future is intentionally not `Send`: the production workflow
/// transitively holds a `rand::ThreadRng` across `.await` in
/// `crate::retry::retry_with_backoff`, which is `!Send`. The hierarchy
/// traversal itself drives the future on a single-task local scope so this
/// is sufficient.
pub trait IssueExporter {
    fn export<'a>(
        &'a self,
        issue_key: &'a str,
        output_dir: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>>;
}

/// Production implementation that delegates to the existing workflow.
pub struct WorkflowIssueExporter<'a> {
    pub api_client: &'a JiraApiClient,
    pub options: ExportWorkflowOptions<'a>,
}

impl<'a> IssueExporter for WorkflowIssueExporter<'a> {
    fn export<'b>(
        &'b self,
        issue_key: &'b str,
        output_dir: &'b Path,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + 'b>> {
        Box::pin(async move {
            perform_export_with_options(
                self.api_client,
                issue_key,
                output_dir,
                self.options.clone(),
            )
            .await
            .map(|_| ())
        })
    }
}
