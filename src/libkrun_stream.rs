//! Vsock command bridge + streaming I/O (Unix sockets vs Windows named pipes).

use color_eyre::eyre;
use color_eyre::Result;
use std::io::{BufRead, IsTerminal, Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use crate::models::IoMode;

#[cfg(unix)]
use std::os::fd::FromRawFd;
#[cfg(unix)]
use lazy_static::lazy_static;
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(unix)]
lazy_static! {
    static ref RESIZE_PENDING: AtomicBool = AtomicBool::new(false);
}

#[cfg(unix)]
extern "C" fn handle_sigwinch(_: i32) {
    RESIZE_PENDING.store(true, Ordering::SeqCst);
}

/// Streaming message types for interactive/TUI modes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
enum StreamMessage {
    #[serde(rename = "stdin")]
    Stdin { data: String, seq: u64 },
    #[serde(rename = "stdout")]
    Stdout { data: String, seq: u64 },
    #[serde(rename = "stderr")]
    Stderr { data: String, seq: u64 },
    #[serde(rename = "resize")]
    Resize { cols: u16, rows: u16 },
    #[serde(rename = "exit")]
    Exit { code: i32 },
    #[serde(rename = "signal")]
    Signal { sig: i32 },
    #[serde(rename = "error")]
    Error { message: String },
}

pub(crate) fn build_command_request(cmd_parts: &[String], io_mode: IoMode, reuse_vm: bool) -> serde_json::Value {
    eprintln!("[epkg-debug] build_command_request: starting");
    // On Windows, is_terminal() can hang - avoid calling it
    let use_pty = matches!(io_mode, IoMode::Tty) ||
        (matches!(io_mode, IoMode::Auto) && {
            #[cfg(windows)]
            { false }  // Default to non-PTY on Windows to avoid is_terminal hang
            #[cfg(not(windows))]
            { std::io::stdin().is_terminal() }
        });
    let is_batch = matches!(io_mode, IoMode::Batch) ||
        (matches!(io_mode, IoMode::Auto) && {
            #[cfg(windows)]
            { true }  // Default to batch on Windows
            #[cfg(not(windows))]
            { false }
        });

    let mut m = serde_json::Map::new();
    m.insert("type".to_string(), serde_json::json!("command"));
    m.insert(
        "command".to_string(),
        serde_json::Value::Array(
            cmd_parts
                .iter()
                .map(|s| serde_json::Value::String(s.clone()))
                .collect(),
        ),
    );
    m.insert("pty".to_string(), serde_json::Value::Bool(use_pty));
    if is_batch {
        m.insert("batch".to_string(), serde_json::Value::Bool(true));
    }
    if reuse_vm {
        m.insert("reuse_vm".to_string(), serde_json::Value::Bool(true));
    }
    serde_json::Value::Object(m)
}

fn resolve_io_mode(io_mode: IoMode) -> (bool, bool) {
    eprintln!("[epkg-debug] resolve_io_mode: io_mode={:?}", io_mode);
    match io_mode {
        IoMode::Auto => {
            eprintln!("[epkg-debug] resolve_io_mode: checking is_terminal...");
            // On Windows, is_terminal() can hang in some contexts.
            // Use a timeout to avoid blocking indefinitely.
            #[cfg(windows)]
            {
                // On Windows, default to batch mode to avoid is_terminal hang
                eprintln!("[epkg-debug] resolve_io_mode: Windows - defaulting to batch mode");
                (false, true)
            }
            #[cfg(not(windows))]
            {
                let is_tty = std::io::stdin().is_terminal();
                eprintln!("[epkg-debug] resolve_io_mode: is_terminal={}", is_tty);
                (is_tty, false)
            }
        }
        IoMode::Tty => (true, false),
        IoMode::Stream => (false, false),
        IoMode::Batch => (false, true),
    }
}

