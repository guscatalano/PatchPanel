//! Debian/Ubuntu (apt) and RHEL-family (dnf) backends.

use std::time::Duration;

use anyhow::{Context, Result};
use pp_proto::{
    AppSource, AvailableUpdate, BootReport, Cleanup, Disk, Ensure, FirmwareDevice, FirmwareUpdate,
    Guest, MidUpgrade, Package, Repository, ScanIssue, Severity, Virtualization,
};

use super::{AppOutcome, Backend, Platform};
use crate::exec::{self, Progress};

pub fn detect() -> Vec<Backend> {
    let mut v = Vec::new();
    if exec::have("apt-get") {
        v.push(Backend::Apt);
    }
    if exec::have("dnf") {
        v.push(Backend::Dnf);
    }
    if exec::have("fwupdmgr") {
        v.push(Backend::Fwupd);
    }
    v
}

pub async fn installed_packages(pf: &Platform, p: &Progress) -> Result<Vec<Package>> {
    let mut out = Vec::new();

    if pf.has(Backend::Apt) {
        // dpkg-query is the source of truth for what apt considers installed,
        // and its format string means no table parsing.
        let o = exec::run(
            "dpkg-query",
            &["-W", "-f=${Package}\t${Version}\t${db:Status-Status}\n"],
            p,
        )
        .await?
        .require(&[])?;
        for line in o.text.lines() {
            let mut f = line.split('\t');
            let (Some(name), Some(version), Some(status)) = (f.next(), f.next(), f.next()) else {
                continue;
            };
            if status.trim() != "installed" {
                continue;
            }
            out.push(Package {
                name: name.to_string(),
                version: version.to_string(),
                source: "apt".into(),
            });
        }
    }

    if pf.has(Backend::Dnf) {
        let o = exec::run("rpm", &["-qa", "--qf", "%{NAME}\t%{VERSION}-%{RELEASE}\n"], p)
            .await?
            .require(&[])?;
        for line in o.text.lines() {
            if let Some((name, version)) = line.split_once('\t') {
                out.push(Package {
                    name: name.to_string(),
                    version: version.to_string(),
                    source: "dnf".into(),
                });
            }
        }
    }

    Ok(out)
}

/// apt reports a broken source on stderr and then carries on with a non-zero
/// exit, so its output is the only place the failure exists. A repository that
/// will not refresh means the update list is incomplete - or, when apt bails
/// entirely, that patching this machine cannot work at all.
fn parse_apt_errors(text: &str) -> Vec<ScanIssue> {
    let mut out = Vec::new();
    for line in text.lines() {
        let t = line.trim().trim_start_matches("stderr:").trim();
        let is_err = t.starts_with("E:")
            || t.contains("does not have a Release file")
            || t.contains("Failed to fetch");
        if !is_err {
            continue;
        }
        let msg = t.trim_start_matches("E:").trim().to_string();
        if out.iter().any(|i: &ScanIssue| i.problem == msg) {
            continue;
        }
        out.push(ScanIssue {
            backend: "apt".into(),
            problem: msg,
            remedy: "Fix or disable this source. Until it resolves, apt refuses to \
                     apply updates and the pending list is incomplete."
                .into(),
        });
    }
    out
}

pub async fn available_updates(
    pf: &Platform,
    p: &Progress,
) -> Result<(Vec<AvailableUpdate>, Vec<ScanIssue>)> {
    let mut out = Vec::new();
    let mut issues = Vec::new();

    if pf.has(Backend::Apt) {
        // Refresh metadata first, or `apt list --upgradable` reports whatever
        // was true the last time someone happened to run an update.
        if let Ok(o) = exec::run("apt-get", &["-qq", "update"], p).await {
            issues.extend(parse_apt_errors(&o.text));
        }
        let o = exec::run("apt", &["list", "--upgradable"], p)
            .await?
            .require(&[])?;
        out.extend(parse_apt_upgradable(&o.text));
    }

    if pf.has(Backend::Dnf) {
        // Exit code 100 means "updates are available" - expected, not an error.
        let o = exec::run("dnf", &["-q", "check-update"], p)
            .await?
            .require(&[100])?;
        let mut updates = parse_dnf_check_update(&o.text);

        // dnf reports severity separately, so a second pass marks the security
        // subset. If updateinfo is unavailable the flags simply stay false.
        if let Ok(sec) = exec::run("dnf", &["-q", "updateinfo", "list", "security"], p).await {
            let names: Vec<String> = sec
                .text
                .lines()
                .filter_map(|l| l.split_whitespace().nth(2).map(str::to_string))
                .collect();
            for u in &mut updates {
                u.security = names.iter().any(|n| n.starts_with(&u.name));
            }
        }
        out.extend(updates);
    }

    Ok((out, issues))
}

/// Lines look like:
/// `openssl/noble-security 3.0.13-1ubuntu2 amd64 [upgradable from: 3.0.13-1]`
fn parse_apt_upgradable(text: &str) -> Vec<AvailableUpdate> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("Listing") || !line.contains("upgradable from:") {
            continue;
        }
        let Some((head, rest)) = line.split_once(' ') else {
            continue;
        };
        let Some((name, suite)) = head.split_once('/') else {
            continue;
        };
        let Some(new_version) = rest.split_whitespace().next() else {
            continue;
        };
        let current = line
            .split_once("upgradable from:")
            .map(|(_, v)| v.trim().trim_end_matches(']').trim())
            .unwrap_or("")
            .to_string();

        out.push(AvailableUpdate {
            name: name.to_string(),
            current_version: current,
            new_version: new_version.to_string(),
            source: "apt".into(),
            // The suite name is how apt exposes provenance here.
            security: suite.contains("-security"),
        });
    }
    out
}

/// Lines look like: `openssl.x86_64   1:3.2.2-6.el9   baseos`
fn parse_dnf_check_update(text: &str) -> Vec<AvailableUpdate> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        // Blank lines separate sections; the trailers are not packages.
        if line.is_empty() || line.starts_with("Obsoleting") || line.starts_with("Last metadata") {
            continue;
        }
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 3 {
            continue;
        }
        let name = f[0].rsplit_once('.').map(|(n, _)| n).unwrap_or(f[0]);
        out.push(AvailableUpdate {
            name: name.to_string(),
            current_version: String::new(),
            new_version: f[1].to_string(),
            source: "dnf".into(),
            security: false,
        });
    }
    out
}

pub fn scan_issues(pf: &Platform) -> Vec<ScanIssue> {
    if pf.has(Backend::Apt) || pf.has(Backend::Dnf) {
        return Vec::new();
    }
    vec![ScanIssue {
        backend: "linux".into(),
        problem: "no supported package manager was found (looked for apt-get and dnf)".into(),
        remedy: "This machine's updates cannot be counted. If it uses another package \
                 manager, PatchPanel does not support it yet."
            .into(),
    }]
}

/// Packages a plain `upgrade` will not touch because they need new packages
/// installed - a kernel metapackage being the usual one. They are pending
/// updates that will never apply until someone runs a full upgrade, so they
/// deserve to be visible rather than silently skipped.
///
/// The caller subtracts anything `deferred` has claimed: a package waiting on
/// a phased rollout also shows up here, and offering a full upgrade for it
/// would be a button that cannot work.
pub async fn held_back(pf: &Platform, p: &Progress) -> Vec<String> {
    if !pf.has(Backend::Apt) {
        return Vec::new();
    }
    let Ok(o) = exec::run("apt-get", &["-s", "upgrade"], p).await else {
        return Vec::new();
    };
    parse_kept_back(&o.text)
}

/// Updates that no amount of clicking will install right now.
///
/// Two things end up here. Ubuntu names most of them outright, under "deferred
/// due to phasing": the archive withholds an update from a share of machines
/// until it has proven itself. The rest are packages apt reports as merely
/// "kept back", which reads like something a full upgrade would fix - but when
/// what they are waiting for is itself phased, they are equally untouchable.
/// Calling those held back sends the operator to a Run full upgrade button
/// that cannot help, so we resolve the distinction here rather than making
/// them discover it.
pub async fn deferred(pf: &Platform, p: &Progress) -> Vec<String> {
    if !pf.has(Backend::Apt) {
        return Vec::new();
    }
    let Ok(o) = exec::run("apt-get", &["-s", "full-upgrade"], p).await else {
        return Vec::new();
    };

    let mut phased = parse_phased(&o.text);
    let kept = parse_kept_back(&o.text);
    phased.extend(phase_bound(&kept, &phased, p).await);
    phased.sort();
    phased.dedup();
    phased
}

/// Which of `kept` are only stuck because something they are bound to is
/// phased.
///
/// apt does not say, so ask it what these packages relate to and look for a
/// phased name on either side of the relation: `language-pack-gnome-fr` is
/// kept back because `language-pack-gnome-fr-base` is phased, and
/// `language-pack-gnome-de-base` because the package that depends on *it* is.
async fn phase_bound(kept: &[String], phased: &[String], p: &Progress) -> Vec<String> {
    if kept.is_empty() || phased.is_empty() {
        return Vec::new();
    }
    let held: std::collections::HashSet<&str> = phased.iter().map(String::as_str).collect();

    let mut related: std::collections::HashMap<String, Vec<String>> = Default::default();
    for verb in ["depends", "rdepends"] {
        let mut args: Vec<&str> = vec![verb];
        args.extend(kept.iter().map(String::as_str));
        if let Ok(o) = exec::run("apt-cache", &args, p).await {
            for (pkg, names) in parse_relations(&o.text) {
                related.entry(pkg).or_default().extend(names);
            }
        }
    }

    kept.iter()
        .filter(|k| {
            related
                .get(*k)
                .is_some_and(|names| names.iter().any(|n| held.contains(n.as_str())))
        })
        .cloned()
        .collect()
}

