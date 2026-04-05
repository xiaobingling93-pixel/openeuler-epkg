//! VM keeper process implementation.
//!
//! The keeper process:
//! 1. Creates and configures the VM (libkrun or qemu backend)
//! 2. Registers the VM session
//! 3. Runs VM (blocking)
//! 4. Cleans up on exit

use std::path::Path;
use color_eyre::Result;
use color_eyre::eyre::eyre;

use super::session::VmConfig;

/// Run as VM keeper process.
/// This is called by the spawned child process to hold the VM alive.
pub fn run_vm_keeper(env_root: &Path, env_name: &str, config: VmConfig) -> Result<()> {
    log::info!("VM keeper starting for {} (backend={}, timeout={}s)",
               env_name, config.backend, config.timeout);

    match config.backend.as_str() {
        "libkrun" => run_libkrun_keeper(env_root, env_name, &config),
        "qemu" => run_qemu_keeper(env_root, env_name, &config),
        _ => Err(eyre!("Unknown VMM backend: {}", config.backend)),
    }
}

/// libkrun keeper implementation.
#[cfg(all(feature = "libkrun", not(target_os = "linux")))]
fn run_libkrun_keeper(env_root: &Path, env_name: &str, config: &VmConfig) -> Result<()> {
    crate::libkrun::run_vm_daemon_mode(
        env_root,
        env_name,
        config.timeout,
        config.cpus,
        config.memory_mib,
    )
}

#[cfg(not(all(feature = "libkrun", not(target_os = "linux"))))]
fn run_libkrun_keeper(_env_root: &Path, _env_name: &str, _config: &VmConfig) -> Result<()> {
    Err(eyre!("libkrun backend not available on this platform"))
}

/// qemu keeper implementation.
#[cfg(target_os = "linux")]
fn run_qemu_keeper(env_root: &Path, env_name: &str, config: &VmConfig) -> Result<()> {
    crate::qemu::run_vm_daemon_mode(
        env_root,
        env_name,
        config.timeout,
        config.cpus,
        config.memory_mib,
    )
}

#[cfg(not(target_os = "linux"))]
fn run_qemu_keeper(_env_root: &Path, _env_name: &str, _config: &VmConfig) -> Result<()> {
    Err(eyre!("qemu backend only available on Linux"))
}