//! TCP client for VM (vm-daemon) command execution.
//!
//! This module provides functionality to send commands to a guest VM over TCP
//! (typically forwarded via QEMU's user networking). It supports both simple
//! command execution and interactive PTY streaming for terminal applications.
//!
//! # Protocol
//! - Commands are sent as JSON requests to localhost:10000
//! - Responses can be JSON or plain text (fallback)
//! - PTY mode uses a streaming protocol with base64-encoded data
//! - Supports terminal resizing, signal forwarding, and raw terminal mode
#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader, Read, Write, IsTerminal};
use std::net::TcpStream;
use std::time::Duration;
use std::sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex};
use std::os::fd::{AsFd, FromRawFd, IntoRawFd};
use color_eyre::eyre;
use color_eyre::Result;
use serde::{Deserialize, Serialize};
use console::Term;
use ctrlc;
use lazy_static::lazy_static;
use nix::sys::signal::{signal, Signal, SigHandler};
use nix::sys::termios;
use nix::sys::socket::{self, AddressFamily, SockType, SockFlag, VsockAddr};
use nix::poll::{poll, PollFd, PollFlags};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use crate::models::IoMode;

lazy_static! {
    static ref RESIZE_PENDING: AtomicBool = AtomicBool::new(false);
}

extern "C" fn handle_sigwinch(_: i32) {
    RESIZE_PENDING.store(true, Ordering::SeqCst);
}


/// Streaming message types for interactive/TUI modes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum StreamMessage {
    #[serde(rename = "stdin")]
    Stdin { data: String, seq: u64 },
    #[serde(rename = "stdout")]
    Stdout { data: String, seq: u64 },
    #[serde(rename = "stderr")]
    Stderr { data: String, seq: u64 },
    #[serde(rename = "signal")]
    Signal { signal: String },
    #[serde(rename = "resize")]
    Resize { rows: u16, cols: u16 },
    #[serde(rename = "heartbeat")]
    Heartbeat,
    #[serde(rename = "exit")]
    Exit { code: i32 },
}

