#[cfg(unix)]
use std::env;
#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
#[cfg(target_os = "linux")]
use std::os::fd::OwnedFd;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

use crate::models::*;

/// Reserved argv for vm-daemon JSON requests: single-element `command` array ends install/upgrade VM reuse.
/// Present when the guest daemon (`vm_daemon`, Linux) or libkrun host teardown (`libkrun`, non-Linux) is built.
#[cfg(any(
    target_os = "linux",
    all(feature = "libkrun", not(target_os = "linux"))
))]
pub const VM_SESSION_DONE_CMD: &str = "__epkg_vm_session_done__";

/// Check if there's an active VM reuse session for a specific env_root.
/// Returns true if there's an active VM session for the same environment.
/// Used by scriptlets/hooks to inherit VM settings during install/upgrade.
#[cfg(feature = "libkrun")]
pub fn is_vm_reuse_active_for_env(env_root: &Path) -> bool {
    crate::libkrun::is_vm_reuse_active_for_env(env_root)
}

/// Stub for non-libkrun builds - always returns false.
#[cfg(not(feature = "libkrun"))]
pub fn is_vm_reuse_active_for_env(_env_root: &Path) -> bool {
    false
}

/// Try to connect to an existing VM session and execute the command.
/// Returns Some(exit_code) if successfully connected and executed.
/// Returns None if no existing VM session exists.
#[cfg(feature = "libkrun")]
fn try_connect_and_execute_vm(env_root: &Path, run_options: &RunOptions) -> Result<Option<i32>> {
    // Build command parts
    let mut cmd_parts = vec![run_options.command.clone()];
    cmd_parts.extend(run_options.args.clone());

    log::debug!("run: checking for existing VM session for {}", env_root.display());

    // For VM mode, chdir_to_env_root means working directory should be "/" (VM root)
    // which maps to the environment root via virtiofs. Otherwise, use current cwd.
    let cwd_str;
    let cwd = if run_options.chdir_to_env_root {
        Some("/")
    } else {
        cwd_str = std::env::current_dir().ok().map(|p| p.to_string_lossy().to_string());
        cwd_str.as_deref()
    };

    crate::libkrun::execute_via_existing_vm(
        env_root,
        &cmd_parts,
        run_options.io_mode,
        Some(&run_options.env_vars),
        cwd,
        run_options.reuse_vm,
    )
}

#[cfg(target_os = "linux")]
use crate::namespace::{determine_process_config, build_unified_context, create_process_with_namespaces};
use crate::lfs;
#[cfg(target_os = "linux")]
use crate::utils::is_suid;
use color_eyre::eyre;
use color_eyre::Result;
use log::debug;
#[cfg(unix)]
use log::trace;
#[cfg(target_os = "linux")]
use log::{info, warn};
#[cfg(target_os = "linux")]
use nix::errno::Errno;
#[cfg(target_os = "linux")]
use nix::sys::signal::{self, Signal};
#[cfg(target_os = "linux")]
use nix::unistd::{close, pipe, write};
#[cfg(target_os = "linux")]
use nix::unistd::setuid;
#[cfg(target_os = "linux")]
use nix::unistd::Uid;
#[cfg(target_os = "linux")]
use users::get_current_uid;

#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    pub user: Option<String>,
    #[allow(dead_code)]
    pub group: Option<String>,
    pub command: String,
    pub args: Vec<String>,
    pub env_vars: std::collections::HashMap<String, String>,
    pub stdin: Option<Vec<u8>>,
    pub no_exit: bool,
    pub chdir_to_env_root: bool,
    pub skip_namespace_isolation: bool,
    pub timeout: u64, // Timeout in seconds, 0 means no timeout
    pub background: bool, // Run in background and return PID instead of waiting
    pub redirect_stdio: bool, // Redirect stdin/stdout/stderr to /dev/null for daemon processes
    pub io_mode: crate::models::IoMode, // I/O mode: auto, tty, stream, or batch

    /// Optional external kernel image for VM backends that support it (e.g. libkrun, qemu).
    /// Can be provided via `--kernel`.
    pub kernel: Option<String>,
    /// Optional extra kernel arguments for VM backends that support them.
    /// Can be provided via `--kernel-args` for `epkg run`.
    pub kernel_args: Option<String>,
    /// Optional initrd image for VM backends that support it (e.g. libkrun, qemu).
    /// Can be provided via `--initrd`.
    pub initrd: Option<String>,

    /// Optional override for VM vCPU count (applies to VM backends).
    /// Can be provided via `--cpus` for `epkg run`.
    pub vm_cpus: Option<u8>,
    /// Optional override for VM memory size in MiB (applies to VM backends).
    /// Can be provided via `--memory` for `epkg run`.
    pub vm_memory_mib: Option<u32>,

    /// Unix socket path for vsock communication (used by libkrun).
    /// libkrun uses Unix sockets instead of AF_VSOCK for host-guest communication.
    pub vsock_socket_path: Option<std::path::PathBuf>,

    /// Input sandbox options from CLI or caller
    pub sandbox: crate::models::SandboxOptions,
    /// Effective sandbox options (merged from all configuration levels)
    pub effective_sandbox: crate::models::SandboxOptions,
    /// Preferred VMM backend order for IsolateMode::Vm.
    /// Example: ["libkrun", "qemu"] or ["qemu"].
    pub vmm_order: Vec<String>,
    /// Keep the microVM alive between `fork_and_execute` calls (install/upgrade on non-Linux hosts).
    /// Cleared after the transaction sends the reserved session-done command to the guest VM.
    pub reuse_vm: bool,
    /// Skip QEMU and connect to an already-running QEMU guest over AF_VSOCK (`epkg run --reuse`).
    pub vm_reuse_connect: bool,
    /// With `--isolate=vm`, after each command finishes (and while no follow-up is connected),
    /// wait this many seconds for another connection (`epkg run --reuse`). `None` = one-shot VM.
    pub vm_keep_timeout: Option<u32>,

    /// UID mapping specifications for virtiofs in VM mode.
    /// Format: same as virtiofsd (e.g., "map:0:501:1", "squash-guest:0:501:65536").
    pub translate_uid: Vec<String>,
    /// GID mapping specifications for virtiofs in VM mode.
    /// Format: same as virtiofsd (e.g., "map:0:20:1", "squash-guest:0:20:65536").
    pub translate_gid: Vec<String>,

    /// Original host UID before any namespace setup (for VM mount configuration).
    /// This is the real UID on the host, which may differ from the namespaced UID.
    pub host_uid: Option<u32>,
    /// Working directory to use after namespace setup (for Fs mode pivot).
    /// When pivot_root changes the root filesystem, the original working directory
    /// becomes invalid. This field stores the path to restore after pivot.
    pub working_dir: Option<std::path::PathBuf>,
}

/// Temporarily set SIGPIPE handler
///
/// SIGPIPE handling principles:
/// 1. Package manager should ignore SIGPIPE and handle EPIPE explicitly
///    - This function is used when writing to child stdin pipes
///    - EPIPE errors are checked explicitly (Errno::EPIPE)
/// 2. Children don't inherit SIGPIPE ignore setting by default
///    - Child processes are not forced to ignore SIGPIPE
///    - They inherit default signal handling unless they change it
/// 3. Scriptlets should use defaults (SIG_DFL) unless they have special needs
///    - Child processes (including scriptlets) use default SIGPIPE handling
/// 4. Don't force SIGPIPE setup on child processes - let them decide
///    - Child processes can set their own SIGPIPE handling as needed
///
/// This function is used by the package manager to temporarily ignore SIGPIPE
/// while performing pipe operations, then restore the previous handler.
#[cfg(target_os = "linux")]
pub(crate) fn with_sigpipe_handler<F, R>(handler: usize, f: F) -> R
where
    F: FnOnce() -> R,
{
    unsafe {
        let old_handler = libc::signal(libc::SIGPIPE, handler);
        let result = f();
        libc::signal(libc::SIGPIPE, old_handler);
        result
    }
}

/// Resolve vCPU count for VM backends.
///
/// Source precedence:
/// 1. RunOptions.vm_cpus (from --cpus)
/// 2. EPKG_VM_CPUS (u8)
/// 3. Default: 2 vCPUs
#[cfg(any(target_os = "linux", feature = "libkrun"))]
pub fn resolve_vm_cpus(run_options: &RunOptions) -> u8 {
    if let Some(cpus) = run_options.vm_cpus {
        return cpus;
    }
    std::env::var("EPKG_VM_CPUS")
        .ok()
        .and_then(|s| s.parse::<u8>().ok())
        .unwrap_or(1)  // Default 1 vCPU for faster boot
}

