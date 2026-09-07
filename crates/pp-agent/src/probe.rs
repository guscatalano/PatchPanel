//! Probing devices that cannot run an agent.
//!
//! Every probe here is read-only and short: PatchPanel reports what firmware a
//! device claims to be running and whether that matches the manifest, but it
//! never writes to the device. Pushing firmware to a PLC or a camera is a
//! vendor-specific, physically risky operation and does not belong behind a
//! generic "apply" button.

use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::Utc;
use ipnet::IpNet;
use pp_proto::{DeviceReport, DeviceSpec, DiscoveredHost, DiscoveryScan, Probe, OID_SYS_DESCR};
use regex::Regex;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio::sync::Semaphore;

/// Devices are on the far side of flaky field networks; a slow answer is
/// normal, a missing one should not stall the whole sweep.
const PROBE_TIMEOUT: Duration = Duration::from_secs(8);
/// Evidence kept per device — enough to identify a model, not a whole page.
const MAX_DETAIL: usize = 512;
/// How many devices to interrogate at once.
const PROBE_CONCURRENCY: usize = 16;
/// How many sockets a discovery sweep may hold open at once. Field switches
/// are easy to overwhelm, so this stays modest on purpose.
const SCAN_CONCURRENCY: usize = 64;
/// Refuse to sweep anything larger than a /22, which is already 1022 hosts.
const MAX_SCAN_HOSTS: usize = 1024;

/// Probe every device in `specs`, bounded in parallel.
pub async fn probe_all(specs: &[DeviceSpec]) -> Vec<DeviceReport> {
    let sem = Arc::new(Semaphore::new(PROBE_CONCURRENCY));
    let mut tasks = Vec::with_capacity(specs.len());

    for spec in specs {
        let spec = spec.clone();
        let sem = sem.clone();
        tasks.push(tokio::spawn(async move {
            let _permit = sem.acquire().await;
            probe_one(&spec).await
        }));
    }

    let mut out = Vec::with_capacity(tasks.len());
    for t in tasks {
        match t.await {
            Ok(r) => out.push(r),
            // A panicking probe must not take down the collector's report.
            Err(e) => tracing::error!(error = %e, "device probe task failed"),
        }
    }
    out
}

pub async fn probe_one(spec: &DeviceSpec) -> DeviceReport {
    let started = Instant::now();
    let result = tokio::time::timeout(PROBE_TIMEOUT + Duration::from_secs(2), run_probe(spec)).await;

    let mut report = DeviceReport {
        id: spec.id.clone(),
        target: spec.target.clone(),
        reachable: false,
        firmware: None,
        detail: String::new(),
        latency_ms: None,
        error: None,
        drift: false,
        checked_at: Utc::now(),
    };

    match result {
        Ok(Ok((firmware, detail))) => {
            report.reachable = true;
            report.firmware = firmware;
            report.detail = truncate(&detail, MAX_DETAIL);
            report.latency_ms = Some(started.elapsed().as_millis() as u64);
        }
        Ok(Err(e)) => report.error = Some(format!("{e:#}")),
        Err(_) => report.error = Some("probe timed out".into()),
    }

    // Drift is only meaningful when we both expect a version and read one.
    if let (Some(want), Some(got)) = (&spec.expect_version, &report.firmware) {
        report.drift = !version_matches(want, got);
    }

    report
}

/// Devices report versions inconsistently ("v2.10.1", "2.10.1-rel"), so a
/// containment check beats equality without being as loose as a substring
/// match on the whole banner.
fn version_matches(want: &str, got: &str) -> bool {
    let norm = |s: &str| s.trim().trim_start_matches(['v', 'V']).to_ascii_lowercase();
    let (w, g) = (norm(want), norm(got));
    g == w || g.starts_with(&format!("{w}-")) || g.starts_with(&format!("{w}."))
}

