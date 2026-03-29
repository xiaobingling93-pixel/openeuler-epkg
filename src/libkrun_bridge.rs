//! Host↔guest vsock bridge: Unix domain sockets (macOS/Linux) vs named pipes (Windows WHPX).

use color_eyre::eyre;
use color_eyre::Result;
use std::path::Path;
use std::time::Duration;

#[cfg(unix)]
use std::os::fd::FromRawFd;
#[cfg(unix)]
use std::os::unix::io::AsRawFd;

#[cfg(unix)]
pub fn setup_vsock_ready_listener() -> Result<Option<std::os::unix::net::UnixListener>> {
    let vmm_logs_dir = crate::models::dirs().epkg_cache.join("vmm-logs");
    if let Ok(entries) = std::fs::read_dir(&vmm_logs_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("vsock-") && name.ends_with(".sock") {
                let _ = std::fs::remove_file(entry.path());
                log::trace!("libkrun: cleaned up stale socket {}", name);
            }
            if name.starts_with("ready-") && name.ends_with(".sock") {
                let _ = std::fs::remove_file(entry.path());
                log::trace!("libkrun: cleaned up stale socket {}", name);
            }
        }
    }

    let _pid = std::process::id();
    let ready_path = vmm_logs_dir.join(format!("ready-{}.sock", _pid));
    let _ = std::fs::remove_file(&ready_path);

    log::debug!("libkrun: creating ready listener on {}", ready_path.display());
    let listener = std::os::unix::net::UnixListener::bind(&ready_path)
        .map_err(|e| eyre::eyre!("Failed to bind ready socket {}: {}", ready_path.display(), e))?;

    listener.set_nonblocking(true)
        .map_err(|e| eyre::eyre!("Failed to set non-blocking on ready socket: {}", e))?;

    Ok(Some(listener))
}

#[cfg(unix)]
pub fn wait_guest_ready_unix(
    listener: &std::os::unix::net::UnixListener,
    vm_start_failed_rx: Option<&std::sync::mpsc::Receiver<()>>,
) -> Result<()> {
    let listener_fd = listener.as_raw_fd();

    // Poll with shorter intervals to check for VM start failure
    const POLL_INTERVAL_MS: i32 = 100;
    const TOTAL_TIMEOUT_MS: i32 = 30_000;
    let mut elapsed_ms: i32 = 0;

    loop {
        // Check if VM start failed
        if let Some(ref failed_rx) = vm_start_failed_rx {
            if failed_rx.try_recv().is_ok() {
                return Err(eyre::eyre!("VM failed to start (krun_start_enter error)"));
            }
        }

        let mut poll_fds = [libc::pollfd {
            fd:      listener_fd,
            events:  libc::POLLIN,
            revents: 0,
        }];

        let poll_result = unsafe { libc::poll(poll_fds.as_mut_ptr(), 1, POLL_INTERVAL_MS) };

        match poll_result {
            0 => {
                // Timeout on this poll, continue checking
                elapsed_ms += POLL_INTERVAL_MS;
                if elapsed_ms >= TOTAL_TIMEOUT_MS {
                    log::error!("libkrun: timeout waiting for VM to become ready");
                    return Err(eyre::eyre!("Timeout waiting for VM to start"));
                }
            }
            n if n < 0 => {
                log::error!("libkrun: poll error on ready socket");
                return Err(eyre::eyre!("Poll error on ready socket"));
            }
            _ => {
                let (stream, _addr) = listener
                    .accept()
                    .map_err(|e| eyre::eyre!("Failed to accept on ready socket: {}", e))?;
                log::debug!("libkrun: guest connected to ready socket, guest is ready!");
                drop(stream);
                return Ok(());
            }
        }
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
pub fn connect_vsock_bridge(sock_path: &Path, max_retries: u32) -> Result<std::net::TcpStream> {
    use std::os::unix::io::IntoRawFd;
    use std::os::unix::net::UnixStream;

    let mut retry_count = 0;
    let mut last_error = None;
    while retry_count < max_retries {
        match UnixStream::connect(sock_path) {
            Ok(unix_stream) => {
                let raw_fd = unix_stream.into_raw_fd();
                let stream = unsafe { std::net::TcpStream::from_raw_fd(raw_fd) };
                return Ok(stream);
            }
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
        "Failed to connect to Unix socket {} after {} retries: {}",
        sock_path.display(),
        max_retries,
        last_error.unwrap_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "connection failed"))
    ))
}