/// Connect to guest TCP server with retry logic.
fn connect_with_retry(max_retries: u32) -> Result<TcpStream> {
    const GUEST_PORT: u16 = 10000;
    let mut retry_count = 0;
    let mut last_error = None;
    while retry_count < max_retries {
        match TcpStream::connect(("127.0.0.1", GUEST_PORT)) {
            Ok(stream) => return Ok(stream),
            Err(e) => {
                last_error = Some(e);
                retry_count += 1;
                if retry_count >= max_retries {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
    Err(eyre::eyre!(
        "Failed to connect to guest TCP server after {} retries: {}. \
         If the guest is slow to boot, check ~/.cache/epkg/vmm-logs/latest-qemu.log",
        max_retries,
        last_error.unwrap_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "connection failed"))
    ))
}

/// Connect to guest vsock server with retry logic.
fn connect_vsock_with_retry(port: u32, max_retries: u32) -> Result<TcpStream> {
    let mut retry_count = 0;
    let mut last_error = None;
    while retry_count < max_retries {
        match connect_vsock_once(port) {
            Ok(stream) => return Ok(stream),
            Err(e) => {
                last_error = Some(e);
                retry_count += 1;
                if retry_count >= max_retries {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
    Err(eyre::eyre!(
        "Failed to connect to guest vsock server on port {} after {} retries: {}",
        port,
        max_retries,
        last_error.unwrap_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "connection failed"))
    ))
}

/// Connect to Unix socket with retry logic (for libkrun vsock).
fn connect_unix_socket_with_retry(sock_path: &std::path::Path, max_retries: u32) -> Result<TcpStream> {
    let mut retry_count = 0;
    let mut last_error = None;
    while retry_count < max_retries {
        match std::os::unix::net::UnixStream::connect(sock_path) {
            Ok(unix_stream) => {
                // Convert UnixStream to TcpStream by using the raw fd
                // This works because both are stream sockets and TcpStream's read/write
                // operations only depend on the underlying fd
                let raw_fd = unix_stream.into_raw_fd();
                // SAFETY: raw_fd is a valid, connected Unix stream socket
                let stream = unsafe { TcpStream::from_raw_fd(raw_fd) };
                return Ok(stream);
            }
            Err(e) => {
                last_error = Some(e);
                retry_count += 1;
                if retry_count >= max_retries {
                    break;
                }
                // Use 5ms retry interval for faster connection establishment.
                // The vsock Unix socket typically becomes ready within ~100-200ms after VM start.
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
    Err(eyre::eyre!(
        "Failed to connect to Unix socket {} after {} retries: {}",
        sock_path.display(),
        max_retries,
        last_error.unwrap_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "connection failed"))
    ))
}

/// Single vsock connect attempt, returns a TcpStream wrapper over AF_VSOCK fd.
fn connect_vsock_once(port: u32) -> std::io::Result<TcpStream> {
    // Use a fixed guest CID (3) matching the QEMU vsock configuration.
    const GUEST_CID: u32 = 3;
    let fd = socket::socket(
        AddressFamily::Vsock,
        SockType::Stream,
        SockFlag::SOCK_CLOEXEC,
        None,
    )?;
    let addr = VsockAddr::new(GUEST_CID, port);
    // Transfer ownership of fd to raw_fd (consumes fd, no double-close)
    let raw_fd = fd.into_raw_fd();
    match socket::connect(raw_fd, &addr) {
        Ok(()) => {
            // SAFETY: raw_fd is a valid, connected AF_VSOCK stream socket; TcpStream only
            // relies on the underlying fd for read/write/dup, which works for vsock too.
            // We own raw_fd (transferred from OwnedFd), so TcpStream takes ownership.
            Ok(unsafe { TcpStream::from_raw_fd(raw_fd) })
        }
        Err(e) => {
            // On error, close fd ourselves since we own it (into_raw_fd consumed OwnedFd)
            let _ = socket::shutdown(raw_fd, socket::Shutdown::Both);
            Err(std::io::Error::from(e))
        }
    }
}

/// Build JSON command request for guest execution.
fn build_command_request(
    cmd_parts: &[String],
    io_mode: IoMode,
    reuse_session: bool,
    vm_keep_timeout_secs: Option<u32>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut request = serde_json::Map::new();
    request.insert("command".to_string(), serde_json::Value::Array(
        cmd_parts.iter().map(|s| serde_json::Value::String(s.clone())).collect()
    ));
    request.insert("cwd".to_string(), serde_json::Value::Null);
    request.insert("env".to_string(), serde_json::Value::Object(serde_json::Map::new()));
    request.insert("stdin".to_string(), serde_json::Value::String("".to_string()));

    // Determine PTY and batch mode
    let use_pty = matches!(io_mode, IoMode::Tty) ||
        (matches!(io_mode, IoMode::Auto) && std::io::stdin().is_terminal());

    if use_pty {
        request.insert("pty".to_string(), serde_json::Value::Bool(true));
        // Try to get terminal size
        let (rows, cols) = Term::stdout().size();
        if rows > 0 && cols > 0 {
            let mut terminal = serde_json::Map::new();
            terminal.insert("rows".to_string(), serde_json::Value::Number(rows.into()));
            terminal.insert("cols".to_string(), serde_json::Value::Number(cols.into()));
            request.insert("terminal".to_string(), serde_json::Value::Object(terminal));
        }
    } else {
        request.insert("pty".to_string(), serde_json::Value::Bool(false));
    }

    // Batch mode: collect output and return in single response
    if matches!(io_mode, IoMode::Batch) {
        request.insert("batch".to_string(), serde_json::Value::Bool(true));
    }

    if reuse_session {
        request.insert("reuse_vm".to_string(), serde_json::Value::Bool(true));
        if let Some(secs) = vm_keep_timeout_secs {
            request.insert("vm_keep_timeout_secs".to_string(), serde_json::Value::Number(secs.into()));
        }
    }
    request
}

/// Resolve IoMode to actual PTY usage.
/// Returns (use_pty, is_batch) tuple.
fn resolve_io_mode(io_mode: IoMode) -> (bool, bool) {
    match io_mode {
        IoMode::Auto => {
            let is_tty = std::io::stdin().is_terminal();
            (is_tty, false)
        }
        IoMode::Tty => (true, false),
        IoMode::Stream => (false, false),
        IoMode::Batch => (false, true),
    }
}

/// Helper to send a command via TCP to the guest VM.
pub fn send_command_via_tcp(cmd_parts: &[String], io_mode: IoMode) -> Result<i32> {
    send_command_via_tcp_impl(cmd_parts, io_mode, false, None)
}

fn send_command_via_tcp_impl(
    cmd_parts: &[String],
    io_mode: IoMode,
    reuse_session: bool,
    vm_keep_timeout_secs: Option<u32>,
) -> Result<i32> {
    let (use_pty, is_batch) = resolve_io_mode(io_mode);
    log::debug!(
        "vm_client: io_mode={:?}, use_pty={}, is_batch={}, reuse_session={}",
        io_mode,
        use_pty,
        is_batch,
        reuse_session
    );
    // Connect to guest TCP server with retry
    let mut stream = connect_with_retry(60)?;
    log::debug!("vm_client: TCP connected, sending command {:?}", cmd_parts);

    // Build and send JSON request
    let request = build_command_request(cmd_parts, io_mode, reuse_session, vm_keep_timeout_secs);
    let request_json = serde_json::to_vec(&request)?;
    stream.write_all(&request_json)?;
    stream.write_all(b"\n")?;
    log::debug!("vm_client: request sent ({} bytes), pty={}, batch={}", request_json.len(), use_pty, is_batch);

    if is_batch {
        handle_batch(&mut stream)
    } else {
        handle_streaming(&mut stream, use_pty)
    }
}

/// Helper to send a command via vsock to the guest VM.
///
/// For libkrun, pass `unix_socket_path` to connect via Unix socket instead of AF_VSOCK.
/// For QEMU, pass `None` to use AF_VSOCK.
/// Vsock command execution (QEMU or explicit callers). Prefer [`wait_ready_and_send_command`] when using the ready handshake.
#[allow(dead_code)]
pub fn send_command_via_vsock(
    cmd_parts: &[String],
    io_mode: IoMode,
    port: u32,
    unix_socket_path: Option<&std::path::Path>,
) -> Result<i32> {
    send_command_via_vsock_impl(cmd_parts, io_mode, port, unix_socket_path, false, None)
}

/// Connect to an already-running QEMU guest (AF_VSOCK to fixed guest CID, command port) without
/// the ready handshake. Use after starting a VM with `epkg run --isolate=vm --vm-keep-timeout …`.
pub fn send_command_to_running_qemu_guest(
    cmd_parts: &[String],
    io_mode: IoMode,
    vm_keep_timeout_secs: Option<u32>,
) -> Result<i32> {
    send_command_via_vsock_impl(cmd_parts, io_mode, 10000, None, true, vm_keep_timeout_secs)
}

fn send_command_via_vsock_impl(
    cmd_parts: &[String],
    io_mode: IoMode,
    port: u32,
    unix_socket_path: Option<&std::path::Path>,
    reuse_session: bool,
    vm_keep_timeout_secs: Option<u32>,
) -> Result<i32> {
    let (use_pty, is_batch) = resolve_io_mode(io_mode);
    log::debug!(
        "vm_client: io_mode={:?}, use_pty={}, is_batch={} (vsock port {}), reuse_session={}",
        io_mode,
        use_pty,
        is_batch,
        port,
        reuse_session
    );

    let mut stream = if let Some(sock_path) = unix_socket_path {
        // libkrun mode: connect via Unix socket
        log::debug!("vm_client: connecting via Unix socket {}", sock_path.display());
        connect_unix_socket_with_retry(sock_path, 30)?
    } else {
        // QEMU mode: connect via AF_VSOCK
        connect_vsock_with_retry(port, 30)?
    };
    log::debug!("vm_client: vsock connected, sending command {:?}", cmd_parts);

    let request = build_command_request(cmd_parts, io_mode, reuse_session, vm_keep_timeout_secs);
    let request_json = serde_json::to_vec(&request)?;
    stream.write_all(&request_json)?;
    stream.write_all(b"\n")?;
    log::debug!(
        "vm_client: vsock request sent ({} bytes), pty={}, batch={}, port={}",
        request_json.len(),
        use_pty,
        is_batch,
        port
    );

    if is_batch {
        handle_batch(&mut stream)
    } else {
        handle_streaming(&mut stream, use_pty)
    }
}

/// Wait for guest to signal readiness, then send command via vsock.
///
/// This implements the "ready notification" pattern from boxlite:
/// 1. Host creates listener on ready port
/// 2. Guest connects to signal "I'm ready to accept commands"
/// 3. Host connects to command port and sends command
///
/// # Modes
/// * **libkrun**: Pass `unix_socket_path` (command socket path).
///   Ready socket path is derived by replacing `vsock-` with `ready-` in filename.
///   Uses Unix socket files as vsock bridge (libkrun's vsock is Unix-based).
/// * **QEMU**: Pass `None`. Uses native AF_VSOCK on port 10001.
///
/// The mode is determined by `unix_socket_path.is_none()`:
/// - None → QEMU mode (native AF_VSOCK)
/// - Some(_) → libkrun mode (Unix socket bridge)
#[allow(dead_code)]
pub fn wait_ready_and_send_command(
    cmd_parts: &[String],
    io_mode: IoMode,
    cmd_port: u32,
    unix_socket_path: Option<&std::path::Path>,
    reuse_session: bool,
    vm_keep_timeout_secs: Option<u32>,
) -> Result<i32> {
    wait_ready_and_send_command_impl(
        cmd_parts,
        io_mode,
        cmd_port,
        unix_socket_path,
        reuse_session,
        vm_keep_timeout_secs,
        None,
        None,
    )
}

/// Extended version with QEMU process monitoring for early failure detection.
pub fn wait_ready_and_send_command_with_qemu(
    cmd_parts: &[String],
    io_mode: IoMode,
    cmd_port: u32,
    unix_socket_path: Option<&std::path::Path>,
    reuse_session: bool,
    vm_keep_timeout_secs: Option<u32>,
    qemu_child: &mut std::process::Child,
    qemu_stderr_path: &std::path::Path,
) -> Result<i32> {
    wait_ready_and_send_command_impl(
        cmd_parts,
        io_mode,
        cmd_port,
        unix_socket_path,
        reuse_session,
        vm_keep_timeout_secs,
        Some(qemu_child),
        Some(qemu_stderr_path),
    )
}

/// libkrun: wait on Unix ready socket, then send command via vsock bridge.
fn wait_ready_unix_socket_then_send(
    cmd_parts: &[String],
    io_mode: IoMode,
    cmd_port: u32,
    cmd_path: &std::path::Path,
    reuse_session: bool,
    vm_keep_timeout_secs: Option<u32>,
) -> Result<i32> {
    // Derive ready socket path from command socket path
    // e.g., vsock-123.sock → ready-123.sock
    let ready_path = cmd_path.parent().unwrap_or(std::path::Path::new(""))
        .join(cmd_path.file_name().unwrap().to_string_lossy().replace("vsock-", "ready-"));

    let _ = std::fs::remove_file(&ready_path);

    log::debug!("vm_client: creating listener on ready socket {}", ready_path.display());
    let listener = std::os::unix::net::UnixListener::bind(&ready_path)
        .map_err(|e| eyre::eyre!("Failed to bind ready socket {}: {}", ready_path.display(), e))?;

    log::debug!("vm_client: waiting for guest to signal ready...");
    let (stream, _addr) = listener.accept()
        .map_err(|e| eyre::eyre!("Failed to accept on ready socket: {}", e))?;
    log::debug!("vm_client: guest connected to ready socket, guest is ready!");
    drop(stream);
    drop(listener);

    send_command_via_vsock_impl(cmd_parts, io_mode, cmd_port, Some(cmd_path), reuse_session, vm_keep_timeout_secs)
}

/// QEMU: AF_VSOCK ready port, optional QEMU child monitoring, then send command.
fn wait_ready_qemu_vsock_then_send(
    cmd_parts: &[String],
    io_mode: IoMode,
    cmd_port: u32,
    reuse_session: bool,
    vm_keep_timeout_secs: Option<u32>,
    mut qemu_child: Option<&mut std::process::Child>,
    qemu_stderr_path: Option<&std::path::Path>,
) -> Result<i32> {
    const READY_PORT: u32 = 10001;
    log::debug!("vm_client: creating AF_VSOCK listener on ready port {}", READY_PORT);

    use std::os::fd::IntoRawFd;

    let ready_fd = socket::socket(
        AddressFamily::Vsock,
        SockType::Stream,
        SockFlag::SOCK_CLOEXEC,
        None,
    ).map_err(|e| eyre::eyre!("Failed to create ready vsock socket: {}", e))?;

    let ready_addr = VsockAddr::new(libc::VMADDR_CID_ANY, READY_PORT);
    let raw_fd = ready_fd.into_raw_fd();
    socket::bind(raw_fd, &ready_addr)
        .map_err(|e| eyre::eyre!("Failed to bind ready vsock port: {}", e))?;
    socket::listen(unsafe { &std::os::fd::BorrowedFd::borrow_raw(raw_fd) }, socket::Backlog::new(1)?)
        .map_err(|e| eyre::eyre!("Failed to listen on ready vsock port: {}", e))?;

    log::debug!("vm_client: waiting for guest to connect to ready port {}...", READY_PORT);

    let poll_timeout_ms = 100;
    let max_wait_ms = 60000;
    let mut total_waited_ms = 0;

    let client_fd = loop {
        if let Some(ref mut child) = qemu_child {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let error_msg = if let Some(stderr_path) = qemu_stderr_path {
                        std::fs::read_to_string(stderr_path).unwrap_or_default()
                    } else {
                        String::new()
                    };
                    let exit_info = status.code()
                        .map(|c| format!("exit code {}", c))
                        .unwrap_or_else(|| "killed by signal".to_string());

                    let key_error = error_msg.lines()
                        .find(|line| line.contains("error") || line.contains("failed") || line.contains("unable to"))
                        .map(|s| s.trim().to_string())
                        .unwrap_or_else(|| error_msg.lines().last().map(|s| s.trim().to_string()).unwrap_or_default());

                    return Err(eyre::eyre!(
                        "QEMU exited prematurely ({}): {}\n\
                         If vsock CID conflict, kill existing VM processes: pkill -f qemu-system\n\
                         Log: {}",
                        exit_info,
                        key_error,
                        qemu_stderr_path.map(|p| p.display().to_string()).unwrap_or_default()
                    ));
                }
                Ok(None) => {}
                Err(e) => {
                    log::debug!("vm_client: try_wait error: {}", e);
                }
            }
        }

        let mut pfd = [libc::pollfd {
            fd:      raw_fd,
            events:  libc::POLLIN,
            revents: 0,
        }];
        let ready = unsafe { libc::poll(pfd.as_mut_ptr(), 1, poll_timeout_ms as i32) };

        total_waited_ms += poll_timeout_ms;
        if total_waited_ms > max_wait_ms {
            return Err(eyre::eyre!("Timeout waiting for guest to connect to ready port"));
        }

        match ready {
            0 => {
                log::trace!("vm_client: poll timeout, continuing to wait...");
                continue;
            }
            n if n > 0 => {
                if (pfd[0].revents & libc::POLLIN) != 0 {
                    break socket::accept(raw_fd)
                        .map_err(|e| eyre::eyre!("Failed to accept on ready vsock port: {}", e))?;
                }
                if (pfd[0].revents & (libc::POLLERR | libc::POLLHUP)) != 0 {
                    return Err(eyre::eyre!("Ready socket error during poll"));
                }
            }
            _ => {
                continue;
            }
        }
    };

    log::debug!("vm_client: guest connected to ready vsock port, guest is ready!");

    let _ = nix::unistd::close(client_fd);
    let _ = nix::unistd::close(raw_fd);

    send_command_via_vsock_impl(cmd_parts, io_mode, cmd_port, None, reuse_session, vm_keep_timeout_secs)
}

fn wait_ready_and_send_command_impl(
    cmd_parts: &[String],
    io_mode: IoMode,
    cmd_port: u32,
    unix_socket_path: Option<&std::path::Path>,
    reuse_session: bool,
    vm_keep_timeout_secs: Option<u32>,
    qemu_child: Option<&mut std::process::Child>,
    qemu_stderr_path: Option<&std::path::Path>,
) -> Result<i32> {
    let (use_pty, is_batch) = resolve_io_mode(io_mode);
    log::debug!(
        "vm_client: io_mode={:?}, use_pty={}, is_batch={} (cmd port {}), reuse_session={}",
        io_mode,
        use_pty,
        is_batch,
        cmd_port,
        reuse_session
    );

    if let Some(cmd_path) = unix_socket_path {
        return wait_ready_unix_socket_then_send(
            cmd_parts,
            io_mode,
            cmd_port,
            cmd_path,
            reuse_session,
            vm_keep_timeout_secs,
        );
    }

    wait_ready_qemu_vsock_then_send(
        cmd_parts,
        io_mode,
        cmd_port,
        reuse_session,
        vm_keep_timeout_secs,
        qemu_child,
        qemu_stderr_path,
    )
}

/// Guard for raw terminal mode restoration.
struct RawTerminalGuard {
    original_termios: termios::Termios,
}

impl RawTerminalGuard {
    fn new() -> Result<Self> {
        let stdin = std::io::stdin();
        let stdin_fd = stdin.as_fd();
        log::debug!("RawTerminalGuard::new: stdin_fd={:?}", stdin_fd);
        let original_termios = termios::tcgetattr(stdin_fd)?;
        log::debug!("RawTerminalGuard::new: original local_flags={:?}", original_termios.local_flags);
        let mut raw_termios = original_termios.clone();
        termios::cfmakeraw(&mut raw_termios);
        // Ensure ECHO is definitely disabled
        raw_termios.local_flags.remove(termios::LocalFlags::ECHO);
        raw_termios.local_flags.remove(termios::LocalFlags::ECHONL);
        raw_termios.local_flags.remove(termios::LocalFlags::ECHOCTL);
        raw_termios.local_flags.remove(termios::LocalFlags::ECHOE);
        raw_termios.local_flags.remove(termios::LocalFlags::ECHOK);
        raw_termios.local_flags.remove(termios::LocalFlags::ECHOKE);
        log::debug!("RawTerminalGuard::new: raw local_flags={:?}", raw_termios.local_flags);
        log::debug!("RawTerminalGuard::new: calling tcsetattr to set raw mode");
        // Use TCSAFLUSH to discard any pending input
        termios::tcsetattr(stdin_fd, termios::SetArg::TCSAFLUSH, &raw_termios)?;
        log::debug!("RawTerminalGuard::new: raw terminal mode set successfully");
        Ok(Self { original_termios })
    }
}

impl Drop for RawTerminalGuard {
    fn drop(&mut self) {
        log::debug!("RawTerminalGuard::drop: restoring original terminal settings");
        let stdin = std::io::stdin();
        let stdin_fd = stdin.as_fd();
        let _ = termios::tcsetattr(stdin_fd, termios::SetArg::TCSADRAIN, &self.original_termios);
    }
}

/// Setup Ctrl+C signal handler to forward SIGINT to guest.
fn setup_ctrl_c_handler(signal_stream: Arc<Mutex<TcpStream>>) -> Result<()> {
    ctrlc::set_handler(move || {
        let msg = StreamMessage::Signal { signal: "INT".to_string() };
        if let Ok(json) = serde_json::to_string(&msg) {
            if let Ok(mut stream) = signal_stream.lock() {
                let _ = stream.write_all(json.as_bytes());
                let _ = stream.write_all(b"\n");
            }
        }
    })?;
    Ok(())
}

/// Send initial terminal size to guest.
fn send_initial_terminal_size(resize_stream: Arc<Mutex<TcpStream>>) {
    let (rows, cols) = Term::stdout().size();
    let msg = StreamMessage::Resize { rows, cols };
    if let Ok(json) = serde_json::to_string(&msg) {
        if let Ok(mut stream) = resize_stream.lock() {
            let _ = stream.write_all(json.as_bytes());
            let _ = stream.write_all(b"\n");
        }
    }
}

/// Create raw terminal guard if stdin is a terminal.
fn create_raw_terminal_guard() -> Option<RawTerminalGuard> {
    if std::io::stdin().is_terminal() {
        log::debug!("vm_client: stdin is a terminal, attempting raw mode");
        match RawTerminalGuard::new() {
            Ok(guard) => {
                log::debug!("vm_client: raw terminal mode enabled");
                Some(guard)
            },
            Err(e) => {
                log::debug!("Warning: failed to set terminal to raw mode: {}", e);
                None
            }
        }
    } else {
        log::debug!("vm_client: stdin is not a terminal, skipping raw mode");
        None
    }
}

/// Spawn stdin reading thread with stop flag support.
/// Uses poll() with timeout to allow graceful shutdown when stop_flag is set.
fn spawn_stdin_thread(mut stream: TcpStream, stop_flag: Arc<AtomicBool>) {
    use std::os::fd::AsFd;
    use std::thread;

    thread::spawn(move || {
        let mut seq = 0u64;
        let mut buf = [0; 4096];
        let stdin = std::io::stdin();
        let stdin_fd = stdin.as_fd();

        while !stop_flag.load(Ordering::SeqCst) {
            // Poll stdin with 10ms timeout to allow checking stop_flag
            let mut pfd = [PollFd::new(stdin_fd, PollFlags::POLLIN)];
            match poll(&mut pfd, 10u16) {
                Ok(0) => continue, // timeout, check stop_flag again
                Ok(_) => {
                    // stdin has data ready
                    match std::io::stdin().read(&mut buf) {
                        Ok(0) => break, // EOF (Ctrl+D)
                        Ok(n) => {
                            seq += 1;
                            let data = STANDARD.encode(&buf[..n]);
                            let msg = StreamMessage::Stdin { data, seq };
                            let json = match serde_json::to_string(&msg) {
                                Ok(j) => j,
                                Err(e) => {
                                    log::debug!("Failed to serialize stdin message: {}", e);
                                    break;
                                }
                            };
                            if let Err(e) = stream.write_all(json.as_bytes()) {
                                log::debug!("Failed to send stdin to server: {}", e);
                                break;
                            }
                            if let Err(e) = stream.write_all(b"\n") {
                                log::debug!("Failed to send newline to server: {}", e);
                                break;
                            }
                        }
                        Err(e) => {
                            log::debug!("Failed to read from stdin: {}", e);
                            break;
                        }
                    }
                }
                Err(e) => {
                    log::debug!("poll() failed: {}", e);
                    break;
                }
            }
        }
        log::debug!("stdin thread exiting, stop_flag={}", stop_flag.load(Ordering::SeqCst));
    });
}

/// Write bytes to stdout/stderr. When the output is a terminal (e.g. PTY mode),
/// translate \n to \r\n so line endings display correctly in raw mode.
fn write_stream_output(
    output: &mut dyn Write,
    bytes: &[u8],
    is_terminal: bool,
) -> std::io::Result<()> {
    log::trace!("write_stream_output: {} bytes, is_terminal={}", bytes.len(), is_terminal);
    if is_terminal {
        let mut last = 0;
        for i in 0..bytes.len() {
            if bytes[i] == b'\n' && (i == 0 || bytes[i - 1] != b'\r') {
                output.write_all(&bytes[last..i])?;
                output.write_all(b"\r\n")?;
                last = i + 1;
            }
        }
        if last < bytes.len() {
            output.write_all(&bytes[last..])?;
        }
    } else {
        output.write_all(&bytes)?;
    }
    output.flush()
}

/// Check and send terminal resize if pending.
fn check_and_send_resize(resize_stream: &Arc<Mutex<TcpStream>>) {
    if RESIZE_PENDING.swap(false, Ordering::SeqCst) {
        let (rows, cols) = Term::stdout().size();
        let msg = StreamMessage::Resize { rows, cols };
        if let Ok(json) = serde_json::to_string(&msg) {
            if let Ok(mut stream) = resize_stream.lock() {
                let _ = stream.write_all(json.as_bytes());
                let _ = stream.write_all(b"\n");
            }
        }
    }
}

/// Process a single stream message (stdout/stderr/exit).
fn process_stream_message(msg: StreamMessage) -> Result<Option<i32>> {
    match msg {
        StreamMessage::Stdout { data, .. } => {
            let bytes = STANDARD.decode(&data)?;
            write_stream_output(
                &mut std::io::stdout(),
                &bytes,
                std::io::stdout().is_terminal(),
            )?;
            Ok(None)
        }
        StreamMessage::Stderr { data, .. } => {
            let bytes = STANDARD.decode(&data)?;
            write_stream_output(
                &mut std::io::stderr(),
                &bytes,
                std::io::stderr().is_terminal(),
            )?;
            Ok(None)
        }
        StreamMessage::Exit { code } => {
            log::debug!("vm_client: received exit message, code={}", code);
            Ok(Some(code))
        }
        // Ignore other message types
        _ => Ok(None)
    }
}

/// Run main PTY message processing loop.
fn run_pty_main_loop(
    reader: &mut BufReader<&mut TcpStream>,
    resize_stream: Arc<Mutex<TcpStream>>,
) -> Result<i32> {
    let mut line = String::new();
    let mut exit_code = 0;

    loop {
        check_and_send_resize(&resize_stream);

        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => {
                log::debug!("vm_client: TCP EOF");
                break;
            },
            Ok(_) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                match serde_json::from_str::<StreamMessage>(line) {
                    Ok(msg) => {
                        if let Some(code) = process_stream_message(msg)? {
                            exit_code = code;
                            break;
                        }
                    }
                    Err(e) => {
                        log::debug!("Failed to parse stream message: {} (line: {:?})", e, line);
                        break;
                    }
                }
            }
            Err(e) => {
                return Err(e.into());
            }
        }
    }

    Ok(exit_code)
}

/// Setup PTY mode: signal handlers, terminal size, raw mode.
fn setup_pty_mode(
    signal_stream: Arc<Mutex<TcpStream>>,
    resize_stream: Arc<Mutex<TcpStream>>,
) -> Result<Option<RawTerminalGuard>> {
    setup_ctrl_c_handler(Arc::clone(&signal_stream))?;
    unsafe {
        signal(Signal::SIGWINCH, SigHandler::Handler(handle_sigwinch))?;
    }
    send_initial_terminal_size(Arc::clone(&resize_stream));
    let raw_guard = create_raw_terminal_guard();
    if raw_guard.is_some() {
        log::debug!("vm_client: raw terminal guard created");
    } else {
        log::debug!("vm_client: no raw terminal guard (stdin not terminal or failed)");
    }
    Ok(raw_guard)
}

/// Run non-PTY message loop (simplified, no resize checks).
fn run_non_pty_loop(reader: &mut BufReader<&mut TcpStream>) -> Result<i32> {
    let mut line = String::new();
    let mut exit_code = 0;
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                match serde_json::from_str::<StreamMessage>(line) {
                    Ok(msg) => {
                        if let Some(code) = process_stream_message(msg)? {
                            exit_code = code;
                            break;
                        }
                    }
                    Err(e) => {
                        log::debug!("Failed to parse stream message: {} (line: {:?})", e, line);
                        break;
                    }
                }
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(exit_code)
}

fn handle_streaming(stream: &mut TcpStream, use_pty: bool) -> Result<i32> {
    use std::io::BufReader;
    use std::sync::{Arc, Mutex};

    // Stop flag to signal stdin thread to exit
    let stop_flag = Arc::new(AtomicBool::new(false));

    // Clone TCP stream for stdin thread
    let stream_for_stdin = stream.try_clone()?;
    let signal_stream = if use_pty {
        let stream_for_signal = stream.try_clone()?;
        Arc::new(Mutex::new(stream_for_signal))
    } else {
        Arc::new(Mutex::new(stream_for_stdin.try_clone()?))
    };
    let resize_stream = Arc::clone(&signal_stream);

    // Raw terminal guard - must live for the duration of PTY mode
    let _raw_guard: Option<RawTerminalGuard> = if use_pty {
        setup_pty_mode(Arc::clone(&signal_stream), Arc::clone(&resize_stream))?
    } else {
        None
    };

    // Spawn stdin thread (needed for both modes)
    spawn_stdin_thread(stream_for_stdin, Arc::clone(&stop_flag));

    // Main thread: read from TCP and handle messages
    let mut reader = BufReader::new(stream);
    let result = if use_pty {
        run_pty_main_loop(&mut reader, resize_stream)
    } else {
        run_non_pty_loop(&mut reader)
    };

    // Signal stdin thread to stop before returning
    stop_flag.store(true, Ordering::SeqCst);

    result
}

/// Batch mode response from vm-daemon.
#[derive(Debug, Deserialize)]
struct BatchResult {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

/// Handle batch mode: read single JSON response with all output.
fn handle_batch(stream: &mut TcpStream) -> Result<i32> {
    let mut response = String::new();
    let mut reader = BufReader::new(stream);
    reader.read_line(&mut response)?;

    let result: BatchResult = serde_json::from_str(response.trim())
        .map_err(|e| eyre::eyre!("Failed to parse batch response: {} ({:?})", e, response))?;

    // Decode and write stdout
    if !result.stdout.is_empty() {
        let stdout_bytes = STANDARD.decode(&result.stdout)?;
        std::io::stdout().write_all(&stdout_bytes)?;
    }

    // Decode and write stderr
    if !result.stderr.is_empty() {
        let stderr_bytes = STANDARD.decode(&result.stderr)?;
        std::io::stderr().write_all(&stderr_bytes)?;
    }

    Ok(result.exit_code)
}
