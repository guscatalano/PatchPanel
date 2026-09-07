//! The agent's main loop: one persistent WebSocket to the portal, everything
//! else hanging off it.
//!
//! Long work (an apt upgrade, a device sweep) runs in spawned tasks so
//! heartbeats keep flowing while a forty-minute patch run is in progress. The
//! connection is treated as disposable — every scheduled job rebuilds a full
//! snapshot, so a dropped socket costs nothing but a reconnect.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use pp_proto::{
    ClientMsg, Command, CommandEnvelope, CommandResult, Drift, Inventory, Manifest, OsKind,
    ServerMsg, SystemInfo, PROTOCOL_VERSION,
};
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::task::JoinSet;
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

use crate::config::{AgentState, Config};
use crate::exec::Progress;
use crate::platform::Platform;
use crate::{probe, selfupdate};

pub const AGENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Never wait longer than this between reconnect attempts. A minute keeps a
/// fleet recovering promptly after a portal restart without stampeding it.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// How long a session must survive before we treat it as proof that the portal
/// is healthy, and reset the reconnect backoff no matter how it ended.
const HEALTHY_SESSION: Duration = Duration::from_secs(30);

/// Consecutive failures against a manifest-supplied portal URL before falling
/// back to the one this agent was installed with. Without this, publishing a
/// manifest naming an address that does not resolve everywhere would silently
/// orphan part of the fleet with no way back.
const OVERRIDE_FAILURES_BEFORE_FALLBACK: u32 = 3;

/// Why a session ended.
enum Disposition {
    /// Normal drop — reconnect.
    Reconnect,
    /// A new binary is in place; exit so the supervisor restarts us.
    Restart,
}

/// Everything a command handler needs, cheap to clone into spawned tasks.
#[derive(Clone)]
struct Ctx {
    cfg: Arc<Config>,
    state: Arc<Mutex<AgentState>>,
    platform: Arc<Platform>,
    manifest: Arc<RwLock<Manifest>>,
    /// Last full snapshot, so a device-only or package-only refresh can still
    /// send the portal a complete picture.
    last: Arc<RwLock<Option<Inventory>>>,
    tx: mpsc::UnboundedSender<ClientMsg>,
    progress_tx: mpsc::UnboundedSender<(Uuid, String)>,
    /// Signals that a new binary is in place and this process should exit.
    ///
    /// A channel rather than a `Notify`: `select!` rebuilds its futures on
    /// every iteration, and a `Notified` that is woken but dropped because a
    /// sibling branch also became ready swallows the notification. That made
    /// self-update replace the binary and then never restart. `recv()` is
    /// cancel-safe, so a message queued here cannot be lost.
    restart_tx: mpsc::UnboundedSender<()>,
}

impl Ctx {
    fn send(&self, msg: ClientMsg) {
        // A closed channel means the session is already tearing down; the
        // next connection will resend a fresh snapshot anyway.
        let _ = self.tx.send(msg);
    }
}

pub async fn run(cfg: Config) -> Result<()> {
    selfupdate::cleanup_old();

    let platform = Arc::new(Platform::detect());
    let state = Arc::new(Mutex::new(AgentState::load_or_init(&cfg.state_dir)?));
    let cfg = Arc::new(cfg);

    // Static facts, read once: none of this changes while we run.
    let hardware = Arc::new(crate::hardware::collect());
    tracing::info!(
        cpu = %hardware.cpu_model,
        cores = hardware.cpu_cores,
        threads = hardware.cpu_threads,
        memory_mb = hardware.memory_mb,
        ips = ?hardware.ip_addresses,
        "hardware"
    );

    {
        let s = state.lock().await;
        tracing::info!(
            agent_id = %s.agent_id,
            portal = %cfg.portal_url,
            site = %cfg.site,
            version = AGENT_VERSION,
            "patchpanel agent starting"
        );
    }

    let mut backoff = Duration::from_secs(1);
    let mut failures: u32 = 0;
    loop {
        // Prefer the manifest's address, but return to the bootstrap one once
        // it has repeatedly failed: that is the only way home from a bad push.
        let url = {
            let s = state.lock().await;
            match &s.portal_url_override {
                Some(u) if failures < OVERRIDE_FAILURES_BEFORE_FALLBACK => u.clone(),
                Some(u) => {
                    tracing::warn!(
                        tried = %u,
                        bootstrap = %cfg.portal_url,
                        "manifest portal URL keeps failing; falling back"
                    );
                    cfg.portal_url.clone()
                }
                None => cfg.portal_url.clone(),
            }
        };

        let started = std::time::Instant::now();
        let outcome = session(
            cfg.clone(),
            state.clone(),
            platform.clone(),
            hardware.clone(),
            url,
        )
        .await;
        // A session that stayed up this long proves the portal is reachable and
        // that our credentials work. How it *ended* says nothing about that — a
        // portal restart severs the socket with "connection reset", which is an
        // error — so the next attempt starts from a clean backoff either way.
        // Without this, every portal restart ratchets the whole fleet toward
        // the 60s cap and leaves it there.
        let was_healthy = started.elapsed() >= HEALTHY_SESSION;

        match outcome {
            Ok(Disposition::Restart) => {
                tracing::info!("exiting for self-update; supervisor will restart");
                return Ok(());
            }
            Ok(Disposition::Reconnect) => {
                tracing::warn!("portal connection closed; reconnecting");
                backoff = Duration::from_secs(1);
                failures = 0;
            }
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "session failed");
                if was_healthy {
                    backoff = Duration::from_secs(1);
                    failures = 0;
                } else {
                    failures = failures.saturating_add(1);
                }
            }
        }

        tokio::time::sleep(jitter(backoff)).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

