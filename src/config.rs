//! Config (`~/.mcodex/config.toml`) and shared paths.

use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("MCODEX_HOME").unwrap_or_else(|_| {
        format!("{}/.mcodex", std::env::var("HOME").unwrap_or_else(|_| ".".into()))
    }))
}

#[derive(Debug, Default, Deserialize, Clone)]
pub struct McpServerConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Config {
    pub model: Option<String>,
    pub sandbox: Option<String>,
    pub instructions: Option<String>,
    #[serde(default)]
    pub mcp_servers: HashMap<String, McpServerConfig>,
}

impl Config {
    pub fn load() -> Self {
        std::fs::read_to_string(home().join("config.toml"))
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }
}
