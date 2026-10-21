//! Git half of a chat handoff. A prepared transaction keeps both original
//! checkouts recoverable until the chat half has saved its new owner.
use super::*;
use sha2::{Digest, Sha256};

#[derive(Clone, Serialize, Deserialize)]
struct Image {
    manifest: Manifest,
    index_blob: String,
    branch: Option<String>,
}

// Unlike an archive manifest, a transaction image can describe a conflicted
// index. Its raw index blob is authoritative; the tree is only a recovery parent.
fn image(repo: &Repo, operation: &str) -> Result<Image> {
    repo.ensure_not_busy()?;
    let directories = walk_directories(&repo.root)?;
    let mut paths = Vec::new();
    for directory in std::iter::once(repo.root.clone()).chain(
        directories
            .iter()
            .map(|directory| repo.root.join(&directory.path)),
    ) {
        for entry in fs::read_dir(&directory).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if directory == repo.root && entry.file_name().eq_ignore_ascii_case(".git") {
                continue;
            }
            let meta = fs::symlink_metadata(entry.path()).map_err(|e| e.to_string())?;
            if !meta.is_dir() || meta.file_type().is_symlink() {
                paths.push(entry.path());
            }
        }
    }
    paths.sort();
    let mut entries = Vec::new();
    for path in paths {
        let relative = path_text(path.strip_prefix(&repo.root).map_err(|e| e.to_string())?)?;
        valid_relative(relative)?;
        let meta = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        let (mode, permissions, blob) = if meta.file_type().is_symlink() {
            (
                "120000",
                0,
                utf8(
                    repo.input(
                        &["hash-object", "-w", "--no-filters", "--stdin"],
                        &symlink_bytes(&path)?,
                        None,
                    )?,
                    "Symlink hash",
                )?,
            )
        } else if meta.is_file() {
            let mode = permissions(&meta);
            (
                if mode & 0o111 == 0 {
                    "100644"
                } else {
                    "100755"
                },
                mode,
                String::new(),
            )
        } else {
            return Err(format!("Cannot hand off special file: {}", path.display()));
        };
        entries.push(Entry {
            path: relative.into(),
            mode: mode.into(),
            permissions,
            blob: blob.trim().into(),
        });
    }
    let mut regular: Vec<_> = entries
        .iter_mut()
        .filter(|entry| entry.mode != "120000")
        .collect();
    for chunk in regular.chunks_mut(256) {
        let mut args = vec!["hash-object", "-w", "--no-filters", "--"];
        args.extend(chunk.iter().map(|entry| entry.path.as_str()));
        let output = repo.text(&args)?;
        let hashes: Vec<_> = output.lines().collect();
        if hashes.len() != chunk.len() {
            return Err("Git did not hash every handoff file".into());
        }
        for (entry, hash) in chunk.iter_mut().zip(hashes) {
            entry.blob = hash.into();
        }
    }
    let head = repo.head()?.ok_or("A handoff requires a committed HEAD")?;
    let index_bytes =
        fs::read(repo.admin_path("index")?).map_err(|e| format!("Read handoff index: {e}"))?;
    let index_blob = utf8(
        repo.input(
            &["hash-object", "-w", "--no-filters", "--stdin"],
            &index_bytes,
            None,
        )?,
        "Index blob",
    )?
    .trim()
    .into();
    // A tree parent keeps every staged blob reachable during Git GC. The raw
    // index blob alone cannot do that because Git does not follow OIDs in blobs.
    // Only a private index may be refreshed while collecting this tree.
    let private_index = TempIndex(repo.admin_path(&format!("{}.index", id("handoff-image")))?);
    let mut private = private_options()
        .open(&private_index.0)
        .map_err(|e| e.to_string())?;
    private.write_all(&index_bytes).map_err(|e| e.to_string())?;
    drop(private);
    let index_tree = match repo.with_index(&["write-tree"], &private_index.0) {
        Ok(bytes) => utf8(bytes, "Handoff staged tree")?.trim().to_string(),
        Err(error) => {
            if repo.bytes(&["ls-files", "--unmerged", "-z"])?.is_empty() {
                return Err(error);
            }
            // Conflict images are used only to verify our own failed apply.
            // Its original staged blobs already have protected preimage refs.
            repo.text(&["rev-parse", &format!("{head}^{{tree}}")])?
        }
    };
    Ok(Image {
        manifest: Manifest {
            version: 1,
            id: operation.into(),
            head,
            index_tree,
            entries,
            directories,
            root_permissions: permissions(&fs::metadata(&repo.root).map_err(|e| e.to_string())?),
            include_ignored: true,
            ignored_excluded: 0,
        },
        index_blob,
        branch: repo.branch()?,
    })
}

fn stamp(image: &Image) -> Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(image).map_err(|e| e.to_string())?)
    ))
}

fn recovery(repo: &Repo, image: &Image, home: &Path) -> Result<Value> {
    let mut snapshot = create_snapshot(repo, &image.manifest, home, image.branch.clone())?;
    let reference = format!("{}-index", required(&snapshot, "ref")?);
    repo.bytes(&["update-ref", &reference, &image.index_blob])?;
    snapshot["indexRef"] = json!(reference);
    snapshot["indexBlob"] = json!(image.index_blob);
    Ok(snapshot)
}

fn default_branch(repo: &Repo) -> Result<Option<String>> {
    let refs = repo.text(&["for-each-ref", "--format=%(symref)", "refs/remotes/"])?;
    if let Some(reference) = refs.lines().find(|reference| !reference.is_empty()) {
        let name = reference
            .strip_prefix("refs/remotes/")
            .and_then(|s| s.split_once('/'))
            .map(|(_, s)| s);
        if let Some(name) = name {
            if local_head(repo, name)?.is_some() {
                return Ok(Some(name.into()));
            }
        }
    }
    for name in ["main", "master"] {
        if local_head(repo, name)?.is_some() {
            return Ok(Some(name.into()));
        }
    }
    Ok(None)
}

fn local_head(repo: &Repo, branch: &str) -> Result<Option<String>> {
    branch_name(repo, branch)?;
    let reference = format!("refs/heads/{branch}");
    let mut command = git_command(&repo.root, &repo.filters, None);
    command.args(["show-ref", "--verify", "--hash", &reference]);
    let result = execute(command, None, MAX_OUTPUT)?.output;
    if result.status.success() {
        return Ok(Some(utf8(result.stdout, "Branch HEAD")?.trim().into()));
    }
    // show-ref --verify returns 128 for a missing ref. Verify absence separately
    // so permissions/corrupt refs do not get mistaken for a branch to create.
    let refs = repo.text(&["for-each-ref", "--format=%(refname)", &reference])?;
    if refs.lines().all(|line| line != reference) {
        Ok(None)
    } else {
        Err(String::from_utf8_lossy(&result.stderr).trim().into())
    }
}

