#![cfg(unix)]
use std::path::Path;
use crate::lfs;

use crate::run::RunOptions;
use crate::vm_client;
use color_eyre::eyre;
use color_eyre::Result;
use crate::models::dirs;

/// Parse VMM configuration (kernel via run::resolve_vm_kernel_path; initrd from run_options or env, etc.).
fn parse_vmm_config(run_options: &RunOptions) -> Result<(String, Option<String>, String, String, String)> {
    let kernel = crate::run::resolve_vm_kernel_path(run_options)?;
    // Prefer --initrd CLI option, then EPKG_VM_INITRD env var
    let initrd = run_options.initrd.clone().or_else(|| std::env::var("EPKG_VM_INITRD").ok());
    let qemu_bin = std::env::var("EPKG_VM_QEMU").unwrap_or_else(|_| "qemu-system-x86_64".to_string());
    let virtiofsd_bin = std::env::var("EPKG_VM_VIRTIOFSD")
        .unwrap_or_else(|_| "virtiofsd".to_string());
    let virtiofsd_path = crate::utils::find_command_in_paths(&virtiofsd_bin)
        .ok_or_else(|| eyre::eyre!(
            "virtiofsd binary not found. Tried: '{}' in PATH and common system paths. \
             Please install virtiofsd or set EPKG_VM_VIRTIOFSD environment variable.",
            virtiofsd_bin
        ))?;
    let virtiofsd_bin = virtiofsd_path.to_string_lossy().to_string();
    // QEMU-specific extra kernel cmdline arguments. Prefer CLI --kernel-args,
    // then EPKG_QEMU_EXTRA_ARGS, then legacy EPKG_VM_EXTRA_ARGS.
    let env_extra = std::env::var("EPKG_QEMU_EXTRA_ARGS")
        .or_else(|_| std::env::var("EPKG_VM_EXTRA_ARGS"))
        .unwrap_or_default();
    let extra_qemu_args = if let Some(cli_args) = &run_options.kernel_args {
        if env_extra.is_empty() {
            cli_args.clone()
        } else {
            format!("{} {}", env_extra, cli_args)
        }
    } else {
        env_extra
    };
    Ok((kernel, initrd, qemu_bin, virtiofsd_bin, extra_qemu_args))
}

/// Ensure the VMM log directory exists so that e.g. tail ~/.cache/epkg/vmm-logs/latest-qemu.log
/// can be used after at least one QEMU run. Call this when entering the VM path.
pub(crate) fn ensure_vmm_log_dir() -> Result<()> {
    let base_log_dir = dirs().epkg_cache.join("vmm-logs");
    lfs::create_dir_all(&base_log_dir)
        .map_err(|e| eyre::eyre!("Failed to create VMM log directory: {}", e))?;
    Ok(())
}

/// Create a PID-based log file with symlink for a given log name.
/// Returns the full path to the log file.
/// The log file will be created in {epkg_cache}/vmm-logs/{log_name}-{pid}.log
/// and a symlink latest-{log_name}.log will point to it.
fn create_pid_log_with_symlink(log_name: &str) -> Result<std::path::PathBuf> {
    let base_log_dir = dirs().epkg_cache.join("vmm-logs");
    lfs::create_dir_all(&base_log_dir)
        .map_err(|e| eyre::eyre!("Failed to create VMM log directory: {}", e))?;

    let pid = std::process::id();
    let log_path = base_log_dir.join(format!("{}-{}.log", log_name, pid));

    // Create symlink to latest log
    let latest_log = base_log_dir.join(format!("latest-{}.log", log_name));
    let _ = lfs::remove_file(&latest_log);
    if let Err(e) = lfs::symlink(&log_path, &latest_log) {
        log::warn!("Failed to create symlink {} -> {}: {}", latest_log.display(), log_path.display(), e);
    }

    Ok(log_path)
}

/// Set up VMM log directory and create PID-based log files with symlinks.
/// Returns (qemu_log_path, virtiofsd_log_path).
/// Strategy:
/// - Fixed directory under epkg_cache (persistent across runs)
/// - PID-based filenames for uniqueness (qemu-{pid}.log, virtiofsd-{pid}.log)
/// - Symlinks "latest-qemu.log" and "latest-virtiofsd.log" point to most recent logs
/// This ensures logs survive program exit and are human-friendly for debugging.
pub fn setup_vmm_logs() -> Result<(std::path::PathBuf, std::path::PathBuf)> {
    let qemu_log_path = create_pid_log_with_symlink("qemu")?;
    let virtiofsd_log_path = create_pid_log_with_symlink("virtiofsd")?;

    log::info!("VMM logs: QEMU={} virtiofsd={}", qemu_log_path.display(), virtiofsd_log_path.display());
    Ok((qemu_log_path, virtiofsd_log_path))
}

