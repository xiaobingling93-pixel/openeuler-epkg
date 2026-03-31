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

// CRITICAL: Windows guest init path is CARVED IN STONE as /usr/bin/init.
// The alpine environment has the epkg guest init at /usr/bin/init (190MB binary).
// NEVER change this path - it must remain /usr/bin/init forever.
// (Alternative: virtiofs virtual /init.krun when using embedded_init feature)

/// CARVED IN STONE: Guest init path for all VM Linux guests (non-embedded_init mode).
/// The alpine environment has the epkg guest init binary at this path (190MB).
/// This constant exists to prevent accidental changes - DO NOT MODIFY THIS VALUE.
/// Applies to: kernel cmdline init= param AND krun_set_exec() path.
/// Note: In embedded_init mode, the init path is /init.krun (virtiofs virtual path
/// to the embedded init binary, defined in libkrun git's embedded_init feature).
#[cfg(feature = "libkrun")]
const GUEST_INIT_PATH: &str = "/usr/bin/init";

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
    /// Add a legacy serial console device (ttyS0) with the given input/output fds.
    /// This is required for Windows WHPX VMs to have working serial console.
    /// Must be called before krun_start_enter.
    fn krun_add_serial_console_default(
        ctx_id: u32,
        input_fd: libc::c_int,
        output_fd: libc::c_int,
    ) -> i32;
    /// Mount an additional directory via virtiofs into the guest.
    /// tag: the filesystem tag (e.g., "self")
    /// path: the host directory path to mount
    unsafe fn krun_add_virtiofs(
        ctx_id: u32,
        c_tag: *const std::ffi::c_char,
        c_path: *const std::ffi::c_char,
    ) -> i32;
    /// Signal the shutdown eventfd to trigger VM shutdown.
    /// This properly writes to the EventFd on all platforms.
    fn krun_signal_shutdown(ctx_id: u32) -> i32;
    /// Set the executable path and arguments for the guest init process.
    /// This is used for embedded_init mode to specify the init program.
    fn krun_set_exec(
        ctx_id: u32,
        c_exec_path: *const std::ffi::c_char,
        c_args: *const *const std::ffi::c_char,
        c_env: *const *const std::ffi::c_char,
    ) -> i32;
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
    // First check file exists and get metadata
    let metadata = std::fs::metadata(kernel_path)
        .map_err(|e| eyre::eyre!("Kernel file not accessible at {}: {}", kernel_path, e))?;

    let size = metadata.len();
    if size < 1024 * 1024 {
        return Err(eyre::eyre!(
            "Kernel file at {} is too small ({} bytes). Expected at least 1MB",
            kernel_path, size
        ));
    }
    log::info!("libkrun: kernel file size: {} bytes ({:.2} MB)", size, size as f64 / 1024.0 / 1024.0);

    if is_elf_kernel(kernel_path)? {
        log::info!("libkrun: kernel {} is ELF format (vmlinux)", kernel_path);
        Ok(1) // ELF (vmlinux)
    } else {
        // Assume Raw format (e.g., aarch64 Image)
        log::info!("libkrun: kernel {} is Raw format (Image)", kernel_path);
        Ok(0) // Raw (Image)
    }
}

