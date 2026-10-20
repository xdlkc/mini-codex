//! Parser/applier for the `*** Begin Patch` format used by Codex's `apply_patch` tool.

use anyhow::{anyhow, bail, Context, Result};
use std::fs;
use std::path::{Component, Path, PathBuf};

/// Resolve every existing ancestor before allowing a write. This also covers a
/// new file whose parent is a symlink outside the workspace.
fn workspace_path(root: &Path, path: &str) -> Result<PathBuf> {
    let requested = Path::new(path.trim());
    if requested.as_os_str().is_empty()
        || requested
            .components()
            .any(|c| matches!(c, Component::ParentDir))
    {
        bail!("patch path must stay inside the workspace: {path}");
    }
    let root = root.canonicalize().context("resolving patch workspace")?;
    let candidate = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        root.join(requested)
    };
    let mut ancestor = candidate.as_path();
    loop {
        match fs::symlink_metadata(ancestor) {
            Ok(_) => {
                let resolved = ancestor
                    .canonicalize()
                    .with_context(|| format!("resolving patch path {path}"))?;
                if !resolved.starts_with(&root) {
                    bail!("patch path is outside the workspace: {path}");
                }
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                ancestor = ancestor
                    .parent()
                    .ok_or_else(|| anyhow!("invalid patch path: {path}"))?;
            }
            Err(e) => return Err(e).with_context(|| format!("checking patch path {path}")),
        }
    }
    // Keep the original final component, so deleting an in-workspace symlink
    // removes the link rather than its target.
    Ok(candidate)
}

pub fn apply(patch: &str, root: &Path) -> Result<String> {
    let lines: Vec<&str> = patch.lines().collect();
    if lines.first().map(|l| l.trim()) != Some("*** Begin Patch") {
        bail!("patch must start with '*** Begin Patch'");
    }
    if lines.last().map(|l| l.trim()) != Some("*** End Patch") {
        bail!("patch must end with '*** End Patch'");
    }
    let body = &lines[1..lines.len() - 1];
    // Validate all paths before the first mutation, including rename targets.
    for line in body {
        for prefix in [
            "*** Add File: ",
            "*** Delete File: ",
            "*** Update File: ",
            "*** Move to: ",
        ] {
            if let Some(path) = line.strip_prefix(prefix) {
                workspace_path(root, path)?;
            }
        }
    }
    let mut i = 0;
    let mut report = Vec::new();
    while i < body.len() {
        let line = body[i];
        if let Some(p) = line.strip_prefix("*** Add File: ") {
            i += 1;
            let mut content = Vec::new();
            while i < body.len() && !body[i].starts_with("*** ") {
                content.push(
                    body[i]
                        .strip_prefix('+')
                        .ok_or_else(|| anyhow!("add-file lines must start with '+'"))?,
                );
                i += 1;
            }
            let path = workspace_path(root, p)?;
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&path, content.join("\n") + "\n")?;
            report.push(format!("A {}", p.trim()));
        } else if let Some(p) = line.strip_prefix("*** Delete File: ") {
            i += 1;
            fs::remove_file(workspace_path(root, p)?).with_context(|| format!("deleting {p}"))?;
            report.push(format!("D {}", p.trim()));
        } else if let Some(p) = line.strip_prefix("*** Update File: ") {
            i += 1;
            let mut dest: Option<PathBuf> = None;
            if i < body.len() {
                if let Some(m) = body[i].strip_prefix("*** Move to: ") {
                    dest = Some(workspace_path(root, m)?);
                    i += 1;
                }
            }
            let start = i;
            while i < body.len() && !(body[i].starts_with("*** ") && body[i] != "*** End of File") {
                i += 1;
            }
            let src = workspace_path(root, p)?;
            let original = fs::read_to_string(&src).with_context(|| format!("reading {p}"))?;
            let updated =
                update(&original, &body[start..i]).with_context(|| format!("patching {p}"))?;
            let out = dest.clone().unwrap_or_else(|| src.clone());
            if let Some(parent) = out.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&out, updated)?;
            if dest.is_some() {
                fs::remove_file(&src)?;
            }
            report.push(format!("M {}", p.trim()));
        } else if line.trim().is_empty() {
            i += 1;
        } else {
            bail!("unexpected patch line: {line}");
        }
    }
    Ok(format!("Done!\n{}", report.join("\n")))
}

