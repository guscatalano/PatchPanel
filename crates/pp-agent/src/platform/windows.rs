//! Windows backends: winget for applications, PSWindowsUpdate for OS patches,
//! and the uninstall registry keys for a complete installed-software list.
//!
//! Only winget is assumed present. The Windows Update path needs the
//! PSWindowsUpdate module, so it is detected rather than required — a machine
//! without it still reports full inventory, it just cannot apply OS patches.

use anyhow::{Context, Result};
use pp_proto::{AppSource, AvailableUpdate, Ensure, Package};
use serde::Deserialize;

use super::{AppOutcome, Backend, Platform};
use crate::exec::{self, Progress};

/// Run a PowerShell script with no profile and no prompts.
async fn ps(script: &str, p: &Progress) -> Result<exec::Output> {
    exec::run(
        "powershell.exe",
        &[
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ],
        p,
    )
    .await
}

/// Parse `ConvertTo-Json` output, which collapses a one-element collection
/// into a bare object instead of an array.
fn ps_json<T: for<'de> Deserialize<'de>>(text: &str) -> Result<Vec<T>> {
    let t = text.trim();
    if t.is_empty() || t == "null" {
        return Ok(Vec::new());
    }
    // Ignore any stderr lines the capture interleaved before the JSON body.
    let start = t.find(['[', '{']).unwrap_or(0);
    let t = &t[start..];
    if t.starts_with('[') {
        Ok(serde_json::from_str(t).context("parsing PowerShell JSON array")?)
    } else {
        Ok(vec![
            serde_json::from_str(t).context("parsing PowerShell JSON object")?,
        ])
    }
}

pub fn detect() -> Vec<Backend> {
    // Every Windows machine has the registry inventory path.
    let mut v = vec![Backend::Registry];
    if exec::have("winget.exe") {
        v.push(Backend::Winget);
    }
    if has_pswindowsupdate() {
        v.push(Backend::WindowsUpdate);
    }
    v
}

