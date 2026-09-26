//! The tool table, and what each one answers.
//!
//! Every tool here is a question. The portal has already decided what the
//! answers mean - which counts are trustworthy, what needs a person, whether a
//! pool has a machine in hand - and these pass those judgements through rather
//! than re-deriving them, so a caller cannot get a different answer here than
//! the dashboard gives.

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::render::*;
use crate::Portal;

pub const INSTRUCTIONS: &str = "\
PatchPanel manages patching for a mixed fleet: Linux and Windows machines with an \
agent, plus appliances probed over their own APIs.

Its central rule is that a number is never reported as trustworthy unless it is. \
When a machine reads `not scanned`, its real update count is unknown - it is not \
zero, and it must not be summarised as clean. Counts also separate what can be \
installed now from what is phased (the archive is withholding it), held back \
(needs a full upgrade), blocked (a run tried and the version did not move), and \
ignored (somebody decided against that version).

Start with patchpanel_overview. It returns the portal's own list of what needs a \
person, which deliberately excludes anything a schedule will resolve - so a large \
pending count with an empty list means the pools are working, not that something \
is hidden.";

pub fn catalogue(allow_actions: bool) -> Vec<Value> {
    let mut tools = vec![
        json!({
            "name": "patchpanel_overview",
            "description": "What needs a person across the whole fleet, plus a one-line summary. \
                The list excludes anything a pool will resolve on schedule, so it is short by \
                design. Start here.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "patchpanel_machines",
            "description": "Every machine with an agent: OS, pool, update counts and any \
                problems. Optionally filtered by a substring of the hostname, OS or pool.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "filter": { "type": "string", "description": "substring to match" }
                }
            }
        }),
        json!({
            "name": "patchpanel_machine",
            "description": "One machine in detail: pending updates by name, apt sources, \
                virtual machines it hosts, firmware, and why it last rebooted.",
            "inputSchema": {
                "type": "object",
                "properties": { "hostname": { "type": "string" } },
                "required": ["hostname"]
            }
        }),
        json!({
            "name": "patchpanel_appliances",
            "description": "Devices with no agent - firewalls, NAS, Home Assistant - with their \
                firmware version, pending updates and whether the release is end of life.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "patchpanel_backups",
            "description": "Virtual machines and whether they have a recent backup, including \
                ones deliberately held to a longer schedule or not tracked at all.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "patchpanel_schedule",
            "description": "Patching pools, what each would do if it ran now, and when it next \
                fires.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "patchpanel_jobs",
            "description": "Work PatchPanel does not run itself, reported by whatever does - \
                backup scripts, dynamic DNS - with whether each is on time and what values it \
                last reported.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
    ];

    if allow_actions {
        tools.push(json!({
            "name": "patchpanel_rescan",
            "description": "Ask a machine to re-read its installed packages and available \
                updates. Changes nothing on the machine.",
            "inputSchema": {
                "type": "object",
                "properties": { "hostname": { "type": "string" } },
                "required": ["hostname"]
            }
        }));
        tools.push(json!({
            "name": "patchpanel_install_updates",
            "description": "Install a machine's pending OS updates now. THIS CHANGES THE \
                MACHINE and may require a reboot afterwards. Confirm with the person who asked \
                before calling it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "hostname": { "type": "string" },
                    "security_only": { "type": "boolean" }
                },
                "required": ["hostname"]
            }
        }));
    }
    tools
}

/// Find one agent by hostname, case-insensitively.
async fn agent_id(portal: &Portal, hostname: &str) -> Result<(String, Value)> {
    let fleet = portal.get("/api/fleet").await?;
    let want = hostname.trim().to_lowercase();
    let found = arr(&fleet, "agents").into_iter().find(|a| {
        let h = s(a, "hostname").to_lowercase();
        h == want || h.split('.').next() == Some(want.as_str())
    });
    let a = found.with_context(|| {
        let names: Vec<String> = arr(&fleet, "agents")
            .iter()
            .map(|a| s(a, "hostname"))
            .collect();
        format!("no machine called {hostname}. Known: {}", names.join(", "))
    })?;
    Ok((s(&a, "id"), a))
}

