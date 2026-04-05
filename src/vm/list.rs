//! VM list command implementation.

use color_eyre::Result;
use clap::ArgMatches;

use super::session::{list_vm_sessions, is_process_alive};

/// Entry point for `epkg vm list` command.
pub fn cmd_vm_list(_args: &ArgMatches) -> Result<()> {
    let sessions = list_vm_sessions()?;

    if sessions.is_empty() {
        println!("No VMs running");
        return Ok(());
    }

    // Print header
    println!("{:<15} {:<8} {:<8} {:<6} {:<8} {:<10}",
             "ENV", "PID", "TIMEOUT", "CPUS", "MEMORY", "STATUS");

    for session in sessions {
        let status = if is_process_alive(session.daemon_pid) {
            "running"
        } else {
            "stale"
        };

        println!("{:<15} {:<8} {:<8} {:<6} {:<8} {:<10}",
                 session.env_name,
                 session.daemon_pid,
                 session.config.timeout,
                 session.config.cpus,
                 format!("{}M", session.config.memory_mib),
                 status);
    }

    Ok(())
}