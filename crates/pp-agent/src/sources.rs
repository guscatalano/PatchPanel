//! Reading and editing apt source files from the portal.
//!
//! This is the most destructive thing the agent can be asked to do that is not
//! an upgrade: a bad sources file means apt refuses to do anything at all, and
//! the machine is then unreachable by the very tool that broke it. So every
//! write is: back up, write, ask apt whether it still works, and put the old
//! file back if it does not. An edit that breaks apt undoes itself.

use anyhow::{Context, Result};
use pp_proto::SourceFile;

use crate::exec::{self, Progress};

/// The only directory tree we will touch.
const APT_DIR: &str = "/etc/apt";

/// Read every apt source file, so the portal can show and edit the real text
/// rather than a parsed approximation of it.
pub fn read_all() -> Vec<SourceFile> {
    let mut out = Vec::new();

    let main = format!("{APT_DIR}/sources.list");
    if let Ok(content) = std::fs::read_to_string(&main) {
        out.push(SourceFile { path: main, content });
    }

    if let Ok(dir) = std::fs::read_dir(format!("{APT_DIR}/sources.list.d")) {
        let mut entries: Vec<_> = dir.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        entries.sort();
        for path in entries {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext != "list" && ext != "sources" {
                continue;
            }
            if let Ok(content) = std::fs::read_to_string(&path) {
                out.push(SourceFile {
                    path: path.to_string_lossy().into_owned(),
                    content,
                });
            }
        }
    }
    out
}

/// Reject anything outside the apt source files.
///
/// The portal supplies this path, so it is untrusted input that names a file
/// we are about to overwrite as root. Only two shapes are allowed, no
/// traversal, and the extension must be one apt actually reads.
fn validate(path: &str) -> Result<std::path::PathBuf> {
    if path.contains("..") {
        anyhow::bail!("`{path}` contains a path traversal");
    }
    let p = std::path::Path::new(path);
    if !p.is_absolute() {
        anyhow::bail!("`{path}` is not an absolute path");
    }

    let main = std::path::Path::new(APT_DIR).join("sources.list");
    if p == main {
        return Ok(p.to_path_buf());
    }

    let dir = std::path::Path::new(APT_DIR).join("sources.list.d");
    if p.parent() != Some(dir.as_path()) {
        anyhow::bail!(
            "`{path}` is not an apt source file; only {} and files in {} may be edited",
            main.display(),
            dir.display()
        );
    }
    match p.extension().and_then(|e| e.to_str()) {
        Some("list") | Some("sources") => Ok(p.to_path_buf()),
        _ => anyhow::bail!("`{path}` must end in .list or .sources"),
    }
}

/// Does apt complain about any of these URIs?
///
/// Only the URIs from the file just written are considered: a machine with a
/// pre-existing problem elsewhere should not have an unrelated edit rolled
/// back on account of it.
fn breaks_on(text: &str, uris: &[String]) -> Option<String> {
    for line in text.lines() {
        let t = line.trim().trim_start_matches("stderr:").trim();
        let is_err = t.starts_with("E:")
            || t.contains("does not have a Release file")
            || t.contains("Failed to fetch");
        if is_err && uris.iter().any(|u| t.contains(u.as_str())) {
            return Some(t.to_string());
        }
    }
    None
}

fn uris_in(content: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in content.lines() {
        let t = line.trim().trim_start_matches('#').trim();
        for word in t.split_whitespace() {
            if word.starts_with("http://") || word.starts_with("https://") {
                let w = word.trim_end_matches('/').to_string();
                if !out.contains(&w) {
                    out.push(w);
                }
            }
        }
    }
    out
}

/// Write a source file, then make apt prove it still works.
pub async fn write(path: &str, content: &str, p: &Progress) -> Result<String> {
    let target = validate(path)?;
    let existed = target.exists();
    let previous = if existed {
        Some(std::fs::read_to_string(&target).unwrap_or_default())
    } else {
        None
    };

    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Keep a copy on disk too, so a human can recover it without the portal.
    if let Some(prev) = &previous {
        let backup = target.with_extension("patchpanel-bak");
        let _ = std::fs::write(&backup, prev);
    }

    std::fs::write(&target, content)
        .with_context(|| format!("writing {}", target.display()))?;
    p.line(&format!("wrote {}", target.display()));

    p.line("validating with apt-get update");
    let out = exec::run("apt-get", &["-qq", "update"], p).await?;

    if let Some(err) = breaks_on(&out.text, &uris_in(content)) {
        // Undo rather than leave the machine with an apt that refuses to run.
        match previous {
            Some(prev) => {
                std::fs::write(&target, prev)?;
                let _ = exec::run("apt-get", &["-qq", "update"], p).await;
                anyhow::bail!(
                    "reverted {}: apt rejected the new contents\n  {err}",
                    target.display()
                );
            }
            None => {
                let _ = std::fs::remove_file(&target);
                let _ = exec::run("apt-get", &["-qq", "update"], p).await;
                anyhow::bail!(
                    "removed {}: apt rejected the new file\n  {err}",
                    target.display()
                );
            }
        }
    }

    Ok(format!(
        "{} {} and apt accepted it",
        if existed { "updated" } else { "created" },
        target.display()
    ))
}

/// Delete a source file. The backup stays behind.
pub async fn remove(path: &str, p: &Progress) -> Result<String> {
    let target = validate(path)?;
    if !target.exists() {
        return Ok(format!("{} does not exist", target.display()));
    }

    let previous = std::fs::read_to_string(&target).unwrap_or_default();
    let backup = target.with_extension("patchpanel-bak");
    let _ = std::fs::write(&backup, &previous);

    std::fs::remove_file(&target).with_context(|| format!("removing {}", target.display()))?;
    p.line(&format!("removed {}", target.display()));

    let _ = exec::run("apt-get", &["-qq", "update"], p).await;
    Ok(format!(
        "removed {} (a copy is at {})",
        target.display(),
        backup.display()
    ))
}
