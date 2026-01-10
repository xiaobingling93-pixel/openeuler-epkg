//! Package exposure module
//!
//! This module handles "exposing" packages to the environment by creating ebin wrappers.

use std::path::{Path, PathBuf};
use std::fs;
use std::io::Write;
use std::os::unix::fs::symlink;
use std::sync::Arc;
use color_eyre::Result;
use color_eyre::eyre::{self, WrapErr};
use crate::models::SELF_ENV;
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
    let absolute_ebin_paths = create_ebin_wrappers(env_root, store_fs_dir, &fs_files)?;
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

/// Handle unexpose operations
pub fn execute_unexpose_operations(plan: &InstallationPlan, env_root: &Path) -> Result<()> {
    for op in &plan.ordered_operations {
        if !op.should_unexpose() {
            continue;
        }
        if let Some(pkgkey) = &op.old_pkgkey {
            if let Some(pkg_info) = crate::plan::pkgkey2installed_pkg_info(pkgkey) {
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
                    let info_mut = Arc::make_mut(installed_package_info_mut);
                    info_mut.ebin_links.clear();
                    info_mut.ebin_exposure = false;
                }
            }
        }
    }

    Ok(())
}

/// Handle expose operations
pub fn execute_expose_operations(plan: &InstallationPlan, store_root: &Path, env_root: &Path) -> Result<()> {
    for op in &plan.ordered_operations {
        if !op.should_expose() {
            continue;
        }
        if let Some(pkgkey) = &op.new_pkgkey {
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
                let info_mut = Arc::make_mut(installed_package_info_mut);
                info_mut.ebin_links = links.clone();
                info_mut.ebin_exposure = true;
            } else {
                log::warn!("execute_expose_operations: pkgkey '{}' not found in installed_packages. Ebin links not stored.", pkgkey);
            }
        }
    }

    Ok(())
}

/// Handle ELF binary with elf-loader wrapper (non-conda environments)
fn handle_elf(target_path: &Path, env_root: &Path, fs_file: &Path) -> Result<()> {
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

fn create_ebin_wrappers(env_root: &Path, store_fs_dir: &Path, fs_files: &[utils::MtreeFileInfo]) -> Result<Vec<PathBuf>> {
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

        // Construct absolute path by joining store_fs_dir with the relative path
        let fs_file_relative = Path::new(fs_file);
        let fs_file_absolute = store_fs_dir.join(fs_file_relative);
        if let Some(created_path) = create_ebin_wrapper(env_root, &fs_file_absolute, fs_file_relative)
            .with_context(|| format!("Failed to create ebin wrapper for {}", fs_file_absolute.display()))? {
            created_ebin_paths.push(created_path);
        }
    }
    log::debug!("create_ebin_wrappers: returning {} created paths: {:?}", created_ebin_paths.len(), created_ebin_paths);
    Ok(created_ebin_paths)
}

fn create_ebin_wrapper(env_root: &Path, fs_file_absolute: &Path, fs_file_relative: &Path) -> Result<Option<PathBuf>> {
    let (file_type, first_line) = utils::get_file_type(fs_file_absolute)
        .with_context(|| format!("Failed to determine file type for {}", fs_file_absolute.display()))?;
    let basename = fs_file_relative.file_name()
        .ok_or_else(|| eyre::eyre!("Failed to get filename for {}", fs_file_relative.display()))?;
    let ebin_path = env_root.join("usr/ebin").join(basename);

    log::debug!(
        "Creating ebin wrapper: ebin_path={}, fs_file_absolute={}, fs_file_relative={}, file_type={:?}, first_line={:?}",
        ebin_path.display(),
        fs_file_absolute.display(),
        fs_file_relative.display(),
        file_type,
        first_line
    );
    match file_type {
        FileType::Elf => {
            handle_elf(&ebin_path, env_root, fs_file_absolute)
                .with_context(|| format!("Failed to handle elf for {}", ebin_path.display()))?;
            return Ok(Some(ebin_path));
        }
        FileType::ShellScript
        | FileType::PerlScript
        | FileType::PythonScript
        | FileType::RubyScript
        | FileType::NodeScript
        | FileType::LuaScript => {
            create_script_wrapper(env_root, fs_file_absolute, &ebin_path, file_type, &first_line)
                .with_context(|| format!("Failed to create script wrapper for {}", fs_file_absolute.display()))?;
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

fn create_shebang_line(env_root: &Path, first_line: &str, script_path: &Path) -> Result<String> {
    let shebang_info = crate::shebang::parse_shebang_for_wrapper(first_line)?;

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