/// `apt-cache depends` and `apt-cache rdepends` both print one block per
/// package: an unindented name, then indented relations. Only hard
/// dependencies count - a Suggests is not why anything is held back.
fn parse_relations(text: &str) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t == "Reverse Depends:" {
            continue;
        }
        if !line.starts_with(char::is_whitespace) {
            out.push((t.to_string(), Vec::new()));
            continue;
        }
        let Some((_, current)) = out.last_mut().map(|e| (&e.0, &mut e.1)) else {
            continue;
        };
        // `  Depends: libc6` in one direction, a bare `  libc6` in the other.
        // Only the first word can be the relation keyword: splitting on the
        // first colon instead swallows the epoch in `foo (>= 1:22.04)`.
        let body = t.trim_start_matches('|').trim();
        let first = body.split_whitespace().next().unwrap_or("");
        let name = if first.ends_with(':') {
            if !matches!(first, "Depends:" | "PreDepends:") {
                continue;
            }
            body[first.len()..].trim()
        } else {
            body
        };
        // Version constraints and virtual packages: `foo (>= 1)`, `<foo>`.
        let name = name
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_matches(|c| c == '<' || c == '>');
        if !name.is_empty() {
            current.push(name.to_string());
        }
    }
    out
}

fn parse_kept_back(text: &str) -> Vec<String> {
    parse_list_block(text, "The following packages have been kept back")
}

fn parse_phased(text: &str) -> Vec<String> {
    parse_list_block(text, "The following upgrades have been deferred due to phasing")
}

/// apt announces a set of packages with a headline and then indents the names
/// across as many lines as it needs.
fn parse_list_block(text: &str, header: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_block = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with(header) {
            in_block = true;
            continue;
        }
        if in_block {
            if t.is_empty() || t.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                break;
            }
            if line.starts_with(char::is_whitespace) {
                out.extend(t.split_whitespace().map(str::to_string));
            } else {
                break;
            }
        }
    }
    out
}

pub async fn apply_patches(
    pf: &Platform,
    security_only: bool,
    only: &[String],
    exclude: &[String],
    full: bool,
    p: &Progress,
) -> Result<String> {
    let mut log = Vec::new();

    if pf.has(Backend::Apt) {
        exec::run("apt-get", &["-qq", "update"], p).await?.require(&[])?;

        // apt has no per-run exclude flag; marking a package on hold is the
        // supported way to keep an upgrade from touching it.
        for ex in exclude {
            let _ = exec::run("apt-mark", &["hold", ex], p).await;
        }

        let mut args: Vec<String> = vec![
            "-y".into(),
            "-o".into(),
            "Dpkg::Options::=--force-confold".into(),
        ];
        let mut skip = false;

        if !only.is_empty() {
            args.push("install".into());
            args.push("--only-upgrade".into());
            args.extend(only.iter().cloned());
        } else if security_only {
            // Narrow to the packages apt attributes to a security suite.
            let names: Vec<String> = available_updates(pf, p)
                .await?
                .0
                .into_iter()
                .filter(|u| u.security && u.source == "apt" && !exclude.contains(&u.name))
                .map(|u| u.name)
                .collect();
            if names.is_empty() {
                log.push("apt: no security updates pending".to_string());
                skip = true;
            } else {
                args.push("install".into());
                args.push("--only-upgrade".into());
                args.extend(names);
            }
        } else if full {
            // dist-upgrade/full-upgrade resolves changed dependencies, which is
            // what moves a held-back kernel metapackage. It can also remove
            // packages, so it is never the default.
            args.push("full-upgrade".into());
        } else {
            args.push("upgrade".into());
        }

        if !skip {
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            let o = exec::run("apt-get", &refs, p).await?.require(&[])?;
            log.push(exec::tail(&o.text, 4000));
        }

        for ex in exclude {
            let _ = exec::run("apt-mark", &["unhold", ex], p).await;
        }
    }

    if pf.has(Backend::Dnf) {
        let mut args: Vec<String> = vec!["-y".into()];
        if security_only {
            args.push("--security".into());
        }
        let _ = full;
        for ex in exclude {
            args.push(format!("--exclude={ex}"));
        }
        args.push("upgrade".into());
        args.extend(only.iter().cloned());

        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let o = exec::run("dnf", &refs, p).await?.require(&[])?;
        log.push(exec::tail(&o.text, 4000));
    }

    if log.is_empty() {
        anyhow::bail!("no Linux patch backend available");
    }
    Ok(log.join("\n"))
}

/// What `autoremove` would take, without taking it. `-s` simulates.
pub async fn cleanup_preview(pf: &Platform, p: &Progress) -> Cleanup {
    let mut out = Cleanup {
        cache_bytes: dir_size("/var/cache/apt/archives") + dir_size("/var/cache/dnf"),
        ..Default::default()
    };

    if pf.has(Backend::Apt) {
        if let Ok(o) = exec::run("apt-get", &["-s", "autoremove"], p).await {
            let (pkgs, bytes) = parse_apt_autoremove(&o.text);
            out.packages.extend(pkgs);
            out.reclaim_bytes += bytes;
        }
    }
    if pf.has(Backend::Dnf) {
        if let Ok(o) = exec::run("dnf", &["-q", "repoquery", "--unneeded"], p).await {
            out.packages.extend(
                o.text
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .map(|l| l.split(':').next().unwrap_or(l).to_string()),
            );
        }
    }
    out
}

/// apt prints the set as an indented block, then a summary line carrying the
/// size in human units.
fn parse_apt_autoremove(text: &str) -> (Vec<String>, u64) {
    let mut pkgs = Vec::new();
    let mut bytes = 0u64;
    let mut in_block = false;

    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("The following packages will be REMOVED") {
            in_block = true;
            continue;
        }
        if in_block {
            // The block ends at the summary line, which starts with a count.
            if t.is_empty() || t.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                in_block = false;
            } else if line.starts_with(char::is_whitespace) {
                pkgs.extend(t.split_whitespace().map(|w| w.trim_end_matches('*').to_string()));
                continue;
            } else {
                in_block = false;
            }
        }
        if let Some(rest) = t.strip_prefix("After this operation, ") {
            if let Some(freed) = rest.split(" disk space will be freed").next() {
                bytes = parse_size(freed);
            }
        }
    }
    (pkgs, bytes)
}

/// apt reports "12.3 MB" / "980 kB"; SI units, as apt uses them.
fn parse_size(s: &str) -> u64 {
    let s = s.trim();
    let (num, unit): (String, String) = s
        .chars()
        .partition(|c| c.is_ascii_digit() || *c == '.' || *c == ',');
    let n: f64 = num.replace(',', "").parse().unwrap_or(0.0);
    let mult = match unit.trim().to_ascii_lowercase().as_str() {
        "kb" => 1_000.0,
        "mb" => 1_000_000.0,
        "gb" => 1_000_000_000.0,
        _ => 1.0,
    };
    (n * mult) as u64
}

