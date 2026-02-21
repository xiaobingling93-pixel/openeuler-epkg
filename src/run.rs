use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::mem;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::time::{Duration, Instant};

use nix::unistd::{Uid, Gid, getuid, getgid, geteuid, dup2, pipe, close, write, fork, setuid, ForkResult};
use nix::sys::signal::{self, Signal};
use nix::sched::{unshare, CloneFlags};
use nix::mount::{mount, MsFlags};
use nix::errno::Errno;
use users::{get_current_uid};
use color_eyre::Result;
use color_eyre::eyre;
use color_eyre::eyre::WrapErr;
use log::{info, debug, warn, trace};
use crate::models::*;
use crate::utils;
use crate::utils::is_suid;
use crate::dirs;

#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    pub mount_dirs: Vec<String>,
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

#[allow(dead_code)]
pub fn privdrop_on_suid() {
    if is_suid() {
        setuid(Uid::from_raw(get_current_uid())).expect("Failed to drop privileges");
    }
}

/// Kill child process when timeout occurs
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

/// Execute command in child process with namespace setup
fn execute_in_child(env_root: &Path, run_options: &RunOptions, cmd_path: &Path) -> ! {
    // Kkip namespace isolation when env_root is the system root
    let skip_namespace_isolation = run_options.skip_namespace_isolation
                                    || config().common.in_env_root
                                    || env_root.as_os_str() == "/";

    // Resolve command path (if namespace isolation is used, canonicalize before mounts)
    let final_cmd_path = if skip_namespace_isolation {
        // No namespace isolation, use original path
        cmd_path.to_path_buf()
    } else {
        // Convert command path to a path relative to env_root (e.g., usr/bin/htop),
        // then after mounts, use the absolute path from root (e.g., /usr/bin/htop).
        // This makes the ELF binary think it's running from within the environment root.
        let rel_cmd_path = if cmd_path.starts_with(env_root) {
            match cmd_path.strip_prefix(env_root) {
                Ok(rel_path) => {
                    // Convert to absolute path from root (e.g., /usr/bin/htop)
                    let abs_from_root = Path::new("/").join(rel_path);
                    trace!("Converted command path to env-relative: {} -> {}", cmd_path.display(), abs_from_root.display());
                    abs_from_root
                }
                Err(_) => {
                    debug!("Could not strip env_root prefix, using original: {}", cmd_path.display());
                    cmd_path.to_path_buf()
                }
            }
        } else {
            trace!("Command path not under env_root, using original: {}", cmd_path.display());
            cmd_path.to_path_buf()
        };

        // Set up namespace and bind mounts
        trace!("Child process starting namespace setup (cmd: {})", rel_cmd_path.display());

        if let Err(e) = setup_namespace_and_mounts(env_root, run_options) {
            eprintln!("Failed to setup namespaces: {}", e);
            std::process::exit(1);
        }

        // Change to environment root directory before executing command if requested
        if run_options.chdir_to_env_root {
            // This ensures that scriptlets and other commands run relative to the environment root
            // rather than the current working directory, which is important for commands like
            // "chown etc/shadow" that expect to operate on files within the environment.
            //
            // We used to `cd $env_root`, however /opt/epkg/envs/ dir can be empty if it's
            // standalone mounted, so now we simply do `cd /`.
            if let Err(e) = std::env::set_current_dir("/") {
                eprintln!("Failed to change dir to / (env_root={}): {}", env_root.display(), e);
                std::process::exit(1);
            }
        }

        // After mounts, use the path relative to root (e.g., /usr/bin/htop)
        // which will be accessible through the mount structure
        rel_cmd_path
    };

    // Prepare environment variables
    let mut env_vars = run_options.env_vars.clone();

    // Set locale to C to avoid Perl locale warnings
    env_vars.insert("LANG".to_string(), "C".to_string());

    // Execute the command - this replaces the current process
    if let Err(e) = exec_command(&final_cmd_path, &run_options.args, Some(&env_vars)) {
        eprintln!("Failed to execute command '{}': {} (error: {:?})",
            cmd_path.display(), e, std::io::Error::last_os_error());
        std::process::exit(127);
    }

    // This should never be reached due to execvp
    unreachable!();
}

