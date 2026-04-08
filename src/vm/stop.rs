//! VM stop command implementation.

use std::path::Path;
use color_eyre::{Result, eyre};
use clap::ArgMatches;

use super::session::{discover_vm_session, is_process_alive, cleanup_vm_session_files, vm_session_file_path};

/// Entry point for `epkg vm stop` command.
pub fn cmd_vm_stop(_args: &ArgMatches) -> Result<()> {
    let cfg = crate::models::config();
    let env_name = cfg.common.env_name.clone();
    let _env_root = if cfg.common.env_root.is_empty() {
        crate::dirs::get_env_root(env_name.clone())?
    } else {
        std::path::PathBuf::from(&cfg.common.env_root)
    };

    // Check if VM is running
    let session = discover_vm_session(&env_name)?
        .ok_or_else(|| eyre::eyre!("No VM running for {}", env_name))?;

    log::info!("Stopping VM for {} (PID {}, backend={})", env_name, session.daemon_pid, session.backend);

    // Send shutdown signal to guest vm_daemon via vsock or Unix socket
    // The guest will close connections and VM will shut down
    if let Err(e) = send_shutdown_to_guest(&session.socket_path, &session.backend) {
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
        let session_file = vm_session_file_path(&env_name);
        cleanup_vm_session_files(&session_file, &session.socket_path);
    }

    println!("VM stopped for {}", env_name);
    Ok(())
}

/// Send shutdown signal to guest vm_daemon.
/// For QEMU: uses vsock (socket_path format: "vsock:3")
/// For libkrun: uses Unix socket
fn send_shutdown_to_guest(socket_path: &Path, _backend: &str) -> Result<()> {
    let socket_str = socket_path.to_string_lossy();

    // Check if this is a vsock address (QEMU backend)
    if socket_str.starts_with("vsock:") {
        send_shutdown_via_vsock(&socket_str)?;
        return Ok(());
    }

    // libkrun uses Unix socket
    #[cfg(all(unix, feature = "libkrun"))]
    {
        use std::io::Write;

        let mut stream = std::os::unix::net::UnixStream::connect(socket_path)?;

        // Use proper command request format that guest daemon expects
        let request = super::client::build_command_request(
            &[crate::run::VM_SESSION_DONE_CMD.to_string()],
            crate::models::IoMode::Stream,
            false,
            None,
            None,
        );
        let request_json = serde_json::Value::Object(request);
        writeln!(stream, "{}", request_json)?;

        log::debug!("Sent {} to guest vm_daemon via Unix socket", crate::run::VM_SESSION_DONE_CMD);
        Ok(())
    }

    #[cfg(all(windows, feature = "libkrun"))]
    {
        use std::io::Write;

        let mut stream = crate::libkrun::bridge::connect_vsock_bridge(socket_path, 5)?;

        // Use proper command request format that guest daemon expects
        let request = super::client::build_command_request(
            &[crate::run::VM_SESSION_DONE_CMD.to_string()],
            crate::models::IoMode::Stream,
            false,
            None,
            None,
        );
        let request_json = serde_json::Value::Object(request);
        writeln!(stream, "{}", request_json)?;

        log::debug!("Sent {} to guest vm_daemon via named pipe", crate::run::VM_SESSION_DONE_CMD);
        Ok(())
    }

    #[cfg(not(feature = "libkrun"))]
    {
        // Non-libkrun backend but not vsock - should not happen
        log::warn!("Unknown socket type for shutdown: {}", socket_str);
        Ok(())
    }
}

/// Send shutdown signal to guest via vsock.
/// Format: "vsock:3" -> connects to CID 3, port 10000 (vm_daemon control port)
#[cfg(target_os = "linux")]
fn send_shutdown_via_vsock(socket_str: &str) -> Result<()> {
    use nix::sys::socket::{socket, connect, AddressFamily, SockType, SockFlag, VsockAddr};
    use std::os::fd::{IntoRawFd, FromRawFd};

    // Parse "vsock:3" -> CID 3
    let cid: u32 = socket_str
        .strip_prefix("vsock:")
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| eyre::eyre!("Invalid vsock address: {}", socket_str))?;

    // vm_daemon control port is 10000
    const VM_DAEMON_PORT: u32 = 10000;

    log::debug!("Sending shutdown via vsock to CID {} port {}", cid, VM_DAEMON_PORT);

    let fd = socket(
        AddressFamily::Vsock,
        SockType::Stream,
        SockFlag::SOCK_CLOEXEC,
        None,
    ).map_err(|e| eyre::eyre!("Failed to create vsock socket: {}", e))?;

    let raw_fd = fd.into_raw_fd();
    // Connect from host to guest CID (guest-cid=3 in QEMU config)
    let addr = VsockAddr::new(cid, VM_DAEMON_PORT);
    connect(raw_fd, &addr)
        .map_err(|e| eyre::eyre!("Failed to connect to vsock CID {} port {}: {}", cid, VM_DAEMON_PORT, e))?;

    // Send session_done command using proper JSON request format
    // The guest daemon expects: {"command": ["__epkg_vm_session_done__"], ...}
    use std::io::Write;
    let mut stream = unsafe { std::os::unix::net::UnixStream::from_raw_fd(raw_fd) };

    let request = super::client::build_command_request(
        &[crate::run::VM_SESSION_DONE_CMD.to_string()],
        crate::models::IoMode::Stream,
        false,
        None,
        None,
    );
    let request_json = serde_json::Value::Object(request);
    writeln!(stream, "{}", request_json)?;

    log::debug!("Sent {} to guest vm_daemon via vsock", crate::run::VM_SESSION_DONE_CMD);

    // Close the stream (this will also close the fd)
    let _ = stream.shutdown(std::net::Shutdown::Both);

    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn send_shutdown_via_vsock(_socket_str: &str) -> Result<()> {
    Err(eyre::eyre!("vsock not supported on this platform"))
}