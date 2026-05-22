//! Atlassian Document Format (ADF) → Markdown conversion.
//!
//! The recursive walk takes `&AttachmentIndex` so `media` / `mediaSingle`
//! nodes can resolve to local filenames without the renderer needing
//! mutable state.

use serde_json::Value;
use urlencoding::encode as url_encode;

use crate::markdown::attachments::AttachmentIndex;

/// Render an ADF tree to Markdown.
pub(crate) fn parse_adf_to_markdown(adf: &Value, attachments: &AttachmentIndex<'_>) -> String {
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
                    .map(|n| parse_adf_to_markdown(n, attachments))
                    .collect::<Vec<_>>()
                    .join("\n\n")
            })
            .unwrap_or_default(),

        "paragraph" => content
            .map(|c| {
                c.iter()
                    .map(|n| parse_adf_to_markdown(n, attachments))
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
                        parse_adf_to_markdown(item, attachments)
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
                        let text = parse_adf_to_markdown(item, attachments);
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
                    .map(|n| parse_adf_to_markdown(n, attachments))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default(),

        "heading" => {
            let level = attrs["level"].as_u64().unwrap_or(1) as usize;
            let text = content
                .map(|c| {
                    c.iter()
                        .map(|n| parse_adf_to_markdown(n, attachments))
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
                        .map(|n| parse_adf_to_markdown(n, attachments))
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
                        .map(|n| parse_adf_to_markdown(n, attachments))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            text.lines()
                .map(|l| format!("> {}", l))
                .collect::<Vec<_>>()
                .join("\n")
        }

        "mediaSingle" => {
            if let Some(c) = content {
                let rendered: Vec<String> = c
                    .iter()
                    .map(|n| parse_adf_to_markdown(n, attachments))
                    .filter(|s| !s.is_empty())
                    .collect();
                if !rendered.is_empty() {
                    return rendered.join("\n");
                }
            }
            media_attrs_to_markdown(attrs, attachments)
        }

        "media" => media_attrs_to_markdown(attrs, attachments),

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
                                                .map(|n| parse_adf_to_markdown(n, attachments))
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
                            cell_texts
                                .iter()
                                .map(|_| "---")
                                .collect::<Vec<_>>()
                                .join(" | ")
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
                        .map(|n| parse_adf_to_markdown(n, attachments))
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
                        .map(|n| parse_adf_to_markdown(n, attachments))
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
                    .map(|n| parse_adf_to_markdown(n, attachments))
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
                        .map(|n| parse_adf_to_markdown(n, attachments))
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
                        .map(|n| parse_adf_to_markdown(n, attachments))
                        .collect::<Vec<_>>()
                        .join("")
                })
                .unwrap_or_default();
            format!("> **Decision:** {}", text)
        }

        _ => content
            .map(|c| {
                c.iter()
                    .map(|n| parse_adf_to_markdown(n, attachments))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default(),
    }
}

/// Render a `media` / `mediaSingle` node's `attrs` to a markdown image/link.
pub(crate) fn media_attrs_to_markdown(attrs: &Value, attachments: &AttachmentIndex<'_>) -> String {
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
            let alt = if filename_hint.is_empty() {
                url
            } else {
                filename_hint
            };
            return format!("![{}]({})", alt, url);
        }
    }

    let hint = if filename_hint.is_empty() {
        None
    } else {
        Some(filename_hint)
    };
    let att = attachments.lookup_media(attrs["id"].as_str(), hint);

    if let Some(att) = att {
        let encoded = url_encode(&att.filename);
        let alt = if !filename_hint.is_empty() {
            filename_hint
        } else {
            &att.original_filename
        };
        return format!("![{}]({})", alt, encoded);
    }

    if let Some(att) = attachments.lookup_skipped(attrs["id"].as_str(), hint) {
        let alt = if filename_hint.is_empty() {
            &att.filename
        } else {
            filename_hint
        };
        if let Some(url) = &att.url {
            return format!("[{}]({})", alt, url);
        }
        return alt.to_string();
    }

    let alt = if filename_hint.is_empty() {
        "attachment"
    } else {
        filename_hint
    };
    format!("![{}](attachment)", alt)
}

/// Extract plain text from ADF content, stripping all formatting.
pub(crate) fn adf_to_plain_text(adf: &Value) -> String {
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
                .map(adf_to_plain_text)
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

/// Title-case a string (matching Python's str.title()).
pub(crate) fn capitalize(s: &str) -> String {
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
