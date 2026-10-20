//! Config (`~/.mcodex/config.toml`) and shared paths.

use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("MCODEX_HOME").unwrap_or_else(|_| {
        format!(
            "{}/.mcodex",
            std::env::var("HOME").unwrap_or_else(|_| ".".into())
        )
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
    pub model_reasoning_effort: Option<String>,
    pub service_tier: Option<String>,
    pub model: Option<String>,
    #[serde(alias = "sandbox_mode")]
    pub sandbox: Option<String>,
    pub instructions: Option<String>,
    #[serde(default)]
    pub mcp_servers: HashMap<String, McpServerConfig>,
}

impl Config {
    pub fn load() -> Self {
        read_table().try_into().unwrap_or_default()
    }
}

// Only model preferences are inherited. Permissions, credentials and tools stay app-owned.
pub fn codex_home() -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".codex")
        })
}
pub fn codex_import_enabled() -> bool {
    std::env::var("MCODEX_NO_CODEX_IMPORT").as_deref() != Ok("1")
}
pub fn read_table() -> toml::Table {
    read_table_at(
        &home().join("config.toml"),
        &codex_home().join("config.toml"),
        codex_import_enabled(),
    )
}
fn read_table_at(local: &std::path::Path, codex: &std::path::Path, import: bool) -> toml::Table {
    let mut defaults = toml::Table::new();
    if import {
        if let Some(mut source) = std::fs::read_to_string(codex)
            .ok()
            .and_then(|text| text.parse::<toml::Table>().ok())
        {
            if let Some(profile) = source
                .get("profile")
                .and_then(|value| value.as_str())
                .and_then(|name| source.get("profiles")?.get(name)?.as_table())
                .cloned()
            {
                source.extend(profile);
            }
            for key in ["model", "model_reasoning_effort", "service_tier"] {
                if let Some(value) = source.get(key).filter(|value| value.is_str()) {
                    defaults.insert(key.into(), value.clone());
                }
            }
        }
    }
    if let Some(local) = std::fs::read_to_string(local)
        .ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
    {
        defaults.extend(local);
    }
    defaults
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn imports_only_model_defaults_and_local_settings_win_without_writing_codex() {
        let root = std::env::temp_dir().join(format!(
            "mcodex-config-test-{:032x}",
            rand::random::<u128>()
        ));
        std::fs::create_dir(&root).unwrap();
        let local = root.join("local.toml");
        let codex = root.join("codex.toml");
        let original="model='base'\nmodel_reasoning_effort='high'\nservice_tier='priority'\nsandbox_mode='danger-full-access'\napproval_policy='never'\nprofile='focus'\n[profiles.focus]\nmodel='gpt-6.1-sol'\nmodel_reasoning_effort='ultra'\n[mcp_servers.private]\ncommand='never-import'\n";
        std::fs::write(&codex, original).unwrap();
        let inherited = read_table_at(&local, &codex, true);
        assert_eq!(inherited["model"].as_str(), Some("gpt-6.1-sol"));
        assert_eq!(inherited["model_reasoning_effort"].as_str(), Some("ultra"));
        assert_eq!(inherited.len(), 3);
        std::fs::write(
            &local,
            "model='custom'\nmodel_reasoning_effort='low'\nsandbox_mode='read-only'\n",
        )
        .unwrap();
        let overridden = read_table_at(&local, &codex, true);
        assert_eq!(overridden["model"].as_str(), Some("custom"));
        assert_eq!(overridden["sandbox_mode"].as_str(), Some("read-only"));
        assert_eq!(std::fs::read_to_string(&codex).unwrap(), original);
        assert!(read_table_at(&root.join("absent"), &codex, false).is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}