/// Returns `(extracted version, raw evidence)`.
async fn run_probe(spec: &DeviceSpec) -> Result<(Option<String>, String)> {
    match &spec.probe {
        Probe::Http {
            url,
            insecure,
            headers,
            version_json_pointer,
            version_regex,
        } => {
            let client = reqwest::Client::builder()
                .timeout(PROBE_TIMEOUT)
                .danger_accept_invalid_certs(*insecure)
                .user_agent(concat!("patchpanel-agent/", env!("CARGO_PKG_VERSION")))
                .build()?;

            let mut req = client.get(url);
            for (k, v) in headers {
                req = req.header(k.as_str(), v.as_str());
            }
            let resp = req.send().await.context("http probe")?;
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            if !status.is_success() {
                anyhow::bail!("http {} from {}", status.as_u16(), url);
            }

            let version = if let Some(ptr) = version_json_pointer {
                serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| v.pointer(ptr).cloned())
                    .map(|v| match v {
                        serde_json::Value::String(s) => s,
                        other => other.to_string(),
                    })
            } else if let Some(re) = version_regex {
                capture(re, &body)?
            } else {
                None
            };
            Ok((version, body))
        }

        Probe::Snmp {
            community,
            oid,
            version_regex,
        } => {
            let oid_str = oid.as_deref().unwrap_or(OID_SYS_DESCR);
            let addr = resolve(&spec.target, 161).await?;

            let client = csnmp::Snmp2cClient::new(
                addr,
                community.as_bytes().to_vec(),
                None,
                Some(PROBE_TIMEOUT),
                1,
            )
            .await
            .context("creating SNMP client")?;

            let oid = csnmp::ObjectIdentifier::from_str(oid_str)
                .map_err(|e| anyhow::anyhow!("bad OID `{oid_str}`: {e:?}"))?;
            let value = client
                .get(oid)
                .await
                .map_err(|e| anyhow::anyhow!("SNMP get failed: {e}"))?;

            let text = match value {
                csnmp::ObjectValue::String(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                csnmp::ObjectValue::ObjectId(o) => o.to_string(),
                csnmp::ObjectValue::Integer(i) => i.to_string(),
                csnmp::ObjectValue::Counter32(n) | csnmp::ObjectValue::Unsigned32(n) => {
                    n.to_string()
                }
                csnmp::ObjectValue::TimeTicks(n) => n.to_string(),
                csnmp::ObjectValue::Counter64(n) => n.to_string(),
                csnmp::ObjectValue::IpAddress(a) => a.to_string(),
                csnmp::ObjectValue::Opaque(b) => hex::encode(b),
            };

            let version = match version_regex {
                Some(re) => capture(re, &text)?,
                // Without a pattern, sysDescr itself is the best evidence we
                // have; the operator can add a regex to sharpen it.
                None => Some(text.clone()),
            };
            Ok((version, text))
        }

        Probe::Tcp {
            port,
            read_banner,
            version_regex,
        } => {
            let addr = resolve(&spec.target, *port).await?;
            let mut stream = tokio::time::timeout(PROBE_TIMEOUT, TcpStream::connect(addr))
                .await
                .map_err(|_| anyhow::anyhow!("connect to {addr} timed out"))?
                .with_context(|| format!("connecting to {addr}"))?;

            if !read_banner {
                return Ok((None, format!("tcp/{port} open")));
            }

            let mut buf = [0u8; 512];
            // Plenty of services stay silent until spoken to; an empty read is
            // still a successful probe, just without a banner.
            let n = tokio::time::timeout(Duration::from_secs(3), stream.read(&mut buf))
                .await
                .unwrap_or(Ok(0))
                .unwrap_or(0);
            let banner = String::from_utf8_lossy(&buf[..n]).trim().to_string();

            let version = match version_regex {
                Some(re) => capture(re, &banner)?,
                None => (!banner.is_empty()).then(|| banner.clone()),
            };
            Ok((version, banner))
        }
    }
}

