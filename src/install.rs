use std::fs;
use std::path::{Path, PathBuf};
use std::collections::HashMap;
use std::os::unix::fs::symlink;
use color_eyre::eyre::{self, Result, WrapErr, eyre};
use crate::models::*;
use crate::dirs;
use crate::utils;
use crate::utils::FileType;
use crate::package;
use crate::download;
use crate::scriptlets::{run_scriptlets, ScriptletType};
use std::io::Write;
use regex;

#[derive(Debug, Default, Clone)]
pub struct InstallationPlan {
    pub fresh_installs: HashMap<String, InstalledPackageInfo>,
    pub upgrades_new: HashMap<String, InstalledPackageInfo>,
    pub upgrades_old: HashMap<String, InstalledPackageInfo>,
    pub skipped_reinstalls: HashMap<String, InstalledPackageInfo>,
    pub old_removes: HashMap<String, InstalledPackageInfo>,
    pub new_exposes: HashMap<String, InstalledPackageInfo>,
    pub del_exposes: HashMap<String, InstalledPackageInfo>,
}
use std::time::SystemTime;



fn handle_elf(target_path: &Path, env_root: &Path, fs_file: &Path) -> Result<()> {
    let base_env_root = dirs::find_env_root(BASE_ENV)
        .ok_or_else(|| eyre::eyre!("Base environment not found"))?;

    let elf_loader_path = base_env_root.join("usr/bin/elf-loader");

    // Create hardlink from elf-loader to target path (replace copy&replace)
    if target_path.exists() {
        fs::remove_file(target_path)
            .with_context(|| format!("Failed to remove existing file {}", target_path.display()))?;
    }

    // Create parent directory if it doesn't exist
    if let Some(parent) = target_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create parent directory {}", parent.display()))?;
    }

    fs::hard_link(&elf_loader_path, target_path)
        .with_context(|| format!(
            "Failed to create hardlink from {} to {}",
            elf_loader_path.display(),
            target_path.display()
        ))?;

    let has_symlink1 = replace_existing_symlink1(target_path, fs_file)
        .with_context(|| format!("Failed to ensure symlink1 for {}", target_path.display()))?;

    if !has_symlink1 {
        create_symlink2(target_path, fs_file)
            .with_context(|| format!("Failed to ensure symlink2 for {}", target_path.display()))?;
    }

    log::debug!(
        "handle_elf target_path={}, env_root={}, fs_file={}",
        target_path.display(),
        env_root.display(),
        fs_file.display()
    );
    Ok(())
}

// symlink1 = target_path.replace("ebin", "bin")
fn replace_existing_symlink1(target_path: &Path, fs_file: &Path) -> Result<bool> {
    let target_path_str = target_path.to_string_lossy();
    let symlink1_path = PathBuf::from(target_path_str.replace("/ebin/", "/bin/"));

    if !symlink1_path.exists() {
        return Ok(false);
    }

    // Check if symlink1 points to fs_file
    match fs::read_link(&symlink1_path) {
        Ok(current_target) => {
            if current_target == fs_file {
                // symlink1 already points to the correct target or has been updated
                return Ok(true);
            }

            log::debug!("symlink1 {} exists but points to {:?}, updating to point to {:?}",
                       symlink1_path.display(), current_target, fs_file);
            // Remove existing symlink and create new one
            fs::remove_file(&symlink1_path)
                .with_context(|| format!("Failed to remove existing symlink {}", symlink1_path.display()))?;

            // Create parent directory if it doesn't exist
            if let Some(parent) = symlink1_path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("Failed to create parent directory {}", parent.display()))?;
            }

            symlink(fs_file, &symlink1_path)
                .with_context(|| format!(
                    "Failed to create symlink from {} to {}",
                    symlink1_path.display(),
                    fs_file.display()
                ))?;
            Ok(true)
        }
        Err(_) => {
            // symlink1 exists but is not a symlink (regular file/directory)
            // Don't modify it, indicate that symlink2 is needed
            Ok(false)
        }
    }
}

// Create symlink2: "{dirname(target_path)}/.{filename(target_path)}" -> fs_file
fn create_symlink2(target_path: &Path, fs_file: &Path) -> Result<()> {
    let target_filename = target_path.file_name()
        .ok_or_else(|| eyre::eyre!("Failed to get filename from {}", target_path.display()))?
        .to_string_lossy();
    let target_dirname = target_path.parent()
        .ok_or_else(|| eyre::eyre!("Failed to get parent directory from {}", target_path.display()))?;

    let symlink2_path = target_dirname.join(format!(".{}", target_filename));

    // Remove existing symlink2 if it exists
    if symlink2_path.exists() {
        fs::remove_file(&symlink2_path)
            .with_context(|| format!("Failed to remove existing symlink {}", symlink2_path.display()))?;
    }

    // Create symlink2 -> fs_file
    symlink(fs_file, &symlink2_path)
        .with_context(|| format!(
            "Failed to create symlink from {} to {}",
            symlink2_path.display(),
            fs_file.display()
        ))?;

    log::debug!("Created symlink2: {} -> {}", symlink2_path.display(), fs_file.display());
    Ok(())
}

fn mirror_dir(env_root: &Path, store_fs_dir: &Path, fs_files: &[crate::utils::MtreeFileInfo]) -> Result<()> {
    for fs_file_info in fs_files {
        let fs_file = &fs_file_info.path;
        let fhs_file = fs_file.strip_prefix(store_fs_dir)
            .with_context(|| format!("Failed to strip prefix {} from {}", store_fs_dir.display(), fs_file.display()))?;
        let target_path = env_root.join(fhs_file);

        if fs_file_info.is_dir() {
            // Check if target path exists and is not a directory
            if target_path.exists() && !target_path.is_dir() {
                // Remove the non-directory file first
                fs::remove_file(&target_path)
                    .with_context(|| format!("Failed to remove non-directory file {} for mirror_dir", target_path.display()))?;
            }
            fs::create_dir_all(&target_path)
                .with_context(|| format!("Failed to create directory {}", target_path.display()))?;
            continue;
        }

        // Create parent directory if it doesn't exist
        // No longer necessary, since filelist.txt always show dir before files under it
        // if let Some(parent) = target_path.parent() {
        //     fs::create_dir_all(parent)
        //         .with_context(|| format!("Failed to create parent directory {}", parent.display()))?;
        // }

        if fs_file_info.is_link() {
            mirror_symlink_file(fs_file, &target_path, fhs_file)
                .with_context(|| format!("Failed to handle symlink file {}", fs_file.display()))?;
        } else {
            mirror_regular_file(fs_file, &target_path, fhs_file)
                .with_context(|| format!("Failed to handle regular file {}", fs_file.display()))?;
        }
    }
    Ok(())
}

/// Handle symlink files in mirror_dir function
///
/// This function processes symlinks that may point to either files or directories.
/// For top-level directory symlinks (sbin, bin, lib, lib64, lib32), it skips them
/// as they are handled by the environment setup process.
/// For other symlinks pointing to files, it creates a shortcut symlink.
///
/// Examples:
/// - sbin -> usr/sbin (top-level dir symlink): skipped (handled by env setup)
/// - bin -> usr/bin (top-level dir symlink): skipped (handled by env setup)
/// - python3 -> python3.11 (file symlink): creates shortcut symlink
/// - /usr/bin/python3 -> /usr/bin/python3.11 (absolute file symlink): creates shortcut symlink
///
/// Parameters:
/// - fs_file: Path to the symlink in the store
/// - target_path: Where to create the symlink in the environment
fn mirror_symlink_file(fs_file: &Path, target_path: &Path, fhs_file: &Path) -> Result<()> {
    // Skip symlinks for top-level directories: sbin, bin, lib, lib64, lib32
    if matches!(fhs_file.to_string_lossy().as_ref(), "sbin" | "bin" | "lib" | "lib64" | "lib32") {
        return Ok(());
    }

    utils::remove_any_existing_file(target_path, true)?;

    // Handle regular symlink (not pointing to directory)
    shortcut_symlink(fs_file, target_path)
        .with_context(|| format!("Failed to shortcut_symlink from {} to {}", fs_file.display(), target_path.display()))?;
    Ok(())
}

/// Handle regular files in mirror_dir function
///
/// This function processes regular files (not symlinks or directories).
/// For files in /etc/, it copies the file content.
/// For other files, it creates a symlink to the store location.
///
/// Examples:
/// - /etc/resolv.conf: copied to environment (preserves content)
/// - /usr/bin/python3.11: symlinked to store location
/// - /usr/lib/libpython3.11.so: symlinked to store location
///
/// Parameters:
/// - fs_file: Path to the file in the store
/// - target_path: Where to create the file/symlink in the environment
/// - fhs_file: Relative path from store_fs_dir (used to determine if file is in /etc/)
fn mirror_regular_file(fs_file: &Path, target_path: &Path, fhs_file: &Path) -> Result<()> {
    // Remove any existing file/dirs
    if fs::symlink_metadata(target_path).is_ok() {
        // On upgrade, it's normal to overwrite old files from previous version
        log::trace!("File already exists, overwriting {} with {}", target_path.display(), fs_file.display());
        // Check if target path is a directory and handle accordingly
        if target_path.is_dir() {
            fs::remove_dir_all(target_path)
                .with_context(|| format!("Failed to remove directory {} for mirror_dir", target_path.display()))?;
        } else {
            fs::remove_file(target_path)
                .with_context(|| format!("Failed to remove file {} for mirror_dir", target_path.display()))?;
        }
    }

    if fhs_file.starts_with("etc/") {
        fs::copy(fs_file, target_path)
            .with_context(|| format!("Failed to copy {} to {}", fs_file.display(), target_path.display()))?;
    } else {
        symlink(fs_file, target_path)
            .with_context(|| format!("Failed to create symlink from {} to {}", fs_file.display(), target_path.display()))?;
    }
    Ok(())
}

// Like symlink() but try to remove one level of indirection
fn shortcut_symlink(fs_file: &Path, target_path: &Path) -> Result<()> {
    if let Ok(link_target) = fs::read_link(fs_file) {
        let new_link_target = if link_target.is_absolute() || !link_target.exists() {
            // This prevents
            //      /usr/bin/python3 -> /home/wfg/.epkg/store/lsl4sc64f2ccp62cxfquizdaj5k4fpcu__python3-minimal__3.13.3-1__amd64/fs/usr/bin/python3.13
            // in case
            //      /home/wfg/.epkg/store/lsl4sc64f2ccp62cxfquizdaj5k4fpcu__python3-minimal__3.13.3-1__amd64/fs/usr/bin/python3 -> python3.13
            //
            // Prevents
            //      /home/wfg/.epkg/envs/main/bin/sh -> /home/wfg/.epkg/store/g53cxe55pxbwqgq2k2nk7owjnv7zmlsj__busybox-binsh__1.37.0-r18__noarch/fs//bin/busybox
            // in case /bin/busybox happen to exist in host os but not in env:
            //      /home/wfg/.epkg/store/g53cxe55pxbwqgq2k2nk7owjnv7zmlsj__busybox-binsh__1.37.0-r18__noarch/fs//bin/sh -> /bin/busybox
            link_target
        } else if link_target.starts_with("../") {
            // For parent-relative paths like ../bin/pidof, normalize against fs_file
            normalize_join(fs_file.parent().ok_or_else(|| eyre::eyre!("Failed to get parent directory for {}", fs_file.display()))?,
                           &link_target)
        } else {
            // For sibling-relative paths like python3.11, join with source file's parent
            fs_file.parent()
                .ok_or_else(|| eyre::eyre!("Failed to get parent directory for {}", fs_file.display()))?
                .join(link_target)
        };

        symlink(&new_link_target, target_path)
            .with_context(|| format!("Failed to create symlink from {} to {}", fs_file.display(), target_path.display()))?;
    }
    Ok(())
}

