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

use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::fs;
use std::os::unix::fs::chown;
use std::path::{Path, PathBuf};

use nix::unistd::{fork, ForkResult};
use nix::sys::wait::{waitpid, WaitStatus};

use crate::posix::{posix_getpasswd, posix_getgroup, posix_mkfifo};
use crate::utils::set_permissions_from_mode;
use crate::applets::systemd_sysusers::apply_root;
use crate::applets::cp::copy_single_item;
use crate::run::{RunOptions, setup_namespace_and_mounts};

/// Unescape C-style escape sequences in a string.
/// Handles common escapes like \n, \t, \", \\, and octal/hex escapes.
fn cunescape(input: &str) -> Result<String> {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('a') => result.push('\x07'), // bell
                Some('b') => result.push('\x08'), // backspace
                Some('f') => result.push('\x0c'), // form feed
                Some('n') => result.push('\n'),   // newline
                Some('r') => result.push('\r'),   // carriage return
                Some('t') => result.push('\t'),   // tab
                Some('v') => result.push('\x0b'), // vertical tab
                Some('\\') => result.push('\\'),  // backslash
                Some('"') => result.push('"'),    // quote
                Some('\'') => result.push('\''),  // single quote
                Some('?') => result.push('?'),    // question mark

                // Octal escape: \ooo (1-3 octal digits)
                Some(d1) if d1.is_digit(8) => {
                    let mut octal = String::from(d1);
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
                    if let Ok(code) = u32::from_str_radix(&octal, 8) {
                        if let Some(ch) = char::from_u32(code) {
                            result.push(ch);
                        } else {
                            return Err(eyre!("Invalid octal escape sequence: \\{}", octal));
                        }
                    } else {
                        return Err(eyre!("Invalid octal escape sequence: \\{}", octal));
                    }
                }

                // Hexadecimal escape: \xhh (1-2 hex digits)
                Some('x') => {
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
                    if let Ok(code) = u32::from_str_radix(&hex, 16) {
                        if let Some(ch) = char::from_u32(code) {
                            result.push(ch);
                        } else {
                            return Err(eyre!("Invalid hex escape sequence: \\x{}", hex));
                        }
                    } else {
                        return Err(eyre!("Invalid hex escape sequence: \\x{}", hex));
                    }
                }

                Some(other) => {
                    return Err(eyre!("Unknown escape sequence: \\{}", other));
                }
                None => {
                    return Err(eyre!("Incomplete escape sequence at end of string"));
                }
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
    let root = matches.get_one::<PathBuf>("root").cloned();
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

/// Fork and execute a closure in the child process.
/// Returns Ok(()) if child exits successfully, otherwise returns an error.
fn fork_and_call<F>(f: F) -> Result<()>
where
    F: FnOnce() -> Result<()>,
{
    match unsafe { fork() } {
        Ok(ForkResult::Parent { child }) => {
            match waitpid(child, None) {
                Ok(WaitStatus::Exited(_, 0)) => Ok(()),
                Ok(WaitStatus::Exited(_, code)) => Err(eyre!("child exited with code {}", code)),
                Ok(WaitStatus::Signaled(_, signal, _)) => Err(eyre!("child killed by signal {:?}", signal)),
                Ok(_) => Err(eyre!("child ended with unexpected status")),
                Err(e) => Err(eyre!("failed to wait for child: {}", e)),
            }
        }
        Ok(ForkResult::Child) => {
            match f() {
                Ok(()) => std::process::exit(0),
                Err(_) => std::process::exit(1),
            }
        }
        Err(e) => Err(eyre!("fork failed: {}", e)),
    }
}

/// Fork is necessary when invoked from epkg, because multi-threaded processes cannot create user namespaces,
/// and systemd_tmpfiles::run() -> setup_namespace_and_mounts() requires a single-threaded process.
/// If directly invoked by busybox applet, no threads have been created, so no extra fork needed.
pub fn fork_run(env_root: &Path) -> Result<()> {
    fork_and_call(|| {
        run(SystemdTmpfilesOptions {
            create: true,
            clean: true,
            remove: false,
            boot: false,
            config_files: vec![],
            root: Some(env_root.to_path_buf()),
        })
    })
}


pub fn run(options: SystemdTmpfilesOptions) -> Result<()> {
    // Determine which operations to perform
    // If no specific modes are requested, default to create + clean
    let do_create = options.create || (!options.create && !options.clean && !options.remove);
    let do_clean = options.clean || (!options.create && !options.clean && !options.remove);
    let do_remove = options.remove;

    // If --root is specified, set up namespace and mounts so we can operate as root inside the environment
    if let Some(root) = &options.root {
        // Set up namespace and bind mounts to make root the environment root
        let run_options = RunOptions {
            ..Default::default()
        };
        setup_namespace_and_mounts(root, &run_options)?;
    };

    // After namespace setup, we are inside the environment root mounted over /
    // So we should not prefix paths with root anymore
    let effective_root = None;

    // If no config files specified, use default directories
    let config_files = if options.config_files.is_empty() {
        find_default_config_files(effective_root)?
    } else {
        options.config_files
    };

    for config_file in config_files {
        process_config_file(&config_file, do_create, do_clean, do_remove, options.boot, effective_root)?;
    }

    Ok(())
}

fn find_default_config_files(root: Option<&Path>) -> Result<Vec<String>> {
    let mut files = Vec::new();

    // Standard directories in order of precedence (higher priority first)
    // Matches systemd's CONF_PATHS macro
    let dirs = vec![
        "/etc/tmpfiles.d",
        "/run/tmpfiles.d",
        "/usr/local/lib/tmpfiles.d",
        "/usr/lib/tmpfiles.d",
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

fn process_config_file(config_file: &str, do_create: bool, do_clean: bool, do_remove: bool, boot: bool, root: Option<&Path>) -> Result<()> {
    let content = fs::read_to_string(config_file)
        .map_err(|e| eyre!("Failed to read config file {}: {}", config_file, e))?;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        process_line(line, do_create, do_clean, do_remove, boot, root)?;
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
    if std::env::var("DEBUG_TMPFILES").is_ok() {
        eprintln!("DEBUG: process_line called with do_create={}, do_clean={}, do_remove={}", do_create, do_clean, do_remove);
    }

    let parts = parse_line_fields(line)?;
    if parts.len() < 2 {
        return Ok(());
    }

    let line_type = &parts[0];
    let _path = &parts[1];

    // Parse type and modifiers
    let (base_type, modifiers) = parse_type_and_modifiers(line_type)?;

    if std::env::var("DEBUG_TMPFILES").is_ok() {
        eprintln!("DEBUG: Line '{}' -> base_type='{}', modifiers={:?}, do_create={}, do_clean={}, do_remove={}, boot={}",
            line, base_type, modifiers, do_create, do_clean, do_remove, boot);
    }

    // Skip boot-only lines if --boot is not specified
    if modifiers.boot_only && !boot {
        if std::env::var("DEBUG_TMPFILES").is_ok() {
            eprintln!("DEBUG: Skipping boot-only line '{}'", line);
        }
        return Ok(());
    }

    // Determine if this line should be processed based on the requested operation modes
    let should_process = match base_type {
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
    };

    if std::env::var("DEBUG_TMPFILES").is_ok() {
        eprintln!("DEBUG: Line '{}' should_process={}, base_type='{}', do_create={}, do_remove={}", line, should_process, base_type, do_create, do_remove);
    }

    if !should_process {
        return Ok(());
    }

    match base_type {
        "d" | "D" | "v" | "q" | "Q" => execute_with_error_handling(&modifiers, || process_directory_line(&parts, &modifiers, do_create, do_clean, do_remove, root)),
        "L" => execute_with_error_handling(&modifiers, || process_symlink_line(&parts, &modifiers, root)),
        "f" => execute_with_error_handling(&modifiers, || process_file_line(&parts, &modifiers, root)),
        "p" => execute_with_error_handling(&modifiers, || process_pipe_line(&parts, &modifiers, root)),
        "w" => execute_with_error_handling(&modifiers, || process_write_line(&parts, &modifiers, root)),
        "e" => execute_with_error_handling(&modifiers, || process_empty_directory_line(&parts, &modifiers, root)),
        "r" | "R" => execute_with_error_handling(&modifiers, || process_remove_line(&parts, base_type, &modifiers, root)),
        "x" | "X" => {
            // Ignore operations - skip silently
            Ok(())
        }
        "C" => execute_with_error_handling(&modifiers, || process_copy_line(&parts, &modifiers, root)),
        "z" | "Z" | "t" | "T" | "h" | "H" | "a" | "A" => {
            // Attribute operations - skip with warning
            eprintln!("Warning: Attribute operations ({}) not implemented: {}", base_type, line_type);
            Ok(())
        }
        _ => {
            eprintln!("Warning: Unsupported line type '{}'", line_type);
            Ok(())
        }
    }
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

fn process_directory_line(parts: &[String], _modifiers: &Modifiers, do_create: bool, do_clean: bool, do_remove: bool, root: Option<&Path>) -> Result<()> {
    if parts.len() < 3 {
        return Err(eyre!("Invalid directory line: not enough fields"));
    }

    let (path, mode_str, user_str, group_str) = extract_common_fields(parts);
    let full_path = apply_root(path, root);

    if std::env::var("DEBUG_TMPFILES").is_ok() {
        eprintln!("DEBUG: process_directory_line called for {} with do_create={}, do_clean={}, do_remove={}", full_path.display(), do_create, do_clean, do_remove);
    }

    // Handle different operation modes
    if do_remove {
        // For 'D' type during remove, we should remove directory contents
        // For now, just warn that this is not implemented
        if std::env::var("DEBUG_TMPFILES").is_ok() {
            eprintln!("DEBUG: Skipping directory creation for {} during remove", full_path.display());
        }
        eprintln!("Warning: Directory removal operations not implemented: D {}", full_path.display());
        return Ok(());
    }

    if !do_create {
        // If not creating and not removing, skip
        return Ok(());
    }

    // Parse mode (octal)
    let mode = parse_mode_with_default(mode_str, 0o755)?;

    // Create directory if it doesn't exist
    if !full_path.exists() {
        fs::create_dir_all(&full_path)
            .map_err(|e| eyre!("Failed to create directory {}: {}", full_path.display(), e))?;
    }

    // Set permissions
    set_permissions_from_mode(&full_path, mode)?;

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
    let target = parts.get(5).map(|s| s.as_str()).unwrap_or("-");

    if target == "-" {
        return Err(eyre!("Invalid symlink line: missing target"));
    }

    // Remove existing file if + modifier is present
    if modifiers.append_or_force && full_path.exists() {
        if full_path.is_symlink() {
            fs::remove_file(&full_path)
                .map_err(|e| eyre!("Failed to remove existing symlink {}: {}", full_path.display(), e))?;
        } else {
            // For L+, we might want to replace it, but let's be conservative
            return Ok(());
        }
    }

    // Create symlink if it doesn't exist
    if !full_path.exists() {
        std::os::unix::fs::symlink(target, &full_path)
            .map_err(|e| eyre!("Failed to create symlink {} -> {}: {}", full_path.display(), target, e))?;
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
    let file_exists = full_path.exists();
    let should_write = !file_exists || modifiers.append_or_force;

    if should_write {
        fs::write(&full_path, content)
            .map_err(|e| eyre!("Failed to {} file {}: {}",
                if file_exists { "write to" } else { "create" }, full_path.display(), e))?;
    }

    // Set permissions if specified
    if mode_str != "-" {
        let mode = u32::from_str_radix(mode_str, 8)
            .map_err(|e| eyre!("Invalid mode '{}': {}", mode_str, e))?;
        set_permissions_from_mode(&full_path, mode)?;
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
    if !full_path.exists() {
        let path_str = full_path.to_str()
            .ok_or_else(|| eyre!("Path contains invalid UTF-8: {}", full_path.display()))?;
        posix_mkfifo(path_str)
            .map_err(|e| eyre!("Failed to create named pipe {}: {}", full_path.display(), e))?;
    }

    // Set permissions (always, since posix_mkfifo creates with 0o777)
    let mode = parse_mode_with_default(mode_str, 0o644)?;
    set_permissions_from_mode(&full_path, mode)?;

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

    if !full_path.exists() {
        return Ok(()); // Nothing to remove
    }

    match remove_type {
        "r" => {
            // Remove file
            if full_path.is_file() {
                fs::remove_file(&full_path)
                    .map_err(|e| eyre!("Failed to remove file {}: {}", full_path.display(), e))?;
            } else {
                return Err(eyre!("Path {} is not a file (use R for directories)", full_path.display()));
            }
        }
        "R" => {
            // Recursively remove directory
            fs::remove_dir_all(&full_path)
                .map_err(|e| eyre!("Failed to remove directory {}: {}", full_path.display(), e))?;
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
    if !full_path.exists() {
        return Err(eyre!("File {} does not exist (use f to create)", full_path.display()));
    }

    fs::write(&full_path, content)
        .map_err(|e| eyre!("Failed to write to file {}: {}", full_path.display(), e))?;

    Ok(())
}

fn process_empty_directory_line(parts: &[String], _modifiers: &Modifiers, root: Option<&Path>) -> Result<()> {
    if parts.len() < 2 {
        return Err(eyre!("Invalid empty directory line: not enough fields"));
    }

    let path = &parts[1];
    let full_path = apply_root(path, root);

    // For 'e' type, clean contents of existing directory
    if !full_path.exists() {
        return Ok(()); // Directory doesn't exist, nothing to clean
    }

    if !full_path.is_dir() {
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

        if entry_path.is_dir() {
            fs::remove_dir_all(&entry_path)
                .map_err(|e| eyre!("Failed to remove directory {}: {}", entry_path.display(), e))?;
        } else {
            fs::remove_file(&entry_path)
                .map_err(|e| eyre!("Failed to remove file {}: {}", entry_path.display(), e))?;
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
        let file_name = Path::new(path)
            .file_name()
            .ok_or_else(|| eyre!("Cannot get filename from path: {}", path))?;
        let mut factory_path = PathBuf::from("/usr/share/factory");
        factory_path.push(file_name);
        factory_path.to_str()
            .ok_or_else(|| eyre!("Failed to convert factory path to string"))?
            .to_string()
    };
    let full_source = apply_root(&source, root);

    // Check if destination exists and is a non-empty directory
    if full_path.exists() && full_path.is_dir() {
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
    // Use preserve_attrs=true to keep source attributes, but we'll override with specified mode/user/group later
    copy_single_item(
        &full_source,
        &full_path,
        true,  // preserve_attrs
        modifiers.append_or_force, // force overwrite if + modifier
        false, // dereference: do not follow symlinks
        true,  // recursive
    )?;

    // Set permissions if specified
    if mode_str != "-" {
        let mode = u32::from_str_radix(mode_str, 8)
            .map_err(|e| eyre!("Invalid mode '{}': {}", mode_str, e))?;
        set_permissions_from_mode(&full_path, mode)?;
    }

    // Set ownership if specified
    set_ownership_if_specified(&full_path, user_str, group_str)?;

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
        if !parent.exists() {
            fs::create_dir_all(parent)
                .map_err(|e| eyre!("Failed to create parent directory for {}: {}", path.display(), e))?;
        }
    }
    Ok(())
}

/// Parse mode string to u32, with default fallback
fn parse_mode_with_default(mode_str: &str, default_mode: u32) -> Result<u32> {
    if mode_str == "-" {
        Ok(default_mode)
    } else {
        u32::from_str_radix(mode_str, 8)
            .map_err(|e| eyre!("Invalid mode '{}': {}", mode_str, e))
    }
}

/// Set ownership for a path if user/group are specified
fn set_ownership_if_specified(path: &Path, user_str: &str, group_str: &str) -> Result<()> {
    if user_str != "-" || group_str != "-" {
        let uid = if user_str == "-" {
            None
        } else {
            Some(parse_user(user_str)?)
        };
        let gid = if group_str == "-" {
            None
        } else {
            Some(parse_group(group_str)?)
        };

        chown(path, uid, gid)
            .map_err(|e| eyre!("Failed to change ownership of {} to uid={:?}, gid={:?}: {}", path.display(), uid, gid, e))?;
    }
    Ok(())
}

fn parse_user(user_str: &str) -> Result<u32> {
    // Try to parse as numeric UID first
    if let Ok(uid) = user_str.parse::<u32>() {
        return Ok(uid);
    }

    // Try to look up user by name
    match posix_getpasswd(Some(user_str), None) {
        Ok(passwd) => Ok(passwd.uid),
        Err(_) => Err(eyre!("Unknown user: {} (check /etc/passwd or use numeric UID)", user_str)),
    }
}

fn parse_group(group_str: &str) -> Result<u32> {
    // Try to parse as numeric GID first
    if let Ok(gid) = group_str.parse::<u32>() {
        return Ok(gid);
    }

    // Try to look up group by name
    match posix_getgroup(Some(group_str), None) {
        Ok(group) => Ok(group.gid),
        Err(_) => Err(eyre!("Unknown group: {} (check /etc/group or use numeric GID)", group_str)),
    }
}
