//! Plugins: directories under `~/.mcodex/plugins/<name>/plugin.toml` that add instructions and tools.
//!
//! ```toml
//! name = "hello"
//! instructions = "Use hello_world when greeted."
//! [[tools]]
//! name = "hello_world"
//! description = "Greets someone"
//! command = "./hello.sh"          # run in the plugin dir; JSON args on stdin; stdout is the result
//! parameters = { type = "object", properties = { who = { type = "string" } } }
//! ```

use crate::config::home;
use anyhow::{bail, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

#[derive(Deserialize, Clone)]
pub struct PluginTool {
    pub name: String,
    pub description: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub parameters: Option<Value>,
}

#[derive(Deserialize, Clone)]
pub struct Plugin {
    pub name: String,
    pub instructions: Option<String>,
    #[serde(default)]
    pub tools: Vec<PluginTool>,
    #[serde(skip)]
    pub dir: PathBuf,
}

pub fn load_all() -> Vec<Plugin> {
    let Ok(rd) = std::fs::read_dir(home().join("plugins")) else { return vec![] };
    rd.flatten()
        .filter_map(|e| {
            let dir = e.path();
            let mut p: Plugin = toml::from_str(&std::fs::read_to_string(dir.join("plugin.toml")).ok()?).ok()?;
            p.dir = dir;
            Some(p)
        })
        .collect()
}

pub fn qualified(p: &Plugin, t: &PluginTool) -> String {
    format!("plugin__{}__{}", p.name, t.name)
}

pub fn definitions(plugins: &[Plugin]) -> Vec<Value> {
    plugins
        .iter()
        .flat_map(|p| {
            p.tools.iter().map(move |t| {
                json!({"type": "function", "name": qualified(p, t), "description": t.description,
                    "parameters": t.parameters.clone().unwrap_or(json!({"type": "object", "properties": {}}))})
            })
        })
        .collect()
}

pub async fn call(plugins: &[Plugin], qualified_name: &str, args: &str) -> Result<String> {
    let Some((p, t)) = plugins.iter().find_map(|p| p.tools.iter().find(|t| qualified(p, t) == qualified_name).map(|t| (p, t))) else {
        bail!("unknown plugin tool")
    };
    let mut child = Command::new(&t.command)
        .args(&t.args)
        .current_dir(&p.dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    child.stdin.take().unwrap().write_all(args.as_bytes()).await?;
    let out = tokio::time::timeout(std::time::Duration::from_secs(60), child.wait_with_output()).await??;
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok(s)
}
