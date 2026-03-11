//! Minimal init for VMM mode: mount /proc, /sys, /dev; read kernel cmdline or env for cwd and command; exec.
//! Invoked by the kernel via symlink (e.g. /usr/bin/init -> epkg); run as a normal applet.
//!
//! **Debug-friendly logging:** This code is easy to get wrong and hard to root-cause. At every
//! possible failure point we add `log::debug!` (or eprintln where logging is not yet available)
//! with rich context: what we were doing, paths, errno, and which step failed. Do not fail
//! silently; keep this file step-by-step debuggable.
#![cfg(target_os = "linux")]

use clap::Command as ClapCommand;
use color_eyre::Result;
use color_eyre::eyre::{eyre, WrapErr};
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::path::Path;
use std::sync::OnceLock;

static CMDLINE: OnceLock<HashMap<String, String>> = OnceLock::new();

fn parse_cmdline() -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let Err(e) = std::fs::read_to_string("/proc/cmdline").map(|cmdline| {
        for token in cmdline.split_whitespace() {
            if let Some((k, v)) = token.split_once('=') {
                map.insert(k.to_string(), v.to_string());
            }
        }
    }) {
        eprintln!("init: read /proc/cmdline failed: {} (cmdline params unavailable)", e);
    }
    map
}

fn get_cmdline_param(key: &str) -> Option<String> {
    CMDLINE.get_or_init(parse_cmdline).get(key).cloned()
}

pub fn command() -> ClapCommand {
    ClapCommand::new("init")
        .about("Minimal init for VMM guest: mount proc/sys/dev, read epkg.init_cmd/epkg.init_pwd from cmdline, exec")
        .arg(clap::arg!([command] ... "Command to exec"))  // It's here to accept and ignore kernel passed arguments
}

pub fn run(_options: ()) -> Result<()> {
    run_init()
}

pub fn parse_options(_matches: &clap::ArgMatches) -> Result<()> {
    // init doesn't use CLI args, it reads directly from kernel cmdline
    Ok(())
}

/// Called from main() before setup_logging when argv[0] is init. Mounts /proc, reads
/// epkg.rust_log from kernel cmdline (percent-encoded), sets RUST_LOG so env_logger sees it.
#[cfg(target_os = "linux")]
pub fn init_logging_early() {
    eprintln!("init: init_logging_early() started");
    let proc_path = Path::new("/proc");
    if !proc_path.exists() {
        if let Err(e) = std::fs::create_dir_all(proc_path) {
            eprintln!("init: cannot create /proc: {} (later steps may fail)", e);
        }
    }
    if proc_path.exists() {
        if let Err(e) = nix::mount::mount(
            Some("proc"),
            proc_path,
            Some("proc"),
            nix::mount::MsFlags::empty(),
            None::<&str>,
        ) {
            eprintln!("init: mount proc on /proc failed: {} (cmdline/RUST_LOG unavailable)", e);
        }
    }
    if let Some(v) = get_cmdline_param("epkg.rust_log") {
        let decoded = percent_decode(&v);
        if !decoded.is_empty() {
            std::env::set_var("RUST_LOG", &decoded);
        }
    }
}

fn run_init() -> Result<()> {
    let (pwd, cmd_str) = (get_cmdline_param("epkg.init_pwd"), get_cmdline_param("epkg.init_cmd"));
    log::debug!("init: config pwd={:?} cmd={:?}", pwd, cmd_str.as_deref());

    if let Err(e) = setup_mounts() {
        log::debug!("init: setup_mounts failed: {}", e);
        return Err(e).wrap_err("init: setup_mounts failed");
    }
    if let Some(ref r) = pwd {
        if let Err(e) = std::env::set_current_dir(r) {
            log::debug!("init: chdir {} failed: {}", r, e);
        } else {
            log::debug!("init: chdir {} ok", r);
        }
    }
    raise_system_file_limit();

    // Fork and run command in child process
    match unsafe { nix::unistd::fork() } {
        Ok(nix::unistd::ForkResult::Parent { child }) => {
            log::debug!("init: forked child pid={}, parent entering idle loop", child);
            let status = pid1_idle_loop();
            log::debug!("init: parent idle_loop returned with status: {:?}", status);
            Ok(())
        }
        Ok(nix::unistd::ForkResult::Child) => {
            log::debug!("init: child process running exec_init_command");
            match exec_init_command(cmd_str) {
                Ok(_) => unreachable!(),
                Err(e) => {
                    log::debug!("init: exec_init_command failed: {}", e);
                    // Fallback to shell if command execution fails
                    if let Err(e2) = exec_command("/bin/sh -i") {
                        log::debug!("init: /bin/sh fallback failed: {}", e2);
                        poweroff_guest();
                    }
                    unreachable!()
                }
            }
        }
        Err(e) => {
            log::debug!("init: fork failed: {}", e);
            Err(eyre!("init: fork failed: {}", e))
        }
    }
}

