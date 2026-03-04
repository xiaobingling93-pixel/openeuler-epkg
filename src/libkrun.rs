use std::env;
use std::ffi::CString;
use std::path::Path;
use std::ptr;

use color_eyre::eyre;
use color_eyre::Result;

use crate::run::RunOptions;

#[cfg(all(feature = "libkrun", target_os = "linux"))]
extern crate krun as krun_crate;

// FFI for statically linked libkrun (C API from libkrun crate built as staticlib).
#[cfg(all(feature = "libkrun", target_os = "linux"))]
extern "C" {
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
}


// Force the staticlib to be linked when we only reference it via extern "C".
#[cfg(all(feature = "libkrun", target_os = "linux"))]
fn ensure_libkrun_linked() {
    krun_crate::ensure_linked();
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

/// Detect kernel image format: 0 = Raw (Image), 1 = Elf (vmlinux).
fn detect_kernel_format(path: &str) -> Result<u32> {
    let mut f = std::fs::File::open(path).map_err(|e| eyre::eyre!("open kernel {}: {}", path, e))?;
    let mut magic = [0u8; 4];
    use std::io::Read;
    f.read_exact(&mut magic).map_err(|e| eyre::eyre!("read kernel {}: {}", path, e))?;
    if magic == [0x7f, b'E', b'L', b'F'] {
        Ok(1) // Elf
    } else {
        Ok(0) // Raw (e.g. aarch64/riscv64 Image)
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
    let rootfs = env_root
        .to_str()
        .ok_or_else(|| eyre::eyre!("env_root path is not valid UTF-8"))?;
    let exec = guest_cmd_path
        .to_str()
        .ok_or_else(|| eyre::eyre!("guest command path is not valid UTF-8"))?;

    let mut args: Vec<String> = Vec::new();
    args.push(exec.to_string());
    args.extend(run_options.args.clone());

    let env_vec: Vec<(String, String)> = env::vars().collect();

    let kernel_path = crate::run::resolve_vm_kernel_path(run_options)?;
    let kernel_format = detect_kernel_format(&kernel_path)?;
    ensure_libkrun_linked();

    unsafe {
        // Skip libkrun-internal logger initialization because epkg already
        // installed a global env_logger in main(); attempting to initialize a
        // second global logger would panic. Libkrun will still function without
        // its own logger configured here.
        let ctx = KrunContext::create()?;
        let cpus = crate::run::resolve_vm_cpus(run_options);
        let requested_mib = crate::run::resolve_vm_memory_mib(run_options);
        let memory_mib = crate::run::round_up_vm_memory_for_libkrun(requested_mib, &kernel_path);

        ctx.set_vm_config(cpus, memory_mib)?;
        ctx.set_kernel(
            &kernel_path,
            kernel_format,
            run_options.kernel_args.as_deref(),
        )?;
        ctx.set_root(rootfs)?;
        ctx.set_env(&env_vec)?;
        ctx.set_workdir("/")?;
        ctx.set_exec(exec, &args, &env_vec)?;

        let status = ctx.start_enter();
        if status < 0 {
            return Err(eyre::eyre!(
                "krun_start_enter failed with status {}",
                status
            ));
        }
        std::process::exit(status);
    }
}

