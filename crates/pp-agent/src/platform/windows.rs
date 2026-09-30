//! Windows backends: winget for applications, PSWindowsUpdate for OS patches,
//! and the uninstall registry keys for a complete installed-software list.
//!
//! Only winget is assumed present. The Windows Update path needs the
//! PSWindowsUpdate module, so it is detected rather than required — a machine
//! without it still reports full inventory, it just cannot apply OS patches.

use anyhow::{Context, Result};
use pp_proto::{
    AppSource, AvailableUpdate, BootReport, Ensure, Guest, Package, ScanIssue, Virtualization,
};
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

/// Locate winget.
///
/// It ships as a per-user MSIX (App Installer), so a service running as
/// LocalSystem does not get it on PATH and `where winget.exe` fails. That made
/// every service-mode Windows agent silently report zero app updates. Fall back
/// to the versioned WindowsApps directory where the package actually lives.
pub fn winget_path() -> Option<String> {
    if exec::have("winget.exe") {
        return Some("winget.exe".to_string());
    }
    let base = std::env::var("ProgramFiles").ok()?;
    let dir = std::path::Path::new(&base).join("WindowsApps");
    let mut found: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("Microsoft.DesktopAppInstaller_")
        })
        .map(|e| e.path().join("winget.exe"))
        .filter(|p| p.exists())
        .collect();
    // Several versions can be installed side by side; the newest sorts last.
    found.sort();
    found.pop().map(|p| p.to_string_lossy().into_owned())
}

pub fn scan_issues(pf: &Platform) -> Vec<ScanIssue> {
    let mut out = Vec::new();
    if !pf.has(Backend::Winget) {
        out.push(ScanIssue {
            backend: "winget".into(),
            problem: "winget was not found, so application updates are not being scanned".into(),
            remedy: "Install the App Installer package for all users, or run the agent as a \
                     user that has winget. Note winget ships per-user, so a LocalSystem \
                     service often cannot see it."
                .into(),
        });
    }
    if !pf.has(Backend::WindowsUpdate) {
        out.push(ScanIssue {
            backend: "windowsupdate".into(),
            problem: "the PSWindowsUpdate module is missing, so operating system updates are \
                      not being scanned"
                .into(),
            remedy: "Install-Module PSWindowsUpdate -Force -Scope AllUsers".into(),
        });
    }
    out
}

