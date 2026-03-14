//! Mount management subsystem for epkg sandboxes
//!
//! This module implements a layered architecture for mount specification and execution:
//! 1. **Configuration Layer**: Callers express mount requirements as strings:
//!    - `mount_specs: Vec<String>`: Docker-like specifications (e.g., "/usr,ro")
//! 2. **Parsing Layer**: Only `parse_mount_spec()` creates validated `MountSpec` objects
//!    from string specifications, enforcing consistency and correctness.
//! 3. **Execution Layer**: `mount_spec()` executes `MountSpec` objects with appropriate
//!    context (environment root, mount mode).
//!
//! Key architectural principles:
//! - **Single Authority**: Only `parse_mount_spec()` constructs `MountSpec` objects
//! - **String-Based Configuration**: All mount requirements expressed as strings
//! - **Absolute Paths Only**: Mount specification paths must start with '/' (absolute host path) or '@' (env_root substitution). Relative paths are not allowed.
//!
//! The system supports three mount modes:
//! - `SandboxMode::Env`: Overlay mounts for namespace sandbox (environment→host)
//! - `SandboxMode::Fs`: pivot_root into environment root; proc, tmpfs, and dev mounts under it (env as root)
//! - `SandboxMode::Vm`: Virtual machine mounts via virtiofs (host→guest environment)
//!
//! See `models.rs` for `MountSpec` structure and `namespace.rs` for integration with
//! sandbox modes.

use color_eyre::eyre;
use color_eyre::Result;
use log::{debug, trace, warn};
use nix::errno::Errno;
use nix::mount::{mount, umount2, MntFlags, MsFlags};
use nix::unistd::{pivot_root, Uid};
use libc;
use serde_json;
use std::fs;
use crate::lfs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::busybox::mount::parse_mount_options;
use crate::dirs;
use crate::models::{config, MountSpec, SandboxMode};
use crate::run::RunOptions;
use crate::utils;

// Mount specification constants
// ============================================================================

/// Mount specifications for parse_mount_spec() - namespace mode
pub static MOUNT_SPECS_ENV: &[&str] = &[
    "@/usr://usr",      // Bind $env_root/usr to /usr
    "@/etc://etc",
    "@/var://var",
    "@/run://run:try",
];

/// Mount specifications for parse_mount_spec() - tmpfs sandbox mode (basic)
static MOUNT_SPECS_FS: &[&str] = &[
    "tmpfs:/tmp:mode=0755,silent,relatime,try",
    "proc:/proc:silent,relatime,try",
];

/// Device node bind mounts
static DEVFS_MOUNTS: &[&str] = &[
    "tmpfs:/dev:mode=0755,silent,relatime,try",
    "devpts:/dev/pts:newinstance,ptmxmode=0666,mode=620,silent,relatime,try",
    "mqueue:/dev/mqueue:nosuid,nodev,noexec,silent,relatime,try",

    "/dev/null:recursive,silent,relatime,try",
    "/dev/zero:recursive,silent,relatime,try",
    "/dev/full:recursive,silent,relatime,try",
    "/dev/random:recursive,silent,relatime,try",
    "/dev/urandom:recursive,silent,relatime,try",
    "/dev/tty:recursive,silent,relatime,try",
    "/dev/console:recursive,silent,relatime,try",
];

/// sysfs and subdirectory read-only mounts
/// Targets are absolute paths inside sandbox (e.g., "/sys")
static SYSFS_MOUNTS_RO: &[&str] = &[
    "/sys:ro,recursive,silent,nosuid,nodev,noexec,relatime,try",
];
#[allow(dead_code)]
static SYSFS_MOUNTS_RW: &[&str] = &[
    "/sys:recursive,silent,nosuid,nodev,noexec,relatime,try",
    "remount:/sys/kernel/security:ro,silent,nosuid,nodev,noexec,relatime,try",
    "remount:/sys/fs/cgroup:ro,silent,nosuid,nodev,noexec,relatime,try",
    "remount:/sys/fs/pstore:ro,silent,nosuid,nodev,noexec,relatime,try",
    "remount:/sys/firmware/efi/efivars:ro,silent,nosuid,nodev,noexec,relatime,try",
    "remount:/sys/fs/bpf:ro,silent,nosuid,nodev,noexec,relatime,try",
    "remount:/sys/kernel/debug:ro,silent,nosuid,nodev,noexec,relatime,try",
    "remount:/sys/kernel/tracing:ro,silent,nosuid,nodev,noexec,relatime,try",
    "remount:/sys/fs/fuse/connections:ro,silent,nosuid,nodev,noexec,relatime,try",
    "remount:/sys/kernel/config:ro,silent,nosuid,nodev,noexec,relatime,try",
];

/// Linux pseudo filesystem types that can be used as FS_TYPE in mount specifications
static PSEUDO_FS_TYPES: &[&str] = &[
    "tmpfs", "proc", "devtmpfs", "devpts", "mqueue", "debugfs", "hugetlbfs", "tracefs",
    "fusectl", "configfs", "binfmt_misc", "rpc_pipefs", "sysfs", "cgroup", "cgroup2",
    "pstore", "bpf", "securityfs", "efivarfs", "ramfs", "overlay",
];

/// Check if a string is a known pseudo filesystem type
fn is_pseudo_fs_type(s: &str) -> bool {
    PSEUDO_FS_TYPES.contains(&s)
}

/// Strip leading '@' prefix if present (used for env_root placeholder)
fn strip_at_prefix(s: &str) -> &str {
    s.strip_prefix('@').unwrap_or(s)
}

/// Returns the pseudo-fs mount specification strings for SandboxMode::Fs
pub(crate) fn pseudo_fs_mount_spec_strings() -> Vec<&'static str> {
    let mut specs = Vec::new();
    specs.extend_from_slice(MOUNT_SPECS_FS);
    specs.extend_from_slice(DEVFS_MOUNTS);
    specs.extend_from_slice(SYSFS_MOUNTS_RO);
    specs
}