fn dir_size(path: &str) -> u64 {
    let Ok(dir) = std::fs::read_dir(path) else {
        return 0;
    };
    dir.filter_map(|e| e.ok())
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

pub async fn cleanup(pf: &Platform, purge: bool, p: &Progress) -> Result<String> {
    let mut log = Vec::new();

    if pf.has(Backend::Apt) {
        let mut args = vec!["-y", "autoremove"];
        if purge {
            args.push("--purge");
        }
        let o = exec::run("apt-get", &args, p).await?.require(&[])?;
        log.push(exec::tail(&o.text, 3000));
        // Emptying the cache reclaims space with no risk at all.
        let o = exec::run("apt-get", &["-y", "clean"], p).await?.require(&[])?;
        log.push(exec::tail(&o.text, 200));
    }
    if pf.has(Backend::Dnf) {
        let o = exec::run("dnf", &["-y", "autoremove"], p).await?.require(&[])?;
        log.push(exec::tail(&o.text, 3000));
        let _ = exec::run("dnf", &["-y", "clean", "packages"], p).await;
    }

    if log.is_empty() {
        anyhow::bail!("no cleanup backend available");
    }
    Ok(log.join("
"))
}

pub async fn reboot_required() -> bool {
    if tokio::fs::metadata("/var/run/reboot-required").await.is_ok() {
        return true;
    }
    // needs-restarting exits 1 when a reboot is required, 0 when it is not.
    if exec::have("needs-restarting") {
        if let Ok(o) = exec::run("needs-restarting", &["-r"], &Progress::detached()).await {
            return o.code == 1;
        }
    }
    false
}

pub async fn ensure_app(
    pf: &Platform,
    name: &str,
    version: Option<&str>,
    ensure: Ensure,
    source: &AppSource,
    p: &Progress,
) -> Result<AppOutcome> {
    let (tool, pkg) = match source {
        AppSource::Apt { package } if pf.has(Backend::Apt) => ("apt-get", package.clone()),
        AppSource::Dnf { package } if pf.has(Backend::Dnf) => ("dnf", package.clone()),
        AppSource::Apt { .. } | AppSource::Dnf { .. } => {
            anyhow::bail!("app `{name}` needs a backend this machine does not have")
        }
        other => anyhow::bail!("source `{}` is not usable on Linux", other.backend()),
    };

    let installed = installed_version(tool, &pkg, p).await;

    match ensure {
        Ensure::Absent => {
            if installed.is_none() {
                return Ok(AppOutcome::unchanged(format!("`{name}` already absent"), None));
            }
            exec::run(tool, &["-y", "remove", &pkg], p).await?.require(&[])?;
            Ok(AppOutcome::changed(format!("removed `{name}`"), None))
        }
        Ensure::Present => {
            // A pinned version still has to match, otherwise "present" would
            // silently accept whatever happens to be on the box already.
            if let Some(cur) = &installed {
                if version.is_none_or(|want| cur.starts_with(want)) {
                    return Ok(AppOutcome::unchanged(
                        format!("`{name}` present at {cur}"),
                        installed,
                    ));
                }
            }
            let target = match version {
                Some(v) if tool == "apt-get" => format!("{pkg}={v}"),
                Some(v) => format!("{pkg}-{v}"),
                None => pkg.clone(),
            };
            exec::run(tool, &["-y", "install", &target], p)
                .await?
                .require(&[])?;
            let now = installed_version(tool, &pkg, p).await;
            Ok(AppOutcome::changed(format!("installed `{name}`"), now))
        }
        Ensure::Latest => {
            if installed.is_none() {
                exec::run(tool, &["-y", "install", &pkg], p)
                    .await?
                    .require(&[])?;
            } else if tool == "apt-get" {
                exec::run(tool, &["-y", "install", "--only-upgrade", &pkg], p)
                    .await?
                    .require(&[])?;
            } else {
                exec::run(tool, &["-y", "upgrade", &pkg], p)
                    .await?
                    .require(&[])?;
            }
            let now = installed_version(tool, &pkg, p).await;
            let changed = now != installed;
            let note = if changed {
                format!("`{name}` -> {}", now.clone().unwrap_or_default())
            } else {
                format!("`{name}` already latest")
            };
            Ok(AppOutcome {
                changed,
                note,
                observed: now,
            })
        }
    }
}

pub async fn observed_version(pf: &Platform, source: &AppSource, p: &Progress) -> Option<String> {
    match source {
        AppSource::Apt { package } if pf.has(Backend::Apt) => {
            installed_version("apt-get", package, p).await
        }
        AppSource::Dnf { package } if pf.has(Backend::Dnf) => {
            installed_version("dnf", package, p).await
        }
        _ => None,
    }
}

async fn installed_version(tool: &str, pkg: &str, p: &Progress) -> Option<String> {
    let o = if tool == "apt-get" {
        exec::run("dpkg-query", &["-W", "-f=${Version}", pkg], p).await.ok()?
    } else {
        exec::run("rpm", &["-q", "--qf", "%{VERSION}-%{RELEASE}", pkg], p).await.ok()?
    };
    if !o.ok() {
        return None;
    }
    let v = o.text.trim().to_string();
    (!v.is_empty()).then_some(v)
}

// ---------------------------------------------------------------------------
// Release upgrades
// ---------------------------------------------------------------------------

/// How long a full release upgrade may take before we give up on it. Two
/// hours is generous for a small server and still bounded: a run that hangs
/// forever holds the agent's command slot and tells the operator nothing.
const UPGRADE_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);

/// Options that keep dpkg from stopping to ask about a modified config file.
/// Without them the upgrade blocks on a prompt nobody can answer, halfway
/// through replacing every package on the machine.
const KEEP_CONF: [&str; 4] = [
    "-o",
    "Dpkg::Options::=--force-confold",
    "-o",
    "Dpkg::Options::=--force-confdef",
];

/// Move this machine to the next Debian release, or report whether it could.
///
/// The checks are the point. A release upgrade cannot be undone from inside
/// the machine, so everything that can be known beforehand is established
/// first, and `check` stops there.
pub async fn distro_upgrade(
    pf: &Platform,
    to: &str,
    check: bool,
    p: &Progress,
) -> Result<String> {
    if !pf.has(Backend::Apt) {
        anyhow::bail!("release upgrades need apt; this machine does not have it");
    }

    let repos = crate::sources::read_all().await;
    let repo_list = crate::repos::collect();
    let stable = crate::release::stable_codename().await;
    let rel = crate::release::collect(&repo_list, stable.as_deref())
        .ok_or_else(|| anyhow::anyhow!("could not read /etc/os-release"))?;

    let mut log = Vec::new();
    log.push(format!(
        "{} {} ({}) -> {to}",
        rel.distro, rel.version_id, rel.codename
    ));

    // -------------------------------------------------------------- preflight
    let mut stop: Vec<String> = Vec::new();

    if rel.distro != "debian" {
        stop.push(format!(
            "only Debian upgrades are supported. {} moves between releases with its own \
             tooling ({}), which PatchPanel does not drive yet.",
            rel.distro,
            if rel.distro == "ubuntu" { "do-release-upgrade" } else { "its vendor's" }
        ));
    }

    // The target has to be a release Debian has actually made. The codename
    // after the newest one is testing, and upgrading a server onto testing is
    // never what "upgrade to the next release" is meant to do.
    match stable.as_deref() {
        Some(s) if !crate::release::at_or_before(to, s) => stop.push(format!(
            "`{to}` is not a released Debian yet - `{s}` is current stable. Upgrading to it \
             would put this machine on testing."
        )),
        None => stop.push(
            "could not reach the Debian archive to check that `{to}` is a released version. \
             Refusing rather than guessing."
                .to_string(),
        ),
        _ => {}
    }

    match rel.next.as_deref() {
        Some(next) if next == to => {}
        Some(next) => stop.push(format!(
            "asked to upgrade to `{to}`, but the next release for {} is `{next}`. \
             Reload the page.",
            rel.codename
        )),
        None => stop.push(format!(
            "no upgrade is available from {} {}",
            rel.distro, rel.codename
        )),
    }

    for f in rel.findings.iter().filter(|f| f.severity == Severity::Blocker) {
        stop.push(format!("{} - fix this first", f.summary));
    }

    // Proxmox is Debian underneath, which is exactly what makes this
    // dangerous: the upgrade would repoint the Debian archives, treat the
    // Proxmox repositories as third-party and switch them off, and leave a
    // hypervisor running a kernel and a stack that no longer match. Proxmox
    // ships its own upgrade path with its own preflight (`pve8to9` and
    // friends) that knows about cluster quorum, storage and the kernel.
    if std::path::Path::new("/usr/bin/pveversion").exists()
        || repo_list.iter().any(|r| r.uri.contains("proxmox.com"))
    {
        stop.push(
            "this is a Proxmox host. Use Proxmox's own release upgrade, which checks the \
             cluster and storage first: run `pve8to9` (or the matching checker for your \
             version) and follow the upgrade guide. PatchPanel will keep patching it \
             normally in the meantime."
                .to_string(),
        );
    }

    // Broken or half-configured packages become a failed upgrade, every time.
    if let Ok(o) = exec::run("dpkg", &["--audit"], p).await {
        let t = o.text.trim();
        if !t.is_empty() {
            stop.push(format!("dpkg reports packages in a bad state:\n{}", exec::tail(t, 600)));
        }
    }

    // Everything available on the current release must be installed first;
    // upgrading from a half-patched machine is how a release upgrade turns
    // into an afternoon.
    let phased = deferred(pf, p).await;
    let (pending, _) = available_updates(pf, p).await?;
    let outstanding: Vec<&AvailableUpdate> = pending
        .iter()
        .filter(|u| !phased.contains(&u.name))
        .collect();
    if !outstanding.is_empty() {
        stop.push(format!(
            "{} update(s) are still pending on {}. Install updates first.",
            outstanding.len(),
            rel.codename
        ));
    }

    // Space. A release upgrade downloads and unpacks the whole system.
    if let Some(free) = free_bytes("/") {
        let gb = free as f64 / 1e9;
        log.push(format!("{gb:.1} GB free on /"));
        if free < 5_000_000_000 {
            stop.push(format!(
                "only {gb:.1} GB free on /. A release upgrade needs several GB; \
                 clear space first."
            ));
        }
    }

    // Third-party repos rarely have anything for the new release on day one,
    // and apt will happily strand packages because of it. They get disabled
    // for the duration rather than silently left to break the run.
    let third_party: Vec<&pp_proto::Repository> = repo_list
        .iter()
        .filter(|r| r.source == "apt" && r.enabled && !r.uri.contains("debian.org"))
        .collect();

    // Ask each one whether it has anything for the target, rather than
    // assuming. Glenn R's UniFi repo publishes per Debian release, so on a
    // bullseye box it can simply be moved to bookworm; Docker's does too.
    let mut movable: Vec<String> = Vec::new();
    let (mut ready, mut outside): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    for r in &third_party {
        let label = format!("{} {}", r.uri, r.suite);
        if r.suite.contains(to) {
            ready.push(label);
        } else if publishes_suite(&r.uri, to).await {
            movable.push(r.uri.clone());
            ready.push(format!("{label}  ->  {to}"));
        } else {
            outside.push(label);
        }
    }
    if !ready.is_empty() {
        log.push(format!(
            "{} third-party source(s) publish for {to} and stay enabled:\n  {}",
            ready.len(),
            ready.join("\n  ")
        ));
    }
    if !outside.is_empty() {
        log.push(format!(
            "{} third-party source(s) have nothing at dists/{to}/ and will be disabled for \
             the upgrade, left off afterwards for you to re-enable once they publish:\n  {}",
            outside.len(),
            outside.join("\n  ")
        ));
    }

    // What the sources will become. Computing it now means `check` can show
    // the operator the exact rewrite before anything is touched.
    let plan = plan_sources(&repos, &rel.codename, to, &movable);
    if plan.is_empty() {
        stop.push(format!(
            "no source file mentions `{}`, so there is nothing to repoint at `{to}`",
            rel.codename
        ));
    }
    for (path, _, note) in &plan {
        log.push(format!("{path}: {note}"));
    }

    if !stop.is_empty() {
        let body = stop
            .iter()
            .enumerate()
            .map(|(i, s)| format!("{}. {s}", i + 1))
            .collect::<Vec<_>>()
            .join("\n");
        if check {
            return Ok(format!(
                "{}\n\nNOT READY - {} blocker(s):\n{body}",
                log.join("\n"),
                stop.len()
            ));
        }
        anyhow::bail!("refusing to upgrade, {} blocker(s):\n{body}", stop.len());
    }

    if check {
        return Ok(format!(
            "{}\n\nREADY. Nothing has been changed.\n\n\
             Take a snapshot of this machine before upgrading: PatchPanel cannot undo a \
             release upgrade, and a virtual machine or container that has one is the only \
             cheap way back.",
            log.join("\n")
        ));
    }

    // -------------------------------------------------------------- the upgrade
    p.line("finishing the current release before moving");
    let o = exec::run_with_timeout(
        "env",
        &[
            &["DEBIAN_FRONTEND=noninteractive", "apt-get", "-y"][..],
            &KEEP_CONF[..],
            &["full-upgrade"][..],
        ]
        .concat(),
        p,
        UPGRADE_TIMEOUT,
    )
    .await?;
    o.require(&[])?;
    log.push("current release fully upgraded".into());

    p.line(&format!("repointing apt at {to}"));
    let mut written: Vec<(String, String)> = Vec::new();
    for (path, content, _) in &plan {
        // Keep our own copy: sources::write keeps one too, but it is overwritten
        // by any later edit, and this one has to survive the whole upgrade.
        let original = std::fs::read_to_string(path).unwrap_or_default();
        std::fs::write(format!("{path}.pre-{to}"), &original).ok();
        std::fs::write(path, content)
            .with_context(|| format!("writing {path}"))?;
        written.push((path.clone(), original));
    }

    let o = exec::run("apt-get", &["update"], p).await?;
    if !o.ok() {
        for (path, original) in &written {
            std::fs::write(path, original).ok();
        }
        let _ = exec::run("apt-get", &["update"], p).await;
        anyhow::bail!(
            "apt rejected the {to} sources, so they have been put back and nothing was \
             upgraded:\n{}",
            exec::tail(&o.text, 800)
        );
    }
    log.push(format!("apt is now reading {to}"));

    // Debian's documented order: get everything that can move without adding
    // packages first, then let the full upgrade do the rest. Doing it in one
    // step works until it doesn't, and when it doesn't the machine is halfway.
    p.line("stage 1 of 2: upgrading without adding packages");
    let o = exec::run_with_timeout(
        "env",
        &[
            &["DEBIAN_FRONTEND=noninteractive", "apt-get", "-y"][..],
            &KEEP_CONF[..],
            &["upgrade", "--without-new-pkgs"][..],
        ]
        .concat(),
        p,
        UPGRADE_TIMEOUT,
    )
    .await?;
    log.push(format!("stage 1 exit {}", o.code));
    if !o.ok() {
        anyhow::bail!(
            "stage 1 failed, and this machine is now part way between {} and {to}. \
             Its sources point at {to} and the previous ones are saved beside them as \
             `.pre-{to}`. Resolve it on the machine before rebooting:\n{}",
            rel.codename,
            exec::tail(&o.text, 1200)
        );
    }

    p.line("stage 2 of 2: full upgrade");
    let o = exec::run_with_timeout(
        "env",
        &[
            &["DEBIAN_FRONTEND=noninteractive", "apt-get", "-y"][..],
            &KEEP_CONF[..],
            &["full-upgrade"][..],
        ]
        .concat(),
        p,
        UPGRADE_TIMEOUT,
    )
    .await?;
    log.push(format!("stage 2 exit {}", o.code));
    if !o.ok() {
        anyhow::bail!(
            "stage 2 failed. Most packages are on {to}; finish it on the machine with \
             `apt full-upgrade`:\n{}",
            exec::tail(&o.text, 1200)
        );
    }

    log.push(format!(
        "upgraded to {to}. Reboot to start the new kernel, then run a cleanup to remove \
         what {} left behind.",
        rel.codename
    ));
    Ok(log.join("\n"))
}

/// Does this repository publish the suite we are about to move to?
///
/// Guessing is the wrong answer in both directions: disabling a third-party
/// repo that has a build for the target strands the packages it provides, and
/// repointing one that does not breaks `apt-get update` outright. Every apt
/// repository serves `dists/<suite>/Release`, so ask.
async fn publishes_suite(uri: &str, suite: &str) -> bool {
    let url = format!("{}/dists/{suite}/Release", uri.trim_end_matches('/'));
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
    else {
        return false;
    };
    match client.get(&url).send().await {
        Ok(r) => r.status().is_success(),
        // A repo we cannot reach is one we cannot vouch for, and disabling it
        // for the upgrade is the safe half of the guess.
        Err(_) => false,
    }
}

/// Free bytes on the filesystem holding `path`.
fn free_bytes(path: &str) -> Option<u64> {
    let out = std::process::Command::new("df")
        .args(["--output=avail", "-B1", path])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .nth(1)?
        .trim()
        .parse()
        .ok()
}

/// The rewrite each source file needs, without performing it.
///
/// Returns `(path, new content, what changed)` for every file that changes.
/// Debian's own archives are repointed at the new release; anything else is
/// commented out, because a third-party repo with no build for the target is
/// the most common way a release upgrade strands a machine.
fn plan_sources(
    files: &[pp_proto::SourceFile],
    from: &str,
    to: &str,
    movable: &[String],
) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for f in files {
        let mut repointed = 0;
        let mut disabled = 0;
        let mut moved = 0;
        let mut kept = 0;
        let mut lines: Vec<String> = Vec::new();

        for line in f.content.lines() {
            let t = line.trim();
            if t.starts_with('#') || !(t.starts_with("deb ") || t.starts_with("deb-src ")) {
                lines.push(line.to_string());
                continue;
            }
            let fields: Vec<&str> = t.split_whitespace().collect();
            let Some(ui) = fields.iter().position(|x| x.contains("://")) else {
                lines.push(line.to_string());
                continue;
            };
            if fields[ui].contains("debian.org") {
                if fields.len() > ui + 1 && fields[ui + 1].contains(from) {
                    let suite = fields[ui + 1];
                    // An end-of-life release is served by archive.debian.org
                    // and nothing else; the release being upgraded to is not
                    // there yet. Carrying the archive host across the upgrade
                    // produces sources that 404, which is a wasted attempt.
                    let uri = if crate::sources::is_eol(to) {
                        fields[ui].to_string()
                    } else if fields[ui].contains("archive.debian.org/debian-security") {
                        moved += 1;
                        fields[ui].replace(
                            "archive.debian.org/debian-security",
                            "security.debian.org/debian-security",
                        )
                    } else if fields[ui].contains("archive.debian.org") {
                        moved += 1;
                        fields[ui].replace("archive.debian.org", "deb.debian.org")
                    } else {
                        fields[ui].to_string()
                    };
                    // Debian renamed the security suite at 12, so a bullseye
                    // machine's `bullseye/updates` becomes `bookworm-security`
                    // rather than `bookworm/updates`, which does not exist.
                    let new_suite = if suite.ends_with("/updates") || suite.ends_with("-security") {
                        format!("{to}-security")
                    } else {
                        suite.replace(from, to)
                    };
                    let mut f2: Vec<String> = fields.iter().map(|x| x.to_string()).collect();
                    f2[ui] = uri;
                    f2[ui + 1] = new_suite;
                    lines.push(f2.join(" "));
                    repointed += 1;
                    continue;
                }
                lines.push(line.to_string());
            } else if fields.len() > ui + 1 && fields[ui + 1].contains(to) {
                lines.push(line.to_string());
                kept += 1;
            } else if movable.iter().any(|u| u == fields[ui]) {
                // Verified to publish for the target, so move it across with
                // the rest instead of leaving the machine without it.
                let mut f2: Vec<String> = fields.iter().map(|x| x.to_string()).collect();
                f2[ui + 1] = fields[ui + 1].replace(from, to);
                lines.push(f2.join(" "));
                repointed += 1;
            } else {
                lines.push(format!("# disabled by PatchPanel for the {to} upgrade: {t}"));
                disabled += 1;
            }
        }

        if repointed == 0 && disabled == 0 {
            continue;
        }
        let mut note = match (repointed, disabled) {
            (r, 0) => format!("{r} line(s) repointed at {to}"),
            (0, d) => format!("{d} third-party line(s) disabled"),
            (r, d) => format!("{r} line(s) repointed at {to}, {d} third-party line(s) disabled"),
        };
        if kept > 0 {
            note.push_str(&format!(
                ", {kept} third-party line(s) left enabled because they already publish for {to}"
            ));
        }
        if moved > 0 {
            note.push_str(&format!(
                " ({moved} moved off archive.debian.org, which only carries EOL releases)"
            ));
        }
        out.push((f.path.clone(), lines.join("\n") + "\n", note));
    }
    out
}

// ---------------------------------------------------------------------------
// Whether a repository can actually deliver
// ---------------------------------------------------------------------------

/// Check that the packages a repository advertises are really there.
///
/// A source can pass every test apt applies at `update` time and still be
/// useless: Debian empties a release's pool when it moves to the archive, and
/// for a while the indices remain, listing packages that now 404. `apt upgrade`
/// downloads 294 MB and then fails on the missing ones, which is a confusing
/// way to find out. Ask apt for the URLs it would fetch and try a couple.
pub async fn repo_problems(pf: &Platform, repos: &[Repository], p: &Progress) -> Vec<(String, String)> {
    if !pf.has(Backend::Apt) {
        return Vec::new();
    }
    let Ok(o) = exec::run("apt-get", &["-y", "--print-uris", "upgrade"], p).await else {
        return Vec::new();
    };

    // `'http://host/pool/...deb' name size SHA256:...`
    let uris: Vec<String> = o
        .text
        .lines()
        .filter_map(|l| l.trim().strip_prefix('\''))
        .filter_map(|l| l.split('\'').next())
        .filter(|u| u.starts_with("http"))
        .map(str::to_string)
        .collect();
    if uris.is_empty() {
        return Vec::new();
    }

    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
    else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for repo in repos.iter().filter(|r| r.source == "apt" && r.enabled) {
        let base = repo.uri.trim_end_matches('/');
        // Two is enough to tell an emptied pool from one missing file, and
        // keeps a scan from turning into a crawl of the archive.
        let sample: Vec<&String> = uris.iter().filter(|u| u.starts_with(base)).take(2).collect();
        if sample.is_empty() {
            continue;
        }

        let mut missing = 0;
        for u in &sample {
            if !exists(&client, u).await {
                missing += 1;
            }
        }
        if missing == sample.len() {
            out.push((
                format!("{} {}", repo.uri, repo.suite),
                format!(
                    "the index lists packages this server no longer has ({} of {} sampled \
                     returned 404). Nothing can be installed from it: a patch run will \
                     download everything else and then fail. For a release being retired \
                     this is normal and permanent - the files move to archive.debian.org, \
                     or are gone for good.",
                    missing,
                    sample.len()
                ),
            ));
        }
    }
    out
}

/// Is this file actually served? HEAD where the server allows it.
async fn exists(client: &reqwest::Client, url: &str) -> bool {
    match client.head(url).send().await {
        Ok(r) if r.status().is_success() => true,
        // Some mirrors refuse HEAD outright; ask for one byte instead of
        // calling the package missing on the strength of a 405.
        Ok(r) if r.status().as_u16() == 405 || r.status().as_u16() == 501 => client
            .get(url)
            .header("Range", "bytes=0-0")
            .send()
            .await
            .is_ok_and(|r| r.status().is_success()),
        Ok(_) => false,
        // A network failure is not evidence the file is gone.
        Err(_) => true,
    }
}

// ---------------------------------------------------------------------------
// Recovering an interrupted upgrade
// ---------------------------------------------------------------------------

/// Is dpkg stuck part-way through an upgrade, and if so, on what?
///
/// dpkg stops at the first postinst that fails and then refuses to do anything
/// at all until someone resolves it. Nothing else on the machine reports this:
/// the update list still looks normal while every install silently cannot run.
pub async fn mid_upgrade(pf: &Platform, p: &Progress) -> Option<MidUpgrade> {
    if !pf.has(Backend::Apt) {
        return None;
    }
    // `dpkg --audit` writes prose - a paragraph of explanation, then the
    // package name padded out with its description - and misreading it cost a machine
    // an hour of looking fine while grub-pc was wedged. Ask for the states
    // directly instead.
    let o = exec::run(
        "dpkg-query",
        &["-W", "-f=${Package} ${Status}\n"],
        p,
    )
    .await
    .ok()?;

    let packages: Vec<String> = o
        .text
        .lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            let name = w.next()?;
            let want = w.next()?; // install / hold / deinstall
            let _err = w.next()?; // ok / reinstreq
            let state = w.next()?; // installed / half-configured / ...
            // `config-files` is a package removed but not purged, which is a
            // normal resting state, not a machine that is stuck.
            let stuck = matches!(
                state,
                "half-configured"
                    | "half-installed"
                    | "unpacked"
                    | "triggers-awaited"
                    | "triggers-pending"
            );
            (stuck && want != "deinstall").then(|| format!("{name} ({state})"))
        })
        .collect();
    if packages.is_empty() {
        return None;
    }

    // A bootloader package cannot be configured without knowing which disk to
    // write to, and the recorded answer is often a device that no longer
    // exists - a disk replaced, or a VM moved between hosts.
    let grub_stuck = packages.iter().any(|k| k.starts_with("grub"));
    let disks = if grub_stuck { list_disks(p).await } else { Vec::new() };

    Some(MidUpgrade {
        packages,
        grub_stuck,
        disks,
    })
}

