use color_eyre::Result;
use color_eyre::eyre::eyre;
use log;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct PasswdEntry {
    pub name:       String,
    pub passwd:     String,
    pub uid:        u32,
    pub gid:        u32,
    pub gecos:      String,
    pub dir:        String,
    pub shell:      String,
}

#[derive(Debug, Clone)]
pub struct GroupEntry {
    pub name:       String,
    pub passwd:     String,
    pub gid:        u32,
    pub members:    Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ShadowEntry {
    pub name:           String,
    pub passwd:         String,
    pub last_change:    Option<String>,
    pub min_days:       Option<String>,
    pub max_days:       Option<String>,
    pub warn_days:      Option<String>,
    pub inactive_days:  Option<String>,
    pub expire_date:    Option<String>,
    pub reserved:       Option<String>,
}

fn passwd_path(root: Option<&Path>) -> PathBuf {
    root.unwrap_or_else(|| Path::new("/")).join("etc/passwd")
}

fn group_path(root: Option<&Path>) -> PathBuf {
    root.unwrap_or_else(|| Path::new("/")).join("etc/group")
}

fn shadow_path(root: Option<&Path>) -> PathBuf {
    root.unwrap_or_else(|| Path::new("/")).join("etc/shadow")
}

fn gshadow_path(root: Option<&Path>) -> PathBuf {
    root.unwrap_or_else(|| Path::new("/")).join("etc/gshadow")
}

fn read_lines(path: &Path) -> Result<Vec<String>> {
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Vec::new());
        }
        Err(e) => return Err(eyre!("Failed to open {}: {}", path.display(), e)),
    };
    let reader = BufReader::new(file);
    let mut lines = Vec::new();
    for line in reader.lines() {
        let line = line?;
        lines.push(line);
    }
    Ok(lines)
}

fn write_lines_atomic(path: &Path, lines: &[String]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp_path = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp_path)?;
        for line in lines {
            writeln!(f, "{}", line)?;
        }
    }
    fs::rename(&tmp_path, path)?;
    Ok(())
}

pub fn read_passwd(root: Option<&Path>) -> Result<Vec<PasswdEntry>> {
    let path = passwd_path(root);
    let mut entries = Vec::new();
    for line in read_lines(&path)? {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split(':').collect();
        if parts.len() < 7 {
            continue;
        }
        let uid = parts[2].parse::<u32>().unwrap_or(0);
        let gid = parts[3].parse::<u32>().unwrap_or(0);
        entries.push(PasswdEntry {
            name:   parts[0].to_string(),
            passwd: parts[1].to_string(),
            uid,
            gid,
            gecos:  parts[4].to_string(),
            dir:    parts[5].to_string(),
            shell:  parts[6].to_string(),
        });
    }
    Ok(entries)
}

pub fn write_passwd(root: Option<&Path>, entries: &[PasswdEntry]) -> Result<()> {
    let path = passwd_path(root);
    let mut lines = Vec::new();
    for e in entries {
        lines.push(format!(
            "{}:{}:{}:{}:{}:{}:{}",
            e.name, e.passwd, e.uid, e.gid, e.gecos, e.dir, e.shell
        ));
    }
    write_lines_atomic(&path, &lines)
}

pub fn read_group(root: Option<&Path>) -> Result<Vec<GroupEntry>> {
    let path = group_path(root);
    let mut entries = Vec::new();
    for line in read_lines(&path)? {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split(':').collect();
        if parts.len() < 3 {
            continue;
        }
        let gid = parts[2].parse::<u32>().unwrap_or(0);
        let members = if parts.len() >= 4 && !parts[3].is_empty() {
            parts[3].split(',').map(|s| s.to_string()).collect()
        } else {
            Vec::new()
        };
        entries.push(GroupEntry {
            name: parts[0].to_string(),
            passwd: if parts.len() >= 2 { parts[1].to_string() } else { "x".to_string() },
            gid,
            members,
        });
    }
    Ok(entries)
}

pub fn write_group(root: Option<&Path>, entries: &[GroupEntry]) -> Result<()> {
    let path = group_path(root);
    let mut lines = Vec::new();
    for e in entries {
        let members = if e.members.is_empty() {
            String::new()
        } else {
            e.members.join(",")
        };
        lines.push(format!("{}:{}:{}:{}", e.name, e.passwd, e.gid, members));
    }
    write_lines_atomic(&path, &lines)
}

fn extract_optional_field(parts: &[&str], index: usize) -> Option<String> {
    parts.get(index).filter(|s| !s.is_empty()).map(|s| s.to_string())
}

