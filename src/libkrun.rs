use std::ffi::CString;
#[cfg(not(target_os = "linux"))]
use std::io::Write;
use std::path::Path;
use std::ptr;
#[cfg(not(target_os = "linux"))]
use std::sync::Mutex;
use std::thread;

use color_eyre::eyre;
use color_eyre::Result;

use crate::lfs;
use crate::run::RunOptions;

// Embed init.krun binary for Windows VM environments
// This is the Linux init binary from libkrun that will be written to the environment root
#[cfg(all(feature = "libkrun", not(target_os = "linux")))]
static INIT_KRUN_BYTES: &[u8] = include_bytes!("../git/libkrun/init/init");

#[cfg(feature = "libkrun")]
#[path = "libkrun_bridge.rs"]
mod libkrun_bridge;
#[cfg(feature = "libkrun")]
#[path = "libkrun_stream.rs"]
mod libkrun_stream;

#[cfg(feature = "libkrun")]
extern crate krun as krun_crate;

// FFI for statically linked libkrun (C API from libkrun crate built as staticlib).
#[cfg(feature = "libkrun")]
#[allow(dead_code)]
unsafe extern "C" {
    fn krun_create_ctx() -> i32;
    fn krun_free_ctx(ctx_id: u32) -> i32;
    fn krun_init_log(target_fd: i32, level: u32, style: u32, options: u32) -> i32;
    fn krun_set_vm_config(ctx_id: u32, num_vcpus: u8, ram_mib: u32) -> i32;
    fn krun_set_root(ctx_id: u32, root_path: *const std::ffi::c_char) -> i32;
    fn krun_set_kernel(
        ctx_id: u32,
        c_kernel_path: *const std::ffi::c_char,
        kernel_format: u32,
        c_initramfs_path: *const std::ffi::c_char,
        c_cmdline: *const std::ffi::c_char,
    ) -> i32;
    fn krun_set_firmware(ctx_id: u32, c_firmware_path: *const std::ffi::c_char) -> i32;
    fn krun_split_irqchip(ctx_id: u32, enable: bool) -> i32;
    fn krun_start_enter(ctx_id: u32) -> i32;
    fn krun_disable_implicit_vsock(ctx_id: u32) -> i32;
    fn krun_disable_implicit_console(ctx_id: u32) -> i32;
    fn krun_add_vsock(ctx_id: u32, tsi_features: u32) -> i32;
    unsafe fn krun_add_virtio_console_default(
        ctx_id: u32,
        input_fd: libc::c_int,
        output_fd: libc::c_int,
        err_fd: libc::c_int,
    ) -> i32;
    /// Set a file path to redirect the console output to.
    /// Must be called before krun_start_enter.
    fn krun_set_console_output(ctx_id: u32, filepath: *const std::ffi::c_char) -> i32;
    /// Set the kernel console device (e.g., "ttyS0" or "hvc0").
    /// Must be called before krun_start_enter.
    fn krun_set_kernel_console(ctx_id: u32, console_id: *const std::ffi::c_char) -> i32;
    /// Mount an additional directory via virtiofs into the guest.
    /// tag: the filesystem tag (e.g., "self")
    /// path: the host directory path to mount
    unsafe fn krun_add_virtiofs(
        ctx_id: u32,
        c_tag: *const std::ffi::c_char,
        c_path: *const std::ffi::c_char,
    ) -> i32;
    /// Get the eventfd for triggering VM shutdown from host.
    /// Writing 1u64 to this fd will cause the VM to exit gracefully.
    fn krun_get_shutdown_eventfd(ctx_id: u32) -> i32;
}

// Map vsock port to a Unix socket path (host). Not exported on Windows builds of libkrun.
#[cfg(all(feature = "libkrun", unix))]
#[allow(dead_code)]
unsafe extern "C" {
    fn krun_add_vsock_port2(
        ctx_id: u32,
        port: u32,
        c_filepath: *const std::ffi::c_char,
        listen: bool,
    ) -> i32;
}

// Map vsock port to `\\.\pipe\<stem>` on the host (Windows libkrun only).
#[cfg(all(feature = "libkrun", windows))]
#[allow(dead_code)]
unsafe extern "C" {
    fn krun_add_vsock_port_windows(
        ctx_id: u32,
        port: u32,
        c_pipe_name: *const std::ffi::c_char,
    ) -> i32;
}

// Map vsock port to `\\.\pipe\<stem>` with listen support (Windows libkrun only).
#[cfg(all(feature = "libkrun", windows))]
#[allow(dead_code)]
unsafe extern "C" {
    fn krun_add_vsock_port2_windows(
        ctx_id: u32,
        port: u32,
        c_pipe_name: *const std::ffi::c_char,
        listen: bool,
    ) -> i32;
}

// Force the staticlib to be linked when we only reference it via extern "C".
#[cfg(feature = "libkrun")]
fn ensure_libkrun_linked() {
    krun_crate::ensure_linked();
}

/// Check if kernel is ELF format by reading magic bytes.
#[cfg(feature = "libkrun")]
fn is_elf_kernel(kernel_path: &str) -> Result<bool> {
    use std::fs::File;
    use std::io::Read;

    let mut file = File::open(kernel_path)
        .map_err(|e| eyre::eyre!("Failed to open kernel {}: {}", kernel_path, e))?;
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic)
        .map_err(|e| eyre::eyre!("Failed to read kernel magic: {}", e))?;

    // ELF magic: 0x7f 'E' 'L' 'F'
    Ok(magic == [0x7f, b'E', b'L', b'F'])
}

/// Detect kernel format for libkrun's krun_set_kernel().
/// Returns: 0=Raw (Image), 1=ELF (vmlinux)
#[cfg(feature = "libkrun")]
#[allow(dead_code)]
fn detect_kernel_format_for_libkrun(kernel_path: &str) -> Result<u32> {
    if is_elf_kernel(kernel_path)? {
        Ok(1) // ELF (vmlinux)
    } else {
        // Assume Raw format (e.g., aarch64 Image)
        log::debug!("libkrun: kernel {} is not ELF, assuming Raw format", kernel_path);
        Ok(0) // Raw (Image)
    }
}