/// Mount specification strings for VMM init (proc, sys, tmp). Run at root (/).
/// /dev is handled in run_init with mount().or_else() for devtmpfs/tmpfs fallback.
pub(crate) static MOUNT_SPECS_VMM_INIT: &[&str] = &[
    // "proc:/proc:silent,relatime,try",  // mounted by init_logging_early()
    "sysfs:/sys:silent,relatime,try",
    "tmpfs:/tmp:mode=0755,silent,relatime,try",
];

/// Mount spec for devpts (use with env_root=/dev so target is /dev/pts).
pub(crate) static MOUNT_SPEC_VMM_INIT_DEVPTS: &str = "devpts:@/pts:newinstance,ptmxmode=0666,mode=0620,try";

/// Returns mount spec strings for VMM init (proc, sys, tmp).
pub(crate) fn vmm_init_mount_spec_strings() -> Vec<&'static str> {
    MOUNT_SPECS_VMM_INIT.to_vec()
}

/// Mount spec strings from a slice, then execute. No RunOptions (for init).
pub(crate) fn mount_spec_strings(
    spec_strings: &[&str],
    env_root: &Path,
    sandbox_mode: SandboxMode,
) -> Result<()> {
    let mounts = parse_mount_specs(spec_strings);
    mount_batch_specs(&mounts, env_root, sandbox_mode)
}

/// Create /dev/shm and standard I/O symlinks (shared by setup_sandbox_dev_tree and VMM init).
pub(crate) fn ensure_dev_symlinks(dev_root: &Path) -> Result<()> {
    let dev_shm = dev_root.join("shm");
    if let Err(e) = lfs::create_dir_all(&dev_shm) {
        warn!("Failed to create {}/shm: {}. Continuing.", dev_root.display(), e);
    }

    let symlinks = [
        ("stdin", "/proc/self/fd/0"),
        ("stdout", "/proc/self/fd/1"),
        ("stderr", "/proc/self/fd/2"),
        ("fd", "/proc/self/fd"),
        ("core", "/proc/kcore"),
    ];

    for (name, target) in &symlinks {
        let link_path = dev_root.join(name);
        if lfs::exists_on_host(&link_path) {
            continue;
        }
        if let Err(e) = lfs::symlink(target, &link_path) {
            warn!("Failed to create symlink {}/{} -> {}: {}. Continuing.", dev_root.display(), name, target, e);
        }
    }

    Ok(())
}

/// Create minimal device nodes when /dev is tmpfs or devtmpfs is incomplete (VMM init only).
#[cfg(target_os = "linux")]
pub(crate) fn ensure_minimal_dev_nodes(dev_root: &Path) -> Result<()> {
    use nix::sys::stat::{makedev, mknod, Mode, SFlag};

    let dev_nodes: &[(&str, u64, u64)] = &[
        ("null", 1, 3),
        ("zero", 1, 5),
        ("full", 1, 7),
        ("random", 1, 8),
        ("urandom", 1, 9),
        ("tty", 5, 0),
        ("console", 5, 1),
        ("ptmx", 5, 2),
    ];
    let perm = Mode::from_bits(0o0666).unwrap_or(Mode::empty());
    let kind = SFlag::from_bits_truncate(libc::S_IFCHR as nix::libc::mode_t);
    for (name, major, minor) in dev_nodes {
        let path = dev_root.join(name);
        if lfs::exists_on_host(&path) {
            continue;
        }
        let dev = makedev(*major, *minor);
        if let Err(e) = mknod(&path, kind, perm, dev) {
            warn!("mknod {}/{}: {}", dev_root.display(), name, e);
        }
    }

    for minor in 0u64..=6 {
        let name = format!("tty{}", minor);
        let path = dev_root.join(&name);
        if lfs::exists_on_host(&path) {
            continue;
        }
        let dev = makedev(4, minor);
        if let Err(e) = mknod(&path, kind, perm, dev) {
            warn!("mknod {}/{}: {}", dev_root.display(), name, e);
        }
    }

    Ok(())
}

/// Mount devpts on dev_root/pts for PTY support (VMM init, vm-daemon pty mode).
/// Uses mount spec string rather than direct mount().
#[cfg(target_os = "linux")]
pub(crate) fn ensure_devpts_mount(dev_root: &Path) -> Result<()> {
    let _ = mount_spec_strings(
        &[MOUNT_SPEC_VMM_INIT_DEVPTS],
        dev_root,
        SandboxMode::Vm,
    );
    Ok(())
}

/// Substitute '@' prefix with env_root in a path string.
/// - If path starts with '@', replace '@' with env_root (e.g., "@/tmp" -> env_root + "/tmp")
/// - If path starts with '/', treat as absolute host path
/// - Otherwise, join with env_root (relative path)
///
/// Note: Mount specification strings must start with '/' or '@'; relative paths are not allowed.
/// This function still supports relative paths for other internal uses.
fn substitute_env_root(path: &str, env_root: &Path) -> PathBuf {
    if let Some(stripped) = path.strip_prefix("@/") {
        // '@' prefix: substitute env_root
        env_root.join(stripped)
    } else if path.starts_with('/') {
        // Absolute host path
        PathBuf::from(path)
    } else {
        // Relative to env_root
        env_root.join(path)
    }
}