pub fn read_shadow(root: Option<&Path>) -> Result<Vec<ShadowEntry>> {
    let path = shadow_path(root);
    let mut entries = Vec::new();
    for line in read_lines(&path)? {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split(':').collect();
        if parts.len() < 2 {
            continue;
        }
        // Shadow file has 9 fields: name:passwd:last_change:min_days:max_days:warn_days:inactive_days:expire_date:reserved
        entries.push(ShadowEntry {
            name:           parts[0].to_string(),
            passwd:         parts[1].to_string(),
            last_change:    extract_optional_field(&parts, 2),
            min_days:       extract_optional_field(&parts, 3),
            max_days:       extract_optional_field(&parts, 4),
            warn_days:      extract_optional_field(&parts, 5),
            inactive_days:  extract_optional_field(&parts, 6),
            expire_date:    extract_optional_field(&parts, 7),
            reserved:       extract_optional_field(&parts, 8),
        });
    }
    Ok(entries)
}

fn format_shadow_entry(entry: &ShadowEntry) -> String {
    // Format: username:password:last_change:min_days:max_days:warn_days:inactive_days:expire_date:reserved
    format!(
        "{}:{}:{}:{}:{}:{}:{}:{}:{}",
        entry.name,
        entry.passwd,
        entry.last_change   .as_deref().unwrap_or(""),
        entry.min_days      .as_deref().unwrap_or(""),
        entry.max_days      .as_deref().unwrap_or(""),
        entry.warn_days     .as_deref().unwrap_or(""),
        entry.inactive_days .as_deref().unwrap_or(""),
        entry.expire_date   .as_deref().unwrap_or(""),
        entry.reserved      .as_deref().unwrap_or("")
    )
}

pub fn write_shadow(root: Option<&Path>, entries: &[ShadowEntry]) -> Result<()> {
    let path = shadow_path(root);
    let existing_lines = read_lines(&path)?;
    let mut lines = Vec::new();
    let entries_to_update: HashMap<String, &ShadowEntry> =
        entries.iter().map(|e| (e.name.clone(), e)).collect();

    // Process existing lines, updating entries we're managing
    for line in existing_lines {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            lines.push(line.to_string());
            continue;
        }
        let parts: Vec<&str> = line.split(':').collect();
        if parts.len() >= 1 {
            if let Some(entry) = entries_to_update.get(parts[0]) {
                // Update this entry - merge existing fields with new entry
                // Preserve existing fields if not set in new entry
                let merged = ShadowEntry {
                    name:               entry.name.clone(),
                    passwd:             entry.passwd.clone(),
                    last_change:        entry.last_change   .clone().or_else(|| extract_optional_field(&parts, 2)),
                    min_days:           entry.min_days      .clone().or_else(|| extract_optional_field(&parts, 3)),
                    max_days:           entry.max_days      .clone().or_else(|| extract_optional_field(&parts, 4)),
                    warn_days:          entry.warn_days     .clone().or_else(|| extract_optional_field(&parts, 5)),
                    inactive_days:      entry.inactive_days .clone().or_else(|| extract_optional_field(&parts, 6)),
                    expire_date:        entry.expire_date   .clone().or_else(|| extract_optional_field(&parts, 7)),
                    reserved:           entry.reserved      .clone().or_else(|| extract_optional_field(&parts, 8)),
                };
                lines.push(format_shadow_entry(&merged));
                continue;
            }
        }
        // Preserve existing entry unchanged
        lines.push(line.to_string());
    }

    // Add new entries that didn't exist
    for entry in entries {
        if !lines.iter().any(|l| {
            l.split(':').next().map(|n| n == entry.name).unwrap_or(false)
        }) {
            lines.push(format_shadow_entry(entry));
        }
    }

    write_lines_atomic(&path, &lines)
}


fn collect_used_uids(entries: &[PasswdEntry]) -> HashSet<u32> {
    entries.iter().map(|e| e.uid).collect()
}

fn collect_used_gids(entries: &[GroupEntry]) -> HashSet<u32> {
    entries.iter().map(|e| e.gid).collect()
}

fn next_free_id(used: &HashSet<u32>, start: u32) -> u32 {
    let mut id = start;
    while used.contains(&id) {
        id += 1;
    }
    id
}

pub fn next_free_uid(root: Option<&Path>, system: bool) -> Result<u32> {
    let entries = read_passwd(root)?;
    let used = collect_used_uids(&entries);
    let base = if system { 100 } else { 1000 };
    Ok(next_free_id(&used, base))
}

pub fn next_free_gid(root: Option<&Path>, system: bool) -> Result<u32> {
    let entries = read_group(root)?;
    let used = collect_used_gids(&entries);
    let base = if system { 100 } else { 1000 };
    Ok(next_free_id(&used, base))
}

