//! Per-OS package management, behind one interface.
//!
//! The session loop never learns which OS it is on: it asks the `Platform` for
//! inventory or for convergence and the right backend is picked here. Adding a
//! new OS means adding a module and three match arms, not touching the loop.

use anyhow::{Context, Result};
use pp_proto::{AppSource, AvailableUpdate, Cleanup, Ensure, Package, ScanIssue};

use crate::exec::{self, Progress};

// Both backends are compiled on every host, not just their own.
//
// They shell out to package managers and touch only cross-platform std APIs,
// so this costs nothing but catches the whole class of error where a change to
// the Linux backend cannot even be type-checked from a Windows dev machine -
// which is exactly how a literal tab inside a char literal reached a release.
// Dispatch below is still cfg'd; only the compilation is unconditional.
// Visible to the crate, not just to this module: the session layer reaches in
// for the few things that are genuinely Linux-shaped, such as writing an
// rsyslog rule. Private, those references only fail to compile on Linux, which
// a Windows host build never notices.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) mod linux;
#[cfg_attr(not(windows), allow(dead_code))]
mod windows;

/// A package manager this machine can actually drive, decided once at startup.
///
/// Every variant exists on every build so the type is one shared vocabulary,
/// but only the current OS's module ever constructs its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code, reason = "Linux backends"))]
#[cfg_attr(not(windows), allow(dead_code, reason = "Windows backends"))]
pub enum Backend {
    Apt,
    Dnf,
    /// Firmware, through fwupd and LVFS. Reported, never installed by a patch
    /// run: a package can be rolled back and firmware cannot.
    Fwupd,
    Winget,
    /// Windows Update, driven through the PSWindowsUpdate PowerShell module.
    WindowsUpdate,
    /// Installed-software inventory from the Windows uninstall registry keys.
    Registry,
}

impl Backend {
    pub fn name(self) -> &'static str {
        match self {
            Backend::Apt => "apt",
            Backend::Dnf => "dnf",
            Backend::Fwupd => "fwupd",
            Backend::Winget => "winget",
            Backend::WindowsUpdate => "windowsupdate",
            Backend::Registry => "registry",
        }
    }
}

pub struct Platform {
    backends: Vec<Backend>,
}

/// What happened when an app was reconciled against its spec.
pub struct AppOutcome {
    pub changed: bool,
    pub note: String,
    /// Version observed after the action, when the backend reports one.
    pub observed: Option<String>,
}

impl AppOutcome {
    fn unchanged(note: impl Into<String>, observed: Option<String>) -> Self {
        AppOutcome { changed: false, note: note.into(), observed }
    }
    fn changed(note: impl Into<String>, observed: Option<String>) -> Self {
        AppOutcome { changed: true, note: note.into(), observed }
    }

    /// One line for the command log, with the resulting version when the
    /// backend told us one.
    pub fn describe(&self) -> String {
        match &self.observed {
            Some(v) if !self.note.contains(v.as_str()) => format!("{} [{v}]", self.note),
            _ => self.note.clone(),
        }
    }
}

impl Platform {
    pub fn detect() -> Self {
        #[cfg(target_os = "linux")]
        let backends = linux::detect();
        #[cfg(windows)]
        let backends = windows::detect();
        #[cfg(not(any(target_os = "linux", windows)))]
        let backends: Vec<Backend> = Vec::new();

        tracing::info!(
            backends = ?backends.iter().map(|b| b.name()).collect::<Vec<_>>(),
            "detected package backends"
        );
        Platform { backends }
    }

    pub fn backend_names(&self) -> Vec<String> {
        self.backends.iter().map(|b| b.name().to_string()).collect()
    }

    pub fn has(&self, b: Backend) -> bool {
        self.backends.contains(&b)
    }

    /// Distro or Windows edition string for the dashboard.
    pub fn os_version() -> String {
        // On Linux, /etc/os-release is the machine's own answer and os_info is
        // a guess at it. The guess is "Debian n/a" for a testing install,
        // which reads as a broken field rather than as what it is - and a
        // machine silently sitting on testing is worth naming plainly.
        #[cfg(target_os = "linux")]
        if let Ok(text) = std::fs::read_to_string("/etc/os-release") {
            if let Some(v) = describe_os_release(&text) {
                return v;
            }
        }

        let info = os_info::get();
        let ver = info.version().to_string();
        // os_info spells an unknown version both ways depending on backend.
        if ver == "Unknown" || ver == "n/a" {
            info.os_type().to_string()
        } else {
            format!("{} {}", info.os_type(), ver)
        }
    }

    pub async fn installed_packages(&self, p: &Progress) -> Result<Vec<Package>> {
        let _ = p;
        #[cfg(target_os = "linux")]
        return linux::installed_packages(self, p).await;
        #[cfg(windows)]
        return windows::installed_packages(self, p).await;
        #[cfg(not(any(target_os = "linux", windows)))]
        return Ok(Vec::new());
    }