/// Resolve source and target paths for a mount specification.
/// Paths are resolved relative to `env_root` (which may be the environment root
/// or the sandbox root, depending on caller):
/// - If a path starts with '/', it's treated as absolute (host path).
/// - Otherwise, it's joined with `env_root`.
/// - If a path starts with '@', the '@' is replaced with env_root.
///
/// Note: Mount specification strings must start with '/' or '@'; relative paths are not allowed.
/// This function still supports relative paths for other internal uses.
fn resolve_mount_paths(spec: &MountSpec, env_root: &Path) -> (PathBuf, PathBuf) {
    let source = substitute_env_root(&spec.source, env_root);
    let target = substitute_env_root(&spec.target, env_root);
    (source, target)
}

/// Unified mount function for MountSpec
pub(crate) fn mount_spec(spec: &MountSpec, env_root: &Path, sandbox_mode: SandboxMode) -> Result<()> {
    let (source, target) = resolve_mount_paths(spec, env_root);

    // Bind mount with source: check existence once and handle try vs required
    match bind_mount_source_exists(spec, &source) {
        Some(false) => {
            if spec.try_only {
                return Ok(());
            }
            return Err(eyre::eyre!(
                "Bind mount source does not exist: {} -> {}\n\
                 Create the path or add ':try' option to mount spec if this mount is optional.",
                source.display(),
                target.display()
            ));
        }
        _ => {}
    }

    // Use flags from spec
    let flags = spec.ms_flags();
    let options = spec.options.as_deref();

    // Determine filesystem type: empty string indicates bind/remount operations
    let result = if spec.fs_type.is_empty() {
        mount_bind_remount_propagation(spec, source, target, flags, options, sandbox_mode)
    } else {
        mount_filesystem_type(spec, target, flags, options, sandbox_mode)
    };

    match result {
        Ok(()) => Ok(()),
        Err(e) => {
            if spec.try_only {
                warn!("Mount spec failed (try_only): {} -> {}: {}",
                      if spec.source.is_empty() { "none" } else { &spec.source },
                      spec.target,
                      e);
                Ok(())
            } else {
                Err(e)
            }
        }
    }
}

/// Check if a path exists after resolving symlinks (Docker behavior).
/// Returns true if the canonical path exists, false otherwise (dangling symlink, missing file, etc.).
fn source_exists_after_resolution(path: &Path) -> bool {
    std::fs::canonicalize(path).is_ok()
}

/// If the spec is a bind mount (not remount) with a source, returns `Some(true)` when the source
/// exists after symlink resolution, `Some(false)` when it does not. Returns `None` when the spec
/// is not a bind-with-source (e.g. remount or no source). Used to run the existence check once.
fn bind_mount_source_exists(spec: &MountSpec, source: &PathBuf) -> Option<bool> {
    if !spec.fs_type.is_empty()
        || !spec.ms_flags().contains(MsFlags::MS_BIND)
        || spec.ms_flags().contains(MsFlags::MS_REMOUNT)
    {
        return None;
    }
    if spec.source.is_empty() {
        return None;
    }
    Some(source_exists_after_resolution(source))
}

/// Ensure target path exists for a bind mount, creating a placeholder if needed.
/// Only creates placeholders in Tmpfs/Vmm modes, plus self-bind case in Env mode (see below).
fn ensure_bind_target_exists(source: &PathBuf, target: &Path, sandbox_mode: SandboxMode) -> Result<()> {
    use std::fs;

    // If target already exists, nothing to do
    // For Fs/Vm modes, target is in env_root; for Env mode, we return early
    if sandbox_mode == SandboxMode::Fs || sandbox_mode == SandboxMode::Vm {
        if lfs::exists_or_any_symlink(target) {
            return Ok(());
        }
    } else {
        if lfs::exists_on_host(target) {
            return Ok(());
        }
    }

    // Self-bind (source == target): e.g. @/run:/run resolves to env_root/run for both.
    // If the path does not exist, create it so the bind mount can succeed. Safe because
    // we are creating the path we are about to mount, not arbitrary host directories.
    if source == target {
        if let Some(parent) = target.parent() {
            if !lfs::exists_or_any_symlink(parent) {
                lfs::create_dir_all(parent)?;
            }
        }
        trace!("Creating directory for self-bind mount: {}", target.display());
        lfs::create_dir_all(target)?;
        return Ok(());
    }

    // Only create placeholders in sandbox environments (Fs/Vm); don't create in host root for Env
    match sandbox_mode {
        SandboxMode::Fs | SandboxMode::Vm => (),
        SandboxMode::Env => return Ok(()), // Don't create files in host root
    }

    // If source doesn't exist, cannot determine type (may be try_only mount)
    if !lfs::exists_on_host(source) {
        return Ok(());
    }

    // Get source metadata to determine file type
    let metadata = match fs::symlink_metadata(source) {
        Ok(md) => md,
        Err(_) => return Ok(()), // Cannot stat source, skip
    };

    // Ensure parent directory exists
    if let Some(parent) = target.parent() {
        if !lfs::exists_or_any_symlink(parent) {
            let _ = lfs::create_dir_all(parent);
        }
    }

    // Create appropriate placeholder
    if metadata.is_file() {
        trace!("Creating file placeholder for bind mount: {}", target.display());
        let _ = lfs::file_create(target);
    } else if metadata.is_dir() {
        trace!("Creating directory placeholder for bind mount: {}", target.display());
        let _ = lfs::create_dir_all(target);
    }
    // Other types (symlinks, device nodes, etc.) - create empty file as fallback
    // Device nodes cannot be created without mknod capability; empty file works as bind mount target
    else {
        trace!("Creating empty file placeholder for special file bind mount: {}", target.display());
        let _ = lfs::file_create(target);
    }

    Ok(())
}