pub fn ensure_group(
    name: &str,
    gid_str: Option<&str>,
    system: bool,
    root: Option<&Path>,
) -> Result<GroupEntry> {
    let mut groups = read_group(root)?;
    if let Some(g) = groups.iter().find(|g| g.name == name) {
        return Ok(g.clone());
    }

    let gid = if let Some(gid_s) = gid_str {
        gid_s.parse::<u32>().map_err(|e| eyre!("Invalid gid {}: {}", gid_s, e))?
    } else {
        next_free_gid(root, system)?
    };

    let entry = GroupEntry {
        name: name.to_string(),
        passwd: "x".to_string(),
        gid,
        members: Vec::new(),
    };
    groups.push(entry.clone());
    write_group(root, &groups)?;
    Ok(entry)
}

fn resolve_uid(uid_str: Option<&str>, system: bool, root: Option<&Path>) -> Result<u32> {
    if let Some(uid_s) = uid_str {
        uid_s.parse::<u32>().map_err(|e| eyre!("Invalid uid {}: {}", uid_s, e))
    } else {
        next_free_uid(root, system)
    }
}

fn resolve_gid(gid_str: Option<&str>, username: &str, system: bool, root: Option<&Path>) -> Result<u32> {
    if let Some(g_str) = gid_str {
        if g_str.chars().all(|c| c.is_ascii_digit()) {
            g_str.parse::<u32>().map_err(|e| eyre!("Invalid gid {}: {}", g_str, e))
        } else {
            // treat as group name
            let g = ensure_group(g_str, None, system, root)?;
            Ok(g.gid)
        }
    } else {
        // prefer group with same name, or allocate new one
        let groups = read_group(root)?;
        let existing_gid = groups
            .iter()
            .find(|g| g.name == username)
            .map(|g| g.gid);
        match existing_gid {
            Some(id) => Ok(id),
            None => {
                let g = ensure_group(username, None, system, root)?;
                Ok(g.gid)
            }
        }
    }
}

fn days_since_epoch() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() / 86400
}

fn create_shadow_entry(name: &str, locked: bool, root: Option<&Path>) -> Result<()> {
    let mut shadows = read_shadow(root)?;
    if !shadows.iter().any(|s| s.name == name) {
        // Shadow password field handling (matching C implementation in useradd.c):
        // - Default value is "!" for locked accounts (line 116: user_pass = "!")
        // - Set via -p option for encrypted passwords (line 1361: user_pass = optarg)
        // - In new_spent(), shadow password is set from user_pass (line 961: spent->sp_pwdp = user_pass)
        // For locked accounts, use "!" (matches C code default)
        // For unlocked accounts, would use encrypted password, but system accounts should be locked
        let shadow_passwd = if locked { "!".to_string() } else { "!".to_string() };

        // Set last_change to current date in days since epoch (Jan 1, 1970)
        // This matches Debian's format where entries have a last_change date
        let last_change_days = days_since_epoch();

        shadows.push(ShadowEntry {
            name: name.to_string(),
            passwd: shadow_passwd,
            last_change: Some(last_change_days.to_string()),
            min_days: None,
            max_days: None,
            warn_days: None,
            inactive_days: None,
            expire_date: None,
            reserved: None,
        });
        write_shadow(root, &shadows)?;
    }
    Ok(())
}

pub fn create_user(
    name: &str,
    uid_str: Option<&str>,
    gid_str: Option<&str>,
    gecos: &str,
    home: &str,
    shell: &str,
    system: bool,
    locked: bool,
    root: Option<&Path>,
) -> Result<()> {
    let mut users = read_passwd(root)?;
    if users.iter().any(|u| u.name == name) {
        return Ok(());
    }

    let uid = resolve_uid(uid_str, system, root)?;
    let gid = resolve_gid(gid_str, name, system, root)?;

    let passwd_path = passwd_path(root);
    log::info!(
        "userdb::create_user: name={} uid={} gid={} home={} shell={} system={} locked={} passwd_path={}",
        name,
        uid,
        gid,
        home,
        shell,
        system,
        locked,
        passwd_path.display()
    );
    // Passwd field handling (matching C implementation in useradd.c):
    // - When shadow is enabled (is_shadow_pwd), use SHADOW_PASSWD_STRING ("x") (line 939)
    // - When shadow is not enabled, use user_pass ("!") (line 941)
    // Since we create shadow entries, shadow is enabled, so use "x"
    let passwd_field = "x".to_string();

    users.push(PasswdEntry {
        name: name.to_string(),
        passwd: passwd_field,
        uid,
        gid,
        gecos: gecos.to_string(),
        dir: home.to_string(),
        shell: shell.to_string(),
    });
    write_passwd(root, &users)?;

    // Create shadow entry for system users
    if system {
        create_shadow_entry(name, locked, root)?;
    }

    Ok(())
}