    /// Pending updates, plus anything that stopped a backend from answering.
    ///
    /// The two travel together on purpose: a caller that takes the list without
    /// the problems will report "no updates" for a machine whose package
    /// manager is broken, which is the single most dangerous thing this tool
    /// can say.
    pub async fn available_updates(
        &self,
        p: &Progress,
    ) -> Result<(Vec<AvailableUpdate>, Vec<ScanIssue>)> {
        let _ = p;
        #[cfg(target_os = "linux")]
        return linux::available_updates(self, p).await;
        #[cfg(windows)]
        return windows::available_updates(self, p).await;
        #[cfg(not(any(target_os = "linux", windows)))]
        return Ok((Vec::new(), Vec::new()));
    }

    /// Install what this machine needs in order to be scannable.
    pub async fn install_prerequisites(&self, p: &Progress) -> Result<String> {
        #[cfg(target_os = "linux")]
        return linux::install_prerequisites(self, p).await;
        #[allow(unreachable_code)]
        let _ = p;
        #[cfg(windows)]
        return windows::install_prerequisites(p).await;
        #[cfg(not(windows))]
        Ok("nothing to install: this platform's package tooling is part of the OS".into())
    }

    /// Install pending OS updates. `only` narrows to named packages; `exclude`
    /// is the policy's never-touch list and always wins.
    pub async fn apply_patches(
        &self,
        security_only: bool,
        only: &[String],
        exclude: &[String],
        full: bool,
        p: &Progress,
    ) -> Result<String> {
        let _ = (security_only, only, exclude, full, p);
        #[cfg(target_os = "linux")]
        return linux::apply_patches(self, security_only, only, exclude, full, p).await;
        #[cfg(windows)]
        return windows::apply_patches(self, security_only, only, exclude, p).await;
        #[cfg(not(any(target_os = "linux", windows)))]
        anyhow::bail!("no patch backend on this platform");
    }

    /// Backends this machine cannot scan, and why.
    ///
    /// Reported so a zero update count can be shown as "unknown" rather than
    /// "clean" - the difference between a patched machine and a blind one.
    pub fn scan_issues(&self) -> Vec<ScanIssue> {
        #[cfg(target_os = "linux")]
        return linux::scan_issues(self);
        #[cfg(windows)]
        return windows::scan_issues(self);
        #[cfg(not(any(target_os = "linux", windows)))]
        return Vec::new();
    }

    /// Upgrades apt is refusing to perform with a plain `upgrade`.
    pub async fn held_back(&self, p: &Progress) -> Vec<String> {
        let _ = p;
        #[cfg(target_os = "linux")]
        return linux::held_back(self, p).await;
        #[cfg(not(target_os = "linux"))]
        return Vec::new();
    }

    /// Upgrades that even a full upgrade will not apply.
    pub async fn deferred(&self, p: &Progress) -> Vec<String> {
        let _ = p;
        #[cfg(target_os = "linux")]
        return linux::deferred(self, p).await;
        #[cfg(not(target_os = "linux"))]
        return Vec::new();
    }

    /// What could be freed, without freeing it.
    pub async fn cleanup_preview(&self, p: &Progress) -> Cleanup {
        let _ = p;
        #[cfg(target_os = "linux")]
        return linux::cleanup_preview(self, p).await;
        #[cfg(not(target_os = "linux"))]
        return Cleanup::default();
    }

    /// Firmware updates, everything fwupd can see, and why it could not look.
    pub async fn firmware(
        &self,
        p: &Progress,
    ) -> (
        Vec<pp_proto::FirmwareUpdate>,
        Vec<pp_proto::FirmwareDevice>,
        Option<ScanIssue>,
    ) {
        let _ = p;
        #[cfg(target_os = "linux")]
        return linux::firmware(self, p).await;
        #[cfg(not(target_os = "linux"))]
        return (Vec::new(), Vec::new(), None);
    }

    /// Flash firmware. Only ever reached by an explicit request.
    pub async fn update_firmware(&self, only: &[String], p: &Progress) -> Result<String> {
        let _ = (only, p);
        #[cfg(target_os = "linux")]
        return linux::update_firmware(self, only, p).await;
        #[cfg(not(target_os = "linux"))]
        anyhow::bail!(
            "PatchPanel does not drive firmware on this platform. On Windows, firmware \
             arrives through Windows Update."
        );
    }

    /// Whether this machine hosts virtual machines, or is one.
    pub async fn virtualization(&self, p: &Progress) -> Option<pp_proto::Virtualization> {
        let _ = p;
        #[cfg(target_os = "linux")]
        return linux::virtualization(p).await;
        #[cfg(windows)]
        return windows::virtualization(p).await;
        #[cfg(not(any(target_os = "linux", windows)))]
        return None;
    }

