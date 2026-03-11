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
use color_eyre::eyre::WrapErr;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::userdb;
use crate::busybox::systemd_tmpfiles::find_default_config_files;

const SYSUSERS_DIRS: &[&str] = &[
    "/etc/sysusers.d",
    "/run/sysusers.d",
    "/usr/lib/sysusers.d",
];

pub struct SystemdSysusersOptions {
    pub config_files: Vec<String>,
    pub root: Option<PathBuf>,
}

// NOTE: systemd-sysusers previously relied on external useradd/groupadd/usermod
// via RunOptions. This created a cyclic dependency when those tools are
// themselves provided by epkg. We now implement the required user/group
// operations directly in Rust using the internal userdb helpers.

pub fn parse_options(matches: &clap::ArgMatches) -> Result<SystemdSysusersOptions> {
    let config_files: Vec<String> = matches.get_many::<String>("config_files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();
    let root = matches
        .get_one::<String>("root")
        .map(|s| PathBuf::from(s))
        .clone();

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
    // Always call find_default_config_files to get full paths to config files
    // It handles both explicit config files (relative/absolute) and default scanning
    let config_files = find_default_config_files(options.root.as_deref(), &options.config_files, SYSUSERS_DIRS)?;

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


fn process_config_file(config_file: &str, root: Option<&Path>) -> Result<()> {
    let full_path = apply_root(config_file, root);
    let content = fs::read_to_string(&full_path)
        .map_err(|e| eyre!("Failed to read config file {}: {}", full_path.display(), e))?;

    log::info!("systemd_sysusers: handling file {}", full_path.display());

    for (line_num, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        log::debug!("systemd_sysusers: processing line: {}", line);
        process_line(line, root)
            .wrap_err_with(|| format!("in file {}, line {}", full_path.display(), line_num + 1))?;
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

fn process_line(line: &str, root: Option<&Path>) -> Result<()> {
    let parts = parse_line_fields(line)?;
    if parts.is_empty() {
        return Ok(());
    }

    let line_type = &parts[0];

    match line_type.as_str() {
        "u" | "u!" => process_user_line(&parts, line_type == "u!", root),
        "g" => process_group_line(&parts, root),
        "m" => process_member_line(&parts, root),
        "r" => process_range_line(&parts),
        _ => {
            eprintln!("Warning: Unknown line type '{}'", line_type);
            Ok(())
        }
    }
}

fn process_user_line(parts: &[String], locked: bool, root: Option<&Path>) -> Result<()> {
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
    let home  = if home  == "-" || home.is_empty() { "/" } else { home };
    let shell = if shell == "-" || shell.is_empty() { "/sbin/nologin" } else { shell };

    // Create user
    userdb::create_user(name, uid.as_deref(), gid.as_deref(), gecos, home, shell, true, locked, root)?;

    Ok(())
}

fn process_group_line(parts: &[String], root: Option<&Path>) -> Result<()> {
    if parts.len() < 2 {
        return Err(eyre!("Invalid group line: not enough fields"));
    }

    let name = &parts[1];
    validate_user_group_name(name)?;
    let id_field = parts.get(2).map(|s| s.as_str()).unwrap_or("-");

    let gid = parse_gid_field(id_field)?;

    let _ = userdb::ensure_group(name, gid.as_deref(), true, root)?;

    Ok(())
}

fn process_member_line(parts: &[String], root: Option<&Path>) -> Result<()> {
    if parts.len() < 3 {
        return Err(eyre!("Invalid member line: not enough fields"));
    }

    let user = &parts[1];
    let group = &parts[2];

    validate_user_group_name(user)?;
    validate_user_group_name(group)?;

    userdb::add_user_to_group(user, group, root)?;

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

pub fn validate_user_group_name(name: &str) -> Result<()> {
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
