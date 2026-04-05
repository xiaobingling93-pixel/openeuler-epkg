//! VM Daemon - TCP server for guest command execution.
//!
//! # Overview
//! This module implements a TCP server that runs inside a QEMU virtual machine as the init process (PID 1).
//! It accepts commands from the host over a forwarded TCP port (default: 10000), executes them in the guest,
//! and streams back output/exit codes. The behavior is similar to `docker exec [-it]` or `ssh [-t|-T]`:
//! - Starts VM to run one single command inside and exit
//! - Supports both simple commands (ls) and interactive programs (bash, vim, htop)
//! - Handles PTY allocation for terminal-based applications
//! - Gracefully shuts down the VM after command completion
//!
//! # Invocation
//! **IMPORTANT:** vm-daemon is NOT invoked as a separate applet binary. Instead, it is called directly
//! as a function from init.rs (`crate::vm::guest_daemon::run(options)`) after the guest kernel boots.
//! The symlink `/usr/bin/vm-daemon -> epkg` exists for build system compatibility, but the init process
//! never execs the vm-daemon binary - it calls the function directly.
//!
//! # Protocol Design
//! ## Connection Model
//! - Default: one command per connection; after the command the daemon may wait for more
//!   connections (install/upgrade reuse, see `reuse_vm` and reserved `command` `__epkg_vm_session_done__`)
//!   or power off the guest (e.g. `epkg run --isolate=vm` one-shot)
//! - Note: vm-daemon is forked from init.rs (PID 1), not running as PID 1 itself
//!
//! ## Message Format
//! ### Request (Client → Server)
//! ```json
//! {
//!   "command": ["/usr/bin/ls", "/"],
//!   "cwd": null,
//!   "env": {},
//!   "stdin": "",
//!   "pty": false,
//!   "terminal": {"rows": 49, "cols": 201}  // optional, for PTY mode
//! }
//! ```
//!
//! ### Response Protocol (Both PTY and Non-PTY Modes)
//! All communication uses JSON messages with newline separation:
//! - `{"type":"stdout", "data":"<base64>", "seq":N}` - Standard output data
//! - `{"type":"stderr", "data":"<base64>", "seq":N}` - Standard error data
//! - `{"type":"exit", "code":N}` - Final exit code (always sent, uniform for both modes)
//! - `{"type":"resize", "rows":R, "cols":C}` - Terminal resize notifications (PTY only)
//! - `{"type":"signal", "signal":"INT"}` - Signal forwarding (PTY only)
//! - `{"type":"stdin", "data":"<base64>", "seq":N}` - Standard input data (client→server)
//!
//! ## PTY vs Non-PTY Mode
//! - **PTY mode** (`pty: true`): For interactive terminal applications (bash, vim, htop)
//!   - Allocates pseudo-terminal using `openpty()`
//!   - Forks child process with PTY as stdin/stdout/stderr
//!   - Poll loop forwards PTY output to TCP and TCP input to PTY
//!   - Supports terminal resizing and signal forwarding
//!   - Uses base64 encoding for binary-safe transport of raw terminal data
//!
//! - **Non-PTY mode** (`pty: false`): Progressive output for all commands
//!   - Uses regular pipes for stdin/stdout/stderr
//!   - Poll loop forwards pipe output to TCP as it arrives (real-time progressive output)
//!   - TCP input forwarded to child's stdin via `stdin` messages
//!   - Exit code sent via `exit` message (same as PTY mode)
//!   - Behaves like `ssh -T` with real-time output streaming
//!   - Optional `--batch` mode could be added later for batched output collection
//!
//! # Key Implementation Details
//! ## TCP Handling
//! - Server listens on 0.0.0.0:10000 (bound inside guest)
//! - QEMU hostfwd forwards host's TCP port 10000 to guest port 10000
//! - Single `read()` may contain multiple JSON messages (request + other messages)
//!   - Code splits at first newline to separate request from leftover messages
//!   - Leftover bytes are injected into poll loop for processing (both PTY and non-PTY)
//!
//! ## Process Management
//! - vm-daemon is forked from init.rs (PID 1), which continues as system init
//! - Uses `nix::waitpid` with `WNOHANG` to check child status during poll loop
//! - After child exit, drains remaining output before sending exit message
//!
//! ## Terminal Handling
//! - PTY master/slave setup with proper session management (`setsid`, `TIOCSCTTY`)
//! - Terminal resizing via `ioctl(TIOCSWINSZ)` on master PTY
//! - Signal forwarding from host to guest process (e.g., Ctrl+C → SIGINT)
//!
//! # Expected Behavior
//! ## Debug Commands (for testing)
//! ```bash
//! epkg run --isolate=vm [--io=tty] bash /
//! epkg run --isolate=vm --vm-keep-timeout 60 bash   # then: epkg run --isolate=vm --reuse …
//! timeout 10 epkg run --isolate=vm [--io=tty|--io=stream|--io=batch] ls /
//! file_list=$(timeout 10 epkg run --isolate=vm --io=batch ls /)
//! ```
//!
//! ## Success Criteria
//! 1. Interactive bash session works (PTY mode) - user can type commands and see output
//! 2. Simple ls command shows output in console (both PTY and non-PTY modes)
//! 3. Scripting: ls output can be captured in shell variable (`file_list=$(epkg ...)`)
//! 4. Exit code propagation: command's exit code sent via JSON (PTY: exit message, non-PTY: response field)
//! 5. Graceful shutdown: VM powers off without kernel panic after command completion
//!
//! # Error Handling
//! - JSON parse failure returns error response (no plain text fallback)
//! - PTY allocation failure falls back to non-PTY mode
//! - Child process spawn errors sent via `exit` message with code=-1
//! - All errors logged via `log::debug!` for troubleshooting
//!
//! # Debug Tips
//! - Monitor QEMU logs: `tail /home/wfg/.cache/epkg/vmm-logs/latest-qemu.log`
//! - Enable debug logging: set `RUST_LOG=debug` environment variable
//! - Check network connectivity in VM: verify virtio_net module is loaded
//! - On verifying a bug, always run epkg with timeout 10 to avoid being blocked.
//!
//! - With debug logs (RUST_LOG=debug epkg run --isolate=vm --io=tty bash):
//!   debug log lines will be misaligned because env_logger writes directly to stderr while
//!   the terminal is raw (bypasses our translation). This is expected and harmless.
//!
//! # Dependencies
//! - `nix`: PTY, process, signal handling
//! - `serde_json`: JSON serialization
//! - `base64`: Binary-safe encoding for PTY data
//! - `color_eyre`: Error handling
//!
#![cfg(target_os = "linux")]

use crate::busybox::init::kmsg_write;
use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use serde::Deserialize;
use serde_json;
use libc;
use std::net::TcpStream;
use std::io::{Read, Write};
use std::process::Command as StdCommand;
use nix::poll::{poll, PollFd, PollFlags, PollTimeout};
use nix::pty::{openpty, Winsize, OpenptyResult};
use nix::sys::wait::{waitpid, WaitPidFlag};
use nix::unistd::{fork, ForkResult, dup2, setsid, close, Pid, getuid, geteuid, getgid, getegid};
use nix::fcntl::{fcntl, FcntlArg, OFlag};
use std::os::fd::{OwnedFd, AsRawFd, BorrowedFd, FromRawFd, IntoRawFd, RawFd};
use nix::sys::socket::{self, AddressFamily, SockType, SockFlag, VsockAddr, Backlog};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use crate::run::VM_SESSION_DONE_CMD;
use crate::vm_client::StreamMessage;

/// Poll timeout in milliseconds for PTY/pipe and TCP wait.
const POLL_TIMEOUT_MS: u32 = 1000;

/// Default idle window (ms) for another vsock connection when `reuse_vm` is set but
/// `vm_keep_timeout_secs` is omitted in the JSON request.
const VM_REUSE_IDLE_TIMEOUT_MS: u32 = 30_000;

/// Maximum poll loop iterations in non-PTY mode before forcing exit (safety limit).

/// TCP line buffer size (must fit at least one JSON message line).
const TCP_LINE_BUF_SIZE: usize = 4096;

fn log_process_identity(tag: &str) {
    log::debug!(
        "{}: pid={} uid={} euid={} gid={} egid={}",
        tag,
        std::process::id(),
        getuid().as_raw(),
        geteuid().as_raw(),
        getgid().as_raw(),
        getegid().as_raw()
    );

    match std::fs::read_to_string("/proc/self/status") {
        Ok(status) => {
            for line in status.lines() {
                if line.starts_with("Uid:")
                    || line.starts_with("Gid:")
                    || line.starts_with("Groups:")
                    || line.starts_with("CapInh:")
                    || line.starts_with("CapPrm:")
                    || line.starts_with("CapEff:")
                    || line.starts_with("CapBnd:")
                    || line.starts_with("NoNewPrivs:")
                {
                    log::debug!("{}: {}", tag, line);
                }
            }
        }
        Err(e) => {
            log::debug!("{}: failed to read /proc/self/status: {}", tag, e);
        }
    }
}