fn normalize_join(base: &Path, subpath: &Path) -> PathBuf {
    let mut components: Vec<_> = base.components().collect();

    for component in subpath.components() {
        match component {
            std::path::Component::ParentDir if !components.is_empty() => {
                components.pop();
            },
            std::path::Component::CurDir => {},
            _ => components.push(component),
        }
    }

    components.iter().collect()
}

fn create_ebin_wrappers(env_root: &Path, fs_files: &[crate::utils::MtreeFileInfo]) -> Result<Vec<PathBuf>> {
    let mut created_ebin_paths: Vec<PathBuf> = Vec::new();
    log::debug!("Creating ebin wrappers for {} files in {}", fs_files.len(), env_root.display());
    for fs_file_info in fs_files {
        let fs_file = &fs_file_info.path;
        let path_str = fs_file.to_string_lossy();

        if !path_str.contains("/bin/") && !path_str.contains("/sbin/") && !path_str.contains("/libexec/") {
            continue;
        }

        let lib_regex = regex::Regex::new(r"\.(so|so\.\d+)$").unwrap();
        if lib_regex.is_match(&path_str) {
            continue;
        }

        // Skip if not executable or is directory
        if fs_file_info.is_dir() {
            continue;
        }
        let mode = fs_file_info.mode.unwrap_or(0o644);
        if mode & 0o111 == 0 {
            continue;
        }

        if let Some(created_path) = create_ebin_wrapper(env_root, fs_file)
            .with_context(|| format!("Failed to create ebin wrapper for {}", fs_file.display()))? {
            created_ebin_paths.push(created_path);
        }
    }
    log::debug!("create_ebin_wrappers: returning {} created paths: {:?}", created_ebin_paths.len(), created_ebin_paths);
    Ok(created_ebin_paths)
}

fn create_ebin_wrapper(env_root: &Path, fs_file: &Path) -> Result<Option<PathBuf>> {
    let (file_type, first_line) = utils::get_file_type(fs_file)
        .with_context(|| format!("Failed to determine file type for {}", fs_file.display()))?;
    let basename = fs_file.file_name()
        .ok_or_else(|| eyre::eyre!("Failed to get filename for {}", fs_file.display()))?;
    let ebin_path = env_root.join("usr/ebin").join(basename);

    log::debug!(
        "Creating ebin wrapper: ebin_path={}, fs_file={}, file_type={:?}, first_line={:?}",
        ebin_path.display(),
        fs_file.display(),
        file_type,
        first_line
    );
    match file_type {
        FileType::Elf => {
            handle_elf(&ebin_path, env_root, fs_file)
                .with_context(|| format!("Failed to handle elf for {}", ebin_path.display()))?;
            return Ok(Some(ebin_path));
        }
        FileType::ShellScript
        | FileType::PerlScript
        | FileType::PythonScript
        | FileType::RubyScript
        | FileType::NodeScript
        | FileType::LuaScript => {
            create_script_wrapper(env_root, fs_file, &ebin_path, file_type, &first_line)
                .with_context(|| format!("Failed to create script wrapper for {}", fs_file.display()))?;
            return Ok(Some(ebin_path));
        }
        _ => return Ok(None),
    }
}

fn create_script_wrapper(
    env_root: &Path,
    fs_file: &Path,
    ebin_path: &Path,
    file_type: FileType,
    first_line: &str,
) -> Result<()> {
    // Try to create shebang line, but handle errors gracefully
    let env_shell_bang_line = match create_shebang_line(env_root, first_line) {
        Ok(line) => line,
        Err(e) => {
            let root_cause = e.root_cause().to_string();
            let path_str = fs_file.to_string_lossy();
            let error_msg = format!(
                "Cannot create script wrapper for {} at {}: failed to create shebang line for '{}': {}",
                fs_file.display(),
                ebin_path.display(),
                first_line,
                root_cause
            );

            if path_str.contains("/usr/bin") {
                return Err(eyre::eyre!("{}", error_msg));
            } else {
                /* Handle missing interpreters as warnings rather than errors for
                 * fs_file = ".../fs/usr/share/rustc-1.74/bin/wasi-node" case.
                 *
                 * Some packages (like rustc) may include scripts that require interpreters
                 * not listed in their dependencies (e.g., node for wasi-node). Since these
                 * interpreters aren't in the package's dependency list, we shouldn't fail
                 * the entire package installation just because we can't create wrappers
                 * for these optional scripts.
                 *
                 * By logging a warning and continuing, we ensure the package installation
                 * completes successfully while still informing the user about the missing
                 * interpreter.
                 */
                log::warn!("{}", error_msg);
                return Ok(());
            }
        }
    };

    let exec_cmd = get_exec_command(&file_type, fs_file);

    let mut wrapper = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(ebin_path)
        .with_context(|| format!("Failed to open {} for create_script_wrapper", ebin_path.display()))?;

    if !env_shell_bang_line.is_empty() {
        wrapper.write_all(env_shell_bang_line.as_bytes())
            .with_context(|| format!("Failed to write shebang line to {}", ebin_path.display()))?;
    }

    wrapper.write_all(exec_cmd.as_bytes())
        .with_context(|| format!("Failed to write exec command to {}", ebin_path.display()))?;

    set_wrapper_permissions(ebin_path)?;

    log::debug!(
        "Created script wrapper: ebin_path={}, fs_file={}, file_type={:?}, first_line={:?}",
        ebin_path.display(),
        fs_file.display(),
        file_type,
        first_line
    );
    Ok(())
}

/// Parse a shebang line into interpreter path and parameters
fn parse_shebang_line(first_line: &str) -> Result<(String, String)> {
    if !first_line.starts_with("#!") {
        return Err(eyre::eyre!("No shebang line found"));
    }

    let interpreter_with_params = first_line[2..].trim().replace("\t", " ");
    // Example: interpreter_with_params = "/bin/sh"
    let (interpreter_path, params) = match interpreter_with_params.split_once(' ') {
        Some((path, params)) => (path.to_string(), params.to_string()),  // Example: path="/usr/bin/env", params="python3"
        None => (interpreter_with_params.to_string(), String::new()),    // Example: path="/bin/sh", params=""
    };
    log::debug!("interpreter_path: '{}', params: '{}'", interpreter_path, params);

    Ok((interpreter_path, params))
}

/// Find and link the appropriate interpreter if it doesn't exist
fn find_link_interpreter(interpreter_in_env: &Path, interpreter_basename: &str) -> Result<()> {
    if interpreter_in_env.exists() {
        return Ok(());
    }

    // if the soft link is broken, delete it
    if let Ok(metadata) = fs::symlink_metadata(interpreter_in_env) {
        if metadata.file_type().is_symlink() {
            if fs::read_link(interpreter_in_env).map(|t| !t.exists()).unwrap_or(false) {
                fs::remove_file(interpreter_in_env)?
            } else {
                return Ok(());
            }
        } else {
            return Ok(());
        }
    }

    // Get the parent directory to search in
    let parent = interpreter_in_env.parent()
        .ok_or_else(|| eyre::eyre!("Failed to get parent directory of {}", interpreter_in_env.display()))?;

    // Find candidate interpreters based on the type
    let targets = match interpreter_basename {
        // For shell scripts, look for bash or dash as alternatives
        "sh" => glob::glob(&format!("{}/{{bash,dash}}", parent.display()))
            .with_context(|| "Failed to glob for shell interpreters")?,

        // For other interpreters (python, ruby etc), look for versioned variants
        // e.g. python3.8, python3.9 etc
        _ => glob::glob(&format!("{}?*", interpreter_in_env.display()))
            .with_context(|| format!("Failed to glob for {} interpreters", interpreter_basename))?
    };

    // Find the "latest" interpreter by comparing filenames
    let target = targets
        .filter_map(Result::ok)
        .max_by(|a, b| {
            let a_name = a.file_name().unwrap_or_default().to_string_lossy();
            let b_name = b.file_name().unwrap_or_default().to_string_lossy();
            a_name.cmp(&b_name)
        })
        .ok_or_else(|| eyre::eyre!("No suitable interpreter found for {}", interpreter_basename))?;

    // Create a symlink from the found interpreter to the expected location
    symlink(&target, interpreter_in_env)
        .with_context(|| format!("Failed to create symlink from {} to {}",
            target.display(), interpreter_in_env.display()))?;

    Ok(())
}

/// Create the wrapper for the interpreter in the ebin directory
fn create_interpreter_wrapper(env_root: &Path, interpreter_path: &str, interpreter_basename: &str) -> Result<String> {
    // Example: env_interpreter_path = "/home/wfg/.epkg/envs/main/ebin/sh"
    let env_interpreter_path = format!("{}/ebin/{}", env_root.display(), interpreter_basename);
    let env_interpreter = Path::new(&env_interpreter_path);

    if !env_interpreter.exists() {
        // Example: interpreter_in_env = "/home/wfg/.epkg/envs/main/bin/sh"
        // Which is a symlink to: "/home/wfg/.epkg/store/twktsyye3ksj068w2fx9pz5fefwy70mw__bash__5.2.15__9.oe2403/fs/usr/bin/bash"
        // use format!() instead of Path::join() to enforce simple string operation
        let interpreter_in_env = format!("{}{}", env_root.display(), interpreter_path);
        let interpreter_in_env = Path::new(&interpreter_in_env);

        // Find and link the interpreter if needed
        match find_link_interpreter(interpreter_in_env, interpreter_basename) {
            Ok(()) => {},
            Err(e) => {
                eprintln!("WARNING: script interpreter {} is not found in environment. Please install it later.\n env_path: {}, error: {}",
                    interpreter_basename, interpreter_in_env.display(), e);
                return Ok("".to_string());
            }
        }

        // Example: store_interpreter = "/home/wfg/.epkg/store/twktsyye3ksj068w2fx9pz5fefwy70mw__bash__5.2.15__9.oe2403/fs/usr/bin/bash"
        // Create the wrapper
        let store_interpreter = fs::canonicalize(interpreter_in_env)
            .with_context(|| format!("Failed to resolve interpreter path: {}", interpreter_in_env.display()))?;

        log::debug!("handle_elf params: env_interpreter={:?}, env_root={:?}, store_interpreter={:?}, interpreter_in_env={:?}",
            env_interpreter, env_root, store_interpreter, interpreter_in_env);
        // Example output:
        // handle_elf params:
        // env_interpreter="/home/wfg/.epkg/envs/main/ebin/sh",
        // env_root="/home/wfg/.epkg/envs/main",
        // store_interpreter="/home/wfg/.epkg/store/twktsyye3ksj068w2fx9pz5fefwy70mw__bash__5.2.15__9.oe2403/fs/usr/bin/bash",
        // interpreter_in_env="/home/wfg/.epkg/envs/main/bin/sh"
        handle_elf(env_interpreter, env_root, &store_interpreter)?;
    }

    Ok(env_interpreter_path)
}

