//! Cross-process exclusion between a thread handoff, public edits, and execution.
//! Locks are scoped to this app's profile. Model accounting may still drain while
//! a handoff owns the edit gate; it never changes the thread's execution owner.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

type Result<T> = std::result::Result<T, String>;

fn path(root: &Path, id: &str, suffix: &str) -> Result<PathBuf> {
    if id.is_empty() || id.len() > 256 {
        return Err("Invalid thread ID".into());
    }
    let key: String = id
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let parent = root.join("thread-lifecycle");
    fs::create_dir_all(&parent).map_err(|e| format!("Create thread lock directory: {e}"))?;
    Ok(parent.join(format!("{key}.{suffix}")))
}

fn open(root: &Path, id: &str, suffix: &str) -> Result<File> {
    let path = path(root, id, suffix)?;
    if fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err("Thread lock must not be a symlink".into());
    }
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|e| format!("Open thread lock: {e}"))
}

fn check_recovery(root: &Path, file: &File) -> Result<()> {
    let marker: Value = if file.metadata().map_err(|e| e.to_string())?.len() == 0 {
        return Ok(());
    } else {
        serde_json::from_reader(file).map_err(|e| format!("Read thread handoff marker: {e}"))?
    };
    if marker["status"] != "running" {
        return Ok(());
    }
    let records: Value = match fs::read(root.join("workspaces.json")) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|e| format!("Read handoff recovery: {e}"))?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("Read handoff recovery: {e}")),
    };
    let pending = records["handoffs"].as_object().is_some_and(|records| {
        records.values().any(|operation| {
            operation["requestId"] == marker["operationId"]
                && !matches!(
                    operation["status"].as_str(),
                    Some("committed" | "rolledBack")
                )
        })
    });
    if pending {
        return Err("This chat has an unfinished handoff; check its status and recover it before continuing".into());
    }
    Ok(())
}

pub fn access_at(root: &Path, id: &str, allow_archived: bool) -> Result<File> {
    let file = open(root, id, "handoff.lock")?;
    file.try_lock_shared()
        .map_err(|_| "This chat is being handed off; wait for it to finish".to_string())?;
    check_recovery(root, &file)?;
    if !allow_archived && crate::session::is_archived_at(root, id)? {
        return Err("This chat is archived; unarchive it before continuing".into());
    }
    Ok(file)
}

pub fn access(id: &str) -> Result<File> {
    access_at(&crate::config::home(), id, false)
}

pub fn exclusive(id: &str) -> Result<File> {
    let root = crate::config::home();
    let file = open(&root, id, "handoff.lock")?;
    file.try_lock().map_err(|_| {
        "This chat is busy in another operation; retry when it has finished".to_string()
    })?;
    check_recovery(&root, &file)?;
    Ok(file)
}

/// Must be acquired while the caller owns the thread's access or exclusive gate.
/// The returned file stays alive until turn completion and accounting are saved.
pub fn execution(id: &str) -> Result<File> {
    let file = open(&crate::config::home(), id, "execution.lock")?;
    file.try_lock().map_err(|_| {
        "A response is already running in another window; stop it there or wait for completion"
            .to_string()
    })?;
    Ok(file)
}

/// Execution holds a shared checkout lease until its final state is saved.
/// A handoff owns exclusive leases for the two checkouts it can change.
pub(crate) fn checkout_at(home: &Path, root: &Path, exclusive: bool) -> Result<File> {
    let root = root
        .canonicalize()
        .map_err(|e| format!("Resolve checkout: {e}"))?;
    #[cfg(unix)]
    let bytes = {
        use std::os::unix::ffi::OsStrExt;
        root.as_os_str().as_bytes().to_vec()
    };
    #[cfg(not(unix))]
    let bytes = root.to_string_lossy().as_bytes().to_vec();
    let key = format!("{:x}", Sha256::digest(bytes));
    let directory = home.join("thread-lifecycle/checkouts");
    fs::create_dir_all(&directory).map_err(|e| format!("Create checkout lease directory: {e}"))?;
    let path = directory.join(format!("{key}.lock"));
    if fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err("Checkout lease must not be a symlink".into());
    }
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(path)
        .map_err(|e| format!("Open checkout lease: {e}"))?;
    if exclusive {
        file.try_lock().map_err(|_| "Stop other running responses in the source and target checkouts before handing off this chat".to_string())?;
    } else {
        file.try_lock_shared()
            .map_err(|_| "This checkout is being handed off; wait for it to finish".to_string())?;
    }
    Ok(file)
}

