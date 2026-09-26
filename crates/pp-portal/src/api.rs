//! REST API behind the dashboard.
//!
//! Everything the UI does is available here as plain JSON, because the two
//! things operators always end up wanting — a scripted rollout and a Nagios
//! check — should not require scraping HTML.


use axum::extract::{Path, Query, State};
use axum::http::{header, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use pp_proto::{AgentId, Command, CommandEnvelope, DeviceReport, Manifest, ServerMsg};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::db::AgentBuild;
use crate::state::{ApiError, ApiResult, SharedState};

/// Routes served without authentication. Deliberately tiny: it exists so the
/// dashboard can discover whether it needs to ask for a token at all.
pub fn public_routes(state: SharedState) -> Router {
    Router::new()
        .route("/api/auth-mode", get(auth_mode))
        .with_state(state)
}

async fn auth_mode(State(state): State<SharedState>) -> Json<serde_json::Value> {
    Json(json!({ "required": state.require_admin_auth }))
}

pub fn routes(state: SharedState) -> Router {
    Router::new()
        .route("/api/fleet", get(fleet))
        .route("/api/pools", get(list_pools).post(put_pool))
        .route("/api/pools/{name}", delete(delete_pool))
        .route("/api/pools/{name}/run", post(run_pool))
        .route("/api/agents/{id}/pool", post(set_pool))
        .route("/api/backups", get(backups))
        .route("/api/backups/exempt", post(set_exempt).delete(clear_exempt))
        .route("/api/devices", get(devices))
        .route("/api/devices/{id}/history", get(device_history))
        .route("/api/devices/{id}/id", post(rename_device))
        .route("/api/agents/{id}", get(agent).delete(delete_agent))
        .route("/api/agents/{id}/commands", post(dispatch))
        .route("/api/agents/{id}/ignores", post(add_ignore).delete(remove_ignore))
        .route("/api/commands", get(command_log))
        .route("/api/schedule", get(schedule))
        .route("/api/backups/snooze", post(snooze_backup))
        .route("/api/jobs", get(list_jobs).post(job_checkin))
        .route("/api/jobs/{name}", axum::routing::delete(forget_job))
        .route("/api/jobs/{name}/runs", get(job_runs))
        .route("/api/logs", get(log_senders))
        .route("/api/logs/{source}", get(device_log_tail))
        .route("/api/logs/{source}/export", get(device_log_export))
        .route("/api/logs/{source}/retention", post(set_retention))
        .route("/api/logs/{source}/level", post(set_level))
        .route("/api/jobs/{name}/mute", post(mute_job))
        .route("/api/commands/broadcast", post(broadcast))
        .route("/api/manifest", get(get_manifest).put(put_manifest))
        .route("/api/builds", get(list_builds).post(add_build))
        .route("/api/enrollment", get(enrollment))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_admin,
        ))
        .with_state(state)
}

/// Bearer auth for every `/api` route. The agent WebSocket is mounted outside
/// this router because agents authenticate with their own tokens instead.
async fn require_admin(
    State(state): State<SharedState>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    // Auth explicitly turned off by the operator.
    if !state.require_admin_auth {
        return next.run(req).await;
    }

    let presented = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");

    if crate::ws::constant_time_eq(presented, &state.admin_token) {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "admin token required" })),
        )
            .into_response()
    }
}

// ---------------------------------------------------------------------------
// Fleet
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct FleetResponse {
    summary: FleetSummary,
    agents: Vec<AgentView>,
    manifest_revision: u64,
    /// What needs a person, grouped by cause. Rides along with the fleet
    /// because the page already polls this endpoint every few seconds, and a
    /// second request would give the badge and the list different answers.
    attention: Vec<crate::attention::Item>,
    gist: crate::attention::Gist,
}

/// The numbers an operator wants before they want any detail.
#[derive(Default, Serialize)]
struct FleetSummary {
    agents: usize,
    online: usize,
    offline: usize,
    /// Updates that could be installed right now, fleet-wide.
    pending_updates: usize,
    /// Counted but not installable: phased rollouts and the like.
    deferred_updates: usize,
    pending_security: usize,
    needs_reboot: usize,
    app_drift: usize,
    stale_manifest: usize,
    devices: usize,
    devices_unreachable: usize,
    devices_drifted: usize,
    /// Devices with updates waiting, and devices whose release is retired.
    /// Both belong in the summary so the navigation can carry a count without
    /// the Devices tab having to have been opened.
    #[serde(default)]
    devices_pending: usize,
    #[serde(default)]
    devices_eol: usize,
    /// Machines with something installable waiting.
    #[serde(default)]
    agents_pending: usize,
    /// How many things need a person. This is what the navigation badge
    /// should carry: `agents_pending` counts machines a pool will handle
    /// tonight, which is not a number anyone can act on.
    #[serde(default)]
    attention: usize,
    /// Jobs that have failed or stopped checking in. On the summary so the
    /// Jobs badge is right before the tab has ever been opened - a badge that
    /// only appears once you look at the thing it is warning you about is
    /// worse than no badge.
    #[serde(default)]
    jobs_bad: usize,
    /// Hosts answering on a discovery range that nothing here accounts for, and
    /// machines asked to forward logs that have never sent any. Both on the
    /// summary for the same reason as the rest: a badge that only appears once
    /// you open the tab it is warning you about is worse than no badge.
    #[serde(default)]
    unexplained_hosts: usize,
    #[serde(default)]
    logs_silent: usize,
    /// Running guests with no recent backup, so the navigation can carry the
    /// count without the Backups tab having been opened.
    #[serde(default)]
    backups_at_risk: usize,
}

#[derive(Serialize)]
struct AgentView {
    #[serde(flatten)]
    row: crate::db::AgentRow,
    /// Distinct from `online`: this is a live socket, not a recent heartbeat.
    connected: bool,
}

async fn fleet(State(state): State<SharedState>) -> ApiResult<Json<FleetResponse>> {
    let manifest = state.db.manifest()?;
    let mut rows = state.db.agents()?;

    // Count each host's guests that no agent reports for. Only the fleet as a
    // whole can answer that, so it is filled in here rather than per row.
    // Pool membership and the last patch run, so the fleet table can show
    // whether the schedule is doing what it was asked to.
    let pools = state.db.pool_of_each().unwrap_or_default();
    let patched = state.db.last_patch_runs().unwrap_or_default();
    for row in rows.iter_mut() {
        let id = row.id.to_string();
        row.pool = pools.get(&id).cloned().unwrap_or_default();
        if let Some((at, ok)) = patched.get(&id) {
            row.last_patched = Some(*at);
            row.last_patch_ok = *ok;
        }
    }

    // Whether a pending update is somebody's job or the schedule's.
    //
    // A count nobody can act on is noise, and a count that looks the same
    // whether the schedule is working or broken is worse than noise. These are
    // different situations and they should not look alike.
    let pool_defs = state.db.pools().unwrap_or_default();
    for row in rows.iter_mut() {
        let pool = pool_defs.iter().find(|p| p.name == row.pool);
        let scheduled = pool.is_some_and(|p| {
            p.scope != crate::pools::Scope::None && p.schedule != crate::pools::Schedule::Manual
        });

        row.patch_state = if row.last_patch_ok == Some(false) {
            row.patch_note = "the last patch run failed".into();
            row.patch_short = "run failed".into();
            "failed".into()
        } else if scheduled {
            let pool = pool.expect("scheduled implies a pool");
            // A run happened and this machine still has work left: it was
            // offline, busy, or something refused. That is the case worth
            // seeing, and it is invisible if every pending count looks alike.
            let waiting = pool.last_run.is_some_and(|ran| {
                row.actionable_count > 0 && row.last_patched.is_none_or(|p| p < ran)
            });
            // Still inside the window, so the pool has simply not reached it
            // yet - it dispatches a few machines at a time on purpose.
            let run_open = pool.last_run.is_some_and(|ran| {
                chrono::Utc::now().signed_duration_since(ran).num_hours() < 4
            });

            if waiting && run_open {
                // Say "pool" out loud: a pool called `servers` reads as a server
                // otherwise, and the sentence stops making sense.
                row.patch_note =
                    format!("the \"{}\" pool is working through its machines", pool.name);
                row.patch_short = "in this run".into();
                "queued".into()
            } else if waiting {
                row.patch_note = if !row.online {
                    "it was offline when the pool ran; it will be picked up next time".into()
                } else if row.scan_issue_count > 0 {
                    "the pool ran but this machine cannot be scanned - open it to see why".into()
                } else {
                    format!(
                        "the \"{}\" pool ran without patching this one. Install updates here, or wait for the next run.",
                        pool.name
                    )
                };
                row.patch_short = if row.online { "not patched".into() } else { "was offline".into() };
                "missed".into()
            } else if row.actionable_count > 0 {
                let next = pool.schedule.next_after(chrono::Local::now());
                row.patch_note = next
                    .map(|t| format!("the \"{}\" pool patches it {}", pool.name, t.format("%a %H:%M")))
                    .unwrap_or_else(|| format!("the \"{}\" pool has it", pool.name));
                row.patch_short = next
                    .map(|t| t.format("%a %H:%M").to_string())
                    .unwrap_or_else(|| pool.name.clone());
                "scheduled".into()
            } else {
                "clean".into()
            }
        } else if row.actionable_count > 0 {
            row.patch_note = if row.pool.is_empty() {
                "no pool patches this machine".into()
            } else {
                format!("the \"{}\" pool does not patch on a schedule", row.pool)
            };
            row.patch_short = "no schedule".into();
            "yours".into()
        } else {
            "clean".into()
        };
    }

    // Devices were being collected twice further down. Once is enough, and
    // the attention list needs them before the rows are consumed.
    let all_devices = collect_devices(&state).unwrap_or_default();
    let trouble: Vec<crate::attention::DeviceTrouble> = all_devices
        .iter()
        .map(|d| crate::attention::DeviceTrouble {
            id: d.report.id.clone(),
            label: if d.label.is_empty() {
                d.report.id.clone()
            } else {
                d.label.clone()
            },
            firmware: d.report.firmware.clone().unwrap_or_default(),
            reachable: d.report.reachable,
            eol: d.report.eol,
            eol_note: d.report.eol_note.clone(),
            updates: d.report.updates,
            collector: d.collector_host.clone(),
        })
        .collect();

    let known: Vec<String> = rows.iter().map(|a| a.hostname.to_lowercase()).collect();
    let short = |s: &str| s.split('.').next().unwrap_or(s).to_string();
    for row in rows.iter_mut() {
        let Some(inv) = state.db.inventory(row.id)? else {
            continue;
        };
        let Some(virt) = inv.virt else { continue };
        row.unmanaged_guests = virt
            .guests
            .iter()
            .filter(|g| g.state.to_lowercase().starts_with("running"))
            .filter(|g| {
                let name = g.name.to_lowercase();
                !known.iter().any(|k| *k == name || short(k) == short(&name))
            })
            .count();
    }
    let rows = rows;
    let mut summary = FleetSummary {
        agents: rows.len(),
        ..Default::default()
    };

    let risky = at_risk_guests(&state);
    let now = chrono::Utc::now();
    let job_trouble: Vec<crate::attention::JobTrouble> = state
        .db
        .jobs()
        .unwrap_or_default()
        .into_iter()
        .map(|j| {
            let (status, _) = job_status(&j, now);
            let late_by = j
                .last_at
                .map(|t| {
                    let h = now.signed_duration_since(t).num_hours();
                    if h < 48 {
                        format!("last ran {h}h ago")
                    } else {
                        format!("last ran {} days ago", h / 24)
                    }
                })
                .unwrap_or_else(|| "has never run".into());
            crate::attention::JobTrouble {
                name: j.name,
                status: status.to_string(),
                detail: j.last_detail,
                late_by,
            }
        })
        .collect();
    let attention =
        crate::attention::collect(&rows, manifest.revision, &risky, &trouble, &job_trouble);
    let gist = crate::attention::gist(&rows);

    let agents: Vec<AgentView> = rows
        .into_iter()
        .map(|row| {
            if row.online {
                summary.online += 1;
            } else {
                summary.offline += 1;
            }
            summary.pending_updates += row.actionable_count;
            summary.deferred_updates += row.deferred_count;
            summary.pending_security += row.security_count;
            summary.needs_reboot += row.reboot_required as usize;
            summary.app_drift += row.drift_count;
            summary.devices += row.device_count;
            summary.devices_unreachable += row.device_problem_count;
            if row.applied_revision < manifest.revision {
                summary.stale_manifest += 1;
            }
            if row.actionable_count > 0 {
                summary.agents_pending += 1;
            }
            let connected = state.hub.is_connected(row.id);
            AgentView { row, connected }
        })
        .collect();

    // Device counts come from the same deduplicated view the Machines tab
    // uses, so a badge and the page it points at cannot disagree.
    summary.devices_drifted = all_devices.iter().filter(|d| d.report.drift).count();
    summary.devices_unreachable = all_devices.iter().filter(|d| !d.report.reachable).count();
    summary.devices_pending = all_devices
        .iter()
        .filter(|d| d.report.updates_known && d.report.updates > 0)
        .count();
    summary.devices_eol = all_devices.iter().filter(|d| d.report.eol).count();
    // The same answer the Network tab shows, from the same function, so the
    // badge and the page cannot disagree about what counts as unexplained.
    {
        let accounted = accounted_for(&all_devices, agents.iter().map(|a| &a.row));
        let mut seen: std::collections::HashSet<String> = Default::default();
        for a in &agents {
            if let Ok(Some(inv)) = state.db.inventory(a.row.id) {
                for host in inv.discovered.iter().filter(|h| h.unmanaged) {
                    if !accounted.contains_key(&host.ip) {
                        seen.insert(host.ip.clone());
                    }
                }
            }
        }
        summary.unexplained_hosts = seen.len();
    }

    // Same grace as the list, so the badge and the page agree.
    summary.logs_silent = agents
        .iter()
        .filter(|a| {
            let asked = state.db.log_forward(a.row.id).ok().flatten();
            let Some((_, since)) = asked else { return false };
            chrono::Utc::now().signed_duration_since(since).num_hours() >= 6
                && !crate::syslog::senders(&state.log_dir)
                    .iter()
                    .any(|s| s.source == a.row.hostname)
        })
        .count();

    summary.jobs_bad = job_trouble
        .iter()
        .filter(|j| matches!(j.status.as_str(), "overdue" | "failed" | "suspect"))
        .count();
    summary.backups_at_risk = risky.len();
    summary.attention = attention.len();

    Ok(Json(FleetResponse {
        summary,
        agents,
        manifest_revision: manifest.revision,
        attention,
        gist,
    }))
}

