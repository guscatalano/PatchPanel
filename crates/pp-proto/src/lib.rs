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

pub use devices::{
    DeviceReport, DeviceSpec, DiscoveredHost, DiscoveryScan, Probe, OID_SYS_DESCR,
};
pub use manifest::{AppSource, AppSpec, Ensure, Manifest, PatchPolicy};

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

/// A configured package source. Knowing which repositories a machine trusts is
/// often the actual answer to "why is this one different" - a box pinned to an
/// old suite or carrying a third-party repo will never converge on the others.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repository {
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
    Discover,
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
