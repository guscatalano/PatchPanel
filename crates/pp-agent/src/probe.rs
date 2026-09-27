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
use pp_proto::{
    DeviceReport, DeviceSpec, DiscoveredHost, DiscoveryScan, Probe, Scanner, SnmpVersion,
    OID_SYS_DESCR,
};
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
        // Port 0 because only the address is wanted. `resolve` already
        // handles a bare address, so this does no lookup for a target that is
        // one, and it is the same resolution the probe itself goes on to use.
        resolved_ip: resolve(&spec.target, 0)
            .await
            .ok()
            .map(|a| a.ip().to_string()),
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

    // If the operator said where the current release is published, go and
    // find out. Only for probes that do not already answer it themselves -
    // a firewall counting its own pending packages knows better than a
    // version string comparison does.
    if report.reachable && !spec.latest_url.is_empty() && !report.updates_known {
        if let Some(installed) = report.firmware.clone() {
            match latest_version(spec).await {
                Ok(latest) => {
                    let behind = newer_version(&latest, &installed);
                    report.updates = usize::from(behind);
                    report.updates_known = true;
                    report.detail = format!(
                        "{}\nlatest published: {latest}{}",
                        report.detail.trim_end(),
                        if behind { " - this is behind" } else { " - up to date" }
                    );
                }
                Err(e) => {
                    // Unknown, not current: an unreachable release feed says
                    // nothing about the device.
                    report.detail = format!(
                        "{}\ncould not read the published version: {e:#}",
                        report.detail.trim_end()
                    );
                }
            }
        }
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
    notifications { overview { unread { total info warning alert } } \
      list(filter: { type: UNREAD, offset: 0, limit: 10 }) { title subject importance } } }";