/// Whole disks this machine could install a bootloader onto.
async fn list_disks(p: &Progress) -> Vec<Disk> {
    let Ok(o) = exec::run("lsblk", &["-dpno", "NAME,SIZE,MODEL", "--nodeps"], p).await else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for line in o.text.lines() {
        let mut it = line.split_whitespace();
        let (Some(path), Some(size)) = (it.next(), it.next()) else {
            continue;
        };
        // Loop and ram devices are not somewhere a bootloader goes.
        if path.starts_with("/dev/loop") || path.starts_with("/dev/ram") || path.starts_with("/dev/sr") {
            continue;
        }
        let model = it.collect::<Vec<_>>().join(" ");

        // The by-id names are what grub records, so showing them lets the
        // operator recognise the disk the old entry was talking about.
        let mut by_id = Vec::new();
        if let Ok(dir) = std::fs::read_dir("/dev/disk/by-id") {
            for e in dir.flatten() {
                if std::fs::canonicalize(e.path()).is_ok_and(|t| t.to_string_lossy() == path) {
                    by_id.push(e.file_name().to_string_lossy().into_owned());
                }
            }
        }
        by_id.sort();

        out.push(Disk {
            path: path.to_string(),
            size: size.to_string(),
            model,
            by_id,
        });
    }
    out
}