fn update(original: &str, hunk_lines: &[&str]) -> Result<String> {
    let mut file: Vec<String> = original.lines().map(String::from).collect();
    let mut cursor = 0usize;
    let mut old: Vec<String> = Vec::new();
    let mut new: Vec<String> = Vec::new();

    let flush = |old: &mut Vec<String>,
                 new: &mut Vec<String>,
                 file: &mut Vec<String>,
                 cursor: &mut usize|
     -> Result<()> {
        if old.is_empty() && new.is_empty() {
            return Ok(());
        }
        let pos = if old.is_empty() {
            file.len()
        } else {
            if old.len() > file.len().saturating_sub(*cursor) {
                bail!("could not locate hunk context:\n{}", old.join("\n"));
            }
            (*cursor..=file.len() - old.len())
                .find(|&s| file[s..s + old.len()] == old[..])
                .ok_or_else(|| anyhow!("could not locate hunk context:\n{}", old.join("\n")))?
        };
        file.splice(pos..pos + old.len(), new.iter().cloned());
        *cursor = pos + new.len();
        old.clear();
        new.clear();
        Ok(())
    };

    for l in hunk_lines {
        if l.starts_with("@@") || *l == "*** End of File" {
            flush(&mut old, &mut new, &mut file, &mut cursor)?;
        } else if let Some(r) = l.strip_prefix('+') {
            new.push(r.to_string());
        } else if let Some(r) = l.strip_prefix('-') {
            old.push(r.to_string());
        } else {
            let r = l.strip_prefix(' ').unwrap_or(l);
            old.push(r.to_string());
            new.push(r.to_string());
        }
    }
    flush(&mut old, &mut new, &mut file, &mut cursor)?;
    Ok(file.join("\n") + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "mc-patch-test-{}-{:032x}",
                std::process::id(),
                rand::random::<u128>()
            ));
            fs::create_dir_all(path.join("workspace")).unwrap();
            Self(path)
        }
        fn workspace(&self) -> PathBuf {
            self.0.join("workspace")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn update_add_delete() {
        let fixture = Fixture::new();
        let d = fixture.workspace();
        std::fs::write(d.join("f.txt"), "a\nb\nc\n").unwrap();
        let p = "*** Begin Patch\n*** Update File: f.txt\n@@\n a\n-b\n+B\n c\n*** Add File: n/new.txt\n+hello\n*** End Patch";
        apply(p, &d).unwrap();
        assert_eq!(
            std::fs::read_to_string(d.join("f.txt")).unwrap(),
            "a\nB\nc\n"
        );
        assert_eq!(
            std::fs::read_to_string(d.join("n/new.txt")).unwrap(),
            "hello\n"
        );
        apply(
            "*** Begin Patch\n*** Delete File: n/new.txt\n*** End Patch",
            &d,
        )
        .unwrap();
        assert!(!d.join("n/new.txt").exists());
    }

    #[test]
    fn rejects_traversal_absolute_escape_and_move_before_writing() {
        let fixture = Fixture::new();
        let root = fixture.workspace();
        let outside = fixture.0.join("outside.txt");
        fs::write(root.join("original.txt"), "keep\n").unwrap();
        for target in ["../outside.txt".to_string(), outside.display().to_string()] {
            let patch = format!("*** Begin Patch\n*** Add File: safe.txt\n+safe\n*** Add File: {target}\n+escape\n*** End Patch");
            assert!(apply(&patch, &root).is_err());
            assert!(!root.join("safe.txt").exists());
            assert!(!outside.exists());
        }
        let patch = "*** Begin Patch\n*** Update File: original.txt\n*** Move to: ../outside.txt\n@@\n-keep\n+changed\n*** End Patch";
        assert!(apply(patch, &root).is_err());
        assert_eq!(
            fs::read_to_string(root.join("original.txt")).unwrap(),
            "keep\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape_for_new_existing_and_deleted_files() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let root = fixture.workspace();
        let outside = fixture.0.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep.txt"), "keep\n").unwrap();
        symlink(&outside, root.join("link")).unwrap();
        symlink(outside.join("keep.txt"), root.join("file-link")).unwrap();
        for patch in [
            "*** Begin Patch\n*** Add File: link/new/nested.txt\n+escape\n*** End Patch",
            "*** Begin Patch\n*** Update File: file-link\n@@\n-keep\n+changed\n*** End Patch",
            "*** Begin Patch\n*** Delete File: link/keep.txt\n*** End Patch",
        ] {
            assert!(apply(patch, &root).is_err());
        }
        assert_eq!(
            fs::read_to_string(outside.join("keep.txt")).unwrap(),
            "keep\n"
        );
        assert!(!outside.join("new").exists());
    }

    #[test]
    fn oversized_or_later_missing_context_returns_error() {
        assert!(update("a\n", &[" a", " b", " c", "+z"]).is_err());
        assert!(update("a\nb\n", &[" a", "@@", " a", " b"]).is_err());
    }
}
