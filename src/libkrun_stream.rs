//! Vsock command bridge + streaming I/O (Unix sockets vs Windows named pipes).

use color_eyre::eyre;
use color_eyre::Result;
use std::io::{BufRead, Read, Write};
#[cfg(not(windows))]
use std::io::IsTerminal;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use crate::models::IoMode;

#[cfg(unix)]
use lazy_static::lazy_static;
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
#[cfg(windows)]
use windows::Win32::Storage::FileSystem::FlushFileBuffers;
#[cfg(windows)]
use windows::Win32::Foundation::HANDLE;

#[cfg(unix)]
lazy_static! {
    static ref RESIZE_PENDING: AtomicBool = AtomicBool::new(false);
}

#[cfg(unix)]
extern "C" fn handle_sigwinch(_: i32) {
    RESIZE_PENDING.store(true, Ordering::SeqCst);
}

/// Flush Windows named pipe to ensure data is sent to the other end.
/// Standard File::flush() is a no-op; we need FlushFileBuffers for named pipes.
#[cfg(windows)]
fn flush_named_pipe(file: &std::fs::File) -> std::io::Result<()> {
    let handle = file.as_raw_handle();
    unsafe {
        let result = FlushFileBuffers(HANDLE(handle as *mut _));
        if result.is_err() {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
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

pub(crate) fn build_command_request(
    cmd_parts: &[String],
    io_mode: IoMode,
    reuse_vm: bool,
    env_vars: Option<&std::collections::HashMap<String, String>>,
) -> serde_json::Value {
    crate::debug_epkg!("build_command_request: starting");
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
    // Add environment variables if provided
    if let Some(env) = env_vars {
        if !env.is_empty() {
            m.insert(
                "env".to_string(),
                serde_json::Value::Object(
                    env.iter()
                        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                        .collect(),
                ),
            );
        }
    }
    serde_json::Value::Object(m)
}

fn resolve_io_mode(io_mode: IoMode) -> (bool, bool) {
    crate::debug_epkg!("resolve_io_mode: io_mode={:?}", io_mode);
    match io_mode {
        IoMode::Auto => {
            crate::debug_epkg!("resolve_io_mode: checking is_terminal...");
            // On Windows, is_terminal() can hang in some contexts.
            // Use a timeout to avoid blocking indefinitely.
            #[cfg(windows)]
            {
                // On Windows, default to batch mode to avoid is_terminal hang
                crate::debug_epkg!("resolve_io_mode: Windows - defaulting to batch mode");
                (false, true)
            }
            #[cfg(not(windows))]
            {
                let is_tty = std::io::stdin().is_terminal();
                crate::debug_epkg!("resolve_io_mode: is_terminal={}", is_tty);
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

    crate::debug_epkg!("handle_streaming_simple: [PERF] starting, is_batch={}", is_batch);
    let func_start = std::time::Instant::now();
    // In reverse vsock mode, the guest sends "READY\n" first to signal readiness.
    // We need to read and skip this signal before reading the actual response.
    // For non-batch mode, this is handled by the line-by-line parser.
    if is_batch {
        // Batch mode: read entire response as single JSON object
        // First, read lines until we get a non-"READY" line (the actual JSON response)
        let mut response = String::new();
        let reader = BufReader::new(stream);
        crate::debug_epkg!("handle_streaming_simple: batch mode - reading response...");

        for line in reader.lines() {
            crate::debug_epkg!("handle_streaming_simple: waiting for line...");
            let line = match line {
                Ok(l) => l,
                Err(e) => {
                    crate::debug_epkg!("handle_streaming_simple: ERROR reading line: {}", e);
                    return Err(e.into());
                }
            };
            crate::debug_epkg!("handle_streaming_simple: read line: {:?}", line);
            // Skip "READY" signal from reverse vsock handshake
            if line == "READY" {
                crate::debug_epkg!("handle_streaming_simple: skipped READY signal");
                continue;
            }
            // First non-READY line is the start of the JSON response
            // For batch mode, we expect the entire JSON on one line (or concatenated)
            response = line;
            // Try to parse what we have so far
            break;
        }
        crate::debug_epkg!("handle_streaming_simple: broke from first loop, response len={}", response.len());

        // Continue reading remaining lines and append (for large responses)
        // Note: this is a simplified approach; proper JSON streaming would be better
        crate::debug_epkg!("handle_streaming_simple: read {} bytes response", response.len());

        #[derive(Deserialize)]
        struct BatchResult {
            exit_code: i32,
            stdout: String,
            stderr: String,
        }
        crate::debug_epkg!("handle_streaming_simple: parsing JSON response: {}", response);
        let result: BatchResult = match serde_json::from_str(&response) {
            Ok(r) => {
                crate::debug_epkg!("handle_streaming_simple: JSON parsed successfully");
                r
            }
            Err(e) => {
                crate::debug_epkg!("handle_streaming_simple: JSON parse FAILED: {} (response: {:?})", e, response);
                return Err(eyre::eyre!("Failed to parse batch response: {} ({:?})", e, response));
            }
        };

        crate::debug_epkg!("handle_streaming_simple: stdout={} bytes, stderr={} bytes",
            result.stdout.len(), result.stderr.len());

        if !result.stdout.is_empty() {
            let stdout_bytes = STANDARD.decode(&result.stdout)?;
            std::io::stdout().write_all(&stdout_bytes)?;
        }
        if !result.stderr.is_empty() {
            let stderr_bytes = STANDARD.decode(&result.stderr)?;
            std::io::stderr().write_all(&stderr_bytes)?;
        }
        crate::debug_epkg!("handle_streaming_simple: returning exit_code={}", result.exit_code);
        crate::debug_epkg!("handle_streaming_simple: [PERF] total time {:.3}ms", func_start.elapsed().as_secs_f64() * 1000.0);
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
            // Skip "READY" signal from reverse vsock handshake
            if line == "READY" {
                crate::debug_epkg!("handle_streaming_simple: stream mode - skipped READY signal");
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
    env_vars: Option<&std::collections::HashMap<String, String>>,
) -> Result<i32> {
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
                    s = Some(unix_stream);
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

    let request = build_command_request(cmd_parts, io_mode, reuse_vm, env_vars);
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
fn handle_streaming_unix(stream: &mut std::os::unix::net::UnixStream) -> Result<i32> {
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
    env_vars: Option<&std::collections::HashMap<String, String>>,
) -> Result<i32> {
    crate::debug_epkg!("libkrun_stream: send_command_via_vsock starting");
    crate::debug_epkg!("libkrun_stream: about to resolve io_mode...");
    let (use_pty, is_batch) = resolve_io_mode(io_mode);
    crate::debug_epkg!("libkrun_stream: io_mode resolved");
    crate::debug_epkg!("libkrun_stream: io_mode={:?}, use_pty={}, is_batch={}, reuse_vm={}",
        io_mode, use_pty, is_batch, reuse_vm);
    crate::debug_epkg!("libkrun_stream: connecting to vsock bridge at {:?}", sock_path);

    crate::debug_epkg!("libkrun_stream: about to call connect_vsock_bridge");
    let mut stream = super::libkrun_bridge::connect_vsock_bridge(sock_path, 30)?;
    crate::debug_epkg!("libkrun_stream: connected to vsock bridge");

    // WaitNamedPipeA already ensures the named pipe is ready (guest has connected).
    // The guest sends READY signal immediately after connection.
    // We can proceed directly - handle_streaming_simple will skip the READY signal.
    // No additional delay needed since WaitNamedPipeA ensures the guest is ready.
    crate::debug_epkg!("libkrun_stream: connection ready, proceeding immediately");
    let request = build_command_request(cmd_parts, io_mode, reuse_vm, env_vars);
    crate::debug_epkg!("libkrun_stream: serializing to json");
    let request_json = serde_json::to_vec(&request)?;
    crate::debug_epkg!("libkrun_stream: writing {} bytes to stream", request_json.len());
    stream.write_all(&request_json)?;
    crate::debug_epkg!("libkrun_stream: writing newline");
    stream.write_all(b"\n")?;
    crate::debug_epkg!("libkrun_stream: flushing named pipe with FlushFileBuffers");
    flush_named_pipe(&stream)?;
    crate::debug_epkg!("libkrun_stream: request sent");

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
#[cfg(not(windows))]
pub fn send_command_over_stream(
    cmd_parts: &[String],
    io_mode: IoMode,
    reuse_vm: bool,
    env_vars: Option<&std::collections::HashMap<String, String>>,
    mut stream: impl Read + Write + Send + 'static,
) -> Result<i32> {
    crate::debug_epkg!("libkrun_stream: send_command_over_stream starting");
    let (use_pty, is_batch) = resolve_io_mode(io_mode);
    crate::debug_epkg!("libkrun_stream: io_mode={:?}, use_pty={}, is_batch={}, reuse_vm={}",
        io_mode, use_pty, is_batch, reuse_vm);

    // In reverse mode, the connection is already fully established when we receive it.
    // The Guest sent READY immediately after connect, so we can proceed directly.
    // No delay needed here - the vsock handshake is complete.
    crate::debug_epkg!("libkrun_stream: sending command request...");

    // Build and send command request
    let request = build_command_request(cmd_parts, io_mode, reuse_vm, env_vars);
    let request_json = serde_json::to_vec(&request)?;
    crate::debug_epkg!("libkrun_stream: writing {} bytes to stream", request_json.len());
    stream.write_all(&request_json)?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    crate::debug_epkg!("libkrun_stream: request sent, waiting for response...");
    
    // CRITICAL: In reverse mode, we need to ensure the request is fully sent before reading.
    // Without this, the guest may not receive the request due to buffering.
    stream.flush()?;
    crate::debug_epkg!("libkrun_stream: flushed stream after sending request");

    // Handle response based on mode
    let result = if use_pty {
        // PTY mode: Use the generic handler since stream type may vary
        handle_streaming_simple(&mut stream, false)
    } else if is_batch {
        handle_streaming_simple(&mut stream, true)
    } else {
        handle_streaming_simple(&mut stream, false)
    };

    match &result {
        Ok(code) => crate::debug_epkg!("libkrun_stream: command completed with exit code {}", code),
        Err(e) => crate::debug_epkg!("libkrun_stream: command failed with error: {}", e),
    }

    result
}

/// Windows-specific function to send command over a named pipe.
/// Uses FlushFileBuffers to ensure data is sent immediately.
#[cfg(windows)]
pub fn send_command_over_named_pipe(
    cmd_parts: &[String],
    io_mode: IoMode,
    reuse_vm: bool,
    mut stream: std::fs::File,
) -> Result<i32> {
    crate::debug_epkg!("libkrun_stream: send_command_over_named_pipe starting");
    let (use_pty, is_batch) = resolve_io_mode(io_mode);
    crate::debug_epkg!("libkrun_stream: io_mode={:?}, use_pty={}, is_batch={}, reuse_vm={}",
        io_mode, use_pty, is_batch, reuse_vm);

    // Build and send command request
    let request = build_command_request(cmd_parts, io_mode, reuse_vm, None);
    let request_json = serde_json::to_vec(&request)?;
    crate::debug_epkg!("libkrun_stream: [PERF] writing {} bytes to named pipe", request_json.len());
    let write_start = std::time::Instant::now();
    stream.write_all(&request_json)?;
    stream.write_all(b"\n")?;

    // CRITICAL: Use FlushFileBuffers to ensure data is sent to the named pipe.
    // Standard flush() is a no-op for File; named pipes need this Windows API.
    flush_named_pipe(&stream)?;
    crate::debug_epkg!("libkrun_stream: [PERF] write+flush took {:.3}ms", write_start.elapsed().as_secs_f64() * 1000.0);
    crate::debug_epkg!("libkrun_stream: [PERF] waiting for response...");

    // Handle response
    let response_start = std::time::Instant::now();
    let result = if use_pty {
        handle_streaming_simple(&mut stream, false)
    } else if is_batch {
        handle_streaming_simple(&mut stream, true)
    } else {
        handle_streaming_simple(&mut stream, false)
    };
    crate::debug_epkg!("libkrun_stream: [PERF] response handling took {:.3}ms", response_start.elapsed().as_secs_f64() * 1000.0);

    match &result {
        Ok(code) => crate::debug_epkg!("libkrun_stream: command completed with exit code {}", code),
        Err(e) => crate::debug_epkg!("libkrun_stream: command failed with error: {}", e),
    }

    result
}
