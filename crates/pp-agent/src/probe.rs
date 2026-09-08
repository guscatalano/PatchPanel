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
use chrono::{DateTime, Utc};
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
        updates: 0,
        updates_known: false,
        reboot_required: false,
        eol: false,
        eol_note: String::new(),
        checked_at: Utc::now(),
    };

    match result {
        Ok(Ok((firmware, detail, extra))) => {
            report.reachable = true;
            report.firmware = firmware;
            report.detail = truncate(&detail, MAX_DETAIL);
            report.latency_ms = Some(started.elapsed().as_millis() as u64);
            report.updates = extra.updates.unwrap_or(0);
            report.updates_known = extra.updates.is_some();
            report.reboot_required = extra.reboot_required;
            report.eol = extra.eol;
            report.eol_note = extra.eol_note;
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

/// Ask an OPNsense firewall about its own firmware.
///
/// The awkward part is that a firewall which has never checked returns almost
/// nothing: no package lists, no counts, just `status: none` and a message
/// saying a check is needed. Counting those absent lists as zero would report
/// a firewall as up to date having established nothing at all, which is the
/// one mistake this whole tool exists to avoid.
async fn opnsense(spec: &DeviceSpec) -> Result<(Option<String>, String, Extra)> {
    let Probe::Opnsense {
        api_key,
        api_secret,
        insecure,
        check_after_hours,
    } = &spec.probe
    else {
        anyhow::bail!("not an opnsense probe");
    };

    let base = if spec.target.starts_with("http") {
        spec.target.trim_end_matches('/').to_string()
    } else {
        format!("https://{}", spec.target.trim_end_matches('/'))
    };

    let client = reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .danger_accept_invalid_certs(*insecure)
        .user_agent(concat!("patchpanel-agent/", env!("CARGO_PKG_VERSION")))
        .build()?;

    let status_url = format!("{base}/api/core/firmware/status");
    let get = |url: String| {
        let client = client.clone();
        let (k, sec) = (api_key.clone(), api_secret.clone());
        async move {
            let resp = client
                .get(&url)
                .basic_auth(&k, Some(&sec))
                .send()
                .await
                .context("asking opnsense for its firmware status")?;
            let code = resp.status();
            let body = resp.text().await.unwrap_or_default();
            if code == reqwest::StatusCode::UNAUTHORIZED {
                anyhow::bail!(
                    "opnsense rejected the API key (401). The key's user needs the \
                     `System: Firmware` privilege."
                );
            }
            if !code.is_success() {
                anyhow::bail!("http {} from opnsense", code.as_u16());
            }
            serde_json::from_str::<serde_json::Value>(&body)
                .context("opnsense returned something that is not JSON")
        }
    };

    let mut doc = get(status_url.clone()).await?;

    // A payload without `last_check` is one from a firewall that has never
    // checked: the package lists are absent rather than empty.
    let stale = match doc.get("last_check").and_then(|v| v.as_str()) {
        None => true,
        Some(when) => match parse_bsd_date(when) {
            Some(t) => Utc::now().signed_duration_since(t).num_hours()
                >= i64::from(*check_after_hours),
            // A timestamp we cannot read is not a reason to hammer the mirror.
            None => false,
        },
    };

    if stale && *check_after_hours > 0 {
        // The check runs in the background and returns immediately, so the
        // answer has to be waited for. An empty body is required: a bare POST
        // is a 411.
        let started = client
            .post(format!("{base}/api/core/firmware/check"))
            .basic_auth(api_key, Some(api_secret))
            .header("Content-Type", "application/json")
            .body("{}")
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false);

        if started {
            for _ in 0..6 {
                tokio::time::sleep(Duration::from_secs(5)).await;
                if let Ok(next) = get(status_url.clone()).await {
                    let done = next.get("last_check").is_some();
                    doc = next;
                    if done {
                        break;
                    }
                }
            }
        }
    }

    let text = |k: &str| doc.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let count = |k: &str| doc.get(k).and_then(|v| v.as_array()).map(|a| a.len());
    // OPNsense is inconsistent about whether these are numbers or strings.
    let flag = |k: &str| match doc.get(k) {
        Some(serde_json::Value::String(s)) => s == "1" || s == "true",
        Some(serde_json::Value::Number(n)) => n.as_i64().unwrap_or(0) != 0,
        Some(serde_json::Value::Bool(b)) => *b,
        _ => false,
    };

    let version = {
        let v = text("product_version");
        if v.is_empty() {
            doc.pointer("/product/product_version")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string()
        } else {
            v
        }
    };

    // Whether we know anything at all. `upgrade_packages` is present once a
    // check has run, and only then.
    let upgrades = count("upgrade_packages");
    let known = upgrades.is_some() || doc.get("last_check").is_some();
    let pending = upgrades.unwrap_or(0) + count("new_packages").unwrap_or(0);

    let major = text("upgrade_major_version");
    let major_msg = strip_html(&text("upgrade_major_message"));
    // OPNsense spells it out in the upgrade notice for a retired series.
    let eol = major_msg.to_lowercase().contains("end of life")
        || text("status_msg").to_lowercase().contains("end of life");
    let mut summary = format!(
        "{} {}\n{}",
        {
            let n = doc
                .pointer("/product/product_name")
                .and_then(|v| v.as_str())
                .unwrap_or("OPNsense");
            n.to_string()
        },
        version,
        text("os_version")
    );
    if known {
        summary.push_str(&format!(
            "\n{} package upgrade(s) pending{}\nchecked {}",
            pending,
            if text("download_size").is_empty() {
                String::new()
            } else {
                format!(", {} to download", text("download_size"))
            },
            text("last_check")
        ));
        if !major.is_empty() {
            summary.push_str(&format!("\nA major release is available: {major}."));
            if !major_msg.is_empty() {
                summary.push_str(&format!(" {major_msg}"));
            }
        }
        if eol {
            summary.push_str(
                "\nThis release is end of life: it will receive no further updates, which is \
                 why nothing is pending.",
            );
        }
        for (field, what) in [("connection", "mirror connection"), ("repository", "repository")] {
            let v = text(field);
            if !v.is_empty() && v != "ok" {
                summary.push_str(&format!("\n{what}: {v}"));
            }
        }
    } else {
        summary.push_str(
            "\nThis firewall has not checked for updates, so the number pending is unknown. \
             Set `check_after_hours` on the device, or enable the firmware check on the box.",
        );
    }

    Ok((
        (!version.is_empty()).then_some(version),
        summary,
        Extra {
            updates: known.then_some(pending),
            // `needs_reboot` is the box wanting one now; the `upgrade_*` and
            // `status_reboot` flags are about what applying the update would
            // need, which is not the same question.
            reboot_required: flag("needs_reboot"),
            eol,
            eol_note: if eol && !major.is_empty() {
                format!("{major} is the supported series; this one is retired")
            } else {
                String::new()
            },
        },
    ))
}

/// Everything worth knowing from an Unraid server, in one round trip.
///
/// The API refuses introspection in production, so these fields were
/// established against a real server rather than read off a schema. `vars`
/// carries the precise version; `info.os.release` is rounded to the minor.
const UNRAID_QUERY: &str = "{ online \
    vars { version regState } \
    server { name } \
    info { os { distro release kernel } } \
    array { state } \
    notifications { overview { unread { total info warning alert } } } }";

async fn unraid(spec: &DeviceSpec) -> Result<(Option<String>, String, Extra)> {
    let Probe::Unraid {
        api_key,
        insecure,
        check_releases,
    } = &spec.probe
    else {
        anyhow::bail!("not an unraid probe");
    };

    let base = if spec.target.starts_with("http") {
        spec.target.trim_end_matches('/').to_string()
    } else {
        format!("https://{}", spec.target.trim_end_matches('/'))
    };

    let client = reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .danger_accept_invalid_certs(*insecure)
        .user_agent(concat!("patchpanel-agent/", env!("CARGO_PKG_VERSION")))
        .build()?;

    let resp = client
        .post(format!("{base}/graphql"))
        .header("x-api-key", api_key)
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({ "query": UNRAID_QUERY }))
        .send()
        .await
        .context("asking unraid over graphql")?;

    let code = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if code == reqwest::StatusCode::UNAUTHORIZED || code == reqwest::StatusCode::FORBIDDEN {
        anyhow::bail!(
            "unraid rejected the API key ({}). It needs the VIEWER role with the INFO and \
             OS resources.",
            code.as_u16()
        );
    }
    let doc: serde_json::Value =
        serde_json::from_str(&body).context("unraid returned something that is not JSON")?;

    // A partial result is normal: a key without a resource gets an error for
    // that field and data for the rest. Only a missing version is fatal.
    if let Some(first) = doc
        .pointer("/errors/0/message")
        .and_then(|m| m.as_str())
        .filter(|_| doc.get("data").is_none())
    {
        anyhow::bail!("unraid: {first}");
    }

    let at = |ptr: &str| doc.pointer(ptr).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let num = |ptr: &str| doc.pointer(ptr).and_then(|v| v.as_i64()).unwrap_or(0);

    let version = at("/data/vars/version");
    if version.is_empty() {
        anyhow::bail!(
            "unraid answered but reported no version. The key may not carry the OS resource."
        );
    }

    let mut summary = format!(
        "{} {}\n{} on {}",
        at("/data/info/os/distro"),
        version,
        at("/data/info/os/kernel"),
        at("/data/server/name")
    );
    let array = at("/data/array/state");
    if !array.is_empty() {
        summary.push_str(&format!("\narray: {array}"));
    }
    let (unread, alerts, warns) = (
        num("/data/notifications/overview/unread/total"),
        num("/data/notifications/overview/unread/alert"),
        num("/data/notifications/overview/unread/warning"),
    );
    if unread > 0 {
        summary.push_str(&format!(
            "\n{unread} unread notification(s): {alerts} alert(s), {warns} warning(s)"
        ));
    }

    // Whether it is behind. Unraid does not report this itself, so the only
    // honest answers are "compare against the published list" or "unknown".
    let mut updates = None;
    if *check_releases {
        match latest_unraid(&client).await {
            Some(latest) => {
                let behind = newer_version(&latest, &version);
                summary.push_str(&format!(
                    "\nlatest published release: {latest}{}",
                    if behind { " - this server is behind" } else { " - up to date" }
                ));
                updates = Some(usize::from(behind));
            }
            None => summary.push_str(
                "\ncould not read Unraid's published release list, so whether this server is \
                 behind is unknown",
            ),
        }
    } else {
        summary.push_str(
            "\nUnraid does not report whether an update exists; set `check_releases` to \
             compare against its published releases.",
        );
    }

    Ok((
        Some(version),
        summary,
        Extra {
            updates,
            reboot_required: false,
            eol: false,
            eol_note: String::new(),
        },
    ))
}