/// Spread reconnects so a fleet does not retry in lockstep after an outage.
fn jitter(base: Duration) -> Duration {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0) as u64;
    let spread = base.as_millis() as u64 / 2;
    base + Duration::from_millis(if spread == 0 { 0 } else { nanos % spread })
}

async fn session(
    cfg: Arc<Config>,
    state: Arc<Mutex<AgentState>>,
    platform: Arc<Platform>,
    hardware: Arc<pp_proto::Hardware>,
    portal_url: String,
) -> Result<Disposition> {
    let (ws, _) = tokio_tungstenite::connect_async(portal_url.as_str())
        .await
        .with_context(|| format!("connecting to {portal_url}"))?;
    tracing::info!(portal = %portal_url, "connected");

    let (mut sink, mut stream) = ws.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<ClientMsg>();
    let (progress_tx, mut progress_rx) = mpsc::unbounded_channel::<(Uuid, String)>();

    // Fold live command output into the single outbound stream.
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            while let Some((id, line)) = progress_rx.recv().await {
                if tx.send(ClientMsg::CommandProgress { id, line }).is_err() {
                    break;
                }
            }
        });
    }

    let (restart_tx, mut restart_rx) = mpsc::unbounded_channel::<()>();
    let writer_failed = Arc::new(AtomicBool::new(false));
    let writer = {
        let writer_failed = writer_failed.clone();
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                let text = match serde_json::to_string(&msg) {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::error!(error = %e, "failed to encode outbound frame");
                        continue;
                    }
                };
                if sink.send(Message::Text(text.into())).await.is_err() {
                    writer_failed.store(true, Ordering::SeqCst);
                    break;
                }
            }
            let _ = sink.close().await;
        })
    };

    let ctx = Ctx {
        cfg: cfg.clone(),
        state: state.clone(),
        platform: platform.clone(),
        manifest: Arc::new(RwLock::new(Manifest::default())),
        last: Arc::new(RwLock::new(None)),
        tx: tx.clone(),
        progress_tx,
        restart_tx,
    };

    // Hello first: the portal will not accept any other frame before it.
    {
        let s = state.lock().await;
        ctx.send(ClientMsg::Hello {
            protocol: PROTOCOL_VERSION,
            agent_id: s.agent_id,
            enrollment_token: (!cfg.enrollment_token.is_empty() && s.agent_token.is_none())
                .then(|| cfg.enrollment_token.clone()),
            agent_token: s.agent_token.clone(),
            system: system_info(&platform, &cfg.site, &hardware),
            applied_revision: s.applied_revision,
        });
    }

    let mut schedules = JoinSet::new();
    let mut disposition = Disposition::Reconnect;

    loop {
        tokio::select! {
            // A self-update finished; leave so the new binary takes over.
            _ = restart_rx.recv() => {
                disposition = Disposition::Restart;
                break;
            }
            frame = stream.next() => {
                let Some(frame) = frame else { break };
                let frame = frame.context("reading from portal")?;
                match frame {
                    Message::Text(text) => {
                        match serde_json::from_str::<ServerMsg>(text.as_str()) {
                            Ok(msg) => {
                                if handle_server_msg(msg, &ctx, &mut schedules).await? {
                                    break;
                                }
                            }
                            // A frame we cannot parse is the portal's problem to
                            // fix; dropping the connection over it would just
                            // spin, so log and keep going.
                            Err(e) => tracing::warn!(error = %e, "unparseable frame from portal"),
                        }
                    }
                    Message::Close(_) => break,
                    // tungstenite answers pings for us.
                    _ => {}
                }
            }
        }

        if writer_failed.load(Ordering::SeqCst) {
            break;
        }
    }

    schedules.shutdown().await;
    drop(tx);
    let _ = writer.await;
    Ok(disposition)
}