pub async fn call(portal: &Portal, name: &str, args: Value) -> Result<String> {
    match name {
        "patchpanel_overview" => {
            let fleet = portal.get("/api/fleet").await?;
            Ok(format!("{}\n\n{}", gist(&fleet), attention(&fleet)))
        }

        "patchpanel_machines" => {
            let fleet = portal.get("/api/fleet").await?;
            let filter = s(&args, "filter").to_lowercase();
            let mut lines = Vec::new();
            for m in arr(&fleet, "agents") {
                if !filter.is_empty() {
                    let hay = format!(
                        "{} {} {}",
                        s(&m, "hostname"),
                        s(&m, "os_version"),
                        s(&m, "pool")
                    )
                    .to_lowercase();
                    if !hay.contains(&filter) {
                        continue;
                    }
                }
                lines.push(machine_line(&m));
            }
            if lines.is_empty() {
                return Ok(format!("No machine matches {filter:?}."));
            }
            Ok(format!("{}\n{}", gist(&fleet), lines.join("\n")))
        }

        "patchpanel_machine" => {
            let hostname = s(&args, "hostname");
            let (id, row) = agent_id(portal, &hostname).await?;
            let d = portal.get(&format!("/api/agents/{id}")).await?;
            let inv = d.get("inventory").cloned().unwrap_or(Value::Null);
            let mut out = vec![machine_line(&row)];

            if n(&row, "scan_issue_count") > 0 {
                out.push("\nNOT FULLY SCANNED - the update count below is a floor, not a total:".into());
                for i in arr(&inv, "scan_issues") {
                    out.push(format!("  {} - {}", s(&i, "backend"), s(&i, "detail")));
                }
            }

            let updates = arr(&inv, "updates");
            if !updates.is_empty() {
                out.push(format!("\nPending ({}):", updates.len()));
                for u in updates.iter().take(60) {
                    out.push(format!(
                        "  {} {} -> {}{}",
                        s(u, "name"),
                        s(u, "current_version"),
                        s(u, "new_version"),
                        if b(u, "security") { " [security]" } else { "" }
                    ));
                }
                if updates.len() > 60 {
                    out.push(format!("  ... and {} more", updates.len() - 60));
                }
            }
            for (label, key) in [
                ("Held back (needs a full upgrade)", "held_back"),
                ("Phased - nothing will install these", "deferred"),
                ("Blocked - a run tried and nothing moved", "blocked"),
            ] {
                let list = arr(&inv, key);
                if !list.is_empty() {
                    out.push(format!(
                        "\n{label}: {}",
                        list.iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
            }
            let guests = arr(inv.get("virt").unwrap_or(&Value::Null), "guests");
            if !guests.is_empty() {
                out.push(format!("\nHosts {} virtual machine(s):", guests.len()));
                for g in guests {
                    out.push(format!(
                        "  {} ({}) {}",
                        s(&g, "name"),
                        s(&g, "kind"),
                        s(&g, "state")
                    ));
                }
            }
            if let Some(boot) = inv.get("boot_report").filter(|v| !v.is_null()) {
                out.push(format!(
                    "\nLast restart: {} - {}",
                    s(boot, "kind"),
                    s(boot, "detail")
                ));
            }
            Ok(out.join("\n"))
        }

        "patchpanel_appliances" => {
            let d = portal.get("/api/devices").await?;
            let mut lines = Vec::new();
            for x in arr(&d, "devices") {
                let label = {
                    let l = s(&x, "label");
                    if l.is_empty() { s(&x, "id") } else { l }
                };
                let state = if !b(&x, "probed") {
                    "never probed".to_string()
                } else if !b(&x, "reachable") {
                    let stale = if b(&x, "stale") {
                        " (values below are the last reading that worked)"
                    } else {
                        ""
                    };
                    format!("NOT ANSWERING{stale}")
                } else if b(&x, "eol") {
                    format!("END OF LIFE - {}", s(&x, "eol_note"))
                } else if !b(&x, "updates_known") {
                    "reachable, cannot report updates".into()
                } else if n(&x, "updates") > 0 {
                    format!("{} update(s) waiting", n(&x, "updates"))
                } else {
                    "up to date".into()
                };
                lines.push(format!(
                    "- {label} {} - {state}",
                    x.get("firmware")
                        .and_then(Value::as_str)
                        .unwrap_or("version unknown")
                ));
            }
            Ok(if lines.is_empty() {
                "No appliances are declared.".into()
            } else {
                lines.join("\n")
            })
        }

        "patchpanel_backups" => {
            let d = portal.get("/api/backups").await?;
            let sum = d.get("summary").cloned().unwrap_or(Value::Null);
            let mut lines = vec![format!(
                "{} guest(s): {} fresh, {} stale, {} never, {} not tracked. {} running with \
                 nothing recent to restore from.",
                n(&sum, "guests"),
                n(&sum, "fresh"),
                n(&sum, "stale"),
                n(&sum, "never"),
                n(&sum, "exempt"),
                n(&sum, "unprotected_running")
            )];
            for g in arr(&d, "guests") {
                let status = s(&g, "status");
                let when = g
                    .get("last_backup")
                    .and_then(Value::as_str)
                    .map(ago)
                    .unwrap_or_else(|| "never".into());
                let cadence = s(&g, "cadence");
                lines.push(format!(
                    "- {} on {} ({}) {} - last backup {when}{}",
                    s(&g, "name"),
                    s(&g, "host"),
                    s(&g, "state"),
                    status,
                    if cadence.is_empty() {
                        String::new()
                    } else {
                        format!(", wanted {cadence}")
                    }
                ));
            }
            Ok(lines.join("\n"))
        }

        "patchpanel_schedule" => {
            let pools = portal.get("/api/pools").await?;
            let mut lines = Vec::new();
            for p in pools.as_array().cloned().unwrap_or_default() {
                let sched = p.get("schedule").cloned().unwrap_or(Value::Null);
                let kind = s(&sched, "kind");
                let when = match kind.as_str() {
                    "manual" => "manual only".to_string(),
                    "daily" => format!("daily {:02}:{:02}", n(&sched, "hour"), n(&sched, "minute")),
                    "weekly" => format!(
                        "weekly, day {} at {:02}:{:02}",
                        n(&sched, "dow"),
                        n(&sched, "hour"),
                        n(&sched, "minute")
                    ),
                    other => other.to_string(),
                };
                lines.push(format!(
                    "- pool \"{}\": installs {}, reboot {}, {} machine(s), {when}{}",
                    s(&p, "name"),
                    s(&p, "scope"),
                    s(&p, "reboot"),
                    arr(&p, "members").len(),
                    p.get("next_run")
                        .and_then(Value::as_str)
                        .map(|t| format!(", next {t}"))
                        .unwrap_or_default()
                ));
                for x in arr(&p, "plan") {
                    lines.push(format!(
                        "    {} would {}: {}",
                        s(&x, "hostname"),
                        s(&x, "action"),
                        s(&x, "detail")
                    ));
                }
            }
            Ok(if lines.is_empty() {
                "No pools are defined, so nothing is patched on a schedule.".into()
            } else {
                lines.join("\n")
            })
        }

        "patchpanel_jobs" => {
            let jobs = portal.get("/api/jobs").await?;
            let mut lines = Vec::new();
            for j in jobs.as_array().cloned().unwrap_or_default() {
                let facts = j
                    .get("facts")
                    .and_then(Value::as_object)
                    .map(|m| {
                        m.iter()
                            .map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or("")))
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default();
                lines.push(format!(
                    "- {} [{}] last ran {}{}{}",
                    s(&j, "name"),
                    s(&j, "status"),
                    j.get("last_at")
                        .and_then(Value::as_str)
                        .map(ago)
                        .unwrap_or_else(|| "never".into()),
                    if s(&j, "last_detail").is_empty() {
                        String::new()
                    } else {
                        format!("\n    {}", s(&j, "last_detail"))
                    },
                    if facts.is_empty() {
                        String::new()
                    } else {
                        format!("\n    {facts}")
                    }
                ));
            }
            Ok(if lines.is_empty() {
                "No external jobs report to this portal.".into()
            } else {
                lines.join("\n")
            })
        }

        "patchpanel_rescan" | "patchpanel_install_updates" if !portal.allow_actions => {
            anyhow::bail!("this server is read-only; it was started without --allow-actions")
        }

        "patchpanel_rescan" => {
            let (id, _) = agent_id(portal, &s(&args, "hostname")).await?;
            portal
                .post(
                    &format!("/api/agents/{id}/commands"),
                    json!({ "kind": "collect_inventory" }),
                )
                .await?;
            Ok("Rescan dispatched. Ask again in a minute for the new counts.".into())
        }

        "patchpanel_install_updates" => {
            let hostname = s(&args, "hostname");
            let (id, row) = agent_id(portal, &hostname).await?;
            if n(&row, "actionable_count") == 0 {
                anyhow::bail!(
                    "{hostname} has nothing installable pending, so this would do nothing. \
                     Its pending count is {}.",
                    n(&row, "update_count")
                );
            }
            let security_only = b(&args, "security_only");
            portal
                .post(
                    &format!("/api/agents/{id}/commands"),
                    json!({ "kind": "apply_patches", "security_only": security_only }),
                )
                .await?;
            Ok(format!(
                "Patch run dispatched to {hostname}{}. It reports back when finished.",
                if security_only { " (security only)" } else { "" }
            ))
        }

        other => anyhow::bail!("no tool called {other}"),
    }
}