/// Vendor notices arrive as HTML; the tags carry nothing worth keeping.
fn strip_html(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The newest stable release Unraid publishes. The feed is newest first.
async fn latest_unraid(client: &reqwest::Client) -> Option<String> {
    let feed: serde_json::Value = client
        .get("https://releases.unraid.net/json")
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    feed.as_array()?
        .iter()
        .filter_map(|e| e.get("version").and_then(|v| v.as_str()))
        // Skip anything that is not a plain release: rc and beta builds are
        // not what "behind" should mean for a NAS.
        .find(|v| v.chars().all(|c| c.is_ascii_digit() || c == '.'))
        .map(str::to_string)
}

/// Is `a` a newer version than `b`? Compares dotted numbers, ignoring any
/// suffix, so `7.3.2` beats `7.3.1` and `7.10.0` beats `7.9.9`.
fn newer_version(a: &str, b: &str) -> bool {
    let parts = |s: &str| -> Vec<u32> {
        s.split('.')
            .map(|p| {
                p.chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse()
                    .unwrap_or(0)
            })
            .collect()
    };
    let (x, y) = (parts(a), parts(b));
    for i in 0..x.len().max(y.len()) {
        let (l, r) = (x.get(i).copied().unwrap_or(0), y.get(i).copied().unwrap_or(0));
        if l != r {
            return l > r;
        }
    }
    false
}

/// Parse the date format FreeBSD's `date` prints, which is what OPNsense puts
/// in `last_check`: `Mon Sep  7 23:01:19 PDT 2026`.
///
/// chrono cannot map an alphabetic zone to an offset, so the common ones are
/// listed. An unknown zone is read as UTC, which is wrong by at most half a
/// day - harmless for a threshold measured in hours, and it errs towards
/// checking sooner rather than never.
fn parse_bsd_date(s: &str) -> Option<DateTime<Utc>> {
    const ZONES: &[(&str, i32)] = &[
        ("UTC", 0), ("GMT", 0), ("Z", 0),
        ("BST", 1), ("CET", 1), ("CEST", 2), ("EET", 2), ("EEST", 3),
        ("EST", -5), ("EDT", -4), ("CST", -6), ("CDT", -5),
        ("MST", -7), ("MDT", -6), ("PST", -8), ("PDT", -7),
        ("AKST", -9), ("AKDT", -8), ("HST", -10),
        ("JST", 9), ("AEST", 10), ("AEDT", 11), ("NZST", 12),
    ];

    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() < 5 {
        return None;
    }
    // `Mon Sep 7 23:01:19 PDT 2026` - the zone is the second-to-last field
    // when a year follows it, and the year is always last.
    let year = parts.last()?;
    let zone = parts[parts.len() - 2];
    let offset = ZONES
        .iter()
        .find(|(name, _)| *name == zone)
        .map(|(_, h)| *h)
        .unwrap_or(0);

    let rebuilt = format!(
        "{} {} {} {} {}",
        parts[0],
        parts[1],
        parts[2],
        parts[3],
        year
    );
    let naive = chrono::NaiveDateTime::parse_from_str(&rebuilt, "%a %b %e %H:%M:%S %Y").ok()?;
    Some(DateTime::from_naive_utc_and_offset(
        naive - chrono::Duration::hours(i64::from(offset)),
        Utc,
    ))
}

/// Devices report versions inconsistently ("v2.10.1", "2.10.1-rel"), so a
/// containment check beats equality without being as loose as a substring
/// match on the whole banner.
fn version_matches(want: &str, got: &str) -> bool {
    let norm = |s: &str| s.trim().trim_start_matches(['v', 'V']).to_ascii_lowercase();
    let (w, g) = (norm(want), norm(got));
    g == w || g.starts_with(&format!("{w}-")) || g.starts_with(&format!("{w}."))
}

/// What a device told us beyond its version.
#[derive(Default)]
pub struct Extra {
    pub updates: Option<usize>,
    pub reboot_required: bool,
    pub eol: bool,
    pub eol_note: String,
}

/// Returns `(extracted version, raw evidence, anything else the device said)`.
///
/// Most probes have nothing to add beyond a version, so they return the
/// default; asking the device twice to collect the rest would mean a second
/// round trip and, for OPNsense, a second request to go and check its mirrors.
async fn run_probe(spec: &DeviceSpec) -> Result<(Option<String>, String, Extra)> {
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
            Ok((version, body, Extra::default()))
        }

        Probe::Opnsense { .. } => opnsense(spec).await,

        Probe::Unraid { .. } => unraid(spec).await,

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
            Ok((version, text, Extra::default()))
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
                return Ok((None, format!("tcp/{port} open"), Extra::default()));
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
            Ok((version, banner, Extra::default()))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A NAS one point release behind should say so, and one that is current
    /// should not - including across a two-digit minor, where a string
    /// comparison quietly gets it backwards.
    #[test]
    fn compares_versions_numerically() {
        assert!(newer_version("7.3.2", "7.3.1"));
        assert!(!newer_version("7.3.1", "7.3.2"));
        assert!(!newer_version("7.3.1", "7.3.1"));
        assert!(newer_version("7.10.0", "7.9.9"), "10 is not less than 9");
        assert!(newer_version("8.0", "7.3.2"));
        // A suffix is not part of the comparison.
        assert!(!newer_version("7.3.1-rc1", "7.3.1"));
    }

    #[test]
    fn reads_the_freebsd_date_opnsense_reports() {
        let t = parse_bsd_date("Mon Sep  7 23:01:19 PDT 2026").expect("parses");
        assert_eq!(t.format("%Y-%m-%d %H:%M").to_string(), "2026-09-08 06:01");
        // An unknown zone is read as UTC rather than rejected.
        assert!(parse_bsd_date("Mon Sep  7 23:01:19 XYZ 2026").is_some());
        assert!(parse_bsd_date("not a date").is_none());
    }
}
