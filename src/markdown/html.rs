//! HTML → Markdown conversion for Jira `renderedFields.*` bodies.
//!
//! The renderer takes the HTML string verbatim, strips Atlassian-specific
//! wrappers, hands the result to `html2md`, then cleans up residual tags
//! and excessive whitespace.

use regex::Regex;

/// Convert HTML content to Markdown. Pure: takes a `&str`, returns a `String`.
pub(crate) fn convert_html_to_markdown(html_content: &str) -> String {
    if html_content.is_empty() {
        return String::new();
    }

    // Remove Atlassian-specific wrappers
    let re_thumbnail =
        Regex::new(r"(?si)<jira-attachment-thumbnail[^>]*>(.*?)</jira-attachment-thumbnail>")
            .unwrap();
    let html = re_thumbnail.replace_all(html_content, "$1").to_string();

    let re_img_link = Regex::new(r"(?si)<a\b[^>]*>\s*(<img\b[^>]*>)\s*</a>").unwrap();
    let html = re_img_link.replace_all(&html, "$1").to_string();

    // Convert HTML to Markdown using html2md
    let markdown = html2md::parse_html(&html);

    // Clean up residual tags
    let re_tags = Regex::new(r"<[^>]+>").unwrap();
    let markdown = re_tags.replace_all(&markdown, "").to_string();

    // Clean up excessive whitespace
    let re_ws = Regex::new(r"\n{3,}").unwrap();
    let markdown = re_ws.replace_all(&markdown, "\n\n").to_string();

    markdown.trim().to_string()
}
