//! Fork's working-tree semantics: stage tracked bytes in a private index,
//! apply its binary diff without changing the destination's real index, then
//! copy untracked bytes. Generic local-state creation retains its own contract.
use super::*;
use sha2::{Digest, Sha256};

pub(super) fn repository(repo: &Repo) -> Repo {
    let mut repo = repo.clone();
    repo.filters
        .extend(["attr.tree=".into(), "core.attributesFile=".into()]);
    repo
}
pub(super) struct Scratch(pub PathBuf);
impl Scratch {
    pub(super) fn new(home: &Path) -> Result<Self> {
        let parent = home.join("workspace-tmp");
        no_symlink(&parent)?;
        private_dir(&parent)?;
        let path = parent.join(id("fork-input"));
        fs::create_dir(&path).map_err(|e| e.to_string())?;
        let scratch = Self(path);
        private_dir(&scratch.0)?;
        Ok(scratch)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub(super) fn copy_index(repo: &Repo, directory: &Path) -> Result<PathBuf> {
    fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let source = repo.admin_path("index")?;
    no_symlink(&source)?;
    let target = directory.join("index");
    fs::copy(&source, &target).map_err(|e| format!("Copy private Git index: {e}"))?;
    apply_permissions(&target, 0o600)?;
    let shared = repo.text(&["rev-parse", "--shared-index-path"])?;
    if !shared.is_empty() {
        let shared = PathBuf::from(shared);
        let shared = if shared.is_absolute() {
            shared
        } else {
            repo.root.join(shared)
        };
        no_symlink(&shared)?;
        let destination = directory.join(shared.file_name().ok_or("Invalid shared Git index")?);
        fs::copy(&shared, &destination).map_err(|e| format!("Copy shared Git index: {e}"))?;
        apply_permissions(&destination, 0o600)?;
    }
    Ok(target)
}
fn digest(path: &Path) -> Result<String> {
    let mut file = read_file(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let size = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if size == 0 {
            break;
        }
        hash.update(&buffer[..size]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn read_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path).map_err(|e| e.to_string())
}
fn parent_check(root: &Path, relative: &str) -> Result<()> {
    valid_relative(relative)?;
    let mut parent = root.to_path_buf();
    if let Some(path) = Path::new(relative).parent() {
        for component in path.components() {
            parent.push(component);
            no_symlink(&parent)?;
        }
    }
    Ok(())
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Fingerprint {
    path: String,
    mode: u32,
    kind: &'static str,
    hash: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Baseline {
    head: Option<String>,
    branch: Option<String>,
    index: String,
    files: Vec<Fingerprint>,
    directories: Vec<Directory>,
    root_mode: u32,
}
impl Baseline {
    pub(super) fn capture(repo: &Repo) -> Result<Self> {
        let directories = walk_directories(&repo.root)?;
        let mut files = Vec::new();
        let mut folders = vec![repo.root.clone()];
        folders.extend(
            directories
                .iter()
                .map(|directory| repo.root.join(&directory.path)),
        );
        for folder in folders {
            for item in fs::read_dir(&folder).map_err(|e| e.to_string())? {
                let item = item.map_err(|e| e.to_string())?;
                let path = item.path();
                if folder == repo.root
                    && item
                        .file_name()
                        .to_string_lossy()
                        .eq_ignore_ascii_case(".git")
                {
                    continue;
                }
                let meta = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
                if meta.is_dir() && !meta.file_type().is_symlink() {
                    continue;
                }
                let relative =
                    path_text(path.strip_prefix(&repo.root).map_err(|e| e.to_string())?)?
                        .to_string();
                valid_relative(&relative)?;
                let (kind, hash) = if meta.file_type().is_symlink() {
                    (
                        "symlink",
                        format!("{:x}", Sha256::digest(symlink_bytes(&path)?)),
                    )
                } else if meta.is_file() {
                    ("file", digest(&path)?)
                } else {
                    return Err("Cannot verify a special file in the fork checkout".into());
                };
                files.push(Fingerprint {
                    path: relative,
                    mode: if kind == "symlink" {
                        0
                    } else {
                        permissions(&meta)
                    },
                    kind,
                    hash,
                });
            }
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(Self {
            head: repo.head()?,
            branch: repo.branch()?,
            index: digest(&repo.admin_path("index")?)?,
            files,
            directories,
            root_mode: permissions(&fs::metadata(&repo.root).map_err(|e| e.to_string())?),
        })
    }
}
struct Untracked {
    path: String,
    mode: u32,
    hash: String,
    link: Option<Vec<u8>>,
}
pub(super) struct Plan {
    scratch: Scratch,
    pub head: String,
    index_hash: String,
    patch: Vec<u8>,
    files: Vec<Untracked>,
}
fn tracked_diff(repo: &Repo, directory: &Path) -> Result<Vec<u8>> {
    let index = copy_index(repo, directory)?;
    repo.with_index(&["add", "-u"], &index)?;
    repo.with_index(
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--color=never",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "--binary",
            "--cached",
        ],
        &index,
    )
}
impl Plan {
    pub(super) fn capture(store: &Store, source: &Repo) -> Result<Self> {
        let source = repository(source);
        let source = &source;
        let scratch = Scratch::new(&store.home)?;
        let head = source
            .head()?
            .ok_or("A commit is required to fork a worktree")?;
        let index_hash = digest(&source.admin_path("index")?)?;
        let patch = tracked_diff(source, &scratch.0.join("source-index"))?;
        let paths =
            nul_paths(source.bytes(&["ls-files", "--others", "--exclude-standard", "-z"])?)?;
        let mut files = Vec::new();
        let staged = scratch.0.join("files");
        private_dir(&staged)?;
        for path in paths {
            let path = path.trim_end_matches('/').to_string();
            parent_check(&source.root, &path)?;
            let input = source.root.join(&path);
            let meta = fs::symlink_metadata(&input).map_err(|e| e.to_string())?;
            if meta.file_type().is_symlink() {
                let link = symlink_bytes(&input)?;
                files.push(Untracked {
                    path,
                    mode: 0,
                    hash: format!("{:x}", Sha256::digest(&link)),
                    link: Some(link),
                });
                continue;
            }
            if !meta.is_file() {
                if meta.is_dir() {
                    // Codex's B0 excludes embedded repositories without a HEAD.
                    // Its non-recursive host copy rejects committed directories.
                    let mut command = git_command(&input, &source.filters, None);
                    command.args(["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]);
                    if execute(command, None, MAX_OUTPUT)?.output.status.code() == Some(1) {
                        continue;
                    }
                }
                return Err(format!(
                    "Cannot safely copy untracked directory or special file {path}"
                ));
            }
            if let Some(parent) = Path::new(&path)
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
            {
                safe_directory(&staged, path_text(parent)?)?;
            }
            let output = staged.join(&path);
            let mut reader = read_file(&input)?;
            let mut writer = private_options().open(&output).map_err(|e| e.to_string())?;
            std::io::copy(&mut reader, &mut writer).map_err(|e| e.to_string())?;
            writer.sync_all().map_err(|e| e.to_string())?;
            parent_check(&source.root, &path)?;
            let hash = digest(&output)?;
            if digest(&input)? != hash {
                return Err(
                    "Untracked files changed while preparing the fork; retry when idle".into(),
                );
            }
            files.push(Untracked {
                path,
                mode: permissions(&meta),
                hash,
                link: None,
            });
        }
        if source.head()?.as_deref() != Some(&head)
            || digest(&source.admin_path("index")?)? != index_hash
            || tracked_diff(source, &scratch.0.join("source-check"))? != patch
        {
            return Err("Local files, HEAD or the Git index changed while preparing the fork; retry when idle".into());
        }
        Ok(Self {
            scratch,
            head,
            index_hash,
            patch,
            files,
        })
    }
    pub(super) fn apply(&self, source: &Repo, target: &Repo) -> Result<()> {
        let target = repository(target);
        let target = &target;
        if source.head()?.as_deref() != Some(&self.head)
            || target.head()?.as_deref() != Some(&self.head)
            || digest(&source.admin_path("index")?)? != self.index_hash
        {
            return Err("The source changed before the fork checkout was ready".into());
        }
        if !self.patch.is_empty() {
            let patch = self.scratch.0.join("patch.diff");
            fs::write(&patch, &self.patch).map_err(|e| e.to_string())?;
            let index = copy_index(target, &self.scratch.0.join("target-index"))?;
            target.with_index(&["apply", "--3way", path_text(&patch)?], &index)?;
        }
        for entry in &self.files {
            parent_check(&target.root, &entry.path)?;
            if let Some(parent) = Path::new(&entry.path)
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
            {
                safe_directory(&target.root, path_text(parent)?)?;
            }
            let output = target.root.join(&entry.path);
            if exists(&output) {
                return Err(format!(
                    "The fork destination was edited before copying {}",
                    entry.path
                ));
            }
            if let Some(link) = &entry.link {
                restore_symlink(link, &output)?;
            } else {
                let staged = self.scratch.0.join("files").join(&entry.path);
                if digest(&staged)? != entry.hash {
                    return Err("The captured fork input changed".into());
                }
                fs::copy(staged, &output).map_err(|e| e.to_string())?;
                apply_permissions(&output, entry.mode)?;
            }
        }
        copy_worktree_includes(source, &target.root)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        root: PathBuf,
        repo: Repo,
        store: Store,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(id("mcodex-fork-state"));
            fs::create_dir_all(root.join("repo/src")).unwrap();
            fs::create_dir(root.join("repo/other")).unwrap();
            let root = root.canonicalize().unwrap();
            let cwd = root.join("repo");
            checked(
                &cwd,
                &[],
                &["init", "--template=", "-b", "main"],
                None,
                None,
            )
            .unwrap();
            for file in ["tracked.txt", "src/file.txt", "other/file.txt"] {
                fs::write(cwd.join(file), b"base\n").unwrap();
            }
            checked(&cwd, &[], &["add", "."], None, None).unwrap();
            checked(&cwd, &[], &["commit", "-m", "fixture"], None, None).unwrap();
            let repo = Repo::open(&cwd).unwrap().unwrap();
            Self {
                store: Store {
                    home: root.join("home"),
                },
                root,
                repo,
            }
        }
        fn git(&self, args: &[&str]) {
            self.repo.bytes(args).unwrap();
        }
        fn copy(&self) -> (Repo, Vec<u8>) {
            let before = fs::read(self.repo.admin_path("index").unwrap()).unwrap();
            let plan = Plan::capture(&self.store, &self.repo).unwrap();
            let mut state = self.store.load().unwrap();
            let info = self
                .store
                .create_with_includes(
                    &self.repo,
                    &json!({"detached":true,"baseRef":plan.head}),
                    &mut state,
                    false,
                )
                .unwrap()["worktree"]
                .clone();
            let target = Repo::open(Path::new(info["cwd"].as_str().unwrap()))
                .unwrap()
                .unwrap();
            plan.apply(&self.repo, &target).unwrap();
            assert_eq!(
                fs::read(self.repo.admin_path("index").unwrap()).unwrap(),
                before
            );
            assert!(target.bytes(&["diff", "--cached", "--quiet"]).is_ok());
            (target, before)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    #[test]
    fn intent_to_add_is_copied_as_working_bytes_without_copying_staging_flags() {
        let f = Fixture::new();
        fs::write(f.repo.root.join("intent.txt"), b"intent contents\n").unwrap();
        f.git(&["add", "-N", "intent.txt"]);
        let (target, _) = f.copy();
        assert_eq!(
            fs::read(target.root.join("intent.txt")).unwrap(),
            b"intent contents\n"
        );
        assert!(target
            .text(&["ls-files", "--error-unmatch", "intent.txt"])
            .is_err());
    }
    #[test]
    fn assume_unchanged_and_skip_worktree_match_the_reference_index_semantics() {
        for flag in ["--assume-unchanged", "--skip-worktree"] {
            let f = Fixture::new();
            f.git(&["update-index", flag, "tracked.txt"]);
            fs::write(f.repo.root.join("tracked.txt"), b"hidden working change\n").unwrap();
            let (target, _) = f.copy();
            assert_eq!(
                fs::read(target.root.join("tracked.txt")).unwrap(),
                b"base\n"
            );
        }
    }
    #[test]
    fn sparse_and_split_indexes_can_fork_and_keep_the_original_index_bytes() {
        for sparse in [false, true] {
            let f = Fixture::new();
            if sparse {
                f.git(&["sparse-checkout", "init", "--cone", "--sparse-index"]);
                f.git(&["sparse-checkout", "set", "src"]);
            } else {
                f.git(&["update-index", "--split-index"]);
            }
            fs::write(f.repo.root.join("src/file.txt"), b"working contents\n").unwrap();
            let (target, _) = f.copy();
            assert_eq!(
                fs::read(target.root.join("src/file.txt")).unwrap(),
                b"working contents\n"
            );
            let before = Baseline::capture(&target).unwrap();
            fs::write(target.root.join("later-edit"), b"preserve").unwrap();
            assert_ne!(Baseline::capture(&target).unwrap(), before);
        }
    }
    #[test]
    fn unmerged_working_files_are_copied_while_the_target_index_stays_clean() {
        let f = Fixture::new();
        f.git(&["switch", "-c", "other-branch"]);
        fs::write(f.repo.root.join("tracked.txt"), b"right\n").unwrap();
        f.git(&["commit", "-am", "right"]);
        f.git(&["switch", "main"]);
        fs::write(f.repo.root.join("tracked.txt"), b"left\n").unwrap();
        f.git(&["commit", "-am", "left"]);
        assert!(f.repo.bytes(&["merge", "other-branch"]).is_err());
        let expected = fs::read(f.repo.root.join("tracked.txt")).unwrap();
        let (target, _) = f.copy();
        assert_eq!(fs::read(target.root.join("tracked.txt")).unwrap(), expected);
        assert!(f.repo.admin_path("MERGE_HEAD").unwrap().exists());
        assert!(!target.admin_path("MERGE_HEAD").unwrap().exists());
    }
    #[test]
    fn uninitialized_submodules_do_not_block_fork_or_its_private_cleanup_baseline() {
        let f = Fixture::new();
        let head = f.repo.head().unwrap().unwrap();
        f.git(&[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{head},module"),
        ]);
        f.git(&["commit", "-m", "gitlink"]);
        fs::create_dir(f.repo.root.join("module")).unwrap();
        let (target, _) = f.copy();
        assert!(target.root.join("module").is_dir());
        Baseline::capture(&target).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn ignored_copies_follow_replacement_of_a_tracked_directory_symlink() {
        use std::os::unix::fs::symlink;
        let f = Fixture::new();
        symlink("missing", f.repo.root.join("replacement")).unwrap();
        fs::write(f.repo.root.join(".gitignore"), "AGENTS.override.md\n").unwrap();
        f.git(&["add", "."]);
        f.git(&["commit", "-m", "symlink"]);
        fs::remove_file(f.repo.root.join("replacement")).unwrap();
        fs::create_dir(f.repo.root.join("replacement")).unwrap();
        fs::write(f.repo.root.join("replacement/file"), b"untracked").unwrap();
        fs::write(
            f.repo.root.join("replacement/AGENTS.override.md"),
            b"instructions",
        )
        .unwrap();
        let (target, _) = f.copy();
        assert_eq!(
            fs::read(target.root.join("replacement/file")).unwrap(),
            b"untracked"
        );
        assert_eq!(
            fs::read(target.root.join("replacement/AGENTS.override.md")).unwrap(),
            b"instructions"
        );
    }
    #[cfg(unix)]
    #[test]
    fn untracked_binary_modes_and_raw_symlink_targets_are_copied_without_empty_directories() {
        use std::os::unix::{
            ffi::{OsStrExt, OsStringExt},
            fs::symlink,
        };
        let f = Fixture::new();
        let file = "quoted 'name\nfile.bin";
        fs::write(f.repo.root.join(file), [0, 255, 1]).unwrap();
        apply_permissions(&f.repo.root.join(file), 0o640).unwrap();
        fs::create_dir(f.repo.root.join("empty")).unwrap();
        let link = std::ffi::OsString::from_vec(b"missing-\xff".to_vec());
        symlink(&link, f.repo.root.join("raw-link")).unwrap();
        let (target, _) = f.copy();
        assert_eq!(fs::read(target.root.join(file)).unwrap(), [0, 255, 1]);
        assert_eq!(
            permissions(&fs::metadata(target.root.join(file)).unwrap()),
            0o640
        );
        assert_eq!(
            fs::read_link(target.root.join("raw-link"))
                .unwrap()
                .as_os_str()
                .as_bytes(),
            link.as_os_str().as_bytes()
        );
        assert!(!target.root.join("empty").exists());
    }
}
