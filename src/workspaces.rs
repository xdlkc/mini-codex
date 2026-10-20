//! Local Git/worktree operations scoped to an explicitly selected working directory.
//! Archive preserves HEAD ancestry, the original index, and raw working files in
//! local recovery refs before Git removes a managed checkout. No remote commands run.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

type Result<T> = std::result::Result<T, String>;
static LOCK: Mutex<()> = Mutex::new(());
const MAX_OUTPUT: usize = 64 * 1024 * 1024;
const MAX_DIFF: usize = 1_000_000;
const METHODS: &[&str] = &[
    "workspace/gitStatus",
    "workspace/gitDiff",
    "workspace/branch/list",
    "workspace/branch/create",
    "workspace/branch/switch",
    "workspace/worktree/list",
    "workspace/worktree/create",
    "workspace/worktree/archive",
    "workspace/worktree/restore",
];

pub async fn handle(method: &str, params: &Value) -> Option<Result<Value>> {
    if !METHODS.contains(&method) {
        return None;
    }
    let method = method.to_string();
    let params = params.clone();
    Some(
        tokio::task::spawn_blocking(move || Store::configured().rpc(&method, &params))
            .await
            .map_err(|e| format!("Git workspace worker failed: {e}"))
            .and_then(|result| result),
    )
}

