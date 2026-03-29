#![cfg(target_os = "linux")]

use color_eyre::eyre;
use color_eyre::Result;
use libc::{c_int, c_void, prctl, PR_CAPBSET_DROP, sethostname};
use log::{debug, trace, warn};
use nix::sched::{unshare, CloneFlags};
use nix::unistd::{fork, geteuid, getuid, getgid, ForkResult, Gid, Uid, Pid};
use std::fs;

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::ptr;
use std::panic::Location;

use crate::dirs;
use crate::mount::*;
use crate::models::{IsolateMode, ProcessCreationConfig, UnifiedChildContext, NamespaceStrategy};
use crate::run::RunOptions;
use crate::idmap::{IdMapSync, check_user_namespace_support, execute_idmap_for_parent, wait_for_idmap_sync};

/// Convert a host-side absolute path to a guest-side path.
/// If `host_path` is inside `env_root`, strip the env_root prefix and prepend "/".
/// Otherwise return the path unchanged.
fn convert_host_path_to_guest_path(host_path: &Path, env_root: &Path) -> PathBuf {
    if let Ok(stripped) = host_path.strip_prefix(env_root) {
        Path::new("/").join(stripped)
    } else {
        host_path.to_path_buf()
    }
}

// ============================================================================
// CALL GRAPH & CRITICAL PHASES
// ============================================================================
//
// High-level flow (entry point from run.rs):
//   fork_and_execute() → fork_and_execute_raw() → prepare_and_create_process()
//
// prepare_and_create_process():
//   ├── determine_process_config()  → ProcessCreationConfig
//   ├── build_unified_context()     → UnifiedChildContext
//   └── create_process_with_namespaces()
//
// create_process_with_namespaces():
//   └── Calls either:
//       • create_process_via_unshare()   (Unshare strategy)
//       • create_process_via_clone()     (Clone strategy)
//
// ============================================================================
// Unshare Strategy (namespace_strategy = Unshare)
// ============================================================================
//
// create_process_via_unshare(config, context)
// ├── unshare_namespaces_with_idmap(clone_flags, uid, gid)
// │   ├── unshare_with_error_handling(clone_flags)  → creates namespaces
// │   └── write_self_idmap(uid, gid)                → process writes own ID maps
// └── child_mount_and_exec(Box::new(context))
//     ├── mount_batch_specs()
//     ├── sandbox-mode specific setup (pivot_root for Fs, etc.)
//     └── prepare_and_execute_command()
//
// ============================================================================
// Clone Strategy (namespace_strategy = Clone)
// ============================================================================
//
// create_process_via_clone()
// ├── libc::clone(raw_flags = namespace_flags.bits() | SIGCHLD)
// ├── unified_child_main() (child entry)
// │   └── child_setup_with_namespaces()
// │       ├── write_self_idmap(uid, gid)  ← child writes own ID maps
// │       └── child_mount_and_exec()      → mounts and exec
// └── Parent: wait for child
//
// Note: Namespaces are always created at clone time.
//       For Fs mode, namespace_flags includes all namespaces (full_namespace_flags).
//       For Env/Vm modes, namespace_flags includes basic namespaces (mount+user if non-root).
//
// ============================================================================
// Key Insight: Self ID Mapping
// ============================================================================
//
// When creating a user namespace (clone/unshare CLONE_NEWUSER), the calling
// process has CAP_SETUID in the NEW namespace and can write /proc/self/uid_map.
// This is simpler and more reliable than using newuidmap/newgidmap.
//
// ============================================================================
// • Unshare strategy: Uses forked helper child to map parent (IdMapSync)
// • Clone strategy: Parent maps child (IdMapSync)
// • All mappings eventually call execute_idmap_for_pid() → newuidmap/newgidmap or simple fallback
//
// ============================================================================
// Mount Setup
// ============================================================================
//
// • Mount specs parsed in build_unified_context() via parse_mount_specs()
// • Executed in child_mount_and_exec() via mount_batch_specs()
// • Sandbox-mode‑specific mounts added in determine_process_config()
// • Order: make‑private/rslave mounts first, then compatibility, then user specs
//
// ============================================================================


