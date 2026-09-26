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
    /// Where to go to do something about it: the device's own management
    /// page. PatchPanel does not update appliances, so the next step is always
    /// somewhere else, and hunting for the address is the boring part.
    #[serde(default)]
    pub url: String,
    /// Where to learn the version this device *should* be running.
    ///
    /// Most appliances will tell you what they are running and nothing about
    /// whether that is current - the two halves come from different places.
    /// Given both, PatchPanel can say "behind" instead of just printing a
    /// number nobody can judge.
    #[serde(default)]
    pub latest_url: String,
    /// RFC-6901 pointer into the response, e.g. `/homeassistant/default`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_pointer: Option<String>,
    /// Regex with one capture group, for a response that is not JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_regex: Option<String>,
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

    /// Ask Home Assistant what it is running, and optionally keep it there.
    ///
    /// HA tracks updates for itself, its add-ons, and every device it manages
    /// - 85 of them on a modest install - which is far more than its `/config`
    /// endpoint admits to. It also has no read-only credential: a long-lived
    /// token can do anything the user can, so the only meaningful control over
    /// what PatchPanel does with it is here, in `auto_update`.
    HomeAssistant {
        /// Profile > Security > Long-lived access tokens.
        token: String,
        #[serde(default)]
        insecure: bool,
        /// What PatchPanel installs by itself. Off by default: this is the one
        /// probe that can change the thing it is looking at.
        #[serde(default)]
        auto_update: HaAutoUpdate,
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

/// How much of Home Assistant PatchPanel keeps updated by itself.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HaAutoUpdate {
    /// Report only. Home Assistant updates on its own terms, or you do.
    #[default]
    Off,
    /// Home Assistant itself, its operating system, supervisor and add-ons.
    /// Not the firmware of the devices it manages.
    Software,
    /// The above, and device firmware. Firmware is the one thing here that can
    /// leave hardware that does not come back, so it is never included by
    /// accident.
    Everything,
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
    /// The address this target actually resolved to, from the collector that
    /// probed it.
    ///
    /// Devices are declared by name and a sweep finds addresses, so nothing can
    /// connect "192.168.6.1" to "winetown router" without this - and the portal
    /// is the wrong place to resolve it, because that would mean blocking DNS
    /// lookups inside an endpoint the dashboard polls every few seconds. The
    /// collector has already resolved the name in order to probe it, so this
    /// costs nothing and, unlike a lookup done later somewhere else, it is the
    /// address the probe genuinely spoke to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_ip: Option<String>,
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
    /// Hand this range to `nmap` instead of the built-in connect sweep.
    ///
    /// Opt-in because it is a second binary that may not be installed, and
    /// because a version scan is an order of magnitude more traffic than a
    /// connect is - it speaks each protocol rather than reading whatever the
    /// port happens to say first. What it buys is the difference between "22
    /// is open" and "Dropbear sshd 2022.82", which is what decides whether a
    /// row is actionable. A collector without nmap falls back to the sweep and
    /// says so on every host it reports.
    #[serde(default)]
    pub use_nmap: bool,
}

fn default_probe_ports() -> Vec<u16> {
    // http, https, telnet, ssh, modbus, ewon/webui, mqtt
    vec![80, 443, 23, 22, 502, 8080, 1883]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredHost {
    pub ip: String,
    pub open_ports: Vec<u16>,
    /// Best-effort identification: a banner line from the built-in sweep, or
    /// the software nmap named, e.g. "Dropbear sshd 2022.82".
    #[serde(default)]
    pub hint: String,
    /// False once a `DeviceSpec` in the manifest covers this address.
    pub unmanaged: bool,
    /// What each open port turned out to be. Only ever the ports a scanner
    /// actually spoke to and recognised, so a short list against a long
    /// `open_ports` means most of them are still a mystery.
    #[serde(default)]
    pub services: Vec<DiscoveredService>,
    /// Which scanner produced this row.
    ///
    /// On the wire rather than inferred from the manifest, because the manifest
    /// says what was asked for and this says what happened. A range set to use
    /// nmap on a collector that has none comes back from the built-in sweep,
    /// and a row with no services then means "nothing was identified" rather
    /// than "nothing could be".
    #[serde(default)]
    pub scanner: Scanner,
    /// Why this row came from a different scanner than the one asked for.
    ///
    /// Repeated on every host of the affected sweep: a discovery result is a
    /// list of hosts and nothing else, so there is nowhere else for a sweep to
    /// leave a note that survives the trip to the portal.
    #[serde(default)]
    pub scan_note: String,
    /// What the scanner concluded about the host itself. Empty from the
    /// built-in sweep, which cannot see any of it.
    #[serde(default)]
    pub identity: HostIdentity,
}

/// What nmap concluded about a host itself, as opposed to one of its ports.
///
/// Every field here is a guess with a stated confidence, which is why the
/// accuracy travels with the name. An OS fingerprint reported as fact is the
/// kind of thing somebody acts on and then spends an afternoon confused by.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HostIdentity {
    /// Best OS match, and how sure nmap is of it, 0-100.
    #[serde(default)]
    pub os: String,
    #[serde(default)]
    pub os_accuracy: u8,
    /// Other matches it considered, best first. Kept because a 90% and an 88%
    /// guess are not the same thing as one confident answer.
    #[serde(default)]
    pub os_alternatives: Vec<String>,
    /// Hardware address and the vendor that owns its OUI prefix. Only present
    /// on the scanner's own segment - a MAC does not cross a router - and the
    /// single most reliable identifier here, because it is assigned rather than
    /// inferred.
    #[serde(default)]
    pub mac: String,
    #[serde(default)]
    pub mac_vendor: String,
    /// Seconds of uptime nmap inferred from TCP timestamps. Approximate, absent
    /// more often than not, and discarded below an hour - see `nmap.rs`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uptime_secs: Option<i64>,
    /// Platform CPEs from the OS fingerprint, e.g. `cpe:/o:linux:linux_kernel:5.4`.
    /// The one machine-readable identifier in the record, and what a CVE list
    /// wants as its query.
    #[serde(default)]
    pub os_cpe: Vec<String>,
}

/// One open port, as far as the scanner could tell.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredService {
    pub port: u16,
    /// The protocol the scanner spoke, in nmap's vocabulary: "ssh", "http",
    /// "ssl/http". Never filled in from the port number alone - that is a
    /// lookup table, not evidence, and the port is already in `open_ports`.
    #[serde(default)]
    pub name: String,
    /// The software behind it, e.g. "Dropbear sshd". Empty when the scanner
    /// recognised the protocol but not what was speaking it.
    #[serde(default)]
    pub product: String,
    #[serde(default)]
    pub version: String,
    /// Whatever else the probe learned, in nmap's own words - "Ubuntu Linux;
    /// protocol 2.0" for an OpenSSH banner, "workgroup: WORKGROUP" for SMB.
    /// Often the most informative field on the row and the one most likely to
    /// name the distribution.
    #[serde(default)]
    pub extra: String,
    /// The CPE identifiers nmap matched, e.g. `cpe:/o:linux:linux_kernel`.
    /// Machine-readable and worth keeping verbatim: it is the only field here
    /// that another tool could join on.
    #[serde(default)]
    pub cpe: Vec<String>,
}

/// How a host was found.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scanner {
    /// The agent's own connect sweep: open ports, and the first line anything
    /// volunteered.
    #[default]
    Tcp,
    Nmap,
}
