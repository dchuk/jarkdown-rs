//! Attachment lookup index + body-local URL rewriter.
//!
//! The render path borrows a [`AttachmentIndex`] inside [`crate::markdown::RenderContext`].
//! It is built once per issue at the [`crate::export`] seam from
//! `(downloaded, skipped)` — the markdown layer never mutates it.
//!
//! Semantics are load-bearing for `--strict-md` byte-identity and must be
//! preserved exactly:
//!
//! * downloaded lookup populates `by_id` + a *dual* `by_name` slot
//!   (`original_filename` and the conflict-resolved local `filename`);
//! * skipped lookup populates `skipped_by_id` + a *single* `skipped_by_name`
//!   slot (the raw `filename` only);
//! * name lookups normalize via `.trim().to_lowercase()`;
//! * `lookup_media` / `lookup_skipped` try the id first, then the name hint.

use std::collections::HashMap;

use regex::Regex;
use serde_json::Value;
use urlencoding::encode as url_encode;

use crate::attachment::DownloadedAttachment;

/// A Jira attachment we intentionally did *not* download (e.g. `--no-attachments`
/// mode). The renderer still needs the filename and source URL so it can emit a
/// link back to Jira instead of a broken local reference.
#[derive(Debug, Clone)]
pub struct SkippedAttachment {
    pub filename: String,
    pub url: Option<String>,
}

/// Borrowed lookup index over the downloaded + skipped attachments for one issue.
///
/// The index borrows from caller-owned slices so [`AttachmentIndex::build`] does
/// no cloning of the `DownloadedAttachment` payloads. Skipped entries are
/// constructed on the fly from raw Jira attachment `Value`s, so those are
/// owned by the index.
pub struct AttachmentIndex<'a> {
    by_id: HashMap<String, &'a DownloadedAttachment>,
    by_name: HashMap<String, &'a DownloadedAttachment>,
    skipped_by_id: HashMap<String, SkippedAttachment>,
    skipped_by_name: HashMap<String, SkippedAttachment>,
}

impl<'a> AttachmentIndex<'a> {
    /// Build the index from the downloaded + skipped attachment slices.
    ///
    /// Both buckets are populated in a single pass — see the module docs for
    /// the load-bearing dual-name / single-name asymmetry.
    pub fn build(downloaded: &'a [DownloadedAttachment], skipped: &'a [Value]) -> Self {
        let mut by_id: HashMap<String, &'a DownloadedAttachment> = HashMap::new();
        let mut by_name: HashMap<String, &'a DownloadedAttachment> = HashMap::new();
        for att in downloaded {
            if let Some(ref id) = att.attachment_id {
                by_id.insert(id.clone(), att);
            }
            by_name.insert(att.original_filename.to_lowercase(), att);
            by_name.insert(att.filename.to_lowercase(), att);
        }

        let mut skipped_by_id: HashMap<String, SkippedAttachment> = HashMap::new();
        let mut skipped_by_name: HashMap<String, SkippedAttachment> = HashMap::new();
        for attachment in skipped {
            let filename = attachment["filename"]
                .as_str()
                .unwrap_or("unknown")
                .to_string();
            let url = attachment["content"].as_str().map(|s| s.to_string());
            let entry = SkippedAttachment {
                filename: filename.clone(),
                url,
            };
            if let Some(id) = attachment["id"].as_str() {
                skipped_by_id.insert(id.to_string(), entry.clone());
            }
            skipped_by_name.insert(filename.to_lowercase(), entry);
        }

        Self {
            by_id,
            by_name,
            skipped_by_id,
            skipped_by_name,
        }
    }

    /// An empty index — used by tests and by render paths that have no
    /// attachment context.
    pub fn empty() -> Self {
        Self {
            by_id: HashMap::new(),
            by_name: HashMap::new(),
            skipped_by_id: HashMap::new(),
            skipped_by_name: HashMap::new(),
        }
    }

    /// Resolve a media node to a downloaded attachment. Tries the id first,
    /// then the trimmed/lowercased name hint.
    pub(crate) fn lookup_media(
        &self,
        id: Option<&str>,
        hint: Option<&str>,
    ) -> Option<&DownloadedAttachment> {
        if let Some(id) = id {
            if let Some(att) = self.by_id.get(id) {
                return Some(*att);
            }
        }
        if let Some(hint) = hint {
            let normalized = hint.trim().to_lowercase();
            if let Some(att) = self.by_name.get(&normalized) {
                return Some(*att);
            }
        }
        None
    }

    /// Resolve a media node to a *skipped* attachment (id-first, then name).
    pub(crate) fn lookup_skipped(
        &self,
        id: Option<&str>,
        hint: Option<&str>,
    ) -> Option<&SkippedAttachment> {
        if let Some(id) = id {
            if let Some(att) = self.skipped_by_id.get(id) {
                return Some(att);
            }
        }
        if let Some(hint) = hint {
            let normalized = hint.trim().to_lowercase();
            if let Some(att) = self.skipped_by_name.get(&normalized) {
                return Some(att);
            }
        }
        None
    }
}

/// Rewrite Jira attachment URLs in a single body of markdown to point at the
/// local downloaded file. Applied *per body* (description / each comment) —
/// running this globally over a composed file would also rewrite the
/// Attachments-section bullets, which breaks `--strict-md` byte-identity.
pub(crate) fn replace_attachment_links(
    markdown: &str,
    downloaded: &[DownloadedAttachment],
    domain: &str,
) -> String {
    if downloaded.is_empty() {
        return markdown.to_string();
    }

    let escaped_domain = regex::escape(domain);
    let optional_domain = format!(r"(?:https?://{})?", escaped_domain);
    let rest_prefix = format!(r"{}/(?:jira/)?rest/api/[0-9]+/attachment", optional_domain);
    let secure_prefix = format!(r"{}/secure/attachment", optional_domain);

    let mut result = markdown.to_string();

    for att in downloaded {
        let encoded = url_encode(&att.filename).to_string();
        let escaped_orig = regex::escape(&att.original_filename);
        let encoded_orig = regex::escape(&url_encode(&att.original_filename));

        for pattern_name in &[&escaped_orig, &encoded_orig] {
            let pattern = format!(r"{}/[0-9]+/{}", secure_prefix, pattern_name);

            // Replace in images
            if let Ok(re) = Regex::new(&format!(r"(!\[[^\]]*\])\({}(?:\?[^)]*)?\)", pattern)) {
                result = re
                    .replace_all(&result, format!("$1({})", encoded))
                    .to_string();
            }
            // Replace in links
            if let Ok(re) = Regex::new(&format!(r"(\[[^\]]+\])\({}(?:\?[^)]*)?\)", pattern)) {
                result = re
                    .replace_all(&result, format!("$1({})", encoded))
                    .to_string();
            }
        }

        if let Some(ref att_id) = att.attachment_id {
            let escaped_id = regex::escape(att_id);
            let id_pattern = format!(r"{}/(?:content|thumbnail)/{}", rest_prefix, escaped_id);

            if let Ok(re) = Regex::new(&format!(r"(!\[[^\]]*\])\({}\)", id_pattern)) {
                result = re
                    .replace_all(&result, format!("$1({})", encoded))
                    .to_string();
            }
            if let Ok(re) = Regex::new(&format!(r"(\[[^\]]+\])\({}\)", id_pattern)) {
                result = re
                    .replace_all(&result, format!("$1({})", encoded))
                    .to_string();
            }
        }
    }

    result
}
