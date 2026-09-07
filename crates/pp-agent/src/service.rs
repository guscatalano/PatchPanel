//! Windows Service integration.
//!
//! On Linux the agent is an ordinary foreground process and systemd supervises
//! it, so there is nothing to do there. Windows needs the process to register a
//! control handler and report its state to the SCM, which is all this is.

#![cfg(windows)]

use std::ffi::OsString;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result};
use windows_service::service::{
    ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode,
    ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_service::{define_windows_service, service_dispatcher};

pub const SERVICE_NAME: &str = "PatchPanelAgent";
const DISPLAY_NAME: &str = "PatchPanel Agent";
const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;

define_windows_service!(ffi_service_main, service_main);

/// Entry point when the SCM starts us. Returns once the service stops.
pub fn run_as_service() -> Result<()> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
        .context("registering with the service control manager")?;
    Ok(())
}

fn service_main(_args: Vec<OsString>) {
    if let Err(e) = service_body() {
        // Nothing is attached to stdout under the SCM, so the event log and
        // the agent's own log file are the only places this can surface.
        tracing::error!(error = %format!("{e:#}"), "service exited with an error");
    }
}

fn service_body() -> Result<()> {
    let (shutdown_tx, shutdown_rx) = mpsc::channel();

    let handler = move |control| -> ServiceControlHandlerResult {
        match control {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                let _ = shutdown_tx.send(());
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };

    let status_handle = service_control_handler::register(SERVICE_NAME, handler)
        .context("registering service control handler")?;

    let report = |state: ServiceState, accept: ServiceControlAccept| ServiceStatus {
        service_type: SERVICE_TYPE,
        current_state: state,
        controls_accepted: accept,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::from_secs(10),
        process_id: None,
    };

    status_handle.set_service_status(report(
        ServiceState::Running,
        ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
    ))?;

    let runtime = tokio::runtime::Runtime::new().context("starting the async runtime")?;

    // The agent loop returns only after a self-update has put a new binary in
    // place. Watch for that as well as for the SCM's stop request: without it
    // the service sits here "running" with nothing inside it, and a Windows
    // agent silently stops reporting the moment it upgrades itself.
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let agent = runtime.spawn(async move {
        let path = crate::config::default_config_path();
        match crate::config::Config::load(&path) {
            Ok(cfg) => {
                if let Err(e) = crate::session::run(cfg).await {
                    tracing::error!(error = %format!("{e:#}"), "agent stopped");
                }
            }
            Err(e) => tracing::error!(error = %format!("{e:#}"), "failed to load config"),
        }
        let _ = done_tx.send(());
    });

    let mut replaced = false;
    loop {
        if shutdown_rx.try_recv().is_ok() {
            tracing::info!("stop requested by service control manager");
            break;
        }
        if done_rx.try_recv().is_ok() {
            tracing::info!("agent exited to pick up a new binary");
            replaced = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    agent.abort();
    runtime.shutdown_timeout(Duration::from_secs(5));

    // Exiting cleanly would tell the SCM we meant to stop, and the restart
    // actions configured at install time would never fire. Report a service
    // specific code so it restarts us onto the binary we just installed.
    let exit_code = if replaced {
        ServiceExitCode::ServiceSpecific(1)
    } else {
        ServiceExitCode::Win32(0)
    };
    status_handle.set_service_status(ServiceStatus {
        service_type: SERVICE_TYPE,
        current_state: ServiceState::Stopped,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code,
        checkpoint: 0,
        wait_hint: Duration::from_secs(0),
        process_id: None,
    })?;
    Ok(())
}

/// Register the service, pointing it at the currently running executable.
pub fn install() -> Result<()> {
    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )
    .context("opening the service control manager (run as Administrator)")?;

    let exe = std::env::current_exe()?;
    let info = ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from(DISPLAY_NAME),
        service_type: SERVICE_TYPE,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: exe,
        // The SCM must invoke the service entry point, not the CLI.
        launch_arguments: vec![OsString::from("run-service")],
        dependencies: vec![],
        // LocalSystem: patching and winget both need administrative rights.
        account_name: None,
        account_password: None,
    };

    let service = manager
        .create_service(&info, ServiceAccess::CHANGE_CONFIG | ServiceAccess::START)
        .context("creating the service")?;
    service.set_description(
        "Keeps this machine's packages, applications, and agent in sync with the PatchPanel portal, \
         and probes the IoT devices assigned to its site.",
    )?;

    // Self-update works by replacing the binary and exiting; without restart
    // actions the machine would simply drop off the fleet.
    configure_recovery()?;

    println!("installed service `{SERVICE_NAME}`");
    println!("start it with:  sc.exe start {SERVICE_NAME}");
    Ok(())
}

/// Make Windows restart the agent after a self-update.
///
/// Two settings are needed, and only having the first is a trap: `sc failure`
/// defines the actions, but the SCM applies them **only when a service
/// crashes**. An agent that exits deliberately to load a new binary counts as
/// an orderly stop, so the actions never fire and the machine silently drops
/// off the fleet. `sc failureflag 1` is what extends recovery to a service that
/// stops itself with an error code, which is exactly what we do.
///
/// Idempotent, so re-running the installer repairs an existing install.
pub fn configure_recovery() -> Result<()> {
    std::process::Command::new("sc.exe")
        .args([
            "failure",
            SERVICE_NAME,
            "reset=",
            "86400",
            "actions=",
            "restart/5000/restart/5000/restart/10000",
        ])
        .status()
        .context("configuring restart actions")?;

    std::process::Command::new("sc.exe")
        .args(["failureflag", SERVICE_NAME, "1"])
        .status()
        .context("enabling recovery for non-crash exits")?;

    Ok(())
}

pub fn uninstall() -> Result<()> {
    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT,
    )
    .context("opening the service control manager (run as Administrator)")?;

    let service = manager
        .open_service(SERVICE_NAME, ServiceAccess::STOP | ServiceAccess::DELETE)
        .context("opening the service")?;

    // Stopping is best-effort: deleting an already-stopped service is fine.
    let _ = service.stop();
    service.delete().context("deleting the service")?;
    println!("removed service `{SERVICE_NAME}`");
    Ok(())
}