/// Apply a single-capture-group regex, returning group 1.
fn capture(pattern: &str, haystack: &str) -> Result<Option<String>> {
    let re = Regex::new(pattern).with_context(|| format!("bad regex `{pattern}`"))?;
    Ok(re
        .captures(haystack)
        .and_then(|c| c.get(1).or_else(|| c.get(0)))
        .map(|m| m.as_str().trim().to_string()))
}

async fn resolve(target: &str, default_port: u16) -> Result<SocketAddr> {
    // Accept a bare address, an address with a port, or a hostname.
    if let Ok(addr) = target.parse::<SocketAddr>() {
        return Ok(addr);
    }
    if let Ok(ip) = target.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, default_port));
    }
    let host = if target.contains(':') {
        target.to_string()
    } else {
        format!("{target}:{default_port}")
    };
    let mut addrs = tokio::net::lookup_host(host.clone())
        .await
        .with_context(|| format!("resolving `{host}`"))?;
    addrs
        .next()
        .with_context(|| format!("`{host}` resolved to no addresses"))
}

fn truncate(s: &str, n: usize) -> String {
    let s = s.trim();
    if s.len() <= n {
        return s.to_string();
    }
    let mut end = n;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &s[..end])
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

/// Sweep the configured ranges for devices nobody has declared, so the fleet
/// view includes the things people forgot to write down.
pub async fn discover(scans: &[DiscoveryScan], known: &[DeviceSpec]) -> Vec<DiscoveredHost> {
    let mut out = Vec::new();
    for scan in scans {
        match sweep(scan, known).await {
            Ok(hosts) => out.extend(hosts),
            Err(e) => tracing::warn!(cidr = %scan.cidr, error = %e, "discovery sweep failed"),
        }
    }
    out
}

async fn sweep(scan: &DiscoveryScan, known: &[DeviceSpec]) -> Result<Vec<DiscoveredHost>> {
    let net: IpNet = scan.cidr.parse().with_context(|| format!("bad CIDR `{}`", scan.cidr))?;
    let hosts: Vec<IpAddr> = net.hosts().take(MAX_SCAN_HOSTS + 1).collect();
    if hosts.len() > MAX_SCAN_HOSTS {
        anyhow::bail!(
            "`{}` covers more than {MAX_SCAN_HOSTS} hosts; narrow the range",
            scan.cidr
        );
    }

    let sem = Arc::new(Semaphore::new(SCAN_CONCURRENCY));
    let ports = Arc::new(scan.ports.clone());
    let mut tasks = Vec::new();

    for ip in hosts {
        let sem = sem.clone();
        let ports = ports.clone();
        tasks.push(tokio::spawn(async move {
            let _permit = sem.acquire().await;
            scan_host(ip, &ports).await
        }));
    }

    let mut found = Vec::new();
    for t in tasks {
        if let Ok(Some(mut host)) = t.await {
            host.unmanaged = !known.iter().any(|d| {
                d.target == host.ip || d.target.starts_with(&format!("{}:", host.ip))
            });
            found.push(host);
        }
    }
    Ok(found)
}

async fn scan_host(ip: IpAddr, ports: &[u16]) -> Option<DiscoveredHost> {
    let mut open = Vec::new();
    let mut hint = String::new();

    for &port in ports {
        let addr = SocketAddr::new(ip, port);
        // A short timeout is what keeps a /24 sweep to seconds rather than
        // minutes: unused addresses simply never answer.
        let Ok(Ok(mut stream)) =
            tokio::time::timeout(Duration::from_millis(600), TcpStream::connect(addr)).await
        else {
            continue;
        };
        open.push(port);

        if hint.is_empty() {
            let mut buf = [0u8; 128];
            if let Ok(Ok(n)) =
                tokio::time::timeout(Duration::from_millis(400), stream.read(&mut buf)).await
            {
                if n > 0 {
                    hint = String::from_utf8_lossy(&buf[..n])
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .trim()
                        .chars()
                        .take(80)
                        .collect();
                }
            }
        }
    }

    (!open.is_empty()).then(|| DiscoveredHost {
        ip: ip.to_string(),
        open_ports: open,
        hint,
        unmanaged: true,
    })
}