fn handle_streaming_simple(stream: &mut impl Read, is_batch: bool) -> Result<i32> {
    use std::io::BufReader;
    use std::io::BufRead;

    if is_batch {
        // Batch mode: read entire response as single JSON object
        let mut response = String::new();
        eprintln!("[epkg-debug] handle_streaming_simple: batch mode - reading response...");
        stream.read_to_string(&mut response)?;
        eprintln!("[epkg-debug] handle_streaming_simple: read {} bytes response", response.len());

        #[derive(Deserialize)]
        struct BatchResult {
            exit_code: i32,
            stdout: String,
            stderr: String,
        }
        let result: BatchResult = serde_json::from_str(&response)
            .map_err(|e| eyre::eyre!("Failed to parse batch response: {} ({:?})", e, response))?;

        eprintln!("[epkg-debug] handle_streaming_simple: stdout={} bytes, stderr={} bytes",
            result.stdout.len(), result.stderr.len());

        if !result.stdout.is_empty() {
            let stdout_bytes = STANDARD.decode(&result.stdout)?;
            std::io::stdout().write_all(&stdout_bytes)?;
        }
        if !result.stderr.is_empty() {
            let stderr_bytes = STANDARD.decode(&result.stderr)?;
            std::io::stderr().write_all(&stderr_bytes)?;
        }
        eprintln!("[epkg-debug] handle_streaming_simple: returning exit_code={}", result.exit_code);
        Ok(result.exit_code)
    } else {
        // Stream mode: read line by line, each line is a JSON message
        let reader = BufReader::new(stream);
        let mut exit_code = 0;

        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }

            let msg: StreamMessage = match serde_json::from_str(&line) {
                Ok(m) => m,
                Err(e) => {
                    log::debug!("Failed to parse stream message: {} (line: {})", e, line);
                    continue;
                }
            };

            match msg {
                StreamMessage::Stdout { data, .. } => {
                    let stdout_bytes = STANDARD.decode(&data)
                        .map_err(|e| eyre::eyre!("Failed to decode stdout: {}", e))?;
                    std::io::stdout().write_all(&stdout_bytes)?;
                    std::io::stdout().flush()?;
                }
                StreamMessage::Stderr { data, .. } => {
                    let stderr_bytes = STANDARD.decode(&data)
                        .map_err(|e| eyre::eyre!("Failed to decode stderr: {}", e))?;
                    std::io::stderr().write_all(&stderr_bytes)?;
                    std::io::stderr().flush()?;
                }
                StreamMessage::Exit { code } => {
                    exit_code = code;
                    break;
                }
                StreamMessage::Error { message } => {
                    return Err(eyre::eyre!("VM error: {}", message));
                }
                _ => {}
            }
        }
        Ok(exit_code)
    }
}