/// Build guest command string and percent-encode it for kernel command line
pub fn build_guest_command(cmd_path: &Path, args: &[String]) -> Result<(Vec<String>, String)> {
    let mut cmd_parts: Vec<String> = Vec::new();
    cmd_parts.push(cmd_path.to_string_lossy().to_string());
    cmd_parts.extend(args.iter().cloned());
    // Use shlex-style quoting to survive kernel cmdline parsing
    let raw_cmd = shlex::try_join(cmd_parts.iter().map(|s| s.as_str()))
        .map_err(|e| eyre::eyre!("Failed to join command parts: {}", e))?;
    let init_cmd = percent_encode(&raw_cmd);
    Ok((cmd_parts, init_cmd))
}

/// Start virtiofsd daemon for sharing a directory with guest.
///
/// For VM mode we run this in the child after allowing setgroups in the user
/// namespace (allow_setgroups=true when writing gid_map), so virtiofsd sees
/// the same mounts and UID mapping as the rest of the sandbox. See virtio-fs/virtiofsd#36.
///
/// Logs are written to `virtiofsd_log_path` so the terminal stays free for send_command_via_tcp.
fn start_virtiofsd_at(
    shared_dir: &Path,
    virtiofsd_bin: &str,
    virtiofsd_log_path: &Path,
) -> Result<(tempfile::TempDir, std::process::Child, std::path::PathBuf)> {
    use std::fs::File;
    use std::process::{Command, Stdio};

    // Create a temporary directory for VMM artifacts (virtiofsd socket, etc.)
    let tmpdir = tempfile::Builder::new()
        .prefix("epkg-vmm-")
        .tempdir()
        .map_err(|e| eyre::eyre!("Failed to create temporary directory for VMM: {}", e))?;
    let socket_path = tmpdir.path().join("vhostqemu.sock");

    // Start virtiofsd pointing at shared_dir
    let mut virtiofsd_cmd = Command::new(virtiofsd_bin);
    virtiofsd_cmd
        .arg("--shared-dir")
        .arg(shared_dir.display().to_string())
        .arg("--socket-path")
        .arg(socket_path.display().to_string())
        .arg("--cache")
        .arg("auto")
        // Use file handles when permitted so guest caches by inode/handle (avoids ENFILE when available).
        // mandatory requires DAC_READ_SEARCH; "prefer" tries handles and falls back to fds if EPERM.
        .arg("--inode-file-handles=prefer")
        .arg("--sandbox").arg("none")
        // 0 = leave RLIMIT_NOFILE unchanged; avoids WARN when hard limit < 1000000
        .arg("--rlimit-nofile").arg("0");

    // UID/GID translation handled by user namespace
    // Disable virtiofsd's internal sandboxing since we run it in namespace

    // Redirect stdout/stderr to log file so terminal stays free for send_command_via_tcp.
    let log_file = File::create(virtiofsd_log_path)
        .map_err(|e| eyre::eyre!("Failed to create virtiofsd log {}: {}", virtiofsd_log_path.display(), e))?;
    virtiofsd_cmd
        .stdout(Stdio::from(log_file.try_clone().map_err(|e| eyre::eyre!("Failed to dup virtiofsd log: {}", e))?))
        .stderr(Stdio::from(log_file));

    log::debug!("virtiofsd command: {} {}",
              virtiofsd_bin,
              virtiofsd_cmd.get_args()
                  .map(|s| {
                      let owned = s.to_string_lossy().into_owned();
                      shlex::try_quote(&owned)
                          .map(|cow| cow.into_owned())
                          .unwrap_or_else(|_| owned)
                  })
                  .collect::<Vec<_>>()
                  .join(" "));

    let mut virtiofsd_child = virtiofsd_cmd
        .spawn()
        .map_err(|e| eyre::eyre!("Failed to spawn virtiofsd ({}): {}", virtiofsd_bin, e))?;

    wait_for_virtiofsd_socket(&mut virtiofsd_child, &socket_path)?;

    Ok((tmpdir, virtiofsd_child, socket_path))
}

