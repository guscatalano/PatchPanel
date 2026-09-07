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
    /// Open a TCP port and optionally read whatever banner comes back.
    Tcp {
        port: u16,
        #[serde(default)]
        read_banner: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version_regex: Option<String>,
    },
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
