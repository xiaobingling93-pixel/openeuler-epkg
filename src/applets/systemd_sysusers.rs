//! systemd-sysusers implementation status:
//!
//! CORRECT:
//! - Configuration file parsing with quoted field handling
//! - User creation with proper defaults (home="/", shell="/sbin/nologin")
//! - Group creation and membership management
//! - ID field parsing (uid, uid:gid, path-based resolution)
//! - User/group name validation
//! - Implicit user/group creation for member lines
//! - Locked account support (u! syntax)
//!
//! MISSING FEATURES (compared to C reference):
//! - Specifier expansion (%U, %G, etc. in all fields)
//! - Advanced field validation (GECOS, home/shell paths)
//! - Root directory support (--root, --image options)
//! - UID/GID allocation and collision detection
//! - File locking and backup creation
//! - Audit logging
//! - Credential-based configuration
//! - Duplicate/conflict detection between entries
//! - Range line processing ('r' lines)
//!
//! DIFFERENT APPROACH (acceptable):
//! - Uses external commands (useradd/groupadd/usermod) instead of direct file manipulation
//! - Ignores range lines (not needed for current use case)
//!
//! REFERENCES
//! - man sysusers.d
//! - man systemd-sysusers
//! - /usr/sbin/minsysusers
//! - /c/rpm-software-management/rpm/scripts/sysusers.sh
//! - /c/systemd/src/sysusers/sysusers.c
//! - /usr/lib/sysusers.d% tail *

use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use crate::posix::{posix_getpasswd, posix_getgroup};