/// Register a worktree that another trusted Rust component just created. This
/// is deliberately not an RPC: arbitrary user paths cannot become managed.
pub async fn register_created_worktree(
    source_cwd: &Path,
    path: &Path,
    name: Option<&str>,
) -> Result<Value> {
    let source_cwd = source_cwd.to_path_buf();
    let path = path.to_path_buf();
    let name = name.map(String::from);
    tokio::task::spawn_blocking(move || {
        Store::configured().register(&source_cwd, &path, name.as_deref())
    })
    .await
    .map_err(|error| format!("Register worktree worker failed: {error}"))?
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
fn id(prefix: &str) -> String {
    format!("{prefix}_{:032x}", rand::random::<u128>())
}
fn required<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("{key} must be a non-empty string"))
}
fn bool_param(value: &Value, key: &str, default: bool) -> Result<bool> {
    match value.get(key) {
        None => Ok(default),
        Some(value) => value
            .as_bool()
            .ok_or_else(|| format!("{key} must be a boolean")),
    }
}
fn optional_string<'a>(value: &'a Value, key: &str) -> Result<Option<&'a str>> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        _ => Err(format!("{key} must be a string or null")),
    }
}
fn utf8(bytes: Vec<u8>, label: &str) -> Result<String> {
    String::from_utf8(bytes).map_err(|_| format!("{label} cannot be represented as UTF-8"))
}
fn path_text(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| format!("Path cannot be represented as UTF-8: {}", path.display()))
}
fn selected_directory(params: &Value) -> Result<PathBuf> {
    if !params.is_object() {
        return Err("parameters must be an object".into());
    }
    let raw = Path::new(required(params, "cwd")?);
    if !raw.is_absolute() {
        return Err("cwd must be an explicitly selected absolute directory".into());
    }
    let path = raw
        .canonicalize()
        .map_err(|e| format!("Cannot open selected directory {}: {e}", raw.display()))?;
    path_text(&path)?;
    if !path.is_dir() {
        return Err("cwd is not a directory".into());
    }
    Ok(path)
}
fn valid_relative(path: &str) -> Result<()> {
    if path.is_empty()
        || Path::new(path).is_absolute()
        || path.contains('\0')
        || Path::new(path).components().any(|c| match c {
            Component::Normal(name) => name.to_str().is_none_or(|s| s.eq_ignore_ascii_case(".git")),
            _ => true,
        })
    {
        return Err(format!("Unsafe workspace path: {path:?}"));
    }
    Ok(())
}
fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}
fn no_symlink(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(format!(
            "Refusing symlink at managed path {}",
            path.display()
        )),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("Inspect {}: {e}", path.display())),
    }
}
fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(|e| format!("Create {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    }
    Ok(())
}
fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

struct Captured {
    output: Output,
    truncated: bool,
}
fn read_capped(mut stream: impl Read, cap: usize) -> std::io::Result<(Vec<u8>, bool)> {
    let mut output = Vec::new();
    let mut buffer = [0u8; 16384];
    let mut truncated = false;
    loop {
        let size = stream.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        let keep = size.min(cap.saturating_sub(output.len()));
        output.extend_from_slice(&buffer[..keep]);
        truncated |= keep < size;
    }
    Ok((output, truncated))
}
fn execute(mut command: Command, input: Option<&[u8]>, cap: usize) -> Result<Captured> {
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|e| format!("Start local Git: {e}"))?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let output_reader = std::thread::spawn(move || read_capped(stdout, cap));
    let error_reader = std::thread::spawn(move || read_capped(stderr, 64 * 1024));
    let writer = input.map(|input| {
        let bytes = input.to_vec();
        let mut stdin = child.stdin.take().unwrap();
        std::thread::spawn(move || stdin.write_all(&bytes))
    });
    let started = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait().map_err(|e| format!("Wait for Git: {e}"))? {
            Some(status) => break status,
            None if started.elapsed() > Duration::from_secs(120) => {
                timed_out = true;
                let _ = child.kill();
                break child.wait().map_err(|e| e.to_string())?;
            }
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    };
    let (stdout, truncated) = output_reader
        .join()
        .map_err(|_| "Git stdout reader failed")?
        .map_err(|e| e.to_string())?;
    let (stderr, _) = error_reader
        .join()
        .map_err(|_| "Git stderr reader failed")?
        .map_err(|e| e.to_string())?;
    if let Some(writer) = writer {
        let result = writer.join().map_err(|_| "Git stdin writer failed")?;
        if result.is_err() && status.success() {
            return Err("Could not send snapshot data to Git".into());
        }
    }
    if timed_out {
        return Err(
            "Local Git command exceeded 120 seconds; no checkout was intentionally discarded"
                .into(),
        );
    }
    Ok(Captured {
        output: Output {
            status,
            stdout,
            stderr,
        },
        truncated,
    })
}
fn git_command(cwd: &Path, filters: &[String], index: Option<&Path>) -> Command {
    let mut command = Command::new("git");
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("GIT_") {
            command.env_remove(name);
        }
    }
    #[cfg(test)]
    {
        command
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
    }
    command
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_AUTHOR_NAME", "Mini Codex")
        .env("GIT_AUTHOR_EMAIL", "snapshot@localhost")
        .env("GIT_COMMITTER_NAME", "Mini Codex")
        .env("GIT_COMMITTER_EMAIL", "snapshot@localhost")
        .arg("--no-pager")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "gc.auto=0",
            "-c",
            "maintenance.auto=false",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
        ]);
    for filter in filters {
        command.arg("-c").arg(filter);
    }
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    command.arg("-C").arg(cwd);
    command
}
fn checked(
    cwd: &Path,
    filters: &[String],
    args: &[&str],
    input: Option<&[u8]>,
    index: Option<&Path>,
) -> Result<Vec<u8>> {
    let mut command = git_command(cwd, filters, index);
    command.args(args);
    let captured = execute(command, input, MAX_OUTPUT)?;
    if !captured.output.status.success() {
        return Err(format!(
            "git {}: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&captured.output.stderr).trim()
        ));
    }
    if captured.truncated {
        return Err(
            "Git output exceeds the safe processing limit; the operation was not completed".into(),
        );
    }
    Ok(captured.output.stdout)
}
#[derive(Clone)]
struct Repo {
    root: PathBuf,
    common: PathBuf,
    cwd: PathBuf,
    filters: Vec<String>,
}
impl Repo {
    fn open(cwd: &Path) -> Result<Option<Self>> {
        let mut command = git_command(cwd, &[], None);
        command.args(["rev-parse", "--show-toplevel"]);
        let output = execute(command, None, MAX_OUTPUT)?.output;
        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            if error.contains("not a git repository") {
                return Ok(None);
            }
            return Err(format!(
                "Cannot inspect selected Git directory: {}",
                error.trim()
            ));
        }
        let root = PathBuf::from(utf8(output.stdout, "Git root")?.trim_end_matches(['\r', '\n']))
            .canonicalize()
            .map_err(|e| format!("Resolve Git root: {e}"))?;
        if !cwd.starts_with(&root) {
            return Err("The selected directory is outside the Git working tree".into());
        }
        let common = checked(&root, &[], &["rev-parse", "--git-common-dir"], None, None)?;
        let common =
            PathBuf::from(utf8(common, "Git common directory")?.trim_end_matches(['\r', '\n']));
        let common = if common.is_absolute() {
            common
        } else {
            root.join(common)
        }
        .canonicalize()
        .map_err(|e| format!("Resolve Git common directory: {e}"))?;
        path_text(&common)?;
        let mut command = git_command(&root, &[], None);
        command.args(["config", "--name-only", "--get-regexp", r"^filter\."]);
        let config = execute(command, None, MAX_OUTPUT)?;
        if !config.output.status.success() && config.output.status.code() != Some(1) {
            return Err(format!(
                "Read Git filter configuration: {}",
                String::from_utf8_lossy(&config.output.stderr)
            ));
        }
        let mut filters = Vec::new();
        for key in utf8(config.output.stdout, "Git filter keys")?.lines() {
            if key.ends_with(".required") {
                filters.push(format!("{key}=false"));
            } else if [".process", ".smudge", ".clean"]
                .iter()
                .any(|suffix| key.ends_with(suffix))
            {
                filters.push(format!("{key}="));
            }
        }
        Ok(Some(Self {
            root,
            common,
            cwd: cwd.to_path_buf(),
            filters,
        }))
    }
    fn bytes(&self, args: &[&str]) -> Result<Vec<u8>> {
        checked(&self.root, &self.filters, args, None, None)
    }
    fn text(&self, args: &[&str]) -> Result<String> {
        Ok(utf8(self.bytes(args)?, "Git output")?
            .trim_end_matches(['\r', '\n'])
            .to_string())
    }
    fn input(&self, args: &[&str], input: &[u8], index: Option<&Path>) -> Result<Vec<u8>> {
        checked(&self.root, &self.filters, args, Some(input), index)
    }
    fn with_index(&self, args: &[&str], index: &Path) -> Result<Vec<u8>> {
        checked(&self.root, &self.filters, args, None, Some(index))
    }
    fn branch(&self) -> Result<Option<String>> {
        let mut command = git_command(&self.root, &self.filters, None);
        command.args(["symbolic-ref", "--quiet", "--short", "HEAD"]);
        let result = execute(command, None, MAX_OUTPUT)?.output;
        if result.status.success() {
            Ok(Some(utf8(result.stdout, "Branch name")?.trim().to_string()))
        } else if result.status.code() == Some(1) {
            Ok(None)
        } else {
            Err(format!(
                "Read current branch: {}",
                String::from_utf8_lossy(&result.stderr)
            ))
        }
    }
    fn head(&self) -> Result<Option<String>> {
        let mut command = git_command(&self.root, &self.filters, None);
        command.args(["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]);
        let result = execute(command, None, MAX_OUTPUT)?.output;
        if result.status.success() {
            return Ok(Some(utf8(result.stdout, "HEAD")?.trim().to_string()));
        }
        if let Some(branch) = self.branch()? {
            let reference = format!("refs/heads/{branch}");
            let mut command = git_command(&self.root, &self.filters, None);
            command.args(["show-ref", "--verify", "--quiet", &reference]);
            let exists = execute(command, None, MAX_OUTPUT)?.output;
            if exists.status.code() == Some(1) {
                return Ok(None);
            }
        }
        Err(format!(
            "Cannot read HEAD: {}",
            String::from_utf8_lossy(&result.stderr)
        ))
    }
    fn resolve(&self, reference: &str) -> Result<String> {
        if reference.is_empty()
            || reference.starts_with('-')
            || reference.contains(['\n', '\r', '\0'])
        {
            return Err("Invalid baseRef".into());
        }
        self.text(&[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{reference}^{{commit}}"),
        ])
    }
    fn admin_path(&self, name: &str) -> Result<PathBuf> {
        let path = PathBuf::from(self.text(&["rev-parse", "--git-path", name])?);
        Ok(if path.is_absolute() {
            path
        } else {
            self.root.join(path)
        })
    }
    fn ensure_not_busy(&self) -> Result<()> {
        for name in [
            "index.lock",
            "HEAD.lock",
            "MERGE_HEAD",
            "CHERRY_PICK_HEAD",
            "REVERT_HEAD",
            "BISECT_LOG",
            "rebase-merge",
            "rebase-apply",
            "sequencer",
        ] {
            if exists(&self.admin_path(name)?) {
                return Err(format!(
                    "Finish the Git operation or release its lock before continuing: {name}"
                ));
            }
        }
        Ok(())
    }
    fn ensure_archivable(&self) -> Result<()> {
        self.ensure_not_busy()?;
        let staged = self.bytes(&["ls-files", "--stage", "-z"])?;
        for record in staged.split(|byte| *byte == 0).filter(|r| !r.is_empty()) {
            let header = record.split(|byte| *byte == b'\t').next().unwrap();
            let fields = std::str::from_utf8(header)
                .map_err(|_| "Invalid Git index header")?
                .split_whitespace()
                .collect::<Vec<_>>();
            if fields.len() != 3 || fields[2] != "0" {
                return Err("Cannot archive an unmerged index".into());
            }
            if fields[0] == "160000" {
                return Err("Cannot archive a checkout containing Git submodules".into());
            }
        }
        let flags = self.bytes(&["ls-files", "-v", "-z"])?;
        if flags.split(|byte| *byte == 0).any(|entry| {
            entry
                .first()
                .is_some_and(|flag| *flag == b'S' || flag.is_ascii_lowercase())
        }) {
            return Err(
                "Cannot archive sparse or assume-unchanged index entries; clear those flags first"
                    .into(),
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct GitWorktree {
    path: PathBuf,
    head: String,
    branch: Option<String>,
    locked: Option<String>,
    prunable: Option<String>,
    main: bool,
}
fn git_worktrees(repo: &Repo) -> Result<Vec<GitWorktree>> {
    let bytes = repo.bytes(&["worktree", "list", "--porcelain", "-z"])?;
    let mut data = Vec::new();
    let mut current: Option<GitWorktree> = None;
    for field in bytes.split(|byte| *byte == 0) {
        if field.is_empty() {
            if let Some(item) = current.take() {
                data.push(item);
            }
            continue;
        }
        let field = std::str::from_utf8(field)
            .map_err(|_| "Worktree paths cannot be represented as UTF-8")?;
        if let Some(path) = field.strip_prefix("worktree ") {
            if let Some(item) = current.take() {
                data.push(item);
            }
            current = Some(GitWorktree {
                path: PathBuf::from(path),
                head: String::new(),
                branch: None,
                locked: None,
                prunable: None,
                main: data.is_empty(),
            });
        } else if let Some(item) = current.as_mut() {
            if let Some(head) = field.strip_prefix("HEAD ") {
                item.head = head.into();
            } else if let Some(branch) = field.strip_prefix("branch ") {
                item.branch = Some(branch.strip_prefix("refs/heads/").unwrap_or(branch).into());
            } else if field == "locked" || field.starts_with("locked ") {
                item.locked = Some(
                    field
                        .strip_prefix("locked ")
                        .unwrap_or("Locked by Git")
                        .into(),
                );
            } else if field == "prunable" || field.starts_with("prunable ") {
                item.prunable = Some(
                    field
                        .strip_prefix("prunable ")
                        .unwrap_or("Unavailable checkout")
                        .into(),
                );
            }
        }
    }
    if let Some(item) = current {
        data.push(item);
    }
    Ok(data)
}
fn nul_paths(bytes: Vec<u8>) -> Result<BTreeSet<String>> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            let path = std::str::from_utf8(path)
                .map_err(|_| "A workspace filename cannot be represented as UTF-8")?
                .to_string();
            valid_relative(&path)?;
            Ok(path)
        })
        .collect()
}
fn ignored_paths(repo: &Repo) -> Result<BTreeSet<String>> {
    nul_paths(repo.bytes(&[
        "ls-files",
        "--others",
        "--ignored",
        "--exclude-standard",
        "-z",
    ])?)
}
fn cut_diff(repo: &Repo, args: &[&str]) -> Result<(String, bool)> {
    let mut command = git_command(&repo.root, &repo.filters, None);
    command.args(args);
    let result = execute(command, None, MAX_DIFF)?;
    if !result.output.status.success() {
        return Err(format!(
            "Read Git diff: {}",
            String::from_utf8_lossy(&result.output.stderr)
        ));
    }
    Ok((
        String::from_utf8_lossy(&result.output.stdout).into_owned(),
        result.truncated,
    ))
}
fn numstats(repo: &Repo, head: bool) -> Result<BTreeMap<String, (u64, u64, bool)>> {
    let mut args = vec!["diff", "--no-ext-diff", "--no-textconv", "--numstat", "-z"];
    if head {
        args.push("HEAD");
    } else {
        args.push("--cached");
    }
    let bytes = repo.bytes(&args)?;
    let records = bytes.split(|byte| *byte == 0).collect::<Vec<_>>();
    let mut cursor = 0;
    let mut data = BTreeMap::new();
    while cursor < records.len() {
        let record = records[cursor];
        cursor += 1;
        if record.is_empty() {
            continue;
        }
        let fields = record.splitn(3, |byte| *byte == b'\t').collect::<Vec<_>>();
        if fields.len() != 3 {
            return Err("Malformed Git numstat output".into());
        }
        let path = if fields[2].is_empty() {
            if cursor + 1 >= records.len() {
                return Err("Malformed Git rename statistics".into());
            }
            let path = records[cursor + 1];
            cursor += 2;
            path
        } else {
            fields[2]
        };
        let path = std::str::from_utf8(path)
            .map_err(|_| "Git filename cannot be represented as UTF-8")?
            .to_string();
        let count = |bytes: &[u8]| {
            std::str::from_utf8(bytes)
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0)
        };
        data.insert(
            path,
            (count(fields[0]), count(fields[1]), fields[0] == b"-"),
        );
    }
    Ok(data)
}
fn git_status(repo: &Repo) -> Result<Value> {
    let head = repo.head()?;
    let branch = repo.branch()?;
    let bytes = repo.bytes(&["status", "--porcelain=v1", "-z", "--untracked-files=all"])?;
    let records = bytes.split(|byte| *byte == 0).collect::<Vec<_>>();
    let stats = numstats(repo, head.is_some())?;
    let mut cursor = 0;
    let mut files = Vec::new();
    while cursor < records.len() {
        let record = records[cursor];
        cursor += 1;
        if record.is_empty() {
            continue;
        }
        if record.len() < 4 {
            return Err("Malformed Git status output".into());
        }
        let path = std::str::from_utf8(&record[3..])
            .map_err(|_| "Git filename cannot be represented as UTF-8")?;
        let status = std::str::from_utf8(&record[..2]).map_err(|_| "Malformed Git status code")?;
        let original = if record[..2]
            .iter()
            .any(|code| *code == b'R' || *code == b'C')
        {
            let original = records.get(cursor).ok_or("Missing Git rename source")?;
            cursor += 1;
            Some(std::str::from_utf8(original).map_err(|_| "Git rename filename is not UTF-8")?)
        } else {
            None
        };
        let (added, deleted, binary) = stats.get(path).copied().unwrap_or_default();
        files.push(json!({"path":path,"originalPath":original,"status":status.trim(),"indexStatus":(record[0] as char).to_string(),"worktreeStatus":(record[1] as char).to_string(),"added":added,"deleted":deleted,"binary":binary,"untracked":status=="??"}));
    }
    let (staged, staged_truncated) = cut_diff(
        repo,
        &["diff", "--cached", "--no-ext-diff", "--no-textconv", "--"],
    )?;
    let (unstaged, unstaged_truncated) =
        cut_diff(repo, &["diff", "--no-ext-diff", "--no-textconv", "--"])?;
    let (diff, truncated) = if head.is_some() {
        cut_diff(
            repo,
            &["diff", "--no-ext-diff", "--no-textconv", "HEAD", "--"],
        )?
    } else {
        (
            format!("{staged}\n{unstaged}"),
            staged_truncated || unstaged_truncated,
        )
    };
    let total_added: u64 = stats.values().map(|(a, _, _)| a).sum();
    let total_deleted: u64 = stats.values().map(|(_, d, _)| d).sum();
    let ignored = ignored_paths(repo)?.len();
    let untracked = files
        .iter()
        .filter(|file| file["untracked"] == true)
        .count();
    Ok(
        json!({"isGit":true,"repoRoot":repo.root,"cwd":repo.cwd,"head":head,"branch":branch,"clean":files.is_empty(),"changedFiles":files,"untrackedCount":untracked,"ignoredCount":ignored,"totalAdded":total_added,"totalDeleted":total_deleted,"diff":diff,"truncated":truncated,"diffTruncated":truncated,"stagedDiff":staged,"unstagedDiff":unstaged,"stagedDiffTruncated":staged_truncated,"unstagedDiffTruncated":unstaged_truncated,"filtersDisabled":!repo.filters.is_empty()}),
    )
}
fn branch_name(repo: &Repo, name: &str) -> Result<()> {
    if name.starts_with('-') || name.contains("@{") || name.contains(['\0', '\n', '\r']) {
        return Err("Invalid branch name".into());
    }
    repo.bytes(&["check-ref-format", "--branch", name])?;
    Ok(())
}
fn branch_list(repo: &Repo) -> Result<Value> {
    let worktrees = git_worktrees(repo)?;
    let branch = repo.branch()?;
    let output = repo.text(&[
        "for-each-ref",
        "--format=%(refname)%00%(objectname)%00%(symref)",
        "refs/heads/",
        "refs/remotes/",
    ])?;
    let mut data = Vec::new();
    for line in output.lines() {
        let fields = line.split('\0').collect::<Vec<_>>();
        if fields.len() != 3 {
            return Err("Malformed Git branch list".into());
        }
        let remote = fields[0].starts_with("refs/remotes/");
        let name = fields[0]
            .strip_prefix("refs/heads/")
            .or_else(|| fields[0].strip_prefix("refs/remotes/"))
            .ok_or("Invalid branch reference")?;
        let checkout = worktrees
            .iter()
            .find(|tree| !remote && tree.branch.as_deref() == Some(name));
        data.push(json!({"name":name,"commit":fields[1],"current":!remote&&branch.as_deref()==Some(name),"isRemote":remote,"symbolic":!fields[2].is_empty(),"worktreePath":checkout.map(|tree|&tree.path)}));
    }
    Ok(
        json!({"cwd":repo.cwd,"repoRoot":repo.root,"currentBranch":branch,"head":repo.head()?,"data":data}),
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    path: String,
    mode: String,
    permissions: u32,
    blob: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Directory {
    path: String,
    permissions: u32,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Manifest {
    version: u32,
    id: String,
    head: String,
    index_tree: String,
    entries: Vec<Entry>,
    directories: Vec<Directory>,
    root_permissions: u32,
    include_ignored: bool,
    ignored_excluded: usize,
}
#[cfg(unix)]
fn permissions(meta: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o777
}
#[cfg(not(unix))]
fn permissions(_: &fs::Metadata) -> u32 {
    0o644
}
#[cfg(unix)]
fn apply_permissions(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|e| format!("Restore permissions {}: {e}", path.display()))
}
#[cfg(not(unix))]
fn apply_permissions(_: &Path, _: u32) -> Result<()> {
    Ok(())
}
#[cfg(unix)]
fn symlink_bytes(path: &Path) -> Result<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt;
    Ok(fs::read_link(path)
        .map_err(|e| e.to_string())?
        .as_os_str()
        .as_bytes()
        .to_vec())
}

#[cfg(unix)]
fn restore_symlink(target: &[u8], path: &Path) -> Result<()> {
    use std::os::unix::{ffi::OsStrExt, fs::symlink};
    symlink(std::ffi::OsStr::from_bytes(target), path)
        .map_err(|e| format!("Restore symlink {}: {e}", path.display()))
}
#[cfg(not(unix))]
fn restore_symlink(_target: &[u8], _path: &Path) -> Result<()> {
    Err("Restoring symbolic links is unsupported on this platform".into())
}

fn stream_blob(repo: &Repo, blob: &str, target: &Path) -> Result<()> {
    let file = private_options()
        .open(target)
        .map_err(|e| format!("Create restored file {}: {e}", target.display()))?;
    let mut command = git_command(&repo.root, &repo.filters, None);
    command
        .args(["cat-file", "blob", blob])
        .stdin(Stdio::null())
        .stdout(Stdio::from(file))
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|e| format!("Read recovery blob: {e}"))?;
    let stderr = child.stderr.take().unwrap();
    let reader = std::thread::spawn(move || read_capped(stderr, 64 * 1024));
    let started = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) => break status,
            None if started.elapsed() > Duration::from_secs(120) => {
                timed_out = true;
                let _ = child.kill();
                break child.wait().map_err(|e| e.to_string())?;
            }
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    };
    let (error, _) = reader
        .join()
        .map_err(|_| "Recovery blob reader failed")?
        .map_err(|e| e.to_string())?;
    if timed_out {
        return Err(
            "Reading a recovery blob exceeded 120 seconds; partial restore was retained".into(),
        );
    }
    if !status.success() {
        return Err(format!(
            "Read recovery blob: {}",
            String::from_utf8_lossy(&error)
        ));
    }
    File::open(target)
        .and_then(|file| file.sync_all())
        .map_err(|e| format!("Sync restored file: {e}"))?;
    Ok(())
}
#[cfg(not(unix))]
fn symlink_bytes(path: &Path) -> Result<Vec<u8>> {
    Ok(path_text(&fs::read_link(path).map_err(|e| e.to_string())?)?
        .as_bytes()
        .to_vec())
}
fn walk_directories(root: &Path) -> Result<Vec<Directory>> {
    let mut stack = vec![root.to_path_buf()];
    let mut directories = Vec::new();
    #[cfg(unix)]
    let device = {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(root).map_err(|e| e.to_string())?.dev()
    };
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)
            .map_err(|e| format!("Read workspace directory {}: {e}", dir.display()))?
        {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            let name = entry.file_name();
            if name
                .to_str()
                .is_some_and(|name| name.eq_ignore_ascii_case(".git"))
            {
                if dir == root {
                    continue;
                }
                return Err(format!(
                    "Cannot archive an embedded Git repository: {}",
                    path.display()
                ));
            }
            let meta = fs::symlink_metadata(&path)
                .map_err(|e| format!("Inspect {}: {e}", path.display()))?;
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    if meta.dev() != device {
                        return Err("Cannot archive a nested filesystem mount".into());
                    }
                }
                let rel = path_text(
                    path.strip_prefix(root)
                        .map_err(|_| "Directory escaped workspace")?,
                )?
                .to_string();
                valid_relative(&rel)?;
                directories.push(Directory {
                    path: rel,
                    permissions: permissions(&meta),
                });
                stack.push(path);
            } else if !meta.is_file() {
                return Err(format!("Cannot archive special file {}", path.display()));
            }
        }
    }
    directories.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(directories)
}
fn raw_snapshot(repo: &Repo, id: &str, include_ignored: bool) -> Result<Manifest> {
    repo.ensure_archivable()?;
    let directories = walk_directories(&repo.root)?;
    let ignored = ignored_paths(repo)?;
    let mut paths = nul_paths(repo.bytes(&[
        "ls-files",
        "--cached",
        "--others",
        "--exclude-standard",
        "-z",
    ])?)?;
    if include_ignored {
        paths.extend(ignored.iter().cloned());
    }
    let mut entries = Vec::new();
    let mut regular = Vec::new();
    for path in paths {
        let full = repo.root.join(&path);
        let meta = match fs::symlink_metadata(&full) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("Read {path}: {e}")),
        };
        if meta.file_type().is_symlink() {
            let blob = utf8(
                repo.input(
                    &["hash-object", "-w", "--no-filters", "--stdin"],
                    &symlink_bytes(&full)?,
                    None,
                )?,
                "Symlink object",
            )?
            .trim()
            .to_string();
            entries.push(Entry {
                path,
                mode: "120000".into(),
                permissions: 0,
                blob,
            });
        } else if meta.is_file() {
            let mode = if permissions(&meta) & 0o111 != 0 {
                "100755"
            } else {
                "100644"
            };
            regular.push(Entry {
                path,
                mode: mode.into(),
                permissions: permissions(&meta),
                blob: String::new(),
            });
        } else {
            return Err(format!("Cannot snapshot non-file path {path}"));
        }
    }
    for chunk in regular.chunks_mut(256) {
        let mut args = vec!["hash-object", "-w", "--no-filters", "--"];
        args.extend(chunk.iter().map(|entry| entry.path.as_str()));
        let hashes = repo
            .text(&args)?
            .lines()
            .map(String::from)
            .collect::<Vec<_>>();
        if hashes.len() != chunk.len() {
            return Err("Git did not hash every workspace file".into());
        }
        for (entry, hash) in chunk.iter_mut().zip(hashes) {
            entry.blob = hash;
        }
    }
    entries.extend(regular);
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let head = repo
        .head()?
        .ok_or("Cannot archive a worktree with an unborn HEAD")?;
    let index_tree = repo.text(&["write-tree"])?;
    Ok(Manifest {
        version: 1,
        id: id.into(),
        head,
        index_tree,
        entries,
        directories,
        root_permissions: permissions(&fs::metadata(&repo.root).map_err(|e| e.to_string())?),
        include_ignored,
        ignored_excluded: if include_ignored { 0 } else { ignored.len() },
    })
}
struct TempIndex(PathBuf);
impl Drop for TempIndex {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
        let _ = fs::remove_file(self.0.with_extension("index.lock"));
    }
}
fn create_snapshot(
    repo: &Repo,
    manifest: &Manifest,
    home: &Path,
    branch: Option<String>,
) -> Result<Value> {
    let tempdir = home.join("workspace-tmp");
    private_dir(&tempdir)?;
    let index = TempIndex(tempdir.join(format!("{}.index", id("snapshot"))));
    repo.with_index(&["read-tree", "--empty"], &index.0)?;
    let mut input = Vec::new();
    for entry in &manifest.entries {
        input.extend_from_slice(format!("{} {}\t", entry.mode, entry.blob).as_bytes());
        input.extend_from_slice(entry.path.as_bytes());
        input.push(0);
    }
    repo.input(
        &["update-index", "--add", "-z", "--index-info"],
        &input,
        Some(&index.0),
    )?;
    let tree = utf8(repo.with_index(&["write-tree"], &index.0)?, "Snapshot tree")?
        .trim()
        .to_string();
    let staged = utf8(
        repo.input(
            &["commit-tree", &manifest.index_tree, "-p", &manifest.head],
            b"Mini Codex staged recovery snapshot\n",
            None,
        )?,
        "Index snapshot",
    )?
    .trim()
    .to_string();
    let commit = utf8(
        repo.input(
            &["commit-tree", &tree, "-p", &staged],
            b"Mini Codex raw workspace recovery snapshot\n",
            None,
        )?,
        "Working snapshot",
    )?
    .trim()
    .to_string();
    let blob = utf8(
        repo.input(
            &["hash-object", "-w", "--no-filters", "--stdin"],
            &serde_json::to_vec(manifest).map_err(|e| e.to_string())?,
            None,
        )?,
        "Recovery manifest",
    )?
    .trim()
    .to_string();
    let reference = format!("refs/mcodex/worktrees/{}/{}", manifest.id, id("snapshot"));
    let manifest_ref = format!("{reference}-manifest");
    repo.input(
        &["update-ref", "--stdin"],
        format!(
            "start\ncreate {reference} {commit}\ncreate {manifest_ref} {blob}\nprepare\ncommit\n"
        )
        .as_bytes(),
        None,
    )?;
    Ok(
        json!({"ref":reference,"commit":commit,"indexCommit":staged,"manifestRef":manifest_ref,"manifestBlob":blob,"head":manifest.head,"originalBranch":branch,"createdAt":now_ms(),"includeIgnored":manifest.include_ignored,"ignoredExcluded":manifest.ignored_excluded,"entryCount":manifest.entries.len()}),
    )
}