#[cfg(feature = "libkrun")]
fn check_status(op: &str, status: i32) -> Result<()> {
    if status < 0 {
        Err(eyre::eyre!("{} failed with status {}", op, status))
    } else {
        Ok(())
    }
}

#[cfg(feature = "libkrun")]
struct LibkrunConfig {
    use_vsock:           bool,
    cmd_parts:           Vec<String>,
    kernel_args:         String,
    kernel_path:         Option<String>,
    kernel_format:       Option<u32>,
    /// Additional virtiofs mounts: (tag, host_path, guest_path, read_only)
    virtiofs_mounts:     Vec<(String, String, String, bool)>,
}

#[cfg(all(feature = "libkrun", not(target_os = "linux")))]
/// Ensure init.krun is written to the environment root for VM boot.
/// This embeds the Linux init binary into epkg and writes it to the target environment.
fn ensure_init_krun(env_root: &Path) -> Result<()> {
    let init_krun_path = env_root.join("init.krun");

    // Always overwrite to ensure we have the correct version
    log::debug!("Writing embedded init.krun to {}", init_krun_path.display());
    lfs::write(&init_krun_path, INIT_KRUN_BYTES)
        .map_err(|e| eyre::eyre!("Failed to write init.krun to {}: {}", init_krun_path.display(), e))?;

    // Set execute permission using NTFS EA on Windows
    const S_IFREG: u32 = 0o100000;
    const MODE_755: u32 = S_IFREG | 0o755;
    if let Err(e) = crate::ntfs_ea::set_posix_mode(&init_krun_path, MODE_755, false) {
        log::warn!("Failed to set execute permission on init.krun: {}", e);
    } else {
        log::debug!("Set execute permission (100755) on init.krun");
    }

    log::info!("init.krun ({:.1} KB) written to {}",
        INIT_KRUN_BYTES.len() as f64 / 1024.0,
        init_krun_path.display());
    Ok(())
}

#[cfg(feature = "libkrun")]
fn build_libkrun_config(
    env_root: &Path,
    run_options: &RunOptions,
    guest_cmd_path: &Path,
) -> Result<LibkrunConfig> {
    // Ensure init.krun is present for VM boot (embedded binary)
    #[cfg(not(target_os = "linux"))]
    ensure_init_krun(env_root)?;

    let use_cmdline_mode = std::env::var("EPKG_VM_NO_DAEMON").is_ok();
    let use_vsock = !use_cmdline_mode;
    log::debug!("libkrun: mode: cmdline={}, vsock={}", use_cmdline_mode, use_vsock);
    log::debug!("libkrun: EPKG_VM_NO_DAEMON={}", std::env::var("EPKG_VM_NO_DAEMON").unwrap_or_else(|_| "not set".to_string()));

    let guest_exec_path = guest_cmd_path
        .strip_prefix(env_root)
        .map(|rel| {
            let rel_str = rel.to_string_lossy().to_string();
            if rel_str.starts_with('/') {
                rel_str
            } else {
                format!("/{}", rel_str)
            }
        })
        .unwrap_or_else(|_| guest_cmd_path.to_string_lossy().to_string());

    let (cmd_parts, init_cmd) = build_guest_command(Path::new(&guest_exec_path), &run_options.args)
        .map_err(|e| eyre::eyre!("Failed to build guest command: {}", e))?;

    // root=/dev/root: specifies the virtiofs tag for root filesystem
    // (krun_set_root sets up a virtiofs device with tag "/dev/root")
    // On Windows, use ttyS0 for serial console since libkrun auto-adds COM1 device
    #[cfg(target_os = "windows")]
    let base_cmdline = "reboot=k panic=-1 panic_print=0 nomodule console=ttyS0 earlyprintk=serial \
                        loglevel=8 debug root=/dev/root rootfstype=virtiofs rw no-kvmapf init=/init.krun";
    #[cfg(not(target_os = "windows"))]
    let base_cmdline = "reboot=k panic=-1 panic_print=0 nomodule console=hvc0 earlyprintk=hvc0 \
                        loglevel=8 debug root=/dev/root rootfstype=virtiofs rw no-kvmapf init=/usr/bin/init";
    let mut kernel_args = String::from(base_cmdline);
    if let Some(ref user_args) = run_options.kernel_args {
        kernel_args.push(' ');
        kernel_args.push_str(user_args);
    };

    if use_cmdline_mode {
        kernel_args.push(' ');
        kernel_args.push_str(&format!("epkg.init_cmd={}", init_cmd));
    }

    if let Ok(rust_log) = std::env::var("RUST_LOG") {
        if !rust_log.is_empty() {
            kernel_args.push_str(&format!(" epkg.rust_log={}", percent_encode(&rust_log)));
        }
    }

    // Pass TSI disablement status to guest init only when explicitly disabled.
    // Default is TSI enabled, so we only pass the parameter when disabling.
    if std::env::var("EPKG_TSI_DISABLE").is_ok() {
        kernel_args.push_str(" epkg.tsi=0");
        log::debug!("libkrun: TSI disabled via EPKG_TSI_DISABLE env var");
    }

    // Set init_pwd to current working directory.
    // On Windows, skip this since PWD is a Windows path which is invalid in the Linux guest.
    #[cfg(not(windows))]
    if let Ok(pwd) = std::env::var("PWD") {
        if !pwd.is_empty() && pwd != "/" {
            kernel_args.push_str(&format!(" epkg.init_pwd={}", percent_encode(&pwd)));
        }
    }

    // Add virtiofs mount specs for guest init to mount
    // Format: epkg.vol_N=tag:guest_path[:ro]
    let mount_specs = build_virtiofs_mount_specs(env_root, run_options);
    for (i, (tag, _host_path, guest_path, read_only)) in mount_specs.iter().enumerate() {
        let spec = if *read_only {
            format!("{}:{}:ro", tag, guest_path)
        } else {
            format!("{}:{}", tag, guest_path)
        };
        kernel_args.push_str(&format!(" epkg.vol_{}={}", i, percent_encode(&spec)));
    }

    let kernel_path = if run_options.kernel.is_some() {
        run_options.kernel.clone()
    } else {
        // Fall back to default kernel path (envs/self/boot/kernel from `epkg self install`)
        crate::init::default_kernel_path_if_exists()
    };
    let kernel_format = if let Some(ref kernel) = kernel_path {
        Some(detect_kernel_format_for_libkrun(kernel)?)
    } else {
        None
    };
    if let Some(ref kernel) = kernel_path {
        log::debug!("libkrun: kernel path: {}", kernel);
        log::debug!("libkrun: kernel format: {:?}", kernel_format);
    } else {
        log::debug!("libkrun: no kernel available (no --kernel specified and no default kernel found)");
    }

    Ok(LibkrunConfig {
        use_vsock,
        cmd_parts,
        kernel_args,
        kernel_path,
        kernel_format,
        virtiofs_mounts: build_virtiofs_mount_specs(env_root, run_options),
    })
}

