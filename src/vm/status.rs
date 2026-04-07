//! VM status command implementation.

use color_eyre::{Result, eyre};
use clap::ArgMatches;

use super::session::{load_session_by_name, is_process_alive};

/// Entry point for `epkg vm status` command.
pub fn cmd_vm_status(_args: &ArgMatches) -> Result<()> {
    let cfg = crate::models::config();
    let env_name = cfg.common.env_name.clone();

    let session = load_session_by_name(&env_name)?
        .ok_or_else(|| eyre::eyre!("No VM running for {}", env_name))?;

    // Check if daemon is alive
    if !is_process_alive(session.daemon_pid) {
        return Err(eyre::eyre!("VM daemon process {} is not running", session.daemon_pid));
    }

    // Output as YAML
    println!("{}", serde_yaml::to_string(&session)?);

    Ok(())
}