/// Configure what is half-installed, then carry on with the upgrade.
pub async fn finish_upgrade(
    pf: &Platform,
    grub_device: Option<&str>,
    p: &Progress,
) -> Result<String> {
    if !pf.has(Backend::Apt) {
        anyhow::bail!("this only applies to apt machines");
    }
    let mut log = Vec::new();

    if let Some(dev) = grub_device {
        // debconf holds the answer grub's postinst asks for. Setting it is the
        // supported way to answer a question nobody is there to see.
        p.line(&format!("pointing the bootloader at {dev}"));
        let line = format!("grub-pc grub-pc/install_devices multiselect {dev}\n");
        let tmp = "/tmp/pp-grub-selection";
        std::fs::write(tmp, &line).with_context(|| format!("writing {tmp}"))?;
        let o = exec::run("sh", &["-c", &format!("debconf-set-selections < {tmp}")], p).await?;
        let _ = std::fs::remove_file(tmp);
        log.push(format!("bootloader device set to {dev} (exit {})", o.code));
    }

    p.line("configuring what dpkg left unfinished");
    let o = exec::run_with_timeout(
        "env",
        &[
            &["DEBIAN_FRONTEND=noninteractive", "dpkg", "--configure", "-a"][..],
        ]
        .concat(),
        p,
        UPGRADE_TIMEOUT,
    )
    .await?;
    log.push(format!("dpkg --configure -a exit {}", o.code));
    if !o.ok() {
        anyhow::bail!(
            "{}\n\ndpkg still cannot configure everything:\n{}",
            log.join("\n"),
            exec::tail(&o.text, 1500)
        );
    }

    p.line("resolving anything left half-installed");
    let o = exec::run_with_timeout(
        "env",
        &[
            &["DEBIAN_FRONTEND=noninteractive", "apt-get", "-y"][..],
            &KEEP_CONF[..],
            &["-f", "install"][..],
        ]
        .concat(),
        p,
        UPGRADE_TIMEOUT,
    )
    .await?;
    log.push(format!("apt-get -f install exit {}", o.code));

    p.line("continuing the upgrade");
    let o = exec::run_with_timeout(
        "env",
        &[
            &["DEBIAN_FRONTEND=noninteractive", "apt-get", "-y"][..],
            &KEEP_CONF[..],
            &["full-upgrade"][..],
        ]
        .concat(),
        p,
        UPGRADE_TIMEOUT,
    )
    .await?;
    log.push(format!("full-upgrade exit {}", o.code));
    if !o.ok() {
        anyhow::bail!(
            "{}\n\nthe upgrade still cannot finish:\n{}",
            log.join("\n"),
            exec::tail(&o.text, 1500)
        );
    }

    log.push("upgrade completed. Reboot to start the new kernel.".into());
    Ok(log.join("\n"))
}

