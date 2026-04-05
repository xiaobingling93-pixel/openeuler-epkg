//! Cross-process VM session discovery and management.
//!
//! This module provides file-based VM session tracking that enables:
//! - Cross-process VM reuse (child processes can detect parent's VM)
//! - Data integrity (ONE active VM per env_root)
//! - Stale session cleanup (automatic detection of crashed processes)
//!
//! Session file location: `{epkg_run}/vm-sessions/{env_name}.json`
//! Socket path pattern: `{epkg_run}/vsock-{env_name}.sock`

use std::path::Path;
use color_eyre::Result;
use crate::lfs;

/// VM configuration parameters.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VmConfig {
    /// Idle timeout in seconds (0 = never timeout, keep VM alive indefinitely)
    pub timeout: u32,
    /// Seconds to extend timeout after each command
    pub extend: u32,
    /// Number of CPUs for VM
    pub cpus: u32,
    /// Memory in MiB for VM
    pub memory_mib: u32,
    /// VMM backend: "libkrun" or "qemu"
    pub backend: String,
}

impl Default for VmConfig {
    fn default() -> Self {
        Self {
            timeout: 0,  // 0 = never auto timeout
            extend: 10,
            cpus: 2,
            memory_mib: 1024,
            backend: "libkrun".to_string(),
        }
    }
}

/// VM session information stored on disk for cross-process discovery.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VmSessionInfo {
    /// Session file format version
    pub version: u32,
    /// Environment name
    pub env_name: String,
    /// Environment root path (for validation)
    pub env_root: std::path::PathBuf,
    /// PID of the daemon process that owns this VM session
    pub daemon_pid: u32,
    /// Path to the vsock Unix socket/named pipe
    pub socket_path: std::path::PathBuf,
    /// Backend type: "libkrun" or "qemu"
    pub backend: String,
    /// VM configuration
    pub config: VmConfig,
    /// Unix timestamp when session was created
    pub created_at: u64,
    /// Unix timestamp of last activity (for stale detection)
    pub last_activity: u64,
}

/// Get env_name from env_root path.
/// Uses the same logic as main.rs env_name_from_path.
pub fn env_name_from_path(env_root: &Path) -> String {
    let dir = env_root.display().to_string();
    // Trim trailing slashes
    let trimmed = dir.trim_matches(|c| c == '/' || c == '\\');
    if trimmed.is_empty() {
        return "sysroot".to_string();
    }
    // Replace path separators with "__"
    trimmed
        .replace('/', "__")
        .replace('\\', "__")
        .replace(':', "_")
}

/// Get the VM session file path for an env_root.
/// Location: {epkg_run}/vm-sessions/{env_name}.json
pub fn vm_session_file_path(env_root: &Path) -> std::path::PathBuf {
    let env_name = env_name_from_path(env_root);
    crate::models::dirs().epkg_run
        .join("vm-sessions")
        .join(format!("{}.json", env_name))
}

/// Get the vsock socket path for an env_root.
/// Pattern: {epkg_run}/vsock-{env_name}.sock
pub fn vm_socket_path_for_env(env_root: &Path) -> std::path::PathBuf {
    let env_name = env_name_from_path(env_root);
    crate::models::dirs().epkg_run.join(format!("vsock-{}.sock", env_name))
}

/// Check if a process with the given PID is still alive.
#[cfg(unix)]
pub fn is_process_alive(pid: u32) -> bool {
    // Send signal 0 to check if process exists
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_ok()
}

#[cfg(windows)]
pub fn is_process_alive(pid: u32) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::OpenProcess;
    use windows::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION;

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid);
        if let Ok(h) = handle {
            let _ = CloseHandle(h);
            true
        } else {
            false
        }
    }
}

/// Discover an existing VM session for the given env_root.
/// Returns session info if a live VM exists, None otherwise.
pub fn discover_vm_session(env_root: &Path) -> Result<Option<VmSessionInfo>> {
    let session_file = vm_session_file_path(env_root);

    if !session_file.exists() {
        return Ok(None);
    }

    let content = match std::fs::read_to_string(&session_file) {
        Ok(c) => c,
        Err(e) => {
            log::debug!("vm_session: failed to read session file {}: {}", session_file.display(), e);
            return Ok(None);
        }
    };

    let info: VmSessionInfo = match serde_json::from_str(&content) {
        Ok(i) => i,
        Err(e) => {
            log::debug!("vm_session: failed to parse session file {}: {}", session_file.display(), e);
            // Corrupt file, clean up
            let _ = std::fs::remove_file(&session_file);
            return Ok(None);
        }
    };

    // Verify env_root matches
    if info.env_root != env_root {
        log::debug!("vm_session: session file env_root mismatch: {} vs {}", info.env_root.display(), env_root.display());
        return Ok(None);
    }

    // Check if daemon process is still alive
    if !is_process_alive(info.daemon_pid) {
        log::debug!("vm_session: daemon process {} is dead, cleaning up", info.daemon_pid);
        cleanup_vm_session_files(&session_file, &info.socket_path);
        return Ok(None);
    }

    // Verify socket exists and is connectable
    #[cfg(all(unix, feature = "libkrun"))]
    let socket_connectable = std::os::unix::net::UnixStream::connect(&info.socket_path).is_ok();
    #[cfg(all(windows, feature = "libkrun"))]
    let socket_connectable = crate::libkrun::libkrun_bridge::connect_vsock_bridge(&info.socket_path, 1).is_ok();
    #[cfg(not(feature = "libkrun"))]
    let socket_connectable = false;

    if !socket_connectable {
        log::debug!("vm_session: session socket {} is not connectable, cleaning up", info.socket_path.display());
        cleanup_vm_session_files(&session_file, &info.socket_path);
        return Ok(None);
    }

    log::info!("vm_session: discovered active VM session for {} (PID {}, socket {})",
               env_root.display(), info.daemon_pid, info.socket_path.display());
    Ok(Some(info))
}

