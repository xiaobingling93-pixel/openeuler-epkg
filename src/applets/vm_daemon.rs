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
//! # Protocol Design
//! ## Connection Model
//! - Single connection per VM lifetime: daemon accepts exactly one TCP connection
//! - After processing the command, the daemon powers off the guest (graceful shutdown)
//! - This matches the "one command per VM invocation" semantic of `epkg run --sandbox=vm`
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
//! epkg run --sandbox=vm [--tty] bash /
//! timeout 10 epkg run --sandbox=vm [--tty|--no-tty] ls /
//! file_list=$(timeout 10 epkg run --sandbox=vm [--no-tty] ls /)
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
//! - With debug logs (RUST_LOG=debug epkg run --sandbox=vm --tty bash):
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
use nix::unistd::{fork, ForkResult, dup2, setsid, close, Pid};
use nix::sys::reboot;
use nix::fcntl::{fcntl, FcntlArg, OFlag};
use std::os::fd::{OwnedFd, AsRawFd, BorrowedFd, FromRawFd};
use nix::sys::socket::{self, AddressFamily, SockType, SockFlag, VsockAddr, Backlog};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use crate::vm_client::StreamMessage;

/// Poll timeout in milliseconds for PTY/pipe and TCP wait.
const POLL_TIMEOUT_MS: u32 = 1000;

/// Maximum poll loop iterations in non-PTY mode before forcing exit (safety limit).
const MAX_POLL_ITERATIONS: u32 = 100;

/// TCP line buffer size (must fit at least one JSON message line).
const TCP_LINE_BUF_SIZE: usize = 4096;

/// Write a single newline-delimited JSON stream message to the TCP stream.
fn write_stream_message(stream: &mut TcpStream, msg: &StreamMessage) -> Result<()> {
    let json = serde_json::to_string(msg)?;
    stream.write_all(json.as_bytes())?;
    stream.write_all(b"\n")?;
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
pub struct VmDaemonOptions {
    pub port: u16,
    pub host: String,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<VmDaemonOptions> {
    let port = matches.get_one::<u16>("port").copied().unwrap_or(10000);
    let host = matches.get_one::<String>("host").cloned().unwrap_or_else(|| "0.0.0.0".to_string());

    Ok(VmDaemonOptions { port, host })
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
    command: Vec<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    env: std::collections::HashMap<String, String>,
    #[serde(default)]
    stdin: String,
    #[serde(default)]
    pty: bool,
    #[serde(default)]
    terminal: Option<TerminalConfig>,
}

/// Outcome of spawning the child with pipes. Either the child and its stdio pipes, or we already sent an error and exit code.
enum SpawnOutcome {
    Spawned(
        std::process::Child,
        std::process::ChildStdin,
        std::process::ChildStdout,
        std::process::ChildStderr,
    ),
    SpawnFailedExitSent(i32),
}

/// Spawn the command with piped stdin/stdout/stderr. On spawn failure, sends stderr + exit -1 to stream and returns SpawnFailedExitSent(-1).
fn spawn_child_piped(
    request: &CommandRequest,
    stream: &mut TcpStream,
) -> Result<SpawnOutcome> {
    let mut child = StdCommand::new(&request.command[0]);
    child.args(&request.command[1..]);
    if let Some(cwd) = &request.cwd {
        child.current_dir(cwd);
    }
    child.envs(&request.env);
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
            write_stream_message(stream, &StreamMessage::Stderr {
                data: STANDARD.encode(error_msg.as_bytes()),
                seq: 1,
            })?;
            write_stream_message(stream, &StreamMessage::Exit { code: -1 })?;
            stream.flush()?;
            Ok(SpawnOutcome::SpawnFailedExitSent(-1))
        }
    }
}

