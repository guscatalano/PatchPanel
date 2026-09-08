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
async fn make(path: String, content: String) -> SourceFile {
    let (suggested, notes) = match suggest(&content) {
        Some((text, mut notes)) if !same_text(&text, &content) => {
            match verify(&content, &text, &mut notes).await {
                Some(checked) => (Some(checked), notes),
                None => (None, Vec::new()),
            }
        }
        _ => (None, Vec::new()),
    };
    SourceFile {
        path,
        content,
        suggested,
        notes,
    }
}

pub async fn read_all() -> Vec<SourceFile> {
    let mut out = Vec::new();

    let main = format!("{APT_DIR}/sources.list");
    if let Ok(content) = std::fs::read_to_string(&main) {
        out.push(make(main, content).await);
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
                out.push(make(path.to_string_lossy().into_owned(), content).await);
            }
        }
    }
    out
}

/// Debian releases whose archives have moved off the main mirrors.
const DEBIAN_EOL: &[&str] = &["jessie", "stretch", "buster", "bullseye"];

/// Whether a Debian release has moved off the main mirrors.
pub fn is_eol(codename: &str) -> bool {
    DEBIAN_EOL.contains(&codename)
}

/// Suites that mean "whatever is current" rather than a fixed release.
const MOVING: &[&str] = &["stable", "testing", "unstable", "oldstable", "sid"];

