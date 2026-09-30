//! Wire protocol shared by the PatchPanel agent and portal.
//!
//! Both sides speak JSON text frames over a single persistent WebSocket.
//! Every frame is one [`ClientMsg`] or [`ServerMsg`], externally tagged by
//! a `type` field so the wire stays readable in logs and `websocat`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub mod devices;
pub mod manifest;

pub use devices::{DeviceReport, DeviceSpec, DiscoveredHost, DiscoveredService, DiscoveryScan, HaAutoUpdate, HostIdentity, OID_SYS_DESCR, Probe, Scanner,
    SnmpAuth,
    SnmpCipher,
    SnmpVersion};
pub use manifest::{
    AppSource, AppSpec, Ensure, Manifest, PatchPolicy, SourcePolicy, VersionCheck,
};

/// Bumped whenever a frame shape changes incompatibly. The portal refuses
/// agents that speak a different major protocol rather than guessing.
pub const PROTOCOL_VERSION: u32 = 1;

/// Stable identity of an enrolled machine. Generated once by the agent on
/// first run and persisted alongside its token.
pub type AgentId = Uuid;
/// Identity of a single dispatched command, echoed back in every result.
pub type CommandId = Uuid;

// ---------------------------------------------------------------------------
// System description
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OsKind {
    Linux,
    Windows,
    Other,
}

impl OsKind {
    pub fn current() -> Self {
        match std::env::consts::OS {
            "linux" => OsKind::Linux,
            "windows" => OsKind::Windows,
            _ => OsKind::Other,
        }
    }
}

impl std::fmt::Display for OsKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            OsKind::Linux => "linux",
            OsKind::Windows => "windows",
            OsKind::Other => "other",
        };
        f.write_str(s)
    }
}

/// Static facts about the machine. Deliberately not usage: what CPU it has,
/// not how busy it is. Collected once at startup, because none of it changes
/// while the agent runs and polling it would be pure noise.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Hardware {
    #[serde(default)]
    pub cpu_model: String,
    /// Physical cores.
    #[serde(default)]
    pub cpu_cores: u32,
    /// Logical processors, i.e. what `nproc` reports.
    #[serde(default)]
    pub cpu_threads: u32,
    #[serde(default)]
    pub memory_mb: u64,
    /// Non-loopback IPv4 addresses.
    #[serde(default)]
    pub ip_addresses: Vec<String>,
    #[serde(default)]
    pub kernel: String,
    /// System manufacturer and model, where the machine will tell us.
    #[serde(default)]
    pub vendor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemInfo {
    pub hostname: String,
    pub os: OsKind,
    /// Human-readable distro/edition, e.g. "Ubuntu 24.04" or "Windows 11 Pro".
    pub os_version: String,
    pub arch: String,
    pub agent_version: String,
    /// Which package backends the agent detected it can drive here.
    pub backends: Vec<String>,
    /// Collector site this agent belongs to; selects the devices it probes.
    #[serde(default)]
    pub site: String,
    /// Static hardware description. Defaulted so an older agent that does not
    /// send it still enrolls.
    #[serde(default)]
    pub hardware: Hardware,
    /// When this machine last booted. Constant for the life of a connection —
    /// a reboot necessarily means a reconnect — so it is reported here rather
    /// than repeated on every heartbeat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot_time: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Inventory
// ---------------------------------------------------------------------------

/// The raw text of one apt source file, so the portal can edit what is
/// actually on disk rather than a reconstruction of it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceFile {
    pub path: String,
    pub content: String,
    /// A corrected version of this file, when the agent can see something
    /// wrong with it. Offered rather than applied: the operator reviews and
    /// edits before anything is written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested: Option<String>,
    /// One line per change, explaining why.
    #[serde(default)]
    pub notes: Vec<String>,
}

