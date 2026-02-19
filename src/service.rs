//! Run services in environments
//!
//! This module managers systemd services within isolated epkg environments.
//! Supported commands:
//! - epkg -e ENV service start/stop/reload/restart SERVICE
//! - epkg -e ENV service status SERVICE
//! - epkg -e ENV service status # show all services in ENV
//! - epkg service status --all # show all services in all ENV
//!
//! ## Systemd Service Files
//!
//! Services are found in systemd service files located within the environment root:
//! - `etc/systemd/system` (highest priority)
//! - `usr/lib/systemd/system` (fallback)
//!
//! The module reads service configuration including
//! - ExecStart/ExecStop commands
//! - user/group settings
//! - environment variables
//! to properly execute services in isolated environments.
//!
//! ## PID File Location
//!
//! Service PID files are stored in the environment's run directory:
//! `{environment_base}/run/{service_name}.pid`
//!
//! This ensures proper process tracking and prevents conflicts between different environments.
//!
//! ## Running Services as Normal User
//!
//! When running `epkg service start nginx` as a normal (non-root) user, the HTTP service will
//! execute under its designated system user account (typically 'nginx') within the user namespace.
//! However, since the outer process still runs as the normal user, the service cannot bind to
//! privileged ports (like HTTP's default port 80). You may need to modify the service
//! configuration to use non-privileged ports (e.g., ports >= 1024) before starting the service.

use std::fs;
use std::path::{Path, PathBuf};
use std::io::{BufRead, BufReader};
use color_eyre::Result;
use color_eyre::eyre::{self, WrapErr};
use clap::ArgMatches;
use crate::environment::get_all_env_names;

/// Service status information
#[derive(Debug)]
pub struct ServiceStatus {
    pub name: String,
    pub pid: Option<i32>,
    pub running: bool,
    pub environment: String,
}

/// Parsed systemd service configuration
#[derive(Debug, Default)]
pub struct SystemdService {
    pub exec_start: Vec<String>,
    pub exec_stop: Vec<String>,
    pub service_type: Option<String>,
    pub user: Option<String>,
    pub group: Option<String>,
    pub environment: Vec<String>,
    pub environment_file: Vec<String>,
    pub unset_environment: Vec<String>,
}

/// Load environment variables from a file
fn load_environment_file(env_file: &str, env_vars: &mut std::collections::HashMap<String, String>) -> Result<()> {
    if !std::path::Path::new(env_file).exists() {
        return Err(eyre::eyre!("Environment file not found: {}", env_file));
    }

    let content = fs::read_to_string(env_file)
        .wrap_err(format!("Failed to read environment file: {}", env_file))?;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        parse_environment_variable(line, env_vars)?;
    }

    Ok(())
}

/// Parse a single environment variable (KEY=VALUE or KEY)
fn parse_environment_variable(env_var: &str, env_vars: &mut std::collections::HashMap<String, String>) -> Result<()> {
    if let Some((key, value)) = env_var.split_once('=') {
        env_vars.insert(key.trim().to_string(), value.trim().to_string());
    } else {
        // Environment variable without value (systemd style)
        env_vars.insert(env_var.trim().to_string(), String::new());
    }
    Ok(())
}

/// Macro to parse systemd service directives
macro_rules! parse_directive {
    ($service:expr, $line:expr, $prefix:expr, $field:ident, push) => {
        if let Some(value) = $line.strip_prefix($prefix) {
            let trimmed_value = value.trim().to_string();
            if !trimmed_value.is_empty() {
                $service.$field.push(trimmed_value);
            }
        }
    };
    ($service:expr, $line:expr, $prefix:expr, $field:ident, assign) => {
        if let Some(value) = $line.strip_prefix($prefix) {
            $service.$field = Some(value.trim().to_string());
        }
    };
}

