use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use nix::unistd::{Uid, Gid, getuid, getgid, geteuid};

use nix::sched::{unshare, CloneFlags};
use nix::mount::{mount, MsFlags};
use color_eyre::Result;
use color_eyre::eyre;
use color_eyre::eyre::WrapErr;
use log::{info, debug, warn};
use crate::models::*;


#[derive(Debug, Clone)]
pub struct RunOptions {
    pub mount_dirs: Vec<String>,
    pub user: Option<String>,
    pub command: String,
    pub args: Vec<String>,
    pub env_vars: std::collections::HashMap<String, String>,
}

/// Fork a new process and execute command with namespace isolation
pub fn fork_and_execute(env_root: &Path, run_options: &RunOptions, cmd_path: &Path) -> Result<()> {
    // Fork a new process to handle namespace creation and command execution
    // This is necessary because multi-threaded processes cannot create user namespaces
    match unsafe { nix::unistd::fork() } {
        Ok(nix::unistd::ForkResult::Parent { child }) => {
            // Parent process: wait for child to complete
            debug!("Parent process waiting for child {}", child);

            match nix::sys::wait::waitpid(child, None) {
                Ok(wait_status) => {
                    use nix::sys::wait::WaitStatus;
                    match wait_status {
                        WaitStatus::Exited(_, exit_code) => {
                            debug!("Child process exited with code {}", exit_code);
                            if exit_code != 0 {
                                // Instead of terminating epkg, return an error
                                return Err(eyre::eyre!("Command failed with exit code {}", exit_code));
                            }
                        }
                        WaitStatus::Signaled(_, signal, _) => {
                            debug!("Child process killed by signal {:?}", signal);
                            return Err(eyre::eyre!("Command killed by signal {:?}", signal));
                        }
                        _ => {
                            debug!("Child process ended with status: {:?}", wait_status);
                            return Err(eyre::eyre!("Command ended with unexpected status: {:?}", wait_status));
                        }
                    }
                }
                Err(e) => {
                    return Err(eyre::eyre!("Failed to wait for child process: {}", e));
                }
            }
        }
        Ok(nix::unistd::ForkResult::Child) => {
            // Child process: set up namespaces and execute command
            debug!("Child process starting namespace setup");

            // Set up namespace and bind mounts
            if let Err(e) = setup_namespace_and_mounts(env_root, run_options) {
                eprintln!("Failed to setup namespaces: {}", e);
                std::process::exit(1);
            }

            // Execute the command - this replaces the current process
            if let Err(e) = exec_command(cmd_path, &run_options.args, Some(&run_options.env_vars)) {
                eprintln!("Failed to execute command '{}': {} (error: {:?})",
                    cmd_path.display(), e, std::io::Error::last_os_error());
                std::process::exit(127);
            }

            // This should never be reached due to execvp
            unreachable!();
        }
        Err(e) => {
            return Err(eyre::eyre!("Failed to fork process: {}", e));
        }
    }

    Ok(())
}

/// Check if a file is executable
pub fn is_executable(path: &Path) -> Result<bool> {
    let metadata = fs::metadata(path)
        .map_err(|e| eyre::eyre!("Failed to get metadata for {}: {}", path.display(), e))?;

    let permissions = metadata.permissions();
    Ok(permissions.mode() & 0o111 != 0)
}

/// Find command in environment PATH
pub fn find_command_in_env_path(cmd_name: &str, env_root: &Path) -> Result<PathBuf> {
    let paths = env::var("PATH")
        .unwrap_or_else(|_| "/usr/bin:/bin:/usr/sbin:/sbin".to_string());

    for path_dir in paths.split(':') {
        if path_dir.is_empty() {
            continue;
        }

        // Skip paths ending with "/ebin"
        if path_dir.ends_with("/ebin") {
            continue;
        }

        // Check if this path is within the environment root
        let rel_path = path_dir.strip_prefix("/").unwrap_or(path_dir);
        let cmd_path = env_root.join(rel_path).join(cmd_name);

        if cmd_path.exists() && is_executable(&cmd_path)? {
            // Check if this command is under the env_root prefix
            if cmd_path.starts_with(env_root) {
                return Ok(cmd_path);
            }
        }
    }
    Err(eyre::eyre!("Command '{}' not found in environment PATH under {}", cmd_name, env_root.display()))
}