/// Ensure target path exists for a filesystem mount, creating directory if needed.
/// Only creates directories for Tmpfs and Vmm mount modes (sandbox environments).
fn ensure_mount_target_exists(target: &Path, sandbox_mode: SandboxMode) -> Result<()> {
    // Only create directories in sandbox environments
    match sandbox_mode {
        SandboxMode::Fs | SandboxMode::Vm => (),
        SandboxMode::Env => return Ok(()), // Don't create directories in host root
    }

    // If target already exists, nothing to do
    if lfs::exists_or_any_symlink(target) {
        return Ok(());
    }

    // Ensure parent directory exists
    if let Some(parent) = target.parent() {
        if !lfs::exists_or_any_symlink(parent) {
            let _ = lfs::create_dir_all(parent);
        }
    }

    // Create target directory
    trace!("Creating directory for filesystem mount: {}", target.display());
    let _ = lfs::create_dir_all(target);

    Ok(())
}

/// Handle bind, remount, and propagation mounts (when fs_type is empty)
fn mount_bind_remount_propagation(
    spec: &MountSpec,
    source: PathBuf,
    target: PathBuf,
    flags: MsFlags,
    options: Option<&str>,
    sandbox_mode: SandboxMode,
) -> Result<()> {
    let raw_flags = spec.flags;
    let propagation_mask = libc::MS_SHARED as i32 | libc::MS_SLAVE as i32 | libc::MS_PRIVATE as i32 | libc::MS_UNBINDABLE as i32;
    if (raw_flags as i32) & propagation_mask != 0 {
        // Propagation-only mount (e.g., mount --make-slave)
        // Use source "none" and empty fstype
        mount_filesystem("none", &target, "", flags, options)
    } else if flags.contains(MsFlags::MS_REMOUNT) {
        // Remount operation (may also have MS_BIND for bind mount remount)
        mount_remount_ro(&target, flags, options)
    } else if flags.contains(MsFlags::MS_BIND) {
        ensure_bind_target_exists(&source, &target, sandbox_mode)?;
        if flags.contains(MsFlags::MS_RDONLY) {
            // Linux kernel ignores MS_RDONLY when passed with MS_BIND; the mount inherits
            // the read-write/read-only state from the source. To guarantee a read-only
            // bind mount, we must use two syscalls: first bind mount (without MS_RDONLY),
            // then remount with MS_REMOUNT|MS_BIND|MS_RDONLY (like bwrap).
            mount_bind(&source, &target, flags & !MsFlags::MS_RDONLY, options)
                .and_then(|()| mount_remount_ro(&target, flags, options))
        } else {
            // Regular bind mount
            mount_bind(&source, &target, flags, options)
        }
    } else {
        // Invalid: neither bind nor remount nor propagation
        Err(eyre::eyre!(
            "Invalid mount specification: no filesystem type and neither MS_REMOUNT nor MS_BIND nor propagation flag set"
        ))
    }
}

/// Handle filesystem mounts (when fs_type is not empty)
fn mount_filesystem_type(
    spec: &MountSpec,
    target: PathBuf,
    flags: MsFlags,
    options: Option<&str>,
    sandbox_mode: SandboxMode,
) -> Result<()> {
    let fstype = spec.fs_type.as_str();
    let mut final_flags = flags;
    // Add default flags based on filesystem type
    match fstype {
        "proc" | "sysfs" => {
            final_flags |= MsFlags::MS_NOSUID | MsFlags::MS_NODEV | MsFlags::MS_NOEXEC;
        }
        "tmpfs" => {
            final_flags |= MsFlags::MS_NOSUID | MsFlags::MS_NODEV;
        }
        _ => {}
    }
    ensure_mount_target_exists(&target, sandbox_mode)?;
    mount_filesystem(fstype, &target, fstype, final_flags, options)
}

/// Helper for bind mounts
fn mount_bind(source: &PathBuf, target: &Path, flags: MsFlags, options: Option<&str>) -> Result<()> {
    debug!("Attempting bind mount: {} -> {} (flags: {:?}, options: {:?})", source.display(), target.display(), flags, options);
    mount(Some(source.as_path()), target, Some(""), flags | MsFlags::MS_BIND, options)
        .map_err(|e| eyre::eyre!("Failed to bind mount {} -> {}: {}", source.display(), target.display(), e))
}

/// Remount root as read-write. Used by VMM init when virtiofs mounts root readonly.
pub(crate) fn remount_root_rw() -> Result<()> {
    let root = Path::new("/");
    mount::<Path, Path, str, str>(Some(root), root, None, MsFlags::MS_REMOUNT, None::<&str>)
        .map_err(|e| eyre::eyre!("Failed to remount / read-write: {}", e))
}

/// Helper to remount existing mount point as read-only
fn mount_remount_ro(target: &Path, flags: MsFlags, options: Option<&str>) -> Result<()> {
    debug!("Attempting remount read-only: {} (flags: {:?}, options: {:?})", target.display(), flags, options);
    // Second step of read-only bind mount: change mount properties without affecting source.
    // MS_REMOUNT|MS_BIND|MS_RDONLY ensures the bind mount becomes read-only even if the
    // underlying filesystem is writable. This is the standard security sandboxing pattern.
    mount::<Path, Path, str, str>(Some(target), target, None, flags | MsFlags::MS_REMOUNT | MsFlags::MS_BIND | MsFlags::MS_RDONLY, options)
        .map_err(|e| eyre::eyre!("Failed to remount {} read-only: {}", target.display(), e))
}

