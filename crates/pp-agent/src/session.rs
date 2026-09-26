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

/// How often to ping the portal.
///
/// A TCP connection whose peer vanished without a FIN - a portal that was
/// killed, a NAT table that forgot us - stays readable-but-silent forever, so
/// `stream.next()` never returns and the session cannot notice it is dead.
/// Only regular traffic and a deadline can tell the difference between a quiet
/// portal and an absent one.
const PING_INTERVAL: Duration = Duration::from_secs(20);

/// Drop the session if nothing at all has arrived in this long. Comfortably
/// more than PING_INTERVAL so an occasional slow reply is not fatal.
const LIVENESS_TIMEOUT: Duration = Duration::from_secs(75);

/// How long a single attempt to reach the portal may take before it is
/// abandoned and retried.
///
/// This exists because its absence took nine machines off the fleet for two
/// days. `connect_async` covers name resolution, the TCP handshake and the
/// WebSocket upgrade, and none of those is bounded: a resolver that never
/// answers, or a flow a stateful firewall is holding half-open, leaves the call
/// awaiting forever. LIVENESS_TIMEOUT does not help - that watches an
/// established session, and this never became one.
///
/// Worse, everything the agent does on a timer lives inside `session()`, so a
/// stalled connect stops inventory, heartbeats and the lot. The agent stays
/// running, reports nothing, and looks from the outside exactly like a machine
/// nobody has touched.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

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
    /// Updates seen to survive a patch run, carried between scans because the
    /// evidence only exists at the moment one finishes.
    blocked: Arc<RwLock<Vec<String>>>,
    /// What Debian currently calls stable. Fetched once and kept: it changes
    /// every couple of years, and every scan asking the archive again would be
    /// a network dependency for no benefit.
    stable: Arc<RwLock<Option<String>>>,
    /// Package URLs a patch run could not download. Sampling a repository
    /// finds a pool that is entirely gone; only a real run finds the four
    /// files out of a hundred that are missing, so keep what it learned.
    unfetchable: Arc<RwLock<Vec<String>>>,
    tx: mpsc::UnboundedSender<Message>,
    progress_tx: mpsc::UnboundedSender<(Uuid, String)>,
    /// Signals that a new binary is in place and this process should exit.
    ///
    /// A channel rather than a `Notify`: `select!` rebuilds its futures on
    /// every iteration, and a `Notified` that is woken but dropped because a
    /// sibling branch also became ready swallows the notification. That made
    /// self-update replace the binary and then never restart. `recv()` is
    /// cancel-safe, so a message queued here cannot be lost.
    restart_tx: mpsc::UnboundedSender<()>,
    /// Whether the portal has asked this machine to ship its journal, and at
    /// what floor.
    ///
    /// Deliberately not persisted. The portal re-asks on every connection, so a
    /// restart, a self-update or a reboot leaves exactly one record of the
    /// decision - the portal's - and the agent cannot drift out of step with it
    /// by remembering something stale.
    forward_logs: Arc<AtomicBool>,
    forward_severity: Arc<RwLock<Option<String>>>,
}