/// Resolve VM memory size in MiB for VM backends.
///
/// Source precedence:
/// 1. RunOptions.vm_memory_mib (from --memory, already normalized to MiB)
/// 2. EPKG_VM_MEMORY as a human-readable size (e.g. "2048M", "2G") via parse_size_bytes_opt
/// 3. EPKG_VM_MEMORY parsed as plain MiB (u32) for backward compatibility
/// 4. Default: 4096 MiB
#[cfg(any(target_os = "linux", feature = "libkrun"))]
pub fn resolve_vm_memory_mib(run_options: &RunOptions) -> u32 {
    if let Some(mib) = run_options.vm_memory_mib {
        return mib;
    }
    std::env::var("EPKG_VM_MEMORY")
        .ok()
        .and_then(|s| {
            if let Some(bytes) = crate::utils::parse_size_bytes_opt(&s) {
                Some((bytes / (1024 * 1024)) as u32)
            } else {
                s.parse::<u32>().ok()
            }
        })
        .unwrap_or(4096)
}

/// Fail fast when `/dev/kvm` is missing or unusable so libkrun and QEMU (`-enable-kvm`) surface a
/// clear fix instead of stalling (vsock wait) or panicking (libkrun).
#[cfg(target_os = "linux")]
pub fn ensure_linux_kvm_ready_for_vm() -> Result<()> {
    use std::fs::OpenOptions;

    const KVM_DEVICE: &str = "/dev/kvm";
    if !Path::new(KVM_DEVICE).exists() {
        return Err(eyre::eyre!(
            "KVM is not available: {} is missing (VM backends need the host KVM device).\n\
             Typical fix (as root): `modprobe kvm` and `modprobe kvm_intel` or `modprobe kvm_amd` for your CPU.\n\
             If the file is still missing, enable virtualization in firmware/BIOS; \
             if this machine is already a VM, enable nested virtualization.",
            KVM_DEVICE
        ));
    }
    match OpenOptions::new().read(true).write(true).open(KVM_DEVICE) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Err(eyre::eyre!(
            "Cannot access {}: permission denied. Add your user to the `kvm` group, then re-login or `newgrp kvm`.",
            KVM_DEVICE
        )),
        Err(e) => Err(eyre::eyre!(
            "Cannot open {}: {}. VM backends require read/write access to KVM.",
            KVM_DEVICE,
            e
        )),
    }
}

/// No-op on non-Linux: KVM device exists only on Linux hosts. Kept for `libkrun` callers on macOS/Windows.
#[cfg(all(not(target_os = "linux"), feature = "libkrun"))]
pub fn ensure_linux_kvm_ready_for_vm() -> Result<()> {
    Ok(())
}

/// Minimum MiB required for libkrun: kernel is loaded at 0x2000_0000 (512 MiB), so RAM must be
/// at least 512 MiB + kernel size.
#[allow(dead_code)]
const LIBKRUN_KERNEL_LOAD_MIB: u32 = 512;
#[allow(dead_code)]
const LIBKRUN_MEMORY_SLACK_MIB: u32 = 64;

/// Round up VM memory for libkrun so the kernel fits. Libkrun loads the kernel at 2 GiB;
/// total RAM must be >= 2048 MiB + kernel_size. No extra floor (HostAddressNotAvailable
/// is a host address-space layout issue, not lack of RAM).
#[allow(dead_code)]
pub fn round_up_vm_memory_for_libkrun(requested_mib: u32, kernel_path: &str) -> u32 {
    let kernel_size_mib = lfs::metadata_on_host(kernel_path)
        .ok()
        .map(|m| (m.len() as u32 + (1024 * 1024) - 1) / (1024 * 1024))
        .unwrap_or(128);
    let min_mib = LIBKRUN_KERNEL_LOAD_MIB
        .saturating_add(kernel_size_mib)
        .saturating_add(LIBKRUN_MEMORY_SLACK_MIB);
    std::cmp::max(requested_mib, min_mib)
}

#[cfg(target_os = "linux")]
#[allow(dead_code)]
pub fn privdrop_on_suid() {
    if is_suid() {
        setuid(Uid::from_raw(get_current_uid())).expect("Failed to drop privileges");
    }
}

/// Kill child process when timeout occurs
#[cfg(target_os = "linux")]
fn kill_child_on_timeout(child: nix::unistd::Pid, cmd_path: &Path, timeout: u64) -> Result<()> {
    warn!("Command '{}' timed out after {} seconds, killing child process", cmd_path.display(), timeout);
    // Send SIGTERM first (graceful shutdown)
    if let Err(e) = signal::kill(child, Signal::SIGTERM) {
        warn!("Failed to send SIGTERM to child {}: {}", child, e);
    }

    // Wait a bit for graceful shutdown
    std::thread::sleep(Duration::from_millis(100));

    // Check if process is still alive and force kill if needed
    match nix::sys::wait::waitpid(child, Some(nix::sys::wait::WaitPidFlag::WNOHANG)) {
        Ok(nix::sys::wait::WaitStatus::StillAlive) => {
            // Process still running, force kill
            if let Err(e) = signal::kill(child, Signal::SIGKILL) {
                warn!("Failed to send SIGKILL to child {}: {}", child, e);
            }
        }
        _ => {
            // Process already terminated
        }
    }

    // Wait for the process to actually terminate
    let _ = nix::sys::wait::waitpid(child, None);
    Err(eyre::eyre!("Command '{}' timed out after {} seconds", cmd_path.display(), timeout))
}

/// Handle wait status result and process exit codes
#[cfg(target_os = "linux")]
fn handle_wait_status(wait_status: nix::sys::wait::WaitStatus, cmd_path: &Path, run_options: &RunOptions) -> Result<()> {
    use nix::sys::wait::WaitStatus;
    match wait_status {
        WaitStatus::Exited(_, exit_code) => {
            if exit_code != 0 {
                if run_options.no_exit {
                    eprintln!("Command '{}' exited with code {} (no_exit=true, continuing)", cmd_path.display(), exit_code);
                } else {
                    warn!("Child process exited with code {} (cmd: {})", exit_code, cmd_path.display());
                    // Instead of returning an error, just exit with the same code
                    std::process::exit(exit_code);
                }
            }
            Ok(())
        }
        WaitStatus::Signaled(_, signal, _) => {
            // SIGPIPE is expected when child writes to a closed pipe (e.g., rpm -qa | head)
            // According to SIGPIPE handling principles, treat this as normal exit
            if signal == Signal::SIGPIPE {
                debug!("Child process terminated by SIGPIPE (broken pipe) - treating as normal exit (cmd: {})", cmd_path.display());
                Ok(())
            } else {
                debug!("Child process killed by signal {:?} (cmd: {})", signal, cmd_path.display());
                Err(eyre::eyre!("Command killed by signal {:?}", signal))
            }
        }
        _ => {
            debug!("Child process ended with status: {:?} (cmd: {})", wait_status, cmd_path.display());
            Err(eyre::eyre!("Command ended with unexpected status: {:?}", wait_status))
        }
    }
}

/// Wait for child process with timeout using polling
#[cfg(target_os = "linux")]
fn wait_for_child_with_timeout_polling(child: nix::unistd::Pid, cmd_path: &Path, run_options: &RunOptions, timeout_duration: Duration) -> Result<()> {
    let start_time = Instant::now();

    loop {
        // Check if timeout has elapsed
        if start_time.elapsed() >= timeout_duration {
            return kill_child_on_timeout(child, cmd_path, run_options.timeout);
        }

        // Check if child has exited (non-blocking)
        match nix::sys::wait::waitpid(child, Some(nix::sys::wait::WaitPidFlag::WNOHANG)) {
            Ok(wait_status) => {
                use nix::sys::wait::WaitStatus;
                match wait_status {
                    WaitStatus::StillAlive => {
                        // Child still running, wait a bit and check again
                        std::thread::sleep(Duration::from_millis(100));
                        continue;
                    }
                    _ => {
                        return handle_wait_status(wait_status, cmd_path, run_options);
                    }
                }
            }
            Err(nix::errno::Errno::ECHILD) => {
                // Child already reaped, exit loop
                break;
            }
            Err(e) => {
                return Err(eyre::eyre!("Failed to wait for child process (cmd: {}): {}", cmd_path.display(), e));
            }
        }
    }

    Ok(())
}

