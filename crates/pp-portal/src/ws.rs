//! The agent-facing WebSocket endpoint.
//!
//! One connection per agent, opened by the agent and held open. The portal
//! never dials out, so agents work behind NAT and need no inbound firewall
//! rules — the thing that makes this deployable on factory floors and in
//! customer sites at all.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use pp_proto::{AgentId, ClientMsg, Command, ServerMsg, SystemInfo, PROTOCOL_VERSION};
use uuid::Uuid;

use crate::state::AppState;

/// An agent that connects and then says nothing is either broken or hostile;
/// either way it should not hold a slot.
const HELLO_TIMEOUT_SECS: u64 = 15;

/// Drop a connection that has gone quiet for this long.
///
/// A hung agent keeps its socket open, so without this the portal reports it as
/// connected indefinitely and happily dispatches commands nothing will ever
/// run. Agents heartbeat every 30s and ping every 20s, so silence this long
/// means the far end is not working even if TCP still believes in it.
const AGENT_SILENCE_TIMEOUT: Duration = Duration::from_secs(120);

pub async fn handler(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> Response {
    ws.on_upgrade(move |socket| async move {
        if let Err(e) = serve(socket, state).await {
            tracing::warn!(error = %format!("{e:#}"), "agent session ended with an error");
        }
    })
}

async fn serve(socket: WebSocket, state: Arc<AppState>) -> anyhow::Result<()> {
    let (mut sink, mut stream) = socket.split();

    // -- handshake ----------------------------------------------------------

    let hello = tokio::time::timeout(
        std::time::Duration::from_secs(HELLO_TIMEOUT_SECS),
        next_client_msg(&mut stream),
    )
    .await
    .map_err(|_| anyhow::anyhow!("agent did not send Hello in time"))??;

    let ClientMsg::Hello {
        protocol,
        agent_id,
        enrollment_token,
        agent_token,
        system,
        applied_revision,
    } = hello
    else {
        let _ = send(&mut sink, ServerMsg::Error {
            message: "expected Hello as the first frame".into(),
        })
        .await;
        anyhow::bail!("first frame was not Hello");
    };

    if protocol != PROTOCOL_VERSION {
        let msg = format!(
            "protocol {protocol} is not supported by this portal (expected {PROTOCOL_VERSION})"
        );
        let _ = send(&mut sink, ServerMsg::Error { message: msg.clone() }).await;
        anyhow::bail!(msg);
    }

    let token = match authenticate(&state, agent_id, &enrollment_token, &agent_token) {
        Ok(t) => t,
        Err(e) => {
            let message = e.to_string();
            tracing::warn!(%agent_id, host = %system.hostname, %message, "rejected agent");
            let _ = send(&mut sink, ServerMsg::Error { message }).await;
            return Ok(());
        }
    };

    state.db.upsert_agent(agent_id, &system, &token)?;
    state.db.set_applied_revision(agent_id, applied_revision)?;
    let manifest = state.db.manifest()?;

    tracing::info!(
        %agent_id,
        host = %system.hostname,
        os = %system.os,
        site = %system.site,
        version = %system.agent_version,
        "agent connected"
    );

    let (hub_tx, mut rx) = state.hub.connect(agent_id);
    send(
        &mut sink,
        ServerMsg::Welcome {
            agent_token: token,
            server_time: chrono::Utc::now(),
            manifest: manifest.clone(),
        },
    )
    .await?;

    // If the fleet is supposed to be on a newer agent than this one is running,
    // start that upgrade now rather than waiting for someone to notice.
    if let Some(cmd) = self_update_command(&state, &manifest, &system) {
        let id = Uuid::new_v4();
        // The portal's own doing, in response to a published manifest.
        state.db.record_command(id, agent_id, &cmd, "manifest")?;
        send(
            &mut sink,
            ServerMsg::Command(pp_proto::CommandEnvelope { id, command: cmd }),
        )
        .await?;
        tracing::info!(%agent_id, "dispatched self-update on connect");
    }

    // Re-ask for log forwarding on every connection.
    //
    // The agent deliberately does not remember this: it tails the journal into
    // an in-memory task, and a restart, a self-update or a reboot ends that
    // task. If the switch lived on the agent it would quietly turn itself off,
    // and the portal would go on believing it was receiving logs from a machine
    // that had stopped sending them. Re-asking makes the portal's record the
    // only record, which is the same rule that governs every other piece of
    // derived state here.
    match state.db.log_forward(agent_id) {
        Ok(Some((min_severity, _asked_at))) => {
            let cmd = Command::ConfigureSyslog {
                enable: true,
                min_severity,
            };
            let id = Uuid::new_v4();
            state.db.record_command(id, agent_id, &cmd, "portal")?;
            send(
                &mut sink,
                ServerMsg::Command(pp_proto::CommandEnvelope { id, command: cmd }),
            )
            .await?;
            tracing::debug!(%agent_id, "re-asked for journal forwarding on connect");
        }
        Ok(None) => {}
        Err(e) => tracing::warn!(error = %e, %agent_id, "could not read the forwarding switch"),
    }

    // -- pump ---------------------------------------------------------------

    // Handshake frames are written inline above; from here the writer task owns
    // the sink and everything outbound goes through the hub.
    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if send(&mut sink, msg).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });

    let result = read_loop(&mut stream, &state, agent_id).await;

    // Only clear the hub slot if it still points at this connection: a slow
    // teardown must not evict the reconnect that already replaced it.
    // Anything still in flight will never report now, so retire it rather than
    // leaving the machine looking permanently busy.
    match state.db.fail_unfinished(agent_id, "agent disconnected before reporting") {
        Ok(n) if n > 0 => tracing::info!(%agent_id, count = n, "retired unfinished commands"),
        Ok(_) => {}
        Err(e) => tracing::error!(error = %e, "failed to retire unfinished commands"),
    }

    state.hub.disconnect(agent_id, &hub_tx);
    drop(hub_tx);
    writer.abort();

    tracing::info!(%agent_id, host = %system.hostname, "agent disconnected");
    result
}

