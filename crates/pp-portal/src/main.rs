//! PatchPanel portal: the central service agents connect to.
//!
//! It holds one manifest — the desired state for OS patches, applications, the
//! agent's own version, and the IoT devices each site should be watching — and
//! keeps every connected agent converged on it.

mod api;
mod db;
mod hub;
mod state;
mod ui;
mod ws;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::routing::get;
use axum::Router;
use clap::Parser;
use tower_http::trace::TraceLayer;
use uuid::Uuid;

use crate::db::Db;
use crate::hub::Hub;
use crate::state::AppState;

#[derive(Parser)]
#[command(name = "pp-portal", version, about = "PatchPanel portal")]
struct Cli {
    /// Address to listen on.
    #[arg(long, default_value = "0.0.0.0:8080", env = "PATCHPANEL_BIND")]
    bind: SocketAddr,

    /// SQLite database file. Created if missing.
    #[arg(long, default_value = "data/patchpanel.db", env = "PATCHPANEL_DB")]
    db: PathBuf,

    /// Shared secret new agents present to enroll. Generated if omitted.
    #[arg(long, env = "PATCHPANEL_ENROLLMENT_TOKEN")]
    enrollment_token: Option<String>,

    /// Bearer token for the dashboard and REST API. Generated if omitted.
    #[arg(long, env = "PATCHPANEL_ADMIN_TOKEN")]
    admin_token: Option<String>,

    #[arg(long, default_value = "info")]
    log: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("PATCHPANEL_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&cli.log)),
        )
        .with_target(false)
        .init();

    let generated_enrollment = cli.enrollment_token.is_none();
    let generated_admin = cli.admin_token.is_none();
    let enrollment_token = cli.enrollment_token.unwrap_or_else(random_token);
    let admin_token = cli.admin_token.unwrap_or_else(random_token);

    let state = Arc::new(AppState {
        db: Db::open(&cli.db).context("opening the portal database")?,
        hub: Hub::new(),
        enrollment_token: enrollment_token.clone(),
        admin_token: admin_token.clone(),
    });

    let manifest = state.db.manifest()?;
    let known = state.db.agents()?.len();

    let app = Router::new()
        // Agents authenticate with their own tokens inside the handshake, so
        // this route sits outside the admin-bearer middleware.
        .route("/api/agent/ws", get(ws::handler))
        .with_state(state.clone())
        .merge(api::routes(state.clone()))
        .merge(ui::routes())
        .layer(TraceLayer::new_for_http());

    let listener = tokio::net::TcpListener::bind(cli.bind)
        .await
        .with_context(|| format!("binding {}", cli.bind))?;

    banner(
        &cli.bind,
        &enrollment_token,
        &admin_token,
        generated_enrollment,
        generated_admin,
        manifest.revision,
        known,
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serving")?;
    Ok(())
}

/// 256 bits of randomness, which is what `Uuid::new_v4` gives us twice over
/// without pulling in another RNG.
fn random_token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

#[allow(clippy::too_many_arguments)]
fn banner(
    bind: &SocketAddr,
    enrollment: &str,
    admin: &str,
    generated_enrollment: bool,
    generated_admin: bool,
    revision: u64,
    known: usize,
) {
    let host = if bind.ip().is_unspecified() {
        format!("127.0.0.1:{}", bind.port())
    } else {
        bind.to_string()
    };

    println!("\n  PatchPanel portal");
    println!("  dashboard   http://{host}/");
    println!("  agent ws    ws://{host}/api/agent/ws");
    println!("  manifest    revision {revision}, {known} agent(s) on record\n");

    if generated_admin {
        // Only printed when generated: an operator-supplied token should not
        // be echoed into logs or a terminal recording.
        println!("  admin token       {admin}");
    } else {
        println!("  admin token       (from --admin-token)");
    }
    if generated_enrollment {
        println!("  enrollment token  {enrollment}");
    } else {
        println!("  enrollment token  (from --enrollment-token)");
    }
    if generated_admin || generated_enrollment {
        println!("\n  These are regenerated on every restart. Pass --admin-token and");
        println!("  --enrollment-token (or the matching env vars) to keep them stable.");
    }

    println!("\n  Enroll an agent with:");
    println!("    pp-agent enroll --portal ws://{host}/api/agent/ws \\");
    println!("      --token {enrollment} --site <site>\n");
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    tracing::info!("shutting down");
}
