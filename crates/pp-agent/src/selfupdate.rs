//! Replacing the agent's own binary.
//!
//! The swap itself is deliberately dumb: verify, put the new file in place,
//! then exit and let the service supervisor start us again. Trying to
//! hand off in-process would mean the code performing the upgrade is the code
//! being replaced, which is exactly where self-updaters go wrong.
//!
//! This is why both service definitions restart on exit: systemd with
//! `Restart=always`, Windows with `sc failure ... actions= restart`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use crate::exec::Progress;

/// Suffix for the binary we displace. Windows cannot delete a running image,
/// but it can rename it, so the old file lingers until the next start.
const OLD_SUFFIX: &str = ".old";

/// Download, verify and install a new agent binary. Returns a summary; the
/// caller is responsible for exiting so the supervisor restarts us.
pub async fn apply(version: &str, url: &str, sha256: &str, p: &Progress) -> Result<String> {
    let exe = std::env::current_exe().context("locating current executable")?;
    let dir = exe.parent().context("executable has no parent directory")?;

    p.line(&format!("self-update -> {version} from {url}"));
    let bytes = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()?
        .get(url)
        .send()
        .await
        .context("downloading agent build")?
        .error_for_status()?
        .bytes()
        .await?;

    let got = hex::encode(Sha256::digest(&bytes));
    if !got.eq_ignore_ascii_case(sha256.trim()) {
        anyhow::bail!("agent build checksum mismatch: expected {sha256}, got {got}");
    }
    p.line(&format!("verified {} bytes", bytes.len()));

    // Stage in the same directory so the final move is an atomic rename rather
    // than a cross-filesystem copy that could be interrupted half-written.
    let staged = dir.join(format!("pp-agent-{version}.new"));
    tokio::fs::write(&staged, &bytes)
        .await
        .with_context(|| format!("writing {}", staged.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = tokio::fs::metadata(&staged).await?.permissions();
        perms.set_mode(0o755);
        tokio::fs::set_permissions(&staged, perms).await?;
    }

    let backup = PathBuf::from(format!("{}{OLD_SUFFIX}", exe.display()));
    let _ = tokio::fs::remove_file(&backup).await;

    // Move the running image aside first. On Windows this is the only legal
    // way to free the path; on Unix it also gives us something to roll back to.
    tokio::fs::rename(&exe, &backup)
        .await
        .with_context(|| format!("moving {} aside", exe.display()))?;

    if let Err(e) = tokio::fs::rename(&staged, &exe).await {
        // Put the working binary back rather than leaving the host with none.
        let _ = tokio::fs::rename(&backup, &exe).await;
        return Err(e).with_context(|| format!("installing {}", exe.display()));
    }

    Ok(format!(
        "installed agent {version}; restarting to pick it up"
    ))
}

/// Remove the displaced binary from a previous update. Called at startup,
/// once the new image is demonstrably running.
pub fn cleanup_old() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let backup = Path::new(&format!("{}{OLD_SUFFIX}", exe.display())).to_path_buf();
    if backup.exists() {
        match std::fs::remove_file(&backup) {
            Ok(()) => tracing::info!(path = %backup.display(), "removed previous agent binary"),
            Err(e) => tracing::debug!(error = %e, "previous agent binary still locked"),
        }
    }
}