/// Read the distribution id and codename this machine is actually running.
pub fn running_release() -> (String, String) {
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

        // Is this Debian's own archive, or a third party that merely publishes
        // a Debian build? Components are the reliable signal: Debian's are
        // main/contrib/non-free, while a vendor repo uses its own name. Rules
        // that rewrite suites or hosts must only fire on the former - UniFi's
        // repository genuinely calls its suite `stable`, and "correcting" that
        // to a Debian codename breaks it.
        let debian_archive = fields.len() > ui + 2
            && fields[ui + 2..].iter().all(|c| {
                matches!(
                    c.as_str(),
                    "main" | "contrib" | "non-free" | "non-free-firmware"
                )
            });

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
            if debian_archive && MOVING.contains(&base.as_str()) {
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
        if eol && debian_archive && !fields[ui + 1].contains('/') {
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

/// What an archive said when we asked for a suite.
#[derive(Clone, Copy, PartialEq)]
enum Serves {
    /// The suite is there and its Release file is still within its own
    /// validity window.
    Yes,
    /// The files are served, but the Release file's `Valid-Until` has passed.
    /// apt refuses these by default; it is the normal state of a release the
    /// security team has stopped signing.
    Expired,
    No,
}

/// Ask an archive whether it serves a suite, and whether apt will accept it.
async fn serves(client: &reqwest::Client, uri: &str, suite: &str) -> Serves {
    let url = format!("{}/dists/{suite}/Release", uri.trim_end_matches('/'));
    let Ok(res) = client.get(&url).send().await else {
        return Serves::No;
    };
    if !res.status().is_success() {
        return Serves::No;
    }
    let Ok(body) = res.text().await else {
        return Serves::Yes;
    };
    let expired = body
        .lines()
        .find_map(|l| l.strip_prefix("Valid-Until:"))
        .and_then(parse_valid_until)
        .is_some_and(|t| t < chrono::Utc::now());
    if expired { Serves::Expired } else { Serves::Yes }
}

/// Add an option to a source line, into the existing `[...]` group if there
/// is one.
///
/// apt allows exactly one bracket group per line, so a second one is a syntax
/// error - and the lines that need this most are third-party repos, which are
/// the ones that already carry `signed-by=`.
fn add_option(fields: &mut Vec<String>, uri_at: usize, option: &str) {
    // Options sit between the type and the URI. `[a=1 b=2]` may arrive as one
    // token or several, depending on how the file was written.
    if uri_at > 1 && fields[uri_at - 1].ends_with(']') {
        let last = fields[uri_at - 1].clone();
        if last == "]" {
            fields.insert(uri_at - 1, option.to_string());
        } else {
            fields[uri_at - 1] = format!("{} {option}]", last.trim_end_matches(']'));
        }
        return;
    }
    fields.insert(uri_at, format!("[{option}]"));
}

/// Debian writes `Mon, 07 Sep 2026 21:13:04 UTC`. RFC 2822 wants a numeric
/// offset and chrono rejects the alphabetic zone, so translate it first -
/// failing to parse here reads as "not expired", which is the dangerous way
/// round.
fn parse_valid_until(v: &str) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    let v = v.trim();
    let numeric = v
        .strip_suffix("UTC")
        .or_else(|| v.strip_suffix("GMT"))
        .map(|head| format!("{head}+0000"))
        .unwrap_or_else(|| v.to_string());
    chrono::DateTime::parse_from_rfc2822(&numeric).ok()
}

/// Where else the same suite might live.
///
/// Debian moves a release's files twice in its life: security leaves
/// security.debian.org, and the main archive moves to archive.debian.org. The
/// suite is renamed on the way (`bullseye/updates` became `bullseye-security`
/// at Debian 12), and the two changes do not happen together - bullseye's
/// main archive had moved while its security suite had not. Guessing which
/// combination is live produces a source file apt rejects, so try them.
fn alternates(uri: &str, suite: &str) -> Vec<(String, String)> {
    let mut hosts = vec![uri.to_string()];
    for (from, to) in [
        ("archive.debian.org/debian-security", "security.debian.org/debian-security"),
        ("security.debian.org/debian-security", "archive.debian.org/debian-security"),
        ("deb.debian.org/debian-security", "security.debian.org/debian-security"),
        ("archive.debian.org/debian", "deb.debian.org/debian"),
        ("deb.debian.org/debian", "archive.debian.org/debian"),
        ("ftp.debian.org/debian", "archive.debian.org/debian"),
    ] {
        if uri.contains(from) {
            hosts.push(uri.replace(from, to));
        }
    }

    let mut suites = vec![suite.to_string()];
    if let Some(base) = suite.strip_suffix("-security") {
        suites.push(format!("{base}/updates"));
    }
    if let Some(base) = suite.strip_suffix("/updates") {
        suites.push(format!("{base}-security"));
    }

    let mut out = Vec::new();
    for h in &hosts {
        for s in &suites {
            let pair = (h.clone(), s.clone());
            if !out.contains(&pair) {
                out.push(pair);
            }
        }
    }
    out
}

/// Check a proposed source file against the archives before offering it.
///
/// The rules that build a suggestion encode where Debian's files usually are.
/// "Usually" produced a suggestion that apt threw out - archive.debian.org has
/// no bullseye-security - and an operator who applies a fix should not be the
/// one who discovers it was wrong. Every rewritten line is fetched: if the
/// suite is not there, the alternates are tried, and a line nothing serves is
/// put back the way it was rather than offered as a fix.
pub async fn verify(original: &str, suggested: &str, notes: &mut Vec<String>) -> Option<String> {
    let Ok(client) = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
    else {
        return Some(suggested.to_string());
    };

    let before: Vec<&str> = original.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut usable = false;

    for (i, line) in suggested.lines().enumerate() {
        let t = line.trim();
        let unchanged = before.get(i).map(|b| b.trim() == t).unwrap_or(false);
        if unchanged || t.starts_with('#') || !(t.starts_with("deb ") || t.starts_with("deb-src ")) {
            out.push(line.to_string());
            continue;
        }

        let mut fields: Vec<String> = t.split_whitespace().map(str::to_string).collect();
        let Some(ui) = fields.iter().position(|f| f.contains("://")) else {
            out.push(line.to_string());
            continue;
        };
        if fields.len() <= ui + 1 {
            out.push(line.to_string());
            continue;
        }

        let mut resolved = None;
        for (uri, suite) in alternates(&fields[ui], &fields[ui + 1]) {
            match serves(&client, &uri, &suite).await {
                Serves::Yes => {
                    resolved = Some((uri, suite, false));
                    break;
                }
                // Keep looking for a live one, but remember this works if
                // apt is told to accept a stale Release.
                Serves::Expired if resolved.is_none() => {
                    resolved = Some((uri, suite, true));
                }
                _ => {}
            }
        }

        match resolved {
            Some((uri, suite, expired)) => {
                if uri != fields[ui] || suite != fields[ui + 1] {
                    notes.push(format!(
                        "`{} {}` is not served; used `{uri} {suite}`, which is",
                        fields[ui], fields[ui + 1]
                    ));
                }
                // Write the address before touching the options: inserting a
                // token first shifts everything after it, which turned the
                // suite into a duplicate of itself.
                fields[ui] = uri;
                fields[ui + 1] = suite.clone();

                if expired && !t.contains("check-valid-until") {
                    // apt refuses a Release past its Valid-Until unless told
                    // otherwise. For a release nobody signs any more, that is
                    // the only way to keep installing from it at all.
                    add_option(&mut fields, ui, "check-valid-until=no");
                    notes.push(format!(
                        "`{suite}` is no longer refreshed, so its Release file has expired; \
                         added `check-valid-until=no` so apt will still read it"
                    ));
                }
                out.push(fields.join(" "));
                usable = true;
            }
            None => {
                // Nothing anywhere serves this. Offering it would produce the
                // exact rejection this function exists to prevent.
                notes.push(format!(
                    "nothing serves `{} {}` any more, so that line is left as it was",
                    fields[ui],
                    fields[ui + 1]
                ));
                out.push(before.get(i).map(|b| b.to_string()).unwrap_or_else(|| line.to_string()));
            }
        }
    }

    let text = out.join("\n") + "\n";
    (usable && !same_text(&text, original)).then_some(text)
}

/// Do these two files say the same thing to apt?
///
/// The rules rebuild each line by joining its fields with single spaces, so a
/// file that was aligned by hand comes back "changed" on every line while
/// meaning exactly what it did before. Offering that as a fix wastes the
/// operator's attention and an `apt-get update` on the machine.
fn same_text(a: &str, b: &str) -> bool {
    let norm = |t: &str| -> Vec<String> {
        t.lines()
            .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|l| !l.is_empty())
            .collect()
    };
    norm(a) == norm(b)
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

/// Does apt complain about the file we just wrote, or anything in it?
///
/// Matching only the URIs was not enough. A line that is not a source line at
/// all - `apt http://...`, copied out of a table that displays the backend
/// name - makes apt report the *file and line number* and no URI, so the write
/// sailed through validation and left the machine unable to read its sources.
/// Errors naming the path count too.
///
/// Still scoped to this file: a machine with a pre-existing problem elsewhere
/// should not have an unrelated edit rolled back on account of it.
fn breaks_on(text: &str, uris: &[String], path: &str) -> Option<String> {
    // `/etc/apt/sources.list.d/x.list` and the bare name, since apt quotes it
    // both ways depending on the message.
    let file = path.rsplit('/').next().unwrap_or(path);
    for line in text.lines() {
        let t = line.trim().trim_start_matches("stderr:").trim();
        let is_err = t.starts_with("E:")
            || t.contains("does not have a Release file")
            || t.contains("Failed to fetch");
        if !is_err {
            continue;
        }
        if uris.iter().any(|u| t.contains(u.as_str()))
            || t.contains(path)
            || t.contains(file)
        {
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

    if let Some(err) = breaks_on(&out.text, &uris_in(content), path) {
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
    use super::*;

    /// A file someone aligned by hand comes back from the rules with single
    /// spaces everywhere. That is not a fix, and offering it as one costs an
    /// `apt-get update` on the machine and the operator's attention.
    #[test]
    fn spacing_alone_is_not_a_change() {
        assert!(same_text(
            "deb  http://deb.debian.org/debian   bookworm  main",
            "deb http://deb.debian.org/debian bookworm main"
        ));
        assert!(same_text("deb http://x/y z main

", "deb http://x/y z main
"));
        assert!(!same_text(
            "deb http://x/y bookworm main",
            "deb http://x/y trixie main"
        ));
        // A line appearing or vanishing is a real change.
        assert!(!same_text("deb http://x/y z main", "deb http://x/y z main
deb http://a/b c d"));
    }

    /// A failure that got past validation once: a line pasted out
    /// of the repositories table, which shows the backend name rather than
    /// the source type. apt names the file and the line, never a URI, so a
    /// check that only looked for URIs let it through and left the machine
    /// unable to read any of its sources.
    #[test]
    fn rolls_back_a_line_apt_cannot_parse() {
        let apt_said = "stderr: E: Type 'apt' is not known on line 8 in source list                         /etc/apt/sources.list
stderr: E: The list of sources could not be read.";
        let uris = vec!["http://security.debian.org".to_string()];
        assert!(breaks_on(apt_said, &uris, "/etc/apt/sources.list").is_some());
        // And still catches the case it always did.
        let unreachable = "stderr: E: Failed to fetch http://nope.example/dists/x/Release";
        assert!(breaks_on(unreachable, &["http://nope.example".to_string()], "/etc/apt/sources.list").is_some());
    }

    /// Someone else's broken repository is not a reason to undo this edit.
    #[test]
    fn leaves_an_unrelated_failure_alone() {
        let other = "stderr: E: Failed to fetch http://elsewhere.example/dists/x/Release";
        assert!(breaks_on(
            other,
            &["http://deb.debian.org/debian".to_string()],
            "/etc/apt/sources.list.d/mine.list"
        )
        .is_none());
    }

    /// Debian's Release files spell the zone `UTC`, which RFC 2822 does not
    /// allow. Parsing it wrongly is invisible and expensive: every expired
    /// archive reads as current, so the fix offered to an operator is one apt
    /// then rejects.
    #[test]
    fn reads_debians_spelling_of_a_date() {
        assert!(parse_valid_until("Mon, 07 Sep 2026 21:13:04 UTC").is_some());
        assert!(parse_valid_until("Mon, 07 Sep 2026 21:13:04 GMT").is_some());
        assert!(parse_valid_until("Mon, 07 Sep 2026 21:13:04 +0000").is_some());
        assert!(parse_valid_until("nonsense").is_none());

        let past = parse_valid_until("Sat, 31 Aug 2024 11:02:15 UTC").unwrap();
        assert!(past < chrono::Utc::now(), "a 2024 date is in the past");
    }

    #[test]
    fn adds_an_option_without_breaking_the_line() {
        // No options yet: a group of its own.
        let mut f: Vec<String> = "deb http://x/y suite main"
            .split_whitespace().map(str::to_string).collect();
        add_option(&mut f, 1, "check-valid-until=no");
        assert_eq!(f.join(" "), "deb [check-valid-until=no] http://x/y suite main");

        // An existing group, written as one token: apt takes only one `[...]`,
        // so it has to go inside.
        let mut f: Vec<String> = "deb [signed-by=/k.gpg] http://x/y suite main"
            .split_whitespace().map(str::to_string).collect();
        add_option(&mut f, 2, "check-valid-until=no");
        assert_eq!(
            f.join(" "),
            "deb [signed-by=/k.gpg check-valid-until=no] http://x/y suite main"
        );

        // The spaced-out spelling Glenn R's installer writes.
        let mut f: Vec<String> = "deb [ arch=amd64 signed-by=/k.gpg ] http://x/y suite main"
            .split_whitespace().map(str::to_string).collect();
        add_option(&mut f, 5, "check-valid-until=no");
        assert_eq!(
            f.join(" "),
            "deb [ arch=amd64 signed-by=/k.gpg check-valid-until=no ] http://x/y suite main"
        );
    }

    /// bullseye's main archive had moved while its security suite had not, so
    /// the end-of-life rule's first guess 404s. Every place it could be has to
    /// be on the list.
    #[test]
    fn offers_every_place_a_moved_suite_could_be() {
        let alts = alternates("http://archive.debian.org/debian-security", "bullseye-security");
        assert!(
            alts.iter().any(|(u, s)| u.contains("security.debian.org") && s == "bullseye-security"),
            "{alts:?}"
        );
        // The pre-Debian-12 spelling, which is where buster's ended up.
        assert!(alts.iter().any(|(_, s)| s == "bullseye/updates"), "{alts:?}");
        // The original is tried first: a source that works is not moved.
        assert_eq!(
            alts[0],
            (
                "http://archive.debian.org/debian-security".to_string(),
                "bullseye-security".to_string()
            )
        );
    }

    use super::suggest_for;

    fn fix(line: &str, codename: &str) -> String {
        suggest_for(line, "debian", codename)
            .map(|(text, _)| text.trim().to_string())
            .unwrap_or_else(|| line.trim().to_string())
    }

    #[test]
    fn renames_the_pre_debian_12_security_suite() {
        // A real breakage: apt refuses the whole run over this.
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
        // Docker published under /linux/ubuntu on a Debian box.
        let got = fix("deb https://download.docker.com/linux/ubuntu buster stable", "bookworm");
        assert!(got.contains("/linux/debian"), "{got}");
        assert!(got.contains("bookworm"), "{got}");
    }

    #[test]
    fn pins_a_moving_suite_to_the_installed_release() {
        // `stable` had silently become Debian 13 under a Debian 11 box.
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
    fn does_not_repin_a_vendors_own_stable_suite() {
        // UniFi publishes under a suite it calls `stable`; rewriting that to a
        // Debian codename would point at a distribution that does not exist.
        let line = "deb https://www.ui.com/downloads/unifi/debian stable ubiquiti";
        assert!(
            suggest_for(line, "debian", "bullseye").is_none(),
            "a vendor suite was rewritten"
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
