//! Package exposure module
//!
//! This module handles "exposing" packages to the environment by creating ebin wrappers.
//!
//! ## Ebin Wrapper Design
//!
//! Ebin wrappers are scripts in `$env_root/ebin/` that allow running tools from the host
//! without entering the environment. They work by:
//!
//! 1. **Starting from env path**: For each file to expose, we start with the path that
//!    will exist in the env (e.g., `$env_root/usr/bin/npm`)
//!
//! 2. **Following symlinks in env context**: Many distros use symlinks like:
//!    - `/usr/bin/npm` -> `../lib/node_modules/npm/bin/npm-cli.js`
//!    - `/usr/bin/node` -> `nodejs` (or similar)
//!
//!    We follow these symlinks INSIDE the env namespace to find the actual file location.
//!    This is crucial because:
//!    - The symlink target must be valid inside the env
//!    - The same symlink on host might point to a different location
//!
//! 3. **Using resolved path in wrapper**: The wrapper script uses the resolved path
//!    so that when run from the host, it correctly accesses the file in the env.
//!
//! ### Example: Alpine npm
//! ```
//! Env structure:
//!   $env_root/usr/bin/npm -> ../share/nodejs/npm/bin/npm-cli.js
//!   $env_root/usr/share/nodejs/npm/bin/npm-cli.js -> (real file)
//!
//! Generated ebin wrapper ($env_root/ebin/npm):
//!   #!/bin/sh
//!   exec node "$env_root/usr/share/nodejs/npm/bin/npm-cli.js" "$@"
//! ```
//!
//! ### Why not use store paths directly?
//! Store paths like `/home/wfg/.epkg/store/xxx__npm/fs/usr/bin/npm` don't work because:
//! - Symlinks in the store point to other store locations
//! - The relative structure may differ between distros
//! - Module resolution (e.g., `require('../lib/cli.js')`) expects specific paths

use std::path::{Path, PathBuf};
use std::fs;
use std::io::Write;
use crate::lfs;
use std::sync::Arc;
use color_eyre::Result;
use color_eyre::eyre::{self, WrapErr};
use crate::models::SELF_ENV;
use crate::plan::InstallationPlan;
use crate::models::PACKAGE_CACHE;
use crate::package_cache::map_pkgline2filelist;
use crate::utils;
use crate::utils::FileType;
#[cfg(target_os = "linux")]
use crate::xdesktop;
use crate::dirs;
use crate::link::{hard_link_or_copy, bin_file_exists, create_symlink2};
use log;

// Create ebin wrappers.
// Returns a list of relative paths to the created ebin wrappers (relative to env_root).
fn expose_package_ebin(store_fs_dir: &PathBuf, env_root: &PathBuf) -> Result<Vec<String>> {
    let fs_files = utils::list_package_files_with_info(store_fs_dir.to_str().ok_or_else(|| eyre::eyre!("Invalid store_fs_dir path"))?)?;
    let absolute_ebin_paths = create_ebin_wrappers(env_root, store_fs_dir, &fs_files)?;
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
    log::debug!("expose_package_ebin for store_fs_dir '{}': returning {} relative_ebin_links: {:?}", store_fs_dir.display(), relative_ebin_links.len(), relative_ebin_links);
    Ok(relative_ebin_links)
}

/// Handle unexpose operations for ebin wrappers of a single package
fn unexpose_package_ebin(env_root: &Path, pkgkey: &str) -> Result<()> {
    if let Some(pkg_info) = crate::plan::pkgkey2installed_pkg_info(pkgkey) {
        // Remove ebin wrappers for packages being unexposed
        if !pkg_info.ebin_links.is_empty() {
            log::debug!("Unexposing ebin wrappers for package: {}", pkgkey);

            for relative_ebin_path_str in &pkg_info.ebin_links {
                let ebin_path = env_root.join(relative_ebin_path_str);
                if lfs::symlink_metadata(&ebin_path).is_ok() {
                    log::debug!("Removing ebin wrapper: {}", ebin_path.display());
                    lfs::remove_file(&ebin_path)?;
                } else {
                    log::warn!("Ebin wrapper listed in metadata not found for removal: {}", ebin_path.display());
                }
            }
        }

        // Update the package info to clear ebin links
        if let Some(installed_package_info_mut) = PACKAGE_CACHE.installed_packages.write().unwrap().get_mut(pkgkey) {
            let info_mut = Arc::make_mut(installed_package_info_mut);
            info_mut.ebin_links.clear();
        }
    }

    Ok(())
}