/// Write a single newline-delimited JSON stream message to the TCP stream.
fn write_stream_message(stream: &mut TcpStream, msg: &StreamMessage) -> Result<()> {
    log::trace!("write_stream_message: {:?}", msg);
    let json = serde_json::to_string(msg)?;
    stream.write_all(json.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    Ok(())
}

/// Compute exit code from wait status (exited code, or 128+signal for signaled).
fn exit_code_from_wait_status(status: nix::sys::wait::WaitStatus) -> i32 {
    match status {
        nix::sys::wait::WaitStatus::Exited(_, code) => code,
        nix::sys::wait::WaitStatus::Signaled(_, sig, _) => 128 + sig as i32,
        _ => 1,
    }
}

/// Send exit message, flush stream, and return the exit code.
fn send_exit_and_flush(stream: &mut TcpStream, exit_code: i32) -> Result<i32> {
    write_stream_message(stream, &StreamMessage::Exit { code: exit_code })?;
    stream.flush()?;
    Ok(exit_code)
}

/// Encode data as base64, send as stdout message, increment seq.
fn send_stdout_chunk(stream: &mut TcpStream, data: &[u8], seq: &mut u64) -> Result<()> {
    *seq += 1;
    let msg = StreamMessage::Stdout {
        data: STANDARD.encode(data),
        seq: *seq,
    };
    write_stream_message(stream, &msg)
}

/// Encode data as base64, send as stderr message, increment seq.
fn send_stderr_chunk(stream: &mut TcpStream, data: &[u8], seq: &mut u64) -> Result<()> {
    *seq += 1;
    let msg = StreamMessage::Stderr {
        data: STANDARD.encode(data),
        seq: *seq,
    };
    write_stream_message(stream, &msg)
}

/// Parse newline-separated lines in `tcp_buf[..*tcp_buf_pos]`, call `handler` for each JSON message,
/// then shift remaining data to the start of the buffer and update `*tcp_buf_pos`.
/// If the buffer is full with no newline, resets position to avoid infinite loop.
fn process_tcp_line_buffer<F>(tcp_buf: &mut [u8], tcp_buf_pos: &mut usize, mut handler: F) -> Result<()>
where
    F: FnMut(StreamMessage) -> Result<()>,
{
    let mut start = 0;
    for i in 0..*tcp_buf_pos {
        if tcp_buf[i] == b'\n' {
            let line = &tcp_buf[start..i];
            if !line.is_empty() {
                if let Ok(msg) = serde_json::from_slice::<StreamMessage>(line) {
                    handler(msg)?;
                }
            }
            start = i + 1;
        }
    }
    if start < *tcp_buf_pos {
        tcp_buf.copy_within(start..*tcp_buf_pos, 0);
        *tcp_buf_pos -= start;
    } else {
        *tcp_buf_pos = 0;
    }
    if *tcp_buf_pos == tcp_buf.len() && !tcp_buf[..*tcp_buf_pos].contains(&b'\n') {
        log::debug!("TCP line buffer full with no newline, discarding");
        *tcp_buf_pos = 0;
    }
    Ok(())
}

/// Drain readable bytes from `reader` and send as stdout messages. Stops on EOF (0), WouldBlock, or EIO.
/// Returns `true` if EOF was seen.
fn drain_reader_to_stdout<R: Read>(
    reader: &mut R,
    stream: &mut TcpStream,
    buf: &mut [u8],
    seq_out: &mut u64,
) -> Result<bool> {
    loop {
        match reader.read(buf) {
            Ok(0) => return Ok(true),
            Ok(n) => {
                send_stdout_chunk(stream, &buf[..n], seq_out)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
            Err(e) if e.raw_os_error() == Some(libc::EIO) => return Ok(true),
            Err(e) => return Err(e.into()),
        }
    }
}

/// Drain readable bytes from `reader` and send as stderr messages. Stops on EOF (0) or WouldBlock.
fn drain_reader_to_stderr<R: Read>(
    reader: &mut R,
    stream: &mut TcpStream,
    buf: &mut [u8],
    seq_err: &mut u64,
) -> Result<bool> {
    loop {
        match reader.read(buf) {
            Ok(0) => return Ok(true),
            Ok(n) => {
                send_stderr_chunk(stream, &buf[..n], seq_err)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
            Err(e) => return Err(e.into()),
        }
    }
}

#[derive(Debug, Deserialize)]
struct TerminalConfig {
    rows: u16,
    cols: u16,
}

#[derive(Debug, Default)]
#[allow(dead_code)]
pub struct VmDaemonOptions {
    pub port:        u16,
    pub host:        String,
    /// Reverse mode: Guest connects to Host instead of listening.
    /// This avoids vsock handshake timing issues on Windows/WHPX.
    pub reverse_mode: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<VmDaemonOptions> {
    let port = matches.get_one::<u16>("port").copied().unwrap_or(10000);
    let host = matches.get_one::<String>("host").cloned().unwrap_or_else(|| "0.0.0.0".to_string());

    Ok(VmDaemonOptions { port, host, reverse_mode: false })
}

pub fn command() -> Command {
    Command::new("vm-daemon")
        .about("Server for VM guest command execution (TCP or vsock)")
        .arg(
            Arg::new("port")
                .short('p')
                .long("port")
                .value_name("PORT")
                .help("Port to listen on")
                .default_value("10000")
                .value_parser(clap::value_parser!(u16))
        )
        .arg(
            Arg::new("host")
                .long("host")
                .value_name("HOST")
                .help("Host address to bind to")
                .default_value("0.0.0.0")
        )
}

#[derive(Debug, Deserialize)]
struct CommandRequest {
    /// After this command, wait for more connections instead of powering off.
    #[serde(default)]
    reuse_vm: bool,
    /// Idle time in whole seconds to wait for the next connection after this command completes.
    /// When omitted with `reuse_vm`, [`VM_REUSE_IDLE_TIMEOUT_MS`] is used.
    #[serde(default)]
    vm_keep_timeout_secs: Option<u32>,
    #[serde(default)]
    command: Vec<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    env: std::collections::HashMap<String, String>,
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    stdin: String,
    #[serde(default)]
    pty: bool,
    #[serde(default)]
    batch: bool,
    #[serde(default)]
    terminal: Option<TerminalConfig>,
}

/// What to do after handling one vsock connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionDisposition {
    /// Stop accepting; guest will power off.
    Shutdown,
    /// Keep listening for another command; poll interval before idle shutdown.
    ReuseWait { idle_timeout_ms: u32 },
}

fn resolve_request_user(user: Option<&str>) -> Result<Option<(u32, u32)>> {
    let Some(user_str) = user else {
        return Ok(None);
    };
    if user_str.is_empty() {
        return Ok(None);
    }

    let passwd_entries = crate::userdb::read_passwd(None)?;
    if let Ok(uid_raw) = user_str.parse::<u32>() {
        let gid_raw = passwd_entries
            .iter()
            .find(|u| u.uid == uid_raw)
            .map(|u| u.gid)
            .unwrap_or(uid_raw);
        return Ok(Some((uid_raw, gid_raw)));
    }

    if user_str == "root" {
        return Ok(Some((0, 0)));
    }

    match passwd_entries.iter().find(|u| u.name == user_str) {
        Some(u) => Ok(Some((u.uid, u.gid))),
        None => Err(eyre!("Requested user not found in guest: {}", user_str)),
    }
}

/// Outcome of spawning the child with pipes. Either the child and its stdio pipes, or spawn failed with error message.
enum SpawnOutcome {
    Spawned(
        std::process::Child,
        std::process::ChildStdin,
        std::process::ChildStdout,
        std::process::ChildStderr,
    ),
    SpawnFailed { error_msg: String },
}

/// Spawn the command with piped stdin/stdout/stderr. On spawn failure, returns error message without sending to stream.
/// Caller is responsible for sending error response in appropriate format (streaming or batch).
fn spawn_child_piped(
    request: &CommandRequest,
) -> Result<SpawnOutcome> {
    log_process_identity("vm-daemon spawn_child_piped parent");
    let mut child = StdCommand::new(&request.command[0]);
    child.args(&request.command[1..]);
    if let Some(cwd) = &request.cwd {
        child.current_dir(cwd);
    }
    let uid_gid = resolve_request_user(request.user.as_deref())?;
    if let Some((uid, gid)) = uid_gid {
        use std::os::unix::process::CommandExt;
        log::debug!("vm-daemon spawn_child_piped: setting child uid={} gid={}", uid, gid);
        child.uid(uid);
        child.gid(gid);
    }

    // Build environment with HOME set appropriately
    let mut env = request.env.clone();
    if !env.contains_key("HOME") {
        let home = if let Some((uid, _)) = uid_gid {
            if uid == 0 {
                "/root".to_string()
            } else {
                // Try to find user's home from passwd
                let passwd_entries = crate::userdb::read_passwd(None)?;
                passwd_entries
                    .iter()
                    .find(|u| u.uid == uid)
                    .map(|u| u.dir.clone())
                    .unwrap_or_else(|| format!("/home/{}", uid))
            }
        } else {
            // No user specified, running as current user (likely root from init)
            "/root".to_string()
        };
        env.insert("HOME".to_string(), home);
    }

    log::debug!("vm-daemon spawn_child_piped: setting env: {:?}", env);
    child.envs(&env);
    child.stdin(std::process::Stdio::piped());
    child.stdout(std::process::Stdio::piped());
    child.stderr(std::process::Stdio::piped());

    match child.spawn() {
        Ok(mut c) => {
            log::debug!("Child spawned successfully");
            let stdin_pipe = c.stdin.take().expect("stdin pipe");
            let stdout_pipe = c.stdout.take().expect("stdout pipe");
            let stderr_pipe = c.stderr.take().expect("stderr pipe");
            Ok(SpawnOutcome::Spawned(c, stdin_pipe, stdout_pipe, stderr_pipe))
        }
        Err(e) => {
            let error_msg = format!("Failed to spawn {}: {}", request.command[0], e);
            log::debug!("{}", error_msg);
            Ok(SpawnOutcome::SpawnFailed { error_msg })
        }
    }
}

/// Run in the forked child: set up PTY as stdio and exec the command. Does not return on success.
fn pty_run_child(request: &CommandRequest, master: OwnedFd, slave: OwnedFd) -> ! {
    use std::os::unix::process::CommandExt;
    log_process_identity("vm-daemon pty child before stdio setup");
    close(master).expect("close master");
    setsid().expect("setsid");
    let slave_fd = slave.as_raw_fd();
    unsafe { libc::ioctl(slave_fd, libc::TIOCSCTTY, 0) };
    let mut stdin_fd = unsafe { OwnedFd::from_raw_fd(0) };
    let mut stdout_fd = unsafe { OwnedFd::from_raw_fd(1) };
    let mut stderr_fd = unsafe { OwnedFd::from_raw_fd(2) };
    dup2(&slave, &mut stdin_fd).expect("dup2 stdin");
    dup2(&slave, &mut stdout_fd).expect("dup2 stdout");
    dup2(&slave, &mut stderr_fd).expect("dup2 stderr");
    close(slave).expect("close slave");
    let mut cmd = std::process::Command::new(&request.command[0]);
    cmd.args(&request.command[1..]);
    if let Some(cwd) = &request.cwd {
        cmd.current_dir(cwd);
    }
    let uid_gid = match resolve_request_user(request.user.as_deref()) {
        Ok(Some((uid, gid))) => {
            log::debug!("vm-daemon pty child: setting child uid={} gid={}", uid, gid);
            cmd.uid(uid);
            cmd.gid(gid);
            Some((uid, gid))
        }
        Ok(None) => None,
        Err(e) => {
            log::debug!("vm-daemon pty child: failed to resolve user {:?}: {}", request.user, e);
            std::process::exit(1);
        }
    };

    // Build environment with HOME set appropriately
    let mut env = request.env.clone();
    if !env.contains_key("HOME") {
        let home = if let Some((uid, _)) = uid_gid {
            if uid == 0 {
                "/root".to_string()
            } else {
                // Try to find user's home from passwd
                let passwd_entries = crate::userdb::read_passwd(None);
                passwd_entries
                    .ok()
                    .and_then(|entries| entries.iter().find(|u| u.uid == uid).map(|u| u.dir.clone()))
                    .unwrap_or_else(|| format!("/home/{}", uid))
            }
        } else {
            // No user specified, running as current user (likely root from init)
            "/root".to_string()
        };
        env.insert("HOME".to_string(), home);
    }

    cmd.envs(&env);
    log_process_identity("vm-daemon pty child before exec");
    let err = cmd.exec();
    log::debug!("Failed to execute command: {}", err);
    std::process::exit(1);
}

/// Handle PTY master output: read data and send as stdout messages.
fn handle_pty_output<F: std::io::Read + std::io::Write>(
    master_file: &mut F,
    stream: &mut TcpStream,
    buf: &mut [u8],
    seq_out: &mut u64,
) -> Result<bool> {
    match master_file.read(buf) {
        Ok(0) => {
            log::debug!("execute_with_pty: PTY EOF");
            Ok(true)
        }
        Ok(n) => {
            log::trace!("execute_with_pty: read {} bytes from PTY", n);
            send_stdout_chunk(stream, &buf[..n], seq_out)?;
            Ok(false)
        }
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(false),
        Err(e) if e.raw_os_error() == Some(libc::EIO) => {
            log::debug!("execute_with_pty: PTY EIO (slave closed), treating as EOF");
            Ok(true)
        }
        Err(e) => Err(e.into()),
    }
}

/// Handle TCP input: parse messages and forward stdin/signals/resize to PTY.
fn handle_pty_tcp_input<F: std::io::Write + AsRawFd>(
    stream: &mut TcpStream,
    master_file: &mut F,
    tcp_buf: &mut [u8],
    tcp_buf_pos: &mut usize,
    child: Pid,
) -> Result<bool> {
    match stream.read(&mut tcp_buf[*tcp_buf_pos..]) {
        Ok(0) => Ok(true), // EOF from TCP
        Ok(n) => {
            *tcp_buf_pos += n;
            process_tcp_line_buffer(tcp_buf, tcp_buf_pos, |msg| {
                match msg {
                    StreamMessage::Stdin { data, .. } => {
                        let bytes = STANDARD.decode(&data)?;
                        master_file.write_all(&bytes)?;
                        Ok(())
                    }
                    StreamMessage::Signal { signal } => {
                        let sig = match signal.as_str() {
                            "INT"   => nix::sys::signal::Signal::SIGINT,
                            "TERM"  => nix::sys::signal::Signal::SIGTERM,
                            "HUP"   => nix::sys::signal::Signal::SIGHUP,
                            "QUIT"  => nix::sys::signal::Signal::SIGQUIT,
                            "KILL"  => nix::sys::signal::Signal::SIGKILL,
                            "WINCH" => nix::sys::signal::Signal::SIGWINCH,
                            _ => {
                                log::debug!("Unknown signal: {}", signal);
                                return Ok(());
                            }
                        };
                        let _ = nix::sys::signal::kill(child, sig);
                        Ok(())
                    }
                    StreamMessage::Resize { rows, cols } => {
                        let ws = Winsize {
                            ws_row:    rows,
                            ws_col:    cols,
                            ws_xpixel: 0,
                            ws_ypixel: 0,
                        };
                        let master_fd = master_file.as_raw_fd();
                        unsafe { libc::ioctl(master_fd, libc::TIOCSWINSZ, &ws) };
                        Ok(())
                    }
                    _ => Ok(()),
                }
            })?;
            Ok(false)
        }
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Check if child exited, drain PTY output if so. Returns (child_exited, child_status).
fn check_child_and_drain_pty<F: std::io::Read>(
    child: Pid,
    child_status: &mut Option<nix::sys::wait::WaitStatus>,
    master_file: &mut F,
    stream: &mut TcpStream,
    buf: &mut [u8],
    seq_out: &mut u64,
) -> Result<bool> {
    if child_status.is_some() {
        return Ok(true);
    }
    match waitpid(child, Some(WaitPidFlag::WNOHANG)) {
        Ok(status) => match status {
            nix::sys::wait::WaitStatus::StillAlive => Ok(false),
            other_status => {
                log::debug!("execute_with_pty: child exit detected: {:?}", other_status);
                *child_status = Some(other_status);
                let _eof = drain_reader_to_stdout(master_file, stream, buf, seq_out)?;
                let _ = stream.flush();
                Ok(true)
            }
        },
        Err(_) => Ok(true),
    }
}

/// Check if child exited (non-blocking). Returns true if exited.
fn check_child_status(
    child_pid: Pid,
    child_status: &mut Option<nix::sys::wait::WaitStatus>,
) -> bool {
    if child_status.is_some() {
        return true;
    }
    match waitpid(Some(child_pid), Some(WaitPidFlag::WNOHANG)) {
        Ok(status) => match status {
            nix::sys::wait::WaitStatus::StillAlive => false,
            other_status => {
                *child_status = Some(other_status);
                true
            }
        },
        Err(e) => {
            log::debug!("waitpid error: {}", e);
            *child_status = Some(nix::sys::wait::WaitStatus::Exited(Pid::from_raw(0), 1));
            true
        }
    }
}

/// Drain stdout and stderr pipes after child exit.
fn drain_pipes(
    stdout_file: &mut std::process::ChildStdout,
    stderr_file: &mut std::process::ChildStderr,
    stream: &mut TcpStream,
    buf: &mut [u8],
    seq_out: &mut u64,
    seq_err: &mut u64,
) -> Result<()> {
    let _ = drain_reader_to_stdout(stdout_file, stream, buf, seq_out)?;
    let _ = drain_reader_to_stderr(stderr_file, stream, buf, seq_err)?;
    let _ = stream.flush();
    Ok(())
}

/// Handle TCP input for non-PTY mode: parse stdin messages and forward to child.
/// Handle TCP input: parse messages and forward stdin/signals to child process.
/// Returns (tcp_eof, stdin_eof) - true if TCP closed, true if stdin EOF received.
fn handle_nonpty_tcp_input<W: std::io::Write>(
    stream: &mut TcpStream,
    stdin_file: &mut Option<W>,
    tcp_buf: &mut [u8],
    tcp_buf_pos: &mut usize,
) -> Result<(bool, bool)> {
    match stream.read(&mut tcp_buf[*tcp_buf_pos..]) {
        Ok(0) => Ok((true, false)), // EOF from TCP
        Ok(n) => {
            *tcp_buf_pos += n;
            let mut stdin_eof = false;
            process_tcp_line_buffer(tcp_buf, tcp_buf_pos, |msg| {
                match msg {
                    StreamMessage::Stdin { data, .. } => {
                        if let Some(ref mut stdin) = stdin_file {
                            let bytes = STANDARD.decode(&data)?;
                            stdin.write_all(&bytes)?;
                        }
                        Ok(())
                    }
                    StreamMessage::StdinEof { .. } => {
                        stdin_eof = true;
                        Ok(())
                    }
                    _ => Ok(()),
                }
            })?;
            Ok((false, stdin_eof))
        }
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok((false, false)),
        Err(e) => Err(e.into()),
    }
}

/// Setup PTY with nonblocking master and return (master_file, child_pid).
fn setup_pty_and_fork(request: &CommandRequest) -> Result<(std::fs::File, Pid)> {
    use std::fs::File;
    use std::os::fd::{IntoRawFd, FromRawFd};

    let winsize = request.terminal.as_ref().map(|term| Winsize {
        ws_row:    term.rows,
        ws_col:    term.cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    });

    let OpenptyResult { master, slave } = openpty(winsize.as_ref(), None)?;

    match unsafe { fork() }? {
        ForkResult::Child => pty_run_child(request, master, slave),
        ForkResult::Parent { child } => {
            log::debug!("execute_with_pty: child spawned with PID {:?}", child);
            close(slave)?;

            let master_raw = master.into_raw_fd();
            let master_bfd = unsafe { BorrowedFd::borrow_raw(master_raw) };
            fcntl(master_bfd, FcntlArg::F_SETFL(OFlag::O_NONBLOCK))?;
            let master_file = unsafe { File::from_raw_fd(master_raw) };

            Ok((master_file, child))
        }
    }
}

/// Handle poll events for PTY mode: process PTY output and TCP input. Returns true if should break loop.
fn handle_pty_poll_events(
    poll_fds: &[PollFd],
    child: Pid,
    master_file: &mut std::fs::File,
    stream: &mut TcpStream,
    buf: &mut [u8],
    tcp_buf: &mut [u8],
    tcp_buf_pos: &mut usize,
    seq_out: &mut u64,
) -> Result<bool> {
    let master_revents = poll_fds[0].revents().unwrap();
    if master_revents.contains(PollFlags::POLLIN) || master_revents.contains(PollFlags::POLLHUP) {
        if handle_pty_output(master_file, stream, buf, seq_out)? {
            return Ok(true);
        }
    }

    let tcp_revents = poll_fds[1].revents().unwrap();
    if tcp_revents.contains(PollFlags::POLLIN) {
        if handle_pty_tcp_input(stream, master_file, tcp_buf, tcp_buf_pos, child)? {
            return Ok(true);
        }
    }

    Ok(false)
}

/// Poll loop for PTY mode: forward PTY output to TCP, TCP input to PTY, check child status.
fn pty_poll_loop(
    child: Pid,
    master_file: &mut std::fs::File,
    stream: &mut TcpStream,
    initial_data: Option<Vec<u8>>,
) -> Result<Option<nix::sys::wait::WaitStatus>> {
    let master_bfd = unsafe { BorrowedFd::borrow_raw(master_file.as_raw_fd()) };
    let stream_bfd = unsafe { BorrowedFd::borrow_raw(stream.as_raw_fd()) };

    let mut poll_fds = vec![
        PollFd::new(master_bfd, PollFlags::POLLIN),
        PollFd::new(stream_bfd, PollFlags::POLLIN),
    ];

    let mut seq_out      = 0u64;
    let mut child_status = None;
    let mut buf          = [0; TCP_LINE_BUF_SIZE];
    let mut tcp_buf      = [0; TCP_LINE_BUF_SIZE];
    let mut tcp_buf_pos  = 0;
    if let Some(data) = initial_data {
        let len = data.len().min(tcp_buf.len());
        tcp_buf[..len].copy_from_slice(&data[..len]);
        tcp_buf_pos = len;
    }

    loop {
        match poll(&mut poll_fds, PollTimeout::try_from(POLL_TIMEOUT_MS).unwrap()) {
            Ok(0) => {
                if check_child_and_drain_pty(child, &mut child_status, master_file, stream, &mut buf, &mut seq_out)? {
                    if child_status.is_some() {
                        log::debug!("execute_with_pty: child already exited, no data for 1s, breaking");
                        break;
                    }
                }
            }
            Ok(_) => {
                if handle_pty_poll_events(&poll_fds, child, master_file, stream, &mut buf, &mut tcp_buf, &mut tcp_buf_pos, &mut seq_out)? {
                    break;
                }

                if check_child_and_drain_pty(child, &mut child_status, master_file, stream, &mut buf, &mut seq_out)? {
                    continue;
                }
            }
            Err(e) => return Err(e.into()),
        }
    }

    Ok(child_status)
}

fn execute_with_pty(request: &CommandRequest, stream: &mut TcpStream, initial_data: Option<Vec<u8>>) -> Result<i32> {
    log::debug!("execute_with_pty: starting");

    let (mut master_file, child) = setup_pty_and_fork(request)?;
    let child_status = pty_poll_loop(child, &mut master_file, stream, initial_data)?;

    let status = match child_status {
        Some(s) => s,
        None    => waitpid(child, None)?,
    };
    let exit_code = exit_code_from_wait_status(status);
    log::debug!("execute_with_pty: sending exit message code={}", exit_code);
    send_exit_and_flush(stream, exit_code)
}

/// Handle stdout pipe events: read and send data, return true on EOF.
fn handle_stdout_event(
    stdout_file: &mut std::process::ChildStdout,
    stream: &mut TcpStream,
    buf: &mut [u8],
    seq_out: &mut u64,
) -> Result<bool> {
    match stdout_file.read(buf) {
        Ok(0) => {
            log::debug!("execute_without_pty: stdout EOF (non-timeout)");
            Ok(true)
        }
        Ok(n) => {
            log::debug!("execute_without_pty: read stdout {} bytes", n);
            send_stdout_chunk(stream, &buf[..n], seq_out)?;
            Ok(false)
        }
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
            log::debug!("execute_without_pty: stdout WouldBlock (unexpected)");
            Ok(false)
        }
        Err(e) => Err(e.into()),
    }
}

/// Handle stderr pipe events: read and send data, return true on EOF.
fn handle_stderr_event(
    stderr_file: &mut std::process::ChildStderr,
    stream: &mut TcpStream,
    buf: &mut [u8],
    seq_err: &mut u64,
) -> Result<bool> {
    match stderr_file.read(buf) {
        Ok(0) => {
            log::debug!("execute_without_pty: stderr EOF (non-timeout)");
            Ok(true)
        }
        Ok(n) => {
            log::debug!("execute_without_pty: read stderr {} bytes", n);
            send_stderr_chunk(stream, &buf[..n], seq_err)?;
            Ok(false)
        }
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
            log::debug!("execute_without_pty: stderr WouldBlock (unexpected)");
            Ok(false)
        }
        Err(e) => Err(e.into()),
    }
}

/// Handle poll events for non-PTY mode: process stdout/stderr/stdin. Returns (should_break, stdin_eof_received).
fn handle_nonpty_poll_events<W: std::io::Write>(
    poll_fds: &[PollFd],
    stdout_file: &mut std::process::ChildStdout,
    stderr_file: &mut std::process::ChildStderr,
    stdin_file: &mut Option<W>,
    stream: &mut TcpStream,
    buf: &mut [u8],
    tcp_buf: &mut [u8],
    tcp_buf_pos: &mut usize,
    seq_out: &mut u64,
    seq_err: &mut u64,
) -> Result<(bool, bool)> {
    if poll_fds[0].revents().unwrap().contains(PollFlags::POLLIN) {
        if handle_stdout_event(stdout_file, stream, buf, seq_out)? {
            return Ok((true, false));
        }
    }

    if poll_fds[1].revents().unwrap().contains(PollFlags::POLLIN) {
        if handle_stderr_event(stderr_file, stream, buf, seq_err)? {
            return Ok((true, false));
        }
    }

    if poll_fds[2].revents().unwrap().contains(PollFlags::POLLIN) {
        let (tcp_eof, stdin_eof) = handle_nonpty_tcp_input(stream, stdin_file, tcp_buf, tcp_buf_pos)?;
        if tcp_eof {
            return Ok((true, false));
        }
        if stdin_eof {
            return Ok((false, true));
        }
    }

    Ok((false, false))
}

/// Poll loop for non-PTY mode: forward pipe output to TCP, TCP input to stdin, check child status.
fn nonpty_poll_loop<W: std::io::Write>(
    child_pid: Pid,
    stdout_file: &mut std::process::ChildStdout,
    stderr_file: &mut std::process::ChildStderr,
    mut stdin_file: Option<W>,
    stream: &mut TcpStream,
    initial_data: Option<Vec<u8>>,
) -> Result<Option<nix::sys::wait::WaitStatus>> {
    use std::os::fd::{AsRawFd, BorrowedFd};

    let stdout_fd = unsafe { BorrowedFd::borrow_raw(stdout_file.as_raw_fd()) };
    let stderr_fd = unsafe { BorrowedFd::borrow_raw(stderr_file.as_raw_fd()) };
    let stream_fd = unsafe { BorrowedFd::borrow_raw(stream.as_raw_fd()) };

    let mut poll_fds = vec![
        PollFd::new(stdout_fd, PollFlags::POLLIN),
        PollFd::new(stderr_fd, PollFlags::POLLIN),
        PollFd::new(stream_fd, PollFlags::POLLIN),
    ];

    let mut seq_out     = 0u64;
    let mut seq_err     = 0u64;
    let mut buf         = [0; TCP_LINE_BUF_SIZE];
    let mut tcp_buf     = [0; TCP_LINE_BUF_SIZE];
    let mut tcp_buf_pos = 0;
    if let Some(data) = initial_data {
        let len = data.len().min(tcp_buf.len());
        tcp_buf[..len].copy_from_slice(&data[..len]);
        tcp_buf_pos = len;
    }

    let mut child_status = None;
    loop {
        if check_child_status(child_pid, &mut child_status) {
            log::debug!("execute_without_pty: child already exited, draining pipes");
            drain_pipes(stdout_file, stderr_file, stream, &mut buf, &mut seq_out, &mut seq_err)?;
            break;
        }

        match poll(&mut poll_fds, PollTimeout::try_from(POLL_TIMEOUT_MS).unwrap()) {
            Ok(0) => {
                if check_child_status(child_pid, &mut child_status) {
                    log::debug!("execute_without_pty: child exited, draining pipes");
                    drain_pipes(stdout_file, stderr_file, stream, &mut buf, &mut seq_out, &mut seq_err)?;
                    break;
                } else {
                    log::trace!("execute_without_pty: child still alive");
                }
            }
            Ok(_) => {
                let (should_break, stdin_eof) = handle_nonpty_poll_events(
                    &poll_fds, stdout_file, stderr_file, &mut stdin_file, stream,
                    &mut buf, &mut tcp_buf, &mut tcp_buf_pos, &mut seq_out, &mut seq_err
                )?;

                // If stdin EOF received, drop stdin_file to close the pipe
                if stdin_eof {
                    log::debug!("execute_without_pty: stdin EOF received, closing stdin pipe");
                    stdin_file = None;
                }

                if should_break {
                    break;
                }

                if check_child_status(child_pid, &mut child_status) {
                    log::debug!("execute_without_pty: child exited (post-event check)");
                    drain_pipes(stdout_file, stderr_file, stream, &mut buf, &mut seq_out, &mut seq_err)?;
                    break;
                }
            }
            Err(e) => return Err(e.into()),
        }
    }

    Ok(child_status)
}

fn execute_without_pty(request: &CommandRequest, stream: &mut TcpStream, initial_data: Option<Vec<u8>>) -> Result<i32> {
    log::debug!("execute_without_pty: starting");

    match spawn_child_piped(request)? {
        SpawnOutcome::SpawnFailed { error_msg } => {
            // Send error as stderr chunk and exit message
            send_stderr_chunk(stream, error_msg.as_bytes(), &mut 0)?;
            send_exit_and_flush(stream, -1)?;
            Ok(-1)
        }
        SpawnOutcome::Spawned(child, stdin_pipe, stdout_pipe, stderr_pipe) => {
            let mut stdin_file  = Some(stdin_pipe);
            let mut stdout_file = stdout_pipe;
            let mut stderr_file = stderr_pipe;

            // Write request.stdin field if provided
            if !request.stdin.is_empty() {
                if let Some(ref mut stdin) = stdin_file {
                    stdin.write_all(request.stdin.as_bytes())?;
                }
            }

            // Process initial data (leftover bytes from request parsing) as JSON messages
            // These may contain stdin messages that need to be decoded and forwarded
            if let Some(data) = initial_data {
                if !data.is_empty() {
                    log::debug!("execute_without_pty: processing initial data ({} bytes)", data.len());
                    let mut tcp_buf = [0u8; TCP_LINE_BUF_SIZE];
                    let len = data.len().min(tcp_buf.len());
                    tcp_buf[..len].copy_from_slice(&data[..len]);
                    let mut tcp_buf_pos = len;

                    // Process stdin messages in the initial data
                    process_tcp_line_buffer(&mut tcp_buf, &mut tcp_buf_pos, |msg| {
                        match msg {
                            StreamMessage::Stdin { data, .. } => {
                                if let Some(ref mut stdin) = stdin_file {
                                    let bytes = STANDARD.decode(&data)?;
                                    stdin.write_all(&bytes)?;
                                    log::debug!("execute_without_pty: wrote {} bytes to stdin from initial_data", bytes.len());
                                }
                                Ok(())
                            }
                            StreamMessage::StdinEof { .. } => {
                                log::debug!("execute_without_pty: stdin EOF in initial_data, closing stdin pipe");
                                stdin_file = None;
                                Ok(())
                            }
                            _ => Ok(())
                        }
                    })?;
                }
            }

            let child_pid    = Pid::from_raw(child.id() as i32);
            let child_status = nonpty_poll_loop(child_pid, &mut stdout_file, &mut stderr_file, stdin_file, stream, None)?;

            let status = match child_status {
                Some(s) => s,
                None    => waitpid(Some(child_pid), None)?,
            };
            let exit_code = exit_code_from_wait_status(status);
            log::debug!("execute_without_pty: sending exit message code={}", exit_code);
            send_exit_and_flush(stream, exit_code)
        }
    }
}

/// Execute command in batch mode: collect all output and return in single JSON response.
fn execute_batch(request: &CommandRequest, stream: &mut TcpStream) -> Result<i32> {
    log::debug!("execute_batch: starting");
    let _ = kmsg_write("<6>execute_batch: starting\n");

    // Debug file write for visibility from host
    let debug_file_write = |msg: &str| {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/opt/epkg/guest-debug.log") {
            let _ = write!(f, "{}", msg);
        }
    };

    debug_file_write(&format!("execute_batch: starting, command={:?}\n", request.command));

    match spawn_child_piped(request)? {
        SpawnOutcome::SpawnFailed { error_msg } => {
            debug_file_write(&format!("execute_batch: spawn FAILED: {}\n", error_msg));
            log::debug!("execute_batch: spawn failed: {}", error_msg);

            // Send batch response with error
            let response = serde_json::json!({
                "exit_code": -1,
                "stdout": "",
                "stderr": STANDARD.encode(error_msg.as_bytes()),
            });
            let json = serde_json::to_string(&response)?;
            debug_file_write(&format!("execute_batch: error response JSON size={} bytes\n", json.len()));
            stream.write_all(json.as_bytes())?;
            stream.write_all(b"\n")?;
            stream.flush()?;
            debug_file_write("execute_batch: error response written and flushed\n");

            // Shutdown the write side to signal EOF to host
            let _ = stream.shutdown(std::net::Shutdown::Write);
            debug_file_write("execute_batch: done (error case)\n");
            Ok(-1)
        }
        SpawnOutcome::Spawned(child, stdin_pipe, stdout_pipe, stderr_pipe) => {
            debug_file_write(&format!("execute_batch: child spawned, pid={}\n", child.id()));

            // Write stdin if provided, then close it
            if !request.stdin.is_empty() {
                use std::io::Write;
                let mut stdin_ref = &stdin_pipe;
                stdin_ref.write_all(request.stdin.as_bytes())?;
            }
            drop(stdin_pipe);

            // Wait for child to complete
            let child_pid = Pid::from_raw(child.id() as i32);
            debug_file_write("execute_batch: waiting for child to complete\n");
            let status = waitpid(child_pid, None)?;
            let exit_code = exit_code_from_wait_status(status);

            debug_file_write(&format!("execute_batch: child exited with code={}, collecting output\n", exit_code));

            // Collect all output
            use std::io::Read;
            let mut stdout_data = Vec::new();
            let mut stderr_data = Vec::new();
            let mut stdout_pipe = stdout_pipe;
            let mut stderr_pipe = stderr_pipe;
            stdout_pipe.read_to_end(&mut stdout_data)?;
            stderr_pipe.read_to_end(&mut stderr_data)?;

            log::debug!("execute_batch: exit_code={}, stdout={} bytes, stderr={} bytes",
                        exit_code, stdout_data.len(), stderr_data.len());
            debug_file_write(&format!("execute_batch: stdout={} bytes, stderr={} bytes\n", stdout_data.len(), stderr_data.len()));

            log::debug!("execute_batch: sending response (exit_code={})", exit_code);
            let _ = kmsg_write(&format!("<6>execute_batch: sending response (exit_code={})\n", exit_code));
            debug_file_write("execute_batch: sending response JSON\n");

            // Send batch response
            let response = serde_json::json!({
                "exit_code": exit_code,
                "stdout": STANDARD.encode(&stdout_data),
                "stderr": STANDARD.encode(&stderr_data),
            });
            let json = serde_json::to_string(&response)?;
            debug_file_write(&format!("execute_batch: response JSON size={} bytes\n", json.len()));
            stream.write_all(json.as_bytes())?;
            stream.write_all(b"\n")?;
            stream.flush()?;
            debug_file_write("execute_batch: response written and flushed\n");

            // CRITICAL: Shutdown the write side to signal EOF to host
            // This ensures the host's read_to_string() returns
            let _ = kmsg_write("<6>execute_batch: shutting down stream write side\n");
            debug_file_write("execute_batch: shutting down stream write side\n");
            let _ = stream.shutdown(std::net::Shutdown::Write);

            let _ = kmsg_write("<6>execute_batch: response sent, returning\n");
            debug_file_write("execute_batch: done\n");
            Ok(exit_code)
        }
    }
}

fn handle_connection(mut stream: TcpStream) -> Result<ConnectionDisposition> {
    log::debug!("[vm_daemon] Connection accepted, starting handle_connection");
    let _ = kmsg_write("<6>handle_connection: handle_connection started\n");

    // Debug file write for visibility from host (with timestamp)
    let debug_file_write = |msg: &str| {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/opt/epkg/guest-debug.log") {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            let secs = now.as_secs() % 86400;
            let hours = secs / 3600;
            let mins = (secs % 3600) / 60;
            let secs = secs % 60;
            let millis = now.subsec_millis();
            let _ = write!(f, "{:02}:{:02}:{:02}.{:03} {}", hours, mins, secs, millis, msg);
        }
    };

    let _func_start = std::time::Instant::now();
    debug_file_write("[PERF] handle_connection: started\n");
    log::debug!("handle_connection: new connection");
    let _ = kmsg_write("<6>handle_connection: new connection\n");
    log_process_identity("vm-daemon handle_connection");

    let mut buf = [0; TCP_LINE_BUF_SIZE];
    const MAX_WAIT_MS: i32 = 5000; // 5 seconds total timeout

    let _ = kmsg_write("<6>handle_connection: about to read from stream (using poll)\n");
    debug_file_write("handle_connection: waiting to read from stream...\n");

    // Use poll() for efficient blocking wait instead of sleep-based retry
    let stream_fd = stream.as_raw_fd();
    let start = std::time::Instant::now();

    loop {
        // Calculate remaining timeout
        let elapsed_ms = start.elapsed().as_millis() as i32;
        let remaining_ms = MAX_WAIT_MS.saturating_sub(elapsed_ms);
        if remaining_ms <= 0 {
            let _ = kmsg_write(&format!("<6>handle_connection: poll timeout after {}ms\n", elapsed_ms));
            debug_file_write(&format!("handle_connection: poll TIMEOUT after {}ms\n", elapsed_ms));
            return Ok(ConnectionDisposition::Shutdown);
        }

        // Poll for data availability
        let mut poll_fds = [libc::pollfd {
            fd: stream_fd,
            events: libc::POLLIN,
            revents: 0,
        }];

        let poll_result = unsafe { libc::poll(poll_fds.as_mut_ptr(), 1, remaining_ms) };

        match poll_result {
            0 => {
                // Poll timeout
                let _ = kmsg_write(&format!("<6>handle_connection: poll timeout\n"));
                debug_file_write("handle_connection: poll timeout\n");
                return Ok(ConnectionDisposition::Shutdown);
            }
            n if n < 0 => {
                let errno = std::io::Error::last_os_error();
                if errno.raw_os_error() == Some(libc::EINTR) {
                    // Interrupted by signal, retry
                    continue;
                }
                let _ = kmsg_write(&format!("<3>handle_connection: poll error: {}\n", errno));
                return Err(eyre!("Poll error: {}", errno));
            }
            _ => {
                // Data available, try to read
                match stream.read(&mut buf) {
                    Ok(0) => {
                        // EOF - peer closed connection
                        let _ = kmsg_write("<6>handle_connection: read EOF (peer closed)\n");
                        debug_file_write("handle_connection: read EOF\n");
                        return Ok(ConnectionDisposition::Shutdown);
                    }
                    Ok(n) => {
                        let elapsed_ms = start.elapsed().as_millis();
                        let _ = kmsg_write(&format!("<6>handle_connection: read {} bytes after {}ms\n", n, elapsed_ms));
                        // Log first few bytes for debugging
                        let preview = String::from_utf8_lossy(&buf[..n.min(100)]);
                        let _ = kmsg_write(&format!("<6>handle_connection: data preview: {:?}\n", preview));
                        debug_file_write(&format!("handle_connection: read {} bytes: {:?}\n", n, preview));
                        // Find first newline to separate request from extra messages
                        let mut split_at = n;
                        for i in 0..n {
                            if buf[i] == b'\n' {
                                split_at = i;
                                break;
                            }
                        }
                        let request_slice = &buf[..split_at];
                        let leftover = if split_at < n {
                            &buf[split_at + 1..n]
                        } else {
                            &buf[n..n] // empty
                        };
                        let input = String::from_utf8_lossy(request_slice).trim().to_string();
                        let leftover_bytes = leftover.to_vec();

                        log::debug!("Request line ({} bytes): {:?}", request_slice.len(), input);
                        log::debug!("Leftover bytes ({} bytes): {:?}", leftover_bytes.len(), String::from_utf8_lossy(&leftover_bytes));

                        debug_file_write(&format!("handle_connection: parsing JSON: {:?}\n", input));

                        // Parse JSON request (no plain text fallback)
                        let request: CommandRequest = serde_json::from_str(&input)
                            .map_err(|e| {
                                debug_file_write(&format!("handle_connection: JSON parse FAILED: {}\n", e));
                                eyre!("JSON parse failed: {} (input: {:?})", e, input)
                            })?;
                        log::debug!("[vm_daemon] Command received: {:?}", request.command);
                        debug_file_write(&format!("handle_connection: parsed command: {:?}, batch={}\n", request.command, request.batch));

                        if request.command.len() == 1 && request.command[0] == VM_SESSION_DONE_CMD {
                            log::debug!(
                                "vm session done command ({VM_SESSION_DONE_CMD}): host finished install/upgrade"
                            );
                            return Ok(ConnectionDisposition::Shutdown);
                        }

                        if request.command.is_empty() {
                            return Err(eyre!(
                                "empty command (send command [\"{0}\"] to end reuse session)",
                                VM_SESSION_DONE_CMD
                            ));
                        }

                        log::debug!("Received command: {:?}", request.command);
                        log::debug!("[vm_daemon] Executing command: {:?}", request.command);
                        log_process_identity("vm-daemon before command dispatch");
                        debug_file_write(&format!("handle_connection: executing command: {:?}\n", request.command));

                        // Execute command
                        let exit_code;
                        if request.batch {
                            log::debug!("Using batch mode");
                            debug_file_write("handle_connection: using batch mode\n");
                            exit_code = execute_batch(&request, &mut stream)?;
                        } else if request.pty {
                            log::debug!("Using PTY mode");
                            debug_file_write("handle_connection: using PTY mode\n");
                            exit_code = execute_with_pty(&request, &mut stream, Some(leftover_bytes))?;
                        } else {
                            log::debug!("Using non-PTY mode");
                            debug_file_write("handle_connection: using non-PTY mode\n");
                            exit_code = execute_without_pty(&request, &mut stream, Some(leftover_bytes))?;
                        }
                        log::debug!("[vm_daemon] Command completed with exit code: {}", exit_code);
                        debug_file_write(&format!("handle_connection: command completed with exit_code={}\n", exit_code));

                        if request.reuse_vm {
                            // timeout=0 means "never timeout" - use ~1 year in ms
                            let idle_ms = request
                                .vm_keep_timeout_secs
                                .map(|s| if s == 0 { u32::MAX } else { s.saturating_mul(1000) })
                                .unwrap_or(VM_REUSE_IDLE_TIMEOUT_MS);
                            let _ = kmsg_write("<6>handle_connection: reuse_vm=true, returning ReuseWait\n");
                            return Ok(ConnectionDisposition::ReuseWait {
                                idle_timeout_ms: idle_ms,
                            });
                        } else {
                            let _ = kmsg_write("<6>handle_connection: reuse_vm=false, returning Shutdown\n");
                            return Ok(ConnectionDisposition::Shutdown);
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        // Poll said data was ready but read returned WouldBlock
                        // This is rare but possible; retry the poll loop
                        log::trace!("handle_connection: spurious WouldBlock after poll, retrying");
                        continue;
                    }
                    Err(e) => {
                        let _ = kmsg_write(&format!("<3>handle_connection: read error: {}\n", e));
                        return Err(e.into());
                    }
                }
            }
        }
    }
}

/// Poll the listen socket for readability, then accept. Returns `None` on idle timeout.
#[cfg(target_os = "linux")]
fn accept_vsock_with_timeout(raw_fd: RawFd, timeout_ms: u32) -> Result<Option<RawFd>> {
    let borrowed = unsafe { BorrowedFd::borrow_raw(raw_fd) };
    let mut poll_fds = [PollFd::new(borrowed, PollFlags::POLLIN)];
    match poll(&mut poll_fds, PollTimeout::try_from(timeout_ms).unwrap()) {
        Ok(0) => Ok(None),
        Ok(_) => {
            let client_fd = socket::accept(raw_fd)
                .map_err(|e| eyre!("vsock accept failed: {}", e))?;
            Ok(Some(client_fd))
        }
        Err(e) => Err(e.into()),
    }
}

#[cfg(target_os = "linux")]
fn run_vsock_server() -> Result<()> {
    use std::os::fd::FromRawFd;
    use std::io::Write;

    let kmsg_write = |msg: &str| {
        if let Ok(mut kmsg) = std::fs::OpenOptions::new().write(true).open("/dev/kmsg") {
            let _ = write!(kmsg, "{}", msg);
        }
    };

    kmsg_write("<6>run_vsock_server: start\n");
    log_process_identity("vm-daemon run_vsock_server start");

    // Fixed vsock ports matching host/client side.
    const VSOCK_PORT: u32 = 10000;      // Command port
    const READY_PORT: u32 = 10001;      // Ready notification port

    // Step 1: Create and bind command port FIRST, before signaling readiness.
    // This prevents race condition where host connects before we're listening.
    log::debug!("vm-daemon: creating vsock socket for command port...");
    kmsg_write("<6>run_vsock_server: creating socket\n");
    let fd = socket::socket(
        AddressFamily::Vsock,
        SockType::Stream,
        SockFlag::SOCK_CLOEXEC,
        None,
    ).map_err(|e| { log::debug!("vm-daemon: socket() failed: {}", e); e })?;
    kmsg_write("<6>run_vsock_server: socket created\n");
    log::debug!("vm-daemon: socket created, fd={}", fd.as_raw_fd());

    let addr = VsockAddr::new(libc::VMADDR_CID_ANY, VSOCK_PORT);
    let raw_fd = fd.as_raw_fd();

    log::debug!("vm-daemon: binding to cid=ANY port={}...", VSOCK_PORT);
    kmsg_write("<6>run_vsock_server: binding\n");
    socket::bind(raw_fd, &addr).map_err(|e| { log::debug!("vm-daemon: bind() failed: {}", e); e })?;
    kmsg_write("<6>run_vsock_server: bind ok\n");
    log::debug!("vm-daemon: bind succeeded");

    log::debug!("vm-daemon: calling listen()...");
    kmsg_write("<6>run_vsock_server: listening\n");
    socket::listen(&fd, Backlog::new(8)?).map_err(|e| { log::debug!("vm-daemon: listen() failed: {}", e); e })?;
    kmsg_write("<6>run_vsock_server: listen ok\n");
    log::debug!("vm-daemon: listen succeeded");

    // Step 2: Notify host that we're ready to accept commands.
    // Now that command port is bound and listening, signal readiness.
    // Connect to ready port on host (CID=VMADDR_CID_HOST=2).
    log::debug!("vm-daemon: notifying host that we're ready...");
    kmsg_write("<6>run_vsock_server: creating ready socket\n");
    let ready_fd = socket::socket(
        AddressFamily::Vsock,
        SockType::Stream,
        SockFlag::SOCK_CLOEXEC,
        None,
    ).map_err(|e| { log::debug!("vm-daemon: ready socket() failed: {}", e); e })?;
    let ready_addr = VsockAddr::new(libc::VMADDR_CID_HOST, READY_PORT);
    kmsg_write("<6>run_vsock_server: connecting to ready port\n");
    match socket::connect(ready_fd.as_raw_fd(), &ready_addr) {
        Ok(_) => {
            kmsg_write("<6>run_vsock_server: ready connect ok\n");
            log::debug!("vm-daemon: connected to ready port {}, host knows we're ready", READY_PORT);
        }
        Err(e) => {
            kmsg_write(&format!("<6>run_vsock_server: ready connect failed: {}\n", e));
            // Ready port connection is best-effort (may not be configured for QEMU mode)
            log::debug!("vm-daemon: ready port connection failed (non-fatal): {}", e);
        }
    }
    // Close ready socket - the connection itself is the signal
    drop(ready_fd);

    log::debug!("vm-daemon starting (vsock), listening on port {}", VSOCK_PORT);
    kmsg_write("<6>run_vsock_server: entering accept loop\n");

    let mut next_idle_timeout_ms = VM_REUSE_IDLE_TIMEOUT_MS;
    let mut first_accept = true;
    loop {
        let client_fd = if first_accept {
            first_accept = false;
            log::debug!("vm-daemon: calling accept()...");
            kmsg_write("<6>run_vsock_server: about to call accept (blocking)\n");
            log::debug!("vm-daemon: calling accept()...");
            let fd = match socket::accept(raw_fd) {
                Ok(fd) => {
                    kmsg_write(&format!("<6>run_vsock_server: accept returned fd={}\n", fd));
                    fd
                }
                Err(e) => {
                    kmsg_write(&format!("<3>run_vsock_server: accept FAILED: {} (errno={})\n", e, e as i32));
                    return Err(eyre!("vsock accept failed: {}", e));
                }
            };
            fd
        } else {
            log::debug!(
                "vm-daemon: waiting for next connection (reuse, {} ms)...",
                next_idle_timeout_ms
            );
            match accept_vsock_with_timeout(raw_fd, next_idle_timeout_ms)? {
                Some(fd) => fd,
                None => {
                    log::debug!("vm-daemon: idle timeout, powering off guest");
                    break;
                }
            }
        };

        log::debug!("vm-daemon: accept() succeeded, fd={}", client_fd);
        kmsg_write("<6>run_vsock_server: creating TcpStream from fd\n");
        let stream = unsafe { TcpStream::from_raw_fd(client_fd) };
        log::debug!("vm-daemon vsock: accepted connection");

        kmsg_write("<6>run_vsock_server: about to call handle_connection\n");
        let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            handle_connection(stream)
        })) {
            Ok(r) => {
                kmsg_write("<6>run_vsock_server: handle_connection returned\n");
                r
            }
            Err(_) => {
                kmsg_write("<3>run_vsock_server: handle_connection PANICKED!\n");
                Err(eyre!("handle_connection panicked"))
            }
        };
        match result {
            Ok(ConnectionDisposition::Shutdown) => {
                kmsg_write("<6>run_vsock_server: handle_connection returned Shutdown, breaking loop\n");
                log::debug!("vm-daemon: connection closed, powering off guest (vsock)");
                break;
            }
            Ok(ConnectionDisposition::ReuseWait { idle_timeout_ms }) => {
                next_idle_timeout_ms = idle_timeout_ms;
                log::debug!(
                    "vm-daemon: reuse_vm — next idle window {} ms",
                    next_idle_timeout_ms
                );
                continue;
            }
            Err(e) => {
                let _ = kmsg_write(&format!("<3>run_vsock_server: handle_connection ERROR: {}\n", e));
                log::debug!("Error handling vsock connection: {}", e);
                return Err(e);
            }
        }
    }

    let _ = kmsg_write("<6>run_vsock_server: loop exited normally\n");
    Ok(())
}

