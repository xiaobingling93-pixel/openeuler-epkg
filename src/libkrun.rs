use std::ffi::CString;
use std::path::Path;
use std::ptr;
use std::thread;

use color_eyre::eyre;
use color_eyre::Result;

use crate::lfs;
use crate::run::RunOptions;
use crate::qemu;
use crate::vm_client;

#[cfg(all(feature = "libkrun", target_os = "linux"))]
extern crate krun as krun_crate;

// FFI for statically linked libkrun (C API from libkrun crate built as staticlib).
#[cfg(all(feature = "libkrun", target_os = "linux"))]
#[allow(dead_code)]
unsafe extern "C" {
    fn krun_create_ctx() -> i32;
    fn krun_free_ctx(ctx_id: u32) -> i32;
    fn krun_init_log(target_fd: i32, level: u32, style: u32, options: u32) -> i32;
    fn krun_set_vm_config(ctx_id: u32, num_vcpus: u8, ram_mib: u32) -> i32;
    fn krun_set_root(ctx_id: u32, root_path: *const std::ffi::c_char) -> i32;
    fn krun_set_workdir(ctx_id: u32, workdir_path: *const std::ffi::c_char) -> i32;
    fn krun_set_exec(
        ctx_id: u32,
        exec_path: *const std::ffi::c_char,
        argv: *const *const std::ffi::c_char,
        envp: *const *const std::ffi::c_char,
    ) -> i32;
    fn krun_set_env(ctx_id: u32, envp: *const *const std::ffi::c_char) -> i32;
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
    /// Add a vsock port mapping to a Unix socket on the host.
    /// This allows host processes to connect to guest vsock via Unix socket.
    /// listen=true means host initiates connections to guest.
    fn krun_add_vsock_port2(
        ctx_id: u32,
        port: u32,
        c_filepath: *const std::ffi::c_char,
        listen: bool,
    ) -> i32;
}


// Force the staticlib to be linked when we only reference it via extern "C".
#[cfg(all(feature = "libkrun", target_os = "linux"))]
fn ensure_libkrun_linked() {
    // We use libkrun's krun_set_kernel() with format=0 (Raw) which calls map_kernel().
    // This treats the kernel as a bundled raw binary and loads it at guest_addr=0x2000_0000.
    krun_crate::ensure_linked();
}

/// Check if kernel is ELF format by reading magic bytes.
#[cfg(all(feature = "libkrun", target_os = "linux"))]
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
/// Returns: 0=Raw (for x86_64 non-ELF), 1=ELF (vmlinux)
#[cfg(all(feature = "libkrun", target_os = "linux"))]
#[allow(dead_code)]
fn detect_kernel_format_for_libkrun(kernel_path: &str) -> Result<u32> {
    if is_elf_kernel(kernel_path)? {
        Ok(1) // ELF (vmlinux)
    } else {
        // On x86_64, all non-ELF kernels use Raw format (handled by map_kernel)
        Ok(0) // Raw
    }
}

#[cfg(all(feature = "libkrun", target_os = "linux"))]
fn check_status(op: &str, status: i32) -> Result<()> {
    if status < 0 {
        Err(eyre::eyre!("{} failed with status {}", op, status))
    } else {
        Ok(())
    }
}

/// Thin wrapper that owns a libkrun context.
#[cfg(all(feature = "libkrun", target_os = "linux"))]
struct KrunContext {
    ctx_id: u32,
}

#[cfg(all(feature = "libkrun", target_os = "linux"))]
unsafe impl Send for KrunContext {}