fn create_shebang_line(env_root: &Path, first_line: &str) -> Result<String> {
    let shebang_info = parse_shebang_for_wrapper(first_line)?;

    let env_interpreter_path = match create_interpreter_wrapper(env_root, &shebang_info.interpreter_path, &shebang_info.interpreter_basename)
        .with_context(|| format!("Failed to create interpreter wrapper for {} with basename {}", shebang_info.interpreter_path, shebang_info.interpreter_basename))
    {
        Ok(path) => {
            if path == "" {
                return Ok(first_line.to_string());
            }
            path
        },
        Err(e) => return Err(e),
    };

    // Create the final shebang line
    if shebang_info.remaining_params.is_empty() {
        Ok(format!("#!{}\n", env_interpreter_path))
    } else {
        Ok(format!("#!{} {}\n", env_interpreter_path, shebang_info.remaining_params))
    }
}

/// Information extracted from a shebang line for creating wrappers
#[derive(Debug, PartialEq)]
pub struct ShebangInfo {
    pub interpreter_path: String,      // Path to interpreter for wrapper creation (e.g., "/usr/bin/python")
    pub interpreter_basename: String,  // Basename for wrapper lookup (e.g., "python")
    pub remaining_params: String,      // Additional parameters to pass (e.g., "-u -O")
}

/// Parse a shebang line and extract information needed for wrapper creation
/// This function handles env-based shebangs specially by resolving the actual interpreter
///
/// # Examples
///
/// ```
/// # use epkg::install::parse_shebang_for_wrapper;
/// let info = parse_shebang_for_wrapper("#!/usr/bin/env python").unwrap();
/// assert_eq!(info.interpreter_path, "/usr/bin/python");
/// assert_eq!(info.interpreter_basename, "python");
/// assert_eq!(info.remaining_params, "");
///
/// let info = parse_shebang_for_wrapper("#!/usr/bin/env python3 -u").unwrap();
/// assert_eq!(info.interpreter_path, "/usr/bin/python3");
/// assert_eq!(info.interpreter_basename, "python3");
/// assert_eq!(info.remaining_params, "-u");
///
/// let info = parse_shebang_for_wrapper("#!/bin/bash").unwrap();
/// assert_eq!(info.interpreter_path, "/bin/bash");
/// assert_eq!(info.interpreter_basename, "bash");
/// assert_eq!(info.remaining_params, "");
/// ```
pub fn parse_shebang_for_wrapper(first_line: &str) -> Result<ShebangInfo> {
    let (interpreter_path, params) = parse_shebang_line(first_line)
        .with_context(|| format!("Failed to parse shebang line: '{}'", first_line))?;

    // Special handling for env-based shebangs like "#!/usr/bin/env python"
    if interpreter_path == "/usr/bin/env" {
        // Check for case where line has trailing space after env but empty params
        // This catches "#!/usr/bin/env " with trailing space (but not tabs)
        if params.is_empty() {
            return Err(eyre::eyre!("env requires an interpreter to be specified"));
        }

        if !params.trim().is_empty() {
            let mut param_parts: Vec<&str> = params.split_whitespace().collect();

            // Handle env -S flag which allows env to split arguments on whitespace
            // Example: "#!/usr/bin/env -S awk -f" should be treated as "awk -f"
            if param_parts.len() >= 2 && param_parts[0] == "-S" {
                // Remove the -S flag and process the rest
                param_parts.remove(0);
            }

            if param_parts.is_empty() {
                return Err(eyre::eyre!("env -S requires an interpreter to be specified"));
            }

            // For env-based shebangs, the actual interpreter is in the first remaining parameter
            let actual_interpreter = param_parts[0];
            let remaining_params = param_parts[1..].join(" ");

            return Ok(ShebangInfo {
                interpreter_path: format!("/usr/bin/{}", actual_interpreter),
                interpreter_basename: actual_interpreter.to_string(),
                remaining_params,
            });
        }
    }

    // Original logic for non-env shebangs OR env without parameters
    // Handle edge case where interpreter_path is empty (e.g., just "#!")
    if interpreter_path.is_empty() {
        return Ok(ShebangInfo {
            interpreter_path: String::new(),
            interpreter_basename: String::new(),
            remaining_params: params,
        });
    }

    let interpreter_basename = Path::new(&interpreter_path).file_name()
        .ok_or_else(|| eyre::eyre!("Failed to get interpreter basename from: {}", interpreter_path))?
        .to_string_lossy()
        .to_string();

    Ok(ShebangInfo {
        interpreter_path,
        interpreter_basename,
        remaining_params: params,
    })
}

fn get_exec_command(file_type: &FileType, fs_file: &Path) -> String {
    match file_type {
        FileType::ShellScript => format!("exec {:?} \"$@\"\n", fs_file),
        FileType::PythonScript => format!("exec(open({:?}).read())\n", fs_file),
        FileType::RubyScript => format!("load({:?})\n", fs_file),
        FileType::LuaScript => format!("dofile({:?})\n", fs_file),
        _ => format!("exec {:?} \"$@\"\n", fs_file),
    }
}

fn set_wrapper_permissions(ebin_path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = fs::Permissions::from_mode(0o755);
    fs::set_permissions(ebin_path, perms)
        .with_context(|| format!("Failed to set permissions for {}", ebin_path.display()))?;
    Ok(())
}

/// Run ldconfig if the library cache needs updating
fn run_ldconfig_if_needed(env_root: &Path) -> Result<()> {
    let ld_so_cache = env_root.join("etc/ld.so.cache");
    let lib_dirs = [
        env_root.join("etc/ld.so.conf.d"),
        env_root.join("lib"),
        env_root.join("lib64"),
        env_root.join("usr/lib"),
        env_root.join("usr/lib64"),
    ];

    // Get mtime of ld.so.cache if it exists
    let cache_mtime = if ld_so_cache.exists() {
        fs::metadata(&ld_so_cache)
            .with_context(|| format!("Failed to get metadata for {}", ld_so_cache.display()))?
            .modified()
            .with_context(|| format!("Failed to get modification time for {}", ld_so_cache.display()))?
    } else {
        // If cache doesn't exist, we need to run ldconfig
        SystemTime::UNIX_EPOCH
    };

    // Check if any lib directory has been modified more recently than the cache
    let needs_update = lib_dirs.iter().any(|dir| {
        if !dir.exists() {
            return false;
        }
        match fs::metadata(dir) {
            Ok(metadata) => {
                match metadata.modified() {
                    Ok(dir_mtime) => dir_mtime > cache_mtime,
                    Err(_) => true, // If we can't get mtime, assume update needed
                }
            }
            Err(_) => false, // If we can't get metadata, skip this directory
        }
    });

    if needs_update {
        log::info!("Library cache needs updating, running ldconfig");

        // Check if ldconfig exists in the environment before trying to run it
        match crate::run::find_command_in_env_path("ldconfig", env_root) {
            Ok(ldconfig_path) => {
                let run_options = crate::run::RunOptions {
                    mount_dirs: Vec::new(),
                    user: None,
                    command: "ldconfig".to_string(),
                    args: Vec::new(),
                    env_vars: std::collections::HashMap::new(),
                    no_exit: false,  // Fail installation if ldconfig fails
                };

                // Execute ldconfig
                crate::run::fork_and_execute(env_root, &run_options, &ldconfig_path)?;
            }
            Err(_) => {
                log::warn!("ldconfig command not found in environment, skipping library cache update");
            }
        }
    } else {
        log::debug!("Library cache is up to date, skipping ldconfig");
    }

    Ok(())
}

impl PackageManager {
    /// Replace symlinks with their target file content
    fn replace_symlinks_with_content(&self, env_root: &Path) -> Result<()> {
        let symlink_replace_list = [
            // Fixes:
            //      /usr/share/debconf/confmodule: line 28: /usr/lib/cdebconf/debconf: No such file or directory
            // Root cause: that script relies on this being normal file
            //      elif [ -x /usr/share/debconf/frontend ] && \
            //           [ ! -h /usr/share/debconf/frontend ]; then
            //              _DEBCONF_IMPL=debconf
            "/usr/share/debconf/frontend",
        ];

        for symlink_path in &symlink_replace_list {
            let full_symlink_path = env_root.join(
                symlink_path.strip_prefix("/")
                .unwrap_or(symlink_path)  // Fallback to original if no prefix
            );

            if full_symlink_path.exists() && full_symlink_path.is_symlink() {
                // Read the symlink target
                let target_path = std::fs::read_link(&full_symlink_path)?;

                // Remove the symlink
                std::fs::remove_file(&full_symlink_path)?;

                // Try to hardlink the target file to the symlink location, fall back to copy
                if let Err(_) = std::fs::hard_link(&target_path, &full_symlink_path) {
                    // If hardlink fails, copy the file
                    std::fs::copy(&target_path, &full_symlink_path)?;
                }
            }
        }
        Ok(())
    }

    /// Create common symlinks for shell and utilities if they don't exist
    fn create_common_symlinks(&self, env_root: &Path) -> Result<()> {
        // List of symlinks to create: [(symlink, [possible_targets])]
        let symlinks = [
            ("bin/sh", ["bash", "dash"]),
            ("usr/bin/awk", ["mawk", "gawk"]),

            // These are optional and will fail due to no "dpkg -L" output
            ("usr/local/bin/py3compile", ["/usr/bin/true", "/bin/true"]),
            ("usr/local/bin/py3clean", ["/usr/bin/true", "/bin/true"]),
        ];

        for (link_name, possible_targets) in &symlinks {
            let link_path = env_root.join(link_name);

            // Skip if symlink already exists
            if link_path.exists() {
                continue;
            }

            // Try each possible target until we find one that exists
            for target in possible_targets.iter() {
                let target_path = Path::new("/").join(link_name).parent().unwrap().join(target);
                if target_path.exists() {
                    if let Some(parent) = link_path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    symlink(target_path, &link_path)
                        .with_context(|| format!("Failed to create symlink: {} -> {}", link_path.display(), target))?;
                    break;
                }
            }
        }
        Ok(())
    }

    /// Fix up environment links and remove system directories
    fn fixup_env_links(&self, env_root: &Path) -> Result<()> {
        // Prevent running and stalling on `systemctl --system daemon-reload`
        let _ = std::fs::remove_dir(env_root.join("run/systemd/system"));

        // Replace symlinks with their target file content
        self.replace_symlinks_with_content(env_root)?;

        // Create common symlinks for shells and utilities
        self.create_common_symlinks(env_root)?;

        Ok(())
    }

