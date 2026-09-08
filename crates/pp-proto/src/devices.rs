//! IoT / appliance devices: things PatchPanel must keep an eye on but that
//! cannot host an agent — PLCs, cameras, switches, sensors, embedded panels.
//!
//! Each agent doubles as a *site collector*: it probes the devices assigned to
//! its site over the network and reports firmware back on the same connection
//! it uses for its own inventory. That way one portal sees servers and devices
//! in a single view, and device probing scales out with the fleet instead of
//! funnelling every packet through the portal.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A device the portal wants watched.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceSpec {
    /// Stable key chosen by the operator, e.g. "plc-line3".
    pub id: String,
    #[serde(default)]
    pub label: String,
    /// IP or hostname, resolved by the collector, not the portal.
    pub target: String,
    pub probe: Probe,
    /// Which site's collector owns this device. Empty means "any collector",
    /// which is only sensible in a single-site deployment.
    #[serde(default)]
    pub site: String,
    /// The single machine that probes this device, by hostname.
    ///
    /// Left empty, the portal's own machine takes it. That used to mean every
    /// agent in the site probed it, which turned one firewall into thirteen
    /// identical rows and thirteen times the requests against it - a device
    /// gets probed by one machine, and the only question is which.
    #[serde(default)]
    pub collector: String,
    /// Firmware the device is supposed to be running. A mismatch is reported
    /// as drift; PatchPanel never pushes firmware itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_version: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// How to interrogate a device. Deliberately read-only: every variant asks a
/// question, none of them change the device.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Probe {
    /// Fetch a URL and pull a version out of the response.
    Http {
        url: String,
        /// Skip TLS verification. Necessary for the self-signed certs most
        /// appliances ship with, so it is explicit per device.
        #[serde(default)]
        insecure: bool,
        #[serde(default)]
        headers: Vec<(String, String)>,
        /// RFC-6901 pointer into a JSON body, e.g. "/system/firmware".
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version_json_pointer: Option<String>,
        /// Regex with one capture group, applied to a non-JSON body.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version_regex: Option<String>,
    },
    /// SNMP v2c GET. Defaults to sysDescr, which nearly every device answers.
    Snmp {
        #[serde(default = "default_community")]
        community: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        oid: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version_regex: Option<String>,
    },
    /// Ask an OPNsense firewall what its firmware status is.
    ///
    /// A firewall is the machine you least want to install an agent on, and
    /// the one you least want unpatched. OPNsense answers both problems
    /// itself: it reports its version, the packages waiting to be upgraded and
    /// whether a reboot is pending, over an API, so nothing has to be put on
    /// the box.
    Opnsense {
        /// From System > Access > Users > API keys on the firewall.
        api_key: String,
        api_secret: String,
        /// Appliances ship self-signed certificates, so this is usually true -
        /// and explicit, per device, rather than a global default.
        #[serde(default)]
        insecure: bool,
        /// Ask the firewall to re-check its mirrors when its own last check
        /// is older than this many hours. Zero never checks.
        ///
        /// Every other probe here is strictly read-only; this one is not, so
        /// it is rationed. Devices are probed every few minutes and a check
        /// makes the firewall talk to `pkg.opnsense.org` - doing that on every
        /// probe would be hundreds of pointless requests a day for something
        /// that changes weekly. The firewall records its own last check, so
        /// the throttle lives on the device rather than in the agent, where a
        /// reconnect would forget it.
        #[serde(default = "default_check_hours")]
        check_after_hours: u32,
    },

    /// Ask an Unraid server about itself, over its GraphQL API.
    ///
    /// Unraid runs from RAM and updates itself through its own updater, so an
    /// agent on it would be both awkward to persist and unable to count what
    /// matters. It answers all of this over an API instead, with a read-only
    /// key, and nothing has to be installed.
    Unraid {
        /// A key from Settings > Management Access > API Keys. The VIEWER
        /// role with the INFO and OS resources is enough; do not use ADMIN.
        api_key: String,
        #[serde(default)]
        insecure: bool,
        /// Compare the running version against Unraid's published releases to
        /// decide whether this server is behind.
        ///
        /// Off by default, and deliberately: it is the one thing here that
        /// makes the collector reach the internet rather than only the
        /// hardware you own. Without it the version is reported and the
        /// update count stays unknown - which is honest, where reporting zero
        /// would not be.
        #[serde(default)]
        check_releases: bool,
    },

    /// Open a TCP port and optionally read whatever banner comes back.
    Tcp {
        port: u16,
        #[serde(default)]
        read_banner: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version_regex: Option<String>,
    },
}

fn default_check_hours() -> u32 {
    12
}

fn default_community() -> String {
    "public".to_string()
}

/// sysDescr.0 — the one OID it is safe to assume is implemented.
pub const OID_SYS_DESCR: &str = "1.3.6.1.2.1.1.1.0";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceReport {
    pub id: String,
    pub target: String,
    pub reachable: bool,
    /// Extracted version string, when the probe was able to find one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firmware: Option<String>,
    /// Raw evidence: sysDescr, banner, or a body snippet. Kept short.
    #[serde(default)]
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// True when `firmware` disagrees with the spec's `expect_version`.
    #[serde(default)]
    pub drift: bool,
    /// Packages the device says are waiting to be upgraded, where it can tell
    /// us. A device that cannot report this leaves it at zero, which is why
    /// `updates_known` exists alongside it.
    #[serde(default)]
    pub updates: usize,
    #[serde(default)]
    pub updates_known: bool,
    /// The device is waiting on a reboot to finish applying something.
    #[serde(default)]
    pub reboot_required: bool,
    /// This release is past its end of life.
    ///
    /// Worth its own field rather than being left in the detail text: a device
    /// at end of life reports zero pending updates, and truthfully - there
    /// will never be another one. Counting packages alone shows it as
    /// perfectly healthy right up until something is found in it.
    #[serde(default)]
    pub eol: bool,
    /// What the device said about it, in its own words.
    #[serde(default)]
    pub eol_note: String,
    pub checked_at: DateTime<Utc>,
}

/// An opt-in sweep for devices nobody has declared yet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryScan {
    /// CIDR to sweep, e.g. "10.20.0.0/24".
    pub cidr: String,
    /// Ports that suggest "this is a device worth declaring".
    #[serde(default = "default_probe_ports")]
    pub ports: Vec<u16>,
    /// Site whose collector performs the sweep.
    #[serde(default)]
    pub site: String,
}

fn default_probe_ports() -> Vec<u16> {
    // http, https, telnet, ssh, modbus, ewon/webui, mqtt
    vec![80, 443, 23, 22, 502, 8080, 1883]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredHost {
    pub ip: String,
    pub open_ports: Vec<u16>,
    /// Best-effort identification from banners, e.g. "SSH-2.0-dropbear".
    #[serde(default)]
    pub hint: String,
    /// False once a `DeviceSpec` in the manifest covers this address.
    pub unmanaged: bool,
}
