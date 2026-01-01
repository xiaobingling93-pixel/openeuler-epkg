//! Package exposure module
//!
//! This module handles "exposing" packages to the environment by creating ebin wrappers.

use std::path::{Path, PathBuf};
use std::fs;
use std::io::Write;
use std::os::unix::fs::symlink;
use color_eyre::Result;
use color_eyre::eyre::{self, WrapErr};
use crate::models::{PackageFormat, SELF_ENV};
use crate::plan::InstallationPlan;
use crate::models::PACKAGE_CACHE;
use crate::utils;
use crate::utils::FileType;
use crate::dirs;
use crate::link::{hard_link_or_copy, replace_existing_symlink1, create_symlink2};
use log;

// Create ebin wrappers.
// Returns a list of relative paths to the created ebin wrappers (relative to env_root).
fn expose_package(store_fs_dir: &PathBuf, env_root: &PathBuf) -> Result<Vec<String>> {
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

/// Handle unexpose operations (del_exposes)
pub fn execute_unexpose_operations(plan: &InstallationPlan, env_root: &Path) -> Result<()> {
    for (pkgkey, pkg_info) in plan.del_exposes.iter() {
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
        if let Some(installed_package_info_mut) = PACKAGE_CACHE.installed_packages.write().unwrap().get_mut(pkgkey) {
            installed_package_info_mut.ebin_links.clear();
            installed_package_info_mut.ebin_exposure = false;
        }
    }

    Ok(())
}

/// Handle expose operations (new_exposes)
pub fn execute_expose_operations(plan: &InstallationPlan, store_root: &Path, env_root: &Path) -> Result<()> {
    for (pkgkey, _pkg_info) in plan.new_exposes.iter() {
        log::info!("Exposing package: {}", pkgkey);

        // Use the updated package info from installed_packages which has the correct pkgline
        let installed_pkg_info = PACKAGE_CACHE.installed_packages.read().unwrap().get(pkgkey)
            .ok_or_else(|| eyre::eyre!("Package {} not found in installed_packages for exposure", pkgkey))?
            .clone();

        // Check if pkgline is empty, which would indicate the package wasn't properly processed
        if installed_pkg_info.pkgline.is_empty() {
            return Err(eyre::eyre!("Package {} has empty pkgline, cannot expose. This indicates the package wasn't properly downloaded and processed.", pkgkey));
        }

        let store_fs_dir = store_root.join(installed_pkg_info.pkgline.clone()).join("fs");
        let links = expose_package(&store_fs_dir, &env_root.to_path_buf())
            .with_context(|| format!("Failed to expose package {}", pkgkey))?;

        // Update the package info with the new links
        if let Some(installed_package_info_mut) = PACKAGE_CACHE.installed_packages.write().unwrap().get_mut(pkgkey) {
            installed_package_info_mut.ebin_links = links.clone();
            installed_package_info_mut.ebin_exposure = true;
        } else {
            log::warn!("execute_expose_operations: pkgkey '{}' from new_exposes not found in installed_packages. Ebin links not stored.", pkgkey);
        }
    }

    Ok(())
}

fn handle_elf(target_path: &Path, env_root: &Path, fs_file: &Path) -> Result<()> {
    // Check if this is a conda environment - conda ELF binaries can run directly
    let is_conda = crate::models::channel_config().format == PackageFormat::Conda;

    if is_conda {
        handle_elf_conda(target_path, env_root, fs_file)
    } else {
        handle_elf_with_loader(target_path, env_root, fs_file)
    }
}

/// Handle ELF binary for conda environment - create hardlink directly from ELF binary to ebin
fn handle_elf_conda(target_path: &Path, env_root: &Path, fs_file: &Path) -> Result<()> {
    // For conda, create a hardlink directly from bin to ebin (no elf-loader wrapper needed)
    if target_path.exists() {
        fs::remove_file(target_path)
            .with_context(|| format!("Failed to remove existing file {}", target_path.display()))?;
    }

    // Create parent directory if it doesn't exist
    if let Some(parent) = target_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create parent directory {}", parent.display()))?;
    }

    // Create hardlink from fs_file (the actual ELF binary) to target_path (ebin)
    fs::hard_link(fs_file, target_path)
        .with_context(|| format!(
            "Failed to create hardlink from {} to {}",
            fs_file.display(),
            target_path.display()
        ))?;

    log::debug!(
        "handle_elf_conda target_path={}, env_root={}, fs_file={}",
        target_path.display(),
        env_root.display(),
        fs_file.display()
    );
    Ok(())
}

/// Handle ELF binary with elf-loader wrapper (non-conda environments)
fn handle_elf_with_loader(target_path: &Path, env_root: &Path, fs_file: &Path) -> Result<()> {
    let self_env_root = dirs::find_env_root(SELF_ENV)
        .ok_or_else(|| eyre::eyre!("Self environment not found"))?;

    let elf_loader_path = self_env_root.join("usr/bin/elf-loader");

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

    // Try hardlink first, fall back to copy if cross-device
    // Preserve permissions when copying (important for elf-loader)
    hard_link_or_copy(&elf_loader_path, target_path, true)
        .with_context(|| format!(
            "Failed to create hardlink or copy elf-loader from {} to {}",
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
        "handle_elf_with_loader target_path={}, env_root={}, fs_file={}",
        target_path.display(),
        env_root.display(),
        fs_file.display()
    );
    Ok(())
}

fn create_ebin_wrappers(env_root: &Path, fs_files: &[utils::MtreeFileInfo]) -> Result<Vec<PathBuf>> {
    let mut created_ebin_paths: Vec<PathBuf> = Vec::new();
    log::debug!("Creating ebin wrappers for {} files in {}", fs_files.len(), env_root.display());
    for fs_file_info in fs_files {
        let fs_file = &fs_file_info.path;
        let path_str = fs_file.as_str();

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

        let fs_file_path = Path::new(fs_file);
        if let Some(created_path) = create_ebin_wrapper(env_root, fs_file_path)
            .with_context(|| format!("Failed to create ebin wrapper for {}", fs_file_path.display()))? {
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
    let env_shell_bang_line = match create_shebang_line(env_root, first_line, fs_file) {
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
        .ok_or_else(|| eyre::eyre!("No suitable interpreter found for '{}'", interpreter_basename))?;

    // Create a symlink from the found interpreter to the expected location
    symlink(&target, interpreter_in_env)
        .with_context(|| format!("Failed to create symlink from {} to {}",
            target.display(), interpreter_in_env.display()))?;

    Ok(())
}

/// Create the wrapper for the interpreter in the ebin directory
fn create_interpreter_wrapper(env_root: &Path, interpreter_path: &str, interpreter_basename: &str, script_path: &Path) -> Result<String> {
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
                eprintln!("WARNING: script interpreter not found. Please install '{}' to make below script work:\n script_path: {}\n env_path: {}, error: {}",
                    interpreter_path, script_path.display(), interpreter_in_env.display(), e);
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

fn create_shebang_line(env_root: &Path, first_line: &str, script_path: &Path) -> Result<String> {
    let shebang_info = parse_shebang_for_wrapper(first_line)?;

    let env_interpreter_path = match create_interpreter_wrapper(env_root, &shebang_info.interpreter_path, &shebang_info.interpreter_basename, script_path)
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