    fn prepare_installation_plan(
        &self,
        all_packages_for_session: &HashMap<String, InstalledPackageInfo>,
        packages_to_expose_from_args: HashMap<String, InstalledPackageInfo>,
        original_installed_packages: &HashMap<String, InstalledPackageInfo>,
    ) -> Result<InstallationPlan> {
        let mut plan = InstallationPlan::default();

        for (session_pkgkey, session_pkg_info) in all_packages_for_session {
            if original_installed_packages.contains_key(session_pkgkey) {
                plan.skipped_reinstalls.insert(session_pkgkey.clone(), session_pkg_info.clone());
                continue;
            }

            let _ = match package::parse_pkgkey(session_pkgkey) {
                Ok(_) => {},
                Err(e) => {
                    log::warn!(
                        "Failed to parse session_pkgkey {}: {}. Considering as fresh install.",
                        session_pkgkey, e
                    );
                    plan.fresh_installs.insert(session_pkgkey.clone(), session_pkg_info.clone());
                    continue;
                }
            };

            let (is_upgrade, old_pkgkey) = find_upgrade_target(session_pkgkey, session_pkg_info, original_installed_packages);
            if is_upgrade {
                plan.upgrades_new.insert(session_pkgkey.clone(), session_pkg_info.clone());
                plan.upgrades_old.insert(old_pkgkey.clone(), original_installed_packages[&old_pkgkey].clone());
            } else {
                plan.fresh_installs.insert(session_pkgkey.clone(), session_pkg_info.clone());
            }
        }

        // Auto-populate expose plan based on installation/removal actions
        Self::auto_populate_expose_plan(&mut plan);

        // Track additional exposure changes for existing packages (not covered by auto-populate)
        track_additional_exposure_changes(
            &mut plan,
            &packages_to_expose_from_args,
            original_installed_packages,
        );
        Ok(plan)
    }


    // link files from env_root to store_fs_dir
    pub fn link_package(&self, store_fs_dir: &PathBuf, env_root: &PathBuf) -> Result<()> {
        let fs_files = utils::list_package_files_with_info(store_fs_dir.to_str().ok_or_else(|| eyre::eyre!("Invalid store_fs_dir path: {}", store_fs_dir.display()))?)
            .with_context(|| format!("Failed to list package files in {}", store_fs_dir.display()))?;
        mirror_dir(env_root, store_fs_dir, &fs_files)
            .with_context(|| format!("Failed to mirror directory from {} to {}", store_fs_dir.display(), env_root.display()))?;
        Ok(())
    }

    // - run post-install scriptlets
    // - create ebin wrappers
    // Returns a list of relative paths to the created ebin wrappers (relative to env_root).
    pub fn expose_package(&self, store_fs_dir: &PathBuf, env_root: &PathBuf) -> Result<Vec<String>> {
        log::debug!("expose_package called for store_fs_dir: {}", store_fs_dir.display());
        let fs_files = utils::list_package_files_with_info(store_fs_dir.to_str().ok_or_else(|| eyre::eyre!("Invalid store_fs_dir path"))?)?;
        let absolute_ebin_paths = create_ebin_wrappers(env_root, &fs_files)?;
        log::debug!("expose_package for store_fs_dir '{}': received {} absolute_ebin_paths: {:?}", store_fs_dir.display(), absolute_ebin_paths.len(), absolute_ebin_paths);
        let mut relative_ebin_links: Vec<String> = Vec::new();
        for abs_path in absolute_ebin_paths {
            match abs_path.strip_prefix(env_root) {
                Ok(rel_path) => {
                    relative_ebin_links.push(rel_path.to_string_lossy().into_owned());
                }
                Err(e) => {
                    // Still log a warning, as this indicates a potential issue in path generation or env_root handling.
                    log::warn!("Failed to strip prefix {} from path {} for store_fs_dir '{}': {}", env_root.display(), abs_path.display(), store_fs_dir.display(), e);
                }
            }
        }
        log::debug!("expose_package for store_fs_dir '{}': returning {} relative_ebin_links: {:?}", store_fs_dir.display(), relative_ebin_links.len(), relative_ebin_links);
        Ok(relative_ebin_links)
    }

    /// Installs specified packages and their dependencies.
    ///
    /// This function orchestrates the package installation process as follows:
    /// 1.  Loads existing installed package metadata (`installed-packages.json`) into
    ///     `self.installed_packages` and creates a copy (`original_installed_packages`)
    ///     to represent the state at the start of the operation.
    /// 2.  Resolves the initial `package_specs` (user-provided package names) against
    ///     repository metadata to get basic package information.
    /// 3.  Recursively collects all dependencies for these initial packages, resulting
    ///     in `all_packages_for_session`. This map contains `InstalledPackageInfo`
    ///     for all packages involved in the current session, with `pkgline` fields
    ///     correctly initialized as empty strings (`String::new()`).
    /// 4.  Further processes this set collect essential packages (`collect_essential_packages`),
    ///     resulting in `processed_session_packages`.
    /// 5.  Determines `packages_needing_file_ops`:
    ///     Iterates through `processed_session_packages`. A package is added to
    ///     `packages_needing_file_ops` if:
    ///     a. It was not present in `original_installed_packages` (i.e., it's a new package).
    ///     b. Or, if its `ebin_exposure` has changed compared to `original_installed_packages`.
    ///        (This implies a change in how it should be exposed, requiring file operations).
    /// 6.  Updates `self.installed_packages` (the in-memory representation that will be saved):
    ///     Packages from `all_packages_for_session` (which includes newly requested packages
    ///     and all their dependencies, potentially re-evaluating existing ones) are merged into
    ///     `self.installed_packages`. If a package from `all_packages_for_session` already
    ///     exists in `self.installed_packages`, its information is updated. If not, it's added.
    ///     This ensures that metadata (depend_depth, rdepends, depends, ebin_exposure, and
    ///     initially empty pkglines) is updated for packages involved in this session, while
    ///     preserving other unrelated, already-installed packages.
    /// 7.  If `packages_needing_file_ops` is empty:
    ///     Prints a message indicating no new files need to be installed/linked.
    ///     Saves the (potentially updated metadata in) `self.installed_packages` to a new
    ///     generation, as metadata like dependency relationships might have changed even
    ///     if no files were physically altered.
    ///     Then, exits.
    /// 8.  If `packages_needing_file_ops` is NOT empty:
    ///     Calls `self.install_pkgkeys()` with `packages_needing_file_ops`.
    ///     `install_pkgkeys` handles the actual downloading, unpacking, linking, and
    ///     exposure of package files. Critically, during this process (specifically in
    ///     `process_downloaded_package`), the `pkgline` in `InstalledPackageInfo`
    ///     is populated with its final value. The pkgline format is `{ca_hash}__{pkgkey}`,
    ///     where `ca_hash` is computed from the content of the unpacked package.
    ///     `install_pkgkeys` is responsible for ensuring these `InstalledPackageInfo`
    ///     objects (with corrected `pkglines`) are used to update `self.installed_packages`
    ///     before it finally saves `installed-packages.json` for the new generation.
    pub fn install_packages(&mut self, package_specs: Vec<String>) -> Result<()> {
        self.load_installed_packages()?;
        let original_installed_packages = self.installed_packages.clone();

        let package_format = channel_config().format;
        let mut initial_packages_info = self.resolve_package_info(package_specs.clone(), package_format);

        // Filter out packages that are already installed
        let current_installed_from_request: Vec<String> = initial_packages_info
            .keys()
            .filter(|name| original_installed_packages.contains_key(*name))
            .cloned()
            .collect();

        if !current_installed_from_request.is_empty() {
            println!("Packages already installed: {}", current_installed_from_request.join(" "));

            // Remove already installed packages from the initial packages info
            for pkgkey in &current_installed_from_request {
                initial_packages_info.remove(pkgkey);
            }

            // If all requested packages are already installed, exit early
            if initial_packages_info.is_empty() {
                return Ok(());
            }
        }

        self.collect_essential_packages(&mut initial_packages_info)?;
        let mut all_packages_for_session = self.collect_recursive_depends(&initial_packages_info, package_format)?;

        let packages_to_expose = self.extend_appbin_by_source(&mut all_packages_for_session)?;

        if packages_to_expose.is_empty() && all_packages_for_session.is_empty() {
            println!("No packages to install or upgrade.");
            return Ok(());
        }


        self.install_pkgkeys(all_packages_for_session, packages_to_expose, &original_installed_packages)
    }

    pub fn install_pkgkeys(
        &mut self,
        all_packages_for_session: HashMap<String, InstalledPackageInfo>,
        packages_to_expose_from_args: HashMap<String, InstalledPackageInfo>,
        original_installed_packages: &HashMap<String, InstalledPackageInfo>,
    ) -> Result<()> {
        let plan = self.prepare_installation_plan(&all_packages_for_session, packages_to_expose_from_args, original_installed_packages)?;

        // If we reach here, actions_planned was true, user confirmed, and not dry_run.
        // Proceed with actual installation steps by calling the unified execution method.
        self.execute_installation_plan(plan)
    }

    /// Prompt the user with the installation plan and confirm before proceeding.
    /// Returns actions_planned
    fn prompt_and_confirm_install_plan(
        &mut self,
        plan: &InstallationPlan,
    ) -> Result<bool> {
        let actions_planned = self.display_installation_plan(plan);

        if !actions_planned {
            println!("\nNo changes planned based on the current request.");
            return Ok(false);
        }

        self.print_installation_summary(plan);
        self.print_download_requirements(plan)?;

        utils::user_prompt_and_confirm()
    }

    /// Display the installation plan details to the user
    fn display_installation_plan(&mut self, plan: &InstallationPlan) -> bool {
        let mut actions_planned = false;

        if !plan.fresh_installs.is_empty() {
            actions_planned = true;
            println!("Packages to be freshly installed:");
            self.print_packages_by_depend_depth(&plan.fresh_installs);
        }

        if !plan.upgrades_new.is_empty() {
            actions_planned = true;
            println!("Packages to be upgraded:");
            for (new_pkgkey, _) in &plan.upgrades_new {
                let (new_name_parsed, _, new_arch_parsed) = package::parse_pkgkey(new_pkgkey).unwrap_or_default();
                let old_pkgkey_display = plan.upgrades_old.iter()
                    .find_map(|(old_key, _)| {
                        let (old_name, _, old_arch) = package::parse_pkgkey(old_key).unwrap_or_default();
                        if new_name_parsed == old_name && new_arch_parsed == old_arch { Some(old_key.as_str()) } else { None }
                    })
                    .unwrap_or("unknown previous version");
                println!("- {} (replacing {})", new_pkgkey, old_pkgkey_display);
            }
        }

        if !plan.old_removes.is_empty() {
            actions_planned = true;
            println!("Packages to be removed:");
            for pkgkey in plan.old_removes.keys() {
                println!("- {}", pkgkey);
            }
        }

        if !plan.new_exposes.is_empty() {
            actions_planned = true;
            println!("Packages to be exposed:");
            for pkgkey in plan.new_exposes.keys() {
                println!("- {}", pkgkey);
            }
        }

        if !plan.del_exposes.is_empty() {
            actions_planned = true;
            println!("Packages to be unexposed:");
            for pkgkey in plan.del_exposes.keys() {
                println!("- {}", pkgkey);
            }
        }

        actions_planned
    }

