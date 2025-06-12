use std::fs;
use std::io::Seek;
use std::io::Write;
use std::io::SeekFrom;
use std::path::Path;
use color_eyre::eyre::eyre;
use std::path::PathBuf;
use std::collections::HashMap;
use std::os::unix::fs::symlink;
use std::os::unix::fs::PermissionsExt;
use color_eyre::eyre::{self, Result, Context};
use crate::models::*;
use crate::utils::*;
use crate::dirs::find_env_root;
use crate::package;
use crate::download::wait_for_any_download_task;
use crate::scriptlets::{run_scriptlets, run_scriptlet, ScriptletType};

fn print_packages_by_depend_depth(packages: &HashMap<String, InstalledPackageInfo>) {
    // Convert HashMap to a Vec of tuples (pkgkey, info)
    let mut packages_vec: Vec<(&String, &InstalledPackageInfo)> = packages.iter().collect();

    // Sort by depend_depth
    packages_vec.sort_by(|a, b| a.1.depend_depth.cmp(&b.1.depend_depth));

    // Print the header
    println!("{:<12} {:<10}", "depend_depth", "package");

    // Print each package
    for (pkgkey, info) in packages_vec {
        println!("{:<12} {:<10}", info.depend_depth, pkgkey);
    }
}

/// Finds duplicates between `a` and `b`,
/// shows a warning about the duplicates, and removes them from `b`.
fn remove_duplicates(
    a: &HashMap<String, InstalledPackageInfo>,
    b: &mut HashMap<String, InstalledPackageInfo>,
    warn: &str) {

    let duplicates: Vec<_> = b
        .keys()
        .filter(|&package_name| a.contains_key(package_name))
        .cloned()
        .collect();

    if !duplicates.is_empty() {
        if !warn.is_empty() {
            log::info!("{} {:?}", warn, duplicates);
        }

        // Remove duplicates from `b`
        for package_name in duplicates {
            // appbin_flag 变更需要处理
            if a.get(&package_name).unwrap().appbin_flag ==
               b.get(&package_name).unwrap().appbin_flag {
                b.remove(&package_name);
            }
        }
    }
}