/// Create symlink usr/bin/node_modules -> ../lib/node_modules for npm compatibility
/// Some distros (e.g., openEuler) install npm modules in /usr/lib/node_modules,
/// but npm expects to find them in /usr/bin/node_modules when resolving modules.
fn create_node_modules_symlink(env_root: &Path) -> Result<()> {
    let node_modules_in_lib = env_root.join("usr/lib/node_modules");
    let node_modules_in_bin = env_root.join("usr/bin/node_modules");

    // Only create symlink if source exists
    if !lfs::exists_in_env(&node_modules_in_lib) {
        return Ok(());
    }

    // Remove existing symlink/file if any
    if lfs::symlink_metadata(&node_modules_in_bin).is_ok() {
        lfs::remove_file(&node_modules_in_bin)?;
    }

    // Create symlink: usr/bin/node_modules -> ../lib/node_modules
    lfs::symlink("../lib/node_modules", &node_modules_in_bin)
        .with_context(|| format!("Failed to create node_modules symlink at {}", node_modules_in_bin.display()))?;

    log::debug!("Created symlink: {} -> ../lib/node_modules", node_modules_in_bin.display());
    Ok(())
}


/// Handle ELF binary with elf-loader wrapper (non-conda environments)
fn handle_elf(target_path: &Path, env_root: &Path, fs_file: &Path) -> Result<()> {
    log::info!("handle_elf: target_path={}, fs_file={}", target_path.display(), fs_file.display());

    let self_env_root = dirs::find_env_root(SELF_ENV)
        .ok_or_else(|| eyre::eyre!("Self environment not found"))?;

    let elf_loader_path = self_env_root.join("usr/bin/elf-loader");
    log::info!("  elf_loader_path={}", elf_loader_path.display());

    // Create hardlink from elf-loader to target path (replace copy&replace)
    if lfs::exists_in_env(target_path) {
        log::info!("  Target exists, removing...");
        if let Err(e) = lfs::remove_file(target_path) {
            log::error!("  Failed to remove file: {}", e);
            return Err(e);
        }
        log::info!("  Removed existing target");
    }

    // Create parent directory if it doesn't exist
    if let Some(parent) = target_path.parent() {
        lfs::create_dir_all(parent)?;
    }

    // Try hardlink first, fall back to copy if cross-device
    // Preserve permissions when copying (important for elf-loader)
    hard_link_or_copy(&elf_loader_path, target_path, true)
        .with_context(|| format!(
            "Failed to create hardlink or copy elf-loader from {} to {}",
            elf_loader_path.display(),
            target_path.display()
        ))?;

    // Only create symlink2 if bin/<program> file doesn't exist (any file type, any target).
    // We don't care whether bin/<program> is a symlink or what it points to;
    // if it exists, elf-loader will attempt to use it, and we avoid creating
    // a redundant hidden symlink.
    let has_bin_file = bin_file_exists(target_path, fs_file)
        .with_context(|| format!("Failed to check bin file for {}", target_path.display()))?;
    if !has_bin_file {
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

fn create_ebin_wrappers(env_root: &Path, store_fs_dir: &Path, fs_files: &[crate::mtree::MtreeFileInfo]) -> Result<Vec<PathBuf>> {
    let mut created_ebin_paths: Vec<PathBuf> = Vec::new();
    for fs_file_info in fs_files {
        let fs_file = &fs_file_info.path;
        let path_str = fs_file.as_str();

        // Check for bin/sbin/libexec directories (with or without leading /)
        let is_bin_path = path_str.starts_with("bin/") ||
                          path_str.contains("/bin/") ||
                          path_str.starts_with("sbin/") ||
                          path_str.contains("/sbin/");
        let is_libexec_path = path_str.starts_with("libexec/") || path_str.contains("/libexec/");

        if !is_bin_path && !is_libexec_path {
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
        // Symlinks may not have mode; skip mode check for them
        if !fs_file_info.is_link() {
            let mode = fs_file_info.mode.unwrap_or(0o644);
            if mode & 0o111 == 0 {
                continue;
            }
        }

        // Construct absolute path by joining store_fs_dir with the relative path
        let fs_file_relative = Path::new(fs_file);
        let fs_file_absolute = store_fs_dir.join(fs_file_relative);
        if let Some(created_path) = create_ebin_wrapper(env_root, &fs_file_absolute, fs_file_relative)
            .with_context(|| format!("Failed to create ebin wrapper for {}", fs_file_absolute.display()))? {
            created_ebin_paths.push(created_path);
        }
    }

    // Special-case: provide a `ruby` alias in ebin when the distro package
    // only ships an alternative binary name (e.g. `ruby-mri` on Fedora).
    //
    // If `ENV_ROOT/ebin/ruby-mri` exists, create/overwrite `ENV_ROOT/ebin/ruby`
    // as a small shell wrapper that delegates to `ruby-mri`. This avoids
    // confusing elf-loader by treating the launcher script in bin/ruby as an
    // ELF binary, and keeps the canonical interpreter behind `ruby-mri`.
    let ruby_ebin_path = env_root.join("ebin").join("ruby");
    let ruby_mri_ebin_path = env_root.join("ebin").join("ruby-mri");
    if lfs::exists_in_env(&ruby_mri_ebin_path) {
        // Remove any existing alias (from older buggy versions or previous runs)
        if lfs::symlink_metadata(&ruby_ebin_path).is_ok() {
            lfs::remove_file(&ruby_ebin_path)?;
        }

        // Create a simple shell wrapper that calls ruby-mri from ebin.
        let script_content = format!(
            "#!/bin/sh\nexec {:?} \"$@\"\n",
            ruby_mri_ebin_path
        );

        let ebin_dir = ruby_ebin_path.parent()
            .ok_or_else(|| eyre::eyre!("Failed to get parent directory for {}", ruby_ebin_path.display()))?;
        let temp_path = ebin_dir.join(".tmp-ruby-alias");

        let mut wrapper = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp_path)
            .with_context(|| format!("Failed to open temp file {} for ruby alias wrapper", temp_path.display()))?;

        wrapper.write_all(script_content.as_bytes())
            .with_context(|| format!("Failed to write ruby alias wrapper to {}", temp_path.display()))?;

        drop(wrapper);
        set_wrapper_permissions(&temp_path)?;
        fs::rename(&temp_path, &ruby_ebin_path)
            .with_context(|| format!("Failed to rename temp file {} to {}", temp_path.display(), ruby_ebin_path.display()))?;

        created_ebin_paths.push(ruby_ebin_path);
    }

    log::debug!("create_ebin_wrappers: returning {} created paths: {:?}", created_ebin_paths.len(), created_ebin_paths);
    Ok(created_ebin_paths)
}

/// Resolve the target path for an ebin wrapper by following symlinks in the env.
///
/// This function follows symlinks starting from `env_path` until:
/// 1. We hit a non-symlink file (regular file), OR
/// 2. We hit a symlink that stays within the env
///
/// The returned path satisfies:
/// - From host OS POV, it's a valid file path (not a broken symlink)
/// - The path itself is inside the env directory
/// - Its final realpath may be either in the env or in the epkg store
///
/// This is important because:
/// - Many interpreters use $0 (script path) as library search path
/// - We want to preserve the symlink chain that stays within the env
///
/// Example:
/// - Input:  $env_root/usr/bin/npm -> ../share/nodejs/npm/bin/npm-cli.js
/// - Output: $env_root/usr/share/nodejs/npm/bin/npm-cli.js
fn resolve_ebin_target_path(env_root: &Path, env_path: &Path) -> PathBuf {
    let mut current = env_path.to_path_buf();
    let mut visited = std::collections::HashSet::new();

    loop {
        // Prevent infinite loops
        if !visited.insert(current.clone()) {
            log::warn!("Symlink loop detected at: {}", current.display());
            break;
        }

        // Check if current path exists (from host POV)
        if !lfs::exists_in_env(&current) {
            log::debug!("Path does not exist: {}", current.display());
            break;
        }

        // Try to read symlink
        match fs::read_link(&current) {
            Ok(target) => {
                // It's a symlink - resolve the target
                if target.is_absolute() {
                    // Absolute symlink target
                    // If it points inside env_root, follow it
                    if target.starts_with(env_root) {
                        current = target.clone();
                    } else {
                        // Points outside env (e.g., /etc/alternatives/go -> /usr/lib/go/bin/go)
                        // Map it back to env context by prepending env_root
                        log::debug!("Absolute symlink outside env: {} -> {}", current.display(), target.display());
                        current = env_root.join(target.strip_prefix("/").unwrap_or(target.as_ref()));
                        break;
                    }
                } else {
                    // Relative symlink - resolve relative to current's parent
                    if let Some(parent) = current.parent() {
                        current = parent.join(&target);
                    } else {
                        break;
                    }
                }
            }
            Err(_) => {
                // Not a symlink (regular file/dir) - this is our final target
                break;
            }
        }
    }

    current
}

fn create_ebin_wrapper(env_root: &Path, fs_file_absolute: &Path, fs_file_relative: &Path) -> Result<Option<PathBuf>> {
    // First, determine the env path for this file
    // The env_path is where the file will be accessible inside the environment
    let env_path = env_root.join(fs_file_relative);

    // Resolve symlinks in the env context to get the actual file location
    // This is crucial for distros that use symlinks (e.g., alpine's npm -> npm-cli.js)
    // and for packages with cross-package symlinks (e.g., clang -> clang-18)
    let resolved_env_path = resolve_ebin_target_path(env_root, &env_path);

    // Determine file type from the RESOLVED env path, not the store file
    // This handles broken symlinks in store that become valid in env context
    // (e.g., clang package symlinks pointing to clang-18 package files)
    let (file_type, first_line) = if lfs::is_symlink(fs_file_absolute) {
        // For symlinks, check the resolved target in env context.
        // If the target is missing (e.g. helper like rust-clang points to
        // a non-installed clang), skip creating an ebin wrapper instead of
        // failing the whole exposure for the package.
        match utils::get_file_type(&resolved_env_path) {
            Ok(info) => info,
            Err(e) => {
                log::info!(
                    "Skipping ebin wrapper for {}: failed to determine file type for resolved path {}: {}",
                    fs_file_absolute.display(),
                    resolved_env_path.display(),
                    e
                );
                return Ok(None);
            }
        }
    } else {
        // For regular files, check the store file; if missing, skip wrapper.
        match utils::get_file_type(fs_file_absolute) {
            Ok(info) => info,
            Err(e) => {
                log::info!(
                    "Skipping ebin wrapper for {}: failed to determine file type: {}",
                    fs_file_absolute.display(),
                    e
                );
                return Ok(None);
            }
        }
    };
    let basename = fs_file_relative.file_name()
        .ok_or_else(|| eyre::eyre!("Failed to get filename for {}", fs_file_relative.display()))?;
    let ebin_path = env_root.join("ebin").join(basename);

    log::debug!(
        "Creating ebin wrapper: ebin_path={}, fs_file_absolute={}, fs_file_relative={}, resolved_env_path={}, file_type={:?}, first_line={:?}",
        ebin_path.display(),
        fs_file_absolute.display(),
        fs_file_relative.display(),
        resolved_env_path.display(),
        file_type,
        first_line
    );
    match file_type {
        FileType::Elf => {
            handle_elf(&ebin_path, env_root, &resolved_env_path)
                .with_context(|| format!("Failed to handle elf for {}", ebin_path.display()))?;
            return Ok(Some(ebin_path));
        }
        FileType::ShellScript
        | FileType::PerlScript
        | FileType::PythonScript
        | FileType::RubyScript
        | FileType::NodeScript
        | FileType::LuaScript => {
            create_script_wrapper(env_root, &resolved_env_path, &ebin_path, file_type, &first_line)
                .with_context(|| format!("Failed to create script wrapper for {}", resolved_env_path.display()))?;
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
    // For NodeScript and npm/npx scripts, use /bin/sh shebang
    let env_shell_bang_line = if file_type == FileType::NodeScript {
        // Use /bin/sh as the interpreter for Node.js wrappers
        // This allows the wrapper to work when called directly (e.g., from ebin/)
        format!("#!/bin/sh\n")
    } else if file_type == FileType::ShellScript && fs_file.file_name().map_or(false, |n| n == "npm" || n == "npx") {
        // npm and npx are shell scripts but we handle them specially to call node directly
        format!("#!/bin/sh\n")
    } else {
        match create_shebang_line(env_root, first_line, fs_file) {
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
                    log::info!("{}", error_msg);
                    return Ok(());
                }
            }
        }
    };

    let exec_cmd = get_exec_command(&file_type, fs_file, Some(env_root));

    // Create script wrapper atomically: write to temp file first, then rename.
    // This avoids corrupting hard links (e.g., elf-loader) and prevents partial
    // writes if the process is interrupted.
    let ebin_dir = ebin_path.parent()
        .ok_or_else(|| eyre::eyre!("Failed to get parent directory for {}", ebin_path.display()))?;
    let temp_path = ebin_dir.join(format!(".tmp-{}", ebin_path.file_name().unwrap().to_string_lossy()));

    // Write to temporary file
    let mut wrapper = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temp_path)
        .with_context(|| format!("Failed to open temp file {} for create_script_wrapper", temp_path.display()))?;

    if !env_shell_bang_line.is_empty() {
        wrapper.write_all(env_shell_bang_line.as_bytes())
            .with_context(|| format!("Failed to write shebang line to {}", temp_path.display()))?;
    }

    wrapper.write_all(exec_cmd.as_bytes())
        .with_context(|| format!("Failed to write exec command to {}", temp_path.display()))?;

    drop(wrapper); // Close file before rename

    // Set permissions on temp file before rename
    set_wrapper_permissions(&temp_path)?;

    // Atomic rename: this replaces any existing file at ebin_path
    fs::rename(&temp_path, ebin_path)
        .with_context(|| format!("Failed to rename temp file {} to {}", temp_path.display(), ebin_path.display()))?;

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
                return Ok(format!("{}\n", first_line));
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

    if !lfs::exists_in_env(env_interpreter) {
        // Example: interpreter_in_env = "/home/wfg/.epkg/envs/main/bin/sh"
        // Which is a symlink to: "/home/wfg/.epkg/store/twktsyye3ksj068w2fx9pz5fefwy70mw__bash__5.2.15__9.oe2403/fs/usr/bin/bash"
        // use format!() instead of Path::join() to enforce simple string operation
        let interpreter_in_env = format!("{}{}", env_root.display(), interpreter_path);
        let interpreter_in_env = Path::new(&interpreter_in_env);

        // Find and link the interpreter if needed
        match find_link_interpreter(interpreter_in_env, interpreter_basename, env_root) {
            Ok(()) => {}
            Err(e) => {
                log::info!(
                    "Script interpreter not found. Please install '{}' to make below script work:\n script_path: {}\n env_path: {}, error: {}",
                    interpreter_path,
                    script_path.display(),
                    interpreter_in_env.display(),
                    e
                );
                return Ok("".to_string());
            }
        }

        // Resolve to a path within the env first (e.g. env_root/usr/bin/yash), then canonicalize.
        // Using canonicalize(interpreter_in_env) would follow bin/sh -> /usr/bin/yash and fail with
        // ENOENT in containers where only env_root/usr/bin/yash exists.
        let path_to_canonicalize = lfs::resolve_symlink_in_env(interpreter_in_env, env_root)
            .unwrap_or_else(|| interpreter_in_env.to_path_buf());
        let store_interpreter = fs::canonicalize(&path_to_canonicalize)
            .with_context(|| format!("Failed to resolve interpreter path: {}", path_to_canonicalize.display()))?;

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
fn find_link_interpreter(interpreter_in_env: &Path, interpreter_basename: &str, env_root: &Path) -> Result<()> {
    // Use the environment‑aware resolver to determine if the path is valid
    if lfs::resolve_symlink_in_env(interpreter_in_env, env_root).is_some() {
        return Ok(());
    }

    // If the path exists as a symlink but resolve_symlink_in_env returned None,
    // the symlink is broken (target does not exist in the environment).
    if let Ok(metadata) = lfs::symlink_metadata(interpreter_in_env) {
        if metadata.file_type().is_symlink() {
            // Read the target for logging before removal
            let target = match fs::read_link(interpreter_in_env) {
                Ok(t) => t.to_string_lossy().into_owned(),
                Err(_) => "???".to_string(),
            };
            log::warn!(
                "Removing broken symlink: {} -> {} (target not found in environment)",
                interpreter_in_env.display(),
                target
            );
            lfs::remove_file(interpreter_in_env)?;
            // After removing broken symlink, continue to search for alternatives
        } else {
            // Not a symlink (regular file or directory) - leave it alone
            return Ok(());
        }
    }

    find_and_link_alternative_interpreter(interpreter_in_env, interpreter_basename)?;

    Ok(())
}

/// Find and link an alternative interpreter when the expected one is missing
fn find_and_link_alternative_interpreter(interpreter_in_env: &Path, interpreter_basename: &str) -> Result<()> {
    // Get the parent directory to search in
    let parent = interpreter_in_env.parent()
        .ok_or_else(|| eyre::eyre!("Failed to get parent directory of {}", interpreter_in_env.display()))?;

    // Find candidate interpreters based on the type
    let targets = match interpreter_basename {
        // For shell scripts, look for bash, dash, or yash as alternatives (e.g. Alpine uses yash for sh)
        "sh" => glob::glob(&format!("{}/{{bash,dash,yash,busybox}}", parent.display()))
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
    lfs::symlink(&target, interpreter_in_env)?;

    Ok(())
}

fn get_exec_command(file_type: &FileType, resolved_path: &Path, _env_root: Option<&Path>) -> String {
    // The resolved_path is already the final path in the env (symlinks resolved)
    // No need for store-to-env conversion

    // Special handling for npm and npx shell scripts - call the -cli.js directly
    // This avoids issues with the npm shell script's dynamic node detection
    // Only apply this for shell scripts, not Node.js scripts (e.g., alpine uses Node.js scripts)
    if *file_type == FileType::ShellScript {
        let path_str = resolved_path.to_string_lossy();
        if path_str.ends_with("/bin/npm") {
            let cli_path = path_str.trim_end_matches("/bin/npm").to_string() + "/bin/npm-cli.js";
            return format!("exec node {:?} \"$@\"\n", cli_path);
        }
        if path_str.ends_with("/bin/npx") {
            let cli_path = path_str.trim_end_matches("/bin/npx").to_string() + "/bin/npx-cli.js";
            return format!("exec node {:?} \"$@\"\n", cli_path);
        }
    }

    match file_type {
        FileType::ShellScript => format!("exec {:?} \"$@\"\n", resolved_path),
        FileType::PythonScript => format!("exec(open({:?}).read())\n", resolved_path),
        FileType::RubyScript => format!("load({:?})\n", resolved_path),
        FileType::LuaScript => format!("dofile({:?})\n", resolved_path),
        FileType::NodeScript => {
            // For Node.js scripts, use a shell wrapper that calls node explicitly
            // This ensures proper module resolution from the script's directory
            format!("exec node {:?} \"$@\"\n", resolved_path)
        }
        _ => format!("exec {:?} \"$@\"\n", resolved_path),
    }
}

fn set_wrapper_permissions(ebin_path: &Path) -> Result<()> {
    utils::set_executable_permissions(ebin_path, 0o755)
}

/// Handle unexpose operations for a single package
#[allow(unused)]
pub fn unexpose_package(plan: &mut InstallationPlan, env_root: &Path, pkgkey: &str) -> Result<()> {
    // First unexpose ebin wrappers
    unexpose_package_ebin(env_root, pkgkey)?;

    if let Some(installed_package_info_mut) = PACKAGE_CACHE.installed_packages.write().unwrap().get_mut(pkgkey) {
        let info_mut = Arc::make_mut(installed_package_info_mut);
        // Remove desktop integration links based on stored xdesktop_links info
        #[cfg(target_os = "linux")]
        {
            xdesktop::unexpose_package_xdesktop(&info_mut.xdesktop_links, env_root, &mut plan.desktop_integration_occurred)?;
            // Update the package info to clear xdesktop links
            info_mut.xdesktop_links.clear();
        }
        info_mut.ebin_exposure = false;
    }

    Ok(())
}

/// Handle expose operations for a single package
pub fn expose_package(plan: &mut InstallationPlan, store_fs_dir: &Path, pkgkey: &str) -> Result<()> {
    log::debug!("Exposing package: {} (store_fs_dir: {})", pkgkey, store_fs_dir.display());

    // Check if pkgkey is in new_pkgs
    if let Some(info) = crate::plan::pkgkey2new_pkg_info(plan, pkgkey) {
        log::debug!("  Found in new_pkgs: ebin_exposure={}", info.ebin_exposure);
    } else {
        log::debug!("  NOT found in new_pkgs!");
    }

    // Use the updated package info from installed_packages which has the correct pkgline
    let installed_pkg_info = PACKAGE_CACHE.installed_packages.read().unwrap().get(pkgkey)
        .ok_or_else(|| eyre::eyre!("Package {} not found in installed_packages for exposure", pkgkey))?
        .clone();

    // Check if pkgline is empty, which would indicate the package wasn't properly processed
    if installed_pkg_info.pkgline.is_empty() {
        return Err(eyre::eyre!("Package {} has empty pkgline, cannot expose. This indicates the package wasn't properly downloaded and processed.", pkgkey));
    }

    // Get filelist for desktop integration
    let store_root = store_fs_dir.parent().unwrap().parent().unwrap(); // /opt/epkg/store from /opt/epkg/store/$pkgline/fs
    #[allow(unused)]
    let filelist = map_pkgline2filelist(store_root, &installed_pkg_info.pkgline)?;

    // Expose ebin wrappers
    let ebin_links = expose_package_ebin(&store_fs_dir.to_path_buf(), &plan.env_root)
        .with_context(|| format!("Failed to expose package {}", pkgkey))?;

    // Create usr/bin/node_modules -> ../lib/node_modules symlink for npm/nodejs packages
    // This is needed for distros (e.g., openEuler) where npm modules are in /usr/lib/node_modules
    // but npm expects to find them in /usr/bin/node_modules when resolving modules
    if let Ok(parsed) = crate::package::parse_pkgline(&installed_pkg_info.pkgline) {
        if parsed.pkgname == "npm" || parsed.pkgname == "nodejs" {
            create_node_modules_symlink(&plan.env_root)
                .with_context(|| format!("Failed to create node_modules symlink for package {}", pkgkey))?;
        }
    }

    // Desktop integration
    #[cfg(target_os = "linux")]
    let xdesktop_links = xdesktop::expose_package_xdesktop(&plan.env_root, &filelist, &mut plan.desktop_integration_occurred)?;

    // Update the package info with the new links
    if let Some(installed_package_info_mut) = PACKAGE_CACHE.installed_packages.write().unwrap().get_mut(pkgkey) {
        let info_mut = Arc::make_mut(installed_package_info_mut);
        info_mut.ebin_links = ebin_links;
        #[cfg(target_os = "linux")]
        {
            info_mut.xdesktop_links = xdesktop_links;
        }
        info_mut.ebin_exposure = true;
    } else {
        log::warn!("expose_package_operations: pkgkey '{}' not found in installed_packages. Links not stored.", pkgkey);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test resolve_ebin_target_path with real-world distro layouts
    ///
    /// Test cases cover:
    /// 1. Alpine: npm -> ../share/nodejs/npm/bin/npm-cli.js (relative symlink to file)
    /// 2. OpenEuler: npm -> ../share/nodejs/npm/bin/npm (relative symlink to shell script)
    /// 3. Debian: npm -> ../share/nodejs/npm/bin/npm-cli.js, nodejs -> node
    mod resolve_ebin_target_path_tests {
        use super::*;

        fn get_test_osroot() -> PathBuf {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/osroot")
        }

        #[test]
        fn test_alpine_npm_symlink() {
            // Alpine: /usr/bin/npm -> ../share/nodejs/npm/bin/npm-cli.js
            let osroot = get_test_osroot().join("alpine");
            let env_path = osroot.join("usr/bin/npm");

            let resolved = resolve_ebin_target_path(&osroot, &env_path);

            // Should resolve to: osroot/usr/share/nodejs/npm/bin/npm-cli.js
            let expected = osroot.join("usr/share/nodejs/npm/bin/npm-cli.js");
            assert_eq!(resolved.canonicalize().unwrap(), expected.canonicalize().unwrap(), "Alpine npm symlink should resolve to npm-cli.js");
        }

        #[test]
        fn test_alpine_npx_symlink() {
            // Alpine: /usr/bin/npx -> ../share/nodejs/npm/bin/npx-cli.js
            let osroot = get_test_osroot().join("alpine");
            let env_path = osroot.join("usr/bin/npx");

            let resolved = resolve_ebin_target_path(&osroot, &env_path);

            let expected = osroot.join("usr/share/nodejs/npm/bin/npx-cli.js");
            assert_eq!(resolved.canonicalize().unwrap(), expected.canonicalize().unwrap(), "Alpine npx symlink should resolve to npx-cli.js");
        }

        #[test]
        fn test_openeuler_npm_symlink() {
            // OpenEuler: /usr/bin/npm -> ../share/nodejs/npm/bin/npm (symlink to shell script)
            let osroot = get_test_osroot().join("openeuler");
            let env_path = osroot.join("usr/bin/npm");

            let resolved = resolve_ebin_target_path(&osroot, &env_path);

            // Should resolve to: osroot/usr/share/nodejs/npm/bin/npm (the shell script)
            let expected = osroot.join("usr/share/nodejs/npm/bin/npm");
            assert_eq!(resolved.canonicalize().unwrap(), expected.canonicalize().unwrap(), "OpenEuler npm symlink should resolve to npm shell script");
        }

        #[test]
        fn test_debian_npm_symlink() {
            // Debian: /usr/bin/npm -> ../share/nodejs/npm/bin/npm-cli.js
            let osroot = get_test_osroot().join("debian");
            let env_path = osroot.join("usr/bin/npm");

            let resolved = resolve_ebin_target_path(&osroot, &env_path);

            let expected = osroot.join("usr/share/nodejs/npm/bin/npm-cli.js");
            assert_eq!(resolved.canonicalize().unwrap(), expected.canonicalize().unwrap(), "Debian npm symlink should resolve to npm-cli.js");
        }

        #[test]
        fn test_debian_nodejs_symlink() {
            // Debian: /usr/bin/nodejs -> node (symlink to real file)
            let osroot = get_test_osroot().join("debian");
            let env_path = osroot.join("usr/bin/nodejs");

            let resolved = resolve_ebin_target_path(&osroot, &env_path);

            let expected = osroot.join("usr/bin/node");
            assert_eq!(resolved.canonicalize().unwrap(), expected.canonicalize().unwrap(), "Debian nodejs symlink should resolve to node");
        }

        #[test]
        fn test_regular_file_no_symlink() {
            // Test with a regular file (no symlink) - should return the same path
            let osroot = get_test_osroot().join("debian");
            let env_path = osroot.join("usr/bin/node");

            let resolved = resolve_ebin_target_path(&osroot, &env_path);

            assert_eq!(resolved, env_path, "Regular file should return same path");
        }
    }
}
