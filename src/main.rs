mod account;
mod agent;
mod api;
mod appserver;
mod attachments;
mod auth;
mod config;
mod desktop;
mod document_ai;
mod document_export;
mod documents;
mod goals;
mod gui;
mod images;
mod mcp;
mod models;
mod patch;
mod plugin;
mod preview;
mod remote;
mod scheduler;
mod session;
mod stream_display;
mod tools;
mod tui;
mod voice;
mod web_search;
mod workspaces;

use agent::{Agent, Event};
use anyhow::Result;
use clap::{Parser, Subcommand};
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc::unbounded_channel, Mutex};
use tools::{Sandbox, ToolCtx};

#[derive(Parser)]
#[command(
    name = "mcodex",
    version,
    about = "Rust re-implementation of the Codex CLI + App"
)]
struct Cli {
    /// Model name (default: config.toml `model`, else gpt-5)
    #[arg(short, long, global = true)]
    model: Option<String>,
    /// Working directory
    #[arg(short = 'C', long, global = true)]
    cd: Option<PathBuf>,
    /// Sandbox for shell commands (Seatbelt on macOS; default workspace-write)
    #[arg(short, long, value_enum, global = true)]
    sandbox: Option<Sandbox>,
    /// Never ask for approval
    #[arg(long, global = true)]
    yolo: bool,
    #[command(subcommand)]
    command: Option<Cmd>,
    /// Initial prompt for the interactive session
    prompt: Option<String>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run non-interactively (auto-approves within the sandbox)
    Exec { prompt: String },
    /// Launch the desktop GUI application (native macOS WebKit window driving app-server)
    App,
    /// Preview the desktop UI with the real Rust backend on loopback
    Preview {
        #[arg(long, default_value_t = 5173)]
        port: u16,
    },
    /// Run the Codex JSON-RPC app-server over stdio
    AppServer {
        /// Transport endpoint (stdio:// supported)
        #[arg(long, default_value = "stdio://")]
        listen: String,
    },
    /// Resume a session (most recent by default)
    Resume { id: Option<String> },
    /// List saved sessions
    Sessions,
    /// Sign in with ChatGPT (browser) or an API key
    Login {
        #[arg(long)]
        with_api_key: Option<String>,
    },
    /// Remove stored credentials
    Logout,
    /// Inspect MCP servers from config.toml
    Mcp {
        #[command(subcommand)]
        sub: ListCmd,
    },
    /// Inspect plugins in ~/.mcodex/plugins
    Plugin {
        #[command(subcommand)]
        sub: ListCmd,
    },
}

#[derive(Subcommand)]
enum ListCmd {
    List,
}

const BASE_INSTRUCTIONS: &str = "You are a coding agent running in a terminal. Be concise. \
Use the `shell` tool to inspect the project and run commands, and `apply_patch` to edit files. \
Keep going until the user's task is fully resolved.";

pub fn build_instructions(
    cfg: &config::Config,
    plugins: &[plugin::Plugin],
    cwd: &std::path::Path,
) -> String {
    let mut s = BASE_INSTRUCTIONS.to_string();
    if let Some(i) = &cfg.instructions {
        s.push_str(&format!("\n\n{i}"));
    }
    if let Ok(global) = std::fs::read_to_string(config::home().join("AGENTS.md")) {
        s.push_str(&format!("\n\n# Global instructions\n{global}"));
    }
    for p in plugins {
        if let Some(i) = &p.instructions {
            s.push_str(&format!("\n\n# Plugin {}\n{i}", p.name));
        }
    }
    if let Ok(a) = std::fs::read_to_string(cwd.join("AGENTS.md")) {
        s.push_str(&format!("\n\n# AGENTS.md\n{a}"));
    }
    s
}

fn ensure_macos_gui_path() {
    let mut paths: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    let mut extras = vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/opt/homebrew/sbin"),
        PathBuf::from("/usr/local/bin"),
    ];
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        extras.push(home.join(".cargo/bin"));
        extras.push(home.join(".local/bin"));
    }
    for extra in extras {
        if extra.is_dir() && !paths.contains(&extra) {
            paths.push(extra);
        }
    }
    if let Ok(joined) = std::env::join_paths(paths) {
        std::env::set_var("PATH", joined);
    }
}

