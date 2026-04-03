//! Converter for transforming Jira issues to Markdown format.

use std::collections::HashMap;

use regex::Regex;
use serde_json::Value;
use urlencoding::encode as url_encode;

use crate::attachment::DownloadedAttachment;
use crate::config::FieldFilter;
use crate::custom_field::CustomFieldRenderer;
use crate::field_cache::FieldMetadataCache;

/// Converts Jira issue data into Markdown format.
pub struct MarkdownConverter {
    base_url: String,
    domain: String,
    attachments_by_id: HashMap<String, DownloadedAttachment>,
    attachments_by_name: HashMap<String, DownloadedAttachment>,
}

impl MarkdownConverter {
    pub fn new(base_url: &str, domain: &str) -> Self {
        Self {
            base_url: base_url.to_string(),
            domain: domain.to_string(),
            attachments_by_id: HashMap::new(),
            attachments_by_name: HashMap::new(),
        }
    }

    fn prepare_attachment_lookup(&mut self, downloaded: &[DownloadedAttachment]) {
        self.attachments_by_id.clear();
        self.attachments_by_name.clear();

        for att in downloaded {
            if let Some(ref id) = att.attachment_id {
                self.attachments_by_id.insert(id.clone(), att.clone());
            }
            self.attachments_by_name
                .insert(att.original_filename.to_lowercase(), att.clone());
            self.attachments_by_name
                .insert(att.filename.to_lowercase(), att.clone());
        }
    }

    fn get_attachment_for_media(
        &self,
        attachment_id: Option<&str>,
        filename_hint: Option<&str>,
    ) -> Option<&DownloadedAttachment> {
        if let Some(id) = attachment_id {
            if let Some(att) = self.attachments_by_id.get(id) {
                return Some(att);
            }
        }
        if let Some(hint) = filename_hint {
            let normalized = hint.trim().to_lowercase();
            if let Some(att) = self.attachments_by_name.get(&normalized) {
                return Some(att);
            }
        }
        None
    }

    fn media_attrs_to_markdown(&self, attrs: &Value) -> String {
        if !attrs.is_object() {
            return "![attachment](attachment)".to_string();
        }

        let media_type = attrs["type"].as_str().unwrap_or("file");
        let filename_hint = attrs["alt"]
            .as_str()
            .or_else(|| attrs["title"].as_str())
            .or_else(|| attrs["fileName"].as_str())
            .unwrap_or("");

        if media_type == "external" {
            if let Some(url) = attrs["url"].as_str() {
                let alt = if filename_hint.is_empty() { url } else { filename_hint };
                return format!("![{}]({})", alt, url);
            }
        }

        let att = self.get_attachment_for_media(
            attrs["id"].as_str(),
            if filename_hint.is_empty() { None } else { Some(filename_hint) },
        );

        if let Some(att) = att {
            let encoded = url_encode(&att.filename);
            let alt = if !filename_hint.is_empty() {
                filename_hint
            } else {
                &att.original_filename
            };
            return format!("![{}]({})", alt, encoded);
        }

        let alt = if filename_hint.is_empty() { "attachment" } else { filename_hint };
        format!("![{}](attachment)", alt)
    }