/// A configured package source. Knowing which repositories a machine trusts is
/// often the actual answer to "why is this one different" - a box pinned to an
/// old suite or carrying a third-party repo will never converge on the others.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repository {
    /// Something wrong with this repository that only shows up when you try
    /// to install from it - most of the metadata can be perfectly valid while
    /// the packages themselves have gone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
    /// Backend that owns it: "apt", "dnf", "winget".
    pub source: String,
    /// Where the packages come from.
    pub uri: String,
    /// Release/suite, e.g. "trixie" or "noble-security". Empty when not
    /// applicable.
    #[serde(default)]
    pub suite: String,
    #[serde(default)]
    pub components: Vec<String>,
    #[serde(default)]
    pub enabled: bool,
    /// Which file declares it, for when someone has to go and fix it.
    #[serde(default)]
    pub origin_file: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Upgrading, or in some cases even patching, would likely break this box.
    Blocker,
    /// Worth resolving first, but not disqualifying on its own.
    Warning,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseFinding {
    pub severity: Severity,
    pub summary: String,
    #[serde(default)]
    pub detail: String,
}

/// Which distribution release a machine is on, and whether it is in a fit
/// state to move to the next one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseInfo {
    pub distro: String,
    pub codename: String,
    pub version_id: String,
    /// The next major release, where the sequence is known and that release
    /// has actually been made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    /// Its version number, so the portal can say "Debian 13 (trixie)" rather
    /// than a codename an operator has to look up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_version: Option<String>,
    /// What the distribution currently calls stable, as reported by the
    /// archive itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stable: Option<String>,
    #[serde(default)]
    pub findings: Vec<ReleaseFinding>,
}

impl ReleaseInfo {
    pub fn blockers(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Blocker)
            .count()
    }
}

/// Firmware waiting to be flashed onto a device in this machine.
///
/// Deliberately separate from `AvailableUpdate`: a package can be reinstalled
/// and a bad one rolled back, while a firmware write can leave hardware that
/// does not come back. Nothing here is ever installed by a patch run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FirmwareUpdate {
    /// fwupd's opaque id, which is what an install is addressed to.
    pub device_id: String,
    pub device: String,
    #[serde(default)]
    pub current: String,
    pub available: String,
    /// The vendor's description of what changed.
    #[serde(default)]
    pub summary: String,
    /// Applied at the next boot rather than immediately, which most system
    /// firmware is.
    #[serde(default)]
    pub needs_reboot: bool,
    /// fwupd's own warning about this update, when it carries one.
    #[serde(default)]
    pub caution: String,
}

/// A virtual machine or container running on this host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Guest {
    /// The hypervisor's own identifier - a Proxmox VMID, a Hyper-V name.
    pub id: String,
    pub name: String,
    /// "qemu", "lxc", "hyper-v".
    pub kind: String,
    #[serde(default)]
    pub state: String,
    /// Filled in by the portal: is there an agent reporting for this guest?
    /// The agent cannot know, and it is the whole point of listing them.
    #[serde(default)]
    pub managed: bool,
    /// When this guest was last backed up, from the backup files that exist
    /// rather than from a job claiming to have run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_backup: Option<DateTime<Utc>>,
    /// True when `last_backup` is remembered rather than observed.
    ///
    /// Set by the portal, never by an agent. A host whose backup storage is
    /// offline enumerates no archives, which is indistinguishable at the wire from
    /// a guest that has genuinely never been backed up - and reporting the second
    /// when the first is true is the worst mistake this product can make. It said
    /// `never` for twenty-six guests while twelve terabytes of archives sat on an
    /// unreachable NAS.
    ///
    /// So the portal keeps the last date it did observe and marks it unverified.
    /// The date is still the most useful thing known; what changes is that nothing
    /// downstream may treat it as current.
    #[serde(default)]
    pub backup_unverified: bool,
}

/// How a host's backups are going.
///
/// Not a backup product: the question here is only "is this happening, and is
/// anything stuck". A backup existing is not the same as a backup restoring,
/// and nothing below claims otherwise.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Backups {
    /// Jobs running right now. One that has been running for hours is the
    /// usual shape of "stuck".
    #[serde(default)]
    pub running: Vec<BackupJob>,
    /// The most recent finished jobs, newest first.
    #[serde(default)]
    pub recent: Vec<BackupJob>,
    /// Why the backup state could not be read, when it could not. Empty is
    /// not the same as "no backups", and this is what tells them apart.
    #[serde(default)]
    pub note: String,
}