#[cfg(unix)]
pub fn send_command_via_vsock(
    cmd_parts: &[String],
    io_mode: IoMode,
    reuse_vm: bool,
    sock_path: &Path,
) -> Result<i32> {
    use std::os::unix::io::IntoRawFd;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    let (use_pty, is_batch) = resolve_io_mode(io_mode);
    log::debug!(
        "libkrun: io_mode={:?}, use_pty={}, is_batch={}, reuse_vm={}",
        io_mode,
        use_pty,
        is_batch,
        reuse_vm
    );

    let mut stream = {
        let mut retry_count = 0;
        let mut last_error = None;
        let mut s = None;
        while retry_count < 30 {
            match UnixStream::connect(sock_path) {
                Ok(unix_stream) => {
                    let raw_fd = unix_stream.into_raw_fd();
                    s = Some(unsafe { std::net::TcpStream::from_raw_fd(raw_fd) });
                    break;
                }
                Err(e) => {
                    last_error = Some(e);
                    retry_count += 1;
                    if retry_count >= 30 {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
        s.ok_or_else(|| {
            eyre::eyre!(
                "Failed to connect to Unix socket {} after 30 retries: {}",
                sock_path.display(),
                last_error.unwrap_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "connection failed"))
            )
        })?
    };

    log::debug!("libkrun: Unix socket connected, sending command {:?}", cmd_parts);

    let request = build_command_request(cmd_parts, io_mode, reuse_vm);
    let request_json = serde_json::to_vec(&request)?;
    stream.write_all(&request_json)?;
    stream.write_all(b"\n")?;
    log::debug!("libkrun: request sent ({} bytes)", request_json.len());

    if use_pty {
        handle_streaming_unix(&mut stream)
    } else {
        handle_streaming_simple(&mut stream, is_batch)
    }
}

#[cfg(unix)]
fn handle_streaming_unix(stream: &mut std::net::TcpStream) -> Result<i32> {
    use std::os::unix::io::AsRawFd;

    use console::Term;
    use nix::sys::signal::{signal, SigHandler, Signal};
    use nix::sys::termios;

    let term = Term::stdout();
    let original_mode = termios::tcgetattr(std::io::stdin()).ok();

    if let Some(ref orig) = original_mode {
        let mut raw = orig.clone();
        termios::cfmakeraw(&mut raw);
        let _ = termios::tcsetattr(std::io::stdin(), termios::SetArg::TCSANOW, &raw);
    }

    unsafe {
        let _ = signal(Signal::SIGWINCH, SigHandler::Handler(handle_sigwinch));
        let _ = signal(Signal::SIGINT, SigHandler::SigIgn);
        let _ = signal(Signal::SIGTERM, SigHandler::SigIgn);
    }

    let stdin_fd = std::io::stdin().as_raw_fd();
    let stream_clone = stream.try_clone()?;
    let exit_code = Arc::new(Mutex::new(None));
    let exit_code_clone = exit_code.clone();

    let reader = thread::spawn(move || {
        let mut reader = std::io::BufReader::new(&stream_clone);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if let Ok(msg) = serde_json::from_str::<StreamMessage>(&line) {
                        match msg {
                            StreamMessage::Stdout { data, .. } => {
                                if let Ok(decoded) = STANDARD.decode(&data) {
                                    let _ = std::io::stdout().write_all(&decoded);
                                    let _ = std::io::stdout().flush();
                                }
                            }
                            StreamMessage::Stderr { data, .. } => {
                                if let Ok(decoded) = STANDARD.decode(&data) {
                                    let _ = std::io::stderr().write_all(&decoded);
                                    let _ = std::io::stderr().flush();
                                }
                            }
                            StreamMessage::Exit { code } => {
                                *exit_code_clone.lock().unwrap() = Some(code);
                                break;
                            }
                            _ => {}
                        }
                    }
                }
                Err(_) => break,
            }
        }
    });

    let mut seq: u64 = 0;
    let mut buf = [0u8; 4096];
    loop {
        if exit_code.lock().unwrap().is_some() {
            break;
        }

        if RESIZE_PENDING.swap(false, Ordering::SeqCst) {
            let (cols, rows) = term.size();
            let resize_msg = StreamMessage::Resize { cols, rows };
            if let Ok(json) = serde_json::to_string(&resize_msg) {
                let _ = stream.write_all(json.as_bytes());
                let _ = stream.write_all(b"\n");
            }
        }

        let mut pfd = [libc::pollfd {
            fd:      stdin_fd,
            events:  libc::POLLIN,
            revents: 0,
        }];
        let ready = unsafe { libc::poll(pfd.as_mut_ptr(), 1, 50) };
        if ready > 0 && (pfd[0].revents & libc::POLLIN) != 0 {
            match std::io::stdin().read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let data = STANDARD.encode(&buf[..n]);
                    let msg = StreamMessage::Stdin { data, seq };
                    seq += 1;
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = stream.write_all(json.as_bytes());
                        let _ = stream.write_all(b"\n");
                    }
                }
                Err(_) => break,
            }
        }
    }

    reader.join().ok();

    if let Some(orig) = original_mode {
        let _ = termios::tcsetattr(std::io::stdin(), termios::SetArg::TCSANOW, &orig);
    }

    let code = exit_code.lock().unwrap().unwrap_or(0);
    Ok(code)
}

#[cfg(windows)]
pub fn send_command_via_vsock(
    cmd_parts: &[String],
    io_mode: IoMode,
    reuse_vm: bool,
    sock_path: &Path,
) -> Result<i32> {
    eprintln!("[epkg-debug] libkrun_stream: send_command_via_vsock starting");
    eprintln!("[epkg-debug] libkrun_stream: about to resolve io_mode...");
    let (use_pty, is_batch) = resolve_io_mode(io_mode);
    eprintln!("[epkg-debug] libkrun_stream: io_mode resolved");
    eprintln!(
        "[epkg-debug] libkrun_stream: io_mode={:?}, use_pty={}, is_batch={}, reuse_vm={}",
        io_mode,
        use_pty,
        is_batch,
        reuse_vm
    );
    eprintln!("[epkg-debug] libkrun_stream: connecting to vsock bridge at {:?}", sock_path);

    eprintln!("[epkg-debug] libkrun_stream: about to call connect_vsock_bridge");
    let mut stream = super::libkrun_bridge::connect_vsock_bridge(sock_path, 30)?;
    eprintln!("[epkg-debug] libkrun_stream: connected to vsock bridge");

    // CRITICAL FIX: Wait for vsock handshake to complete before sending data
    // The vsock handshake involves: host REQUEST -> guest RESPONSE -> ESTABLISHED
    // This process takes time, especially on Windows/WHPX where the virtio device
    // processes requests asynchronously. The guest's vm_daemon must:
    // 1. Receive the VSOCK_OP_REQUEST from libkrun
    // 2. Call accept() on the vsock socket
    // 3. Send VSOCK_OP_RESPONSE back to libkrun
    // 4. Only THEN can data flow reliably
    // Without sufficient delay, the host sends data before the handshake completes,
    // causing the data to be lost and the guest to read EOF.
    eprintln!("[epkg-debug] libkrun_stream: waiting for vsock handshake to complete...");
    std::thread::sleep(std::time::Duration::from_millis(1000));
    eprintln!("[epkg-debug] libkrun_stream: vsock handshake wait complete");
    eprintln!("[epkg-debug] libkrun_stream: building command request");
    let request = build_command_request(cmd_parts, io_mode, reuse_vm);
    eprintln!("[epkg-debug] libkrun_stream: serializing to json");
    let request_json = serde_json::to_vec(&request)?;
    eprintln!("[epkg-debug] libkrun_stream: writing {} bytes to stream", request_json.len());
    stream.write_all(&request_json)?;
    eprintln!("[epkg-debug] libkrun_stream: writing newline");
    stream.write_all(b"\n")?;
    eprintln!("[epkg-debug] libkrun_stream: flushing stream");
    stream.flush()?;
    eprintln!("[epkg-debug] libkrun_stream: request sent");

    if use_pty {
        handle_streaming_windows(&mut stream)
    } else {
        handle_streaming_simple(&mut stream, is_batch)
    }
}

