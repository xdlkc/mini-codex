//! User-operated PTY sessions. Shell input is independent of model execution.
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs::File,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::sync::{mpsc::UnboundedSender, watch};
type Result<T> = std::result::Result<T, String>;
const MAX_OUTPUT: usize = 1024 * 1024;

#[derive(Clone)]
pub struct Terminals {
    out: UnboundedSender<Value>,
    home: PathBuf,
    sessions: Arc<Mutex<HashMap<String, Arc<Session>>>>,
    next_order: Arc<std::sync::atomic::AtomicU64>,
    #[cfg(test)]
    shell: Option<PathBuf>,
}
struct Session {
    state: Mutex<Value>,
    stop: watch::Sender<bool>,
    finished: watch::Sender<bool>,
    acknowledged: watch::Sender<u64>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    #[cfg(unix)]
    fd: std::sync::Weak<tokio::io::unix::AsyncFd<File>>,
    input: tokio::sync::Mutex<()>,
}
impl Terminals {
    pub fn new(out: UnboundedSender<Value>) -> Self {
        Self {
            out,
            home: crate::config::home(),
            sessions: Default::default(),
            next_order: Default::default(),
            #[cfg(test)]
            shell: None,
        }
    }
    fn event(&self, method: &str, state: &Value) {
        let _ = self
            .out
            .send(json!({"method":method,"params":{"terminal":state}}));
    }
    pub async fn handle(&self, method: &str, params: &Value) -> Option<Result<Value>> {
        if !method.starts_with("terminal/") {
            return None;
        }
        Some(self.request(method, params).await)
    }
    async fn request(&self, method: &str, params: &Value) -> Result<Value> {
        if method == "terminal/create" {
            return self.create(params).await;
        }
        if method == "terminal/list" {
            let thread = params.get("threadId");
            let cwd = params["cwd"]
                .as_str()
                .map(|s| Path::new(s).canonicalize().map_err(|e| e.to_string()))
                .transpose()?;
            let mut data = self
                .sessions
                .lock()
                .unwrap()
                .values()
                .filter_map(|session| {
                    let state = session.state.lock().unwrap();
                    if thread.is_some_and(|thread| state["threadId"] != *thread)
                        || cwd
                            .as_ref()
                            .is_some_and(|cwd| state["cwd"].as_str() != cwd.to_str())
                    {
                        return None;
                    }
                    Some(state.clone())
                })
                .collect::<Vec<_>>();
            data.sort_by_key(|state| state["order"].as_u64().unwrap_or(0));
            return Ok(json!({"data":data}));
        }
        if !matches!(
            method,
            "terminal/status"
                | "terminal/write"
                | "terminal/resize"
                | "terminal/close"
                | "terminal/ack"
        ) {
            return Err(format!("Unknown terminal method: {method}"));
        }
        let id = params["id"].as_str().ok_or("A terminal ID is required")?;
        let session = self.sessions.lock().unwrap().get(id).cloned();
        let Some(session) = session else {
            return if method == "terminal/close" {
                Ok(json!({"terminal":null}))
            } else {
                Err("Terminal no longer exists".into())
            };
        };
        match method {
            "terminal/ack" => {
                let sequence = params["sequence"]
                    .as_u64()
                    .ok_or("Invalid terminal output acknowledgement")?;
                let sequence = sequence.min(
                    session.state.lock().unwrap()["sequence"]
                        .as_u64()
                        .unwrap_or(0),
                );
                session.acknowledged.send_if_modified(|previous| {
                    if sequence > *previous {
                        *previous = sequence;
                        true
                    } else {
                        false
                    }
                });
            }

            "terminal/write" => {
                let data = params["data"]
                    .as_str()
                    .ok_or("Terminal input is required")?;
                if data.len() > MAX_OUTPUT {
                    return Err("Terminal input exceeds 1 MiB".into());
                }
                if session.state.lock().unwrap()["status"] != "running" {
                    return Err("Terminal has exited".into());
                }
                #[cfg(unix)]
                write_session(&session, data.as_bytes()).await?;
            }
            "terminal/resize" => {
                let (cols, rows) = dimensions(params)?;
                #[cfg(unix)]
                resize(
                    session.fd.upgrade().ok_or("Terminal has exited")?.as_ref(),
                    cols,
                    rows,
                )?;
                let mut state = session.state.lock().unwrap();
                state["cols"] = json!(cols);
                state["rows"] = json!(rows);
            }
            "terminal/close" => {
                stop_session(&session).await;
            }
            _ => {}
        }
        if matches!(
            method,
            "terminal/write" | "terminal/resize" | "terminal/ack"
        ) {
            return Ok(json!({"ok":true}));
        }
        let state = session.state.lock().unwrap().clone();
        if method == "terminal/close" {
            self.sessions.lock().unwrap().remove(id);
        }
        Ok(json!({"terminal":state}))
    }
    #[cfg(not(unix))]
    async fn create(&self, _: &Value) -> Result<Value> {
        Err("Interactive terminals are not available on this host".into())
    }
    #[cfg(unix)]
    async fn create(&self, params: &Value) -> Result<Value> {
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::os::unix::process::CommandExt;
        use std::process::Stdio;
        let cwd = PathBuf::from(params["cwd"].as_str().ok_or("Terminal cwd is required")?);
        if !cwd.is_absolute() {
            return Err("Terminal cwd must be absolute".into());
        }
        let cwd = cwd.canonicalize().map_err(|e| e.to_string())?;
        if !cwd.is_dir() {
            return Err("Terminal cwd must be a directory".into());
        }
        let (cols, rows) = dimensions(params)?;
        let command_text = params["command"].as_str().unwrap_or("");
        if command_text.len() > MAX_OUTPUT || command_text.contains('\0') {
            return Err("Invalid terminal command".into());
        }
        let lease = crate::workspaces::environment_lease(&cwd, &self.home, false).await?;
        let (session, state, mut child, group, mut stopped) = {
            let mut sessions = self.sessions.lock().unwrap();
            if sessions
                .values()
                .filter(|s| s.state.lock().unwrap()["status"] == "running")
                .count()
                >= 32
            {
                return Err("Close a terminal before opening another one".into());
            }
            let mut master = -1;
            let mut slave = -1;
            let mut size = libc::winsize {
                ws_row: rows,
                ws_col: cols,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            if unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut size,
                )
            } < 0
            {
                return Err(std::io::Error::last_os_error().to_string());
            }
            let master = unsafe { File::from_raw_fd(master) };
            let slave = unsafe { File::from_raw_fd(slave) };
            let descriptor = master.as_raw_fd();
            unsafe {
                if libc::fcntl(descriptor, libc::F_SETFL, libc::O_NONBLOCK) < 0
                    || libc::fcntl(descriptor, libc::F_SETFD, libc::FD_CLOEXEC) < 0
                    || libc::fcntl(slave.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) < 0
                {
                    return Err(std::io::Error::last_os_error().to_string());
                }
            }
            let shell = std::env::var_os("SHELL")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute() && path.is_file())
                .unwrap_or_else(|| PathBuf::from("/bin/bash"));
            #[cfg(test)]
            let shell = self.shell.clone().unwrap_or(shell);
            let mut command = tokio::process::Command::new(shell);
            #[cfg(test)]
            if self.shell.is_some() {
                command.args(["--noprofile", "--norc", "-i"]);
            } else {
                command.args(["-il"]);
            }
            #[cfg(not(test))]
            command.args(["-il"]);
            command
                .current_dir(&cwd)
                .env("TERM", "xterm-256color")
                .env("COLORTERM", "truecolor")
                .stdin(Stdio::from(slave.try_clone().map_err(|e| e.to_string())?))
                .stdout(Stdio::from(slave.try_clone().map_err(|e| e.to_string())?))
                .stderr(Stdio::from(slave))
                .kill_on_drop(true);
            crate::local_environments::apply_shell_environment(&mut command, &cwd);
            unsafe {
                command.as_std_mut().pre_exec(|| {
                    if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as libc::c_ulong, 0) < 0
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let child = command.spawn().map_err(|e| e.to_string())?;
            drop(command);
            let pid = child.id().ok_or("Terminal did not start")?;
            let fd = Arc::new(
                unsafe { tokio::io::unix::AsyncFd::register(master) }.map_err(|e| e.to_string())?,
            );
            let group = ProcessGroup {
                pid,
                fd: fd.clone(),
            };
            let id = format!("pty_{:032x}", rand::random::<u128>());
            let (stop, stopped) = watch::channel(false);
            let (finished, _) = watch::channel(false);
            let (acknowledged, _) = watch::channel(0);
            let fixed_title = params["title"].as_str().filter(|t| !t.is_empty());
            let state = json!({"id":id,"cwd":cwd,"threadId":params["threadId"],"title":fixed_title.unwrap_or("Terminal"),"fixedTitle":fixed_title,"status":"running","cols":cols,"rows":rows,"sequence":0,"output":"","exitCode":null,"order":self.next_order.fetch_add(1,std::sync::atomic::Ordering::Relaxed)});
            let session = Arc::new(Session {
                state: Mutex::new(state.clone()),
                stop,
                finished,
                acknowledged,
                task: Default::default(),
                fd: Arc::downgrade(&fd),
                input: Default::default(),
            });
            sessions.insert(id.clone(), session.clone());
            (session, state, child, group, stopped)
        };
        self.event("terminal/updated", &state);
        let owner = self.clone();
        let owned = session.clone();
        let task = tokio::spawn(async move {
            let reader_owner = owner.clone();
            let reader_session = owned.clone();
            let mut reader =
                tokio::spawn(async move { reader_owner.output(&reader_session).await });
            let status = tokio::select! { result=child.wait()=>result.map_err(|e|e.to_string()), _=stopped.changed()=>{
                group.hangup();
                let _=tokio::time::timeout(std::time::Duration::from_millis(400),child.wait()).await;
                Err("closed".into())
            } };
            // Closing must release the master before reaping. On macOS an
            // exiting session leader can wait for PTY output to drain, while
            // its reader is paused for the renderer's acknowledgement.
            if tokio::time::timeout(std::time::Duration::from_millis(100), &mut reader)
                .await
                .is_err()
            {
                reader.abort();
                let _ = reader.await;
            }
            drop(group);
            // Stop the owned child after signaling its process groups, then
            // reap it before releasing the checkout lease.
            let _ = child.start_kill();
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) => tokio::time::sleep(std::time::Duration::from_millis(5)).await,
                    Err(_) => {
                        let _ = child.wait().await;
                        break;
                    }
                }
            }
            let mut state = owned.state.lock().unwrap();
            match status {
                Ok(status) => {
                    state["status"] = json!("exited");
                    state["exitCode"] = json!(status.code());
                }
                Err(error) => {
                    state["status"] = json!("closed");
                    if error != "closed" {
                        state["error"] = json!(error);
                    }
                }
            }
            drop(lease);
            owner.event("terminal/updated", &state);
            owned.finished.send_replace(true);
        });
        *session.task.lock().unwrap() = Some(task);
        if !command_text.is_empty() {
            let input = format!("{command_text}\r");
            if let Err(error) = write_session(&session, input.as_bytes()).await {
                let _ = session.stop.send(true);
                return Err(error);
            }
        }
        let state = session.state.lock().unwrap().clone();
        Ok(json!({"terminal":state}))
    }
    #[cfg(unix)]
    async fn output(&self, session: &Session) -> Result<()> {
        use std::io::Read;
        let fd = session.fd.upgrade().ok_or("Terminal has exited")?;
        let mut buffer = [0; 8192];
        let mut pending = Vec::new();
        let mut acknowledged = session.acknowledged.subscribe();
        loop {
            let count = loop {
                let mut ready = fd.readable().await.map_err(|e| e.to_string())?;
                match ready.try_io(|fd| {
                    let mut file = fd.get_ref();
                    file.read(&mut buffer)
                }) {
                    Ok(Ok(n)) => break n,
                    Ok(Err(error)) if error.raw_os_error() == Some(libc::EIO) => break 0,
                    Ok(Err(error)) => return Err(error.to_string()),
                    Err(_) => continue,
                }
            };
            let delta = crate::local_environments::decode_output(
                &mut pending,
                &buffer[..count],
                count == 0,
            );
            if !delta.is_empty() {
                let (sequence, delivered) = {
                    let mut state = session.state.lock().unwrap();
                    let mut output = state["output"].as_str().unwrap_or("").to_string();
                    output.push_str(&delta);
                    if output.len() > MAX_OUTPUT {
                        let mut from = output.len() - MAX_OUTPUT;
                        while !output.is_char_boundary(from) {
                            from += 1;
                        }
                        output = output[from..].to_string();
                    }
                    let sequence = state["sequence"].as_u64().unwrap_or(0) + 1;
                    state["sequence"] = json!(sequence);
                    state["output"] = json!(output);
                    let delivered=self.out.send(json!({"method":"terminal/output","params":{"id":state["id"],"threadId":state["threadId"],"sequence":sequence,"delta":delta}})).is_ok();
                    (sequence, delivered)
                };
                if delivered {
                    loop {
                        let waiting =
                            sequence.saturating_sub(*acknowledged.borrow_and_update()) >= 8;
                        if !waiting {
                            break;
                        }
                        if acknowledged.changed().await.is_err() {
                            break;
                        }
                    }
                }
            }
            if count == 0 {
                return Ok(());
            }
        }
    }
    pub async fn shutdown(&self) {
        let sessions = self
            .sessions
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for session in &sessions {
            session.stop.send_replace(true);
            session.acknowledged.send_replace(u64::MAX);
        }
        for session in sessions {
            stop_session(&session).await;
        }
    }
}
async fn stop_session(session: &Session) {
    let mut finished = session.finished.subscribe();
    session.stop.send_replace(true);
    session.acknowledged.send_replace(u64::MAX);
    let task = session.task.lock().unwrap().take();
    if let Some(task) = task {
        if let Err(error) = task.await {
            if let Ok(mut state) = session.state.lock() {
                state["status"] = json!("closed");
                state["error"] = json!(error.to_string());
            }
            session.finished.send_replace(true);
        }
    }
    if !*finished.borrow() {
        let _ = finished.wait_for(|done| *done).await;
    }
}
fn dimensions(params: &Value) -> Result<(u16, u16)> {
    let get = |name, default| {
        let value = params
            .get(name)
            .map_or(Some(default), Value::as_u64)
            .ok_or("Invalid terminal dimensions")?;
        if !(2..=1000).contains(&value) {
            return Err("Terminal dimensions must be between 2 and 1000".to_string());
        }
        Ok(value as u16)
    };
    Ok((get("cols", 80)?, get("rows", 24)?))
}
#[cfg(unix)]
fn resize(fd: &tokio::io::unix::AsyncFd<File>, cols: u16, rows: u16) -> Result<()> {
    use std::os::fd::AsRawFd;
    let size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    if unsafe {
        libc::ioctl(
            fd.get_ref().as_raw_fd(),
            libc::TIOCSWINSZ as libc::c_ulong,
            &size,
        )
    } < 0
    {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(())
}
#[cfg(unix)]
async fn write_session(session: &Session, bytes: &[u8]) -> Result<()> {
    let mut stop = session.stop.subscribe();
    if *stop.borrow() {
        return Err("Terminal has exited".into());
    }
    tokio::select! { result=async {let _input=session.input.lock().await;let fd=session.fd.upgrade().ok_or("Terminal has exited")?;write(&fd,bytes).await}=>result, _=stop.changed()=>Err("Terminal has exited".into()) }
}
#[cfg(unix)]
async fn write(fd: &tokio::io::unix::AsyncFd<File>, mut bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    while !bytes.is_empty() {
        let mut ready = fd.writable().await.map_err(|e| e.to_string())?;
        match ready.try_io(|fd| {
            let mut file = fd.get_ref();
            file.write(bytes)
        }) {
            Ok(Ok(0)) => return Err("Terminal input closed".into()),
            Ok(Ok(size)) => bytes = &bytes[size..],
            Ok(Err(error)) => return Err(error.to_string()),
            Err(_) => continue,
        }
    }
    Ok(())
}
#[cfg(unix)]
struct ProcessGroup {
    pid: u32,
    fd: Arc<tokio::io::unix::AsyncFd<File>>,
}
#[cfg(unix)]
impl ProcessGroup {
    fn hangup(&self) {
        // Let an interactive shell relay SIGHUP to its job-control groups.
        unsafe {
            libc::kill(self.pid as i32, libc::SIGHUP);
        }
    }
}
#[cfg(unix)]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        unsafe {
            use std::os::fd::AsRawFd;
            let foreground = libc::tcgetpgrp(self.fd.get_ref().as_raw_fd());
            if foreground > 0 {
                libc::kill(-foreground, libc::SIGKILL);
            }
            libc::kill(-(self.pid as i32), libc::SIGKILL);
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{fs, process::Command, time::Duration};
    struct Fixture {
        root: PathBuf,
        cwd: PathBuf,
        terminals: Terminals,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir()
                .join(format!("mcodex-pty-test-{:032x}", rand::random::<u128>()));
            fs::create_dir_all(root.join("repo")).unwrap();
            let root = root.canonicalize().unwrap();
            let cwd = root.join("repo");
            let output = Command::new("git")
                .args(["init", "-q"])
                .current_dir(&cwd)
                .output()
                .unwrap();
            assert!(output.status.success());
            fs::write(
                cwd.join(".git/codex-shell-environment.json"),
                r#"{"version":1,"set":{"PS1":"__MC_PTY_READY__ "},"exclude":["PROMPT_COMMAND"]}"#,
            )
            .unwrap();
            let (out, _) = tokio::sync::mpsc::unbounded_channel();
            let mut terminals = Terminals::new(out);
            terminals.home = root.join("home");
            terminals.shell = Some("/bin/bash".into());
            Self {
                root,
                cwd,
                terminals,
            }
        }
        async fn create(&self, command: &str) -> Value {
            self.terminals
                .request(
                    "terminal/create",
                    &json!({"cwd":self.cwd,"threadId":"chat-a","command":command}),
                )
                .await
                .unwrap()["terminal"]
                .clone()
        }
        async fn input(&self, id: &str, data: &str) {
            self.terminals
                .request("terminal/write", &json!({"id":id,"data":data}))
                .await
                .unwrap();
        }
        async fn interrupt(&self, id: &str) {
            self.output(id, "__MC_PTY_READY__ ").await;
            let before = self
                .terminals
                .request("terminal/status", &json!({"id":id}))
                .await
                .unwrap()["terminal"]["output"]
                .as_str()
                .unwrap()
                .matches("__MC_PTY_READY__ ")
                .count();
            self.input(id, "\u{3}").await;
            tokio::time::timeout(Duration::from_secs(8), async {
                loop {
                    let state = self
                        .terminals
                        .request("terminal/status", &json!({"id":id}))
                        .await
                        .unwrap();
                    if state["terminal"]["output"]
                        .as_str()
                        .unwrap()
                        .matches("__MC_PTY_READY__ ")
                        .count()
                        > before
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("shell did not return to its prompt after Ctrl+C");
        }
        async fn output(&self, id: &str, needle: &str) -> Value {
            tokio::time::timeout(Duration::from_secs(8), async {
                loop {
                    let state = self
                        .terminals
                        .request("terminal/status", &json!({"id":id}))
                        .await
                        .unwrap()["terminal"]
                        .clone();
                    if state["output"].as_str().unwrap().contains(needle) {
                        return state;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap_or_else(|_| panic!("terminal did not produce {needle:?}"))
        }
        async fn close(&self, id: &str) {
            self.terminals
                .request("terminal/close", &json!({"id":id}))
                .await
                .unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            for session in self.terminals.sessions.lock().unwrap().values() {
                let _ = session.stop.send(true);
            }
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    #[tokio::test]
    async fn real_pty_inherits_setup_supports_interactive_unicode_ansi_and_resize() {
        let f = Fixture::new();
        fs::write(
            f.cwd.join(".git/codex-shell-environment.json"),
            r#"{"version":1,"set":{"MC_PTY_READY":"ready"},"exclude":[]}"#,
        )
        .unwrap();
        let state=f.create("printf '<BOOT:%s>\\n' \"$MC_PTY_READY\"; test -t 0 && test -t 1 && printf '<TTY:true>\\n'").await;
        let id = state["id"].as_str().unwrap();
        f.output(id, "<BOOT:ready>").await;
        f.output(id, "<TTY:true>").await;
        f.input(
            id,
            "IFS= read -r answer; printf '<READ:%s>\\n' \"$answer\"\r中文\r",
        )
        .await;
        f.output(id, "<READ:中文>").await;
        f.terminals
            .request("terminal/resize", &json!({"id":id,"cols":111,"rows":37}))
            .await
            .unwrap();
        f.input(
            id,
            "printf '<SIZE:%s>\\n' \"$(stty size)\"; printf '\\033[31mCOLOR\\033[0m\\n'\r",
        )
        .await;
        let result = f.output(id, "<SIZE:37 111>").await;
        assert_eq!(result["cols"], 111);
        f.output(id, "\u{1b}[31mCOLOR\u{1b}[0m").await;
        f.close(id).await;
        assert!(f
            .terminals
            .request("terminal/status", &json!({"id":id}))
            .await
            .is_err());
    }
    #[tokio::test]
    async fn ctrl_c_interrupts_the_foreground_command_and_the_shell_remains_usable() {
        let f = Fixture::new();
        let state = f
            .create("/bin/bash -c 'printf ready > started; exec sleep 30'")
            .await;
        let id = state["id"].as_str().unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !f.cwd.join("started").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        f.interrupt(id).await;
        f.input(id, "printf '<AFTER:ok>\\n'\r").await;
        f.output(id, "<AFTER:ok>").await;
        f.close(id).await;
    }
    #[tokio::test]
    async fn close_stops_shell_jobs_and_releases_the_checkout_after_they_stop() {
        let f = Fixture::new();
        let state = f
            .create("/bin/bash -c 'while :; do printf x >> live; sleep .02; done' & wait")
            .await;
        let id = state["id"].as_str().unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !f.cwd.join("live").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(crate::thread_lifecycle::checkout_at(&f.terminals.home, &f.cwd, true).is_err());
        f.close(id).await;
        let _lease = crate::thread_lifecycle::checkout_at(&f.terminals.home, &f.cwd, true).unwrap();
        let size = fs::metadata(f.cwd.join("live")).unwrap().len();
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(fs::metadata(f.cwd.join("live")).unwrap().len(), size);
    }
    #[tokio::test]
    async fn session_lists_keep_creation_order_and_thread_isolation_and_shutdown_closes_all() {
        let f = Fixture::new();
        let a = f.create("").await;
        let b = f.create("").await;
        let list = f
            .terminals
            .request("terminal/list", &json!({"cwd":f.cwd,"threadId":"chat-a"}))
            .await
            .unwrap();
        assert_eq!(list["data"][0]["id"], a["id"]);
        assert_eq!(list["data"][1]["id"], b["id"]);
        assert_eq!(
            f.terminals
                .request("terminal/list", &json!({"cwd":f.cwd,"threadId":"chat-b"}))
                .await
                .unwrap()["data"],
            json!([])
        );
        assert!(f
            .terminals
            .request("terminal/resize", &json!({"id":a["id"],"cols":0,"rows":24}))
            .await
            .is_err());
        f.terminals.shutdown().await;
        assert!(crate::thread_lifecycle::checkout_at(&f.terminals.home, &f.cwd, true).is_ok());
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn simultaneous_close_requests_both_wait_until_the_terminal_has_stopped() {
        let f = Fixture::new();
        for iteration in 0..12 {
            let state = f
                .create(&format!(
                    "/bin/bash -c 'printf ready > started-{iteration}; exec sleep 30'"
                ))
                .await;
            let id = state["id"].as_str().unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                while !f.cwd.join(format!("started-{iteration}")).exists() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            f.interrupt(id).await;
            f.input(id, "printf '<AFTER:%s>\\n' ok\r").await;
            f.output(id, "<AFTER:ok>").await;
            f.input(id,&format!("/bin/bash -c 'while :; do printf x >> live-{iteration}; sleep .02; done' & wait\r")).await;
            tokio::time::timeout(Duration::from_secs(5), async {
                while !f.cwd.join(format!("live-{iteration}")).exists() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            let mut tasks = Vec::new();
            for _ in 0..2 {
                let terminals = f.terminals.clone();
                let params = json!({"id":state["id"]});
                tasks.push(tokio::spawn(async move {
                    terminals.request("terminal/close", &params).await
                }));
            }
            for task in tasks {
                let terminal = tokio::time::timeout(Duration::from_secs(5), task)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap()["terminal"]
                    .clone();
                assert!(terminal.is_null() || terminal["status"] == "closed");
            }
        }
        assert!(crate::thread_lifecycle::checkout_at(&f.terminals.home, &f.cwd, true).is_ok());
    }
    #[tokio::test]
    async fn high_volume_output_waits_for_acknowledgement_and_keeps_a_bounded_replay_buffer() {
        let mut f = Fixture::new();
        let (out, _events) = tokio::sync::mpsc::unbounded_channel();
        f.terminals.out = out;
        let state = f
            .create("head -c 1200000 /dev/zero | tr '\\0' X; printf '<END:%s>\\n' true")
            .await;
        let id = state["id"].as_str().unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let state = f
                    .terminals
                    .request("terminal/status", &json!({"id":id}))
                    .await
                    .unwrap()["terminal"]
                    .clone();
                if state["sequence"].as_u64().unwrap() >= 8 {
                    assert!(!state["output"].as_str().unwrap().contains("<END:true>"));
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let final_state = tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let state = f
                    .terminals
                    .request("terminal/status", &json!({"id":id}))
                    .await
                    .unwrap()["terminal"]
                    .clone();
                assert!(state["output"].as_str().unwrap().len() <= MAX_OUTPUT);
                if state["output"].as_str().unwrap().contains("<END:true>") {
                    break state;
                }
                assert_eq!(
                    f.terminals
                        .request(
                            "terminal/ack",
                            &json!({"id":id,"sequence":state["sequence"]})
                        )
                        .await
                        .unwrap(),
                    json!({"ok":true})
                );
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap();
        assert!(final_state["sequence"].as_u64().unwrap() > 8);
        f.close(id).await;
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn closing_a_backpressured_pty_releases_the_master_before_reaping_its_shell() {
        let mut f = Fixture::new();
        let (out, _events) = tokio::sync::mpsc::unbounded_channel();
        f.terminals.out = out;
        let state = f.create("head -c 12000000 /dev/zero | tr '\\0' X").await;
        let id = state["id"].as_str().unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let state = f
                    .terminals
                    .request("terminal/status", &json!({"id":id}))
                    .await
                    .unwrap();
                if state["terminal"]["sequence"].as_u64().unwrap() >= 8 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(3), f.close(id))
            .await
            .unwrap();
        assert!(crate::thread_lifecycle::checkout_at(&f.terminals.home, &f.cwd, true).is_ok());
    }
}