    /// Print summary statistics for the installation plan
    fn print_installation_summary(&self, plan: &InstallationPlan) {
        let num_upgraded = plan.upgrades_new.len();
        let num_new = plan.fresh_installs.len();
        let num_remove = plan.old_removes.len();
        let num_expose = plan.new_exposes.len();
        let num_unexpose = plan.del_exposes.len();

        println!(
            "\n{} upgraded, {} newly installed, {} to remove, {} to expose, {} to unexpose.",
            num_upgraded, num_new, num_remove, num_expose, num_unexpose
        );
    }

    /// Calculate and print download and disk space requirements
    fn print_download_requirements(&mut self, plan: &InstallationPlan) -> Result<()> {
        // Sum sizes for downloads
        let mut total_download: u64 = 0;
        let mut total_install: u64 = 0;
        for pkgkey in plan.fresh_installs.keys().chain(plan.upgrades_new.keys()) {
            if let Ok(pkginfo) = self.load_package_info(pkgkey) {
                total_download += pkginfo.size as u64;
                total_install += pkginfo.installed_size as u64;
            }
        }

        if total_download > 0 {
            println!(
                "Need to get {} archives.",
                utils::format_size(total_download)
            );
            println!(
                "After this operation, {} of additional disk space will be used.",
                utils::format_size(total_install)
            );
        }

        Ok(())
    }

    /// Execute an InstallationPlan by performing the actual installation/removal operations.
    /// This function can be reused by both install and remove operations.
    pub fn execute_installation_plan(&mut self, plan: InstallationPlan) -> Result<()> {
        // --- USER PROMPT AND PRE-EXECUTION CHECKS ---
        let go_on = self.prompt_and_confirm_install_plan(&plan)?;
        if !go_on {
            return Ok(());
        }

        let new_generation = self.create_new_generation()?;
        let env_root = crate::dirs::get_default_env_root()?.clone();
        let store_root = dirs().epkg_store.clone();
        let package_format = channel_config().format;

        // Execute removals
        self.execute_removals(&plan, &store_root, &env_root, package_format)?;

        // Execute installations and upgrades
        self.execute_installations(&plan, &store_root, &env_root, package_format)?;

        // Execute exposure changes
        self.execute_unexpose_operations(&plan, &env_root)?;
        self.execute_expose_operations(&plan, &store_root, &env_root)?;

        // Update metadata for skipped reinstalls
        self.update_skipped_reinstalls_metadata(&plan)?;

        self.record_history(&new_generation, Some(&plan))?;
        self.save_installed_packages(&new_generation)?;
        self.update_current_generation_symlink(new_generation)?;

        Ok(())
    }

    /// Execute package removals
    fn execute_removals(&mut self, plan: &InstallationPlan, store_root: &Path, env_root: &Path, package_format: PackageFormat) -> Result<()> {
        if plan.old_removes.is_empty() {
            return Ok(());
        }

        // Update rdepends of packages that depended on the removed packages
        for (removed_pkg_key, removed_pkg_info) in &plan.old_removes {
            for dep_on_key in &removed_pkg_info.depends {
                // If the dependency itself is NOT being removed
                if !plan.old_removes.contains_key(dep_on_key) {
                    // Get the mutable info of this dependency from the main installed_packages map
                    if let Some(dep_pkg_info_mut) = self.installed_packages.get_mut(dep_on_key) {
                        let initial_rdep_count = dep_pkg_info_mut.rdepends.len();
                        dep_pkg_info_mut.rdepends.retain(|r| r != removed_pkg_key);
                        if dep_pkg_info_mut.rdepends.len() < initial_rdep_count {
                            log::debug!("Updated rdepends for '{}': removed '{}' (was one of its rdepends)", dep_on_key, removed_pkg_key);
                        } else {
                            log::trace!("Checked rdepends for '{}': '{}' was not found as an rdepend (or already removed)", dep_on_key, removed_pkg_key);
                        }
                    }
                }
            }
        }

        // Run pre-remove scriptlets
        run_scriptlets(
            &plan.old_removes,
            store_root,
            env_root,
            package_format,
            ScriptletType::PreRemove,
            false, // is_upgrade
        )?;

        // Unlink packages
        for (pkgkey, pkg_info) in &plan.old_removes {
            // Ensure pkgline is valid for path construction
            if pkg_info.pkgline.is_empty() || pkg_info.pkgline.contains("/") || pkg_info.pkgline.contains("..") {
                log::error!("Invalid pkgline for {}: '{}'. Skipping unlink.", pkgkey, pkg_info.pkgline);
                return Err(eyre!("Invalid pkgline for {}: '{}'", pkgkey, pkg_info.pkgline));
            }
            let pkg_store_path = store_root.join(&pkg_info.pkgline);
            log::info!("Unlinking files for package: {} from store path {}", pkgkey, pkg_store_path.display());
            self.unlink_package(&pkg_store_path, &env_root.to_path_buf())
                .with_context(|| format!("Failed to unlink package {} (store path: {})", pkgkey, pkg_store_path.display()))?;
            self.installed_packages.remove(pkgkey);
        }

        // Run post-remove scriptlets
        run_scriptlets(
            &plan.old_removes,
            store_root,
            env_root,
            package_format,
            ScriptletType::PostRemove,
            false, // is_upgrade
        )?;

        Ok(())
    }

    /// Execute package installations and upgrades
    fn execute_installations(&mut self, plan: &InstallationPlan, store_root: &Path, env_root: &Path, package_format: PackageFormat) -> Result<()> {
        if plan.fresh_installs.is_empty() && plan.upgrades_new.is_empty() {
            return Ok(());
        }

        // Step 1: Prepare packages for download and processing
        let packages_to_download_and_process = self.prepare_packages_for_installation(plan);

        // Step 2: Download and install packages
        let completed_packages = self.download_and_install_packages(
            &packages_to_download_and_process,
            store_root,
            env_root,
        )?;

        // Step 3: Process upgrades and fresh installations
        self.process_installation_results(plan, &completed_packages, store_root, env_root, package_format)?;

        // Step 4: Update installed packages metadata
        self.installed_packages.extend(completed_packages);

        Ok(())
    }

    /// Prepare packages for download and processing
    fn prepare_packages_for_installation(&mut self, plan: &InstallationPlan) -> HashMap<String, InstalledPackageInfo> {
        let mut packages_to_download_and_process = plan.fresh_installs.clone();
        packages_to_download_and_process.extend(plan.upgrades_new.clone());

        // Remove old versions of upgraded packages from self.installed_packages *before* downloads
        for old_pkgkey_to_remove in plan.upgrades_old.keys() {
            self.installed_packages.remove(old_pkgkey_to_remove);
        }

        packages_to_download_and_process
    }

    /// Download and install packages
    fn download_and_install_packages(
        &mut self,
        packages_to_download_and_process: &HashMap<String, InstalledPackageInfo>,
        store_root: &Path,
        env_root: &Path,
    ) -> Result<HashMap<String, InstalledPackageInfo>> {
        let url_to_pkgkey = self.submit_download_tasks(packages_to_download_and_process)?;
        let pending_urls: Vec<String> = url_to_pkgkey.keys().cloned().collect();

        let mut mutable_packages_for_processing = packages_to_download_and_process.clone();
        let completed_packages = self.process_downloads_and_install(
            &url_to_pkgkey,
            pending_urls,
            &mut mutable_packages_for_processing,
            store_root,
            env_root,
        )?;

        self.fixup_env_links(env_root)?;

        Ok(completed_packages)
    }

    /// Process installation results (upgrades and fresh installations)
    fn process_installation_results(
        &mut self,
        plan: &InstallationPlan,
        completed_packages: &HashMap<String, InstalledPackageInfo>,
        store_root: &Path,
        env_root: &Path,
        package_format: PackageFormat,
    ) -> Result<()> {
        // Process upgrades
        let mut upgrades_new_completed: HashMap<String, InstalledPackageInfo> = HashMap::new();
        for (pkgkey, info) in completed_packages {
            if plan.upgrades_new.contains_key(pkgkey) {
                upgrades_new_completed.insert(pkgkey.clone(), info.clone());
            }
        }

        if !upgrades_new_completed.is_empty() {
            log::info!("Processing {} upgrades", upgrades_new_completed.len());
            self.process_upgrades(&plan.upgrades_old, &upgrades_new_completed, store_root, env_root, package_format)?;
        }

        // Process fresh installations
        let mut fresh_installs_completed: HashMap<String, InstalledPackageInfo> = HashMap::new();
        for (pkgkey, info) in completed_packages {
            if plan.fresh_installs.contains_key(pkgkey) {
                fresh_installs_completed.insert(pkgkey.clone(), info.clone());
            }
        }

        if !fresh_installs_completed.is_empty() {
            log::info!("Processing {} fresh installations", fresh_installs_completed.len());
            self.process_fresh_installs(&fresh_installs_completed, store_root, env_root, package_format)?;
        }

        Ok(())
    }

    /// Handle unexpose operations (del_exposes)
    fn execute_unexpose_operations(&mut self, plan: &InstallationPlan, env_root: &Path) -> Result<()> {
        for (pkgkey, pkg_info) in &plan.del_exposes {
            // Remove ebin wrappers for packages being unexposed
            if !pkg_info.ebin_links.is_empty() {
                log::info!("Unexposing package: {}", pkgkey);
                for relative_ebin_path_str in &pkg_info.ebin_links {
                    let ebin_path = env_root.join(relative_ebin_path_str);
                    if fs::symlink_metadata(&ebin_path).is_ok() {
                        log::debug!("Removing ebin wrapper: {}", ebin_path.display());
                        fs::remove_file(&ebin_path)
                            .with_context(|| format!("Failed to remove ebin wrapper {}", ebin_path.display()))?;
                    } else {
                        log::warn!("Ebin wrapper listed in metadata not found for removal: {}", ebin_path.display());
                    }
                }
            }

            // Update the package info to clear ebin_links
            if let Some(installed_package_info_mut) = self.installed_packages.get_mut(pkgkey) {
                installed_package_info_mut.ebin_links.clear();
                installed_package_info_mut.ebin_exposure = false;
            }
        }

        Ok(())
    }

    /// Handle expose operations (new_exposes)
    fn execute_expose_operations(&mut self, plan: &InstallationPlan, store_root: &Path, env_root: &Path) -> Result<()> {
        for (pkgkey, _pkg_info) in &plan.new_exposes {
            log::info!("Exposing package: {}", pkgkey);

            // Use the updated package info from self.installed_packages which has the correct pkgline
            let installed_pkg_info = self.installed_packages.get(pkgkey)
                .ok_or_else(|| eyre::eyre!("Package {} not found in installed_packages for exposure", pkgkey))?;

            // Check if pkgline is empty, which would indicate the package wasn't properly processed
            if installed_pkg_info.pkgline.is_empty() {
                return Err(eyre::eyre!("Package {} has empty pkgline, cannot expose. This indicates the package wasn't properly downloaded and processed.", pkgkey));
            }

            let store_fs_dir = store_root.join(installed_pkg_info.pkgline.clone()).join("fs");
            let links = self.expose_package(&store_fs_dir, &env_root.to_path_buf())
                .with_context(|| format!("Failed to expose package {}", pkgkey))?;

            // Update the package info with the new links
            if let Some(installed_package_info_mut) = self.installed_packages.get_mut(pkgkey) {
                installed_package_info_mut.ebin_links = links.clone();
                installed_package_info_mut.ebin_exposure = true;
            } else {
                log::warn!("execute_expose_operations: pkgkey '{}' from new_exposes not found in self.installed_packages. Ebin links not stored.", pkgkey);
            }
        }

        Ok(())
    }

