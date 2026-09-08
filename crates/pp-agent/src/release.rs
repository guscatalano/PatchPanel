//! Distribution release state, and whether this machine is safe to upgrade.
//!
//! A major-release upgrade is the most destructive thing a fleet tool can do,
//! so the first job is not performing one — it is refusing to. Most broken
//! upgrades are broken before they start: sources pinned to a moving `stable`
//! suite, a third-party repo with no build for the target, security updates
//! quietly switched off. All of that is visible without touching anything.

use pp_proto::{ReleaseFinding, ReleaseInfo, Repository, Severity};

/// Debian's release order, so we can name the next hop rather than making the
/// operator remember whether bookworm follows bullseye.
const DEBIAN_ORDER: &[(&str, &str)] = &[
    ("stretch", "9"),
    ("buster", "10"),
    ("bullseye", "11"),
    ("bookworm", "12"),
    ("trixie", "13"),
    ("forky", "14"),
];

/// Suites that mean "whatever is current", which is precisely what you do not
/// want pinned on a machine you are not watching: the distribution moves
/// underneath it and an ordinary `apt upgrade` becomes a release jump.
const MOVING_SUITES: &[&str] = &["stable", "testing", "unstable", "oldstable", "sid"];

/// The codename Debian currently calls `stable`, straight from the archive.
///
/// Without this the release order below is just a list of names, and the one
/// after the newest release is whatever is in testing. Offering that as "the
/// next release" moves a server onto an unreleased distribution - which is
/// exactly what happened to one of these machines. Ask the archive instead of
/// hard-coding an answer that goes stale every two years.
pub async fn stable_codename() -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .ok()?;
    let text = client
        .get("https://deb.debian.org/debian/dists/stable/Release")
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix("Codename:"))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Is `codename` the same release as `other`, or an older one?
///
/// Used to refuse an upgrade to something newer than stable. An unknown
/// codename answers `false`: a name we do not recognise is not one we should
/// be moving a machine onto.
pub fn at_or_before(codename: &str, other: &str) -> bool {
    match (position(codename), position(other)) {
        (Some(a), Some(b)) => a <= b,
        _ => false,
    }
}

/// Where a codename sits in the release order, if we know it at all.
fn position(codename: &str) -> Option<usize> {
    DEBIAN_ORDER.iter().position(|(n, _)| *n == codename)
}

/// Has this release actually been released?
///
/// Unknown stable means unknown answer, and the callers treat that as "do not
/// offer an upgrade" rather than guessing.
fn is_released(codename: &str, stable: Option<&str>) -> Option<bool> {
    let want = position(codename)?;
    let now = position(stable?)?;
    Some(want <= now)
}

pub fn collect(repos: &[Repository], stable: Option<&str>) -> Option<ReleaseInfo> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let os = std::fs::read_to_string("/etc/os-release").ok()?;

    let field = |key: &str| -> String {
        os.lines()
            .find_map(|l| l.strip_prefix(key))
            .map(|v| v.trim_matches(['=', '"', '\'']).trim().to_string())
            .unwrap_or_default()
    };

    let distro = field("ID");
    let codename = {
        let c = field("VERSION_CODENAME");
        if c.is_empty() { field("VERSION_ID") } else { c }
    };
    let version_id = field("VERSION_ID");

    let mut findings = Vec::new();

    // A machine on a codename newer than stable is running testing, whatever
    // it was upgraded from. Say so: it is a different support model, and
    // /etc/os-release stops carrying a version number.
    if distro == "debian" && is_released(&codename, stable) == Some(false) {
        findings.push(ReleaseFinding {
            severity: Severity::Warning,
            summary: format!("running Debian testing ({codename})"),
            detail: format!(
                "{} is the current stable release; {codename} is still testing. Testing has \
                 no security team of its own and no version number, which is why this machine \
                 reports no release version.",
                stable.unwrap_or("the current release")
            ),
        });
    }

    // Only offer a hop to something that exists as a release.
    let next = match next_release(&distro, &codename) {
        Some(n) => match is_released(&n, stable) {
            Some(true) => Some(n),
            Some(false) => {
                findings.push(ReleaseFinding {
                    severity: Severity::Warning,
                    summary: format!("{codename} is the newest released Debian"),
                    detail: format!(
                        "The next codename in the sequence is {n}, but it is still testing. \
                         There is nothing to upgrade to yet."
                    ),
                });
                None
            }
            // The archive could not be reached, so we cannot tell a release
            // from testing. Offering the upgrade anyway is how a machine ends
            // up on an unreleased distribution.
            None => None,
        },
        None => None,
    };

    findings.extend(audit(&distro, &codename, next.as_deref(), stable, repos));

    let next_version = next
        .as_deref()
        .and_then(|n| DEBIAN_ORDER.iter().find(|(c, _)| *c == n))
        .map(|(_, v)| v.to_string());

    Some(ReleaseInfo {
        distro,
        codename,
        version_id,
        next,
        next_version,
        stable: stable.map(str::to_string),
        findings,
    })
}

