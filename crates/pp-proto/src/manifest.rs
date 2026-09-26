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
    /// Apt source files every matching machine should have.
    ///
    /// One machine's broken sources are usually the whole release's broken
    /// sources - when Debian retires a release, every box still on it breaks
    /// the same way on the same day. Fixing them one at a time means finding
    /// them one at a time.
    #[serde(default)]
    pub apt_sources: Vec<SourcePolicy>,
    /// Applications whose real version PatchPanel cannot learn from a package
    /// manager, watched by comparing what is installed against what the vendor
    /// publishes.
    #[serde(default)]
    pub version_checks: Vec<VersionCheck>,
    pub patch_policy: PatchPolicy,
    /// The address agents should use to reach the portal. Set this when the
    /// portal's canonical name differs from whatever an agent was bootstrapped
    /// with - a short name vs an FQDN, say. Agents switch to it and fall back
    /// to their bootstrap URL if it does not work, so a typo here cannot
    /// orphan the fleet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub portal_url: Option<String>,

    /// Desired agent version. When it differs from what an agent reports,
    /// the portal dispatches a `SelfUpdate`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_version: Option<String>,
    pub heartbeat_secs: u64,
    pub inventory_secs: u64,
    /// How often collectors re-probe their devices.
    #[serde(default = "default_device_secs")]
    pub device_probe_secs: u64,
    /// How often collectors re-sweep their discovery ranges.
    ///
    /// Longer than a device probe by an order of magnitude, because a sweep is
    /// the one thing here that touches machines nobody asked us to touch: it
    /// opens connections to every address in a range, and with `use_nmap` it
    /// fingerprints them. Half an hour is frequent enough that a new device on
    /// the network is noticed the same afternoon, and rare enough that the scan
    /// is not a permanent feature of anyone's traffic graphs.
    ///
    /// Only agents whose site owns a range sweep it, so this is one sweep per
    /// range per interval and not one per agent.
    #[serde(default = "default_discovery_secs")]
    pub discovery_secs: u64,
}

/// Watch an application's version without being able to update it.
///
/// Some software updates through a vendor's own script, or sits in a
/// repository pinned to an old series, and a package manager will happily
/// report nothing pending while the vendor is three major versions ahead. This
/// closes that blind spot the cheapest way it can be closed: read what is
/// installed from the inventory PatchPanel already has, read what exists from
/// the vendor, and compare.
///
/// Deliberately does not install anything. Running a vendor's update script
/// would mean arbitrary code execution as root across the fleet, and the
/// answer to "am I behind?" is most of the value without any of that.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionCheck {
    /// Shown in the UI, e.g. "UniFi Network Application".
    pub name: String,
    /// The installed package to match, as it appears in the inventory.
    pub package: String,
    /// Where the current version is published.
    pub url: String,
    /// RFC-6901 pointer into a JSON response, e.g.
    /// `/_embedded/firmware/0/version`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_pointer: Option<String>,
    /// Regex with one capture group, for a response that is not JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regex: Option<String>,
    /// How this actually gets updated, since PatchPanel will not do it.
    #[serde(default)]
    pub note: String,
    /// Release notes, or the vendor's instructions.
    #[serde(default)]
    pub link: String,
    /// How often to ask the vendor. Versions are published weekly at most.
    #[serde(default = "default_version_check_hours")]
    pub check_after_hours: u32,
}

fn default_version_check_hours() -> u32 {
    12
}

/// A source file to place on every machine matching a distribution and
/// release.
///
/// Applied by `ApplyManifest` through the same path as a hand edit: written,
/// checked with `apt-get update`, and rolled back automatically if apt rejects
/// it. A policy that is wrong cannot take the fleet's package manager down
/// with it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourcePolicy {
    /// `debian`, `ubuntu`, ... matched against the machine's os-release ID.
    pub distro: String,
    /// Release codename this applies to: `bullseye`, `noble`. Empty means
    /// every release of that distribution, which is rarely what you want.
    #[serde(default)]
    pub codename: String,
    /// Absolute path under /etc/apt.
    pub path: String,
    /// The file's full contents.
    pub content: String,
    /// Why, for the operator reading it later.
    #[serde(default)]
    pub note: String,
}

impl Manifest {
    /// The source policies that apply to a machine.
    pub fn sources_for<'a>(
        &'a self,
        distro: &'a str,
        codename: &'a str,
    ) -> impl Iterator<Item = &'a SourcePolicy> {
        self.apt_sources.iter().filter(move |s| {
            s.distro.eq_ignore_ascii_case(distro)
                && (s.codename.is_empty() || s.codename.eq_ignore_ascii_case(codename))
        })
    }
}

/// Do these name the same machine? Tolerates an FQDN on either side, since
/// an agent reports its short hostname while the portal is usually configured
/// by a full name.
fn same_host(a: &str, b: &str) -> bool {
    let short = |s: &str| s.split('.').next().unwrap_or(s).to_ascii_lowercase();
    !a.is_empty() && !b.is_empty() && short(a) == short(b)
}

fn default_device_secs() -> u64 {
    300
}

fn default_discovery_secs() -> u64 {
    1800
}

impl Default for Manifest {
    fn default() -> Self {
        Manifest {
            revision: 1,
            portal_url: None,
            apps: Vec::new(),
            apt_sources: Vec::new(),
            version_checks: Vec::new(),
            devices: Vec::new(),
            discovery: Vec::new(),
            patch_policy: PatchPolicy::default(),
            agent_version: None,
            heartbeat_secs: 30,
            inventory_secs: 3600,
            device_probe_secs: default_device_secs(),
            discovery_secs: default_discovery_secs(),
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
    /// The devices this collector should probe.
    ///
    /// Exactly one machine probes each device. A device that names a
    /// `collector` is probed by that machine; one that does not falls to
    /// whichever agent runs on the portal itself, which needs no coordination
    /// between agents to agree on - every agent already knows the portal's
    /// address, and only one of them is it.
    pub fn devices_for<'a>(
        &'a self,
        site: &'a str,
        host: &'a str,
        portal_host: &'a str,
    ) -> impl Iterator<Item = &'a DeviceSpec> {
        self.devices.iter().filter(move |d| {
            let site_ok = d.site.is_empty() || d.site == site;
            let mine = if d.collector.is_empty() {
                same_host(host, portal_host)
            } else {
                same_host(host, &d.collector)
            };
            site_ok && mine
        })
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
