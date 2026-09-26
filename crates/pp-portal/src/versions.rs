//! Watching applications PatchPanel cannot update.
//!
//! A package manager answers "is there a newer package in the repositories I
//! am configured for", which is a narrower question than "am I running an old
//! version". Software installed from a vendor's own repository pinned to an
//! old series, or updated by a script, reports nothing pending while the
//! vendor is several major versions ahead.
//!
//! The fetch happens here, once for the whole fleet, rather than on every
//! machine running the software: the answer is identical everywhere and
//! vendors do not deserve one request per host per hour.

use std::time::Duration;

use chrono::Utc;
use pp_proto::VersionCheck;

use crate::state::SharedState;

/// How often the loop wakes to see whether anything is due.
const TICK: Duration = Duration::from_secs(15 * 60);

/// Keep the published versions fresh, for as long as the portal runs.
pub fn spawn(state: SharedState) {
    tokio::spawn(async move {
        loop {
            if let Err(e) = refresh_due(&state).await {
                tracing::warn!(error = %format!("{e:#}"), "version check refresh failed");
            }
            tokio::time::sleep(TICK).await;
        }
    });
}

/// Fetch the checks whose last answer has gone stale.
async fn refresh_due(state: &SharedState) -> anyhow::Result<()> {
    let checks = state.db.manifest()?.version_checks;
    if checks.is_empty() {
        return Ok(());
    }
    let known = state.db.latest_versions()?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent(concat!("patchpanel-portal/", env!("CARGO_PKG_VERSION")))
        .build()?;

    for check in checks {
        let stale = match known.iter().find(|k| k.name == check.name) {
            // A check pointed somewhere new is stale immediately, whatever the
            // interval says: the old answer came from a different question.
            Some(k) if k.url != check.url => true,
            Some(k) => {
                Utc::now().signed_duration_since(k.checked_at).num_hours()
                    >= i64::from(check.check_after_hours)
            }
            None => true,
        };
        if !stale {
            continue;
        }

        match fetch_one(&client, &check).await {
            Ok(version) => {
                tracing::info!(name = %check.name, %version, "published version");
                state.db.set_latest_version(&check.name, &version, None, &check.url)?;
            }
            Err(e) => {
                // Keep whatever was last known: a vendor being unreachable is
                // not evidence that the software is current.
                let message = format!("{e:#}");
                tracing::warn!(name = %check.name, error = %message, "version check failed");
                let previous = known
                    .iter()
                    .find(|k| k.name == check.name)
                    .map(|k| k.version.clone())
                    .unwrap_or_default();
                state
                    .db
                    .set_latest_version(&check.name, &previous, Some(&message), &check.url)?;
            }
        }
    }
    Ok(())
}

async fn fetch_one(client: &reqwest::Client, check: &VersionCheck) -> anyhow::Result<String> {
    let body = client
        .get(&check.url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;

    let found = if let Some(pointer) = &check.json_pointer {
        let doc: serde_json::Value = serde_json::from_str(&body)?;
        doc.pointer(pointer)
            .map(|v| match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .ok_or_else(|| anyhow::anyhow!("nothing at {pointer} in the response"))?
    } else if let Some(pattern) = &check.regex {
        let re = regex::Regex::new(pattern)?;
        re.captures(&body)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
            .ok_or_else(|| anyhow::anyhow!("the pattern matched nothing"))?
    } else {
        body.trim().to_string()
    };

    let version = clean(&found);
    if version.is_empty() {
        anyhow::bail!("no version found in the response");
    }
    Ok(version)
}

/// Reduce a vendor's version string to the part worth comparing.
///
/// Vendors decorate them: `v10.6.101+atag-10.6.101-35991` is one release, and
/// the useful half is at the front.
fn clean(raw: &str) -> String {
    let trimmed = raw.trim().trim_start_matches(['v', 'V']);
    trimmed
        .split(['+', ' ', '~'])
        .next()
        .unwrap_or(trimmed)
        .to_string()
}

/// Is `latest` newer than `installed`?
///
/// Compares dotted numbers and ignores anything after them, so `10.6.101`
/// beats `9.0.114` - which a string comparison gets backwards - and a Debian
/// revision suffix does not count as a difference on its own.
pub fn is_behind(installed: &str, latest: &str) -> bool {
    // A release is its dotted numbers. Everything after the first `-`, `_` or
    // `+` is packaging: a Debian revision, a build id, a git hash. Counting
    // those as version components makes `1.2.3-2` look newer than `1.2.3`,
    // which is a difference nobody wants to be told about.
    let parts = |s: &str| -> Vec<u64> {
        let head = s
            .trim()
            .trim_start_matches(['v', 'V'])
            .split(['-', '_', '+', ' ', '~'])
            .next()
            .unwrap_or("");
        head.split('.')
            .take(4)
            .map_while(|p| p.parse().ok())
            .collect()
    };
    let (a, b) = (parts(installed), parts(latest));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    for i in 0..a.len().max(b.len()) {
        let (l, r) = (
            a.get(i).copied().unwrap_or(0),
            b.get(i).copied().unwrap_or(0),
        );
        if l != r {
            return r > l;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_a_vendors_decoration() {
        assert_eq!(clean("v10.6.101+atag-10.6.101-35991"), "10.6.101");
        assert_eq!(clean("  7.3.2 "), "7.3.2");
        assert_eq!(clean("26.1.11_5"), "26.1.11_5");
    }

    /// The case this exists for: a controller pinned to an old series while
    /// the vendor is a major version ahead, which apt reports as nothing
    /// pending because its repository only carries the old series.
    #[test]
    fn spots_a_major_version_behind() {
        assert!(is_behind("9.0.114-28033-1", "10.6.101"));
        assert!(!is_behind("10.6.101", "10.6.101"));
        assert!(!is_behind("10.6.102", "10.6.101"));
        // A string comparison would call 9 newer than 10.
        assert!(is_behind("9.9.9", "10.0.0"));
    }

    #[test]
    fn a_packaging_revision_is_not_a_new_version() {
        assert!(!is_behind("9.0.114-28033-1", "9.0.114"));
        assert!(!is_behind("1.2.3", "1.2.3-2"));
    }

    #[test]
    fn says_nothing_when_it_cannot_tell() {
        assert!(!is_behind("", "10.6.101"));
        assert!(!is_behind("unknown", "10.6.101"));
        assert!(!is_behind("9.0.114", ""));
    }
}
