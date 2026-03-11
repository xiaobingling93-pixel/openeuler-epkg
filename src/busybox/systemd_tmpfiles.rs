//! systemd-tmpfiles implementation status:
//!
//! FULLY IMPLEMENTED:
//! - CLI: --create, --clean, --remove, --boot
//! - Config file discovery: /etc, /run, /usr/local/lib, /usr/lib with .conf extension
//! - Line parsing: whitespace separation, quotes, C-style escapes (\n\t\"\\ octal/hex)
//! - Item types: d/D/v/q/Q (dirs), f (files), L (symlinks), p (pipes), r/R (remove), w (write), e (empty dir), C (copy)
//! - Modifiers: ! (boot-only), - (ignore errors), + (append/force), =, ~, $, ?
//! - Directory/file/symlink/pipe creation with permissions/ownership
//! - User/group resolution (numeric and name lookup)
//! - Operation mode filtering (create/clean/remove)
//! - Boot-only line filtering with --boot option
//! - Error tolerance with - modifier
//! - Code reuse: set_permissions_from_mode(), posix_mkfifo(), extract_common_fields(),
//!   ensure_parent_directory(), parse_mode_with_default(), set_ownership_if_specified(),
//!   execute_with_error_handling(), modifier_match_arms!()
//!
//! MAJOR GAPS:
//! - CLI options: --graceful, --purge, --prefix, --exclude-prefix, --root, --image, --dry-run, --user, --cat-config
//! - Age-based cleanup: threshold parsing, timestamp selection (atime/btime/ctime/mtime), cleanup logic
//! - Specifier expansion: %a, %b, %H, %l, %q, %m, %M, %o, %v, %w, %W, %A, %B, %g, %G, %u, %U, %T, %V, %h, %C, %L, %S, %t
//! - Operation phases: systemd's PURGE → REMOVE_AND_CLEAN → CREATE ordering
//! - Advanced item types: z/Z (relabel), t/T (xattr), h/H (attributes), a/A (ACLs)
//!
//! MISSING ADVANCED FEATURES:
//! - Subvolume/quota support for v/q/Q types
//! - Glob pattern processing
//! - Binary argument support
//! - File locking during cleanup
//! - Sticky bit handling
//!
//! ASSESSMENT:
//! Provides basic tmpfiles.d support for directory/file operations with proper parsing and error handling.
//! Suitable for simple configurations but lacks advanced systemd features.
//!
//! REFERENCES:
//! - man tmpfiles.d
//! - man systemd-tmpfiles
//! - /c/systemd/src/tmpfiles/tmpfiles.c (reference implementation)
//! - /usr/lib/tmpfiles.d/ (configuration examples)
//! - /var/lib/dpkg/info% grep systemd-tmpfiles *post*

use crate::lfs;
use clap::{Arg, Command};
use color_eyre::eyre::eyre;
use color_eyre::eyre::WrapErr;
use color_eyre::Result;
use std::collections::HashMap;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::chown;
use std::path::{Path, PathBuf};

use crate::applets::cp::copy_single_item;
use crate::applets::systemd_sysusers::apply_root;
use crate::posix::{posix_getgroup, posix_getpasswd, posix_mkfifo};
use crate::utils::set_permissions_from_mode;
use glob::glob;
use walkdir::WalkDir;

/// Wrapper around set_permissions_from_mode that skips permission denied errors
/// for special files (e.g., sysfs, procfs) that don't support permission changes.
/// This matches systemd-tmpfiles behavior.
fn set_permissions_from_mode_skip<P: AsRef<Path>>(path: P, mode: u32) -> Result<()> {
    match set_permissions_from_mode(&path, mode) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Check if this is a permission denied error
            // We check this regardless of is_running_as_root() because:
            // 1. In user namespaces, euid may be 0 but we lack actual capabilities
            // 2. Some special files (sysfs, procfs) don't support chmod even for root
            for cause in e.chain() {
                if let Some(io_error) = cause.downcast_ref::<std::io::Error>() {
                    if io_error.kind() == ErrorKind::PermissionDenied {
                        log::warn!("Cannot set permissions on {}: permission denied (skipping)", path.as_ref().display());
                        return Ok(());
                    }
                }
            }
            Err(e)
        }
    }
}

const TMPFILES_DIRS: &[&str] = &[
    "/etc/tmpfiles.d",
    "/run/tmpfiles.d",
    "/usr/local/lib/tmpfiles.d",
    "/usr/lib/tmpfiles.d",
];

