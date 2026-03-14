#![cfg(unix)]
use std::env;
#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
#[cfg(target_os = "linux")]
use std::os::fd::OwnedFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

use crate::models::*;
#[cfg(target_os = "linux")]
use crate::namespace::{determine_process_config, build_unified_context, create_process_with_namespaces};
use crate::lfs;
#[cfg(target_os = "linux")]
use crate::utils::is_suid;
use color_eyre::eyre;
use color_eyre::Result;
use log::{debug, trace};
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
    pub use_pty: Option<bool>, // None=auto-detect (isatty(stdin)), Some(true)=force PTY, Some(false)=force no-PTY

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
    /// Preferred VMM backend order for SandboxMode::Vm.
    /// Example: ["libkrun", "qemu"] or ["qemu"].
    pub vmm_order: Vec<String>,
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
pub fn resolve_vm_cpus(run_options: &RunOptions) -> u8 {
    if let Some(cpus) = run_options.vm_cpus {
        return cpus;
    }
    env::var("EPKG_VM_CPUS")
        .ok()
        .and_then(|s| s.parse::<u8>().ok())
        .unwrap_or(2)
}

/// Resolve VM memory size in MiB for VM backends.
///
/// Source precedence:
/// 1. RunOptions.vm_memory_mib (from --memory, already normalized to MiB)
/// 2. EPKG_VM_MEMORY as a human-readable size (e.g. "2048M", "2G") via parse_size_bytes_opt
/// 3. EPKG_VM_MEMORY parsed as plain MiB (u32) for backward compatibility
/// 4. Default: 2048 MiB
pub fn resolve_vm_memory_mib(run_options: &RunOptions) -> u32 {
    if let Some(mib) = run_options.vm_memory_mib {
        return mib;
    }
    env::var("EPKG_VM_MEMORY")
        .ok()
        .and_then(|s| {
            if let Some(bytes) = crate::utils::parse_size_bytes_opt(&s) {
                Some((bytes / (1024 * 1024)) as u32)
            } else {
                s.parse::<u32>().ok()
            }
        })
        .unwrap_or(2048)
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
#[cfg(target_os = "linux")]
pub fn fork_and_execute(env_root: &Path, run_options: &RunOptions) -> Result<Option<i32>> {
    // Clone run_options to allow preparation
    let mut prepared_opts = run_options.clone();
    prepare_run_options_for_command(env_root, &mut prepared_opts);

    // Execute with prepared options (handles both Clone and Unshare strategies)
    fork_and_execute_raw(env_root, &prepared_opts)
}

#[cfg(not(target_os = "linux"))]
pub fn fork_and_execute(env_root: &Path, run_options: &RunOptions) -> Result<Option<i32>> {
    // Prepare options (merge sandbox settings)
    let mut prepared_opts = run_options.clone();
    prepare_run_options_for_command(env_root, &mut prepared_opts);

    // Non-Linux platforms only support VM sandbox mode
    let sandbox_mode = prepared_opts.effective_sandbox.sandbox_mode
        .unwrap_or(SandboxMode::Env);

    match sandbox_mode {
        SandboxMode::Vm => {
            // VM sandbox mode - supported via libkrun
            #[cfg(feature = "libkrun")]
            {
                let cmd_path = resolve_command_path(env_root, &prepared_opts)?;

                // Convert host path to guest path (strip env_root prefix if inside)
                let guest_cmd_path = if let Ok(stripped) = cmd_path.strip_prefix(env_root) {
                    Path::new("/").join(stripped)
                } else {
                    cmd_path.clone()
                };

                debug!("Running in VM sandbox with libkrun");
                debug!("Guest command path: {}", guest_cmd_path.display());

                // Note: run_command_in_krun never returns on success
                crate::libkrun::run_command_in_krun(env_root, &prepared_opts, &guest_cmd_path)?;
                Ok(None) // unreachable, but needed for type consistency
            }
            #[cfg(not(feature = "libkrun"))]
            {
                Err(eyre::eyre!(
                    "VM sandbox requires libkrun feature. \
                     Recompile epkg with libkrun support for --sandbox=vm"
                ))
            }
        }
        SandboxMode::Env | SandboxMode::Fs => {
            Err(eyre::eyre!(
                "Sandbox mode '{}' is not supported on this platform. \
                 Only --sandbox=vm is available on macOS. \
                 Use Linux for other sandbox modes.",
                match sandbox_mode {
                    SandboxMode::Env => "env",
                    SandboxMode::Fs => "fs",
                    _ => unreachable!(),
                }
            ))
        }
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

/// Check if a file is executable, handling symlinks that may point to targets within environment root
fn is_executable_within_env(path: &Path, env_root: &Path) -> Result<bool> {
    trace!("is_executable_within_env checking: {}", path.display());

    match lfs::resolve_symlink_in_env(path, env_root) {
        Some(resolved) => {
            trace!("Resolved {} -> {}", path.display(), resolved.display());
            is_executable(&resolved)
        }
        None => {
            trace!("Path {} cannot be resolved within environment root", path.display());
            Ok(false)
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
pub fn find_command_in_env_path(cmd_name: &str, env_root: &Path) -> Result<PathBuf> {
    // Collect non-empty PATH directories; if none, use default system paths
    let path_str = env::var("PATH").unwrap_or_default();
    let mut dirs: Vec<&str> = path_str.split(':').filter(|d| !d.is_empty()).collect();
    if dirs.is_empty() {
        dirs.extend(["/usr/bin", "/bin", "/usr/sbin", "/sbin"]);
    }

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

        if is_executable_within_env(&cmd_path, env_root)? {
            // Check if this command is under the env_root prefix
            if cmd_path.starts_with(env_root) {
                // Strip env_root prefix to get the guest path (e.g., /usr/bin/go)
                // The env_root/usr is bind-mounted to /usr in the namespace
                let guest_path = PathBuf::from("/").join(rel_path).join(cmd_name);
                return Ok(guest_path);
            }
        }
    }
    Err(eyre::eyre!("Command '{}' not found in environment PATH under {}", cmd_name, env_root.display()))
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
/// - sandbox_mode: Override - higher priority completely replaces lower priority
/// - mount_specs: Additive - directories from all sources are combined
///
/// # Example:
/// If user defaults have sandbox_mode=Env and mount_specs=["/home"],
/// env defaults have sandbox_mode=Fs and mount_specs=["/tmp"],
/// and CLI specifies mount_specs=["/data"], the result will be:
/// sandbox_mode=Fs (from env), mount_specs=["/home", "/tmp", "/data"] (combined)
fn merge_sandbox_options(sources: &[&crate::models::SandboxOptions]) -> crate::models::SandboxOptions {
    let mut result = crate::models::SandboxOptions::default();

    // Process sources in reverse order (lowest to highest priority)
    // so higher priority can override lower priority
    for source in sources.iter().rev() {
        if let Some(mode) = source.sandbox_mode {
            result.sandbox_mode = Some(mode);
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
    if run_options.effective_sandbox.sandbox_mode.is_none() {
        run_options.effective_sandbox.sandbox_mode = Some(crate::models::SandboxMode::Env);
    }

    // Normalise skip_namespace_isolation based on channel and environment context.
    let is_conda =
        crate::models::channel_config().format == crate::models::PackageFormat::Conda;
    if is_conda {
        // conda ELF binary has RPATH
        run_options.skip_namespace_isolation = true;
    }

    // When running directly inside env_root or with env_root=/, always bypass namespace isolation.
    if config_guard.common.in_env_root || env_root.as_os_str() == "/" {
        run_options.skip_namespace_isolation = true;
    }

    // Allow bypassing namespace isolation via environment variable for testing
    if std::env::var("EPKG_SKIP_NAMESPACE").is_ok() {
        run_options.skip_namespace_isolation = true;
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
             epkg run --sandbox=vm --vmm=qemu --no-tty -- whoami \
             or epkg run whoami --sandbox=vm --vmm=qemu --no-tty",
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
                    "Invalid --memory value '{}': {} (expected size like 2048M or MiB integer)",
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

    let sandbox_mode = sub_matches
        .get_one::<String>("sandbox")
        .map(|s| s.parse::<crate::models::SandboxMode>().expect("clap validates env|fs|vm"));

    let namespace_strategy = sub_matches
        .get_one::<String>("namespace-strategy")
        .map(|s| match s.as_str() {
            "clone" => crate::models::NamespaceStrategy::Clone,
            "unshare" => crate::models::NamespaceStrategy::Unshare,
            _ => unreachable!("clap validates clone|unshare"),
        });

    let use_pty = if sub_matches.get_flag("tty") {
        Some(true)
    } else if sub_matches.get_flag("no-tty") {
        Some(false)
    } else {
        None
    };

    // Create sandbox options from CLI inputs
    let sandbox = crate::models::SandboxOptions {
        sandbox_mode,
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
        use_pty,
        sandbox,
        vmm_order,
        ..Default::default()
    };

    Ok(())
}