/// Build virtiofs mount specs from mount specs.
/// Returns Vec<(tag, host_path, guest_path, read_only)> for each directory mount.
/// Skips non-directory sources since virtiofs only supports directory bind mounts.
#[cfg(feature = "libkrun")]
fn build_virtiofs_mount_specs(env_root: &Path, run_options: &RunOptions) -> Vec<(String, String, String, bool)> {
    use std::fs;
    use crate::models::dirs;

    let mut mounts = Vec::new();
    let mut seen_paths: Vec<std::path::PathBuf> = Vec::new();

    // Helper to add a mount if path is a directory and not already seen
    // guest_path defaults to host_path if not specified
    let mut try_add_mount = |host_path: &Path, guest_path: Option<&Path>, read_only: bool, try_only: bool| {
        // Skip if already added, or if a parent mount already covers this path.
        if seen_paths.iter().any(|seen| host_path == seen || host_path.starts_with(seen)) {
            return;
        }
        let path_str = host_path.to_string_lossy().to_string();

        // Skip if not a directory (virtiofs only supports directories)
        match fs::metadata(host_path) {
            Ok(meta) if meta.is_dir() => {
                // Generate a unique tag from the path
                let tag = generate_virtiofs_tag(host_path);
                let guest = guest_path
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| path_str.clone());
                log::debug!("libkrun: adding virtiofs mount: {} -> {} (guest: {}) ({})",
                           host_path.display(), tag, guest, if read_only { "ro" } else { "rw" });
                mounts.push((tag, host_path.to_string_lossy().to_string(), guest, read_only));
                seen_paths.push(host_path.to_path_buf());
            }
            Ok(_) => {
                log::info!("libkrun: skipping non-directory mount: {}", host_path.display());
            }
            Err(e) if !try_only => {
                log::warn!("libkrun: cannot access mount path {}: {}", host_path.display(), e);
            }
            Err(_) => {
                // try_only=true, silently skip
            }
        }
    };

    // Add epkg system directories
    //
    // We need to consider both host user (who owns the files being mounted)
    // and guest user (who will run in the VM, via -u option).
    //
    // Combinations to handle:
    // 1. Host root + Guest root: Mount to same paths, all writable
    // 2. Host root + Guest non-root: Same paths, but guest may not write to root-owned files
    // 3. Host non-root + Guest root: Mount to /opt/epkg, root can write anywhere
    // 4. Host non-root + Guest same UID: Works if UIDs match
    // 5. Host non-root + Guest different UID: Guest can't write to host-owned dirs
    //
    // For cases 2, 4, 5 where there's a UID mismatch, we rely on the guest init
    // to set up a writable temp location (e.g., /tmp) for operations that need it.
    // The key is to ensure downloads and cache writes work for the typical case
    // (host non-root, guest root) which is the default VM behavior.
    #[cfg(unix)]
    let is_host_root = run_options.host_uid.map_or(false, |uid| uid == 0);
    #[cfg(not(unix))]
    let is_host_root = false;

    // Check if guest will run as root (no -u or -u root)
    let is_guest_root = run_options.user.as_ref().map_or(true, |u| u == "root" || u == "0");

    if is_host_root {
        // For host root: mount to same path in guest (host path = guest path)
        try_add_mount(&dirs().home_epkg, None, false, true);
        try_add_mount(&dirs().home_cache, None, false, true);
        try_add_mount(&dirs().opt_epkg, None, false, true);
    } else if is_guest_root {
        // For non-root host + root guest: mount user dirs to system paths
        // Root in guest can write anywhere, so this works well
        try_add_mount(&dirs().home_epkg, Some(Path::new("/opt/epkg")), false, true);
        try_add_mount(&dirs().home_cache, Some(Path::new("/opt/epkg/cache")), false, true);
        // Don't mount host /opt/epkg - it's not writable by non-root host user
    } else {
        // For non-root host + non-root guest: mount to same paths
        // The guest user will have the same UID as the host user (via virtiofs
        // passthrough), so they can access their own files
        try_add_mount(&dirs().home_epkg, None, false, true);
        try_add_mount(&dirs().home_cache, None, false, true);
        // Don't mount host /opt/epkg - not writable by non-root user
    }

    // Add user-provided mount specs from run_options
    for mount_spec_str in &run_options.effective_sandbox.mount_specs {
        if let Some((host_path, guest_path, read_only, try_only)) = parse_mount_spec_for_virtiofs(mount_spec_str, env_root) {
            try_add_mount(&host_path, Some(&guest_path), read_only, try_only);
        }
    }

    // Add epkg binary directory if outside env.
    // This runs after user mounts so a broader user mount (e.g. /c/epkg)
    // can cover the binary directory and avoid consuming an extra virtiofs device.
    if let Ok(epkg_exe) = std::env::current_exe() {
        if let Some(epkg_bin_dir) = epkg_exe.parent() {
            if !epkg_bin_dir.starts_with(&dirs().home_epkg)
               && !epkg_bin_dir.starts_with(&dirs().opt_epkg) {
                try_add_mount(epkg_bin_dir, None, true, false);
            }
        }
    }

    // Add /lib/modules if exists (for kernel module loading)
    try_add_mount(Path::new("/lib/modules"), None, true, true);

    mounts
}

