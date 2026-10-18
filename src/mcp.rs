//! Minimal MCP (Model Context Protocol) client over stdio JSON-RPC.

use crate::config::McpServerConfig;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{oneshot, Mutex as AsyncMutex};

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>;

pub struct McpClient {
    stdin: AsyncMutex<ChildStdin>,
    pending: Pending,
    next: AtomicU64,
    _child: Child,
}

#[derive(Clone)]
pub struct McpTool {
    pub server: String,
    pub name: String,
    pub description: String,
    pub schema: Value,
}

impl McpTool {
    /// Name exposed to the model.
    pub fn qualified(&self) -> String {
        format!("mcp__{}__{}", self.server, self.name)
    }
}

impl McpClient {
    pub async fn spawn(cfg: &McpServerConfig) -> Result<Self> {
        let mut child = Command::new(&cfg.command)
            .args(&cfg.args)
            .envs(&cfg.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawning MCP server `{}`", cfg.command))?;
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let pending: Pending = Default::default();
        let p2 = pending.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                // Only responses (have id and no method) are routed; server requests/notifications are ignored.
                if v.get("method").is_none() {
                    if let Some(id) = v["id"].as_u64() {
                        if let Some(tx) = p2.lock().unwrap().remove(&id) {
                            let _ = tx.send(v);
                        }
                    }
                }
            }
        });
        let c = Self { stdin: AsyncMutex::new(stdin), pending, next: AtomicU64::new(1), _child: child };
        c.request(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "mcodex", "version": env!("CARGO_PKG_VERSION")}}),
        )
        .await?;
        c.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).await?;
        Ok(c)
    }

    async fn send(&self, v: &Value) -> Result<()> {
        let mut s = self.stdin.lock().await;
        s.write_all(format!("{v}\n").as_bytes()).await?;
        s.flush().await?;
        Ok(())
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})).await?;
        let resp = tokio::time::timeout(std::time::Duration::from_secs(60), rx)
            .await
            .map_err(|_| anyhow!("MCP `{method}` timed out"))??;
        if let Some(e) = resp.get("error") {
            bail!("MCP error: {e}");
        }
        Ok(resp["result"].clone())
    }

    pub async fn list_tools(&self, server: &str) -> Result<Vec<McpTool>> {
        let r = self.request("tools/list", json!({})).await?;
        Ok(r["tools"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|t| McpTool {
                server: server.into(),
                name: t["name"].as_str().unwrap_or_default().into(),
                description: t["description"].as_str().unwrap_or_default().into(),
                schema: t.get("inputSchema").cloned().unwrap_or(json!({"type": "object", "properties": {}})),
            })
            .collect())
    }

    pub async fn call(&self, tool: &str, args: Value) -> Result<String> {
        let r = self.request("tools/call", json!({"name": tool, "arguments": args})).await?;
        let text = r["content"]
            .as_array()
            .map(|a| a.iter().filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join("\n"))
            .unwrap_or_else(|| r.to_string());
        Ok(if r["isError"].as_bool().unwrap_or(false) { format!("error: {text}") } else { text })
    }
}

/// All configured MCP servers and their tools.
#[derive(Default)]
pub struct Mcp {
    pub clients: HashMap<String, Arc<McpClient>>,
    pub tools: Vec<McpTool>,
    pub failures: Vec<String>,
}

impl Mcp {
    pub async fn start(servers: &HashMap<String, McpServerConfig>) -> Self {
        let mut m = Mcp::default();
        for (name, cfg) in servers {
            match async {
                let c = McpClient::spawn(cfg).await?;
                let t = c.list_tools(name).await?;
                Ok::<_, anyhow::Error>((c, t))
            }
            .await
            {
                Ok((c, t)) => {
                    m.tools.extend(t);
                    m.clients.insert(name.clone(), Arc::new(c));
                }
                Err(e) => m.failures.push(format!("{name}: {e:#}")),
            }
        }
        m
    }

    pub async fn call(&self, qualified: &str, args: Value) -> Result<String> {
        let t = self.tools.iter().find(|t| t.qualified() == qualified).ok_or_else(|| anyhow!("unknown MCP tool"))?;
        self.clients[&t.server].call(&t.name, args).await
    }
}