pub fn add_user_to_group(name: &str, group: &str, root: Option<&Path>) -> Result<()> {
    let users = read_passwd(root)?;
    if !users.iter().any(|u| u.name == name) {
        // create minimal locked system user with nologin
        create_user(
            name,
            None,
            None,
            "",
            "/",
            "/sbin/nologin",
            true,
            true,
            root,
        )?;
    }

    let mut groups = read_group(root)?;
    let mut found = false;
    for g in groups.iter_mut() {
        if g.name == group {
            found = true;
            if !g.members.iter().any(|m| m == name) {
                g.members.push(name.to_string());
            }
            break;
        }
    }
    if !found {
        let mut g = ensure_group(group, None, true, root)?;
        if !g.members.iter().any(|m| m == name) {
            g.members.push(name.to_string());
        }
        groups.push(g);
    }
    write_group(root, &groups)?;
    Ok(())
}

pub fn delete_user(name: &str, remove_home: bool, root: Option<&Path>) -> Result<()> {
    let mut users = read_passwd(root)?;
    let home_dir = users
        .iter()
        .find(|u| u.name == name)
        .map(|u| u.dir.clone());

    if !users.iter().any(|u| u.name == name) {
        return Ok(());
    }
    users.retain(|u| u.name != name);
    write_passwd(root, &users)?;

    // Remove from all groups
    let mut groups = read_group(root)?;
    for g in groups.iter_mut() {
        g.members.retain(|m| m != name);
    }
    write_group(root, &groups)?;

    // Also delete from shadow if it exists
    let _ = delete_shadow_entry(name, root);

    if remove_home {
        if let Some(home) = home_dir {
            let path = root.unwrap_or_else(|| Path::new("/")).join(home.trim_start_matches('/'));
            let _ = fs::remove_dir_all(path);
        }
    }

    Ok(())
}

pub fn delete_group(name: &str, only_if_empty: bool, root: Option<&Path>) -> Result<()> {
    let mut groups = read_group(root)?;
    let mut changed = false;
    groups.retain(|g| {
        if g.name != name {
            true
        } else if only_if_empty && !g.members.is_empty() {
            true
        } else {
            changed = true;
            false
        }
    });
    if changed {
        write_group(root, &groups)?;
        // Also delete from gshadow if it exists
        let _ = delete_gshadow_entry(name, root);
    }
    Ok(())
}

fn delete_gshadow_entry(name: &str, root: Option<&Path>) -> Result<()> {
    let path = gshadow_path(root);
    let existing_lines = read_lines(&path)?;
    let mut lines = Vec::new();
    let mut changed = false;

    for line in existing_lines {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            lines.push(line.to_string());
            continue;
        }
        let parts: Vec<&str> = line.split(':').collect();
        if parts.len() >= 1 && parts[0] == name {
            changed = true;
            // Skip this line (delete it)
        } else {
            lines.push(line.to_string());
        }
    }
    if changed {
        write_lines_atomic(&path, &lines)?;
    }
    Ok(())
}

fn delete_shadow_entry(name: &str, root: Option<&Path>) -> Result<()> {
    let path = shadow_path(root);
    let existing_lines = read_lines(&path)?;
    let mut lines = Vec::new();
    let mut changed = false;

    for line in existing_lines {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            lines.push(line.to_string());
            continue;
        }
        let parts: Vec<&str> = line.split(':').collect();
        if parts.len() >= 1 && parts[0] == name {
            changed = true;
            // Skip this line (delete it)
        } else {
            lines.push(line.to_string());
        }
    }
    if changed {
        write_lines_atomic(&path, &lines)?;
    }
    Ok(())
}

pub fn remove_user_from_group(
    user: &str,
    group: &str,
    remove_empty_group: bool,
    root: Option<&Path>,
) -> Result<()> {
    let groups = read_group(root)?;
    let mut changed = false;
    let mut new_groups = Vec::new();

    for mut g in groups.into_iter() {
        if g.name == group {
            let before = g.members.len();
            g.members.retain(|m| m != user);
            let after = g.members.len();

            if after != before {
                changed = true;
            }

            if remove_empty_group && g.members.is_empty() {
                // drop group
                changed = true;
                continue;
            }
        }
        new_groups.push(g);
    }

    if changed {
        write_group(root, &new_groups)?;
    }
    Ok(())
}