#[cfg(all(feature = "libkrun", target_os = "windows"))]
/// Set up Windows VM diagnostics environment variables for libkrun WHPX debugging.
/// These variables are read by libkrun to enable detailed logging.
fn setup_windows_vm_diagnostics() {
    // Default libkrun file logs next to epkg's vmm-logs (matches %USERPROFILE%\.epkg\cache\vmm-logs).
    #[cfg(target_os = "windows")]
    if std::env::var("LIBKRUN_LOG_DIR").is_err() {
        let log_dir = crate::models::dirs().epkg_cache.join("vmm-logs");
        if let Some(log_dir_str) = log_dir.to_str() {
            std::env::set_var("LIBKRUN_LOG_DIR", log_dir_str);
            log::info!("libkrun: LIBKRUN_LOG_DIR={} (epkg default; same root as session logs)", log_dir_str);
        }
    }

    if std::env::var("EPKG_VM_DEBUG").is_ok() {
        log::info!("libkrun: enabling Windows VM diagnostics (EPKG_VM_DEBUG set)");

        if std::env::var("LIBKRUN_WINDOWS_VERBOSE_DEBUG").is_err() {
            std::env::set_var("LIBKRUN_WINDOWS_VERBOSE_DEBUG", "1");
            log::info!("libkrun: set LIBKRUN_WINDOWS_VERBOSE_DEBUG=1 for libkrun WHPX tracing");
        }

        log::info!("libkrun: optional extra knobs (set manually if needed):");
        log::info!("  - LIBKRUN_WHPX_PIC_IRQ0_FIXED=1");
        log::info!("  - LIBKRUN_WHPX_PIC_FIXED_INJECT=pending-interruption");
        log::info!("  - LIBKRUN_WHPX_SKIP_CANCEL_ON_HLT_IRQ=1");
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
    use_reverse_vsock:   bool,  // true: Guest connects to Host (first run)
    cmd_parts:           Vec<String>,
    kernel_args:         String,
    kernel_path:         Option<String>,
    kernel_format:       Option<u32>,
    /// Additional virtiofs mounts: (tag, host_path, guest_path, read_only)
    virtiofs_mounts:     Vec<(String, String, String, bool)>,
}

#[cfg(feature = "libkrun")]
fn build_libkrun_config(
    env_root: &Path,
    run_options: &RunOptions,
    guest_cmd_path: &Path,
) -> Result<LibkrunConfig> {
    #[cfg(all(target_os = "windows", feature = "embedded_init"))]
    log::info!(
        "libkrun: embedded_init enabled: guest uses virtual /init.krun (libkrun devices; no epkg file extract)"
    );
    #[cfg(all(target_os = "windows", not(feature = "embedded_init")))]
    log::info!(
        "libkrun: guest init is /usr/bin/init (epkg Linux ELF in env). For early bootstrap only, rebuild with --features embedded_init (virtual init.krun)."
    );

    let use_cmdline_mode = std::env::var("EPKG_VM_NO_DAEMON").is_ok();
    let use_vsock = !use_cmdline_mode;
    log::debug!("libkrun: mode: cmdline={}, vsock={}", use_cmdline_mode, use_vsock);
    log::debug!("libkrun: EPKG_VM_NO_DAEMON={}", std::env::var("EPKG_VM_NO_DAEMON").unwrap_or_else(|_| "not set".to_string()));

    // Use reverse vsock mode for first run to avoid potential vsock handshake timing issues.
    // In reverse mode, Guest connects to Host after full initialization.
    // For reuse mode on non-Linux, check if there's an existing session:
    // - If existing session: use forward mode (Host connects to Guest)
    // - If no existing session: use reverse mode for first command, then switch to forward
    #[cfg(all(feature = "libkrun", not(target_os = "linux")))]
    let has_existing_session = if run_options.reuse_vm {
        VM_REUSE_SESSION.lock().unwrap().is_some()
    } else {
        false
    };
    #[cfg(any(not(feature = "libkrun"), target_os = "linux"))]
    let has_existing_session = false;
    // TEMPORARY DEBUG: Always use reverse mode to test the fix
    let use_reverse_vsock = use_vsock;
    crate::debug_epkg!("libkrun: use_vsock={} has_existing_session={} use_reverse_vsock={}",
               use_vsock, has_existing_session, use_reverse_vsock);
    log::info!("libkrun: use_vsock={} has_existing_session={} use_reverse_vsock={}",
               use_vsock, has_existing_session, use_reverse_vsock);

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
    // On Windows, use ttyS0 for serial console since libkrun auto-adds COM1 device.
    // embedded_init: virtiofs synthetic /init.krun. Otherwise: epkg guest at /usr/bin/init.
    //
    // Performance note: Serial console output on WHPX is slow (~10x slower than Linux KVM)
    // because each character requires a VM exit and MMIO write. Reduce loglevel and disable
    // earlyprintk when not debugging to speed up boot.
    let vm_debug = std::env::var("EPKG_VM_DEBUG").is_ok();
    let loglevel = if vm_debug { "loglevel=8 debug" } else { "quiet loglevel=1" };

    // Additional performance optimizations for VMs:
    // - nowatchdog: Disable watchdog timers (not needed in VMs)
    // - nmi_watchdog=0: Disable NMI watchdog
    // - lpj=11979608: Pre-set loops per jiffy to avoid PIT calibration
    //   (calculated from BogoMIPS 5989.80: lpj = BogoMIPS * 2 * HZ/500)
    // - tsc=reliable: Use TSC as reliable clocksource (avoids PIT calibration hang on WHPX)
    // - disable_kvm_pv: Disable KVM PV extensions that may interfere with WHPX
    // Note: The vmm timer fix (85d5f9b) in libkrun makes noapic/rootdelay/notsc unnecessary.
    // These parameters caused 100x slowdown in boot time and have been removed.
    #[cfg(target_os = "windows")]
    let vm_perf = "nowatchdog nmi_watchdog=0 lpj=11979608 tsc=reliable disable_kvm_pv=1";
    #[cfg(not(target_os = "windows"))]
    let vm_perf = "nowatchdog nmi_watchdog=0";

    #[cfg(all(target_os = "windows", feature = "embedded_init"))]
    let base_cmdline = format!(
        "reboot=k panic=-1 panic_print=0 nomodule console=ttyS0 {} {} {} \
         root=/dev/root rootfstype=virtiofs rw no-kvmapf init=/init.krun",
        if vm_debug { "earlyprintk=serial" } else { "" },
        loglevel, vm_perf
    );
    // Uses GUEST_INIT_PATH constant (CARVED IN STONE as /usr/bin/init)
    #[cfg(all(target_os = "windows", not(feature = "embedded_init")))]
    let base_cmdline = format!(
        "reboot=k panic=-1 panic_print=0 nomodule console=ttyS0 {} {} {} \
         root=/dev/root rootfstype=virtiofs rw no-kvmapf init={}",
        if vm_debug { "earlyprintk=serial" } else { "" },
        loglevel, vm_perf, GUEST_INIT_PATH
    );
    #[cfg(not(target_os = "windows"))]
    let base_cmdline = {
        let ep = if vm_debug { "earlyprintk=hvc0" } else { "" };
        format!(
            "reboot=k panic=-1 panic_print=0 nomodule console=hvc0 {} {} {} \
             root=/dev/root rootfstype=virtiofs rw no-kvmapf init={}",
            ep, loglevel, vm_perf, GUEST_INIT_PATH
        )
    };
    let mut kernel_args = base_cmdline;
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

    // Enable reverse vsock mode for first run on Windows/WHPX.
    // In reverse mode, Guest connects to Host, avoiding vsock handshake timing issues.
    if use_reverse_vsock {
        kernel_args.push_str(" epkg.vsock_reverse=1");
        crate::debug_epkg!("libkrun: reverse vsock mode enabled (epkg.vsock_reverse=1)");
        crate::debug_epkg!("libkrun: kernel_args after adding vsock_reverse: {}", kernel_args);
    } else {
        crate::debug_epkg!("libkrun: reverse vsock mode NOT enabled (use_reverse_vsock=false)");
    }
    crate::debug_epkg!("libkrun: kernel_args before virtiofs: {}", kernel_args);

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
    crate::debug_epkg!("libkrun: FINAL kernel_args passed to VM: {}", kernel_args);

    let kernel_path = if run_options.kernel.is_some() {
        run_options.kernel.clone()
    } else {
        // Fall back to default kernel path (envs/self/boot/kernel from `epkg self install`)
        crate::init::default_kernel_path_if_exists()
    };

    // Debug: trace kernel path resolution on Windows
    #[cfg(target_os = "windows")]
    if let Some(ref kernel) = kernel_path {
        crate::debug_epkg!("libkrun: resolved kernel path: {}", kernel);
        let exists = std::fs::metadata(kernel).is_ok();
        crate::debug_epkg!("libkrun: kernel file exists: {}", exists);
        if exists {
            match detect_kernel_format_for_libkrun(kernel) {
                Ok(format) => {
                    let format_name = if format == 1 { "ELF (vmlinux)" } else { "Raw (Image)" };
                    crate::debug_epkg!("libkrun: kernel format detected: {} ({})", format, format_name);
                }
                Err(e) => {
                    crate::debug_epkg!("libkrun: kernel format detection failed: {}", e);
                }
            }
        }
    } else {
        crate::debug_epkg!("libkrun: no kernel available (no --kernel specified and no default kernel found)");
    }

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
        use_reverse_vsock,
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
    vsock_sock_path: Option<std::path::PathBuf>,
}

/// Configure vsock ports 10000 (command) and optionally 10001 (ready).
///
/// In forward mode (reverse=false): Port 10000 listen=true (Guest listens), 10001 listen=false (Host listens for ready)
/// In reverse mode (reverse=true): Port 10000 listen=false (Host listens), no ready port needed
///
/// Returns host path for the command socket/pipe.
#[cfg(feature = "libkrun")]
fn setup_libkrun_vsock_host_sockets(ctx: &KrunContext, reverse: bool) -> Result<std::path::PathBuf> {
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
        const KRUN_TSI_HIJACK_INET: u32 = 1 << 0;
        const KRUN_TSI_HIJACK_UNIX: u32 = 1 << 1;
        let tsi_features = KRUN_TSI_HIJACK_INET | KRUN_TSI_HIJACK_UNIX;
        check_status("krun_add_vsock", krun_add_vsock(ctx.ctx_id, tsi_features))?;

        // Port 10000: Command port
        // In reverse mode: listen=false, Host creates listener, Guest connects
        // In forward mode: listen=true, Guest creates listener, Host connects
        let listen_10000 = !reverse;

        #[cfg(unix)]
        {
            let sock_path_c = CString::new(sock_path.to_string_lossy().as_bytes())
                .map_err(|e| eyre::eyre!("invalid socket path: {}", e))?;
            check_status(
                "krun_add_vsock_port2",
                krun_add_vsock_port2(ctx.ctx_id, 10000, sock_path_c.as_ptr(), listen_10000),
            )?;
        }
        #[cfg(windows)]
        {
            let stem = libkrun_bridge::pipe_name_from_sock_path(&sock_path)?;
            let stem_c = CString::new(stem).map_err(|e| eyre::eyre!("invalid vsock pipe name: {}", e))?;
            check_status(
                "krun_add_vsock_port2_windows",
                krun_add_vsock_port2_windows(ctx.ctx_id, 10000, stem_c.as_ptr(), listen_10000),
            )?;
        }

        // Port 10001: Ready notification (only needed in forward mode)
        // In reverse mode, Guest connects directly to port 10000, no separate ready port needed
        if !reverse {
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
            log::debug!("libkrun: ready port 10001 mapped to {}", ready_path.display());
        }
    }

    log::debug!("libkrun: vsock port 10000 mapped to {} (listen={}, reverse={})",
               sock_path.display(), !reverse, reverse);

    Ok(sock_path)
}