    /// Why this machine last restarted, when its logs can say.
    pub async fn boot_report(&self, p: &Progress) -> Option<pp_proto::BootReport> {
        let _ = p;
        #[cfg(target_os = "linux")]
        return linux::boot_report(p).await;
        #[cfg(windows)]
        return windows::boot_report(p).await;
        #[cfg(not(any(target_os = "linux", windows)))]
        return None;
    }

    /// Is dpkg stuck part-way through an upgrade?
    pub async fn mid_upgrade(&self, p: &Progress) -> Option<pp_proto::MidUpgrade> {
        let _ = p;
        #[cfg(target_os = "linux")]
        return linux::mid_upgrade(self, p).await;
        #[cfg(not(target_os = "linux"))]
        return None;
    }

    /// Configure what is half-installed and carry on.
    pub async fn finish_upgrade(&self, grub_device: Option<&str>, p: &Progress) -> Result<String> {
        let _ = (grub_device, p);
        #[cfg(target_os = "linux")]
        return linux::finish_upgrade(self, grub_device, p).await;
        #[cfg(not(target_os = "linux"))]
        anyhow::bail!("there is no interrupted upgrade to finish on this platform");
    }

    /// Which configured repositories cannot actually deliver packages.
    ///
    /// Returns `(repo label, problem)` pairs.
    pub async fn repo_problems(
        &self,
        repos: &[pp_proto::Repository],
        p: &Progress,
    ) -> Vec<(String, String)> {
        let _ = (repos, p);
        #[cfg(target_os = "linux")]
        return linux::repo_problems(self, repos, p).await;
        #[cfg(not(target_os = "linux"))]
        return Vec::new();
    }

    /// Move to the next distribution release, or just report whether it is
    /// safe to.
    pub async fn distro_upgrade(&self, to: &str, check: bool, p: &Progress) -> Result<String> {
        let _ = (to, check, p);
        #[cfg(target_os = "linux")]
        return linux::distro_upgrade(self, to, check, p).await;
        #[cfg(not(target_os = "linux"))]
        anyhow::bail!("release upgrades are only supported on Debian for now");
    }

    /// Remove orphaned packages and empty the package cache.
    pub async fn cleanup(&self, purge: bool, p: &Progress) -> Result<String> {
        let _ = (purge, p);
        #[cfg(target_os = "linux")]
        return linux::cleanup(self, purge, p).await;
        #[cfg(not(target_os = "linux"))]
        anyhow::bail!("no package cleanup is available on this platform");
    }

    pub async fn reboot_required(&self) -> bool {
        #[cfg(target_os = "linux")]
        return linux::reboot_required().await;
        #[cfg(windows)]
        return windows::reboot_required().await;
        #[cfg(not(any(target_os = "linux", windows)))]
        return false;
    }

    pub async fn reboot(&self, delay_secs: u64, p: &Progress) -> Result<()> {
        let mins = delay_secs.div_ceil(60).to_string();
        if cfg!(windows) {
            exec::run("shutdown.exe", &["/r", "/t", &delay_secs.to_string(), "/c", "PatchPanel"], p)
                .await?
                .require(&[])?;
        } else {
            exec::run("shutdown", &["-r", &format!("+{mins}"), "PatchPanel"], p)
                .await?
                .require(&[])?;
        }
        Ok(())
    }

    /// Version of `source` currently installed, if the backend can tell us.
    /// Used to report drift without changing anything.
    pub async fn observed_version(&self, source: &AppSource, p: &Progress) -> Option<String> {
        let _ = (source, p);
        #[cfg(target_os = "linux")]
        return linux::observed_version(self, source, p).await;
        #[cfg(windows)]
        return windows::observed_version(self, source, p).await;
        #[cfg(not(any(target_os = "linux", windows)))]
        return None;
    }

    /// Bring one app in line with its spec. Backend-specific work is delegated;
    /// the `Url` source is handled here because it is identical on every OS.
    pub async fn ensure_app(
        &self,
        name: &str,
        version: Option<&str>,
        ensure: Ensure,
        source: &AppSource,
        p: &Progress,
    ) -> Result<AppOutcome> {
        if let AppSource::Url { url, sha256, install_cmd } = source {
            if ensure == Ensure::Absent {
                anyhow::bail!("`absent` is not supported for url-sourced app `{name}`");
            }
            return ensure_from_url(name, url, sha256, install_cmd, p).await;
        }

        let _ = (name, version, ensure, source, p);
        #[cfg(target_os = "linux")]
        return linux::ensure_app(self, name, version, ensure, source, p).await;
        #[cfg(windows)]
        return windows::ensure_app(self, name, version, ensure, source, p).await;
        #[cfg(not(any(target_os = "linux", windows)))]
        anyhow::bail!("no app backend on this platform");
    }
}