pub(super) fn public_record(record: &Value) -> Value {
    let mut public = json!({});
    for key in [
        "id",
        "requestId",
        "sourceThreadId",
        "ownerThreadId",
        "status",
        "stage",
        "direction",
        "continueExisting",
        "sourcePath",
        "targetPath",
        "cwd",
        "warnings",
        "error",
        "rollbackError",
        "sourceRecovery",
        "targetRecovery",
        "createdAt",
        "updatedAt",
    ] {
        if let Some(value) = record.get(key) {
            public[key] = value.clone();
        }
    }
    public
}
pub(super) fn inspect(store: &Store, repo: &Repo, state: &mut Value) -> Result<Value> {
    let list = store.list(repo, state)?;
    let local = list["data"]
        .as_array()
        .and_then(|trees| trees.iter().find(|tree| tree["primary"] == true));
    let pending: Vec<_> = state["handoffs"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, item)| {
            item["commonDir"].as_str() == repo.common.to_str()
                && !["committed", "rolledBack"]
                    .iter()
                    .any(|s| item["status"] == *s)
        })
        .map(|(_, item)| public_record(item))
        .collect();
    Ok(
        json!({"repository":list["repository"],"sourceBranch":repo.branch()?,"head":repo.head()?,
        "defaultBranch":default_branch(repo)?,"localCheckout":local,"worktrees":list["data"],
        "branches":branch_list(repo)?["data"],"pending":pending}),
    )
}

/// This is intentionally not a writable workspace RPC. The chat coordinator
/// owns the prepare/commit boundary; clients cannot claim an arbitrary owner.
pub struct PreparedHandoff {
    pub info: Value,
    store: Store,
    operation: String,
    _checkout_leases: Vec<File>,
}

pub async fn prepare_handoff(
    params: Value,
    progress: impl Fn(&str) + Send + 'static,
) -> Result<PreparedHandoff> {
    tokio::task::spawn_blocking(move || {
        let store = Store::configured();
        store.prepare_handoff(&params, progress)
    })
    .await
    .map_err(|e| format!("Handoff worker failed: {e}"))?
}

impl PreparedHandoff {
    pub async fn commit(&self, thread_id: &str) -> Result<Value> {
        let store = self.store.clone();
        let operation = self.operation.clone();
        let thread = thread_id.to_string();
        tokio::task::spawn_blocking(move || store.finish_handoff(&operation, Some(&thread)))
            .await
            .map_err(|e| format!("Commit handoff worker failed: {e}"))?
    }
}

pub async fn handoff_warnings(mut record: Value, warnings: &[String]) -> Result<Value> {
    record["warnings"] = json!(warnings);
    let saved = record.clone();
    tokio::task::spawn_blocking(move || {
        let store = Store::configured();
        store.locked(|state| {
            let operation = required(&saved, "id")?;
            if state["handoffs"][operation]["status"] != "committed" {
                return Err("Handoff is no longer committed".into());
            }
            state["handoffs"][operation]["warnings"] = saved["warnings"].clone();
            store.save(state)
        })
    })
    .await
    .map_err(|e| format!("Save handoff warnings: {e}"))??;
    Ok(public_record(&record))
}

fn dirty(repo: &Repo) -> Result<bool> {
    Ok(!repo
        .bytes(&["status", "--porcelain=v1", "-z", "--untracked-files=all"])?
        .is_empty())
}

fn checkout(repo: &Repo, branch: &str) -> Result<()> {
    branch_name(repo, branch)?;
    repo.bytes(&[
        "switch",
        "--no-guess",
        "--no-overwrite-ignore",
        "--no-recurse-submodules",
        branch,
    ])?;
    Ok(())
}

fn available(repo: &Repo, branch: &str, allowed: &[&Path]) -> Result<()> {
    branch_name(repo, branch)?;
    if let Some(tree) = git_worktrees(repo)?.iter().find(|tree| {
        tree.branch.as_deref() == Some(branch) && !allowed.contains(&tree.path.as_path())
    }) {
        return Err(format!(
            "Branch {branch} is checked out elsewhere: {}",
            tree.path.display()
        ));
    }
    Ok(())
}

struct Transaction<'a> {
    store: &'a Store,
    state: &'a mut Value,
    source: Repo,
    target: Repo,
    id: String,
}
impl Transaction<'_> {
    fn record(&self) -> &Value {
        &self.state["handoffs"][&self.id]
    }
    fn field(&mut self, name: &str, value: Value) {
        self.state["handoffs"][&self.id][name] = value;
    }
    fn save(&self) -> Result<()> {
        self.store.save(self.state)
    }
    fn verify(&self) -> Result<()> {
        if self.record()["continueExisting"] == true {
            // Continuing in an existing checkout does not move files or refs.
            // Other editors can keep editing either checkout during this path.
            return Ok(());
        }
        for (name, repo) in [("source", &self.source), ("target", &self.target)] {
            if name == "target" && self.record()["targetRemoved"] == true {
                if exists(&repo.root) {
                    return Err(
                        "A directory appeared at the removed target; it was retained".into(),
                    );
                }
                continue;
            }
            let actual = stamp(&image(repo, &self.id)?)?;
            if self.record()[format!("{name}Stamp")] != actual {
                return Err(format!("The {name} checkout changed outside this handoff; its files and recovery data were retained: {}", repo.root.display()));
            }
        }
        Ok(())
    }
    fn capture(&mut self) -> Result<()> {
        for (name, repo) in [
            ("source", self.source.clone()),
            ("target", self.target.clone()),
        ] {
            self.field(
                &format!("{name}Stamp"),
                json!(stamp(&image(&repo, &self.id)?)?),
            );
        }
        self.save()
    }
    fn step(
        &mut self,
        stage: &str,
        progress: &impl Fn(&str),
        action: impl FnOnce(&mut Self) -> Result<()>,
    ) -> Result<()> {
        self.verify()?;
        self.field("stage", json!(stage));
        self.save()?;
        progress(stage);
        let result = action(self);
        // Failure may leave an applied conflict. Record its exact files/index
        // before any rollback, rather than treating a failed command as atomic.
        let captured = self.capture();
        match (result, captured) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) => Err(error),
            (result, Err(error)) => Err(format!(
                "{}; cannot record the post-operation state: {error}",
                result
                    .err()
                    .unwrap_or_else(|| "Git operation finished".into())
            )),
        }
    }
    fn stash(&mut self, source: bool) -> Result<()> {
        let repo = if source {
            self.source.clone()
        } else {
            self.target.clone()
        };
        if !dirty(&repo)? {
            return Ok(());
        }
        let before = repo.resolve("refs/stash").ok();
        let result = repo.bytes(&[
            "stash",
            "push",
            "--include-untracked",
            "-m",
            &format!(
                "Mini Codex handoff {} {}",
                self.id,
                if source { "source" } else { "target" }
            ),
        ]);
        let after = repo.resolve("refs/stash").ok();
        if after != before {
            let field = if source { "sourceStash" } else { "targetStash" };
            self.field(field, json!(after));
            self.save()?;
            if let Some(oid) = after.as_deref() {
                // A stash can include edits arriving during Git's own capture.
                // Protect its complete object graph even after list cleanup.
                let reference = format!("refs/mcodex/handoffs/{}/{field}", self.id);
                repo.bytes(&["update-ref", &reference, oid])?;
                self.field(&format!("{field}RecoveryRef"), json!(reference));
                self.save()?;
            }
        }
        result?;
        if dirty(&repo)? {
            return Err(
                "Stashing did not leave a clean checkout; recovery data was retained".into(),
            );
        }
        Ok(())
    }
}