pub fn run(options: VmDaemonOptions) -> Result<()> {
    // vm-daemon always uses vsock for control plane
    #[cfg(target_os = "linux")]
    {
        if options.reverse_mode {
            let _ = kmsg_write("<6>vm_daemon::run: calling run_reverse_vsock_client\n");
            let result = run_reverse_vsock_client();
            match &result {
                Ok(_) => { let _ = kmsg_write("<6>vm_daemon::run: run_reverse_vsock_client returned Ok\n"); }
                Err(e) => { let _ = kmsg_write(&format!("<3>vm_daemon::run: run_reverse_vsock_client returned Err: {}\n", e)); }
            }
            return result;
        } else {
            let _ = kmsg_write("<6>vm_daemon::run: calling run_vsock_server\n");
            let result = run_vsock_server();
            match &result {
                Ok(_) => { let _ = kmsg_write("<6>vm_daemon::run: run_vsock_server returned Ok\n"); }
                Err(e) => { let _ = kmsg_write(&format!("<3>vm_daemon::run: run_vsock_server returned Err: {}\n", e)); }
            }
            return result;
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        return Err(eyre!("vm-daemon vsock mode not supported on this platform"));
    }
}

/// Run in reverse vsock mode: Guest connects to Host.
/// This avoids vsock handshake timing issues on Windows/WHPX.
#[cfg(target_os = "linux")]
fn run_reverse_vsock_client() -> Result<()> {
    use nix::sys::socket::{connect, socket, AddressFamily, SockFlag, SockType, VsockAddr};
    use std::io::Write;
    use std::time::Duration;

    const HOST_CID: u32 = libc::VMADDR_CID_HOST;  // CID 2 = Host
    const HOST_PORT: u32 = 10000;  // Command port (Host is listening)
    const CONNECT_RETRY_MAX: u32 = 60;  // Increased from 30 for slower systems
    const CONNECT_RETRY_DELAY_MS: u64 = 50;  // Initial delay
    const CONNECT_RETRY_DELAY_MAX_MS: u64 = 500;  // Max delay with exponential backoff

    let kmsg_write = |msg: &str| {
        if let Ok(mut kmsg) = std::fs::OpenOptions::new().write(true).open("/dev/kmsg") {
            let _ = write!(kmsg, "{}", msg);
        }
    };

    // Also write to virtiofs-mounted file for visibility from host
    let debug_file_write = |msg: &str| {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/opt/epkg/guest-debug.log") {
            // Add timestamp HH:MM:SS.mmm
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            let secs = now.as_secs() % 86400;
            let hours = secs / 3600;
            let mins = (secs % 3600) / 60;
            let secs = secs % 60;
            let millis = now.subsec_millis();
            let _ = write!(f, "{:02}:{:02}:{:02}.{:03} {}", hours, mins, secs, millis, msg);
        }
    };

    kmsg_write("<6>run_reverse_vsock_client: starting\n");
    debug_file_write("[PERF] === REVERSE_VSOCK_CLIENT START ===\n");
    let total_start = std::time::Instant::now();
    log::debug!("vm-daemon: reverse mode - connecting to Host...");

    // Create vsock socket
    let fd = socket(
        AddressFamily::Vsock,
        SockType::Stream,
        SockFlag::SOCK_CLOEXEC,
        None,
    ).map_err(|e| { log::debug!("vm-daemon: reverse socket() failed: {}", e); e })?;
    kmsg_write("<6>run_reverse_vsock_client: socket created\n");
    debug_file_write("run_reverse_vsock_client: socket created\n");

    let host_addr = VsockAddr::new(HOST_CID, HOST_PORT);

    // Connect to Host with retry
    log::debug!("vm-daemon: connecting to Host CID={} PORT={}...", HOST_CID, HOST_PORT);
    kmsg_write("<6>run_reverse_vsock_client: connecting to host\n");
    debug_file_write(&format!("run_reverse_vsock_client: connecting to CID={} PORT={}\n", HOST_CID, HOST_PORT));
    log::debug!("vm-daemon: attempting to connect to host CID={} PORT={}", HOST_CID, HOST_PORT);

    let mut retry_count = 0;
    let mut retry_delay_ms = CONNECT_RETRY_DELAY_MS;
    let connect_start = std::time::Instant::now();
    let stream = loop {
        match connect(fd.as_raw_fd(), &host_addr) {
            Ok(_) => {
                kmsg_write("<6>run_reverse_vsock_client: connected to host\n");
                debug_file_write(&format!("[PERF] vsock connect took {:.3}ms\n", connect_start.elapsed().as_secs_f64() * 1000.0));
                log::debug!("vm-daemon: connected to Host");
                break unsafe { std::net::TcpStream::from_raw_fd(fd.into_raw_fd()) };
            }
            Err(e) => {
                retry_count += 1;
                if retry_count >= CONNECT_RETRY_MAX {
                    kmsg_write(&format!("<3>run_reverse_vsock_client: connect failed after {} retries: {}\n", retry_count, e));
                    debug_file_write(&format!("run_reverse_vsock_client: connect FAILED after {} retries: {}\n", retry_count, e));
                    return Err(eyre!("Failed to connect to Host after {} retries: {}", retry_count, e));
                }
                log::debug!("vm-daemon: connect retry {}/{} (delay {}ms): {}", retry_count, CONNECT_RETRY_MAX, retry_delay_ms, e);
                std::thread::sleep(Duration::from_millis(retry_delay_ms));
                // Exponential backoff with jitter
                retry_delay_ms = (retry_delay_ms * 2).min(CONNECT_RETRY_DELAY_MAX_MS);
            }
        }
    };

    // Send ready signal to Host
    kmsg_write("<6>run_reverse_vsock_client: sending READY\n");
    debug_file_write("[PERF] sending READY signal to Host\n");
    log::debug!("vm-daemon: sending READY signal to Host");
    let ready_start = std::time::Instant::now();
    let mut stream = stream;
    stream.write_all(b"READY\n")?;
    stream.flush()?;
    debug_file_write(&format!("[PERF] READY sent and flushed ({:.3}ms)\n", ready_start.elapsed().as_secs_f64() * 1000.0));

    kmsg_write("<6>run_reverse_vsock_client: entering handle_connection\n");
    debug_file_write(&format!("[PERF] entering handle_connection (total so far: {:.3}ms)\n", total_start.elapsed().as_secs_f64() * 1000.0));
    log::debug!("vm-daemon: handling connection in reverse mode");

    // Handle the connection (same as forward mode)
    match handle_connection(stream) {
        Ok(disposition) => {
            kmsg_write(&format!("<6>run_reverse_vsock_client: handle_connection returned {:?}\n", disposition));
            log::debug!("vm-daemon: handle_connection returned {:?}", disposition);
            match disposition {
                ConnectionDisposition::Shutdown => {
                    kmsg_write("<6>run_reverse_vsock_client: shutdown requested, exiting\n");
                    log::debug!("vm-daemon: shutdown requested in reverse mode");
                    return Ok(());
                }
                ConnectionDisposition::ReuseWait { idle_timeout_ms } => {
                    kmsg_write("<6>run_reverse_vsock_client: switching to forward mode for reuse\n");
                    // Switch to forward mode for reuse: become listener, allow any host process to connect.
                    // This enables cross-process VM reuse - the session file points to our socket.
                    log::info!("vm-daemon: switching to forward mode for reuse (cross-process capable)");
                    let _ = idle_timeout_ms;
                    run_vsock_server()
                }
            }
        }
        Err(e) => {
            kmsg_write(&format!("<3>run_reverse_vsock_client: handle_connection error: {}\n", e));
            Err(e)
        }
    }
}

