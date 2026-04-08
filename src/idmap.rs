use std::fs;
use std::os::fd::{AsRawFd, OwnedFd};
use std::process::Command;

use color_eyre::eyre;
use color_eyre::Result;
use log::{debug, trace, warn};
use nix::unistd::{getppid, pipe, read, write, Pid, Uid, Gid};
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

    #[allow(dead_code)]
    pub fn wait_for_mapping(&self) -> Result<()> {
        let mut buffer = [0u8; 1];
        read(&self.read_fd, &mut buffer)?;
        if buffer[0] != PIPE_SYNC_BYTE {
            return Err(eyre::eyre!("Invalid sync byte received"));
        }
        Ok(())
    }

    /// Perform ID mapping using newuidmap/newgidmap and signal completion.
    /// For Clone strategy: parent maps child (self.target_pid = child PID).
    /// For Unshare strategy: helper maps parent (self.target_pid = parent PID).
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

    #[allow(dead_code)]
    /// Signal completion without doing ID mapping.
    /// Used by helper after it has already called execute_idmap_for_parent.
    pub fn signal_only(&self) -> Result<()> {
        write(&self.write_fd, &[PIPE_SYNC_BYTE])?;
        Ok(())
    }
}

/// Check if user namespaces are supported on this system
pub(crate) fn check_user_namespace_support() -> Result<()> {
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

/// Read subuid/subgid ranges for the current user from /etc/subuid or /etc/subgid.
fn read_subid_ranges(subid_file: &str) -> Result<Vec<(u32, u32)>> {
    let username = dirs::get_username()?;
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

    trace!("No subid ranges found for user {} in {}", username, subid_file);
    Ok(Vec::new())
}

/// Execute ID mapping for a target PID using newuidmap/newgidmap (setuid helpers).
///
/// The newuidmap/newgidmap commands are setuid root and can map subuid/subgid ranges
/// defined in /etc/subuid and /etc/subgid. This allows unprivileged users to create
/// user namespaces with more than just the single UID mapping.
///
/// When `allow_setgroups` is true (used for VM mode), writes "allow" to setgroups.
pub fn execute_idmap_for_pid(
    target_pid: Pid,
    uid: Uid,
    gid: Gid,
    opt_user: &Option<String>,
    allow_setgroups: bool,
) -> Result<()> {
    let uid_raw = uid.as_raw();
    let gid_raw = gid.as_raw();

    trace!("Executing ID mapping for PID {}", target_pid);

    // Check if newuidmap and newgidmap commands are available
    let has_newuidmap = utils::command_exists("newuidmap");
    let has_newgidmap = utils::command_exists("newgidmap");

    if has_newuidmap && has_newgidmap {
        // Try with newuidmap/newgidmap for subuid/subgid support
        match execute_newidmap_for_pid(target_pid, uid_raw, gid_raw, allow_setgroups) {
            Ok(()) => {
                debug!("Successfully used newuidmap/newgidmap for ID mapping");
                return Ok(());
            }
            Err(e) => {
                // Fall back to simple mapping
                warn!("newuidmap/newgidmap failed: {}. Using simple mapping.", e);
            }
        }
    }

    // Fallback: simple mapping (0 -> current uid/gid only)
    // This works when process writes its own /proc/self/uid_map,
    // but for external PID we need newuidmap.
    execute_simple_idmap_for_pid(target_pid, uid_raw, gid_raw, allow_setgroups)?;

    // Set environment variables if user was specified
    if let Some(user_spec) = opt_user {
        if user_spec.parse::<u32>().is_err() {
            std::env::set_var("USER", user_spec);
            std::env::set_var("LOGNAME", user_spec);
        }
    }

    Ok(())
}

/// Legacy helper that executes ID mapping for the current parent process.
pub fn execute_idmap_for_parent(
    uid: Uid,
    gid: Gid,
    opt_user: &Option<String>,
    allow_setgroups: bool,
) -> Result<()> {
    execute_idmap_for_pid(getppid(), uid, gid, opt_user, allow_setgroups)
}

/// Execute newuidmap/newgidmap for the target process.
fn execute_newidmap_for_pid(
    target_pid: Pid,
    uid_raw: u32,
    gid_raw: u32,
    allow_setgroups: bool,
) -> Result<()> {
    // Read subuid/subgid ranges
    let subuid_ranges = read_subid_ranges("/etc/subuid")?;
    let subgid_ranges = read_subid_ranges("/etc/subgid")?;

    trace!("Subuid ranges: {:?}", subuid_ranges);
    trace!("Subgid ranges: {:?}", subgid_ranges);

    // Write setgroups allow/deny before gid_map
    let setgroups_path = format!("/proc/{}/setgroups", target_pid.as_raw());
    let setgroups_val = if allow_setgroups { "allow" } else { "deny" };
    fs::write(&setgroups_path, setgroups_val)
        .map_err(|e| eyre::eyre!("Failed to write {}: {}", setgroups_path, e))?;

    // Build newuidmap args: <pid> <ns_id> <host_id> <count> ...
    // Map: 0 -> current_uid, 1+ -> subuid ranges
    let mut uid_args = vec![
        target_pid.as_raw().to_string(),
        "0".to_string(),
        uid_raw.to_string(),
        "1".to_string(),
    ];
    for (start, count) in &subuid_ranges {
        uid_args.push("1".to_string());
        uid_args.push(start.to_string());
        uid_args.push(count.to_string());
        break; // Use first range only
    }

    let mut gid_args = vec![
        target_pid.as_raw().to_string(),
        "0".to_string(),
        gid_raw.to_string(),
        "1".to_string(),
    ];
    for (start, count) in &subgid_ranges {
        gid_args.push("1".to_string());
        gid_args.push(start.to_string());
        gid_args.push(count.to_string());
        break;
    }

    // Execute newuidmap
    trace!("Executing newuidmap with args: {:?}", uid_args);
    let status = Command::new("newuidmap")
        .args(&uid_args)
        .status()
        .map_err(|e| eyre::eyre!("Failed to execute newuidmap: {}", e))?;
    if !status.success() {
        return Err(eyre::eyre!("newuidmap failed with status: {}", status));
    }

    // Execute newgidmap
    trace!("Executing newgidmap with args: {:?}", gid_args);
    let status = Command::new("newgidmap")
        .args(&gid_args)
        .status()
        .map_err(|e| eyre::eyre!("Failed to execute newgidmap: {}", e))?;
    if !status.success() {
        return Err(eyre::eyre!("newgidmap failed with status: {}", status));
    }

    Ok(())
}

/// Execute simple ID mapping for the target process (no subuid/subgid).
/// Maps UID/GID range 0-65535 if possible, falling back to single ID mapping.
fn execute_simple_idmap_for_pid(
    target_pid: Pid,
    uid_raw: u32,
    gid_raw: u32,
    allow_setgroups: bool,
) -> Result<()> {
    // Try to map a range of IDs (0-65535) to allow chown to system UIDs/GIDs
    // Format: ID-inside-ns ID-outside-ns LENGTH
    // First line: map ns root (0) to current user
    // Second line: map ns IDs 1-65535 to host IDs 1-65535
    let uid_map_full = format!("0 {} 1\n1 1 65535", uid_raw);
    let gid_map_full = format!("0 {} 1\n1 1 65535", gid_raw);

    let setgroups_path = format!("/proc/{}/setgroups", target_pid.as_raw());
    let setgroups_val = if allow_setgroups { "allow" } else { "deny" };
    fs::write(&setgroups_path, setgroups_val)
        .map_err(|e| eyre::eyre!("Failed to write {}: {}", setgroups_path, e))?;

    let uid_map_path = format!("/proc/{}/uid_map", target_pid.as_raw());
    let gid_map_path = format!("/proc/{}/gid_map", target_pid.as_raw());

    // Try full range mapping first
    debug!("Attempting full ID range map for PID {}: uid='{}', gid='{}'", target_pid, uid_map_full.trim(), gid_map_full.trim());

    if fs::write(&uid_map_path, &uid_map_full).is_ok() && fs::write(&gid_map_path, &gid_map_full).is_ok() {
        debug!("Successfully mapped full ID range 0-65535");
        return Ok(());
    }

    // Fall back to single ID mapping
    let uid_map_single = format!("0 {} 1", uid_raw);
    let gid_map_single = format!("0 {} 1", gid_raw);

    warn!("Full ID range mapping failed, falling back to single ID mapping");
    debug!("Setting simple ID map for PID {}: uid='{}', gid='{}'", target_pid, uid_map_single, gid_map_single);

    fs::write(&uid_map_path, &uid_map_single)
        .map_err(|e| eyre::eyre!("Failed to write {}: {}", uid_map_path, e))?;
    fs::write(&gid_map_path, &gid_map_single)
        .map_err(|e| eyre::eyre!("Failed to write {}: {}", gid_map_path, e))?;

    Ok(())
}

/// Write UID/GID mapping for the current process (self).
/// Called by a process after entering a new user namespace via clone(CLONE_NEWUSER) or unshare().
///
/// Note: For unprivileged users, only the simplest mapping (0 -> current_uid) is allowed
/// when writing directly. For subuid/subgid ranges, use newuidmap/newgidmap instead.
#[allow(dead_code)]
pub fn write_self_idmap(uid: Uid, gid: Gid) -> Result<()> {
    let uid_raw = uid.as_raw();
    let gid_raw = gid.as_raw();

    let uid_map = format!("0 {} 1", uid_raw);
    let gid_map = format!("0 {} 1", gid_raw);

    debug!("Writing self ID map: uid='{}', gid='{}'", uid_map, gid_map);

    fs::write("/proc/self/setgroups", "deny")
        .map_err(|e| eyre::eyre!("Failed to write /proc/self/setgroups: {}", e))?;

    fs::write("/proc/self/uid_map", &uid_map)
        .map_err(|e| eyre::eyre!("Failed to write /proc/self/uid_map: {}", e))?;

    fs::write("/proc/self/gid_map", &gid_map)
        .map_err(|e| eyre::eyre!("Failed to write /proc/self/gid_map: {}", e))?;

    debug!("Successfully wrote self ID map");
    Ok(())
}