/// Parse a mount spec string for virtiofs.
/// Returns Some((host_path, guest_path, read_only, try_only)) if it's a valid directory mount spec.
/// Returns None for non-bind mounts or invalid specs.
#[cfg(feature = "libkrun")]
fn parse_mount_spec_for_virtiofs(spec_str: &str, env_root: &Path) -> Option<(std::path::PathBuf, std::path::PathBuf, bool, bool)> {
    // Parse mount spec: [SOURCE:]TARGET[:OPTIONS]
    // For VM: SOURCE is host_path, TARGET is guest_path
    let parts: Vec<&str> = spec_str.split(':').collect();

    // Check for special filesystem types that virtiofs cannot handle (e.g., "tmpfs:/path")
    // These must be mounted inside the VM by the guest init, not shared from host via virtiofs
    #[cfg(target_os = "linux")]
    if parts.len() >= 2 {
        let source = parts[0];
        if crate::mount::PSEUDO_FS_TYPES.contains(&source) {
            return None;
        }
    }

    let (source, target, options) = if parts.len() == 1 {
        // Just a path, use same path for both host and guest
        (parts[0], parts[0], "")
    } else if parts.len() == 2 {
        // Could be "SOURCE:TARGET" or "PATH:OPTIONS"
        // Check if second part looks like options (contains commas or known option names)
        if parts[1].contains(',') || parts[1].starts_with("ro") || parts[1].starts_with("rw") {
            // PATH:OPTIONS format - same path for host and guest
            (parts[0], parts[0], parts[1])
        } else {
            // SOURCE:TARGET format
            (parts[0], parts[1], "")
        }
    } else if parts.len() >= 3 {
        // SOURCE:TARGET:OPTIONS
        (parts[0], parts[1], parts[2])
    } else {
        return None;
    };

    // Handle @ prefix for env_root substitution (only for source)
    let host_path = if source.starts_with('@') {
        lfs::normalize_path_separators(&env_root.join(&source[1..]))
    } else {
        std::path::PathBuf::from(source)
    };

    // Guest path is the target (may also have @ prefix)
    let guest_path = if target.starts_with('@') {
        lfs::normalize_path_separators(&env_root.join(&target[1..]))
    } else {
        std::path::PathBuf::from(target)
    };

    // Parse options for read_only and try flags
    let read_only = options.contains("ro");
    let try_only = options.contains("try");

    Some((host_path, guest_path, read_only, try_only))
}

/// Generate a unique virtiofs tag from a path.
/// The tag is used as the mount point identifier in the guest.
#[cfg(feature = "libkrun")]
fn generate_virtiofs_tag(path: &Path) -> String {
    // Use the last component of the path as the tag, or the full path if root
    let tag = path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("root");

    // Make tag unique by using a simple hash of the full path
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    let hash = hasher.finish();

    format!("{}_{}", tag, hash % 10000)
}

#[cfg(feature = "libkrun")]
struct VmContext {
    ctx: KrunContext,
    shutdown_fd: i32,
    vsock_sock_path: Option<std::path::PathBuf>,
}