/// Is this release far enough behind that Debian has stopped securing it?
///
/// stable and oldstable get security updates; anything older does not. Derived
/// from what the archive says stable is, so it stays true without being
/// edited every two years.
fn retired_release(codename: &str, stable: Option<&str>) -> bool {
    match (position(codename), stable.and_then(position)) {
        (Some(mine), Some(now)) => now.saturating_sub(mine) >= 2,
        _ => false,
    }
}

fn next_release(distro: &str, codename: &str) -> Option<String> {
    if distro != "debian" {
        // Ubuntu's sequence is time-based and its upgrades go through
        // do-release-upgrade, which has its own rules; do not guess.
        return None;
    }
    DEBIAN_ORDER.get(position(codename)? + 1).map(|(n, _)| n.to_string())
}

/// Everything that would make an upgrade — or even an ordinary `apt upgrade` —
/// unsafe on this machine.
fn audit(
    distro: &str,
    codename: &str,
    next: Option<&str>,
    stable: Option<&str>,
    repos: &[Repository],
) -> Vec<ReleaseFinding> {
    let mut out = Vec::new();
    let apt: Vec<&Repository> = repos
        .iter()
        .filter(|r| r.source == "apt" && r.enabled)
        .collect();

    if apt.is_empty() {
        return out;
    }

    // 1. Moving suites. The dangerous one: the machine is on release X but apt
    //    resolves to whatever is current, so "apply updates" is a release jump.
    let moving: Vec<&&Repository> = apt
        .iter()
        .filter(|r| {
            let base = r.suite.split('/').next().unwrap_or(&r.suite);
            MOVING_SUITES.contains(&base)
        })
        .collect();
    if !moving.is_empty() {
        out.push(ReleaseFinding {
            severity: Severity::Blocker,
            summary: format!(
                "{} source(s) track a moving suite instead of a release codename",
                moving.len()
            ),
            detail: format!(
                "This machine runs {distro} {codename}, but these sources follow whatever is \
                 current:\n{}\n\nInstalling updates would drag it across release boundaries \
                 unsupervised. Pin them to `{codename}` first.",
                moving
                    .iter()
                    .map(|r| format!("  {} {}   ({})", r.uri, r.suite, r.origin_file))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        });
    }

    // 2. Sources naming a different release than the one booted.
    let known: Vec<&str> = DEBIAN_ORDER.iter().map(|(n, _)| *n).collect();
    let mismatched: Vec<&&Repository> = apt
        .iter()
        .filter(|r| {
            let base = r.suite.split('-').next().unwrap_or(&r.suite);
            known.contains(&base) && base != codename && Some(base) != next
        })
        .collect();
    if !mismatched.is_empty() {
        out.push(ReleaseFinding {
            severity: Severity::Warning,
            summary: format!("{} source(s) name a different release", mismatched.len()),
            detail: mismatched
                .iter()
                .map(|r| format!("  {} {}   ({})", r.uri, r.suite, r.origin_file))
                .collect::<Vec<_>>()
                .join("\n"),
        });
    }

    // 3. Security updates switched off is worth saying loudly on its own -
    //    unless the release is old enough that Debian has stopped publishing
    //    them, in which case there is no line to add and calling it a blocker
    //    only stands in the way of the upgrade that actually fixes it.
    let has_security = apt
        .iter()
        .any(|r| r.uri.contains("security") || r.suite.contains("security"));
    if !has_security && distro == "debian" {
        let retired = retired_release(codename, stable);
        out.push(ReleaseFinding {
            severity: if retired { Severity::Warning } else { Severity::Blocker },
            summary: "no enabled security source".into(),
            detail: if retired {
                format!(
                    "This machine receives no security updates, and there is no source to \
                     add: Debian stopped publishing security updates for {codename} and the \
                     files have gone from security.debian.org. Upgrading{} is the fix.",
                    next.map(|n| format!(" to {n}")).unwrap_or_default()
                )
            } else {
                "This machine receives no security updates. Add the security suite for \
                 its release before doing anything else."
                    .to_string()
            },
        });
    }

    // 4. Third-party repos are the usual reason an upgrade strands packages:
    //    they rarely have a build ready for the new release on day one.
    if let Some(target) = next {
        let third_party: Vec<&&Repository> = apt
            .iter()
            .filter(|r| !r.uri.contains("debian.org") && !r.uri.starts_with("cdrom:"))
            .filter(|r| !r.suite.contains(target))
            .collect();
        if !third_party.is_empty() {
            out.push(ReleaseFinding {
                severity: Severity::Warning,
                summary: format!(
                    "{} third-party source(s) have nothing for {target}",
                    third_party.len()
                ),
                detail: format!(
                    "These would hold packages back or break the upgrade. Check each has a \
                     {target} suite, or disable it for the duration:\n{}",
                    third_party
                        .iter()
                        .map(|r| format!("  {} {}", r.uri, r.suite))
                        .collect::<Vec<_>>()
                        .join("\n")
                ),
            });
        }
    }

    out
}