// ---------------------------------------------------------------------------
// Firmware, via fwupd
// ---------------------------------------------------------------------------

/// Firmware updates offered for this machine's hardware.
///
/// fwupd is the vendor-neutral path: system firmware, SSDs, docks, Thunderbolt
/// controllers and anything else with a LVFS entry. Machines without it simply
/// report nothing rather than an error - a container has no firmware, and
/// saying so on every scan would be noise.
pub async fn firmware(
    pf: &Platform,
    p: &Progress,
) -> (Vec<FirmwareUpdate>, Vec<FirmwareDevice>, Option<ScanIssue>) {
    if !pf.has(Backend::Fwupd) {
        return (Vec::new(), Vec::new(), None);
    }

    // What the machine has, before asking what is available. A device list
    // that comes back empty means fwupd itself could not answer, which is
    // worth saying rather than rendering as "nothing to do".
    let devices = match exec::run("fwupdmgr", &["get-devices", "--json"], p).await {
        Ok(o) => parse_fwupd_devices(&o.text),
        Err(_) => Vec::new(),
    };
    if devices.is_empty() {
        return (
            Vec::new(),
            Vec::new(),
            Some(ScanIssue {
                backend: "fwupd".into(),
                problem: "fwupd is installed but reported no devices".into(),
                remedy: "Firmware on this machine is not being checked. The daemon may not \
                         be running (`systemctl status fwupd`), or this hardware may expose \
                         nothing fwupd understands - which is normal for a virtual machine."
                    .into(),
            }),
        );
    }

    // Without fresh metadata fwupd only knows about firmware it has already
    // heard of. It rate-limits itself and fails harmlessly when the metadata
    // is current, so the result is deliberately ignored.
    let _ = exec::run("fwupdmgr", &["refresh", "--force"], p).await;

    let updates = match exec::run("fwupdmgr", &["get-updates", "--json"], p).await {
        // fwupd exits non-zero when there is nothing to do, which is not a fault.
        Ok(o) => parse_fwupd(&o.text),
        Err(_) => Vec::new(),
    };
    (updates, devices, None)
}

fn parse_fwupd_devices(text: &str) -> Vec<FirmwareDevice> {
    let Ok(doc): Result<serde_json::Value, _> = serde_json::from_str(text) else {
        return Vec::new();
    };
    let Some(devices) = doc.get("Devices").and_then(|d| d.as_array()) else {
        return Vec::new();
    };
    devices
        .iter()
        .map(|dev| {
            let str_of = |k: &str| dev.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let flags: Vec<&str> = dev
                .get("Flags")
                .and_then(|f| f.as_array())
                .map(|f| f.iter().filter_map(|x| x.as_str()).collect())
                .unwrap_or_default();
            FirmwareDevice {
                name: str_of("Name"),
                vendor: str_of("Vendor"),
                version: str_of("Version"),
                updatable: flags.contains(&"updatable"),
            }
        })
        .filter(|d| !d.name.is_empty())
        .collect()
}