/// Poll for virtiofsd socket creation with timeout.
/// Returns Ok if socket is created, Err if virtiofsd exits or timeout occurs.
fn wait_for_virtiofsd_socket(
    virtiofsd_child: &mut std::process::Child,
    socket_path: &Path,
) -> Result<()> {
    const SOCKET_WAIT_TIMEOUT_MS: u64 = 500;
    const SOCKET_POLL_INTERVAL_MS: u64 = 5;
    let start = std::time::Instant::now();
    loop {
        // Check if virtiofsd is still running
        match virtiofsd_child.try_wait() {
            Ok(Some(status)) => {
                return Err(eyre::eyre!("virtiofsd exited early with status: {}", status));
            }
            Ok(None) => {
                // Still running, check for socket
                if socket_path.exists() {
                    log::debug!("virtiofsd socket created after {:?}", start.elapsed());
                    return Ok(());
                }
            }
            Err(e) => {
                return Err(eyre::eyre!("Failed to check virtiofsd status: {}", e));
            }
        }

        if start.elapsed().as_millis() as u64 > SOCKET_WAIT_TIMEOUT_MS {
            let _ = virtiofsd_child.kill();
            let _ = virtiofsd_child.wait();
            return Err(eyre::eyre!(
                "virtiofsd socket not created at {} after {}ms timeout",
                socket_path.display(),
                SOCKET_WAIT_TIMEOUT_MS
            ));
        }

        std::thread::sleep(std::time::Duration::from_millis(SOCKET_POLL_INTERVAL_MS));
    }
}

/// Build QEMU command line for starting the VM.
/// When init_cmd is Some (cmdline mode), it is percent-encoded and appended as epkg.init_cmd=...
fn build_qemu_command(
    kernel: &str,
    initrd: &Option<String>,
    qemu_bin: &str,
    socket_path: &std::path::Path,
    mount_tag: &str,
    use_vsock: bool,
    extra_qemu_args: &str,
    serial_log_path: &std::path::Path,
    vm_cpus: u8,
    vm_memory_mb: u32,
    init_cmd: Option<&str>,
) -> std::process::Command {
    use std::process::Command;

    let mut qemu_cmd = Command::new(qemu_bin);
    qemu_cmd
        .arg("-enable-kvm")
        .arg("-cpu").arg("host")
        .arg("-m").arg(vm_memory_mb.to_string())
        .arg("-smp").arg(vm_cpus.to_string())
        .arg("-no-reboot")
        .arg("-nographic")
        .arg("-serial").arg(format!("file:{}", serial_log_path.display()))
        .arg("-monitor").arg("none")
        .arg("-kernel").arg(kernel);

    if let Some(ref initrd_path) = initrd {
        qemu_cmd.arg("-initrd").arg(initrd_path);
    }

    // Shared memory backend required for virtiofs
    qemu_cmd
        .arg("-object")
        .arg(format!("memory-backend-file,id=mem,size={}M,mem-path=/dev/shm,share=on", vm_memory_mb))
        .arg("-numa")
        .arg("node,memdev=mem");

    // Wire virtiofs device for env root
    qemu_cmd
        .arg("-chardev")
        .arg(format!("socket,id=char0,path={}", socket_path.display()))
        .arg("-device")
        .arg(format!("vhost-user-fs-pci,queue-size=1024,chardev=char0,tag={}", mount_tag));


    // Add user networking for guest-host communication (for normal guest networking).
    // TCP hostfwd is no longer used; control-plane uses vsock.
    // romfile="" disables the virtio-net option ROM (iPXE) so SeaBIOS boots the -kernel directly.
    qemu_cmd
        .arg("-netdev")
        .arg("user,id=net0")
        .arg("-device")
        .arg("virtio-net-pci,netdev=net0,romfile=");

    // Optional virtio-vsock device for vsock-based control plane.
    if use_vsock {
        // Guest CID 3 matches the host-side vm_client vsock connector.
        qemu_cmd
            .arg("-device")
            .arg("vhost-vsock-pci,guest-cid=3");
    }

    // Kernel cmdline: console, panic, root filesystem, and epkg init parameters
    // init=/bin/init: kernel runs epkg init (bin->usr/bin, init at usr/bin/init)
    // sysctl.fs.file-max: avoid "Too many open files in system" (ENFILE) with virtiofs
    let mut append_args = format!(
        "console=ttyS0 panic=1 root={} rootfstype=virtiofs init=/bin/init sysctl.fs.file-max=1048576",
        mount_tag
    );
    // Pass host RUST_LOG into guest so init can enable debug logging
    if let Ok(rust_log) = std::env::var("RUST_LOG") {
        if !rust_log.is_empty() {
            append_args.push_str(&format!(" epkg.rust_log={}", percent_encode(&rust_log)));
        }
    }
    // Cmdline mode: pass command and working dir to init via kernel cmdline
    if let Some(cmd) = init_cmd {
        if !cmd.is_empty() {
            append_args.push_str(&format!(" epkg.init_cmd={}", cmd));
        }
    }
    // Pass working directory (from client) to init
    if let Ok(pwd) = std::env::var("PWD") {
        if !pwd.is_empty() && pwd != "/" {
            append_args.push_str(&format!(" epkg.init_pwd={}", percent_encode(&pwd)));
        }
    }
    if !extra_qemu_args.is_empty() {
        append_args.push(' ');
        append_args.push_str(extra_qemu_args);
    }
    qemu_cmd.arg("-append").arg(append_args);

    qemu_cmd
}

