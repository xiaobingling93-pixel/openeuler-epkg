use std::env;
use std::fs;
use std::os::fd::{AsRawFd, OwnedFd};

use color_eyre::eyre;
use color_eyre::Result;
use log::{debug, trace, warn};
use nix::unistd::{pipe, read, write, getppid, Pid, Uid, Gid};
use libc;

use crate::dirs;
use crate::utils;

/// Synchronization byte used for parent-child communication
const PIPE_SYNC_BYTE: u8 = 0x69;

/// Unified sync helper for ID mapping coordination.
pub struct IdMapSync {
    read_fd: OwnedFd,
    write_fd: OwnedFd,
    target_pid: Pid,
}

impl IdMapSync {
    pub fn new(target_pid: Pid) -> Result<Self> {
        let (read_fd, write_fd) = pipe()?;
        Ok(Self { read_fd, write_fd, target_pid })
    }

    pub fn set_target_pid(&mut self, target_pid: Pid) {
        self.target_pid = target_pid;
    }

    pub fn read_fd(&self) -> &OwnedFd {
        &self.read_fd
    }

    pub fn wait_for_mapping(&self) -> Result<()> {
        let mut buffer = [0u8; 1];
        read(&self.read_fd, &mut buffer)?;
        if buffer[0] != PIPE_SYNC_BYTE {
            return Err(eyre::eyre!("Invalid sync byte received"));
        }
        Ok(())
    }

    pub fn perform_mapping_and_signal(
        &self,
        uid: Uid,
        gid: Gid,
        user: &Option<String>,
        allow_setgroups: bool,
    ) -> Result<()> {
        execute_idmap_for_pid(self.target_pid, uid, gid, user, allow_setgroups)?;
        write(&self.write_fd, &[PIPE_SYNC_BYTE])?;
        Ok(())
    }

    /// Signal the child to proceed without performing ID mapping.
    /// Used when the child writes its own ID maps (Clone strategy with CLONE_NEWUSER).
    pub fn signal_only(&self) -> Result<()> {
        write(&self.write_fd, &[PIPE_SYNC_BYTE])?;
        Ok(())
    }
}

/// Wait for the parent to finish UID/GID mapping (if a sync pipe is present).
pub(crate) fn wait_for_idmap_sync(sync_read_fd: &OwnedFd) -> Result<()> {
    let fd = sync_read_fd.as_raw_fd();
    let mut buffer = [0u8; 1];
    let result = unsafe {
        libc::read(
            fd,
            buffer.as_mut_ptr() as *mut libc::c_void,
            buffer.len(),
        )
    };
    if result < 0 {
        return Err(eyre::eyre!(
            "Failed to read ID mapping sync byte: {}",
            std::io::Error::last_os_error()
        ));
    }
    if result != 1 {
        return Err(eyre::eyre!(
            "Unexpected ID mapping sync read size: {}",
            result
        ));
    }
    if buffer[0] != PIPE_SYNC_BYTE {
        return Err(eyre::eyre!(
            "Invalid ID mapping sync byte: expected {}, got {}",
            PIPE_SYNC_BYTE,
            buffer[0]
        ));
    }
    trace!("Clone child: received ID mapping sync byte");
    Ok(())
}

/// Check if user namespaces are supported on this system
pub(crate) fn check_user_namespace_support() -> Result<()> {
    // Check if user namespaces are enabled in the kernel
    let proc_files = vec![
        "/proc/sys/user/max_user_namespaces",
        "/proc/sys/kernel/unprivileged_userns_clone",
    ];

    for file in proc_files {
        if let Ok(content) = fs::read_to_string(file) {
            trace!("{}: {}", file, content.trim());
            if file.contains("max_user_namespaces") && content.trim() == "0" {
                return Err(eyre::eyre!(
                    "User namespaces disabled: max_user_namespaces = 0"
                ));
            }
            if file.contains("unprivileged_userns_clone") && content.trim() == "0" {
                return Err(eyre::eyre!("Unprivileged user namespaces disabled"));
            }
        }
    }

    Ok(())
}

