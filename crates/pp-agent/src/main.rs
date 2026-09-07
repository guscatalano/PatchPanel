//! PatchPanel agent — one binary that runs as a systemd unit on Linux and a
//! Windows Service on Windows, keeps its host converged on the portal's
//! manifest, and probes the IoT devices assigned to its site.

mod config;
mod exec;
mod hardware;
mod platform;
mod probe;
mod release;
mod repos;
mod selfupdate;
mod session;
mod sources;

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

    /// Enroll, install the service, and start it - the whole install in one go.
    ///
    /// This is what the portal's one-line installers call. It exists so adding
    /// a machine never depends on getting shell quoting right.
    Setup {
        /// Portal address. A bare hostname is enough ("patchpanel"); a full
        /// http:// or ws:// URL also works.
        #[arg(long)]
        portal: String,
        /// Enrollment secret. Omitted, the agent asks the portal for it, which
        /// works when the portal has admin auth disabled.
        #[arg(long, default_value = "")]
        token: String,
        /// Defaults to this machine's hostname.
        #[arg(long, default_value = "")]
        site: String,
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

            Cmd::Setup {
                portal,
                token,
                site,
                state_dir,
            } => {
                let (ws_url, http_base) = normalize_portal(&portal)?;
                let token = if token.is_empty() {
                    fetch_enrollment_token(&http_base).await?
                } else {
                    token
                };
                let site = if site.is_empty() {
                    gethostname::gethostname().to_string_lossy().into_owned()
                } else {
                    site
                };
                let mut cfg = config::Config {
                    portal_url: ws_url,
                    enrollment_token: token,
                    site,
                    ..Default::default()
                };
                if let Some(dir) = state_dir {
                    cfg.state_dir = dir;
                }
                setup(cfg, &config_path).await
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

/// Accept whatever an operator types: a bare host, an http(s) URL, or a full
/// ws:// endpoint. Returns the agent websocket URL and the matching http base.
///
/// This exists because "--portal patchpanel" is what people try first, and
/// making that work removes the most common install-time mistake.
pub fn normalize_portal(input: &str) -> Result<(String, String)> {
    let s = input.trim();

    // Strip the scheme *before* touching slashes. Trimming them first turns
    // "http://" into "http:", which then looks like a perfectly good hostname
    // and yields "ws://http:/api/agent/ws".
    let (secure, rest) = if let Some(r) = s.strip_prefix("wss://") {
        (true, r)
    } else if let Some(r) = s.strip_prefix("ws://") {
        (false, r)
    } else if let Some(r) = s.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = s.strip_prefix("http://") {
        (false, r)
    } else {
        (false, s)
    };

    // Drop any path the operator pasted; we know the endpoint we need.
    let host = rest.split('/').next().unwrap_or("").trim();
    let (name, port) = match host.split_once(':') {
        Some((n, p)) => (n, Some(p)),
        None => (host, None),
    };
    if name.is_empty() {
        anyhow::bail!("`{input}` has no hostname");
    }
    if let Some(p) = port {
        if p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()) {
            anyhow::bail!("`{input}` has an invalid port");
        }
    }

    let (ws, http) = if secure { ("wss", "https") } else { ("ws", "http") };
    Ok((
        format!("{ws}://{host}/api/agent/ws"),
        format!("{http}://{host}"),
    ))
}

/// Ask the portal for the shared enrollment secret. Only succeeds when the
/// portal is running without admin auth; otherwise the operator must supply it.
async fn fetch_enrollment_token(http_base: &str) -> Result<String> {
    use anyhow::Context as _;

    let url = format!("{http_base}/api/enrollment");
    let resp = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?
        .get(&url)
        .send()
        .await
        .with_context(|| format!("asking {url} for an enrollment token"))?;

    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        anyhow::bail!(
            "this portal requires authentication, so it will not hand out the              enrollment token. Pass it explicitly:  --token <enrollment-token>
             You can copy the whole command from the portal's \"Add machine\" tab."
        );
    }
    let resp = resp.error_for_status()?;

    #[derive(serde::Deserialize)]
    struct Enrollment {
        token: String,
    }
    let body: Enrollment = resp.json().await.context("reading the enrollment token")?;
    if body.token.is_empty() {
        anyhow::bail!("the portal returned an empty enrollment token");
    }
    println!("==> got an enrollment token from {http_base}");
    Ok(body.token)
}