/// Find a systemd service file by checking search paths within the environment root
fn find_systemd_service_file(service_name: &str, env_root: &Path) -> Result<PathBuf> {
    // systemd service file search paths in order of precedence within environment root
    let search_paths = [
        "etc/systemd/system",
        "usr/lib/systemd/system",
    ];

    // Find the first service file that exists (highest precedence first)
    for relative_path in &search_paths {
        let candidate = env_root.join(relative_path).join(format!("{}.service", service_name));
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    Err(eyre::eyre!("Service file not found for '{}' in environment root {:?}", service_name, env_root))
}

/// Parse a systemd service file and extract service configuration
fn parse_systemd_service_file(service_file: &Path) -> Result<SystemdService> {
    let file = fs::File::open(service_file)
        .wrap_err(format!("Failed to open service file: {:?}", service_file))?;
    let reader = BufReader::new(file);

    let mut service = SystemdService::default();
    let mut in_service_section = false;

    for line in reader.lines() {
        let line = line?;
        let line = line.trim();

        // Skip empty lines and comments
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if line.starts_with('[') && line.ends_with(']') {
            in_service_section = line == "[Service]";
            continue;
        }

        if !in_service_section {
            continue;
        }

        // Parse systemd service directives
        parse_directive!(service, &line, "ExecStart=", exec_start, push);
        parse_directive!(service, &line, "ExecStop=", exec_stop, push);
        parse_directive!(service, &line, "Type=", service_type, assign);
        parse_directive!(service, &line, "User=", user, assign);
        parse_directive!(service, &line, "Group=", group, assign);
        parse_directive!(service, &line, "Environment=", environment, push);
        parse_directive!(service, &line, "EnvironmentFile=", environment_file, push);
        parse_directive!(service, &line, "UnsetEnvironment=", unset_environment, push);
    }

    if service.exec_start.is_empty() {
        return Err(eyre::eyre!("No ExecStart commands found in service file: {:?}", service_file));
    }

    Ok(service)
}

/// Parse a systemd service file and extract service configuration
fn parse_systemd_service(service_name: &str, env_name: &str) -> Result<SystemdService> {
    let env_root = crate::dirs::find_env_base(env_name)
        .ok_or_else(|| eyre::eyre!("Environment '{}' not found", env_name))?;
    let service_file = find_systemd_service_file(service_name, &env_root)?;
    parse_systemd_service_file(&service_file)
}


/// Fork and execute a command, returning the PID
fn start_daemon(
    command: &str,
    args: &[&str],
    env_name: &str,
    user: Option<&str>,
    group: Option<&str>,
    env_vars: std::collections::HashMap<String, String>,
) -> Result<i32> {

    // Get environment base path
    let env_base = crate::dirs::find_env_base(env_name)
        .ok_or_else(|| eyre::eyre!("Environment '{}' not found", env_name))?;

    // Create RunOptions for background service execution
    let run_options = crate::run::RunOptions {
        command: command.to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
        user: user.map(|s| s.to_string()),
        group: group.map(|s| s.to_string()),
        env_vars,
        background: true,
        redirect_stdio: true,
        skip_namespace_isolation: false, // Services should run in the environment namespace
        ..Default::default()
    };

    match crate::run::fork_and_execute(&env_base, &run_options)? {
        Some(pid) => Ok(pid),
        None => Err(eyre::eyre!("Expected PID from background process execution")),
    }
}

/// Get the PID file path for a service
fn get_pid_file_path(service_name: &str, env_name: &str) -> Result<PathBuf> {
    let env_root = crate::dirs::find_env_base(env_name)
        .ok_or_else(|| eyre::eyre!("Environment '{}' not found", env_name))?;

    Ok(env_root
        .join("run")
        .join(format!("{}.pid", service_name)))
}

/// Read PID from file
fn read_pid_from_file(pid_file: &Path) -> Result<Option<i32>> {
    if !pid_file.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(pid_file)
        .wrap_err(format!("Failed to read PID file: {:?}", pid_file))?;

    let pid = content.trim().parse::<i32>()
        .wrap_err(format!("Invalid PID in file: {:?}", pid_file))?;

    Ok(Some(pid))
}

/// Write PID to file
fn write_pid_to_file(pid_file: &Path, pid: i32) -> Result<()> {
    // Ensure parent directory exists
    if let Some(parent) = pid_file.parent() {
        fs::create_dir_all(parent)
            .wrap_err(format!("Failed to create directory: {:?}", parent))?;
    }

    fs::write(pid_file, pid.to_string())
        .wrap_err(format!("Failed to write PID file: {:?}", pid_file))?;

    Ok(())
}

/// Check if a process is running
fn is_process_running(pid: i32) -> bool {
    // Use /proc filesystem to check if process exists
    Path::new(&format!("/proc/{}", pid)).exists()
}

/// Parse an ExecStart command into command and arguments
fn parse_exec_command(exec_command: &str) -> Result<(&str, Vec<&str>)> {
    // Parse command and arguments (simple parsing, doesn't handle complex systemd syntax)
    let parts: Vec<&str> = exec_command.split_whitespace().collect();
    if parts.is_empty() {
        return Err(eyre::eyre!("Invalid ExecStart command: {}", exec_command));
    }

    let command = parts[0];
    let args = parts[1..].to_vec();

    Ok((command, args))
}

/// Prepare environment variables for service execution
fn prepare_service_environment(service_config: &SystemdService) -> Result<std::collections::HashMap<String, String>> {
    let mut env_vars = std::collections::HashMap::new();

    // Load environment from EnvironmentFile directives
    for env_file in &service_config.environment_file {
        load_environment_file(env_file, &mut env_vars)?;
    }

    // Add Environment directives (these override files)
    for env_var in &service_config.environment {
        parse_environment_variable(env_var, &mut env_vars)?;
    }

    Ok(env_vars)
}

/// Start a service daemon and write PID file
fn start_service_daemon(
    service_name: &str,
    env_name: &str,
    service_config: &SystemdService,
) -> Result<()> {
    if service_config.exec_start.is_empty() {
        return Err(eyre::eyre!("No executable commands found for service: {}", service_name));
    }

    // For simplicity, use the first ExecStart command
    let exec_command = &service_config.exec_start[0];

    let (command, args) = parse_exec_command(exec_command)?;
    let env_vars = prepare_service_environment(service_config)?;

    // Fork and execute with user/group and environment settings
    let pid = start_daemon(
        command,
        &args,
        env_name,
        service_config.user.as_deref(),
        service_config.group.as_deref(),
        env_vars,
    )?;

    // Store PID
    let pid_file = get_pid_file_path(service_name, env_name)?;
    write_pid_to_file(&pid_file, pid)?;

    println!("Service {} started with PID: {}", service_name, pid);
    Ok(())
}

/// Execute ExecStop commands from service configuration
fn execute_exec_stop_commands(service_config: &SystemdService) -> Result<bool> {
    if service_config.exec_stop.is_empty() {
        return Ok(false); // No ExecStop commands executed
    }

    for exec_stop in &service_config.exec_stop {
        println!("Executing ExecStop: {}", exec_stop);

        // Parse command and arguments
        let parts: Vec<&str> = exec_stop.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }

        let command = parts[0];
        let args = &parts[1..];

        // Execute the stop command
        match std::process::Command::new(command)
            .args(args)
            .status() {
            Ok(status) => {
                if status.success() {
                    println!("ExecStop command succeeded");
                } else {
                    println!("ExecStop command failed with exit code: {}", status.code().unwrap_or(-1));
                }
            }
            Err(e) => {
                println!("Failed to execute ExecStop command: {}", e);
            }
        }

        // Wait a bit after each ExecStop command
        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    Ok(true) // ExecStop commands were executed
}

/// Stop a process by sending signals (SIGTERM, then SIGKILL)
fn stop_process_by_signal(pid: i32, service_name: &str) -> Result<()> {
    // Send SIGTERM to the process
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;

    match kill(Pid::from_raw(pid), Signal::SIGTERM) {
        Ok(_) => {
            println!("Sent SIGTERM to process {}", pid);

            // Wait a bit for graceful shutdown
            std::thread::sleep(std::time::Duration::from_secs(1));

            // Check if process is still running
            if is_process_running(pid) {
                println!("Process {} still running, sending SIGKILL", pid);
                let _ = kill(Pid::from_raw(pid), Signal::SIGKILL);
            }

            println!("Service {} stopped", service_name);
        }
        Err(e) => {
            eprintln!("Failed to stop service {}: {}", service_name, e);
            return Err(e.into());
        }
    }

    Ok(())
}

/// Clean up PID file after stopping service
fn cleanup_pid_file(pid_file: &Path) {
    let _ = fs::remove_file(pid_file);
}

/// Stop a service daemon and clean up PID file
fn stop_service_daemon(service_name: &str, env_name: &str, service_config: &SystemdService) -> Result<()> {
    let pid_file = get_pid_file_path(service_name, env_name)?;

    let pid = match read_pid_from_file(&pid_file)? {
        Some(pid) => pid,
        None => {
            println!("Service {} is not running (no PID file found)", service_name);
            return Ok(());
        }
    };

    if !is_process_running(pid) {
        println!("Service {} is not running (process {} not found)", service_name, pid);
        // Remove stale PID file
        cleanup_pid_file(&pid_file);
        return Ok(());
    }

    // Try ExecStop commands first, if available
    execute_exec_stop_commands(service_config)?;

    // Check if the process is still running after ExecStop commands
    if !is_process_running(pid) {
        // Remove PID file
        cleanup_pid_file(&pid_file);
        println!("Service {} stopped via ExecStop", service_name);
        return Ok(());
    }

    // Fallback to signal-based stopping
    stop_process_by_signal(pid, service_name)?;

    // Remove PID file
    cleanup_pid_file(&pid_file);

    Ok(())
}

/// Start a service
fn start_service(service_name: &str, env_name: &str) -> Result<()> {
    println!("Starting service: {} in environment: {}", service_name, env_name);

    // Parse service file
    let service_config = parse_systemd_service(service_name, env_name)?;

    // Start the service daemon
    start_service_daemon(service_name, env_name, &service_config)
}

/// Stop a service
fn stop_service(service_name: &str, env_name: &str) -> Result<()> {
    println!("Stopping service: {} in environment: {}", service_name, env_name);

    // Parse service file to get ExecStop commands
    let service_config = parse_systemd_service(service_name, env_name)?;

    // Stop the service daemon
    stop_service_daemon(service_name, env_name, &service_config)
}

/// Get service status
fn get_service_status(service_name: &str, env_name: &str) -> Result<ServiceStatus> {
    let pid_file = get_pid_file_path(service_name, env_name)?;

    let (pid, running) = match read_pid_from_file(&pid_file)? {
        Some(pid) => {
            let running = is_process_running(pid);
            (Some(pid), running)
        }
        None => (None, false),
    };

    Ok(ServiceStatus {
        name: service_name.to_string(),
        pid,
        running,
        environment: env_name.to_string(),
    })
}

/// Restart a service (stop and then start)
fn restart_service(service_name: &str, env_name: &str) -> Result<()> {
    println!("Restarting service: {} in environment: {}", service_name, env_name);

    // Try to stop the service first (ignore errors if it's not running)
    let stop_result = stop_service(service_name, env_name);
    if stop_result.is_ok() {
        println!("Service {} stopped successfully", service_name);
    } else {
        println!("Service {} was not running or failed to stop, proceeding with start", service_name);
    }

    // Always try to start the service
    start_service(service_name, env_name)?;

    println!("Service {} restarted successfully", service_name);
    Ok(())
}

/// Reload a service (send SIGHUP)
fn reload_service(service_name: &str, env_name: &str) -> Result<()> {
    println!("Reloading service: {} in environment: {}", service_name, env_name);

    let pid_file = get_pid_file_path(service_name, env_name)?;

    let pid = match read_pid_from_file(&pid_file)? {
        Some(pid) => pid,
        None => {
            return Err(eyre::eyre!("Service {} is not running", service_name));
        }
    };

    if !is_process_running(pid) {
        return Err(eyre::eyre!("Service {} process {} is not running", service_name, pid));
    }

    // Send SIGHUP to reload
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;

    kill(Pid::from_raw(pid), Signal::SIGHUP)
        .wrap_err(format!("Failed to send SIGHUP to service {}", service_name))?;

    println!("Service {} reloaded", service_name);
    Ok(())
}

/// Get all services status for an environment
fn get_services_status_for_env(env_name: &str) -> Result<Vec<ServiceStatus>> {
    // Use get_pid_file_path to get the run directory (by getting a dummy pid file path and taking its parent)
    let dummy_pid_path = get_pid_file_path("dummy", env_name)?;
    let run_dir = dummy_pid_path.parent()
        .ok_or_else(|| eyre::eyre!("Invalid PID file path structure"))?;

    if !run_dir.exists() {
        return Ok(Vec::new());
    }

    let mut statuses = Vec::new();

    // List all .pid files in the environment's run directory
    for entry in fs::read_dir(run_dir)
        .wrap_err(format!("Failed to read run directory: {:?}", run_dir))? {

        let entry = entry?;
        let path = entry.path();

        if let Some(extension) = path.extension() {
            if extension == "pid" {
                if let Some(file_stem) = path.file_stem() {
                    if let Some(service_name) = file_stem.to_str() {
                        let status = get_service_status(service_name, env_name)?;
                        statuses.push(status);
                    }
                }
            }
        }
    }

    Ok(statuses)
}

/// Get all services status for all environments
fn get_all_services_status() -> Result<Vec<ServiceStatus>> {
    let mut all_statuses = Vec::new();

    // Get list of environments
    let environments = get_all_env_names()?;

    for (env_name, _) in environments {
        let statuses = get_services_status_for_env(&env_name)?;
        all_statuses.extend(statuses);
    }

    Ok(all_statuses)
}

/// Print status of a single service
fn print_service_status(status: &ServiceStatus) {
    let status_str = if status.running { "running" } else { "stopped" };
    let pid_str = status.pid.map_or("N/A".to_string(), |p| p.to_string());

    println!("{:<20} {:<8} {:<10} {}",
             status.name,
             status_str,
             pid_str,
             status.environment);
}

/// Print status of multiple services
fn print_services_status(statuses: &[ServiceStatus]) {
    if statuses.is_empty() {
        println!("No services found");
        return;
    }

    println!("{:<20} {:<8} {:<10} {}",
             "SERVICE", "STATUS", "PID", "ENV");
    println!("{}", "-".repeat(60));

    for status in statuses {
        print_service_status(status);
    }
}

/// Handle service command
pub fn command_service(sub_matches: &ArgMatches) -> Result<()> {
    match sub_matches.subcommand() {
        Some(("start", sub_matches)) => {
            if let Some(service_name) = sub_matches.get_one::<String>("SERVICE_NAME") {
                let config = crate::config();
                start_service(service_name, &config.common.env_name)?;
            }
        }
        Some(("stop", sub_matches)) => {
            if let Some(service_name) = sub_matches.get_one::<String>("SERVICE_NAME") {
                let config = crate::config();
                stop_service(service_name, &config.common.env_name)?;
            }
        }
        Some(("status", sub_matches)) => {
            let config = crate::config();

            if sub_matches.get_flag("all") {
                // Show all services across all environments
                let all_statuses = get_all_services_status()?;
                print_services_status(&all_statuses);
            } else if let Some(service_name) = sub_matches.get_one::<String>("SERVICE_NAME") {
                // Show specific service status
                let status = get_service_status(service_name, &config.common.env_name)?;
                print_service_status(&status);
            } else {
                // Show all services for current environment
                let statuses = get_services_status_for_env(&config.common.env_name)?;
                print_services_status(&statuses);
            }
        }
        Some(("reload", sub_matches)) => {
            if let Some(service_name) = sub_matches.get_one::<String>("SERVICE_NAME") {
                let config = crate::config();
                reload_service(service_name, &config.common.env_name)?;
            }
        }
        Some(("restart", sub_matches)) => {
            if let Some(service_name) = sub_matches.get_one::<String>("SERVICE_NAME") {
                let config = crate::config();
                restart_service(service_name, &config.common.env_name)?;
            }
        }
        _ => {
            return Err(eyre::eyre!("Invalid service subcommand"));
        }
    }

    Ok(())
}