/// Helper to mount special filesystems
fn mount_filesystem(source: &str, target: &Path, fstype: &str, flags: MsFlags, options: Option<&str>) -> Result<()> {
    debug!("Attempting filesystem mount: {} at {} (type: {}, flags: {:?}, options: {:?})", source, target.display(), fstype, flags, options);
    mount(Some(source), target, Some(fstype), flags, options)
        .map_err(|e| eyre::eyre!("Failed to mount {} at {}: {}", fstype, target.display(), e))
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

// Helper to check if a bind mount should be created (both paths exist and are directories)
fn should_bind_mount(host_path: &Path, guest_path: &Path) -> bool {
    if !lfs::exists_on_host(host_path) || !lfs::exists_or_any_symlink(guest_path) {
        return false;
    }
    match (fs::symlink_metadata(host_path), fs::symlink_metadata(guest_path)) {
        (Ok(host_meta), Ok(guest_meta)) => host_meta.is_dir() && guest_meta.is_dir(),
        _ => false,
    }
}

/// Generate mount specification strings for traditional layout host compatibility for usr-merge guest environments.
/// Returns spec strings for binding host's /bin, /sbin, /lib to guest's usr/bin, usr/sbin, usr/lib.
/// This must be called BEFORE mounting $env_root/usr over /usr.
pub(crate) fn mount_traditional_host_compatibility(env_root: &Path) -> Result<Vec<String>> {
    if !crate::run::host_uses_traditional_layout() {
        return Ok(Vec::new());
    }

    debug!("Host uses traditional layout, generating mount specs for host /bin, /sbin, /lib to environment usr directories");

    let mut specs = Vec::new();

    // Bind mount host's /bin to $env_root/usr/bin
    let guest_bin = env_root.join("usr/bin");
    if should_bind_mount(Path::new("/bin"), &guest_bin) {
        specs.push("/bin:@/usr/bin".to_string());
    }

    // Bind mount host's /sbin to $env_root/usr/sbin
    let guest_sbin = env_root.join("usr/sbin");
    if should_bind_mount(Path::new("/sbin"), &guest_sbin) {
        specs.push("/sbin:@/usr/sbin".to_string());
    }

    // Bind mount host's /lib to $env_root/usr/lib
    let guest_lib = env_root.join("usr/lib");
    if should_bind_mount(Path::new("/lib"), &guest_lib) {
        specs.push("/lib:@/usr/lib".to_string());
    }

    Ok(specs)
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
    let run_user_path = PathBuf::from(format!(
        "/run/user/{}/epkg-opt_real/{}-{}",
        euid_raw, uid_raw, env_name
    ));
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
                            trace!(
                                "Using fallback opt_real directory: {}",
                                home_opt_real.display()
                            );
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
                        e,
                        e2
                    ));
                }
            }
        }
    }
}

/// Generate mount specification strings for /opt/epkg mount isolation to preserve access to system /opt/epkg.
/// This ensures that when we mount the guest's /opt, we don't lose access to the host's /opt/epkg.
pub(crate) fn mount_opt_epkg_isolation(euid: Uid, uid: Uid, env_root: &Path) -> Result<Vec<String>> {
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
    debug!("mount_opt_epkg_isolation: env_root={}, opt_real_path={}, is_public_env={}",
           env_root.display(), opt_real_path.display(), env_root.starts_with("/opt/epkg"));

    // Ensure the opt_real directory exists (for private environments,
    // or as a safety check for public environments where it was already created)
    utils::safe_mkdir_p(&opt_real_path).map_err(|e| {
        eyre::eyre!(
            "Failed to create opt_real directory '{}': {}",
            opt_real_path.display(),
            e
        )
    })?;

    let opt_epkg_path = Path::new("/opt/epkg");
    // Store whether /opt/epkg existed BEFORE mounting env_root/opt over /opt
    // This is critical because after mounting, /opt/epkg will be hidden
    let opt_epkg_existed = lfs::exists_on_host(opt_epkg_path);
    trace!("mount_opt_epkg_isolation: /opt/epkg exists? {} (before mount)", opt_epkg_existed);

    let mut specs = Vec::new();

    if opt_epkg_existed {
        trace!(
            "Generating mount spec for {} -> {}",
            opt_epkg_path.display(),
            opt_real_path.display()
        );
        specs.push(format!("/opt/epkg:/{}", opt_real_path.display()));
    }

    // Mount environment /opt directory
    let src = env_root.join("opt");
    let _host_path = Path::new("/opt");
    if lfs::exists_in_env(&src) {
        trace!(
            "Generating mount spec for @/opt -> /opt"
        );
        specs.push("@/opt://opt".to_string());
    }

    // If /opt/epkg existed BEFORE mounting, bind mount it back
    // Use the stored value, not a new check, because /opt/epkg is now hidden
    if opt_epkg_existed {
        if lfs::exists_in_env(&opt_real_path) {
            trace!(
                "Generating mount spec for {} -> {}",
                opt_real_path.display(),
                opt_epkg_path.display()
            );
            specs.push(format!("{}://opt/epkg", opt_real_path.display()));
        }
    }
    trace!("mount_opt_epkg_isolation: generated {} mount specs: {:?}", specs.len(), specs);

    Ok(specs)
}

/// Returns true if the path is a mount point (different device or root of filesystem).
/// Used to satisfy pivot_root(2) requirement that new_root be a mount point.
fn path_is_mount_point(path: &Path) -> bool {
    let meta = match lfs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => return false,
    };
    if !meta.file_type().is_dir() {
        return false;
    }
    let dev = meta.dev();
    let ino = meta.ino();
    let parent = path.join("..");
    let parent_meta = match lfs::symlink_metadata(&parent) {
        Ok(m) => m,
        Err(_) => return false,
    };
    dev != parent_meta.dev() || (dev == parent_meta.dev() && ino == parent_meta.ino())
}

