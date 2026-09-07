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
fn make(path: String, content: String) -> SourceFile {
    let (suggested, notes) = match suggest(&content) {
        Some((text, notes)) if text.trim() != content.trim() => (Some(text), notes),
        _ => (None, Vec::new()),
    };
    SourceFile {
        path,
        content,
        suggested,
        notes,
    }
}

pub fn read_all() -> Vec<SourceFile> {
    let mut out = Vec::new();

    let main = format!("{APT_DIR}/sources.list");
    if let Ok(content) = std::fs::read_to_string(&main) {
        out.push(make(main, content));
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
                out.push(make(path.to_string_lossy().into_owned(), content));
            }
        }
    }
    out
}

/// Debian releases whose archives have moved off the main mirrors.
const DEBIAN_EOL: &[&str] = &["jessie", "stretch", "buster", "bullseye"];

/// Suites that mean "whatever is current" rather than a fixed release.
const MOVING: &[&str] = &["stable", "testing", "unstable", "oldstable", "sid"];

/// Read the distribution id and codename this machine is actually running.
fn running_release() -> (String, String) {
    let Ok(os) = std::fs::read_to_string("/etc/os-release") else {
        return (String::new(), String::new());
    };
    let field = |key: &str| -> String {
        os.lines()
            .find_map(|l| l.strip_prefix(key))
            .map(|v| v.trim_matches(|c| c == '=' || c == '"' || c == '\'').trim().to_string())
            .unwrap_or_default()
    };
    (field("ID"), field("VERSION_CODENAME"))
}

/// Propose a corrected version of a source file.
///
/// Every rule here encodes a mistake seen on a real machine in this fleet: a
/// suite that moves underneath the release, the security suite rename Debian
/// made at 12, a third-party repo pointing at the wrong distribution entirely,
/// and archives that have moved after end-of-life. Returning the whole file
/// rather than a patch means the operator can read exactly what they are about
/// to apply.
pub fn suggest(content: &str) -> Option<(String, Vec<String>)> {
    let (distro, codename) = running_release();
    suggest_for(content, &distro, &codename)
}