/// Fork a new process and execute command with optional namespace isolation
/// If `run_options.skip_namespace_isolation` is true, executes without namespace setup (for conda environments).
/// Otherwise, sets up namespace isolation before executing.
///
/// The command path is derived from `run_options.command`:
/// - If `run_options.command` is already an absolute path, it's used directly
/// - Otherwise, PATH lookup is performed using `find_command_in_env_path()`
///
/// Returns:
/// - Ok(Some(pid)) for background processes (run_options.background = true)
/// - Ok(None) for foreground processes (waits for completion)
pub fn fork_and_execute(env_root: &Path, run_options: &RunOptions) -> Result<Option<i32>> {
    // Resolve command path from run_options.command
    let cmd_path = if Path::new(&run_options.command).is_absolute() {
        // Already an absolute path, use it directly
        PathBuf::from(&run_options.command)
    } else if run_options.command.contains('/') {
        // Contains slash, treat as relative path (don't search PATH)
        PathBuf::from(&run_options.command)
    } else if Path::new(&run_options.command).exists() {
        // Exists in current dir, likely not a system command
        PathBuf::from(&run_options.command)
    } else {
        // Command name, do PATH lookup
        find_command_in_env_path(&run_options.command, env_root)?
    };

    let stdin_bytes = run_options.stdin.as_ref().map(|v| v.as_slice());
    let mut stdin_pipe = if stdin_bytes.is_some() {
        Some(pipe().map_err(|e| eyre::eyre!("Failed to create stdin pipe: {}", e))?)
    } else {
        None
    };

    // Fork a new process to handle namespace creation and command execution
    // This is necessary because multi-threaded processes cannot create user namespaces
    match unsafe { fork() } {
        Ok(ForkResult::Parent { child }) => {
            if let (Some(bytes), Some((read_fd, write_fd))) = (stdin_bytes, stdin_pipe.take()) {
                let _ = close(read_fd);
                with_sigpipe_handler(libc::SIG_IGN, move || {
                    let mut written = 0;
                    while written < bytes.len() {
                        match write(&write_fd, &bytes[written..]) {
                            Ok(0) => break, // Should not happen, but avoid infinite loop
                            Ok(n) => written += n,
                            Err(e) => {
                                // EPIPE means child closed stdin (doesn't need input)
                                // This is OK for hooks with NeedsTargets but scripts that ignore stdin
                                // Following SIGPIPE principle 1: handle EPIPE explicitly
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
                // For background processes, return the PID without waiting
                Ok(Some(child.as_raw() as i32))
            } else {
                // For foreground processes, wait for completion
                wait_for_child_with_timeout(child, &cmd_path, run_options)?;
                Ok(None)
            }
        }
        Ok(ForkResult::Child) => {
            if let Some((read_fd, write_fd)) = stdin_pipe {
                let _ = close(write_fd);
                // Duplicate the pipe read end onto STDIN without closing STDIN prematurely.
                // We create an OwnedFd for STDIN and forget it after dup2 so it isn't closed on drop.
                let mut stdin_fd = unsafe { OwnedFd::from_raw_fd(libc::STDIN_FILENO) };
                if let Err(e) = dup2(&read_fd, &mut stdin_fd) {
                    eprintln!("Failed to set up stdin for child: {}", e);
                    std::process::exit(1);
                }
                mem::forget(stdin_fd);
                let _ = close(read_fd);
            }

            // Redirect stdio to /dev/null for background daemon processes
            if run_options.redirect_stdio {
                unsafe {
                    let null_fd = libc::open(b"/dev/null\0".as_ptr() as *const libc::c_char, libc::O_RDWR);
                    if null_fd >= 0 {
                        libc::dup2(null_fd, 0); // stdin
                        libc::dup2(null_fd, 1); // stdout
                        libc::dup2(null_fd, 2); // stderr
                        libc::close(null_fd);
                    }
                }
            }

            execute_in_child(env_root, run_options, &cmd_path)
        }
        Err(e) => {
            Err(eyre::eyre!("Failed to fork process: {}", e))
        }
    }
}

/// Check if a file is executable
pub fn is_executable(path: &Path) -> Result<bool> {
    trace!("is_executable checking: {}", path.display());
    let metadata = fs::metadata(path)
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

    match utils::resolve_symlink_in_env(path, env_root) {
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

/// Find command in environment PATH
pub fn find_command_in_env_path(cmd_name: &str, env_root: &Path) -> Result<PathBuf> {
    // Collect non-empty PATH directories; if none, use default system paths
    let path_str = env::var("PATH").unwrap_or_default();
    let mut dirs: Vec<&str> = path_str.split(':').filter(|d| !d.is_empty()).collect();
    if dirs.is_empty() {
        dirs.extend(["/usr/bin", "/bin", "/usr/sbin", "/sbin"]);
    }

    for path_dir in dirs {
        // Skip paths ending with "/ebin"
        if path_dir.ends_with("/ebin") {
            continue;
        }

        // Check if this path is within the environment root
        let rel_path = path_dir.strip_prefix("/").unwrap_or(path_dir);
        let cmd_path = env_root.join(rel_path).join(cmd_name);

        if is_executable_within_env(&cmd_path, env_root)? {
            // Check if this command is under the env_root prefix
            if cmd_path.starts_with(env_root) {
                return Ok(cmd_path);
            }
        }
    }
    Err(eyre::eyre!("Command '{}' not found in environment PATH under {}", cmd_name, env_root.display()))
}

/// Set up namespace and bind mounts
pub(crate) fn setup_namespace_and_mounts(env_root: &Path, run_options: &RunOptions) -> Result<()> {
    if env_root.as_os_str() == "/" {
        return Ok(());
    }

    let euid = geteuid();
    let uid = getuid();
    let gid = getgid();

    trace!("Setting up namespace: euid={}, uid={}, gid={}", euid, uid, gid);

    // Create namespaces (die on error like C version)
    create_namespaces(euid, uid, gid, &run_options.user)?;

    // Set up bind mounts for the environment
    mount_env_dirs(euid, uid, env_root)?;

    // Mount additional directories if specified
    for mount_dir in &run_options.mount_dirs {
        mount_additional_dir(env_root, mount_dir)?;
    }

    Ok(())
}

/// Create namespaces following the C version logic
fn create_namespaces(euid: Uid, uid: Uid, gid: Gid, opt_user: &Option<String>) -> Result<()> {
    // Check if user namespaces are available first (for better error messages)
    if let Err(e) = check_user_namespace_support() {
        warn!("User namespace check failed: {}", e);
    }

    // Following C version logic:
    // if (euid) clone_flags = CLONE_NEWUSER;
    // if (unshare(clone_flags|CLONE_NEWNS) != 0) die("unshare");
    let mut clone_flags = CloneFlags::CLONE_NEWNS;
    if !euid.is_root() {
        clone_flags |= CloneFlags::CLONE_NEWUSER;
    }

    trace!("Creating namespaces with flags: {:?}", clone_flags);

    // Handle user mapping if we need to create user namespace
    if clone_flags.contains(CloneFlags::CLONE_NEWUSER) {
        // Fork a child process to handle newuidmap/newgidmap execution
        let (child_pid, sync_fd) = fork_idmap_child(uid, gid, opt_user)?;

        // Die on error like C version
        unshare_with_error_handling(clone_flags)?;

        trace!("Successfully created namespaces");

        // Signal child to proceed with ID mapping
        sync_with_idmap_child(child_pid, sync_fd)?;
    } else {
        // Die on error like C version
        unshare_with_error_handling(clone_flags)?;

        trace!("Successfully created namespaces");
    }

    if !clone_flags.contains(CloneFlags::CLONE_NEWUSER) {
        mount_make_rprivate()?;
    }

    Ok(())
}

/// Execute unshare with comprehensive error handling
fn unshare_with_error_handling(clone_flags: CloneFlags) -> Result<()> {
    unshare(clone_flags)
        .map_err(|e| {
            // Provide user-friendly error message
            let context = match e {
                nix::errno::Errno::EINVAL => {
                    "Invalid argument - possible causes:\n\
                     • User namespaces disabled in kernel\n\
                     • Process is multi-threaded (this should not happen in child process)\n\
                     • Invalid flag combination"
                }
                nix::errno::Errno::EPERM => {
                    "Operation not permitted - possible causes:\n\
                     • Insufficient privileges\n\
                     • Security policy preventing namespace creation"
                }
                nix::errno::Errno::ENOSPC => {
                    "No space left - possible causes:\n\
                     • Maximum number of namespaces reached\n\
                     • Resource limits exceeded"
                }
                _ => "Unknown error creating namespaces"
            };
            eyre::eyre!("unshare failed: {}\n{}", e, context)
        })
}

/// Check if user namespaces are supported on this system
fn check_user_namespace_support() -> Result<()> {
    use std::fs;

    // Check if user namespaces are enabled in the kernel
    let proc_files = vec![
        "/proc/sys/user/max_user_namespaces",
        "/proc/sys/kernel/unprivileged_userns_clone",
    ];

    for file in proc_files {
        if let Ok(content) = fs::read_to_string(file) {
            trace!("{}: {}", file, content.trim());
            if file.contains("max_user_namespaces") && content.trim() == "0" {
                return Err(eyre::eyre!("User namespaces disabled: max_user_namespaces = 0"));
            }
            if file.contains("unprivileged_userns_clone") && content.trim() == "0" {
                return Err(eyre::eyre!("Unprivileged user namespaces disabled"));
            }
        }
    }

    // Try a simple test of user namespace creation
    trace!("Testing simple user namespace creation...");
    match std::process::Command::new("unshare")
        .args(&["--user", "--map-root-user", "true"])
        .output()
    {
        Ok(output) => {
            if output.status.success() {
                trace!("Simple user namespace test: SUCCESS");
            } else {
                trace!("Simple user namespace test: FAILED - {}",
                    String::from_utf8_lossy(&output.stderr));
            }
        }
        Err(e) => {
            trace!("Failed to run unshare test command: {}", e);
        }
    }

    Ok(())
}

/// Make mount points private
fn mount_make_rprivate() -> Result<()> {
    mount(
        Some("none"),
        "/",
        Some(""),
        MsFlags::MS_REC | MsFlags::MS_PRIVATE,
        Some(""),
    ).map_err(|e| eyre::eyre!("Failed to make mounts private: {}", e))?;

    Ok(())
}

/// Check if the host OS uses traditional directory layout (dirs) or usr-merge layout (symlinks).
/// Returns true if the host uses traditional layout (e.g., Alpine < 3.22), false if usr-merge.
fn host_uses_traditional_layout() -> bool {
    // Check if /lib is a directory (traditional) or symlink (usr-merge)
    let lib_path = Path::new("/lib");
    if let Ok(metadata) = fs::symlink_metadata(lib_path) {
        return metadata.file_type().is_dir();
    }
    // If we can't check, assume traditional layout to be safe
    true
}

/*
 * Directory Layout Compatibility: Traditional (dirs) vs Usr-merge (symlinks)
 *
 * BACKGROUND:
 * Linux distributions have evolved from traditional directory layout to usr-merge layout:
 *
 * Traditional Layout (dirs):
 *   - /bin, /sbin, /lib are actual directories
 *   - Examples: Alpine Linux, older distributions
 *   - Structure: /bin, /sbin, /lib are separate from /usr
 *
 * Usr-merge Layout (symlinks):
 *   - /bin -> usr/bin, /sbin -> usr/sbin, /lib -> usr/lib, /lib64 -> usr/lib64
 *   - Examples: Modern RPM/Debian/Arch Linux, Alpine >= 3.22
 *   - Structure: Everything under /usr, with symlinks for compatibility
 *
 * GUEST ENVIRONMENT:
 *   - epkg environments always use usr-merge layout (symlinks)
 *   - /bin -> usr/bin, /sbin -> usr/sbin, /lib -> usr/lib, /lib64 -> usr/lib64
 *
 * STRATEGY TABLE:
 *   host        guest           strategy
 *   ===================================================================
 *   dirs        dirs            bind mount /bin /sbin /lib to $env_root/usr/bin .. on 'epkg run'
 *   dirs        symlinks        bind mount /bin /sbin /lib to $env_root/usr/bin .. on 'epkg run';
 *                               check and create the '/lib64 -> usr/lib64' symlink in host os on 'epkg self install', if it's run by root.
 *                               archlinux host has '/lib64 -> usr/lib' which can be safely fixed pointing to usr/lib64
 *   symlinks    dirs            current code works, no more fixup
 *   symlinks    symlinks        current code works, no more fixup
 *
 * IMPLEMENTATION:
 *   - For "dirs (host) + symlinks (guest)": We bind mount host's /bin, /sbin, /lib to
 *     $env_root/usr/bin, $env_root/usr/sbin, $env_root/usr/lib BEFORE mounting $env_root/usr.
 *   - For "symlinks (host) + symlinks (guest)": No special handling needed, symlinks work naturally.
 */

/// Helper function to bind mount a host directory to a guest directory.
fn bind_mount_host_to_guest(host_path: &Path, guest_path: &Path, error_msg: &str) -> Result<()> {
    if host_path.exists() && guest_path.exists() {
        // Check that both paths are real directories, not symlinks
        let host_metadata = fs::symlink_metadata(host_path)
            .wrap_err_with(|| format!("Failed to get metadata for host path: {}", host_path.display()))?;
        let guest_metadata = fs::symlink_metadata(guest_path)
            .wrap_err_with(|| format!("Failed to get metadata for guest path: {}", guest_path.display()))?;

        if !host_metadata.is_dir() {
            return Ok(());
        }
        if !guest_metadata.is_dir() {
            return Ok(());
        }

        trace!("Bind mounting host {} -> {}", host_path.display(), guest_path.display());
        mount(
            Some(guest_path),
            host_path,
            Some(""),
            MsFlags::MS_BIND,
            Some(""),
        ).wrap_err_with(|| error_msg.to_string())?;
    }
    Ok(())
}

/// Handle traditional layout host compatibility for usr-merge guest environments.
/// Bind mounts host's /bin, /sbin, /lib to guest's usr/bin, usr/sbin, usr/lib.
/// This must be called BEFORE mounting $env_root/usr over /usr.
fn mount_traditional_host_compatibility(env_root: &Path) -> Result<()> {
    if !host_uses_traditional_layout() {
        return Ok(());
    }

    debug!("Host uses traditional layout, binding host /bin, /sbin, /lib to environment usr directories");

    // Bind mount host's /bin to $env_root/usr/bin
    bind_mount_host_to_guest(
        Path::new("/bin"),
        &env_root.join("usr/bin"),
        "Failed to bind mount host /bin to env usr/bin",
    )?;

    // Bind mount host's /sbin to $env_root/usr/sbin
    bind_mount_host_to_guest(
        Path::new("/sbin"),
        &env_root.join("usr/sbin"),
        "Failed to bind mount host /sbin to env usr/sbin",
    )?;

    // Bind mount host's /lib to $env_root/usr/lib
    bind_mount_host_to_guest(
        Path::new("/lib"),
        &env_root.join("usr/lib"),
        "Failed to bind mount host /lib to env usr/lib",
    )?;

    Ok(())
}

/// Mount core environment directories (usr, etc, var, run, root).
fn mount_core_env_dirs(uid: Uid, env_root: &Path) -> Result<()> {
    mount_env_dir(env_root, "/usr")?;
    mount_env_dir(env_root, "/etc")?;
    mount_env_dir(env_root, "/var")?;
    mount_env_dir(env_root, "/run")?;   // fatal: could not open lock file /run/adduser!

    // "DPKG_MAINTSCRIPT_PACKAGE": "base-files"
    // "DPKG_MAINTSCRIPT_NAME": "postinst"
    // Triggered error when trying to create .profile and .bashrc for root:
    //      cp: cannot stat '/root/.profile': Permission denied
    if !uid.is_root() {
        mount_env_dir(env_root, "/root")?;
    }

    Ok(())
}

/*
 * Special handling for /opt/epkg mount isolation:
 *
 * Problem:
 * - When we bind-mount the user's /opt ($env_root/opt over /opt), it hides the original /opt/epkg
 * - This breaks dependencies (e.g., LLVM libraries in /opt/openEuler) that are symlinked through /opt/epkg
 *
 * Solution:
 * 1. Before mounting user's /opt:
 *    - If /opt/epkg exists, create a backup bind-mount at $env_root/opt_real
 * 2. Mount user's /opt normally (shadowing original)
 * 3. Restore /opt/epkg access:
 *    - Bind-mount the backup ($env_root/opt_real) back to /opt/epkg
 *
 * This gives us:
 * - User's isolated /opt environment
 * - Continued access to system /opt/epkg contents
 * - No root privileges required after initial setup
 *
 * Note: Uses MS_BIND instead of MS_MOVE for reliability across different filesystem setups
 */
/// Try to create an opt_real directory for public environments, attempting multiple locations.
/// Returns the path to the created directory.
fn create_opt_real_path_for_public_env(euid: Uid, uid: Uid, env_name: &str) -> Result<PathBuf> {
    let uid_raw = uid.as_raw();
    let euid_raw = euid.as_raw();

    // Location 1: /run/user/{euid}/epkg-opt_real/{uid}-{env_name}
    let run_user_path = PathBuf::from(format!("/run/user/{}/epkg-opt_real/{}-{}", euid_raw, uid_raw, env_name));
    match utils::safe_mkdir_p(&run_user_path) {
        Ok(_) => {
            trace!("Using opt_real directory: {}", run_user_path.display());
            return Ok(run_user_path);
        }
        Err(e) => {
            trace!("Failed to create /run/user/ opt_real directory: {}", e);

            // Location 2: $HOME/.epkg/opt-real/{uid}-{env_name}
            match dirs::get_home() {
                Ok(home) => {
                    let home_opt_real = PathBuf::from(&home)
                        .join(".epkg")
                        .join("opt-real")
                        .join(format!("{}-{}", uid_raw, env_name));
                    match utils::safe_mkdir_p(&home_opt_real) {
                        Ok(_) => {
                            trace!("Using fallback opt_real directory: {}", home_opt_real.display());
                            return Ok(home_opt_real);
                        }
                        Err(e2) => {
                            return Err(eyre::eyre!(
                                "Failed to create opt_real directory in both /run/user/ and $HOME/.epkg/:\n\
                                 /run/user/ attempt: {}\n\
                                 $HOME/.epkg/ attempt: {}",
                                e, e2
                            ));
                        }
                    }
                }
                Err(e2) => {
                    return Err(eyre::eyre!(
                        "Failed to create /run/user/ opt_real directory: {}\n\
                         Also failed to get home directory for fallback: {}",
                        e, e2
                    ));
                }
            }
        }
    }
}

/// Handle /opt/epkg mount isolation to preserve access to system /opt/epkg.
/// This ensures that when we mount the guest's /opt, we don't lose access to the host's /opt/epkg.
fn mount_opt_epkg_isolation(euid: Uid, uid: Uid, env_root: &Path) -> Result<()> {
    let opt_real_path = if env_root.starts_with("/opt/epkg") {
        /*
         * Use a path outside /opt/epkg to avoid circular dependency
         *
         * We must NOT place the opt_real backup inside the public environment tree (/opt/epkg/...),
         * because if we do, bind-mounting /opt/epkg into a subdirectory of itself creates a recursive
         * mount loop, leading to ELOOP (Too many levels of symbolic links) errors when resolving paths.
         *
         * To avoid this, if the current env_root is a public environment (i.e., starts with /opt/epkg),
         * we use a temporary directory outside /opt/epkg for the backup. This ensures the backup is outside
         * the tree being bind-mounted, breaking the loop. We try locations in order:
         * 1. /run/user/{outside_euid}/epkg-opt_real/{outside_uid}-{env_name} (auto-cleaned on logout)
         * 2. $HOME/.epkg/opt-real/{outside_uid}-{env_name} (fallback for containers without /run/user/)
         * For private environments, we can safely use env_root.join("opt_real") as before.
         */
        let env_name = config().common.env_name.clone();
        create_opt_real_path_for_public_env(euid, uid, &env_name)?
    } else {
        env_root.join("opt_real")
    };

    // Ensure the opt_real directory exists (for private environments,
    // or as a safety check for public environments where it was already created)
    utils::safe_mkdir_p(&opt_real_path)
        .map_err(|e| eyre::eyre!("Failed to create opt_real directory '{}': {}", opt_real_path.display(), e))?;

    let opt_epkg_path = Path::new("/opt/epkg");
    // Store whether /opt/epkg existed BEFORE mounting env_root/opt over /opt
    // This is critical because after mounting, /opt/epkg will be hidden
    let opt_epkg_existed = opt_epkg_path.exists();

    if opt_epkg_existed {
        trace!("Bind mounting {} -> {}", opt_epkg_path.display(), opt_real_path.display());
        mount(
            Some(opt_epkg_path),
            &opt_real_path,
            Some(""),
            // MsFlags::MS_MOVE, // will fail if src is not a mount point
            MsFlags::MS_BIND,
            Some(""),
        ).wrap_err("Failed to move /opt mount")?;
    }

    // Mount environment /opt directory
    mount_env_dir(env_root, "/opt")?;

    // If /opt/epkg existed BEFORE mounting, bind mount it back
    // Use the stored value, not a new check, because /opt/epkg is now hidden
    if opt_epkg_existed {
        if opt_real_path.exists() {
            trace!("Bind mounting {} -> {}", opt_real_path.display(), opt_epkg_path.display());
            mount(
                Some(&opt_real_path),
                opt_epkg_path,
                Some(""),
                MsFlags::MS_BIND,
                Some(""),
            ).wrap_err("Failed to bind mount opt_real/epkg to /opt/epkg")?;
        }
    }

    Ok(())
}

/// Mount environment directories
fn mount_env_dirs(euid: Uid, uid: Uid, env_root: &Path) -> Result<()> {
    // Handle traditional layout host compatibility (must be done BEFORE mounting /usr)
    mount_traditional_host_compatibility(env_root)?;

    // Mount core environment directories
    mount_core_env_dirs(uid, env_root)?;

    // Handle /opt/epkg mount isolation
    mount_opt_epkg_isolation(euid, uid, env_root)?;

    Ok(())
}

/// Mount a single environment directory
fn mount_env_dir(env_root: &Path, dir: &str) -> Result<()> {
    let src = env_root.join(dir.trim_start_matches('/'));
    let host_path = Path::new(dir);

    if src.exists() {
        trace!("Bind mounting host {} -> {}", host_path.display(), src.display());

        mount(
            Some(&src),
            host_path,
            Some(""),
            MsFlags::MS_BIND,
            Some(""),
        ).map_err(|e| eyre::eyre!("Failed to bind mount host {} to {}: {}", host_path.display(), src.display(), e))?;
    }

    Ok(())
}

/// Mount additional directory specified by user
fn mount_additional_dir(env_root: &Path, mount_dir: &str) -> Result<()> {
    let src = env_root.join(mount_dir.trim_start_matches('/'));
    let host_path = Path::new(mount_dir);

    if src.exists() && host_path.exists() {
        trace!("Bind mounting additional host {} -> {}", host_path.display(), src.display());

        mount(
            Some(&src),
            host_path,
            Some(""),
            MsFlags::MS_BIND,
            Some(""),
        ).map_err(|e| eyre::eyre!("Failed to bind mount additional host {} to {}: {}", host_path.display(), src.display(), e))?;
    } else {
        warn!("Additional mount directory host {} or {} does not exist, skipping", host_path.display(), src.display());
    }

    Ok(())
}


/// Read subuid/subgid ranges for a user
fn read_subid_ranges(username: &str, subid_file: &str) -> Result<Vec<(u32, u32)>> {
    let content = fs::read_to_string(subid_file)
        .map_err(|e| eyre::eyre!("Failed to read {}: {}", subid_file, e))?;

    for line in content.lines() {
        let parts: Vec<&str> = line.split(':').collect();
        if parts.len() == 3 && parts[0] == username {
            let start = parts[1].parse::<u32>()
                .map_err(|e| eyre::eyre!("Invalid start ID in {}: {}", subid_file, e))?;
            let count = parts[2].parse::<u32>()
                .map_err(|e| eyre::eyre!("Invalid count in {}: {}", subid_file, e))?;
            return Ok(vec![(start, count)]);
        }
    }

    Err(eyre::eyre!("No subid ranges found for user {} in {}", username, subid_file))
}

/// Synchronization byte used for parent-child communication
const PIPE_SYNC_BYTE: u8 = 0x69;

/// Fork a child process to handle ID mapping with newuidmap/newgidmap
fn fork_idmap_child(uid: Uid, gid: Gid, opt_user: &Option<String>) -> Result<(nix::unistd::Pid, OwnedFd)> {
    let (read_fd, write_fd) = nix::unistd::pipe()
        .map_err(|e| eyre::eyre!("Failed to create pipe: {}", e))?;

    match unsafe { fork() } {
        Ok(ForkResult::Parent { child }) => {
            drop(read_fd); // Close read end in parent
            trace!("Forked ID mapping child process: {}", child);
            Ok((child, write_fd))
        }
        Ok(ForkResult::Child) => {
            drop(write_fd); // Close write end in child
            // Wait for parent to signal us to proceed
            let mut buffer = [0u8; 1];
            match nix::unistd::read(&read_fd, &mut buffer) {
                Ok(1) => {
                    if buffer[0] == PIPE_SYNC_BYTE {
                        trace!("Child received sync signal, proceeding with ID mapping");
                        execute_idmap_for_parent(uid, gid, opt_user)?;
                        std::process::exit(0);
                    } else {
                        eprintln!("Invalid sync byte received");
                        std::process::exit(1);
                    }
                }
                Ok(n) => {
                    eprintln!("Unexpected read size: {}", n);
                    std::process::exit(1);
                }
                Err(e) => {
                    eprintln!("Failed to read sync signal: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Err(e) => {
            return Err(eyre::eyre!("Failed to fork ID mapping child: {}", e));
        }
    }
}

/// Signal the ID mapping child to proceed
fn sync_with_idmap_child(child_pid: nix::unistd::Pid, sync_fd: OwnedFd) -> Result<()> {
    let fd = sync_fd.as_raw_fd();
    let sync_byte = [PIPE_SYNC_BYTE];
    let result = unsafe {
        libc::write(fd, sync_byte.as_ptr() as *const libc::c_void, sync_byte.len())
    };
    if result < 0 {
        return Err(eyre::eyre!("Failed to send sync signal to child: {}", std::io::Error::last_os_error()));
    } else if result != 1 {
        return Err(eyre::eyre!("Unexpected write size: {}", result));
    }
    trace!("Sent sync signal to child");
    // OwnedFd will close fd when dropped
    drop(sync_fd);
    match nix::sys::wait::waitpid(child_pid, None) {
        Ok(wait_status) => {
            use nix::sys::wait::WaitStatus;
            match wait_status {
                WaitStatus::Exited(_, exit_code) => {
                    if exit_code != 0 {
                        return Err(eyre::eyre!("ID mapping child failed with exit code {}", exit_code));
                    }
                    trace!("ID mapping child completed successfully");
                }
                WaitStatus::Signaled(_, signal, _) => {
                    return Err(eyre::eyre!("ID mapping child killed by signal {:?}", signal));
                }
                _ => {
                    return Err(eyre::eyre!("ID mapping child ended with unexpected status: {:?}", wait_status));
                }
            }
        }
        Err(e) => {
            return Err(eyre::eyre!("Failed to wait for ID mapping child: {}", e));
        }
    }
    Ok(())
}

/// Execute ID mapping for the parent process using newuidmap/newgidmap
fn execute_idmap_for_parent(uid: Uid, gid: Gid, opt_user: &Option<String>) -> Result<()> {
    let parent_pid = nix::unistd::getppid();
    let username = dirs::get_username()?;
    let uid_raw = uid.as_raw();
    let gid_raw = gid.as_raw();

    trace!("Executing ID mapping for parent PID {} (user: {}, UID: {}, GID: {})",
           parent_pid, username, uid_raw, gid_raw);

    // Check if newuidmap and newgidmap commands are available
    let has_newuidmap = utils::command_exists("newuidmap");
    let has_newgidmap = utils::command_exists("newgidmap");

    trace!("UID mapping tools: newuidmap={}, newgidmap={}", has_newuidmap, has_newgidmap);

    if has_newuidmap && has_newgidmap {
        // Try Podman's approach with newuidmap/newgidmap
        match execute_newidmap_for_parent(parent_pid, uid_raw, gid_raw, &username) {
            Ok(()) => {
                trace!("Successfully used newuidmap/newgidmap for UID/GID mapping");
                return Ok(());
            }
            Err(e) => {
                warn!("newuidmap/newgidmap failed: {}, falling back to simple mapping", e);
                execute_simple_idmap_for_parent(parent_pid, uid_raw, gid_raw)?;
            }
        }
    } else {
        // Fallback to simple mapping
        execute_simple_idmap_for_parent(parent_pid, uid_raw, gid_raw)?;
    }

    // Set environment variables if user was specified (this will be inherited by the parent)
    if let Some(user_spec) = opt_user {
        if let Ok(_parsed_uid) = user_spec.parse::<u32>() {
            // For numeric UIDs, we don't change environment variables
        } else {
            // For username, set environment variables
            env::set_var("USER", user_spec);
            env::set_var("LOGNAME", user_spec);
        }
    }

    Ok(())
}

/// Execute newuidmap/newgidmap for the parent process
fn execute_newidmap_for_parent(parent_pid: nix::unistd::Pid, uid_raw: u32, gid_raw: u32, username: &str) -> Result<()> {
    // Read subuid and subgid ranges
    let subuid_ranges = read_subid_ranges(username, "/etc/subuid")?;
    let subgid_ranges = read_subid_ranges(username, "/etc/subgid")?;

    trace!("Subuid ranges: {:?}", subuid_ranges);
    trace!("Subgid ranges: {:?}", subgid_ranges);

    // Write setgroups deny first
    write_id_map_for_pid(parent_pid, "/proc/{}/setgroups", "deny")?;

    // Set up UID mapping using newuidmap
    execute_newidmap_for_pid("newuidmap", parent_pid, uid_raw, &subuid_ranges)?;

    // Set up GID mapping using newgidmap
    execute_newidmap_for_pid("newgidmap", parent_pid, gid_raw, &subgid_ranges)?;

    trace!("Successfully mapped UID/GID ranges using newuidmap/newgidmap");
    Ok(())
}

/// Execute newuidmap or newgidmap command for a specific PID
fn execute_newidmap_for_pid(cmd: &str, target_pid: nix::unistd::Pid, current_id: u32, ranges: &[(u32, u32)]) -> Result<()> {
    let mut args = vec![
        cmd.to_string(),
        target_pid.as_raw().to_string(), // target PID (parent)
    ];

    // Map root (0) to current user/group
    args.push("0".to_string());
    args.push(current_id.to_string());
    args.push("1".to_string());

    // Map additional ranges starting from 1
    for (start, count) in ranges {
        if *count > 1 {
            args.push("1".to_string());
            args.push(start.to_string());
            args.push(count.to_string());
            break; // Use first range for now
        }
    }

    trace!("Executing {} with args: {:?}", cmd, args);
    let status = std::process::Command::new(&args[0])
        .args(&args[1..])
        .status()
        .map_err(|e| eyre::eyre!("Failed to execute {}: {}", cmd, e))?;

    if !status.success() {
        return Err(eyre::eyre!("{} failed with status: {}", cmd, status));
    }

    Ok(())
}

/// Execute simple ID mapping for the parent process
fn execute_simple_idmap_for_parent(parent_pid: nix::unistd::Pid, uid_raw: u32, gid_raw: u32) -> Result<()> {
    // In user namespaces, we typically map ourselves to become root inside the namespace
    // This gives us the privileges needed for bind mounting
    // Format: "inside_id outside_id count"
    let uid_map = format!("0 {} 1", uid_raw);
    let gid_map = format!("0 {} 1", gid_raw);

    debug!("Setting up simple user namespace mapping for PID {}: uid_map='{}', gid_map='{}'",
           parent_pid, uid_map, gid_map);

    // Write user mapping files in the correct order
    write_id_map_for_pid(parent_pid, "/proc/{}/setgroups", "deny")?;
    write_id_map_for_pid(parent_pid, "/proc/{}/uid_map", &uid_map)?;
    write_id_map_for_pid(parent_pid, "/proc/{}/gid_map", &gid_map)?;

    Ok(())
}

/// Write to ID mapping files for a specific PID
fn write_id_map_for_pid(pid: nix::unistd::Pid, path_template: &str, content: &str) -> Result<()> {
    let path = path_template.replace("{}", &pid.as_raw().to_string());
    fs::write(&path, content)
        .map_err(|e| eyre::eyre!("Failed to write to {}: {}", path, e))?;
    Ok(())
}

/// Execute the command with arguments and optional environment variables
fn exec_command(cmd_path: &Path, args: &[String], env_vars: Option<&std::collections::HashMap<String, String>>) -> Result<()> {
    debug!("Executing: {} {:?}", cmd_path.display(), args);

    // Convert Path to CString for execvp
    let cmd_cstr = std::ffi::CString::new(cmd_path.to_str()
        .ok_or_else(|| eyre::eyre!("Invalid command path"))?)?;

    // Convert args to CStrings
    let mut c_args: Vec<std::ffi::CString> = vec![cmd_cstr.clone()];
    for arg in args {
        let c_arg = std::ffi::CString::new(arg.as_str())
            .map_err(|e| eyre::eyre!("Invalid argument: {}", e))?;
        c_args.push(c_arg);
    }

    // Convert to pointers for execvp
    let mut c_args_ptrs: Vec<*const libc::c_char> = c_args.iter()
        .map(|arg| arg.as_ptr() as *const libc::c_char)
        .collect();
    c_args_ptrs.push(std::ptr::null());

    // Set environment variables if provided
    if let Some(vars) = env_vars {
        debug!("With environment variables: {:?}", vars);
        for (key, value) in vars {
            if let Ok(key_cstr) = std::ffi::CString::new(key.as_str()) {
                if let Ok(val_cstr) = std::ffi::CString::new(value.as_str()) {
                    unsafe {
                        libc::setenv(key_cstr.as_ptr(), val_cstr.as_ptr(), 1);
                    }
                }
            }
        }
    }

    // Execute the command using execvp
    nix::unistd::execvp(&cmd_cstr, &c_args)
        .map_err(|e| eyre::eyre!("Failed to execute command '{}' with args {:?}: {}",
            cmd_path.display(), args, e))?;

    // This should never be reached as execvp replaces the current process
    unreachable!();
}

/// Execute command with environment PATH lookup and namespace isolation
pub fn command_run(_sub_matches: &clap::ArgMatches) -> Result<()> {
    let mut run_options = config().run.clone();

    debug!("Running command: {} with args: {:?}", run_options.command, run_options.args);
    debug!("Mount dirs: {:?}, User: {:?}", run_options.mount_dirs, run_options.user);

    let env_root = crate::dirs::get_default_env_root()?;
    info!("Using environment root: {}", env_root.display());

    let is_conda = crate::models::channel_config().format == crate::models::PackageFormat::Conda;
    if is_conda {
        // conda ELF binary has RPATH
        run_options.skip_namespace_isolation = true;
    }

    let _ = fork_and_execute(&env_root, &run_options)?;

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
    debug!("command_busybox sub_matches: {:?}", sub_matches);

    // Handle --list flag
    if sub_matches.get_flag("list") {
        println!("{}", crate::applets::sorted_applet_names().join("\n"));
        return Ok(());
    }

    /* Parse the subcommand structure:
     * - Some((cmd_name, cmd_matches)): A subcommand was specified
     * - None: No subcommand specified (error case)
     */
    match sub_matches.subcommand() {
        Some((cmd_name, cmd_matches)) => {
            debug!("cmd_name: {}, cmd_matches: {:?}", cmd_name, cmd_matches);
            let known = crate::applets::busybox_subcommands()
                .iter()
                .any(|c| c.get_name() == cmd_name);
            if known {
                debug!("Running built-in command: {}", cmd_name);
                // Find the applet command
                let applet_cmd = crate::applets::busybox_subcommands()
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
                    match applet_cmd.try_get_matches_from(all_args) {
                        Ok(parsed_matches) => {
                            crate::applets::exec_builtin_command(cmd_name, &parsed_matches)
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
                    crate::applets::exec_builtin_command(cmd_name, cmd_matches)
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

