//! Parser/applier for the `*** Begin Patch` format used by Codex's `apply_patch` tool.

use anyhow::{anyhow, bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

pub fn apply(patch: &str, root: &Path) -> Result<String> {
    let lines: Vec<&str> = patch.lines().collect();
    if lines.first().map(|l| l.trim()) != Some("*** Begin Patch") {
        bail!("patch must start with '*** Begin Patch'");
    }
    if lines.last().map(|l| l.trim()) != Some("*** End Patch") {
        bail!("patch must end with '*** End Patch'");
    }
    let body = &lines[1..lines.len() - 1];
    let mut i = 0;
    let mut report = Vec::new();
    while i < body.len() {
        let line = body[i];
        if let Some(p) = line.strip_prefix("*** Add File: ") {
            i += 1;
            let mut content = Vec::new();
            while i < body.len() && !body[i].starts_with("*** ") {
                content.push(body[i].strip_prefix('+').ok_or_else(|| anyhow!("add-file lines must start with '+'"))?);
                i += 1;
            }
            let path = root.join(p.trim());
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&path, content.join("\n") + "\n")?;
            report.push(format!("A {}", p.trim()));
        } else if let Some(p) = line.strip_prefix("*** Delete File: ") {
            i += 1;
            fs::remove_file(root.join(p.trim())).with_context(|| format!("deleting {p}"))?;
            report.push(format!("D {}", p.trim()));
        } else if let Some(p) = line.strip_prefix("*** Update File: ") {
            i += 1;
            let mut dest: Option<PathBuf> = None;
            if i < body.len() {
                if let Some(m) = body[i].strip_prefix("*** Move to: ") {
                    dest = Some(root.join(m.trim()));
                    i += 1;
                }
            }
            let start = i;
            while i < body.len() && !(body[i].starts_with("*** ") && body[i] != "*** End of File") {
                i += 1;
            }
            let src = root.join(p.trim());
            let original = fs::read_to_string(&src).with_context(|| format!("reading {p}"))?;
            let updated = update(&original, &body[start..i]).with_context(|| format!("patching {p}"))?;
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

    let flush = |old: &mut Vec<String>, new: &mut Vec<String>, file: &mut Vec<String>, cursor: &mut usize| -> Result<()> {
        if old.is_empty() && new.is_empty() {
            return Ok(());
        }
        let pos = if old.is_empty() {
            file.len()
        } else {
            (*cursor..=file.len().saturating_sub(old.len()))
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
    #[test]
    fn update_add_delete() {
        let d = std::env::temp_dir().join(format!("mc-test-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("f.txt"), "a\nb\nc\n").unwrap();
        let p = "*** Begin Patch\n*** Update File: f.txt\n@@\n a\n-b\n+B\n c\n*** Add File: n/new.txt\n+hello\n*** End Patch";
        apply(p, &d).unwrap();
        assert_eq!(std::fs::read_to_string(d.join("f.txt")).unwrap(), "a\nB\nc\n");
        assert_eq!(std::fs::read_to_string(d.join("n/new.txt")).unwrap(), "hello\n");
        apply("*** Begin Patch\n*** Delete File: n/new.txt\n*** End Patch", &d).unwrap();
        assert!(!d.join("n/new.txt").exists());
    }
}