#[cfg(windows)]
fn handle_streaming_windows(stream: &mut std::fs::File) -> Result<i32> {
    use std::sync::mpsc;
    use std::time::Duration;

    let stream_clone = stream.try_clone()?;
    let exit_code = Arc::new(Mutex::new(None));
    let exit_code_clone = exit_code.clone();

    let reader = thread::spawn(move || {
        let mut reader = std::io::BufReader::new(&stream_clone);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if let Ok(msg) = serde_json::from_str::<StreamMessage>(&line) {
                        match msg {
                            StreamMessage::Stdout { data, .. } => {
                                if let Ok(decoded) = STANDARD.decode(&data) {
                                    let _ = std::io::stdout().write_all(&decoded);
                                    let _ = std::io::stdout().flush();
                                }
                            }
                            StreamMessage::Stderr { data, .. } => {
                                if let Ok(decoded) = STANDARD.decode(&data) {
                                    let _ = std::io::stderr().write_all(&decoded);
                                    let _ = std::io::stderr().flush();
                                }
                            }
                            StreamMessage::Exit { code } => {
                                *exit_code_clone.lock().unwrap() = Some(code);
                                break;
                            }
                            _ => {}
                        }
                    }
                }
                Err(_) => break,
            }
        }
    });

    let (stdin_tx, stdin_rx) = mpsc::channel::<Vec<u8>>();
    let _stdin_thread = thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match std::io::stdin().read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if stdin_tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let mut seq: u64 = 0;
    loop {
        if exit_code.lock().unwrap().is_some() {
            break;
        }
        match stdin_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(bytes) => {
                let data = STANDARD.encode(&bytes);
                let msg = StreamMessage::Stdin { data, seq };
                seq += 1;
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = stream.write_all(json.as_bytes());
                    let _ = stream.write_all(b"\n");
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    reader.join().ok();
    let code = exit_code.lock().unwrap().unwrap_or(0);
    Ok(code)
}

// =============================================================================
// Reverse mode support: Send command over an existing stream
// =============================================================================

/// Send command over an existing stream (for reverse mode).
/// In reverse mode, the Host accepts a connection from Guest, then uses that
/// connection to send commands and receive results.
pub fn send_command_over_stream(
    cmd_parts: &[String],
    io_mode: IoMode,
    reuse_vm: bool,
    mut stream: impl Read + Write + Send + 'static,
) -> Result<i32> {
    eprintln!("[epkg-debug] libkrun_stream: send_command_over_stream starting");
    let (use_pty, is_batch) = resolve_io_mode(io_mode);
    eprintln!(
        "[epkg-debug] libkrun_stream: io_mode={:?}, use_pty={}, is_batch={}, reuse_vm={}",
        io_mode, use_pty, is_batch, reuse_vm
    );

    // Build and send command request
    let request = build_command_request(cmd_parts, io_mode, reuse_vm);
    let request_json = serde_json::to_vec(&request)?;
    eprintln!("[epkg-debug] libkrun_stream: writing {} bytes to stream", request_json.len());
    stream.write_all(&request_json)?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    eprintln!("[epkg-debug] libkrun_stream: request sent");

    // Handle response based on mode
    if use_pty {
        // PTY mode: Use the generic handler since stream type may vary
        handle_streaming_simple(&mut stream, false)
    } else if is_batch {
        handle_streaming_simple(&mut stream, true)
    } else {
        handle_streaming_simple(&mut stream, false)
    }
}
