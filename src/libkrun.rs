use std::env;
use std::ffi::CString;
use std::path::Path;
use std::ptr;
use std::thread;

use color_eyre::eyre;
use color_eyre::Result;

use crate::run::RunOptions;
use crate::qemu;
use crate::vm_client;
use crate::lfs;

#[cfg(all(feature = "libkrun", target_os = "linux"))]
extern crate krun as krun_crate;

// FFI for statically linked libkrun (C API from libkrun crate built as staticlib).
#[cfg(all(feature = "libkrun", target_os = "linux"))]
unsafe extern "C" {
    fn krun_create_ctx() -> i32;
    fn krun_free_ctx(ctx_id: u32) -> i32;
    #[allow(dead_code)]
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
    fn krun_start_enter(ctx_id: u32) -> i32;
    fn krun_disable_implicit_vsock(ctx_id: u32) -> i32;
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
    /// Mount an additional directory via virtiofs into the guest.
    /// tag: the filesystem tag (e.g., "self")
    /// path: the host directory path to mount
    unsafe fn krun_add_virtiofs(
        ctx_id: u32,
        c_tag: *const std::ffi::c_char,
        c_path: *const std::ffi::c_char,
    ) -> i32;
    /// Set kernel bundle directly without using libkrunfw dlopen.
    /// This allows statically-linked binaries to mmap the kernel themselves
    /// and pass the bundle parameters (host_addr, guest_addr, entry_addr, size) directly.
    fn krun_set_kernel_bundle(
        ctx_id: u32,
        host_addr: u64,
        guest_addr: u64,
        entry_addr: u64,
        size: usize,
    ) -> i32;
}


// Force the staticlib to be linked when we only reference it via extern "C".
#[cfg(all(feature = "libkrun", target_os = "linux"))]
fn ensure_libkrun_linked() {
    // Note: We do NOT use libkrunfw's dlopen-based kernel loading.
    // Instead, we mmap the pre-extracted kernel directly via krun_set_kernel_bundle().
    // This allows statically-linked epkg binaries (which cannot use dlopen) to work.
    //
    // Architecture rationale: We load the kernel ourselves to allow end users to
    // flexibly change the kernel by replacing the extracted kernel file, without
    // being tied to libkrunfw's bundled kernel.
    krun_crate::ensure_linked();
}

/// Parse the entry point from an x86_64 bzImage header.
///
/// The bzImage header format:
/// - Header starts at offset 0x1F1
/// - Field 'code32_start' (32-bit entry point) is at offset 0x214 from header start
///   which is 0x1F1 + 0x214 = 0x405 from file start
///
/// Returns None if the image doesn't have a valid bzImage header.
#[cfg(all(feature = "libkrun", target_os = "linux"))]
fn parse_bzimage_entry_point(data: &[u8]) -> Option<u64> {
    // Check for bzImage signature "HdrS" at offset 0x202 (0x1F1 + 0x11)
    const HDR_SIG_OFFSET: usize = 0x202;
    const HDR_SIG: &[u8] = b"HdrS";

    if data.len() < HDR_SIG_OFFSET + HDR_SIG.len() {
        return None;
    }

    if &data[HDR_SIG_OFFSET..HDR_SIG_OFFSET + HDR_SIG.len()] != HDR_SIG {
        return None;
    }

    // Check header version (must be >= 2.03 for bzImage)
    const VERSION_OFFSET: usize = 0x206;
    if data.len() < VERSION_OFFSET + 2 {
        return None;
    }
    let version = u16::from_le_bytes([data[VERSION_OFFSET], data[VERSION_OFFSET + 1]]);
    if version < 0x0203 {
        return None;
    }

    // Read code32_start (32-bit entry point) at offset 0x405
    const CODE32_START_OFFSET: usize = 0x405;
    if data.len() < CODE32_START_OFFSET + 4 {
        return None;
    }
    let entry = u32::from_le_bytes([
        data[CODE32_START_OFFSET],
        data[CODE32_START_OFFSET + 1],
        data[CODE32_START_OFFSET + 2],
        data[CODE32_START_OFFSET + 3],
    ]);

    // If code32_start is 0, use the default load address
    if entry == 0 {
        Some(0x1000000)
    } else {
        Some(entry as u64)
    }
}

