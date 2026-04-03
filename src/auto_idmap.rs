//! Auto UID/GID mapping policy for VM mode.
//!
//! When a normal (non-root) host user runs a VM, files created inside the guest
//! appear with mismatched UIDs/GIDs on the host. This module provides automatic
//! generation of sensible UID/GID mappings for virtiofs/libkrun.
//!
//! # Policy
//!
//! - All guest UIDs are squashed to the current host UID
//! - All guest GIDs are squashed to the current host GID
//! - Host UID is mapped back to the requested guest UID (from --user option)
//! - Host GID is mapped back to the requested guest GID
//!
//! # Result
//!
//! - Files created by any user in guest → owned by host user on host filesystem
//! - Files owned by host user on host → appear as owned by --user in guest

/// Maximum UID/GID value for squash mapping
const MAX_ID: u32 = 4294967295;

/// Auto-generate UID/GID mapping specs for VM mode.
///
/// # Arguments
///
/// * `host_uid` - Current host user's UID
/// * `host_gid` - Current host user's GID
/// * `guest_user` - Requested guest user from --user option (None means root)
///
/// # Returns
///
/// (uid_specs, gid_specs) - Vectors of mapping spec strings for --translate-uid/gid
///
/// # Example
///
/// For host UID=501, GID=20, --user=root:
/// ```text
/// uid_specs: ["squash-guest:0:501:4294967295", "host:501:0:1"]
/// gid_specs: ["squash-guest:0:20:4294967295", "host:20:0:1"]
/// ```
pub fn auto_idmap_specs(
    host_uid: u32,
    host_gid: u32,
    guest_user: Option<&str>,
) -> (Vec<String>, Vec<String>) {
    let (guest_uid, guest_gid) = resolve_guest_user(guest_user);

    // Generate mappings:
    // 1. squash all guest UIDs/GIDs to host UID/GID
    // 2. reverse map host UID/GID to guest UID/GID
    let uid_specs = vec![
        format!("squash-guest:0:{}:{}", host_uid, MAX_ID),
        format!("host:{}:{}:1", host_uid, guest_uid),
    ];
    let gid_specs = vec![
        format!("squash-guest:0:{}:{}", host_gid, MAX_ID),
        format!("host:{}:{}:1", host_gid, guest_gid),
    ];

    (uid_specs, gid_specs)
}

/// Check if auto-mapping should be applied.
///
/// Returns true if:
/// - Host user is non-root (root doesn't need mapping)
/// - Running in VM mode (caller's responsibility to check)
pub fn should_auto_map(host_uid: u32) -> bool {
    host_uid != 0
}

/// Merge auto-generated specs with user-specified specs.
///
/// User-specified specs are appended after auto specs.
/// In virtiofsd, later rules have higher priority.
pub fn merge_idmap_specs(
    auto_specs: Vec<String>,
    user_specs: Vec<String>,
) -> Vec<String> {
    [auto_specs, user_specs].concat()
}

/// Resolve guest user string to (UID, GID).
///
/// - None or "root" → (0, 0)
/// - Numeric string like "1000" → (1000, 1000)  (GID defaults to UID)
/// - Username → TODO: lookup from environment's /etc/passwd
fn resolve_guest_user(user: Option<&str>) -> (u32, u32) {
    match user {
        None | Some("root") => (0, 0),
        Some(uid_str) if uid_str.parse::<u32>().is_ok() => {
            let uid = uid_str.parse().unwrap();
            (uid, uid) // Default GID = UID
        }
        Some(_username) => {
            // TODO: lookup from environment's /etc/passwd
            // For now, default to root
            (0, 0)
        }
    }
}

/// Convert ID map specs to a single newline-separated string.
///
/// Used for passing to libkrun FFI.
pub fn specs_to_string(specs: &[String]) -> String {
    specs.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auto_idmap_specs_root() {
        let (uid, gid) = auto_idmap_specs(501, 20, None);
        assert_eq!(uid, vec![
            "squash-guest:0:501:4294967295",
            "host:501:0:1",
        ]);
        assert_eq!(gid, vec![
            "squash-guest:0:20:4294967295",
            "host:20:0:1",
        ]);
    }

    #[test]
    fn test_auto_idmap_specs_user_1000() {
        let (uid, gid) = auto_idmap_specs(501, 20, Some("1000"));
        assert_eq!(uid, vec![
            "squash-guest:0:501:4294967295",
            "host:501:1000:1",
        ]);
        assert_eq!(gid, vec![
            "squash-guest:0:20:4294967295",
            "host:20:1000:1",
        ]);
    }

    #[test]
    fn test_should_auto_map() {
        assert!(!should_auto_map(0));  // root - no auto map
        assert!(should_auto_map(501)); // normal user - auto map
    }

    #[test]
    fn test_merge_idmap_specs() {
        let auto = vec!["auto1".to_string(), "auto2".to_string()];
        let user = vec!["user1".to_string()];
        let merged = merge_idmap_specs(auto, user);
        assert_eq!(merged, vec!["auto1", "auto2", "user1"]);
    }
}