/// Wait for child process to complete, with optional timeout
#[cfg(target_os = "linux")]
fn wait_for_child_with_timeout(child: nix::unistd::Pid, cmd_path: &Path, run_options: &RunOptions) -> Result<()> {
    trace!("Parent process waiting for child {} (cmd: {})", child, cmd_path.display());

    if run_options.timeout > 0 {
        // Handle timeout with polling
        let timeout_duration = Duration::from_secs(run_options.timeout);
        wait_for_child_with_timeout_polling(child, cmd_path, run_options, timeout_duration)
    } else {
        // No timeout, use blocking wait
        match nix::sys::wait::waitpid(child, None) {
            Ok(wait_status) => {
                handle_wait_status(wait_status, cmd_path, run_options)
            }
            Err(e) => {
                Err(eyre::eyre!("Failed to wait for child process (cmd: {}): {}", cmd_path.display(), e))
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn resolve_command_path(env_root: &Path, run_options: &RunOptions) -> Result<PathBuf> {
    if Path::new(&run_options.command).is_absolute() {
        Ok(PathBuf::from(&run_options.command))
    } else if run_options.command.contains('/') {
        Ok(PathBuf::from(&run_options.command))
    } else if lfs::exists_on_host(Path::new(&run_options.command)) {
        Ok(PathBuf::from(&run_options.command))
    } else {
        find_command_in_env_path(&run_options.command, env_root)
    }
}

#[cfg(not(target_os = "linux"))]
fn resolve_command_path(env_root: &Path, run_options: &RunOptions) -> Result<PathBuf> {
    // Check if we're in VM isolation mode - use different path resolution
    let is_vm_mode = run_options.effective_sandbox.isolate_mode == Some(IsolateMode::Vm);

    // On non-Linux platforms, Unix-style absolute paths (e.g., /usr/bin/sh from hooks)
    // need to be resolved within the environment, not treated as host paths.
    // Windows-style absolute paths (C:\...) are used as-is.
    let is_unix_absolute = run_options.command.starts_with('/');

    if is_unix_absolute {
        let cmd_path = PathBuf::from(&run_options.command);

        // First, check if the path is already under env_root (e.g., /Users/aa/.epkg/envs/debian/usr/bin/sh)
        // This happens when scriptlets.rs passes a full host path. In this case, return it directly.
        if cmd_path.starts_with(env_root) {
            debug!("Command {} is already under env_root, using directly", cmd_path.display());
            return Ok(cmd_path);
        }

        // On macOS, absolute paths like /Users/aa/... are host paths.
        // If the path exists on the host, use it directly.
        // Only treat paths like /usr/bin/... as Unix-style paths to resolve within env.
        if cmd_path.exists() {
            debug!("Command {} exists on host, using directly", cmd_path.display());
            return Ok(cmd_path);
        }

        // Unix-style absolute path: convert to environment path
        // /usr/bin/sh -> env_root/usr/bin/sh (or .exe on Windows)
        let relative_path = run_options.command.trim_start_matches('/');
        let cmd_path = env_root.join(relative_path);

        // On Windows, also try with .exe extension
        #[cfg(windows)]
        {
            if !cmd_path.exists() && !run_options.command.ends_with(".exe") {
                let cmd_with_exe = env_root.join(format!("{}.exe", relative_path));
                if cmd_with_exe.exists() {
                    return Ok(cmd_with_exe);
                }
            }
        }

        // For VM mode, use exists_in_env which handles symlinks correctly
        let exists = if is_vm_mode {
            lfs::exists_in_env(&cmd_path)
        } else {
            cmd_path.exists()
        };

        if exists {
            return Ok(cmd_path);
        }

        // For VM mode with Linux distro, accept path even if not found on host
        // The binary may be a Linux ELF that can't be checked on Windows host
        if is_vm_mode {
            debug!("VM mode: accepting Unix path {} (guest will resolve)", cmd_path.display());
            return Ok(cmd_path);
        }

        return Err(eyre::eyre!(
            "Command '{}' not found at {}",
            run_options.command,
            cmd_path.display()
        ));
    }

    // Windows-style absolute path (C:\...) - use as-is
    if Path::new(&run_options.command).is_absolute() {
        return Ok(PathBuf::from(&run_options.command));
    }

    // Relative path with '/' (e.g., bin/sh) - resolve within env_root
    if run_options.command.contains('/') {
        let cmd_path = env_root.join(&run_options.command);

        #[cfg(windows)]
        {
            if !cmd_path.exists() && !run_options.command.ends_with(".exe") {
                let cmd_with_exe = env_root.join(format!("{}.exe", &run_options.command));
                if cmd_with_exe.exists() {
                    return Ok(cmd_with_exe);
                }
            }
        }

        // For VM mode, use exists_in_env which handles symlinks correctly
        let exists = if is_vm_mode {
            lfs::exists_in_env(&cmd_path)
        } else {
            cmd_path.exists()
        };

        if exists {
            return Ok(cmd_path);
        }

        // For VM mode with Linux distro, accept path even if not found on host
        if is_vm_mode {
            debug!("VM mode: accepting relative path {} (guest will resolve)", cmd_path.display());
            return Ok(cmd_path);
        }
    }

    // For simple command names (no path separators), search in standard locations
    // Order: bin/ -> usr/bin/ -> Scripts/ -> Library/bin/ -> Library/mingw-w64/bin/ -> env_root
    //
    // Rationale:
    // - bin/ is preferred because some programs (like cmake) calculate
    //   their installation prefix based on the binary path.
    //   bin/cmake -> looks for ../share/cmake (correct)
    //   usr/bin/cmake -> looks for usr/share/cmake (wrong, should be ../../share/cmake)
    // - Scripts/ is used by conda on Windows for pip-installed scripts
    // - Library/bin/ is the standard conda location for Windows binaries (curl, etc.)
    // - Library/mingw-w64/bin/ is used by conda-forge for mingw-w64 packages (jq, etc.)
    // - env_root is used by conda on Windows for main executables (python.exe, etc.)

    // On Windows, also try with .exe extension
    #[cfg(windows)]
    let cmd_with_exe: String;
    #[cfg(windows)]
    let cmd_names: Vec<&str> = if run_options.command.ends_with(".exe") {
        vec![&run_options.command]
    } else {
        cmd_with_exe = format!("{}.exe", run_options.command);
        vec![&run_options.command, &cmd_with_exe]
    };
    #[cfg(not(windows))]
    let cmd_names: Vec<&str> = vec![&run_options.command];

    for cmd_name in cmd_names {
        let cmd_in_bin = env_root.join("bin").join(cmd_name);
        if lfs::exists_in_env(&cmd_in_bin) {
            return Ok(cmd_in_bin);
        }

        let cmd_in_usr_bin = crate::dirs::path_join(env_root, &["usr", "bin"]).join(cmd_name);
        if lfs::exists_in_env(&cmd_in_usr_bin) {
            return Ok(cmd_in_usr_bin);
        }

        // Check Scripts/ directory (conda on Windows)
        let cmd_in_scripts = env_root.join("Scripts").join(cmd_name);
        if cmd_in_scripts.exists() {
            return Ok(cmd_in_scripts);
        }

        // Check Library/bin/ (standard conda location on Windows)
        let cmd_in_library_bin = crate::dirs::path_join(env_root, &["Library", "bin"]).join(cmd_name);
        if cmd_in_library_bin.exists() {
            return Ok(cmd_in_library_bin);
        }

        // Check Library/mingw-w64/bin/ (conda-forge mingw packages on Windows)
        let cmd_in_mingw = crate::dirs::path_join(env_root, &["Library", "mingw-w64", "bin"]).join(cmd_name);
        if cmd_in_mingw.exists() {
            return Ok(cmd_in_mingw);
        }

        // Check env_root directly (conda on Windows places main executables at root)
        let cmd_in_root = env_root.join(cmd_name);
        if cmd_in_root.exists() {
            return Ok(cmd_in_root);
        }

        // Check MSYS2 MinGW subdirectories (ucrt64/bin, mingw64/bin, etc.)
        // Order follows MSYS2's default priority: ucrt64 > mingw64 > mingw32 > clang* > msys
        for msys2_prefix in &["ucrt64", "mingw64", "mingw32", "clang64", "clang32", "clangarm64"] {
            let cmd_in_msys2 = env_root.join(msys2_prefix).join("bin").join(cmd_name);
            if cmd_in_msys2.exists() {
                return Ok(cmd_in_msys2);
            }
        }
    }

    // For VM mode, if command not found in any standard location, return error
    // rather than a non-existent default path
    if is_vm_mode {
        return Err(eyre::eyre!(
            "Command '{}' not found in {} (checked: bin/, usr/bin/)",
            run_options.command,
            env_root.display()
        ));
    }

    if lfs::exists_on_host(Path::new(&run_options.command)) {
        Ok(PathBuf::from(&run_options.command))
    } else {
        Err(eyre::eyre!("Command '{}' not found in {}", run_options.command, env_root.display()))
    }
}

#[cfg(target_os = "linux")]
fn prepare_and_create_process(
    env_root: &Path,
    run_options: &RunOptions,
    stdin_read_fd: Option<i32>,
) -> Result<(nix::unistd::Pid, PathBuf, ProcessCreationConfig)> {
    let cmd_path = resolve_command_path(env_root, run_options)?;
    let config = determine_process_config(env_root, run_options);
    let context = build_unified_context(
        env_root,
        run_options,
        &config,
        cmd_path.clone(),
        run_options.args.clone(),
        stdin_read_fd,
    )?;

    let child_pid = create_process_with_namespaces(&config, context)?;
    Ok((child_pid, cmd_path, config))
}

// ============================================================================
// CALL GRAPH & CRITICAL PHASES
// ============================================================================
//
// High-level flow:
//   fork_and_execute() → fork_and_execute_raw() → prepare_and_create_process()
//
// prepare_and_create_process():
//   ├── determine_process_config()  → ProcessCreationConfig
//   ├── build_unified_context()     → UnifiedChildContext
//   └── create_process_with_namespaces()
//
// create_process_with_namespaces():
//   ├── IdMapSync setup (two cases):
//   │   1. Unshare: forked helper maps parent
//   │   2. Clone: parent maps child
//   └── Calls either:
//       • create_process_via_unshare()   (Unshare strategy)
//       • create_process_via_clone()     (Clone strategy)
//
// Unshare strategy:
//   create_process_via_unshare() → unshare_namespaces_with_idmap() → child_mount_and_exec()
//   - Namespaces created before child_mount_and_exec()
//   - child_mount_and_exec() only handles mounts and exec
//
// Clone strategy:
//   create_process_via_clone() → libc::clone() → unified_child_main() → child_setup_with_namespaces()
//   - Namespaces created at clone time, child_setup_with_namespaces() waits for sync if needed
//
// Critical phases:
//   1. Namespace creation (unshare() or clone())
//   2. UID/GID mapping (via newuidmap/newgidmap or simple mapping)
//   3. Mount setup (mount_batch_specs())
//   4. Command execution (prepare_and_execute_command())
//
// ============================================================================

/// Create a new child process using clone() and execute command with optional
/// namespace isolation. This replaces the previous fork()-based implementation.
///
/// The command path is derived from `run_options.command`:
/// - If `run_options.command` is already an absolute path, it's used directly
/// - Otherwise, PATH lookup is performed using `find_command_in_env_path()`
///
/// Returns:
/// - Ok(Some(pid)) for background processes (run_options.background = true)
/// - Ok(None) for foreground processes (waits for completion)
pub fn fork_and_execute(env_root: &Path, run_options: &RunOptions) -> Result<Option<i32>> {
    // Clone run_options to allow preparation
    let mut prepared_opts = run_options.clone();
    prepare_run_options_for_command(env_root, &mut prepared_opts);

    let isolate_mode = prepared_opts.effective_sandbox.isolate_mode
        .unwrap_or(IsolateMode::Env);

    match isolate_mode {
        IsolateMode::Vm => {
            crate::debug_epkg!("fork_and_execute: starting for VM mode");
            crate::debug_epkg!("fork_and_execute: options prepared, isolate_mode={:?}", prepared_opts.effective_sandbox.isolate_mode);
            crate::debug_epkg!("fork_and_execute: VM mode selected");

            // Check for existing VM session to reuse (cross-process discovery)
            #[cfg(feature = "libkrun")]
            if let Some(exit_code) = try_connect_and_execute_vm(env_root, &prepared_opts)? {
                log::info!("run: reused existing VM session, exit_code={}", exit_code);
                // For foreground processes, return Ok(None) on success, or error on failure
                // This matches the documented behavior: Ok(None) for foreground processes
                if exit_code == 0 {
                    return Ok(None);
                } else {
                    return Err(eyre::eyre!("Command exited with code {} in reused VM session", exit_code));
                }
            }

            crate::debug_epkg!("fork_and_execute: resolving command path");
            let cmd_path = resolve_command_path(env_root, &prepared_opts)?;
            crate::debug_epkg!("fork_and_execute: command path resolved: {:?}", cmd_path);

            // Add EPKG_ACTIVE_ENV and EPKG_ENV_ROOT for VM guest process
            // This is critical for nested epkg calls to know the current environment
            // In VM guest, ~/.epkg is mounted at /opt/epkg, so we need to convert paths
            let env_name = config().common.env_name.clone();
            if !env_name.is_empty() {
                prepared_opts.env_vars.insert("EPKG_ACTIVE_ENV".to_string(), env_name.clone());
                // Convert host env_root to guest path: ~/.epkg/envs/NAME -> /opt/epkg/envs/NAME
                let home_epkg = crate::models::dirs().home_epkg.clone();
                let guest_env_root = if let Ok(stripped) = env_root.strip_prefix(&home_epkg) {
                    let s = stripped.to_string_lossy();
                    let prefix = if s.starts_with('/') { "/opt/epkg" } else { "/opt/epkg/" };
                    format!("{}{}", prefix, s)
                } else {
                    env_root.display().to_string()
                };
                prepared_opts.env_vars.insert("EPKG_ENV_ROOT".to_string(), guest_env_root.clone());
                debug!("Added EPKG_ACTIVE_ENV={} EPKG_ENV_ROOT={} (converted from {}) for VM execution",
                       env_name, guest_env_root, env_root.display());
            }

            // Convert host path to guest path (strip env_root prefix if inside)
            let guest_cmd_path = if let Ok(stripped) = cmd_path.strip_prefix(env_root) {
                Path::new("/").join(stripped)
            } else {
                cmd_path.clone()
            };

            crate::debug_epkg!("Guest command path: {}", guest_cmd_path.display());

            // Use VMM backend selection that respects --vmm option
            // On Linux: use vmm::try_vmm_backends which tries backends in vmm_order
            // On non-Linux: use direct libkrun call
            #[cfg(target_os = "linux")]
            {
                crate::debug_epkg!("fork_and_execute: calling try_vmm_backends with order {:?}...", prepared_opts.vmm_order);
                crate::vmm::try_vmm_backends(
                    env_root,
                    &prepared_opts,
                    &guest_cmd_path,
                    None,
                    &prepared_opts.vmm_order,
                    prepared_opts.vm_reuse_connect,
                )?;
            }
            #[cfg(not(target_os = "linux"))]
            {
                #[cfg(feature = "libkrun")]
                {
                    crate::debug_epkg!("fork_and_execute: calling run_command_in_krun...");
                    crate::libkrun::run_command_in_krun(env_root, &prepared_opts, &guest_cmd_path)?;
                }
                #[cfg(not(feature = "libkrun"))]
                {
                    return Err(eyre::eyre!(
                        "VM sandbox requires libkrun feature. \
                         Recompile epkg with libkrun support for --isolate=vm"
                    ));
                }
            }
            Ok(None) // unreachable, but needed for type consistency
        }
        IsolateMode::Env | IsolateMode::Fs => {
            // For conda/homebrew/msys2 packages, they work like portable apps
            // with their own library paths (RPATH), so we can run them directly
            // from the host OS without namespace isolation
            if prepared_opts.skip_namespace_isolation {
                fork_and_execute_direct(env_root, &prepared_opts)
            } else {
                // Execute with prepared options (handles both Clone and Unshare strategies)
                fork_and_execute_raw(env_root, &prepared_opts)
            }
        }
    }
}

/// On Windows, prepend conda env dirs to PATH so MSVC/conda binaries resolve.
/// Build PATH with conda directories first, then Windows system directories so MSVC binaries
/// (curl, wget, etc.) find their DLLs without interference from MSYS2/MinGW runtime DLLs.
///
/// Order: env_root (vcruntime*.dll), bin/usr/bin/Scripts, Library/bin, Library/mingw-w64/bin,
/// Windows system dirs, then original PATH.
#[cfg(all(not(target_os = "linux"), windows))]
fn conda_windows_path_env(env_root: &Path) -> String {
    let library_bin = env_root.join("Library").join("bin");
    let mingw_bin = env_root.join("Library").join("mingw-w64").join("bin");
    let scripts_bin = env_root.join("Scripts");
    let usr_bin = env_root.join("usr").join("bin");
    let bin_dir = env_root.join("bin");

    let mut path_dirs = vec![
        env_root.display().to_string(),
        bin_dir.display().to_string(),
        usr_bin.display().to_string(),
        scripts_bin.display().to_string(),
        library_bin.display().to_string(),
        mingw_bin.display().to_string(),
        "C:\\Windows\\System32".to_string(),
        "C:\\Windows".to_string(),
    ];

    path_dirs.push(std::env::var("PATH").unwrap_or_default());
    path_dirs.join(";")
}

/// MSYS2-style pacman env: prepend merged bin paths so .exe and MinGW DLLs resolve.
#[cfg(all(not(target_os = "linux"), windows))]
fn msys2_pacman_path_env(env_root: &Path) -> String {
    let bin_dir = env_root.join("bin");
    let usr_bin = env_root.join("usr").join("bin");
    let original_path = std::env::var("PATH").unwrap_or_default();
    [
        bin_dir.display().to_string(),
        usr_bin.display().to_string(),
        original_path,
    ]
    .join(";")
}

/// Execute command directly on the host without namespace isolation.
/// Used for conda/homebrew/msys2 packages that have RPATH and can run natively.
fn fork_and_execute_direct(env_root: &Path, run_options: &RunOptions) -> Result<Option<i32>> {
    use std::process::{Command, Stdio};

    let cmd_path = resolve_command_path(env_root, run_options)?;

    // Convert guest path to host path if needed.
    // When skip_namespace_isolation is true, resolve_command_path may return a guest path
    // (e.g., /usr/bin/curl) but we need the full host path for direct execution.
    // However, if the path already exists on the host (e.g., /Users/aa/... on macOS),
    // use it directly without conversion.
    let cmd_path = if cmd_path.is_absolute() && !cmd_path.starts_with(env_root) && !cmd_path.exists() {
        // This is a guest path - convert to host path
        let host_path = env_root.join(cmd_path.strip_prefix("/").unwrap_or(&cmd_path));
        debug!("Converting guest path {} to host path {}", cmd_path.display(), host_path.display());
        host_path
    } else {
        cmd_path
    };

    debug!("Running command directly on host: {}", cmd_path.display());
    debug!("Args: {:?}", run_options.args);

    // Without mount namespaces, exec of a glibc ELF fails (host/guest dynamic linker path). The
    // e2e microVM cannot nest clone/unshare; run via the environment's ld-linux when present.
    #[cfg(target_os = "linux")]
    let use_env_ld_linux = crate::utils::e2e_backend_is_vm()
        && cmd_path.starts_with(env_root)
        && {
            let ld = env_root.join("lib64").join("ld-linux-x86-64.so.2");
            ld.is_file()
        };
    #[cfg(not(target_os = "linux"))]
    let use_env_ld_linux = false;

    // Build the command
    let mut cmd = if use_env_ld_linux {
        let ld = env_root.join("lib64").join("ld-linux-x86-64.so.2");
        debug!("E2E_BACKEND=vm: running via dynamic linker {}", ld.display());
        let mut c = Command::new(&ld);
        c.arg(&cmd_path);
        c.args(&run_options.args);
        c
    } else {
        let mut c = Command::new(&cmd_path);
        c.args(&run_options.args);
        c
    };

    // Set up environment variables
    let mut env_vars = run_options.env_vars.clone();

    let ch = crate::models::channel_config();
    let channel_format = ch.format;

    if channel_format == crate::models::PackageFormat::Conda {
        env_vars.insert("CONDA_PREFIX".to_string(), env_root.display().to_string());

        #[cfg(windows)]
        env_vars.insert("PATH".to_string(), conda_windows_path_env(env_root));
    }

    #[cfg(windows)]
    if channel_format == crate::models::PackageFormat::Pacman && ch.distro == "msys2" {
        env_vars.insert("PATH".to_string(), msys2_pacman_path_env(env_root));
    }

    // Note: Brew packages use absolute paths rewritten at link time (LinkType::Move),
    // so no DYLD_LIBRARY_PATH is needed. However, we need to add ebin to PATH
    // so that scripts can find commands from the environment.
    if channel_format == crate::models::PackageFormat::Brew {
        let ebin_path = env_root.join("ebin");
        if ebin_path.exists() {
            let current_path = std::env::var("PATH").unwrap_or_default();
            let new_path = format!("{}:{}", ebin_path.display(), current_path);
            env_vars.insert("PATH".to_string(), new_path);
            debug!("Added ebin to PATH: {}", ebin_path.display());
        }
    }

    // Apply environment variables
    for (key, value) in &env_vars {
        cmd.env(key, value);
    }

    #[cfg(target_os = "linux")]
    if use_env_ld_linux {
        let prefix = format!("{}/lib64:{}/usr/lib64", env_root.display(), env_root.display());
        let merged = env_vars
            .get("LD_LIBRARY_PATH")
            .map(|e| format!("{}:{}", prefix, e))
            .unwrap_or(prefix);
        cmd.env("LD_LIBRARY_PATH", merged);
    }

    // Set working directory if requested
    if run_options.chdir_to_env_root {
        cmd.current_dir(env_root);
    }

    // Handle stdin
    if run_options.stdin.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::inherit());
    }

    // Handle stdio redirection for background/daemon processes
    if run_options.redirect_stdio {
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());
    } else {
        cmd.stdout(Stdio::inherit());
        cmd.stderr(Stdio::inherit());
    }

    // Spawn the process
    let mut child = cmd.spawn()
        .map_err(|e| eyre::eyre!("Failed to spawn command '{}': {}", cmd_path.display(), e))?;

    // Write stdin if provided
    if let Some(stdin_data) = &run_options.stdin {
        use std::io::Write;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(stdin_data)
                .map_err(|e| eyre::eyre!("Failed to write to stdin: {}", e))?;
        }
    }

    if run_options.background {
        // Return the PID for background processes
        let pid = child.id() as i32;
        debug!("Background process started with PID: {}", pid);
        Ok(Some(pid))
    } else {
        // Wait for completion with optional timeout (polling-based)
        let result = if run_options.timeout > 0 {
            let timeout_duration = std::time::Duration::from_secs(run_options.timeout);
            let start_time = std::time::Instant::now();
            let mut status_opt = None;

            while start_time.elapsed() < timeout_duration {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        status_opt = Some(status);
                        break;
                    }
                    Ok(None) => {
                        // Child still running, wait a bit and check again
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                    Err(e) => {
                        return Err(eyre::eyre!("Failed to check child status: {}", e));
                    }
                }
            }

            if let Some(status) = status_opt {
                status
            } else {
                // Timeout occurred
                let _ = child.kill();
                return Err(eyre::eyre!(
                    "Command '{}' timed out after {} seconds",
                    cmd_path.display(),
                    run_options.timeout
                ));
            }
        } else {
            child.wait()
                .map_err(|e| eyre::eyre!("Failed to wait for child: {}", e))?
        };

        // Handle exit code
        if let Some(code) = result.code() {
            if code != 0 && !run_options.no_exit {
                std::process::exit(code);
            }
        }

        Ok(None)
    }
}