pub fn detect() -> Vec<Backend> {
    // Every Windows machine has the registry inventory path.
    let mut v = vec![Backend::Registry];
    if winget_path().is_some() {
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

/// Bring a machine up to the point where it can actually be scanned.
///
/// Both of these are missing by default on a fresh Windows box, and a service
/// running as LocalSystem cannot see a per-user winget at all - which is why a
/// Windows agent so often reports a confident and completely wrong zero.
pub async fn install_prerequisites(p: &Progress) -> Result<String> {
    let mut log = Vec::new();

    p.line("installing the NuGet provider and trusting PSGallery");
    let bootstrap = concat!(
        "[Net.ServicePointManager]::SecurityProtocol = ",
        "[Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12; ",
        "Install-PackageProvider -Name NuGet -MinimumVersion 2.8.5.201 -Force ",
        "-ErrorAction SilentlyContinue | Out-Null; ",
        "Set-PSRepository -Name PSGallery -InstallationPolicy Trusted ",
        "-ErrorAction SilentlyContinue; 'ok'"
    );
    let o = ps(bootstrap, p).await?;
    log.push(format!("bootstrap: {}", exec::tail(o.text.trim(), 200)));

    p.line("installing PSWindowsUpdate");
    let o = ps(
        "Install-Module PSWindowsUpdate -Force -Scope AllUsers -AllowClobber -ErrorAction Stop; 'installed'",
        p,
    )
    .await?;
    if !o.ok() {
        log.push(format!("PSWindowsUpdate failed: {}", exec::tail(&o.text, 400)));
    }

    // winget is the awkward one. Repair-WinGetPackageManager is the supported
    // route but it only runs under PowerShell 7 - under Windows PowerShell it
    // fails with WindowsPowerShellNotSupported. Try it only where pwsh exists,
    // and otherwise provision the App Installer bundle directly, which works
    // from 5.1 and from a SYSTEM service.
    if winget_path().is_none() {
        if exec::have("pwsh.exe") {
            p.line("repairing winget with PowerShell 7");
            let o = exec::run(
                "pwsh.exe",
                &[
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "Install-Module Microsoft.WinGet.Client -Force -Scope AllUsers -AllowClobber;                      Import-Module Microsoft.WinGet.Client;                      Repair-WinGetPackageManager -AllUsers -Force; 'done'",
                ],
                p,
            )
            .await?;
            log.push(format!("winget via pwsh: exit {}", o.code));
        } else {
            p.line("provisioning the App Installer bundle (no PowerShell 7 present)");
            // Forward slashes are valid in PowerShell paths and avoid a pile of
            // escaping for no benefit.
            let provision = concat!(
                "$ErrorActionPreference='Stop'; $ProgressPreference='SilentlyContinue'; ",
                "[Net.ServicePointManager]::SecurityProtocol = ",
                "[Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12; ",
                "$d = Join-Path $env:TEMP 'pp-winget'; ",
                "New-Item -ItemType Directory -Force $d | Out-Null; ",
                "$b = \"$d/winget.msixbundle\"; $v = \"$d/vclibs.appx\"; ",
                "Invoke-WebRequest -UseBasicParsing 'https://aka.ms/getwinget' -OutFile $b; ",
                "Invoke-WebRequest -UseBasicParsing ",
                "'https://aka.ms/Microsoft.VCLibs.x64.14.00.Desktop.appx' -OutFile $v; ",
                "Add-AppxProvisionedPackage -Online -PackagePath $b ",
                "-DependencyPackagePath $v -SkipLicense | Out-Null; 'provisioned'"
            );
            let o = ps(provision, p).await?;
            if !o.ok() {
                log.push(format!(
                    "winget provisioning failed: {}",
                    exec::tail(&o.text, 500)
                ));
            }
        }
    }

    // Report what is actually true now, not what the commands claimed. The
    // previous version printed "winget repaired" even when the cmdlet had
    // refused to run, which is worse than not trying.
    let winget_ok = winget_path().is_some();
    let wu_ok = has_pswindowsupdate();
    log.push(format!(
        "result: PSWindowsUpdate {}, winget {}",
        if wu_ok { "available" } else { "STILL MISSING" },
        if winget_ok { "available" } else { "STILL MISSING" }
    ));

    if wu_ok && winget_ok {
        log.push("restart the agent service so the new backends are detected".into());
        Ok(log.join("\n"))
    } else {
        // A partial result is a failure: the machine still cannot be fully
        // scanned, and saying otherwise recreates the false-zero problem.
        anyhow::bail!(
            "{}\n\nThis machine still cannot be fully scanned. Installing PowerShell 7 \
             (winget's supported tooling) and re-running usually resolves the winget half.",
            log.join("\n")
        )
    }
}

/// Does this machine host virtual machines, or is it one?
///
/// The same question as on Linux, asked the Windows way: Hyper-V exposes its
/// guests through a PowerShell module that only exists when the role is
/// installed, and the firmware tells any machine whether it is itself running
/// under a hypervisor.
pub async fn virtualization(p: &Progress) -> Option<Virtualization> {
    let script = concat!(
        "$out = [ordered]@{ guest = $false; model = ''; guests = @() }; ",
        "$cs = Get-CimInstance Win32_ComputerSystem -ErrorAction SilentlyContinue; ",
        "if ($cs) { $out.guest = [bool]$cs.HypervisorPresent; $out.model = \"$($cs.Manufacturer) $($cs.Model)\" } ",
        "if (Get-Command Get-VM -ErrorAction SilentlyContinue) { ",
        "  $out.guests = @(Get-VM -ErrorAction SilentlyContinue | ",
        "    Select-Object @{n='id';e={$_.Id.ToString()}}, @{n='name';e={$_.Name}}, ",
        "                  @{n='state';e={$_.State.ToString()}}) } ",
        "$out | ConvertTo-Json -Depth 4 -Compress"
    );
    let o = ps(script, p).await.ok()?;
    if !o.ok() {
        return None;
    }

    #[derive(serde::Deserialize)]
    struct WinGuest {
        id: String,
        name: String,
        state: String,
    }
    #[derive(serde::Deserialize)]
    struct Report {
        guest: bool,
        #[serde(default)]
        model: String,
        #[serde(default)]
        guests: Vec<WinGuest>,
    }

    // ps_json always hands back a list, even for a single object.
    let r: Report = ps_json(&o.text).ok()?.into_iter().next()?;
    let host = !r.guests.is_empty();

    // HypervisorPresent is true on a Hyper-V host as well as inside a VM, so
    // it only means "guest" when this machine is not the one doing the
    // hosting. Saying a hypervisor is a guest of itself helps nobody.
    let role = match (host, r.guest) {
        (true, _) => "host",
        (false, true) => "guest",
        _ => return None,
    };

    Some(Virtualization {
        role: role.to_string(),
        platform: if host {
            "hyper-v".to_string()
        } else if r.model.trim().is_empty() {
            "a hypervisor".to_string()
        } else {
            r.model.trim().to_string()
        },
        guests: r
            .guests
            .into_iter()
            .map(|g| Guest {
                id: g.id,
                name: g.name,
                kind: "hyper-v".into(),
                state: g.state,
                managed: false,
                last_backup: None,
                // Portal-derived; an agent only ever reports what it observed.
                backup_unverified: false,
            })
            .collect(),
        note: String::new(),
        // Hyper-V's own backup story is not one PatchPanel reads.
        backups: None,
    })
}

/// Why this machine last restarted, from the System event log.
///
/// Windows is unusually good about this: it records an explicit "the previous
/// shutdown was unexpected" event, and a bugcheck event carrying the stop code
/// when a crash was the cause. The events outlive the reboot, which is exactly
/// what makes them worth reading.
pub async fn boot_report(p: &Progress) -> Option<BootReport> {
    // 1074 planned shutdown, 6008 previous shutdown was unexpected,
    // 41 kernel power (lost power or bugchecked), 1001 bugcheck details.
    let script = concat!(
        "$ids = 1074,6008,41,1001; ",
        "Get-WinEvent -FilterHashtable @{LogName='System'; Id=$ids} -MaxEvents 12 ",
        "-ErrorAction SilentlyContinue | ",
        "Select-Object Id, TimeCreated, ",
        "@{n='Message';e={($_.Message -split \"`r`n\")[0]}} | ConvertTo-Json -Compress"
    );
    let o = ps(script, p).await.ok()?;
    if !o.ok() {
        return None;
    }

    #[derive(serde::Deserialize)]
    struct Ev {
        #[serde(rename = "Id")]
        id: u32,
        #[serde(rename = "TimeCreated")]
        time: Option<String>,
        #[serde(rename = "Message")]
        message: Option<String>,
    }

    let events: Vec<Ev> = ps_json(&o.text).unwrap_or_default();
    if events.is_empty() {
        return None;
    }

    // Newest first, which is how Get-WinEvent returns them.
    let newest = events.first()?;
    let line = |e: &Ev| {
        format!(
            "{}  event {}  {}",
            e.time.clone().unwrap_or_default(),
            e.id,
            e.message.clone().unwrap_or_default()
        )
    };
    let detail = events.iter().take(6).map(line).collect::<Vec<_>>().join("\n");

    let bugcheck = events.iter().find(|e| e.id == 1001);
    let unexpected = matches!(newest.id, 6008 | 41 | 1001);

    Some(BootReport {
        unexpected,
        summary: match newest.id {
            1001 => "bugcheck (blue screen)".into(),
            6008 => "previous shutdown was unexpected".into(),
            41 => "lost power or stopped responding".into(),
            _ => "clean shutdown".into(),
        },
        detail: if let Some(b) = bugcheck {
            format!("{}\n\n{}", line(b), detail)
        } else {
            detail
        },
    })
}

pub async fn available_updates(
    pf: &Platform,
    p: &Progress,
) -> Result<(Vec<AvailableUpdate>, Vec<ScanIssue>)> {
    let mut out = Vec::new();
    let mut issues = Vec::new();

    if pf.has(Backend::Winget) {
        let (found, text) = winget_scan(p).await;
        out.extend(found);
        issues.extend(winget_blocked(&text));
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

    Ok((out, issues))
}

/// Ask winget what it would upgrade, returning the parsed rows along with the
/// raw output: the footers underneath the table say things the table does not.
async fn winget_scan(p: &Progress) -> (Vec<AvailableUpdate>, String) {
    let Ok(o) = exec::run(
        &winget_path().unwrap_or_else(|| "winget.exe".into()),
        &[
            "upgrade",
            "--include-unknown",
            "--disable-interactivity",
            "--accept-source-agreements",
        ],
        p,
    )
    .await
    else {
        return (Vec::new(), String::new());
    };
    // winget exits non-zero when nothing is upgradable; that is not a fault.
    (parse_winget_table(&o.text), o.text)
}

async fn winget_pending(p: &Progress) -> Vec<AvailableUpdate> {
    winget_scan(p).await.0
}

/// winget will not replace a package whose new version ships as a different
/// kind of installer - an MSI superseded by an MSIX, most often - because
/// doing so means uninstalling first, which it refuses to decide on its own.
///
/// It says so only in a footer, and the package stays in the upgrade table
/// forever. Left alone it is a pending update that no amount of pressing
/// Install updates will ever clear, so name the condition and say what
/// actually resolves it.
fn winget_blocked(text: &str) -> Vec<ScanIssue> {
    let Some(line) = text
        .lines()
        .map(str::trim)
        .find(|l| l.contains("different install technology"))
    else {
        return Vec::new();
    };

    let n = line
        .split_whitespace()
        .next()
        .and_then(|w| w.parse::<u32>().ok())
        .unwrap_or(1);

    vec![ScanIssue {
        backend: "winget".into(),
        problem: format!(
            "{n} application(s) cannot be upgraded in place: the newer version ships as a \
             different kind of installer (an MSI replaced by an MSIX, usually)"
        ),
        remedy: format!(
            "These stay in the pending list no matter how many times updates are installed, \
             because winget will not uninstall an application to upgrade it.\n\n\
             winget said:\n  {line}\n\n\
             To clear it, on this machine run:\n  \
             winget upgrade --include-unknown\n\
             then, for the application that will not move:\n  \
             winget uninstall --id <Id>\n  \
             winget install --id <Id>\n\n\
             Uninstalling removes that application's settings in some cases, which is why \
             PatchPanel will not do it for you."
        ),
    }]
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
                &winget_path().unwrap_or_else(|| "winget.exe".into()),
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

            // winget names the packages it is about to upgrade, and separately
            // reports a count of the ones it refused without saying which.
            // Asking again afterwards settles it: whatever is still offered at
            // the same version did not move, whatever the exit code claimed.
            let attempted = parse_winget_table(&o.text);
            let remaining = winget_pending(p).await;
            let stuck: Vec<&AvailableUpdate> = attempted
                .iter()
                .filter(|a| {
                    remaining
                        .iter()
                        .any(|r| r.name == a.name && r.new_version == a.new_version)
                })
                .collect();

            // The footer explaining why is otherwise the last line of a wall
            // of output, where nobody looking for a reason will find it.
            for issue in winget_blocked(&o.text) {
                log.push(format!("\n{}\n{}", issue.problem, issue.remedy));
            }
            if !stuck.is_empty() {
                let names = stuck
                    .iter()
                    .map(|u| {
                        let from = if u.current_version.is_empty() {
                            "unknown"
                        } else {
                            &u.current_version
                        };
                        format!("  {} ({from} -> {})", u.name, u.new_version)
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                log.push(format!(
                    "\nstill offered after the run, so nothing was installed:\n{names}\n\n\
                     Pressing Install updates again will do exactly the same thing. Each of \
                     these has to be uninstalled and reinstalled by hand, or pinned with \
                     `winget pin add --id <Id>` if you would rather it stopped being offered."
                ));
            }
            log.push(exec::tail(&o.text, 3000));
        } else {
            for id in only.iter().filter(|i| !exclude.contains(i)) {
                let o = exec::run(
                    &winget_path().unwrap_or_else(|| "winget.exe".into()),
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
            exec::run(&winget_path().unwrap_or_else(|| "winget.exe".into()), &args, p).await?.require(&[])?;
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
            exec::run(&winget_path().unwrap_or_else(|| "winget.exe".into()), &args, p).await?.require(&[])?;
            let now = winget_installed_version(id, p).await;
            Ok(AppOutcome::changed(format!("installed `{name}`"), now))
        }
        Ensure::Latest => {
            let verb = if installed.is_some() { "upgrade" } else { "install" };
            let mut args = vec![verb, "--id", id];
            args.extend_from_slice(&base);
            let o = exec::run(&winget_path().unwrap_or_else(|| "winget.exe".into()), &args, p).await?;
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
        &winget_path().unwrap_or_else(|| "winget.exe".into()),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape winget prints in practice: a normal upgrade table, and a
    /// footer that is the only mention of the package it will not touch.
    const BLOCKED: &str = "Name              Id                   Version   Available Source
-------------------------------------------------------------------------
Some App          Vendor.SomeApp       1.0.0     1.1.0     winget
2 upgrades available.
1 package(s) have upgrades blocked because newer versions use a different install technology than the current installation. Uninstall each package, then install the newer version.
";

    #[test]
    fn names_the_install_technology_block() {
        let issues = winget_blocked(BLOCKED);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].backend, "winget");
        assert!(issues[0].problem.starts_with("1 application(s)"), "{}", issues[0].problem);
        // The operator needs winget's own words, not only our paraphrase.
        assert!(issues[0].remedy.contains("different install technology"));
        assert!(issues[0].remedy.contains("winget uninstall --id"));
    }

    #[test]
    fn a_clean_machine_raises_nothing() {
        assert!(winget_blocked("No installed package found matching input criteria.").is_empty());
        assert!(winget_blocked("").is_empty());
    }

    #[test]
    fn the_footer_is_not_parsed_as_a_package() {
        // Both trailing lines look enough like table rows to be caught by a
        // column slice, and a phantom package would be a pending update that
        // can never be installed.
        let rows = parse_winget_table(BLOCKED);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].name, "Vendor.SomeApp");
        assert_eq!(rows[0].new_version, "1.1.0");
    }
}
