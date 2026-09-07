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
        .route("/api/agents/{id}", get(agent).delete(delete_agent))
        .route("/api/agents/{id}/commands", post(dispatch))
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
    let rows = state.db.agents()?;
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
            let connected = state.hub.is_connected(row.id);
            AgentView { row, connected }
        })
        .collect();

    // Device drift needs the reports themselves, not just the per-agent count.
    let all = collect_devices(&state)?;
    summary.devices_drifted = all.iter().filter(|d| d.report.drift).count();
    summary.devices_unreachable = all.iter().filter(|d| !d.report.reachable).count();

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
}

/// How this machine's package sources compare with its peers.
///
/// "Peers" means agents on the same OS, because comparing apt sources against a
/// Windows box would be noise. A repository present here but nowhere else, or
/// missing here but present on every peer, is usually the reason one machine
/// behaves differently.
#[derive(Default, Serialize)]
struct RepoDiff {
    /// How many same-OS agents this was compared against.
    peers: usize,
    /// Configured here, on no peer.
    only_here: Vec<String>,
    /// Configured on every peer, but not here.
    missing_here: Vec<String>,
}

/// Identity of a repository for comparison: where it points and at what suite.
/// The declaring filename is deliberately excluded - the same repo added under
/// a different filename is still the same repo.
fn repo_key(r: &pp_proto::Repository) -> String {
    format!("{} {} {}", r.source, r.uri.trim_end_matches('/'), r.suite)
}

fn repo_diff(
    state: &SharedState,
    me: pp_proto::AgentId,
    my_os: &str,
    mine: &[pp_proto::Repository],
) -> anyhow::Result<RepoDiff> {
    use std::collections::HashSet;

    let my_keys: HashSet<String> = mine
        .iter()
        .filter(|r| r.enabled)
        .map(repo_key)
        .collect();

    let mut peer_sets: Vec<HashSet<String>> = Vec::new();
    for row in state.db.agents()? {
        if row.id == me || row.os != my_os {
            continue;
        }
        let Some(inv) = state.db.inventory(row.id)? else {
            continue;
        };
        if inv.repositories.is_empty() {
            continue;
        }
        peer_sets.push(
            inv.repositories
                .iter()
                .filter(|r| r.enabled)
                .map(repo_key)
                .collect(),
        );
    }

    if peer_sets.is_empty() {
        return Ok(RepoDiff::default());
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
        only_here,
        missing_here,
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

    let inventory = state.db.inventory(id)?;
    let diff = match &inventory {
        Some(inv) => repo_diff(&state, id, &row.os, &inv.repositories)?,
        None => RepoDiff::default(),
    };

    Ok(Json(AgentDetail {
        connected: state.hub.is_connected(id),
        commands: state.db.commands(Some(id), 100)?,
        inventory,
        repo_diff: diff,
        row,
    }))
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
}

fn collect_devices(state: &SharedState) -> anyhow::Result<Vec<DeviceView>> {
    let manifest = state.db.manifest()?;
    let mut out = Vec::new();

    for row in state.db.agents()? {
        let Some(inv) = state.db.inventory(row.id)? else {
            continue;
        };
        for report in inv.devices {
            let spec = manifest.devices.iter().find(|d| d.id == report.id);
            out.push(DeviceView {
                collector: row.id,
                collector_host: row.hostname.clone(),
                site: row.site.clone(),
                label: spec.map(|s| s.label.clone()).unwrap_or_default(),
                tags: spec.map(|s| s.tags.clone()).unwrap_or_default(),
                expect_version: spec.and_then(|s| s.expect_version.clone()),
                report,
            });
        }
    }

    out.sort_by(|a, b| a.report.id.cmp(&b.report.id));
    Ok(out)
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
