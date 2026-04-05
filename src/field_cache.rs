//! Cache for Jira field metadata with XDG-compliant storage.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use log::warn;
use serde_json::Value;

const CACHE_TTL_SECONDS: u64 = 86400; // 24 hours

/// Manages cached Jira field metadata with 24-hour TTL.
pub struct FieldMetadataCache {
    domain: String,
    cache_dir: PathBuf,
    cache_file: PathBuf,
    field_map: Option<HashMap<String, String>>,
    fields_raw: Option<Vec<Value>>,
}

impl FieldMetadataCache {
    pub fn new(domain: &str) -> Self {
        let cache_dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("jarkdown");
        let cache_file = cache_dir.join(format!("fields-{}.json", domain));

        Self {
            domain: domain.to_string(),
            cache_dir,
            cache_file,
            field_map: None,
            fields_raw: None,
        }
    }

    /// Check if cached data is older than TTL.
    pub fn is_stale(&self) -> bool {
        if !self.cache_file.exists() {
            return true;
        }
        match std::fs::read_to_string(&self.cache_file) {
            Ok(content) => {
                let data: Value = match serde_json::from_str(&content) {
                    Ok(v) => v,
                    Err(_) => return true,
                };
                let cached_at = data["cached_at"].as_f64().unwrap_or(0.0);
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64();
                (now - cached_at) > CACHE_TTL_SECONDS as f64
            }
            Err(_) => true,
        }
    }

    /// Save field metadata to cache with timestamp.
    pub fn save(&mut self, fields: &[Value]) {
        std::fs::create_dir_all(&self.cache_dir).ok();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let cache_data = serde_json::json!({
            "cached_at": now,
            "domain": self.domain,
            "fields": fields,
        });
        if let Err(e) = std::fs::write(
            &self.cache_file,
            serde_json::to_string_pretty(&cache_data).unwrap_or_default(),
        ) {
            warn!("Failed to write field cache: {}", e);
        }
        self.field_map = None;
        self.fields_raw = None;
    }

    /// Load field metadata from cache.
    pub fn load(&self) -> Vec<Value> {
        if !self.cache_file.exists() {
            return Vec::new();
        }
        match std::fs::read_to_string(&self.cache_file) {
            Ok(content) => {
                let data: Value = serde_json::from_str(&content).unwrap_or_default();
                data["fields"].as_array().cloned().unwrap_or_default()
            }
            Err(_) => Vec::new(),
        }
    }

    /// Resolve a field ID to its display name.
    pub fn get_field_name(&mut self, field_id: &str) -> String {
        if self.field_map.is_none() {
            let fields = self.load();
            let mut map = HashMap::new();
            for f in &fields {
                if let Some(id) = f["id"].as_str() {
                    let name = f["name"].as_str().unwrap_or(id);
                    map.insert(id.to_string(), name.to_string());
                }
            }
            self.field_map = Some(map);
        }
        self.field_map
            .as_ref()
            .unwrap()
            .get(field_id)
            .cloned()
            .unwrap_or_else(|| field_id.to_string())
    }

    /// Reverse-lookup: find a field ID by its display name (e.g., "Epic Link").
    pub fn get_field_id_by_name(&mut self, name: &str) -> Option<String> {
        if self.field_map.is_none() {
            // Populate the map so we can iterate it
            self.get_field_name("");
        }
        self.field_map
            .as_ref()
            .and_then(|map| {
                map.iter()
                    .find(|(_, v)| v.as_str() == name)
                    .map(|(k, _)| k.clone())
            })
    }

    /// Get the schema for a field by ID.
    pub fn get_field_schema(&self, field_id: &str) -> Value {
        let fields = self.load();
        for f in &fields {
            if f["id"].as_str() == Some(field_id) {
                return f["schema"].clone();
            }
        }
        Value::Null
    }
}