/// Set up namespace and bind mounts
pub fn setup_namespace_and_mounts(env_root: &Path, run_options: &RunOptions) -> Result<()> {
    let euid = geteuid();
    let uid = getuid();
    let gid = getgid();

    debug!("Setting up namespace: euid={}, uid={}, gid={}", euid, uid, gid);

    // Create namespaces (die on error like C version)
    create_namespaces(euid, uid, gid, &run_options.user)?;

    // Set up bind mounts for the environment
    mount_env_dirs(env_root)?;

    // Mount additional directories if specified
    for mount_dir in &run_options.mount_dirs {
        mount_additional_dir(env_root, mount_dir)?;
    }

    Ok(())
}

/// Create namespaces following the C version logic
pub fn create_namespaces(euid: Uid, uid: Uid, gid: Gid, opt_user: &Option<String>) -> Result<()> {
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

    debug!("Creating namespaces with flags: {:?}", clone_flags);

    // Die on error like C version
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
        })?;

    debug!("Successfully created namespaces");

    if !clone_flags.contains(CloneFlags::CLONE_NEWUSER) {
        mount_make_rprivate()?;
    }

    // Handle user mapping if we created user namespace
    if clone_flags.contains(CloneFlags::CLONE_NEWUSER) {
        map_user(uid, gid, opt_user)?;
    }

    Ok(())
}

/// Check if user namespaces are supported on this system
pub fn check_user_namespace_support() -> Result<()> {
    use std::fs;

    // Check if user namespaces are enabled in the kernel
    let proc_files = vec![
        "/proc/sys/user/max_user_namespaces",
        "/proc/sys/kernel/unprivileged_userns_clone",
    ];

    for file in proc_files {
        if let Ok(content) = fs::read_to_string(file) {
            debug!("{}: {}", file, content.trim());
            if file.contains("max_user_namespaces") && content.trim() == "0" {
                return Err(eyre::eyre!("User namespaces disabled: max_user_namespaces = 0"));
            }
            if file.contains("unprivileged_userns_clone") && content.trim() == "0" {
                return Err(eyre::eyre!("Unprivileged user namespaces disabled"));
            }
        }
    }

    // Try a simple test of user namespace creation
    debug!("Testing simple user namespace creation...");
    match std::process::Command::new("unshare")
        .args(&["--user", "--map-root-user", "true"])
        .output()
    {
        Ok(output) => {
            if output.status.success() {
                debug!("Simple user namespace test: SUCCESS");
            } else {
                debug!("Simple user namespace test: FAILED - {}",
                    String::from_utf8_lossy(&output.stderr));
            }
        }
        Err(e) => {
            debug!("Failed to run unshare test command: {}", e);
        }
    }

    Ok(())
}

/// Make mount points private
pub fn mount_make_rprivate() -> Result<()> {
    mount(
        Some("none"),
        "/",
        Some(""),
        MsFlags::MS_REC | MsFlags::MS_PRIVATE,
        Some(""),
    ).map_err(|e| eyre::eyre!("Failed to make mounts private: {}", e))?;

    Ok(())
}

