//! An archive keeps its checkout lease across asynchronous cleanup, without
//! holding the registry lock while a project script is running.
use super::*;
pub(super) struct PreparedArchive {
    pub(super) store: Store,
    pub(super) repo: Repo,
    pub(super) checkout: Repo,
    pub(super) record: Value,
    pub(super) id: String,
    pub(super) path: PathBuf,
    pub(super) manifest: Manifest,
    pub(super) snapshot: Value,
    pub(super) include: bool,
    pub(super) discard: bool,
    pub(super) branch: Option<String>,
    pub(super) execution: File,
}
pub async fn archive_with_environment(
    params: Value,
    service: crate::local_environments::Environments,
) -> Result<Value> {
    let prepared = tokio::task::spawn_blocking(move || {
        let store = Store::configured();
        let directory = selected_directory(&params)?;
        let repo = Repo::open(&directory)?.ok_or("A Git checkout is required")?;
        store.locked(|state| store.prepare_archive(&repo, &params, state))
    })
    .await
    .map_err(|e| e.to_string())??;
    let source = PathBuf::from(prepared.record["sourceCwd"].as_str().unwrap_or("."));
    let verify = prepared.checkout.clone();
    let expected = prepared.manifest.clone();
    let id = prepared.id.clone();
    let include = prepared.include;
    let unchanged = tokio::task::spawn_blocking(move || {
        if raw_snapshot(&verify, &id, include)? != expected {
            return Err(
                "Worktree changed while its snapshot was being prepared; checkout was retained"
                    .into(),
            );
        }
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?;
    let request =
        unchanged.and_then(|_| crate::local_environments::cleanup_request(&prepared.path, &source));
    let outcome = match request {
        Ok(Some(request)) => match prepared.execution.try_clone() {
            Ok(lease) => service
                .run_owned(&prepared.path, &request, lease)
                .await
                .map(Some),
            Err(error) => Err(error.to_string()),
        },
        Ok(None) => Ok(None),
        Err(error) => Err(error),
    };
    tokio::task::spawn_blocking(move || {
        let store = prepared.store.clone();
        match outcome {
            Ok(cleanup) => {
                store.locked(|state| store.complete_archive(&prepared, cleanup, None, state))
            }
            Err(error) => {
                store.locked(|state| store.complete_archive(&prepared, None, Some(error), state))
            }
        }
    })
    .await
    .map_err(|e| e.to_string())?
}
impl Store {
    pub(super) fn complete_archive(
        &self,
        prepared: &PreparedArchive,
        cleanup: Option<Value>,
        cleanup_error: Option<String>,
        state: &mut Value,
    ) -> Result<Value> {
        let PreparedArchive {
            repo,
            checkout,
            id,
            path,
            manifest,
            snapshot,
            include,
            discard,
            branch,
            ..
        } = prepared;
        if state["worktrees"][id]["path"] != json!(path)
            || state["worktrees"][id]["snapshot"]["ref"] != snapshot["ref"]
            || state["worktrees"][id]["state"] != "archiving"
        {
            return Err(
                "Archive ownership changed; the checkout and recovery snapshot were retained"
                    .into(),
            );
        }
        let result = (|| {
            if let Some(error) = cleanup_error {
                return Err(error);
            }
            let after = raw_snapshot(checkout, id, *include)?;
            let verification = if let Some(cleanup) = cleanup {
                if !include && !discard && !ignored_paths(checkout)?.is_empty() {
                    return Err("Cleanup created ignored files; preserve or explicitly discard them before retrying archive".into());
                }
                // Preserve both the pre-cleanup files and any new output. The
                // regular restore remains the user's pre-cleanup snapshot.
                let cleanup_snapshot =
                    create_snapshot(checkout, &after, &self.home, branch.clone())?;
                state["worktrees"][id]["cleanupSnapshot"] = cleanup_snapshot;
                state["worktrees"][id]["cleanupResult"] = cleanup;
                self.save(state)?;
                after
            } else {
                if &after != manifest {
                    return Err("Worktree changed while its snapshot was being prepared; checkout was retained".into());
                }
                after
            };
            if raw_snapshot(checkout, id, *include)? != verification {
                return Err("Worktree changed while cleanup recovery was being saved; checkout was retained".into());
            }
            let current = git_worktrees(repo)?;
            if current
                .iter()
                .find(|tree| tree.path == *path)
                .is_none_or(|tree| tree.locked.is_some() || tree.main)
            {
                return Err("Worktree registration or lock changed while preparing archive".into());
            }
            repo.bytes(&["worktree", "remove", "--force", "--", path_text(path)?])?;
            cleanup_empty_worktree_parent(path);
            Ok(())
        })();
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
}