    /// Update metadata for packages that were already installed but involved in this session
    fn update_skipped_reinstalls_metadata(&mut self, plan: &InstallationPlan) -> Result<()> {
        for (pkgkey, session_info) in &plan.skipped_reinstalls {
            if let Some(installed_info) = self.installed_packages.get_mut(pkgkey) {
                // Only update fields that can change between sessions.
                // Crucially, DO NOT overwrite `pkgline` or `install_time`.
                installed_info.depend_depth = session_info.depend_depth;
                installed_info.ebin_exposure = session_info.ebin_exposure;
                installed_info.depends = session_info.depends.clone();
                installed_info.rdepends = session_info.rdepends.clone();
            }
        }
        Ok(())
    }

    /// Process downloads and install packages as they complete
    fn process_downloads_and_install(
        &mut self,
        url_to_pkgkey: &HashMap<String, String>,
        mut pending_urls: Vec<String>,
        packages_to_install: &mut HashMap<String, InstalledPackageInfo>,
        store_root: &Path,
        env_root: &Path,
    ) -> Result<HashMap<String, InstalledPackageInfo>> {
        let mut completed_packages: HashMap<String, InstalledPackageInfo> = HashMap::new();

        // Process packages as downloads complete
        while !pending_urls.is_empty() {
            // Wait for any download to complete
            if let Some(completed_url) = download::wait_for_any_download_task(&pending_urls)? {
                // Get the package key for this completed URL
                let completed_pkgkey = url_to_pkgkey.get(&completed_url).cloned();

                if let Some(pkgkey) = completed_pkgkey {
                    // Remove from pending list
                    pending_urls.retain(|url| *url != completed_url);

                    // Process the downloaded package
                    if let Some((actual_pkgkey, package_info)) = self.process_downloaded_package(
                        &pkgkey,
                        packages_to_install,
                        store_root,
                        env_root,
                    )? {
                        // Store completed package
                        completed_packages.insert(actual_pkgkey, package_info);
                    }
                } else {
                    log::warn!("Could not find package key for completed URL: {}", completed_url);
                }
            }
        }

        Ok(completed_packages)
    }

    /// Process a downloaded package file
    ///
    /// - Gets the file path for the package
    /// - Unpacks the package
    /// - Updates package info with the pkgline
    /// - Links the package (exposure happens later)
    ///
    /// Returns the actual package key and updated package info if successful
    fn process_downloaded_package(
        &mut self,
        pkgkey: &str,
        packages_to_install: &mut HashMap<String, InstalledPackageInfo>,
        store_root: &Path,
        env_root: &Path,
    ) -> Result<Option<(String, InstalledPackageInfo)>> {
        // Get the downloaded file path
        let file_path = self.get_package_file_path(pkgkey)?;

        // Unpack the package
        let final_dir = crate::store::unpack_mv_package(&file_path, Some(pkgkey))
            .with_context(|| format!("Failed to unpack package: {}", file_path))?;

        // Get the pkgline from the directory name
        let pkgline = final_dir.file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| eyre!("Invalid UTF-8 in package directory name: {}", final_dir.display()))?;

        // Parse the pkgline which now includes architecture
        let parsed = package::parse_pkgline(pkgline)
            .map_err(|e| eyre!("Failed to parse package line: {}", e))?;

        // Format the package key using the exact architecture from the package
        let actual_pkgkey = package::format_pkgkey(&parsed.pkgname, &parsed.version, &parsed.arch);

        // Update the package info with the pkgline
        let mut package_info = packages_to_install.remove(pkgkey)
            .or_else(|| packages_to_install.remove(&actual_pkgkey))
            .ok_or_else(|| eyre!("Package key not found: {} (or {})", pkgkey, actual_pkgkey))?;
        package_info.pkgline = pkgline.to_string();

        // Link new package immediately after unpacking
        let store_fs_dir = store_root.join(package_info.pkgline.clone()).join("fs");
        self.link_package(&store_fs_dir, &env_root.to_path_buf())
            .with_context(|| format!("Failed to link package {}", actual_pkgkey))?;

        // Exposure will happen later in install_pkgkeys

        Ok(Some((actual_pkgkey, package_info)))
    }

    /// Process upgrade flow for packages
    fn process_upgrades(
        &mut self,
        old_packages: &HashMap<String, InstalledPackageInfo>,
        new_packages: &HashMap<String, InstalledPackageInfo>,
        store_root: &Path,
        env_root: &Path,
        package_format: PackageFormat,
    ) -> Result<()> {
        // Process each package upgrade individually
        for (pkgkey, new_package_info) in new_packages {
            if let Some(old_package_info) = old_packages.get(pkgkey) {
                log::info!("Upgrading package: {}", pkgkey);
                self.process_single_package_upgrade(
                    pkgkey,
                    old_package_info,
                    new_package_info,
                    store_root,
                    env_root,
                    package_format,
                )?;
            } else {
                log::warn!("Old package info not found for upgrade: {}", pkgkey);
            }
        }
        Ok(())
    }

    /// Process upgrade flow for a single package pair
    fn process_single_package_upgrade(
        &mut self,
        pkgkey: &str,
        old_package_info: &InstalledPackageInfo,
        new_package_info: &InstalledPackageInfo,
        store_root: &Path,
        env_root: &Path,
        package_format: PackageFormat,
    ) -> Result<()> {
        use crate::scriptlets::{run_scriptlet, ScriptletType};

        // Extract version information
        let old_version = crate::package::pkgkey2version(pkgkey).ok();
        let new_version = crate::package::pkgkey2version(pkgkey).ok();

        log::debug!(
            "Processing upgrade for {}: {} -> {}",
            pkgkey,
            old_version.as_deref().unwrap_or("unknown"),
            new_version.as_deref().unwrap_or("unknown")
        );

        // Step 1: New package pre-upgrade (with old version info)
        run_scriptlet(
            pkgkey,
            new_package_info,
            store_root,
            env_root,
            package_format,
            ScriptletType::PreUpgrade,
            true, // is_upgrade
            old_version.as_deref(),
            new_version.as_deref(),
        )?;

        // Step 2: Old package pre-remove (with new version info)
        run_scriptlet(
            pkgkey,
            old_package_info,
            store_root,
            env_root,
            package_format,
            ScriptletType::PreRemove,
            true, // is_upgrade
            old_version.as_deref(),
            new_version.as_deref(),
        )?;

        // Step 3: Link new package files to env
        // Done in process_downloaded_package() for now
        // let new_store_fs_dir = store_root.join(&new_package_info.pkgline).join("fs");
        // self.link_package(&new_store_fs_dir, &env_root.to_path_buf())
        //     .with_context(|| format!("Failed to link new package {}", new_package_info.pkgline))?;

        // Step 4: Unlink old package unique files (files in old_pkg but not in new_pkg)
        self.unlink_package_diff(old_package_info, new_package_info, store_root, env_root)
            .with_context(|| format!("Failed to unlink old package files for {}", pkgkey))?;

        // Step 5: New package post-upgrade (with old version info)
        run_scriptlet(
            pkgkey,
            new_package_info,
            store_root,
            env_root,
            package_format,
            ScriptletType::PostUpgrade,
            true, // is_upgrade
            old_version.as_deref(),
            new_version.as_deref(),
        )?;

        // Step 6: Old package post-remove (with new version info)
        run_scriptlet(
            pkgkey,
            old_package_info,
            store_root,
            env_root,
            package_format,
            ScriptletType::PostRemove,
            true, // is_upgrade
            old_version.as_deref(),
            new_version.as_deref(),
        )?;

        log::info!("Successfully upgraded package: {}", pkgkey);
        Ok(())
    }

    /// Unlink files that are in old_package but not in new_package
    /// This implements the Set(old_pkg - new_pkg) logic
    fn unlink_package_diff(
        &self,
        old_package_info: &InstalledPackageInfo,
        new_package_info: &InstalledPackageInfo,
        store_root: &Path,
        env_root: &Path,
    ) -> Result<()> {
        // Get file lists for both packages
        let old_store_fs_dir = store_root.join(&old_package_info.pkgline).join("fs");
        let new_store_fs_dir = store_root.join(&new_package_info.pkgline).join("fs");

        let old_files = utils::list_package_files(old_store_fs_dir.to_str()
            .ok_or_else(|| eyre::eyre!("Invalid old package fs path"))?)?;
        let new_files = utils::list_package_files(new_store_fs_dir.to_str()
            .ok_or_else(|| eyre::eyre!("Invalid new package fs path"))?)?;

        // Convert to sets of relative paths for comparison
        let old_rel_paths: std::collections::HashSet<PathBuf> = old_files
            .iter()
            .filter_map(|path| path.strip_prefix(&old_store_fs_dir).ok().map(|p| p.to_path_buf()))
            .collect();

        let new_rel_paths: std::collections::HashSet<PathBuf> = new_files
            .iter()
            .filter_map(|path| path.strip_prefix(&new_store_fs_dir).ok().map(|p| p.to_path_buf()))
            .collect();

        // Find files that are in old package but not in new package
        let files_to_remove: Vec<PathBuf> = old_rel_paths
            .difference(&new_rel_paths)
            .cloned()
            .collect();

        log::debug!(
            "Found {} files to remove during upgrade: old_pkg={}, new_pkg={}",
            files_to_remove.len(),
            old_package_info.pkgline,
            new_package_info.pkgline
        );

        // Remove the files from environment
        for rel_path in &files_to_remove {
            let env_file_path = env_root.join(rel_path);

            if env_file_path.exists() {
                if env_file_path.is_dir() {
                    // Only remove directory if it's empty
                    match std::fs::read_dir(&env_file_path) {
                        Ok(mut entries) => {
                            if entries.next().is_none() {
                                log::debug!("Removing empty directory: {}", env_file_path.display());
                                std::fs::remove_dir(&env_file_path)
                                    .with_context(|| format!("Failed to remove directory {}", env_file_path.display()))?;
                            } else {
                                log::debug!("Directory not empty, skipping: {}", env_file_path.display());
                            }
                        }
                        Err(_) => {
                            log::debug!("Cannot read directory, skipping: {}", env_file_path.display());
                        }
                    }
                } else {
                    log::debug!("Removing file: {}", env_file_path.display());
                    std::fs::remove_file(&env_file_path)
                        .with_context(|| format!("Failed to remove file {}", env_file_path.display()))?;
                }
            }
        }

        if !files_to_remove.is_empty() {
            log::info!(
                "Removed {} unique files from old package during upgrade",
                files_to_remove.len()
            );
        }

        Ok(())
    }

    /// Process fresh install flow for packages
    fn process_fresh_installs(
        &mut self,
        fresh_installs: &HashMap<String, InstalledPackageInfo>,
        store_root: &Path,
        env_root: &Path,
        package_format: PackageFormat,
    ) -> Result<()> {
        use crate::scriptlets::{run_scriptlets, ScriptletType};

        // Fresh install flow:
        // 1. pre_install  (check dependencies/conflicts)
        // 2. install files (link packages)
        // 3. post_install (start services/update config)

        // Step 1: Pre-install
        run_scriptlets(
            fresh_installs,
            store_root,
            env_root,
            package_format,
            ScriptletType::PreInstall,
            false, // is_upgrade
        )?;

        // Step 2: Install files (link packages)
        // This is moved earlier to process_downloaded_package(), so that scriptlets have command to run.
        // for (_, package_info) in fresh_installs {
        //     let store_fs_dir = store_root.join(&package_info.pkgline).join("fs");
        //     self.link_package(&store_fs_dir, &env_root.to_path_buf())
        //         .with_context(|| format!("Failed to link package {}", package_info.pkgline))?;
        // }

        // Step 3: Post-install
        run_scriptlets(
            fresh_installs,
            store_root,
            env_root,
            package_format,
            ScriptletType::PostInstall,
            false, // is_upgrade
        )?;

        // Run ldconfig if needed
        run_ldconfig_if_needed(env_root)?;

        Ok(())
    }

    /// Auto-add items to plan.del_exposes/new_exposes based on ebin_exposure status
    /// This function automatically populates the expose fields based on the installation/removal plan
    pub fn auto_populate_expose_plan(plan: &mut InstallationPlan) {
        // Track exposure changes for packages being removed
        for (pkgkey, pkg_info) in &plan.old_removes {
            if pkg_info.ebin_exposure {
                // Package being removed was exposed - will be unexposed
                plan.del_exposes.insert(pkgkey.clone(), pkg_info.clone());
            }
        }

        // Track exposure changes for packages being upgraded (old versions)
        for (pkgkey, pkg_info) in &plan.upgrades_old {
            if pkg_info.ebin_exposure {
                // Old version being upgraded was exposed - will be unexposed
                plan.del_exposes.insert(pkgkey.clone(), pkg_info.clone());
            }
        }

        // Track exposure changes for new packages being installed
        for (pkgkey, pkg_info) in &plan.fresh_installs {
            if pkg_info.ebin_exposure {
                // New package being installed should be exposed
                plan.new_exposes.insert(pkgkey.clone(), pkg_info.clone());
            }
        }

        // Track exposure changes for packages being upgraded (new versions)
        for (pkgkey, pkg_info) in &plan.upgrades_new {
            if pkg_info.ebin_exposure {
                // New version being upgraded should be exposed
                plan.new_exposes.insert(pkgkey.clone(), pkg_info.clone());
            }
        }
    }

    fn print_packages_by_depend_depth(&mut self, packages: &HashMap<String, InstalledPackageInfo>) {
        // Convert HashMap to a Vec of tuples (pkgkey, info)
        let mut packages_vec: Vec<(&String, &InstalledPackageInfo)> = packages.iter().collect();

        // Sort by depend_depth
        packages_vec.sort_by(|a, b| a.1.depend_depth.cmp(&b.1.depend_depth));

        // Print the header
        println!("{:<5} {:>10}  {:<30}", "DEPTH", "SIZE", "PACKAGE");

        // Print each package
        for (pkgkey, info) in packages_vec {
            // Try to load package info to get size
            let size_str = match self.load_package_info(pkgkey) {
                Ok(package) => {
                    if package.size > 0 {
                        format!("{}", utils::format_size(package.size as u64))
                    } else {
                        "".to_string()
                    }
                }
                Err(_) => "".to_string(),
            };

            println!("{:<5} {:>10}  {:<30}", info.depend_depth, size_str, pkgkey);
        }
    }
}