#[cfg(target_os = "linux")]
fn fork_and_execute_raw(env_root: &Path, run_options: &RunOptions) -> Result<Option<i32>> {
    // Create stdin pipe if needed (same logic as original fork_and_execute)
    let stdin_bytes = run_options.stdin.as_ref().map(|v| v.as_slice());
    let (mut stdin_read_fd_opt, stdin_write_fd_opt) = create_stdin_pipe_if_needed(run_options)?;

    // Use shared helper to create process
    let (child_pid, cmd_path, _config) = prepare_and_create_process(
        env_root,
        run_options,
        stdin_read_fd_opt.as_ref().map(|fd| fd.as_raw_fd()),
    )?;

    // Parent: close read end of stdin pipe and write data, if any
    if let (Some(bytes), Some(write_fd)) = (stdin_bytes, stdin_write_fd_opt) {
        if let Some(read_fd) = stdin_read_fd_opt.take() {
            if let Err(e) = close(read_fd) {
                trace!("Failed to close child stdin read fd in parent: {}", e);
            }
        }
        with_sigpipe_handler(libc::SIG_IGN, move || {
            let mut written = 0;
            while written < bytes.len() {
                match write(&write_fd, &bytes[written..]) {
                    Ok(0) => break,
                    Ok(n) => written += n,
                    Err(e) => {
                        if e == Errno::EPIPE {
                            break;
                        }
                        let _ = close(write_fd);
                        return Err(eyre::eyre!("Failed to write to child stdin: {}", e));
                    }
                }
            }
            let _ = close(write_fd);
            Ok(())
        })?;
    }

    if run_options.background {
        Ok(Some(child_pid.as_raw() as i32))
    } else {
        wait_for_child_with_timeout(child_pid, &cmd_path, run_options)?;
        Ok(None)
    }
}