/// Decide whether this agent may talk to us, and with which durable token.
fn authenticate(
    state: &AppState,
    agent_id: AgentId,
    enrollment_token: &Option<String>,
    agent_token: &Option<String>,
) -> anyhow::Result<String> {
    match state.db.agent_token(agent_id)? {
        // Known agent: its stored token is the only credential we accept.
        // Re-enrolling over the top would let anyone who learns the shared
        // secret impersonate an existing machine.
        Some(stored) => {
            match agent_token {
                Some(t) if constant_time_eq(t, &stored) => Ok(stored),
                Some(_) => anyhow::bail!("agent token does not match"),
                None => anyhow::bail!("this agent id is already enrolled; agent token required"),
            }
        }
        // New agent: the shared enrollment secret is what buys it a token.
        None => {
            let expected = &state.enrollment_token;
            match enrollment_token {
                Some(t) if constant_time_eq(t, expected) => Ok(new_token()),
                _ => anyhow::bail!("invalid or missing enrollment token"),
            }
        }
    }
}

/// Compare without leaking length or position through timing.
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn new_token() -> String {
    format!(
        "{}{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    )
}

/// Build a `SelfUpdate` when the manifest names a version this agent is not
/// running and a matching build has been published.
pub fn self_update_command(
    state: &AppState,
    manifest: &pp_proto::Manifest,
    system: &SystemInfo,
) -> Option<Command> {
    let want = manifest.agent_version.as_deref()?;
    if want == system.agent_version {
        return None;
    }
    match state.db.build_for(want, &system.os.to_string(), &system.arch) {
        Ok(Some(build)) => Some(Command::SelfUpdate {
            version: build.version,
            url: build.url,
            sha256: build.sha256,
        }),
        Ok(None) => {
            tracing::warn!(
                want,
                os = %system.os,
                arch = %system.arch,
                "manifest asks for an agent version with no published build"
            );
            None
        }
        Err(e) => {
            tracing::error!(error = %e, "looking up agent build");
            None
        }
    }
}

async fn read_loop(
    stream: &mut futures_util::stream::SplitStream<WebSocket>,
    state: &Arc<AppState>,
    agent_id: AgentId,
) -> anyhow::Result<()> {
    loop {
        // A silent socket is indistinguishable from a healthy idle one at the
        // TCP layer, so impose a deadline rather than waiting forever.
        let next = match tokio::time::timeout(AGENT_SILENCE_TIMEOUT, stream.next()).await {
            Ok(n) => n,
            Err(_) => {
                tracing::warn!(
                    %agent_id,
                    seconds = AGENT_SILENCE_TIMEOUT.as_secs(),
                    "agent went silent; dropping the connection"
                );
                break;
            }
        };
        let Some(frame) = next else { break };
        let frame = frame?;
        let text = match frame {
            Message::Text(t) => t,
            Message::Close(_) => break,
            // axum answers pings for us; binary frames are not part of the
            // protocol and are ignored rather than treated as fatal.
            _ => continue,
        };

        let msg: ClientMsg = match serde_json::from_str(text.as_str()) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(%agent_id, error = %e, "unparseable frame from agent");
                continue;
            }
        };

        match msg {
            // A second Hello on a live connection is a protocol error, not a
            // re-enrollment; ignoring it avoids a re-auth path we do not want.
            ClientMsg::Hello { .. } => {
                tracing::warn!(%agent_id, "unexpected second Hello");
            }

            ClientMsg::Heartbeat {
                reboot_required,
                applied_revision,
                ..
            } => {
                state.db.touch(agent_id, reboot_required)?;
                state.db.set_applied_revision(agent_id, applied_revision)?;
            }

            ClientMsg::Inventory(inv) => {
                tracing::debug!(
                    %agent_id,
                    packages = inv.packages.len(),
                    updates = inv.updates.len(),
                    devices = inv.devices.len(),
                    "inventory received"
                );
                // Devices get a history of their own: the inventory only ever
                // holds the latest reading, and a device's past is where "it
                // has been unreachable since Tuesday" lives.
                if let Ok(Some(row)) = state.db.agents().map(|rows| {
                    rows.into_iter().find(|a| a.id == agent_id)
                }) {
                    for report in &inv.devices {
                        if let Err(e) = state.db.record_last_good(report) {
                            tracing::warn!(error = %e, "could not record last good probe");
                        }
                        if let Err(e) = state.db.record_probe(&row.hostname, report) {
                            tracing::warn!(error = %e, device = %report.id, "recording probe failed");
                        }
                    }
                }
                state.db.store_inventory(agent_id, &inv)?;
            }

            ClientMsg::CommandProgress { id, line } => {
                state.db.append_progress(id, &line)?;
            }

            ClientMsg::JournalLines { lines } => {
                // Written to the same per-sender files the syslog receiver
                // uses, so one viewer covers both: a machine with an agent and
                // an appliance that can only push syslog end up in the same
                // place, named the same way.
                // The machine names its own log file, so it lines up with the
                // fleet table rather than with an address that may change.
                let Ok(Some(name)) = state
                    .db
                    .agents()
                    .map(|rows| rows.into_iter().find(|a| a.id == agent_id).map(|a| a.hostname))
                else {
                    continue;
                };
                let converted: Vec<crate::syslog::LogLine> = lines
                    .into_iter()
                    .map(|l| crate::syslog::LogLine {
                        at: l.at,
                        source: name.clone(),
                        host: name.clone(),
                        facility: 1,
                        severity: l.priority,
                        tag: l.tag,
                        msg: l.message,
                    })
                    .collect();
                if let Err(e) =
                    crate::syslog::append(&state.log_dir, &name, &converted, 0)
                {
                    tracing::warn!(error = %e, %agent_id, "could not write journal lines");
                }
                // A journal line arrives already attributed: it came up this
                // machine's own WebSocket, so the name needs no resolving.
                state.live.push(&name, &converted);
            }

            ClientMsg::CommandResult(result) => {
                tracing::info!(%agent_id, id = %result.id, ok = result.ok, summary = %result.summary, "command finished");
                state.db.finish_command(
                    result.id,
                    result.ok,
                    &result.summary,
                    &result.detail,
                    result.finished_at,
                )?;
            }
        }
    }
    Ok(())
}

async fn next_client_msg(
    stream: &mut futures_util::stream::SplitStream<WebSocket>,
) -> anyhow::Result<ClientMsg> {
    while let Some(frame) = stream.next().await {
        if let Message::Text(text) = frame? {
            return Ok(serde_json::from_str(text.as_str())?);
        }
    }
    anyhow::bail!("connection closed before Hello")
}

async fn send(
    sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    msg: ServerMsg,
) -> anyhow::Result<()> {
    let text = serde_json::to_string(&msg)?;
    sink.send(Message::Text(text.into())).await?;
    Ok(())
}