pub fn modify_user(
    name: &str,
    comment: Option<&str>,
    home: Option<&str>,
    primary_group: Option<&str>,
    shell: Option<&str>,
    root: Option<&Path>,
) -> Result<()> {
    let mut users = read_passwd(root)?;

    if !users.iter().any(|u| u.name == name) {
        // C code errors if user doesn't exist in passwd file
        return Err(eyre!("user '{}' does not exist in {}", name, passwd_path(root).display()));
    }

    let mut new_gid: Option<u32> = None;
    if let Some(pg) = primary_group {
        let groups = read_group(root)?;
        let gid = if pg.chars().all(|c| c.is_ascii_digit()) {
            // Numeric GID provided - verify the group exists
            let gid_val = pg.parse::<u32>().map_err(|e| eyre!("Invalid gid {}: {}", pg, e))?;
            groups
                .iter()
                .find(|g| g.gid == gid_val)
                .ok_or_else(|| eyre!("Group with GID {} does not exist", gid_val))?
                .gid
        } else {
            // Group name provided
            let g = ensure_group(pg, None, true, root)?;
            g.gid
        };
        new_gid = Some(gid);
    }

    for u in users.iter_mut() {
        if u.name == name {
            if let Some(c) = comment {
                u.gecos = c.to_string();
            }
            if let Some(h) = home {
                // C code removes trailing '/' if length > 1 (usermod.c:542-545)
                let mut h_clean = h.to_string();
                if h_clean.len() > 1 && h_clean.ends_with('/') {
                    h_clean.pop();
                }
                u.dir = h_clean;
            }
            if let Some(s) = shell {
                u.shell = s.to_string();
            }
            if let Some(id) = new_gid {
                u.gid = id;
            }
        }
    }

    write_passwd(root, &users)?;
    Ok(())
}

/// Get passwd entry for a given UID by parsing /etc/passwd directly.
/// This works in statically linked binaries where getpwuid() may fail.
#[cfg(unix)]
fn get_passwd_entry_by_uid(uid: u32, root: Option<&Path>) -> Result<PasswdEntry> {
    let entries = read_passwd(root)?;
    for entry in entries {
        if entry.uid == uid {
            return Ok(entry);
        }
    }
    Err(eyre!("UID {} not found in passwd file", uid))
}

/// Get username for a given UID by parsing /etc/passwd directly.
/// This works in statically linked binaries where getpwuid() may fail.
#[cfg(unix)]
pub fn get_username_by_uid(uid: u32, root: Option<&Path>) -> Result<String> {
    Ok(get_passwd_entry_by_uid(uid, root)?.name)
}

/// Get home directory for a given UID by parsing /etc/passwd directly.
/// This works in statically linked binaries where getpwuid() may fail.
#[cfg(unix)]
pub fn get_home_by_uid(uid: u32, root: Option<&Path>) -> Result<String> {
    Ok(get_passwd_entry_by_uid(uid, root)?.dir)
}

/// Get group entry for a given GID by parsing /etc/group directly.
/// This works in statically linked binaries where getgrgid() may fail.
#[cfg(unix)]
fn get_group_entry_by_gid(gid: u32, root: Option<&Path>) -> Result<GroupEntry> {
    let entries = read_group(root)?;
    for entry in entries {
        if entry.gid == gid {
            return Ok(entry);
        }
    }
    Err(eyre!("GID {} not found in group file", gid))
}

/// Get group name for a given GID by parsing /etc/group directly.
/// This works in statically linked binaries where getgrgid() may fail.
#[cfg(unix)]
pub fn get_groupname_by_gid(gid: u32, root: Option<&Path>) -> Result<String> {
    Ok(get_group_entry_by_gid(gid, root)?.name)
}

pub fn user_exists(name: &str, root: Option<&Path>) -> Result<bool> {
    entry_exists(name, "passwd", root)
}

pub fn group_exists(name: &str, root: Option<&Path>) -> Result<bool> {
    entry_exists(name, "group", root)
}

fn entry_exists(name: &str, filename: &str, root: Option<&Path>) -> Result<bool> {
    let file_path = match root {
        Some(root) => root.join(format!("etc/{}", filename)),
        None => PathBuf::from(format!("/etc/{}", filename)),
    };
    let file = match fs::File::open(&file_path) {
        Ok(f) => f,
        Err(_) => return Ok(false), // file doesn't exist, entry doesn't exist
    };
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = line.map_err(|e| eyre!("Failed to read {} file: {}", filename, e))?;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split(':').collect();
        if parts.len() >= 1 && parts[0] == name {
            return Ok(true);
        }
    }
    Ok(false)
}
