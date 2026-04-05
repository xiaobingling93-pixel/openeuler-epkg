//! VM keeper process implementation.
//!
//! The keeper process:
//! 1. Creates and configures the libkrun VM
//! 2. Registers the VM session
//! 3. Runs krun_start_enter() (blocking)
//! 4. Cleans up on exit

use std::path::Path;
use color_eyre::Result;

use super::session::VmConfig;

/// Run as VM keeper process.
/// This is called by the spawned child process to hold the VM alive.
#[cfg(all(feature = "libkrun", not(target_os = "linux")))]
pub fn run_vm_keeper(env_root: &Path, env_name: &str, config: VmConfig) -> Result<()> {
    log::info!("VM keeper starting for {} (timeout={}s)", env_name, config.timeout);

    // Call into libkrun's daemon mode
    crate::libkrun::run_vm_daemon_mode(
        env_root,
        env_name,
        config.timeout,
        config.cpus,
        config.memory_mib,
    )
}

#[cfg(not(all(feature = "libkrun", not(target_os = "linux"))))]
pub fn run_vm_keeper(_env_root: &Path, _env_name: &str, _config: VmConfig) -> Result<()> {
    Err(eyre::eyre!("VM keeper not supported on this platform"))
}