/// Returns `true` when the portal asked us to disconnect.
async fn handle_server_msg(
    msg: ServerMsg,
    ctx: &Ctx,
    schedules: &mut JoinSet<()>,
) -> Result<bool> {
    match msg {
        ServerMsg::Welcome {
            agent_token,
            manifest,
            ..
        } => {
            {
                let mut s = ctx.state.lock().await;
                if s.agent_token.as_deref() != Some(agent_token.as_str()) {
                    s.agent_token = Some(agent_token);
                    s.save(&ctx.cfg.state_dir)?;
                    tracing::info!("stored agent token from portal");
                }
            }
            tracing::info!(revision = manifest.revision, "enrolled");
            apply_manifest_update(manifest, ctx).await;

            // Schedules only start once we know the intervals to use.
            if schedules.is_empty() {
                start_schedules(ctx.clone(), schedules);
            }
            Ok(false)
        }

        ServerMsg::Manifest(manifest) => {
            tracing::info!(revision = manifest.revision, "manifest updated");
            apply_manifest_update(manifest, ctx).await;
            Ok(false)
        }

        ServerMsg::Command(env) => {
            let ctx = ctx.clone();
            tokio::spawn(async move { run_command(env, ctx).await });
            Ok(false)
        }

        ServerMsg::Error { message } => {
            tracing::error!(%message, "portal rejected this agent");
            Ok(true)
        }
    }
}

/// Store a new manifest and, when it is newer than what we last applied,
/// converge on it in the background.
async fn apply_manifest_update(manifest: Manifest, ctx: &Ctx) {
    let revision = manifest.revision;

    // The portal can name its canonical address, typically to move a fleet from
    // a short hostname onto an FQDN without touching every machine by hand.
    if let Some(want) = manifest.portal_url.as_deref().map(str::trim) {
        if !want.is_empty() {
            let normalised = crate::normalize_portal(want)
                .map(|(ws, _)| ws)
                .unwrap_or_else(|_| want.to_string());
            let mut s = ctx.state.lock().await;
            if s.portal_url_override.as_deref() != Some(normalised.as_str()) {
                tracing::info!(url = %normalised, "adopting portal URL from manifest");
                s.portal_url_override = Some(normalised);
                if let Err(e) = s.save(&ctx.cfg.state_dir) {
                    tracing::error!(error = %e, "failed to persist portal URL");
                }
            }
        }
    }

    *ctx.manifest.write().await = manifest;

    let applied = ctx.state.lock().await.applied_revision;
    if revision > applied {
        let ctx = ctx.clone();
        tokio::spawn(async move {
            let p = Progress::detached();
            match reconcile_apps(&ctx, &p).await {
                Ok(summary) => {
                    tracing::info!(revision, %summary, "converged on manifest");
                    let reboot_required = ctx.platform.reboot_required().await;
                    {
                        let mut s = ctx.state.lock().await;
                        s.applied_revision = revision;
                        if let Err(e) = s.save(&ctx.cfg.state_dir) {
                            tracing::error!(error = %e, "failed to persist applied revision");
                        }
                    }
                    // Tell the portal now rather than on the next heartbeat.
                    // Otherwise every freshly enrolled agent shows as "stale
                    // manifest" for up to a full heartbeat interval, which
                    // looks like a fault and is not one.
                    ctx.send(ClientMsg::Heartbeat {
                        at: Utc::now(),
                        reboot_required,
                        applied_revision: revision,
                    });
                }
                // Leave applied_revision alone so the next manifest push, or
                // the next reconnect, tries again.
                Err(e) => tracing::error!(error = %format!("{e:#}"), revision, "reconcile failed"),
            }
            refresh_packages(&ctx).await;
        });
    }
}