fn verify_snapshot_tree(repo: &Repo, commit: &str, manifest: &Manifest) -> Result<()> {
    let bytes = repo.bytes(&["ls-tree", "-r", "-z", commit])?;
    let mut actual = BTreeMap::new();
    for record in bytes
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let tab = record
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or("Invalid snapshot tree entry")?;
        let header = &record[..tab];
        let path = &record[tab + 1..];
        let header = std::str::from_utf8(header)
            .map_err(|_| "Invalid snapshot object metadata")?
            .split_whitespace()
            .collect::<Vec<_>>();
        if header.len() != 3 || header[1] != "blob" {
            return Err("Recovery tree contains unsupported objects".into());
        }
        let path = std::str::from_utf8(path).map_err(|_| "Recovery filename is not UTF-8")?;
        valid_relative(path)?;
        actual.insert(
            path.to_string(),
            (header[0].to_string(), header[2].to_string()),
        );
    }
    let expected: BTreeMap<_, _> = manifest
        .entries
        .iter()
        .map(|entry| (entry.path.clone(), (entry.mode.clone(), entry.blob.clone())))
        .collect();
    if expected.len() != manifest.entries.len() || actual != expected {
        return Err("Recovery manifest does not match its protected snapshot tree".into());
    }
    Ok(())
}