/// Run in the forked child: set up PTY as stdio and exec the command. Does not return on success.
fn pty_run_child(request: &CommandRequest, master: OwnedFd, slave: OwnedFd) -> ! {
    use std::os::unix::process::CommandExt;
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
    cmd.envs(&request.env);
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
fn handle_nonpty_tcp_input(
    stream: &mut TcpStream,
    stdin_file: &mut std::process::ChildStdin,
    tcp_buf: &mut [u8],
    tcp_buf_pos: &mut usize,
) -> Result<bool> {
    match stream.read(&mut tcp_buf[*tcp_buf_pos..]) {
        Ok(0) => Ok(true), // EOF from TCP
        Ok(n) => {
            *tcp_buf_pos += n;
            process_tcp_line_buffer(tcp_buf, tcp_buf_pos, |msg| {
                match msg {
                    StreamMessage::Stdin { data, .. } => {
                        let bytes = STANDARD.decode(&data)?;
                        stdin_file.write_all(&bytes)?;
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

/// Handle poll events for non-PTY mode: process stdout/stderr/stdin. Returns true if should break loop.
fn handle_nonpty_poll_events(
    poll_fds: &[PollFd],
    stdout_file: &mut std::process::ChildStdout,
    stderr_file: &mut std::process::ChildStderr,
    stdin_file: &mut std::process::ChildStdin,
    stream: &mut TcpStream,
    buf: &mut [u8],
    tcp_buf: &mut [u8],
    tcp_buf_pos: &mut usize,
    seq_out: &mut u64,
    seq_err: &mut u64,
) -> Result<bool> {
    if poll_fds[0].revents().unwrap().contains(PollFlags::POLLIN) {
        if handle_stdout_event(stdout_file, stream, buf, seq_out)? {
            return Ok(true);
        }
    }

    if poll_fds[1].revents().unwrap().contains(PollFlags::POLLIN) {
        if handle_stderr_event(stderr_file, stream, buf, seq_err)? {
            return Ok(true);
        }
    }

    if poll_fds[2].revents().unwrap().contains(PollFlags::POLLIN) {
        if handle_nonpty_tcp_input(stream, stdin_file, tcp_buf, tcp_buf_pos)? {
            return Ok(true);
        }
    }

    Ok(false)
}

/// Poll loop for non-PTY mode: forward pipe output to TCP, TCP input to stdin, check child status.
fn nonpty_poll_loop(
    child_pid: Pid,
    stdin_file: &mut std::process::ChildStdin,
    stdout_file: &mut std::process::ChildStdout,
    stderr_file: &mut std::process::ChildStderr,
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
    let mut iteration    = 0u32;

    loop {
        iteration += 1;
        if iteration > MAX_POLL_ITERATIONS {
            log::debug!("execute_without_pty: too many iterations ({}), forcing break", iteration);
            let _ = nix::sys::signal::kill(child_pid, nix::sys::signal::Signal::SIGKILL);
            break;
        }
        log::debug!("execute_without_pty: poll loop iteration {}", iteration);

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
                    log::debug!("execute_without_pty: child still alive");
                }
            }
            Ok(_) => {
                if handle_nonpty_poll_events(&poll_fds, stdout_file, stderr_file, stdin_file, stream, &mut buf, &mut tcp_buf, &mut tcp_buf_pos, &mut seq_out, &mut seq_err)? {
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

    let (child, stdin_pipe, stdout_pipe, stderr_pipe) = match spawn_child_piped(request, stream)? {
        SpawnOutcome::SpawnFailedExitSent(code) => return Ok(code),
        SpawnOutcome::Spawned(c, si, so, se)    => (c, si, so, se),
    };

    let mut stdin_file  = stdin_pipe;
    let mut stdout_file = stdout_pipe;
    let mut stderr_file = stderr_pipe;

    if !request.stdin.is_empty() {
        stdin_file.write_all(request.stdin.as_bytes())?;
    }

    let child_pid    = Pid::from_raw(child.id() as i32);
    let child_status = nonpty_poll_loop(child_pid, &mut stdin_file, &mut stdout_file, &mut stderr_file, stream, initial_data)?;

    let status = match child_status {
        Some(s) => s,
        None    => waitpid(Some(child_pid), None)?,
    };
    let exit_code = exit_code_from_wait_status(status);
    log::debug!("execute_without_pty: sending exit message code={}", exit_code);
    send_exit_and_flush(stream, exit_code)
}

fn handle_connection(mut stream: TcpStream) -> Result<bool> {
    log::debug!("handle_connection: new connection");
    let mut buf = [0; TCP_LINE_BUF_SIZE];
    match stream.read(&mut buf) {
        Ok(n) if n > 0 => {
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

            // Parse JSON request (no plain text fallback)
            let request: CommandRequest = serde_json::from_str(&input)
                .map_err(|e| eyre!("JSON parse failed: {} (input: {:?})", e, input))?;

            log::debug!("Received command: {:?}", request.command);

            // Execute command
            if request.pty {
                log::debug!("Using PTY mode");
                let _exit_code = execute_with_pty(&request, &mut stream, Some(leftover_bytes))?;
                // Return true to exit daemon after PTY command
                return Ok(true);
            } else {
                log::debug!("Using non-PTY mode");
                let _exit_code = execute_without_pty(&request, &mut stream, Some(leftover_bytes))?;
                // Return true to exit daemon after command
                return Ok(true);
            }
        }
        Ok(_) => {
            // Empty read, but still exit to avoid hanging
            Ok(true)
        }
        Err(e) => {
            eprintln!("Failed to read from socket: {}", e);
            Ok(true) // Exit on error
        }
    }
}

#[cfg(target_os = "linux")]
fn run_vsock_server() -> Result<()> {
    use std::os::fd::FromRawFd;

    // Fixed vsock port matching host/client side.
    const VSOCK_PORT: u32 = 10000;

    let fd = socket::socket(
        AddressFamily::Vsock,
        SockType::Stream,
        SockFlag::SOCK_CLOEXEC,
        None,
    )?;
    let addr = VsockAddr::new(libc::VMADDR_CID_ANY, VSOCK_PORT);
    let raw_fd = fd.as_raw_fd();
    socket::bind(raw_fd, &addr)?;
    socket::listen(&fd, Backlog::new(1)?)?;

    eprintln!("vm-daemon starting (vsock)");
    eprintln!("vsock server listening on cid=ANY port={}", VSOCK_PORT);
    log::debug!("vm-daemon vsock listening on port {}", VSOCK_PORT);

    match socket::accept(raw_fd) {
        Ok(client_fd) => {
            let stream = unsafe { TcpStream::from_raw_fd(client_fd) };
            log::debug!("vm-daemon vsock: accepted connection");
            match handle_connection(stream) {
                Ok(_) => {
                    log::debug!("Command processed, powering off guest (vsock)");
                    let _ = reboot::reboot(reboot::RebootMode::RB_POWER_OFF);
                    log::debug!("reboot(RB_POWER_OFF) failed (should not return)");
                    Ok(())
                }
                Err(e) => {
                    log::debug!("Error handling vsock connection: {}", e);
                    Err(e)
                }
            }
        }
        Err(e) => {
            log::debug!("vsock accept failed: {}", e);
            Err(eyre!("vsock accept failed: {}", e))
        }
    }
}

pub fn run(options: VmDaemonOptions) -> Result<()> {
    // When EPKG_VM_VSOCK is set, prefer vsock-based control plane.
    let use_vsock = std::env::var("EPKG_VM_VSOCK").is_ok();

    if use_vsock {
        #[cfg(target_os = "linux")]
        {
            return run_vsock_server();
        }
        #[cfg(not(target_os = "linux"))]
        {
            return Err(eyre!("vm-daemon vsock mode not supported on this platform"));
        }
    }

    // Default: TCP listener as before.
    use std::net::TcpListener;

    let listener = TcpListener::bind(format!("{}:{}", options.host, options.port))
        .map_err(|e| eyre!("Failed to bind TCP listener: {}", e))?;

    eprintln!("vm-daemon starting");
    eprintln!("TCP server listening on {}:{}", options.host, options.port);

    // Accept exactly one connection, handle it, then power off
    match listener.accept() {
        Ok((stream, addr)) => {
            log::debug!("New connection from {:?}", addr);
            match handle_connection(stream) {
                Ok(_) => {
                    log::debug!("Command processed, powering off guest");
                    // Power off guest gracefully
                    let _ = reboot::reboot(reboot::RebootMode::RB_POWER_OFF);
                    // reboot only returns on error
                    log::debug!("reboot(RB_POWER_OFF) failed (should not return)");
                    Ok(())
                }
                Err(e) => {
                    log::debug!("Error handling connection: {}", e);
                    Err(e)
                }
            }
        }
        Err(e) => {
            log::debug!("Connection failed: {}", e);
            Err(eyre!("Connection failed: {}", e))
        }
    }
}