// Note: fork_and_execute_raw is only called on Linux.
// This stub exists for completeness but is never used on non-Linux platforms.
#[cfg(not(target_os = "linux"))]
#[allow(dead_code)]
fn fork_and_execute_raw(_env_root: &Path, _run_options: &RunOptions) -> Result<Option<i32>> {
    use color_eyre::eyre;
    Err(eyre::eyre!("fork_and_execute_raw not implemented for this platform"))
}

/// Check if a file is executable
#[cfg(unix)]
pub fn is_executable(path: &Path) -> Result<bool> {
    trace!("is_executable checking: {}", path.display());
    let metadata = lfs::symlink_metadata(path)
        .map_err(|e| {
            trace!("is_executable metadata error for {}: {}", path.display(), e);
            eyre::eyre!("Failed to get metadata for {}: {}", path.display(), e)
        })?;

    let permissions = metadata.permissions();
    let executable = permissions.mode() & 0o111 != 0;
    trace!("is_executable result for {}: {}", path.display(), executable);
    Ok(executable)
}

/// Check if a file is executable, handling symlinks that may point to targets within environment root.
/// Returns the resolved path if executable, or None if not.
#[cfg(unix)]
fn is_executable_within_env(path: &Path, env_root: &Path) -> Result<Option<PathBuf>> {
    trace!("is_executable_within_env checking: {}", path.display());

    match lfs::resolve_symlink_in_env(path, env_root) {
        Some(resolved) => {
            trace!("Resolved {} -> {}", path.display(), resolved.display());
            if is_executable(&resolved)? {
                Ok(Some(resolved))
            } else {
                Ok(None)
            }
        }
        None => {
            trace!("Path {} cannot be resolved within environment root", path.display());
            Ok(None)
        }
    }
}

