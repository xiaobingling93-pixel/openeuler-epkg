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
        log::warn!("VM daemon process {} did not exit gracefully, forcing termination", session.daemon_pid);

        // Send SIGKILL to force termination of the keeper process
        #[cfg(unix)]
        {
            use nix::sys::signal::{kill, Signal};
            let pid = nix::unistd::Pid::from_raw(session.daemon_pid as i32);
            if let Err(e) = kill(pid, Signal::SIGKILL) {
                log::warn!("Failed to kill daemon process {}: {}", session.daemon_pid, e);
            } else {
                // Wait a moment for process to actually die
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        }
        #[cfg(windows)]
        {
            // On Windows, we'd need to use TerminateProcess, but for now just log
            log::warn!("Windows: cannot force kill daemon process {}", session.daemon_pid);
        }

        // Clean up session and socket files
        let session_file = vm_session_file_path(&env_name);
        cleanup_vm_session_files(&session_file, &session.socket_path);
    }

    println!("VM stopped for {}", env_name);
    Ok(())
}

/// Send shutdown signal to guest vm_daemon.
/// For QEMU: uses vsock (socket_path format: "vsock:3") - Linux only
/// For libkrun: uses Unix socket
fn send_shutdown_to_guest(socket_path: &Path, _backend: &str) -> Result<()> {
    // vsock path (Linux only)
    #[cfg(target_os = "linux")]
    {
        let socket_str = socket_path.to_string_lossy();
        if socket_str.starts_with("vsock:") {
            send_shutdown_via_vsock(&socket_str)?;
            return Ok(());
        }
    }

    // libkrun uses Unix socket (Linux only - client module is Linux-specific)
    #[cfg(all(target_os = "linux", feature = "libkrun"))]
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

        // Build simple JSON request inline (client module is Linux-only)
        let request = serde_json::json!({
            "command": [crate::run::VM_SESSION_DONE_CMD],
            "cwd": null,
            "env": {},
            "stdin": "",
            "pty": false,
            "reuse_vm": false,
        });
        writeln!(stream, "{}", request)?;

        log::debug!("Sent {} to guest vm_daemon via named pipe", crate::run::VM_SESSION_DONE_CMD);
        Ok(())
    }

    // macOS libkrun: use Unix socket with inline JSON request
    #[cfg(all(target_os = "macos", feature = "libkrun"))]
    {
        use std::io::Write;

        let mut stream = std::os::unix::net::UnixStream::connect(socket_path)?;

        // Build simple JSON request inline (client module is Linux-only)
        let request = serde_json::json!({
            "command": [crate::run::VM_SESSION_DONE_CMD],
            "cwd": null,
            "env": {},
            "stdin": "",
            "pty": false,
            "reuse_vm": false,
        });
        writeln!(stream, "{}", request)?;

        log::debug!("Sent {} to guest vm_daemon via Unix socket", crate::run::VM_SESSION_DONE_CMD);
        Ok(())
    }

    #[cfg(not(feature = "libkrun"))]
    {
        let socket_str = socket_path.to_string_lossy();
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