#[derive(Serialize)]
struct AgentDetail {
    #[serde(flatten)]
    row: crate::db::AgentRow,
    connected: bool,
    inventory: Option<pp_proto::Inventory>,
    commands: Vec<crate::db::CommandRow>,
    repo_diff: RepoDiff,
    /// Updates set aside on this machine, and at which version.
    ignored: Vec<crate::db::IgnoredUpdate>,
    /// Applications watched by version, which PatchPanel reports but does not
    /// update.
    tracked: Vec<TrackedApp>,
    /// When the hypervisor hosting this machine last backed it up, if any
    /// hypervisor in the fleet reports it as a guest.
    backup: GuestBackup,
}

/// What is known about this machine being backed up by its host.
#[derive(Serialize, Default)]
struct GuestBackup {
    /// The host reporting it, empty when no hypervisor here claims it.
    host: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    last: Option<chrono::DateTime<chrono::Utc>>,
}

/// Find the hypervisor that lists this machine as a guest, and what it says
/// about backing it up.
fn host_backup_of(state: &SharedState, hostname: &str) -> anyhow::Result<GuestBackup> {
    let short = |s: &str| s.split('.').next().unwrap_or(s).to_ascii_lowercase();
    let me = short(hostname);

    for row in state.db.agents()? {
        let Some(inv) = state.db.inventory(row.id)? else {
            continue;
        };
        let Some(virt) = inv.virt else { continue };
        if let Some(guest) = virt.guests.iter().find(|g| short(&g.name) == me) {
            return Ok(GuestBackup {
                host: row.hostname.clone(),
                last: guest.last_backup,
            });
        }
    }
    Ok(GuestBackup::default())
}

/// How this machine's package sources compare with its peers.
///
/// "Peers" means agents on the same OS, because comparing apt sources against a
/// Windows box would be noise. A repository present here but nowhere else, or
/// missing here but present on every peer, is usually the reason one machine
/// behaves differently.
#[derive(Default, Serialize)]
struct RepoDiff {
    /// How many comparable agents this was compared against.
    peers: usize,
    /// What made them comparable: the distribution and release, or the bare
    /// OS for a machine that has not reported one. Shown so the comparison is
    /// never mistaken for a wider one than it is.
    #[serde(default)]
    group: String,
    /// Configured here, on no peer.
    only_here: Vec<String>,
    /// Configured on every peer, but not here.
    missing_here: Vec<String>,
    /// Security suites seen on comparable machines, whether or not every one
    /// of them has it. A machine with no security source needs to be told
    /// which line to add, and its neighbours running the same release are the
    /// authoritative answer.
    #[serde(default)]
    peer_security: Vec<String>,
}

/// Identity of a repository for comparison: where it points and at what suite.
/// The declaring filename is deliberately excluded - the same repo added under
/// a different filename is still the same repo.
fn repo_key(r: &pp_proto::Repository) -> String {
    format!("{} {} {}", r.source, r.uri.trim_end_matches('/'), r.suite)
}

/// What makes two machines comparable for sources.
///
/// The release, not the operating system. Every Debian 12 box should have the
/// same archives; a Debian 13 box next to it correctly has different ones, and
/// comparing the two produces a page full of differences that are all correct.
/// Falls back to the OS for a machine that has not reported a release, which
/// is coarse but never claims more than it knows.
fn compare_group(inv: Option<&pp_proto::Inventory>, os: &str) -> String {
    match inv.and_then(|i| i.release.as_ref()) {
        Some(r) if !r.codename.is_empty() => format!("{} {}", r.distro, r.codename),
        _ => os.to_string(),
    }
}

/// Does this repository carry security updates?
fn is_security(r: &pp_proto::Repository) -> bool {
    r.uri.contains("security") || r.suite.contains("security") || r.suite.ends_with("/updates")
}

fn repo_diff(
    state: &SharedState,
    me: pp_proto::AgentId,
    my_os: &str,
    my_inv: Option<&pp_proto::Inventory>,
    mine: &[pp_proto::Repository],
) -> anyhow::Result<RepoDiff> {
    use std::collections::HashSet;

    let my_keys: HashSet<String> = mine
        .iter()
        .filter(|r| r.enabled)
        .map(repo_key)
        .collect();
    let group = compare_group(my_inv, my_os);

    let mut peer_sets: Vec<HashSet<String>> = Vec::new();
    let mut peer_security: Vec<String> = Vec::new();
    for row in state.db.agents()? {
        if row.id == me {
            continue;
        }
        let Some(inv) = state.db.inventory(row.id)? else {
            continue;
        };
        if inv.repositories.is_empty() || compare_group(Some(&inv), &row.os) != group {
            continue;
        }
        peer_security.extend(
            inv.repositories
                .iter()
                .filter(|r| r.enabled && is_security(r))
                .map(|r| format!("{} {} {}", r.uri, r.suite, r.components.join(" "))),
        );
        peer_sets.push(
            inv.repositories
                .iter()
                .filter(|r| r.enabled)
                .map(repo_key)
                .collect(),
        );
    }
    peer_security.sort();
    peer_security.dedup();

    if peer_sets.is_empty() {
        // Still say what it looked for. "No peers" and "no peers running
        // Debian 11" are different statements, and the second is the one that
        // explains why a lone machine has nothing to compare against.
        return Ok(RepoDiff {
            group,
            peer_security,
            ..RepoDiff::default()
        });
    }

    let mut only_here: Vec<String> = my_keys
        .iter()
        .filter(|k| !peer_sets.iter().any(|p| p.contains(*k)))
        .cloned()
        .collect();
    // On every peer but not here.
    let mut missing_here: Vec<String> = peer_sets[0]
        .iter()
        .filter(|k| peer_sets.iter().all(|p| p.contains(*k)) && !my_keys.contains(*k))
        .cloned()
        .collect();
    only_here.sort();
    missing_here.sort();

    Ok(RepoDiff {
        peers: peer_sets.len(),
        group,
        only_here,
        missing_here,
        peer_security,
    })
}