impl Store {
    fn prepare_handoff(&self, params: &Value, progress: impl Fn(&str)) -> Result<PreparedHandoff> {
        self.locked(|state| {
            let cwd = selected_directory(params)?;
            let source = Repo::open(&cwd)?.ok_or("A handoff requires a Git working tree")?;
            let direction = required(params,"direction")?;
            if !["toWorktree","toLocal"].contains(&direction) {return Err("direction must be toWorktree or toLocal".into());}
            let trees = git_worktrees(&source)?;
            let primary = trees.iter().find(|tree| tree.main).ok_or("The local checkout is unavailable")?;
            let is_local = primary.path == source.root;
            if (direction == "toWorktree") != is_local {return Err("The handoff direction does not match this checkout".into());}
            if state["handoffs"].as_object().into_iter().flatten().any(|(_, item)| item["commonDir"].as_str()==source.common.to_str() && item["status"]!="committed" && item["status"]!="rolledBack") {
                return Err("An unfinished handoff exists for this repository; inspect its recovery record first".into());
            }
            let mut checkout_leases=vec![crate::thread_lifecycle::checkout_at(&self.home,&source.root,true)?];
            if bool_param(params,"continueExisting",false)? {
                if direction!="toWorktree" {return Err("continueExisting requires a target worktree".into());}
                return self.prepare_continue(source,params,state,progress,checkout_leases);
            }
            source.ensure_archivable()?;
            let source_head = source.head()?.ok_or("A handoff requires a committed HEAD")?;
            let source_branch = source.branch()?;
            let desired = if direction=="toWorktree" {required(params,"worktreeBranch")?.to_string()}
                else {optional_string(params,"localBranch")?.map(String::from).or(source_branch.clone()).ok_or("Choose a local branch for this detached worktree")?};
            branch_name(&source,&desired)?;
            if direction=="toWorktree" && default_branch(&source)?.as_deref()==Some(&desired) {return Err("Choose a worktree branch other than the default branch".into());}
            let local_branch = if direction=="toWorktree" {
                optional_string(params,"localBranch")?.map(String::from).or(source_branch.clone())
            } else {None};
            if direction=="toWorktree" && source_branch.as_deref()==Some(&desired) && local_branch.as_deref()==Some(&desired) {
                return Err("Choose a different branch for the local checkout before handing off its current branch".into());
            }
            if let Some(local) = local_branch.as_deref() {
                if local_head(&source,local)?.is_none() {return Err("The local checkout branch does not exist".into());}
                available(&source,local,&[&source.root])?;
            }
            let existing = if direction=="toLocal" {Some(primary.path.clone())} else {
                optional_string(params,"targetPath")?.map(|path| PathBuf::from(path).canonicalize().map_err(|e|format!("Resolve target worktree: {e}"))).transpose()?
            };
            if let Some(path) = existing.as_ref() {
                let tree = trees.iter().find(|tree| &tree.path==path && tree.main == (direction=="toLocal")).ok_or("The target is not a matching checkout of this repository")?;
                if tree.locked.is_some() || tree.prunable.is_some() {return Err("The target worktree is locked or unavailable".into());}
                checkout_leases.push(crate::thread_lifecycle::checkout_at(&self.home,path,true)?);
            }
            available(&source,&desired,&[&source.root,existing.as_deref().unwrap_or(&source.root)])?;
            let desired_head = local_head(&source,&desired)?;
            if direction=="toLocal" && desired_head.as_deref().is_some_and(|head| head!=source_head) {
                return Err("The local branch points to a different commit than the source worktree".into());
            }
            let operation = id("handoff");
            let original_source = image(&source,&operation)?;
            let source_recovery = recovery(&source,&original_source,&self.home)?;
            let mut created = None;
            // A new target starts detached. The source may still hold the branch
            // that will be checked out there after its changes have been stashed.
            let target = if let Some(path) = existing {
                progress("reuse-existing-worktree");
                Repo::open(&path)?.ok_or("The target worktree disappeared")?
            } else {
                progress("create-new-worktree");
                let info = self.create(&source,&json!({"name":optional_string(params,"name")?.unwrap_or("Handoff"),"baseRef":"HEAD","detached":true}),state)?["worktree"].clone();
                let repo = Repo::open(&PathBuf::from(required(&info,"path")?))?.ok_or("The new worktree disappeared")?;
                created = Some(info); repo
            };
            if created.is_some() {
                checkout_leases.push(crate::thread_lifecycle::checkout_at(&self.home,&target.root,true).map_err(|e|format!("{e}; the new worktree was retained at {}",target.root.display()))?);
            }
            // All preconditions for an existing target run before touching Git.
            let preparation: Result<(Image,Value,PathBuf)> = (|| {
                target.ensure_archivable()?;
                if target.common!=source.common {return Err("The target belongs to another repository".into());}
                if direction=="toLocal" && dirty(&target)? {return Err("The local checkout has tracked or untracked changes; bring it to a clean state first".into());}
                let relative = source.cwd.strip_prefix(&source.root).map_err(|e| e.to_string())?;
                // A non-existing subdirectory cannot be silently replaced by the
                // repository root in the destination conversation.
                let target_cwd = target.root.join(relative);
                if !relative.as_os_str().is_empty() && source.text(&["cat-file","-t",&format!("{}:{}",desired_head.as_deref().unwrap_or(&source_head),path_text(relative)?)])? != "tree" {
                    return Err("The chat subdirectory does not exist on the destination branch".into());
                }
                let original_target = image(&target,&operation)?;
                let target_recovery = recovery(&target,&original_target,&self.home)?;
                // Confirm that neither initial image raced another editor/Git
                // operation before making a mutation.
                if stamp(&image(&source,&operation)?)?!=stamp(&original_source)? || stamp(&image(&target,&operation)?)?!=stamp(&original_target)? {return Err("A checkout changed while preparing the handoff; retry when it is idle".into());}
                Ok((original_target,target_recovery,target_cwd))
            })();
            let (original_target,target_recovery,target_cwd) = match preparation {
                Ok(value)=>value,
                Err(mut error)=>{
                    if let Some(info)=created.as_ref() {
                        // The fresh target is clean and still owned by this call.
                        // Existing targets are never removed by preflight cleanup.
                        if let Err(cleanup)=rollback_created_worktree(&source,&target.root,None,&source_head) {error.push_str(&format!("; new worktree retained: {cleanup}"));}
                        else {state["worktrees"].as_object_mut().unwrap().remove(required(info,"id")?);if let Err(cleanup)=self.save(state){error.push_str(&format!("; registry cleanup: {cleanup}"));}}
                    }
                    return Err(error);
                }
            };
            if !state["handoffs"].is_object() {state["handoffs"]=json!({});}
            state["handoffs"][&operation]=json!({"id":operation,"requestId":params["handoffRequestId"],"sourceThreadId":params["threadId"],"status":"preparing","stage":"prepared","direction":direction,
                "commonDir":source.common,"sourcePath":source.root,"sourceCwd":source.cwd,"targetPath":target.root,"cwd":target_cwd,
                "sourceBefore":original_source,"targetBefore":original_target,"sourceRecovery":source_recovery,"targetRecovery":target_recovery,
                "sourceStamp":stamp(&original_source)?,"targetStamp":stamp(&original_target)?,"sourceStash":null,"targetStash":null,
                "createdWorktree":created,"branch":desired,"createdBranch":desired_head.is_none(),"branchCreationAttempted":false,"createdAt":now_ms(),"warnings":[]});
            let mut tx=Transaction{store:self,state,source,target,id:operation.clone()};
            let result: Result<()>=(|| {
                tx.save()?;
                tx.step("stash-source-changes",&progress,|tx|tx.stash(true))?;
                if direction=="toWorktree" {
                    tx.step("checkout-local-branch",&progress,|tx| {
                        if let Some(branch)=local_branch.as_deref() {if tx.source.branch()?.as_deref()!=Some(branch){checkout(&tx.source,branch)?;}}
                        Ok(())
                    })?;
                    tx.step("stash-target-worktree-changes",&progress,|tx|tx.stash(false))?;
                } else {
                    tx.step("detach-worktree-branch",&progress,|tx|{tx.source.bytes(&["switch","--detach","--no-overwrite-ignore","--no-recurse-submodules",&source_head])?;Ok(())})?;
                }
                tx.step(if direction=="toWorktree"{"checkout-worktree-branch"}else{"checkout-local-branch"},&progress,|tx| {
                    if desired_head.is_none() {
                        tx.field("branchCreationAttempted",json!(true));tx.save()?;
                        tx.target.bytes(&["branch","--no-track",&desired,&source_head])?;
                    }
                    checkout(&tx.target,&desired)
                })?;
                tx.step(if direction=="toWorktree"{"apply-changes-to-worktree"}else{"apply-changes-to-local"},&progress,|tx|{
                    if let Some(stash)=tx.record()["sourceStash"].as_str(){tx.target.bytes(&["stash","apply",stash])?;}
                    Ok(())
                })?;
                if !target_cwd.is_dir() || !target_cwd.canonicalize().is_ok_and(|path|path.starts_with(&tx.target.root)) {return Err("The destination chat directory is unavailable or escaped the worktree after applying changes".into());}
                tx.field("status",json!("prepared"));tx.field("stage",json!("switching-thread"));tx.save()?;progress("switching-thread");Ok(())
            })();
            if let Err(error)=result {
                tx.field("error",json!(error));
                let rollback=rollback_transaction(&mut tx);
                return Err(match rollback {Ok(_)=>format!("Handoff failed and was rolled back: {error}; recovery record {} was retained",tx.id),Err(rollback)=>format!("Handoff failed: {error}; {rollback}; recovery record {} was retained",tx.id)});
            }
            Ok(PreparedHandoff{info:tx.record().clone(),store:self.clone(),operation,_checkout_leases:checkout_leases})
        })
    }