pub fn mark_handoff(file: &mut File, operation: &str) -> Result<()> {
    file.set_len(0)
        .and_then(|_| file.seek(SeekFrom::Start(0)))
        .map_err(|e| e.to_string())?;
    serde_json::to_writer(
        &mut *file,
        &json!({"operationId":operation,"status":"running"}),
    )
    .map_err(|e| e.to_string())?;
    file.flush()
        .and_then(|_| file.sync_all())
        .map_err(|e| format!("Save handoff marker: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn handoff_excludes_edits_and_retains_unfinished_recovery() {
        let root =
            std::env::temp_dir().join(format!("mcodex-lifecycle-{:032x}", rand::random::<u128>()));
        let edit = access_at(&root, "thread", false).unwrap();
        let mut move_lock = open(&root, "thread", "handoff.lock").unwrap();
        assert!(move_lock.try_lock().is_err());
        drop(edit);
        move_lock.try_lock().unwrap();
        mark_handoff(&mut move_lock, "operation").unwrap();
        assert!(access_at(&root, "thread", false).is_err());
        fs::write(
            root.join("workspaces.json"),
            json!({"handoffs":{"h":{"requestId":"operation","status":"prepared"}}}).to_string(),
        )
        .unwrap();
        drop(move_lock);
        // Other test threads can be spawning processes as this descriptor is
        // closed. Wait for the OS lease to drain, then test the recovery gate.
        // Only the transient busy result is retried; all other errors are kept.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let recovered = loop {
            let result = access_at(&root, "thread", false);
            if result.as_ref().err().is_some_and(|error| {
                error == "This chat is being handed off; wait for it to finish"
            }) && std::time::Instant::now() < deadline
            {
                std::thread::sleep(std::time::Duration::from_millis(5));
                continue;
            }
            break result;
        };
        let error = recovered.unwrap_err();
        assert!(error.contains("unfinished handoff"), "{error}");
        fs::write(
            root.join("workspaces.json"),
            json!({"handoffs":{"h":{"requestId":"operation","status":"committed"}}}).to_string(),
        )
        .unwrap();
        assert!(access_at(&root, "thread", false).is_ok());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn checkout_execution_can_share_but_handoff_excludes_execution() {
        let home = std::env::temp_dir().join(format!(
            "mcodex-checkout-leases-{:032x}",
            rand::random::<u128>()
        ));
        let first = home.join("first");
        let other = home.join("other");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&other).unwrap();
        let a = checkout_at(&home, &first, false).unwrap();
        let b = checkout_at(&home, &first, false).unwrap();
        assert!(checkout_at(&home, &first, true)
            .unwrap_err()
            .contains("running responses"));
        let unrelated = checkout_at(&home, &other, true).unwrap();
        // A concurrent fork can briefly inherit a close-on-exec lease. Only
        // release checks wait for that copy; held-lease assertions stay strict.
        let released = |exclusive| {
            let start = std::time::Instant::now();
            loop {
                match checkout_at(&home, &first, exclusive) {
                    Ok(file) => break file,
                    Err(error)
                        if start.elapsed() < std::time::Duration::from_secs(1)
                            && (error.contains("running responses")
                                || error.contains("being handed off")) =>
                    {
                        std::thread::sleep(std::time::Duration::from_millis(2))
                    }
                    Err(error) => panic!("released checkout lease remained unavailable: {error}"),
                }
            }
        };
        drop((a, b));
        let move_lease = released(true);
        assert!(checkout_at(&home, &first, false)
            .unwrap_err()
            .contains("being handed off"));
        drop(move_lease);
        drop(released(false));
        drop(unrelated);
        fs::remove_dir_all(home).unwrap();
    }
}
