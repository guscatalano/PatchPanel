//! Desired state: the single document the portal keeps and every agent
//! converges on. One manifest covers all three things PatchPanel manages —
//! OS patches, application versions, and the agent's own version.

use serde::{Deserialize, Serialize};

use crate::devices::{DeviceSpec, DiscoveryScan};
use crate::OsKind;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// Monotonic. Agents only act when this is newer than what they applied.
    pub revision: u64,
    pub apps: Vec<AppSpec>,
    /// Devices that cannot host an agent, probed by their site's collector.
    #[serde(default)]
    pub devices: Vec<DeviceSpec>,
    /// Opt-in sweeps for devices nobody has declared yet.
    #[serde(default)]
    pub discovery: Vec<DiscoveryScan>,
    pub patch_policy: PatchPolicy,
    /// Desired agent version. When it differs from what an agent reports,
    /// the portal dispatches a `SelfUpdate`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_version: Option<String>,
    pub heartbeat_secs: u64,
    pub inventory_secs: u64,
    /// How often collectors re-probe their devices.
    #[serde(default = "default_device_secs")]
    pub device_probe_secs: u64,
}

fn default_device_secs() -> u64 {
    300
}

impl Default for Manifest {
    fn default() -> Self {
        Manifest {
            revision: 1,
            apps: Vec::new(),
            devices: Vec::new(),
            discovery: Vec::new(),
            patch_policy: PatchPolicy::default(),
            agent_version: None,
            heartbeat_secs: 30,
            inventory_secs: 3600,
            device_probe_secs: default_device_secs(),
        }
    }
}

impl Manifest {
    /// Apps that apply to the given OS. An empty `os` list means "any".
    pub fn apps_for(&self, os: OsKind) -> impl Iterator<Item = &AppSpec> {
        self.apps
            .iter()
            .filter(move |a| a.os.is_empty() || a.os.contains(&os))
    }

    /// Devices a collector at `site` is responsible for. A device with no
    /// site is everyone's, which only makes sense with one collector.
    pub fn devices_for<'a>(&'a self, site: &'a str) -> impl Iterator<Item = &'a DeviceSpec> {
        self.devices
            .iter()
            .filter(move |d| d.site.is_empty() || d.site == site)
    }

    pub fn discovery_for<'a>(&'a self, site: &'a str) -> impl Iterator<Item = &'a DiscoveryScan> {
        self.discovery
            .iter()
            .filter(move |d| d.site.is_empty() || d.site == site)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchPolicy {
    /// Install pending updates automatically on the inventory schedule.
    pub auto_apply: bool,
    pub security_only: bool,
    /// Whether the agent may reboot itself when an update demands it.
    pub allow_reboot: bool,
    /// Package names never to touch, e.g. a pinned kernel or database.
    #[serde(default)]
    pub exclude: Vec<String>,
}

impl Default for PatchPolicy {
    fn default() -> Self {
        // Safe by default: observe and report, change nothing until asked.
        PatchPolicy {
            auto_apply: false,
            security_only: true,
            allow_reboot: false,
            exclude: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSpec {
    pub name: String,
    /// Ignored when `ensure` is `Latest` or `Absent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub ensure: Ensure,
    pub source: AppSource,
    /// Restrict this app to certain platforms; empty means all.
    #[serde(default)]
    pub os: Vec<OsKind>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ensure {
    /// Install if missing; leave the version alone once present.
    Present,
    /// Install, and upgrade whenever the backend has something newer.
    Latest,
    Absent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AppSource {
    Apt { package: String },
    Dnf { package: String },
    Winget { id: String },
    /// Escape hatch: fetch a file, verify it, run a command against it.
    /// `{}` in `install_cmd` is replaced with the downloaded file's path.
    Url {
        url: String,
        sha256: String,
        install_cmd: String,
    },
}

impl AppSource {
    pub fn backend(&self) -> &'static str {
        match self {
            AppSource::Apt { .. } => "apt",
            AppSource::Dnf { .. } => "dnf",
            AppSource::Winget { .. } => "winget",
            AppSource::Url { .. } => "url",
        }
    }
}
