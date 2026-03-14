use std::ffi::CString;
use std::io::{Read, Write};
use std::os::unix::io::{AsRawFd, FromRawFd, IntoRawFd};
use std::path::Path;
use std::ptr;
use std::thread;

use color_eyre::eyre;
use color_eyre::Result;

use crate::lfs;
use crate::run::RunOptions;

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
    /// Add a vsock port mapping to a Unix socket on the host.
    /// This allows host processes to connect to guest vsock via Unix socket.
    /// listen=true means host initiates connections to guest.
    fn krun_add_vsock_port2(
        ctx_id: u32,
        port: u32,
        c_filepath: *const std::ffi::c_char,
        listen: bool,
    ) -> i32;
    /// Get the eventfd for triggering VM shutdown from host.
    /// Writing 1u64 to this fd will cause the VM to exit gracefully.
    fn krun_get_shutdown_eventfd(ctx_id: u32) -> i32;
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
/// Returns: 1=ELF (vmlinux), error for non-ELF
#[cfg(feature = "libkrun")]
#[allow(dead_code)]
fn detect_kernel_format_for_libkrun(kernel_path: &str) -> Result<u32> {
    if is_elf_kernel(kernel_path)? {
        Ok(1) // ELF (vmlinux)
    } else {
        Err(eyre::eyre!("Non-ELF kernel format not supported: {}", kernel_path))
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
    use_vsock: bool,
    cmd_parts: Vec<String>,
    kernel_args: String,
    kernel_path: Option<String>,
    kernel_format: Option<u32>,
}

#[cfg(feature = "libkrun")]
fn build_libkrun_config(
    env_root: &Path,
    run_options: &RunOptions,
    guest_cmd_path: &Path,
) -> Result<LibkrunConfig> {
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

    let base_cmdline = "reboot=k panic=-1 panic_print=0 nomodule console=hvc0 earlyprintk=hvc0 \
                        loglevel=8 debug rootfstype=virtiofs rw no-kvmapf init=/usr/bin/init";
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

    if let Ok(pwd) = std::env::var("PWD") {
        if !pwd.is_empty() && pwd != "/" {
            kernel_args.push_str(&format!(" epkg.init_pwd={}", percent_encode(&pwd)));
        }
    }

    let kernel_path = if run_options.kernel.is_some() {
        run_options.kernel.clone()
    } else {
        // Fall back to default kernel path (envs/self/boot/vmlinux from `epkg self install`)
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
    })
}

#[cfg(feature = "libkrun")]
struct VmContext {
    ctx: KrunContext,
    shutdown_fd: i32,
    vsock_sock_path: Option<std::path::PathBuf>,
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

        check_status("krun_split_irqchip",
            krun_split_irqchip(ctx.ctx_id, true)
        )?;
        log::debug!("libkrun: split IRQ chip configured");

        setup_console_output(ctx.ctx_id)?;