/// Download, verify, and run an installer. Verification is not optional: an
/// unverified binary fetched over the network and run as root is the worst
/// thing this agent could possibly do.
async fn ensure_from_url(
    name: &str,
    url: &str,
    sha256: &str,
    install_cmd: &str,
    p: &Progress,
) -> Result<AppOutcome> {
    use sha2::{Digest, Sha256};

    p.line(&format!("fetching {url}"));
    let bytes = reqwest::Client::new()
        .get(url)
        .send()
        .await
        .with_context(|| format!("fetching installer for `{name}`"))?
        .error_for_status()?
        .bytes()
        .await?;

    let got = hex::encode(Sha256::digest(&bytes));
    if !got.eq_ignore_ascii_case(sha256.trim()) {
        anyhow::bail!("checksum mismatch for `{name}`: expected {sha256}, got {got}");
    }
    p.line(&format!("sha256 ok ({} bytes)", bytes.len()));

    let ext = url.rsplit('.').next().filter(|e| e.len() <= 4).unwrap_or("bin");
    let path = std::env::temp_dir().join(format!("patchpanel-{name}.{ext}"));
    tokio::fs::write(&path, &bytes).await?;

    let cmd = install_cmd.replace("{}", &path.to_string_lossy());
    let out = if cfg!(windows) {
        exec::run("powershell.exe", &["-NoProfile", "-NonInteractive", "-Command", &cmd], p).await?
    } else {
        exec::run("sh", &["-c", &cmd], p).await?
    };
    let _ = tokio::fs::remove_file(&path).await;
    out.require(&[])?;

    Ok(AppOutcome::changed(format!("installed `{name}` from {url}"), None))
}

/// Turn /etc/os-release into something worth showing in a table.
///
/// A released Debian gives `Debian 12 (bookworm)`. A testing install has no
/// VERSION_ID at all, which is the case worth being explicit about: it is a
/// different support model, and "Debian n/a" tells nobody that.
fn describe_os_release(text: &str) -> Option<String> {
    let field = |key: &str| -> Option<String> {
        text.lines()
            .filter_map(|l| l.split_once('='))
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.trim().trim_matches(['"', '\'']).to_string())
            .filter(|v| !v.is_empty())
    };

    // NAME is "Debian GNU/Linux"; the shorter half is enough for a column.
    let name = field("NAME")
        .map(|n| n.split_whitespace().next().unwrap_or(&n).to_string())
        .or_else(|| field("ID"))?;
    let codename = field("VERSION_CODENAME");

    Some(match (field("VERSION_ID"), codename) {
        (Some(v), Some(c)) => format!("{name} {v} ({c})"),
        (Some(v), None) => format!("{name} {v}"),
        // No version number: a rolling suite. sid is unstable by definition;
        // anything else with a codename and no version is testing.
        (None, Some(c)) if c == "sid" => format!("{name} unstable (sid)"),
        (None, Some(c)) => format!("{name} testing ({c})"),
        (None, None) => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_a_released_debian_by_its_number() {
        let released = "PRETTY_NAME=\"Debian GNU/Linux 12 (bookworm)\"\n\
                        NAME=\"Debian GNU/Linux\"\n\
                        VERSION_ID=\"12\"\n\
                        VERSION_CODENAME=bookworm\n\
                        ID=debian\n";
        assert_eq!(describe_os_release(released).unwrap(), "Debian 12 (bookworm)");
    }

    /// A machine upgraded one release too far. os_info renders this as
    /// "Debian n/a", which looks like a bug in the dashboard rather than a
    /// fact about the machine.
    #[test]
    fn says_testing_when_there_is_no_version() {
        let testing = "PRETTY_NAME=\"Debian GNU/Linux forky/sid\"\n\
                       NAME=\"Debian GNU/Linux\"\n\
                       VERSION_CODENAME=forky\n\
                       ID=debian\n";
        assert_eq!(describe_os_release(testing).unwrap(), "Debian testing (forky)");
    }

    #[test]
    fn sid_is_unstable_not_testing() {
        let sid = "NAME=\"Debian GNU/Linux\"\nVERSION_CODENAME=sid\nID=debian\n";
        assert_eq!(describe_os_release(sid).unwrap(), "Debian unstable (sid)");
    }

    #[test]
    fn ubuntu_keeps_its_number() {
        let ubuntu = "NAME=\"Ubuntu\"\nVERSION_ID=\"24.04\"\nVERSION_CODENAME=noble\nID=ubuntu\n";
        assert_eq!(describe_os_release(ubuntu).unwrap(), "Ubuntu 24.04 (noble)");
    }

    #[test]
    fn nothing_useful_falls_back_to_os_info() {
        assert!(describe_os_release("").is_none());
        assert!(describe_os_release("SOMETHING=else\n").is_none());
    }
}