/// Load kernel via mmap and set kernel bundle directly, bypassing libkrunfw dlopen.
///
/// This is required for statically-linked epkg binaries which cannot use dlopen().
/// The kernel should be pre-extracted by `epkg self install` to envs/self/boot/kernel.
#[cfg(all(feature = "libkrun", target_os = "linux"))]
fn load_kernel_bundle(kernel_path: &str) -> Result<(u64, u64, u64, usize)> {
    use std::fs::File;
    use std::io::Read;

    // Read kernel file
    let mut file = File::open(kernel_path)
        .map_err(|e| eyre::eyre!("Failed to open kernel {}: {}", kernel_path, e))?;
    let mut kernel_data = Vec::new();
    file.read_to_end(&mut kernel_data)
        .map_err(|e| eyre::eyre!("Failed to read kernel {}: {}", kernel_path, e))?;

    let kernel_size = kernel_data.len();
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    let rounded_size = (kernel_size + page_size - 1) & !(page_size - 1);

    // Allocate anonymous memory with read/write permissions
    let kernel_host_addr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            rounded_size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0_i64,
        )
    };
    if std::ptr::eq(kernel_host_addr, libc::MAP_FAILED) {
        return Err(eyre::eyre!("Failed to mmap memory for kernel"));
    }

    // Copy kernel data to mapped memory
    unsafe {
        std::ptr::copy_nonoverlapping(
            kernel_data.as_ptr(),
            kernel_host_addr as *mut u8,
            kernel_data.len(),
        );
    }

    // Zero-fill remaining bytes (already zero due to anonymous mapping)
    // Change protection to read-only
    if unsafe { libc::mprotect(kernel_host_addr, rounded_size, libc::PROT_READ) } != 0 {
        unsafe { libc::munmap(kernel_host_addr, rounded_size) };
        return Err(eyre::eyre!("Failed to set kernel memory read-only"));
    }

    // Parse entry point from bzImage header
    let (guest_addr, entry_addr) = parse_bzimage_entry_point(&kernel_data)
        .map(|entry| (0x1000000u64, entry))
        .unwrap_or_else(|| {
            // Check if this is a libkrunfw-style raw kernel bundle
            // libkrunfw's get_kernel returns: guest_addr=0x1000000, entry_addr=0x1000123, size=0x1230000
            // However, libkrun's map_kernel() uses guest_addr=0x2000_0000, entry_addr=0x2000_0000
            // for raw kernels in non-TEE mode. We follow the map_kernel convention.
            if kernel_data.len() >= 0x1230000 {
                log::debug!("detected libkrunfw-style raw kernel bundle, using map_kernel convention");
                (0x2000_0000u64, 0x2000_0000u64)
            } else {
                log::debug!("using default entry point 0x2000_0000 for unknown kernel format");
                (0x2000_0000u64, 0x2000_0000u64)
            }
        });

    log::debug!(
        "kernel bundle: host_addr={:#x}, guest_addr={:#x}, entry_addr={:#x}, size={}",
        kernel_host_addr as u64,
        guest_addr,
        entry_addr,
        rounded_size
    );

    Ok((kernel_host_addr as u64, guest_addr, entry_addr, rounded_size))
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

    unsafe fn set_workdir(&self, workdir: &str) -> Result<()> {
        let workdir_c = CString::new(workdir)
            .map_err(|e| eyre::eyre!("invalid workdir path: {}", e))?;
        check_status(
            "krun_set_workdir",
            unsafe { krun_set_workdir(self.ctx_id, workdir_c.as_ptr()) },
        )
    }

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

    /// kernel_format: 0 = Raw (e.g. aarch64/riscv64 Image), 1 = Elf (e.g. x86_64 vmlinux)
    /// kernel_cmdline: optional extra kernel command line (e.g. from --kernel-args)
    unsafe fn set_kernel(
        &self,
        kernel_path: &str,
        kernel_format: u32,
        kernel_cmdline: Option<&str>,
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
        check_status(
            "krun_set_kernel",
            unsafe {
                krun_set_kernel(
                    self.ctx_id,
                    kernel_c.as_ptr(),
                    kernel_format,
                    ptr::null(), // no initramfs
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

    let rootfs = env_root
        .to_str()
        .ok_or_else(|| eyre::eyre!("env_root path is not valid UTF-8"))?;
    // Convert host absolute path to guest path relative to environment root
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

    // Start with default kernel cmdline (console=hvc0, root=/dev/root rootfstype=virtiofs, etc.)
    let mut kernel_args = String::from("reboot=k panic=0 panic_print=0 nomodule console=hvc0 console=ttyS0 root=/dev/root rootfstype=virtiofs rw loglevel=8 ignore_loglevel debug no-kvmapf init=/usr/bin/init");

    // Append user-provided kernel args
    if let Some(user_args) = &run_options.kernel_args {
        if !user_args.trim().is_empty() {
            kernel_args.push(' ');
            kernel_args.push_str(user_args.trim());
        }
    }

    // Add earlyprintk for debugging
    kernel_args.push(' ');
    kernel_args.push_str("earlyprintk=virtio earlyprintk=ttyS0");
    // Add debug flag for init
    kernel_args.push(' ');
    kernel_args.push_str("epkg.debug=1");


    // Add command to kernel cmdline for both cmdline and vsock modes
    if use_cmdline_mode || use_vsock {
        kernel_args.push(' ');
        kernel_args.push_str(&format!("epkg.init_cmd={}", init_cmd));
    }

    // Pass host RUST_LOG into guest
    if let Ok(rust_log) = std::env::var("RUST_LOG") {
        if !rust_log.is_empty() {
            kernel_args.push(' ');
            kernel_args.push_str(&format!("epkg.rust_log={}", qemu::percent_encode(&rust_log)));
        }
    }

    // Build environment variables for guest
    let mut env_vec: Vec<(String, String)> = env::vars().collect();

    // In vsock mode, add EPKG_INIT_CMD to tell the guest what command to run
    if use_vsock && !init_cmd.is_empty() {
        log::debug!("libkrun: setting EPKG_INIT_CMD={}", init_cmd);
        env_vec.push(("EPKG_INIT_CMD".to_string(), init_cmd.clone()));
    }

    // Resolve kernel path and load it via mmap, bypassing libkrunfw dlopen.
    // This is required because statically-linked epkg cannot use dlopen().
    // The kernel should be pre-extracted by `epkg self install` to envs/self/boot/kernel.
    let kernel_path = crate::run::resolve_vm_kernel_path(run_options)?;
    log::debug!("libkrun: resolving kernel path: {}", kernel_path);

    // Load kernel via mmap and get bundle parameters (host_addr, guest_addr, entry_addr, size)
    let (kernel_host_addr, kernel_guest_addr, kernel_entry_addr, kernel_size) =
        load_kernel_bundle(&kernel_path)?;
    log::info!("libkrun: kernel loaded: {} bytes, entry at {:#x}", kernel_size, kernel_entry_addr);

    ensure_libkrun_linked();

    // Create console output log file for debugging
    let console_log_path = {
        let base_log_dir = crate::models::dirs().epkg_cache.join("vmm-logs");
        lfs::create_dir_all(&base_log_dir)
            .map_err(|e| eyre::eyre!("Failed to create VMM log directory: {}", e))?;
        let pid = std::process::id();
        base_log_dir.join(format!("libkrun-console-{}.log", pid))
    };
    log::debug!("libkrun: console output log: {}", console_log_path.display());

    // Create libkrun context and configure VM (will be moved to thread)
    let ctx = unsafe { KrunContext::create()? };
    let cpus = crate::run::resolve_vm_cpus(run_options);
    let requested_mib = crate::run::resolve_vm_memory_mib(run_options);
    log::debug!("libkrun: run_options.vm_memory_mib = {:?}", run_options.vm_memory_mib);
    let memory_mib = crate::run::round_up_vm_memory_for_libkrun(requested_mib, &kernel_path);
    log::debug!("libkrun: requested_mib = {}", requested_mib);
    log::debug!("libkrun: round_up_vm_memory_for_libkrun = {}", memory_mib);
    log::debug!("libkrun: kernel cmdline: {}", kernel_args);
    unsafe {
        ctx.set_vm_config(cpus, memory_mib)?;

        // Set console output to log file for debugging
        let console_log_c = CString::new(console_log_path.to_string_lossy().to_string())
            .map_err(|e| eyre::eyre!("invalid console log path: {}", e))?;
        check_status("krun_set_console_output",
            krun_set_console_output(ctx.ctx_id, console_log_c.as_ptr())
        )?;
        log::debug!("libkrun: console output redirected to {}", console_log_path.display());

        // Set kernel bundle directly via mmap'd kernel, bypassing libkrunfw dlopen.
        // This is required for statically-linked epkg binaries which cannot use dlopen().
        check_status("krun_set_kernel_bundle",
            krun_set_kernel_bundle(
                ctx.ctx_id,
                kernel_host_addr,
                kernel_guest_addr,
                kernel_entry_addr,
                kernel_size,
            )
        )?;
        log::debug!("libkrun: kernel bundle set via krun_set_kernel_bundle()");

        // NOTE: We do NOT call ctx.set_kernel() here. The kernel is already set
        // via krun_set_kernel_bundle() with the correct entry point parsed from
        // the bzImage header.

        ctx.set_root(rootfs)?;
        ctx.set_env(&env_vec)?;
        ctx.set_workdir("/")?;

        // Mount self env at /self for epkg symlinks (../../../self/usr/bin/epkg)
        if let Some(self_env) = crate::dirs::find_env_root("self") {
            if let Some(self_env_str) = self_env.to_str() {
                log::debug!("libkrun: mounting self env at /self: {}", self_env_str);
                match ctx.add_virtiofs("self", self_env_str) {
                    Ok(()) => log::debug!("libkrun: successfully mounted self env at /self"),
                    Err(e) => {
                        log::warn!("libkrun: failed to mount self env at /self: {}", e);
                        log::warn!("libkrun: symlinks to self may not work");
                    }
                }
            }
        } else {
            log::debug!("libkrun: self env not found, symlinks to self may not work");
        }

        // In vsock mode, let init handle command execution via EPKG_INIT_CMD
        // set_exec would override init, so skip it entirely
        // ctx.set_exec(exec, &args, &env_vec)?;

        // Configure vsock device for vsock mode
        if use_vsock {
            // Disable implicit vsock (created by libkrun by default)
            check_status("krun_disable_implicit_vsock",
                krun_disable_implicit_vsock(ctx.ctx_id)
            )?;
            // Add explicit vsock with TSI feature HIJACK_INET (value 1)
            check_status("krun_add_vsock",
                krun_add_vsock(ctx.ctx_id, 1)
            )?;
            log::debug!("libkrun: vsock configured successfully");
        }

        // Enable virtio-console for guest output (connect to host stdio)
        check_status("krun_add_virtio_console_default",
            krun_add_virtio_console_default(
                ctx.ctx_id,
                libc::STDIN_FILENO,
                libc::STDOUT_FILENO,
                libc::STDERR_FILENO,
            )
        )?;
        log::debug!("libkrun: virtio-console configured successfully");
        log::debug!("libkrun: virtio-console is connected to host stdin/stdout/stderr");
        log::debug!("libkrun: guest console output should appear on stdout/stderr and log file");
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
        let exit_code = vm_client::send_command_via_vsock(&cmd_parts, run_options.use_pty, 10000)
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