    /// Convert HTML content to Markdown.
    pub fn convert_html_to_markdown(&self, html_content: &str) -> String {
        if html_content.is_empty() {
            return String::new();
        }

        // Remove Atlassian-specific wrappers
        let re_thumbnail = Regex::new(
            r"(?si)<jira-attachment-thumbnail[^>]*>(.*?)</jira-attachment-thumbnail>"
        ).unwrap();
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

    /// Replace Jira attachment URLs with local file references.
    pub fn replace_attachment_links(
        &self,
        markdown_content: &str,
        downloaded: &[DownloadedAttachment],
    ) -> String {
        if downloaded.is_empty() {
            return markdown_content.to_string();
        }

        let escaped_domain = regex::escape(&self.domain);
        let optional_domain = format!(r"(?:https?://{})?", escaped_domain);
        let rest_prefix = format!(r"{}/(?:jira/)?rest/api/[0-9]+/attachment", optional_domain);
        let secure_prefix = format!(r"{}/secure/attachment", optional_domain);

        let mut result = markdown_content.to_string();

        for att in downloaded {
            let encoded = url_encode(&att.filename).to_string();
            let escaped_orig = regex::escape(&att.original_filename);
            let encoded_orig = regex::escape(&url_encode(&att.original_filename));

            for pattern_name in &[&escaped_orig, &encoded_orig] {
                let pattern = format!(r"{}/[0-9]+/{}", secure_prefix, pattern_name);

                // Replace in images
                if let Ok(re) = Regex::new(&format!(r"(!\[[^\]]*\])\({}(?:\?[^)]*)?\)", pattern)) {
                    result = re.replace_all(&result, format!("$1({})", encoded)).to_string();
                }
                // Replace in links
                if let Ok(re) = Regex::new(&format!(r"(\[[^\]]+\])\({}(?:\?[^)]*)?\)", pattern)) {
                    result = re.replace_all(&result, format!("$1({})", encoded)).to_string();
                }
            }

            if let Some(ref att_id) = att.attachment_id {
                let escaped_id = regex::escape(att_id);
                let id_pattern = format!(r"{}/(?:content|thumbnail)/{}", rest_prefix, escaped_id);

                if let Ok(re) = Regex::new(&format!(r"(!\[[^\]]*\])\({}\)", id_pattern)) {
                    result = re.replace_all(&result, format!("$1({})", encoded)).to_string();
                }
                if let Ok(re) = Regex::new(&format!(r"(\[[^\]]+\])\({}\)", id_pattern)) {
                    result = re.replace_all(&result, format!("$1({})", encoded)).to_string();
                }
            }
        }

        result
    }

    /// Parse Atlassian Document Format to Markdown.
    pub fn parse_adf_to_markdown(&self, adf: &Value) -> String {
        if let Some(s) = adf.as_str() {
            return s.to_string();
        }
        if !adf.is_object() {
            return String::new();
        }

        let doc_type = adf["type"].as_str().unwrap_or("");
        let content = adf["content"].as_array();
        let attrs = &adf["attrs"];

        match doc_type {
            "doc" => content
                .map(|c| {
                    c.iter()
                        .map(|n| self.parse_adf_to_markdown(n))
                        .collect::<Vec<_>>()
                        .join("\n\n")
                })
                .unwrap_or_default(),

            "paragraph" => content
                .map(|c| {
                    c.iter()
                        .map(|n| self.parse_adf_to_markdown(n))
                        .collect::<Vec<_>>()
                        .join("")
                })
                .unwrap_or_default(),

            "text" => {
                let mut text = adf["text"].as_str().unwrap_or("").to_string();
                if let Some(marks) = adf["marks"].as_array() {
                    for mark in marks {
                        match mark["type"].as_str().unwrap_or("") {
                            "strong" => text = format!("**{}**", text),
                            "em" => text = format!("*{}*", text),
                            "code" => text = format!("`{}`", text),
                            "link" => {
                                let href = mark["attrs"]["href"].as_str().unwrap_or("");
                                text = format!("[{}]({})", text, href);
                            }
                            _ => {}
                        }
                    }
                }
                text
            }

            "bulletList" => content
                .map(|c| {
                    c.iter()
                        .flat_map(|item| {
                            self.parse_adf_to_markdown(item)
                                .lines()
                                .filter(|l| !l.is_empty())
                                .map(|l| format!("- {}", l))
                                .collect::<Vec<_>>()
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default(),

            "orderedList" => content
                .map(|c| {
                    c.iter()
                        .enumerate()
                        .flat_map(|(i, item)| {
                            let text = self.parse_adf_to_markdown(item);
                            text.lines()
                                .enumerate()
                                .filter(|(_, l)| !l.is_empty())
                                .map(|(j, l)| {
                                    if j == 0 {
                                        format!("{}. {}", i + 1, l)
                                    } else {
                                        format!("   {}", l)
                                    }
                                })
                                .collect::<Vec<_>>()
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default(),

            "listItem" => content
                .map(|c| {
                    c.iter()
                        .map(|n| self.parse_adf_to_markdown(n))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default(),

            "heading" => {
                let level = attrs["level"].as_u64().unwrap_or(1) as usize;
                let text = content
                    .map(|c| {
                        c.iter()
                            .map(|n| self.parse_adf_to_markdown(n))
                            .collect::<Vec<_>>()
                            .join("")
                    })
                    .unwrap_or_default();
                format!("{} {}", "#".repeat(level), text)
            }

            "codeBlock" => {
                let code = content
                    .map(|c| {
                        c.iter()
                            .map(|n| self.parse_adf_to_markdown(n))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                let lang = attrs["language"].as_str().unwrap_or("");
                format!("```{}\n{}\n```", lang, code)
            }

            "blockquote" => {
                let text = content
                    .map(|c| {
                        c.iter()
                            .map(|n| self.parse_adf_to_markdown(n))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                text.lines().map(|l| format!("> {}", l)).collect::<Vec<_>>().join("\n")
            }

            "mediaSingle" => {
                if let Some(c) = content {
                    let rendered: Vec<String> = c
                        .iter()
                        .map(|n| self.parse_adf_to_markdown(n))
                        .filter(|s| !s.is_empty())
                        .collect();
                    if !rendered.is_empty() {
                        return rendered.join("\n");
                    }
                }
                self.media_attrs_to_markdown(attrs)
            }

            "media" => self.media_attrs_to_markdown(attrs),

            "mention" => {
                let text = attrs["text"]
                    .as_str()
                    .or_else(|| attrs["id"].as_str())
                    .unwrap_or("@user");
                format!("@{}", text)
            }

            "hardBreak" => "\n".to_string(),

            "table" => {
                let mut rows = Vec::new();
                if let Some(c) = content {
                    for (i, row_node) in c.iter().enumerate() {
                        let cells = row_node["content"].as_array();
                        let cell_texts: Vec<String> = cells
                            .map(|cells| {
                                cells
                                    .iter()
                                    .map(|cell| {
                                        let cell_content = cell["content"].as_array();
                                        cell_content
                                            .map(|cc| {
                                                cc.iter()
                                                    .map(|n| self.parse_adf_to_markdown(n))
                                                    .collect::<Vec<_>>()
                                                    .join(" ")
                                            })
                                            .unwrap_or_default()
                                            .replace('\n', " ")
                                            .trim()
                                            .to_string()
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        rows.push(format!("| {} |", cell_texts.join(" | ")));
                        if i == 0 {
                            rows.push(format!(
                                "| {} |",
                                cell_texts.iter().map(|_| "---").collect::<Vec<_>>().join(" | ")
                            ));
                        }
                    }
                }
                rows.join("\n")
            }

            "panel" => {
                let panel_type = attrs["panelType"].as_str().unwrap_or("info");
                let title = capitalize(panel_type);
                let body = content
                    .map(|c| {
                        c.iter()
                            .map(|n| self.parse_adf_to_markdown(n))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                let mut lines = vec![format!("> **{}:**", title)];
                for line in body.lines() {
                    if line.is_empty() {
                        lines.push(">".to_string());
                    } else {
                        lines.push(format!("> {}", line));
                    }
                }
                lines.join("\n")
            }

            "expand" => {
                let title = attrs["title"].as_str().unwrap_or("Details");
                let body = content
                    .map(|c| {
                        c.iter()
                            .map(|n| self.parse_adf_to_markdown(n))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                let mut lines = vec![format!("**{}**", title), String::new()];
                for line in body.lines() {
                    if line.is_empty() {
                        lines.push(String::new());
                    } else {
                        lines.push(format!("  {}", line));
                    }
                }
                lines.join("\n")
            }

            "rule" => "---".to_string(),

            "emoji" => attrs["shortName"]
                .as_str()
                .or_else(|| attrs["text"].as_str())
                .unwrap_or("")
                .to_string(),

            "status" => {
                let text = attrs["text"].as_str().unwrap_or("");
                format!("**{}**", text)
            }

            "date" => {
                if let Some(ts) = attrs["timestamp"].as_str() {
                    if let Ok(ms) = ts.parse::<i64>() {
                        if let Some(dt) = chrono::DateTime::from_timestamp(ms / 1000, 0) {
                            return dt.format("%Y-%m-%d").to_string();
                        }
                    }
                    ts.to_string()
                } else {
                    String::new()
                }
            }

            "inlineCard" => {
                if let Some(url) = attrs["url"].as_str() {
                    format!("[{}]({})", url, url)
                } else {
                    String::new()
                }
            }

            "taskList" | "decisionList" | "mediaGroup" => content
                .map(|c| {
                    c.iter()
                        .map(|n| self.parse_adf_to_markdown(n))
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default(),

            "taskItem" => {
                let state = attrs["state"].as_str().unwrap_or("TODO");
                let checkbox = if state == "DONE" { "[x]" } else { "[ ]" };
                let text = content
                    .map(|c| {
                        c.iter()
                            .map(|n| self.parse_adf_to_markdown(n))
                            .collect::<Vec<_>>()
                            .join("")
                    })
                    .unwrap_or_default();
                format!("- {} {}", checkbox, text)
            }

            "decisionItem" => {
                let text = content
                    .map(|c| {
                        c.iter()
                            .map(|n| self.parse_adf_to_markdown(n))
                            .collect::<Vec<_>>()
                            .join("")
                    })
                    .unwrap_or_default();
                format!("> **Decision:** {}", text)
            }

            _ => content
                .map(|c| {
                    c.iter()
                        .map(|n| self.parse_adf_to_markdown(n))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default(),
        }
    }

    /// Extract plain text from ADF content, stripping all formatting.
    fn adf_to_plain_text(&self, adf: &Value) -> String {
        if adf.is_null() {
            return String::new();
        }
        if let Some(s) = adf.as_str() {
            return s.to_string();
        }
        if !adf.is_object() {
            return String::new();
        }
        if adf["type"].as_str() == Some("text") {
            return adf["text"].as_str().unwrap_or("").to_string();
        }
        adf["content"]
            .as_array()
            .map(|c| {
                c.iter()
                    .map(|n| self.adf_to_plain_text(n))
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default()
    }

    /// Generate metadata dictionary from Jira issue data.
    fn generate_metadata(&self, issue_data: &Value) -> serde_yaml::Value {
        use serde_yaml::Value as Y;

        let fields = &issue_data["fields"];
        let mut map = serde_yaml::Mapping::new();

        let set_str = |map: &mut serde_yaml::Mapping, key: &str, val: Option<&str>| {
            map.insert(Y::String(key.into()), match val {
                Some(s) => Y::String(s.to_string()),
                None => Y::Null,
            });
        };

        set_str(&mut map, "key", issue_data["key"].as_str());
        set_str(&mut map, "summary", fields["summary"].as_str());
        set_str(&mut map, "type", fields["issuetype"]["name"].as_str());
        set_str(&mut map, "status", fields["status"]["name"].as_str());
        set_str(&mut map, "status_category", fields["status"]["statusCategory"]["name"].as_str());
        set_str(&mut map, "priority", fields["priority"]["name"].as_str());
        set_str(&mut map, "resolution", fields["resolution"]["name"].as_str());
        set_str(&mut map, "project", fields["project"]["name"].as_str());
        set_str(&mut map, "project_key", fields["project"]["key"].as_str());
        set_str(&mut map, "assignee", fields["assignee"]["displayName"].as_str());
        set_str(&mut map, "reporter", fields["reporter"]["displayName"].as_str());
        set_str(&mut map, "creator", fields["creator"]["displayName"].as_str());

        // Labels
        let labels: Vec<Y> = fields["labels"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| Y::String(s.into()))).collect())
            .unwrap_or_default();
        map.insert(Y::String("labels".into()), Y::Sequence(labels));

        // Components
        let components: Vec<Y> = fields["components"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|c| c["name"].as_str().map(|s| Y::String(s.into())))
                    .collect()
            })
            .unwrap_or_default();
        map.insert(Y::String("components".into()), Y::Sequence(components));

        // Parent
        set_str(&mut map, "parent_key", fields["parent"]["key"].as_str());
        set_str(&mut map, "parent_summary", fields["parent"]["fields"]["summary"].as_str());

        // Versions
        let affects: Vec<Y> = fields["versions"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v["name"].as_str().map(|s| Y::String(s.into()))).collect())
            .unwrap_or_default();
        map.insert(Y::String("affects_versions".into()), Y::Sequence(affects));

        let fix_ver: Vec<Y> = fields["fixVersions"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v["name"].as_str().map(|s| Y::String(s.into()))).collect())
            .unwrap_or_default();
        map.insert(Y::String("fix_versions".into()), Y::Sequence(fix_ver));

        // Dates
        set_str(&mut map, "created_at", fields["created"].as_str());
        set_str(&mut map, "updated_at", fields["updated"].as_str());
        set_str(&mut map, "resolved_at", fields["resolutiondate"].as_str());
        set_str(&mut map, "duedate", fields["duedate"].as_str());

        // Time tracking
        let tt = &fields["timetracking"];
        set_str(&mut map, "original_estimate", tt["originalEstimate"].as_str());
        set_str(&mut map, "time_spent", tt["timeSpent"].as_str());
        set_str(&mut map, "remaining_estimate", tt["remainingEstimate"].as_str());

        // Progress
        let progress = fields["progress"]["percent"].as_u64().unwrap_or(0);
        let agg_progress = fields["aggregateprogress"]["percent"].as_u64().unwrap_or(0);
        map.insert(Y::String("progress".into()), Y::Number(progress.into()));
        map.insert(Y::String("aggregate_progress".into()), Y::Number(agg_progress.into()));

        // Votes and watches
        let votes = fields["votes"]["votes"].as_u64().unwrap_or(0);
        let watches = fields["watches"]["watchCount"].as_u64().unwrap_or(0);
        map.insert(Y::String("votes".into()), Y::Number(votes.into()));
        map.insert(Y::String("watches".into()), Y::Number(watches.into()));

        Y::Mapping(map)
    }

    fn compose_environment_section(&self, issue_data: &Value) -> Vec<String> {
        let mut lines = vec!["## Environment".into(), String::new()];
        let rendered_env = issue_data["renderedFields"]["environment"].as_str();
        if let Some(html) = rendered_env {
            if !html.is_empty() {
                lines.push(self.convert_html_to_markdown(html));
            } else {
                lines.push("None".into());
            }
        } else {
            let raw_env = &issue_data["fields"]["environment"];
            if raw_env.is_object() {
                lines.push(self.parse_adf_to_markdown(raw_env));
            } else if let Some(s) = raw_env.as_str() {
                lines.push(s.to_string());
            } else {
                lines.push("None".into());
            }
        }
        lines.push(String::new());
        lines
    }

    fn compose_linked_issues_section(&self, issue_data: &Value) -> Vec<String> {
        let mut lines = vec!["## Linked Issues".into(), String::new()];
        let issuelinks = issue_data["fields"]["issuelinks"].as_array();

        let links = match issuelinks {
            Some(l) if !l.is_empty() => l,
            _ => {
                lines.push("None".into());
                lines.push(String::new());
                return lines;
            }
        };

        let mut groups: HashMap<String, Vec<&Value>> = HashMap::new();
        for link in links {
            let link_type = &link["type"];
            let (label, issue) = if link.get("outwardIssue").is_some() && !link["outwardIssue"].is_null() {
                let l = link_type["outward"].as_str().unwrap_or("Related");
                (capitalize(l), &link["outwardIssue"])
            } else if link.get("inwardIssue").is_some() && !link["inwardIssue"].is_null() {
                let l = link_type["inward"].as_str().unwrap_or("Related");
                (capitalize(l), &link["inwardIssue"])
            } else {
                continue;
            };
            groups.entry(label).or_default().push(issue);
        }

        for (label, issues) in &groups {
            lines.push(format!("### {}", label));
            lines.push(String::new());
            for issue in issues {
                let key = issue["key"].as_str().unwrap_or("UNKNOWN");
                let summary = issue["fields"]["summary"].as_str().unwrap_or("");
                let status = issue["fields"]["status"]["name"].as_str().unwrap_or("");
                lines.push(format!(
                    "- [{}]({}/browse/{}): {} ({})",
                    key, self.base_url, key, summary, status
                ));
            }
            lines.push(String::new());
        }
        lines
    }

    fn compose_subtasks_section(&self, issue_data: &Value) -> Vec<String> {
        let mut lines = vec!["## Subtasks".into(), String::new()];
        let subtasks = issue_data["fields"]["subtasks"].as_array();

        match subtasks {
            Some(s) if !s.is_empty() => {
                for subtask in s {
                    let key = subtask["key"].as_str().unwrap_or("UNKNOWN");
                    let summary = subtask["fields"]["summary"].as_str().unwrap_or("");
                    let status = subtask["fields"]["status"]["name"].as_str().unwrap_or("");
                    let itype = subtask["fields"]["issuetype"]["name"].as_str().unwrap_or("");
                    lines.push(format!(
                        "- [{}]({}/browse/{}): {} ({}) \u{2014} {}",
                        key, self.base_url, key, summary, status, itype
                    ));
                }
            }
            _ => lines.push("None".into()),
        }
        lines.push(String::new());
        lines
    }

    fn compose_worklogs_section(&self, issue_data: &Value) -> Vec<String> {
        let fields = &issue_data["fields"];
        let worklog_data = &fields["worklog"];
        let worklogs = worklog_data["worklogs"].as_array();
        let total = worklog_data["total"].as_u64().unwrap_or(0);

        let mut lines = vec!["## Worklogs".into(), String::new()];

        let wl = match worklogs {
            Some(w) if !w.is_empty() => w,
            _ => {
                lines.push("None".into());
                lines.push(String::new());
                return lines;
            }
        };

        let total_seconds: u64 = wl
            .iter()
            .map(|e| e["timeSpentSeconds"].as_u64().unwrap_or(0))
            .sum();
        lines.push(format!("**Total Time Logged:** {}", format_time(total_seconds)));
        lines.push(String::new());

        if total > wl.len() as u64 {
            lines.push(format!(
                "> **Note:** Showing {} of {} worklogs. Additional worklogs may exist.",
                wl.len(),
                total
            ));
            lines.push(String::new());
        }

        lines.push("| Author | Time Spent | Date | Comment |".into());
        lines.push("|--------|-----------|------|---------|".into());

        for entry in wl {
            let author = entry["author"]["displayName"].as_str().unwrap_or("Unknown");
            let time_spent = entry["timeSpent"].as_str().unwrap_or("");
            let started = entry["started"].as_str().unwrap_or("");
            let date = if started.len() >= 10 { &started[..10] } else { started };
            let comment = self.adf_to_plain_text(&entry["comment"]).replace('|', "\\|");
            lines.push(format!("| {} | {} | {} | {} |", author, time_spent, date, comment));
        }
        lines.push(String::new());
        lines
    }

    fn compose_custom_fields_section(
        &self,
        issue_data: &Value,
        field_cache: &mut Option<FieldMetadataCache>,
        field_filter: &Option<FieldFilter>,
    ) -> Vec<String> {
        let fields = match issue_data["fields"].as_object() {
            Some(f) => f,
            None => return Vec::new(),
        };

        let mut custom_fields: Vec<(String, String, Value)> = Vec::new();

        for (key, value) in fields {
            if !key.starts_with("customfield_") || value.is_null() {
                continue;
            }

            let display_name = field_cache
                .as_mut()
                .map(|fc| fc.get_field_name(key))
                .unwrap_or_else(|| key.clone());

            // Apply field filter
            if let Some(ref filter) = field_filter {
                if filter.exclude.contains(&display_name) {
                    continue;
                }
                if let Some(ref include) = filter.include {
                    if !include.contains(&display_name) {
                        continue;
                    }
                }
            }

            let _schema = field_cache
                .as_ref()
                .map(|fc| fc.get_field_schema(key))
                .unwrap_or(Value::Null);

            custom_fields.push((display_name, key.clone(), value.clone()));
        }

        if custom_fields.is_empty() {
            return Vec::new();
        }

        custom_fields.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));

        let converter_ref = self;
        let renderer = CustomFieldRenderer::new(|v: &Value| converter_ref.parse_adf_to_markdown(v));

        let mut lines = vec!["## Custom Fields".into(), String::new()];

        for (display_name, field_id, value) in &custom_fields {
            let schema = field_cache
                .as_ref()
                .map(|fc| fc.get_field_schema(field_id))
                .unwrap_or(Value::Null);

            let rendered = match renderer.render_value(value, &schema) {
                Some(r) => r,
                None => continue,
            };

            if rendered.contains('\n') {
                lines.push(format!("### {}", display_name));
                lines.push(String::new());
                lines.push(rendered);
                lines.push(String::new());
            } else {
                lines.push(format!("- **{}:** {}", display_name, rendered));
            }
        }
        lines.push(String::new());
        lines
    }

    fn compose_comments_section(
        &self,
        issue_data: &Value,
        downloaded: &[DownloadedAttachment],
    ) -> Vec<String> {
        let comments = issue_data["fields"]["comment"]["comments"].as_array();
        let comments = match comments {
            Some(c) if !c.is_empty() => c,
            _ => return Vec::new(),
        };

        // Build rendered comment lookup
        let mut rendered_lookup: HashMap<String, &Value> = HashMap::new();
        if let Some(rendered_comments) = issue_data["renderedFields"]["comment"]["comments"].as_array() {
            for rc in rendered_comments {
                if let Some(id) = rc["id"].as_str() {
                    rendered_lookup.insert(id.to_string(), rc);
                }
            }
        }

        let mut lines = vec!["## Comments".into(), String::new()];

        for (i, comment) in comments.iter().enumerate() {
            let author = comment["author"]["displayName"].as_str().unwrap_or("Unknown");
            let created = comment["created"].as_str().unwrap_or("");
            let formatted_date = format_jira_date(created);

            lines.push(format!("**{}** - _{}_", author, formatted_date));
            lines.push(String::new());

            // Process comment body
            let body_html = comment["renderedBody"].as_str().unwrap_or("");
            let body_md = if !body_html.is_empty() {
                self.convert_html_to_markdown(body_html)
            } else {
                let rendered = comment["id"]
                    .as_str()
                    .and_then(|id| rendered_lookup.get(id))
                    .and_then(|rc| rc["body"].as_str());

                if let Some(html) = rendered {
                    self.convert_html_to_markdown(html)
                } else {
                    let body = &comment["body"];
                    if body.is_object() {
                        self.parse_adf_to_markdown(body)
                    } else if let Some(s) = body.as_str() {
                        s.to_string()
                    } else {
                        "*No comment body*".to_string()
                    }
                }
            };

            let body_md = self.replace_attachment_links(&body_md, downloaded);
            lines.push(body_md);

            if i < comments.len() - 1 {
                lines.push(String::new());
                lines.push("---".into());
                lines.push(String::new());
            }
        }
        lines.push(String::new());
        lines
    }

    /// Compose the final markdown file content.
    pub fn compose_markdown(
        &mut self,
        issue_data: &Value,
        downloaded: &[DownloadedAttachment],
        field_cache: &mut Option<FieldMetadataCache>,
        field_filter: &Option<FieldFilter>,
    ) -> String {
        self.prepare_attachment_lookup(downloaded);

        let metadata = self.generate_metadata(issue_data);
        let key = issue_data["key"].as_str().unwrap_or("UNKNOWN");
        let summary = issue_data["fields"]["summary"].as_str().unwrap_or("No Summary");

        let mut lines: Vec<String> = Vec::new();

        // YAML frontmatter
        let yaml_str = serde_yaml::to_string(&metadata).unwrap_or_default();
        lines.push("---".into());
        lines.push(yaml_str.trim_end().to_string());
        lines.push("---".into());
        lines.push(String::new());

        // Title
        lines.push(format!("# [{}]({}/browse/{}): {}", key, self.base_url, key, summary));
        lines.push(String::new());

        // Description
        lines.push("## Description".into());
        lines.push(String::new());

        let rendered_desc = issue_data["renderedFields"]["description"].as_str();
        if let Some(html) = rendered_desc {
            if !html.is_empty() {
                let md = self.convert_html_to_markdown(html);
                let md = self.replace_attachment_links(&md, downloaded);
                lines.push(md);
            } else {
                lines.push("*No description provided*".into());
            }
        } else {
            let raw_desc = &issue_data["fields"]["description"];
            if raw_desc.is_object() {
                let md = self.parse_adf_to_markdown(raw_desc);
                let md = self.replace_attachment_links(&md, downloaded);
                lines.push(md);
            } else if let Some(s) = raw_desc.as_str() {
                let md = self.replace_attachment_links(s, downloaded);
                lines.push(md);
            } else {
                lines.push("*No description provided*".into());
            }
        }
        lines.push(String::new());

        // Sections
        lines.extend(self.compose_environment_section(issue_data));
        lines.extend(self.compose_linked_issues_section(issue_data));
        lines.extend(self.compose_subtasks_section(issue_data));
        lines.extend(self.compose_worklogs_section(issue_data));

        let custom_lines = self.compose_custom_fields_section(issue_data, field_cache, field_filter);
        if !custom_lines.is_empty() {
            lines.extend(custom_lines);
        }

        let comment_lines = self.compose_comments_section(issue_data, downloaded);
        if !comment_lines.is_empty() {
            lines.extend(comment_lines);
        }

        // Attachments
        if !downloaded.is_empty() {
            lines.push("## Attachments".into());
            lines.push(String::new());
            for att in downloaded {
                let encoded = url_encode(&att.filename);
                if att.mime_type.starts_with("image/") {
                    lines.push(format!("- ![{}]({})", att.filename, encoded));
                } else {
                    lines.push(format!("- [{}]({})", att.filename, encoded));
                }
            }
            lines.push(String::new());
        }

        lines.join("\n")
    }
}

/// Title-case a string (matching Python's str.title()).
fn capitalize(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut capitalize_next = true;
    for c in s.chars() {
        if c.is_whitespace() || c == '-' || c == '_' {
            result.push(c);
            capitalize_next = true;
        } else if capitalize_next {
            for uc in c.to_uppercase() {
                result.push(uc);
            }
            capitalize_next = false;
        } else {
            for lc in c.to_lowercase() {
                result.push(lc);
            }
        }
    }
    result
}

fn format_time(seconds: u64) -> String {
    let days = seconds / 28800; // 8h workday
    let remaining = seconds % 28800;
    let hours = remaining / 3600;
    let remaining = remaining % 3600;
    let minutes = remaining / 60;

    let mut parts = Vec::new();
    if days > 0 { parts.push(format!("{}d", days)); }
    if hours > 0 { parts.push(format!("{}h", hours)); }
    if minutes > 0 { parts.push(format!("{}m", minutes)); }
    if parts.is_empty() { "0m".to_string() } else { parts.join(" ") }
}

fn format_jira_date(created: &str) -> String {
    if created.is_empty() {
        return "Unknown date".to_string();
    }
    // Try parsing ISO 8601
    let normalized = if created.ends_with('Z') {
        created.replace('Z', "+00:00")
    } else if created.contains('+') && !created.ends_with("+00:00") {
        // Replace +0000 with +00:00
        let re = regex::Regex::new(r"\+(\d{2})(\d{2})$").unwrap();
        re.replace(created, "+$1:$2").to_string()
    } else {
        created.to_string()
    };

    match chrono::DateTime::parse_from_rfc3339(&normalized) {
        Ok(dt) => dt.format("%Y-%m-%d %I:%M %p").to_string(),
        Err(_) => {
            // Try chrono's more lenient parsing
            match chrono::NaiveDateTime::parse_from_str(&created[..19.min(created.len())], "%Y-%m-%dT%H:%M:%S") {
                Ok(dt) => dt.format("%Y-%m-%d %I:%M %p").to_string(),
                Err(_) => created.to_string(),
            }
        }
    }
}