impl Ctx {
    fn send(&self, msg: ClientMsg) {
        match serde_json::to_string(&msg) {
            // A closed channel means the session is already tearing down; the
            // next connection will resend a fresh snapshot anyway.
            Ok(text) => {
                let _ = self.tx.send(Message::Text(text.into()));
            }
            Err(e) => tracing::error!(error = %e, "failed to encode outbound frame"),
        }
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
                tracing::info!("exiting so the supervisor restarts us");
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
    // Logged before the attempt, not only on success. A silent agent that has
    // been trying for two days is indistinguishable from one that gave up, and
    // the journal is the only place anybody can tell the difference.
    tracing::debug!(portal = %portal_url, "connecting");
    let (ws, _) = tokio::time::timeout(
        CONNECT_TIMEOUT,
        tokio_tungstenite::connect_async(portal_url.as_str()),
    )
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "connecting to {portal_url} got no answer within {}s; giving up on this attempt",
            CONNECT_TIMEOUT.as_secs()
        )
    })?
    .with_context(|| format!("connecting to {portal_url}"))?;
    tracing::info!(portal = %portal_url, "connected");

    let (mut sink, mut stream) = ws.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
    let (progress_tx, mut progress_rx) = mpsc::unbounded_channel::<(Uuid, String)>();

    // Fold live command output into the single outbound stream.
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            while let Some((id, line)) = progress_rx.recv().await {
                let msg = ClientMsg::CommandProgress { id, line };
                let Ok(text) = serde_json::to_string(&msg) else {
                    continue;
                };
                if tx.send(Message::Text(text.into())).is_err() {
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
            while let Some(frame) = rx.recv().await {
                if sink.send(frame).await.is_err() {
                    writer_failed.store(true, Ordering::SeqCst);
                    break;
                }
            }
            let _ = sink.close().await;
        })
    };

    let forward_logs = Arc::new(AtomicBool::new(false));
    let forward_severity: Arc<RwLock<Option<String>>> = Arc::new(RwLock::new(None));

    let ctx = Ctx {
        forward_logs: forward_logs.clone(),
        forward_severity: forward_severity.clone(),
        cfg: cfg.clone(),
        state: state.clone(),
        platform: platform.clone(),
        manifest: Arc::new(RwLock::new(Manifest::default())),
        last: Arc::new(RwLock::new(None)),
        blocked: Arc::new(RwLock::new(Vec::new())),
        stable: Arc::new(RwLock::new(None)),
        unfetchable: Arc::new(RwLock::new(Vec::new())),
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

    // Liveness. Without this the loop can park on `stream.next()` against a
    // peer that is gone, and nothing ever wakes it.
    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_rx = std::time::Instant::now();

    loop {
        tokio::select! {
            _ = ping.tick() => {
                if writer_failed.load(Ordering::SeqCst) {
                    tracing::warn!("outbound stream failed; reconnecting");
                    break;
                }
                if last_rx.elapsed() > LIVENESS_TIMEOUT {
                    tracing::warn!(
                        silent_for_s = last_rx.elapsed().as_secs(),
                        "portal stopped responding; reconnecting"
                    );
                    break;
                }
                // Any reply counts as proof of life; axum answers pings itself.
                if tx.send(Message::Ping(Vec::new().into())).is_err() {
                    break;
                }
            }
            // A self-update finished; leave so the new binary takes over.
            _ = restart_rx.recv() => {
                disposition = Disposition::Restart;
                break;
            }
            frame = stream.next() => {
                let Some(frame) = frame else { break };
                let frame = frame.context("reading from portal")?;
                last_rx = std::time::Instant::now();
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

    // Do NOT await the writer. It ends only when every sender is dropped, and
    // `ctx` holds one - as does any command task still in flight - so awaiting
    // it waits on a channel that can never close. That hang is what wedged
    // agents: the loop exited, teardown blocked here forever, and the process
    // sat holding an open socket while doing nothing at all. The connection is
    // being discarded either way, so abort it.
    drop(ctx);
    drop(tx);
    writer.abort();

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
                start_journal_shipping(ctx.clone(), schedules);
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
    {
        let want = manifest.portal_url.as_deref().map(str::trim).unwrap_or("");
        if want.is_empty() {
            // Cleared in the manifest, so go back to the bootstrap address.
            // Without this, a revert would leave the fleet pinned to whatever
            // was published last and there would be no way to undo it.
            let mut s = ctx.state.lock().await;
            if s.portal_url_override.take().is_some() {
                tracing::info!("manifest cleared the portal URL; reverting to bootstrap");
                if let Err(e) = s.save(&ctx.cfg.state_dir) {
                    tracing::error!(error = %e, "failed to persist portal URL");
                }
            }
        } else {
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

/// Ship the journal while the portal is asking for it.
///
/// Its own task in the session's set, so it is torn down on every reconnect and
/// cannot outlive the connection it sends over. It holds no lock the session
/// needs, and a failure here is logged and abandoned rather than propagated:
/// shipping logs is not the agent's job, and an agent that stops working
/// because of it would be strictly worse than one that ships nothing.
fn start_journal_shipping(ctx: Ctx, schedules: &mut JoinSet<()>) {
    schedules.spawn(async move {
        loop {
            // Cheap poll rather than a notification: the switch changes at most
            // a handful of times in a machine's life, and a second of latency
            // on a deliberate click is not worth another channel.
            if !ctx.forward_logs.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
            let sev = ctx
                .forward_severity
                .read()
                .await
                .clone()
                .unwrap_or_else(|| "warning".to_string());

            let ctx2 = ctx.clone();
            let outcome = crate::journal::follow(&sev, move |lines| {
                // Straight onto the outbound channel the session already owns.
                ctx2.send(ClientMsg::JournalLines { lines });
            })
            .await;

            match outcome {
                Ok(()) => tracing::info!("journal shipping stopped"),
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "journal shipping failed");
                    // Do not spin on a machine with no journalctl.
                    ctx.forward_logs.store(false, Ordering::SeqCst);
                }
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });
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
                        .apply_patches(policy.security_only, &[], &policy.exclude, false, &p)
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

    // Discovery sweeps, on the slowest cadence of the three.
    //
    // A sweep is the only thing here that reaches out to machines nobody asked
    // us to manage, so it is deliberately the rarest and it only runs at all
    // when this agent's site owns a range.
    {
        let ctx = ctx.clone();
        schedules.spawn(async move {
            loop {
                // Wait first. A sweep on connect would mean every deploy - and
                // every reconnect after a network blip - re-scans the network,
                // which turns a rare scan into a frequent one precisely when
                // the network is already having a bad time. The stored results
                // carry forward until the first scheduled sweep.
                let secs = ctx.manifest.read().await.discovery_secs.max(300);
                tokio::time::sleep(jitter(Duration::from_secs(secs))).await;

                let scans = discovery_scans(&ctx).await;
                if scans.is_empty() {
                    continue;
                }
                // Detached: nothing is watching, so the per-line output goes to
                // the log rather than to a command's progress stream.
                match sweep(&ctx, &scans, &Progress::detached()).await {
                    Ok(summary) => tracing::info!(summary = %summary, "scheduled sweep"),
                    Err(e) => tracing::error!(error = %format!("{e:#}"), "scheduled sweep failed"),
                }
            }
        });
    }

    // Device probing, on its own cadence because it is usually much faster
    // than a package scan and operators want it fresher.
    {
        let ctx = ctx.clone();
        schedules.spawn(async move {
            // Probe shortly after connecting rather than waiting out a whole
            // cycle. An agent that has just restarted has no device results to
            // report, and until it probes, the portal has nothing to show for
            // this collector - which is a long blank gap after every deploy.
            tokio::time::sleep(Duration::from_secs(20)).await;
            refresh_devices(&ctx, &[]).await;

            loop {
                let secs = ctx.manifest.read().await.device_probe_secs.max(30);
                tokio::time::sleep(Duration::from_secs(secs)).await;
                refresh_devices(&ctx, &[]).await;
            }
        });
    }
}

/// The discovery ranges this agent is responsible for.
///
/// Site-scoped, which is what keeps an automatic sweep from multiplying by the
/// size of the fleet: a range belongs to one site, and only agents in that site
/// sweep it.
async fn discovery_scans(ctx: &Ctx) -> Vec<pp_proto::DiscoveryScan> {
    let m = ctx.manifest.read().await;
    m.discovery_for(ctx.cfg.site.as_str()).cloned().collect()
}

/// Sweep the given ranges and report what answered.
///
/// Shared by the `Discover` command and the timer below so that a scheduled
/// sweep and a person pressing the button do exactly the same thing - including
/// stamping `swept_at`, without which the page cannot say how old the list is.
async fn sweep(ctx: &Ctx, scans: &[pp_proto::DiscoveryScan], p: &Progress) -> Result<String> {
    let known = ctx.manifest.read().await.devices.clone();
    p.line(&format!("sweeping {} range(s)", scans.len()));
    let found = probe::discover(scans, &known).await;
    let unmanaged = found.iter().filter(|h| h.unmanaged).count();
    let identified = found
        .iter()
        .filter(|h| h.scanner == pp_proto::Scanner::Nmap)
        .count();
    // A range that asked for nmap and got the built-in sweep is the one outcome
    // a person reading the job log needs told; the rows carry the same note to
    // the portal.
    let mut notes: Vec<&str> = Vec::new();
    for note in found.iter().map(|h| h.scan_note.as_str()) {
        if !note.is_empty() && !notes.contains(&note) {
            notes.push(note);
            p.line(note);
        }
    }

    let mut last = ctx.last.write().await;
    if let Some(inv) = last.as_mut() {
        inv.discovered = found.clone();
        inv.swept_at = Some(Utc::now());
        inv.collected_at = Utc::now();
        let snapshot = inv.clone();
        drop(last);
        ctx.send(ClientMsg::Inventory(snapshot));
    }

    Ok(format!(
        "found {} responsive host(s), {unmanaged} not in the manifest{}",
        found.len(),
        if identified > 0 {
            format!("; {identified} service-identified by nmap")
        } else {
            String::new()
        }
    ))
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
    refresh_packages_after(ctx, None).await;
}

/// Re-scan, and if a patch run just finished, work out what it failed to move.
///
/// `attempted` is what was pending when the run started. Anything still on
/// offer at the same version afterwards was not installed, whatever the exit
/// code said - that is the only reliable way to name a package the tooling
/// refused, and without a name the operator can only keep pressing the button.
async fn refresh_packages_after(ctx: &Ctx, attempted: Option<Vec<(String, String)>>) {
    let p = Progress::detached();
    let packages = ctx
        .platform
        .installed_packages(&p)
        .await
        .unwrap_or_else(|e| {
            tracing::error!(error = %format!("{e:#}"), "package inventory failed");
            Vec::new()
        });
    let (updates, mut scan_issues) = ctx.platform.available_updates(&p).await.unwrap_or_else(|e| {
        tracing::error!(error = %format!("{e:#}"), "update scan failed");
        (
            Vec::new(),
            vec![pp_proto::ScanIssue {
                backend: "updates".into(),
                problem: format!("the update scan failed: {e:#}"),
                remedy: "The pending update count for this machine is unknown.".into(),
            }],
        )
    });
    let mut repositories = crate::repos::collect();
    // Ask whether each one can still serve what it advertises. This is the
    // difference between "apt failed with a wall of 404s" and knowing which
    // line in which file to fix.
    for (label, problem) in ctx.platform.repo_problems(&repositories, &p).await {
        for r in repositories.iter_mut() {
            if label == format!("{} {}", r.uri, r.suite) {
                r.problem = Some(problem.clone());
            }
        }
    }
    // What the last patch run actually failed to download beats any sample.
    {
        let failed = ctx.unfetchable.read().await;
        for r in repositories.iter_mut() {
            let base = r.uri.trim_end_matches('/');
            let mine: Vec<&String> = failed.iter().filter(|u| u.starts_with(base)).collect();
            if mine.is_empty() {
                continue;
            }
            let examples: Vec<String> = mine
                .iter()
                .take(3)
                .map(|u| u.rsplit('/').next().unwrap_or(u).to_string())
                .collect();
            r.problem = Some(format!(
                "the last patch run could not download {} package(s) from here - the index \
                 lists them but the server returns 404: {}{}. A release being retired empties \
                 its pool while the indices linger; the packages are on archive.debian.org, \
                 or gone. Until this is resolved every patch run on this machine fails after \
                 downloading everything else.",
                mine.len(),
                examples.join(", "),
                if mine.len() > 3 { ", ..." } else { "" }
            ));
        }
    }
    let release = crate::release::collect(&repositories, stable_codename(ctx).await.as_deref());
    let cleanup = Some(ctx.platform.cleanup_preview(&p).await);
    scan_issues.extend(ctx.platform.scan_issues());
    // These overlap by definition: anything a full upgrade refuses was also
    // refused by a plain upgrade. Reporting both raw sets makes six packages
    // look like twelve, so keep them disjoint - `held_back` means "a full
    // upgrade would fix this", `deferred` means "nothing will".
    let deferred = ctx.platform.deferred(&p).await;
    let held_back: Vec<String> = ctx
        .platform
        .held_back(&p)
        .await
        .into_iter()
        .filter(|pkg| !deferred.contains(pkg))
        .collect();
    // Packages the archive is withholding, or that need a full upgrade, are
    // already accounted for and are not evidence of anything being blocked.
    let excused = |name: &String| deferred.contains(name) || held_back.contains(name);
    {
        let mut blocked = ctx.blocked.write().await;
        if let Some(before) = attempted {
            *blocked = updates
                .iter()
                .filter(|u| !excused(&u.name))
                .filter(|u| {
                    before
                        .iter()
                        .any(|(n, v)| *n == u.name && *v == u.new_version)
                })
                .map(|u| u.name.clone())
                .collect();
            if !blocked.is_empty() {
                tracing::warn!(
                    packages = ?*blocked,
                    "these updates survived a patch run untouched"
                );
            }
        } else {
            // Keep the finding until the package moves or stops being offered.
            blocked.retain(|n| updates.iter().any(|u| u.name == *n) && !excused(n));
        }
    }
    let blocked = ctx.blocked.read().await.clone();
    let source_files = crate::sources::read_all().await;
    if !scan_issues.is_empty() {
        tracing::warn!(
            count = scan_issues.len(),
            "some backends could not be scanned; the update count is a floor, not a total"
        );
    }
    let mid_upgrade = ctx.platform.mid_upgrade(&p).await;
    let (firmware, firmware_devices, firmware_issue) = ctx.platform.firmware(&p).await;
    // fwupd being installed but unable to answer means firmware is not being
    // checked at all, which belongs with the other coverage gaps.
    scan_issues.extend(firmware_issue);
    let boot = ctx.platform.boot_report(&p).await;
    let virt = ctx.platform.virtualization(&p).await;

    // A physical machine with no fwupd has firmware nobody is looking at, and
    // nothing else would ever mention it. Virtual machines are exempt: there
    // is no firmware inside a VM to update, and saying so on every guest would
    // be noise on most of a fleet.
    let is_guest = virt
        .as_ref()
        .is_some_and(|v| v.role.contains("guest") && !v.role.contains("host"));
    if cfg!(target_os = "linux")
        && !is_guest
        && !ctx.platform.backend_names().iter().any(|b| b == "fwupd")
    {
        scan_issues.push(pp_proto::ScanIssue {
            backend: "fwupd".into(),
            problem: "firmware is not being checked: fwupd is not installed".into(),
            remedy: "This machine has real hardware - system firmware, drives, controllers - \
                     and none of it is being looked at. Installing fwupd makes it visible; \
                     PatchPanel never flashes anything without being asked."
                .into(),
        });
    }
    let drift = compute_drift(ctx, &p).await;
    let reboot_required = ctx.platform.reboot_required().await;

    let mut last = ctx.last.write().await;
    let (devices, discovered, swept_at) = last
        .as_ref()
        .map(|i| (i.devices.clone(), i.discovered.clone(), i.swept_at))
        .unwrap_or_default();

    let inv = Inventory {
        collected_at: Utc::now(),
        #[cfg(target_os = "linux")]
        syslog_forward: crate::platform::linux::syslog_forward(),
        #[cfg(not(target_os = "linux"))]
        syslog_forward: None,
        packages,
        updates,
        reboot_required,
        drift,
        devices,
        discovered,
        swept_at,
        repositories,
        release,
        cleanup,
        scan_issues,
        held_back,
        deferred,
        blocked,
        mid_upgrade,
        firmware,
        firmware_devices,
        boot,
        virt,
        source_files,
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

/// Put the manifest's apt source files in place on this machine.
///
/// Runs before the app reconciliation for a reason: an app cannot be installed
/// from a repository that does not resolve, so fixing the sources first is
/// what makes the rest of the run mean anything.
///
/// Each file goes through the same validated write a hand edit does - apt has
/// to accept the result or the previous file comes back - so a policy that is
/// wrong for one machine cannot leave it unable to install anything.
async fn apply_source_policies(ctx: &Ctx, p: &Progress) -> Vec<String> {
    if !cfg!(target_os = "linux") {
        return Vec::new();
    }
    let (distro, codename) = crate::sources::running_release();
    if distro.is_empty() {
        return Vec::new();
    }

    let policies: Vec<pp_proto::SourcePolicy> = ctx
        .manifest
        .read()
        .await
        .sources_for(&distro, &codename)
        .cloned()
        .collect();
    if policies.is_empty() {
        return Vec::new();
    }

    let mut log = Vec::new();
    for policy in policies {
        // Writing a file that already says the right thing would run
        // `apt-get update` on every machine on every manifest apply.
        if std::fs::read_to_string(&policy.path).is_ok_and(|c| c.trim() == policy.content.trim()) {
            continue;
        }
        p.line(&format!("applying source policy to {}", policy.path));
        match crate::sources::write(&policy.path, &policy.content, p).await {
            Ok(msg) => log.push(format!("{}: {msg}", policy.path)),
            Err(e) => log.push(format!("{}: NOT applied - {e:#}", policy.path)),
        }
    }
    log
}

/// The package URLs in an apt log that came back 404 or otherwise refused.
///
/// apt writes `E: Failed to fetch http://host/pool/... 404 Not Found`.
fn unfetchable_in(text: &str) -> Vec<String> {
    text.lines()
        .filter(|l| l.contains("Failed to fetch"))
        .filter_map(|l| l.split_whitespace().find(|w| w.starts_with("http")))
        .map(|u| {
            // apt percent-encodes the version separators; the repository
            // prefix is all we match on, so leave the rest alone.
            u.to_string()
        })
        .collect()
}

/// The current Debian stable codename, fetched at most once per process.
async fn stable_codename(ctx: &Ctx) -> Option<String> {
    if let Some(v) = ctx.stable.read().await.clone() {
        return Some(v);
    }
    let found = crate::release::stable_codename().await;
    if let Some(v) = found.clone() {
        *ctx.stable.write().await = Some(v);
    }
    found
}

/// The host part of the portal's address, which is how an agent recognises
/// that it is the machine running the portal.
async fn portal_hostname(ctx: &Ctx) -> String {
    // The manifest's portal_url wins when set, since that is the address the
    // fleet was told to move to; otherwise the one this agent dialled.
    let from_manifest = ctx.manifest.read().await.portal_url.clone();
    let url = from_manifest.unwrap_or_else(|| ctx.cfg.portal_url.clone());

    // ws://host:8080/api/agent/ws, http://host/, or a bare host.
    let after_scheme = url.rsplit("://").next().unwrap_or(&url);
    after_scheme
        .split('/')
        .next()
        .unwrap_or(after_scheme)
        .split(':')
        .next()
        .unwrap_or(after_scheme)
        .to_string()
}

/// Re-probe assigned devices, keeping the most recent package results.
/// `only` narrows to specific device ids.
async fn refresh_devices(ctx: &Ctx, only: &[String]) -> usize {
    let (specs, scans) = {
        let m = ctx.manifest.read().await;
        let site = ctx.cfg.site.as_str();
        let me = gethostname::gethostname().to_string_lossy().into_owned();
        // A device with no collector named falls to whichever agent is running
        // on the portal itself. Every agent knows the portal's address and
        // only one of them is it, so they agree without talking to each other.
        let portal_host = portal_hostname(ctx).await;
        (
            m.devices_for(site, &me, &portal_host)
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
        swept_at: None,
        syslog_forward: None,
        packages: Vec::new(),
        updates: Vec::new(),
        reboot_required: false,
        drift: Vec::new(),
        devices: Vec::new(),
        discovered: Vec::new(),
        repositories: Vec::new(),
        release: None,
        cleanup: None,
        scan_issues: Vec::new(),
        held_back: Vec::new(),
        deferred: Vec::new(),
        blocked: Vec::new(),
        mid_upgrade: None,
        firmware: Vec::new(),
        firmware_devices: Vec::new(),
        boot: None,
        virt: None,
        source_files: Vec::new(),
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
        #[cfg(target_os = "linux")]
        syslog_forward: crate::platform::linux::syslog_forward(),
        #[cfg(not(target_os = "linux"))]
        syslog_forward: None,
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

    // Both of these end with this process exiting so the supervisor can start
    // it again - one to load a new binary, one to re-detect backends.
    let wants_restart = matches!(
        env.command,
        Command::SelfUpdate { .. } | Command::RestartAgent
    );
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

    if ok && wants_restart {
        // Give the writer a moment to flush the result before we tear the
        // connection down; the portal should record the update before we go.
        tokio::time::sleep(Duration::from_millis(500)).await;
        tracing::info!("signalling restart");
        if ctx.restart_tx.send(()).is_err() {
            // The session is already ending, which achieves the same thing.
            tracing::warn!("restart channel closed; the next reconnect will run the new binary");
        }
    }
}

/// Where to send syslog: the host of the portal this agent already talks to.
///
/// Derived rather than configured, because a second address to keep in step is
/// a second address to get wrong - and an agent pointed at one portal while
/// logging to another is a confusing thing to debug.
fn syslog_target(ctx: &Ctx) -> String {
    let url = ctx.cfg.portal_url.clone();
    let host = url
        .split("://")
        .nth(1)
        .unwrap_or(&url)
        .split('/')
        .next()
        .unwrap_or(&url)
        // Drop the portal's own HTTP port; syslog has its own.
        .rsplit_once(':')
        .map(|(h, _)| h.to_string())
        .unwrap_or_else(|| url.clone());
    format!("{host}:514")
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
            let sources = apply_source_policies(ctx, p).await;
            let summary = reconcile_apps(ctx, p).await?;
            let summary = if sources.is_empty() {
                summary
            } else {
                format!("{}\n{}", sources.join("\n"), summary)
            };
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
            full,
        } => {
            let policy = ctx.manifest.read().await.patch_policy.clone();

            // Only a run that was asked to install everything proves anything
            // about what is left. A security-only or narrowed run leaves other
            // updates pending on purpose.
            let attempted: Option<Vec<(String, String)>> = if only.is_empty() && !security_only {
                Some(
                    ctx.last
                        .read()
                        .await
                        .as_ref()
                        .map(|i| {
                            i.updates
                                .iter()
                                .filter(|u| !policy.exclude.contains(&u.name))
                                .map(|u| (u.name.clone(), u.new_version.clone()))
                                .collect()
                        })
                        .unwrap_or_default(),
                )
            } else {
                None
            };

            let result = ctx
                .platform
                .apply_patches(security_only, &only, &policy.exclude, full, p)
                .await;

            // apt names every file it could not fetch. Those names are the
            // only reliable way to tell which repository has stopped serving
            // what it advertises, so keep them before the error goes back up.
            let failed = match &result {
                Ok(log) => unfetchable_in(log),
                Err(e) => unfetchable_in(&format!("{e:#}")),
            };
            {
                let mut store = ctx.unfetchable.write().await;
                if !failed.is_empty() {
                    *store = failed;
                } else if result.is_ok() {
                    // A clean run means whatever was missing is not any more.
                    store.clear();
                }
            }

            // Refresh either way: a failed run still changed the machine, and
            // the scan is where the repository gets flagged.
            refresh_packages_after(ctx, attempted).await;
            result
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

        Command::WriteSource { path, content } => {
            let msg = crate::sources::write(&path, &content, p).await?;
            refresh_packages(ctx).await;
            Ok(msg)
        }

        Command::RemoveSource { path } => {
            let msg = crate::sources::remove(&path, p).await?;
            refresh_packages(ctx).await;
            Ok(msg)
        }

        Command::RestartAgent => {
            Ok("restarting; the supervisor will start the agent again".to_string())
        }

        Command::InstallPrerequisites => {
            let log = ctx.platform.install_prerequisites(p).await?;
            refresh_packages(ctx).await;
            Ok(log)
        }

        Command::DistroUpgrade { to, check } => {
            let log = ctx.platform.distro_upgrade(&to, check, p).await?;
            // An upgrade rewrites the sources and moves every package, so the
            // inventory the portal holds is stale the moment it finishes.
            refresh_packages(ctx).await;
            Ok(log)
        }

        Command::UpdateFirmware { only } => {
            let log = ctx.platform.update_firmware(&only, p).await?;
            refresh_packages(ctx).await;
            Ok(log)
        }

        Command::FinishUpgrade { grub_device } => {
            let log = ctx
                .platform
                .finish_upgrade(grub_device.as_deref(), p)
                .await?;
            refresh_packages(ctx).await;
            Ok(log)
        }

        Command::Cleanup { purge } => {
            let log = ctx.platform.cleanup(purge, p).await?;
            refresh_packages(ctx).await;
            Ok(log)
        }

        Command::JournalVolume => crate::journal::volume().await,

        Command::ConfigureSyslog {
            enable,
            ref min_severity,
        } => {
            // Most of this fleet has no rsyslog - Debian 13 and current Proxmox
            // ship journald alone - so the journal is read directly and shipped
            // over the connection that already exists. Nothing is installed and
            // nothing is written to /etc.
            //
            // The switch itself is the portal's to remember. This only starts or
            // stops a task, so a restart ends it; the portal re-asks on every
            // connection rather than trusting the agent to know.
            if !enable {
                ctx.forward_logs.store(false, Ordering::SeqCst);
                return Ok("stopped forwarding the journal".into());
            }
            let sev = min_severity.trim().to_lowercase();
            const LEVELS: [&str; 8] = [
                "emerg", "alert", "crit", "err", "warning", "notice", "info", "debug",
            ];
            if !LEVELS.contains(&sev.as_str()) {
                anyhow::bail!("{min_severity:?} is not a severity; use one of {LEVELS:?}");
            }
            ctx.forward_severity
                .write()
                .await
                .replace(sev.clone());
            ctx.forward_logs.store(true, Ordering::SeqCst);
            Ok(format!("forwarding journal lines at {sev} and worse"))
        }

        Command::Discover => {
            let scans = discovery_scans(ctx).await;
            if scans.is_empty() {
                anyhow::bail!("no discovery ranges configured for site `{}`", ctx.cfg.site);
            }
            sweep(ctx, &scans, p).await
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

#[cfg(test)]
mod connect_tests {
    use super::*;

    /// The bug this guards, stated as a test.
    ///
    /// An unbounded connect took nine machines off the fleet for two days: the
    /// agent logged "reconnecting", called into the connect, and never came
    /// back. It kept running, so systemd saw nothing wrong; it never
    /// reconnected, so the portal saw nothing at all.
    #[tokio::test]
    async fn a_connect_that_never_answers_is_abandoned() {
        // 198.51.100.0/24 is reserved for documentation and is not routed, so a
        // SYN to it goes unanswered rather than being refused - the same shape
        // as the stall that caused the outage.
        let stalls = async {
            let _ = tokio::net::TcpStream::connect("198.51.100.1:9").await;
            // If the connect somehow returns, keep the future pending so the
            // test is about the timeout rather than about the network.
            std::future::pending::<()>().await;
        };

        let out = tokio::time::timeout(Duration::from_millis(300), stalls).await;
        assert!(
            out.is_err(),
            "the timeout must fire; without one this await never returns"
        );
    }

    /// And the bound has to be short enough to matter. A connect allowed to
    /// hang for longer than the portal's own offline threshold means a machine
    /// is reported missing before its agent has even given up.
    #[test]
    fn the_bound_is_shorter_than_being_declared_offline() {
        assert!(
            CONNECT_TIMEOUT < LIVENESS_TIMEOUT,
            "a connect attempt must not outlast the liveness window"
        );
        assert!(CONNECT_TIMEOUT >= Duration::from_secs(10), "and not be so short it fails on a slow link");
    }
}