/// Find command in environment PATH and return the guest path (path inside namespace).
///
/// This function performs PATH lookup for a command name within the environment root.
/// It searches each PATH directory, checks if the command exists and is executable
/// within the environment, and returns the **guest path** (e.g., `/usr/bin/go`)
/// rather than the host path (e.g., `$env_root/usr/bin/go`).
///
/// # Why guest path?
/// The environment uses namespace isolation where `$env_root/usr` is bind-mounted to `/usr`.
/// When the child process executes, it sees `/usr/bin/go`, not the full host path.
/// Returning the guest path ensures the command can be found inside the namespace.
///
/// # Arguments
/// * `cmd_name` - The command name to search for (e.g., "go", "python")
/// * `env_root` - The root directory of the environment
///
/// # Returns
/// * `Ok(PathBuf)` - The guest path to the command (e.g., `/usr/bin/go`)
/// * `Err` - Command not found in environment PATH
///
/// # Example
/// ```
/// // Host path: /home/user/.epkg/envs/myenv/usr/bin/go
/// // Guest path: /usr/bin/go (returned)
/// let guest_path = find_command_in_env_path("go", env_root)?;
/// ```
#[cfg(unix)]
pub fn find_command_in_env_path(cmd_name: &str, env_root: &Path) -> Result<PathBuf> {
    // Check if this is a brew environment
    let is_brew_env = is_brew_environment(env_root);

    // Collect non-empty PATH directories; if none, use default system paths
    let path_str = env::var("PATH").unwrap_or_default();
    let mut dirs: Vec<&str> = path_str.split(':').filter(|d| !d.is_empty()).collect();
    if dirs.is_empty() {
        dirs.extend(["/usr/bin", "/bin", "/usr/sbin", "/sbin"]);
    }

    // For brew environments, also check the root bin/ and libexec/bin/ directories
    // since brew packages don't follow the standard FHS layout
    let brew_paths = if is_brew_env {
        vec!["bin", "libexec/bin"]
    } else {
        vec![]
    };

    // First check brew-specific paths for brew environments
    for subdir in &brew_paths {
        let cmd_path = env_root.join(subdir).join(cmd_name);
        trace!("find_command_in_env_path: checking brew path {:?}", cmd_path);
        if let Some(resolved_path) = is_executable_within_env(&cmd_path, env_root)? {
            if resolved_path.starts_with(env_root) {
                let guest_rel = resolved_path.strip_prefix(env_root).unwrap_or(&resolved_path);
                // For brew environments, prepend HOMEBREW_PREFIX to the guest path
                // so the command is found at the correct location inside the namespace
                let homebrew_prefix = crate::brew_pkg::prefix::preferred_path();
                let guest_path = homebrew_prefix.join(guest_rel);
                return Ok(guest_path);
            }
        }
    }

    // Then check standard PATH directories
    for path_dir in dirs {
        trace!("find_command_in_env_path: checking path_dir={}", path_dir);
        // Skip paths ending with "/ebin"
        // ebin contains elf-loader binaries for running from host, not from inside environment
        if path_dir.ends_with("/ebin") {
            continue;
        }

        // Check if this path is within the environment root
        let rel_path = path_dir.strip_prefix("/").unwrap_or(path_dir);
        let cmd_path = env_root.join(rel_path).join(cmd_name);
        trace!("find_command_in_env_path: cmd_path={:?}", cmd_path);

        if let Some(resolved_path) = is_executable_within_env(&cmd_path, env_root)? {
            // Use the resolved path (which may be different from cmd_path due to symlinks)
            // e.g., bin/go -> libexec/bin/go, we should use libexec/bin/go
            if resolved_path.starts_with(env_root) {
                // Strip env_root prefix to get the guest path (e.g., /libexec/bin/go)
                let guest_rel = resolved_path.strip_prefix(env_root).unwrap_or(&resolved_path);
                let guest_path = PathBuf::from("/").join(guest_rel);
                return Ok(guest_path);
            }
        }
    }
    Err(eyre::eyre!("Command '{}' not found in environment PATH under {}", cmd_name, env_root.display()))
}

/// Check if current environment is a brew environment
/// Uses global channel_config().format for O(1) access instead of deserializing
#[cfg(unix)]
pub fn is_brew_environment(_env_root: &Path) -> bool {
    crate::models::channel_config().format == crate::models::PackageFormat::Brew
}

/// Check if the host OS uses traditional directory layout (dirs) or usr-merge layout (symlinks).
/// Returns true if the host uses traditional layout (e.g., Alpine < 3.22), false if usr-merge.
#[cfg(target_os = "linux")]
pub fn host_uses_traditional_layout() -> bool {
    // Check if /lib is a directory (traditional) or symlink (usr-merge)
    let lib_path = Path::new("/lib");
    if let Ok(metadata) = fs::symlink_metadata(lib_path) {
        return metadata.file_type().is_dir();
    }
    // If we can't check, assume traditional layout to be safe
    true
}

/// Merge sandbox options from multiple sources with priority.
/// Higher priority options override lower priority ones.
/// Mount directories are combined (additive) from all sources.
///
/// # Priority Order (highest to lowest):
/// 1. RunOptions.sandbox - CLI input / per-run settings (highest priority)
/// 2. EPKGConfig.sandbox - User defaults from ~/.config/epkg/options.yaml
/// 3. EnvConfig.sandbox - Environment defaults from env_root/etc/epkg/env.yaml (lowest priority)
///
/// # Merging Behavior:
/// - isolate_mode: Override - higher priority completely replaces lower priority
/// - mount_specs: Additive - directories from all sources are combined
///
/// # Example:
/// If user defaults have isolate_mode=Env and mount_specs=["/home"],
/// env defaults have isolate_mode=Fs and mount_specs=["/tmp"],
/// and CLI specifies mount_specs=["/data"], the result will be:
/// isolate_mode=Fs (from env), mount_specs=["/home", "/tmp", "/data"] (combined)
fn merge_sandbox_options(sources: &[&crate::models::SandboxOptions]) -> crate::models::SandboxOptions {
    let mut result = crate::models::SandboxOptions::default();

    // Process sources in provided order (lowest to highest priority)
    // so later (higher-priority) sources override earlier ones.
    for source in sources {
        if let Some(mode) = source.isolate_mode {
            result.isolate_mode = Some(mode);
        }
        if let Some(strategy) = source.namespace_strategy {
            result.namespace_strategy = Some(strategy);
        }

        // Mount spec strings are additive - combine from all sources
        result.mount_specs
            .extend(source.mount_specs.iter().cloned());
    }

    result
}

