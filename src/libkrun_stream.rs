//! Vsock command bridge + streaming I/O (Unix sockets vs Windows named pipes).

use color_eyre::eyre;
use color_eyre::Result;
use std::io::{BufRead, Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::{Deserialize, Serialize};

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

pub(crate) fn build_command_request(cmd_parts: &[String], use_pty: bool, reuse_vm: bool) -> serde_json::Value {
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
    if reuse_vm {
        m.insert("reuse_vm".to_string(), serde_json::Value::Bool(true));
    }
    serde_json::Value::Object(m)
}

fn resolve_use_pty(use_pty: Option<bool>) -> bool {
    use std::io::IsTerminal;
    use_pty.unwrap_or_else(|| std::io::stdin().is_terminal())
}

fn handle_streaming_simple(stream: &mut impl Read, use_pty: bool) -> Result<i32> {
    if !use_pty {
        let mut response = String::new();
        stream.read_to_string(&mut response)?;
        let msg: StreamMessage = serde_json::from_str(&response)
            .unwrap_or_else(|_| StreamMessage::Exit { code: 0 });
        match msg {
            StreamMessage::Exit { code } => Ok(code),
            StreamMessage::Error { message } => Err(eyre::eyre!("VM error: {}", message)),
            _ => Ok(0),
        }
    } else {
        Err(eyre::eyre!("internal: PTY path should not call handle_streaming_simple"))
    }
}

#[cfg(unix)]
pub fn send_command_via_vsock(
    cmd_parts: &[String],
    use_pty: Option<bool>,
    reuse_vm: bool,
    sock_path: &Path,
) -> Result<i32> {
    use std::os::unix::io::IntoRawFd;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    let should_use_pty = resolve_use_pty(use_pty);
    log::debug!(
        "libkrun: use_pty={:?}, should_use_pty={}, reuse_vm={}",
        use_pty,
        should_use_pty,
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

    let request = build_command_request(cmd_parts, should_use_pty, reuse_vm);
    let request_json = serde_json::to_vec(&request)?;
    stream.write_all(&request_json)?;
    stream.write_all(b"\n")?;
    log::debug!("libkrun: request sent ({} bytes)", request_json.len());

    handle_streaming_unix(&mut stream, should_use_pty)
}

#[cfg(unix)]
fn handle_streaming_unix(stream: &mut std::net::TcpStream, use_pty: bool) -> Result<i32> {
    use std::os::unix::io::AsRawFd;

    use console::Term;
    use nix::sys::signal::{signal, SigHandler, Signal};
    use nix::sys::termios;

    if !use_pty {
        return handle_streaming_simple(stream, false);
    }

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
    use_pty: Option<bool>,
    reuse_vm: bool,
    sock_path: &Path,
) -> Result<i32> {
    let should_use_pty = resolve_use_pty(use_pty);
    log::debug!(
        "libkrun: use_pty={:?}, should_use_pty={}, reuse_vm={}",
        use_pty,
        should_use_pty,
        reuse_vm
    );

    let mut stream = super::libkrun_bridge::connect_vsock_bridge(sock_path, 30)?;
    log::debug!("libkrun: named pipe connected, sending command {:?}", cmd_parts);

    let request = build_command_request(cmd_parts, should_use_pty, reuse_vm);
    let request_json = serde_json::to_vec(&request)?;
    stream.write_all(&request_json)?;
    stream.write_all(b"\n")?;
    log::debug!("libkrun: request sent ({} bytes)", request_json.len());

    handle_streaming_windows(&mut stream, should_use_pty)
}

#[cfg(windows)]
fn handle_streaming_windows(stream: &mut std::fs::File, use_pty: bool) -> Result<i32> {
    use std::sync::mpsc;
    use std::time::Duration;

    if !use_pty {
        return handle_streaming_simple(stream, false);
    }

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