/// Unshare namespaces with ID mapping coordination (replaces create_namespaces_inner)
fn unshare_namespaces_with_idmap(
    clone_flags: CloneFlags,
    uid: Uid,
    gid: Gid,
    opt_user: &Option<String>,
    allow_setgroups: bool,
) -> Result<()> {
    if let Err(e) = check_user_namespace_support() {
        warn!("User namespace check failed: {}", e);
    }

    debug!("unshare_namespaces_with_idmap called: clone_flags={:?}, contains CLONE_NEWUSER={}",
           clone_flags, clone_flags.contains(CloneFlags::CLONE_NEWUSER));

    // Handle user mapping if we need to create user namespace
    if clone_flags.contains(CloneFlags::CLONE_NEWUSER) {
        debug!("unshare_namespaces_with_idmap: calling unshare_with_user_ns_and_idmap");
        unshare_with_user_ns_and_idmap(clone_flags, uid, gid, opt_user, allow_setgroups)
    } else {
        debug!("unshare_namespaces_with_idmap: calling unshare_namespaces_simple");
        unshare_namespaces_simple(clone_flags)
    }
}

/// Unshare namespaces with user namespace and ID mapping via fork helper.
/// Uses fork helper to call newuidmap/newgidmap for subuid/subgid support.
///
/// For WSL2 compatibility, we use proper pipe synchronization:
/// 1. Fork helper before unshare (helper stays in original namespace)
/// 2. Parent unshares, then signals helper via pipe
/// 3. Helper writes parent's ID maps after parent is in new namespace
/// 4. Helper signals completion, parent continues
fn unshare_with_user_ns_and_idmap(
    clone_flags: CloneFlags,
    uid: Uid,
    gid: Gid,
    opt_user: &Option<String>,
    allow_setgroups: bool,
) -> Result<()> {
    use nix::unistd::{pipe, read, write};

    // Create two pipes for bidirectional synchronization:
    // - unshare_pipe: parent writes to signal "unshare complete", helper reads
    // - idmap_pipe: helper writes to signal "ID mapping complete", parent reads
    let (unshare_read, unshare_write) = pipe()?;
    let (idmap_read, idmap_write) = pipe()?;

    const SYNC_BYTE: u8 = 0x69;

    match unsafe { fork() } {
        Ok(ForkResult::Parent { child }) => {
            // Parent: close helper's pipe ends
            drop(unshare_read);
            drop(idmap_write);

            // Unshare namespaces first
            unshare_with_error_handling(clone_flags)?;
            trace!("Parent: successfully created namespaces");

            // Immediately set mount propagation to private
            set_mount_propagation_private_if_needed(clone_flags)?;

            // Signal helper that we've unshared and are ready for ID mapping
            write(&unshare_write, &[SYNC_BYTE])?;
            trace!("Parent: signaled helper to write ID maps");

            // Wait for helper to complete ID mapping
            let mut buf = [0u8; 1];
            read(&idmap_read, &mut buf)?;
            if buf[0] != SYNC_BYTE {
                return Err(eyre::eyre!("Invalid sync byte from helper"));
            }
            trace!("Parent: ID mapping completed");

            // Close remaining pipe ends
            drop(unshare_write);
            drop(idmap_read);

            // Wait for helper to exit
            match nix::sys::wait::waitpid(child, None) {
                Ok(nix::sys::wait::WaitStatus::Exited(_, 0)) => Ok(()),
                Ok(status) => Err(eyre::eyre!("ID mapping helper failed: {:?}", status)),
                Err(e) => Err(eyre::eyre!("Failed to wait for helper: {}", e)),
            }
        }
        Ok(ForkResult::Child) => {
            // Helper: close parent's pipe ends
            drop(unshare_write);
            drop(idmap_read);

            // Wait for parent to signal that unshare is complete
            let mut buf = [0u8; 1];
            read(&unshare_read, &mut buf)?;
            if buf[0] != SYNC_BYTE {
                std::process::exit(1);
            }
            trace!("Helper: parent signaled unshare complete, writing ID maps");

            // Now write parent's ID maps
            // Parent is in new user namespace, we're in original namespace
            // This works because we have same UID as parent in original namespace
            match execute_idmap_for_parent(uid, gid, opt_user, allow_setgroups) {
                Ok(()) => {
                    // Signal parent that ID mapping is done
                    let _ = write(&idmap_write, &[SYNC_BYTE]);
                    drop(unshare_read);
                    drop(idmap_write);
                    std::process::exit(0);
                }
                Err(e) => {
                    warn!("Helper: ID mapping failed: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Err(e) => Err(eyre::eyre!("Failed to fork helper: {}", e)),
    }
}

/// Set mount propagation to private immediately after creating mount namespace.
/// This prevents any mount operations from leaking to parent namespace.
/// Must be called immediately after unshare(CLONE_NEWNS) before any other mount operations.
fn set_mount_propagation_private_if_needed(clone_flags: CloneFlags) -> Result<()> {
    debug!("set_mount_propagation_private_if_needed called: clone_flags={:?}, contains CLONE_NEWNS={}",
           clone_flags, clone_flags.contains(CloneFlags::CLONE_NEWNS));
    if clone_flags.contains(CloneFlags::CLONE_NEWNS) {
        use nix::mount::{mount, MsFlags};
        let flags = MsFlags::MS_REC | MsFlags::MS_PRIVATE | MsFlags::from_bits_truncate(libc::MS_SILENT);
        debug!("Setting mount propagation to private with flags: {:?}", flags);
        mount(Some("none"), "/", Some(""), flags, Some(""))
            .map_err(|e| eyre::eyre!("Failed to set private mount propagation immediately after unshare: {}", e))?;
        debug!("Set mount propagation to private immediately after creating mount namespace");
    } else {
        debug!("SKIP set mount propagation to private (CLONE_NEWNS not in clone_flags)");
    }
    Ok(())
}

/// Unshare namespaces without user namespace (simple case)
fn unshare_namespaces_simple(clone_flags: CloneFlags) -> Result<()> {
    debug!("unshare_namespaces_simple called: clone_flags={:?}", clone_flags);
    // No user namespace needed, just unshare
    unshare_with_error_handling(clone_flags)?;
    debug!("Successfully created namespaces via unshare");

    // Immediately set mount propagation to private to prevent mount leaks
    set_mount_propagation_private_if_needed(clone_flags)?;

    Ok(())
}

/// Returns basic namespace flags (CLONE_NEWNS) with CLONE_NEWUSER added if not root.
fn basic_namespace_flags() -> CloneFlags {
    let mut flags = CloneFlags::CLONE_NEWNS;
    if !geteuid().is_root() {
        flags |= CloneFlags::CLONE_NEWUSER;
    }
    flags
}

/// Returns full namespace flags for Fs mode with Clone strategy.
fn full_namespace_flags() -> CloneFlags {
    let mut flags = basic_namespace_flags() | CloneFlags::CLONE_NEWPID;
    flags |= CloneFlags::CLONE_NEWUTS | CloneFlags::CLONE_NEWIPC | CloneFlags::CLONE_NEWNET;
    flags |= CloneFlags::CLONE_NEWCGROUP;
    flags
}

/// Determine if UID mapping is needed based on namespace flags and root status.
fn needs_uid_mapping(namespace_flags: CloneFlags) -> bool {
    namespace_flags.contains(CloneFlags::CLONE_NEWUSER) && !geteuid().is_root()
}

/// Determine unified process creation configuration from run options.
pub fn determine_process_config(env_root: &Path, run_options: &RunOptions) -> ProcessCreationConfig {
    use crate::models::{IsolateMode, NamespaceStrategy, ProcessCreationConfig};

    let isolate_mode = run_options.effective_sandbox.isolate_mode.unwrap_or(IsolateMode::Env);
    let namespace_strategy = if run_options.skip_namespace_isolation {
        NamespaceStrategy::Unshare
    } else {
        run_options.effective_sandbox.namespace_strategy.unwrap_or(NamespaceStrategy::Clone)
    };

    let namespace_flags = if run_options.skip_namespace_isolation {
        // No namespaces when isolation is skipped
        CloneFlags::empty()
    } else {
        match (isolate_mode, namespace_strategy) {
            (IsolateMode::Fs, NamespaceStrategy::Clone) => {
                // Fs mode uses clone with all namespaces at once
                full_namespace_flags()
            }
            (_, NamespaceStrategy::Clone) => {
                // Other modes with Clone strategy: create namespaces at clone time
                basic_namespace_flags()
            }
            (_, NamespaceStrategy::Unshare) => {
                // Unshare strategy: flags for unshare() call
                basic_namespace_flags()
            }
        }
    };

    let needs_uid_mapping = needs_uid_mapping(namespace_flags);

    // Start with empty mount spec strings
    let mut mount_spec_strings = Vec::new();

    if !run_options.skip_namespace_isolation {
        // Add sandbox-mode specific mount specifications (skip when no isolation)
        match isolate_mode {
            IsolateMode::Env => mount_spec_strings.extend(env_mount_spec_strings(env_root, run_options)),
            IsolateMode::Fs => mount_spec_strings.extend(fs_mount_spec_strings()),
            IsolateMode::Vm => mount_spec_strings.extend(vm_mount_spec_strings()),
        }

        // Add user-provided mount specifications
        mount_spec_strings.extend(run_options.effective_sandbox.mount_specs.iter().cloned());
    }

    ProcessCreationConfig {
        namespace_strategy,
        isolate_mode,
        namespace_flags,
        needs_uid_mapping,
        mount_spec_strings,
    }
}

/// Build a UnifiedChildContext from environment root, run options, and config.
pub fn build_unified_context(
    env_root: &Path,
    run_options: &RunOptions,
    config: &ProcessCreationConfig,
    command: PathBuf,
    args: Vec<String>,
    stdin_read_fd: Option<i32>,
) -> Result<UnifiedChildContext> {
    let uid = getuid();
    let gid = getgid();
    let euid = geteuid();
    let user = run_options.user.clone();

    // Parse mount specs
    let mount_specs = crate::mount::parse_mount_specs(
        &config.mount_spec_strings.iter().map(|s| s.as_str()).collect::<Vec<_>>()
    );

    // Record the original host UID for VM mount configuration
    // (this is before any namespace setup, so it's the real host UID)
    let mut run_options = run_options.clone();
    run_options.host_uid = Some(uid.as_raw());

    Ok(UnifiedChildContext {
        env_root: env_root.to_path_buf(),
        run_options,
        command,
        args,
        stdin_read_fd,
        isolate_mode: config.isolate_mode,
        sync_read_fd: None, // will be set later if needed
        mount_specs,
        uid,
        gid,
        euid,
        user,
        vm_socket_path: None,
    })
}

/// Create process with namespace isolation using one of two strategies:
/// 1. Unshare: unshare namespaces, forked helper maps parent IDs
/// 2. Clone: clone with namespace flags, parent maps child IDs using newuidmap/newgidmap
/// Returns child PID on success.
pub fn create_process_with_namespaces(
    config: &ProcessCreationConfig,
    context: UnifiedChildContext,
) -> Result<Pid> {
    // Create sync pipe for ID mapping coordination
    // - Clone: parent maps child, child waits for signal
    // - Unshare: forked helper maps parent, parent waits for signal
    let id_sync = if config.needs_uid_mapping {
        let target_pid = if config.namespace_strategy == NamespaceStrategy::Clone {
            Pid::from_raw(0) // placeholder, will be updated after clone
        } else {
            nix::unistd::getpid() // parent PID for helper to map
        };
        Some(IdMapSync::new(target_pid)?)
    } else {
        None
    };

    // Update context with sync fd if needed (child waits on this fd)
    let mut context = context;
    if let Some(ref sync) = id_sync {
        context.sync_read_fd = Some(sync.read_fd().try_clone()?);
    }

    match config.namespace_strategy {
        NamespaceStrategy::Unshare => {
            create_process_via_unshare(config, context, id_sync)
        }
        NamespaceStrategy::Clone => {
            create_process_via_clone(config, context, id_sync)
        }
    }
}

/// Implement Clone strategy: create child via clone() with namespace flags.
/// Namespaces are created at clone time. Parent maps child IDs using newuidmap/newgidmap.
fn create_process_via_clone(
    config: &ProcessCreationConfig,
    context: UnifiedChildContext,
    mut id_sync: Option<IdMapSync>,
) -> Result<Pid> {
    // Allocate stack for clone child (1MB, grows down)
    const STACK_SIZE: usize = 1024 * 1024;
    let stack = vec![0u8; STACK_SIZE];
    let stack_top = unsafe { stack.as_ptr().add(STACK_SIZE) as *mut c_void };

    // Prepare raw flags for libc::clone
    let raw_flags = config.namespace_flags.bits() as u64 | libc::SIGCHLD as u64;

    // Capture uid/gid/user before moving context into box
    let uid = context.uid;
    let gid = context.gid;
    let user = context.user.clone();

    // Box context to pass to child
    let context_ptr = Box::into_raw(Box::new(context));

    unsafe {
        let pid = libc::clone(
            unified_child_main as extern "C" fn(*mut c_void) -> c_int,
            stack_top,
            raw_flags as c_int,
            context_ptr as *mut c_void,
            ptr::null_mut::<c_int>(), // parent_tid
            ptr::null_mut::<c_int>(), // child_tid
            ptr::null_mut::<c_int>(), // tls
        );

        if pid < 0 {
            drop(Box::from_raw(context_ptr));
            return Err(eyre::eyre!(
                "Failed to clone process: {}",
                std::io::Error::last_os_error()
            ));
        }

        let child_pid = Pid::from_raw(pid);

        // Parent: map child's IDs using newuidmap/newgidmap and signal child to proceed
        if let Some(ref mut sync) = id_sync {
            sync.set_target_pid(child_pid);
            let allow_setgroups = config.isolate_mode == IsolateMode::Vm;
            sync.perform_mapping_and_signal(uid, gid, &user, allow_setgroups)?;
        }

        Ok(child_pid)
    }
}

/// Implement Unshare strategy: create namespaces via unshare(), then mount and exec.
/// Namespaces are created by unshare_namespaces_with_idmap() which also handles ID mapping.
/// After namespaces are ready, calls child_mount_and_exec() for mounts and command execution.
/// When namespace_flags is empty (e.g. skip_namespace_isolation), skips unshare and goes straight to mount/exec.
fn create_process_via_unshare(
    config: &ProcessCreationConfig,
    context: UnifiedChildContext,
    _id_sync: Option<IdMapSync>,
) -> Result<Pid> {
    // For Unshare strategy, we either:
    // 1. Call unshare() directly and exec (replaces current process)
    // 2. Fork first, then unshare() in child (preserves parent)

    // Implementation similar to current create_namespaces_inner()
    // but integrated with unified context and sync protocol

    let clone_flags = config.namespace_flags;
    if !clone_flags.is_empty() {
        let allow_setgroups = config.isolate_mode == IsolateMode::Vm;
        unshare_namespaces_with_idmap(clone_flags, context.uid, context.gid, &context.user, allow_setgroups)?;
    }

    let context = context;

    // Now execute in child context (replaces current process)
    // Note: For Unshare strategy, this function may not return

    // It should set up mounts and exec the command, never returning.
    // Will call prepare_and_execute_command() which exec's on success.
    child_mount_and_exec(Box::new(context))?;

    // If we reach here, exec failed and error was propagated.
    // Return a dummy PID (never actually used).
    Ok(Pid::from_raw(0))
}

/// Single child entry point for unified flow.
extern "C" fn unified_child_main(arg: *mut c_void) -> c_int {
    unsafe {
        let context = Box::from_raw(arg as *mut UnifiedChildContext);
        match child_setup_with_namespaces(context) {
            Ok(()) => 0,
            Err(e) => {
                debug!(
                    "Failed in child setup: {} (sandbox pivot_root/mount/exec path)",
                    e
                );
                eprintln!("Failed in child setup: {}", e);
                1
            }
        }
    }
}

/// Child-side setup for Clone strategy: wait for parent ID mapping if needed,
/// then mount and exec.
/// Called from unified_child_main() for Clone strategy.
fn child_setup_with_namespaces(context: Box<UnifiedChildContext>) -> Result<()> {
    // Wait for parent to complete ID mapping via newuidmap/newgidmap.
    // The parent (in original user namespace) has called newuidmap on our PID.
    if let Some(ref sync_fd) = context.sync_read_fd {
        wait_for_idmap_sync(sync_fd)?;
        trace!("Child: parent completed ID mapping");
    }

    child_mount_and_exec(context)
}

/// Ensure mount propagation is set to private to prevent mount leaks.
/// This should be called as early as possible after entering a mount namespace.
fn ensure_mount_propagation_private() -> Result<()> {
    use nix::mount::{mount, MsFlags};
    use nix::errno::Errno;

    let flags = MsFlags::MS_REC | MsFlags::MS_PRIVATE | MsFlags::from_bits_truncate(libc::MS_SILENT);
    debug!("ensure_mount_propagation_private: attempting to set private propagation with flags: {:?}", flags);

    match mount(Some("none"), "/", Some(""), flags, Some("")) {
        Ok(()) => {
            Ok(())
        }
        Err(e) => {
            // Check if we're already in private propagation or if error is recoverable
            match e {
                Errno::EINVAL => {
                    // EINVAL could mean we're already in the desired state or invalid arguments
                    // This is relatively safe to continue
                    debug!("ensure_mount_propagation_private: mount() returned EINVAL, assuming already in private propagation or invalid flags");
                    Ok(())
                }
                Errno::EPERM => {
                    // EPERM is critical - we lack CAP_SYS_ADMIN or other permissions
                    // This is unsafe to continue with mount operations
                    warn!("ensure_mount_propagation_private: failed with EPERM - cannot set private propagation, mount operations may leak!");
                    // When skip_namespace_isolation is set, allow continuing without private propagation
                    // This is acceptable for testing in restricted environments
                    Ok(())
                }
                Errno::EACCES => {
                    // EACCES - access denied (block device? read-only?)
                    warn!("ensure_mount_propagation_private: failed with EACCES - cannot access mount point");
                    Err(eyre::eyre!("Cannot ensure private mount propagation: EACCES"))
                }
                _ => {
                    // Other errors - log warning but decide based on context
                    warn!("ensure_mount_propagation_private: failed to set private propagation: {} (error: {:?})", e, e);
                    // For other errors, we might still continue, but this is risky
                    // Returning error to be safe
                    Err(eyre::eyre!("Cannot ensure private mount propagation: {}", e))
                }
            }
        }
    }
}

/// Mount specifications, perform sandbox-mode specific setup, and execute command.
/// Used by both Unshare strategy (after namespaces created) and Clone strategy
/// (after child_setup_with_namespaces()).
fn child_mount_and_exec(mut context: Box<UnifiedChildContext>) -> Result<()> {
    // Ensure mount propagation is private before any mounts (critical for preventing leaks)
    ensure_mount_propagation_private()?;

    // Mount all specifications
    crate::mount::mount_batch_specs(&context.mount_specs, &context.env_root, context.isolate_mode)?;
    setup_isolate_mode(&mut context)?;
    prepare_and_execute_command(
        &context.command,
        &context.args,
        &context.run_options.env_vars,
        context.run_options.chdir_to_env_root,
    )
}

fn setup_isolate_mode(context: &mut UnifiedChildContext) -> Result<()> {
    match context.isolate_mode {
        IsolateMode::Env => Ok(()),
        IsolateMode::Fs => setup_fs_sandbox(context),
        IsolateMode::Vm => setup_vm_sandbox(context),
    }
}

fn setup_fs_sandbox(context: &mut UnifiedChildContext) -> Result<()> {
    perform_fs_sandbox_tasks(context)?;

    let guest_command = convert_host_path_to_guest_path(&context.command, &context.env_root);
    if guest_command != context.command {
        trace!("Fs sandbox: adjusting command path from {} to {} after pivot", context.command.display(), guest_command.display());
        context.command = guest_command;
    }
    Ok(())
}

fn setup_vm_sandbox(context: &UnifiedChildContext) -> Result<()> {
    let guest_command = convert_host_path_to_guest_path(&context.command, &context.env_root);
    let order          = determine_vmm_backend_order(&context.run_options.vmm_order);
    try_vmm_backends(&order, context, &guest_command)
}

fn determine_vmm_backend_order(vmm_order: &[String]) -> Vec<String> {
    let mut order: Vec<String> = if !vmm_order.is_empty() {
        vmm_order.to_vec()
    } else {
        let mut default_order = Vec::new();
        #[cfg(feature = "libkrun")]
        {
            default_order.push("libkrun".to_string());
        }
        default_order.push("qemu".to_string());
        default_order
    };
    order.dedup();
    order
}

fn try_vmm_backends(order: &[String], context: &UnifiedChildContext, guest_command: &Path) -> Result<()> {
    let _ = crate::qemu::ensure_vmm_log_dir();

    let order: Vec<String> = if context.run_options.vm_reuse_connect {
        vec!["qemu".to_string()]
    } else {
        order.to_vec()
    };

    let mut last_err: Option<eyre::Report> = None;

    for backend in &order {
        match backend.as_str() {
            "libkrun" => {
                if let Err(e) = try_krun_backend(context, guest_command) {
                    log::warn!("libkrun backend failed, will try next VMM if any: {}", e);
                    last_err = Some(e);
                    continue;
                }
            }
            "qemu" => {
                if let Err(e) = try_qemu_backend(context, guest_command) {
                    log::warn!("qemu backend failed, will try next VMM if any: {}", e);
                    last_err = Some(e);
                    continue;
                }
            }
            other => {
                log::warn!("Unknown VMM backend '{}' in --vmm list, skipping", other);
            }
        }
    }

    if let Some(e) = last_err {
        return Err(eyre::eyre!(
            "All requested VMM backends failed (order: {:?}); last error: {}",
            order,
            e
        ));
    }

    Err(eyre::eyre!(
        "No usable VMM backend found for order {:?}. \
         Specify --vmm=libkrun,qemu or --vmm=qemu and ensure dependencies are installed.",
        order
    ))
}

fn try_krun_backend(context: &UnifiedChildContext, guest_command: &Path) -> Result<()> {
    #[cfg(feature = "libkrun")]
    {
        log::debug!("Trying VMM backend: libkrun");
        match crate::libkrun::run_command_in_krun(
            &context.env_root,
            &context.run_options,
            guest_command,
        ) {
            Ok(()) => unreachable!("run_command_in_krun never returns on success"),
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("HostAddressNotAvailable") || msg.contains("GuestMemoryMmap") {
                    return Err(eyre::eyre!(
                        "{}. Hint: host address-space layout can cause this; try --memory 4096 or EPKG_VM_MEMORY=4096, or use --vmm=qemu.",
                        msg
                    ));
                }
                Err(e)
            }
        }
    }
    #[cfg(not(feature = "libkrun"))]
    {
        let _ = (context, guest_command);
        log::debug!("VMM backend 'libkrun' requested but libkrun feature is disabled; skipping");
        Err(eyre::eyre!("libkrun feature disabled"))
    }
}

fn try_qemu_backend(context: &UnifiedChildContext, guest_command: &Path) -> Result<()> {
    log::debug!("Trying VMM backend: qemu");
    match crate::qemu::run_command_in_qemu(
        &context.env_root,
        &context.run_options,
        guest_command,
        context.vm_socket_path.as_deref(),
    ) {
        Ok(()) => unreachable!("run_command_in_qemu never returns on success"),
        Err(e) => Err(e),
    }
}

// Helper function for Fs mode setup
fn perform_fs_sandbox_tasks(context: &UnifiedChildContext) -> Result<()> {
    // Create oldroot directory for pivot_root
    let oldroot = context.env_root.join("oldroot");
    fs::create_dir_all(&oldroot)
        .map_err(|e| eyre::eyre!("Failed to create oldroot directory: {}", e))?;

    // Setup /dev symlinks and directories (like bwrap)
    setup_sandbox_dev_tree(&context.env_root)?;

    // Pivot into sandbox and drop capabilities
    pivot_into_sandbox_and_drop_caps(&context.env_root, &oldroot, context.euid)?;

    Ok(())
}


/// Drop all capabilities from the bounding set (like bwrap's PR_CAPBSET_DROP).
fn drop_all_capabilities() {
    // Drop capabilities 0..=40 (CAP_CHECKPOINT_RESTORE)
    for cap in 0..=40 {
        unsafe {
            // prctl returns 0 on success, -1 on error; ignore errors
            let _ = prctl(PR_CAPBSET_DROP, cap as libc::c_ulong, 0, 0, 0);
        }
    }
    trace!("Dropped all capability bounding set entries");
}

/// Create /dev symlinks and directories (like bwrap).
fn setup_sandbox_dev_tree(env_root: &Path) -> Result<()> {
    crate::mount::ensure_dev_symlinks(&env_root.join("dev"))
}

/// Pivot into filesystem/container sandbox and drop capabilities via nested user namespace
fn pivot_into_sandbox_and_drop_caps(new_root_base: &Path, oldroot: &Path, _euid: Uid) -> Result<()> {
    // Set hostname to "sandbox" (like bwrap) for UTS isolation
    unsafe {
        let hostname = b"sandbox\0";
        if sethostname(hostname.as_ptr() as *const _, hostname.len() - 1) < 0 {
            warn!("Failed to set hostname: {}. Continuing.", std::io::Error::last_os_error());
        }
    }

    pivot_to_sandbox(new_root_base, oldroot)?;

    // After pivot, drop capabilities by entering a nested user namespace
    // Always create user namespace (even as root) to drop capabilities
    match unshare_with_error_handling(CloneFlags::CLONE_NEWUSER) {
        Ok(()) => {
            debug!("Clone child: entered nested user namespace (dropped capabilities)");
            drop_all_capabilities();
        }
        Err(e) => warn!("Failed to create nested user namespace after pivot: {}. Continuing.", e),
    }
    Ok(())
}

/// Prepare environment variables and execute command
fn prepare_and_execute_command(command: &Path, args: &[String], env_vars: &std::collections::HashMap<String, String>, chdir_to_env_root: bool) -> Result<()> {
    // Change to environment root directory if requested
    if chdir_to_env_root {
        if let Err(e) = std::env::set_current_dir("/") {
            return Err(eyre::eyre!("Failed to change dir to /: {}", e));
        }
    }

    // Prepare environment variables
    let mut env_vars = env_vars.clone();
    env_vars.insert("LC_ALL".to_string(), "C".to_string());
    env_vars.insert("LANG".to_string(), "C".to_string());
    env_vars.insert("LC_CTYPE".to_string(), "C".to_string());
    env_vars.insert("LC_COLLATE".to_string(), "C".to_string());

    // Execute the command
    debug!("Clone child executing: {} {:?}", command.display(), args);
    let err = std::process::Command::new(command)
        .args(args)
        .envs(&env_vars)
        .exec();
    // exec only returns on error
    Err(eyre::eyre!("Failed to execute command: {}", err))
}


/// Mounts for the "Env" sandbox mode
fn env_mount_spec_strings(env_root: &Path, _run_options: &RunOptions) -> Vec<String> {
    use nix::unistd::{getuid, geteuid};
    let uid = getuid();
    let euid = geteuid();
    let mut specs = Vec::new();

    // Always make mounts private to prevent mount leaks to parent namespace.
    // This ensures that when epkg exits, Linux automatically cleans up all mounts.
    // Without this, recursive bind mounts (especially /opt/epkg) can leak and
    // create thousands of nested mount points if interrupted (Ctrl+C, timeout, etc).
    specs.push("make-rprivate://".to_string());  // use "//" for host dir

    // Add traditional layout compatibility mounts (must be before /usr mount)
    match crate::mount::mount_traditional_host_compatibility(env_root) {
        Ok(mut cspecs) => specs.append(&mut cspecs),
        Err(e) => warn!("Failed to generate traditional layout compatibility mounts: {}", e),
    }

    // Add /opt/epkg isolation mounts
    match crate::mount::mount_opt_epkg_isolation(euid, uid, env_root) {
        Ok(mut ospecs) => specs.append(&mut ospecs),
        Err(e) => warn!("Failed to generate /opt/epkg isolation mounts: {}", e),
    }

    // Add standard environment mount specifications
    specs.extend(crate::mount::MOUNT_SPECS_ENV.iter().map(|s| s.to_string()));

    // Add /root mount for non-root users
    if !uid.is_root() {
        specs.push("@/root://root".to_string());
    }

    // Mount host's network configuration files for DNS resolution.
    // These are mounted after the environment's /etc to override missing files.
    // Use :try to silently skip if files don't exist on host.
    specs.push("/etc/hosts://etc/hosts:try".to_string());
    specs.push("/etc/resolv.conf://etc/resolv.conf:try".to_string());

    specs
}

fn fs_mount_spec_strings() -> Vec<String> {
    let mut specs: Vec<String> = Vec::new();

    // Always make mounts private to prevent mount leaks to parent namespace.
    // This ensures that when epkg exits, Linux automatically cleans up all mounts.
    // Without this, bind mounts (especially /opt/epkg) can leak and create
    // thousands of nested mount points if interrupted (Ctrl+C, timeout, etc).
    specs.insert(0, "make-rprivate://:silent".to_string());  // use "//" for host dir

    specs.extend(crate::mount::pseudo_fs_mount_spec_strings().iter().map(|s| s.to_string()));
    add_epkg_mount_spec_strings(&mut specs);

    specs
}

fn vm_mount_spec_strings() -> Vec<String> {
    let mut spec_strings = Vec::new();

    // Always make mounts private to prevent mount leaks to parent namespace.
    // This ensures that when epkg exits, Linux automatically cleans up all mounts.
    spec_strings.push("make-rprivate://".to_string());

    add_epkg_mount_spec_strings(&mut spec_strings);
    // Mount host /lib/modules read-only for kernel module loading (e.g., virtio_net)
    // only when it actually exists on the host. Keep this best-effort and avoid
    // noisy mount failures on minimal systems where /lib/modules is absent.
    if std::path::Path::new("/lib/modules").exists() {
        spec_strings.push("/lib/modules:ro,try".to_string());
    }

    spec_strings
}

/// Add mount for epkg binary dir so guest init can find epkg for vm-daemon.
/// When epkg is outside self env (e.g. target/debug), bind-mount its dir into env.
fn add_epkg_bin_dir_mount(spec_strings: &mut Vec<String>) {
    let Ok(epkg_exe) = std::env::current_exe() else { return };
    let epkg_bin_dir = match epkg_exe.parent() {
        Some(p) => p.to_path_buf(),
        None => return,
    };

    // Skip if epkg is already inside self env (e.g. installed via epkg install)
    if epkg_bin_dir.starts_with(dirs().home_epkg.clone()) {
        return;
    }
    if epkg_bin_dir.starts_with(dirs().opt_epkg.clone()) {
        return;
    }

    spec_strings.push(format!("{}:ro", epkg_bin_dir.display().to_string()));
}

fn add_epkg_mount_spec_strings(spec_strings: &mut Vec<String>) {
    spec_strings.push(format!("{}:try", dirs().home_epkg.display()));
    spec_strings.push(format!("{}:try", dirs().home_cache.display()));
    // Mount /opt/epkg read-only if we're not root on host.
    // In VM sandbox, we appear as root but host filesystem permissions still apply,
    // so write attempts to /opt/epkg/cache would fail with EPERM.
    let opt_epkg_opts = if crate::utils::should_mount_opt_epkg_readonly() {
        "ro,try"
    } else {
        "try"
    };
    spec_strings.push(format!("{}:{}", dirs().opt_epkg.display(), opt_epkg_opts));
    add_epkg_bin_dir_mount(spec_strings);
}

/// Execute unshare with comprehensive error handling
#[track_caller]
fn unshare_with_error_handling(clone_flags: CloneFlags) -> Result<()> {
    unshare(clone_flags).map_err(|e| {
        let location = Location::caller();
        eyre::eyre!("unshare() failed at {}:{}: {}: {}", location.file(), location.line(), e, e.desc())
    })
}