#[cfg(feature = "libkrun")]
fn create_and_configure_vm(
    env_root: &Path,
    run_options: &RunOptions,
    config: &LibkrunConfig,
) -> Result<VmContext> {
    // Set up Windows VM diagnostics if requested
    #[cfg(target_os = "windows")]
    setup_windows_vm_diagnostics();

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
        log::info!("libkrun: VM config set: {} vCPUs, {} MiB RAM", cpus, memory_mib);

        if let Some(ref kernel) = config.kernel_path {
            // Verify kernel file exists and is readable
            match std::fs::metadata(kernel) {
                Ok(meta) => {
                    log::info!("libkrun: kernel file: {} ({} bytes)", kernel, meta.len());
                }
                Err(e) => {
                    log::error!("libkrun: kernel file not accessible: {}: {}", kernel, e);
                    return Err(eyre::eyre!("Kernel file not accessible: {}", e));
                }
            }

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
                log::info!("libkrun: kernel configured (format={} ({}))", format, format_str);
                log::info!("libkrun: kernel_args passed to VM: {}", config.kernel_args);
            }
        } else {
            log::error!("libkrun: no kernel path configured!");
            return Err(eyre::eyre!("No kernel configured for VM"));
        }

        let root_path_str = env_root.to_str()
            .ok_or_else(|| eyre::eyre!("Environment root path contains invalid UTF-8: {:?}", env_root))?;
        ctx.set_root(root_path_str)?;
        log::info!("libkrun: rootfs configured: {}", root_path_str);

        // Add additional virtiofs mounts
        for (tag, host_path, guest_path, read_only) in &config.virtiofs_mounts {
            // Verify mount source exists before adding
            if !std::path::Path::new(host_path).exists() {
                log::warn!("libkrun: skipping virtiofs mount for {}: source path does not exist: {}",
                    tag, host_path);
                continue;
            }
            ctx.add_virtiofs(tag, host_path)?;
            log::info!("libkrun: virtiofs mount: {} -> {} (guest: {}) ({})",
                       host_path, tag, guest_path, if *read_only { "ro" } else { "rw" });
        }

        // split_irqchip is only supported on x86_64; skip on aarch64
        #[cfg(target_arch = "x86_64")]
        {
            check_status("krun_split_irqchip",
                krun_split_irqchip(ctx.ctx_id, true)
            )?;
            log::info!("libkrun: split IRQ chip enabled");
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            log::debug!("libkrun: skipping split IRQ chip (x86_64 only)");
        }

        setup_console_output(ctx.ctx_id)?;
        log::info!("libkrun: console output configured");

        // Add serial console device for Windows VMs.
        // This is required for the kernel to have a working console on WHPX.
        //
        // IMPORTANT: Only call add_serial_console when EPKG_DEBUG_LIBKRUN is NOT set.
        // When EPKG_DEBUG_LIBKRUN is set, setup_console_output() sets console_output file,
        // and libkrun will automatically create a serial device using that file.
        // Calling add_serial_console would override this and break console logging.
        #[cfg(target_os = "windows")]
        {
            if std::env::var("EPKG_DEBUG_LIBKRUN").is_err() {
                // Not debugging: add serial console to NUL to discard kernel messages
                use windows::Win32::Storage::FileSystem::{
                    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE,
                    OPEN_EXISTING,
                };
                use windows::Win32::Foundation::INVALID_HANDLE_VALUE;
                use windows::Win32::Foundation::CloseHandle;
                use windows::core::PCWSTR;

                let nul_path: Vec<u16> = "NUL".encode_utf16().chain(std::iter::once(0)).collect();
                // GENERIC_READ = 0x80000000, GENERIC_WRITE = 0x40000000
                let access: u32 = 0x80000000u32 | 0x40000000u32;
                let null_handle = CreateFileW(
                    PCWSTR(nul_path.as_ptr()),
                    access,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    None,
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    None,
                );

                match null_handle {
                    Ok(h) if h != INVALID_HANDLE_VALUE => {
                        // Convert HANDLE to CRT fd using open_osfhandle.
                        // CrtFdWriter in libkrun uses libc::write() which requires CRT fd.
                        let handle_value = h.0 as libc::intptr_t;
                        let null_fd = libc::open_osfhandle(handle_value, libc::O_RDWR);
                        if null_fd >= 0 {
                            ctx.add_serial_console(null_fd, null_fd)?;
                            log::info!("libkrun: serial console added (ttyS0 -> NUL, fd={})", null_fd);
                        } else {
                            // open_osfhandle failed, close the handle and fallback
                            let _ = CloseHandle(h);
                            ctx.add_serial_console(0, 1)?;
                            log::warn!("libkrun: open_osfhandle failed, serial console -> stdout");
                        }
                    }
                    _ => {
                        // Fallback: still add serial console but output may appear
                        ctx.add_serial_console(0, 1)?;
                        log::warn!("libkrun: failed to open NUL, serial console -> stdout");
                    }
                }
            }
            // When EPKG_DEBUG_LIBKRUN is set, setup_console_output() already configured
            // console output to a file. libkrun will create a serial device automatically.
        }

        // For non-embedded_init mode, explicitly set the init path via krun_set_exec.
        // Uses GUEST_INIT_PATH constant (CARVED IN STONE as /usr/bin/init).
        #[cfg(all(target_os = "windows", not(feature = "embedded_init")))]
        {
            log::info!("libkrun: setting exec path to {} (production mode)", GUEST_INIT_PATH);
            crate::debug_epkg!("libkrun: calling krun_set_exec for {} (production mode)", GUEST_INIT_PATH);
            if let Err(e) = ctx.set_exec(GUEST_INIT_PATH, None, None) {
                log::warn!("libkrun: krun_set_exec failed (non-fatal, kernel cmdline fallback): {}", e);
            }
        }

        if config.use_vsock {
            let sock_path = setup_libkrun_vsock_host_sockets(&ctx, config.use_reverse_vsock)?;
            let vsock_sock_path = Some(sock_path);
            return Ok(VmContext { ctx, vsock_sock_path });
        }
    }

    Ok(VmContext { ctx, vsock_sock_path: None })
}

