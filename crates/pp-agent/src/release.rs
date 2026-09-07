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

pub fn collect(repos: &[Repository]) -> Option<ReleaseInfo> {
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

    let next = next_release(&distro, &codename);
    let findings = audit(&distro, &codename, next.as_deref(), repos);

    Some(ReleaseInfo {
        distro,
        codename,
        version_id,
        next,
        findings,
    })
}

fn next_release(distro: &str, codename: &str) -> Option<String> {
    if distro != "debian" {
        // Ubuntu's sequence is time-based and its upgrades go through
        // do-release-upgrade, which has its own rules; do not guess.
        return None;
    }
    let i = DEBIAN_ORDER.iter().position(|(n, _)| *n == codename)?;
    DEBIAN_ORDER.get(i + 1).map(|(n, _)| n.to_string())
}

/// Everything that would make an upgrade — or even an ordinary `apt upgrade` —
/// unsafe on this machine.
fn audit(
    distro: &str,
    codename: &str,
    next: Option<&str>,
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

    // 3. Security updates switched off is worth saying loudly on its own.
    let has_security = apt
        .iter()
        .any(|r| r.uri.contains("security") || r.suite.contains("security"));
    if !has_security && distro == "debian" {
        out.push(ReleaseFinding {
            severity: Severity::Blocker,
            summary: "no enabled security source".into(),
            detail: "This machine receives no security updates. Add the security suite for \
                     its release before doing anything else."
                .into(),
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