/// Track additional exposure changes for packages in skipped_reinstalls
/// This function handles cases where packages exist in both old and new states but have exposure changes
pub fn track_additional_exposure_changes(
    plan: &mut InstallationPlan,
    new_packages: &HashMap<String, InstalledPackageInfo>,
    old_packages: &HashMap<String, InstalledPackageInfo>,
) {
    for (pkgkey, new_info) in new_packages {
        if let Some(old_info) = old_packages.get(pkgkey) {
            // Package exists in both - check for exposure changes not covered by auto-populate
            if old_info.ebin_exposure != new_info.ebin_exposure {
                // This is already handled by auto_populate_expose_plan for fresh_installs and upgrades_new
                // Only handle cases where packages are in skipped_reinstalls but have exposure changes
                if plan.skipped_reinstalls.contains_key(pkgkey) {
                    if new_info.ebin_exposure {
                        // Package will be newly exposed
                        plan.new_exposes.insert(pkgkey.clone(), new_info.clone());
                    } else {
                        // Package will be unexposed
                        plan.del_exposes.insert(pkgkey.clone(), old_info.clone());
                    }
                }
            }
        }
    }
}

/// Determine if a package is an upgrade by comparing package names and architectures
/// Returns (is_upgrade, old_pkgkey) if it's an upgrade, (false, "") otherwise
pub fn find_upgrade_target(
    new_pkgkey: &str,
    _new_pkg_info: &InstalledPackageInfo,
    old_packages: &HashMap<String, InstalledPackageInfo>,
) -> (bool, String) {
    let (new_pkgname, _, new_arch) = match package::parse_pkgkey(new_pkgkey) {
        Ok(parts) => parts,
        Err(_) => return (false, String::new()),
    };

    for (old_pkgkey, _old_pkg_info) in old_packages {
        if old_pkgkey == new_pkgkey {
            continue;
        }

        match package::parse_pkgkey(old_pkgkey) {
            Ok((old_pkgname, _, old_arch)) => {
                if new_pkgname == old_pkgname && new_arch == old_arch {
                    return (true, old_pkgkey.clone());
                }
            }
            Err(_) => {
                // Skip invalid package keys
                continue;
            }
        }
    }

    (false, String::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_env_based_shebangs() {
        // Basic env python
        let info = parse_shebang_for_wrapper("#!/usr/bin/env python").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/python");
        assert_eq!(info.interpreter_basename, "python");
        assert_eq!(info.remaining_params, "");

        // Python3 variant
        let info = parse_shebang_for_wrapper("#!/usr/bin/env python3 ").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/python3");
        assert_eq!(info.interpreter_basename, "python3");
        assert_eq!(info.remaining_params, "");

        // Python with version
        let info = parse_shebang_for_wrapper("#!/usr/bin/env python3.11").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/python3.11");
        assert_eq!(info.interpreter_basename, "python3.11");
        assert_eq!(info.remaining_params, "");

        // Python with options
        let info = parse_shebang_for_wrapper("#!/usr/bin/env python -u").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/python");
        assert_eq!(info.interpreter_basename, "python");
        assert_eq!(info.remaining_params, "-u");

        // Python with multiple options
        let info = parse_shebang_for_wrapper("#!/usr/bin/env python3 -u -O ").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/python3");
        assert_eq!(info.interpreter_basename, "python3");
        assert_eq!(info.remaining_params, "-u -O");

        // Node.js
        let info = parse_shebang_for_wrapper("#!/usr/bin/env node").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/node");
        assert_eq!(info.interpreter_basename, "node");
        assert_eq!(info.remaining_params, "");

        // Node.js with options
        let info = parse_shebang_for_wrapper("#!/usr/bin/env node --experimental-modules").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/node");
        assert_eq!(info.interpreter_basename, "node");
        assert_eq!(info.remaining_params, "--experimental-modules");

        // Ruby
        let info = parse_shebang_for_wrapper("#!/usr/bin/env ruby").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/ruby");
        assert_eq!(info.interpreter_basename, "ruby");
        assert_eq!(info.remaining_params, "");

        // Perl
        let info = parse_shebang_for_wrapper("#!/usr/bin/env perl").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/perl");
        assert_eq!(info.interpreter_basename, "perl");
        assert_eq!(info.remaining_params, "");

        // PHP
        let info = parse_shebang_for_wrapper("#!/usr/bin/env php").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/php");
        assert_eq!(info.interpreter_basename, "php");
        assert_eq!(info.remaining_params, "");

        // Bash via env
        let info = parse_shebang_for_wrapper("#!/usr/bin/env bash").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/bash");
        assert_eq!(info.interpreter_basename, "bash");
        assert_eq!(info.remaining_params, "");

        // Zsh via env
        let info = parse_shebang_for_wrapper("#!/usr/bin/env zsh").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/zsh");
        assert_eq!(info.interpreter_basename, "zsh");
        assert_eq!(info.remaining_params, "");
    }

    #[test]
    fn test_direct_interpreter_shebangs() {
        // Standard shell
        let info = parse_shebang_for_wrapper("#! /bin/sh").unwrap();
        assert_eq!(info.interpreter_path, "/bin/sh");
        assert_eq!(info.interpreter_basename, "sh");
        assert_eq!(info.remaining_params, "");

        // Bash
        let info = parse_shebang_for_wrapper("#!/bin/bash ").unwrap();
        assert_eq!(info.interpreter_path, "/bin/bash");
        assert_eq!(info.interpreter_basename, "bash");
        assert_eq!(info.remaining_params, "");

        // Bash with options
        let info = parse_shebang_for_wrapper("#!/bin/bash -e ").unwrap();
        assert_eq!(info.interpreter_path, "/bin/bash");
        assert_eq!(info.interpreter_basename, "bash");
        assert_eq!(info.remaining_params, "-e");

        // Bash with multiple options
        let info = parse_shebang_for_wrapper("#!/bin/bash -eu -o pipefail").unwrap();
        assert_eq!(info.interpreter_path, "/bin/bash");
        assert_eq!(info.interpreter_basename, "bash");
        assert_eq!(info.remaining_params, "-eu -o pipefail");

        // Python direct
        let info = parse_shebang_for_wrapper("#!/usr/bin/python3").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/python3");
        assert_eq!(info.interpreter_basename, "python3");
        assert_eq!(info.remaining_params, "");

        // Python with version and options
        let info = parse_shebang_for_wrapper("#!/usr/bin/python3.11 -u").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/python3.11");
        assert_eq!(info.interpreter_basename, "python3.11");
        assert_eq!(info.remaining_params, "-u");

        // Perl direct
        let info = parse_shebang_for_wrapper("#!/usr/bin/perl").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/perl");
        assert_eq!(info.interpreter_basename, "perl");
        assert_eq!(info.remaining_params, "");

        // Ruby direct
        let info = parse_shebang_for_wrapper("#!/usr/bin/ruby").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/ruby");
        assert_eq!(info.interpreter_basename, "ruby");
        assert_eq!(info.remaining_params, "");

        // Node.js direct
        let info = parse_shebang_for_wrapper("#!/usr/bin/node").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/node");
        assert_eq!(info.interpreter_basename, "node");
        assert_eq!(info.remaining_params, "");

        // Lua
        let info = parse_shebang_for_wrapper("#!/usr/bin/lua").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/lua");
        assert_eq!(info.interpreter_basename, "lua");
        assert_eq!(info.remaining_params, "");

        // AWK
        let info = parse_shebang_for_wrapper("#!/usr/bin/awk -f").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/awk");
        assert_eq!(info.interpreter_basename, "awk");
        assert_eq!(info.remaining_params, "-f");

        // GNU AWK
        let info = parse_shebang_for_wrapper("#!/usr/bin/gawk -f").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/gawk");
        assert_eq!(info.interpreter_basename, "gawk");
        assert_eq!(info.remaining_params, "-f");

        // Tcl/Tk
        let info = parse_shebang_for_wrapper("#!/usr/bin/tclsh").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/tclsh");
        assert_eq!(info.interpreter_basename, "tclsh");
        assert_eq!(info.remaining_params, "");

        // Fish shell
        let info = parse_shebang_for_wrapper("#!/usr/bin/fish").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/fish");
        assert_eq!(info.interpreter_basename, "fish");
        assert_eq!(info.remaining_params, "");
    }

    #[test]
    fn test_exotic_shebangs() {
        // Different env paths
        let info = parse_shebang_for_wrapper("#!/bin/env python").unwrap();
        assert_eq!(info.interpreter_path, "/bin/env");
        assert_eq!(info.interpreter_basename, "env");
        assert_eq!(info.remaining_params, "python");

        // Executable in non-standard location
        let info = parse_shebang_for_wrapper("#!/opt/python/bin/python").unwrap();
        assert_eq!(info.interpreter_path, "/opt/python/bin/python");
        assert_eq!(info.interpreter_basename, "python");
        assert_eq!(info.remaining_params, "");

        // Local installation
        let info = parse_shebang_for_wrapper("#!/usr/local/bin/python3").unwrap();
        assert_eq!(info.interpreter_path, "/usr/local/bin/python3");
        assert_eq!(info.interpreter_basename, "python3");
        assert_eq!(info.remaining_params, "");

        // Complex paths
        let info = parse_shebang_for_wrapper("#!/home/user/.local/bin/custom-script").unwrap();
        assert_eq!(info.interpreter_path, "/home/user/.local/bin/custom-script");
        assert_eq!(info.interpreter_basename, "custom-script");
        assert_eq!(info.remaining_params, "");

        // Hyphenated interpreter names
        let info = parse_shebang_for_wrapper("#!/usr/bin/env python-config").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/python-config");
        assert_eq!(info.interpreter_basename, "python-config");
        assert_eq!(info.remaining_params, "");

        // Dotted interpreter names
        let info = parse_shebang_for_wrapper("#!/usr/bin/env python3.11-config").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/python3.11-config");
        assert_eq!(info.interpreter_basename, "python3.11-config");
        assert_eq!(info.remaining_params, "");
    }

    #[test]
    fn test_edge_cases() {
        // Empty env params should fail
        let result = parse_shebang_for_wrapper("#!/usr/bin/env ");
        assert!(result.is_err());

        // No shebang
        let result = parse_shebang_for_wrapper("#!/usr/bin/env");
        assert!(result.is_err());

        // Multiple spaces
        let info = parse_shebang_for_wrapper("#! /usr/bin/env   python3   -u   -O").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/python3");
        assert_eq!(info.interpreter_basename, "python3");
        assert_eq!(info.remaining_params, "-u -O");

        // Tabs instead of spaces
        let info = parse_shebang_for_wrapper("#!/usr/bin/env\tpython3\t-u").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/python3");
        assert_eq!(info.interpreter_basename, "python3");
        assert_eq!(info.remaining_params, "-u");

        // Space after #! (common in real world)
        let info = parse_shebang_for_wrapper("#! /bin/bash").unwrap();
        assert_eq!(info.interpreter_path, "/bin/bash");
        assert_eq!(info.interpreter_basename, "bash");
        assert_eq!(info.remaining_params, "");

        // Space after #! with parameters
        let info = parse_shebang_for_wrapper("#! /bin/bash -e").unwrap();
        assert_eq!(info.interpreter_path, "/bin/bash");
        assert_eq!(info.interpreter_basename, "bash");
        assert_eq!(info.remaining_params, "-e");

        // Space after #! with env
        let info = parse_shebang_for_wrapper("#! /usr/bin/env python3").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/python3");
        assert_eq!(info.interpreter_basename, "python3");
        assert_eq!(info.remaining_params, "");

        // Space after #! with env and options
        let info = parse_shebang_for_wrapper("#! /usr/bin/env python3 -u").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/python3");
        assert_eq!(info.interpreter_basename, "python3");
        assert_eq!(info.remaining_params, "-u");

        // Multiple spaces after #!
        let info = parse_shebang_for_wrapper("#!   /bin/bash").unwrap();
        assert_eq!(info.interpreter_path, "/bin/bash");
        assert_eq!(info.interpreter_basename, "bash");
        assert_eq!(info.remaining_params, "");

        // Tab after #!
        let info = parse_shebang_for_wrapper("#!\t/bin/bash").unwrap();
        assert_eq!(info.interpreter_path, "/bin/bash");
        assert_eq!(info.interpreter_basename, "bash");
        assert_eq!(info.remaining_params, "");
    }

    #[test]
    fn test_invalid_shebangs() {
        // No shebang prefix
        let result = parse_shebang_for_wrapper("python script");
        assert!(result.is_err());

        // Just hash
        let result = parse_shebang_for_wrapper("#python");
        assert!(result.is_err());

        // Empty string
        let result = parse_shebang_for_wrapper("");
        assert!(result.is_err());

        // Only shebang
        let result = parse_shebang_for_wrapper("#!");
        assert!(result.is_ok()); // This actually parses as empty interpreter path
    }

    #[test]
    fn test_real_world_examples() {
        // From Django management commands
        let info = parse_shebang_for_wrapper("#!/usr/bin/env python").unwrap();
        assert_eq!(info.interpreter_basename, "python");

        // From Node.js scripts
        let info = parse_shebang_for_wrapper("#!/usr/bin/env node").unwrap();
        assert_eq!(info.interpreter_basename, "node");

        // From system scripts
        let info = parse_shebang_for_wrapper("#!/bin/bash").unwrap();
        assert_eq!(info.interpreter_basename, "bash");

        // From build scripts
        let info = parse_shebang_for_wrapper("#!/bin/sh").unwrap();
        assert_eq!(info.interpreter_basename, "sh");

        // From Python virtual environments
        let info = parse_shebang_for_wrapper("#!/usr/bin/env python3").unwrap();
        assert_eq!(info.interpreter_basename, "python3");

        // From Ruby gems
        let info = parse_shebang_for_wrapper("#!/usr/bin/env ruby").unwrap();
        assert_eq!(info.interpreter_basename, "ruby");

        // From Perl scripts
        let info = parse_shebang_for_wrapper("#!/usr/bin/perl -w").unwrap();
        assert_eq!(info.interpreter_basename, "perl");
        assert_eq!(info.remaining_params, "-w");

        // From AWK scripts
        let info = parse_shebang_for_wrapper("#!/usr/bin/awk -f").unwrap();
        assert_eq!(info.interpreter_basename, "awk");
        assert_eq!(info.remaining_params, "-f");
    }

    #[test]
    fn test_user_provided_real_world_cases() {
        // Based on actual usage data from the user

        // #!/usr/bin/env ruby (192 occurrences)
        let info = parse_shebang_for_wrapper("#!/usr/bin/env ruby").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/ruby");
        assert_eq!(info.interpreter_basename, "ruby");
        assert_eq!(info.remaining_params, "");

        // #!/bin/sh (13 occurrences)
        let info = parse_shebang_for_wrapper("#!/bin/sh").unwrap();
        assert_eq!(info.interpreter_path, "/bin/sh");
        assert_eq!(info.interpreter_basename, "sh");
        assert_eq!(info.remaining_params, "");

        // #!/usr/bin/awk -f (9 occurrences)
        let info = parse_shebang_for_wrapper("#!/usr/bin/awk -f").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/awk");
        assert_eq!(info.interpreter_basename, "awk");
        assert_eq!(info.remaining_params, "-f");

        // #!/usr/bin/env -S awk -f (4 occurrences) - env with -S flag
        let info = parse_shebang_for_wrapper("#!/usr/bin/env -S awk -f").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/awk");
        assert_eq!(info.interpreter_basename, "awk");
        assert_eq!(info.remaining_params, "-f");

        // #!/usr/bin/env bash (2 occurrences)
        let info = parse_shebang_for_wrapper("#!/usr/bin/env bash").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/bash");
        assert_eq!(info.interpreter_basename, "bash");
        assert_eq!(info.remaining_params, "");

        // #!/bin/bash (2 occurrences)
        let info = parse_shebang_for_wrapper("#!/bin/bash").unwrap();
        assert_eq!(info.interpreter_path, "/bin/bash");
        assert_eq!(info.interpreter_basename, "bash");
        assert_eq!(info.remaining_params, "");
    }

    #[test]
    fn test_env_s_flag_variations() {
        // Basic env -S usage
        let info = parse_shebang_for_wrapper("#!/usr/bin/env -S awk -f").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/awk");
        assert_eq!(info.interpreter_basename, "awk");
        assert_eq!(info.remaining_params, "-f");

        // env -S with multiple arguments
        let info = parse_shebang_for_wrapper("#!/usr/bin/env -S python3 -u -O").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/python3");
        assert_eq!(info.interpreter_basename, "python3");
        assert_eq!(info.remaining_params, "-u -O");

        // env -S with just interpreter, no additional args
        let info = parse_shebang_for_wrapper("#!/usr/bin/env -S python3").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/python3");
        assert_eq!(info.interpreter_basename, "python3");
        assert_eq!(info.remaining_params, "");

        // Space after #! with env -S
        let info = parse_shebang_for_wrapper("#! /usr/bin/env -S awk -f").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/awk");
        assert_eq!(info.interpreter_basename, "awk");
        assert_eq!(info.remaining_params, "-f");

        // Multiple spaces with env -S
        let info = parse_shebang_for_wrapper("#!/usr/bin/env   -S   python3   -u").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/python3");
        assert_eq!(info.interpreter_basename, "python3");
        assert_eq!(info.remaining_params, "-u");

        // env -S with complex interpreter names
        let info = parse_shebang_for_wrapper("#!/usr/bin/env -S python3.11 -u").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/python3.11");
        assert_eq!(info.interpreter_basename, "python3.11");
        assert_eq!(info.remaining_params, "-u");

        // env -S with hyphenated interpreter
        let info = parse_shebang_for_wrapper("#!/usr/bin/env -S python-config --version").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/python-config");
        assert_eq!(info.interpreter_basename, "python-config");
        assert_eq!(info.remaining_params, "--version");
    }

    #[test]
    fn test_env_s_flag_edge_cases() {
        // Test that regular env (non -S) still works
        let info = parse_shebang_for_wrapper("#!/usr/bin/env python3").unwrap();
        assert_eq!(info.interpreter_path, "/usr/bin/python3");
        assert_eq!(info.interpreter_basename, "python3");
        assert_eq!(info.remaining_params, "");
    }
}