#[cfg(all(feature = "libkrun", target_os = "linux"))]
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
    unsafe fn set_exec(
        &self,
        exec: &str,
        args: &[String],
        env: &[(String, String)],
    ) -> Result<()> {
        let exec_c = CString::new(exec)
            .map_err(|e| eyre::eyre!("invalid exec path: {}", e))?;

        let arg_storage: Vec<CString> = args
            .iter()
            .map(|arg| {
                CString::new(arg.as_str()).map_err(|e| {
                    eyre::eyre!("invalid arg {:?}: {}", arg, e)
                })
            })
            .collect::<Result<_>>()?;
        let mut arg_ptrs: Vec<*const std::ffi::c_char> =
            arg_storage.iter().map(|arg| arg.as_ptr()).collect();
        arg_ptrs.push(ptr::null());

        let env_storage = Self::env_to_cstring(env)?;
        let mut env_ptrs: Vec<*const std::ffi::c_char> =
            env_storage.iter().map(|entry| entry.as_ptr()).collect();
        env_ptrs.push(ptr::null());

        check_status(
            "krun_set_exec",
            unsafe {
                krun_set_exec(
                    self.ctx_id,
                    exec_c.as_ptr(),
                    arg_ptrs.as_ptr(),
                    env_ptrs.as_ptr(),
                )
            },
        )
    }

    #[allow(dead_code)]
    unsafe fn set_env(&self, env: &[(String, String)]) -> Result<()> {
        if env.is_empty() {
            let empty: [*const std::ffi::c_char; 1] = [ptr::null()];
            return check_status(
                "krun_set_env",
                unsafe { krun_set_env(self.ctx_id, empty.as_ptr()) },
            );
        }

        let env_storage = Self::env_to_cstring(env)?;
        let mut ptrs: Vec<*const std::ffi::c_char> =
            env_storage.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(ptr::null());

        check_status("krun_set_env", unsafe { krun_set_env(self.ctx_id, ptrs.as_ptr()) })
    }

    fn env_to_cstring(env: &[(String, String)]) -> Result<Vec<CString>> {
        env.iter()
            .map(|(k, v)| {
                let kv = format!("{}={}", k, v);
                CString::new(kv).map_err(|e| eyre::eyre!("invalid env: {}", e))
            })
            .collect()
    }

    #[allow(dead_code)]
    unsafe fn set_workdir(&self, workdir: &str) -> Result<()> {
        let workdir_c = CString::new(workdir)
            .map_err(|e| eyre::eyre!("invalid workdir path: {}", e))?;
        check_status(
            "krun_set_workdir",
            unsafe { krun_set_workdir(self.ctx_id, workdir_c.as_ptr()) },
        )
    }

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
    /// kernel_format: 0 = Raw (e.g. aarch64/riscv64 Image), 1 = Elf (e.g. x86_64 vmlinux)
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
}

#[cfg(all(feature = "libkrun", target_os = "linux"))]
impl Drop for KrunContext {
    fn drop(&mut self) {
        unsafe {
            let _ = krun_free_ctx(self.ctx_id);
        }
    }
}

/// Detect kernel image format for libkrun.
///
/// For x86_64, libkrun supports:
/// - ELF format (1) - loaded via linux_loader::Elf::load()
/// - Raw format (0) - loaded via map_kernel() which treats it as a bundled kernel
///
/// The bundled kernel from libkrunfw (KERNEL_BUNDLE) is a raw binary image.
#[allow(dead_code)]
fn detect_kernel_format(path: &str) -> Result<u32> {
    let mut f = std::fs::File::open(path).map_err(|e| eyre::eyre!("open kernel {}: {}", path, e))?;
    let mut magic = [0u8; 4];
    use std::io::Read;
    f.read_exact(&mut magic).map_err(|e| eyre::eyre!("read kernel {}: {}", path, e))?;
    if magic == [0x7f, b'E', b'L', b'F'] {
        Ok(1) // Elf (vmlinux)
    } else {
        // Raw format - includes bundled kernel from libkrunfw
        // On x86_64, libkrun's map_kernel() handles this by treating it as a bundled kernel
        log::debug!("detected raw/bundled kernel format (magic: {:02x?})", magic);
        Ok(0) // Raw
    }
}