fn prepare_run_options_for_command(env_root: &Path, run_options: &mut RunOptions) {
    let config_guard = config();

    // Load configuration sources in priority order (lowest to highest)
    let sources = vec![
        &config_guard.sandbox, // User defaults (lowest priority)
        &env_config().sandbox, // Environment defaults
        &run_options.sandbox,  // CLI/run input (highest priority)
    ];

    // Merge all sandbox options
    run_options.effective_sandbox = merge_sandbox_options(&sources);

    // Set default sandbox mode if none specified
    if run_options.effective_sandbox.isolate_mode.is_none() {
        run_options.effective_sandbox.isolate_mode = Some(crate::models::IsolateMode::Env);
    }

    // Normalise skip_namespace_isolation based on channel and environment context.
    // Load channel config from the target environment (not the global default)
    // to correctly determine the package format for VM mode auto-selection.
    let channel_configs = crate::io::deserialize_channel_config_from_root(&env_root.to_path_buf())
        .unwrap_or_default();
    let ch = channel_configs.first();
    let (channel_format, distro) = ch.map(|c| (c.format, c.distro.clone()))
        .unwrap_or((crate::models::PackageFormat::Apk, "alpine".to_string()));
    let is_conda = channel_format == crate::models::PackageFormat::Conda;
    let _is_brew = channel_format == crate::models::PackageFormat::Brew;
    let is_msys2 = channel_format == crate::models::PackageFormat::Pacman && distro == "msys2";
    let is_linux_format = is_linux_package_format(channel_format, &distro);

    // For brew packages, we use namespace isolation with HOMEBREW_PREFIX bind mount.
    // The namespace setup (env_mount_spec_strings) handles brew specially by mounting
    // $env_root to HOMEBREW_PREFIX instead of standard /usr, /bin mounts.
    if is_conda || is_msys2 {
        // conda ELF binary has RPATH; MSYS2/MinGW binaries are native Windows PE
        run_options.skip_namespace_isolation = true;
    }

    // On Windows/macOS, Linux-format packages require VM sandbox.
    // Auto-enable IsolateMode::Vm if not explicitly set by user.
    #[cfg(not(target_os = "linux"))]
    if is_linux_format && run_options.sandbox.isolate_mode.is_none() {
        debug!("Auto-enabling VM sandbox for Linux package format: {:?}/{}",
               channel_format, distro);
        run_options.effective_sandbox.isolate_mode = Some(IsolateMode::Vm);
    }

    // VM reuse is mandatory for data integrity - only ONE VM per env_root.
    // This prevents concurrent host/guest file operations from corrupting data.
    // Always check for existing VM session first and reuse it if available.
    let _env_name = &config().common.env_name;
    #[cfg(not(target_os = "linux"))]
    let has_active_vm_session = is_vm_reuse_active_for_env(env_root) ||
        crate::vm::is_vm_session_active(_env_name);
    #[cfg(target_os = "linux")]
    let has_active_vm_session = is_vm_reuse_active_for_env(env_root);

    // If VM mode is active or an existing session exists, always enable reuse.
    // This ensures scriptlets/hooks reuse the same VM during install/upgrade.
    // VM reuse is mandatory for data integrity - only ONE VM per env_root.
    #[cfg(not(target_os = "linux"))]
    if has_active_vm_session ||
       run_options.effective_sandbox.isolate_mode == Some(IsolateMode::Vm) {
        run_options.reuse_vm = true;
    }

    // Silence unused warning on Linux
    #[cfg(target_os = "linux")]
    let _ = has_active_vm_session;

    // If vm_keep_timeout is set, enable reuse_vm mode to keep the VM alive
    // after the command completes.
    if run_options.vm_keep_timeout.is_some() {
        run_options.reuse_vm = true;
    }

    // Silence unused warning on Linux
    #[cfg(target_os = "linux")]
    let _ = is_linux_format;

    // When running directly inside env_root or with env_root=/, always bypass namespace isolation.
    if config_guard.common.in_env_root || env_root.as_os_str() == "/" {
        run_options.skip_namespace_isolation = true;
    }

    // Allow bypassing namespace isolation via environment variable for testing
    if std::env::var("EPKG_SKIP_NAMESPACE").is_ok() {
        run_options.skip_namespace_isolation = true;
    }

    // Nested under `epkg run --isolate=vm` (e2e guest): cannot create nested user/mount namespaces
    // (clone/unshare EPERM). `fork_and_execute_direct` runs the target via env `ld-linux` when needed.
    if crate::utils::e2e_backend_is_vm() && config_guard.subcommand == EpkgCommand::Run {
        run_options.skip_namespace_isolation = true;
    }
}

/// Check if the package format is a Linux format that requires VM on non-Linux hosts.
///
/// On Windows/macOS, Linux-format packages (deb/rpm/arch/apk) cannot run natively
/// and require a Linux VM via libkrun.
///
/// # Arguments
/// * `format` - The package format (e.g., Deb, Rpm, Apk, Pacman, Conda, Brew)
/// * `distro` - The distro name (e.g., "debian", "fedora", "arch", "msys2")
///
/// # Returns
/// `true` if the package is a Linux format that requires VM on non-Linux hosts.
fn is_linux_package_format(format: crate::models::PackageFormat, distro: &str) -> bool {
    use crate::models::PackageFormat;
    match format {
        PackageFormat::Deb |
        PackageFormat::Rpm |
        PackageFormat::Apk => true,
        PackageFormat::Pacman => {
            // Arch Linux requires VM, but MSYS2 is native Windows
            distro != "msys2"
        }
        PackageFormat::Epkg |
        PackageFormat::Conda |
        PackageFormat::Brew |
        PackageFormat::Python => false,
    }
}

#[cfg(target_os = "linux")]
fn create_stdin_pipe_if_needed(run_options: &RunOptions) -> Result<(Option<OwnedFd>, Option<OwnedFd>)> {
    if let Some(_) = &run_options.stdin {
        let (read_fd, write_fd) = pipe()
            .map_err(|e| eyre::eyre!("Failed to create stdin pipe: {}", e))?;
        Ok((Some(read_fd), Some(write_fd)))
    } else {
        Ok((None, None))
    }
}