pub(crate) fn pivot_to_sandbox(new_root_base: &Path, oldroot: &Path) -> Result<()> {
    // pivot_root(2) requires new_root to be a mount point (EINVAL otherwise).
    // Bind-mount the path onto itself if it is not already a mount point.
    // Use MS_BIND|MS_REC|MS_SILENT like bwrap so submounts are included and kernel is silent.
    if !path_is_mount_point(new_root_base) {
        debug!(
            "pivot_to_sandbox: {} is not a mount point, bind-mounting onto itself (MS_BIND|MS_REC|MS_SILENT)",
            new_root_base.display()
        );
        let bind_flags = MsFlags::MS_BIND
            | MsFlags::MS_REC
            | MsFlags::from_bits_truncate(libc::MS_SILENT);
        mount(
            Some(new_root_base),
            new_root_base,
            Some(""),
            bind_flags,
            None::<&str>,
        )
        .map_err(|e| {
            eyre::eyre!(
                "pivot_root preparation failed: bind mount {} onto itself failed: {}",
                new_root_base.display(),
                e
            )
        })?;
    }

    debug!(
        "pivot_to_sandbox: new_root={}, oldroot={}",
        new_root_base.display(),
        oldroot.display()
    );

    pivot_root(new_root_base, oldroot).map_err(|e| {
        eyre::eyre!(
            "pivot_root failed: {} (new_root={}, put_old={}; new_root must be a mount point, put_old must be under new_root)",
            e,
            new_root_base.display(),
            oldroot.display()
        )
    })?;

    std::env::set_current_dir("/").map_err(|e| eyre::eyre!("chdir(/) failed: {}", e))?;

    // Set oldroot propagation to private (like bwrap's final MS_REC|MS_SILENT|MS_PRIVATE on oldroot)
    let flags = nix::mount::MsFlags::from_bits_truncate(libc::MS_REC | libc::MS_SILENT | libc::MS_PRIVATE);
    debug!("Attempting mount propagation change: MS_REC|MS_SILENT|MS_PRIVATE on /oldroot");
    match mount(Some("none"), "/oldroot", Some(""), flags, Some("")) {
        Ok(()) => debug!("Oldroot propagation set to private"),
        Err(e) if e == Errno::EPERM || e == Errno::EINVAL => {
            debug!("Failed to set oldroot private: {}. Continuing.", e);
        }
        Err(e) => warn!("Failed to set oldroot private: {}", e),
    }

    umount2("/oldroot", MntFlags::MNT_DETACH)
        .map_err(|e| eyre::eyre!("umount /oldroot failed: {}", e))?;

    Ok(())
}

/// Batch mount flexible mounts from MountSpec vector
pub(crate) fn mount_batch_specs(mount_specs: &[MountSpec], env_root: &Path, sandbox_mode: SandboxMode) -> Result<()> {
    debug!("mount_batch_specs: starting batch of {} mount specs", mount_specs.len());
    for (i, spec) in mount_specs.iter().enumerate() {
        trace!("mount_batch_specs: [{}/{}] mounting spec: source={:?}, target={:?}, fs_type={:?}, flags={:?}, try_only={}",
               i+1, mount_specs.len(), spec.source, spec.target, spec.fs_type, spec.flags, spec.try_only);
        mount_spec(spec, env_root, sandbox_mode)?;
    }
    debug!("mount_batch_specs: completed successfully");
    Ok(())
}

/// Collect mount specifications from strings and user options, then mount them.
#[allow(dead_code)]
pub(crate) fn collect_and_mount_specs(
    spec_strings: &[&str],
    env_root: &Path,
    run_options: &RunOptions,
    sandbox_mode: SandboxMode,
) -> Result<()> {
    let mut mounts = parse_mount_specs(spec_strings);
    for spec_str in &run_options.effective_sandbox.mount_specs {
        let spec = parse_mount_spec(spec_str)?;
        mounts.push(spec);
    }
    mount_batch_specs(&mounts, env_root, sandbox_mode)
}

fn parse_mount_flag_from_kv(k: &str, v: &str) -> (u64, bool) {
    let mut flags = 0u64;
    let mut try_only = false;
    let is_true = v.is_empty() || v == "true" || v == "1";
    if !is_true {
        return (flags, try_only);
    }
    // Map Docker-style aliases to mount option names
    let opt = match k {
        "readonly" => "ro",
        _ => k,
    };
    match opt {
        "try" => try_only = true,
        _ => {
            if let Ok(result) = parse_mount_options(Some(opt)) {
                let (mount_flags, _): (MsFlags, _) = result;
                flags |= mount_flags.bits();
            }
        }
    }
    (flags, try_only)
}

