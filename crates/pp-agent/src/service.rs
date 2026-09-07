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
    let agent = runtime.spawn(async {
        let path = crate::config::default_config_path();
        match crate::config::Config::load(&path) {
            Ok(cfg) => {
                if let Err(e) = crate::session::run(cfg).await {
                    tracing::error!(error = %format!("{e:#}"), "agent stopped");
                }
            }
            Err(e) => tracing::error!(error = %format!("{e:#}"), "failed to load config"),
        }
    });

    // Block this thread until the SCM asks us to stop.
    let _ = shutdown_rx.recv();
    tracing::info!("stop requested by service control manager");
    agent.abort();
    runtime.shutdown_timeout(Duration::from_secs(5));

    status_handle.set_service_status(report(ServiceState::Stopped, ServiceControlAccept::empty()))?;
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
    std::process::Command::new("sc.exe")
        .args([
            "failure",
            SERVICE_NAME,
            "reset=",
            "60",
            "actions=",
            "restart/5000/restart/5000/restart/10000",
        ])
        .status()
        .context("configuring restart actions")?;

    println!("installed service `{SERVICE_NAME}`");
    println!("start it with:  sc.exe start {SERVICE_NAME}");
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