    fn finish_handoff(&self, operation: &str, owner: Option<&str>) -> Result<Value> {
        self.finish_handoff_inner(operation, owner, None, true)
    }

    fn prepare_continue(
        &self,
        source: Repo,
        params: &Value,
        state: &mut Value,
        progress: impl Fn(&str),
        mut checkout_leases: Vec<File>,
    ) -> Result<PreparedHandoff> {
        let target_path = PathBuf::from(required(params, "targetPath")?)
            .canonicalize()
            .map_err(|e| format!("Resolve target worktree: {e}"))?;
        if !git_worktrees(&source)?
            .iter()
            .any(|tree| tree.path == target_path && !tree.main && tree.prunable.is_none())
        {
            return Err("Select an existing worktree of this repository".into());
        }
        let target = Repo::open(&target_path)?.ok_or("The target worktree disappeared")?;
        if target.common != source.common {
            return Err("The target belongs to another repository".into());
        }
        checkout_leases.push(crate::thread_lifecycle::checkout_at(
            &self.home,
            &target.root,
            true,
        )?);
        let relative = source
            .cwd
            .strip_prefix(&source.root)
            .map_err(|e| e.to_string())?;
        let cwd = target
            .root
            .join(relative)
            .canonicalize()
            .map_err(|e| format!("The destination chat directory is unavailable: {e}"))?;
        if !cwd.is_dir() || !cwd.starts_with(&target.root) {
            return Err("The destination chat directory escaped the worktree".into());
        }
        let operation = id("handoff");
        if !state["handoffs"].is_object() {
            state["handoffs"] = json!({});
        }
        state["handoffs"][&operation] = json!({"id":operation,"requestId":params["handoffRequestId"],"sourceThreadId":params["threadId"],"status":"prepared","stage":"switching-thread","direction":"toWorktree","continueExisting":true,
            "commonDir":source.common,"sourcePath":source.root,"sourceCwd":source.cwd,"targetPath":target.root,"cwd":cwd,"branch":target.branch()?,
            "createdWorktree":null,"createdBranch":false,"sourceStash":null,"targetStash":null,"createdAt":now_ms(),"warnings":[]});
        self.save(state)?;
        progress("reuse-existing-worktree");
        progress("switching-thread");
        Ok(PreparedHandoff {
            info: state["handoffs"][&operation].clone(),
            store: self.clone(),
            operation,
            _checkout_leases: checkout_leases,
        })
    }
    pub(super) fn finish_handoff_checked(
        &self,
        operation: &str,
        owner: Option<&str>,
        common: Option<&Path>,
    ) -> Result<Value> {
        self.finish_handoff_inner(operation, owner, common, false)
    }

    fn finish_handoff_inner(
        &self,
        operation: &str,
        owner: Option<&str>,
        common: Option<&Path>,
        leases_held: bool,
    ) -> Result<Value> {
        self.locked(|state| {
            let record=state["handoffs"].get(operation).filter(|record|record.is_object()).cloned().ok_or("Unknown handoff operation")?;
            if common.is_some_and(|common|record["commonDir"].as_str()!=common.to_str()){return Err("This handoff belongs to another repository".into());}
            let expected=if owner.is_some(){"committed"}else{"rolledBack"};
            if record["status"]==expected {
                if owner.is_some_and(|owner|record["ownerThreadId"]!=owner){return Err("This handoff already belongs to another chat".into());}
                return Ok(record);
            }
            if record["status"]!="prepared" && (owner.is_some() || !["preparing","recoveryRequired"].iter().any(|status|record["status"]==*status)) {return Err("The handoff is not prepared; inspect its recovery record".into());}
            let source=Repo::open(&PathBuf::from(required(&record,"sourcePath")?))?.ok_or("The source checkout disappeared")?;
            if source.common.to_str()!=record["commonDir"].as_str(){return Err("The source repository changed; recovery data was retained".into());}
            let path=PathBuf::from(required(&record,"targetPath")?);
            let target=if record["targetRemoved"]==true && !exists(&path) {Repo{root:path.clone(),cwd:path,common:source.common.clone(),filters:source.filters.clone()}}
                else {Repo::open(&path)?.ok_or("The target checkout disappeared")?};
            if target.common!=source.common{return Err("The target repository changed; recovery data was retained".into());}
            let _checkout_leases=if leases_held{Vec::new()}else{
                let mut leases=vec![crate::thread_lifecycle::checkout_at(&self.home,&source.root,true)?];
                if target.root.exists(){leases.push(crate::thread_lifecycle::checkout_at(&self.home,&target.root,true)?);}
                leases
            };
            let mut tx=Transaction{store:self,state,source,target,id:operation.into()};
            if let Some(owner)=owner {
                if owner.trim().is_empty() || owner.len()>256 {return Err("Invalid handoff owner".into());}
                tx.verify()?;
                let managed=tx.state["worktrees"].as_object().unwrap().iter().find(|(_,tree)|tree["path"]==tx.record()["targetPath"] && tree["commonDir"]==tx.record()["commonDir"]).map(|(id,_)|id.clone());
                if let Some(id)=managed {
                    tx.state["worktrees"][&id]["ownerThreadId"]=json!(owner);
                    tx.state["worktrees"][&id]["updatedAt"]=json!(now_ms());
                }
                let mut warnings=Vec::new();
                if let Some(stash)=tx.record()["sourceStash"].as_str() {
                    if let Err(error)=drop_stash(&tx.source,stash){warnings.push(format!("Source changes were applied, but their recovery stash was retained: {error}"));}
                }
                if let Some(stash)=tx.record()["targetStash"].as_str(){warnings.push(format!("The target worktree's previous changes were saved in Git stash {stash}"));}
                tx.field("ownerThreadId",json!(owner));tx.field("warnings",json!(warnings));tx.field("status",json!("committed"));tx.field("stage",json!("ready"));tx.field("updatedAt",json!(now_ms()));tx.save()?;
                Ok(tx.record().clone())
            } else {rollback_transaction(&mut tx)}
        })
    }
}

