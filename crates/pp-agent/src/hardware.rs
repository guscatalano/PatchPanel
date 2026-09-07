//! Static machine facts: what this box *is*, not what it is doing.
//!
//! Collected once at startup. None of it changes while the agent runs, and a
//! fleet view wants "which of these has 128 GB" far more often than it wants a
//! CPU graph — live metrics are a different product, and pretending otherwise
//! would mean every agent chattering on an interval forever.

use std::net::{IpAddr, SocketAddr, UdpSocket};

use pp_proto::Hardware;

pub fn collect() -> Hardware {
    let mut hw = platform_hardware();

    if hw.ip_addresses.is_empty() {
        // Fall back to asking the routing table which source address it would
        // use to reach the outside world. No packets are sent - connect() on a
        // UDP socket only fixes the local endpoint.
        if let Some(ip) = primary_ip() {
            hw.ip_addresses.push(ip);
        }
    }
    if hw.cpu_threads == 0 {
        hw.cpu_threads = std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(0);
    }
    hw
}

/// The source address the kernel would pick for outbound traffic.
fn primary_ip() -> Option<String> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    // TEST-NET-1: guaranteed unroutable, so this never leaves the host.
    sock.connect(SocketAddr::from(([192, 0, 2, 1], 53))).ok()?;
    match sock.local_addr().ok()?.ip() {
        IpAddr::V4(v4) if !v4.is_loopback() => Some(v4.to_string()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Linux
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
fn platform_hardware() -> Hardware {
    use std::collections::HashSet;

    let mut hw = Hardware::default();

    if let Ok(text) = std::fs::read_to_string("/proc/cpuinfo") {
        // Count distinct (physical id, core id) pairs for physical cores, and
        // "processor" lines for threads. On a VM these often collapse to the
        // same number, which is correct: the guest has no visible SMT.
        let mut cores: HashSet<(String, String)> = HashSet::new();
        let (mut phys, mut core) = (String::new(), String::new());

        for line in text.lines() {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            match key {
                "model name" if hw.cpu_model.is_empty() => hw.cpu_model = value.to_string(),
                "processor" => hw.cpu_threads += 1,
                "physical id" => phys = value.to_string(),
                "core id" => {
                    core = value.to_string();
                    cores.insert((phys.clone(), core.clone()));
                }
                _ => {}
            }
        }
        let _ = core;
        hw.cpu_cores = cores.len() as u32;
        if hw.cpu_cores == 0 {
            hw.cpu_cores = hw.cpu_threads;
        }
    }

    if let Ok(text) = std::fs::read_to_string("/proc/meminfo") {
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("MemTotal:") {
                if let Some(kb) = rest.split_whitespace().next().and_then(|v| v.parse::<u64>().ok())
                {
                    hw.memory_mb = kb / 1024;
                }
                break;
            }
        }
    }

    hw.kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    let vendor = read_trim("/sys/class/dmi/id/sys_vendor");
    let product = read_trim("/sys/class/dmi/id/product_name");
    hw.vendor = [vendor, product]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");

    hw.ip_addresses = linux_addresses();
    hw
}

#[cfg(target_os = "linux")]
fn read_trim(path: &str) -> String {
    std::fs::read_to_string(path)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// Parse `/proc/net/fib_trie` for local IPv4 addresses. Avoids shelling out to
/// `ip`, which is not installed on every minimal image.
#[cfg(target_os = "linux")]
fn linux_addresses() -> Vec<String> {
    let Ok(text) = std::fs::read_to_string("/proc/net/fib_trie") else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    let mut last = "";
    for line in text.lines() {
        let t = line.trim();
        if t.contains("/32 host LOCAL") {
            if let Some(addr) = last.rsplit("|--").next().map(str::trim) {
                if let Ok(ip) = addr.parse::<std::net::Ipv4Addr>() {
                    let s = ip.to_string();
                    if !ip.is_loopback() && !out.contains(&s) {
                        out.push(s);
                    }
                }
            }
        }
        last = t;
    }
    out
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------

#[cfg(windows)]
const HW_QUERY: &str = r#"
$c = Get-CimInstance Win32_Processor | Select-Object -First 1
$s = Get-CimInstance Win32_ComputerSystem
$ips = @()
try {
  $ips = @(Get-NetIPAddress -AddressFamily IPv4 -ErrorAction Stop |
    Where-Object { $_.IPAddress -notlike '127.*' -and $_.IPAddress -notlike '169.254.*' } |
    Select-Object -ExpandProperty IPAddress)
} catch { }
[pscustomobject]@{
  CpuModel = $c.Name
  Cores    = [int]$c.NumberOfCores
  Threads  = [int]$c.NumberOfLogicalProcessors
  MemoryMB = [int64]($s.TotalPhysicalMemory / 1MB)
  Vendor   = ("$($s.Manufacturer) $($s.Model)").Trim()
  Kernel   = [string][System.Environment]::OSVersion.Version
  Ips      = $ips
} | ConvertTo-Json -Compress
"#;

#[cfg(windows)]
fn platform_hardware() -> Hardware {
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Raw {
        #[serde(rename = "CpuModel")]
        cpu_model: Option<String>,
        #[serde(rename = "Cores")]
        cores: Option<u32>,
        #[serde(rename = "Threads")]
        threads: Option<u32>,
        #[serde(rename = "MemoryMB")]
        memory_mb: Option<u64>,
        #[serde(rename = "Vendor")]
        vendor: Option<String>,
        #[serde(rename = "Kernel")]
        kernel: Option<String>,
        #[serde(rename = "Ips")]
        ips: Option<serde_json::Value>,
    }

    let out = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            HW_QUERY,
        ])
        .stdin(std::process::Stdio::null())
        .output();

    let Ok(out) = out else {
        tracing::warn!("could not run the hardware query");
        return Hardware::default();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let Some(start) = text.find('{') else {
        return Hardware::default();
    };

    let Ok(raw) = serde_json::from_str::<Raw>(&text[start..]) else {
        tracing::warn!(output = %text.chars().take(200).collect::<String>(), "unparseable hardware query");
        return Hardware::default();
    };

    // ConvertTo-Json collapses a one-element array to a bare string, so the
    // address list arrives as either shape.
    let ip_addresses = match raw.ips {
        Some(serde_json::Value::Array(a)) => a
            .into_iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        Some(serde_json::Value::String(s)) => vec![s],
        _ => Vec::new(),
    };

    Hardware {
        cpu_model: raw.cpu_model.unwrap_or_default().trim().to_string(),
        cpu_cores: raw.cores.unwrap_or(0),
        cpu_threads: raw.threads.unwrap_or(0),
        memory_mb: raw.memory_mb.unwrap_or(0),
        ip_addresses,
        kernel: raw.kernel.unwrap_or_default(),
        vendor: raw.vendor.unwrap_or_default(),
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
fn platform_hardware() -> Hardware {
    Hardware::default()
}