/// Configure vsock ports 10000 (command) and 10001 (ready); returns host path for the command socket.
#[cfg(feature = "libkrun")]
fn setup_libkrun_vsock_host_sockets(ctx: &KrunContext) -> Result<std::path::PathBuf> {
    let sock_path = crate::models::dirs().epkg_cache
        .join("vmm-logs")
        .join(format!("vsock-{}.sock", std::process::id()));
    lfs::create_dir_all(sock_path.parent().unwrap())?;
    let _ = std::fs::remove_file(&sock_path);

    let ready_path = crate::models::dirs().epkg_cache
        .join("vmm-logs")
        .join(format!("ready-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&ready_path);

    unsafe {
        check_status("krun_disable_implicit_vsock", krun_disable_implicit_vsock(ctx.ctx_id))?;
        // Enable TSI (Transparent Socket Impersonation) for network access.
        // KRUN_TSI_HIJACK_INET (1 << 0) allows the guest to use host network via socket hijacking.
        // KRUN_TSI_HIJACK_UNIX (1 << 1) enables Unix socket hijacking (required for full TSI support).
        const KRUN_TSI_HIJACK_INET: u32 = 1 << 0;
        const KRUN_TSI_HIJACK_UNIX: u32 = 1 << 1;
        let tsi_features = KRUN_TSI_HIJACK_INET | KRUN_TSI_HIJACK_UNIX;
        check_status("krun_add_vsock", krun_add_vsock(ctx.ctx_id, tsi_features))?;

        #[cfg(unix)]
        {
            let sock_path_c = CString::new(sock_path.to_string_lossy().as_bytes())
                .map_err(|e| eyre::eyre!("invalid socket path: {}", e))?;
            check_status(
                "krun_add_vsock_port2",
                krun_add_vsock_port2(ctx.ctx_id, 10000, sock_path_c.as_ptr(), true),
            )?;
        }
        #[cfg(windows)]
        {
            let stem = libkrun_bridge::pipe_name_from_sock_path(&sock_path)?;
            let stem_c = CString::new(stem).map_err(|e| eyre::eyre!("invalid vsock pipe name: {}", e))?;
            check_status(
                "krun_add_vsock_port2_windows",
                krun_add_vsock_port2_windows(ctx.ctx_id, 10000, stem_c.as_ptr(), true),
            )?;
        }
        #[cfg(unix)]
        {
            let ready_path_c = CString::new(ready_path.to_string_lossy().as_bytes())
                .map_err(|e| eyre::eyre!("invalid ready socket path: {}", e))?;
            check_status(
                "krun_add_vsock_port2",
                krun_add_vsock_port2(ctx.ctx_id, 10001, ready_path_c.as_ptr(), false),
            )?;
        }
        #[cfg(windows)]
        {
            let stem = libkrun_bridge::pipe_name_from_sock_path(&ready_path)?;
            let stem_c = CString::new(stem).map_err(|e| eyre::eyre!("invalid ready pipe name: {}", e))?;
            check_status(
                "krun_add_vsock_port2_windows",
                krun_add_vsock_port2_windows(ctx.ctx_id, 10001, stem_c.as_ptr(), false),
            )?;
        }
    }

    log::debug!("libkrun: vsock port 10000 mapped to {}", sock_path.display());
    log::debug!("libkrun: ready port 10001 mapped to {}", ready_path.display());

    Ok(sock_path)
}

#[cfg(feature = "libkrun")]
fn create_and_configure_vm(
    env_root: &Path,
    run_options: &RunOptions,
    config: &LibkrunConfig,
) -> Result<VmContext> {
    ensure_libkrun_linked();

    let ctx = unsafe { KrunContext::create()? };
    let cpus = crate::run::resolve_vm_cpus(run_options);
    let requested_mib = crate::run::resolve_vm_memory_mib(run_options);
    log::debug!("libkrun: run_options.vm_memory_mib = {:?}", run_options.vm_memory_mib);
    let memory_mib = if let Some(ref kernel) = config.kernel_path {
        crate::run::round_up_vm_memory_for_libkrun(requested_mib, kernel)
    } else {
        requested_mib
    };
    log::debug!("libkrun: requested_mib = {}", requested_mib);
    log::debug!("libkrun: round_up_vm_memory_for_libkrun = {}", memory_mib);
    log::debug!("libkrun: kernel cmdline: {}", config.kernel_args);
    unsafe {
        ctx.set_vm_config(cpus, memory_mib)?;

        if let Some(ref kernel) = config.kernel_path {
            if let Some(format) = config.kernel_format {
                let format_str = match format {
                    0 => "Raw",
                    1 => "ELF (vmlinux)",
                    _ => "Unknown",
                };
                if let Some(ref initrd) = run_options.initrd {
                    log::debug!("libkrun: using initrd: {}", initrd);
                } else {
                    log::debug!("libkrun: no initrd provided");
                }
                ctx.set_kernel(kernel, format, Some(&config.kernel_args), run_options.initrd.as_deref())?;
                log::debug!("libkrun: kernel set via krun_set_kernel() with format={} ({})", format, format_str);
            }
        }

        ctx.set_root(env_root.to_str().unwrap())?;
        log::debug!("libkrun: rootfs configured via virtiofs: {:?}", env_root);

        // Add additional virtiofs mounts
        for (tag, host_path, guest_path, read_only) in &config.virtiofs_mounts {
            ctx.add_virtiofs(tag, host_path)?;
            log::debug!("libkrun: added virtiofs mount: {} -> {} (guest: {}) ({})",
                       host_path, tag, guest_path, if *read_only { "ro" } else { "rw" });
        }

        // split_irqchip is only supported on x86_64; skip on aarch64
        #[cfg(target_arch = "x86_64")]
        {
            check_status("krun_split_irqchip",
                krun_split_irqchip(ctx.ctx_id, true)
            )?;
            log::debug!("libkrun: split IRQ chip configured");
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            log::debug!("libkrun: skipping split IRQ chip (x86_64 only)");
        }

        setup_console_output(ctx.ctx_id)?;

        if config.use_vsock {
            let sock_path = setup_libkrun_vsock_host_sockets(&ctx)?;
            let vsock_sock_path = Some(sock_path);
            let shutdown_fd = ctx.get_shutdown_eventfd()
                .map_err(|e| eyre::eyre!("Failed to get shutdown eventfd: {}", e))?;
            log::debug!("libkrun: shutdown_eventfd = {}", shutdown_fd);
            return Ok(VmContext { ctx, shutdown_fd, vsock_sock_path });
        }
    }

    let shutdown_fd = unsafe { ctx.get_shutdown_eventfd() }
        .map_err(|e| eyre::eyre!("Failed to get shutdown eventfd: {}", e))?;
    log::debug!("libkrun: shutdown_eventfd = {}", shutdown_fd);

    Ok(VmContext { ctx, shutdown_fd, vsock_sock_path: None })
}

#[cfg(feature = "libkrun")]
fn start_libkrun_vm(ctx: KrunContext, start_failed_tx: std::sync::mpsc::Sender<()>) -> std::thread::JoinHandle<i32> {
    thread::spawn(move || {
        unsafe {
            let status = ctx.start_enter();
            if status < 0 {
                log::error!("krun_start_enter failed with status {}", status);
                // Signal failure to main thread so it doesn't wait for timeout
                let _ = start_failed_tx.send(());
            } else {
                log::debug!("libkrun: krun_start_enter returned status {}", status);
            }
            status
        }
    })
}

/// Thin wrapper that owns a libkrun context.
#[cfg(feature = "libkrun")]
struct KrunContext {
    ctx_id: u32,
}

#[cfg(feature = "libkrun")]
unsafe impl Send for KrunContext {}

#[cfg(feature = "libkrun")]
impl KrunContext {

    /// Create a new libkrun context.
    unsafe fn create() -> Result<Self> {
        let ctx = unsafe { krun_create_ctx() };
        if ctx < 0 {
            return Err(eyre::eyre!(
                "krun_create_ctx failed with status {} (is libkrun installed?)",
                ctx
            ));
        }
        Ok(Self { ctx_id: ctx as u32 })
    }

    unsafe fn set_vm_config(&self, cpus: u8, memory_mib: u32) -> Result<()> {
        check_status(
            "krun_set_vm_config",
            unsafe { krun_set_vm_config(self.ctx_id, cpus, memory_mib) },
        )
    }

    unsafe fn set_root(&self, rootfs: &str) -> Result<()> {
        let rootfs_c = CString::new(rootfs)
            .map_err(|e| eyre::eyre!("invalid rootfs path: {}", e))?;
        check_status("krun_set_root", unsafe { krun_set_root(self.ctx_id, rootfs_c.as_ptr()) })
    }

    #[allow(dead_code)]
    #[allow(dead_code)]
    unsafe fn add_virtiofs(&self, tag: &str, path: &str) -> Result<()> {
        let tag_c = CString::new(tag)
            .map_err(|e| eyre::eyre!("invalid tag: {}", e))?;
        let path_c = CString::new(path)
            .map_err(|e| eyre::eyre!("invalid path: {}", e))?;
        check_status(
            "krun_add_virtiofs",
            unsafe { krun_add_virtiofs(self.ctx_id, tag_c.as_ptr(), path_c.as_ptr()) }
        )
    }

    #[allow(dead_code)]
    /// kernel_format: 0 = Raw (Image for aarch64/riscv64), 1 = ELF (vmlinux for x86_64)
    /// kernel_cmdline: optional extra kernel command line (e.g. from --kernel-args)
    /// initrd_path: optional path to initrd image (e.g. from --initrd)
    unsafe fn set_kernel(
        &self,
        kernel_path: &str,
        kernel_format: u32,
        kernel_cmdline: Option<&str>,
        initrd_path: Option<&str>,
    ) -> Result<()> {
        let kernel_c = CString::new(kernel_path)
            .map_err(|e| eyre::eyre!("invalid kernel path: {}", e))?;
        let cmdline_c = kernel_cmdline
            .and_then(|s| {
                let t = s.trim();
                if t.is_empty() {
                    None
                } else {
                    CString::new(t).ok()
                }
            });
        let cmdline_ptr = cmdline_c
            .as_ref()
            .map(|c| c.as_ptr())
            .unwrap_or(ptr::null());

        let initrd_c = initrd_path
            .and_then(|s| {
                let t = s.trim();
                if t.is_empty() {
                    None
                } else {
                    CString::new(t).ok()
                }
            });
        let initrd_ptr = initrd_c
            .as_ref()
            .map(|c| c.as_ptr())
            .unwrap_or(ptr::null());

        check_status(
            "krun_set_kernel",
            unsafe {
                krun_set_kernel(
                    self.ctx_id,
                    kernel_c.as_ptr(),
                    kernel_format,
                    initrd_ptr,
                    cmdline_ptr,
                )
            },
        )
    }

    unsafe fn start_enter(&self) -> i32 {
        unsafe { krun_start_enter(self.ctx_id) }
    }

    /// Get the shutdown eventfd for triggering VM shutdown from host.
    /// Writing 1u64 to this fd will cause the VM to exit gracefully.
    unsafe fn get_shutdown_eventfd(&self) -> Result<i32> {
        let fd = unsafe { krun_get_shutdown_eventfd(self.ctx_id) };
        if fd < 0 {
            Err(eyre::eyre!("krun_get_shutdown_eventfd failed with status {}", fd))
        } else {
            Ok(fd)
        }
    }
}

#[cfg(feature = "libkrun")]
impl Drop for KrunContext {
    fn drop(&mut self) {
        unsafe {
            let _ = krun_free_ctx(self.ctx_id);
        }
    }
}

/// Active libkrun VM for install/upgrade reuse on non-Linux hosts (one session per epkg process).
#[cfg(all(feature = "libkrun", not(target_os = "linux")))]
struct VmReuseSession {
    ctx_id:            u32,
    shutdown_fd:       i32,
    vsock_sock_path:   std::path::PathBuf,
    vm_thread:         thread::JoinHandle<i32>,
    env_root:          std::path::PathBuf,
}

#[cfg(all(feature = "libkrun", not(target_os = "linux")))]
static VM_REUSE_SESSION: Mutex<Option<VmReuseSession>> = Mutex::new(None);

#[cfg(feature = "libkrun")]
fn apply_krun_exit_policy(exit_code: i32, run_options: &RunOptions) -> Result<()> {
    if exit_code != 0 {
        if run_options.no_exit {
            eprintln!(
                "Command exited with code {} (no_exit=true, continuing)",
                exit_code
            );
        } else {
            std::process::exit(exit_code);
        }
    }
    Ok(())
}

#[cfg(all(feature = "libkrun", not(target_os = "linux")))]
fn try_reuse_existing_krun_session(
    env_root: &Path,
    config: &LibkrunConfig,
    run_options: &RunOptions,
) -> Result<Option<i32>> {
    let mut guard = VM_REUSE_SESSION.lock().unwrap();
    if let Some(session) = guard.as_ref() {
        if session.env_root != env_root {
            let old = guard.take().unwrap();
            drop(guard);
            shutdown_krun_session_impl(old)?;
            return Ok(None);
        }
        let sock = session.vsock_sock_path.clone();
        drop(guard);
        let code = libkrun_stream::send_command_via_vsock(
            &config.cmd_parts,
            run_options.io_mode,
            run_options.reuse_vm,
            &sock,
        )
        .map_err(|e| eyre::eyre!("Failed to send command via vsock bridge: {}", e))?;
        Ok(Some(code))
    } else {
        Ok(None)
    }
}

#[cfg(all(feature = "libkrun", target_os = "linux"))]
fn try_reuse_existing_krun_session(
    _env_root: &Path,
    _config: &LibkrunConfig,
    _run_options: &RunOptions,
) -> Result<Option<i32>> {
    Ok(None)
}

#[cfg(all(feature = "libkrun", not(target_os = "linux")))]
fn send_session_done_unix(sock_path: &Path) -> Result<()> {
    let req = serde_json::to_vec(&libkrun_stream::build_command_request(
        &[crate::run::VM_SESSION_DONE_CMD.to_string()],
        crate::models::IoMode::Stream,
        false,
    ))?;
    #[cfg(unix)]
    {
        let mut stream = libkrun_bridge::connect_vsock_bridge(sock_path, 30)?;
        stream.write_all(&req)?;
        stream.write_all(b"\n")?;
        stream.flush()?;
    }
    #[cfg(windows)]
    {
        let mut stream = libkrun_bridge::connect_vsock_bridge(sock_path, 30)?;
        stream.write_all(&req)?;
        stream.write_all(b"\n")?;
        stream.flush()?;
    }
    Ok(())
}

#[cfg(all(feature = "libkrun", not(target_os = "linux")))]
fn shutdown_krun_session_impl(session: VmReuseSession) -> Result<()> {
    log::debug!("libkrun: shutting down reuse VM session");
    send_session_done_unix(&session.vsock_sock_path)?;
    let buf = 1u64.to_le_bytes();
    #[cfg(unix)]
    let write_len = buf.len();
    #[cfg(windows)]
    let write_len = buf.len() as u32;
    let write_result = unsafe { libc::write(session.shutdown_fd, buf.as_ptr() as *const _, write_len) };
    if write_result < 0 {
        log::warn!(
            "libkrun: failed to write shutdown eventfd: {}",
            std::io::Error::last_os_error()
        );
    }
    match session.vm_thread.join() {
        Ok(vm_status) => {
            log::debug!("libkrun: VM thread finished with status {}", vm_status);
        }
        Err(e) => {
            log::error!("libkrun: VM thread join failed: {:?}", e);
        }
    }
    unsafe {
        let _ = krun_free_ctx(session.ctx_id);
    }
    Ok(())
}

/// End a reuse VM session after install/upgrade completes (non-Linux + libkrun only).
#[cfg(feature = "libkrun")]
pub fn shutdown_vm_reuse_session_if_active() -> Result<()> {
    #[cfg(not(target_os = "linux"))]
    {
        let mut guard = VM_REUSE_SESSION.lock().unwrap();
        let Some(session) = guard.take() else {
            return Ok(());
        };
        drop(guard);
        shutdown_krun_session_impl(session)
    }
    #[cfg(target_os = "linux")]
    {
        Ok(())
    }
}

#[cfg(feature = "libkrun")]
fn krun_vsock_shutdown_join_free_exit(
    vm_thread: std::thread::JoinHandle<i32>,
    shutdown_fd: i32,
    ctx_id: u32,
    exit_code: i32,
) -> ! {
    log::debug!("libkrun: triggering VM shutdown via eventfd...");
    let buf = 1u64.to_le_bytes();
    #[cfg(unix)]
    let write_len = buf.len();
    #[cfg(windows)]
    let write_len = buf.len() as u32;
    let write_result = unsafe { libc::write(shutdown_fd, buf.as_ptr() as *const _, write_len) };
    if write_result < 0 {
        log::warn!(
            "libkrun: failed to write shutdown eventfd: {}",
            std::io::Error::last_os_error()
        );
    }

    match vm_thread.join() {
        Ok(vm_status) => {
            log::debug!("libkrun: VM thread finished with status {}", vm_status);
        }
        Err(e) => {
            log::error!("libkrun: VM thread join failed: {:?}", e);
        }
    }

    log::debug!("libkrun: freeing context before exit...");
    unsafe {
        let _ = krun_free_ctx(ctx_id);
    }

    log::debug!("libkrun: exiting with code {}", exit_code);
    std::process::exit(exit_code);
}

#[cfg(feature = "libkrun")]
fn krun_no_vsock_join_vm_thread_exit(vm_thread: std::thread::JoinHandle<i32>, ctx_id: u32) -> ! {
    log::debug!("libkrun: waiting for VM thread to finish...");
    match vm_thread.join() {
        Ok(exit_status) => {
            log::debug!("libkrun: freeing context before exit...");
            unsafe {
                let _ = krun_free_ctx(ctx_id);
            }

            if exit_status < 0 {
                log::error!("libkrun: VM failed with status {}", exit_status);
                std::process::exit(1);
            } else {
                log::debug!("libkrun: VM exited with status {}", exit_status);
                std::process::exit(exit_status);
            }
        }
        Err(e) => {
            log::error!("libkrun: VM thread join failed: {:?}", e);
            unsafe {
                let _ = krun_free_ctx(ctx_id);
            }
            std::process::exit(1);
        }
    }
}

/// Run a command inside a libkrun microVM.
///
/// On non-reuse paths this never returns on success; it exits the process with the
/// guest's exit code, similar to the QEMU backend.
///
/// With `reuse_vm`, returns to the caller so another command can run in the same VM.
///
/// The kernel is provided by sandbox-kernel as a unified kernel file.
/// Architecture-specific format:
/// - x86_64: ELF vmlinux format
/// - aarch64/riscv64: Raw Image format
#[cfg(feature = "libkrun")]
pub fn run_command_in_krun(
    env_root: &Path,
    run_options: &RunOptions,
    guest_cmd_path: &Path,
) -> Result<()> {
    crate::run::ensure_linux_kvm_ready_for_vm()?;
    let config = build_libkrun_config(env_root, run_options, guest_cmd_path)?;

    if config.use_vsock {
        if run_options.reuse_vm {
            if let Some(code) = try_reuse_existing_krun_session(env_root, &config, run_options)? {
                log::debug!("libkrun: reused VM session, exit code {}", code);
                return apply_krun_exit_policy(code, run_options);
            }
        }

        let vm_ctx = create_and_configure_vm(env_root, run_options, &config)?;

        #[cfg(unix)]
        let ready_listener = libkrun_bridge::setup_vsock_ready_listener()?
            .ok_or_else(|| eyre::eyre!("libkrun: missing ready listener"))?;
        #[cfg(windows)]
        let ready_pipe = libkrun_bridge::setup_vsock_ready_listener()?
            .ok_or_else(|| eyre::eyre!("libkrun: missing ready listener"))?;

        log::debug!("libkrun: starting VM thread...");
        let ctx_id = vm_ctx.ctx.ctx_id;
        let shutdown_fd = vm_ctx.shutdown_fd;
        let vsock_sock_path = vm_ctx
            .vsock_sock_path
            .clone()
            .ok_or_else(|| eyre::eyre!("libkrun: missing vsock socket path"))?;

        // Channel to signal VM start failure to avoid waiting 30s on error
        let (start_failed_tx, start_failed_rx) = std::sync::mpsc::channel();
        let vm_thread = start_libkrun_vm(vm_ctx.ctx, start_failed_tx);

        log::debug!("libkrun: waiting for guest to be ready (with timeout)...");
        #[cfg(unix)]
        libkrun_bridge::wait_guest_ready_unix(&ready_listener, Some(&start_failed_rx))?;
        #[cfg(windows)]
        libkrun_bridge::wait_guest_ready_windows(&ready_pipe, Some(&start_failed_rx))?;

        let exit_code = libkrun_stream::send_command_via_vsock(
            &config.cmd_parts,
            run_options.io_mode,
            run_options.reuse_vm,
            &vsock_sock_path,
        )
        .map_err(|e| eyre::eyre!("Failed to send command via vsock bridge: {}", e))?;
        log::debug!("libkrun: vsock command completed with exit code {}", exit_code);

        if run_options.reuse_vm {
            #[cfg(not(target_os = "linux"))]
            {
                *VM_REUSE_SESSION.lock().unwrap() = Some(VmReuseSession {
                    ctx_id,
                    shutdown_fd,
                    vsock_sock_path,
                    vm_thread,
                    env_root: env_root.to_path_buf(),
                });
                log::debug!("libkrun: VM session kept alive for reuse");
                return apply_krun_exit_policy(exit_code, run_options);
            }
        }

        krun_vsock_shutdown_join_free_exit(vm_thread, shutdown_fd, ctx_id, exit_code);
    }

    let vm_ctx = create_and_configure_vm(env_root, run_options, &config)?;
    log::debug!("libkrun: starting VM thread...");
    let ctx_id = vm_ctx.ctx.ctx_id;
    // Channel for signaling VM start failure (unused in no-vsock path)
    let (start_failed_tx, _start_failed_rx) = std::sync::mpsc::channel();
    let vm_thread = start_libkrun_vm(vm_ctx.ctx, start_failed_tx);

    krun_no_vsock_join_vm_thread_exit(vm_thread, ctx_id);
}

/// Setup console output logging to a file for debugging kernel boot.
///
/// Creates a per-PID log file and a symlink at "latest-console.log" for easy access.
///
/// Example paths:
/// - Log file: `$HOME/.cache/epkg/vmm-logs/libkrun-console-<pid>.log`
/// - Symlink:  `$HOME/.cache/epkg/vmm-logs/latest-console.log` -> latest log file
///
/// Usage:
/// ```bash
/// # After running a VM, check the console output:
/// less ~/.cache/epkg/vmm-logs/latest-console.log
/// ```
fn setup_console_output(ctx_id: u32) -> Result<()> {
    use std::ffi::CString;

    let base_log_dir = crate::models::dirs().epkg_cache.join("vmm-logs");
    lfs::create_dir_all(&base_log_dir)
        .map_err(|e| eyre::eyre!("Failed to create VMM log directory: {}", e))?;

    let pid = std::process::id();
    let console_log_path = base_log_dir.join(format!("libkrun-console-{}.log", pid));

    let console_log = CString::new(console_log_path.to_string_lossy().as_bytes())
        .map_err(|e| eyre::eyre!("invalid console log path: {}", e))?;

    check_status("krun_set_console_output",
        unsafe { krun_set_console_output(ctx_id, console_log.as_ptr()) }
    )?;
    log::debug!("libkrun: console output -> {}", console_log_path.display());

    let latest_log_symlink = base_log_dir.join("latest-console.log");
    let _ = std::fs::remove_file(&latest_log_symlink);
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        if let Err(e) = symlink(&console_log_path, &latest_log_symlink) {
            log::warn!("libkrun: failed to create symlink: {}", e);
        }
    }
    #[cfg(windows)]
    {
        if let Err(e) = std::os::windows::fs::symlink_file(&console_log_path, &latest_log_symlink) {
            log::warn!("libkrun: failed to create symlink: {}", e);
        }
    }

    Ok(())
}

// ============================================================================
// Cross-platform helper functions (shared with qemu.rs)
// ============================================================================

/// Build guest command string and percent-encode it for kernel command line.
/// Returns (cmd_parts for vsock, init_cmd for kernel cmdline).
fn build_guest_command(cmd_path: &Path, args: &[String]) -> Result<(Vec<String>, String)> {
    let mut cmd_parts: Vec<String> = Vec::new();
    // Convert Windows backslashes to forward slashes for Linux guest
    let cmd_path_str = cmd_path.to_string_lossy().replace('\\', "/");
    cmd_parts.push(cmd_path_str);
    cmd_parts.extend(args.iter().cloned());
    // Use shlex-style quoting to survive kernel cmdline parsing
    let raw_cmd = shlex::try_join(cmd_parts.iter().map(|s| s.as_str()))
        .map_err(|e| eyre::eyre!("Failed to join command parts: {}", e))?;
    let init_cmd = percent_encode(&raw_cmd);
    Ok((cmd_parts, init_cmd))
}

/// Percent-encode special characters for kernel command line.
/// Spaces -> %20, = -> %3D, " -> %22, ' -> %27, \ -> %5C, % -> %25
/// Keeps slashes and most other characters readable.
fn percent_encode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            ' ' => result.push_str("%20"),
            '=' => result.push_str("%3D"),
            '"' => result.push_str("%22"),
            '\'' => result.push_str("%27"),
            '\\' => result.push_str("%5C"),
            '%' => result.push_str("%25"),
            c => result.push(c),
        }
    }
    result
}