fn has_pswindowsupdate() -> bool {
    std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "if (Get-Module -ListAvailable -Name PSWindowsUpdate) { exit 0 } else { exit 1 }",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[derive(Deserialize)]
struct RegApp {
    #[serde(rename = "DisplayName")]
    name: Option<String>,
    #[serde(rename = "DisplayVersion")]
    version: Option<String>,
}

const REG_INVENTORY: &str = r#"
$keys = @(
  'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*',
  'HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*',
  'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*'
)
$apps = Get-ItemProperty -Path $keys -ErrorAction SilentlyContinue |
  Where-Object { $_.DisplayName -and -not $_.SystemComponent } |
  Select-Object DisplayName, DisplayVersion |
  Sort-Object DisplayName -Unique
@($apps) | ConvertTo-Json -Depth 2 -Compress
"#;

pub async fn installed_packages(_pf: &Platform, p: &Progress) -> Result<Vec<Package>> {
    let o = ps(REG_INVENTORY, p).await?.require(&[])?;
    let apps: Vec<RegApp> = ps_json(&o.text)?;
    Ok(apps
        .into_iter()
        .filter_map(|a| {
            let name = a.name?;
            Some(Package {
                name,
                version: a.version.unwrap_or_else(|| "unknown".into()),
                source: "registry".into(),
            })
        })
        .collect())
}

#[derive(Deserialize)]
struct WuUpdate {
    #[serde(rename = "KB")]
    kb: Option<String>,
    #[serde(rename = "Title")]
    title: Option<String>,
    #[serde(rename = "Severity")]
    severity: Option<String>,
}

const WU_LIST: &str = r#"
Import-Module PSWindowsUpdate -ErrorAction Stop
$u = Get-WindowsUpdate -MicrosoftUpdate -ErrorAction Stop
@($u | Select-Object @{n='KB';e={$_.KB}}, @{n='Title';e={$_.Title}}, @{n='Severity';e={$_.MsrcSeverity}}) |
  ConvertTo-Json -Depth 3 -Compress
"#;

pub async fn available_updates(pf: &Platform, p: &Progress) -> Result<Vec<AvailableUpdate>> {
    let mut out = Vec::new();

    if pf.has(Backend::Winget) {
        let o = exec::run(
            "winget.exe",
            &[
                "upgrade",
                "--include-unknown",
                "--disable-interactivity",
                "--accept-source-agreements",
            ],
            p,
        )
        .await?;
        // winget exits non-zero when nothing is upgradable; that is not a fault.
        out.extend(parse_winget_table(&o.text));
    }

    if pf.has(Backend::WindowsUpdate) {
        match ps(WU_LIST, p).await {
            Ok(o) if o.ok() => {
                let ups: Vec<WuUpdate> = ps_json(&o.text).unwrap_or_default();
                out.extend(ups.into_iter().map(|u| {
                    let sev = u.severity.unwrap_or_default();
                    AvailableUpdate {
                        name: u
                            .kb
                            .filter(|k| !k.is_empty())
                            .or_else(|| u.title.clone())
                            .unwrap_or_else(|| "unknown-update".into()),
                        current_version: String::new(),
                        new_version: u.title.unwrap_or_default(),
                        source: "windowsupdate".into(),
                        security: matches!(sev.as_str(), "Critical" | "Important" | "Moderate"),
                    }
                }));
            }
            Ok(o) => tracing::warn!(output = %exec::tail(&o.text, 400), "Get-WindowsUpdate failed"),
            Err(e) => tracing::warn!(error = %e, "Get-WindowsUpdate failed"),
        }
    }

    Ok(out)
}

/// winget prints a fixed-width table with no machine-readable alternative for
/// `upgrade`. Slicing by the header's column offsets survives values that
/// contain spaces, which splitting on whitespace does not.
fn parse_winget_table(text: &str) -> Vec<AvailableUpdate> {
    let lines: Vec<&str> = text.lines().collect();
    let Some(hdr_idx) = lines.iter().position(|l| {
        l.contains("Name") && l.contains("Id") && l.contains("Version") && l.contains("Available")
    }) else {
        return Vec::new();
    };

    let hdr: Vec<char> = lines[hdr_idx].chars().collect();
    let hdr_s: String = hdr.iter().collect();
    let col = |label: &str| -> Option<usize> {
        hdr_s[..].find(label).map(|b| hdr_s[..b].chars().count())
    };
    let (Some(c_name), Some(c_id), Some(c_ver), Some(c_avail)) = (
        col("Name"),
        col("Id"),
        col("Version"),
        col("Available"),
    ) else {
        return Vec::new();
    };
    let c_src = col("Source").unwrap_or(usize::MAX);

    let slice = |row: &[char], from: usize, to: usize| -> String {
        if from >= row.len() {
            return String::new();
        }
        let to = to.min(row.len());
        row[from..to].iter().collect::<String>().trim().to_string()
    };

    let mut out = Vec::new();
    for line in lines.iter().skip(hdr_idx + 1) {
        let trimmed = line.trim();
        // The rule under the header, blank lines, and the trailing
        // "N upgrades available." summary are not rows.
        if trimmed.is_empty()
            || trimmed.starts_with('-')
            || trimmed.chars().all(|c| c == '-' || c.is_whitespace())
            || trimmed.contains("upgrades available")
            || trimmed.contains("package(s) have")
        {
            continue;
        }
        let row: Vec<char> = line.chars().collect();
        let name = slice(&row, c_name, c_id);
        let id = slice(&row, c_id, c_ver);
        let current = slice(&row, c_ver, c_avail);
        let new = slice(&row, c_avail, c_src);
        if name.is_empty() || new.is_empty() {
            continue;
        }
        out.push(AvailableUpdate {
            // The winget id is what an operator needs to act on it.
            name: if id.is_empty() { name } else { id },
            current_version: current,
            new_version: new,
            source: "winget".into(),
            // winget carries no severity metadata.
            security: false,
        });
    }
    out
}

pub async fn apply_patches(
    pf: &Platform,
    security_only: bool,
    only: &[String],
    exclude: &[String],
    p: &Progress,
) -> Result<String> {
    let mut log = Vec::new();

    if pf.has(Backend::Winget) && !security_only {
        // winget has no notion of a security-only upgrade, so a security-only
        // run deliberately skips it and leaves apps alone.
        if only.is_empty() {
            let o = exec::run(
                "winget.exe",
                &[
                    "upgrade",
                    "--all",
                    "--silent",
                    "--disable-interactivity",
                    "--accept-package-agreements",
                    "--accept-source-agreements",
                ],
                p,
            )
            .await?;
            log.push(format!("winget upgrade --all (exit {})", o.code));
            log.push(exec::tail(&o.text, 3000));
        } else {
            for id in only.iter().filter(|i| !exclude.contains(i)) {
                let o = exec::run(
                    "winget.exe",
                    &[
                        "upgrade",
                        "--id",
                        id,
                        "--exact",
                        "--silent",
                        "--disable-interactivity",
                        "--accept-package-agreements",
                        "--accept-source-agreements",
                    ],
                    p,
                )
                .await?;
                log.push(format!("winget upgrade {id} (exit {})", o.code));
            }
        }
    }

    if pf.has(Backend::WindowsUpdate) {
        let category = if security_only {
            "-Category 'Security Updates','Critical Updates'"
        } else {
            ""
        };
        let script = format!(
            "Import-Module PSWindowsUpdate -ErrorAction Stop; \
             Install-WindowsUpdate -MicrosoftUpdate -AcceptAll -IgnoreReboot {category} -Verbose"
        );
        let o = ps(&script, p).await?.require(&[])?;
        log.push(exec::tail(&o.text, 4000));
    } else if security_only {
        anyhow::bail!(
            "security-only patching needs the PSWindowsUpdate module; \
             install it with `Install-Module PSWindowsUpdate -Force`"
        );
    }

    if log.is_empty() {
        anyhow::bail!("no Windows patch backend available");
    }
    Ok(log.join("\n"))
}

const REBOOT_CHECK: &str = r#"
$paths = @(
 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Component Based Servicing\RebootPending',
 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\WindowsUpdate\Auto Update\RebootRequired',
 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Component Based Servicing\RebootInProgress'
)
if ($paths | Where-Object { Test-Path $_ }) { Write-Output 'yes' } else { Write-Output 'no' }
"#;

pub async fn reboot_required() -> bool {
    match ps(REBOOT_CHECK, &Progress::detached()).await {
        Ok(o) => o.text.contains("yes"),
        Err(_) => false,
    }
}

pub async fn ensure_app(
    pf: &Platform,
    name: &str,
    version: Option<&str>,
    ensure: Ensure,
    source: &AppSource,
    p: &Progress,
) -> Result<AppOutcome> {
    let AppSource::Winget { id } = source else {
        anyhow::bail!("source `{}` is not usable on Windows", source.backend());
    };
    if !pf.has(Backend::Winget) {
        anyhow::bail!("app `{name}` needs winget, which is not installed");
    }

    let installed = winget_installed_version(id, p).await;

    let base = [
        "--exact",
        "--silent",
        "--disable-interactivity",
        "--accept-package-agreements",
        "--accept-source-agreements",
    ];

    match ensure {
        Ensure::Absent => {
            if installed.is_none() {
                return Ok(AppOutcome::unchanged(format!("`{name}` already absent"), None));
            }
            let mut args = vec!["uninstall", "--id", id];
            args.extend_from_slice(&base[..3]);
            exec::run("winget.exe", &args, p).await?.require(&[])?;
            Ok(AppOutcome::changed(format!("removed `{name}`"), None))
        }
        Ensure::Present => {
            if let Some(cur) = &installed {
                if version.is_none_or(|want| cur.starts_with(want)) {
                    return Ok(AppOutcome::unchanged(
                        format!("`{name}` present at {cur}"),
                        installed,
                    ));
                }
            }
            let mut args = vec!["install", "--id", id];
            args.extend_from_slice(&base);
            if let Some(v) = version {
                args.push("--version");
                args.push(v);
            }
            exec::run("winget.exe", &args, p).await?.require(&[])?;
            let now = winget_installed_version(id, p).await;
            Ok(AppOutcome::changed(format!("installed `{name}`"), now))
        }
        Ensure::Latest => {
            let verb = if installed.is_some() { "upgrade" } else { "install" };
            let mut args = vec![verb, "--id", id];
            args.extend_from_slice(&base);
            let o = exec::run("winget.exe", &args, p).await?;
            // 0x8A15002B / "No applicable upgrade found" surfaces as a non-zero
            // exit even though the machine is already in the desired state.
            let already_current = o.text.contains("No applicable")
                || o.text.contains("No installed package found")
                || o.text.contains("no newer");
            if !o.ok() && !already_current {
                anyhow::bail!("winget {verb} `{id}` exited {}: {}", o.code, exec::tail(&o.text, 600));
            }
            let now = winget_installed_version(id, p).await;
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
        AppSource::Winget { id } if pf.has(Backend::Winget) => {
            winget_installed_version(id, p).await
        }
        _ => None,
    }
}

async fn winget_installed_version(id: &str, p: &Progress) -> Option<String> {
    let o = exec::run(
        "winget.exe",
        &[
            "list",
            "--id",
            id,
            "--exact",
            "--disable-interactivity",
            "--accept-source-agreements",
        ],
        p,
    )
    .await
    .ok()?;
    if !o.ok() {
        return None;
    }
    // Reuse the table reader: `list` shares the header shape with `upgrade`
    // minus the Available column, so read the Version column directly.
    let lines: Vec<&str> = o.text.lines().collect();
    let hdr_idx = lines
        .iter()
        .position(|l| l.contains("Name") && l.contains("Id") && l.contains("Version"))?;
    let hdr = lines[hdr_idx];
    let c_ver = hdr[..hdr.find("Version")?].chars().count();
    let c_end = hdr
        .find("Available")
        .or_else(|| hdr.find("Source"))
        .map(|b| hdr[..b].chars().count())
        .unwrap_or(usize::MAX);

    for line in lines.iter().skip(hdr_idx + 1) {
        let t = line.trim();
        if t.is_empty() || t.chars().all(|c| c == '-' || c.is_whitespace()) {
            continue;
        }
        let row: Vec<char> = line.chars().collect();
        if c_ver >= row.len() {
            continue;
        }
        let v: String = row[c_ver..c_end.min(row.len())].iter().collect();
        let v = v.trim().to_string();
        if !v.is_empty() {
            return Some(v);
        }
    }
    None
}
