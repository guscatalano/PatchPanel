//! PatchPanel portal: the central service agents connect to.
//!
//! It holds one manifest — the desired state for OS patches, applications, the
//! agent's own version, and the IoT devices each site should be watching — and
//! keeps every connected agent converged on it.

mod api;
mod db;
mod hub;
mod install;
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

    /// Serve the API and dashboard with no authentication at all.
    ///
    /// Only for a network you fully trust: the API can publish a manifest that
    /// installs software on every agent, and can reboot the whole fleet.
    #[arg(
        long,
        env = "PATCHPANEL_NO_ADMIN_AUTH",
        num_args = 0..=1,
        default_value_t = false,
        default_missing_value = "true",
        value_parser = parse_flag,
    )]
    no_admin_auth: bool,

    /// Directory holding `pp-agent`, `pp-agent.exe` and the systemd unit, which
    /// the portal serves so new machines can install themselves in one line.
    #[arg(long, default_value = "/var/lib/patchpanel-portal/agents", env = "PATCHPANEL_AGENT_DIR")]
    agent_dir: PathBuf,

    #[arg(long, default_value = "info")]
    log: String,
}

/// Accept the spellings an operator actually types in an env file. Clap's
/// built-in bool parser takes only "true"/"false", which makes the obvious
/// `PATCHPANEL_NO_ADMIN_AUTH=1` a startup failure.
fn parse_flag(s: &str) -> std::result::Result<bool, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        other => Err(format!(
            "expected a boolean (1/0, true/false, yes/no, on/off), got `{other}`"
        )),
    }
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

    let require_admin_auth = !cli.no_admin_auth;
    let state = Arc::new(AppState {
        db: Db::open(&cli.db).context("opening the portal database")?,
        hub: Hub::new(),
        enrollment_token: enrollment_token.clone(),
        admin_token: admin_token.clone(),
        require_admin_auth,
        agent_dir: cli.agent_dir.clone(),
        public_host: if cli.bind.ip().is_unspecified() {
            format!("{}:{}", hostname(), cli.bind.port())
        } else {
            cli.bind.to_string()
        },
    });
    if !require_admin_auth {
        tracing::warn!("admin authentication is DISABLED; anyone who can reach this port controls the fleet");
    }

    let manifest = state.db.manifest()?;
    let known = state.db.agents()?.len();

    let app = Router::new()
        // Agents authenticate with their own tokens inside the handshake, so
        // this route sits outside the admin-bearer middleware.
        .route("/api/agent/ws", get(ws::handler))
        .with_state(state.clone())
        .merge(api::public_routes(state.clone()))
        .merge(install::routes(state.clone()))
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
        require_admin_auth,
        manifest.revision,
        known,
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serving")?;
    Ok(())
}

/// Our own hostname, used only as a fallback in generated install scripts.
fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "localhost".into())
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
    require_admin_auth: bool,
    revision: u64,
    known: usize,
) {
    // These lines get copied onto *other* machines, so 127.0.0.1 would be
    // actively wrong. Use our hostname, and omit the port when it is the
    // default so the URL reads like one a person would type.
    let host = if bind.ip().is_unspecified() {
        match bind.port() {
            80 => hostname(),
            p => format!("{}:{p}", hostname()),
        }
    } else if bind.port() == 80 {
        bind.ip().to_string()
    } else {
        bind.to_string()
    };

    println!("\n  PatchPanel portal");
    println!("  dashboard   http://{host}/");
    println!("  agent ws    ws://{host}/api/agent/ws");
    println!("  manifest    revision {revision}, {known} agent(s) on record\n");

    if !require_admin_auth {
        println!("  admin auth        DISABLED (--no-admin-auth)");
        println!("                    anyone who can reach this port controls the fleet");
    } else if generated_admin {
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

    // Never echo an operator-supplied secret: it would land in the journal,
    // which is exactly what the "(from --enrollment-token)" line above claims
    // does not happen.
    let token_hint = if generated_enrollment {
        enrollment
    } else {
        "$PATCHPANEL_ENROLLMENT_TOKEN"
    };
    println!("\n  Add a machine (run as root / elevated):");
    // Without admin auth the agent can ask us for the token itself, so the
    // command needs nothing else. With auth on, it has to be supplied.
    let tok = if require_admin_auth {
        format!(" -- --token {token_hint}")
    } else {
        String::new()
    };
    let wtok = if require_admin_auth {
        format!(" --token {token_hint}")
    } else {
        String::new()
    };
    println!("    linux    curl -fsSL http://{host}/install.sh | sh{tok}");
    // Deliberately no temp-file path: a backslash in a copied command line is
    // one paste away from `$env:TEMPpp.ps1`, and PowerShell's error for that
    // names neither the path nor the cause.
    println!("    windows  irm http://{host}/download/pp-agent.exe -OutFile pp-agent.exe; ./pp-agent.exe setup --portal {host}{wtok}");
    println!("\n  Both accept an optional --site / -Site; it defaults to the hostname.");
    println!("  The dashboard's \"Add machine\" tab has these with the token filled in.\n");
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