type VirtiofsdGuard = Option<(tempfile::TempDir, std::process::Child)>;

/// Setup virtiofsd socket: use existing socket or start new virtiofsd daemon.
/// Returns (virtiofsd_guard, socket_path).
fn setup_virtiofsd_socket(
    env_root: &Path,
    existing_socket_path: Option<&Path>,
    virtiofsd_bin: &str,
    virtiofsd_log_path: &Path,
) -> Result<(VirtiofsdGuard, std::path::PathBuf)> {
    if let Some(path) = existing_socket_path {
        Ok((None, path.to_path_buf()))
    } else {
        let (tmpdir, child, path) = start_virtiofsd_at(env_root, virtiofsd_bin, virtiofsd_log_path)?;
        Ok((Some((tmpdir, child)), path))
    }
}

/// Spawn QEMU process with configured command line.
/// Returns the spawned child process.
/// init_cmd: when Some (cmdline mode), passed as epkg.init_cmd=... in kernel -append.
fn spawn_qemu(
    kernel: &str,
    initrd: &Option<String>,
    qemu_bin: &str,
    socket_path: &Path,
    mount_tag: &str,
    use_vsock: bool,
    extra_qemu_args: &str,
    qemu_log_path: &Path,
    vm_cpus: u8,
    vm_memory_mb: u32,
    init_cmd: Option<&str>,
) -> Result<std::process::Child> {
    use std::process::Stdio;

    let mut qemu_cmd = build_qemu_command(
        kernel,
        initrd,
        qemu_bin,
        socket_path,
        mount_tag,
        use_vsock,
        extra_qemu_args,
        qemu_log_path,
        vm_cpus,
        vm_memory_mb,
        init_cmd,
    );

    // Conditionally log QEMU output based on RUST_LOG level
    // If debug/trace logging is enabled, redirect to log file for debugging
    // Otherwise, send to null to keep terminal clean
    let log_qemu_output = log::log_enabled!(log::Level::Debug);
    if log_qemu_output {
        use std::fs::File;
        let stdout_log = File::create(qemu_log_path.with_extension("stdout.log"))
            .map(Stdio::from)
            .unwrap_or_else(|e| {
                log::warn!("Failed to create QEMU stdout log: {}", e);
                Stdio::null()
            });
        let stderr_log = File::create(qemu_log_path.with_extension("stderr.log"))
            .map(Stdio::from)
            .unwrap_or_else(|e| {
                log::warn!("Failed to create QEMU stderr log: {}", e);
                Stdio::null()
            });
        qemu_cmd.stdout(stdout_log).stderr(stderr_log);
    } else {
        qemu_cmd.stdout(Stdio::null()).stderr(Stdio::null());
    }

    log::debug!("qemu command: {} {}",
              qemu_bin,
              qemu_cmd.get_args()
                  .map(|s| {
                      let owned = s.to_string_lossy().into_owned();
                      shlex::try_quote(&owned)
                          .map(|cow| cow.into_owned())
                          .unwrap_or_else(|_| owned)
                  })
                  .collect::<Vec<_>>()
                  .join(" "));

    qemu_cmd
        .spawn()
        .map_err(|e| eyre::eyre!("Failed to spawn QEMU ({}): {}", qemu_bin, e))
}

