use std::fs;
use std::os::fd::OwnedFd;

use color_eyre::eyre;
use color_eyre::Result;
use log::{debug, trace};
use nix::unistd::{pipe, Pid, Uid, Gid};

/// Unified sync helper for passing a file descriptor to child process.
/// The child checks if sync_read_fd is Some to know if it needs to write its own ID maps.
pub struct IdMapSync {
    read_fd: OwnedFd,
    #[allow(dead_code)]
    write_fd: OwnedFd,
}

impl IdMapSync {
    pub fn new(_target_pid: Pid) -> Result<Self> {
        let (read_fd, write_fd) = pipe()?;
        Ok(Self { read_fd, write_fd })
    }

    pub fn read_fd(&self) -> &OwnedFd {
        &self.read_fd
    }
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

/// Write UID/GID mapping for the current process (self).
/// Called by a process after entering a new user namespace via clone(CLONE_NEWUSER) or unshare().
/// The process has CAP_SETUID in its own user namespace and can write /proc/self/uid_map.
///
/// Note: For unprivileged users, only the simplest mapping (0 -> current_uid) is allowed.
/// To map subuid/subgid ranges, you would need newuidmap/newgidmap (setuid binaries)
/// which check /etc/subuid and /etc/subgid for authorization.
pub fn write_self_idmap(uid: Uid, gid: Gid) -> Result<()> {
    let uid_raw = uid.as_raw();
    let gid_raw = gid.as_raw();

    // Format: "inside_id outside_id count"
    // Map root (0) inside namespace to our real UID/GID outside.
    // For unprivileged users, this is the only mapping allowed when writing directly.
    // To map subuid/subgid ranges, newuidmap/newgidmap (setuid) must be used.
    let uid_map = format!("0 {} 1", uid_raw);
    let gid_map = format!("0 {} 1", gid_raw);

    debug!("Writing self ID map: uid_map='{}', gid_map='{}'", uid_map, gid_map);

    // Must write setgroups deny before gid_map
    fs::write("/proc/self/setgroups", "deny")
        .map_err(|e| eyre::eyre!("Failed to write /proc/self/setgroups: {}", e))?;

    fs::write("/proc/self/uid_map", &uid_map)
        .map_err(|e| eyre::eyre!("Failed to write /proc/self/uid_map: {}", e))?;

    fs::write("/proc/self/gid_map", &gid_map)
        .map_err(|e| eyre::eyre!("Failed to write /proc/self/gid_map: {}", e))?;

    debug!("Successfully wrote self ID map");
    Ok(())
}