/// Run a command inside a libkrun microVM.
///
/// This function never returns on success; it exits the process with the
/// guest's exit code, similar to the QEMU backend.
///
/// Note: epkg release binaries are built as fully static executables (musl),
/// so ELF RPATH/RUNPATH cannot be used to teach the dynamic loader where
/// `libkrunfw.so.5` lives. Instead we rely on the vendored libkrun crate's
/// support for `LIBKRUNFW_DIR` to point it at the firmware library directory.
#[cfg(all(feature = "libkrun", target_os = "linux"))]
pub fn run_command_in_krun(
    env_root: &Path,
    run_options: &RunOptions,
    guest_cmd_path: &Path,
) -> Result<()> {
    // Determine control plane mode for libkrun
    // Default is vsock mode (vm-daemon over vsock) because libkrun has no virtual network.
    // EPKG_VM_NO_DAEMON=1 forces cmdline mode (kernel command line).
    let use_cmdline_mode = std::env::var("EPKG_VM_NO_DAEMON").is_ok();
    let use_vsock = !use_cmdline_mode;
    log::debug!("libkrun: mode: cmdline={}, vsock={}", use_cmdline_mode, use_vsock);
    log::debug!("libkrun: EPKG_VM_NO_DAEMON={}", std::env::var("EPKG_VM_NO_DAEMON").unwrap_or_else(|_| "not set".to_string()));

    // Unix socket path for vsock communication (used by libkrun)
    let mut vsock_sock_path: Option<std::path::PathBuf> = None;

    let _rootfs = env_root
        .to_str()
        .ok_or_else(|| eyre::eyre!("env_root path is not valid UTF-8"))?;
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
    let exec = guest_exec_path.as_str();

    let mut args: Vec<String> = Vec::new();
    args.push(exec.to_string());
    args.extend(run_options.args.clone());

    // Build command for kernel cmdline (epkg.init_cmd=)
    let (cmd_parts, init_cmd) = qemu::build_guest_command(Path::new(&guest_exec_path), &run_options.args)
        .map_err(|e| eyre::eyre!("Failed to build guest command: {}", e))?;

    // Build kernel cmdline based on working example from libkrun/examples/run_external_kernel.sh.
    // Key parameters:
    // - console=hvc0 earlyprintk=hvc0: console output via virtio-console
    // - loglevel=8 debug: enable debug logging for troubleshooting
    // - rootfstype=virtiofs rw: use virtiofs for root filesystem, read-write
    // - init=/usr/bin/init: use epkg (via symlink) as init process
    // - reboot=k panic=-1 panic_print=0 nomodule no-kvmapf: kernel behavior settings
    //
    // Environment variables are passed as part of cmdline (EPKG_* format).
    // The virtio_mmio.device parameters and tsi_hijack are added automatically by libkrun.
    //
    // We also append any extra args from --kernel-args option.

    let base_cmdline = "reboot=k panic=-1 panic_print=0 nomodule console=hvc0 earlyprintk=hvc0 \
                        loglevel=8 debug rootfstype=virtiofs rw no-kvmapf init=/usr/bin/init";
    let mut kernel_args = String::from(base_cmdline);
    if let Some(ref user_args) = run_options.kernel_args {
        kernel_args.push(' ');
        kernel_args.push_str(user_args);
    };

    // In cmdline mode (EPKG_VM_NO_DAEMON=1), set epkg.init_cmd to tell init what to execute
    if use_cmdline_mode {
        kernel_args.push(' ');
        kernel_args.push_str(&format!("epkg.init_cmd={}", init_cmd));
    }

    // Pass host RUST_LOG into guest so init can enable debug logging
    if let Ok(rust_log) = std::env::var("RUST_LOG") {
        if !rust_log.is_empty() {
            kernel_args.push_str(&format!(" epkg.rust_log={}", qemu::percent_encode(&rust_log)));
        }
    }

    // Resolve kernel path and set it via krun_set_kernel() if explicitly specified.
    // If no --kernel option is provided, libkrun will auto-load the bundled kernel
    // from libkrunfw (this is how chroot_vm.c works).
    let kernel_path = run_options.kernel.clone();
    let kernel_format = if let Some(ref kernel) = kernel_path {
        Some(detect_kernel_format_for_libkrun(kernel)?)
    } else {
        None
    };
    if let Some(ref kernel) = kernel_path {
        log::debug!("libkrun: kernel path: {}", kernel);
        log::debug!("libkrun: kernel format: {:?}", kernel_format);
    } else {
        log::debug!("libkrun: using bundled kernel from libkrunfw (no --kernel specified)");
    }

    ensure_libkrun_linked();

    // Note: We don't call krun_init_log() since the application already has
    // logging initialized via env_logger. libkrun's internal debug logs are
    // not critical - kernel boot output goes through virtio-console (hvc0).

    // Create libkrun context and configure VM (will be moved to thread)
    let ctx = unsafe { KrunContext::create()? };
    let cpus = crate::run::resolve_vm_cpus(run_options);
    let requested_mib = crate::run::resolve_vm_memory_mib(run_options);
    log::debug!("libkrun: run_options.vm_memory_mib = {:?}", run_options.vm_memory_mib);
    // For bundled kernel (no --kernel), use default memory; for external kernel, round up based on kernel size
    let memory_mib = if let Some(ref kernel) = kernel_path {
        crate::run::round_up_vm_memory_for_libkrun(requested_mib, kernel)
    } else {
        requested_mib
    };
    log::debug!("libkrun: requested_mib = {}", requested_mib);
    log::debug!("libkrun: round_up_vm_memory_for_libkrun = {}", memory_mib);
    log::debug!("libkrun: kernel cmdline: {}", kernel_args);
    unsafe {
        ctx.set_vm_config(cpus, memory_mib)?;

        // Set kernel via krun_set_kernel() only if explicitly specified with --kernel.
        // If no kernel is specified, libkrun auto-loads the bundled kernel from libkrunfw.
        let initrd_path = run_options.initrd.clone();
        if let Some(ref kernel) = kernel_path {
            if let Some(format) = kernel_format {
                let format_str = match format {
                    0 => "Raw (bundled)",
                    1 => "ELF (vmlinux)",
                    _ => "Unknown",
                };
                if let Some(ref initrd) = initrd_path {
                    log::debug!("libkrun: using initrd: {}", initrd);
                } else {
                    log::debug!("libkrun: no initrd provided");
                }
                ctx.set_kernel(kernel, format, Some(&kernel_args), initrd_path.as_deref())?;
                log::debug!("libkrun: kernel set via krun_set_kernel() with format={} ({})", format, format_str);
            }
        }

        // Configure rootfs via virtiofs - this mounts env_root as the root filesystem
        // The kernel cmdline has rootfstype=virtiofs which tells the kernel to mount it
        ctx.set_root(env_root.to_str().unwrap())?;
        log::debug!("libkrun: rootfs configured via virtiofs: {:?}", env_root);

        // Set environment variables for the guest
        // ctx.set_env(&env_vec)?;

        // Set workdir to root
        // ctx.set_workdir("/")?;

        // In vsock mode, let init handle command execution via epkg.init_cmd
        // set_exec would override init, so skip it entirely

        // Configure split IRQ chip (required for x86_64 KVM)
        check_status("krun_split_irqchip",
            krun_split_irqchip(ctx.ctx_id, true)
        )?;
        log::debug!("libkrun: split IRQ chip configured");

        // Setup console output to a log file.
        // Note: We intentionally do NOT call krun_disable_implicit_console() because:
        // 1. libkrun only writes to console_output file when implicit console is enabled
        // 2. When implicit console is disabled, output goes to Rust log framework instead
        // By keeping implicit console enabled with a file output, we get console output
        // in the log file without polluting host stderr.
        setup_console_output(ctx.ctx_id)?;

        // Configure vsock device for vsock mode
        if use_vsock {
            // Disable implicit vsock (created by libkrun by default)
            check_status("krun_disable_implicit_vsock",
                krun_disable_implicit_vsock(ctx.ctx_id)
            )?;
            // Add explicit vsock without TSI features (we handle command execution ourselves)
            check_status("krun_add_vsock",
                krun_add_vsock(ctx.ctx_id, 0)  // 0 = no TSI hijacking
            )?;

            // Create Unix socket path for host-guest vsock communication
            let sock_path = crate::models::dirs().epkg_cache
                .join("vmm-logs")
                .join(format!("vsock-{}.sock", std::process::id()));
            lfs::create_dir_all(sock_path.parent().unwrap())?;

            // Remove stale socket file if exists
            let _ = std::fs::remove_file(&sock_path);

            // Map guest vsock port 10000 to Unix socket on host
            // listen=true means host initiates connections to guest
            let sock_path_c = CString::new(sock_path.to_string_lossy().as_bytes())
                .map_err(|e| eyre::eyre!("invalid socket path: {}", e))?;
            check_status("krun_add_vsock_port2",
                krun_add_vsock_port2(ctx.ctx_id, 10000, sock_path_c.as_ptr(), true)
            )?;
            log::debug!("libkrun: vsock port 10000 mapped to Unix socket {}", sock_path.display());

            // Store socket path for vm_client to use
            vsock_sock_path = Some(sock_path);
        }

        // Note: libkrun creates an implicit virtio-console (hvc0) by default.
        // With krun_set_console_output() configured, the console output goes to
        // the specified log file instead of host stderr. The kernel cmdline has
        // console=hvc0 which routes kernel boot messages to this virtio-console.
    }

    // Start VM in a separate thread (krun_start_enter blocks until VM exits)
    log::debug!("libkrun: starting VM thread...");
    let vm_thread = thread::spawn(move || {
        unsafe {
            let status = ctx.start_enter();
            if status < 0 {
                log::error!("krun_start_enter failed with status {}", status);
                status
            } else {
                log::debug!("libkrun: krun_start_enter returned status {}", status);
                status
            }
        }
    });

    // For vsock mode, connect to guest vsock server and send command
    if use_vsock {
        log::debug!("libkrun: connecting to guest via vsock...");
        log::debug!("libkrun: attempting vsock connection...");
        // cmd_parts was computed earlier
        let exit_code = vm_client::send_command_via_vsock(
            &cmd_parts,
            run_options.use_pty,
            10000,
            vsock_sock_path.as_deref(),
        )
        .map_err(|e| eyre::eyre!("Failed to send command via vsock: {}", e))?;
        log::debug!("libkrun: vsock command completed with exit code {}", exit_code);
        // VM will power off after command execution
    }

    // Wait for VM thread to finish (VM will run command and exit)
    log::debug!("libkrun: waiting for VM thread to finish...");
    match vm_thread.join() {
        Ok(exit_status) => {
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
            std::process::exit(1);
        }
    }
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
    use std::os::unix::fs::symlink;

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

    // Create/update symlink for easy access to latest log
    let latest_log_symlink = base_log_dir.join("latest-console.log");
    let _ = std::fs::remove_file(&latest_log_symlink); // ignore error if not exists
    if let Err(e) = symlink(&console_log_path, &latest_log_symlink) {
        log::warn!("libkrun: failed to create symlink: {}", e);
    }

    Ok(())
}