fn pid1_idle_loop() -> Result<()> {
    let mut child_exited = false;
    loop {
        match nix::sys::wait::waitpid(None, Some(nix::sys::wait::WaitPidFlag::WNOHANG)) {
            Ok(nix::sys::wait::WaitStatus::Exited(pid, status)) => {
                log::debug!("init: reaped child pid={} status={}", pid, status);
                child_exited = true;
            }
            Ok(nix::sys::wait::WaitStatus::Signaled(pid, sig, _)) => {
                log::debug!("init: reaped child pid={} signal={:?}", pid, sig);
                child_exited = true;
            }
            Ok(nix::sys::wait::WaitStatus::StillAlive) => {}
            Ok(_) => {}
            Err(nix::errno::Errno::ECHILD) => {
                if child_exited {
                    log::debug!("init: ECHILD received (child_exited=true), powering off guest");
                    poweroff_guest();
                }
            }
            Err(e) => {
                log::debug!("init: waitpid error: {}", e);
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

fn exec_init_command(cmd_str: Option<String>) -> Result<()> {
    if let Some(cmd) = cmd_str {
        log::debug!("init: exec user command: {:?}", cmd);
        exec_command(&cmd)
    } else {
        log::debug!("init: no command, starting vm-daemon");

        // Check if vsock is already available (kernel built-in)
        // If /dev/vsock exists, vsock is ready and we can skip module loading
        let vsock_ready = std::path::Path::new("/dev/vsock").exists();

        if !vsock_ready {
            // Load vsock modules if not already available
            // These modules may not be auto-loaded by the kernel
            log::debug!("init: loading vsock modules");
            try_load_module("vsock");
            try_load_module("vmw_vsock_virtio_transport");
        } else {
            log::debug!("init: vsock already available (/dev/vsock exists)");
        }

        // For vsock mode, network setup is optional (vsock works without network)
        // Only attempt network setup if it will be quick (interfaces already present)
        // or if we need to wait (TCP mode - but we're using vsock now)
        // Skip the long network discovery wait for faster vm-daemon startup
        log::debug!("init: skipping network setup for vsock mode (not needed)");
        // Note: If network is needed in the future, call setup_network_for_vm_daemon()

        log::debug!("init: exec vm-daemon");
        exec_vm_daemon()
    }
}

/// Setup essential filesystem mounts for VMM guest init.
///
/// This function performs the following steps in order:
///
/// 1. Remount rootfs read-write:
///    - Virtiofs typically mounts root readonly; remount rw to allow /dev creation
///    - Logs warning but continues on failure (some setups may already be rw)
///
/// 2. Apply VMM init mount specs:
///    - Mounts /proc, /sys, /tmp, and other essential filesystems
///    - Uses mount_spec_strings() with vmm_init_mount_spec_strings()
///    - Returns error if mount specs fail (critical for init operation)
///
/// 3. Setup /dev filesystem:
///    - Creates /dev directory if missing
///    - Attempts devtmpfs mount first (preferred, kernel-managed device nodes)
///    - Falls back to tmpfs if devtmpfs unavailable (manual device node creation)
///    - Skips if /dev/null already exists (host may have pre-populated /dev)
///
/// 4. Populate /dev with minimal device nodes:
///    - Creates symlinks (e.g., /dev/fd -> /proc/self/fd)
///    - Creates device nodes (null, zero, random, urandom, etc.) via mknod
///    - Mounts devpts for PTY support (/dev/pts)
///    - Logs warning but continues if devpts fails (PTY may not work)
///
/// Returns Ok(()) on success, Err on critical mount failures.
#[cfg(target_os = "linux")]
fn setup_mounts() -> Result<()> {
    // Virtiofs mounts root readonly; remount rw so we can create /dev, etc.
    if let Err(e) = crate::mount::remount_root_rw() {
        log::debug!("init: remount / rw failed: {} (continuing; /dev creation may fail)", e);
    }

    // Debug: check if self env is visible through bind mounts
    let self_epkg = Path::new("/home/wfg/.epkg/envs/self/usr/bin/epkg");
    if self_epkg.exists() {
        log::debug!("init: self epkg exists at {:?}", self_epkg);
    } else {
        log::debug!("init: self epkg NOT found at {:?}", self_epkg);
        // Check what's in /home/wfg/.epkg
        if let Ok(entries) = std::fs::read_dir("/home/wfg/.epkg") {
            log::debug!("init: /home/wfg/.epkg contents:");
            for entry in entries.flatten() {
                log::debug!("  {:?}", entry.file_name());
            }
        } else {
            log::debug!("init: /home/wfg/.epkg does not exist or cannot be read");
        }
    }

    // Debug: check vm-daemon and epkg symlinks
    let vm_daemon = Path::new("/usr/bin/vm-daemon");
    let epkg = Path::new("/usr/bin/epkg");
    log::debug!("init: /usr/bin/vm-daemon exists: {}", vm_daemon.exists());
    log::debug!("init: /usr/bin/epkg exists: {}", epkg.exists());
    if let Ok(link) = std::fs::read_link(vm_daemon) {
        log::debug!("init: vm-daemon -> {:?}", link);
    }
    if let Ok(link) = std::fs::read_link(epkg) {
        log::debug!("init: epkg -> {:?}", link);
    }
    // Check if we can stat the final target
    if let Ok(meta) = std::fs::metadata(vm_daemon) {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode();
        log::debug!("init: vm-daemon metadata: is_file={}, mode={:o}, executable={}",
            meta.is_file(), mode & 0o777, (mode & 0o111) != 0);
    } else {
        log::debug!("init: vm-daemon metadata: FAILED");
    }

    let init_specs = crate::mount::vmm_init_mount_spec_strings();
    log::debug!("init: applying {} mount specs (proc, tmp, ...)", init_specs.len());
    crate::mount::mount_spec_strings(
        &init_specs,
        Path::new("/"),
        crate::models::SandboxMode::Vm,
    ).wrap_err_with(|| format!("init: mount_spec_strings failed (specs: {:?})", init_specs))?;

    if Path::new("/dev").exists() && Path::new("/dev/null").exists() {
        log::debug!("init: /dev already populated, skip devtmpfs/tmpfs");
    } else {
        fs_create_dir_if_missing("/dev").wrap_err("init: create /dev")?;
        if let Err(e) = nix::mount::mount(
            Some("devtmpfs"),
            Path::new("/dev"),
            Some("devtmpfs"),
            nix::mount::MsFlags::empty(),
            None::<&str>,
        ) {
            log::debug!("init: mount devtmpfs on /dev failed: {}, trying tmpfs", e);
            nix::mount::mount(
                Some("tmpfs"),
                Path::new("/dev"),
                Some("tmpfs"),
                nix::mount::MsFlags::empty(),
                None::<&str>,
            ).map_err(|e2| eyre!("init: mount devtmpfs and tmpfs on /dev failed: devtmpfs={}, tmpfs={}", e, e2))?;
        } else {
            log::debug!("init: mounted devtmpfs on /dev");
        }
    }

    log::debug!("init: ensure_minimal_dev (symlinks, nodes, devpts)");
    ensure_minimal_dev().wrap_err("init: ensure_minimal_dev")?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn exec_vm_daemon() -> Result<()> {
    // Call vm_daemon::run() directly instead of exec-ing the binary.
    // This avoids issues with symlink resolution when vm-daemon -> epkg -> /home/wfg/.epkg/envs/self/...
    // and the epkg binary is accessed via virtiofs bind mounts.
    log::debug!("init: starting vm-daemon directly (no exec)");
    let options = crate::busybox::vm_daemon::VmDaemonOptions::default();
    let result = crate::busybox::vm_daemon::run(options);
    log::debug!("init: vm_daemon::run() returned: {:?}", result);
    // vm_daemon returns after handling command; trigger immediate shutdown
    // to avoid waiting for PID 1's 1-second poll loop
    if result.is_err() {
        log::debug!("init: vm_daemon failed, still powering off");
    }
    poweroff_guest();
}

#[cfg(target_os = "linux")]
fn exec_command(cmd_str: &str) -> Result<()> {
    use nix::unistd::execvp;
    use std::ffi::CString;

    let decoded_cmd = percent_decode(cmd_str);
    if decoded_cmd != cmd_str {
        log::debug!("init: decoded percent-encoded cmd: {:?}", decoded_cmd);
    }

    let parts: Vec<String> = shlex::split(&decoded_cmd)
        .ok_or_else(|| eyre!("init: failed to parse command: {:?}", decoded_cmd))?;
    let (cmd, args) = if parts.is_empty() {
        log::debug!("init: empty command, fallback to /bin/sh -i");
        ("/bin/sh".to_string(), vec!["-i".to_string()])
    } else {
        (parts[0].clone(), parts[1..].to_vec())
    };

    // Setup color PS1 for bash and non-dash sh (busybox sh supports \w, dash does not)
    let cmd_name = std::path::Path::new(&cmd)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let set_ps1 = if cmd_name == "bash" {
        true
    } else if cmd_name == "sh" {
        // Check if sh is actually dash (dash doesn't support \w)
        std::fs::canonicalize(&cmd)
            .ok()
            .and_then(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|name| name != "dash")
            })
            .unwrap_or(false)
    } else {
        false
    };
    if set_ps1 {
        std::env::set_var("PS1", "\\[\\033[01;32m\\]\\w\\[\\033[0m\\] $ ");
    }

    log::debug!("init: exec cmd={:?} args={:?}", cmd, args);

    let cmd_c = CString::new(cmd.as_str()).map_err(|e| eyre!("init: command CString: {}", e))?;
    let mut args_c: Vec<CString> = vec![cmd_c.clone()];
    for a in &args {
        args_c.push(CString::new(a.as_str()).map_err(|e| eyre!("init: arg {:?} CString: {}", a, e))?);
    }

    execvp(&cmd_c, &args_c).map_err(|e| eyre!("init: exec {} failed: {}", cmd, e))?;
    unreachable!()
}

fn fs_create_dir_if_missing(p: &str) -> Result<()> {
    let path = Path::new(p);
    if !path.exists() {
        std::fs::create_dir_all(path).map_err(|e| eyre!("mkdir {}: {}", p, e))?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn raise_system_file_limit() {
    const FILE_MAX_PATH: &str = "/proc/sys/fs/file-max";
    const TARGET: u64 = 1_048_576;
    if let Err(e) = std::fs::write(FILE_MAX_PATH, TARGET.to_string()) {
        log::debug!("init: could not set {} to {}: {} (kernel cmdline sysctl.fs.file-max may still apply)", FILE_MAX_PATH, TARGET, e);
    } else {
        log::debug!("init: set {} to {}", FILE_MAX_PATH, TARGET);
    }
}

#[cfg(target_os = "linux")]
fn ensure_minimal_dev() -> Result<()> {
    let dev_root = Path::new("/dev");
    crate::mount::ensure_dev_symlinks(dev_root)
        .wrap_err("init: ensure_dev_symlinks(/dev)")?;
    crate::mount::ensure_minimal_dev_nodes(dev_root)
        .wrap_err("init: ensure_minimal_dev_nodes(/dev)")?;
    if let Err(e) = crate::mount::ensure_devpts_mount(dev_root) {
        log::debug!("init: ensure_devpts_mount(/dev) failed: {} (PTY may not work)", e);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn try_load_module(name: &str) -> bool {
    let r = crate::busybox::modprobe::run(crate::busybox::modprobe::ModprobeOptions {
        remove: false,
        quiet: false,
        module: name.to_string(),
        params: vec![],
    });
    match &r {
        Ok(()) => {
            log::debug!("init: modprobe {} -> ok", name);
            true
        }
        Err(e) => {
            log::debug!("init: modprobe {} -> failed: {}", name, e);
            false
        }
    }
}


/// Setup network for vm-daemon: load virtio_net driver and configure network interface.
/// Returns error if network setup fails (caller may fallback to /bin/sh).
#[cfg(target_os = "linux")]
#[allow(dead_code)]
fn setup_network_for_vm_daemon() -> Result<(), String> {
    log::debug!("init: checking virtio_net module / interfaces for vm-daemon");

    // Fast path: if we already see a non-loopback interface, assume the kernel
    // (or initramfs) has brought up virtio networking and skip modprobe noise.
    let net_dir = std::path::Path::new("/sys/class/net");
    if net_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(net_dir) {
            let has_non_lo = entries
                .flatten()
                .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
                .any(|name| name != "lo");
            if has_non_lo {
                log::debug!("init: non-loopback interface already present, skipping virtio_net modprobe");
            } else if std::path::Path::new("/lib/modules").exists() {
                // Only attempt modprobe when /lib/modules exists; on minimal or
                // sandbox-kernel-based systems there may be no module tree at all.
                log::debug!("init: no non-loopback interface yet, trying virtio_net modprobe");
                try_load_module("failover");
                try_load_module("net_failover");
                let net_loaded = try_load_module("virtio_net");
                if net_loaded {
                    log::debug!("init: virtio_net module loaded");
                } else {
                    log::debug!(
                        "init: virtio_net modprobe failed (kernel may have it built-in or no /lib/modules tree)"
                    );
                }
            } else {
                log::debug!(
                    "init: /lib/modules missing and no non-loopback interface yet; \
                     assuming built-in virtio_net or delayed network bring-up"
                );
            }
        }
    }

    log::debug!("init: configuring network for vm-daemon");
    configure_network()
}

/// Parse network interface flags, supporting both decimal and hex (0x prefix)
#[cfg(target_os = "linux")]
#[allow(dead_code)]
fn parse_net_flags(s: &str) -> Option<u32> {
    let s = s.trim();
    if s.starts_with("0x") || s.starts_with("0X") {
        u32::from_str_radix(&s[2..], 16).ok()
    } else {
        s.parse::<u32>().ok()
    }
}

/// Check if interface is suitable (non-loopback). Returns true if suitable, false if loopback.
#[cfg(target_os = "linux")]
#[allow(dead_code)]
fn is_interface_suitable(name: &str, net_dir: &Path) -> bool {
    const IFF_LOOPBACK: u32 = 0x8;
    if name == "lo" {
        return false;
    }
    let flags_path = net_dir.join(name).join("flags");
    match std::fs::read_to_string(&flags_path) {
        Ok(flags) => {
            if let Some(v) = parse_net_flags(&flags) {
                if v & IFF_LOOPBACK != 0 {
                    return false;
                }
            } else {
                log::debug!("init: {} flags parse failed (content: {:?}), treating as non-loopback", name, flags.trim());
            }
        }
        Err(e) => {
            log::debug!("init: read {} failed: {}, treating as non-loopback", flags_path.display(), e);
        }
    }
    true
}

/// Attempt to discover primary interface once. Returns Ok(Some(name)) if found,
/// Ok(None) if not found yet, or Err on read_dir failure.
#[cfg(target_os = "linux")]
#[allow(dead_code)]
fn try_discover_interface_once(net_dir: &Path, attempt: u32, log_first: bool) -> Result<Option<String>, std::io::Error> {
    let mut entries: Vec<_> = std::fs::read_dir(net_dir)?
        .filter_map(|e| e.ok())
        .collect();
    entries.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    let names: Vec<String> = entries.iter()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    if log_first {
        log::debug!("init: net discovery attempt 1: interfaces {:?}", names);
    }
    for entry in &entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_interface_suitable(&name, net_dir) {
            log::debug!("init: found interface {} after {} attempts (candidates: {:?})", name, attempt + 1, names);
            return Ok(Some(name));
        }
    }
    Ok(None)
}

/// Discover the first non-loopback network interface (e.g. eth0, enp0s2).
/// Returns Ok(interface) or Err(last_seen_names) for error context.
/// Retries for up to 10 seconds (50 attempts * 200ms) to allow virtio_net driver to initialize.
#[cfg(target_os = "linux")]
#[allow(dead_code)]
fn discover_primary_interface() -> Result<String, Vec<String>> {
    const MAX_ATTEMPTS: u32 = 50;      // Total attempts before giving up
    const RETRY_MS: u64 = 200;         // Delay between attempts (total: 10 seconds)

    let net_dir = Path::new("/sys/class/net");
    let mut last_seen: Vec<String> = vec![];
    for attempt in 0..MAX_ATTEMPTS {
        match try_discover_interface_once(net_dir, attempt, attempt == 0) {
            Ok(Some(name)) => return Ok(name),
            Ok(None) => {
                // Update last_seen for error context
                if let Ok(rd) = std::fs::read_dir(net_dir) {
                    last_seen = rd.filter_map(|e| e.ok())
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect();
                }
            }
            Err(e) => {
                if attempt == 0 || attempt + 1 == MAX_ATTEMPTS {
                    log::debug!("init: /sys/class/net read_dir failed (attempt {}): {}", attempt + 1, e);
                }
            }
        }
        if attempt + 1 == MAX_ATTEMPTS {
            log::debug!("init: net discovery gave up after {} attempts: saw {:?}", MAX_ATTEMPTS, last_seen);
        }
        if attempt + 1 < MAX_ATTEMPTS {
            std::thread::sleep(std::time::Duration::from_millis(RETRY_MS));
        }
    }
    Err(last_seen)
}

#[cfg(target_os = "linux")]
#[allow(dead_code)]
fn configure_network() -> Result<(), String> {
    // QEMU user networking default configuration (matches -netdev user defaults)
    const GUEST_IP:      (u8, u8, u8, u8) = (10, 0, 2, 15);     // Guest IP address
    const GUEST_NETMASK: (u8, u8, u8, u8) = (255, 255, 255, 0); // /24 subnet
    const GATEWAY_IP:    (u8, u8, u8, u8) = (10, 0, 2, 2);      // QEMU gateway/host

    let iface = discover_primary_interface().map_err(|last_seen| {
        format!("no non-loopback network interface found (saw: {:?})", last_seen)
    })?;
    log::debug!("init: configuring interface {} (up, then {}.{}.{}.{}/{}.{}.{}.{}, then default route)",
                iface, GUEST_IP.0, GUEST_IP.1, GUEST_IP.2, GUEST_IP.3,
                GUEST_NETMASK.0, GUEST_NETMASK.1, GUEST_NETMASK.2, GUEST_NETMASK.3);

    log::debug!("init: ifconfig {} up", iface);
    crate::busybox::ifconfig::run(crate::busybox::ifconfig::IfconfigOptions {
        interface: iface.clone(),
        address: None,
        netmask: None,
        up: true,
        down: false,
    })
    .map_err(|e| format!("ifconfig {} up: {}", iface, e))?;

    log::debug!("init: ifconfig {} {}.{}.{}.{}/{}.{}.{}.{}", iface,
                GUEST_IP.0, GUEST_IP.1, GUEST_IP.2, GUEST_IP.3,
                GUEST_NETMASK.0, GUEST_NETMASK.1, GUEST_NETMASK.2, GUEST_NETMASK.3);
    crate::busybox::ifconfig::run(crate::busybox::ifconfig::IfconfigOptions {
        interface: iface.clone(),
        address: Some(Ipv4Addr::new(GUEST_IP.0, GUEST_IP.1, GUEST_IP.2, GUEST_IP.3)),
        netmask: Some(Ipv4Addr::new(GUEST_NETMASK.0, GUEST_NETMASK.1, GUEST_NETMASK.2, GUEST_NETMASK.3)),
        up: false,
        down: false,
    })
    .map_err(|e| format!("ifconfig {} {}.{}.{}.{}: {}", iface,
                         GUEST_IP.0, GUEST_IP.1, GUEST_IP.2, GUEST_IP.3, e))?;

    log::debug!("init: route add default via {}.{}.{}.{} dev {}",
                GATEWAY_IP.0, GATEWAY_IP.1, GATEWAY_IP.2, GATEWAY_IP.3, iface);
    crate::busybox::route::run(crate::busybox::route::RouteOptions {
        operation: crate::busybox::route::Operation::Add,
        target: crate::busybox::route::Target::Default,
        gateway: Some(Ipv4Addr::new(GATEWAY_IP.0, GATEWAY_IP.1, GATEWAY_IP.2, GATEWAY_IP.3)),
        interface: Some(iface),
    })
    .map_err(|e| format!("route add default: {}", e))?;
    log::debug!("init: network configured");
    Ok(())
}

#[cfg(target_os = "linux")]
fn poweroff_guest() -> ! {
    use std::fs::OpenOptions;
    use std::io::Write;

    log::debug!("poweroff_guest: initiating VM shutdown");

    // Method 1: Try SysRq 'o' (immediate power off)
    // This works even when ACPI/power management is not available
    if let Ok(mut file) = OpenOptions::new().write(true).open("/proc/sysrq-trigger") {
        log::debug!("poweroff_guest: trying SysRq 'o' (power off)");
        let _ = file.write_all(b"o");
    }

    // Method 2: Try proper poweroff syscall (works on QEMU with ACPI)
    // Note: reboot() returns Infallible on success (never returns) or Errno on failure
    match nix::sys::reboot::reboot(nix::sys::reboot::RebootMode::RB_POWER_OFF) {
        Ok(infallible) => match infallible {},
        Err(e) => {
            log::debug!("poweroff_guest: RB_POWER_OFF failed ({}), trying halt", e);
            // Method 3: Try halt (works when power off is not available)
            match nix::sys::reboot::reboot(nix::sys::reboot::RebootMode::RB_HALT_SYSTEM) {
                Ok(infallible) => match infallible {},
                Err(e2) => {
                    log::debug!("poweroff_guest: RB_HALT_SYSTEM also failed ({}), falling back to exit", e2);
                    // Method 4: Fall back to exit which triggers kernel panic and VM shutdown
                    std::process::exit(0);
                }
            }
        }
    }
}

/// Percent-decode a string from kernel command line
/// Decodes %xx hex sequences (e.g., %20 -> space, %3D -> =, %22 -> ", %27 -> ', %5C -> \, %25 -> %)
/// Properly handles UTF-8 multi-byte sequences
fn percent_decode(s: &str) -> String {
    let mut bytes = Vec::with_capacity(s.len());
    let chars = s.chars().collect::<Vec<char>>();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '%' && i + 2 < chars.len() {
            let hex = format!("{}{}", chars[i + 1], chars[i + 2]);
            match u8::from_str_radix(&hex, 16) {
                Ok(byte) => {
                    bytes.push(byte);
                    i += 3;
                    continue;
                }
                Err(_) => {
                    // Not valid hex, keep % as is
                    bytes.push(b'%');
                }
            }
        } else {
            // Push ASCII/UTF-8 bytes for this character
            let mut buf = [0u8; 4];
            let char_bytes = chars[i].encode_utf8(&mut buf);
            bytes.extend_from_slice(char_bytes.as_bytes());
        }
        i += 1;
    }
    // Convert bytes to String, replacing invalid UTF-8 with replacement char
    String::from_utf8(bytes).unwrap_or_else(|e| {
        log::debug!("init: percent_decode produced invalid UTF-8, using lossy conversion");
        String::from_utf8_lossy(&e.into_bytes()).into_owned()
    })
}