        if config.use_vsock {
            check_status("krun_disable_implicit_vsock",
                krun_disable_implicit_vsock(ctx.ctx_id)
            )?;
            check_status("krun_add_vsock",
                krun_add_vsock(ctx.ctx_id, 0)
            )?;

            let sock_path = crate::models::dirs().epkg_cache
                .join("vmm-logs")
                .join(format!("vsock-{}.sock", std::process::id()));
            lfs::create_dir_all(sock_path.parent().unwrap())?;
            let _ = std::fs::remove_file(&sock_path);

            let sock_path_c = CString::new(sock_path.to_string_lossy().as_bytes())
                .map_err(|e| eyre::eyre!("invalid socket path: {}", e))?;
            check_status("krun_add_vsock_port2",
                krun_add_vsock_port2(ctx.ctx_id, 10000, sock_path_c.as_ptr(), true)
            )?;
            log::debug!("libkrun: vsock port 10000 mapped to Unix socket {}", sock_path.display());

            let ready_path = crate::models::dirs().epkg_cache
                .join("vmm-logs")
                .join(format!("ready-{}.sock", std::process::id()));
            let _ = std::fs::remove_file(&ready_path);
            let ready_path_c = CString::new(ready_path.to_string_lossy().as_bytes())
                .map_err(|e| eyre::eyre!("invalid ready socket path: {}", e))?;
            check_status("krun_add_vsock_port2",
                krun_add_vsock_port2(ctx.ctx_id, 10001, ready_path_c.as_ptr(), false)
            )?;
            log::debug!("libkrun: ready port 10001 mapped to Unix socket {}", ready_path.display());

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
fn setup_vsock_ready_listener() -> Result<Option<std::os::unix::net::UnixListener>> {
    let vmm_logs_dir = crate::models::dirs().epkg_cache.join("vmm-logs");
    if let Ok(entries) = std::fs::read_dir(&vmm_logs_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("vsock-") && name.ends_with(".sock") {
                let _ = std::fs::remove_file(entry.path());
                log::trace!("libkrun: cleaned up stale socket {}", name);
            }
            if name.starts_with("ready-") && name.ends_with(".sock") {
                let _ = std::fs::remove_file(entry.path());
                log::trace!("libkrun: cleaned up stale socket {}", name);
            }
        }
    }

    let _pid = std::process::id();
    let _sock_path = vmm_logs_dir.join(format!("vsock-{}.sock", _pid));
    let ready_path = vmm_logs_dir.join(format!("ready-{}.sock", _pid));
    let _ = std::fs::remove_file(&ready_path);

    log::debug!("libkrun: creating ready listener on {}", ready_path.display());
    let listener = std::os::unix::net::UnixListener::bind(&ready_path)
        .map_err(|e| eyre::eyre!("Failed to bind ready socket {}: {}", ready_path.display(), e))?;

    listener.set_nonblocking(true)
        .map_err(|e| eyre::eyre!("Failed to set non-blocking on ready socket: {}", e))?;

    Ok(Some(listener))
}

#[cfg(feature = "libkrun")]
fn start_libkrun_vm(ctx: KrunContext) -> std::thread::JoinHandle<i32> {
    thread::spawn(move || {
        unsafe {
            let status = ctx.start_enter();
            if status < 0 {
                log::error!("krun_start_enter failed with status {}", status);
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
    /// kernel_format: 1 = ELF (vmlinux from sandbox-kernel)
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

/// Run a command inside a libkrun microVM.
///
/// This function never returns on success; it exits the process with the
/// guest's exit code, similar to the QEMU backend.
///
/// The kernel is provided by sandbox-kernel as an ELF vmlinux file.
#[cfg(feature = "libkrun")]
pub fn run_command_in_krun(
    env_root: &Path,
    run_options: &RunOptions,
    guest_cmd_path: &Path,
) -> Result<()> {
    let config = build_libkrun_config(env_root, run_options, guest_cmd_path)?;
    let vm_ctx = create_and_configure_vm(env_root, run_options, &config)?;

    let ready_listener = if config.use_vsock {
        setup_vsock_ready_listener()?
    } else {
        None
    };

    log::debug!("libkrun: starting VM thread...");
    // Save ctx_id before moving ctx to thread, so we can free it later
    let ctx_id = vm_ctx.ctx.ctx_id;
    let vm_thread = start_libkrun_vm(vm_ctx.ctx);

    if config.use_vsock {
        log::debug!("libkrun: waiting for guest to be ready (with timeout)...");
        let listener = ready_listener.unwrap();
        let listener_fd = listener.as_raw_fd();
        let mut poll_fds = [libc::pollfd {
            fd:      listener_fd,
            events:  libc::POLLIN,
            revents: 0,
        }];

        const READY_TIMEOUT_MS: i32 = 30_000;
        let poll_result = unsafe { libc::poll(poll_fds.as_mut_ptr(), 1, READY_TIMEOUT_MS) };

        match poll_result {
            0 => {
                log::error!("libkrun: timeout waiting for VM to become ready");
                return Err(eyre::eyre!("Timeout waiting for VM to start"));
            }
            n if n < 0 => {
                log::error!("libkrun: poll error on ready socket");
                return Err(eyre::eyre!("Poll error on ready socket"));
            }
            _ => {
                let (stream, _addr) = listener.accept()
                    .map_err(|e| eyre::eyre!("Failed to accept on ready socket: {}", e))?;
                log::debug!("libkrun: guest connected to ready socket, guest is ready!");
                drop(stream);
            }
        }

        let exit_code = send_command_via_unix_socket(
            &config.cmd_parts,
            run_options.use_pty,
            vm_ctx.vsock_sock_path.as_deref().unwrap(),
        )
        .map_err(|e| eyre::eyre!("Failed to send command via Unix socket: {}", e))?;
        log::debug!("libkrun: vsock command completed with exit code {}", exit_code);

        log::debug!("libkrun: triggering VM shutdown via eventfd...");
        let buf = 1u64.to_le_bytes();
        let write_result = unsafe { libc::write(vm_ctx.shutdown_fd, buf.as_ptr() as *const _, buf.len()) };
        if write_result < 0 {
            log::warn!("libkrun: failed to write shutdown eventfd: {}", std::io::Error::last_os_error());
        }

        match vm_thread.join() {
            Ok(vm_status) => {
                log::debug!("libkrun: VM thread finished with status {}", vm_status);
            }
            Err(e) => {
                log::error!("libkrun: VM thread join failed: {:?}", e);
            }
        }

        // Explicitly free the context before exit to release KVM resources.
        // std::process::exit() does NOT call Drop on local variables, so we must
        // manually clean up to avoid resource leaks that cause "Out of memory" errors
        // in subsequent runs.
        log::debug!("libkrun: freeing context before exit...");
        unsafe {
            let _ = krun_free_ctx(ctx_id);
        }

        log::debug!("libkrun: exiting with code {}", exit_code);
        std::process::exit(exit_code);
    }

    log::debug!("libkrun: waiting for VM thread to finish...");
    match vm_thread.join() {
        Ok(exit_status) => {
            // Explicitly free the context before exit to release KVM resources.
            // std::process::exit() does NOT call Drop on local variables.
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

// ============================================================================
// Cross-platform helper functions (shared with qemu.rs)
// ============================================================================

/// Build guest command string and percent-encode it for kernel command line.
/// Returns (cmd_parts for vsock, init_cmd for kernel cmdline).
fn build_guest_command(cmd_path: &Path, args: &[String]) -> Result<(Vec<String>, String)> {
    let mut cmd_parts: Vec<String> = Vec::new();
    cmd_parts.push(cmd_path.to_string_lossy().to_string());
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

// ============================================================================
// Unix socket vsock emulation (for libkrun on macOS/Linux)
// ============================================================================

use std::io::BufRead;
use std::time::Duration;
use std::sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex};
use console::Term;
use lazy_static::lazy_static;
use nix::sys::signal::{signal, Signal, SigHandler};
use nix::sys::termios;
use serde::{Deserialize, Serialize};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;

lazy_static! {
    static ref RESIZE_PENDING: AtomicBool = AtomicBool::new(false);
}

extern "C" fn handle_sigwinch(_: i32) {
    RESIZE_PENDING.store(true, Ordering::SeqCst);
}

/// Streaming message types for interactive/TUI modes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
enum StreamMessage {
    #[serde(rename = "stdin")]
    Stdin { data: String, seq: u64 },
    #[serde(rename = "stdout")]
    Stdout { data: String, seq: u64 },
    #[serde(rename = "stderr")]
    Stderr { data: String, seq: u64 },
    #[serde(rename = "resize")]
    Resize { cols: u16, rows: u16 },
    #[serde(rename = "exit")]
    Exit { code: i32 },
    #[serde(rename = "signal")]
    Signal { sig: i32 },
    #[serde(rename = "error")]
    Error { message: String },
}

/// Build command request for vm-daemon.
fn build_command_request(cmd_parts: &[String], use_pty: bool) -> serde_json::Value {
    serde_json::json!({
        "type": "command",
        "command": cmd_parts,
        "pty": use_pty,
    })
}

/// Connect to Unix socket with retry logic (for libkrun vsock emulation).
fn connect_unix_socket_with_retry(sock_path: &Path, max_retries: u32) -> Result<std::net::TcpStream> {
    let mut retry_count = 0;
    let mut last_error = None;
    while retry_count < max_retries {
        match std::os::unix::net::UnixStream::connect(sock_path) {
            Ok(unix_stream) => {
                let raw_fd = unix_stream.into_raw_fd();
                // SAFETY: raw_fd is a valid, connected Unix stream socket
                let stream = unsafe { std::net::TcpStream::from_raw_fd(raw_fd) };
                return Ok(stream);
            }
            Err(e) => {
                last_error = Some(e);
                retry_count += 1;
                if retry_count >= max_retries {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
    Err(eyre::eyre!(
        "Failed to connect to Unix socket {} after {} retries: {}",
        sock_path.display(),
        max_retries,
        last_error.unwrap_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "connection failed"))
    ))
}

/// Resolve use_pty option, auto-detecting if None.
fn resolve_use_pty(use_pty: Option<bool>) -> bool {
    use std::io::IsTerminal;
    use_pty.unwrap_or_else(|| std::io::stdin().is_terminal())
}

/// Send command via Unix socket (libkrun vsock emulation).
fn send_command_via_unix_socket(
    cmd_parts: &[String],
    use_pty: Option<bool>,
    sock_path: &Path,
) -> Result<i32> {
    let should_use_pty = resolve_use_pty(use_pty);
    log::debug!("libkrun: use_pty={:?}, should_use_pty={}", use_pty, should_use_pty);

    let mut stream = connect_unix_socket_with_retry(sock_path, 30)?;
    log::debug!("libkrun: Unix socket connected, sending command {:?}", cmd_parts);

    let request = build_command_request(cmd_parts, should_use_pty);
    let request_json = serde_json::to_vec(&request)?;
    stream.write_all(&request_json)?;
    stream.write_all(b"\n")?;
    log::debug!("libkrun: request sent ({} bytes)", request_json.len());

    handle_streaming(&mut stream, should_use_pty)
}

/// Handle streaming I/O for PTY mode.
fn handle_streaming(stream: &mut std::net::TcpStream, use_pty: bool) -> Result<i32> {
    if !use_pty {
        // Simple mode: just read response
        let mut response = String::new();
        stream.read_to_string(&mut response)?;
        let msg: StreamMessage = serde_json::from_str(&response)
            .unwrap_or_else(|_| StreamMessage::Exit { code: 0 });
        match msg {
            StreamMessage::Exit { code } => return Ok(code),
            StreamMessage::Error { message } => return Err(eyre::eyre!("VM error: {}", message)),
            _ => return Ok(0),
        }
    }

    // PTY mode: streaming I/O
    let term = Term::stdout();
    let original_mode = termios::tcgetattr(std::io::stdin())
        .ok();

    // Set raw mode
    if let Some(ref orig) = original_mode {
        let mut raw = orig.clone();
        termios::cfmakeraw(&mut raw);
        let _ = termios::tcsetattr(std::io::stdin(), termios::SetArg::TCSANOW, &raw);
    }

    // Setup signal handlers
    unsafe {
        let _ = signal(Signal::SIGWINCH, SigHandler::Handler(handle_sigwinch));
        let _ = signal(Signal::SIGINT, SigHandler::SigIgn);
        let _ = signal(Signal::SIGTERM, SigHandler::SigIgn);
    }

    let stdin_fd = std::io::stdin().as_raw_fd();
    let stream_clone = stream.try_clone()?;
    let exit_code = Arc::new(Mutex::new(None));
    let exit_code_clone = exit_code.clone();

    // Reader thread
    let reader = thread::spawn(move || {
        let mut reader = std::io::BufReader::new(&stream_clone);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if let Ok(msg) = serde_json::from_str::<StreamMessage>(&line) {
                        match msg {
                            StreamMessage::Stdout { data, .. } => {
                                if let Ok(decoded) = STANDARD.decode(&data) {
                                    let _ = std::io::stdout().write_all(&decoded);
                                    let _ = std::io::stdout().flush();
                                }
                            }
                            StreamMessage::Stderr { data, .. } => {
                                if let Ok(decoded) = STANDARD.decode(&data) {
                                    let _ = std::io::stderr().write_all(&decoded);
                                    let _ = std::io::stderr().flush();
                                }
                            }
                            StreamMessage::Exit { code } => {
                                *exit_code_clone.lock().unwrap() = Some(code);
                                break;
                            }
                            _ => {}
                        }
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Main loop: read stdin and forward to VM
    let mut seq: u64 = 0;
    let mut buf = [0u8; 4096];
    loop {
        // Check for exit
        if exit_code.lock().unwrap().is_some() {
            break;
        }

        // Check for resize
        if RESIZE_PENDING.swap(false, Ordering::SeqCst) {
            let (cols, rows) = term.size();
            let resize_msg = StreamMessage::Resize { cols, rows };
            if let Ok(json) = serde_json::to_string(&resize_msg) {
                let _ = stream.write_all(json.as_bytes());
                let _ = stream.write_all(b"\n");
            }
        }

        // Read stdin with timeout
        let mut pfd = [libc::pollfd {
            fd: stdin_fd,
            events: libc::POLLIN,
            revents: 0,
        }];
        let ready = unsafe { libc::poll(pfd.as_mut_ptr(), 1, 50) };
        if ready > 0 && (pfd[0].revents & libc::POLLIN) != 0 {
            match std::io::stdin().read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let data = STANDARD.encode(&buf[..n]);
                    let msg = StreamMessage::Stdin { data, seq };
                    seq += 1;
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = stream.write_all(json.as_bytes());
                        let _ = stream.write_all(b"\n");
                    }
                }
                Err(_) => break,
            }
        }
    }

    reader.join().ok();

    // Restore terminal
    if let Some(orig) = original_mode {
        let _ = termios::tcsetattr(std::io::stdin(), termios::SetArg::TCSANOW, &orig);
    }

    let code = exit_code.lock().unwrap().unwrap_or(0);
    Ok(code)
}