pub struct SystemdSysusersOptions {
    pub config_files: Vec<String>,
    pub root: Option<PathBuf>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<SystemdSysusersOptions> {
    let config_files: Vec<String> = matches.get_many::<String>("config_files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();
    let root = matches.get_one::<PathBuf>("root").cloned();

    Ok(SystemdSysusersOptions { config_files, root })
}

pub fn command() -> Command {
    Command::new("systemd-sysusers")
        .about("Create system users and groups")
        .arg(Arg::new("root")
            .long("root")
            .value_name("DIR")
            .help("Operate on files relative to DIR"))
        .arg(Arg::new("config_files")
            .num_args(0..)
            .help("Configuration files to process"))
}

pub fn run(options: SystemdSysusersOptions) -> Result<()> {
    // If no config files specified, use default directories
    let config_files = if options.config_files.is_empty() {
        find_default_config_files(options.root.as_deref())?
    } else {
        options.config_files
    };

    for config_file in config_files {
        process_config_file(&config_file, options.root.as_deref())?;
    }

    Ok(())
}

/// Apply root prefix to a path if root is specified.
/// Strips leading slash from the path before joining with root.
pub fn apply_root(path: &str, root: Option<&Path>) -> PathBuf {
    match root {
        Some(root_dir) => {
            let path_without_slash = path.strip_prefix('/').unwrap_or(path);
            root_dir.join(path_without_slash)
        }
        None => PathBuf::from(path),
    }
}

fn find_default_config_files(root: Option<&Path>) -> Result<Vec<String>> {
    let mut files = Vec::new();

    // Standard directories in order of precedence (higher priority first)
    let dirs = vec![
        "/etc/sysusers.d",
        "/run/sysusers.d",
        "/usr/lib/sysusers.d",
    ];

    for dir in dirs {
        let full_dir = apply_root(dir, root);
        if let Ok(entries) = fs::read_dir(full_dir) {
            for entry in entries {
                if let Ok(entry) = entry {
                    let path = entry.path();
                    if let Some(ext) = path.extension() {
                        if ext == "conf" {
                            if let Some(path_str) = path.to_str() {
                                files.push(path_str.to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    // Sort files by name for consistent processing
    files.sort();
    Ok(files)
}

fn process_config_file(config_file: &str, _root: Option<&Path>) -> Result<()> {
    let content = fs::read_to_string(config_file)
        .map_err(|e| eyre!("Failed to read config file {}: {}", config_file, e))?;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        process_line(line)?;
    }

    Ok(())
}

fn parse_line_fields(line: &str) -> Result<Vec<String>> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '"' if !in_quotes => {
                in_quotes = true;
            }
            '"' if in_quotes => {
                in_quotes = false;
            }
            ' ' | '\t' if !in_quotes => {
                if !current.is_empty() {
                    fields.push(current);
                    current = String::new();
                }
                // Skip multiple whitespace
                while let Some(&next_ch) = chars.peek() {
                    if next_ch == ' ' || next_ch == '\t' {
                        chars.next();
                    } else {
                        break;
                    }
                }
            }
            _ => {
                current.push(ch);
            }
        }
    }

    if !current.is_empty() {
        fields.push(current);
    }

    Ok(fields)
}

fn process_line(line: &str) -> Result<()> {
    let parts = parse_line_fields(line)?;
    if parts.is_empty() {
        return Ok(());
    }

    let line_type = &parts[0];

    match line_type.as_str() {
        "u" | "u!" => process_user_line(&parts, line_type == "u!"),
        "g" => process_group_line(&parts),
        "m" => process_member_line(&parts),
        "r" => process_range_line(&parts),
        _ => {
            eprintln!("Warning: Unknown line type '{}'", line_type);
            Ok(())
        }
    }
}

fn process_user_line(parts: &[String], locked: bool) -> Result<()> {
    if parts.len() < 2 {
        return Err(eyre!("Invalid user line: not enough fields"));
    }

    let name = &parts[1];
    validate_user_group_name(name)?;
    let id_field    = parts.get(2).map(|s| s.as_str()).unwrap_or("-");
    let gecos       = parts.get(3).map(|s| s.as_str()).unwrap_or("-");
    let home        = parts.get(4).map(|s| s.as_str()).unwrap_or("-");
    let shell       = parts.get(5).map(|s| s.as_str()).unwrap_or("-");

    // Parse ID field - can be uid, uid:gid, uid:groupname, or path
    let (uid, gid) = parse_id_field(id_field)?;

    // Set defaults
    let gecos = if gecos == "-" { "" } else { gecos };
    let home = if home == "-" || home.is_empty() { "/" } else { home };
    let shell = if shell == "-" || shell.is_empty() { "/sbin/nologin" } else { shell };

    // Create user
    create_user(name, uid.as_deref(), gid.as_deref(), gecos, home, shell, locked)?;

    Ok(())
}

fn process_group_line(parts: &[String]) -> Result<()> {
    if parts.len() < 2 {
        return Err(eyre!("Invalid group line: not enough fields"));
    }

    let name = &parts[1];
    validate_user_group_name(name)?;
    let id_field = parts.get(2).map(|s| s.as_str()).unwrap_or("-");

    let gid = parse_gid_field(id_field)?;

    create_group(name, gid.as_deref())?;

    Ok(())
}

fn process_member_line(parts: &[String]) -> Result<()> {
    if parts.len() < 3 {
        return Err(eyre!("Invalid member line: not enough fields"));
    }

    let user = &parts[1];
    let group = &parts[2];

    validate_user_group_name(user)?;
    validate_user_group_name(group)?;

    add_user_to_group(user, group)?;

    Ok(())
}

fn process_range_line(_parts: &[String]) -> Result<()> {
    // Range lines are handled globally, not per-line
    // For now, we'll ignore them as the user said "no need support extra options"
    Ok(())
}

fn parse_id_field(id_field: &str) -> Result<(Option<String>, Option<String>)> {
    if id_field == "-" {
        return Ok((None, None));
    }

    if id_field.contains(':') {
        // uid:gid or uid:groupname format
        let parts: Vec<&str> = id_field.split(':').collect();
        if parts.len() != 2 {
            return Err(eyre!("Invalid ID field format: {}", id_field));
        }
        let uid = if parts[0] == "-" { None } else {
            validate_uid(parts[0])?;
            Some(parts[0].to_string())
        };
        let gid = if parts[1] == "-" { None } else { Some(parts[1].to_string()) };
        Ok((uid, gid))
    } else if Path::new(id_field).is_absolute() && Path::new(id_field).exists() {
        // Path format - get uid/gid from file
        let metadata = fs::metadata(id_field)
            .map_err(|e| eyre!("Failed to get metadata for {}: {}", id_field, e))?;
        let uid = metadata.uid().to_string();
        let gid = metadata.gid().to_string();
        Ok((Some(uid), Some(gid)))
    } else {
        // Just uid format
        validate_uid(id_field)?;
        Ok((Some(id_field.to_string()), None))
    }
}

fn validate_uid(uid_str: &str) -> Result<()> {
    uid_str.parse::<u32>()
        .map_err(|_| eyre!("Invalid UID format: {}", uid_str))?;
    Ok(())
}

fn parse_gid_field(id_field: &str) -> Result<Option<String>> {
    if id_field == "-" {
        return Ok(None);
    }

    if Path::new(id_field).is_absolute() && Path::new(id_field).exists() {
        // Path format - get gid from file
        let metadata = fs::metadata(id_field)
            .map_err(|e| eyre!("Failed to get metadata for {}: {}", id_field, e))?;
        Ok(Some(metadata.gid().to_string()))
    } else {
        // Try to parse as numeric GID first, otherwise treat as group name
        if id_field.chars().all(|c| c.is_ascii_digit()) {
            validate_gid(id_field)?;
        } else {
            validate_user_group_name(id_field)?;
        }
        Ok(Some(id_field.to_string()))
    }
}

fn validate_gid(gid_str: &str) -> Result<()> {
    gid_str.parse::<u32>()
        .map_err(|_| eyre!("Invalid GID format: {}", gid_str))?;
    Ok(())
}

fn validate_user_group_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(eyre!("User/group name cannot be empty"));
    }
    if name.len() > 32 {  // Typical limit
        return Err(eyre!("User/group name too long: {}", name));
    }
    if !name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.') {
        return Err(eyre!("Invalid characters in user/group name: {}", name));
    }
    if name.starts_with('-') || name.starts_with('.') {
        return Err(eyre!("User/group name cannot start with dash or dot: {}", name));
    }
    Ok(())
}


pub fn create_user(name: &str, uid: Option<&str>, gid: Option<&str>, gecos: &str, home: &str, shell: &str, locked: bool) -> Result<()> {
    // Check if user already exists
    if user_exists(name)? {
        return Ok(());
    }

    let mut cmd = ProcessCommand::new("useradd");
    cmd.arg("-r"); // system user

    if let Some(uid) = uid {
        cmd.arg("-u").arg(uid);
    }

    if let Some(gid) = gid {
        // Check if gid is numeric or group name
        if gid.chars().all(|c| c.is_ascii_digit()) {
            cmd.arg("-g").arg(gid);
        } else {
            cmd.arg("-g").arg(gid);
        }
    }

    cmd.arg("-d").arg(home);
    cmd.arg("-s").arg(shell);

    if !gecos.is_empty() {
        cmd.arg("-c").arg(gecos);
    }

    cmd.arg(name);

    let output = cmd.output()
        .map_err(|e| eyre!("Failed to execute useradd: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!("Warning: useradd failed for {}: {}", name, stderr);
    }

    if locked {
        // Lock the account
        let lock_output = ProcessCommand::new("usermod")
            .arg("-L")
            .arg(name)
            .output()
            .map_err(|e| eyre!("Failed to execute usermod -L: {}", e))?;

        if !lock_output.status.success() {
            let stderr = String::from_utf8_lossy(&lock_output.stderr);
            eprintln!("Warning: usermod -L failed for {}: {}", name, stderr);
        }
    }

    Ok(())
}

pub fn create_group(name: &str, gid: Option<&str>) -> Result<()> {
    // Check if group already exists
    if group_exists(name)? {
        return Ok(());
    }

    let mut cmd = ProcessCommand::new("groupadd");
    cmd.arg("-r"); // system group

    if let Some(gid) = gid {
        cmd.arg("-g").arg(gid);
    }

    cmd.arg(name);

    let output = cmd.output()
        .map_err(|e| eyre!("Failed to execute groupadd: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!("Warning: groupadd failed for {}: {}", name, stderr);
    }

    Ok(())
}

pub fn add_user_to_group(user: &str, group: &str) -> Result<()> {
    // Ensure group exists
    if !group_exists(group)? {
        create_group(group, None)?;
    }

    // Ensure user exists
    if !user_exists(user)? {
        create_user(user, None, None, "", "/", "/sbin/nologin", false)?;
    }

    let output = ProcessCommand::new("usermod")
        .arg("-a")
        .arg("-G")
        .arg(group)
        .arg(user)
        .output()
        .map_err(|e| eyre!("Failed to execute usermod: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!("Warning: usermod failed for user {} group {}: {}", user, group, stderr);
    }

    Ok(())
}

pub fn user_exists(name: &str) -> Result<bool> {
    match posix_getpasswd(Some(name), None) {
        Ok(_) => Ok(true),
        Err(_) => Ok(false),
    }
}

pub fn group_exists(name: &str) -> Result<bool> {
    match posix_getgroup(Some(name), None) {
        Ok(_) => Ok(true),
        Err(_) => Ok(false),
    }
}