#[cfg(feature = "libkrun")]
fn start_libkrun_vm(ctx: KrunContext, start_failed_tx: std::sync::mpsc::Sender<()>) -> std::thread::JoinHandle<i32> {
    log::info!("libkrun: starting VM thread (ctx_id={})...", ctx.ctx_id);
    crate::debug_epkg!("libkrun: VM thread starting (ctx_id={})", ctx.ctx_id);
    thread::spawn(move || {
        let result = std::panic::catch_unwind(|| {
            unsafe {
                log::info!("libkrun: entering krun_start_enter (ctx_id={})...", ctx.ctx_id);
                crate::debug_epkg!("libkrun: entering krun_start_enter (ctx_id={})...", ctx.ctx_id);
                let status = ctx.start_enter();
                crate::debug_epkg!("libkrun: krun_start_enter returned status {}", status);
                if status < 0 {
                    log::error!("libkrun: krun_start_enter failed with status {} (ctx_id={})", status, ctx.ctx_id);
                    crate::debug_epkg!("libkrun: krun_start_enter FAILED with status {}", status);
                    // Signal failure to main thread so it doesn't wait for timeout
                    let _ = start_failed_tx.send(());
                } else {
                    log::info!("libkrun: krun_start_enter returned status {} (VM exited normally)", status);
                    crate::debug_epkg!("libkrun: VM exited normally with status {}", status);
                }
                status
            }
        });
        match result {
            Ok(status) => status,
            Err(e) => {
                crate::debug_epkg!("libkrun: VM thread panicked: {:?}", e);
                let _ = start_failed_tx.send(());
                -1
            }
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
        crate::debug_epkg!("libkrun: about to call krun_start_enter FFI...");
        let status = unsafe { krun_start_enter(self.ctx_id) };
        crate::debug_epkg!("libkrun: krun_start_enter FFI returned {}", status);
        status
    }

    /// Add a legacy serial console device (ttyS0) for Windows VMs.
    /// This is required for the kernel to have a working console on WHPX.
    /// input_fd: stdin file descriptor (0)
    /// output_fd: stdout file descriptor (1)
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    unsafe fn add_serial_console(&self, input_fd: libc::c_int, output_fd: libc::c_int) -> Result<()> {
        check_status(
            "krun_add_serial_console_default",
            unsafe { krun_add_serial_console_default(self.ctx_id, input_fd, output_fd) }
        )
    }

    /// Set the executable path for the guest init process.
    /// Used when NOT using embedded_init to explicitly set init path.
    #[allow(dead_code)]
    unsafe fn set_exec(&self, exec_path: &str, _args: Option<&[&str]>, _env: Option<&[&str]>) -> Result<()> {
        let exec_c = CString::new(exec_path)
            .map_err(|e| eyre::eyre!("invalid exec path: {}", e))?;

        // For now, we don't pass args or env - just set the exec path
        // The kernel cmdline and virtiofs provide the rest
        check_status(
            "krun_set_exec",
            unsafe { krun_set_exec(self.ctx_id, exec_c.as_ptr(), std::ptr::null(), std::ptr::null()) }
        )
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
    vsock_sock_path:   std::path::PathBuf,
    vm_thread:         thread::JoinHandle<i32>,
    env_root:          std::path::PathBuf,
}

#[cfg(all(feature = "libkrun", not(target_os = "linux")))]
static VM_REUSE_SESSION: Mutex<Option<VmReuseSession>> = Mutex::new(None);

#[cfg(feature = "libkrun")]
fn apply_krun_exit_policy(exit_code: i32, run_options: &RunOptions) -> Result<()> {
    crate::debug_epkg!("libkrun: apply_krun_exit_policy called with exit_code={}", exit_code);
    if exit_code != 0 {
        if run_options.no_exit {
            eprintln!(
                "Command exited with code {} (no_exit=true, continuing)",
                exit_code
            );
        } else {
            crate::debug_epkg!("libkrun: calling std::process::exit({})", exit_code);
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
    let result = unsafe { krun_signal_shutdown(session.ctx_id) };
    if result < 0 {
        log::warn!("libkrun: krun_signal_shutdown failed with status {}", result);
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
fn krun_vsock_shutdown_join_free(
    vm_thread: std::thread::JoinHandle<i32>,
    ctx_id: u32,
) {
    crate::debug_epkg!("libkrun: triggering VM shutdown via krun_signal_shutdown...");
    let result = unsafe { krun_signal_shutdown(ctx_id) };
    if result < 0 {
        crate::debug_epkg!("libkrun: krun_signal_shutdown failed with status {}", result);
    }

    crate::debug_epkg!("libkrun: waiting for VM thread to join...");
    match vm_thread.join() {
        Ok(vm_status) => {
            crate::debug_epkg!("libkrun: VM thread finished with status {}", vm_status);
        }
        Err(e) => {
            crate::debug_epkg!("libkrun: VM thread join failed: {:?}", e);
        }
    }

    crate::debug_epkg!("libkrun: freeing context...");
    unsafe {
        let _ = krun_free_ctx(ctx_id);
    }
}

#[cfg(feature = "libkrun")]
fn krun_vsock_shutdown_join_free_exit(
    vm_thread: std::thread::JoinHandle<i32>,
    ctx_id: u32,
    exit_code: i32,
) -> ! {
    krun_vsock_shutdown_join_free(vm_thread, ctx_id);
    crate::debug_epkg!("libkrun: exiting with code {}", exit_code);
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

/// Run command in reverse vsock mode: Guest connects to Host.
/// This avoids vsock handshake timing issues on Windows/WHPX.
#[cfg(feature = "libkrun")]
fn run_reverse_vsock_mode(
    env_root: &Path,
    run_options: &RunOptions,
    config: &LibkrunConfig,
) -> Result<()> {
    let result = run_reverse_vsock_mode_inner(env_root, run_options, config);
    if let Err(ref e) = result {
        crate::debug_epkg!("libkrun: run_reverse_vsock_mode error: {}", e);
    }
    crate::debug_epkg!("libkrun: run_reverse_vsock_mode returning");
    result
}

#[cfg(feature = "libkrun")]
fn run_reverse_vsock_mode_inner(
    env_root: &Path,
    run_options: &RunOptions,
    config: &LibkrunConfig,
) -> Result<()> {
    crate::debug_epkg!("[PERF] === REVERSE_VSOCK_MODE START ===");
    let total_start = std::time::Instant::now();

    // Create VM with reverse mode (port 10000 listen=false, Host listens)
    let vm_create_start = std::time::Instant::now();
    let vm_ctx = create_and_configure_vm(env_root, run_options, config)?;
    crate::debug_epkg!("[PERF] VM config took {:.3}ms", vm_create_start.elapsed().as_secs_f64() * 1000.0);
    crate::debug_epkg!("VM configured (ctx_id={})", vm_ctx.ctx.ctx_id);

    let vsock_sock_path = vm_ctx
        .vsock_sock_path
        .clone()
        .ok_or_else(|| eyre::eyre!("libkrun: missing vsock socket path"))?;

    // Set up reverse listener on Host (port 10000)
    // In reverse mode, Host listens and Guest connects
    #[cfg(unix)]
    let reverse_listener = libkrun_bridge::setup_reverse_listener(&vsock_sock_path)?;
    #[cfg(windows)]
    let reverse_pipe = libkrun_bridge::setup_reverse_listener(&vsock_sock_path)?;

    crate::debug_epkg!("reverse listener set up on {}", vsock_sock_path.display());

    let ctx_id = vm_ctx.ctx.ctx_id;

    // Channel to signal VM start failure
    let (start_failed_tx, start_failed_rx) = std::sync::mpsc::channel();
    crate::debug_epkg!("[PERF] starting VM thread...");
    let vm_start = std::time::Instant::now();
    let vm_thread = start_libkrun_vm(vm_ctx.ctx, start_failed_tx);

    // Wait for Guest to connect (with timeout)
    crate::debug_epkg!("[PERF] waiting for Guest to connect...");
    #[cfg(unix)]
    let stream = match libkrun_bridge::accept_reverse_connection(&reverse_listener, Some(&start_failed_rx)) {
        Ok(s) => {
            crate::debug_epkg!("[PERF] Guest connected after {:.3}ms", vm_start.elapsed().as_secs_f64() * 1000.0);
            s
        }
        Err(e) => {
            crate::debug_epkg!("accept_reverse_connection FAILED: {}", e);
            return Err(e);
        }
    };
    #[cfg(windows)]
    let stream = match libkrun_bridge::accept_reverse_connection(reverse_pipe, Some(&start_failed_rx)) {
        Ok(s) => {
            crate::debug_epkg!("[PERF] Guest connected after {:.3}ms", vm_start.elapsed().as_secs_f64() * 1000.0);
            s
        }
        Err(e) => {
            crate::debug_epkg!("accept_reverse_connection FAILED: {}", e);
            return Err(e);
        }
    };

    crate::debug_epkg!("[PERF] Guest connected, sending command...");
    let cmd_start = std::time::Instant::now();

    // Send command over the accepted connection
    // On Windows, use the named-pipe-specific function that calls FlushFileBuffers
    #[cfg(windows)]
    let exit_code = match libkrun_stream::send_command_over_named_pipe(
        &config.cmd_parts,
        run_options.io_mode,
        run_options.reuse_vm,
        stream,
    ) {
        Ok(code) => {
            crate::debug_epkg!("[PERF] command execution took {:.3}ms", cmd_start.elapsed().as_secs_f64() * 1000.0);
            crate::debug_epkg!("command completed, exit_code={}", code);
            code
        }
        Err(e) => {
            crate::debug_epkg!("send_command_over_named_pipe FAILED: {}", e);
            return Err(eyre::eyre!("Failed to send command via reverse vsock: {}", e));
        }
    };
    #[cfg(not(windows))]
    let exit_code = match libkrun_stream::send_command_over_stream(
        &config.cmd_parts,
        run_options.io_mode,
        run_options.reuse_vm,
        stream,
    ) {
        Ok(code) => {
            crate::debug_epkg!("[PERF] command execution took {:.3}ms", cmd_start.elapsed().as_secs_f64() * 1000.0);
            crate::debug_epkg!("command completed, exit_code={}", code);
            code
        }
        Err(e) => {
            crate::debug_epkg!("send_command_over_stream FAILED: {}", e);
            return Err(eyre::eyre!("Failed to send command via reverse vsock: {}", e));
        }
    };

    log::debug!("libkrun: reverse vsock command completed with exit code {}", exit_code);

    if run_options.reuse_vm {
        // For reuse mode after reverse start, we need to switch to forward mode
        // This requires notifying Guest to start listening
        #[cfg(not(target_os = "linux"))]
        {
            *VM_REUSE_SESSION.lock().unwrap() = Some(VmReuseSession {
                ctx_id,
                vsock_sock_path,
                vm_thread,
                env_root: env_root.to_path_buf(),
            });
            log::debug!("libkrun: VM session kept alive for reuse (switched to forward mode)");
            return apply_krun_exit_policy(exit_code, run_options);
        }
    }

    // For no_exit mode (e.g., scriptlets), shutdown VM and return instead of exiting process
    if run_options.no_exit {
        crate::debug_epkg!("[PERF] shutting down VM (no_exit mode)...");
        let shutdown_start = std::time::Instant::now();
        krun_vsock_shutdown_join_free(vm_thread, ctx_id);
        crate::debug_epkg!("[PERF] VM shutdown took {:.3}ms", shutdown_start.elapsed().as_secs_f64() * 1000.0);
        return apply_krun_exit_policy(exit_code, run_options);
    }

    crate::debug_epkg!("[PERF] TOTAL time {:.3}ms", total_start.elapsed().as_secs_f64() * 1000.0);
    crate::debug_epkg!("[PERF] shutting down VM and exiting...");
    krun_vsock_shutdown_join_free_exit(vm_thread, ctx_id, exit_code);
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
    crate::debug_epkg!("START run_command_in_krun");
    crate::run::ensure_linux_kvm_ready_for_vm()?;
    crate::debug_epkg!("building config...");
    let config = build_libkrun_config(env_root, run_options, guest_cmd_path)?;
    crate::debug_epkg!("config built, use_vsock={}", config.use_vsock);

    if config.use_vsock {
        crate::debug_epkg!("entering vsock mode...");

        // Handle reverse mode: Guest connects to Host (first run on Windows)
        if config.use_reverse_vsock {
            log::info!("libkrun: calling run_reverse_vsock_mode");
            return run_reverse_vsock_mode(env_root, run_options, &config);
        }
        log::info!("libkrun: NOT using reverse mode, use_reverse_vsock={}", config.use_reverse_vsock);

        // Forward mode: Host connects to Guest (reuse or Unix platforms)
        if run_options.reuse_vm {
            if let Some(code) = try_reuse_existing_krun_session(env_root, &config, run_options)? {
                log::info!("libkrun: reused VM session, exit code {}", code);
                return apply_krun_exit_policy(code, run_options);
            }
        }

        crate::debug_epkg!("creating and configuring VM...");
        let vm_ctx = create_and_configure_vm(env_root, run_options, &config)?;
        crate::debug_epkg!("VM configured (ctx_id={})", vm_ctx.ctx.ctx_id);

        #[cfg(unix)]
        let ready_listener = libkrun_bridge::setup_vsock_ready_listener()?
            .ok_or_else(|| eyre::eyre!("libkrun: missing ready listener"))?;
        #[cfg(windows)]
        let ready_pipe = libkrun_bridge::setup_vsock_ready_listener()?
            .ok_or_else(|| eyre::eyre!("libkrun: missing ready listener"))?;

        crate::debug_epkg!("vsock ready listener set up");

        let ctx_id = vm_ctx.ctx.ctx_id;
        let vsock_sock_path = vm_ctx
            .vsock_sock_path
            .clone()
            .ok_or_else(|| eyre::eyre!("libkrun: missing vsock socket path"))?;

        // Channel to signal VM start failure to avoid waiting 30s on error
        let (start_failed_tx, start_failed_rx) = std::sync::mpsc::channel();
        crate::debug_epkg!("starting VM thread...");
        let vm_thread = start_libkrun_vm(vm_ctx.ctx, start_failed_tx);

        crate::debug_epkg!("libkrun: waiting for guest to be ready (with timeout)...");
        #[cfg(unix)]
        libkrun_bridge::wait_guest_ready_unix(&ready_listener, Some(&start_failed_rx))?;
        #[cfg(windows)]
        libkrun_bridge::wait_guest_ready_windows(&ready_pipe, Some(&start_failed_rx))?;
        crate::debug_epkg!("libkrun: guest is ready, pausing to let vsock bridge setup complete...");

        // Give libkrun time to set up the vsock-to-named-pipe bridge
        // This avoids a race condition where we connect before libkrun is ready
        crate::debug_epkg!("libkrun: sleeping 100ms to let guest accept() setup complete...");
        std::thread::sleep(std::time::Duration::from_millis(100));

        crate::debug_epkg!("libkrun: sending command via vsock...");
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
                    vsock_sock_path,
                    vm_thread,
                    env_root: env_root.to_path_buf(),
                });
                log::debug!("libkrun: VM session kept alive for reuse");
                return apply_krun_exit_policy(exit_code, run_options);
            }
        }

        // For no_exit mode (e.g., scriptlets), shutdown VM and return instead of exiting process
        if run_options.no_exit {
            krun_vsock_shutdown_join_free(vm_thread, ctx_id);
            return apply_krun_exit_policy(exit_code, run_options);
        }

        krun_vsock_shutdown_join_free_exit(vm_thread, ctx_id, exit_code);
    }

    // No-vsock mode (EPKG_VM_NO_DAEMON set)
    log::info!("libkrun: starting VM in no-vsock mode (EPKG_VM_NO_DAEMON)...");
    let vm_ctx = create_and_configure_vm(env_root, run_options, &config)?;
    let ctx_id = vm_ctx.ctx.ctx_id;
    log::info!("libkrun: VM configured (ctx_id={})", ctx_id);
    // Channel for signaling VM start failure (unused in no-vsock path)
    let (start_failed_tx, _start_failed_rx) = std::sync::mpsc::channel();
    let vm_thread = start_libkrun_vm(vm_ctx.ctx, start_failed_tx);

    krun_no_vsock_join_vm_thread_exit(vm_thread, ctx_id);
}

/// Setup console output logging to a file for debugging kernel boot.
///
/// Debug logging is off by default, enabled by EPKG_DEBUG_LIBKRUN env var.
/// Creates a per-PID log file and a symlink at "latest-console.log" for easy access.
///
/// Example paths:
/// - Log file: `$HOME/.cache/epkg/vmm-logs/libkrun-console-<pid>.log`
/// - Symlink:  `$HOME/.cache/epkg/vmm-logs/latest-console.log` -> latest log file
///
/// Usage:
/// ```bash
/// # Enable debug logging:
/// export EPKG_DEBUG_LIBKRUN=1
/// # After running a VM, check the console output:
/// less ~/.cache/epkg/vmm-logs/latest-console.log
/// ```
fn setup_console_output(ctx_id: u32) -> Result<()> {
    use std::ffi::CString;

    // Debug logging is off by default, enabled by EPKG_DEBUG_LIBKRUN env var
    if std::env::var_os("EPKG_DEBUG_LIBKRUN").is_none() {
        log::debug!("libkrun: console output disabled (EPKG_DEBUG_LIBKRUN not set)");
        return Ok(());
    }

    let base_log_dir = crate::models::dirs().epkg_cache.join("vmm-logs");
    lfs::create_dir_all(&base_log_dir)
        .map_err(|e| eyre::eyre!("Failed to create VMM log directory: {}", e))?;

    let pid = std::process::id();
    let console_log_path = base_log_dir.join(format!("libkrun-console-{}.log", pid));

    // Create the file before setting up symlink (avoids dead symlink)
    std::fs::File::create(&console_log_path)
        .map_err(|e| eyre::eyre!("Failed to create console log file: {}", e))?;

    // On Windows, convert path to Windows format (backslashes)
    #[cfg(target_os = "windows")]
    let console_log_path_str = {
        // Convert Unix path to Windows path
        let path_str = console_log_path.to_string_lossy().to_string();
        // The path might already be in Windows format from dirs(), but ensure it
        log::debug!("libkrun: console log path (raw): {}", path_str);
        path_str
    };
    #[cfg(not(target_os = "windows"))]
    let console_log_path_str = console_log_path.to_string_lossy().to_string();

    let console_log = CString::new(console_log_path_str.as_bytes())
        .map_err(|e| eyre::eyre!("invalid console log path: {}", e))?;

    log::info!("libkrun: krun_set_console_output path={}", console_log_path_str);
    check_status("krun_set_console_output",
        unsafe { krun_set_console_output(ctx_id, console_log.as_ptr()) }
    )?;
    log::info!("libkrun: console output -> {}", console_log_path.display());

    // Set the kernel console device to redirect serial console output to the log file.
    // On Windows, use "ttyS0" since kernel cmdline has console=ttyS0.
    #[cfg(target_os = "windows")]
    {
        let console_id = CString::new("ttyS0")
            .map_err(|e| eyre::eyre!("invalid console id: {}", e))?;
        check_status("krun_set_kernel_console",
            unsafe { krun_set_kernel_console(ctx_id, console_id.as_ptr()) }
        )?;
        log::debug!("libkrun: kernel console set to ttyS0");
    }
    // On Unix, use "hvc0" since kernel cmdline has console=hvc0.
    #[cfg(not(target_os = "windows"))]
    {
        let console_id = CString::new("hvc0")
            .map_err(|e| eyre::eyre!("invalid console id: {}", e))?;
        check_status("krun_set_kernel_console",
            unsafe { krun_set_kernel_console(ctx_id, console_id.as_ptr()) }
        )?;
        log::debug!("libkrun: kernel console set to hvc0");
    }

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