/// Parse a mount specification string into a `MountSpec`.
///
/// # Syntax
/// Mount specifications follow Docker-like colon-separated syntax:
///   `[<HOST_DIR|FS_TYPE>:]SANDBOX_DIR[:OPTIONS]`
///
/// - **HOST_DIR**: Absolute host path (may start with '@' for env_root substitution)
/// - **FS_TYPE**: Pseudo filesystem type (tmpfs, proc, devtmpfs, devpts, mqueue, etc.) or "remount"
/// - **SANDBOX_DIR**: Absolute path inside sandbox (may start with '@' for env_root substitution)
/// - **OPTIONS**: Comma-separated key=value pairs and flags (ro, try, recursive, silent, etc.)
///
/// If HOST_DIR/FS_TYPE is omitted: bind mount with source auto-generated.
/// If OPTIONS omitted: no extra options.
///
/// # Path Format Rules
/// - **Leading '/'** → absolute host path (e.g., `/usr` means the host's `/usr`)
/// - **Leading '@'** → substitute with environment root (e.g., `@/tmp` means `$env_root/tmp`)
/// Paths must be absolute (start with '/' or '@'). Relative paths are not allowed.
///
/// These rules apply to both `HOST_DIR` and `SANDBOX_DIR` fields.
///
/// # Source Auto‑generation
/// If `source` is omitted, it is generated based on the context:
/// - **Remount operation** (`MS_REMOUNT` flag) → `source = "none"`
/// - **tmpfs mount** (`FS_TYPE=tmpfs`) → `source = "tmpfs"`
/// - **proc/sysfs mounts** (`FS_TYPE=proc` or `sysfs`) → `source = <filesystem-type>`
/// - **Bind mounts** (no FS_TYPE specified):
///   - If HOST_DIR omitted → `source = SANDBOX_DIR` (with '@' prefix stripped)
///   - If HOST_DIR provided → `source = HOST_DIR`
///
/// This auto‑generation enables concise specifications like:
/// - `"/usr:/usr:ro,try"` → bind host `/usr` to sandbox `/usr` read‑only, skip if missing
/// - `"/dev/zero:recursive,silent"` → bind host `/dev/zero` to sandbox `/dev/zero`
/// - `"tmpfs:/tmp:mode=0755"` → mount tmpfs at sandbox `/tmp`
/// - `"proc:/proc"` → mount proc filesystem at sandbox `/proc`
/// - `"remount:/sys/kernel/security:ro"` → remount existing mount as read‑only
///
/// # Special Fields
/// - `type`: filesystem type (`bind`, `tmpfs`, `proc`, `sysfs`, etc.)
///   - `type=bind` or empty type with `MS_BIND` flag → bind mount
///   - `type=` (empty) is treated as `bind`
/// - `source`: source path (optional, auto‑generated as above)
/// - `uid_map` / `euid_map`: numeric UID mapping values
/// - Boolean flags: `ro`, `try`, `recursive`, `silent`, `relatime`, etc.
///
/// # Examples
/// ```
/// // Bind host /usr to sandbox /usr read‑only, skip if missing
/// parse_mount_spec("/usr:/usr:ro,try")
/// // Bind host /dev/zero to sandbox /dev/zero
/// parse_mount_spec("/dev/zero:recursive,silent")
/// // Mount tmpfs at sandbox /tmp with mode 0755
/// parse_mount_spec("tmpfs:/tmp:mode=0755")
/// // Mount proc filesystem at sandbox /proc
/// parse_mount_spec("proc:/proc")
/// // Remount existing mount as read‑only
/// parse_mount_spec("remount:/sys/kernel/security:ro")
/// ```

/// Check if a string is an absolute path or starts with '@' (env_root placeholder)
fn is_absolute_or_at(s: &str) -> bool {
    s.starts_with('/') || s.starts_with('@')
}

/// Parse colon-separated mount specification parts.
/// Returns (host_or_fs, sandbox_dir, options_str) where each is trimmed.
/// Validates syntax but not path semantics.
fn parse_mount_spec_parts(spec_str: &str) -> Result<(Option<&str>, &str, Option<&str>), eyre::Error> {
    // Docker-like syntax: [<HOST_DIR|FS_TYPE>:]SANDBOX_DIR[:OPTIONS]
    // Split by ':' into at most 3 parts
    let parts: Vec<&str> = spec_str.splitn(3, ':').collect();

    match parts.len() {
        1 => {
            // Only SANDBOX_DIR
            Ok((None, parts[0].trim(), None))
        }
        2 => {
            let first = parts[0].trim();
            let second = parts[1].trim();
            let second_is_path = is_absolute_or_at(second);
            let first_is_fs_type = is_pseudo_fs_type(first) || first == "remount";
            let first_is_path = is_absolute_or_at(first);

            if second_is_path {
                // HOST_DIR|FS_TYPE:SANDBOX_DIR
                Ok((Some(first), second, None))
            } else {
                // SANDBOX_DIR:OPTIONS
                // First must be a path (absolute or '@')
                if !first_is_path {
                    return Err(eyre::eyre!(
                        "Invalid mount specification '{}': expected absolute path or '@' prefix for SANDBOX_DIR",
                        spec_str
                    ));
                }
                // If first is a pseudo FS type, it's invalid (missing SANDBOX_DIR)
                if first_is_fs_type {
                    return Err(eyre::eyre!(
                        "Invalid mount specification '{}': pseudo FS type '{}' requires SANDBOX_DIR",
                        spec_str, first
                    ));
                }
                Ok((None, first, Some(second)))
            }
        }
        3 => {
            // HOST_DIR|FS_TYPE:SANDBOX_DIR:OPTIONS
            Ok((Some(parts[0].trim()), parts[1].trim(), Some(parts[2].trim())))
        }
        _ => {
            Err(eyre::eyre!("Invalid mount specification syntax: {}", spec_str))
        }
    }
}

/// Validate sandbox_dir is not empty and is absolute path (starts with '/' or '@')
fn validate_sandbox_dir(sandbox_dir: &str) -> Result<(), eyre::Error> {
    if sandbox_dir.is_empty() {
        return Err(eyre::eyre!("Empty SANDBOX_DIR in mount specification"));
    }
    if !is_absolute_or_at(sandbox_dir) {
        return Err(eyre::eyre!("SANDBOX_DIR must be absolute path (start with '/' or '@'): {}", sandbox_dir));
    }
    Ok(())
}

/// Parse "make-*" propagation strings and return corresponding mount flags.
/// Returns None if the string doesn't start with "make-" or has unknown suffix.
fn propagation_flags_from_make(make_str: &str) -> Option<u64> {
    use nix::mount::MsFlags;
    let suffix = make_str.strip_prefix("make-")?;
    let bits = match suffix {
        "shared" => MsFlags::MS_SHARED.bits(),
        "slave" => MsFlags::MS_SLAVE.bits(),
        "private" => MsFlags::MS_PRIVATE.bits(),
        "unbindable" => MsFlags::MS_UNBINDABLE.bits(),
        "rshared" => (MsFlags::MS_REC | MsFlags::MS_SHARED).bits(),
        "rslave" => (MsFlags::MS_REC | MsFlags::MS_SLAVE).bits(),
        "rprivate" => (MsFlags::MS_REC | MsFlags::MS_PRIVATE).bits(),
        "runbindable" => (MsFlags::MS_REC | MsFlags::MS_UNBINDABLE).bits(),
        _ => return None,
    };
    Some(bits)
}