/// One backup job as the hypervisor recorded it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupJob {
    /// The hypervisor's task id, for finding it in its own UI.
    pub id: String,
    /// Which guest, when the job names one.
    #[serde(default)]
    pub guest: String,
    pub started: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished: Option<DateTime<Utc>>,
    /// `OK`, or whatever the hypervisor said went wrong.
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub ok: bool,
    /// The tail of what the job printed, for a job that failed.
    ///
    /// Only fetched for failures. A status of "OK" needs no explanation, and
    /// pulling the log of every successful nightly backup would be a lot of
    /// text nobody reads - while "why did that fail" is a question the status
    /// alone can never answer, which is the whole reason this field exists.
    #[serde(default)]
    pub log: String,
}

/// This machine's place in the virtualization stack.
///
/// A hypervisor is the one machine whose patch state affects every other
/// machine on it, and its guests are the most likely place for something to be
/// running unmanaged - nobody installs an agent on the VM they spun up to try
/// something.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Virtualization {
    /// "host", "guest", or both when a VM runs VMs of its own.
    pub role: String,
    /// "proxmox", "hyper-v", "kvm guest", "lxc container", ...
    pub platform: String,
    #[serde(default)]
    pub guests: Vec<Guest>,
    /// Why the guest list could not be read, when it could not.
    #[serde(default)]
    pub note: String,
    /// How this host's backups are going.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backups: Option<Backups>,
}

/// A piece of hardware fwupd can see, whether or not it has an update.
///
/// Reported even when everything is current: "no firmware updates" and "this
/// machine's firmware was never looked at" are very different statements, and
/// an empty section cannot tell them apart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FirmwareDevice {
    pub name: String,
    #[serde(default)]
    pub vendor: String,
    #[serde(default)]
    pub version: String,
    /// Whether fwupd could update it at all. Plenty of devices are visible but
    /// not updatable, and counting those as "up to date" would overstate the
    /// coverage.
    #[serde(default)]
    pub updatable: bool,
}

/// Why a machine last restarted.
///
/// A machine that reboots on its own tells you something is wrong, and the
/// evidence is only in its logs until they rotate. Recording it at the next
/// scan means the answer survives long enough for somebody to look.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootReport {
    /// True when the previous shutdown was not a clean one.
    pub unexpected: bool,
    /// A short verdict: "kernel panic", "out of memory", "power loss or host
    /// reset", "clean shutdown".
    pub summary: String,
    /// What the machine's own logs said, so the verdict can be checked.
    #[serde(default)]
    pub detail: String,
}

/// A disk the machine could install a bootloader onto.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Disk {
    pub path: String,
    #[serde(default)]
    pub size: String,
    #[serde(default)]
    pub model: String,
    /// Stable names under /dev/disk/by-id. These are what grub records, and a
    /// stale one is the usual reason a release upgrade stops halfway.
    #[serde(default)]
    pub by_id: Vec<String>,
}

/// A machine left part-way through an upgrade, with packages unpacked but not
/// configured.
///
/// dpkg stops at the first postinst that fails and refuses to do anything else
/// until someone resolves it, so this state is sticky and completely invisible
/// from the outside: `apt list --upgradable` looks normal while nothing can be
/// installed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MidUpgrade {
    /// Packages dpkg reports as half-installed or half-configured.
    pub packages: Vec<String>,
    /// Set when the stuck package is a bootloader, which needs a disk chosen
    /// before it can be configured.
    #[serde(default)]
    pub grub_stuck: bool,
    /// Candidate disks, for that choice.
    #[serde(default)]
    pub disks: Vec<Disk>,
}

/// A reason this machine's update count cannot be trusted.
///
/// The dangerous failure for a patch tool is reporting zero because nothing
/// could be scanned - indistinguishable, on a dashboard, from a machine that
/// is fully patched. Anything that stops a scan is recorded here so the number
/// can be shown as unknown rather than good.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanIssue {
    /// Which backend could not run: "winget", "windowsupdate", "apt", ...
    pub backend: String,
    pub problem: String,
    /// What an operator should do about it.
    #[serde(default)]
    pub remedy: String,
}