/// The rules, with the release passed in so they can be tested anywhere rather
/// than only on the machine being corrected.
pub fn suggest_for(content: &str, distro: &str, codename: &str) -> Option<(String, Vec<String>)> {
    if distro.is_empty() || codename.is_empty() {
        return None;
    }
    let codename = codename.to_string();

    let mut notes: Vec<String> = Vec::new();
    let mut out: Vec<String> = Vec::new();
    let eol = distro == "debian" && DEBIAN_EOL.contains(&codename.as_str());

    for line in content.lines() {
        let trimmed = line.trim();
        let commented = trimmed.starts_with('#');
        let body = if commented {
            trimmed.trim_start_matches('#').trim()
        } else {
            trimmed
        };

        if !(body.starts_with("deb ") || body.starts_with("deb-src ")) {
            out.push(line.to_string());
            continue;
        }

        let mut fields: Vec<String> = body.split_whitespace().map(str::to_string).collect();
        // Locate the URI and the suite that follows it, skipping any [options].
        let uri_at = fields.iter().position(|f| f.contains("://"));
        let Some(ui) = uri_at else {
            out.push(line.to_string());
            continue;
        };
        if fields.len() <= ui + 1 {
            out.push(line.to_string());
            continue;
        }

        let original = fields.join(" ");
        let is_security = fields[ui].contains("security");

        // 1. A moving suite pins nothing; name the release actually installed.
        {
            let suite = fields[ui + 1].clone();
            // Split on both separators: `stable-updates` and `stable/updates`
            // are the same moving suite wearing different clothes, and missing
            // the hyphenated form leaves half the file unpinned.
            let base = suite
                .split(['/', '-'])
                .next()
                .unwrap_or(&suite)
                .to_string();
            if MOVING.contains(&base.as_str()) {
                let want = if is_security {
                    format!("{codename}-security")
                } else if suite.contains("-updates") {
                    format!("{codename}-updates")
                } else {
                    codename.clone()
                };
                notes.push(format!(
                    "`{suite}` follows whatever release is current; pinned to `{want}`"
                ));
                fields[ui + 1] = want;
            }
        }

        // 2. Debian renamed the security suite at 12: `<name>/updates` became
        //    `<name>-security`. Leftovers from an upgrade break apt outright.
        {
            let suite = fields[ui + 1].clone();
            if is_security && suite.ends_with("/updates") {
                let want = format!("{codename}-security");
                notes.push(format!(
                    "security suite `{suite}` uses the pre-Debian-12 form; changed to `{want}`"
                ));
                fields[ui + 1] = want;
            }
        }

        // 3. A third-party repo built for another distribution will never
        //    resolve. Docker publishing under /linux/ubuntu on a Debian box is
        //    the usual case.
        if distro == "debian" && fields[ui].contains("/ubuntu") && !fields[ui].contains("debian") {
            let fixed = fields[ui].replace("/ubuntu", "/debian");
            notes.push(format!(
                "`{}` is the Ubuntu archive; switched to `{fixed}`",
                fields[ui]
            ));
            fields[ui] = fixed;
        }

        // 4. A third-party suite naming a different release.
        {
            let suite = fields[ui + 1].clone();
            let known = [
                "stretch", "buster", "bullseye", "bookworm", "trixie", "forky",
            ];
            let base = suite.split('-').next().unwrap_or(&suite);
            if distro == "debian" && known.contains(&base) && base != codename {
                let want = suite.replacen(base, &codename, 1);
                notes.push(format!("`{suite}` names another release; changed to `{want}`"));
                fields[ui + 1] = want;
            }
        }

        // 5. After end-of-life the packages move to the archive host.
        //
        // Only Debian's own archive moves. Matching any URI containing
        // "/debian" swept up third-party repositories that merely publish a
        // Debian build - MongoDB and UniFi both do - and repointing those at
        // archive.debian.org destroys them. A real Debian mirror is
        // identifiable by its components: main, contrib, non-free and friends.
        let debian_components = fields[ui + 2..].iter().all(|c| {
            matches!(
                c.as_str(),
                "main" | "contrib" | "non-free" | "non-free-firmware"
            )
        }) && fields.len() > ui + 2;
        if eol && debian_components && !fields[ui + 1].contains('/') {
            let host_ok = fields[ui].contains("archive.debian.org");
            if !host_ok {
                let want = if is_security {
                    "http://archive.debian.org/debian-security".to_string()
                } else {
                    "http://archive.debian.org/debian".to_string()
                };
                notes.push(format!(
                    "{codename} is end-of-life; `{}` moved to `{want}`",
                    fields[ui]
                ));
                fields[ui] = want;
            }
        }

        let rebuilt = fields.join(" ");
        if rebuilt == original {
            out.push(line.to_string());
        } else {
            // Preserve whether the operator had this line disabled.
            out.push(if commented {
                format!("# {rebuilt}")
            } else {
                rebuilt
            });
        }
    }

    if notes.is_empty() {
        return None;
    }
    notes.dedup();
    Some((out.join("\n") + "\n", notes))
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

/// Where a file's backup lives.
///
/// Appends rather than using `with_extension`, which replaces: that turned
/// `foo.list` into `foo.patchpanel-bak`, losing which file it came from and
/// colliding with the backup of `foo.sources`. The backup is the recovery
/// path, so it has to name its original unambiguously.
fn backup_path(target: &std::path::Path) -> std::path::PathBuf {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    target.with_file_name(format!("{name}.patchpanel-bak"))
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
        let backup = backup_path(&target);
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
    let backup = backup_path(&target);
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

#[cfg(test)]
mod tests {
    use super::suggest_for;

    fn fix(line: &str, codename: &str) -> String {
        suggest_for(line, "debian", codename)
            .map(|(text, _)| text.trim().to_string())
            .unwrap_or_else(|| line.trim().to_string())
    }

    #[test]
    fn renames_the_pre_debian_12_security_suite() {
        // Exactly what broke `bit`: apt refuses the whole run over this.
        let got = fix(
            "deb http://security.debian.org/debian-security bookworm/updates main",
            "bookworm",
        );
        assert_eq!(
            got,
            "deb http://security.debian.org/debian-security bookworm-security main"
        );
    }

    #[test]
    fn repoints_a_repo_built_for_another_distribution() {
        // `bit` again: Docker published under /linux/ubuntu on a Debian box.
        let got = fix("deb https://download.docker.com/linux/ubuntu buster stable", "bookworm");
        assert!(got.contains("/linux/debian"), "{got}");
        assert!(got.contains("bookworm"), "{got}");
    }

    #[test]
    fn pins_a_moving_suite_to_the_installed_release() {
        // `hub`: `stable` had silently become Debian 13 under a Debian 11 box.
        assert_eq!(
            fix("deb http://mirrors.example.org/debian/ stable main", "bullseye"),
            "deb http://archive.debian.org/debian bullseye main"
        );
        assert_eq!(
            fix("deb http://mirrors.example.org/debian/ stable-updates main", "bookworm"),
            "deb http://mirrors.example.org/debian/ bookworm-updates main"
        );
    }

    #[test]
    fn moves_an_end_of_life_release_to_the_archive() {
        let got = fix("deb http://deb.debian.org/debian bullseye main", "bullseye");
        assert_eq!(got, "deb http://archive.debian.org/debian bullseye main");
    }


    #[test]
    fn leaves_third_party_repos_off_the_debian_archive() {
        // Both of these publish a Debian build but are not Debian mirrors.
        // Repointing them at archive.debian.org would break them outright.
        for line in [
            "deb [ arch=amd64 signed-by=/etc/apt/keyrings/mongodb.gpg ]              https://repo.mongodb.org/apt/debian bullseye/mongodb-org/8.0 main",
            "deb https://www.ui.com/downloads/unifi/debian bullseye ubiquiti",
        ] {
            let got = suggest_for(line, "debian", "bullseye")
                .map(|(t, _)| t)
                .unwrap_or_else(|| line.to_string());
            assert!(
                !got.contains("archive.debian.org"),
                "third-party repo was rewritten: {got}"
            );
        }
    }

    #[test]
    fn still_moves_a_real_debian_mirror() {
        let got = fix("deb http://mirrors.example.org/debian/ bullseye main contrib", "bullseye");
        assert_eq!(
            got,
            "deb http://archive.debian.org/debian bullseye main contrib"
        );
    }

    #[test]
    fn leaves_a_correct_file_alone() {
        let good = "deb http://deb.debian.org/debian bookworm main
                    deb http://security.debian.org/debian-security bookworm-security main
";
        assert!(suggest_for(good, "debian", "bookworm").is_none());
    }

    #[test]
    fn keeps_disabled_lines_disabled() {
        let got = fix("# deb http://security.debian.org/debian-security bookworm/updates main", "bookworm");
        assert!(got.starts_with('#'), "{got}");
        assert!(got.contains("bookworm-security"), "{got}");
    }

    #[test]
    fn ignores_comments_and_blank_lines() {
        assert!(suggest_for("# just a note

", "debian", "bookworm").is_none());
    }
}