/// Read subuid/subgid ranges for a user
fn read_subid_ranges(username: &str, subid_file: &str) -> Result<Vec<(u32, u32)>> {
    let content = fs::read_to_string(subid_file)
        .map_err(|e| eyre::eyre!("Failed to read {}: {}", subid_file, e))?;

    for line in content.lines() {
        let parts: Vec<&str> = line.split(':').collect();
        if parts.len() == 3 && parts[0] == username {
            let start = parts[1].parse::<u32>()
                .map_err(|e| eyre::eyre!("Invalid start ID in {}: {}", subid_file, e))?;
            let count = parts[2].parse::<u32>()
                .map_err(|e| eyre::eyre!("Invalid count in {}: {}", subid_file, e))?;
            return Ok(vec![(start, count)]);
        }
    }

    Err(eyre::eyre!("No subid ranges found for user {} in {}", username, subid_file))
}

/// Execute ID mapping for an arbitrary target process using newuidmap/newgidmap.
///
/// When `allow_setgroups` is true (used for VM mode), writes "allow" to
/// /proc/<pid>/setgroups before writing gid_map so that virtiofsd (spawned
/// in the same user namespace) can call setgroups(). Otherwise writes "deny"
/// for hardening (default for Env/Fs modes).
pub fn execute_idmap_for_pid(
    target_pid: nix::unistd::Pid,
    uid: Uid,
    gid: Gid,
    opt_user: &Option<String>,
    allow_setgroups: bool,
) -> Result<()> {
    let username = dirs::get_username()?;
    let uid_raw = uid.as_raw();
    let gid_raw = gid.as_raw();

    trace!(
        "Executing ID mapping for PID {} (user: {}, UID: {}, GID: {})",
        target_pid,
        username,
        uid_raw,
        gid_raw
    );

    // Check if newuidmap and newgidmap commands are available
    let has_newuidmap = utils::command_exists("newuidmap");
    let has_newgidmap = utils::command_exists("newgidmap");

    trace!("UID mapping tools: newuidmap={}, newgidmap={}", has_newuidmap, has_newgidmap);

    if has_newuidmap && has_newgidmap {
        // Try Podman's approach with newuidmap/newgidmap
        match execute_newidmap_for_parent(target_pid, uid_raw, gid_raw, &username, allow_setgroups) {
            Ok(()) => {
                trace!("Successfully used newuidmap/newgidmap for UID/GID mapping");
                return Ok(());
            }
            Err(e) => {
                warn!(
                    "newuidmap/newgidmap failed: {} (if 'Operation not permitted', check /etc/subuid and /etc/subgid for your user, or run as root). Falling back to simple UID/GID mapping.",
                    e
                );
                execute_simple_idmap_for_parent(target_pid, uid_raw, gid_raw, allow_setgroups)?;
            }
        }
    } else {
        // Fallback to simple mapping
        execute_simple_idmap_for_parent(target_pid, uid_raw, gid_raw, allow_setgroups)?;
    }

    // Set environment variables if user was specified (this will be inherited by the parent)
    if let Some(user_spec) = opt_user {
        if let Ok(_parsed_uid) = user_spec.parse::<u32>() {
            // For numeric UIDs, we don't change environment variables
        } else {
            // For username, set environment variables
            env::set_var("USER", user_spec);
            env::set_var("LOGNAME", user_spec);
        }
    }

    Ok(())
}

/// Legacy helper that executes ID mapping for the current parent process.
///
/// This is kept for existing callers that operate in a fork()+unshare() model,
/// where the mapping target is always the direct parent of the helper process.
pub fn execute_idmap_for_parent(
    uid: Uid,
    gid: Gid,
    opt_user: &Option<String>,
    allow_setgroups: bool,
) -> Result<()> {
    let parent_pid = getppid();
    execute_idmap_for_pid(parent_pid, uid, gid, opt_user, allow_setgroups)
}

