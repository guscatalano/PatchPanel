//! Debian/Ubuntu (apt) and RHEL-family (dnf) backends.

use anyhow::Result;
use pp_proto::{AppSource, AvailableUpdate, Cleanup, Ensure, Package, ScanIssue};

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
pub async fn held_back(pf: &Platform, p: &Progress) -> Vec<String> {
    if !pf.has(Backend::Apt) {
        return Vec::new();
    }
    let Ok(o) = exec::run("apt-get", &["-s", "upgrade"], p).await else {
        return Vec::new();
    };

    let mut out = Vec::new();
    let mut in_block = false;
    for line in o.text.lines() {
        let t = line.trim();
        if t.starts_with("The following packages have been kept back") {
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