#[derive(Clone)]
struct Store {
    home: PathBuf,
}
impl Store {
    fn configured() -> Self {
        Self {
            home: crate::config::home(),
        }
    }
    fn file(&self) -> PathBuf {
        self.home.join("workspaces.json")
    }
    fn locked<T>(&self, action: impl FnOnce(&mut Value) -> Result<T>) -> Result<T> {
        let _guard = LOCK
            .lock()
            .map_err(|_| "Workspace operation lock is poisoned")?;
        private_dir(&self.home)?;
        let lockpath = self.home.join("workspaces.lock");
        no_symlink(&lockpath)?;
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(lockpath)
            .map_err(|e| format!("Open workspace lock: {e}"))?;
        let started = Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(e) if started.elapsed() > Duration::from_secs(2) => {
                    return Err(format!("Another workspace operation is in progress: {e}"))
                }
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        let mut state = self.load()?;
        action(&mut state)
    }
    fn load(&self) -> Result<Value> {
        no_symlink(&self.file())?;
        let bytes = match fs::read(self.file()) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(json!({"version":1,"worktrees":{}}))
            }
            Err(e) => return Err(format!("Read workspace registry: {e}")),
        };
        let state: Value = serde_json::from_slice(&bytes)
            .map_err(|e| format!("Invalid workspace registry: {e}"))?;
        if state["version"] != 1 || !state["worktrees"].is_object() {
            return Err("Unsupported or invalid workspace registry".into());
        }
        for (id, record) in state["worktrees"].as_object().unwrap() {
            if record["id"] != id.as_str() || !record.is_object() {
                return Err("Invalid managed worktree record".into());
            }
        }
        Ok(state)
    }
    fn save(&self, state: &Value) -> Result<()> {
        let path = self.home.join(format!(".{}.tmp", id("workspaces")));
        let result = (|| {
            let mut file = private_options().open(&path).map_err(|e| e.to_string())?;
            file.write_all(&serde_json::to_vec_pretty(state).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            drop(file);
            fs::rename(&path, self.file()).map_err(|e| e.to_string())?;
            #[cfg(unix)]
            File::open(&self.home)
                .and_then(|file| file.sync_all())
                .map_err(|e| e.to_string())?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(path);
        }
        result.map_err(|e: String| format!("Persist workspace recovery information: {e}"))
    }
    fn managed_root(&self) -> Result<PathBuf> {
        let path = self.home.join("worktrees");
        no_symlink(&path)?;
        private_dir(&path)?;
        path.canonicalize().map_err(|e| e.to_string())
    }
    fn rpc(&self, method: &str, params: &Value) -> Result<Value> {
        let cwd = selected_directory(params)?;
        let repo = match Repo::open(&cwd)? {
            Some(repo) => repo,
            None if method == "workspace/gitStatus" => {
                return Ok(
                    json!({"isGit":false,"cwd":cwd,"repoRoot":null,"branch":null,"head":null,"changedFiles":[],"totalAdded":0,"totalDeleted":0,"diff":"","clean":true}),
                )
            }
            None => return Err("The selected directory is not a Git working tree".into()),
        };
        match method {
            "workspace/gitStatus" => git_status(&repo),
            "workspace/gitDiff" => {
                let staged = match params.get("staged") {
                    Some(value) => Some(value.as_bool().ok_or("staged must be a boolean")?),
                    None => None,
                };
                let base = optional_string(params, "baseRef")?
                    .map(|value| repo.resolve(value))
                    .transpose()?;
                let path = params
                    .get("path")
                    .map(|value| value.as_str().ok_or("path must be a string"))
                    .transpose()?;
                if let Some(path) = path {
                    valid_relative(path)?;
                }
                let mut args = vec!["diff", "--no-ext-diff", "--no-textconv"];
                if staged == Some(true) {
                    args.push("--cached");
                }
                if let Some(base) = base.as_deref() {
                    args.push(base);
                } else if staged.is_none() && repo.head()?.is_some() {
                    args.push("HEAD");
                }
                args.push("--");
                if let Some(path) = path {
                    args.push(path);
                }
                let (diff, truncated) = cut_diff(&repo, &args)?;
                Ok(json!({"cwd":repo.cwd,"repoRoot":repo.root,"diff":diff,"truncated":truncated}))
            }
            "workspace/branch/list" => branch_list(&repo),
            "workspace/branch/switch" | "workspace/branch/create" => {
                repo.ensure_not_busy()?;
                let branch = required(params, "branch")?;
                branch_name(&repo, branch)?;
                if method == "workspace/branch/switch" {
                    // No force/merge/discard flags, and ignored files are protected too.
                    repo.bytes(&[
                        "switch",
                        "--no-guess",
                        "--no-overwrite-ignore",
                        "--no-recurse-submodules",
                        branch,
                    ])?;
                } else {
                    let base =
                        repo.resolve(optional_string(params, "baseRef")?.unwrap_or("HEAD"))?;
                    if bool_param(params, "switch", true)? {
                        repo.bytes(&[
                            "switch",
                            "--no-overwrite-ignore",
                            "--no-recurse-submodules",
                            "-c",
                            branch,
                            &base,
                        ])?;
                    } else {
                        repo.bytes(&["branch", "--no-track", branch, &base])?;
                    }
                }
                Ok(
                    json!({"cwd":repo.cwd,"repoRoot":repo.root,"branch":repo.branch()?,"head":repo.head()?}),
                )
            }
            "workspace/worktree/list" => self.locked(|state| self.list(&repo, state)),
            "workspace/worktree/create" => self.locked(|state| self.create(&repo, params, state)),
            "workspace/worktree/archive" => self.locked(|state| self.archive(&repo, params, state)),
            "workspace/worktree/restore" => self.locked(|state| self.restore(&repo, params, state)),
            _ => Err(format!("Unknown workspace method: {method}")),
        }
    }
    fn owned(&self, repo: &Repo, params: &Value, state: &Value) -> Result<Value> {
        let id = required(params, "id")?;
        let record = state["worktrees"]
            .get(id)
            .ok_or("Unknown managed worktree id")?
            .clone();
        if record["commonDir"].as_str() != Some(path_text(&repo.common)?) {
            return Err(
                "This worktree does not belong to the explicitly selected repository".into(),
            );
        }
        if !id.starts_with("wt_")
            || id.len() != 35
            || !id[3..].bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("Invalid managed worktree identity".into());
        }
        let root = self.managed_root()?;
        let relative = record["ownershipPath"].as_str().unwrap_or(id);
        valid_relative(relative)?;
        let expected = root.join(relative);
        let path = PathBuf::from(required(&record, "path")?);
        if path != expected {
            return Err("Managed worktree path does not match its ownership record".into());
        }
        validate_managed_path(&root, &path)?;
        no_symlink(&path)?;
        if exists(&path) && path.canonicalize().map_err(|e| e.to_string())? != expected {
            return Err("Managed worktree escaped its storage directory".into());
        }
        Ok(record)
    }
    fn register(&self, source_cwd: &Path, path: &Path, name: Option<&str>) -> Result<Value> {
        let source = selected_directory(&json!({"cwd":source_cwd}))?;
        let repo = Repo::open(&source)?.ok_or("Source directory is not a Git checkout")?;
        let path = path
            .canonicalize()
            .map_err(|e| format!("Resolve created worktree: {e}"))?;
        self.locked(|state|{
            let root=self.managed_root()?;
            validate_managed_path(&root,&path)?;
            let relative=path.strip_prefix(&root).map_err(|_|"Created worktree is outside App-managed storage")?;
            let relative=path_text(relative)?;valid_relative(relative)?;
            let trees=git_worktrees(&repo)?;
            let tree=trees.iter().find(|tree|tree.path==path).ok_or("Created directory is not a registered Git worktree")?;
            if tree.main{return Err("The primary checkout cannot be registered as a managed worktree".into());}
            let checkout=Repo::open(&path)?.ok_or("Created worktree is not available")?;
            if checkout.common!=repo.common||checkout.root!=path{return Err("Created worktree belongs to another repository".into());}
            if let Some(record)=state["worktrees"].as_object().unwrap().values().find(|record|record["path"].as_str()==path.to_str()){
                if record["commonDir"].as_str()!=repo.common.to_str(){return Err("Existing ownership record belongs to another repository".into());}
                return Ok(json!({"worktree":record}));
            }
            let id=id("wt");let now=now_ms();let cwd=path.join(repo.cwd.strip_prefix(&repo.root).map_err(|_|"Selected cwd escaped its repository")?);
            let record=json!({"id":id,"name":name.filter(|name|!name.trim().is_empty()).unwrap_or("Scheduled worktree"),"repoRoot":repo.root,"commonDir":repo.common,"sourceCwd":repo.cwd,"path":path,"workspacePath":path,"ownershipPath":relative,"cwd":cwd,"branch":tree.branch,"head":tree.head,"state":"active","managed":true,"snapshot":null,"dirtyStatus":git_status(&checkout)?,"error":null,"createdAt":now,"updatedAt":now});
            state["worktrees"][&id]=record.clone();self.save(state)?;
            Ok(json!({"worktree":record}))
        })
    }
    fn list(&self, repo: &Repo, state: &mut Value) -> Result<Value> {
        let git = git_worktrees(repo)?;
        let mut data = Vec::new();
        let mut seen = BTreeSet::new();
        for tree in &git {
            let stored = state["worktrees"]
                .as_object()
                .unwrap()
                .values()
                .find(|record| {
                    record["commonDir"].as_str() == repo.common.to_str()
                        && record["path"].as_str() == tree.path.to_str()
                });
            let mut item=stored.cloned().unwrap_or_else(||json!({"id":format!("unmanaged:{}",tree.path.display()),"name":tree.path.file_name().map(|name|name.to_string_lossy().to_string()).unwrap_or_else(||tree.path.display().to_string()),"path":tree.path,"workspacePath":tree.path,"cwd":tree.path,"repoRoot":repo.root,"commonDir":repo.common,"sourceCwd":repo.cwd,"managed":false,"snapshot":null,"createdAt":null,"updatedAt":null}));
            if let Some(id) = item["id"].as_str() {
                seen.insert(id.to_string());
            }
            item["head"] = json!(tree.head);
            item["branch"] = json!(tree.branch);
            item["primary"] = json!(tree.main);
            item["locked"] = json!(tree.locked);
            item["prunable"] = json!(tree.prunable);
            item["state"] = json!(if tree.path.is_dir() {
                "active"
            } else {
                "error"
            });
            let checkout = Repo::open(&tree.path);
            match checkout
                .and_then(|repo| repo.ok_or("Registered checkout is unavailable".into()))
                .and_then(|repo| git_status(&repo))
            {
                Ok(mut status) => {
                    for field in ["diff", "stagedDiff", "unstagedDiff"] {
                        status.as_object_mut().unwrap().remove(field);
                    }
                    item["dirtyStatus"] = status;
                    if stored.is_none() {
                        item["error"] = Value::Null;
                    }
                }
                Err(error) => {
                    item["dirtyStatus"] = Value::Null;
                    item["error"] = json!(error);
                }
            }
            data.push(item);
        }
        for (id, record) in state["worktrees"].as_object().unwrap() {
            if seen.contains(id) || record["commonDir"].as_str() != repo.common.to_str() {
                continue;
            }
            let mut item = record.clone();
            let path = Path::new(required(record, "path")?);
            if !exists(path) && !record["snapshot"].is_null() {
                item["state"] = json!("archived");
            } else if record["state"] != "archived" {
                item["state"] = json!("error");
                item["error"] =
                    json!("Managed checkout is not registered with Git; its files were retained");
            }
            data.push(item);
        }
        Ok(
            json!({"repository":{"repoRoot":repo.root,"commonDir":repo.common,"cwd":repo.cwd},"data":data}),
        )
    }
    fn create(&self, repo: &Repo, params: &Value, state: &mut Value) -> Result<Value> {
        let head = repo.resolve(optional_string(params, "baseRef")?.unwrap_or("HEAD"))?;
        let relative = repo
            .cwd
            .strip_prefix(&repo.root)
            .map_err(|_| "Selected directory escaped repository")?;
        if !relative.as_os_str().is_empty() {
            let target = format!("{head}:{}", path_text(relative)?);
            if repo.text(&["cat-file", "-t", &target])? != "tree" {
                return Err("The selected subdirectory does not exist in baseRef; select the repository root".into());
            }
        }
        let id = id("wt");
        let root = self.managed_root()?;
        let path = root.join(&id);
        if exists(&path) {
            return Err("Worktree destination already exists".into());
        }
        let name = optional_string(params, "name")?
            .filter(|name| !name.trim().is_empty())
            .unwrap_or("Worktree")
            .trim();
        let detached = bool_param(params, "detached", false)?;
        if detached && params.get("branch").is_some_and(|branch| !branch.is_null()) {
            return Err("branch and detached cannot be combined".into());
        }
        let slug: String = name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .take(35)
            .collect();
        let generated = format!(
            "codex/{}-{}",
            if slug.is_empty() { "work" } else { &slug },
            &id[3..11]
        );
        let branch = if detached {
            None
        } else {
            let value = optional_string(params, "branch")?.unwrap_or(&generated);
            branch_name(repo, value)?;
            Some(value.to_string())
        };
        let cwd = path.join(relative);
        let now = now_ms();
        let record = json!({"id":id,"name":name,"repoRoot":repo.root,"commonDir":repo.common,"sourceCwd":repo.cwd,"path":path,"workspacePath":path,"ownershipPath":id,"cwd":cwd,"branch":branch,"head":head,"state":"creating","managed":true,"snapshot":null,"dirtyStatus":null,"error":null,"createdAt":now,"updatedAt":now});
        state["worktrees"][&id] = record;
        self.save(state)?;
        let result = if let Some(branch) = branch.as_deref() {
            repo.bytes(&[
                "worktree",
                "add",
                "-b",
                branch,
                "--",
                path_text(&path)?,
                &head,
            ])
        } else {
            repo.bytes(&[
                "worktree",
                "add",
                "--detach",
                "--",
                path_text(&path)?,
                &head,
            ])
        };
        match result {
            Ok(_) => {
                state["worktrees"][&id]["state"] = json!("active");
                let checkout = Repo::open(&path)?.ok_or("Created worktree is unavailable")?;
                state["worktrees"][&id]["dirtyStatus"] = git_status(&checkout)?;
                self.save(state)?;
                Ok(json!({"worktree":state["worktrees"][&id]}))
            }
            Err(error) => {
                state["worktrees"][&id]["state"] = json!("error");
                state["worktrees"][&id]["error"] = json!(error);
                self.save(state)?;
                Err(format!(
                    "{error}; recovery record retained for {}",
                    path.display()
                ))
            }
        }
    }
    fn archive(&self, repo: &Repo, params: &Value, state: &mut Value) -> Result<Value> {
        let record = self.owned(repo, params, state)?;
        let id = required(&record, "id")?;
        let path = PathBuf::from(required(&record, "path")?);
        let trees = git_worktrees(repo)?;
        let tree = trees
            .iter()
            .find(|tree| tree.path == path)
            .ok_or("Worktree is not currently registered; its files were not removed")?;
        if tree.main {
            return Err("The primary working tree cannot be archived".into());
        }
        if tree.locked.is_some() {
            return Err(
                "Git has locked this worktree; unlock it explicitly before archiving".into(),
            );
        }
        if tree.prunable.is_some() {
            return Err(
                "The working tree is unavailable; archive cannot verify its contents".into(),
            );
        }
        let checkout = Repo::open(&path)?.ok_or("Managed worktree is not a Git checkout")?;
        if checkout.common != repo.common {
            return Err("Managed worktree now points to another repository".into());
        }
        let include = bool_param(params, "includeIgnored", false)?;
        let discard = bool_param(params, "discardIgnored", false)?;
        if include && discard {
            return Err("includeIgnored and discardIgnored are mutually exclusive".into());
        }
        checkout.ensure_archivable()?;
        let ignored = ignored_paths(&checkout)?;
        if !include && !discard && !ignored.is_empty() {
            return Err(format!("{} ignored files require includeIgnored:true or an explicit discardIgnored:true; no files were removed",ignored.len()));
        }
        let manifest = raw_snapshot(&checkout, id, include)?;
        let snapshot = create_snapshot(&checkout, &manifest, &self.home, tree.branch.clone())?;
        state["worktrees"][id]["snapshot"] = snapshot.clone();
        state["worktrees"][id]["dirtyStatus"] = git_status(&checkout)?;
        state["worktrees"][id]["state"] = json!("archiving");
        state["worktrees"][id]["updatedAt"] = json!(now_ms());
        self.save(state).map_err(|error| {
            format!(
                "Snapshot {} was saved; checkout was not removed: {error}",
                snapshot["ref"]
            )
        })?;
        // Re-hash before the destructive Git step, after the durable recovery refs.
        let verification = raw_snapshot(&checkout, id, include);
        let result = match verification {
            Ok(current) if current == manifest => {
                let current = git_worktrees(repo)?;
                if current
                    .iter()
                    .find(|tree| tree.path == path)
                    .is_none_or(|tree| tree.locked.is_some() || tree.main)
                {
                    Err("Worktree registration or lock changed while preparing archive".into())
                } else {
                    repo.bytes(&["worktree", "remove", "--force", "--", path_text(&path)?])
                        .map(|_| ())
                }
            }
            Ok(_) => Err(
                "Worktree changed while its snapshot was being prepared; checkout was retained"
                    .into(),
            ),
            Err(error) => Err(error),
        };
        match result {
            Ok(()) => {
                state["worktrees"][id]["state"] = json!("archived");
                state["worktrees"][id]["error"] = Value::Null;
                self.save(state)?;
                Ok(json!({"worktree":state["worktrees"][id]}))
            }
            Err(error) => {
                state["worktrees"][id]["state"] = json!("active");
                state["worktrees"][id]["error"] = json!(error);
                self.save(state)?;
                Err(format!(
                    "{error}; recovery snapshot retained at {}",
                    snapshot["ref"]
                ))
            }
        }
    }
    fn restore(&self, repo: &Repo, params: &Value, state: &mut Value) -> Result<Value> {
        let record = self.owned(repo, params, state)?;
        let id = required(&record, "id")?;
        let path = PathBuf::from(required(&record, "path")?);
        if exists(&path) {
            return Err(
                "Restore destination already exists; its files will not be overwritten".into(),
            );
        }
        if git_worktrees(repo)?.iter().any(|tree| tree.path == path) {
            return Err(
                "Git still registers the restore destination; resolve that registration first"
                    .into(),
            );
        }
        let snapshot = &record["snapshot"];
        let reference = required(snapshot, "ref")?;
        let commit = required(snapshot, "commit")?;
        if repo.resolve(reference)? != commit {
            return Err("Recovery snapshot ref changed; refusing to restore different data".into());
        }
        let manifest_ref = required(snapshot, "manifestRef")?;
        let manifest_blob = required(snapshot, "manifestBlob")?;
        if repo.text(&["rev-parse", "--verify", "--end-of-options", manifest_ref])? != manifest_blob
        {
            return Err("Recovery manifest ref changed".into());
        }
        let manifest: Manifest =
            serde_json::from_slice(&repo.bytes(&["cat-file", "blob", manifest_blob])?)
                .map_err(|e| format!("Invalid recovery manifest: {e}"))?;
        if manifest.version != 1 || manifest.id != id || manifest.head != snapshot["head"] {
            return Err("Recovery manifest identity does not match the selected worktree".into());
        }
        for entry in &manifest.entries {
            valid_relative(&entry.path)?;
            if !["100644", "100755", "120000"].contains(&entry.mode.as_str())
                || entry.permissions > 0o777
            {
                return Err("Invalid recovery file metadata".into());
            }
        }
        for directory in &manifest.directories {
            valid_relative(&directory.path)?;
            if directory.permissions > 0o777 {
                return Err("Invalid recovery directory permissions".into());
            }
        }
        if manifest.root_permissions > 0o777 {
            return Err("Invalid recovery root permissions".into());
        }
        let staged = required(snapshot, "indexCommit")?;
        if repo.resolve(&format!("{commit}^"))? != staged
            || repo.resolve(&format!("{staged}^"))? != manifest.head
        {
            return Err("Recovery snapshot ancestry does not match its original HEAD/index".into());
        }
        verify_snapshot_tree(repo, commit, &manifest)?;
        if repo.text(&[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{staged}^{{tree}}"),
        ])? != manifest.index_tree
        {
            return Err("Staged recovery snapshot changed".into());
        }
        let head = repo.resolve(&manifest.head)?;
        state["worktrees"][id]["state"] = json!("restoring");
        self.save(state)?;
        let result = (|| -> Result<()> {
            repo.bytes(&[
                "worktree",
                "add",
                "--detach",
                "--no-checkout",
                "--",
                path_text(&path)?,
                &head,
            ])?;
            let checkout = Repo::open(&path)?.ok_or("Restored checkout is unavailable")?;
            checkout.bytes(&["read-tree", staged])?;
            for directory in &manifest.directories {
                safe_directory(&path, &directory.path)?;
            }
            for entry in &manifest.entries {
                if let Some(parent) = Path::new(&entry.path)
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                {
                    safe_directory(&path, path_text(parent)?)?;
                }
                let target = path.join(&entry.path);
                if exists(&target) {
                    return Err(format!(
                        "Recovery file already exists and was retained: {}",
                        target.display()
                    ));
                }
                if entry.mode == "120000" {
                    restore_symlink(
                        &checkout.bytes(&["cat-file", "blob", &entry.blob])?,
                        &target,
                    )?;
                } else {
                    stream_blob(&checkout, &entry.blob, &target)?;
                    let meta = fs::symlink_metadata(&target)
                        .map_err(|e| format!("Restored file {}: {e}", entry.path))?;
                    if !meta.is_file()
                        || !target
                            .canonicalize()
                            .map_err(|e| e.to_string())?
                            .starts_with(&path)
                    {
                        return Err("Restored file escaped the managed checkout".into());
                    }
                    apply_permissions(&target, entry.permissions)?;
                }
            }
            let mut directories = manifest.directories.clone();
            directories
                .sort_by_key(|directory| std::cmp::Reverse(directory.path.matches('/').count()));
            for directory in directories {
                apply_permissions(&path.join(directory.path), directory.permissions)?;
            }
            apply_permissions(&path, manifest.root_permissions)?;
            let restored = raw_snapshot(&checkout, id, manifest.include_ignored)?;
            if restored.entries != manifest.entries
                || restored.directories != manifest.directories
                || restored.head != manifest.head
                || restored.index_tree != manifest.index_tree
            {
                return Err("Restored checkout failed content verification; recovery refs and checkout were retained".into());
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                let checkout = Repo::open(&path)?.ok_or("Restored checkout disappeared")?;
                state["worktrees"][id]["state"] = json!("active");
                state["worktrees"][id]["branch"] = Value::Null;
                state["worktrees"][id]["head"] = json!(head);
                state["worktrees"][id]["error"] = Value::Null;
                state["worktrees"][id]["updatedAt"] = json!(now_ms());
                state["worktrees"][id]["dirtyStatus"] = git_status(&checkout)?;
                self.save(state)?;
                Ok(json!({"worktree":state["worktrees"][id]}))
            }
            Err(error) => {
                state["worktrees"][id]["state"] = json!("error");
                state["worktrees"][id]["error"] = json!(error);
                self.save(state)?;
                Err(format!(
                    "{error}; recovery refs and any partially restored files were retained"
                ))
            }
        }
    }
}
fn validate_managed_path(root: &Path, path: &Path) -> Result<()> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| "Worktree is outside App-managed storage")?;
    let relative = path_text(relative)?;
    valid_relative(relative)?;
    let mut current = root.to_path_buf();
    for component in Path::new(relative).components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
                return Err(format!(
                    "Managed worktree ancestor is a file or symlink: {}",
                    current.display()
                ))
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("Inspect managed worktree path: {e}")),
        }
    }
    Ok(())
}