/// Mount environment directories
pub fn mount_env_dirs(env_root: &Path) -> Result<()> {
    mount_env_dir(env_root, "/usr")?;
    mount_env_dir(env_root, "/etc")?;
    mount_env_dir(env_root, "/var")?;

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
    let opt_epkg_path = Path::new("/opt/epkg");
    let opt_real_path = env_root.join("opt_real");
    if opt_epkg_path.exists() {
        // Create opt_real directory in the environment root
        std::fs::create_dir_all(&opt_real_path)
            .wrap_err("Failed to create opt_real directory")?;

        // Bind mount /opt/epkg to $env_root/opt_real
        debug!("Bind mounting /opt/epkg mount to {}", opt_real_path.display());
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

    // If /opt/epkg existed, bind mount it back
    if opt_epkg_path.exists() {
        if opt_real_path.exists() {
            debug!("Bind mounting {} to {}", opt_real_path.display(), opt_epkg_path.display());
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

/// Mount a single environment directory
pub fn mount_env_dir(env_root: &Path, dir: &str) -> Result<()> {
    let src = env_root.join(dir.trim_start_matches('/'));
    let dst = Path::new(dir);

    if src.exists() {
        debug!("Bind mounting {} to {}", src.display(), dst.display());

        mount(
            Some(&src),
            dst,
            Some(""),
            MsFlags::MS_BIND,
            Some(""),
        ).map_err(|e| eyre::eyre!("Failed to bind mount {} to {}: {}", src.display(), dst.display(), e))?;
    }

    Ok(())
}

/// Mount additional directory specified by user
pub fn mount_additional_dir(env_root: &Path, mount_dir: &str) -> Result<()> {
    let src = env_root.join(mount_dir.trim_start_matches('/'));
    let dst = Path::new(mount_dir);

    if src.exists() && dst.exists() {
        debug!("Bind mounting additional dir {} to {}", src.display(), dst.display());

        mount(
            Some(&src),
            dst,
            Some(""),
            MsFlags::MS_BIND,
            Some(""),
        ).map_err(|e| eyre::eyre!("Failed to bind mount additional dir {} to {}: {}", src.display(), dst.display(), e))?;
    } else {
        warn!("Additional mount directory {} or {} does not exist, skipping", src.display(), dst.display());
    }

    Ok(())
}

/// Handle user ID mapping for unprivileged namespaces
pub fn map_user(uid: Uid, gid: Gid, opt_user: &Option<String>) -> Result<()> {
    // In user namespaces, we typically map ourselves to become root inside the namespace
    // This gives us the privileges needed for bind mounting
    // Format: "inside_id outside_id count"
    let uid_map = format!("0 {} 1", uid.as_raw());
    let gid_map = format!("0 {} 1", gid.as_raw());

    debug!("Setting up user namespace mapping: uid_map='{}', gid_map='{}'", uid_map, gid_map);

    // Write user mapping files in the correct order
    write_id_map("/proc/self/setgroups", "deny")?;
    write_id_map("/proc/self/uid_map", &uid_map)?;
    write_id_map("/proc/self/gid_map", &gid_map)?;

    // Set environment variables if user was specified
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

/// Write to ID mapping files
pub fn write_id_map(path: &str, content: &str) -> Result<()> {
    fs::write(path, content)
        .map_err(|e| eyre::eyre!("Failed to write to {}: {}", path, e))?;
    Ok(())
}

/// Execute the command with arguments and optional environment variables
pub fn exec_command(cmd_path: &Path, args: &[String], env_vars: Option<&std::collections::HashMap<String, String>>) -> Result<()> {
    debug!("Executing: {} {:?}", cmd_path.display(), args);
    if let Some(vars) = env_vars {
        debug!("With environment variables: {:?}", vars);
    }

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
    let mut c_args_ptrs: Vec<*const i8> = c_args.iter()
        .map(|arg| arg.as_ptr() as *const i8)
        .collect();
    c_args_ptrs.push(std::ptr::null());

    // Set environment variables if provided
    if let Some(vars) = env_vars {
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

impl PackageManager {
    /// Execute command with environment PATH lookup and namespace isolation
    pub fn command_run(&mut self, sub_matches: &clap::ArgMatches) -> Result<()> {
        let run_options = self.parse_run_options(sub_matches)?;

        debug!("Running command: {} with args: {:?}", run_options.command, run_options.args);
        debug!("Mount dirs: {:?}, User: {:?}", run_options.mount_dirs, run_options.user);

        // Get the default environment root
        let env_root = self.get_default_env_root()?.clone();
        info!("Using environment root: {}", env_root.display());

        // Find the command in environment PATH under env_root prefix
        let cmd_path = find_command_in_env_path(&run_options.command, &env_root)?;
        info!("Found command at: {}", cmd_path.display());

        // Fork and execute with namespace isolation
        fork_and_execute(&env_root, &run_options, &cmd_path)?;

        Ok(())
    }

    /// Parse command line options for run command
    fn parse_run_options(&self, sub_matches: &clap::ArgMatches) -> Result<RunOptions> {
        let mount_dirs = if let Some(mount_str) = sub_matches.get_one::<String>("mount") {
            mount_str.split(',').map(|s| s.trim().to_string()).collect()
        } else {
            Vec::new()
        };

        let user = sub_matches.get_one::<String>("user").cloned();

        let command = sub_matches.get_one::<String>("command")
            .ok_or_else(|| eyre::eyre!("Command is required"))?
            .clone();

        let args: Vec<String> = if let Some(args_iter) = sub_matches.get_many::<String>("args") {
            args_iter.cloned().collect()
        } else {
            Vec::new()
        };

        Ok(RunOptions {
            mount_dirs,
            user,
            command,
            args,
            env_vars: std::collections::HashMap::new(),
        })
    }

    /// Fork and execute with namespace isolation - kept for backward compatibility
    #[allow(dead_code)]
    pub fn fork_and_execute(&self, env_root: &Path, run_options: &RunOptions, cmd_path: &Path) -> Result<()> {
        fork_and_execute(env_root, run_options, cmd_path)
    }
}
