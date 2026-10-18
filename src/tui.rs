//! Full-screen terminal UI (ratatui + crossterm).

use crate::agent::{Agent, Event, Verdict};
use anyhow::Result;
use crossterm::event::{Event as CEvent, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;
use std::sync::Arc;
use tokio::sync::{mpsc::unbounded_channel, oneshot, Mutex};
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    User,
    Assistant,
    Tool,
    Output,
    Error,
    Info,
}

struct State {
    lines: Vec<(Kind, String)>,
    input: String,
    busy: bool,
    assist_open: bool,
    scroll_back: usize,
    approval: Option<(String, oneshot::Sender<Verdict>)>,
    header: String,
}

impl State {
    fn push(&mut self, k: Kind, s: impl Into<String>) {
        self.lines.push((k, s.into()));
        self.assist_open = false;
        self.scroll_back = 0;
    }
}

fn style(k: Kind) -> (Style, &'static str) {
    match k {
        Kind::User => (Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD), "› "),
        Kind::Assistant => (Style::default(), "• "),
        Kind::Tool => (Style::default().fg(Color::Yellow), "▶ "),
        Kind::Output => (Style::default().add_modifier(Modifier::DIM), "  "),
        Kind::Error => (Style::default().fg(Color::Red), "✗ "),
        Kind::Info => (Style::default().fg(Color::DarkGray), ""),
    }
}

fn ui(f: &mut Frame, s: &State) {
    let bottom_h = if s.approval.is_some() { 6 } else { 3 };
    let [chat, bottom] = Layout::vertical([Constraint::Min(1), Constraint::Length(bottom_h)]).areas(f.area());

    let mut lines: Vec<Line> = Vec::new();
    for (k, text) in &s.lines {
        let (st, prefix) = style(*k);
        for (i, l) in text.split('\n').enumerate() {
            let p = if i == 0 { prefix } else { "  " };
            lines.push(Line::from(vec![Span::styled(p, st), Span::styled(l.to_string(), st)]));
        }
    }
    let inner = chat.width.saturating_sub(2).max(1) as usize;
    let total: usize = lines
        .iter()
        .map(|l| {
            let w: usize = l.spans.iter().map(|sp| sp.content.width()).sum();
            w.div_ceil(inner).max(1)
        })
        .sum();
    let view = chat.height.saturating_sub(2) as usize;
    let max_back = total.saturating_sub(view);
    let y = max_back.saturating_sub(s.scroll_back.min(max_back));
    let title = format!(" {} ", s.header);
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((y as u16, 0))
            .block(Block::default().borders(Borders::ALL).title(title)),
        chat,
    );

    if let Some((what, _)) = &s.approval {
        f.render_widget(
            Paragraph::new(vec![Line::from(what.clone()), Line::from(""), Line::from("[y] approve   [a] approve for session   [n] reject")])
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL).title(" Approve? ").border_style(Style::default().fg(Color::Yellow))),
            bottom,
        );
    } else {
        let title = if s.busy { " working… (Ctrl-C quit) " } else { " message (/new /exit /help) " };
        f.render_widget(Paragraph::new(s.input.clone()).block(Block::default().borders(Borders::ALL).title(title)), bottom);
        let x = bottom.x + 1 + s.input.width() as u16;
        f.set_cursor_position((x.min(bottom.x + bottom.width.saturating_sub(2)), bottom.y + 1));
    }
}

pub async fn run(agent: Arc<Mutex<Agent>>, cwd: String, history: Vec<(String, String)>, first: Option<String>) -> Result<()> {
    let header = {
        let a = agent.lock().await;
        format!("mcodex · {} · {} · {}", a.model, a.client.auth.describe(), cwd)
    };
    let mut s = State { lines: vec![], input: String::new(), busy: false, assist_open: false, scroll_back: 0, approval: None, header };
    for (role, text) in history {
        let k = match role.as_str() {
            "user" => Kind::User,
            "tool" => Kind::Tool,
            _ => Kind::Assistant,
        };
        s.push(k, text);
    }
    s.push(Kind::Info, "Ready. Type a task and press Enter.");

    let (tx, mut rx) = unbounded_channel::<Event>();
    let spawn_turn = |text: String, s: &mut State| {
        s.push(Kind::User, text.clone());
        s.busy = true;
        let (a, tx) = (agent.clone(), tx.clone());
        tokio::spawn(async move { a.lock().await.turn(&text, &tx).await });
    };
    if let Some(p) = first {
        spawn_turn(p, &mut s);
    }

    let mut term = ratatui::init();
    let mut keys = EventStream::new();
    let result: Result<()> = loop {
        if let Err(e) = term.draw(|f| ui(f, &s)) {
            break Err(e.into());
        }
        tokio::select! {
            Some(ev) = rx.recv() => match ev {
                Event::Text(t) => {
                    if s.assist_open {
                        if let Some((_, l)) = s.lines.last_mut() { l.push_str(&t); }
                    } else {
                        s.lines.push((Kind::Assistant, t));
                        s.assist_open = true;
                    }
                }
                Event::ReasoningEnd { text, .. } => {
                    if !text.trim().is_empty() {
                        s.push(Kind::Info, format!("💭 {}", text.lines().next().unwrap_or("Thought")));
                    }
                }
                Event::ReasoningStart { .. } | Event::ReasoningDelta { .. } => {}
                Event::ToolStart { name, args, .. } => s.push(Kind::Tool, format!("{name} {args}")),
                Event::ToolEnd { output, .. } => {
                    let short: String = output.lines().take(12).collect::<Vec<_>>().join("\n");
                    s.push(Kind::Output, short);
                }
                Event::Approval { what, reply, .. } => s.approval = Some((what, reply)),
                Event::Done => { s.busy = false; s.assist_open = false; }
                Event::Error(e) => { s.push(Kind::Error, e); s.busy = false; }
            },
            Some(Ok(ev)) = keys.next() => {
                let CEvent::Key(k) = ev else { continue };
                if k.kind != KeyEventKind::Press { continue; }
                if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') { break Ok(()); }
                if s.approval.is_some() {
                    let yes = match k.code {
                        KeyCode::Char('y') | KeyCode::Enter => Some(Verdict::Yes),
                        KeyCode::Char('a') => Some(Verdict::Always),
                        KeyCode::Char('n') | KeyCode::Esc => Some(Verdict::No),
                        _ => None,
                    };
                    if let (Some(v), Some((_, reply))) = (yes, s.approval.take_if(|_| yes.is_some())) { let _ = reply.send(v); }
                    continue;
                }
                match k.code {
                    KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => s.input.clear(),
                    KeyCode::Char(c) => s.input.push(c),
                    KeyCode::Backspace => { s.input.pop(); }
                    KeyCode::PageUp => s.scroll_back += 10,
                    KeyCode::PageDown => s.scroll_back = s.scroll_back.saturating_sub(10),
                    KeyCode::Enter if !s.busy => {
                        let line = std::mem::take(&mut s.input);
                        let line = line.trim().to_string();
                        match line.as_str() {
                            "" => {}
                            "/exit" | "/quit" => break Ok(()),
                            "/help" => s.push(Kind::Info, "/new  start a fresh session\n/exit quit\nPgUp/PgDn scroll"),
                            "/new" => {
                                match agent.lock().await.reset(&cwd) {
                                    Ok(()) => { s.lines.clear(); s.push(Kind::Info, "New session started."); }
                                    Err(e) => s.push(Kind::Error, e.to_string()),
                                }
                            }
                            _ => spawn_turn(line, &mut s),
                        }
                    }
                    _ => {}
                }
            }
        }
    };
    ratatui::restore();
    result
}