/// Execute command with environment PATH lookup and namespace isolation
#[cfg(target_os = "linux")]
pub fn command_run(_sub_matches: &clap::ArgMatches) -> Result<()> {
    let run_options = config().run.clone();

    debug!("Running command: {} with args: {:?}", run_options.command, run_options.args);
    debug!("Sandbox input: {:?}, User: {:?}", run_options.sandbox, run_options.user);

    let env_root = crate::dirs::get_default_env_root()?;
    info!("Using environment root: {}", env_root.display());

    fork_and_execute(&env_root, &run_options)?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn command_run(_sub_matches: &clap::ArgMatches) -> Result<()> {
    let run_options = config().run.clone();

    debug!("Running command: {} with args: {:?}", run_options.command, run_options.args);
    debug!("Sandbox input: {:?}", run_options.sandbox);

    let env_root = crate::dirs::get_default_env_root()?;
    debug!("Using environment root: {}", env_root.display());

    fork_and_execute(&env_root, &run_options)?;
    Ok(())
}

/// Execute built-in command (busybox-style)
///
/// This function handles applet execution when invoked via `epkg busybox <applet>`.
/// It supports two modes of operation:
///
/// 1. **External subcommand mode** (current implementation):
///    - The `busybox` command uses `allow_external_subcommands(true)` to avoid
///      option name conflicts between epkg's global options and applet-specific options.
///    - Applet arguments arrive as raw `OsString` values (key `""` in matches).
///    - We manually re-parse these arguments using each applet's command parser.
///
/// 2. **Registered subcommand mode** (alternative approach):
///    - Applet subcommands are registered directly under `busybox`.
///    - Arguments are already parsed by clap before reaching this function.
///    - This mode causes option name conflicts when applet options overlap with
///      epkg's global options (e.g., `ls -q` vs global `-q --quiet`).
///
/// The current implementation uses external subcommand mode to isolate option
/// namespaces and prevent conflicts. When `get_raw("")` returns arguments,
/// we parse them using the applet's command parser via `try_get_matches_from()`.
/// Otherwise, we assume arguments are already parsed (registered subcommand mode).
///
/// # Arguments
/// * `sub_matches` - Parsed command-line arguments for the `busybox` subcommand
///
/// # Returns
/// * `Result<()>` - Success or error from applet execution
pub fn command_busybox(sub_matches: &clap::ArgMatches) -> Result<()> {
    // Handle --list flag
    if sub_matches.get_flag("list") {
        println!("{}", crate::busybox::sorted_applet_names().join("\n"));
        return Ok(());
    }

    /* Parse the subcommand structure:
     * - Some((cmd_name, cmd_matches)): A subcommand was specified
     * - None: No subcommand specified (error case)
     */
    match sub_matches.subcommand() {
        Some((cmd_name, cmd_matches)) => {
            let known = crate::busybox::busybox_subcommands()
                .iter()
                .any(|c| c.get_name() == cmd_name);
            if known {
                debug!("Running built-in command: {}", cmd_name);
                // Find the applet command
                let applet_cmd = crate::busybox::busybox_subcommands()
                    .into_iter()
                    .find(|c| c.get_name() == cmd_name)
                    .expect("Applet command should exist");

                // Check if we have external subcommand arguments (when using allow_external_subcommands)
                // or if the matches are already parsed by the applet's command parser
                if let Some(raw_args) = cmd_matches.get_raw("") {
                    /* External subcommand mode:
                     * - Arguments arrive as raw OsString values (key "" in matches)
                     * - We need to re-parse them using the applet's command parser
                     * - This avoids option name conflicts with global epkg options
                     */
                    // External subcommand: parse arguments manually
                    let args_vec: Vec<std::ffi::OsString> = raw_args.map(|s| s.to_os_string()).collect();
                    debug!("Parsing external args for {}: {:?}", cmd_name, args_vec);

                    // Build argument list: program name (dummy) + arguments
                    let mut all_args = vec![std::ffi::OsString::from("epkg")];
                    all_args.extend(args_vec.clone());

                    // Parse arguments using the applet's command parser
                    match applet_cmd.clone().try_get_matches_from(all_args) {
                        Ok(parsed_matches) => {
                            crate::busybox::exec_builtin_command(cmd_name, &parsed_matches)
                        }
                        Err(e) => {
                            // If parsing fails, print error and exit with appropriate code
                            let args_display: Vec<String> = args_vec.iter().map(|a| a.to_string_lossy().into_owned()).collect();
                            let cmdline = if args_display.is_empty() {
                                format!("epkg busybox {}", cmd_name)
                            } else {
                                format!("epkg busybox {} {}", cmd_name, args_display.join(" "))
                            };
                            crate::utils::handle_clap_error_with_cmdline(e, cmdline);
                        }
                    }
                } else {
                    /* Registered subcommand mode:
                     * - Applet subcommand is registered directly under busybox
                     * - Arguments are already parsed by clap
                     * - This mode would cause option name conflicts if used
                     */
                    // Matches are already parsed by applet command parser (when subcommands are registered)
                    crate::busybox::exec_builtin_command(cmd_name, cmd_matches)
                }
            } else {
                /* Unknown applet:
                 * - Command name doesn't match any registered applet
                 * - Print error and exit with busybox-style exit code (127)
                 */
                eprintln!("{}: applet not found", cmd_name);
                std::process::exit(127);
            }
        }
        None => {
            /* No subcommand specified:
             * - User ran `epkg busybox` without an applet name
             * - Return error (clap should have prevented this with arg_required_else_help)
             */
            Err(eyre::eyre!("No command specified"))
        }
    }
}
pub fn parse_options_run(options: &mut EPKGConfig, sub_matches: &clap::ArgMatches) -> Result<()> {
    // Parse flexible mount specifications
    let mut mount_specs = Vec::new();
    if let Some(values) = sub_matches.get_many::<String>("mount") {
        mount_specs.extend(values.cloned());
    }

    let user = sub_matches.get_one::<String>("user").cloned();

    let command = sub_matches.get_one::<String>("command")
        .ok_or_else(|| eyre::eyre!("Command is required"))?
        .clone();

    if command.starts_with('-') {
        return Err(eyre::eyre!(
            "Command looks like an option ('{}'). Put the command after options, e.g. \
             epkg run --isolate=vm --vmm=qemu --io=stream -- whoami \
             or epkg run whoami --isolate=vm --vmm=qemu",
            command
        ));
    }

    let args: Vec<String> = if let Some(args_iter) = sub_matches.get_many::<String>("args") {
        args_iter.cloned().collect()
    } else {
        Vec::new()
    };

    let timeout = if let Some(timeout_str) = sub_matches.get_one::<String>("timeout") {
        timeout_str.parse::<u64>()
            .map_err(|e| eyre::eyre!("Invalid timeout value '{}': {}", timeout_str, e))?
    } else {
        0 // Default: no timeout
    };

    let kernel = sub_matches
        .get_one::<String>("kernel")
        .cloned();

    let vm_cpus = if let Some(cpus_str) = sub_matches.get_one::<String>("cpus") {
        Some(
            cpus_str
                .parse::<u8>()
                .map_err(|e| eyre::eyre!("Invalid --cpus value '{}': {}", cpus_str, e))?,
        )
    } else {
        None
    };

    let vm_memory_mib = if let Some(mem_str) = sub_matches.get_one::<String>("memory") {
        let mib = if let Some(bytes) = crate::utils::parse_size_bytes_opt(mem_str) {
            (bytes / (1024 * 1024)) as u32
        } else {
            mem_str.parse::<u32>().map_err(|e| {
                eyre::eyre!(
                    "Invalid --memory value '{}': {} (expected size like 4096M or MiB integer)",
                    mem_str,
                    e
                )
            })?
        };
        Some(mib)
    } else {
        None
    };

    let kernel_args = sub_matches
        .get_one::<String>("kernel-args")
        .cloned();

    let initrd = sub_matches
        .get_one::<String>("initrd")
        .cloned();

    let vmm_order = sub_matches
        .get_one::<String>("vmm")
        .map(|s| {
            s.split(',')
                .map(|part| part.trim().to_lowercase())
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let isolate_mode = sub_matches
        .get_one::<String>("isolate")
        .map(|s| s.parse::<crate::models::IsolateMode>().expect("clap validates env|fs|vm"));

    let namespace_strategy = sub_matches
        .get_one::<String>("namespace-strategy")
        .map(|s| match s.as_str() {
            "clone" => crate::models::NamespaceStrategy::Clone,
            "unshare" => crate::models::NamespaceStrategy::Unshare,
            _ => unreachable!("clap validates clone|unshare"),
        });

    let io_mode = sub_matches
        .get_one::<String>("io")
        .map(|s| s.parse::<crate::models::IoMode>().expect("clap validates auto|tty|stream|batch"))
        .unwrap_or_default();

    let vm_reuse_connect = sub_matches.get_flag("reuse");
    let vm_keep_timeout = sub_matches
        .get_one::<u32>("vm-keep-timeout")
        .copied();
    if vm_reuse_connect || vm_keep_timeout.is_some() {
        let vm_mode = isolate_mode == Some(crate::models::IsolateMode::Vm);
        if !vm_mode {
            return Err(eyre::eyre!(
                "--reuse and --vm-keep-timeout require --isolate=vm"
            ));
        }
    }

    // Parse UID/GID translation options for VM mode
    let translate_uid: Vec<String> = sub_matches
        .get_many::<String>("translate-uid")
        .map(|v| v.cloned().collect())
        .unwrap_or_default();
    let translate_gid: Vec<String> = sub_matches
        .get_many::<String>("translate-gid")
        .map(|v| v.cloned().collect())
        .unwrap_or_default();
    if (!translate_uid.is_empty() || !translate_gid.is_empty()) && isolate_mode != Some(crate::models::IsolateMode::Vm) {
        return Err(eyre::eyre!(
            "--translate-uid and --translate-gid require --isolate=vm"
        ));
    }

    // Create sandbox options from CLI inputs
    let sandbox = crate::models::SandboxOptions {
        isolate_mode,
        namespace_strategy,
        mount_specs,
    };

    options.run = RunOptions {
        user,
        command,
        args,
        timeout,
        kernel,
        vm_cpus,
        vm_memory_mib,
        kernel_args,
        initrd,
        io_mode,
        sandbox,
        vmm_order,
        vm_reuse_connect,
        vm_keep_timeout,
        translate_uid,
        translate_gid,
        ..Default::default()
    };

    Ok(())
}