/// Handle guest communication and wait for exit code.
/// Control-channel mode (TCP or vsock): send command and get exit code.
/// Cmdline mode: command was passed via kernel cmdline; wait for QEMU to exit.
fn handle_guest_execution(
    qemu_child: &mut std::process::Child,
    use_control_channel: bool,
    use_vsock: bool,
    cmd_parts: &[String],
    use_pty: Option<bool>,
) -> Result<i32> {
    if use_vsock {
        // Vsock control plane: wait for guest ready, then connect to command port.
        // QEMU uses AF_VSOCK, so pass None for unix_socket_path.
        // The ready notification uses AF_VSOCK port 10001.
        match vm_client::wait_ready_and_send_command(cmd_parts, use_pty, 10000, None) {
            Ok(cmd_exit_code) => {
                log::debug!("qemu: command completed with exit code {}, waiting for QEMU to exit", cmd_exit_code);
                let _ = qemu_child
                    .wait()
                    .map_err(|e| eyre::eyre!("Failed to wait for QEMU process: {}", e))?;
                log::debug!("qemu: QEMU process exited");
                Ok(cmd_exit_code)
            }
            Err(e) => {
                if let Err(kill_err) = qemu_child.kill() {
                    log::debug!("Failed to kill QEMU process: {}", kill_err);
                }
                if let Err(wait_err) = qemu_child.wait() {
                    log::debug!("Failed to wait for QEMU process: {}", wait_err);
                }
                Err(e)
            }
        }
    } else if use_control_channel {
        match vm_client::send_command_via_tcp(cmd_parts, use_pty) {
            Ok(cmd_exit_code) => {
                let _ = qemu_child
                    .wait()
                    .map_err(|e| eyre::eyre!("Failed to wait for QEMU process: {}", e))?;
                Ok(cmd_exit_code)
            }
            Err(e) => {
                if let Err(kill_err) = qemu_child.kill() {
                    log::debug!("Failed to kill QEMU process: {}", kill_err);
                }
                if let Err(wait_err) = qemu_child.wait() {
                    log::debug!("Failed to wait for QEMU process: {}", wait_err);
                }
                Err(e)
            }
        }
    } else {
        let qemu_status = qemu_child
            .wait()
            .map_err(|e| eyre::eyre!("Failed to wait for QEMU process: {}", e))?;
        Ok(qemu_status.code().unwrap_or(1))
    }
}

/// Cleanup virtiofsd process if we own it.
fn cleanup_virtiofsd(virtiofsd_guard: VirtiofsdGuard) {
    if let Some((_, mut virtiofsd_child)) = virtiofsd_guard {
        let _ = virtiofsd_child.kill();
        let _ = virtiofsd_child.wait();
    }
}

/// Start a QEMU-based VMM sandbox using virtiofs to share env_root into the guest.
/// This function never returns normally; it exits the process with the guest's exit code.
///
/// If `existing_socket_path` is Some, that virtiofsd socket is used and virtiofsd is
/// not started here (caller started it in the parent to avoid user-namespace setgroups issue).
pub fn run_command_in_qemu(
    env_root: &Path,
    run_options: &RunOptions,
    guest_cmd_path: &Path,
    existing_socket_path: Option<&Path>,
) -> Result<()> {
    let (kernel, initrd, qemu_bin, virtiofsd_bin, extra_qemu_args) = parse_vmm_config(run_options)?;

    let (cmd_parts, init_cmd) = build_guest_command(&guest_cmd_path, &run_options.args)?;
    // Cmdline mode: init runs command from kernel cmdline; no vm-daemon. Set EPKG_VM_NO_DAEMON=1.
    let use_cmdline_mode = std::env::var("EPKG_VM_NO_DAEMON").is_ok();
    let use_vsock = !use_cmdline_mode; // default vsock mode (TCP mode removed)
    let use_control_channel = false; // TCP mode no longer supported
    let vm_cpus = crate::run::resolve_vm_cpus(run_options);
    let vm_memory_mb = crate::run::resolve_vm_memory_mib(run_options);
    let (qemu_log_path, virtiofsd_log_path) = setup_vmm_logs()?;

    let (virtiofsd_guard, socket_path) = setup_virtiofsd_socket(
        env_root,
        existing_socket_path,
        &virtiofsd_bin,
        &virtiofsd_log_path,
    )?;

    let mount_tag = "epkg_env";
    let init_cmd_append = if use_cmdline_mode {
        Some(init_cmd.as_str())
    } else {
        None
    };
    let mut qemu_child = spawn_qemu(
        &kernel,
        &initrd,
        &qemu_bin,
        &socket_path,
        mount_tag,
        use_vsock,
        &extra_qemu_args,
        &qemu_log_path,
        vm_cpus,
        vm_memory_mb,
        init_cmd_append,
    )?;

    let exit_code = handle_guest_execution(
        &mut qemu_child,
        use_control_channel,
        use_vsock,
        &cmd_parts,
        run_options.use_pty,
    )?;

    cleanup_virtiofsd(virtiofsd_guard);

    std::process::exit(exit_code);
}

/// Percent-encode a string for kernel command line transmission
/// Encodes special characters to avoid kernel cmdline parsing issues
/// Spaces -> %20, = -> %3D, " -> %22, ' -> %27, \ -> %5C, % -> %25
/// Keeps slashes and most other characters readable
pub fn percent_encode(s: &str) -> String {
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