fn safe_directory(root: &Path, relative: &str) -> Result<()> {
    valid_relative(relative)?;
    let mut path = root.to_path_buf();
    for component in Path::new(relative).components() {
        path.push(component);
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
            Ok(_) => return Err("Recovery directory conflicts with a file or symlink".into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&path).map_err(|e| e.to_string())?
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        root: PathBuf,
        repo: PathBuf,
        store: Store,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(id("mc-workspaces-test"));
            let repo = root.join("repository");
            fs::create_dir_all(repo.join("src")).unwrap();
            checked(&repo, &[], &["init", "-b", "main"], None, None).unwrap();
            fs::write(repo.join("tracked.txt"), b"base\n").unwrap();
            fs::write(repo.join("src/file.txt"), b"source\n").unwrap();
            fs::write(repo.join(".gitignore"), b"ignored.txt\nbuild/\n").unwrap();
            fs::write(repo.join(".gitattributes"), b"*.crlf text eol=lf\n").unwrap();
            checked(&repo, &[], &["add", "--all"], None, None).unwrap();
            checked(&repo, &[], &["commit", "-m", "initial"], None, None).unwrap();
            Self {
                store: Store {
                    home: root.join("home"),
                },
                root,
                repo,
            }
        }
        fn call(&self, method: &str, mut params: Value) -> Result<Value> {
            if params.get("cwd").is_none() {
                params["cwd"] = json!(self.repo);
            }
            self.store.rpc(method, &params)
        }
        fn create(&self) -> Value {
            self.call(
                "workspace/worktree/create",
                json!({"name":"test","branch":format!("codex/test-{}",id("branch"))}),
            )
            .unwrap()["worktree"]
                .clone()
        }
        fn git(&self, cwd: &Path, args: &[&str]) -> String {
            utf8(
                checked(cwd, &[], args, None, None).unwrap(),
                "fixture output",
            )
            .unwrap()
            .trim()
            .into()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn create_list_archive_restore_preserves_index_working_bytes_modes_and_ignored_files() {
        let fixture = Fixture::new();
        let original_head = fixture.git(&fixture.repo, &["rev-parse", "HEAD"]);
        fs::write(
            fixture.repo.join("tracked.txt"),
            b"source dirty is isolated\n",
        )
        .unwrap();
        let worktree = fixture.create();
        let id = worktree["id"].as_str().unwrap();
        let path = PathBuf::from(worktree["path"].as_str().unwrap());
        assert_eq!(fs::read(path.join("tracked.txt")).unwrap(), b"base\n");
        assert_eq!(
            fixture.git(&fixture.repo, &["branch", "--show-current"]),
            "main"
        );
        fs::write(path.join("tracked.txt"), b"staged version\n").unwrap();
        fixture.git(&path, &["add", "tracked.txt"]);
        fs::write(path.join("tracked.txt"), b"unstaged version\r\n").unwrap();
        fs::write(path.join("raw.crlf"), b"one\r\ntwo\r\n").unwrap();
        fixture.git(&path, &["add", "raw.crlf"]);
        fs::write(path.join("raw.crlf"), b"one\r\nchanged\r\n").unwrap();
        fs::remove_file(path.join("src/file.txt")).unwrap();
        let unusual = "中文 \"quoted\"\nfilename.bin";
        fs::write(path.join(unusual), [0, 255, 1, 10, 13]).unwrap();
        fs::write(path.join("ignored.txt"), b"ignored data\0\xff").unwrap();
        fs::create_dir_all(path.join("build/empty")).unwrap();
        fs::write(path.join("build/output.bin"), [9, 0, 7]).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            use std::os::unix::fs::{symlink, PermissionsExt};
            fs::set_permissions(path.join("ignored.txt"), fs::Permissions::from_mode(0o600))
                .unwrap();
            fs::set_permissions(path.join("tracked.txt"), fs::Permissions::from_mode(0o750))
                .unwrap();
            symlink(
                std::ffi::OsStr::from_bytes(b"target-\xff"),
                path.join("raw-link"),
            )
            .unwrap();
        }
        let index = fixture.git(&path, &["write-tree"]);
        let staged = fixture.git(&path, &["diff", "--cached", "--binary"]);
        let before = Repo::open(&path).unwrap().unwrap();
        let manifest = raw_snapshot(&before, id, true).unwrap();
        let archived = fixture
            .call(
                "workspace/worktree/archive",
                json!({"id":id,"includeIgnored":true}),
            )
            .unwrap();
        assert_eq!(archived["worktree"]["state"], "archived");
        assert!(!path.exists());
        assert_eq!(
            fixture.git(&fixture.repo, &["rev-parse", "HEAD"]),
            original_head
        );
        assert_eq!(
            fs::read(fixture.repo.join("tracked.txt")).unwrap(),
            b"source dirty is isolated\n"
        );
        let listed = fixture.call("workspace/worktree/list", json!({})).unwrap();
        assert!(listed["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == id && item["state"] == "archived"));
        let restored = fixture
            .call("workspace/worktree/restore", json!({"id":id}))
            .unwrap();
        assert!(restored["worktree"]["branch"].is_null());
        assert_eq!(restored["worktree"]["head"], original_head);
        assert_eq!(fixture.git(&path, &["write-tree"]), index);
        assert_eq!(
            fixture.git(&path, &["diff", "--cached", "--binary"]),
            staged
        );
        let checkout = Repo::open(&path).unwrap().unwrap();
        assert_eq!(raw_snapshot(&checkout, id, true).unwrap(), manifest);
        assert_eq!(
            fs::read(path.join("raw.crlf")).unwrap(),
            b"one\r\nchanged\r\n"
        );
        assert_eq!(
            fs::read(path.join(unusual)).unwrap(),
            vec![0, 255, 1, 10, 13]
        );
        #[cfg(unix)]
        {
            use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};
            assert_eq!(
                fs::metadata(path.join("ignored.txt"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(path.join("tracked.txt"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o750
            );
            assert_eq!(
                fs::read_link(path.join("raw-link"))
                    .unwrap()
                    .as_os_str()
                    .as_bytes(),
                b"target-\xff"
            );
        }
    }

    #[test]
    fn ignored_data_needs_an_explicit_policy_and_discard_preserves_nonignored_data() {
        let fixture = Fixture::new();
        let tree = fixture.create();
        let id = tree["id"].as_str().unwrap();
        let path = PathBuf::from(tree["path"].as_str().unwrap());
        fs::write(path.join("ignored.txt"), b"do not silently lose this").unwrap();
        fs::write(path.join("draft.txt"), b"keep draft").unwrap();
        assert!(fixture
            .call("workspace/worktree/archive", json!({"id":id}))
            .unwrap_err()
            .contains("ignored files"));
        assert_eq!(
            fs::read(path.join("ignored.txt")).unwrap(),
            b"do not silently lose this"
        );
        assert!(fixture
            .call(
                "workspace/worktree/archive",
                json!({"id":id,"includeIgnored":true,"discardIgnored":true})
            )
            .is_err());
        let archived = fixture
            .call(
                "workspace/worktree/archive",
                json!({"id":id,"discardIgnored":true}),
            )
            .unwrap();
        assert_eq!(archived["worktree"]["snapshot"]["ignoredExcluded"], 1);
        fixture
            .call("workspace/worktree/restore", json!({"id":id}))
            .unwrap();
        assert!(!path.join("ignored.txt").exists());
        assert_eq!(fs::read(path.join("draft.txt")).unwrap(), b"keep draft");
    }

    #[test]
    fn archive_rejects_locked_embedded_submodule_and_in_progress_git_states() {
        let fixture = Fixture::new();
        let tree = fixture.create();
        let id = tree["id"].as_str().unwrap();
        let path = PathBuf::from(tree["path"].as_str().unwrap());
        fixture.git(
            &fixture.repo,
            &["worktree", "lock", path_text(&path).unwrap()],
        );
        assert!(fixture
            .call(
                "workspace/worktree/archive",
                json!({"id":id,"includeIgnored":true})
            )
            .unwrap_err()
            .contains("locked"));
        fixture.git(
            &fixture.repo,
            &["worktree", "unlock", path_text(&path).unwrap()],
        );
        fs::create_dir_all(path.join("build/nested/.git")).unwrap();
        assert!(fixture
            .call(
                "workspace/worktree/archive",
                json!({"id":id,"includeIgnored":true})
            )
            .unwrap_err()
            .contains("embedded"));
        fs::remove_dir_all(path.join("build")).unwrap();
        let checkout = Repo::open(&path).unwrap().unwrap();
        let lock = checkout.admin_path("index.lock").unwrap();
        fs::write(&lock, b"owned by another operation").unwrap();
        assert!(fixture
            .call(
                "workspace/worktree/archive",
                json!({"id":id,"includeIgnored":true})
            )
            .unwrap_err()
            .contains("index.lock"));
        assert!(lock.exists());
        fs::remove_file(&lock).unwrap();
        let head = checkout.head().unwrap().unwrap();
        fixture.git(
            &path,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{head},nested-module"),
            ],
        );
        assert!(fixture
            .call(
                "workspace/worktree/archive",
                json!({"id":id,"includeIgnored":true})
            )
            .unwrap_err()
            .contains("submodules"));
        assert!(path.exists());
    }

    #[test]
    fn restore_never_overwrites_paths_and_ids_cannot_cross_repository_boundaries() {
        let fixture = Fixture::new();
        let tree = fixture.create();
        let id = tree["id"].as_str().unwrap();
        let path = PathBuf::from(tree["path"].as_str().unwrap());
        let other = Fixture::new();
        assert!(fixture
            .call(
                "workspace/worktree/archive",
                json!({"id":id,"cwd":other.repo,"includeIgnored":true})
            )
            .unwrap_err()
            .contains("selected repository"));
        let list = fixture.call("workspace/worktree/list", json!({})).unwrap();
        let primary = list["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["primary"] == true)
            .unwrap();
        assert_eq!(primary["managed"], false);
        assert!(fixture
            .call(
                "workspace/worktree/archive",
                json!({"id":primary["id"],"includeIgnored":true})
            )
            .is_err());
        fixture
            .call(
                "workspace/worktree/archive",
                json!({"id":id,"includeIgnored":true}),
            )
            .unwrap();
        fs::create_dir(&path).unwrap();
        fs::write(path.join("new-data"), b"new user files").unwrap();
        assert!(fixture
            .call("workspace/worktree/restore", json!({"id":id}))
            .unwrap_err()
            .contains("already exists"));
        assert_eq!(fs::read(path.join("new-data")).unwrap(), b"new user files");
    }

    #[test]
    fn branch_switch_preserves_dirty_files_and_refuses_ignored_collisions() {
        let fixture = Fixture::new();
        fixture
            .call(
                "workspace/branch/create",
                json!({"branch":"codex/next","switch":false}),
            )
            .unwrap();
        fs::write(fixture.repo.join("tracked.txt"), b"pending data").unwrap();
        fixture
            .call("workspace/branch/switch", json!({"branch":"codex/next"}))
            .unwrap();
        assert_eq!(
            fs::read(fixture.repo.join("tracked.txt")).unwrap(),
            b"pending data"
        );
        fixture.git(&fixture.repo, &["restore", "tracked.txt"]);
        fs::write(fixture.repo.join("ignored.txt"), b"tracked on next").unwrap();
        fixture.git(&fixture.repo, &["add", "-f", "ignored.txt"]);
        fixture.git(&fixture.repo, &["commit", "-m", "track ignored path"]);
        fixture.git(&fixture.repo, &["switch", "main"]);
        fs::write(fixture.repo.join("ignored.txt"), b"valuable ignored data").unwrap();
        assert!(fixture
            .call("workspace/branch/switch", json!({"branch":"codex/next"}))
            .is_err());
        assert_eq!(
            fixture.git(&fixture.repo, &["branch", "--show-current"]),
            "main"
        );
        assert_eq!(
            fs::read(fixture.repo.join("ignored.txt")).unwrap(),
            b"valuable ignored data"
        );
        let branches = fixture.call("workspace/branch/list", json!({})).unwrap();
        assert!(branches["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|branch| branch["name"] == "main" && branch["current"] == true));
    }

    #[test]
    fn git_status_is_explicit_and_handles_rename_and_newline_paths() {
        let fixture = Fixture::new();
        let nongit = fixture.root.join("not-git");
        fs::create_dir(&nongit).unwrap();
        assert_eq!(
            fixture
                .call("workspace/gitStatus", json!({"cwd":nongit}))
                .unwrap()["isGit"],
            false
        );
        assert!(fixture
            .store
            .rpc("workspace/gitStatus", &json!({}))
            .is_err());
        assert!(fixture
            .call("workspace/gitStatus", json!({"cwd":"relative"}))
            .is_err());
        fixture.git(&fixture.repo, &["mv", "tracked.txt", "renamed\nfile.txt"]);
        fs::write(fixture.repo.join("untracked\nfile"), b"new").unwrap();
        let status = fixture.call("workspace/gitStatus", json!({})).unwrap();
        assert_eq!(status["isGit"], true);
        assert_eq!(
            status["repoRoot"],
            fixture.repo.canonicalize().unwrap().to_str().unwrap()
        );
        assert!(status["changedFiles"]
            .as_array()
            .unwrap()
            .iter()
            .any(
                |file| file["path"] == "renamed\nfile.txt" && file["originalPath"] == "tracked.txt"
            ));
        assert!(status["changedFiles"]
            .as_array()
            .unwrap()
            .iter()
            .any(|file| file["path"] == "untracked\nfile" && file["untracked"] == true));
    }

    #[test]
    fn scheduled_registration_is_internal_idempotent_and_archivable() {
        let fixture = Fixture::new();
        let source = fixture.repo.canonicalize().unwrap();
        let target = fixture.store.managed_root().unwrap().join("run_saved/0");
        fixture.git(
            &source,
            &[
                "worktree",
                "add",
                "-b",
                "codex/scheduled-test",
                path_text(&target).unwrap(),
                "HEAD",
            ],
        );
        let registered = fixture
            .store
            .register(&source, &target, Some("Scheduled recovery"))
            .unwrap();
        let id = registered["worktree"]["id"].as_str().unwrap();
        assert_eq!(
            fixture.store.register(&source, &target, None).unwrap()["worktree"]["id"],
            id
        );
        fs::write(target.join("task-result"), b"persist scheduler result").unwrap();
        fixture
            .call(
                "workspace/worktree/archive",
                json!({"id":id,"includeIgnored":true}),
            )
            .unwrap();
        fixture
            .call("workspace/worktree/restore", json!({"id":id}))
            .unwrap();
        assert_eq!(
            fs::read(target.join("task-result")).unwrap(),
            b"persist scheduler result"
        );
        assert!(fixture.store.register(&source, &source, None).is_err());
    }

    #[test]
    fn external_git_filters_and_hooks_are_not_executed() {
        let fixture = Fixture::new();
        let marker = fixture.root.join("filter-ran");
        fs::write(
            fixture.repo.join(".gitattributes"),
            b"*.txt filter=unsafe\n",
        )
        .unwrap();
        fixture.git(&fixture.repo, &["add", ".gitattributes"]);
        fixture.git(&fixture.repo, &["commit", "-m", "filter attributes"]);
        fixture.git(
            &fixture.repo,
            &[
                "config",
                "filter.unsafe.smudge",
                &format!("touch '{}'", marker.display()),
            ],
        );
        fixture.git(
            &fixture.repo,
            &[
                "config",
                "filter.unsafe.clean",
                &format!("touch '{}'", marker.display()),
            ],
        );
        fixture.git(&fixture.repo, &["config", "filter.unsafe.required", "true"]);
        let tree = fixture.create();
        let id = tree["id"].as_str().unwrap();
        let path = PathBuf::from(tree["path"].as_str().unwrap());
        assert!(!marker.exists());
        assert_eq!(fs::read(path.join("tracked.txt")).unwrap(), b"base\n");
        fixture
            .call(
                "workspace/worktree/archive",
                json!({"id":id,"includeIgnored":true}),
            )
            .unwrap();
        fixture
            .call("workspace/worktree/restore", json!({"id":id}))
            .unwrap();
        assert!(!marker.exists());
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_filenames_and_symlinked_managed_ancestors_cannot_lose_data() {
        use std::os::unix::{ffi::OsStringExt, fs::symlink};
        let fixture = Fixture::new();
        let tree = fixture.create();
        let id = tree["id"].as_str().unwrap();
        let path = PathBuf::from(tree["path"].as_str().unwrap());
        let odd = path.join(std::ffi::OsString::from_vec(vec![b'f', 0xff]));
        // Some macOS filesystems/sandboxes reject non-UTF-8 names at creation.
        // The raw Git decoder must reject them regardless of host filesystem.
        assert!(nul_paths(vec![b'f', 0xff, 0]).is_err());
        match fs::write(&odd, b"keep bytes") {
            Ok(()) => {
                assert!(fixture
                    .call(
                        "workspace/worktree/archive",
                        json!({"id":id,"includeIgnored":true})
                    )
                    .is_err());
                assert_eq!(fs::read(&odd).unwrap(), b"keep bytes");
                fs::remove_file(odd).unwrap();
            }
            Err(error) => {
                // macOS can report EILSEQ (92) without a dedicated ErrorKind.
                assert!(
                    matches!(
                        error.kind(),
                        std::io::ErrorKind::InvalidInput | std::io::ErrorKind::PermissionDenied
                    ) || (cfg!(target_os = "macos") && error.raw_os_error() == Some(92)),
                    "non-UTF-8 fixture creation failed: {error}; kind={:?}; raw_os_error={:?}",
                    error.kind(),
                    error.raw_os_error()
                );
                let probe = path.join("writable-utf8-probe");
                fs::write(&probe, b"writable").unwrap();
                fs::remove_file(probe).unwrap();
            }
        }
        fixture
            .call(
                "workspace/worktree/archive",
                json!({"id":id,"includeIgnored":true}),
            )
            .unwrap();
        let outside = fixture.root.join("outside");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, &path).unwrap();
        assert!(fixture
            .call("workspace/worktree/restore", json!({"id":id}))
            .unwrap_err()
            .contains("symlink"));
        assert!(fs::read_dir(outside).unwrap().next().is_none());
    }
}