/// Ask Home Assistant what it is running, and install what it is allowed to.
///
/// One request answers everything: `/api/states` carries an `update.*` entity
/// per component, with what is installed, what is available, and whether HA
/// itself is set to auto-update it. `device_class` separates HA's own software
/// from the firmware of the devices it manages, which is the line that decides
/// what PatchPanel will touch.
async fn home_assistant(spec: &DeviceSpec) -> Result<(Option<String>, String, Extra)> {
    let Probe::HomeAssistant {
        token,
        insecure,
        auto_update,
    } = &spec.probe
    else {
        anyhow::bail!("not a home assistant probe");
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
        .get(format!("{base}/api/states"))
        .bearer_auth(token)
        .send()
        .await
        .context("asking home assistant for its state")?;
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        anyhow::bail!("home assistant rejected the token (401)");
    }
    let states: serde_json::Value = resp.error_for_status()?.json().await?;

    struct Pending {
        entity: String,
        title: String,
        installed: String,
        latest: String,
        firmware: bool,
    }

    let mut version = String::new();
    let mut pending: Vec<Pending> = Vec::new();
    let mut total = 0usize;
    let mut ha_auto = 0usize;

    for e in states.as_array().map(Vec::as_slice).unwrap_or_default() {
        let entity = e
            .get("entity_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if !entity.starts_with("update.") {
            continue;
        }
        total += 1;
        let attr = |k: &str| {
            e.pointer(&format!("/attributes/{k}"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        if entity == "update.home_assistant_core_update" {
            version = attr("installed_version");
        }
        if e.pointer("/attributes/auto_update")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            ha_auto += 1;
        }
        if e.get("state").and_then(|v| v.as_str()) != Some("on") {
            continue;
        }
        pending.push(Pending {
            entity: entity.to_string(),
            title: attr("friendly_name").max(attr("title")),
            installed: attr("installed_version"),
            latest: attr("latest_version"),
            firmware: attr("device_class") == "firmware",
        });
    }

    let (fw, sw): (Vec<&Pending>, Vec<&Pending>) = pending.iter().partition(|p| p.firmware);

    let mut summary = format!(
        "Home Assistant {version}\n{total} component(s) tracked, {} with an update \
         ({} software, {} device firmware)\n{ha_auto} set to auto-update by Home Assistant itself",
        pending.len(),
        sw.len(),
        fw.len()
    );
    for p in pending.iter().take(12) {
        summary.push_str(&format!(
            "\n  {} {} -> {}{}",
            if p.title.is_empty() { &p.entity } else { &p.title },
            p.installed,
            p.latest,
            if p.firmware { "  (firmware)" } else { "" }
        ));
    }

    // The only place in PatchPanel where looking at something can change it,
    // so it is opt-in, scoped, and says exactly what it did.
    let wanted: Vec<&&Pending> = match auto_update {
        pp_proto::HaAutoUpdate::Off => Vec::new(),
        pp_proto::HaAutoUpdate::Software => sw.iter().collect(),
        pp_proto::HaAutoUpdate::Everything => sw.iter().chain(fw.iter()).collect(),
    };
    if !wanted.is_empty() {
        summary.push_str(&format!("\n\ninstalling {} update(s):", wanted.len()));
        for p in wanted {
            let r = client
                .post(format!("{base}/api/services/update/install"))
                .bearer_auth(token)
                .json(&serde_json::json!({ "entity_id": p.entity }))
                .send()
                .await;
            let outcome = match r {
                Ok(resp) if resp.status().is_success() => "started".to_string(),
                Ok(resp) => format!("refused ({})", resp.status().as_u16()),
                Err(e) => format!("failed ({e})"),
            };
            summary.push_str(&format!("\n  {}: {outcome}", p.entity));
        }
        summary.push_str(
            "\n\nHome Assistant takes its own backup before updating itself, and applies \
             the result on its own schedule.",
        );
    }

    Ok((
        (!version.is_empty()).then_some(version),
        summary,
        Extra {
            updates: Some(pending.len()),
            reboot_required: false,
            eol: false,
            eol_note: String::new(),
        },
    ))
}

/// The version the vendor currently publishes for this device.
///
/// Most appliances report what they are running and say nothing about whether
/// that is current. Given a feed, the comparison can be made here rather than
/// by a person who has to go and look it up.
async fn latest_version(spec: &DeviceSpec) -> Result<String> {
    let insecure = match &spec.probe {
        Probe::Http { insecure, .. }
        | Probe::Opnsense { insecure, .. }
        | Probe::Unraid { insecure, .. } => *insecure,
        _ => false,
    };
    let client = reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .danger_accept_invalid_certs(insecure)
        .user_agent(concat!("patchpanel-agent/", env!("CARGO_PKG_VERSION")))
        .build()?;

    let body = client
        .get(&spec.latest_url)
        .send()
        .await
        .context("fetching the published version")?
        .error_for_status()?
        .text()
        .await?;

    let found = if let Some(pointer) = &spec.latest_pointer {
        let doc: serde_json::Value = serde_json::from_str(&body)?;
        doc.pointer(pointer)
            .map(|v| match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .ok_or_else(|| anyhow::anyhow!("nothing at {pointer}"))?
    } else if let Some(re) = &spec.latest_regex {
        capture(re, &body)?.ok_or_else(|| anyhow::anyhow!("the pattern matched nothing"))?
    } else {
        body.trim().to_string()
    };

    let version = found.trim().trim_start_matches(['v', 'V']).to_string();
    if version.is_empty() {
        anyhow::bail!("no version in the response");
    }
    Ok(version)
}

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
        // The counts alone say something is wrong without saying what, which
        // is the least useful shape a warning can take. Unraid's own notify
        // command is how scripts on the box report failures - the User Scripts
        // plugin is invisible to this API, but anything it runs that calls
        // notify is not - so the titles are the only place a failed backup
        // script is nameable from here.
        if let Some(list) = doc
            .pointer("/data/notifications/list")
            .and_then(|v| v.as_array())
        {
            for n in list.iter().take(5) {
                let title = n.get("title").and_then(|v| v.as_str()).unwrap_or("");
                let subject = n.get("subject").and_then(|v| v.as_str()).unwrap_or("");
                let importance = n
                    .get("importance")
                    .and_then(|v| v.as_str())
                    .unwrap_or("INFO");
                if title.is_empty() && subject.is_empty() {
                    continue;
                }
                summary.push_str(&format!(
                    "\n  [{}] {title}{}",
                    importance.to_lowercase(),
                    if subject.is_empty() || subject == title {
                        String::new()
                    } else {
                        format!(" - {subject}")
                    }
                ));
            }
        }
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

        Probe::HomeAssistant { .. } => home_assistant(spec).await,

        Probe::Snmp {
            community,
            oid,
            version_regex,
            version,
            user,
            auth_password,
            auth_protocol,
            privacy_password,
            privacy_cipher,
        } => {
            let oid_str = oid.as_deref().unwrap_or(OID_SYS_DESCR);
            let addr = resolve(&spec.target, 161).await?;

            let attempts = snmp_attempts(*version, user.is_some());

            let mut last_err = None;
            let mut spoke = SnmpVersion::V2c;
            let mut text = None;
            for attempt in &attempts {
                let got = match attempt {
                    SnmpVersion::V3 => {
                        snmp_v3(
                            addr,
                            oid_str,
                            user.as_deref().unwrap_or_default(),
                            auth_password.as_deref().unwrap_or_default(),
                            *auth_protocol,
                            privacy_password.as_deref(),
                            *privacy_cipher,
                        )
                        .await
                    }
                    _ => snmp_community(addr, oid_str, community).await,
                };
                match got {
                    Ok(v) => {
                        spoke = *attempt;
                        text = Some(v);
                        break;
                    }
                    // Kept rather than returned, so a failed v3 attempt does not
                    // hide what v2c went on to say - or vice versa.
                    Err(e) => last_err = Some((*attempt, e)),
                }
            }

            let text = match text {
                Some(t) => t,
                None => {
                    let (which, e) = last_err.expect("at least one attempt");
                    return Err(e.context(format!("SNMP {} failed", snmp_label(which))));
                }
            };

            let version_found = match version_regex {
                Some(re) => capture(re, &text)?,
                // Without a pattern, sysDescr itself is the best evidence we
                // have; the operator can add a regex to sharpen it.
                None => Some(text.clone()),
            };
            // Which version answered belongs in the evidence, not just in a log
            // line: it is the difference between v3 being enforced and v3 being
            // configured while the device happily answers a community string.
            //
            // And when something better was tried first and failed, why it failed
            // goes here too. Reporting the fallback without the reason leaves the
            // operator knowing v3 did not work and with no way to tell a wrong
            // password from a device that never had v3 enabled - which is the only
            // question they actually want answered at that point.
            let mut detail = format!("via SNMP {}: {text}", snmp_label(spoke));
            if let Some((tried, why)) = last_err {
                if tried != spoke {
                    detail.push_str(&format!(
                        " ({} was tried first and failed: {})",
                        snmp_label(tried),
                        format!("{why:#}").replace('\n', " ")
                    ));
                }
            }
            Ok((version_found, detail, Extra::default()))
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

/// Which SNMP versions to try, in order.
///
/// `auto` prefers v3 and falls back to v2c, but only when there is a user to
/// authenticate as - without one, v3 cannot be attempted at all, and going through
/// the motions would turn a clear "no credentials configured" into a UDP timeout.
///
/// A pinned version is tried alone and never falls back. That is the whole point of
/// pinning: once v1/v2c is switched off on the device, an operator wants a device
/// that has quietly gone back to answering a community string to read as
/// unreachable, not as working.
fn snmp_attempts(version: SnmpVersion, has_user: bool) -> Vec<SnmpVersion> {
    match version {
        SnmpVersion::Auto if has_user => vec![SnmpVersion::V3, SnmpVersion::V2c],
        SnmpVersion::Auto => vec![SnmpVersion::V2c],
        pinned => vec![pinned],
    }
}

fn snmp_label(v: SnmpVersion) -> &'static str {
    match v {
        SnmpVersion::V3 => "v3",
        SnmpVersion::V2c | SnmpVersion::Auto => "v2c",
        SnmpVersion::V1 => "v1",
    }
}

/// Read one OID with a community string, the v1/v2c way.
async fn snmp_community(
    addr: std::net::SocketAddr,
    oid: &str,
    community: &str,
) -> Result<String> {
    let client = csnmp::Snmp2cClient::new(
        addr,
        community.as_bytes().to_vec(),
        None,
        Some(PROBE_TIMEOUT),
        1,
    )
    .await
    .context("creating SNMP client")?;

    let parsed = csnmp::ObjectIdentifier::from_str(oid)
        .map_err(|e| anyhow::anyhow!("bad OID `{oid}`: {e:?}"))?;
    let value = client
        .get(parsed)
        .await
        .map_err(|e| anyhow::anyhow!("SNMP get failed: {e}"))?;

    Ok(match value {
        csnmp::ObjectValue::String(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        csnmp::ObjectValue::ObjectId(o) => o.to_string(),
        csnmp::ObjectValue::Integer(i) => i.to_string(),
        csnmp::ObjectValue::Counter32(n) | csnmp::ObjectValue::Unsigned32(n) => n.to_string(),
        csnmp::ObjectValue::TimeTicks(n) => n.to_string(),
        csnmp::ObjectValue::Counter64(n) => n.to_string(),
        csnmp::ObjectValue::IpAddress(a) => a.to_string(),
        csnmp::ObjectValue::Opaque(b) => hex::encode(b),
    })
}

/// Read one OID as a v3 user.
///
/// Two things here are not optional and are easy to leave out. `init` performs
/// engine discovery - a v3 request cannot be signed without the device's engine id,
/// boots and time - and every step is wrapped in the same timeout as the rest of
/// the probes, because an unreachable device on a UDP protocol otherwise waits on
/// the library's own patience rather than ours.
async fn snmp_v3(
    addr: std::net::SocketAddr,
    oid: &str,
    user: &str,
    auth_password: &str,
    auth: pp_proto::SnmpAuth,
    privacy_password: Option<&str>,
    cipher: pp_proto::SnmpCipher,
) -> Result<String> {
    use snmp2::{v3, AsyncSession, Oid};

    if user.is_empty() {
        anyhow::bail!("no v3 user configured");
    }

    let mut security = v3::Security::new(user.as_bytes(), auth_password.as_bytes())
        .with_auth_protocol(match auth {
            pp_proto::SnmpAuth::Md5 => v3::AuthProtocol::Md5,
            pp_proto::SnmpAuth::Sha1 => v3::AuthProtocol::Sha1,
            pp_proto::SnmpAuth::Sha224 => v3::AuthProtocol::Sha224,
            pp_proto::SnmpAuth::Sha256 => v3::AuthProtocol::Sha256,
            pp_proto::SnmpAuth::Sha384 => v3::AuthProtocol::Sha384,
            pp_proto::SnmpAuth::Sha512 => v3::AuthProtocol::Sha512,
        });
    // A privacy password is what separates authPriv from authNoPriv. Without one
    // the request is still signed, which is the part that replaces the community
    // string; the reply simply is not encrypted.
    security = match privacy_password {
        Some(p) if !p.is_empty() => security.with_auth(v3::Auth::AuthPriv {
            cipher: match cipher {
                pp_proto::SnmpCipher::Des => v3::Cipher::Des,
                pp_proto::SnmpCipher::Aes128 => v3::Cipher::Aes128,
                pp_proto::SnmpCipher::Aes192 => v3::Cipher::Aes192,
                pp_proto::SnmpCipher::Aes256 => v3::Cipher::Aes256,
            },
            privacy_password: p.as_bytes().to_vec(),
        }),
        _ => security.with_auth(v3::Auth::AuthNoPriv),
    };

    let parsed = Oid::from(
        &oid.split('.')
            .map(|p| p.parse::<u64>().map_err(|e| anyhow::anyhow!("bad OID `{oid}`: {e}")))
            .collect::<Result<Vec<u64>>>()?,
    )
    .map_err(|e| anyhow::anyhow!("bad OID `{oid}`: {e:?}"))?
    .to_owned();

    let mut session = tokio::time::timeout(PROBE_TIMEOUT, AsyncSession::new_v3(addr, 1, security))
        .await
        .map_err(|_| anyhow::anyhow!("connecting to {addr} for SNMPv3 timed out"))?
        .with_context(|| format!("opening an SNMPv3 session to {addr}"))?;

    tokio::time::timeout(PROBE_TIMEOUT, session.init())
        .await
        .map_err(|_| anyhow::anyhow!("SNMPv3 engine discovery timed out"))?
        .map_err(|e| anyhow::anyhow!("SNMPv3 engine discovery failed: {e:?}"))?;

    let pdu = tokio::time::timeout(PROBE_TIMEOUT, session.get(&parsed))
        .await
        .map_err(|_| anyhow::anyhow!("SNMPv3 get timed out"))?
        .map_err(|e| anyhow::anyhow!("SNMPv3 get failed: {e:?}"))?;

    for (_, value) in pdu.varbinds {
        return Ok(match value {
            snmp2::Value::OctetString(b) => String::from_utf8_lossy(b).into_owned(),
            snmp2::Value::ObjectIdentifier(o) => format!("{o:?}"),
            snmp2::Value::Integer(i) => i.to_string(),
            snmp2::Value::Counter32(n) | snmp2::Value::Unsigned32(n) | snmp2::Value::Timeticks(n) => {
                n.to_string()
            }
            snmp2::Value::Counter64(n) => n.to_string(),
            snmp2::Value::IpAddress(a) => {
                format!("{}.{}.{}.{}", a[0], a[1], a[2], a[3])
            }
            // A v3 device that authenticated but does not have the OID is a
            // configuration answer, not a transport failure.
            other => anyhow::bail!("SNMPv3 returned {other:?} for {oid}"),
        });
    }
    anyhow::bail!("SNMPv3 response for {oid} had no values in it")
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
pub async fn discover(
    scans: &[DiscoveryScan],
    known: &[DeviceSpec],
    light: &[String],
) -> Vec<DiscoveredHost> {
    let mut out = Vec::new();
    for scan in scans {
        match run_scan(scan, known, light).await {
            Ok(hosts) => out.extend(hosts),
            Err(e) => tracing::warn!(cidr = %scan.cidr, error = %e, "discovery sweep failed"),
        }
    }
    out
}

/// One range, by whichever scanner is actually available.
///
/// A range asking for nmap on a collector that has none is not a failed sweep -
/// the built-in one still finds the hosts - but it is a different answer to the
/// question, so every row it produces carries why. The alternative is a Network
/// tab where "no services identified" and "nothing was looking" render
/// identically.
async fn run_scan(
    scan: &DiscoveryScan,
    known: &[DeviceSpec],
    light: &[String],
) -> Result<Vec<DiscoveredHost>> {
    let hosts = range(&scan.cidr)?;

    let mut note = String::new();
    let mut found = if scan.use_nmap {
        // A range that asked for nmap gets nmap. Fetching it is implementing
        // that request rather than making a decision of its own - the same call
        // `install_prerequisites` makes for fwupd, and for the same reason: a
        // tool that is missing should not quietly downgrade the answer.
        #[cfg(target_os = "linux")]
        if crate::platform::linux::which_nmap().is_none() {
            let p = crate::exec::Progress::detached();
            match crate::platform::linux::install_nmap(&p).await {
                Ok(msg) => tracing::info!(%msg, "installed nmap for discovery"),
                Err(e) => tracing::warn!(
                    error = %format!("{e:#}"),
                    "could not install nmap; the built-in sweep will be used"
                ),
            }
        }
        match crate::nmap::sweep(scan, light).await {
            Ok(found) => found,
            Err(e) => {
                tracing::warn!(
                    cidr = %scan.cidr,
                    error = %format!("{e:#}"),
                    "nmap discovery failed; falling back to the built-in sweep"
                );
                note = format!(
                    "nmap was asked for and did not run ({e:#}), so these ports come from the \
                     built-in TCP sweep and nothing here is service-identified"
                );
                sweep(&hosts, &scan.ports).await
            }
        }
    } else {
        sweep(&hosts, &scan.ports).await
    };

    for host in &mut found {
        // Only the manifest this agent was handed, and deliberately: the portal
        // is the only place that knows the whole fleet, so this answers "did I
        // probe it" and the portal answers "is it accounted for".
        host.unmanaged = !known
            .iter()
            .any(|d| d.target == host.ip || d.target.starts_with(&format!("{}:", host.ip)));
        host.scan_note = note.clone();
    }
    Ok(found)
}

/// The addresses a range covers, refusing anything absurd.
///
/// Checked before either scanner runs: nmap will happily accept a /8 and spend
/// a week on it.
fn range(cidr: &str) -> Result<Vec<IpAddr>> {
    let net: IpNet = cidr.parse().with_context(|| format!("bad CIDR `{cidr}`"))?;
    let hosts: Vec<IpAddr> = net.hosts().take(MAX_SCAN_HOSTS + 1).collect();
    if hosts.len() > MAX_SCAN_HOSTS {
        anyhow::bail!("`{cidr}` covers more than {MAX_SCAN_HOSTS} hosts; narrow the range");
    }
    Ok(hosts)
}

async fn sweep(hosts: &[IpAddr], ports: &[u16]) -> Vec<DiscoveredHost> {
    let sem = Arc::new(Semaphore::new(SCAN_CONCURRENCY));
    let ports = Arc::new(ports.to_vec());
    let mut tasks = Vec::new();

    for ip in hosts {
        let ip = *ip;
        let sem = sem.clone();
        let ports = ports.clone();
        tasks.push(tokio::spawn(async move {
            let _permit = sem.acquire().await;
            scan_host(ip, &ports).await
        }));
    }

    let mut found = Vec::new();
    for t in tasks {
        if let Ok(Some(host)) = t.await {
            found.push(host);
        }
    }
    found
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
        // The built-in sweep learns none of this: it opens a socket and reads
        // what comes back. Empty is the honest answer, not a gap to fill.
        identity: Default::default(),
        ip: ip.to_string(),
        open_ports: open,
        // The built-in sweep connects, and there is no such thing as connecting
        // to a UDP port.
        open_udp: Vec::new(),
        hint,
        unmanaged: true,
        // A connect and a banner read establish nothing per port beyond "it
        // answered", which `open_ports` already says.
        services: Vec::new(),
        scanner: Scanner::Tcp,
        scan_note: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The order matters more than the set, and pinning must not fall back.
    #[test]
    fn snmp_version_order() {
        use pp_proto::SnmpVersion::*;

        // The useful default: v3 where it is possible, v2c where it is not.
        assert_eq!(snmp_attempts(Auto, true), vec![V3, V2c]);
        assert_eq!(snmp_attempts(Auto, false), vec![V2c]);

        // Pinned means pinned. A device that has had v1/v2c turned off and starts
        // answering a community string again should read as unreachable rather
        // than quietly working, which is the entire reason to pin.
        assert_eq!(snmp_attempts(V3, true), vec![V3]);
        assert_eq!(snmp_attempts(V2c, true), vec![V2c]);
        assert_eq!(snmp_attempts(V1, true), vec![V1]);

        // Pinning v3 without a user is a configuration error rather than a
        // fallback: it is attempted and fails saying so, instead of silently
        // becoming v2c.
        assert_eq!(snmp_attempts(V3, false), vec![V3]);
    }

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