/// Determine filesystem type, source, type_specified, and initial flags based on host_or_fs.
/// Returns (fs_type, source, type_specified, flags).
fn determine_fs_type_and_source(host_or_fs: Option<&str>, sandbox_dir: &str) -> Result<(String, String, bool, u64), eyre::Error> {
    use nix::mount::MsFlags;

    let fs_type;
    let mut source;
    let mut type_specified = false;
    let mut flags = 0u64;

    if let Some(host_or_fs) = host_or_fs {
        if host_or_fs == "remount" {
            // Special case: remount operation
            fs_type = String::new();
            type_specified = true;
            flags |= MsFlags::MS_REMOUNT.bits();
            source = "none".to_string();
        } else if let Some(make_flags) = propagation_flags_from_make(host_or_fs) {
            // Special case: propagation-only mount with make-* syntax
            fs_type = String::new();
            type_specified = true;
            flags |= make_flags;
            source = "none".to_string();
        } else if host_or_fs == "none" {
            // Special case: propagation-only mount (e.g., mount --make-slave)
            fs_type = String::new();
            type_specified = true;
            source = "none".to_string();
        } else if is_pseudo_fs_type(host_or_fs) {
            // It's a filesystem type
            fs_type = host_or_fs.to_string();
            type_specified = true;
            source = host_or_fs.to_string();
            // For tmpfs, source should be "tmpfs"
            if fs_type == "tmpfs" {
                source = "tmpfs".to_string();
            }
        } else {
            // It's a host directory path
            if !is_absolute_or_at(host_or_fs) {
                return Err(eyre::eyre!("HOST_DIR must be absolute path: {}", host_or_fs));
            }
            // Bind mount
            fs_type = String::new();
            source = host_or_fs.to_string();
            // MS_BIND flag will be set later if fs_type is empty
        }
    } else {
        // No HOST_DIR|FS_TYPE specified: bind mount with auto-generated source
        fs_type = String::new();
        // Auto-generate source: host_dir = sandbox_dir (without '@' prefix)
        source = strip_at_prefix(sandbox_dir).to_string();
    }

    Ok((fs_type, source, type_specified, flags))
}

/// Parse mount options string into flags, try_only, and additional options.
/// Returns (flags, try_only, options_vec).
fn parse_mount_spec_options(options_str: Option<&str>) -> (u64, bool, Vec<String>) {
    let mut flags = 0u64;
    let mut try_only = false;
    let mut options = Vec::new();

    if let Some(opts) = options_str {
        for kv in opts.split(',') {
            if kv.is_empty() {
                continue;
            }
            if let Some((k, v)) = kv.split_once('=') {
                let k = k.trim();
                let v = v.trim();
                let (flag_bits, try_flag) = parse_mount_flag_from_kv(k, v);
                flags |= flag_bits;
                if try_flag {
                    try_only = true;
                }
                // If not a flag, add to options
                if flag_bits == 0 && !try_flag {
                    options.push(format!("{}={}", k, v));
                }
            } else {
                // Boolean flag without value
                let (flag_bits, try_flag) = parse_mount_flag_from_kv(kv, "");
                flags |= flag_bits;
                if try_flag {
                    try_only = true;
                }
                // If not a flag, add to options
                if flag_bits == 0 && !try_flag {
                    options.push(kv.to_string());
                }
            }
        }
    }
    (flags, try_only, options)
}

/// Prefix SANDBOX_DIR with '@' to make it a tartget path inside sandbox.
fn normalize_target_path(sandbox_dir: &str) -> String {
    // Use "//absolute/path" to indicate host path -- typically only needed by some special code
    // like mount_opt_epkg_isolation() or make-rprivate.
    if sandbox_dir.starts_with('@') || sandbox_dir.starts_with("//") {
        sandbox_dir.to_string()
    } else {
        // Absolute path inside sandbox: prepend '@' to indicate env_root substitution
        format!("@{}", sandbox_dir)
    }
}

pub fn parse_mount_spec(spec_str: &str) -> Result<MountSpec, eyre::Error> {
    use nix::mount::MsFlags;

    // Auto-detect JSON syntax
    if spec_str.trim_start().starts_with('{') {
        // JSON format - parse directly into MountSpec
        let spec: MountSpec = serde_json::from_str(spec_str)
            .map_err(|e| eyre::eyre!("Failed to parse mount JSON '{}': {}", spec_str, e))?;
        return Ok(spec);
    }

    let (host_or_fs, sandbox_dir, options_str) = parse_mount_spec_parts(spec_str)?;
    validate_sandbox_dir(sandbox_dir)?;

    // Determine filesystem type, source, and initial flags
    let (fs_type, source, type_specified, mut flags) = determine_fs_type_and_source(host_or_fs, sandbox_dir)?;

    // Parse options string into flags and options
    let (option_flags, try_only, options) = parse_mount_spec_options(options_str);
    flags |= option_flags;

    // If type was not specified and fs_type is empty, assume bind mount
    if !type_specified && fs_type.is_empty() {
        flags |= MsFlags::MS_BIND.bits();
    }

    // Prefix SANDBOX_DIR with '@' to create target path inside sandbox
    let target = normalize_target_path(sandbox_dir);

    // Build options string if any
    let options_str = if options.is_empty() {
        None
    } else {
        Some(options.join(","))
    };

    Ok(MountSpec {
        source,
        target,
        fs_type,
        try_only,
        flags,
        options: options_str,
        uid_map: None,
        euid_map: None,
    })
}

/// Helper to parse array of mount specification strings
/// Panics if any spec fails to parse (static constants should be valid)
pub(crate) fn parse_mount_specs(spec_strings: &[&str]) -> Vec<MountSpec> {
    spec_strings.iter()
        .map(|s| parse_mount_spec(s).expect(&format!("Failed to parse mount spec '{}'", s)))
        .collect()
}