/// Where the agent must live for a service to reference it reliably.
fn canonical_binary_path() -> PathBuf {
    if cfg!(windows) {
        let base = std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into());
        PathBuf::from(base).join("PatchPanel").join("pp-agent.exe")
    } else {
        PathBuf::from("/usr/local/bin/pp-agent")
    }
}

/// The systemd unit, embedded so a Linux install needs nothing but the binary.
#[cfg(unix)]
const SYSTEMD_UNIT: &str = include_str!("../../../deploy/patchpanel-agent.service");

/// Write the config, register the service, and start it. Idempotent: running
/// it again upgrades in place and keeps the machine's existing identity.
async fn setup(cfg: config::Config, config_path: &std::path::Path) -> Result<()> {
    use anyhow::Context as _;

    // The service records an absolute path to its binary, so the agent must
    // live somewhere permanent before we register it. A one-line installer
    // downloads to a temp directory or the current folder, and neither of those
    // survives; relocate ourselves first.
    let canonical = canonical_binary_path();
    let current = std::env::current_exe().context("locating current executable")?;
    let relocated = current != canonical;

    if relocated {
        if let Some(dir) = canonical.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // On Windows a running service holds its image open, so stop it before
        // overwriting. Harmless when no service exists yet.
        #[cfg(windows)]
        {
            let p = exec::Progress::detached();
            let _ = exec::run("sc.exe", &["stop", service::SERVICE_NAME], &p).await;
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        std::fs::copy(&current, &canonical)
            .with_context(|| format!("installing agent to {}", canonical.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&canonical, std::fs::Permissions::from_mode(0o755))?;
        }
        println!("==> installed agent to {}", canonical.display());
    }

    cfg.save(config_path)?;
    println!("==> wrote {}", config_path.display());

    // Restrict the config: until the portal issues a per-agent token, it holds
    // the shared enrollment secret.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(config_path)?.permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(config_path, perms)?;
    }

    std::fs::create_dir_all(&cfg.state_dir)?;
    println!("==> enrolling with {} as site '{}'", cfg.portal_url, cfg.site);

    #[cfg(unix)]
    {
        let unit = std::path::Path::new("/etc/systemd/system/patchpanel-agent.service");
        std::fs::write(unit, SYSTEMD_UNIT)
            .with_context(|| format!("writing {}", unit.display()))?;
        println!("==> installed {}", unit.display());

        let p = exec::Progress::detached();
        exec::run("systemctl", &["daemon-reload"], &p).await?.require(&[])?;
        exec::run("systemctl", &["enable", "patchpanel-agent"], &p)
            .await?
            .require(&[])?;
        // `enable --now` starts a stopped service but leaves a running one
        // alone, so re-running the installer would not pick up the new binary.
        // Restart is correct whether or not it was running, and makes this
        // command a reliable way to recover a stuck agent.
        exec::run("systemctl", &["restart", "patchpanel-agent"], &p)
            .await?
            .require(&[])?;
        println!("==> service started");
        println!("    logs: journalctl -u patchpanel-agent -f");
    }

    #[cfg(windows)]
    {
        let p = exec::Progress::detached();
        // Register via the installed copy, so the service points at the
        // permanent path rather than wherever this process was launched from.
        let bin = canonical.to_string_lossy().to_string();
        let out = exec::run(&bin, &["install-service"], &p).await?;
        if !out.ok() {
            // Already registered is fine; anything else is not.
            println!("==> service already registered");
        }
        // Always reapply recovery settings, not just on first install: an
        // agent installed before the failure flag was set would otherwise
        // never restart itself after a self-update.
        service::configure_recovery()?;
        println!("==> recovery actions configured");
        let _ = exec::run("sc.exe", &["stop", service::SERVICE_NAME], &p).await;
        exec::run("sc.exe", &["start", service::SERVICE_NAME], &p)
            .await?
            .require(&[])?;
        println!("==> service started");
        println!("    check: Get-Service {}", service::SERVICE_NAME);
    }

    println!("==> done");
    Ok(())
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

    let hw = hardware::collect();
    println!("
cpu:      {}", hw.cpu_model);
    println!("cores:    {} physical / {} logical", hw.cpu_cores, hw.cpu_threads);
    println!("memory:   {} MB ({:.1} GB)", hw.memory_mb, hw.memory_mb as f64 / 1024.0);
    println!("ip:       {}", hw.ip_addresses.join(", "));
    println!("kernel:   {}", hw.kernel);
    if !hw.vendor.is_empty() {
        println!("vendor:   {}", hw.vendor);
    }

    let packages = pf.installed_packages(&p).await?;
    println!("\n{} installed package(s)", packages.len());
    for pkg in packages.iter().take(15) {
        println!("  {:<40} {}", pkg.name, pkg.version);
    }
    if packages.len() > 15 {
        println!("  ... and {} more", packages.len() - 15);
    }

    let repos = repos::collect();
    println!("\n{} package source(s)", repos.len());
    for r in &repos {
        let off = if r.enabled { "" } else { "  [disabled]" };
        println!("  {:<8} {:<50} {}{}", r.source, r.uri, r.suite, off);
    }

    if let Some(rel) = release::collect(&repos) {
        println!("
release:  {} {} ({})", rel.distro, rel.version_id, rel.codename);
        if let Some(n) = &rel.next {
            println!("next:     {n}");
        }
        for f in &rel.findings {
            println!("  [{:?}] {}", f.severity, f.summary);
        }
    }

    let (updates, mut issues) = pf.available_updates(&p).await?;
    issues.extend(pf.scan_issues());
    if !issues.is_empty() {
        println!(
            "\n{} backend(s) could not be scanned, so the count below is a floor:",
            issues.len()
        );
        for i in &issues {
            println!("  [{}] {}", i.backend, i.problem);
        }
    }

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

#[cfg(test)]
mod tests {
    use super::normalize_portal;

    #[test]
    fn accepts_whatever_an_operator_types() {
        let cases = [
            // what they type                     ws url                            http base
            ("patchpanel", "ws://patchpanel/api/agent/ws", "http://patchpanel"),
            ("patchpanel/", "ws://patchpanel/api/agent/ws", "http://patchpanel"),
            ("http://patchpanel", "ws://patchpanel/api/agent/ws", "http://patchpanel"),
            ("http://patchpanel/", "ws://patchpanel/api/agent/ws", "http://patchpanel"),
            ("ws://patchpanel/api/agent/ws", "ws://patchpanel/api/agent/ws", "http://patchpanel"),
            ("patchpanel:8080", "ws://patchpanel:8080/api/agent/ws", "http://patchpanel:8080"),
            ("https://pp.example.com", "wss://pp.example.com/api/agent/ws", "https://pp.example.com"),
            ("wss://pp.example.com/api/agent/ws", "wss://pp.example.com/api/agent/ws", "https://pp.example.com"),
            ("  patchpanel  ", "ws://patchpanel/api/agent/ws", "http://patchpanel"),
            ("192.168.6.59", "ws://192.168.6.59/api/agent/ws", "http://192.168.6.59"),
        ];
        for (input, want_ws, want_http) in cases {
            let (ws, http) = normalize_portal(input).expect(input);
            assert_eq!(ws, want_ws, "ws url for `{input}`");
            assert_eq!(http, want_http, "http base for `{input}`");
        }
    }

    #[test]
    fn rejects_input_with_no_host() {
        for bad in [
            "", "   ", "http://", "https://", "ws://", "ws:///api/agent/ws",
            "http:", ":8080", "patchpanel:", "patchpanel:abc", "patchpanel:80x",
        ] {
            assert!(normalize_portal(bad).is_err(), "`{bad}` should be rejected");
        }
    }
}