fn is_launched_as_app_bundle() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(|s| s.contains(".app/Contents/MacOS/")))
        .unwrap_or(false)
}

fn main() -> Result<()> {
    ensure_macos_gui_path();
    let filtered_args: Vec<String> = std::env::args()
        .filter(|a| !a.starts_with("-psn_"))
        .collect();
    let cli = Cli::parse_from(filtered_args);
    let cfg = config::Config::load();
    let mut cwd = cli
        .cd
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    if cli.cd.is_none() && cwd == std::path::Path::new("/") {
        if let Some(home) = std::env::var_os("HOME") {
            cwd = PathBuf::from(home);
        }
    }
    let sandbox = cli.sandbox.unwrap_or_else(|| match cfg.sandbox.as_deref() {
        Some("read-only") => Sandbox::ReadOnly,
        Some("danger-full-access") => Sandbox::DangerFullAccess,
        _ => Sandbox::WorkspaceWrite,
    });
    let model = cli
        .model
        .clone()
        .or(cfg.model.clone())
        .unwrap_or_else(|| models::DEFAULT_MODEL.into());

    #[cfg(not(feature = "desktop"))]
    if matches!(cli.command, Some(Cmd::App)) {
        anyhow::bail!("Desktop support is disabled in this headless agent. Use app-server, exec or preview; rebuild with --features desktop to open a native window.");
    }
    let auto_gui = cfg!(feature = "desktop")
        && cli.command.is_none()
        && cli.prompt.is_none()
        && (is_launched_as_app_bundle()
            || (!std::io::stdout().is_terminal() && !std::io::stdin().is_terminal()));

    // macOS requires the NSApplication / tao event loop on the main OS thread.
    if matches!(cli.command, Some(Cmd::App)) || auto_gui {
        let rt = tokio::runtime::Runtime::new()?;
        let mcp = rt.block_on(mcp::Mcp::start(&cfg.mcp_servers));
        let shared = appserver::Shared {
            cfg,
            default_model: model,
            default_sandbox: sandbox,
            default_cwd: cwd,
            yolo: cli.yolo,
            mcp: Arc::new(mcp),
        };
        let result = gui::launch(shared);
        drop(rt);
        return result;
    }

    tokio::runtime::Runtime::new()?.block_on(async_main(cli, cfg, cwd, sandbox, model))
}

