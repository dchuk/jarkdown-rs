//! Renderer for Jira custom fields with type-aware formatting.

use serde_json::Value;

/// Renders custom field values based on field type metadata.
pub struct CustomFieldRenderer<F>
where
    F: Fn(&Value) -> String,
{
    adf_parser: F,
}

impl<F> CustomFieldRenderer<F>
where
    F: Fn(&Value) -> String,
{
    pub fn new(adf_parser: F) -> Self {
        Self { adf_parser }
    }

    /// Render a custom field value to markdown string.
    pub fn render_value(&self, value: &Value, schema: &Value) -> Option<String> {
        if value.is_null() {
            return None;
        }

        // Try schema-based rendering first
        if let Some(schema_type) = schema.get("type").and_then(|t| t.as_str()) {
            if let Some(rendered) = self.render_by_schema(value, schema_type) {
                return Some(rendered);
            }
        }

        // Fall back to value shape inspection
        self.render_by_shape(value)
    }

    fn render_by_schema(&self, value: &Value, schema_type: &str) -> Option<String> {
        match schema_type {
            "string" | "number" | "date" => Some(value_to_string(value)),
            "datetime" => {
                let s = value_to_string(value);
                Some(if s.len() > 19 { s[..19].to_string() } else { s })
            }
            "option" => {
                if let Some(obj) = value.as_object() {
                    Some(
                        obj.get("value")
                            .and_then(|v| v.as_str())
                            .unwrap_or(&value.to_string())
                            .to_string(),
                    )
                } else {
                    Some(value_to_string(value))
                }
            }
            "user" => {
                if let Some(obj) = value.as_object() {
                    Some(
                        obj.get("displayName")
                            .or_else(|| obj.get("name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or(&value.to_string())
                            .to_string(),
                    )
                } else {
                    Some(value_to_string(value))
                }
            }
            "array" => self.render_array(value),
            "any" => self.render_by_shape(value),
            _ => None,
        }
    }

    fn render_array(&self, value: &Value) -> Option<String> {
        let arr = match value.as_array() {
            Some(a) if !a.is_empty() => a,
            _ => return None,
        };

        let items: Vec<String> = arr
            .iter()
            .map(|item| {
                if let Some(obj) = item.as_object() {
                    obj.get("value")
                        .or_else(|| obj.get("displayName"))
                        .or_else(|| obj.get("name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or(&item.to_string())
                        .to_string()
                } else {
                    value_to_string(item)
                }
            })
            .collect();

        Some(items.join(", "))
    }

    fn render_by_shape(&self, value: &Value) -> Option<String> {
        match value {
            Value::String(s) if !s.is_empty() => Some(s.clone()),
            Value::String(_) => None,
            Value::Number(n) => Some(n.to_string()),
            Value::Bool(b) => Some(b.to_string()),
            Value::Object(obj) => {
                // ADF document
                if obj.get("type").and_then(|t| t.as_str()) == Some("doc") {
                    let rendered = (self.adf_parser)(value);
                    return if rendered.is_empty() { None } else { Some(rendered) };
                }
                // Option/select
                if let Some(v) = obj.get("value").and_then(|v| v.as_str()) {
                    return Some(v.to_string());
                }
                // User
                if let Some(v) = obj.get("displayName").and_then(|v| v.as_str()) {
                    return Some(v.to_string());
                }
                // Named entity
                if let Some(v) = obj.get("name").and_then(|v| v.as_str()) {
                    return Some(v.to_string());
                }
                Some(value.to_string())
            }
            Value::Array(_) => self.render_array(value),
            _ => None,
        }
    }
}

fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => v.to_string(),
    }
}