async fn agent(
    State(state): State<SharedState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<AgentDetail>> {
    let row = state
        .db
        .agents()?
        .into_iter()
        .find(|a| a.id == id)
        .ok_or_else(|| ApiError::not_found("no such agent"))?;

    let mut inventory = state.db.inventory(id)?;
    let commands = state.db.commands(Some(id), 100)?;

    // Which repositories actually failed to deliver, taken from the last patch
    // run rather than from a probe. The agent cannot keep this reliably - a
    // reconnect gives it a fresh session - and the portal already has every
    // word apt said, including for runs that happened before this existed.
    if let Some(inv) = inventory.as_mut() {
        attach_fetch_failures(inv, &commands);
        attach_blocked(inv, &commands);
    }

    let diff = match &inventory {
        Some(inv) => repo_diff(&state, id, &row.os, Some(inv), &inv.repositories)?,
        None => RepoDiff::default(),
    };

    // Which of this host's guests PatchPanel already knows about. The agent
    // cannot answer that - it is the portal's whole view - and an unmanaged
    // guest is the most common blind spot on a hypervisor: nobody enrols the
    // VM they spun up to try something.
    if let Some(virt) = inventory.as_mut().and_then(|i| i.virt.as_mut()) {
        if !virt.guests.is_empty() {
            let known: Vec<String> = state
                .db
                .agents()?
                .into_iter()
                .map(|a| a.hostname.to_lowercase())
                .collect();
            for g in virt.guests.iter_mut() {
                let name = g.name.to_lowercase();
                // A Proxmox guest is named by the operator and an agent
                // reports its own hostname, so they usually match outright;
                // allow the short name of an FQDN on either side.
                let short = |s: &str| s.split('.').next().unwrap_or(s).to_string();
                g.managed = known
                    .iter()
                    .any(|k| *k == name || short(k) == short(&name));
            }
        }
    }

    // A guest has no idea when its host last backed it up, and the host's own
    // page is the wrong place to find out before upgrading this machine. The
    // portal is the only thing that sees both, so it joins them.
    let backup = host_backup_of(&state, &row.hostname).unwrap_or_default();

    // "Add the security suite for its release" is true and useless on its own.
    // The machines running the same release already have the line.
    if let Some(rel) = inventory.as_mut().and_then(|i| i.release.as_mut()) {
        if !diff.peer_security.is_empty() {
            for f in rel
                .findings
                .iter_mut()
                .filter(|f| f.summary.contains("security source"))
            {
                f.detail = format!(
                    "{}\n\n{} other machine(s) running {} use:\n{}",
                    f.detail.trim_end(),
                    diff.peers.max(1),
                    diff.group,
                    diff.peer_security
                        .iter()
                        .map(|s| format!("  deb {s}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                );
            }
        }
    }

    Ok(Json(AgentDetail {
        connected: state.hub.is_connected(id),
        backup,
        tracked: tracked_apps(&state, inventory.as_ref()),
        ignored: state.db.ignores(id)?,
        commands,
        inventory,
        repo_diff: diff,
        row,
    }))
}

/// Mark updates a patch run demonstrably failed to install.
///
/// The agent works this out at the moment a run finishes - it is the only
/// point where "what was pending before" and "what is pending now" both exist
/// - but it cannot hold on to it. A self-update restarts the agent, and the
/// finding went with it. The portal has the run's own words and keeps them, so
/// read it back from there.
///
/// Only names still being offered are marked: anything since installed has
/// stopped being blocked, whatever a week-old run said.
fn attach_blocked(inv: &mut pp_proto::Inventory, commands: &[crate::db::CommandRow]) {
    if !inv.blocked.is_empty() {
        return;
    }
    let Some(run) = commands.iter().find(|c| c.kind == "apply_patches") else {
        return;
    };

    let text = format!("{}\n{}", run.detail, run.progress);
    let Some(start) = text.find("still offered after the run") else {
        return;
    };

    let mut names = Vec::new();
    for line in text[start..].lines().skip(1) {
        // The block is indented names, and ends at the blank line before the
        // advice that follows it.
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        if !line.starts_with("  ") {
            break;
        }
        let name = trimmed.split(" (").next().unwrap_or(trimmed).trim();
        if !name.is_empty() {
            names.push(name.to_string());
        }
    }

    inv.blocked = names
        .into_iter()
        .filter(|n| inv.updates.iter().any(|u| u.name == *n))
        .collect();
}

/// Mark repositories that the most recent patch run could not download from.
///
/// apt prints `E: Failed to fetch <url>  404  Not Found` for each one. Those
/// URLs carry the repository they came from, which is the difference between a
/// wall of errors and knowing which line in which file has stopped working.
fn attach_fetch_failures(inv: &mut pp_proto::Inventory, commands: &[crate::db::CommandRow]) {
    let Some(run) = commands
        .iter()
        .find(|c| c.kind == "apply_patches" && c.ok == Some(false))
    else {
        return;
    };
    // A run older than the inventory has been overtaken by events.
    if run.created_at < inv.collected_at - chrono::Duration::hours(24) {
        return;
    }

    let text = format!(
        "{}\n{}\n{}",
        run.summary, run.detail, run.progress
    );
    let failed: Vec<&str> = text
        .lines()
        .filter(|l| l.contains("Failed to fetch"))
        .filter_map(|l| l.split_whitespace().find(|w| w.starts_with("http")))
        .collect();
    if failed.is_empty() {
        return;
    }

    for repo in inv.repositories.iter_mut() {
        let base = repo.uri.trim_end_matches('/');
        if base.is_empty() {
            continue;
        }
        let mine: Vec<&&str> = failed.iter().filter(|u| u.starts_with(base)).collect();
        if mine.is_empty() {
            continue;
        }
        let mut names: Vec<String> = mine
            .iter()
            .map(|u| {
                let file = u.rsplit('/').next().unwrap_or(u);
                // apt percent-encodes `+` and `~` in version strings.
                file.replace("%2b", "+").replace("%7e", "~")
            })
            .collect();
        names.sort();
        names.dedup();
        let shown: Vec<String> = names.iter().take(4).cloned().collect();

        repo.problem = Some(format!(
            "the last patch run could not download {} package(s) from here: the index lists \
             them but the server answers 404 ({}{}). Every patch run on this machine will \
             keep failing until this source is fixed or disabled - apt downloads everything \
             else first, then gives up. A release being retired empties its pool while the \
             indices remain; the files move to archive.debian.org, or are gone.",
            names.len(),
            shown.join(", "),
            if names.len() > shown.len() { ", ..." } else { "" }
        ));
    }
}

#[derive(Deserialize)]
struct IgnoreRequest {
    name: String,
    #[serde(default)]
    source: String,
    /// The exact version being set aside. Ignoring a package outright would
    /// hide the next security fix for it too.
    version: String,
}

async fn add_ignore(
    State(state): State<SharedState>,
    Path(id): Path<Uuid>,
    Json(req): Json<IgnoreRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    if req.name.trim().is_empty() || req.version.trim().is_empty() {
        return Err(ApiError::bad_request("a name and a version are required"));
    }
    state
        .db
        .add_ignore(id, req.name.trim(), req.source.trim(), req.version.trim())?;
    Ok(Json(json!({ "ignored": req.name, "version": req.version })))
}

async fn remove_ignore(
    State(state): State<SharedState>,
    Path(id): Path<Uuid>,
    Json(req): Json<IgnoreRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let removed = state.db.remove_ignore(id, req.name.trim(), req.version.trim())?;
    Ok(Json(json!({ "removed": removed })))
}

async fn delete_agent(
    State(state): State<SharedState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    if !state.db.delete_agent(id)? {
        return Err(ApiError::not_found("no such agent"));
    }
    // The agent keeps its token file, so re-enrolling it means clearing that
    // too; say so rather than letting it silently fail to reconnect.
    Ok(Json(json!({
        "deleted": id,
        "note": "remove state.json on the host before re-enrolling it"
    })))
}

// ---------------------------------------------------------------------------
// Backups
// ---------------------------------------------------------------------------

/// One guest, from the backup point of view.
#[derive(Serialize)]
struct BackedUpGuest {
    host: String,
    id: String,
    name: String,
    kind: String,
    state: String,
    managed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_backup: Option<chrono::DateTime<chrono::Utc>>,
    /// `fresh`, `stale`, `never`, or `exempt` - decided once, here, so every
    /// place that shows it agrees.
    status: &'static str,
    /// Why somebody decided this one needs backing up less often, or not at
    /// all.
    #[serde(default)]
    reason: String,
    /// The window this guest is actually held to, and a word for it. Both go
    /// on the row so a relaxed expectation is visible rather than implied by
    /// a number that quietly stopped being red.
    #[serde(default)]
    every_days: i64,
    #[serde(default)]
    cadence: String,
    /// When a snooze on this guest runs out. Shown on the row, because a
    /// silence with no end date is indistinguishable from a bug.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    snooze_until: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Serialize)]
struct HostBackups {
    host: String,
    platform: String,
    #[serde(flatten)]
    state: pp_proto::Backups,
}

#[derive(Serialize, Default)]
struct BackupSummary {
    guests: usize,
    fresh: usize,
    stale: usize,
    never: usize,
    /// Deliberately not backed up.
    exempt: usize,
    /// Running guests with no recent backup: the number that matters.
    unprotected_running: usize,
    jobs_running: usize,
    jobs_stuck: usize,
    jobs_failed: usize,
}

#[derive(Serialize)]
struct BackupsResponse {
    hosts: Vec<HostBackups>,
    guests: Vec<BackedUpGuest>,
    summary: BackupSummary,
}

#[derive(Deserialize)]
struct ExemptRequest {
    host: String,
    guest: String,
    #[serde(default)]
    reason: String,
    /// How many days may pass before this guest counts as a gap. 0 means do
    /// not track it at all; absent keeps the old meaning, which was 0.
    #[serde(default)]
    every_days: i64,
    /// Go quiet about this one until this many days from now, then go back to
    /// normal. Used instead of `every_days`, not alongside it.
    #[serde(default)]
    snooze_days: i64,
}

async fn set_exempt(
    State(state): State<SharedState>,
    Json(req): Json<ExemptRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    if req.host.trim().is_empty() || req.guest.trim().is_empty() {
        return Err(ApiError::bad_request("a host and a guest are required"));
    }
    if req.every_days < 0 {
        return Err(ApiError::bad_request("a backup window cannot be negative"));
    }
    state.db.set_backup_rule(
        req.host.trim(),
        req.guest.trim(),
        req.reason.trim(),
        req.every_days,
    )?;
    Ok(Json(json!({ "every_days": req.every_days })))
}

/// "I know. Tell me again next month."
///
/// Distinct from a cadence, which is a statement about how often this guest
/// should be backed up, and from not tracking it, which is a statement that it
/// never should. A snooze says neither - it defers the question, and the date
/// it comes back is part of what the row shows.
async fn snooze_backup(
    State(state): State<SharedState>,
    Json(req): Json<ExemptRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    if req.host.trim().is_empty() || req.guest.trim().is_empty() {
        return Err(ApiError::bad_request("a host and a guest are required"));
    }
    let days = if req.snooze_days > 0 { req.snooze_days } else { 30 };
    if days > 365 {
        return Err(ApiError::bad_request(
            "a snooze longer than a year is a decision; set the cadence instead",
        ));
    }
    let until = chrono::Utc::now() + chrono::Duration::days(days);
    state
        .db
        .snooze_backup(req.host.trim(), req.guest.trim(), until)?;
    Ok(Json(json!({ "until": until })))
}

async fn clear_exempt(
    State(state): State<SharedState>,
    Json(req): Json<ExemptRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let n = state
        .db
        .clear_backup_rule(req.host.trim(), req.guest.trim())?;
    Ok(Json(json!({ "cleared": n })))
}

/// How often this guest is meant to be backed up, and what to call it.
///
/// Absent means the fleet default. Naming the window matters as much as
/// applying it: a guest held to ninety days has to say so on its own row, or
/// the next person reads "18 days ago, fine" and cannot tell whether that is
/// a decision or a bug.
fn backup_window(rule: Option<&crate::db::BackupRule>) -> (i64, &'static str) {
    match rule.map(|r| r.every_days) {
        None => (STALE_DAYS, ""),
        // Written explicitly by a snooze on a guest that had no rule of its
        // own, so that snoozing never silently means "stop tracking this".
        Some(d) if d < 0 => (STALE_DAYS, ""),
        Some(0) => (0, "not tracked"),
        Some(d) if d <= 10 => (d, "weekly"),
        Some(d) if d <= 45 => (d, "monthly"),
        Some(d) if d <= 120 => (d, "quarterly"),
        Some(d) => (d, "yearly"),
    }
}

/// A backup older than this is stale. Weekly schedules are common, so a week
/// plus a day's grace is the line between "the schedule is working" and "this
/// has stopped happening".
const STALE_DAYS: i64 = 8;
/// A job running this long is stuck rather than slow.
const STUCK_HOURS: i64 = 12;

/// Running guests with nothing recent to restore from.
///
/// Lives here rather than inside the backups handler because the navigation
/// badge needs it on every refresh, and a second implementation would drift
/// from this one within a week.
pub fn at_risk_guests(state: &SharedState) -> Vec<crate::attention::RiskyGuest> {
    let now = chrono::Utc::now();
    let rules = state.db.backup_rules().unwrap_or_default();
    let Ok(rows) = state.db.agents() else {
        return Vec::new();
    };

    let mut at_risk = Vec::new();
    for row in rows {
        let Ok(Some(inv)) = state.db.inventory(row.id) else {
            continue;
        };
        let Some(virt) = inv.virt else { continue };
        for g in virt.guests {
            if !g.state.to_lowercase().starts_with("running") {
                continue;
            }
            let rule = rules.get(&format!("{}/{}", row.hostname, g.id));
            let (window, _) = backup_window(rule);
            // Not tracked is a decision, and a decision is not a gap.
            if window == 0 {
                continue;
            }
            // Snoozed: still a gap, just not one you asked to hear about yet.
            if rule.and_then(|r| r.snooze_until).is_some_and(|t| t > now) {
                continue;
            }
            let age = g
                .last_backup
                .map(|t| now.signed_duration_since(t).num_days());
            if age.is_some_and(|d| d <= window) {
                continue;
            }
            at_risk.push(crate::attention::RiskyGuest {
                host: row.hostname.clone(),
                name: if g.name.is_empty() {
                    g.id.clone()
                } else {
                    g.name.clone()
                },
                kind: format!("{} {}", g.kind, g.id),
                why: match age {
                    Some(d) => format!("last backup {d} days ago, wanted every {window}"),
                    None => "never backed up".into(),
                },
            });
        }
    }
    at_risk
}

async fn backups(State(state): State<SharedState>) -> ApiResult<Json<BackupsResponse>> {
    let now = chrono::Utc::now();
    let rules = state.db.backup_rules().unwrap_or_default();
    let mut hosts = Vec::new();
    let mut guests = Vec::new();
    let mut summary = BackupSummary::default();

    for row in state.db.agents()? {
        let Some(inv) = state.db.inventory(row.id)? else {
            continue;
        };
        let Some(virt) = inv.virt else { continue };
        if virt.guests.is_empty() && virt.backups.is_none() {
            continue;
        }

        if let Some(b) = &virt.backups {
            summary.jobs_running += b.running.len();
            summary.jobs_stuck += b
                .running
                .iter()
                .filter(|j| now.signed_duration_since(j.started).num_hours() >= STUCK_HOURS)
                .count();
            summary.jobs_failed += b.recent.iter().filter(|j| !j.ok).count();
            hosts.push(HostBackups {
                host: row.hostname.clone(),
                platform: virt.platform.clone(),
                state: b.clone(),
            });
        }

        for g in virt.guests {
            let key = format!("{}/{}", row.hostname, g.id);
            let rule = rules.get(&key);
            let (window, cadence) = backup_window(rule);
            let reason = rule.map(|r| r.reason.clone()).filter(|r| !r.is_empty());
            let snoozed = rule.and_then(|r| r.snooze_until).filter(|t| *t > now);
            let status = match (window, g.last_backup) {
                // Not tracking it is a decision about the guest, not about the
                // backup, so it wins over how old the last one is.
                (0, _) => "exempt",
                // A snooze only silences a gap; it never makes a fresh backup
                // look stale, and it never hides one that is fine anyway.
                (w, Some(t)) if now.signed_duration_since(t).num_days() <= w => "fresh",
                _ if snoozed.is_some() => "snoozed",
                (w, Some(t)) if now.signed_duration_since(t).num_days() <= w => "fresh",
                (_, Some(_)) => "stale",
                (_, None) => "never",
            };
            summary.guests += 1;
            match status {
                "fresh" => summary.fresh += 1,
                "stale" => summary.stale += 1,
                "exempt" => summary.exempt += 1,
                _ => summary.never += 1,
            }
            let running = g.state.to_lowercase().starts_with("running");
            if running && matches!(status, "stale" | "never") {
                summary.unprotected_running += 1;
            }
            guests.push(BackedUpGuest {
                host: row.hostname.clone(),
                id: g.id,
                name: g.name,
                kind: g.kind,
                state: g.state,
                managed: g.managed,
                last_backup: g.last_backup,
                status,
                every_days: window,
                cadence: cadence.to_string(),
                snooze_until: snoozed,
                reason: reason.unwrap_or_default(),
            });
        }
    }

    // Worst first: the ones nobody is protecting are the point of the page.
    guests.sort_by_key(|g| {
        (
            match g.status {
                "never" => 0,
                "stale" => 1,
                "fresh" => 2,
                // Exempt last: a decision already made is not a thing to read.
                _ => 3,
            },
            !g.state.to_lowercase().starts_with("running"),
            g.name.to_lowercase(),
        )
    });

    Ok(Json(BackupsResponse {
        hosts,
        guests,
        summary,
    }))
}

// ---------------------------------------------------------------------------
// Pools
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct PoolView {
    #[serde(flatten)]
    pool: crate::pools::Pool,
    members: Vec<String>,
    /// What this pool would do to each of its machines if it ran now.
    plan: Vec<crate::pools::Planned>,
    /// When it next fires, in the portal's local time.
    next_run: Option<String>,
}

async fn list_pools(State(state): State<SharedState>) -> ApiResult<Json<Vec<PoolView>>> {
    let mut out = Vec::new();
    for pool in state.db.pools()? {
        let next_run = pool
            .schedule
            .next_after(chrono::Local::now())
            .map(|t| t.to_rfc3339());
        out.push(PoolView {
            members: state.db.pool_members(&pool.name)?,
            plan: crate::pools::plan(&state, &pool)?,
            next_run,
            pool,
        });
    }
    Ok(Json(out))
}

async fn put_pool(
    State(state): State<SharedState>,
    Json(pool): Json<crate::pools::Pool>,
) -> ApiResult<Json<serde_json::Value>> {
    let name = pool.name.trim().to_string();
    if name.is_empty() {
        return Err(ApiError::bad_request("a pool needs a name"));
    }
    if pool.concurrency == 0 {
        return Err(ApiError::bad_request(
            "concurrency must be at least 1, or the pool would never patch anything",
        ));
    }
    state.db.put_pool(&crate::pools::Pool { name, ..pool })?;
    Ok(Json(json!({ "saved": true })))
}

async fn delete_pool(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    if !state.db.delete_pool(&name)? {
        return Err(ApiError::not_found("no such pool"));
    }
    // Its machines are simply unassigned again, not deleted.
    Ok(Json(json!({ "deleted": name })))
}

#[derive(Deserialize)]
struct PoolMembership {
    /// Empty takes the machine out of every pool.
    #[serde(default)]
    pool: String,
}

async fn set_pool(
    State(state): State<SharedState>,
    Path(id): Path<Uuid>,
    Json(req): Json<PoolMembership>,
) -> ApiResult<Json<serde_json::Value>> {
    let pool = req.pool.trim();
    if !pool.is_empty() && !state.db.pools()?.iter().any(|p| p.name == pool) {
        return Err(ApiError::not_found("no such pool"));
    }
    state.db.set_pool_member(id, pool)?;
    Ok(Json(json!({ "pool": pool })))
}

/// Run a pool now, ignoring its schedule.
async fn run_pool(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let Some(pool) = state.db.pools()?.into_iter().find(|p| p.name == name) else {
        return Err(ApiError::not_found("no such pool"));
    };
    if pool.scope == crate::pools::Scope::None {
        return Err(ApiError::bad_request(
            "this pool is set to patch nothing; change its scope first",
        ));
    }
    state.db.set_pool_last_run(&name, chrono::Utc::now())?;
    let started = crate::pools::run_now(&state, &name).await?;
    Ok(Json(json!({ "dispatched": started })))
}

// ---------------------------------------------------------------------------
// Devices
// ---------------------------------------------------------------------------

/// A device report, plus which collector produced it.
#[derive(Serialize)]
struct DeviceView {
    collector: AgentId,
    collector_host: String,
    site: String,
    #[serde(flatten)]
    report: DeviceReport,
    /// Label and tags come from the manifest, not from the probe.
    label: String,
    tags: Vec<String>,
    expect_version: Option<String>,
    /// The device's management page, when one is declared.
    url: String,
    /// For devices PatchPanel can keep updated: what it is allowed to install.
    /// Absent for everything else, which is most things.
    #[serde(skip_serializing_if = "Option::is_none")]
    auto_update: Option<String>,
    /// How many collectors are reporting this device. More than one means no
    /// `collector` is named on it and every agent in the site is probing it.
    collectors: usize,
    /// False for a device that is declared but has never been probed. It still
    /// belongs in the list: a device nobody has looked at yet is exactly the
    /// one worth seeing, and leaving it out made adding one feel like it had
    /// silently failed.
    probed: bool,
    /// True when the facts on this row are the last ones that were true, kept
    /// because the most recent probe failed. The row still says unreachable -
    /// this only says the version and counts beside it are remembered rather
    /// than fresh.
    #[serde(default)]
    stale: bool,
    /// When those remembered facts were actually observed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_good_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Keep what was known when a probe fails.
///
/// A failed probe means the device did not answer. It does not mean the
/// firewall stopped being end of life, or that ninety-one pending updates
/// installed themselves - but overwriting the report with the failure said
/// exactly that, and one bad minute on the collector blanked every appliance
/// card and silently retired the end-of-life warning with them.
///
/// Reachability itself is never carried forward. That is the one fact the
/// failed probe genuinely established.
fn keep_last_known(
    report: &mut pp_proto::DeviceReport,
    last: Option<&pp_proto::DeviceReport>,
) -> (bool, Option<chrono::DateTime<chrono::Utc>>) {
    if report.reachable {
        return (false, None);
    }
    let Some(good) = last else {
        return (false, None);
    };
    report.firmware = good.firmware.clone();
    report.detail = good.detail.clone();
    report.drift = good.drift;
    report.updates = good.updates;
    report.updates_known = good.updates_known;
    report.reboot_required = good.reboot_required;
    report.eol = good.eol;
    report.eol_note = good.eol_note.clone();
    (true, Some(good.checked_at))
}

/// What a device is allowed to install by itself, where that is a thing it
/// can do at all.
fn auto_update_of(spec: &pp_proto::DeviceSpec) -> Option<String> {
    match &spec.probe {
        pp_proto::Probe::HomeAssistant { auto_update, .. } => serde_json::to_value(auto_update)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string)),
        _ => None,
    }
}

fn collect_devices(state: &SharedState) -> anyhow::Result<Vec<DeviceView>> {
    let manifest = state.db.manifest()?;
    let last_good = state.db.last_good_probes().unwrap_or_default();
    let mut out = Vec::new();
    let mut seen: std::collections::HashSet<String> = Default::default();

    for row in state.db.agents()? {
        let Some(inv) = state.db.inventory(row.id)? else {
            continue;
        };
        for report in inv.devices {
            // A report whose device is no longer declared is history, not a
            // device. Reports are kept across restarts, so this is what makes
            // Remove actually remove.
            let Some(spec) = manifest.devices.iter().find(|d| d.id == report.id) else {
                continue;
            };
            let spec = Some(spec);
            seen.insert(report.id.clone());
            let mut report = report;
            let good = last_good.get(&report.id).cloned();
            let (stale, last_good_at) = keep_last_known(&mut report, good.as_ref());
            out.push(DeviceView {
                stale,
                last_good_at,
                collector: row.id,
                collector_host: row.hostname.clone(),
                site: row.site.clone(),
                label: spec.map(|s| s.label.clone()).unwrap_or_default(),
                tags: spec.map(|s| s.tags.clone()).unwrap_or_default(),
                expect_version: spec.and_then(|s| s.expect_version.clone()),
                url: spec.map(|s| s.url.clone()).unwrap_or_default(),
                auto_update: spec.and_then(auto_update_of),
                collectors: 0,
                probed: true,
                report,
            });
        }
    }

    // A declared device nobody has probed yet still gets a row. Waiting for
    // the first probe to make it appear looks exactly like the add having
    // failed, which is the moment someone most needs to see that it worked.
    for spec in &manifest.devices {
        if seen.contains(&spec.id) {
            continue;
        }
        out.push(DeviceView {
            collector: pp_proto::AgentId::nil(),
            collector_host: if spec.collector.is_empty() {
                "the portal".to_string()
            } else {
                spec.collector.clone()
            },
            site: spec.site.clone(),
            label: spec.label.clone(),
            tags: spec.tags.clone(),
            expect_version: spec.expect_version.clone(),
            url: spec.url.clone(),
            auto_update: auto_update_of(spec),
            collectors: 1,
            probed: false,
            stale: false,
            last_good_at: None,
            report: pp_proto::DeviceReport {
                id: spec.id.clone(),
                target: spec.target.clone(),
                resolved_ip: None,
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
                checked_at: chrono::DateTime::UNIX_EPOCH,
            },
        });
    }

    // One row per device, not one per collector that happened to probe it.
    //
    // A device with no collector named is probed by every agent in its site,
    // which turned one firewall into thirteen identical rows. The newest
    // report wins; `collectors` says how many machines are reporting, because
    // more than one is worth noticing rather than hiding.
    out.sort_by(|a, b| {
        a.report
            .id
            .cmp(&b.report.id)
            .then(b.report.checked_at.cmp(&a.report.checked_at))
    });
    let mut deduped: Vec<DeviceView> = Vec::new();
    for mut view in out {
        match deduped.last_mut() {
            Some(prev) if prev.report.id == view.report.id => prev.collectors += 1,
            _ => {
                view.collectors = 1;
                deduped.push(view);
            }
        }
    }
    Ok(deduped)
}

/// An application being watched by version rather than managed by a package
/// manager.
#[derive(Serialize)]
struct TrackedApp {
    name: String,
    package: String,
    installed: String,
    latest: String,
    behind: bool,
    /// The published version is older than the installed one, which means the
    /// check is pointed at the wrong place.
    suspect: bool,
    checked_at: Option<chrono::DateTime<chrono::Utc>>,
    error: Option<String>,
    note: String,
    link: String,
}

/// Watched applications this machine actually has installed.
///
/// Driven off the inventory that already exists - no extra work on the machine
/// - and reports "unknown" rather than "current" when the vendor could not be
/// reached, which is the whole reason this feature exists.
fn tracked_apps(state: &SharedState, inv: Option<&pp_proto::Inventory>) -> Vec<TrackedApp> {
    let Some(inv) = inv else {
        return Vec::new();
    };
    let (Ok(manifest), Ok(latest)) = (state.db.manifest(), state.db.latest_versions()) else {
        return Vec::new();
    };

    manifest
        .version_checks
        .iter()
        .filter_map(|check| {
            let installed = inv
                .packages
                .iter()
                .find(|p| p.name == check.package)?
                .version
                .clone();
            let known = latest.iter().find(|l| l.name == check.name);
            let published = known.map(|k| k.version.clone()).unwrap_or_default();

            // A "latest" older than what is installed means the source is
            // wrong, not that the machine is ahead of the vendor. Saying
            // "current" there would be the exact false green this feature
            // exists to prevent, so it says the check looks wrong instead.
            let suspect = !published.is_empty()
                && crate::versions::is_behind(&published, &installed);

            Some(TrackedApp {
                name: check.name.clone(),
                package: check.package.clone(),
                behind: crate::versions::is_behind(&installed, &published),
                suspect,
                installed,
                latest: published,
                checked_at: known.map(|k| k.checked_at),
                error: known.and_then(|k| k.error.clone()),
                note: check.note.clone(),
                link: check.link.clone(),
            })
        })
        .collect()
}

/// What the portal already knows about an address a sweep can turn up.
#[derive(Serialize, Clone)]
struct Known {
    /// `agent`, `appliance`, or absent for an address nothing accounts for.
    role: &'static str,
    /// The hostname or manifest label to show instead of a bare IP.
    name: String,
    /// Set for an agent, so the row can link to its machine page.
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<String>,
}

/// Every address this portal can account for, and what accounts for it.
///
/// The sweeping agent only knows the manifest devices it was handed, so it
/// reports every machine in this fleet as unaccounted for - a sweep of a /24
/// came back with 43 unknown hosts, ten of which were agents reporting to this
/// very portal. Resolving that here is the usual rule: only the portal knows the
/// whole fleet.
///
/// One function because two readers depend on the answer - the Network tab and
/// the Overview badge - and a host one of them calls unexplained while the other
/// calls it a machine is precisely the disagreement that makes a count
/// worthless.
fn accounted_for<'a>(
    devices: &[DeviceView],
    rows: impl Iterator<Item = &'a crate::db::AgentRow>,
) -> std::collections::HashMap<String, Known> {
    let mut map: std::collections::HashMap<String, Known> = std::collections::HashMap::new();
    for d in devices {
        let name = if d.label.is_empty() {
            d.report.id.clone()
        } else {
            d.label.clone()
        };
        // Both spellings of the same device. A target declared as a name never
        // equals an address a sweep found, so without `resolved_ip` - which the
        // collector filled in from the lookup it did to reach the device - every
        // declared appliance reads as an unexplained host on its own network.
        let declared = d
            .report
            .target
            .split(':')
            .next()
            .unwrap_or(&d.report.target)
            .to_string();
        for ip in [Some(declared), d.report.resolved_ip.clone()].into_iter().flatten() {
            map.insert(
                ip,
                Known {
                    role: "appliance",
                    name: name.clone(),
                    agent: None,
                },
            );
        }
    }

    // Agents last and unconditionally: a machine running an agent is more
    // completely known than the same address declared as an appliance, and if
    // something is both, the agent is the row a person wants to land on.
    for row in rows {
        if let Some(hw) = &row.hardware {
            for ip in &hw.ip_addresses {
                map.insert(
                    ip.clone(),
                    Known {
                        role: "agent",
                        name: row.hostname.clone(),
                        agent: Some(row.id.to_string()),
                    },
                );
            }
        }
    }
    map
}

#[derive(Serialize)]
struct DevicesResponse {
    devices: Vec<DeviceView>,
    /// Every host a discovery sweep saw, each labelled with whatever accounts
    /// for it. Hosts nothing accounts for carry no `known`.
    ///
    /// The whole sweep rather than only the mysteries, because "what is on this
    /// network" is the question being asked, and a list that silently omits the
    /// machines this portal manages cannot answer it - it also gives no way to
    /// tell "we did not see that machine" from "we saw it and hid it".
    network: Vec<NetworkHost>,
    /// When each collector last swept, newest first. A list rather than one
    /// timestamp because with several collectors "when was the network last
    /// looked at" has several answers, and the stale one is the interesting one.
    sweeps: Vec<Sweep>,
    /// The configured interval, so the page can say what "automatic" means
    /// instead of asking the reader to take it on faith.
    #[serde(skip_serializing_if = "Option::is_none")]
    sweep_every_secs: Option<u64>,
    /// One line per reason a sweep did not run the scanner it was asked to.
    ///
    /// Deduped here rather than on the page: every host of an affected sweep
    /// carries the same note, and a /24 would otherwise print it thirty times.
    discovery_notes: Vec<String>,
}

#[derive(Serialize)]
struct Sweep {
    collector_host: String,
    at: chrono::DateTime<chrono::Utc>,
}

#[derive(Serialize)]
struct NetworkHost {
    collector_host: String,
    site: String,
    ip: String,
    open_ports: Vec<u16>,
    hint: String,
    /// Per-port service detail, for the ports whichever scanner ran could
    /// actually name. Empty from the built-in sweep.
    services: Vec<pp_proto::DiscoveredService>,
    scanner: pp_proto::Scanner,
    /// What the scanner concluded about the host itself: OS guess with its
    /// accuracy, hardware vendor, uptime.
    identity: pp_proto::HostIdentity,
    /// What this portal already knows this address to be, if anything.
    #[serde(skip_serializing_if = "Option::is_none")]
    known: Option<Known>,
}

async fn devices(State(state): State<SharedState>) -> ApiResult<Json<DevicesResponse>> {
    let devices = collect_devices(&state)?;
    let rows = state.db.agents()?;
    let known = accounted_for(&devices, rows.iter());

    let mut network = Vec::new();
    let mut discovery_notes: Vec<String> = Vec::new();
    let mut sweeps: Vec<Sweep> = Vec::new();
    let mut seen: std::collections::HashSet<String> = Default::default();
    for row in &rows {
        let Some(inv) = state.db.inventory(row.id)? else {
            continue;
        };
        if let Some(at) = inv.swept_at {
            sweeps.push(Sweep {
                collector_host: row.hostname.clone(),
                at,
            });
        }
        for host in inv.discovered {
            // Before the dedupe below, because a sweep that fell back to a
            // different scanner still did so even if every host it found turns
            // out to be one this portal already knows about.
            if !host.scan_note.is_empty() && !discovery_notes.contains(&host.scan_note) {
                discovery_notes.push(host.scan_note.clone());
            }
            // Two agents sweeping overlapping ranges find the same host twice.
            if !seen.insert(host.ip.clone()) {
                continue;
            }
            network.push(NetworkHost {
                collector_host: row.hostname.clone(),
                site: row.site.clone(),
                known: known.get(&host.ip).cloned(),
                ip: host.ip,
                open_ports: host.open_ports,
                hint: host.hint,
                identity: host.identity,
                services: host.services,
                scanner: host.scanner,
            });
        }
    }
    // Numerically, so a /24 reads as a list of neighbours rather than as
    // whatever order the scan happened to finish in.
    network.sort_by_key(|u| {
        let mut octets = [0u16; 4];
        for (i, part) in u.ip.split('.').take(4).enumerate() {
            octets[i] = part.parse().unwrap_or(0);
        }
        octets
    });
    // Newest first: the headline is how fresh the freshest sweep is.
    sweeps.sort_by(|a, b| b.at.cmp(&a.at));

    Ok(Json(DevicesResponse {
        devices,
        network,
        sweeps,
        sweep_every_secs: Some(state.db.manifest()?.discovery_secs),
        discovery_notes,
    }))
}

#[derive(Deserialize)]
struct RenameRequest {
    to: String,
}

/// Change a device's id, taking its history with it.
async fn rename_device(
    State(state): State<SharedState>,
    Path(id): Path<String>,
    Json(req): Json<RenameRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let to = req.to.trim().to_string();
    if to.is_empty() {
        return Err(ApiError::bad_request("the new id cannot be empty"));
    }
    if to == id {
        return Ok(Json(json!({ "renamed": false })));
    }

    let mut manifest = state.db.manifest()?;
    if manifest.devices.iter().any(|d| d.id == to) {
        return Err(ApiError::conflict(format!(
            "there is already a device called {to}"
        )));
    }
    let Some(dev) = manifest.devices.iter_mut().find(|d| d.id == id) else {
        return Err(ApiError::not_found("no such device"));
    };
    dev.id = to.clone();

    let moved = state.db.rename_device(&id, &to)?;
    let stored = state.db.put_manifest(manifest)?;
    state.hub.broadcast(&ServerMsg::Manifest(stored.clone()));

    Ok(Json(json!({
        "renamed": true,
        "from": id,
        "to": to,
        "history_moved": moved,
        "revision": stored.revision,
    })))
}

/// What a device has looked like over time.
async fn device_history(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<crate::db::DeviceProbeRow>>> {
    Ok(Json(state.db.device_history(&id, 50)?))
}

// ---------------------------------------------------------------------------
// Manifest
// ---------------------------------------------------------------------------

async fn get_manifest(State(state): State<SharedState>) -> ApiResult<Json<Manifest>> {
    Ok(Json(state.db.manifest()?))
}

/// Replace the manifest and push it to everyone connected. The revision is
/// assigned by the portal, so a submitted one is ignored rather than trusted.
async fn put_manifest(
    State(state): State<SharedState>,
    Json(manifest): Json<Manifest>,
) -> ApiResult<Json<serde_json::Value>> {
    validate_manifest(&manifest)?;
    let stored = state.db.put_manifest(manifest)?;
    // Removing a device is a manifest edit, so this is where "stop monitoring
    // it" takes effect - including forgetting what it last looked like.
    let keep: Vec<String> = stored.devices.iter().map(|d| d.id.clone()).collect();
    if let Err(e) = state.db.prune_last_good(&keep) {
        tracing::warn!(error = %e, "could not prune remembered device readings");
    }
    let pushed = state.hub.broadcast(&ServerMsg::Manifest(stored.clone()));

    // Agents are told to self-update when they connect, but an already
    // connected fleet would then sit on the old version until something
    // happened to drop its connection. Publishing a version is an instruction,
    // so act on it now.
    let upgrading = dispatch_self_updates(&state, &stored)?;

    tracing::info!(
        revision = stored.revision,
        pushed,
        upgrading,
        "manifest published"
    );
    Ok(Json(json!({
        "revision": stored.revision,
        "pushed_to": pushed,
        "upgrading": upgrading,
    })))
}

/// Send `SelfUpdate` to every connected agent running a version other than the
/// one the manifest asks for. Returns how many were dispatched.
fn dispatch_self_updates(state: &SharedState, manifest: &Manifest) -> anyhow::Result<usize> {
    if manifest.agent_version.is_none() {
        return Ok(0);
    }

    let rows = state.db.agents()?;
    let mut sent = 0;
    for id in state.hub.connected() {
        let Some(row) = rows.iter().find(|r| r.id == id) else {
            continue;
        };
        // `build_for` keys on the OS/arch strings the agent reported, so
        // reconstruct just enough of its SystemInfo to ask the same question.
        let system = pp_proto::SystemInfo {
            hostname: row.hostname.clone(),
            os: match row.os.as_str() {
                "linux" => pp_proto::OsKind::Linux,
                "windows" => pp_proto::OsKind::Windows,
                _ => pp_proto::OsKind::Other,
            },
            os_version: row.os_version.clone(),
            arch: row.arch.clone(),
            agent_version: row.agent_version.clone(),
            backends: row.backends.clone(),
            site: row.site.clone(),
            hardware: Default::default(),
            boot_time: None,
        };

        let Some(cmd) = crate::ws::self_update_command(state, manifest, &system) else {
            continue;
        };
        let cmd_id = Uuid::new_v4();
        state.db.record_command(cmd_id, id, &cmd, "manual")?;
        if state.hub.send(
            id,
            ServerMsg::Command(CommandEnvelope {
                id: cmd_id,
                command: cmd,
            }),
        ) {
            sent += 1;
            tracing::info!(agent = %row.hostname, from = %row.agent_version, "dispatched self-update");
        }
    }
    Ok(sent)
}

/// Catch the mistakes that would otherwise only show up as a red row on every
/// agent in the fleet at once.
fn validate_manifest(m: &Manifest) -> ApiResult<()> {
    let mut seen = std::collections::HashSet::new();
    for app in &m.apps {
        if !seen.insert(&app.name) {
            return Err(ApiError::bad_request(format!(
                "duplicate app name `{}`",
                app.name
            )));
        }
        if let pp_proto::AppSource::Url { sha256, .. } = &app.source {
            if sha256.trim().len() != 64 {
                return Err(ApiError::bad_request(format!(
                    "app `{}`: url sources need a 64-character sha256",
                    app.name
                )));
            }
        }
    }

    let mut ids = std::collections::HashSet::new();
    for dev in &m.devices {
        if dev.id.trim().is_empty() {
            return Err(ApiError::bad_request("every device needs an id"));
        }
        if !ids.insert(&dev.id) {
            return Err(ApiError::bad_request(format!(
                "duplicate device id `{}`",
                dev.id
            )));
        }
        if dev.target.trim().is_empty() {
            return Err(ApiError::bad_request(format!(
                "device `{}` has no target",
                dev.id
            )));
        }
    }

    for scan in &m.discovery {
        if scan.cidr.parse::<ipnet::IpNet>().is_err() {
            return Err(ApiError::bad_request(format!(
                "`{}` is not a valid CIDR",
                scan.cidr
            )));
        }
    }

    if m.heartbeat_secs < 5 {
        return Err(ApiError::bad_request("heartbeat_secs must be at least 5"));
    }
    if m.inventory_secs < 60 {
        return Err(ApiError::bad_request("inventory_secs must be at least 60"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct DispatchRequest {
    command: Command,
}

#[derive(Serialize)]
struct DispatchResponse {
    id: Uuid,
    dispatched_to: usize,
}

/// Send a command to a machine, applying the same rules the HTTP path does.
///
/// Shared with the pool scheduler so an automatic run cannot bypass the
/// one-command-at-a-time rule that a manual one obeys.
pub fn dispatch_now(
    state: &SharedState,
    id: AgentId,
    command: pp_proto::Command,
    source: &str,
) -> anyhow::Result<Uuid> {
    if !state.hub.is_connected(id) {
        anyhow::bail!("agent is not connected");
    }
    if let Some(run) = state.db.running_for(id)? {
        anyhow::bail!("{} is already running", run.kind);
    }

    let cmd_id = Uuid::new_v4();
    state.db.record_command(cmd_id, id, &command, source)?;
    if !state.hub.send(
        id,
        ServerMsg::Command(CommandEnvelope {
            id: cmd_id,
            command,
        }),
    ) {
        anyhow::bail!("agent disconnected while dispatching");
    }
    Ok(cmd_id)
}

async fn dispatch(
    State(state): State<SharedState>,
    Path(id): Path<AgentId>,
    Json(req): Json<DispatchRequest>,
) -> ApiResult<Json<DispatchResponse>> {
    if !state.hub.is_connected(id) {
        // Queuing for an offline agent would mean a patch run firing whenever
        // the machine happens to come back, which is not what anyone means by
        // clicking a button now.
        return Err(ApiError::conflict(
            "agent is not connected; commands are not queued",
        ));
    }

    // A forwarding request is also a durable decision, so it is recorded here
    // as well as dispatched - the portal re-asks on every reconnect, and the
    // agent is never asked to remember it.
    if let pp_proto::Command::ConfigureSyslog {
        enable,
        ref min_severity,
    } = req.command
    {
        state.db.set_log_forward(id, enable, min_severity)?;
    }

    // One changing command at a time. Two apt runs collide on the dpkg lock,
    // and a reboot in the middle of a release upgrade is worse than that: the
    // machine comes back half-migrated. Scans and an agent restart stay
    // available, the restart deliberately - it is the way out when something
    // is wedged.
    const ALWAYS: [&str; 4] = [
        "collect_inventory",
        "probe_devices",
        "discover",
        "restart_agent",
    ];
    let kind = crate::db::command_kind(&req.command);
    if !ALWAYS.contains(&kind) {
        if let Some(run) = state.db.running_for(id)? {
            return Err(ApiError::conflict(format!(
                "{} is already running on this machine (activity {}). Wait for it to finish, \
                 or restart the agent if it is stuck.",
                run.kind,
                &run.id.to_string()[..8]
            )));
        }
    }

    let cmd_id = Uuid::new_v4();
    state.db.record_command(cmd_id, id, &req.command, "manual")?;
    let sent = state.hub.send(
        id,
        ServerMsg::Command(CommandEnvelope {
            id: cmd_id,
            command: req.command,
        }),
    );

    if !sent {
        return Err(ApiError::conflict("agent disconnected while dispatching"));
    }
    Ok(Json(DispatchResponse {
        id: cmd_id,
        dispatched_to: 1,
    }))
}

#[derive(Deserialize)]
struct BroadcastRequest {
    command: Command,
    /// Limit to one collector site; omit for the whole fleet.
    #[serde(default)]
    site: Option<String>,
}

async fn broadcast(
    State(state): State<SharedState>,
    Json(req): Json<BroadcastRequest>,
) -> ApiResult<Json<DispatchResponse>> {
    let rows = state.db.agents()?;

    // A sweep goes only to collectors whose site owns a discovery range.
    //
    // Discovery ranges are site-scoped, so sending this to the whole fleet made
    // twelve of thirteen agents answer "no discovery ranges configured for site
    // `homelab`" - twelve recorded failures every time somebody pressed Scan
    // now, for a sweep that worked. A command that cannot apply to an agent is
    // not dispatched to it rather than dispatched and failed.
    let sweep_sites: Option<Vec<String>> = if matches!(req.command, Command::Discover) {
        let m = state.db.manifest()?;
        Some(m.discovery.iter().map(|d| d.site.clone()).collect())
    } else {
        None
    };

    let targets: Vec<AgentId> = state
        .hub
        .connected()
        .into_iter()
        .filter(|id| {
            let Some(row) = rows.iter().find(|r| r.id == *id) else {
                return false;
            };
            if let Some(site) = &req.site {
                if row.site != *site {
                    return false;
                }
            }
            match &sweep_sites {
                // An empty `site` on a range means "any", the same as elsewhere.
                Some(sites) => sites.iter().any(|s| s.is_empty() || *s == row.site),
                None => true,
            }
        })
        .collect();

    // One id across the fan-out would collide in the command log, so each
    // target gets its own.
    let mut dispatched = 0;
    let mut first = None;
    for id in targets {
        let cmd_id = Uuid::new_v4();
        state.db.record_command(cmd_id, id, &req.command, "manual")?;
        if state.hub.send(
            id,
            ServerMsg::Command(CommandEnvelope {
                id: cmd_id,
                command: req.command.clone(),
            }),
        ) {
            dispatched += 1;
            first.get_or_insert(cmd_id);
        }
    }

    tracing::info!(dispatched, site = ?req.site, "broadcast command");
    Ok(Json(DispatchResponse {
        id: first.unwrap_or_default(),
        dispatched_to: dispatched,
    }))
}

#[derive(Deserialize)]
struct LogQuery {
    #[serde(default)]
    agent: Option<AgentId>,
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    50
}

#[derive(Deserialize)]
struct JobCheckin {
    name: String,
    #[serde(default = "yes")]
    ok: bool,
    #[serde(default)]
    detail: String,
    /// How often this is meant to run. Sticky - send it once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    every_hours: Option<i64>,
    /// Anything the job knows that PatchPanel does not: a public IP, a record
    /// count, the number of files it wrote.
    #[serde(default)]
    facts: std::collections::BTreeMap<String, String>,
    /// When the run being reported actually happened. Omit it and the report
    /// is about now, which is what a script reporting on itself means. A
    /// watcher reporting on other scripts must send it, or a job could never
    /// be late as long as the watcher was alive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    at: Option<chrono::DateTime<chrono::Utc>>,
    /// What the run printed. The tail is what matters, so an over-long one is
    /// trimmed from the front rather than rejected - a log too big to accept
    /// is exactly the log somebody needs to read.
    #[serde(default)]
    log: String,
}

/// How much of a run's output to keep. Generous for one run, small enough that
/// an hourly job cannot turn the database into a log server.
const MAX_LOG_BYTES: usize = 32 * 1024;

/// The last `MAX_LOG_BYTES` of a log, cut at a line boundary.
fn log_tail(text: &str) -> String {
    if text.len() <= MAX_LOG_BYTES {
        return text.to_string();
    }
    let start = text.len() - MAX_LOG_BYTES;
    let start = text[start..]
        .find('\n')
        .map(|i| start + i + 1)
        .unwrap_or(start);
    format!(
        "[earlier output trimmed - {} of {} bytes kept]\n{}",
        text.len() - start,
        text.len(),
        &text[start..]
    )
}

fn yes() -> bool {
    true
}

/// A job outside PatchPanel saying it ran.
///
/// Deliberately the least demanding endpoint here: a name is the only required
/// field, because anything a shell script has to get right in order to report
/// is something that will eventually stop being reported.
async fn job_checkin(
    State(state): State<SharedState>,
    Json(req): Json<JobCheckin>,
) -> ApiResult<Json<serde_json::Value>> {
    let name = req.name.trim();
    if name.is_empty() {
        return Err(ApiError::bad_request("a job needs a name"));
    }
    if name.len() > 80 {
        return Err(ApiError::bad_request("that name is too long to be a name"));
    }
    // A time in the future is a clock-skew bug on the reporting box, and
    // accepting it would park the job permanently "on time".
    if req.at.is_some_and(|t| t > chrono::Utc::now() + chrono::Duration::minutes(5)) {
        return Err(ApiError::bad_request(
            "that run is in the future; check the clock on the reporting machine",
        ));
    }
    // Read what the run actually printed. A watcher usually cannot know
    // whether a script succeeded - it only sees that the script ended - so
    // anything the output states plainly is better evidence than the `ok`
    // flag that came with it.
    let log = log_tail(&req.log);
    let scan = crate::logscan::scan(&log);

    // Values the run stated join the facts it declared. Declared ones win: a
    // script that says something explicitly means it.
    let mut facts = req.facts.clone();
    for (k, v) in &scan.values {
        facts.entry(k.clone()).or_insert_with(|| v.clone());
    }

    // Patterns that ship with PatchPanel, each gated on a marker only the
    // output it understands contains. These are why a job says something
    // useful on its first check-in instead of after somebody writes a regex.
    for (_, values) in crate::logscan::builtin_values(&log) {
        facts.extend(values);
    }

    // The honest-counting case. The job says it worked and its own output says
    // otherwise, so the result is neither believed nor discarded - it is
    // recorded as reported, and flagged as contradicted.
    let suspect = req.ok && !scan.failures.is_empty();
    let detail = if suspect {
        format!(
            "{}{}[output reports {} problem(s): {}]",
            req.detail.trim(),
            if req.detail.trim().is_empty() { "" } else { " " },
            scan.failures.len(),
            scan.failures.join(" | ").chars().take(300).collect::<String>()
        )
    } else {
        req.detail.trim().to_string()
    };

    let changed = state.db.record_job_run(
        name,
        req.ok,
        &detail,
        req.every_hours,
        &facts,
        req.at,
        &log,
        suspect,
    )?;
    for c in &changed {
        tracing::info!(job = name, key = %c.key, value = %c.value, "job reported a new value");
    }
    Ok(Json(json!({
        "recorded": true,
        "changed": changed,
        "suspect": suspect,
        "read_from_output": scan.values.len(),
    })))
}

#[derive(Serialize)]
struct JobView {
    #[serde(flatten)]
    job: crate::db::Job,
    /// `ok`, `failed`, `overdue`, or `quiet` - decided here so the list, the
    /// badge and the attention row cannot disagree about it.
    status: &'static str,
    /// When it should next have reported by, when it said how often it runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    due_at: Option<chrono::DateTime<chrono::Utc>>,
    /// When each fact last moved, newest first.
    history: Vec<crate::db::JobFactChange>,
}

/// Whether a job is late enough to be worth saying so.
fn job_status(
    job: &crate::db::Job,
    now: chrono::DateTime<chrono::Utc>,
) -> (&'static str, Option<chrono::DateTime<chrono::Utc>>) {
    // Muted outranks everything, including a failure. That is the whole
    // point of saying you do not want to hear about it - but it stays visible
    // on the page, greyed, so the decision can be seen and undone.
    if job.muted {
        return ("muted", None);
    }
    // Reported as fine, with output that says otherwise. Not "failed" - the
    // job never claimed that - but not something to paint green either.
    if job.last_suspect && job.last_ok != Some(false) {
        return ("suspect", None);
    }
    let Some(last) = job.last_at else {
        return ("quiet", None);
    };
    // Nobody said how often it runs, so it cannot be late. Inventing an
    // expectation here would produce a warning nobody asked for and cannot
    // silence except by turning the whole thing off.
    if job.every_hours <= 0 {
        return (if job.last_ok == Some(false) { "failed" } else { "ok" }, None);
    }
    let due = last + chrono::Duration::hours(job.every_hours);
    if now > due + crate::db::Db::job_grace(job.every_hours) {
        // Overdue outranks a failure: a job that failed loudly yesterday is a
        // smaller problem than one that has not been heard from since.
        return ("overdue", Some(due));
    }
    (
        if job.last_ok == Some(false) { "failed" } else { "ok" },
        Some(due),
    )
}

async fn list_jobs(State(state): State<SharedState>) -> ApiResult<Json<Vec<JobView>>> {
    let now = chrono::Utc::now();
    let mut out = Vec::new();
    for job in state.db.jobs()? {
        let (status, due_at) = job_status(&job, now);
        let history = state.db.job_fact_history(&job.name, 20).unwrap_or_default();
        out.push(JobView {
            job,
            status,
            due_at,
            history,
        });
    }
    Ok(Json(out))
}

async fn job_runs(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> ApiResult<Json<Vec<crate::db::JobRun>>> {
    Ok(Json(state.db.job_runs(&name, 40)?))
}

#[derive(Deserialize)]
struct MuteRequest {
    #[serde(default)]
    muted: bool,
}

async fn mute_job(
    State(state): State<SharedState>,
    Path(name): Path<String>,
    Json(req): Json<MuteRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let n = state.db.set_job_muted(&name, req.muted)?;
    Ok(Json(json!({ "updated": n, "muted": req.muted })))
}

async fn forget_job(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let n = state.db.forget_job(&name)?;
    Ok(Json(json!({ "forgotten": n })))
}

/// Who has sent syslog, and which device each address belongs to.
///
/// The address is the identity, not the hostname in the message: one is
/// observed and the other is whatever the sender chose to claim.
async fn log_senders(State(state): State<SharedState>) -> ApiResult<Json<serde_json::Value>> {
    let retention = state.db.log_retention().unwrap_or_default();
    let senders: Vec<serde_json::Value> = crate::syslog::senders(&state.log_dir)
        .into_iter()
        .map(|s| {
            let owner = crate::syslog::owner(&state, &s.source);
            // Where the level was decided, and what it is.
            //
            // For a machine with an agent, PatchPanel set it and can say so. For
            // an appliance pushing syslog, the level lives in that device's own
            // configuration and PatchPanel genuinely does not know it - so the
            // row says that rather than guessing or showing a blank.
            let asked = state
                .db
                .agents()
                .ok()
                .and_then(|rows| rows.into_iter().find(|a| a.hostname == s.source))
                .and_then(|a| state.db.log_forward(a.id).ok().flatten());
            json!({
                "source": s.source,
                "device": owner,
                "bytes": s.bytes,
                "lines": s.lines,
                "last_line_at": s.modified,
                "min_severity": asked.as_ref().map(|(sev, _)| sev.clone()),
                "set_here": asked.is_some(),
                // The effective window for this sender, not the default -
                // showing 24 to somebody who set 6 is a control that appears
                // not to work.
                "retain_hours": retention
                    .get(&s.source)
                    .copied()
                    .unwrap_or(crate::syslog::RETAIN_HOURS),
            })
        })
        .collect();
    // Machines that were asked to forward and have not. This is the check that
    // catches a bug in the shipping code itself: duplicating the mechanism
    // would only give two things to be broken, whereas comparing what was asked
    // against what arrived notices the difference.
    let mut silent = Vec::new();
    if let Ok(rows) = state.db.agents() {
        for row in rows {
            let Ok(Some((sev, since))) = state.db.log_forward(row.id) else {
                continue;
            };
            // How long silence has to last before it means anything.
            //
            // Measured, not guessed: `hub` holds ten warning-or-worse lines per
            // day - one every two and a half hours - so a thirty minute window
            // flagged a perfectly healthy machine over and over. Six hours is
            // longer than the gap between lines on the quietest host here and
            // still catches a broken tailer within a working day.
            //
            // The proper fix would be to judge against what that host's journal
            // actually holds, which `journal_volume` can now measure. Until the
            // rate is stored, a window wider than the quietest real host is the
            // honest approximation.
            const GRACE_HOURS: i64 = 6;
            if chrono::Utc::now().signed_duration_since(since).num_hours() < GRACE_HOURS {
                continue;
            }
            let file = crate::syslog::senders(&state.log_dir)
                .into_iter()
                .find(|s| s.source == row.hostname);
            // Only "nothing has ever arrived" counts. The agent ships new
            // lines only, so a genuinely quiet host legitimately sends nothing
            // for hours - flagging that would make this warning the thing
            // people learn to ignore, which is the opposite of the point.
            let never = file.is_none();
            if never {
                silent.push(json!({
                    "machine": row.hostname,
                    "min_severity": sev,
                    "online": row.online,
                    "asked_at": since,
                    "last_line_at": file.and_then(|f| f.modified),
                }));
            }
        }
    }

    // Ordered by the name actually shown, so the list reads alphabetically down
    // the column the eye follows rather than by the address behind it.
    let mut senders = senders;
    senders.sort_by_key(|v| {
        let d = v.get("device").and_then(|x| x.as_str()).unwrap_or("");
        let s = v.get("source").and_then(|x| x.as_str()).unwrap_or("");
        if d.is_empty() { s.to_lowercase() } else { d.to_lowercase() }
    });

    Ok(Json(json!({
        "receiving": state.syslog_on,
        "retain_hours": crate::syslog::RETAIN_HOURS,
        "senders": senders,
        "asked_but_silent": silent,
    })))
}

#[derive(Deserialize)]
struct TailQuery {
    #[serde(default)]
    limit: Option<usize>,
    /// Only lines containing this, matched case-insensitively. Enough to answer
    /// "what did it say about the reboot" without a query language.
    #[serde(default)]
    contains: Option<String>,
}

async fn device_log_tail(
    State(state): State<SharedState>,
    Path(source): Path<String>,
    axum::extract::Query(q): axum::extract::Query<TailQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let limit = q.limit.unwrap_or(300).clamp(1, 5000);
    let contains = q.contains.unwrap_or_default();
    let lines = crate::syslog::tail(&state.log_dir, &source, limit, &contains)
        .map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
    Ok(Json(json!({
        "source": source,
        "device": crate::syslog::owner(&state, &source),
        "lines": lines,
        // What the slice is a slice of. Showing 400 lines without saying there
        // are 12,000 invites the reader to believe they have seen everything.
        "total": crate::syslog::line_count(&state.log_dir, &source),
    })))
}

#[derive(Deserialize)]
struct RetentionRequest {
    /// Hours to keep. Null returns this sender to the default.
    hours: Option<i64>,
}

async fn set_retention(
    State(state): State<SharedState>,
    Path(source): Path<String>,
    Json(req): Json<RetentionRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    if let Some(h) = req.hours {
        if h < 1 || h > crate::syslog::MAX_RETAIN_HOURS {
            return Err(ApiError::bad_request(format!(
                "keep between 1 and {} hours; this is not a log server",
                crate::syslog::MAX_RETAIN_HOURS
            )));
        }
    }
    state.db.set_log_retention(&source, req.hours)?;
    // Applied on the scheduler's next tick, but shortening it should take
    // effect now - otherwise "keep 6 hours" leaves a day on disk until
    // something else happens to run.
    let per_source = state.db.log_retention().unwrap_or_default();
    let _ = crate::syslog::prune(&state.log_dir, &per_source);
    Ok(Json(json!({ "hours": req.hours })))
}

#[derive(Deserialize)]
struct LevelRequest {
    min_severity: String,
}

/// Change what a machine forwards, from the page that shows what it has sent.
///
/// Only possible for a sender PatchPanel actually drives. An appliance pushing
/// syslog decides its own level in its own configuration, and pretending
/// otherwise would give the operator a control that silently does nothing.
async fn set_level(
    State(state): State<SharedState>,
    Path(source): Path<String>,
    Json(req): Json<LevelRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let row = state
        .db
        .agents()?
        .into_iter()
        .find(|a| a.hostname == source)
        .ok_or_else(|| {
            ApiError::bad_request(
                "this sender has no agent, so its level is set on the device itself",
            )
        })?;
    let cmd = pp_proto::Command::ConfigureSyslog {
        enable: true,
        min_severity: req.min_severity.clone(),
    };
    state.db.set_log_forward(row.id, true, &req.min_severity)?;
    dispatch_now(&state, row.id, cmd, "manual")?;
    Ok(Json(json!({ "min_severity": req.min_severity })))
}

/// The whole file, as a download.
async fn device_log_export(
    State(state): State<SharedState>,
    Path(source): Path<String>,
    axum::extract::Query(q): axum::extract::Query<TailQuery>,
) -> Result<axum::response::Response, ApiError> {
    use axum::response::IntoResponse;
    let contains = q.contains.unwrap_or_default();
    let body = crate::syslog::whole(&state.log_dir, &source, &contains)
        .map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M");
    let safe: String = source
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' })
        .collect();
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8".to_string()),
            (
                axum::http::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{safe}-{stamp}.log\""),
            ),
        ],
        body,
    )
        .into_response())
}

/// What actually happened, day by day.
///
/// There is no run entity in the database, and adding one would mean the
/// scheduler and the log could disagree about whether a run happened. So a run
/// is reconstructed the same way a person would: commands from the same pool,
/// close together in time, are one run. Anything a person pressed is its own
/// event, because "why did that machine reboot on Tuesday" is the question this
/// view is uniquely good at and the answer is usually a person.
#[derive(Serialize)]
struct Event {
    at: chrono::DateTime<chrono::Utc>,
    /// "pool" or "manual" - they are drawn differently because they mean
    /// different things about who is responsible.
    source: String,
    label: String,
    detail: String,
    /// ran | nothing | failed. Missed is derived on the fleet row, not here:
    /// this endpoint reports what happened, not what should have.
    outcome: String,
}

/// How long after the first command a later one still counts as the same run.
const RUN_GROUP_HOURS: i64 = 4;
/// How far back the calendar looks.
const HISTORY_DAYS: i64 = 35;

async fn schedule(State(state): State<SharedState>) -> ApiResult<Json<Vec<Event>>> {
    let since = chrono::Utc::now() - chrono::Duration::days(HISTORY_DAYS);
    let rows = state.db.commands_since(since)?;
    let hosts: std::collections::HashMap<String, String> = state
        .db
        .agents()?
        .into_iter()
        .map(|a| (a.id.to_string(), a.hostname))
        .collect();

    // Only the kinds that change a machine. A rescan is not an event on a
    // calendar; it is how the page knows anything at all.
    let interesting = |k: &str| {
        matches!(
            k,
            "apply_patches" | "reboot" | "distro_upgrade" | "finish_upgrade" | "update_firmware"
        )
    };

    let mut out: Vec<Event> = Vec::new();
    // pool name -> (started, machines, patched, failed)
    let mut open: Vec<(String, chrono::DateTime<chrono::Utc>, usize, usize, usize)> = Vec::new();

    for c in rows.into_iter().filter(|c| interesting(&c.kind)) {
        let Some(pool) = c.source.strip_prefix("pool:") else {
            out.push(Event {
                at: c.created_at,
                // `source` was added to the table later, so rows older than
                // that carry nothing. Calling those "manual" would attribute
                // a scheduled run to a person, which is exactly the kind of
                // confident wrong answer this view exists to avoid.
                source: if c.source.is_empty() {
                    "unknown".into()
                } else {
                    "manual".into()
                },
                label: hosts
                    .get(&c.agent_id.to_string())
                    .cloned()
                    .unwrap_or_else(|| "unknown".into()),
                detail: KIND_WORDS
                    .iter()
                    .find(|(k, _)| *k == c.kind)
                    .map(|(_, w)| (*w).to_string())
                    .unwrap_or(c.kind.clone()),
                outcome: match c.ok {
                    Some(false) => "failed".into(),
                    _ => "ran".into(),
                },
            });
            continue;
        };

        // Same pool, still inside the window: the same run.
        match open.iter_mut().find(|(name, started, ..)| {
            name == pool
                && c.created_at.signed_duration_since(*started).num_hours() < RUN_GROUP_HOURS
        }) {
            Some(run) => {
                run.2 += 1;
                if c.ok == Some(false) {
                    run.4 += 1;
                } else if c.kind == "apply_patches" {
                    run.3 += 1;
                }
            }
            None => open.push((
                pool.to_string(),
                c.created_at,
                1,
                usize::from(c.ok != Some(false) && c.kind == "apply_patches"),
                usize::from(c.ok == Some(false)),
            )),
        }
    }

    for (name, started, machines, patched, failed) in open {
        let detail = match (patched, failed) {
            (0, 0) => format!("{machines} machine(s), nothing to install"),
            (p, 0) => format!("{p} patched"),
            (p, f) => format!("{p} patched, {f} failed"),
        };
        out.push(Event {
            at: started,
            source: "pool".into(),
            label: name,
            detail,
            outcome: if failed > 0 {
                "failed".into()
            } else if patched > 0 {
                "ran".into()
            } else {
                "nothing".into()
            },
        });
    }

    for (name, at, ok, detail) in state.db.job_runs_since(since).unwrap_or_default() {
        out.push(Event {
            at,
            source: "job".into(),
            label: name,
            detail,
            outcome: if ok { "ran".into() } else { "failed".into() },
        });
    }

    out.sort_by_key(|e| e.at);
    Ok(Json(out))
}

/// How each command kind reads in a sentence.
const KIND_WORDS: &[(&str, &str)] = &[
    ("apply_patches", "install updates"),
    ("reboot", "reboot"),
    ("distro_upgrade", "release upgrade"),
    ("finish_upgrade", "finish upgrade"),
    ("update_firmware", "firmware update"),
];

async fn command_log(
    State(state): State<SharedState>,
    Query(q): Query<LogQuery>,
) -> ApiResult<Json<Vec<crate::db::CommandRow>>> {
    Ok(Json(state.db.commands(q.agent, q.limit.min(500))?))
}

/// The shared secret a new agent needs, so the dashboard can render a
/// ready-to-paste enrolment command instead of making the operator go and
/// read it off the portal's filesystem.
async fn enrollment(State(state): State<SharedState>) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(json!({
        "token": state.enrollment_token,
        "ws_path": "/api/agent/ws",
    })))
}

// ---------------------------------------------------------------------------
// Agent builds
// ---------------------------------------------------------------------------

async fn list_builds(State(state): State<SharedState>) -> ApiResult<Json<Vec<AgentBuild>>> {
    Ok(Json(state.db.builds()?))
}

/// Publish a build so `manifest.agent_version` has something to point at.
async fn add_build(
    State(state): State<SharedState>,
    Json(build): Json<AgentBuild>,
) -> ApiResult<Json<serde_json::Value>> {
    if build.sha256.trim().len() != 64 {
        return Err(ApiError::bad_request("sha256 must be 64 hex characters"));
    }
    if !build.url.starts_with("http://") && !build.url.starts_with("https://") {
        return Err(ApiError::bad_request("url must be http or https"));
    }
    state.db.put_build(&build)?;
    Ok(Json(json!({ "stored": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(every_hours: i64, ran_hours_ago: i64, ok: bool) -> crate::db::Job {
        let now = chrono::Utc::now();
        crate::db::Job {
            name: "router-backup".into(),
            every_hours,
            first_seen: now - chrono::Duration::days(30),
            last_at: Some(now - chrono::Duration::hours(ran_hours_ago)),
            last_ok: Some(ok),
            last_detail: String::new(),
            facts: Default::default(),
            muted: false,
            last_suspect: false,
        }
    }

    /// A job that reports success while its own output reports failures is
    /// neither believed nor called a liar: it is shown as contradicted, and
    /// never painted green.
    #[test]
    fn output_that_disagrees_outranks_a_green_report() {
        let now = chrono::Utc::now();
        let mut j = job(24, 1, true);
        assert_eq!(job_status(&j, now).0, "ok");
        j.last_suspect = true;
        assert_eq!(job_status(&j, now).0, "suspect");

        // A job that said it failed is simply failed - there is nothing to
        // contradict, and "suspect" would be a weaker word than it earned.
        let mut failed = job(24, 1, false);
        failed.last_suspect = true;
        assert_eq!(job_status(&failed, now).0, "failed");

        // And ignoring still means ignoring.
        j.muted = true;
        assert_eq!(job_status(&j, now).0, "muted");
    }

    /// Saying "do not tell me about this" has to mean it everywhere, including
    /// for a job that is genuinely broken - that is the whole point of the
    /// decision, and honouring it in the list but not the badge would be worse
    /// than not offering it.
    #[test]
    fn an_ignored_job_asks_for_nothing() {
        let now = chrono::Utc::now();
        let mut overdue = job(24, 72, true);
        assert_eq!(job_status(&overdue, now).0, "overdue");
        overdue.muted = true;
        assert_eq!(job_status(&overdue, now).0, "muted");

        let mut failed = job(24, 1, false);
        assert_eq!(job_status(&failed, now).0, "failed");
        failed.muted = true;
        assert_eq!(job_status(&failed, now).0, "muted");
    }

    /// The whole point: a job that stops running says nothing, and nothing is
    /// what a working job also says between runs. The expectation is what
    /// separates them.
    #[test]
    fn a_job_that_stops_checking_in_is_noticed() {
        let now = chrono::Utc::now();
        assert_eq!(job_status(&job(24, 2, true), now).0, "ok");
        // A daily job twenty minutes late is not news; the grace is a quarter
        // of its period, so ordinary jitter never raises a warning.
        assert_eq!(job_status(&job(24, 25, true), now).0, "ok");
        assert_eq!(job_status(&job(24, 31, true), now).0, "overdue");
        // Overdue outranks a failure: a job that failed loudly yesterday is a
        // smaller problem than one nobody has heard from since.
        assert_eq!(job_status(&job(24, 72, false), now).0, "overdue");
        assert_eq!(job_status(&job(24, 2, false), now).0, "failed");
    }

    /// Never invent an expectation for someone else's script.
    #[test]
    fn a_job_with_no_stated_cadence_is_never_late() {
        let now = chrono::Utc::now();
        let (status, due) = job_status(&job(0, 24 * 365, true), now);
        assert_eq!(status, "ok", "it cannot be late if nobody said when");
        assert!(due.is_none(), "and there is no due date to show");
    }

    /// A short period still gets an hour of slack, or a job running every
    /// fifteen minutes would alarm on a single missed tick.
    #[test]
    fn short_periods_keep_an_hour_of_slack() {
        let now = chrono::Utc::now();
        assert_eq!(job_status(&job(1, 1, true), now).0, "ok");
        assert_eq!(job_status(&job(1, 3, true), now).0, "overdue");
    }

    fn report(reachable: bool) -> pp_proto::DeviceReport {
        pp_proto::DeviceReport {
            id: "router".into(),
            target: "router.example".into(),
            resolved_ip: None,
            reachable,
            firmware: reachable.then(|| "25.7.11_9".to_string()),
            detail: if reachable { "OPNsense 25.7.11_9".into() } else { String::new() },
            latency_ms: None,
            error: (!reachable).then(|| "error sending request".to_string()),
            drift: false,
            updates: if reachable { 91 } else { 0 },
            updates_known: reachable,
            reboot_required: false,
            eol: reachable,
            eol_note: if reachable { "26.1 is the supported series".into() } else { String::new() },
            checked_at: chrono::Utc::now(),
        }
    }

    /// A probe that fails says the device did not answer. It does not say the
    /// firewall stopped being end of life, and it must not be allowed to.
    #[test]
    fn a_failed_probe_does_not_erase_what_was_known() {
        let good = report(true);
        let mut now = report(false);
        let (stale, at) = keep_last_known(&mut now, Some(&good));

        assert!(stale, "the row has to admit these facts are remembered");
        assert_eq!(at, Some(good.checked_at));
        assert!(!now.reachable, "reachability is the one fact the probe did establish");
        assert_eq!(now.firmware.as_deref(), Some("25.7.11_9"));
        assert_eq!(now.updates, 91);
        assert!(now.updates_known);
        assert!(now.eol, "an unreachable end-of-life box is still end of life");
        assert!(now.error.is_some(), "and the failure itself is still reported");
    }

    /// Nothing to fall back to is a different case from a failure: a device
    /// that has never answered must not claim remembered facts it never had.
    #[test]
    fn nothing_is_invented_without_a_good_reading() {
        let mut now = report(false);
        let (stale, at) = keep_last_known(&mut now, None);
        assert!(!stale);
        assert_eq!(at, None);
        assert_eq!(now.firmware, None);
        assert!(!now.eol);
    }

    /// A successful probe is the truth, whatever was remembered before.
    #[test]
    fn a_good_probe_is_never_overridden() {
        let stale_good = report(true);
        let mut fresh = report(true);
        fresh.eol = false;
        fresh.updates = 0;
        let (stale, _) = keep_last_known(&mut fresh, Some(&stale_good));
        assert!(!stale);
        assert!(!fresh.eol, "an upgraded box must be allowed to stop being EOL");
        assert_eq!(fresh.updates, 0);
    }
}