fn handle_elf(target_path: &Path, env_root: &Path, fs_file: &Path) -> Result<()> {
    // Get common environment root path
    let common_env_root = find_env_root("common")
        .ok_or_else(|| eyre::eyre!("Common environment not found"))?;

    // Path to elf-loader in common environment
    let elf_loader_path = common_env_root.join("usr/bin/elf-loader");

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

fn mirror_dir(env_root: &Path, store_fs_dir: &Path, fs_files: &[PathBuf]) -> Result<()> {
    for fs_file in fs_files {
        let fhs_file = fs_file.strip_prefix(store_fs_dir)
            .with_context(|| format!("Failed to strip prefix {} from {}", store_fs_dir.display(), fs_file.display()))?;
        let target_path = env_root.join(fhs_file);

        // Create parent directory if it doesn't exist
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create parent directory {}", parent.display()))?;
        }

        if fs_file.is_dir() {
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

        if fs::symlink_metadata(&target_path).is_ok() {
            log::info!("Warning: File already exists, overwriting {} with {}", target_path.display(), fs_file.display());
            // Check if target path is a directory and handle accordingly
            if target_path.is_dir() {
                fs::remove_dir_all(&target_path)
                    .with_context(|| format!("Failed to remove directory {} for mirror_dir", target_path.display()))?;
            } else {
                fs::remove_file(&target_path)
                    .with_context(|| format!("Failed to remove file {} for mirror_dir", target_path.display()))?;
            }
        }

        let metadata = fs::symlink_metadata(fs_file)
            .with_context(|| format!("Failed to get metadata for {} for mirror_dir", fs_file.display()))?;
        if metadata.file_type().is_symlink() {
            shortcut_symlink(store_fs_dir, fs_file, &target_path)
                .with_context(|| format!("Failed to shortcut_symlink from {} to {}", fs_file.display(), target_path.display()))?;
        } else {
            if fhs_file.starts_with("etc/") {
                fs::copy(fs_file, &target_path)
                    .with_context(|| format!("Failed to copy {} to {}", fs_file.display(), target_path.display()))?;
            } else {
                symlink(fs_file, &target_path)
                    .with_context(|| format!("Failed to create symlink from {} to {}", fs_file.display(), target_path.display()))?;
            }
        }
    }
    Ok(())
}

// like symlink() but removes one level of indirection
fn shortcut_symlink(store_fs_dir: &Path, fs_file: &Path, target_path: &Path) -> Result<()> {
    if let Ok(link_target) = fs::read_link(fs_file) {
        // Handle different types of symlinks:
        // 1. Absolute paths: e.g. /usr/bin/python3 -> /usr/bin/python3.11
        //    Join with store_fs_dir to make it relative to the package root
        // 2. Parent-relative paths: e.g. ../bin/pidof -> /usr/bin/pidof
        //    Use normalize_join to resolve the ../ components against store_fs_dir
        // 3. Sibling-relative paths: e.g. python3 -> python3.11
        //    Join with the parent directory of the source file
        let new_link_target = if link_target.is_absolute() {
            // For absolute paths like /usr/bin/python3.11, make them relative to store_fs_dir
            // Note: Using Path.join() here would incorrectly handle absolute paths by discarding the base path
            PathBuf::from(format!("{}/{}", store_fs_dir.display(), link_target.display()))
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

fn create_ebin_wrappers(env_root: &Path, fs_files: &[PathBuf]) -> Result<()> {
    log::debug!("Creating ebin wrappers for {} files in {}", fs_files.len(), env_root.display());
    for fs_file in fs_files {
        let path_str = fs_file.to_string_lossy();

        if !path_str.contains("/bin/") && !path_str.contains("/sbin/") && !path_str.contains("/libexec/") {
            continue;
        }

        let lib_regex = regex::Regex::new(r"\.(so|so\.\d+)$").unwrap();
        if lib_regex.is_match(&path_str) {
            continue;
        }

        // Skip if not executable or is directory
        let metadata = fs::symlink_metadata(fs_file)
            .with_context(|| format!("Failed to get metadata for {} for create_ebin_wrappers", fs_file.display()))?;
        let mode = metadata.permissions().mode();
        if mode & 0o111 == 0 || metadata.is_dir() {
            continue;
        }

        create_ebin_wrapper(env_root, fs_file)
            .with_context(|| format!("Failed to create ebin wrapper for {}", fs_file.display()))?;
    }
    Ok(())
}

fn create_ebin_wrapper(env_root: &Path, fs_file: &Path) -> Result<()> {
    let (file_type, first_line) = get_file_type(fs_file)
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
        }
        FileType::ShellScript
        | FileType::PerlScript
        | FileType::PythonScript
        | FileType::RubyScript
        | FileType::NodeScript
        | FileType::LuaScript => {
            create_script_wrapper(env_root, fs_file, &ebin_path, file_type, &first_line)
                .with_context(|| format!("Failed to create script wrapper for {}", fs_file.display()))?;
        }
        _ => {}
    }
    Ok(())
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
                eprintln!("WARNING: script interpreter {} is not found in environment. Please install it later.", interpreter_basename);
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

impl PackageManager {

    // link files from env_root to store_fs_dir
    pub fn link_package(&self, store_fs_dir: &PathBuf, env_root: &PathBuf) -> Result<()> {
        let fs_files = list_package_files(store_fs_dir.to_str().ok_or_else(|| eyre::eyre!("Invalid store_fs_dir path: {}", store_fs_dir.display()))?)
            .with_context(|| format!("Failed to list package files in {}", store_fs_dir.display()))?;
        mirror_dir(env_root, store_fs_dir, &fs_files)
            .with_context(|| format!("Failed to mirror directory from {} to {}", store_fs_dir.display(), env_root.display()))?;
        Ok(())
    }

    // - run post-install scriptlets
    // - create ebin wrappers
    pub fn expose_package(&self, store_fs_dir: &PathBuf, env_root: &PathBuf) -> Result<()> {
        log::debug!("expose_package {}", store_fs_dir.display());
        let fs_files = list_package_files(store_fs_dir.to_str().ok_or_else(|| eyre::eyre!("Invalid store_fs_dir path"))?)?;
        create_ebin_wrappers(env_root, &fs_files)?;
        Ok(())
    }

    pub fn install_packages(&mut self, package_specs: Vec<String>) -> Result<()> {
        self.load_installed_packages()?;

        let channel_config = self.get_channel_config(config().common.env.clone())?;
        let repo_format = channel_config.format;
        let mut packages_to_install = self.resolve_package_info(package_specs.clone(), repo_format);
        let mut packages_to_install_clone = packages_to_install.clone();
        let mut depends_pkg : HashMap<String, InstalledPackageInfo> = HashMap::new();
        for (pkgline, pkginfo) in packages_to_install_clone.drain() {
            let mut tmp_pkg = HashMap::new();
            tmp_pkg.insert(pkgline, pkginfo);
            let mut tmp_depends : HashMap<String, InstalledPackageInfo> = HashMap::new();
            self.collect_depends(&mut tmp_pkg, &mut tmp_depends, 1,  repo_format)?;
            depends_pkg.extend(tmp_depends);
        }

        for pkg in depends_pkg.keys() {
            if let Some(info) = packages_to_install.get_mut(pkg) {
                info.depend_depth = 1;
            }
        }

        let current_installed: Vec<String> = packages_to_install
            .keys()
            .filter(|name| self.installed_packages.contains_key(*name))
            .cloned()
            .collect();
        if packages_to_install.len() == current_installed.len() &&
           packages_to_install.keys().all(|k| current_installed.contains(k)) {
           println!("All packages input have already installed");
           return Ok(());
        }
        if current_installed.len() > 0 {
            println!("These packages have already been installed: {}", current_installed.join(","));
        }

        self.record_appbin_source(&mut packages_to_install)?;
        self.collect_essential_packages(&mut packages_to_install)?;
        let current_env_name_ref = &config().common.env;
        let channel_config = self.channels_config.get(current_env_name_ref)
            .ok_or_else(|| eyre::eyre!(
                "Channel configuration not found for environment '{}'. Ensure environment is initialized and linked to a channel.",
                current_env_name_ref
            ))?;
        let package_format = channel_config.format;
        // First collect all dependencies
        let dependencies = self.collect_recursive_depends(&packages_to_install, repo_format)?;

        // Download all packages including dependencies
        let mut all_packages = packages_to_install.clone();
        all_packages.extend(dependencies);
        remove_duplicates(&self.installed_packages, &mut all_packages, "Warning: Some packages are already installed and will be skipped:");
        if all_packages.is_empty() {
            println!("No packages to install");
            return Ok(());
        }
        self.install_pkgkeys(all_packages)
    }

    pub fn install_pkgkeys(&mut self, mut packages_to_install: HashMap<String, InstalledPackageInfo>) -> Result<()> {
        println!("Packages to install:");
        print_packages_by_depend_depth(&packages_to_install);
        if config().common.dry_run {
            return Ok(());
        }

        // Submit download tasks for all packages
        let url_to_pkgkey = self.submit_download_tasks(&packages_to_install)?;
        let pending_urls: Vec<String> = url_to_pkgkey.keys().cloned().collect();

        self.change_appbin_flag_same_source(&mut packages_to_install)?;
        let new_generation = self.create_new_generation()?;

        let env_root = self.get_default_env_root()?.clone();
        let store_root = dirs().epkg_store.clone();

        // Process packages as downloads complete
        let completed_packages = self.process_downloads_and_install(
            &url_to_pkgkey,
            pending_urls,
            &mut packages_to_install,
            &store_root,
            &env_root,
        )?;

        // Try removing /run/systemd/system directory to prevent blocking on systemctl daemon-reload
        // Ignore any errors since this is just a workaround
        let _ = std::fs::remove_dir("/run/systemd/system");

        // Separate packages into fresh installs and upgrades
        let mut fresh_installs: HashMap<String, InstalledPackageInfo> = HashMap::new();
        let mut upgrades_new: HashMap<String, InstalledPackageInfo> = HashMap::new();
        let mut upgrades_old: HashMap<String, InstalledPackageInfo> = HashMap::new();

        for (pkgkey, package_info) in &completed_packages {
            // todo: pkgkey here is the new package's so you'll always get None for below
            // old_package_info. Instead should search self.installed_packages() for pkgname
            if let Some(old_package_info) = self.installed_packages.get(pkgkey) {
                // This is an upgrade
                upgrades_new.insert(pkgkey.clone(), package_info.clone());
                upgrades_old.insert(pkgkey.clone(), old_package_info.clone());
            } else {
                // This is a fresh install
                fresh_installs.insert(pkgkey.clone(), package_info.clone());
            }
        }

        // Get channel config for scriptlets
        let current_env_name_ref = &config().common.env;
        let channel_config = self.channels_config.get(current_env_name_ref)
            .ok_or_else(|| eyre::eyre!(
                "Channel configuration not found for environment '{}'. Ensure environment is initialized and linked to a channel.",
                current_env_name_ref
            ))?;
        let package_format = channel_config.format;

        // Handle upgrade flow
        if !upgrades_new.is_empty() {
            log::info!("Processing {} upgrades", upgrades_new.len());
            self.process_upgrades(&upgrades_old, &upgrades_new, &store_root, &env_root, package_format)?;
        }

        // Handle fresh install flow
        if !fresh_installs.is_empty() {
            log::info!("Processing {} fresh installations", fresh_installs.len());
            self.process_fresh_installs(&fresh_installs, &store_root, &env_root, package_format)?;
        }

        // Expose packages that need to be exposed (after all packages have been linked)
        let mut appbin_count = 0;
        let mut appbin_packages = Vec::new();
        for (pkgkey, package_info) in &completed_packages {
            if package_info.appbin_flag {
                appbin_count += 1;
                appbin_packages.push(pkgkey.clone());
                let store_fs_dir = store_root.join(package_info.pkgline.clone()).join("fs");
                self.expose_package(&store_fs_dir, &env_root.to_path_buf())
                    .with_context(|| format!("Failed to expose package {}", pkgkey))?;
            }
        }

        // Save installed packages
        self.installed_packages.extend(completed_packages.clone());
        self.save_installed_packages(&new_generation)?;
        self.record_history(&new_generation, "install", completed_packages.keys().cloned().collect(), vec![])?;

        // Last step: update current symlink to point to the new generation
        self.update_current_generation_symlink(new_generation)?;

        println!("Installation successful - Total packages: {}, AppBin packages: {}", completed_packages.len(), appbin_count);
        if !appbin_packages.is_empty() {
            println!("AppBin package list: {}", appbin_packages.join(", "));
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
            if let Some(completed_url) = wait_for_any_download_task(&pending_urls)? {
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
        let pkgline = crate::store::unpack_mv_package(&file_path)
            .with_context(|| format!("Failed to unpack package: {}", file_path))?;

        // Parse the pkgline which now includes architecture
        let parsed = package::parse_pkgline(&pkgline)
            .map_err(|e| eyre!("Failed to parse package line: {}", e))?;

        // Format the package key using the exact architecture from the package
        let actual_pkgkey = package::format_pkgkey(&parsed.pkgname, &parsed.version, &parsed.arch);

        // Update the package info with the pkgline
        let mut package_info = packages_to_install.remove(pkgkey)
            .or_else(|| packages_to_install.remove(&actual_pkgkey))
            .ok_or_else(|| eyre!("Package key not found: {} (or {})", pkgkey, actual_pkgkey))?;
        package_info.pkgline = pkgline;

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

        let old_files = list_package_files(old_store_fs_dir.to_str()
            .ok_or_else(|| eyre::eyre!("Invalid old package fs path"))?)?;
        let new_files = list_package_files(new_store_fs_dir.to_str()
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

        Ok(())
    }
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