/// Reclaimable space: packages kept only as dependencies nothing needs any
/// more, plus the downloaded-package cache.
///
/// Reported before it is acted on, because `autoremove` occasionally proposes
/// something load-bearing - an old kernel you are still booting, a library a
/// hand-installed binary links against - and that is worth a human glance.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Cleanup {
    /// Packages `autoremove` would take.
    #[serde(default)]
    pub packages: Vec<String>,
    /// Bytes those packages occupy.
    #[serde(default)]
    pub reclaim_bytes: u64,
    /// Downloaded .deb/.rpm files. Deleting these is always safe.
    #[serde(default)]
    pub cache_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Package {
    pub name: String,
    pub version: String,
    /// Backend that reported it: "apt", "dnf", "winget", "msu".
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvailableUpdate {
    pub name: String,
    pub current_version: String,
    pub new_version: String,
    pub source: String,
    /// Best-effort: only apt and Windows Update label security updates.
    #[serde(default)]
    pub security: bool,
}

/// A full snapshot of what is installed and what is pending on one machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Inventory {
    pub collected_at: DateTime<Utc>,
    /// When the discovery sweep that produced `discovered` finished.
    ///
    /// Separate from `collected_at` because a sweep runs on its own cadence: a
    /// package refresh five minutes ago does not mean the network was looked at
    /// five minutes ago, and a list of hosts with no honest age on it is a list
    /// nobody can tell is stale. `None` from an agent that has not swept since
    /// it started; the portal carries the stored value forward in that case, the
    /// same way it carries `discovered` itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swept_at: Option<DateTime<Utc>>,
    pub packages: Vec<Package>,
    pub updates: Vec<AvailableUpdate>,
    pub reboot_required: bool,
    /// Apps from the manifest whose observed state differs from desired.
    #[serde(default)]
    pub drift: Vec<Drift>,
    /// Results for every device this agent collects for.
    #[serde(default)]
    pub devices: Vec<DeviceReport>,
    /// Hosts seen by the last discovery sweep, if one is configured.
    #[serde(default)]
    pub discovered: Vec<DiscoveredHost>,
    /// Package repositories this machine is configured to use.
    #[serde(default)]
    pub repositories: Vec<Repository>,
    /// Distribution release state and upgrade readiness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<ReleaseInfo>,
    /// What could be cleaned up, without having done it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup: Option<Cleanup>,
    /// Backends that could not be scanned. Non-empty means `updates` is a
    /// floor, not a total.
    #[serde(default)]
    pub scan_issues: Vec<ScanIssue>,
    /// Upgrades apt declined to perform because they need new packages
    /// installed - `upgrade` will never take these, only `full-upgrade` will.
    #[serde(default)]
    pub held_back: Vec<String>,
    /// Editable apt source files, by path.
    #[serde(default)]
    pub source_files: Vec<SourceFile>,
    /// Upgrades that even a full upgrade refuses. On Ubuntu these are usually
    /// phased: the archive deliberately withholds them from a fraction of
    /// machines until the rollout completes. They are counted as pending and
    /// will never install, which looks exactly like a broken patch run.
    #[serde(default)]
    pub deferred: Vec<String>,
    /// Updates a patch run was asked to install and left exactly where they
    /// were: same installed version, same version on offer.
    ///
    /// Unlike everything above this is observed rather than reported. Neither
    /// apt nor winget will say "I refused that one" in a form worth trusting -
    /// winget gives a count with no names - so the only honest answer comes
    /// from looking at what actually moved.
    #[serde(default)]
    pub blocked: Vec<String>,
    /// Set when dpkg is stuck part-way through an upgrade.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mid_upgrade: Option<MidUpgrade>,
    /// Where this machine is forwarding its syslog, if it is. Read back off
    /// disk on every scan rather than held in memory: an agent that restarts
    /// forgets, and the portal must not conclude "off" from an agent's
    /// amnesia.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub syslog_forward: Option<SyslogForward>,
    /// Firmware updates offered for this machine's hardware.
    #[serde(default)]
    pub firmware: Vec<FirmwareUpdate>,
    /// Everything fwupd can see, so an empty update list can be read as "all
    /// current" rather than "nothing was checked".
    #[serde(default)]
    pub firmware_devices: Vec<FirmwareDevice>,
    /// Why this machine last restarted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot: Option<BootReport>,
    /// Whether this machine hosts virtual machines, or is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub virt: Option<Virtualization>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Drift {
    pub app: String,
    pub desired: String,
    pub observed: String,
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Command {
    /// Re-scan packages and pending updates, then report an [`Inventory`].
    CollectInventory,
    /// Reconcile installed apps against the manifest's desired state.
    ApplyManifest,
    /// Install pending OS updates.
    ApplyPatches {
        #[serde(default)]
        security_only: bool,
        /// Empty means "everything the policy allows".
        #[serde(default)]
        only: Vec<String>,
        /// Use `full-upgrade`, which may install new packages and remove
        /// existing ones. Required to move held-back upgrades such as a kernel
        /// metapackage, and deliberately not the default.
        #[serde(default)]
        full: bool,
    },
    /// Replace the agent binary with the one at `url`, then restart.
    SelfUpdate {
        version: String,
        url: String,
        sha256: String,
    },
    Reboot {
        #[serde(default)]
        delay_secs: u64,
    },
    /// Probe assigned devices now instead of waiting for the schedule.
    ProbeDevices {
        /// Specific device ids, or empty for all assigned to this collector.
        #[serde(default)]
        only: Vec<String>,
    },
    /// Sweep the configured CIDRs for undeclared devices.
    Discover {
        /// Addresses the portal already accounts for, which are swept without
        /// asking SSH to identify itself.
        ///
        /// Version detection completes a TCP connection and reads the banner
        /// without authenticating, and OpenSSH 9.8 and later count that as an
        /// abuse signal: `srclimit_penalise ... connections without attempting
        /// authentication`. An hourly sweep was steadily accruing penalties for
        /// the portal's own address on this fleet's NAS and its controller, which
        /// escalate and would eventually refuse it. For a machine running an agent
        /// or a declared appliance the banner buys nothing - the agent reports its
        /// own version, and the appliance is probed over its API - so the polite
        /// thing and the useful thing agree.
        ///
        /// Only the portal knows the whole fleet, so the list arrives with the
        /// command. Empty means "sweep everything the usual way", which is what an
        /// older portal sends.
        #[serde(default)]
        light: Vec<String>,
    },

    /// Count what the journal holds at each severity, without forwarding any of
    /// it.
    ///
    /// Asked for before turning forwarding on, because the gap between levels is
    /// enormous and invisible until it is too late: a host can hold a hundred
    /// warnings and fifty thousand info lines for the same day, and choosing
    /// between them blind is how somebody ends up shipping a firehose.
    JournalVolume,

    /// Start or stop forwarding this machine's syslog to the portal.
    ///
    /// Deliberately per-machine and never part of a manifest: it writes to
    /// `/etc/rsyslog.d` and reloads a service, which is a change to the
    /// machine, and changes here get asked for rather than applied because a
    /// document said so.
    ///
    /// It fills a gap the hourly inventory cannot: a ZFS checksum error, an OOM
    /// kill, a correctable memory fault or a crash-looping unit happens between
    /// scans and leaves nothing in a package list behind it.
    ConfigureSyslog {
        /// False removes the configuration and reloads, leaving no trace but
        /// the backup.
        #[serde(default)]
        enable: bool,
        /// `warning`, `err`, `notice`, `info`. Anything below this is not sent,
        /// which is what keeps this a trickle rather than the twenty thousand
        /// lines an hour a firewall will happily produce.
        #[serde(default = "default_syslog_severity")]
        min_severity: String,
    },
    /// Replace an apt source file. Validated with `apt-get update` and rolled
    /// back automatically if apt rejects the result, so a bad edit cannot
    /// leave the machine unable to install anything.
    WriteSource { path: String, content: String },

    /// Delete an apt source file, keeping a backup beside it.
    RemoveSource { path: String },

    /// Move this machine to the next distribution release.
    ///
    /// The most destructive thing PatchPanel can do, so it is deliberately
    /// awkward to ask for. `check` performs every preflight and changes
    /// nothing, and `to` must match the release the agent itself worked out -
    /// a page left open for a week cannot order a jump that no longer makes
    /// sense.
    DistroUpgrade {
        to: String,
        #[serde(default)]
        check: bool,
    },

    /// Flash firmware. Never part of a patch run: this is the one action
    /// here that can leave hardware that does not come back, so it is always
    /// asked for on its own.
    UpdateFirmware {
        /// fwupd device ids, or empty for everything on offer.
        #[serde(default)]
        only: Vec<String>,
    },

    /// Finish an upgrade dpkg stopped part-way through.
    ///
    /// `grub_device` answers the question a stuck bootloader package is
    /// waiting on: which disk to install to. It is a choice with consequences,
    /// so it comes from the operator rather than being guessed.
    FinishUpgrade {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        grub_device: Option<String>,
    },

    /// Exit so the supervisor starts the agent again.
    ///
    /// Backends are detected once at startup, so installing package tooling
    /// only takes effect after a restart. Making that a button beats telling
    /// someone to go and do it by hand on the machine.
    RestartAgent,

    /// Install whatever this machine is missing in order to be scannable at
    /// all - on Windows, the PSWindowsUpdate module and a system-wide winget.
    InstallPrerequisites,

    /// Remove packages nothing depends on any more, and empty the package
    /// cache. Reports exactly what it took.
    Cleanup {
        /// Also delete configuration files belonging to removed packages.
        #[serde(default)]
        purge: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandEnvelope {
    pub id: CommandId,
    pub command: Command,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandResult {
    pub id: CommandId,
    pub ok: bool,
    /// One line for the dashboard.
    pub summary: String,
    /// Full captured output, trimmed by the agent before sending.
    pub detail: String,
    pub finished_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Frames
// ---------------------------------------------------------------------------

/// Agent -> portal.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    /// First frame on every connection; carries credentials and identity.
    Hello {
        protocol: u32,
        agent_id: AgentId,
        /// Present only until the portal issues a durable agent token.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        enrollment_token: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_token: Option<String>,
        system: SystemInfo,
        /// Manifest revision the agent currently has applied.
        applied_revision: u64,
    },
    Heartbeat {
        at: DateTime<Utc>,
        reboot_required: bool,
        /// Repeated from `Hello` so the portal learns about a convergence that
        /// happened mid-connection, rather than waiting for a reconnect.
        #[serde(default)]
        applied_revision: u64,
    },
    Inventory(Inventory),
    /// Streamed while a long command runs, so the portal can show live output.
    CommandProgress {
        id: CommandId,
        line: String,
    },
    CommandResult(CommandResult),
    /// Journal lines, for a machine that has been asked to forward them.
    ///
    /// Sent over the connection that already exists rather than over a syslog
    /// socket: most of this fleet has no rsyslog, and installing one to gain a
    /// port is a worse trade than reusing the link the agent is already on.
    ///
    /// Batched, because a host under load emits warnings in bursts and a frame
    /// per line would spend the connection on framing.
    JournalLines {
        lines: Vec<JournalLine>,
    },
}

/// One line out of a machine's journal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalLine {
    pub at: DateTime<Utc>,
    /// Syslog priority, 0-7. Kept as the number the journal reports rather than
    /// a word, so the portal decides how to render it in one place.
    pub priority: i64,
    /// `SYSLOG_IDENTIFIER`, which is the unit or program name.
    #[serde(default)]
    pub tag: String,
    pub message: String,
}

/// Portal -> agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    /// Accepts the connection and hands back the durable token plus the
    /// manifest the agent should converge on.
    Welcome {
        agent_token: String,
        server_time: DateTime<Utc>,
        manifest: Manifest,
    },
    /// Pushed whenever the manifest revision changes while connected.
    Manifest(Manifest),
    Command(CommandEnvelope),
    /// Terminal: the portal is closing the connection and why.
    Error { message: String },
}

fn default_syslog_severity() -> String {
    // Warnings and worse. The host-level events worth knowing about - disk
    // errors, OOM kills, machine checks - are all at this level or above, and
    // everything below it is the noise that makes a log unreadable.
    "warning".to_string()
}

/// What a machine is currently forwarding, as read off its own disk.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyslogForward {
    /// Where it sends. Recorded so a machine pointed at the wrong portal is
    /// visible rather than merely "on".
    pub target: String,
    pub min_severity: String,
    /// Which file says so, so somebody on the box can find and undo it without
    /// PatchPanel's help.
    pub path: String,
}
