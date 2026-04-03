//! Configuration manager for jarkdown field selection.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use log::{debug, warn};

/// Field filter settings for custom field include/exclude.
#[derive(Debug, Clone, Default)]
pub struct FieldFilter {
    /// If Some, only include these fields. If None, include all.
    pub include: Option<HashSet<String>>,
    /// Fields to exclude (always a set, may be empty).
    pub exclude: HashSet<String>,
}

/// Manages field selection configuration from TOML file and CLI args.
pub struct ConfigManager {
    config_dir: PathBuf,
}

impl ConfigManager {
    const CONFIG_FILENAME: &'static str = ".jarkdown.toml";

    pub fn new(config_dir: Option<&Path>) -> Self {
        Self {
            config_dir: config_dir
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))),
        }
    }

    fn config_path(&self) -> PathBuf {
        self.config_dir.join(Self::CONFIG_FILENAME)
    }

    fn load_config(&self) -> toml::Table {
        let path = self.config_path();
        if !path.exists() {
            return toml::Table::new();
        }
        match std::fs::read_to_string(&path) {
            Ok(content) => match content.parse::<toml::Table>() {
                Ok(table) => {
                    debug!("Loaded config from {:?}", path);
                    table
                }
                Err(e) => {
                    warn!("Error parsing config file {:?}: {}", path, e);
                    toml::Table::new()
                }
            },
            Err(e) => {
                warn!("Error reading config file {:?}: {}", path, e);
                toml::Table::new()
            }
        }
    }

    /// Get resolved field include/exclude sets.
    /// CLI args override config file settings.
    pub fn get_field_filter(
        &self,
        cli_include: Option<&str>,
        cli_exclude: Option<&str>,
    ) -> FieldFilter {
        let config = self.load_config();
        let fields_config = config
            .get("fields")
            .and_then(|v| v.as_table())
            .cloned()
            .unwrap_or_default();

        // Determine include list
        let include = if let Some(cli_inc) = cli_include {
            Some(
                cli_inc
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect(),
            )
        } else if let Some(inc_arr) = fields_config.get("include").and_then(|v| v.as_array()) {
            Some(
                inc_arr
                    .iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect(),
            )
        } else {
            None
        };

        // Determine exclude list
        let exclude = if let Some(cli_exc) = cli_exclude {
            cli_exc
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        } else if let Some(exc_arr) = fields_config.get("exclude").and_then(|v| v.as_array()) {
            exc_arr
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        } else {
            HashSet::new()
        };

        FieldFilter { include, exclude }
    }
}