fn drop_stash(repo: &Repo, oid: &str) -> Result<()> {
    // Resolve by object ID each time. A concurrent stash push changes ordinals.
    let listing = repo.text(&["stash", "list", "--format=%H%x00%gd"])?;
    if let Some(reference) = listing.lines().find_map(|line| {
        line.split_once('\0')
            .filter(|(hash, _)| *hash == oid)
            .map(|(_, reference)| reference)
    }) {
        repo.bytes(&["stash", "drop", reference])?;
    }
    Ok(())
}

fn restore(repo: &Repo, original: &Image, expected: &str, operation: &str) -> Result<()> {
    let current = image(repo, operation)?;
    if stamp(&current)? != expected {
        return Err(format!(
            "Checkout changed after handoff; its files were retained: {}",
            repo.root.display()
        ));
    }
    if let Some(branch) = original.branch.as_deref() {
        if local_head(repo, branch)?.as_deref() != Some(&original.manifest.head) {
            return Err(format!(
                "Original branch {branch} moved; recovery files were retained"
            ));
        }
        available(repo, branch, &[&repo.root])?;
    }
    // Both preimages have durable Git refs. Never clean an arbitrary directory:
    // remove only the exact, verified inventory produced by this transaction.
    // This also handles stash-apply conflicts without losing the original index.
    apply_permissions(&repo.root, 0o700)?;
    for directory in &current.manifest.directories {
        apply_permissions(&repo.root.join(&directory.path), 0o700)?;
    }
    for entry in &current.manifest.entries {
        fs::remove_file(repo.root.join(&entry.path))
            .map_err(|e| format!("Restore {}: {e}", entry.path))?;
    }
    let mut directories = current.manifest.directories.clone();
    directories.sort_by_key(|directory| std::cmp::Reverse(directory.path.matches('/').count()));
    for directory in directories {
        fs::remove_dir(repo.root.join(directory.path))
            .map_err(|e| format!("Restore directory: {e}"))?;
    }
    if let Some(branch) = original.branch.as_deref() {
        repo.bytes(&["symbolic-ref", "HEAD", &format!("refs/heads/{branch}")])?;
    } else {
        repo.bytes(&["update-ref", "--no-deref", "HEAD", &original.manifest.head])?;
    }
    materialize_snapshot(repo, &original.manifest)?;
    let index = repo.admin_path("index")?;
    let temporary = index.with_file_name(format!("index-{}.tmp", id("handoff")));
    let result: Result<()> = (|| {
        let mut file = private_options()
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        file.write_all(&repo.bytes(&["cat-file", "blob", &original.index_blob])?)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        drop(file);
        fs::rename(&temporary, &index).map_err(|e| e.to_string())?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result?;
    if stamp(&image(repo, operation)?)? != stamp(original)? {
        return Err("Restored checkout did not match its recovery image".into());
    }
    Ok(())
}

fn rollback_transaction(tx: &mut Transaction<'_>) -> Result<Value> {
    if tx.record()["continueExisting"] == true {
        tx.field("status", json!("rolledBack"));
        tx.field("stage", json!("rolled-back"));
        tx.save()?;
        return Ok(tx.record().clone());
    }
    let attempt: Result<()> = (|| {
        tx.verify()?;
        let source: Image = serde_json::from_value(tx.record()["sourceBefore"].clone())
            .map_err(|e| e.to_string())?;
        let target: Image = serde_json::from_value(tx.record()["targetBefore"].clone())
            .map_err(|e| e.to_string())?;
        let source_stamp = required(tx.record(), "sourceStamp")?.to_string();
        let target_stamp = required(tx.record(), "targetStamp")?.to_string();
        tx.field("stage", json!("rolling-back"));
        tx.save()?;
        if tx.record()["targetRestored"] == true {
            // A prior rollback can stop after restoring just one checkout. The
            // journal lets a restart verify and resume the remaining steps.
        } else if tx.record()["createdWorktree"].is_object() {
            let info = tx.record()["createdWorktree"].clone();
            let id = required(&info, "id")?;
            let record = tx.store.owned(&tx.source, &json!({"id":id}), tx.state)?;
            if record
                .get("ownerThreadId")
                .is_some_and(|owner| !owner.is_null())
            {
                return Err("The new worktree acquired a chat owner; it was retained".into());
            }
            rollback_created_worktree(&tx.source, &tx.target.root, None, &target.manifest.head)?;
            tx.state["worktrees"].as_object_mut().unwrap().remove(id);
            tx.field("targetRemoved", json!(true));
        } else {
            restore(&tx.target, &target, &target_stamp, &tx.id)?;
            tx.field("targetStamp", json!(stamp(&image(&tx.target, &tx.id)?)?));
        }
        tx.field("targetRestored", json!(true));
        tx.save()?;
        // Target first releases the branch that originally belonged to source.
        if tx.record()["sourceRestored"] != true {
            restore(&tx.source, &source, &source_stamp, &tx.id)?;
            tx.field("sourceStamp", json!(stamp(&image(&tx.source, &tx.id)?)?));
            tx.field("sourceRestored", json!(true));
            tx.save()?;
        }
        if tx.record()["createdBranch"] == true && tx.record()["branchCreationAttempted"] == true {
            let branch = required(tx.record(), "branch")?;
            if let Some(head) = local_head(&tx.source, branch)? {
                if head != source.manifest.head {
                    return Err("The newly created branch moved; it was retained".into());
                }
                available(&tx.source, branch, &[])?;
                tx.source
                    .bytes(&["update-ref", "-d", &format!("refs/heads/{branch}"), &head])?;
            }
        }
        for field in ["sourceStash", "targetStash"] {
            if let Some(stash) = tx.record()[field].as_str() {
                // If creating the private recovery ref failed, keep the normal
                // stash entry. Its contents may exceed the initial preimage.
                if tx.record()[format!("{field}RecoveryRef")].is_string() {
                    drop_stash(&tx.source, stash)?;
                }
            }
        }
        Ok(())
    })();
    match attempt {
        Ok(()) => {
            tx.field("status", json!("rolledBack"));
            tx.field("stage", json!("rolled-back"));
            tx.save()?;
            Ok(tx.record().clone())
        }
        Err(error) => {
            tx.field("status", json!("recoveryRequired"));
            tx.field("rollbackError", json!(error));
            let saved = tx.save();
            Err(format!(
                "Rollback could not finish: {error}{}",
                saved
                    .err()
                    .map(|e| format!("; persist recovery record: {e}"))
                    .unwrap_or_default()
            ))
        }
    }
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
            let root = std::env::temp_dir().join(id("mc-handoff-test"));
            fs::create_dir_all(&root).unwrap();
            let root = root.canonicalize().unwrap();
            let repo = root.join("repo");
            fs::create_dir_all(repo.join("nested")).unwrap();
            checked(&repo, &[], &["init", "-b", "main"], None, None).unwrap();
            fs::write(repo.join("file"), b"base\n").unwrap();
            fs::write(repo.join("nested/file"), b"nested\n").unwrap();
            fs::write(repo.join(".gitignore"), b"ignored\nbuild/\n").unwrap();
            checked(&repo, &[], &["add", "."], None, None).unwrap();
            checked(&repo, &[], &["commit", "-m", "base"], None, None).unwrap();
            checked(&repo, &[], &["switch", "-c", "feat/source"], None, None).unwrap();
            Self {
                store: Store {
                    home: root.join("home"),
                },
                root,
                repo,
            }
        }
        fn git(&self, cwd: &Path, args: &[&str]) -> String {
            utf8(checked(cwd, &[], args, None, None).unwrap(), "Test Git")
                .unwrap()
                .trim()
                .into()
        }
        fn dirty(&self, cwd: &Path) {
            fs::write(cwd.join("file"), b"staged\n").unwrap();
            self.git(cwd, &["add", "file"]);
            fs::write(cwd.join("file"), b"working\n").unwrap();
            fs::write(cwd.join("untracked"), [0, 255, 13, 10]).unwrap();
            fs::write(cwd.join("ignored"), b"private ignored\n").unwrap();
            fs::create_dir_all(cwd.join("build/empty")).unwrap();
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink("file", cwd.join("link")).unwrap();
                apply_permissions(&cwd.join("untracked"), 0o750).unwrap();
            }
        }
        fn params(&self) -> Value {
            json!({"cwd":self.repo,"direction":"toWorktree","worktreeBranch":"feat/source","localBranch":"main"})
        }
        fn prepare(&self, p: Value) -> Result<PreparedHandoff> {
            self.store.prepare_handoff(&p, |_| {})
        }
        fn image(&self, cwd: &Path) -> Image {
            image(&Repo::open(cwd).unwrap().unwrap(), "test-image").unwrap()
        }
        fn target(&self) -> PathBuf {
            let repo = Repo::open(&self.repo).unwrap().unwrap();
            self.store
                .locked(|state| {
                    self.store
                        .create(&repo, &json!({"branch":"feat/target"}), state)
                })
                .unwrap()["worktree"]["path"]
                .as_str()
                .map(PathBuf::from)
                .unwrap()
        }
        fn finish(&self, tx: &PreparedHandoff, owner: Option<&str>) -> Result<Value> {
            self.store.finish_handoff(&tx.operation, owner)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    fn equal(left: &Image, right: &Image) {
        let mut left = left.clone();
        left.manifest.id = right.manifest.id.clone();
        assert_eq!(
            stamp(&left).unwrap(),
            stamp(right).unwrap(),
            "workspace/index/branch differed"
        );
    }

    #[test]
    fn handoff_moves_current_branch_and_changes_to_new_worktree() {
        let f = Fixture::new();
        f.dirty(&f.repo);
        let mut p = f.params();
        p["cwd"] = json!(f.repo.join("nested"));
        let tx = f.prepare(p).unwrap();
        let target = PathBuf::from(required(&tx.info, "targetPath").unwrap());
        assert_eq!(tx.info["cwd"], json!(target.join("nested")));
        assert_eq!(f.git(&f.repo, &["branch", "--show-current"]), "main");
        assert_eq!(f.git(&target, &["branch", "--show-current"]), "feat/source");
        assert!(!dirty(&Repo::open(&f.repo).unwrap().unwrap()).unwrap());
        assert_eq!(fs::read(f.repo.join("file")).unwrap(), b"base\n");
        assert!(!f.repo.join("untracked").exists());
        assert_eq!(fs::read(target.join("file")).unwrap(), b"working\n");
        assert_eq!(
            fs::read(target.join("untracked")).unwrap(),
            [0, 255, 13, 10]
        );
        assert!(f
            .git(&target, &["diff", "--cached", "--name-only"])
            .is_empty());
        assert!(!target.join("ignored").exists());
        assert!(f.repo.join("ignored").is_file());
        assert!(tx.info["sourceRecovery"]["indexRef"].is_string());
        let result = f.finish(&tx, Some("new-chat")).unwrap();
        assert_eq!(result["status"], "committed");
        assert!(f.git(&f.repo, &["stash", "list"]).is_empty());
        assert_eq!(
            f.finish(&tx, Some("new-chat")).unwrap()["ownerThreadId"],
            "new-chat"
        );
        assert!(f.finish(&tx, Some("wrong-owner")).is_err());
    }

    #[test]
    fn handoff_rollback_restores_raw_index_files_permissions_and_branch() {
        let f = Fixture::new();
        f.dirty(&f.repo);
        let before = f.image(&f.repo);
        let tx = f.prepare(f.params()).unwrap();
        let target = PathBuf::from(required(&tx.info, "targetPath").unwrap());
        let result = f.finish(&tx, None).unwrap();
        assert_eq!(result["status"], "rolledBack");
        equal(&before, &f.image(&f.repo));
        assert!(!target.exists());
        assert!(f.git(&f.repo, &["stash", "list"]).is_empty());
        assert_eq!(
            f.git(&f.repo, &["worktree", "list", "--porcelain"])
                .matches("worktree ")
                .count(),
            1
        );
    }

    #[test]
    fn handoff_reuses_dirty_target_and_keeps_its_stash_after_commit() {
        let f = Fixture::new();
        let target = f.target();
        fs::write(target.join("target-only"), b"target edits\n").unwrap();
        f.dirty(&f.repo);
        let mut p = f.params();
        p["targetPath"] = json!(target);
        let tx = f.prepare(p).unwrap();
        assert!(!target.join("target-only").exists());
        assert_eq!(fs::read(target.join("file")).unwrap(), b"working\n");
        let target_stash = required(&tx.info, "targetStash").unwrap();
        assert_eq!(
            f.git(&f.repo, &["show", &format!("{target_stash}^3:target-only")]),
            "target edits"
        );
        let result = f.finish(&tx, Some("target-chat")).unwrap();
        assert_eq!(result["warnings"].as_array().unwrap().len(), 1);
        assert!(f
            .git(&f.repo, &["stash", "list", "--format=%H"])
            .lines()
            .any(|oid| oid == target_stash));
        assert!(!f
            .git(&f.repo, &["stash", "list", "--format=%H"])
            .lines()
            .any(|oid| Some(oid) == tx.info["sourceStash"].as_str()));
    }

    #[test]
    fn handoff_reuse_rollback_restores_both_sides() {
        let f = Fixture::new();
        let target = f.target();
        f.dirty(&f.repo);
        f.dirty(&target);
        let source_before = f.image(&f.repo);
        let target_before = f.image(&target);
        let mut p = f.params();
        p["worktreeBranch"] = json!("feat/new");
        p["localBranch"] = Value::Null;
        p["targetPath"] = json!(target);
        let tx = f.prepare(p).unwrap();
        f.finish(&tx, None).unwrap();
        equal(&source_before, &f.image(&f.repo));
        equal(&target_before, &f.image(&target));
        assert!(
            local_head(&Repo::open(&f.repo).unwrap().unwrap(), "feat/new")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn handoff_conflict_rolls_back_both_raw_preimages_and_retains_old_stash() {
        let f = Fixture::new();
        let target = f.target();
        fs::write(target.join("file"), b"conflicting branch\n").unwrap();
        f.git(&target, &["add", "file"]);
        f.git(&target, &["commit", "-m", "diverge"]);
        fs::write(target.join("target-only"), b"target prior work\n").unwrap();
        f.dirty(&f.repo);
        let source_before = f.image(&f.repo);
        let target_before = f.image(&target);
        let mut p = f.params();
        p["worktreeBranch"] = json!("feat/target");
        p["targetPath"] = json!(target);
        let error = f.prepare(p).err().unwrap();
        assert!(error.contains("rolled back"), "{error}");
        equal(&source_before, &f.image(&f.repo));
        equal(&target_before, &f.image(&target));
        assert!(f.git(&f.repo, &["stash", "list"]).is_empty());
    }

    #[test]
    fn handoff_back_to_local_detaches_source_and_preserves_chat_subdirectory() {
        let f = Fixture::new();
        let outbound = f.prepare(f.params()).unwrap();
        f.finish(&outbound, Some("worktree-chat")).unwrap();
        let target = PathBuf::from(required(&outbound.info, "targetPath").unwrap());
        // The coordinator releases these leases only after its final metadata
        // write. End that completed operation before starting the return trip.
        drop(outbound);
        f.dirty(&target);
        let tx = f
            .prepare(json!({"cwd":target.join("nested"),"direction":"toLocal"}))
            .unwrap();
        assert_eq!(tx.info["cwd"], json!(f.repo.join("nested")));
        assert!(f.git(&target, &["branch", "--show-current"]).is_empty());
        assert_eq!(f.git(&f.repo, &["branch", "--show-current"]), "feat/source");
        assert_eq!(fs::read(f.repo.join("file")).unwrap(), b"working\n");
        assert!(!target.join("untracked").exists());
        assert!(target.join("ignored").exists());
        f.finish(&tx, Some("local-chat")).unwrap();
    }

    #[test]
    fn handoff_to_local_rejects_dirty_checkout_and_branch_head_mismatch() {
        let f = Fixture::new();
        let target = f.target();
        fs::write(f.repo.join("local-only"), b"keep\n").unwrap();
        let before = f.image(&f.repo);
        let target_before = f.image(&target);
        let p = json!({"cwd":target,"direction":"toLocal"});
        assert!(f
            .prepare(p.clone())
            .err()
            .unwrap()
            .contains("tracked or untracked"));
        equal(&before, &f.image(&f.repo));
        equal(&target_before, &f.image(&target));
        fs::remove_file(f.repo.join("local-only")).unwrap();
        f.git(&f.repo, &["branch", "feat/mismatch"]);
        fs::write(target.join("file"), b"new commit\n").unwrap();
        f.git(&target, &["add", "file"]);
        f.git(&target, &["commit", "-m", "different"]);
        let mut mismatch = p;
        mismatch["localBranch"] = json!("feat/mismatch");
        assert!(f
            .prepare(mismatch)
            .err()
            .unwrap()
            .contains("different commit"));
    }

    #[test]
    fn handoff_retains_new_edits_and_blocks_an_overlapping_transaction() {
        let f = Fixture::new();
        let tx = f.prepare(f.params()).unwrap();
        let target = PathBuf::from(required(&tx.info, "targetPath").unwrap());
        fs::write(target.join("ignored"), b"new edits after prepare\n").unwrap();
        let error = f.finish(&tx, None).unwrap_err();
        assert!(error.contains("changed outside"), "{error}");
        assert!(target.exists());
        assert_eq!(
            fs::read(target.join("ignored")).unwrap(),
            b"new edits after prepare\n"
        );
        let mut p = f.params();
        p["worktreeBranch"] = json!("feat/another");
        drop(tx);
        assert!(f.prepare(p).err().unwrap().contains("unfinished handoff"));
        let repo = Repo::open(&target).unwrap().unwrap();
        let pending = f
            .store
            .locked(|state| inspect(&f.store, &repo, state))
            .unwrap();
        assert_eq!(pending["pending"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn handoff_validates_default_branch_and_branches_held_elsewhere_before_mutation() {
        let f = Fixture::new();
        let target = f.target();
        let before = f.image(&f.repo);
        let mut p = f.params();
        p["worktreeBranch"] = json!("main");
        assert!(f.prepare(p).err().unwrap().contains("default branch"));
        let mut p = f.params();
        p["worktreeBranch"] = json!("feat/target");
        assert!(f.prepare(p).err().unwrap().contains("elsewhere"));
        let mut p = f.params();
        p["localBranch"] = json!("feat/source");
        assert!(f.prepare(p).err().unwrap().contains("different branch"));
        let mut p = f.params();
        p["targetPath"] = json!(f.root);
        assert!(f.prepare(p).err().unwrap().contains("matching checkout"));
        equal(&before, &f.image(&f.repo));
        assert!(target.exists());
    }

    #[test]
    fn handoff_resolves_stash_ids_after_other_stashes_are_pushed() {
        let f = Fixture::new();
        f.dirty(&f.repo);
        let tx = f.prepare(f.params()).unwrap();
        fs::write(f.repo.join("later"), b"keep newer stash\n").unwrap();
        f.git(
            &f.repo,
            &["stash", "push", "-u", "-m", "unrelated newer stash"],
        );
        // Pushing a stash leaves the same files/index, so committing the prepared
        // transfer is valid. Its source stash must not be confused with stash@{0}.
        f.finish(&tx, Some("chat")).unwrap();
        let list = f.git(&f.repo, &["stash", "list"]);
        assert!(list.contains("unrelated newer stash"));
        assert!(!list.contains("Mini Codex handoff"));
    }

    #[test]
    fn handoff_recovery_keeps_original_staged_blobs_reachable_after_gc() {
        let f = Fixture::new();
        f.dirty(&f.repo);
        let staged = f.git(&f.repo, &["rev-parse", ":file"]);
        let tx = f.prepare(f.params()).unwrap();
        f.finish(&tx, Some("chat")).unwrap();
        let stash = required(&tx.info, "sourceStash").unwrap();
        let stash_ref = required(&tx.info, "sourceStashRecoveryRef").unwrap();
        f.git(&f.repo, &["reflog", "expire", "--expire=now", "--all"]);
        f.git(&f.repo, &["gc", "--prune=now"]);
        assert_eq!(f.git(&f.repo, &["rev-parse", stash_ref]), stash);
        // Verify the raw preimage's staged tree independently of the stash.
        f.git(&f.repo, &["update-ref", "-d", stash_ref]);
        f.git(&f.repo, &["reflog", "expire", "--expire=now", "--all"]);
        f.git(&f.repo, &["gc", "--prune=now"]);
        assert_eq!(f.git(&f.repo, &["cat-file", "blob", &staged]), "staged");
        let reference = required(&tx.info["sourceRecovery"], "indexRef").unwrap();
        assert_eq!(
            f.git(&f.repo, &["rev-parse", reference]),
            tx.info["sourceBefore"]["index_blob"].as_str().unwrap()
        );
    }

    #[test]
    fn handoff_prepared_recovery_can_resume_from_disk_and_checks_repository() {
        let f = Fixture::new();
        let other = Fixture::new();
        f.dirty(&f.repo);
        let before = f.image(&f.repo);
        let tx = f.prepare(f.params()).unwrap();
        let operation = tx.operation.clone();
        drop(tx);
        let reopened = Store {
            home: f.store.home.clone(),
        };
        let other_repo = Repo::open(&other.repo).unwrap().unwrap();
        assert!(reopened
            .finish_handoff_checked(&operation, None, Some(&other_repo.common))
            .unwrap_err()
            .contains("another repository"));
        let result = reopened
            .rpc(
                "workspace/handoff/rollback",
                &json!({"cwd":f.repo,"operationId":operation}),
            )
            .unwrap();
        assert_eq!(result["status"], "rolledBack");
        equal(&before, &f.image(&f.repo));
        assert_eq!(
            reopened
                .rpc(
                    "workspace/handoff/rollback",
                    &json!({"cwd":f.repo,"operationId":operation})
                )
                .unwrap()["status"],
            "rolledBack"
        );
    }

    #[test]
    #[ignore = "launched by handoff_exit_checkpoints_restore_both_checkouts"]
    fn handoff_exit_worker() {
        let path = std::env::var_os("MCODEX_HANDOFF_EXIT_FIXTURE").unwrap();
        let input: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        let store = Store {
            home: PathBuf::from(input["home"].as_str().unwrap()),
        };
        store
            .prepare_handoff(&input["params"], |stage| {
                if input["stage"] == stage {
                    std::process::exit(71);
                }
            })
            .unwrap();
        panic!("The requested exit checkpoint was not reached");
    }

    #[test]
    fn handoff_exit_checkpoints_restore_both_checkouts() {
        for (local, stages) in [
            (
                false,
                &[
                    "stash-source-changes",
                    "checkout-local-branch",
                    "stash-target-worktree-changes",
                    "checkout-worktree-branch",
                    "apply-changes-to-worktree",
                    "switching-thread",
                ][..],
            ),
            (
                true,
                &[
                    "stash-source-changes",
                    "detach-worktree-branch",
                    "checkout-local-branch",
                    "apply-changes-to-local",
                    "switching-thread",
                ][..],
            ),
        ] {
            for stage in stages {
                let f = Fixture::new();
                f.dirty(&f.repo);
                let params = if local {
                    let tx = f.prepare(f.params()).unwrap();
                    let target = tx.info["targetPath"].clone();
                    f.finish(&tx, Some("fixture-chat")).unwrap();
                    drop(tx);
                    json!({"cwd":target,"direction":"toLocal","localBranch":"feat/source"})
                } else {
                    f.params()
                };
                let source = PathBuf::from(params["cwd"].as_str().unwrap());
                let before = f.image(&source);
                let local_before = local.then(|| f.image(&f.repo));
                let input = f.root.join("exit-fixture.json");
                fs::write(
                    &input,
                    serde_json::to_vec(&json!({"home":f.store.home,"params":params,"stage":stage}))
                        .unwrap(),
                )
                .unwrap();
                let output = Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "workspaces::handoff::tests::handoff_exit_worker",
                        "--ignored",
                        "--nocapture",
                    ])
                    .env("MCODEX_HANDOFF_EXIT_FIXTURE", &input)
                    .output()
                    .unwrap();
                assert_eq!(
                    output.status.code(),
                    Some(71),
                    "checkpoint {stage}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let state = f.store.load().unwrap();
                let record = state["handoffs"]
                    .as_object()
                    .unwrap()
                    .values()
                    .find(|record| record["status"] != "committed")
                    .unwrap();
                let operation = record["id"].clone();
                let target = PathBuf::from(record["targetPath"].as_str().unwrap());
                let recovered = f
                    .store
                    .rpc(
                        "workspace/handoff/rollback",
                        &json!({"cwd":source,"operationId":operation}),
                    )
                    .unwrap();
                assert_eq!(recovered["status"], "rolledBack", "checkpoint {stage}");
                equal(&before, &f.image(&source));
                if let Some(expected) = local_before {
                    equal(&expected, &f.image(&f.repo));
                } else {
                    assert!(
                        !target.exists(),
                        "checkpoint {stage} left its owned empty checkout"
                    );
                }
                assert!(
                    crate::thread_lifecycle::checkout_at(&f.store.home, &source, false).is_ok()
                );
            }
        }
    }

    #[test]
    fn handoff_and_recovery_respect_both_checkout_execution_leases() {
        let f = Fixture::new();
        f.dirty(&f.repo);
        let target = f.target();
        let before = stamp(&f.image(&f.repo)).unwrap();
        let target_before = stamp(&f.image(&target)).unwrap();
        let mut params = f.params();
        params["targetPath"] = json!(target);
        let source_execution =
            crate::thread_lifecycle::checkout_at(&f.store.home, &f.repo, false).unwrap();
        assert!(f
            .prepare(params.clone())
            .err()
            .unwrap()
            .contains("running responses"));
        drop(source_execution);
        let target_execution =
            crate::thread_lifecycle::checkout_at(&f.store.home, &target, false).unwrap();
        assert!(f
            .prepare(params.clone())
            .err()
            .unwrap()
            .contains("running responses"));
        drop(target_execution);
        assert_eq!(stamp(&f.image(&f.repo)).unwrap(), before);
        assert_eq!(stamp(&f.image(&target)).unwrap(), target_before);
        let tx = f.prepare(params).unwrap();
        let operation = tx.operation.clone();
        assert!(crate::thread_lifecycle::checkout_at(&f.store.home, &f.repo, false).is_err());
        assert!(crate::thread_lifecycle::checkout_at(&f.store.home, &target, false).is_err());
        drop(tx);
        let source_execution =
            crate::thread_lifecycle::checkout_at(&f.store.home, &f.repo, false).unwrap();
        assert!(f
            .store
            .rpc(
                "workspace/handoff/rollback",
                &json!({"cwd":f.repo,"operationId":operation})
            )
            .unwrap_err()
            .contains("running responses"));
        drop(source_execution);
        f.store
            .rpc(
                "workspace/handoff/rollback",
                &json!({"cwd":f.repo,"operationId":operation}),
            )
            .unwrap();
        assert_eq!(stamp(&f.image(&f.repo)).unwrap(), before);
        assert_eq!(stamp(&f.image(&target)).unwrap(), target_before);
    }

    #[test]
    fn handoff_continue_existing_preserves_both_branches_and_all_files() {
        let f = Fixture::new();
        let target = f.target();
        f.dirty(&f.repo);
        f.dirty(&target);
        let source_before = f.image(&f.repo);
        let target_before = f.image(&target);
        let tx=f.prepare(json!({"cwd":f.repo.join("nested"),"direction":"toWorktree","targetPath":target,"continueExisting":true})).unwrap();
        assert_eq!(tx.info["cwd"], json!(target.join("nested")));
        assert_eq!(tx.info["branch"], "feat/target");
        f.finish(&tx, Some("destination-chat")).unwrap();
        equal(&source_before, &f.image(&f.repo));
        equal(&target_before, &f.image(&target));
        assert!(f.git(&f.repo, &["stash", "list"]).is_empty());
        drop(tx);
        let tx=f.prepare(json!({"cwd":f.repo,"direction":"toWorktree","targetPath":target,"continueExisting":true})).unwrap();
        fs::write(target.join("later"), b"new edit\n").unwrap();
        fs::write(f.repo.join("source-later"), b"another edit\n").unwrap();
        f.finish(&tx, None).unwrap();
        assert_eq!(fs::read(target.join("later")).unwrap(), b"new edit\n");
        assert_eq!(
            fs::read(f.repo.join("source-later")).unwrap(),
            b"another edit\n"
        );
    }
}
