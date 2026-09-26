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
is hidden.

Beyond the fleet itself there are three other areas. patchpanel_network is what a \
discovery sweep saw on the network, managed or not, and is a snapshot on a timer \
rather than live. patchpanel_logs, patchpanel_log and patchpanel_log_live cover \
syslog from appliances and journals forwarded by agents - the live window spans \
minutes on a busy fleet, so reach for patchpanel_log when you need further back. \
patchpanel_jobs and patchpanel_job_runs cover external scripts that report in, \
which is how a backup that printed a failure while exiting zero gets noticed.

Tools that change anything are hidden unless the server was started with \
--allow-actions, and refused by name even then if it was not. Anything that \
patches, reboots, flashes firmware or replaces the manifest should be confirmed \
with the person who asked before it is called.";

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
        json!({
            "name": "patchpanel_network",
            "description": "Every host a discovery sweep saw on the network, whether PatchPanel                 manages it or not. Machines and declared appliances are named; the rest are                 unexplained and are the interesting ones. Carries what the scan established:                 reverse-DNS name, open TCP and UDP ports, services with versions, OS                 fingerprint with its accuracy, hardware vendor. Says how old the sweep is - it                 runs hourly, so this is not live.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "unexplained_only": {
                        "type": "boolean",
                        "description": "only hosts nothing accounts for"
                    },
                    "address": {
                        "type": "string",
                        "description": "one address, for everything known about it"
                    }
                }
            }
        }),
        json!({
            "name": "patchpanel_logs",
            "description": "Which machines and devices are sending logs, how much each has                 sent, what severity it is set to and how long it is kept. A sender that has                 gone quiet is called out, because silence from something that was talking is                 the interesting case.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "patchpanel_log",
            "description": "The tail of one sender's log. Use patchpanel_logs for the names.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "source": { "type": "string", "description": "sender name or address" },
                    "contains": { "type": "string", "description": "only lines matching this" },
                    "lines": { "type": "integer", "description": "how many, default 200" }
                },
                "required": ["source"]
            }
        }),
        json!({
            "name": "patchpanel_log_live",
            "description": "The most recent log lines from every sender at once, merged and                 labelled by machine. A window on the last few thousand lines, so on a busy                 fleet it covers minutes rather than hours - patchpanel_log reaches further                 back for one sender.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "machine": { "type": "string", "description": "only this one" },
                    "contains": { "type": "string" },
                    "min_severity": {
                        "type": "integer",
                        "description": "syslog severity: 3 errors, 4 warnings, 6 info. Lines worse than or equal to this."
                    }
                }
            }
        }),
        json!({
            "name": "patchpanel_activity",
            "description": "What PatchPanel has been asked to do lately and how it went, newest                 first: patch runs, reboots, scans, sweeps, and anything that failed.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "hostname": { "type": "string", "description": "only this machine" },
                    "failed_only": { "type": "boolean" }
                }
            }
        }),
        json!({
            "name": "patchpanel_job_runs",
            "description": "Every recorded run of one external job - a backup script, a cron                 task - with what its own output said. Use patchpanel_jobs for the names.",
            "inputSchema": {
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "required": ["name"]
            }
        }),
        json!({
            "name": "patchpanel_appliance_history",
            "description": "How one appliance's firmware version and reachability have changed                 over time. Use patchpanel_appliances for the ids.",
            "inputSchema": {
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"]
            }
        }),
        json!({
            "name": "patchpanel_manifest",
            "description": "The desired state every agent is working from: apps, appliances to                 probe, discovery ranges, apt sources, patch policy, intervals. This is the                 document, not what is actually installed - compare against patchpanel_machine                 for that.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "patchpanel_agent_builds",
            "description": "Which agent versions are published for each platform, and which                 one the manifest is pointing the fleet at. An agent_version with no matching                 build is why a fleet silently stays on an old binary.",
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
        tools.push(json!({
            "name": "patchpanel_command",
            "description": "Ask one machine's agent to do something. THESE CHANGE THE MACHINE. \
                `reboot` and `distro_upgrade` interrupt service; `update_firmware` can leave \
                hardware that does not come back. Confirm with the person who asked before any \
                of them. `collect_inventory`, `probe_devices`, `discover`, `distro_check` and \
                `journal_volume` change nothing and are safe.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "hostname": { "type": "string" },
                    "action": {
                        "type": "string",
                        "enum": [
                            "collect_inventory", "apply_manifest", "probe_devices", "discover",
                            "journal_volume", "distro_check", "cleanup", "reboot",
                            "update_firmware", "finish_upgrade", "self_update", "distro_upgrade"
                        ]
                    },
                    "delay_secs": {
                        "type": "integer",
                        "description": "reboot only: seconds of warning, default 60"
                    },
                    "to": {
                        "type": "string",
                        "description": "distro_upgrade and distro_check: the release to move to, which must match what the agent itself worked out"
                    },
                    "grub_device": {
                        "type": "string",
                        "description": "finish_upgrade only: which disk a stuck bootloader package should install to"
                    }
                },
                "required": ["hostname", "action"]
            }
        }));
        tools.push(json!({
            "name": "patchpanel_sweep_now",
            "description": "Sweep the discovery ranges again now instead of waiting for the \
                timer. Read-only as far as the network is concerned - it opens connections and \
                reads banners. Refused while a sweep is already running.",
            "inputSchema": { "type": "object", "properties": {} }
        }));
        tools.push(json!({
            "name": "patchpanel_run_pool",
            "description": "Start a pool's run now rather than at its scheduled time. THIS \
                PATCHES AND MAY REBOOT every machine in the pool, respecting its concurrency.",
            "inputSchema": {
                "type": "object",
                "properties": { "pool": { "type": "string" } },
                "required": ["pool"]
            }
        }));
        tools.push(json!({
            "name": "patchpanel_set_pool",
            "description": "Move a machine into a pool, or out of every pool with an empty \
                name. Changes when it will be patched, not the machine itself.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "hostname": { "type": "string" },
                    "pool": { "type": "string", "description": "empty removes it from all pools" }
                },
                "required": ["hostname", "pool"]
            }
        }));
        tools.push(json!({
            "name": "patchpanel_ignore_update",
            "description": "Set one pending update aside so it stops counting as actionable, \
                or with `undo` bring it back. Pinned to the exact version: a newer release of \
                the same package appears again, so this cannot hide a future security fix.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "hostname": { "type": "string" },
                    "name": { "type": "string", "description": "package name" },
                    "version": { "type": "string", "description": "the exact version to set aside" },
                    "source": { "type": "string" },
                    "undo": { "type": "boolean" }
                },
                "required": ["hostname", "name", "version"]
            }
        }));
        tools.push(json!({
            "name": "patchpanel_backup_policy",
            "description": "Change what counts as a backup gap for one guest: `every_days` for \
                how often it should be backed up, `snooze_days` to go quiet about it for a \
                while and then resume, or `undo` to return it to the default. Tracking only - \
                it takes no backups.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "host": { "type": "string", "description": "the hypervisor" },
                    "guest": { "type": "string" },
                    "every_days": { "type": "integer", "description": "0 means do not track it" },
                    "snooze_days": { "type": "integer" },
                    "reason": { "type": "string" },
                    "undo": { "type": "boolean" }
                },
                "required": ["host", "guest"]
            }
        }));
        tools.push(json!({
            "name": "patchpanel_job_policy",
            "description": "Mute an external job so its failures stop asking for attention, \
                unmute it, or forget it entirely along with its history.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "muted": { "type": "boolean" },
                    "forget": { "type": "boolean", "description": "delete the job and its runs" }
                },
                "required": ["name"]
            }
        }));
        tools.push(json!({
            "name": "patchpanel_log_policy",
            "description": "For one sender: how long its log is kept, and - where PatchPanel \
                drives the forwarding - the lowest severity it sends. Raising the severity on a \
                machine writes to its rsyslog configuration and reloads the service.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "source": { "type": "string" },
                    "retain_hours": { "type": "integer", "description": "null returns it to the default" },
                    "min_severity": {
                        "type": "string",
                        "enum": ["err", "warning", "notice", "info"]
                    }
                },
                "required": ["source"]
            }
        }));
        tools.push(json!({
            "name": "patchpanel_rename_appliance",
            "description": "Change an appliance's id, taking its probe history with it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "to": { "type": "string" }
                },
                "required": ["id", "to"]
            }
        }));
        tools.push(json!({
            "name": "patchpanel_forget_machine",
            "description": "Remove a machine from the fleet along with everything recorded \
                about it. For a machine that is genuinely gone - one that is merely off will \
                reappear on its next check-in, having lost its history.",
            "inputSchema": {
                "type": "object",
                "properties": { "hostname": { "type": "string" } },
                "required": ["hostname"]
            }
        }));
        tools.push(json!({
            "name": "patchpanel_edit_manifest",
            "description": "Replace the manifest, the desired state every agent works from. THE \
                MOST CONSEQUENTIAL CALL HERE: it can retarget the whole fleet's agent version, \
                change patch policy, or drop appliances and discovery ranges. Read \
                patchpanel_manifest first, change only what you mean to, and send the whole \
                document back - anything left out is removed. Confirm the change with the \
                person who asked before calling it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "manifest": {
                        "type": "object",
                        "description": "the complete document, as returned by patchpanel_manifest"
                    }
                },
                "required": ["manifest"]
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

/// Whether a tool changes anything.
///
/// Enforced at call time and not only by leaving these out of `tools/list`. The
/// catalogue is a hint: a client that listed the tools while actions were enabled,
/// or that simply guesses a name, can ask for one regardless of what the
/// catalogue currently says - so read-only has to mean the server refuses, not
/// that it declines to advertise.
pub fn is_action(name: &str) -> bool {
    matches!(
        name,
        "patchpanel_rescan"
            | "patchpanel_install_updates"
            | "patchpanel_command"
            | "patchpanel_sweep_now"
            | "patchpanel_run_pool"
            | "patchpanel_set_pool"
            | "patchpanel_ignore_update"
            | "patchpanel_backup_policy"
            | "patchpanel_job_policy"
            | "patchpanel_log_policy"
            | "patchpanel_rename_appliance"
            | "patchpanel_forget_machine"
            | "patchpanel_edit_manifest"
    )
}

pub async fn call(portal: &Portal, name: &str, args: Value) -> Result<String> {
    if is_action(name) && !portal.allow_actions {
        anyhow::bail!(
            "{name} changes something, and this server is running read-only. Start pp-mcp with \
             --allow-actions to permit it."
        );
    }
    match name {
        "patchpanel_overview" => {
            let fleet = portal.get("/api/fleet").await?;
            Ok(format!("{}\n\n{}", gist(&fleet), attention(&fleet)))
        }

        "patchpanel_network" => {
            let d = portal.get("/api/devices").await?;
            let hosts = arr(&d, "network");
            let sweeps = arr(&d, "sweeps");
            let want = s(&args, "address");
            let only_unknown = b(&args, "unexplained_only");

            // Said first, because every answer below is as old as the sweep, and a
            // reader who assumes it is live will draw the wrong conclusion from an
            // address that has since come or gone.
            let mut out = vec![match sweeps.first() {
                Some(w) => format!(
                    "Swept {} by {}. Runs on a timer, so this is a snapshot, not live.",
                    ago(&s(w, "at")),
                    s(w, "collector_host")
                ),
                None => "No sweep has completed yet.".to_string(),
            }];
            for note in arr(&d, "discovery_notes") {
                if let Some(t) = note.as_str() {
                    out.push(format!("NOTE: {t}"));
                }
            }

            if !want.is_empty() {
                let h = hosts
                    .iter()
                    .find(|h| s(h, "ip") == want)
                    .with_context(|| format!("no host at {want} in the last sweep"))?;
                out.push(String::new());
                out.push(host_detail(h));
                return Ok(out.join("\n"));
            }

            let unknown = hosts.iter().filter(|h| h.get("known").is_none()).count();
            out.push(format!(
                "{} host(s) answered; {unknown} that nothing accounts for.",
                hosts.len()
            ));
            out.push(String::new());
            for h in hosts
                .iter()
                .filter(|h| !only_unknown || h.get("known").is_none())
            {
                out.push(host_line(h));
            }
            out.push(String::new());
            out.push("Ask for one `address` to get its ports, services and OS guess.".into());
            Ok(out.join("\n"))
        }

        "patchpanel_logs" => {
            let d = portal.get("/api/logs").await?;
            let mut out = Vec::new();
            if !b(&d, "receiving") {
                out.push(
                    "The syslog receiver is OFF, so only journals an agent forwards over its \
                     own connection appear here."
                        .to_string(),
                );
            }
            let silent = arr(&d, "asked_but_silent");
            if !silent.is_empty() {
                // The interesting case: asked to send, sending nothing.
                let names: Vec<String> = silent
                    .iter()
                    .map(|x| x.as_str().unwrap_or_default().to_string())
                    .collect();
                out.push(format!(
                    "ASKED TO FORWARD BUT SILENT: {} - either nothing has happened, or the \
                     forwarding is broken.",
                    names.join(", ")
                ));
            }
            out.push(String::new());
            for x in arr(&d, "senders") {
                let name = s(&x, "device");
                let source = s(&x, "source");
                out.push(format!(
                    "{:<22} {:>9} lines  {:<8} kept {}h{}  last {}",
                    if name.is_empty() { source } else { name },
                    n(&x, "lines"),
                    s(&x, "min_severity"),
                    n(&x, "retain_hours"),
                    if b(&x, "set_here") { "" } else { " (its own setting)" },
                    ago(&s(&x, "last_line_at"))
                ));
            }
            Ok(out.join("\n"))
        }

        "patchpanel_log" => {
            let source = s(&args, "source");
            let lines = if n(&args, "lines") > 0 { n(&args, "lines") } else { 200 };
            let contains = s(&args, "contains");
            let d = portal
                .get(&format!(
                    "/api/logs/{}?lines={lines}&contains={}",
                    enc(&source),
                    enc(&contains)
                ))
                .await?;
            let got = arr(&d, "lines");
            let mut out = vec![format!(
                "{} - {} of {} line(s) held{}",
                s(&d, "device"),
                got.len(),
                n(&d, "total"),
                if contains.is_empty() {
                    String::new()
                } else {
                    format!(", matching {contains:?}")
                }
            )];
            for l in got {
                out.push(l.as_str().unwrap_or_default().to_string());
            }
            Ok(out.join("\n"))
        }

        "patchpanel_log_live" => {
            let d = portal.get("/api/logs/live").await?;
            let machine = s(&args, "machine").to_lowercase();
            let contains = s(&args, "contains").to_lowercase();
            let sev = if args.get("min_severity").is_some() {
                n(&args, "min_severity")
            } else {
                7
            };
            let mut out = Vec::new();
            if !b(&d, "receiver_on") {
                out.push("The syslog receiver is off; only forwarded journals appear.".to_string());
            }
            let mut kept = 0;
            for l in arr(&d, "lines") {
                let who = s(&l, "who");
                if !machine.is_empty() && !who.to_lowercase().contains(&machine) {
                    continue;
                }
                if n(&l, "severity") > sev {
                    continue;
                }
                let msg = s(&l, "msg");
                let tag = s(&l, "tag");
                if !contains.is_empty()
                    && !format!("{who} {tag} {msg}").to_lowercase().contains(&contains)
                {
                    continue;
                }
                kept += 1;
                out.push(format!(
                    "{} {:<16} {:<14} {msg}",
                    s(&l, "at").chars().take(19).collect::<String>(),
                    who,
                    tag
                ));
            }
            if kept == 0 {
                out.push("Nothing in the portal's live window matches.".to_string());
            }
            Ok(out.join("\n"))
        }

        "patchpanel_activity" => {
            let log = portal.get("/api/commands").await?;
            let rows = log.as_array().cloned().unwrap_or_default();
            let failed_only = b(&args, "failed_only");
            let hostname = s(&args, "hostname");
            // Commands carry an agent id, so filtering by name needs the fleet to
            // turn one into the other.
            let want_id = if hostname.is_empty() {
                String::new()
            } else {
                agent_id(portal, &hostname).await?.0
            };

            let mut out = Vec::new();
            for c in rows.iter().take(200) {
                if !want_id.is_empty() && s(c, "agent_id") != want_id {
                    continue;
                }
                let done = c.get("finished_at").and_then(|v| v.as_str()).is_some();
                let ok = b(c, "ok");
                if failed_only && (!done || ok) {
                    continue;
                }
                out.push(format!(
                    "{:<16} {:<22} {}  {}",
                    ago(&s(c, "created_at")),
                    s(c, "kind"),
                    if !done {
                        "running"
                    } else if ok {
                        "ok     "
                    } else {
                        "FAILED "
                    },
                    s(c, "summary")
                ));
            }
            if out.is_empty() {
                return Ok("Nothing matches.".to_string());
            }
            Ok(out.join("\n"))
        }

        "patchpanel_job_runs" => {
            let name = s(&args, "name");
            let d = portal
                .get(&format!("/api/jobs/{}/runs", enc(&name)))
                .await?;
            let runs = d.as_array().cloned().unwrap_or_default();
            if runs.is_empty() {
                return Ok(format!("No recorded runs for {name}."));
            }
            let mut out = vec![format!("{name} - {} run(s) recorded", runs.len())];
            for r in runs.iter().take(50) {
                out.push(format!(
                    "{:<16} {}  {}",
                    ago(&s(r, "at")),
                    if b(r, "ok") { "ok    " } else { "FAILED" },
                    s(r, "detail")
                ));
            }
            Ok(out.join("\n"))
        }

        "patchpanel_appliance_history" => {
            let id = s(&args, "id");
            let d = portal
                .get(&format!("/api/devices/{}/history", enc(&id)))
                .await?;
            let rows = d.as_array().cloned().unwrap_or_default();
            if rows.is_empty() {
                return Ok(format!("No history recorded for {id}."));
            }
            let mut out = vec![format!("{id} - {} entr(ies)", rows.len())];
            for r in rows.iter().take(60) {
                out.push(format!(
                    "{:<16} {:<18} {}",
                    ago(&s(r, "at")),
                    s(r, "firmware"),
                    s(r, "note")
                ));
            }
            Ok(out.join("\n"))
        }

        "patchpanel_manifest" => {
            let m = portal.get("/api/manifest").await?;
            Ok(format!(
                "Revision {}. This is the desired state, not what is installed.\n\n{}",
                n(&m, "revision"),
                serde_json::to_string_pretty(&m).unwrap_or_default()
            ))
        }

        "patchpanel_agent_builds" => {
            let builds = portal.get("/api/builds").await?;
            let m = portal.get("/api/manifest").await?;
            let want = s(&m, "agent_version");
            let rows = builds.as_array().cloned().unwrap_or_default();
            let mut out = vec![format!(
                "The manifest points the fleet at {}.",
                if want.is_empty() {
                    "no version".to_string()
                } else {
                    want.clone()
                }
            )];
            if !want.is_empty() && !rows.iter().any(|b| s(b, "version") == want) {
                // The failure that leaves a whole fleet on an old binary with
                // nothing reporting an error anywhere.
                out.push(
                    "NO BUILD IS PUBLISHED FOR THAT VERSION, so no agent has anything to fetch \
                     and every one of them will silently stay where it is."
                        .to_string(),
                );
            }
            out.push(String::new());
            for bd in rows {
                out.push(format!(
                    "{:<10} {:<8} {:<9} {}",
                    s(&bd, "version"),
                    s(&bd, "os"),
                    s(&bd, "arch"),
                    if s(&bd, "version") == want { "<- wanted" } else { "" }
                ));
            }
            Ok(out.join("\n"))
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

        "patchpanel_command" => {
            let hostname = s(&args, "hostname");
            let (id, _) = agent_id(portal, &hostname).await?;
            let action = s(&args, "action");
            // Built here rather than passed through, so an action this tool does
            // not name cannot be smuggled into the portal by a caller.
            let body = match action.as_str() {
                "collect_inventory" | "apply_manifest" | "discover" | "journal_volume"
                | "cleanup" | "self_update" => json!({ "kind": action }),
                "probe_devices" => json!({ "kind": "probe_devices", "only": [] }),
                "update_firmware" => json!({ "kind": "update_firmware", "only": [] }),
                "reboot" => {
                    let delay = if n(&args, "delay_secs") > 0 { n(&args, "delay_secs") } else { 60 };
                    json!({ "kind": "reboot", "delay_secs": delay })
                }
                "finish_upgrade" => {
                    let g = s(&args, "grub_device");
                    if g.is_empty() {
                        json!({ "kind": "finish_upgrade" })
                    } else {
                        json!({ "kind": "finish_upgrade", "grub_device": g })
                    }
                }
                "distro_check" | "distro_upgrade" => {
                    let to = s(&args, "to");
                    if to.is_empty() {
                        anyhow::bail!(
                            "{action} needs `to`, the release to move to. It must match what \
                             the agent itself worked out - run patchpanel_machine to see it."
                        );
                    }
                    json!({
                        "kind": "distro_upgrade",
                        "to": to,
                        "check": action == "distro_check"
                    })
                }
                other => anyhow::bail!("unknown action {other:?}"),
            };
            let r = portal
                .post(&format!("/api/agents/{id}/commands"), body)
                .await?;
            Ok(format!(
                "Asked {hostname} to {action}. Command {}. Follow it with patchpanel_activity.",
                s(&r, "id")
            ))
        }

        "patchpanel_sweep_now" => {
            let r = portal
                .post(
                    "/api/commands/broadcast",
                    json!({ "command": { "kind": "discover" } }),
                )
                .await?;
            Ok(format!(
                "Sweep dispatched to {} collector(s) - only those whose site owns a discovery \
                 range. It takes a few minutes; patchpanel_network will say when it last \
                 finished.",
                n(&r, "dispatched_to")
            ))
        }

        "patchpanel_run_pool" => {
            let pool = s(&args, "pool");
            let r = portal
                .post(&format!("/api/pools/{}/run", enc(&pool)), json!({}))
                .await?;
            Ok(format!(
                "Started {pool}. {} machine(s) taken in hand this run.",
                n(&r, "started")
            ))
        }

        "patchpanel_set_pool" => {
            let hostname = s(&args, "hostname");
            let pool = s(&args, "pool");
            let (id, _) = agent_id(portal, &hostname).await?;
            portal
                .post(&format!("/api/agents/{id}/pool"), json!({ "pool": pool }))
                .await?;
            Ok(if pool.is_empty() {
                format!("{hostname} is no longer in any pool, so nothing will patch it on a schedule.")
            } else {
                format!("{hostname} is now in {pool}.")
            })
        }

        "patchpanel_ignore_update" => {
            let hostname = s(&args, "hostname");
            let (id, _) = agent_id(portal, &hostname).await?;
            let body = json!({
                "name": s(&args, "name"),
                "version": s(&args, "version"),
                "source": s(&args, "source"),
            });
            let path = format!("/api/agents/{id}/ignores");
            if b(&args, "undo") {
                portal.delete(&path, Some(body)).await?;
                Ok(format!(
                    "{} {} counts as pending on {hostname} again.",
                    s(&args, "name"),
                    s(&args, "version")
                ))
            } else {
                portal.post(&path, body).await?;
                Ok(format!(
                    "{} {} is set aside on {hostname}. A newer version will appear as new.",
                    s(&args, "name"),
                    s(&args, "version")
                ))
            }
        }

        "patchpanel_backup_policy" => {
            let host = s(&args, "host");
            let guest = s(&args, "guest");
            let body = json!({
                "host": host,
                "guest": guest,
                "reason": s(&args, "reason"),
                "every_days": n(&args, "every_days"),
                "snooze_days": n(&args, "snooze_days"),
            });
            if b(&args, "undo") {
                portal.delete("/api/backups/exempt", Some(body)).await?;
                return Ok(format!("{guest} on {host} is back to the default expectation."));
            }
            if n(&args, "snooze_days") > 0 {
                portal.post("/api/backups/snooze", body).await?;
                Ok(format!(
                    "{guest} on {host} will be quiet for {} day(s), then go back to normal.",
                    n(&args, "snooze_days")
                ))
            } else {
                portal.post("/api/backups/exempt", body).await?;
                Ok(match n(&args, "every_days") {
                    0 => format!("{guest} on {host} is no longer tracked for backups."),
                    d => format!("{guest} on {host} is expected to be backed up every {d} day(s)."),
                })
            }
        }

        "patchpanel_job_policy" => {
            let name = s(&args, "name");
            if b(&args, "forget") {
                portal.delete(&format!("/api/jobs/{}", enc(&name)), None).await?;
                return Ok(format!("Forgot {name} and its run history."));
            }
            let muted = b(&args, "muted");
            portal
                .post(&format!("/api/jobs/{}/mute", enc(&name)), json!({ "muted": muted }))
                .await?;
            Ok(if muted {
                format!("{name} is muted; its failures will stop asking for attention but are still recorded.")
            } else {
                format!("{name} is unmuted.")
            })
        }

        "patchpanel_log_policy" => {
            let source = s(&args, "source");
            let mut done = Vec::new();
            if let Some(h) = args.get("retain_hours") {
                portal
                    .post(
                        &format!("/api/logs/{}/retention", enc(&source)),
                        json!({ "hours": h }),
                    )
                    .await?;
                done.push(match h.as_i64() {
                    Some(v) => format!("kept for {v}h"),
                    None => "back to the default retention".to_string(),
                });
            }
            let sev = s(&args, "min_severity");
            if !sev.is_empty() {
                portal
                    .post(
                        &format!("/api/logs/{}/level", enc(&source)),
                        json!({ "min_severity": sev }),
                    )
                    .await?;
                done.push(format!("forwarding {sev} and worse"));
            }
            if done.is_empty() {
                anyhow::bail!("nothing to change: pass retain_hours, min_severity, or both");
            }
            Ok(format!("{source}: {}.", done.join(", ")))
        }

        "patchpanel_rename_appliance" => {
            let id = s(&args, "id");
            let to = s(&args, "to");
            portal
                .post(&format!("/api/devices/{}/id", enc(&id)), json!({ "to": to }))
                .await?;
            Ok(format!("{id} is now {to}, history included."))
        }

        "patchpanel_forget_machine" => {
            let hostname = s(&args, "hostname");
            let (id, _) = agent_id(portal, &hostname).await?;
            portal.delete(&format!("/api/agents/{id}"), None).await?;
            Ok(format!(
                "Removed {hostname} and everything recorded about it. If it is only switched \
                 off it will enrol again on its next check-in, with no history."
            ))
        }

        "patchpanel_edit_manifest" => {
            let doc = args
                .get("manifest")
                .filter(|m| m.is_object())
                .context("`manifest` must be the whole document, as an object")?;
            // Refusing an empty-looking document, because the usual way to
            // destroy a manifest through an API is to send a fragment of one.
            if arr(doc, "devices").is_empty() && arr(doc, "discovery").is_empty() && arr(doc, "apps").is_empty()
            {
                anyhow::bail!(
                    "that manifest has no apps, appliances or discovery ranges in it, which \
                     would remove everything the fleet has. Read patchpanel_manifest, change \
                     what you mean to, and send the whole document back."
                );
            }
            let r = portal.put("/api/manifest", doc.clone()).await?;
            Ok(format!(
                "Manifest is now revision {}, pushed to {} agent(s); {} will update themselves.",
                n(&r, "revision"),
                n(&r, "pushed_to"),
                n(&r, "upgrading")
            ))
        }

        other => anyhow::bail!("no tool called {other}"),
    }
}