#[cfg(windows)]
use std::os::windows::io::FromRawHandle;

#[cfg(windows)]
use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};

#[cfg(windows)]
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};

#[cfg(windows)]
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, WaitNamedPipeA, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};

#[cfg(windows)]
use windows::core::PCWSTR;

/// Stem used for `\\.\pipe\<stem>`; must match `krun_add_vsock_port_windows` on the host.
#[cfg(windows)]
pub(crate) fn pipe_name_from_sock_path(path: &Path) -> Result<String> {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .ok_or_else(|| eyre::eyre!("invalid vsock path (no file stem): {}", path.display()))
}

#[cfg(windows)]
fn to_wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
pub struct WindowsReadyPipe {
    handle: HANDLE,
}

#[cfg(windows)]
impl Drop for WindowsReadyPipe {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

#[cfg(windows)]
pub fn setup_vsock_ready_listener() -> Result<Option<WindowsReadyPipe>> {
    let vmm_logs_dir = crate::models::dirs().epkg_cache.join("vmm-logs");
    let _ = std::fs::create_dir_all(&vmm_logs_dir);
    let pid = std::process::id();
    let ready_path = vmm_logs_dir.join(format!("ready-{pid}.sock"));
    let pipe_name = pipe_name_from_sock_path(&ready_path)?;
    let full = format!("\\\\.\\pipe\\{}", pipe_name);
    let wide = to_wide_null(&full);

    unsafe {
        let h = CreateNamedPipeW(
            PCWSTR(wide.as_ptr()),
            PIPE_ACCESS_DUPLEX,  // Removed FILE_FLAG_OVERLAPPED for synchronous operation
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            4096,
            4096,
            0,
            None,
        );
        if h == INVALID_HANDLE_VALUE {
            return Err(eyre::eyre!(
                "CreateNamedPipeW failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(Some(WindowsReadyPipe { handle: h }))
    }
}

#[cfg(windows)]
pub fn wait_guest_ready_windows(
    pipe: &WindowsReadyPipe,
    vm_start_failed_rx: Option<&std::sync::mpsc::Receiver<()>>,
) -> Result<()> {
    use std::sync::mpsc;
    use std::thread;

    let handle_raw = pipe.handle.0 as usize;
    let (tx, rx) = mpsc::channel();
    let jh = thread::spawn(move || {
        let handle = HANDLE(handle_raw as *mut _);
        let r = unsafe { ConnectNamedPipe(handle, None) };
        let _ = tx.send(r);
    });

    // Poll for either pipe connection or VM start failure
    let start = std::time::Instant::now();
    let timeout = Duration::from_secs(30);
    let check_interval = Duration::from_millis(100);

    loop {
        // Check if VM start failed
        if let Some(ref failed_rx) = vm_start_failed_rx {
            if failed_rx.try_recv().is_ok() {
                // Note: Don't join the pipe thread here - ConnectNamedPipe is blocking
                // and will never complete since VM failed. The thread will be cleaned
                // up when the process exits.
                return Err(eyre::eyre!("VM failed to start (krun_start_enter error)"));
            }
        }

        // Check if pipe connected
        match rx.try_recv() {
            Ok(Ok(())) => {
                log::debug!("libkrun: guest connected to ready pipe, guest is ready!");
                let _ = jh.join();
                return Ok(());
            }
            Ok(Err(e)) => {
                let _ = jh.join();
                return Err(eyre::eyre!("ConnectNamedPipe failed: {}", e));
            }
            Err(mpsc::TryRecvError::Empty) => {
                // Not ready yet, continue waiting
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                let _ = jh.join();
                return Err(eyre::eyre!("Pipe thread disconnected unexpectedly"));
            }
        }

        if start.elapsed() >= timeout {
            log::error!("libkrun: timeout waiting for VM to become ready");
            // Note: Don't join the pipe thread - ConnectNamedPipe is blocking
            return Err(eyre::eyre!("Timeout waiting for VM to start"));
        }

        std::thread::sleep(check_interval);
    }
}

#[cfg(windows)]
pub fn connect_vsock_bridge(sock_path: &Path, max_retries: u32) -> Result<std::fs::File> {
    let pipe_name = pipe_name_from_sock_path(sock_path)?;
    let full = format!("\\\\.\\pipe\\{}", pipe_name);
    eprintln!("[epkg-debug] libkrun_bridge: connecting to named pipe: {}", full);
    let c_path = std::ffi::CString::new(full.as_bytes())
        .map_err(|_| eyre::eyre!("invalid pipe path"))?;

    let mut retry_count = 0;
    let mut last_error = None;
    eprintln!("[epkg-debug] libkrun_bridge: starting connection retry loop (max={})", max_retries);
    while retry_count < max_retries {
        unsafe {
            eprintln!("[epkg-debug] libkrun_bridge: attempt {} - waiting for named pipe...", retry_count);
            if WaitNamedPipeA(
                windows::core::PCSTR(c_path.as_ptr() as *const u8),
                30_000,
            )
            .is_err()
            {
                last_error = Some(std::io::Error::last_os_error());
                eprintln!("[epkg-debug] libkrun_bridge: WaitNamedPipeA failed: {:?}", last_error);
                retry_count += 1;
                if retry_count >= max_retries {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            eprintln!("[epkg-debug] libkrun_bridge: named pipe is available, connecting...");

            // Use synchronous mode for reliable operation with std::fs::File
            // FILE_FLAG_OVERLAPPED causes issues with synchronous I/O
            // GENERIC_READ = 0x80000000, GENERIC_WRITE = 0x40000000
            let access: u32 = 0x80000000u32 | 0x40000000u32;
            let handle = CreateFileW(
                PCWSTR(to_wide_null(&full).as_ptr()),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,  // Removed FILE_FLAG_OVERLAPPED
                None,
            );

            match handle {
                Ok(h) if h != INVALID_HANDLE_VALUE => {
                    eprintln!("[epkg-debug] libkrun_bridge: successfully connected to named pipe");
                    let file = std::fs::File::from_raw_handle(h.0);
                    return Ok(file);
                }
                Ok(_) => {
                    eprintln!("[epkg-debug] libkrun_bridge: CreateFileW returned INVALID_HANDLE_VALUE");
                    last_error = Some(std::io::Error::last_os_error());
                }
                Err(e) => {
                    eprintln!("[epkg-debug] libkrun_bridge: CreateFileW failed: {}", e);
                    last_error = Some(std::io::Error::last_os_error());
                }
            }
        }
        retry_count += 1;
        if retry_count < max_retries {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    eprintln!("[epkg-debug] libkrun_bridge: failed to connect after {} retries: {:?}", max_retries, last_error);

    Err(eyre::eyre!(
        "Failed to connect to named pipe for {} after {} retries: {}",
        sock_path.display(),
        max_retries,
        last_error.unwrap_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "connection failed"))
    ))
}

// =============================================================================
// Reverse vsock mode: Guest connects to Host
// =============================================================================
// In reverse mode, the Host listens on a socket/pipe and waits for the Guest
// to connect. This avoids the vsock handshake timing issues on Windows/WHPX.

/// Set up a reverse listener for Guest to connect to.
/// In reverse mode, Host listens and Guest initiates the connection.
#[cfg(unix)]
pub fn setup_reverse_listener(sock_path: &Path) -> Result<std::os::unix::net::UnixListener> {
    // Clean up any stale socket
    let _ = std::fs::remove_file(sock_path);

    log::debug!("libkrun: creating reverse listener on {}", sock_path.display());
    let listener = std::os::unix::net::UnixListener::bind(sock_path)
        .map_err(|e| eyre::eyre!("Failed to bind reverse socket {}: {}", sock_path.display(), e))?;

    // Set non-blocking for timeout support
    listener.set_nonblocking(true)
        .map_err(|e| eyre::eyre!("Failed to set non-blocking on reverse socket: {}", e))?;

    Ok(listener)
}

/// Accept a connection from Guest in reverse mode.
#[cfg(unix)]
pub fn accept_reverse_connection(
    listener: &std::os::unix::net::UnixListener,
    vm_start_failed_rx: Option<&std::sync::mpsc::Receiver<()>>,
) -> Result<std::net::TcpStream> {
    use std::os::unix::io::IntoRawFd;
    use std::time::Instant;

    let start = Instant::now();
    let timeout = Duration::from_secs(30);
    let check_interval = Duration::from_millis(100);

    loop {
        // Check if VM start failed
        if let Some(ref failed_rx) = vm_start_failed_rx {
            if failed_rx.try_recv().is_ok() {
                return Err(eyre::eyre!("VM failed to start (krun_start_enter error)"));
            }
        }

        // Try to accept connection
        match listener.accept() {
            Ok((stream, _addr)) => {
                log::debug!("libkrun: Guest connected to reverse listener");
                // Convert Unix stream to TcpStream for compatibility
                let raw_fd = stream.into_raw_fd();
                return Ok(unsafe { std::net::TcpStream::from_raw_fd(raw_fd) });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // No connection yet, continue waiting
            }
            Err(e) => {
                return Err(eyre::eyre!("Failed to accept reverse connection: {}", e));
            }
        }

        if start.elapsed() >= timeout {
            return Err(eyre::eyre!("Timeout waiting for Guest to connect (reverse mode)"));
        }

        std::thread::sleep(check_interval);
    }
}

/// Windows reverse listener setup.
#[cfg(windows)]
pub fn setup_reverse_listener(sock_path: &Path) -> Result<WindowsReadyPipe> {
    let pipe_name = pipe_name_from_sock_path(sock_path)?;
    let full = format!("\\\\.\\pipe\\{}", pipe_name);
    let wide = to_wide_null(&full);

    unsafe {
        // CreateNamedPipeW with PIPE_ACCESS_DUPLEX for bidirectional communication
        let h = CreateNamedPipeW(
            PCWSTR(wide.as_ptr()),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            4096,
            4096,
            0,
            None,
        );
        if h == INVALID_HANDLE_VALUE {
            return Err(eyre::eyre!(
                "CreateNamedPipeW failed for reverse listener: {}",
                std::io::Error::last_os_error()
            ));
        }
        log::debug!("libkrun: reverse listener created on pipe {}", full);
        Ok(WindowsReadyPipe { handle: h })
    }
}

/// Accept a connection from Guest in reverse mode (Windows).
#[cfg(windows)]
pub fn accept_reverse_connection(
    pipe: &WindowsReadyPipe,
    vm_start_failed_rx: Option<&std::sync::mpsc::Receiver<()>>,
) -> Result<std::fs::File> {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Instant;

    let handle_raw = pipe.handle.0 as usize;
    let (tx, rx) = mpsc::channel();

    // Spawn thread to wait for connection
    let jh = thread::spawn(move || {
        let handle = HANDLE(handle_raw as *mut _);
        let r = unsafe { ConnectNamedPipe(handle, None) };
        let _ = tx.send(r);
    });

    let start = Instant::now();
    let timeout = Duration::from_secs(30);
    let check_interval = Duration::from_millis(100);

    loop {
        // Check if VM start failed
        if let Some(ref failed_rx) = vm_start_failed_rx {
            if failed_rx.try_recv().is_ok() {
                return Err(eyre::eyre!("VM failed to start (krun_start_enter error)"));
            }
        }

        // Check if pipe connected
        match rx.try_recv() {
            Ok(Ok(())) => {
                log::debug!("libkrun: Guest connected to reverse pipe");
                let _ = jh.join();
                // Return the pipe handle as a File
                let file = std::fs::File::from(unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(pipe.handle.0) });
                return Ok(file);
            }
            Ok(Err(e)) => {
                let _ = jh.join();
                return Err(eyre::eyre!("ConnectNamedPipe failed: {:?}", e));
            }
            Err(mpsc::TryRecvError::Empty) => {
                // Not ready yet
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                let _ = jh.join();
                return Err(eyre::eyre!("Pipe thread disconnected unexpectedly"));
            }
        }

        if start.elapsed() >= timeout {
            return Err(eyre::eyre!("Timeout waiting for Guest to connect (reverse mode)"));
        }

        std::thread::sleep(check_interval);
    }
}