/// Inner function for reverse mode: connect to Host and handle one command.
/// Called recursively for VM reuse sessions.
#[allow(dead_code)]
fn connect_and_handle_reverse(_idle_timeout_ms: u32) -> Result<()> {
    use nix::sys::socket::{connect, socket, AddressFamily, SockType, SockFlag, VsockAddr};
    use std::os::fd::IntoRawFd;
    use std::io::Write;
    use std::time::Duration;

    let kmsg_write = |msg: &str| {
        if let Ok(mut kmsg) = std::fs::OpenOptions::new().write(true).open("/dev/kmsg") {
            let _ = write!(kmsg, "{}", msg);
        }
    };

    const HOST_CID: u32 = libc::VMADDR_CID_HOST;  // 2
    const HOST_PORT: u32 = 10000;
    const CONNECT_RETRY_DELAY_MS: u64 = 10;
    const CONNECT_RETRY_DELAY_MAX_MS: u64 = 1000;
    const CONNECT_RETRY_MAX: usize = 100;

    kmsg_write("<6>connect_and_handle_reverse: connecting to host\n");
    log::debug!("vm-daemon: reconnecting to Host for reuse...");

    // Create new vsock socket
    let fd = socket(
        AddressFamily::Vsock,
        SockType::Stream,
        SockFlag::SOCK_CLOEXEC,
        None,
    ).map_err(|e| { log::debug!("vm-daemon: reverse socket() failed: {}", e); e })?;

    let host_addr = VsockAddr::new(HOST_CID, HOST_PORT);

    // Connect to Host with retry
    let mut retry_count = 0;
    let mut retry_delay_ms = CONNECT_RETRY_DELAY_MS;
    let stream = loop {
        match connect(fd.as_raw_fd(), &host_addr) {
            Ok(_) => {
                kmsg_write("<6>connect_and_handle_reverse: connected to host\n");
                log::debug!("vm-daemon: reconnected to Host");
                break unsafe { std::net::TcpStream::from_raw_fd(fd.into_raw_fd()) };
            }
            Err(e) => {
                retry_count += 1;
                if retry_count >= CONNECT_RETRY_MAX {
                    kmsg_write(&format!("<3>connect_and_handle_reverse: connect failed after {} retries: {}\n", retry_count, e));
                    return Err(eyre!("Failed to connect to Host after {} retries: {}", retry_count, e));
                }
                std::thread::sleep(Duration::from_millis(retry_delay_ms));
                retry_delay_ms = (retry_delay_ms * 2).min(CONNECT_RETRY_DELAY_MAX_MS);
            }
        }
    };

    // Send ready signal
    let mut stream = stream;
    stream.write_all(b"READY\n")?;
    stream.flush()?;
    kmsg_write("<6>connect_and_handle_reverse: READY sent\n");

    // Handle the connection
    match handle_connection(stream) {
        Ok(disposition) => {
            match disposition {
                ConnectionDisposition::Shutdown => {
                    kmsg_write("<6>connect_and_handle_reverse: shutdown requested\n");
                    Ok(())
                }
                ConnectionDisposition::ReuseWait { idle_timeout_ms } => {
                    kmsg_write("<6>connect_and_handle_reverse: reuse wait, looping\n");
                    std::thread::sleep(Duration::from_millis(100));
                    connect_and_handle_reverse(idle_timeout_ms)
                }
            }
        }
        Err(e) => {
            kmsg_write(&format!("<3>connect_and_handle_reverse: error: {}\n", e));
            Err(e)
        }
    }
}