/// Execute newuidmap/newgidmap for the target process
fn execute_newidmap_for_parent(
    target_pid: nix::unistd::Pid,
    uid_raw: u32,
    gid_raw: u32,
    username: &str,
    allow_setgroups: bool,
) -> Result<()> {
    // Read subuid and subgid ranges
    let subuid_ranges = read_subid_ranges(username, "/etc/subuid")?;
    let subgid_ranges = read_subid_ranges(username, "/etc/subgid")?;

    trace!("Subuid ranges: {:?}", subuid_ranges);
    trace!("Subgid ranges: {:?}", subgid_ranges);

    // Write setgroups allow/deny before gid_map (must be before gid_map is set)
    let setgroups_val = if allow_setgroups { "allow" } else { "deny" };
    write_id_map_for_pid(target_pid, "/proc/{}/setgroups", setgroups_val)?;

    // Set up UID mapping using newuidmap
    execute_newidmap_for_pid("newuidmap", target_pid, uid_raw, &subuid_ranges)?;

    // Set up GID mapping using newgidmap
    execute_newidmap_for_pid("newgidmap", target_pid, gid_raw, &subgid_ranges)?;

    trace!("Successfully mapped UID/GID ranges using newuidmap/newgidmap");
    Ok(())
}

/// Execute newuidmap or newgidmap command for a specific PID
fn execute_newidmap_for_pid(cmd: &str, target_pid: nix::unistd::Pid, current_id: u32, ranges: &[(u32, u32)]) -> Result<()> {
    let mut args = vec![
        cmd.to_string(),
        target_pid.as_raw().to_string(), // target PID (parent)
    ];

    // Map root (0) to current user/group
    args.push("0".to_string());
    args.push(current_id.to_string());
    args.push("1".to_string());

    // Map additional ranges starting from 1
    for (start, count) in ranges {
        if *count > 1 {
            args.push("1".to_string());
            args.push(start.to_string());
            args.push(count.to_string());
            break; // Use first range for now
        }
    }

    trace!("Executing {} with args: {:?}", cmd, args);
    let status = std::process::Command::new(&args[0])
        .args(&args[1..])
        .status()
        .map_err(|e| eyre::eyre!("Failed to execute {}: {}", cmd, e))?;

    if !status.success() {
        return Err(eyre::eyre!("{} failed with status: {}", cmd, status));
    }

    Ok(())
}

/// Execute simple ID mapping for the parent process
fn execute_simple_idmap_for_parent(
    parent_pid: nix::unistd::Pid,
    uid_raw: u32,
    gid_raw: u32,
    allow_setgroups: bool,
) -> Result<()> {
    // In user namespaces, we typically map ourselves to become root inside the namespace
    // This gives us the privileges needed for bind mounting
    // Format: "inside_id outside_id count"
    let uid_map = format!("0 {} 1", uid_raw);
    let gid_map = format!("0 {} 1", gid_raw);

    debug!("Setting up simple user namespace mapping for PID {}: uid_map='{}', gid_map='{}'",
           parent_pid, uid_map, gid_map);

    // Write setgroups allow/deny before gid_map (must be before gid_map is set)
    let setgroups_val = if allow_setgroups { "allow" } else { "deny" };
    write_id_map_for_pid(parent_pid, "/proc/{}/setgroups", setgroups_val)?;
    write_id_map_for_pid(parent_pid, "/proc/{}/uid_map", &uid_map)?;
    write_id_map_for_pid(parent_pid, "/proc/{}/gid_map", &gid_map)?;

    Ok(())
}

/// Write to ID mapping files for a specific PID
fn write_id_map_for_pid(pid: nix::unistd::Pid, path_template: &str, content: &str) -> Result<()> {
    let path = path_template.replace("{}", &pid.as_raw().to_string());
    fs::write(&path, content)
        .map_err(|e| eyre::eyre!("Failed to write to {}: {}", path, e))?;
    Ok(())
}