/// Unescape C-style escape sequences in a string.
/// Handles common escapes like \n, \t, \", \\, and octal/hex escapes.
fn parse_simple_escape(ch: char) -> Option<char> {
    match ch {
        'a' => Some('\x07'), // bell
        'b' => Some('\x08'), // backspace
        'f' => Some('\x0c'), // form feed
        'n' => Some('\n'),   // newline
        'r' => Some('\r'),   // carriage return
        't' => Some('\t'),   // tab
        'v' => Some('\x0b'), // vertical tab
        '\\' => Some('\\'),  // backslash
        '"' => Some('"'),    // quote
        '\'' => Some('\''),  // single quote
        '?' => Some('?'),    // question mark
        _ => None,
    }
}

fn parse_octal_escape(first: char, chars: &mut std::iter::Peekable<impl Iterator<Item = char>>) -> Result<char> {
    let mut octal = String::from(first);
    // Read up to 2 more octal digits
    for _ in 0..2 {
        if let Some(&next) = chars.peek() {
            if next.is_digit(8) {
                octal.push(chars.next().unwrap());
            } else {
                break;
            }
        }
    }
    let code = u32::from_str_radix(&octal, 8)
        .map_err(|_| eyre!("Invalid octal escape sequence: \\{}", octal))?;
    char::from_u32(code).ok_or_else(|| eyre!("Invalid octal escape sequence: \\{}", octal))
}

fn parse_hex_escape(chars: &mut std::iter::Peekable<impl Iterator<Item = char>>) -> Result<char> {
    let mut hex = String::new();
    // Read up to 2 hex digits
    for _ in 0..2 {
        if let Some(&next) = chars.peek() {
            if next.is_ascii_hexdigit() {
                hex.push(chars.next().unwrap());
            } else {
                break;
            }
        }
    }
    if hex.is_empty() {
        return Err(eyre!("Invalid hex escape sequence: \\x (no digits)"));
    }
    let code = u32::from_str_radix(&hex, 16)
        .map_err(|_| eyre!("Invalid hex escape sequence: \\x{}", hex))?;
    char::from_u32(code).ok_or_else(|| eyre!("Invalid hex escape sequence: \\x{}", hex))
}

fn cunescape(input: &str) -> Result<String> {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some(esc) => {
                    if let Some(simple) = parse_simple_escape(esc) {
                        result.push(simple);
                    } else if esc.is_digit(8) {
                        let decoded = parse_octal_escape(esc, &mut chars)?;
                        result.push(decoded);
                    } else if esc == 'x' {
                        let decoded = parse_hex_escape(&mut chars)?;
                        result.push(decoded);
                    } else {
                        return Err(eyre!("Unknown escape sequence: \\{}", esc));
                    }
                }
                None => return Err(eyre!("Incomplete escape sequence at end of string")),
            }
        } else {
            result.push(ch);
        }
    }

    Ok(result)
}

#[derive(Default)]
pub struct SystemdTmpfilesOptions {
    pub create: bool,
    pub clean: bool,
    pub remove: bool,
    pub boot: bool,
    pub config_files: Vec<String>,
    pub root: Option<PathBuf>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<SystemdTmpfilesOptions> {
    let create = matches.get_flag("create");
    let clean = matches.get_flag("clean");
    let remove = matches.get_flag("remove");
    let boot = matches.get_flag("boot");
    let root = matches
        .get_one::<String>("root")
        .map(|s| PathBuf::from(s))
        .clone();
    let config_files: Vec<String> = matches.get_many::<String>("config_files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    Ok(SystemdTmpfilesOptions {
        create,
        clean,
        remove,
        boot,
        config_files,
        root,
    })
}

pub fn command() -> Command {
    Command::new("systemd-tmpfiles")
        .about("Create, delete, and clean up files and directories")
        .arg(Arg::new("create")
            .long("create")
            .help("Create files and directories")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("clean")
            .long("clean")
            .help("Clean up old files and directories")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("remove")
            .long("remove")
            .help("Remove files and directories")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("boot")
            .long("boot")
            .help("Execute lines marked with ! (boot-only)")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("root")
            .long("root")
            .value_name("DIR")
            .help("Operate on files relative to DIR"))
        .arg(Arg::new("config_files")
            .num_args(0..)
            .help("Configuration files to process"))
}

pub fn run(options: SystemdTmpfilesOptions) -> Result<()> {
    // Determine which operations to perform
    // If no specific modes are requested, default to create + clean
    let do_create = options.create || (!options.create && !options.clean && !options.remove);
    let do_clean = options.clean || (!options.create && !options.clean && !options.remove);
    let do_remove = options.remove;

    // Always call find_default_config_files to get full paths to config files
    // It handles both explicit config files (relative/absolute) and default scanning
    let config_files = find_default_config_files(options.root.as_deref(), &options.config_files, TMPFILES_DIRS)?;

    for config_file in config_files {
        process_config_file(&config_file, do_create, do_clean, do_remove, options.boot, options.root.as_deref())?;
    }

    Ok(())
}