fn start_schedules(ctx: Ctx, schedules: &mut JoinSet<()>) {
    // Heartbeat: cheap, frequent, and the only thing that must never be
    // blocked by slower work.
    {
        let ctx = ctx.clone();
        schedules.spawn(async move {
            loop {
                let secs = ctx.manifest.read().await.heartbeat_secs.max(5);
                tokio::time::sleep(Duration::from_secs(secs)).await;
                let reboot_required = ctx.platform.reboot_required().await;
                let applied_revision = ctx.state.lock().await.applied_revision;
                ctx.send(ClientMsg::Heartbeat {
                    at: Utc::now(),
                    reboot_required,
                    applied_revision,
                });
            }
        });
    }

    // Package inventory, plus policy-driven auto-patching.
    {
        let ctx = ctx.clone();
        schedules.spawn(async move {
            refresh_packages(&ctx).await;
            loop {
                let secs = ctx.manifest.read().await.inventory_secs.max(60);
                tokio::time::sleep(Duration::from_secs(secs)).await;

                let policy = ctx.manifest.read().await.patch_policy.clone();
                if policy.auto_apply {
                    let p = Progress::detached();
                    match ctx
                        .platform
                        .apply_patches(policy.security_only, &[], &policy.exclude, &p)
                        .await
                    {
                        Ok(log) => tracing::info!(summary = %crate::exec::tail(&log, 300), "auto-patch run"),
                        Err(e) => tracing::error!(error = %format!("{e:#}"), "auto-patch failed"),
                    }
                    if policy.allow_reboot && ctx.platform.reboot_required().await {
                        tracing::warn!("rebooting per patch policy");
                        let _ = ctx.platform.reboot(60, &p).await;
                    }
                }
                refresh_packages(&ctx).await;
            }
        });
    }

    // Device probing, on its own cadence because it is usually much faster
    // than a package scan and operators want it fresher.
    {
        let ctx = ctx.clone();
        schedules.spawn(async move {
            loop {
                let secs = ctx.manifest.read().await.device_probe_secs.max(30);
                tokio::time::sleep(Duration::from_secs(secs)).await;
                refresh_devices(&ctx, &[]).await;
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Inventory
// ---------------------------------------------------------------------------

fn system_info(platform: &Platform, site: &str, hardware: &pp_proto::Hardware) -> SystemInfo {
    SystemInfo {
        hostname: gethostname::gethostname().to_string_lossy().into_owned(),
        os: OsKind::current(),
        os_version: Platform::os_version(),
        arch: std::env::consts::ARCH.to_string(),
        agent_version: AGENT_VERSION.to_string(),
        backends: platform.backend_names(),
        site: site.to_string(),
        hardware: hardware.clone(),
        boot_time: crate::hardware::boot_time(),
    }
}

/// Rescan packages and updates, keeping the most recent device results, then
/// report a complete snapshot.
async fn refresh_packages(ctx: &Ctx) {
    let p = Progress::detached();
    let packages = ctx
        .platform
        .installed_packages(&p)
        .await
        .unwrap_or_else(|e| {
            tracing::error!(error = %format!("{e:#}"), "package inventory failed");
            Vec::new()
        });
    let updates = ctx.platform.available_updates(&p).await.unwrap_or_else(|e| {
        tracing::error!(error = %format!("{e:#}"), "update scan failed");
        Vec::new()
    });
    let repositories = crate::repos::collect();
    let drift = compute_drift(ctx, &p).await;
    let reboot_required = ctx.platform.reboot_required().await;

    let mut last = ctx.last.write().await;
    let (devices, discovered) = last
        .as_ref()
        .map(|i| (i.devices.clone(), i.discovered.clone()))
        .unwrap_or_default();

    let inv = Inventory {
        collected_at: Utc::now(),
        packages,
        updates,
        reboot_required,
        drift,
        devices,
        discovered,
        repositories,
    };
    *last = Some(inv.clone());
    drop(last);

    tracing::info!(
        packages = inv.packages.len(),
        updates = inv.updates.len(),
        drift = inv.drift.len(),
        "inventory collected"
    );
    ctx.send(ClientMsg::Inventory(inv));
}

/// Re-probe assigned devices, keeping the most recent package results.
/// `only` narrows to specific device ids.
async fn refresh_devices(ctx: &Ctx, only: &[String]) -> usize {
    let (specs, scans) = {
        let m = ctx.manifest.read().await;
        let site = ctx.cfg.site.as_str();
        (
            m.devices_for(site)
                .filter(|d| only.is_empty() || only.contains(&d.id))
                .cloned()
                .collect::<Vec<_>>(),
            m.discovery_for(site).cloned().collect::<Vec<_>>(),
        )
    };

    if specs.is_empty() && scans.is_empty() {
        return 0;
    }

    let reports = probe::probe_all(&specs).await;
    let unreachable = reports.iter().filter(|r| !r.reachable).count();
    let drifted = reports.iter().filter(|r| r.drift).count();
    tracing::info!(
        probed = reports.len(),
        unreachable,
        drifted,
        "device probe complete"
    );

    let mut last = ctx.last.write().await;
    let base = last.clone().unwrap_or(Inventory {
        collected_at: Utc::now(),
        packages: Vec::new(),
        updates: Vec::new(),
        reboot_required: false,
        drift: Vec::new(),
        devices: Vec::new(),
        discovered: Vec::new(),
        repositories: Vec::new(),
    });

    // A narrowed probe updates only the devices it touched.
    let mut devices = if only.is_empty() {
        Vec::new()
    } else {
        base.devices
            .iter()
            .filter(|d| !reports.iter().any(|r| r.id == d.id))
            .cloned()
            .collect()
    };
    let probed = reports.len();
    devices.extend(reports);
    devices.sort_by(|a, b| a.id.cmp(&b.id));

    let inv = Inventory {
        collected_at: Utc::now(),
        devices,
        ..base
    };
    *last = Some(inv.clone());
    drop(last);

    ctx.send(ClientMsg::Inventory(inv));
    probed
}

/// Compare each manifest app against what is actually installed.
async fn compute_drift(ctx: &Ctx, p: &Progress) -> Vec<Drift> {
    let apps: Vec<_> = {
        let m = ctx.manifest.read().await;
        m.apps_for(OsKind::current()).cloned().collect()
    };

    let mut drift = Vec::new();
    for app in apps {
        let observed = ctx.platform.observed_version(&app.source, p).await;
        let desired = match app.ensure {
            pp_proto::Ensure::Absent => "absent".to_string(),
            pp_proto::Ensure::Latest => "latest".to_string(),
            pp_proto::Ensure::Present => app.version.clone().unwrap_or_else(|| "present".into()),
        };

        let ok = match (&app.ensure, &observed) {
            (pp_proto::Ensure::Absent, None) => true,
            (pp_proto::Ensure::Absent, Some(_)) => false,
            // "latest" drift is reported by the update scan, not here: asking
            // every backend for the newest available version on every cycle is
            // far more expensive than it is worth.
            (pp_proto::Ensure::Latest, obs) => obs.is_some(),
            (pp_proto::Ensure::Present, Some(cur)) => {
                app.version.as_ref().is_none_or(|w| cur.starts_with(w))
            }
            (pp_proto::Ensure::Present, None) => false,
        };

        if !ok {
            drift.push(Drift {
                app: app.name.clone(),
                desired,
                observed: observed.unwrap_or_else(|| "absent".into()),
            });
        }
    }
    drift
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

async fn run_command(env: CommandEnvelope, ctx: Ctx) {
    let id = env.id;
    let p = Progress::attached(id, ctx.progress_tx.clone());
    tracing::info!(%id, command = ?env.command, "running command");

    let self_updated = matches!(env.command, Command::SelfUpdate { .. });
    let result = execute(env.command, &ctx, &p).await;

    let (ok, summary, detail) = match result {
        Ok(summary) => (true, crate::exec::tail(&summary, 200), summary),
        Err(e) => {
            let text = format!("{e:#}");
            tracing::error!(%id, error = %text, "command failed");
            (false, crate::exec::tail(&text, 200), text)
        }
    };

    ctx.send(ClientMsg::CommandResult(CommandResult {
        id,
        ok,
        summary,
        detail: crate::exec::tail(&detail, 16_000),
        finished_at: Utc::now(),
    }));

    if ok && self_updated {
        // Give the writer a moment to flush the result before we tear the
        // connection down; the portal should record the update before we go.
        tokio::time::sleep(Duration::from_millis(500)).await;
        tracing::info!("self-update installed; signalling restart");
        if ctx.restart_tx.send(()).is_err() {
            // The session is already ending, which achieves the same thing.
            tracing::warn!("restart channel closed; the next reconnect will run the new binary");
        }
    }
}

async fn execute(cmd: Command, ctx: &Ctx, p: &Progress) -> Result<String> {
    match cmd {
        Command::CollectInventory => {
            refresh_packages(ctx).await;
            let last = ctx.last.read().await;
            let (pkgs, ups) = last
                .as_ref()
                .map(|i| (i.packages.len(), i.updates.len()))
                .unwrap_or((0, 0));
            Ok(format!("{pkgs} packages, {ups} updates pending"))
        }

        Command::ApplyManifest => {
            let summary = reconcile_apps(ctx, p).await?;
            let revision = ctx.manifest.read().await.revision;
            {
                let mut s = ctx.state.lock().await;
                s.applied_revision = revision;
                s.save(&ctx.cfg.state_dir)?;
            }
            refresh_packages(ctx).await;
            Ok(summary)
        }

        Command::ApplyPatches {
            security_only,
            only,
        } => {
            let policy = ctx.manifest.read().await.patch_policy.clone();
            let log = ctx
                .platform
                .apply_patches(security_only, &only, &policy.exclude, p)
                .await?;
            refresh_packages(ctx).await;
            Ok(log)
        }

        Command::SelfUpdate {
            version,
            url,
            sha256,
        } => selfupdate::apply(&version, &url, &sha256, p).await,

        Command::Reboot { delay_secs } => {
            ctx.platform.reboot(delay_secs, p).await?;
            Ok(format!("reboot scheduled in {delay_secs}s"))
        }

        Command::ProbeDevices { only } => {
            let n = refresh_devices(ctx, &only).await;
            Ok(format!("probed {n} device(s)"))
        }

        Command::Discover => {
            let (scans, known) = {
                let m = ctx.manifest.read().await;
                let site = ctx.cfg.site.as_str();
                (
                    m.discovery_for(site).cloned().collect::<Vec<_>>(),
                    m.devices.clone(),
                )
            };
            if scans.is_empty() {
                anyhow::bail!("no discovery ranges configured for site `{}`", ctx.cfg.site);
            }
            p.line(&format!("sweeping {} range(s)", scans.len()));
            let found = probe::discover(&scans, &known).await;
            let unmanaged = found.iter().filter(|h| h.unmanaged).count();

            let mut last = ctx.last.write().await;
            if let Some(inv) = last.as_mut() {
                inv.discovered = found.clone();
                inv.collected_at = Utc::now();
                let snapshot = inv.clone();
                drop(last);
                ctx.send(ClientMsg::Inventory(snapshot));
            }

            Ok(format!(
                "found {} responsive host(s), {unmanaged} not in the manifest",
                found.len()
            ))
        }
    }
}

/// Walk the manifest's apps and bring each one to its desired state.
async fn reconcile_apps(ctx: &Ctx, p: &Progress) -> Result<String> {
    let apps: Vec<_> = {
        let m = ctx.manifest.read().await;
        m.apps_for(OsKind::current()).cloned().collect()
    };

    if apps.is_empty() {
        return Ok("no apps in manifest for this platform".into());
    }

    let mut notes = Vec::new();
    let mut failures = Vec::new();
    let mut changed = 0;

    for app in &apps {
        match ctx
            .platform
            .ensure_app(&app.name, app.version.as_deref(), app.ensure, &app.source, p)
            .await
        {
            Ok(outcome) => {
                if outcome.changed {
                    changed += 1;
                }
                notes.push(outcome.describe());
            }
            // One broken app should not stop the rest from converging; the
            // whole run is still reported as failed.
            Err(e) => {
                let msg = format!("{}: {e:#}", app.name);
                tracing::error!(app = %app.name, error = %format!("{e:#}"), "ensure failed");
                failures.push(msg);
            }
        }
    }

    let summary = format!(
        "{} app(s): {changed} changed, {} failed\n{}",
        apps.len(),
        failures.len(),
        notes.join("\n")
    );

    if failures.is_empty() {
        Ok(summary)
    } else {
        anyhow::bail!("{summary}\nfailures:\n{}", failures.join("\n"))
    }
}
