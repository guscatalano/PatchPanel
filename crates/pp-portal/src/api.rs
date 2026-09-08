//! REST API behind the dashboard.
//!
//! Everything the UI does is available here as plain JSON, because the two
//! things operators always end up wanting — a scripted rollout and a Nagios
//! check — should not require scraping HTML.


use axum::extract::{Path, Query, State};
use axum::http::{header, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
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
        .route("/api/devices", get(devices))
        .route("/api/devices/{id}/history", get(device_history))
        .route("/api/devices/{id}/id", post(rename_device))
        .route("/api/agents/{id}", get(agent).delete(delete_agent))
        .route("/api/agents/{id}/commands", post(dispatch))
        .route("/api/agents/{id}/ignores", post(add_ignore).delete(remove_ignore))
        .route("/api/commands", get(command_log))
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

    // Device drift needs the reports themselves, not just the per-agent count.
    let all = collect_devices(&state)?;
    summary.devices_drifted = all.iter().filter(|d| d.report.drift).count();
    summary.devices_unreachable = all.iter().filter(|d| !d.report.reachable).count();

    // Device counts come from the same deduplicated view the Devices tab
    // uses, so a badge and the page it points at cannot disagree.
    if let Ok(devices) = collect_devices(&state) {
        summary.devices_pending = devices
            .iter()
            .filter(|d| d.report.updates_known && d.report.updates > 0)
            .count();
        summary.devices_eol = devices.iter().filter(|d| d.report.eol).count();
    }

    Ok(Json(FleetResponse {
        summary,
        agents,
        manifest_revision: manifest.revision,
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
    /// How many collectors are reporting this device. More than one means no
    /// `collector` is named on it and every agent in the site is probing it.
    collectors: usize,
    /// False for a device that is declared but has never been probed. It still
    /// belongs in the list: a device nobody has looked at yet is exactly the
    /// one worth seeing, and leaving it out made adding one feel like it had
    /// silently failed.
    probed: bool,
}

fn collect_devices(state: &SharedState) -> anyhow::Result<Vec<DeviceView>> {
    let manifest = state.db.manifest()?;
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
            out.push(DeviceView {
                collector: row.id,
                collector_host: row.hostname.clone(),
                site: row.site.clone(),
                label: spec.map(|s| s.label.clone()).unwrap_or_default(),
                tags: spec.map(|s| s.tags.clone()).unwrap_or_default(),
                expect_version: spec.and_then(|s| s.expect_version.clone()),
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
            collectors: 1,
            probed: false,
            report: pp_proto::DeviceReport {
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

#[derive(Serialize)]
struct DevicesResponse {
    devices: Vec<DeviceView>,
    /// Hosts a discovery sweep saw that no manifest entry covers.
    unmanaged: Vec<UnmanagedView>,
}

#[derive(Serialize)]
struct UnmanagedView {
    collector_host: String,
    site: String,
    ip: String,
    open_ports: Vec<u16>,
    hint: String,
}

async fn devices(State(state): State<SharedState>) -> ApiResult<Json<DevicesResponse>> {
    let devices = collect_devices(&state)?;

    let mut unmanaged = Vec::new();
    for row in state.db.agents()? {
        let Some(inv) = state.db.inventory(row.id)? else {
            continue;
        };
        for host in inv.discovered.into_iter().filter(|h| h.unmanaged) {
            unmanaged.push(UnmanagedView {
                collector_host: row.hostname.clone(),
                site: row.site.clone(),
                ip: host.ip,
                open_ports: host.open_ports,
                hint: host.hint,
            });
        }
    }

    Ok(Json(DevicesResponse { devices, unmanaged }))
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
        state.db.record_command(cmd_id, id, &cmd)?;
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
    state.db.record_command(cmd_id, id, &req.command)?;
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
    let targets: Vec<AgentId> = state
        .hub
        .connected()
        .into_iter()
        .filter(|id| match &req.site {
            Some(site) => rows.iter().any(|r| r.id == *id && r.site == *site),
            None => true,
        })
        .collect();

    // One id across the fan-out would collide in the command log, so each
    // target gets its own.
    let mut dispatched = 0;
    let mut first = None;
    for id in targets {
        let cmd_id = Uuid::new_v4();
        state.db.record_command(cmd_id, id, &req.command)?;
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
