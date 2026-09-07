//! PatchPanel agent — one binary that runs as a systemd unit on Linux and a
//! Windows Service on Windows, keeps its host converged on the portal's
//! manifest, and probes the IoT devices assigned to its site.

mod config;
mod exec;
mod platform;
mod probe;
mod selfupdate;
mod session;

#[cfg(windows)]
mod service;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "pp-agent",
    version,
    about = "PatchPanel agent: keeps a machine and its devices in sync with the portal"
)]
struct Cli {
    /// Config file. Defaults to /etc/patchpanel/agent.json or
    /// %ProgramData%\PatchPanel\agent.json.
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Log filter, e.g. "debug" or "pp_agent=debug,info".
    #[arg(long, global = true, default_value = "info")]
    log: String,

    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Connect to the portal and stay connected (the default).
    Run,

    /// Write a config file so the service knows which portal to join.
    Enroll {
        #[arg(long)]
        portal: String,
        /// Enrollment secret from the portal operator.
        #[arg(long, default_value = "")]
        token: String,
        /// Collector site; selects which devices this agent probes.
        #[arg(long, default_value = "")]
        site: String,
        /// Where identity and tokens live. Must be writable by the service.
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },

    /// Print what this machine looks like, without contacting the portal.
    Inventory,

    /// Probe one device ad hoc, to check a manifest entry before committing it.
    Probe {
        /// IP or hostname.
        target: String,
        /// snmp | http | tcp
        #[arg(long, default_value = "snmp")]
        kind: String,
        /// SNMP community, or the TCP port, depending on --kind.
        #[arg(long)]
        arg: Option<String>,
    },

    /// Register the Windows Service. Run from an elevated prompt.
    #[cfg(windows)]
    InstallService,

    /// Remove the Windows Service.
    #[cfg(windows)]
    UninstallService,

    /// Service Control Manager entry point. Not for interactive use.
    #[cfg(windows)]
    #[command(hide = true)]
    RunService,
}

fn main() -> Result<()> {
    // The SCM starts us with this argument and expects the dispatcher to be
    // running within 30 seconds, so it is handled before anything else.
    #[cfg(windows)]
    if std::env::args().nth(1).as_deref() == Some("run-service") {
        init_logging("info");
        return service::run_as_service();
    }

    let cli = Cli::parse();
    init_logging(&cli.log);

    let config_path = cli.config.clone().unwrap_or_else(config::default_config_path);

    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async move {
        match cli.cmd.unwrap_or(Cmd::Run) {
            Cmd::Run => {
                let cfg = config::Config::load(&config_path)?;
                session::run(cfg).await
            }

            Cmd::Enroll {
                portal,
                token,
                site,
                state_dir,
            } => {
                let mut cfg = config::Config {
                    portal_url: portal,
                    enrollment_token: token,
                    site,
                    ..Default::default()
                };
                if let Some(dir) = state_dir {
                    cfg.state_dir = dir;
                } else if let Ok(dir) = std::env::var("PATCHPANEL_STATE_DIR") {
                    cfg.state_dir = PathBuf::from(dir);
                }
                cfg.save(&config_path)?;
                println!("wrote {}", config_path.display());
                println!("portal: {}", cfg.portal_url);
                if cfg.site.is_empty() {
                    println!("site:   (none) — this collector probes devices with no site set");
                } else {
                    println!("site:   {}", cfg.site);
                }
                println!("state:  {}", cfg.state_dir.display());
                Ok(())
            }

            Cmd::Inventory => local_inventory().await,

            Cmd::Probe { target, kind, arg } => probe_once(target, kind, arg).await,

            #[cfg(windows)]
            Cmd::InstallService => service::install(),
            #[cfg(windows)]
            Cmd::UninstallService => service::uninstall(),
            #[cfg(windows)]
            Cmd::RunService => service::run_as_service(),
        }
    })
}

fn init_logging(filter: &str) {
    use tracing_subscriber::EnvFilter;
    let env = EnvFilter::try_from_env("PATCHPANEL_LOG")
        .unwrap_or_else(|_| EnvFilter::new(filter));
    tracing_subscriber::fmt()
        .with_env_filter(env)
        .with_target(false)
        .init();
}

/// `pp-agent inventory` — a dry run of what the agent would report, useful for
/// checking backend detection on a new image before enrolling it.
async fn local_inventory() -> Result<()> {
    let pf = platform::Platform::detect();
    let p = exec::Progress::detached();

    println!("host:     {}", gethostname::gethostname().to_string_lossy());
    println!("os:       {}", platform::Platform::os_version());
    println!("arch:     {}", std::env::consts::ARCH);
    println!("backends: {}", pf.backend_names().join(", "));
    println!("reboot:   {}", pf.reboot_required().await);

    let packages = pf.installed_packages(&p).await?;
    println!("\n{} installed package(s)", packages.len());
    for pkg in packages.iter().take(15) {
        println!("  {:<40} {}", pkg.name, pkg.version);
    }
    if packages.len() > 15 {
        println!("  ... and {} more", packages.len() - 15);
    }

    let updates = pf.available_updates(&p).await?;
    println!("\n{} pending update(s)", updates.len());
    for u in &updates {
        let flag = if u.security { " [security]" } else { "" };
        println!("  {:<40} {} -> {}{}", u.name, u.current_version, u.new_version, flag);
    }
    Ok(())
}

/// `pp-agent probe` — check that a device answers before adding it to the
/// manifest, so a typo shows up here rather than as a red row in the portal.
async fn probe_once(target: String, kind: String, arg: Option<String>) -> Result<()> {
    use pp_proto::{DeviceSpec, Probe};

    let probe = match kind.as_str() {
        "snmp" => Probe::Snmp {
            community: arg.unwrap_or_else(|| "public".into()),
            oid: None,
            version_regex: None,
        },
        "http" => Probe::Http {
            url: if target.starts_with("http") {
                target.clone()
            } else {
                format!("http://{target}/")
            },
            insecure: true,
            headers: Vec::new(),
            version_json_pointer: None,
            version_regex: None,
        },
        "tcp" => Probe::Tcp {
            port: arg
                .as_deref()
                .unwrap_or("80")
                .parse()
                .map_err(|_| anyhow::anyhow!("--arg must be a port number for --kind tcp"))?,
            read_banner: true,
            version_regex: None,
        },
        other => anyhow::bail!("unknown probe kind `{other}` (expected snmp, http, or tcp)"),
    };

    let spec = DeviceSpec {
        id: "adhoc".into(),
        label: String::new(),
        target,
        probe,
        site: String::new(),
        expect_version: None,
        tags: Vec::new(),
    };

    let report = probe::probe_one(&spec).await;
    println!("target:    {}", report.target);
    println!("reachable: {}", report.reachable);
    if let Some(ms) = report.latency_ms {
        println!("latency:   {ms}ms");
    }
    if let Some(fw) = &report.firmware {
        println!("version:   {fw}");
    }
    if let Some(err) = &report.error {
        println!("error:     {err}");
    }
    if !report.detail.is_empty() {
        println!("detail:    {}", report.detail);
    }
    Ok(())
}
