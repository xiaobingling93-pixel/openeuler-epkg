//! VM stop command implementation.

use std::path::Path;
use color_eyre::{Result, eyre};
use clap::ArgMatches;

use super::session::{discover_vm_session, is_process_alive, cleanup_vm_session_files, vm_session_file_path};

/// Entry point for `epkg vm stop` command.
pub fn cmd_vm_stop(args: &ArgMatches) -> Result<()> {
    let env_root: std::path::PathBuf = args.get_one::<String>("env")
        .expect("env is required")
        .into();

    let env_name = env_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    // Check if VM is running
    let session = discover_vm_session(&env_root)?
        .ok_or_else(|| eyre::eyre!("No VM running for {}", env_name))?;

    log::info!("Stopping VM for {} (PID {})", env_name, session.daemon_pid);

    // Send shutdown signal to guest vm_daemon via vsock
    // The guest will close connections and VM will shut down
    if let Err(e) = send_shutdown_to_guest(&session.socket_path) {
        log::warn!("Failed to send shutdown to guest: {}", e);
    }

    // Wait for daemon process to exit (up to 10 seconds)
    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(10);

    while start.elapsed() < timeout {
        if !is_process_alive(session.daemon_pid) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    // Force cleanup if process is still alive
    if is_process_alive(session.daemon_pid) {
        log::warn!("VM daemon process {} did not exit gracefully, cleaning up", session.daemon_pid);
        let session_file = vm_session_file_path(&env_root);
        cleanup_vm_session_files(&session_file, &session.socket_path);
    }

    println!("VM stopped for {}", env_name);
    Ok(())
}

/// Send shutdown signal to guest vm_daemon via vsock.
#[cfg(all(unix, feature = "libkrun"))]
fn send_shutdown_to_guest(socket_path: &Path) -> Result<()> {
    use std::io::Write;

    // Connect to vm_daemon
    let mut stream = std::os::unix::net::UnixStream::connect(socket_path)?;

    // Send session_done command
    // The vm_daemon will recognize this and initiate shutdown
    let request = serde_json::json!({
        "type": "session_done"
    });
    writeln!(stream, "{}", request)?;

    log::debug!("Sent session_done to guest vm_daemon");
    Ok(())
}

#[cfg(all(windows, feature = "libkrun"))]
fn send_shutdown_to_guest(socket_path: &Path) -> Result<()> {
    // Windows: use named pipe
    use std::io::Write;

    let stream = crate::libkrun::bridge::connect_vsock_bridge(socket_path, 5)?;

    // Send session_done command
    let request = serde_json::json!({
        "type": "session_done"
    });

    // Windows pipe handling would go here
    // For now, just log the intent
    log::debug!("Would send session_done to guest vm_daemon via named pipe");
    Ok(())
}

#[cfg(not(feature = "libkrun"))]
fn send_shutdown_to_guest(_socket_path: &Path) -> Result<()> {
    Ok(())
}