fn parse_fwupd(text: &str) -> Vec<FirmwareUpdate> {
    let Ok(doc): Result<serde_json::Value, _> = serde_json::from_str(text) else {
        return Vec::new();
    };
    let Some(devices) = doc.get("Devices").and_then(|d| d.as_array()) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for dev in devices {
        let str_of = |k: &str| dev.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        // The newest release is the one on offer; fwupd lists them newest
        // first, and a device with none is simply up to date.
        let Some(rel) = dev
            .get("Releases")
            .and_then(|r| r.as_array())
            .and_then(|r| r.first())
        else {
            continue;
        };
        let rel_str = |k: &str| rel.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();

        let flags: Vec<String> = dev
            .get("Flags")
            .and_then(|f| f.as_array())
            .map(|f| {
                f.iter()
                    .filter_map(|x| x.as_str())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        out.push(FirmwareUpdate {
            device_id: str_of("DeviceId"),
            device: {
                let name = str_of("Name");
                let vendor = str_of("Vendor");
                if vendor.is_empty() || name.starts_with(&vendor) {
                    name
                } else {
                    format!("{vendor} {name}")
                }
            },
            current: str_of("Version"),
            available: rel_str("Version"),
            // The description is HTML in fwupd's output; the tags carry no
            // meaning worth keeping in a table cell.
            summary: strip_tags(&rel_str("Description")),
            needs_reboot: flags.iter().any(|f| f == "needs-reboot"),
            caution: strip_tags(&rel_str("DetachCaption")),
        });
    }
    out
}

/// fwupd hands back vendor descriptions as HTML.
fn strip_tags(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                // Paragraph and list breaks are the only structure worth
                // keeping, and a space preserves the sentence boundary.
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Flash firmware. Nothing else in PatchPanel can brick hardware, so this is
/// only ever reached by an operator asking for it explicitly.
pub async fn update_firmware(pf: &Platform, only: &[String], p: &Progress) -> Result<String> {
    if !pf.has(Backend::Fwupd) {
        anyhow::bail!("fwupd is not installed on this machine");
    }
    let mut log = Vec::new();

    if only.is_empty() {
        p.line("flashing every firmware update on offer");
        let o = exec::run_with_timeout(
            "fwupdmgr",
            &["update", "--assume-yes", "--no-reboot-check"],
            p,
            Duration::from_secs(30 * 60),
        )
        .await?;
        log.push(format!("fwupdmgr update exit {}", o.code));
        log.push(exec::tail(&o.text, 3000));
        if !o.ok() {
            anyhow::bail!("{}", log.join("\n"));
        }
    } else {
        for id in only {
            p.line(&format!("flashing {id}"));
            let o = exec::run_with_timeout(
                "fwupdmgr",
                &["update", id, "--assume-yes", "--no-reboot-check"],
                p,
                Duration::from_secs(30 * 60),
            )
            .await?;
            log.push(format!("{id}: exit {}", o.code));
            log.push(exec::tail(&o.text, 1500));
        }
    }

    log.push(
        "firmware is written but most of it only takes effect at the next boot, and some \
         needs a full power cycle rather than a warm reboot."
            .into(),
    );
    Ok(log.join("\n"))
}

// ---------------------------------------------------------------------------
// Why the machine last restarted
// ---------------------------------------------------------------------------

/// Markers worth naming, in the order we would rather report them: a panic
/// explains an OOM that followed it, not the other way round.
const CRASH_MARKERS: &[(&str, &str)] = &[
    ("Kernel panic", "kernel panic"),
    ("BUG: unable to handle", "kernel fault"),
    ("watchdog: BUG: soft lockup", "cpu lockup"),
    ("Out of memory: Killed", "out of memory"),
    ("oom-kill:", "out of memory"),
    ("I/O error", "storage errors"),
    ("EXT4-fs error", "filesystem errors"),
    ("thermal", "thermal event"),
];

/// Did this machine restart cleanly, and if not, what do its logs say?
pub async fn boot_report(p: &Progress) -> Option<BootReport> {
    // systemd records a clean shutdown in the previous boot's journal. Its
    // absence is what "unexpected" means here: the machine stopped without
    // being asked to.
    let prev = exec::run(
        "journalctl",
        &["-b", "-1", "-n", "400", "--no-pager", "-o", "short"],
        p,
    )
    .await
    .ok()?;

    // No previous boot recorded at all - a machine with a volatile journal, or
    // one that has only ever booted once. Nothing can be said honestly.
    if !prev.ok() || prev.text.trim().is_empty() {
        return None;
    }

    let clean = prev.text.contains("Shutting down")
        || prev.text.contains("Reached target Power-Off")
        || prev.text.contains("Reached target Reboot")
        || prev.text.contains("systemd-shutdown")
        || prev.text.contains("Unmounted /");

    if clean {
        return Some(BootReport {
            unexpected: false,
            summary: "clean shutdown".into(),
            detail: String::new(),
        });
    }

    // Something stopped it. The last lines before it went are the evidence.
    let mut cause = None;
    for (marker, verdict) in CRASH_MARKERS {
        if prev.text.contains(marker) {
            cause = Some(*verdict);
            break;
        }
    }

    let evidence: Vec<&str> = prev
        .text
        .lines()
        .filter(|l| CRASH_MARKERS.iter().any(|(m, _)| l.contains(m)))
        .take(8)
        .collect();
    let tail: Vec<&str> = prev.text.lines().rev().take(12).collect();

    Some(BootReport {
        unexpected: true,
        summary: cause
            .unwrap_or(
                "stopped without shutting down - power loss, a host reset, or a hard kill",
            )
            .to_string(),
        detail: if evidence.is_empty() {
            format!(
                "The previous boot's log ends without any shutdown having been started. \
                 Its last lines were:\n{}",
                tail.into_iter().rev().collect::<Vec<_>>().join("\n")
            )
        } else {
            evidence.join("\n")
        },
    })
}

// ---------------------------------------------------------------------------
// Virtualization
// ---------------------------------------------------------------------------

/// Does this machine host virtual machines, or is it one?
pub async fn virtualization(p: &Progress) -> Option<Virtualization> {
    let mut guests = Vec::new();
    let mut note = String::new();
    let mut host = false;
    let mut platform = String::new();

    if std::path::Path::new("/usr/bin/pveversion").exists() {
        host = true;
        platform = "proxmox".into();
        // `qm` and `pct` are the supported way to ask, and both need root -
        // which the agent has, being a system service.
        match exec::run("qm", &["list"], p).await {
            Ok(o) if o.ok() => guests.extend(parse_pve(&o.text, "qemu")),
            _ => note.push_str("could not read the VM list from `qm list`. "),
        }
        match exec::run("pct", &["list"], p).await {
            Ok(o) if o.ok() => guests.extend(parse_pve(&o.text, "lxc")),
            _ => note.push_str("could not read the container list from `pct list`."),
        }
    }

    // Being a guest is worth recording too: it says where to look when the
    // machine misbehaves for reasons that are not its own.
    let mut role = if host { "host".to_string() } else { String::new() };
    if let Ok(o) = exec::run("systemd-detect-virt", &[], p).await {
        let kind = o.text.trim().to_string();
        if !kind.is_empty() && kind != "none" {
            role = if host { "host and guest".into() } else { "guest".into() };
            if platform.is_empty() {
                platform = kind;
            } else {
                platform = format!("{platform} (itself running under {kind})");
            }
        }
    }

    if role.is_empty() {
        return None;
    }
    Some(Virtualization {
        role,
        platform,
        guests,
        note: note.trim().to_string(),
    })
}

/// `qm list` and `pct list` both print a fixed-width table whose first column
/// is the id and whose header is the only line starting with whitespace or the
/// word VMID.
fn parse_pve(text: &str, kind: &str) -> Vec<Guest> {
    let mut out = Vec::new();
    for line in text.lines().skip(1) {
        let mut f = line.split_whitespace();
        let (Some(id), Some(a)) = (f.next(), f.next()) else {
            continue;
        };
        if !id.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        // `qm list` is VMID NAME STATUS ...; `pct list` is VMID STATUS [LOCK] NAME.
        let (name, state) = if kind == "qemu" {
            (a.to_string(), f.next().unwrap_or("").to_string())
        } else {
            (f.last().unwrap_or(a).to_string(), a.to_string())
        };
        out.push(Guest {
            id: id.to_string(),
            name,
            kind: kind.to_string(),
            state,
            managed: false,
        });
    }
    out
}

/// Install what this machine is missing in order to be fully scannable.
///
/// On Linux that means fwupd: without it a physical machine's firmware is
/// simply not looked at, and nothing says so. Package managers are already
/// present by definition - a machine without one could not have got here.
pub async fn install_prerequisites(pf: &Platform, p: &Progress) -> Result<String> {
    let mut log = Vec::new();

    if pf.has(Backend::Fwupd) {
        return Ok("fwupd is already installed; nothing else is missing".into());
    }

    if pf.has(Backend::Apt) {
        p.line("installing fwupd");
        exec::run("apt-get", &["-qq", "update"], p).await?;
        let o = exec::run_with_timeout(
            "env",
            &["DEBIAN_FRONTEND=noninteractive", "apt-get", "-y", "install", "fwupd"],
            p,
            Duration::from_secs(10 * 60),
        )
        .await?;
        log.push(format!("apt-get install fwupd exit {}", o.code));
        if !o.ok() {
            anyhow::bail!("{}\n{}", log.join("\n"), exec::tail(&o.text, 1200));
        }
    } else if pf.has(Backend::Dnf) {
        p.line("installing fwupd");
        let o = exec::run_with_timeout(
            "dnf",
            &["-y", "install", "fwupd"],
            p,
            Duration::from_secs(10 * 60),
        )
        .await?;
        log.push(format!("dnf install fwupd exit {}", o.code));
        if !o.ok() {
            anyhow::bail!("{}\n{}", log.join("\n"), exec::tail(&o.text, 1200));
        }
    } else {
        anyhow::bail!("no supported package manager to install fwupd with");
    }

    // Report what is true now rather than what the command claimed.
    if exec::have("fwupdmgr") {
        log.push(
            "fwupd installed. Restart the agent so it picks up the new backend - backends are \
             detected once at startup."
                .into(),
        );
        Ok(log.join("\n"))
    } else {
        anyhow::bail!("{}\n\nfwupdmgr is still not on PATH.", log.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real case: grub-pc's postinst failed, so dpkg left it
    /// half-configured and refused to do anything else. `dpkg --audit` says so
    /// only in prose, which is what the first attempt at this tried to read.
    #[test]
    fn spots_a_half_configured_package() {
        let out = "bash install ok installed\n\
                   grub-pc install ok half-configured\n\
                   libfoo deinstall ok config-files\n\
                   vim install ok installed\n";
        let stuck: Vec<String> = out
            .lines()
            .filter_map(|l| {
                let mut w = l.split_whitespace();
                let name = w.next()?;
                let want = w.next()?;
                let _err = w.next()?;
                let state = w.next()?;
                let stuck = matches!(
                    state,
                    "half-configured" | "half-installed" | "unpacked"
                        | "triggers-awaited" | "triggers-pending"
                );
                (stuck && want != "deinstall").then(|| format!("{name} ({state})"))
            })
            .collect();
        assert_eq!(stuck, vec!["grub-pc (half-configured)".to_string()]);
    }

    /// Verbatim from a Debian 11 host, whose security suite still uses the form
    /// Debian abandoned at 12, plus a vendor repo with nothing for bookworm.
    #[test]
    fn repoints_debian_and_disables_the_rest() {
        let f = pp_proto::SourceFile {
            path: "/etc/apt/sources.list".into(),
            content: "deb http://deb.debian.org/debian bullseye main contrib\n\
                      deb http://security.debian.org/debian-security bullseye/updates main\n\
                      deb http://deb.debian.org/debian bullseye-updates main\n\
                      # a comment\n\
                      deb https://download.docker.com/linux/debian bullseye stable\n"
                .into(),
            suggested: None,
            notes: Vec::new(),
        };
        let plan = plan_sources(std::slice::from_ref(&f), "bullseye", "bookworm", &[]);
        assert_eq!(plan.len(), 1);
        let new = &plan[0].1;

        assert!(new.contains("deb http://deb.debian.org/debian bookworm main contrib"));
        // The rename, not a mechanical substitution: `bookworm/updates` is not
        // a suite that exists.
        assert!(new.contains("bookworm-security main"), "{new}");
        assert!(!new.contains("bookworm/updates"), "{new}");
        assert!(new.contains("deb http://deb.debian.org/debian bookworm-updates main"));
        // Third-party lines are commented, never rewritten to a suite the
        // vendor may not publish.
        assert!(new.contains("# disabled by PatchPanel"), "{new}");
        assert!(!new.contains("\ndeb https://download.docker.com"), "{new}");
        assert!(new.contains("# a comment"));
    }

    /// An end-of-life machine pinned to archive.debian.org,
    /// because that is the only place bullseye still exists. bookworm is not
    /// there, so carrying the host across would produce sources that 404 and
    /// an upgrade that stops at the first `apt-get update`.
    #[test]
    fn brings_an_end_of_life_machine_back_to_the_live_mirrors() {
        let f = pp_proto::SourceFile {
            path: "/etc/apt/sources.list".into(),
            content: "deb http://archive.debian.org/debian bullseye main\n\
                      deb-src http://archive.debian.org/debian bullseye-updates main\n\
                      deb [trusted=yes] http://archive.debian.org/debian-security bullseye-security main\n"
                .into(),
            suggested: None,
            notes: Vec::new(),
        };
        let plan = plan_sources(std::slice::from_ref(&f), "bullseye", "bookworm", &[]);
        let new = &plan[0].1;
        assert!(new.contains("deb http://deb.debian.org/debian bookworm main"), "{new}");
        assert!(new.contains("deb-src http://deb.debian.org/debian bookworm-updates main"), "{new}");
        assert!(
            new.contains("deb [trusted=yes] http://security.debian.org/debian-security bookworm-security main"),
            "{new}"
        );
        assert!(!new.contains("archive.debian.org"), "{new}");
        assert!(plan[0].2.contains("archive.debian.org"), "the note should say what moved");
    }

    /// Between two dead releases the archive is still the right host.
    #[test]
    fn keeps_the_archive_when_the_target_is_also_end_of_life() {
        let f = pp_proto::SourceFile {
            path: "/etc/apt/sources.list".into(),
            content: "deb http://archive.debian.org/debian buster main\n".into(),
            suggested: None,
            notes: Vec::new(),
        };
        let plan = plan_sources(std::slice::from_ref(&f), "buster", "bullseye", &[]);
        assert!(plan[0].1.contains("archive.debian.org/debian bullseye main"), "{}", plan[0].1);
    }

    /// A MongoDB repo already on `bookworm` while the machine is on
    /// bullseye. Disabling it during the upgrade would remove the one source
    /// that has the packages the target release needs.
    #[test]
    fn keeps_a_third_party_repo_that_already_publishes_for_the_target() {
        let f = pp_proto::SourceFile {
            path: "/etc/apt/sources.list.d/mongodb-org-8.0.list".into(),
            content: "deb [ arch=amd64 signed-by=/etc/apt/keyrings/m.gpg ] \
                      https://repo.mongodb.org/apt/debian bookworm/mongodb-org/8.0 main\n\
                      deb [ arch=amd64 ] https://apt.glennr.nl/repo bullseye mongod/8.0\n"
                .into(),
            suggested: None,
            notes: Vec::new(),
        };
        let plan = plan_sources(std::slice::from_ref(&f), "bullseye", "bookworm", &[]);
        let new = &plan[0].1;
        assert!(new.contains("\ndeb [ arch=amd64 ] https://apt.glennr.nl") == false, "{new}");
        assert!(new.contains("# disabled by PatchPanel"), "{new}");
        // The bookworm one is untouched and still enabled.
        assert!(
            new.lines().any(|l| l.contains("repo.mongodb.org") && !l.trim_start().starts_with('#')),
            "{new}"
        );
    }


    /// UniFi packages come from Glenn R's repo, which publishes one
    /// suite per Debian release. Having checked that `dists/bookworm/` is
    /// there, moving it across is right; disabling it would leave the machine
    /// without the source for the software it exists to run.
    #[test]
    fn moves_a_third_party_repo_that_was_verified() {
        let f = pp_proto::SourceFile {
            path: "/etc/apt/sources.list.d/glennr-mongod-8.0.list".into(),
            content: "deb [ arch=amd64 signed-by=/etc/apt/keyrings/apt-glennr.gpg ] \
https://apt.glennr.nl/repo bullseye mongod/8.0\n"
                .into(),
            suggested: None,
            notes: Vec::new(),
        };
        let movable = vec!["https://apt.glennr.nl/repo".to_string()];
        let plan = plan_sources(std::slice::from_ref(&f), "bullseye", "bookworm", &movable);
        let new = &plan[0].1;
        assert!(new.contains("https://apt.glennr.nl/repo bookworm mongod/8.0"), "{new}");
        assert!(!new.contains("disabled by PatchPanel"), "{new}");
        // The options block survives; losing signed-by would break the repo.
        assert!(new.contains("signed-by=/etc/apt/keyrings/apt-glennr.gpg"), "{new}");
    }

    /// The same repo, unverified, is disabled rather than pointed at a suite
    /// that may not exist.
    #[test]
    fn disables_a_third_party_repo_that_was_not_verified() {
        let f = pp_proto::SourceFile {
            path: "/etc/apt/sources.list.d/glennr-mongod-8.0.list".into(),
            content: "deb https://apt.glennr.nl/repo bullseye mongod/8.0\n".into(),
            suggested: None,
            notes: Vec::new(),
        };
        let plan = plan_sources(std::slice::from_ref(&f), "bullseye", "bookworm", &[]);
        assert!(plan[0].1.contains("# disabled by PatchPanel"), "{}", plan[0].1);
    }
    #[test]
    fn leaves_a_file_with_nothing_to_do_alone() {
        let f = pp_proto::SourceFile {
            path: "/etc/apt/sources.list.d/nothing.list".into(),
            content: "# everything here is commented out\n#deb http://x/y z main\n".into(),
            suggested: None,
            notes: Vec::new(),
        };
        assert!(plan_sources(std::slice::from_ref(&f), "bullseye", "bookworm", &[]).is_empty());
    }

    #[test]
    fn a_disabled_line_stays_disabled() {
        let f = pp_proto::SourceFile {
            path: "/etc/apt/sources.list".into(),
            content: "#deb http://deb.debian.org/debian bullseye main\n\
                      deb http://deb.debian.org/debian bullseye main\n"
                .into(),
            suggested: None,
            notes: Vec::new(),
        };
        let plan = plan_sources(std::slice::from_ref(&f), "bullseye", "bookworm", &[]);
        let new = &plan[0].1;
        // Re-enabling a line the operator turned off would be a surprise, and
        // on an upgrade a dangerous one.
        assert!(new.starts_with("#deb http://deb.debian.org/debian bullseye main"), "{new}");
        assert_eq!(new.matches("bookworm").count(), 1, "{new}");
    }

    /// Verbatim from an Ubuntu box where a patch run kept reporting
    /// that it had done nothing. Both blocks matter: the second one reads as
    /// ordinary held-back packages, and every one of them is in fact waiting
    /// on a phased package in the first.
    const PHASED_OUTPUT: &str = "Reading package lists...
Building dependency tree...
Reading state information...
Calculating upgrade...
The following upgrades have been deferred due to phasing:
  base-files language-pack-de language-pack-de-base language-pack-en
  language-pack-en-base language-pack-es language-pack-es-base
  language-pack-fr language-pack-fr-base language-pack-gnome-de
  language-pack-gnome-en language-pack-gnome-en-base language-pack-gnome-es
  language-pack-gnome-es-base language-pack-gnome-fr-base
  language-pack-gnome-he language-pack-gnome-he-base language-pack-gnome-it
  language-pack-gnome-it-base language-pack-gnome-ja
  language-pack-gnome-ja-base language-pack-gnome-ko
  language-pack-gnome-ko-base language-pack-gnome-pt language-pack-gnome-ru
  language-pack-gnome-ru-base language-pack-gnome-zh-hans
  language-pack-gnome-zh-hans-base language-pack-gnome-zh-hant
  language-pack-gnome-zh-hant-base language-pack-he language-pack-he-base
  language-pack-it language-pack-it-base language-pack-ja
  language-pack-ja-base language-pack-ko language-pack-ko-base
  language-pack-pt language-pack-ru-base language-pack-zh-hans-base
  language-pack-zh-hant language-pack-zh-hant-base motd-news-config
  python-apt-common python3-apt python3-distupgrade
  ubuntu-release-upgrader-core ubuntu-release-upgrader-gtk
The following packages have been kept back:
  language-pack-gnome-de-base language-pack-gnome-fr
  language-pack-gnome-pt-base language-pack-pt-base language-pack-ru
  language-pack-zh-hans
0 upgraded, 0 newly installed, 0 to remove and 55 not upgraded.
";

    #[test]
    fn reads_both_blocks_apt_prints() {
        let phased = parse_phased(PHASED_OUTPUT);
        let kept = parse_kept_back(PHASED_OUTPUT);
        assert_eq!(phased.len(), 49, "{phased:?}");
        assert_eq!(kept.len(), 6, "{kept:?}");
        // 49 + 6 is the 55 apt says are not upgraded, so nothing is missed and
        // nothing is counted twice.
        assert_eq!(phased.len() + kept.len(), 55);
        assert!(phased.contains(&"language-pack-gnome-fr-base".to_string()));
        assert!(kept.contains(&"language-pack-gnome-fr".to_string()));
        // The summary line must never be read as a package name.
        assert!(!kept.iter().any(|k| k.starts_with('0')));
    }

    #[test]
    fn the_two_blocks_do_not_overlap() {
        let phased = parse_phased(PHASED_OUTPUT);
        let kept = parse_kept_back(PHASED_OUTPUT);
        assert!(
            !kept.iter().any(|k| phased.contains(k)),
            "a package in both buckets would be counted twice"
        );
    }

    #[test]
    fn a_machine_with_nothing_pending_reports_nothing() {
        let clean = "Reading package lists...\n\
                     Building dependency tree...\n\
                     0 upgraded, 0 newly installed, 0 to remove and 0 not upgraded.\n";
        assert!(parse_phased(clean).is_empty());
        assert!(parse_kept_back(clean).is_empty());
    }

    #[test]
    fn reads_a_lone_kept_back_block() {
        // Debian has no phased updates; the kernel metapackage case must still
        // land in held-back, where a full upgrade genuinely does fix it.
        let debian = "Calculating upgrade...\n\
                      The following packages have been kept back:\n  \
                      linux-image-amd64\n\
                      0 upgraded, 0 newly installed, 0 to remove and 1 not upgraded.\n";
        assert!(parse_phased(debian).is_empty());
        assert_eq!(parse_kept_back(debian), vec!["linux-image-amd64".to_string()]);
    }

    #[test]
    fn parses_both_shapes_of_apt_cache_output() {
        let depends = "language-pack-gnome-fr\n  \
                       Depends: language-pack-gnome-fr-base\n  \
                       Depends: language-pack-fr\n  \
                       Suggests: something-irrelevant\n\
                       language-pack-ru\n  \
                       PreDepends: libc6\n  \
                       Conflicts: <other>\n";
        let rel = parse_relations(depends);
        assert_eq!(rel.len(), 2);
        assert_eq!(rel[0].0, "language-pack-gnome-fr");
        assert!(rel[0].1.contains(&"language-pack-gnome-fr-base".to_string()));
        // A Suggests is not a reason anything is held back.
        assert!(!rel[0].1.contains(&"something-irrelevant".to_string()));
        assert_eq!(rel[1].1, vec!["libc6".to_string()]);

        let rdepends = "language-pack-gnome-de-base\n\
                        Reverse Depends:\n  \
                        language-pack-gnome-de\n  \
                        language-pack-gnome-de-extra (>= 1:22.04)\n";
        let rel = parse_relations(rdepends);
        assert_eq!(rel.len(), 1);
        assert_eq!(rel[0].0, "language-pack-gnome-de-base");
        assert_eq!(
            rel[0].1,
            vec![
                "language-pack-gnome-de".to_string(),
                // The version constraint is not part of the name.
                "language-pack-gnome-de-extra".to_string(),
            ]
        );
    }

    #[test]
    fn a_header_line_is_never_taken_for_a_package() {
        // "Reverse Depends:" sits at column 0 like a package name does.
        let rel = parse_relations("bash\nReverse Depends:\n  netscript-2.4\n");
        assert_eq!(rel.len(), 1);
        assert_eq!(rel[0].1, vec!["netscript-2.4".to_string()]);
    }
}