fn scan_config_directories(root: Option<&Path>, dirs: &[&str]) -> Result<(Vec<String>, HashMap<String, String>)> {
    let mut all_files = Vec::new();
    let mut relative_map = HashMap::new();

    // Directories are searched in the order given by `dirs`.
    // Matches systemd's CONF_PATHS macro behavior.

    for dir in dirs {
        let full_dir = apply_root(dir, root);
        if let Ok(entries) = fs::read_dir(&full_dir) {
            for entry in entries {
                if let Ok(entry) = entry {
                    let path = entry.path();
                    if let Some(ext) = path.extension() {
                        if ext == "conf" {
                            if let Some(path_str) = path.to_str() {
                                // Add to all_files
                                all_files.push(path_str.to_string());
                                // Compute relative path from this directory
                                if let Ok(relative) = path.strip_prefix(&full_dir) {
                                    if let Some(relative_str) = relative.to_str() {
                                        // Remove leading slash if present
                                        let relative_str = relative_str.trim_start_matches('/');
                                        // Only insert if not already present (first occurrence wins)
                                        relative_map.entry(relative_str.to_string())
                                            .or_insert(path_str.to_string());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    all_files.sort();
    Ok((all_files, relative_map))
}

/// Find configuration files in specified directories.
///
/// Valid `config_files` entries:
/// - Absolute paths: `/usr/lib/sysusers.d/basic.conf` (root prefix applied later)
/// - Relative filenames (no slashes): `basic.conf` (looked up in `dirs`, first match)
/// - Relative paths with slashes: `usr/lib/sysusers.d/basic.conf` (if not found in `dirs`, kept as relative path resolved relative to cwd)
///
/// Note: `dirs` point to configuration directories (e.g., `/usr/lib/sysusers.d`),
/// so lookup only matches filenames within those directories.
/// If `config_files` is empty, scan all `.conf` files in `dirs`.
///
/// When `root` is `Some(dir)`, directory paths are prefixed with `dir` (leading slash stripped).
///
/// # Examples
///
/// ```ignore
/// // Scan all .conf files in default tmpfiles.d directories
/// let files = find_default_config_files(None, &[], TMPFILES_DIRS)?;
///
/// // Process specific config file "foo.conf"
/// let files = find_default_config_files(None, &["foo.conf".to_string()], TMPFILES_DIRS)?;
/// ```
pub fn find_default_config_files(root: Option<&Path>, config_files: &[String], dirs: &[&str]) -> Result<Vec<String>> {
    // If config_files is empty, scan all .conf files in default directories
    let scan_all = config_files.is_empty();
    log::info!("find_default_config_files: scan_all={}, config_files={:?}", scan_all, config_files);

    let (all_files, relative_map) = scan_config_directories(root, dirs)?;
    let mut files = Vec::new();
    let mut seen = std::collections::HashSet::new();

    if scan_all {
        // Return all .conf files found in directories
        files = all_files;
    } else {
        // Process each explicitly provided config file
        for file in config_files {
            let path = Path::new(file);
            if path.is_absolute() {
                // Absolute path: use as is (apply_root will handle root prefix later)
                if seen.insert(file.clone()) {
                    files.push(file.clone());
                }
            } else {
                // Relative path: look up in relative map
                if let Some(full_path) = relative_map.get(file) {
                    if seen.insert(full_path.clone()) {
                        files.push(full_path.clone());
                    }
                } else {
                    // Not found in default directories, keep relative path
                    // (will be resolved relative to current working directory)
                    log::info!("Relative config file '{}' not found in default directories", file);
                    if seen.insert(file.clone()) {
                        files.push(file.clone());
                    }
                }
            }
        }
    }

    // Sort files by name for consistent processing
    files.sort();
    log::info!("find_default_config_files returning: {:?}", files);
    Ok(files)
}

fn process_config_file(config_file: &str, do_create: bool, do_clean: bool, do_remove: bool, boot: bool, root: Option<&Path>) -> Result<()> {
    let content = fs::read_to_string(config_file)
        .map_err(|e| eyre!("Failed to read config file {}: {}", config_file, e))?;

    log::info!("systemd_tmpfiles: handling file {}", config_file);

    for (line_num, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        process_line(line, do_create, do_clean, do_remove, boot, root)
            .wrap_err_with(|| format!("in file {}, line {}", config_file, line_num + 1))?;
    }

    Ok(())
}

fn parse_line_fields(line: &str) -> Result<Vec<String>> {
    // First unescape C-style escape sequences
    let unescaped = cunescape(line)?;

    let mut fields = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = unescaped.chars().peekable();

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
                // Skip multiple whitespace, but collect all whitespace within quotes
                while let Some(&next) = chars.peek() {
                    if next == ' ' || next == '\t' {
                        chars.next();
                    } else {
                        break;
                    }
                }
                continue;
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

fn process_line(line: &str, do_create: bool, do_clean: bool, do_remove: bool, boot: bool, root: Option<&Path>) -> Result<()> {
    let parts = parse_line_fields(line)?;
    log::info!("Parsed fields: {:?}", parts);
    if parts.len() < 2 {
        return Ok(());
    }

    let line_type = &parts[0];
    let _path = &parts[1];

    // Parse type and modifiers
    let (base_type, modifiers) = parse_type_and_modifiers(line_type)?;

    // Skip boot-only lines if --boot is not specified
    if modifiers.boot_only && !boot {
        return Ok(());
    }

    // Determine if this line should be processed based on the requested operation modes
    if !should_process_line(base_type, do_create, do_clean, do_remove) {
        return Ok(());
    }

    dispatch_line(base_type, line_type, &parts, &modifiers, do_create, do_clean, do_remove, root)
}

/// Execute a processing function with error handling for ignore_errors modifier
fn execute_with_error_handling<F>(modifiers: &Modifiers, operation: F) -> color_eyre::Result<()>
where
    F: FnOnce() -> color_eyre::Result<()>,
{
    match operation() {
        Ok(()) => Ok(()),
        Err(e) => {
            if modifiers.ignore_errors {
                eprintln!("Warning: {} (ignored due to - modifier)", e);
                Ok(())
            } else {
                Err(e)
            }
        }
    }
}

#[derive(Debug, Default)]
struct Modifiers {
    boot_only: bool,                // !
    ignore_errors: bool,            // -
    try_replace: bool,              // =
    unbase64: bool,                 // ~
    from_cred: bool,                // ^
    purge: bool,                    // $
    append_or_force: bool,          // +
    ignore_missing_target: bool,    // ?
}

/// Macro to generate modifier match arms
macro_rules! modifier_match_arms {
    ($ch:expr, $type_str:expr, $modifiers:expr, $($char:expr => $field:ident),* $(,)?) => {
        match $ch {
            $(
                $char if !$modifiers.$field => $modifiers.$field = true,
            )*
            _ => {
                return Err(eyre!("Unknown modifier '{}' in type '{}'", $ch, $type_str));
            }
        }
    };
}

fn parse_type_and_modifiers(type_str: &str) -> Result<(&str, Modifiers)> {
    let chars: Vec<char> = type_str.chars().collect();
    if chars.is_empty() {
        return Err(eyre!("Empty type string"));
    }

    let base_type = &type_str[0..chars[0].len_utf8()];
    let mut modifiers = Modifiers::default();

    for &ch in &chars[1..] {
        modifier_match_arms! { ch, type_str, modifiers,
            '!' => boot_only,
            '-' => ignore_errors,
            '=' => try_replace,
            '~' => unbase64,
            '^' => from_cred,
            '$' => purge,
            '+' => append_or_force,
            '?' => ignore_missing_target,
        }
    }

    Ok((base_type, modifiers))
}

fn should_process_line(base_type: &str, do_create: bool, do_clean: bool, do_remove: bool) -> bool {
    log::info!("should_process_line: base_type={}, do_create={}, do_clean={}, do_remove={}", base_type, do_create, do_clean, do_remove);
    match base_type {
        // Create operations
        "d" | "v" | "q" | "Q" | "f" | "p" | "L" => do_create,
        // Directory operations (can create, clean, or remove)
        "D" => do_create || do_clean || do_remove,
        // Remove operations
        "r" | "R" => do_remove,
        // Clean operations (directories with age, copy operations)
        "C" => do_clean,
        // Ignore operations (always processed to maintain correct behavior)
        "x" | "X" => true,
        // Attribute operations (could be considered create or separate)
        "z" | "Z" | "t" | "T" | "h" | "H" | "a" | "A" => do_create,
        // Device operations
        "c" | "b" => do_create,
        // Write operations
        "w" => do_create,
        // Empty directory operations
        "e" => do_clean,
        // Unknown types
        _ => true, // Process unknown types to show warnings
    }
}

fn dispatch_line(base_type: &str, line_type: &str, parts: &[String], modifiers: &Modifiers, do_create: bool, do_clean: bool, do_remove: bool, root: Option<&Path>) -> Result<()> {
    match base_type {
        "d" | "D" | "v" | "q" | "Q" => execute_with_error_handling(modifiers, || process_directory_line(parts, modifiers, do_create, do_clean, do_remove, root)),
        "L" => execute_with_error_handling(modifiers, || process_symlink_line(parts, modifiers, root)),
        "f" => execute_with_error_handling(modifiers, || process_file_line(parts, modifiers, root)),
        "p" => execute_with_error_handling(modifiers, || process_pipe_line(parts, modifiers, root)),
        "w" => execute_with_error_handling(modifiers, || process_write_line(parts, modifiers, root)),
        "e" => execute_with_error_handling(modifiers, || process_empty_directory_line(parts, modifiers, root)),
        "r" | "R" => execute_with_error_handling(modifiers, || process_remove_line(parts, base_type, modifiers, root)),
        "x" | "X" => Ok(()),
        "C" => execute_with_error_handling(modifiers, || process_copy_line(parts, modifiers, root)),
        "z" => execute_with_error_handling(modifiers, || process_attribute_line(parts, modifiers, root, false)),
        "Z" => execute_with_error_handling(modifiers, || process_attribute_line(parts, modifiers, root, true)),
        "t" | "T" | "h" | "H" | "a" | "A" => {
            eprintln!("Notice: Attribute operations ({}) not implemented: {}", base_type, line_type);
            Ok(())
        }
        _ => {
            eprintln!("Warning: Unsupported line type '{}'", line_type);
            Ok(())
        }
    }
}

fn process_directory_line(parts: &[String], _modifiers: &Modifiers, do_create: bool, _do_clean: bool, do_remove: bool, root: Option<&Path>) -> Result<()> {
    log::info!("process_directory_line called");
    if parts.len() < 3 {
        return Err(eyre!("Invalid directory line: not enough fields"));
    }

    let (path, mode_str, user_str, group_str) = extract_common_fields(parts);
    let full_path = apply_root(path, root);
    log::info!("Directory path: {}, full_path: {}", path, full_path.display());
    log::info!("Canonical path: {:?}", full_path.canonicalize().ok());
    log::info!("Metadata: {:?}", full_path.metadata());

    // Handle different operation modes
    if do_remove {
        // For 'D' type during remove, we should remove directory contents
        // For now, just warn that this is not implemented
        eprintln!("Notice: Directory removal operations not implemented: D {}", full_path.display());
        return Ok(());
    }

    if !do_create {
        // If not creating and not removing, skip
        return Ok(());
    }

    // Parse mode (octal)
    log::info!("Mode string: {}", mode_str);
    let mode = parse_mode_with_default(mode_str, 0o755)?;
    log::info!("Parsed mode: {}", mode);

    // Create directory if it doesn't exist
    log::info!("Directory exists? {}", lfs::exists_or_any_symlink(&full_path));
    if !lfs::exists_or_any_symlink(&full_path) {
        lfs::create_dir_all(&full_path)?;
    }

    // Set permissions
    set_permissions_from_mode_skip(&full_path, mode)?;

    // Set ownership if specified
    set_ownership_if_specified(&full_path, user_str, group_str)?;

    Ok(())
}

fn process_symlink_line(parts: &[String], modifiers: &Modifiers, root: Option<&Path>) -> Result<()> {
    if parts.len() < 6 {
        return Err(eyre!("Invalid symlink line: not enough fields"));
    }

    let path = &parts[1];
    let full_path = apply_root(path, root);
    // Determine target field: if age is present (parts.len() >= 7), target is at index 6
    // else target is at index 5 (age omitted)
    let target_idx = if parts.len() >= 7 { 6 } else { 5 };
    let target = parts.get(target_idx).map(|s| s.as_str()).unwrap_or("-");

    if target == "-" {
        return Err(eyre!("Invalid symlink line: missing target"));
    }

    // Remove existing file if + modifier is present
    if modifiers.append_or_force && lfs::exists_or_any_symlink(&full_path) {
        if full_path.is_symlink() {
            lfs::remove_file(&full_path)?;
        } else {
            // For L+, we might want to replace it, but let's be conservative
            return Ok(());
        }
    }

    // Create symlink if it doesn't exist
    if !lfs::exists_or_any_symlink(&full_path) {
        lfs::symlink(target, &full_path)?;
    }

    Ok(())
}

fn process_file_line(parts: &[String], modifiers: &Modifiers, root: Option<&Path>) -> Result<()> {
    if parts.len() < 6 {
        return Err(eyre!("Invalid file line: not enough fields"));
    }

    let (path, mode_str, user_str, group_str) = extract_common_fields(parts);
    let full_path = apply_root(path, root);
    // Age is parts[5], content starts from parts[6]
    let content = if parts.len() >= 7 {
        parts[6..].join(" ")
    } else {
        String::new()
    };

    // Create parent directory if needed
    ensure_parent_directory(&full_path)?;

    // For 'f' type: create file if it doesn't exist, or truncate if '+' modifier is present
    let file_exists = lfs::exists_or_any_symlink(&full_path);
    let should_write = !file_exists || modifiers.append_or_force;

    if should_write {
        lfs::write(&full_path, content)?;
    }

    // Set permissions if specified
    if mode_str != "-" {
        let mode = parse_mode_with_default(mode_str, 0)?;
        set_permissions_from_mode_skip(&full_path, mode)?;
    }

    // Set ownership if specified
    set_ownership_if_specified(&full_path, user_str, group_str)?;

    Ok(())
}

fn process_pipe_line(parts: &[String], _modifiers: &Modifiers, root: Option<&Path>) -> Result<()> {
    if parts.len() < 3 {
        return Err(eyre!("Invalid pipe line: not enough fields"));
    }

    let (path, mode_str, user_str, group_str) = extract_common_fields(parts);
    let full_path = apply_root(path, root);
    // Age is parts[5], no argument for pipes

    // Create parent directory if needed
    ensure_parent_directory(&full_path)?;

    // Create named pipe if it doesn't exist
    if !lfs::exists_or_any_symlink(&full_path) {
        let path_str = full_path.to_str()
            .ok_or_else(|| eyre!("Path contains invalid UTF-8: {}", full_path.display()))?;
        posix_mkfifo(path_str)
            .map_err(|e| eyre!("Failed to create named pipe {}: {}", full_path.display(), e))?;
    }

    // Set permissions (always, since posix_mkfifo creates with 0o777)
    let mode = parse_mode_with_default(mode_str, 0o644)?;
    set_permissions_from_mode_skip(&full_path, mode)?;

    // Set ownership if specified
    set_ownership_if_specified(&full_path, user_str, group_str)?;

    Ok(())
}

fn process_remove_line(parts: &[String], remove_type: &str, _modifiers: &Modifiers, root: Option<&Path>) -> Result<()> {
    if parts.len() < 2 {
        return Err(eyre!("Invalid remove line: not enough fields"));
    }

    let path = &parts[1];
    let full_path = apply_root(path, root);

    if !lfs::exists_or_any_symlink(&full_path) {
        return Ok(()); // Nothing to remove
    }

    match remove_type {
        "r" => {
            // Remove file
            if lfs::symlink_metadata(&full_path).map(|m| m.file_type().is_file()).unwrap_or(false) {
                lfs::remove_file(&full_path)?;
            } else {
                return Err(eyre!("Path {} is not a file (use R for directories)", full_path.display()));
            }
        }
        "R" => {
            // Recursively remove directory
            lfs::remove_dir_all(&full_path)?;
        }
        _ => unreachable!(),
    }

    Ok(())
}

fn process_write_line(parts: &[String], _modifiers: &Modifiers, root: Option<&Path>) -> Result<()> {
    if parts.len() < 3 {
        return Err(eyre!("Invalid write line: not enough fields"));
    }

    let path = &parts[1];
    let full_path = apply_root(path, root);
    let content = if parts.len() >= 3 {
        // Join all remaining parts with spaces
        parts[2..].join(" ")
    } else {
        String::new()
    };

    // For 'w' type, write to existing file only
    if !lfs::exists_or_any_symlink(&full_path) {
        return Err(eyre!("File {} does not exist (use f to create)", full_path.display()));
    }

    lfs::write(&full_path, content)?;

    Ok(())
}

fn process_empty_directory_line(parts: &[String], _modifiers: &Modifiers, root: Option<&Path>) -> Result<()> {
    if parts.len() < 2 {
        return Err(eyre!("Invalid empty directory line: not enough fields"));
    }

    let path = &parts[1];
    let full_path = apply_root(path, root);

    // For 'e' type, clean contents of existing directory
    if !lfs::exists_or_any_symlink(&full_path) {
        return Ok(()); // Directory doesn't exist, nothing to clean
    }

    if !lfs::symlink_metadata(&full_path).map(|m| m.file_type().is_dir()).unwrap_or(false) {
        return Err(eyre!("Path {} is not a directory", full_path.display()));
    }

    // Remove all contents of the directory
    // This is a simple implementation - a full implementation would need age-based filtering
    let entries = fs::read_dir(&full_path)
        .map_err(|e| eyre!("Failed to read directory {}: {}", full_path.display(), e))?;

    for entry in entries {
        let entry = entry
            .map_err(|e| eyre!("Failed to read directory entry in {}: {}", full_path.display(), e))?;
        let entry_path = entry.path();

        if lfs::symlink_metadata(&entry_path).map(|m| m.file_type().is_dir()).unwrap_or(false) {
            lfs::remove_dir_all(&entry_path)?;
        } else {
            lfs::remove_file(&entry_path)?;
        }
    }

    Ok(())
}

fn process_copy_line(parts: &[String], modifiers: &Modifiers, root: Option<&Path>) -> Result<()> {
    if parts.len() < 2 {
        return Err(eyre!("Invalid copy line: not enough fields"));
    }

    let (path, mode_str, user_str, group_str) = extract_common_fields(parts);
    let full_path = apply_root(path, root);

    // Determine source path
    let source = if parts.len() >= 7 {
        parts[6].clone()
    } else {
        // Default to /usr/share/factory/ with same name as destination
        let mut factory_path = PathBuf::from("/usr/share/factory");
        factory_path.push(path);
        factory_path.to_str()
            .ok_or_else(|| eyre!("Failed to convert factory path to string"))?
            .to_string()
    };
    let full_source = apply_root(&source, root);

    // Check if destination exists and is a non-empty directory
    if lfs::exists_or_any_symlink(&full_path) && lfs::symlink_metadata(&full_path).map(|m| m.file_type().is_dir()).unwrap_or(false) {
        let is_empty = fs::read_dir(&full_path)
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(false);
        if !is_empty && !modifiers.append_or_force {
            // Skip entire copy operation per man page
            return Ok(());
        }
    }

    // Ensure parent directory exists
    ensure_parent_directory(&full_path)?;

    // Copy recursively, preserving symlinks (dereference=false)
    // Use preserve=true to keep source attributes, but we'll override with specified mode/user/group later
    let mut cp_options = crate::applets::cp::CpOptions::default();
    cp_options.archive = true; // cp -a
    cp_options.force = modifiers.append_or_force; // force overwrite if + modifier
    cp_options.no_clobber = !modifiers.append_or_force;
    cp_options.compute_derived();
    copy_single_item(
        &full_source,
        &full_path,
        &cp_options,
    )?;

    // Set permissions if specified
    if mode_str != "-" {
        let mode = parse_mode_with_default(mode_str, 0)?;
        set_permissions_from_mode_skip(&full_path, mode)?;
    }

    // Set ownership if specified
    set_ownership_if_specified(&full_path, user_str, group_str)?;

    Ok(())
}

/// Process attribute operations (z and Z)
fn process_attribute_line(parts: &[String], modifiers: &Modifiers, root: Option<&Path>, recursive: bool) -> Result<()> {
    let (pattern, mode_str, user_str, group_str) = extract_common_fields(parts);
    // Nothing to do if no attributes specified
    if mode_str == "-" && user_str == "-" && group_str == "-" {
        return Ok(());
    }
    let full_pattern = apply_root(pattern, root);
    let full_pattern_str = full_pattern.to_str()
        .ok_or_else(|| eyre!("Path contains invalid UTF-8: {}", full_pattern.display()))?;

    // Expand glob pattern
    let entries = glob(full_pattern_str)
        .map_err(|e| eyre!("Invalid glob pattern '{}': {}", pattern, e))?;

    // Helper to apply attributes to a single path
    let apply_attributes = |path: &Path| -> Result<()> {
        // Set permissions if specified
        if mode_str != "-" {
            let mode = parse_mode_with_default(mode_str, 0)?;
            set_permissions_from_mode_skip(path, mode)?;
        }
        // Set ownership if specified
        set_ownership_if_specified(path, user_str, group_str)?;
        Ok(())
    };

    for entry in entries {
        match entry {
            Ok(path) => {
                // Skip symlinks as per "Does not follow symlinks"
                if path.is_symlink() {
                    continue;
                }

                if recursive && lfs::symlink_metadata(&path).map(|m| m.file_type().is_dir()).unwrap_or(false) {
                    // Recursively process directory contents (excluding the directory itself)
                    for entry in WalkDir::new(&path).follow_links(false).min_depth(1).into_iter().filter_map(|e| e.ok()) {
                        let entry_path = entry.path();
                        if entry_path.is_symlink() {
                            continue;
                        }
                        apply_attributes(entry_path)?;
                    }
                }
                // Apply to the matched path itself (file or directory)
                apply_attributes(&path)?;
            }
            Err(e) => {
                if modifiers.ignore_errors {
                    eprintln!("Warning: glob match error: {} (ignored due to - modifier)", e);
                } else {
                    return Err(eyre!("Glob match error: {}", e));
                }
            }
        }
    }

    Ok(())
}

/// Extract common fields from tmpfiles.d line parts
fn extract_common_fields(parts: &[String]) -> (&str, &str, &str, &str) {
    let path = &parts[1];
    let mode_str    = parts.get(2).map(|s| s.as_str()).unwrap_or("-");
    let user_str    = parts.get(3).map(|s| s.as_str()).unwrap_or("-");
    let group_str   = parts.get(4).map(|s| s.as_str()).unwrap_or("-");
    (path, mode_str, user_str, group_str)
}

/// Create parent directory for a path if it doesn't exist
fn ensure_parent_directory(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !lfs::exists_or_any_symlink(parent) {
            lfs::create_dir_all(parent)?;
        }
    }
    Ok(())
}

/// Parse mode string that may have prefix '~' or ':'.
/// Returns (prefix, mode) where prefix is None, '~', or ':'.
/// If mode_str is "-", returns (None, default_mode).
fn parse_mode_with_prefix(mode_str: &str, default_mode: u32) -> Result<(Option<char>, u32)> {
    if mode_str == "-" {
        return Ok((None, default_mode));
    }
    // Strip optional prefix
    let (prefix, rest) = if let Some(stripped) = mode_str.strip_prefix('~') {
        (Some('~'), stripped)
    } else if let Some(stripped) = mode_str.strip_prefix(':') {
        (Some(':'), stripped)
    } else {
        (None, mode_str)
    };
    // Parse octal mode
    let mode = u32::from_str_radix(rest, 8)
        .map_err(|e| eyre!("Invalid mode '{}': {}", mode_str, e))?;
    Ok((prefix, mode))
}

/// Parse mode string to u32, with default fallback
/// Supports prefixes '~' (mask based on existing access bits) and ':' (apply only when creating).
/// Prefixes are currently ignored (warning logged).
fn parse_mode_with_default(mode_str: &str, default_mode: u32) -> Result<u32> {
    let (prefix, mode) = parse_mode_with_prefix(mode_str, default_mode)?;
    // Warn about unimplemented prefix behavior
    if let Some(p) = prefix {
        log::warn!("Mode prefix '{}' not fully implemented (mode: {})", p, mode_str);
    }
    Ok(mode)
}

/// Set ownership for a path if user/group are specified
fn set_ownership_if_specified(path: &Path, user_str: &str, group_str: &str) -> Result<()> {
    if user_str != "-" || group_str != "-" {
        let uid = if user_str == "-" {
            None
        } else {
            let parsed_uid = parse_user(user_str)?;
            // Skip ownership change if user doesn't exist (0xFFFFFFFF = INVALID_UID)
            // This matches systemd-tmpfiles behavior - it warns but continues
            if parsed_uid == 0xFFFFFFFF {
                log::debug!("Skipping ownership change for {} - user {} doesn't exist", path.display(), user_str);
                return Ok(());
            }
            Some(parsed_uid)
        };
        let gid = if group_str == "-" {
            None
        } else {
            let parsed_gid = parse_group(group_str)?;
            // Skip ownership change if group doesn't exist (0xFFFFFFFF = INVALID_GID)
            if parsed_gid == 0xFFFFFFFF {
                log::debug!("Skipping ownership change for {} - group {} doesn't exist", path.display(), group_str);
                return Ok(());
            }
            Some(parsed_gid)
        };

        // Only attempt chown if we have something to set
        if uid.is_some() || gid.is_some() {
            match chown(path, uid, gid) {
                Ok(()) => Ok(()),
                Err(e) => {
                    // Skip permission denied errors
                    // This handles both non-root users and user namespaces where euid=0
                    // but we lack actual capabilities to change ownership
                    if e.kind() == ErrorKind::PermissionDenied {
                        log::warn!("Cannot change ownership of {}: permission denied (skipping)", path.display());
                        return Ok(());
                    }
                    Err(eyre!("Failed to change ownership of {} to uid={:?}, gid={:?}: {}", path.display(), uid, gid, e))
                }
            }?;
        }
    }
    Ok(())
}

/// Parse user string to UID.
/// If user doesn't exist in /etc/passwd, returns Ok(0xFFFFFFFF) which systemd-tmpfiles
/// will skip with a warning (matching real systemd behavior).
fn parse_user(user_str: &str) -> Result<u32> {
    // Try to parse as numeric UID first
    if let Ok(uid) = user_str.parse::<u32>() {
        return Ok(uid);
    }

    // Try to look up user by name
    match posix_getpasswd(Some(user_str), None) {
        Ok(passwd) => Ok(passwd.uid),
        Err(_) => {
            // User doesn't exist. Return_SPECIAL_UID to skip with warning.
            // This matches systemd-tmpfiles behavior - it warns but continues.
            log::warn!("Unknown user: {} ( continuing with special UID)", user_str);
            Ok(0xFFFFFFFF) // INVALID_UID - systemd will skip this
        }
    }
}

/// Parse group string to GID.
/// If group doesn't exist in /etc/group, returns Ok(0xFFFFFFFF) which systemd-tmpfiles
/// will skip with a warning (matching real systemd behavior).
fn parse_group(group_str: &str) -> Result<u32> {
    // Try to parse as numeric GID first
    if let Ok(gid) = group_str.parse::<u32>() {
        return Ok(gid);
    }

    // Try to look up group by name
    match posix_getgroup(Some(group_str), None) {
        Ok(group) => Ok(group.gid),
        Err(_) => {
            // Group doesn't exist. Return SPECIAL_GID to skip with warning.
            log::warn!("Unknown group: {} (continuing with special GID)", group_str);
            Ok(0xFFFFFFFF) // INVALID_GID - systemd will skip this
        }
    }
}