/// Clean up session file and socket file.
pub fn cleanup_vm_session_files(session_file: &Path, socket_path: &Path) {
    let _ = std::fs::remove_file(session_file);
    let _ = std::fs::remove_file(socket_path);
    log::debug!("vm_session: cleaned up session files: {} and {}", session_file.display(), socket_path.display());
}

/// Register a new VM session to the on-disk session file.
/// Must be called after VM starts successfully.
pub fn register_vm_session(
    env_root: &Path,
    env_name: &str,
    socket_path: &Path,
    backend: &str,
    config: &VmConfig,
) -> Result<()> {
    let session_dir = crate::models::dirs().epkg_run.join("vm-sessions");
    lfs::create_dir_all(&session_dir)?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let session_info = VmSessionInfo {
        version: 2,
        env_name: env_name.to_string(),
        env_root: env_root.to_path_buf(),
        daemon_pid: std::process::id(),
        socket_path: socket_path.to_path_buf(),
        backend: backend.to_string(),
        config: config.clone(),
        created_at: now,
        last_activity: now,
    };

    let session_file = vm_session_file_path(env_root);
    let content = serde_json::to_string_pretty(&session_info)?;
    std::fs::write(&session_file, content)?;

    log::info!("vm_session: registered VM session: {}", session_file.display());
    Ok(())
}

/// Simple registration for libkrun's existing path.
/// Uses default config and derives env_name from path.
pub fn register_vm_session_simple(env_root: &Path, socket_path: &Path) -> Result<()> {
    let env_name = env_name_from_path(env_root);
    let config = VmConfig::default();
    register_vm_session(env_root, &env_name, socket_path, "libkrun", &config)
}

/// Unregister a VM session (called when VM shuts down).
pub fn unregister_vm_session(env_root: &Path) -> Result<()> {
    let session_file = vm_session_file_path(env_root);
    if session_file.exists() {
        // Read socket path before removing session file
        if let Ok(content) = std::fs::read_to_string(&session_file) {
            if let Ok(info) = serde_json::from_str::<VmSessionInfo>(&content) {
                let _ = std::fs::remove_file(&info.socket_path);
            }
        }
        let _ = std::fs::remove_file(&session_file);
        log::info!("vm_session: unregistered VM session: {}", session_file.display());
    }
    Ok(())
}

/// Clean up stale VM session files from crashed processes.
/// Called at startup and during VM creation.
pub fn cleanup_stale_vm_sessions() {
    let sessions_dir = crate::models::dirs().epkg_run.join("vm-sessions");
    if !sessions_dir.exists() {
        return;
    }

    if let Ok(entries) = std::fs::read_dir(&sessions_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension() != Some(std::ffi::OsStr::new("json")) {
                continue;
            }

            // Try to parse session file
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(info) = serde_json::from_str::<VmSessionInfo>(&content) {
                    // Check if daemon process is alive
                    if !is_process_alive(info.daemon_pid) {
                        log::debug!("vm_session: cleaning up stale session for PID {}", info.daemon_pid);
                        cleanup_vm_session_files(&path, &info.socket_path);
                    }
                }
            }
        }
    }
}

/// Check if there's an active VM session for the given env_root.
/// This is the primary entry point for cross-process VM detection.
pub fn is_vm_session_active(env_root: &Path) -> bool {
    discover_vm_session(env_root).ok().flatten().is_some()
}

/// Load session info for an env by name.
pub fn load_session_by_name(env_name: &str) -> Result<Option<VmSessionInfo>> {
    let sessions_dir = crate::models::dirs().epkg_run.join("vm-sessions");
    let session_file = sessions_dir.join(format!("{}.json", env_name));

    if !session_file.exists() {
        return Ok(None);
    }

    let content = std::fs::read_to_string(&session_file)?;
    let info: VmSessionInfo = serde_json::from_str(&content)?;

    // Check if daemon is still alive
    if !is_process_alive(info.daemon_pid) {
        cleanup_vm_session_files(&session_file, &info.socket_path);
        return Ok(None);
    }

    Ok(Some(info))
}

/// List all active VM sessions.
pub fn list_vm_sessions() -> Result<Vec<VmSessionInfo>> {
    let sessions_dir = crate::models::dirs().epkg_run.join("vm-sessions");
    if !sessions_dir.exists() {
        return Ok(Vec::new());
    }

    let mut sessions = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&sessions_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension() != Some(std::ffi::OsStr::new("json")) {
                continue;
            }

            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(info) = serde_json::from_str::<VmSessionInfo>(&content) {
                    if is_process_alive(info.daemon_pid) {
                        sessions.push(info);
                    } else {
                        // Clean up stale session
                        cleanup_vm_session_files(&path, &info.socket_path);
                    }
                }
            }
        }
    }

    Ok(sessions)
}