async fn async_main(
    cli: Cli,
    cfg: config::Config,
    cwd: PathBuf,
    sandbox: Sandbox,
    model: String,
) -> Result<()> {
    if let Some(Cmd::Preview { port }) = &cli.command {
        let mcp = Arc::new(mcp::Mcp::start(&cfg.mcp_servers).await);
        return preview::serve(
            appserver::Shared {
                cfg,
                default_model: model,
                default_sandbox: sandbox,
                default_cwd: cwd,
                yolo: cli.yolo,
                mcp,
            },
            *port,
        )
        .await;
    }
    match &cli.command {
        Some(Cmd::Login { with_api_key }) => {
            match with_api_key {
                Some(k) => auth::login_with_api_key(k)?,
                None => auth::login().await?,
            }
            return Ok(());
        }
        Some(Cmd::Logout) => {
            println!(
                "{}",
                if auth::logout()? {
                    "Logged out."
                } else {
                    "No stored credentials."
                }
            );
            return Ok(());
        }
        Some(Cmd::Sessions) => {
            for s in session::list() {
                println!("{}  {:>4} items  {}  {}", s.id, s.items, s.cwd, s.preview);
            }
            return Ok(());
        }
        Some(Cmd::Mcp { .. }) => {
            let m = mcp::Mcp::start(&cfg.mcp_servers).await;
            for t in &m.tools {
                println!(
                    "{}  {}",
                    t.qualified(),
                    t.description.lines().next().unwrap_or("")
                );
            }
            for f in &m.failures {
                eprintln!("failed: {f}");
            }
            return Ok(());
        }
        Some(Cmd::Plugin { .. }) => {
            for p in plugin::load_all() {
                println!("{}  ({} tools)  {}", p.name, p.tools.len(), p.dir.display());
            }
            return Ok(());
        }
        Some(Cmd::AppServer { .. }) => {
            let mcp = Arc::new(mcp::Mcp::start(&cfg.mcp_servers).await);
            let shared = appserver::Shared {
                cfg,
                default_model: model,
                default_sandbox: sandbox,
                default_cwd: cwd,
                yolo: cli.yolo,
                mcp,
            };
            return appserver::run(shared).await;
        }
        Some(Cmd::App) => unreachable!(),
        _ => {}
    }

    let plugins = plugin::load_all();
    let mcp = mcp::Mcp::start(&cfg.mcp_servers).await;
    for f in &mcp.failures {
        eprintln!("MCP server failed: {f}");
    }
    let is_exec = matches!(cli.command, Some(Cmd::Exec { .. }));
    let instructions = build_instructions(&cfg, &plugins, &cwd);
    let ctx = ToolCtx {
        cwd: cwd.clone(),
        sandbox,
        auto_approve: cli.yolo || is_exec,
        mcp: Arc::new(mcp),
        plugins,
        session_allow: Default::default(),
    };
    let cwd_s = cwd.display().to_string();

    let (session, history, resumed_view) = match &cli.command {
        Some(Cmd::Resume { id }) => {
            let (s, items, _meta) = session::Session::open(id.as_deref())?;
            let view = agent::render_history(&items);
            (s, items, view)
        }
        _ => (session::Session::create(&cwd_s, &model)?, vec![], vec![]),
    };
    let mut client=api::Client::new(auth::Auth::load()?);client.service_tier=cfg.service_tier.as_deref().and_then(models::normalize_tier);
    let mut agent = Agent {
        web_search: web_search::SearchMode::Disabled,
        client,
        reasoning_effort: cfg.model_reasoning_effort.clone().unwrap_or_else(||models::default_effort(&model)),
        model,
        mode: "codex".into(),
        goal_thread_id: None,
        scheduled_run_id: None,
        instructions,
        ctx,
        history,
        session,
    };

    if let Some(Cmd::Exec { prompt }) = &cli.command {
        let (tx, mut rx) = unbounded_channel();
        let consumer = async {
            let mut display = stream_display::StreamDisplay::default();
            while let Some(e) = rx.recv().await {
                match e {
                    Event::Text(t) => {
                        display.delta(&t);
                        print!("{t}");
                        let _ = std::io::stdout().flush();
                    }
                    Event::ToolStart { name, args, .. } => {
                        eprintln!("\n\x1b[2m▶ {name} {args}\x1b[0m")
                    }
                    Event::ReasoningStart { .. }
                    | Event::ReasoningDelta { .. }
                    | Event::ReasoningEnd { .. } => {}
                    Event::AssistantMessage(value) => {
                        match display.message(&value) {
                            Some(stream_display::Update::Append(text)) => print!("{text}"),
                            Some(stream_display::Update::Replace(text)) => {
                                print!("\nFinal answer updated:\n{text}")
                            }
                            None => {}
                        }
                        let _ = std::io::stdout().flush();
                    }
                    Event::Usage { .. }
                    | Event::GoalState(_)
                    | Event::TaskReport(_)
                    | Event::WebSearch(_) => {}
                    Event::ToolEnd { .. } | Event::Approval { .. } | Event::Done => {}
                    Event::Error(e) => eprintln!("error: {e}"),
                }
            }
        };
        tokio::join!(
            async {
                agent.turn(prompt, &tx).await;
                drop(tx);
            },
            consumer
        );
        println!();
        return Ok(());
    }

    if !std::io::stdout().is_terminal() {
        anyhow::bail!("interactive mode needs a terminal; use `mcodex exec \"...\"`");
    }
    tui::run(Arc::new(Mutex::new(agent)), cwd_s, resumed_view, cli.prompt).await
}
