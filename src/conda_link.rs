//! Conda-specific linking functionality
//!
//! This module provides conda-specific features for package linking:
//! - Prefix placeholder replacement
//! - Python noarch path remapping
//! - Python entry points
//! Refer to:
//! - https://docs.conda.io/projects/conda-build/en/stable/resources/package-spec.html#info-paths-json
//! Reference/Borrows some code from:
//! - /c/package-managers/conda/core/link.py
//! - /c/package-managers/rattler/crates/rattler/src/install/

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::borrow::Cow;
use color_eyre::Result;
use color_eyre::eyre::{self, WrapErr};
use serde_json::Value;
use crate::io::read_json_file;
use memchr::memmem;
use crate::shebang::{is_valid_shebang_length, convert_shebang_to_env};
use crate::plan::InstallationPlan;
use crate::models::LinkType;
use crate::link::{link_package_generic, mirror_file};
use crate::utils;
use crate::lfs;
use log;

/// Default Python version (major, minor) used when version cannot be determined
const DEFAULT_PYTHON_VERSION: (u64, u64) = (3, 13);

/// Default Python version string for log messages
const DEFAULT_PYTHON_VERSION_STR: &str = "3.13";

/// Python version information for noarch packages
#[derive(Debug, Clone)]
pub struct PythonInfo {
    /// Major and minor version (e.g., (3, 13))
    #[allow(dead_code)]
    pub short_version: (u64, u64),
    /// Relative path to python executable (e.g., "bin/python3.13")
    pub path: PathBuf,
    /// Relative path to site-packages (e.g., "lib/python3.13/site-packages")
    pub site_packages_path: PathBuf,
    /// Binary directory (e.g., "bin" on Unix, "Scripts" on Windows)
    pub bin_dir: PathBuf,
}

/// Entry point definition from link.json
#[derive(Debug, Clone)]
pub struct EntryPoint {
    /// Command name (e.g., "pip", "jupyter")
    pub command: String,
    /// Module name (e.g., "pip._internal.cli.main")
    pub module: String,
    /// Function name (e.g., "main")
    pub function: String,
}

/// Conda package metadata from index.json
#[derive(Debug, Clone)]
pub struct IndexJson {
    /// Package name
    #[allow(dead_code)]
    pub name: String,
    /// Noarch type (None, "python", "generic")
    pub noarch: Option<String>,
    /// Python site packages path (if specified)
    pub python_site_packages_path: Option<String>,
}

/// Path entry from paths.json
#[derive(Debug, Clone)]
pub struct PathsEntry {
    /// Relative path in package
    pub relative_path: PathBuf,
    /// Path type (file, directory, hardlink, softlink)
    pub path_type: String,
    /// SHA256 hash
    #[allow(dead_code)]
    pub sha256: Option<String>,
    /// File size in bytes
    #[allow(dead_code)]
    pub size_in_bytes: Option<u64>,
    /// Prefix placeholder information
    pub prefix_placeholder: Option<PrefixPlaceholder>,
    /// Whether this file should not be linked
    pub no_link: bool,
}

/// Prefix placeholder information
#[derive(Debug, Clone)]
pub struct PrefixPlaceholder {
    /// The placeholder string to replace (e.g., "/opt/anaconda3")
    pub placeholder: String,
    /// File mode (text or binary)
    pub file_mode: FileMode,
}

/// File mode for prefix replacement
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileMode {
    /// Text file (simple find-and-replace)
    Text,
    /// Binary file (C-string replacement with padding)
    Binary,
}

/// Read index.json from conda package
fn read_index_json(package_dir: &Path) -> Result<IndexJson> {
    let index_path = package_dir.join("info/conda/index.json");
    let index_data: Value = crate::io::read_json_file(&index_path)
        .wrap_err_with(|| format!("Failed to read index.json from {}", package_dir.display()))?;

    let name = index_data.get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| eyre::eyre!("Missing 'name' in index.json"))?
        .to_string();

    let noarch = index_data.get("noarch")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let python_site_packages_path = index_data.get("python_site_packages_path")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Ok(IndexJson {
        name,
        noarch,
        python_site_packages_path,
    })
}

/// Parse a single path entry from paths.json
fn parse_paths_entry(path_entry: &Value) -> Result<PathsEntry> {
    let relative_path = path_entry.get("_path")
        .or_else(|| path_entry.get("path"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| eyre::eyre!("Missing path in paths.json entry"))?;

    let path_type = path_entry.get("path_type")
        .and_then(|v| v.as_str())
        .unwrap_or("file")
        .to_string();

    let sha256 = path_entry.get("sha256")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let size_in_bytes = path_entry.get("size_in_bytes")
        .and_then(|v| v.as_u64());

    let prefix_placeholder = path_entry.get("prefix_placeholder")
        .and_then(|v| {
            let placeholder = v.get("placeholder")?.as_str()?.to_string();
            let file_mode_str = v.get("file_mode").and_then(|m| m.as_str()).unwrap_or("text");
            let file_mode = if file_mode_str == "binary" {
                FileMode::Binary
            } else {
                FileMode::Text
            };
            Some(PrefixPlaceholder {
                placeholder,
                file_mode,
            })
        });

    let no_link = path_entry.get("no_link")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    Ok(PathsEntry {
        relative_path: PathBuf::from(relative_path),
        path_type,
        sha256,
        size_in_bytes,
        prefix_placeholder,
        no_link,
    })
}

/// Read paths.json from conda package
/// Returns empty vector if paths.json does not exist (fallback to generic linking)
fn read_paths_json(package_dir: &Path) -> Result<Vec<PathsEntry>> {
    let paths_path = package_dir.join("info/conda/paths.json");

    // Check if file exists first
    if !paths_path.exists() {
        log::info!("paths.json not found: {}", paths_path.display());
        return Ok(Vec::new());
    }

    // File exists, read and parse JSON
    let paths_data: Value = read_json_file(&paths_path)
        .wrap_err_with(|| format!("Failed to read paths.json from {}", package_dir.display()))?;

    let mut entries = Vec::new();

    if let Some(paths_array) = paths_data.get("paths").and_then(|v| v.as_array()) {
        for path_entry in paths_array {
            entries.push(parse_paths_entry(path_entry)?);
        }
    }

    Ok(entries)
}

/// Parse a single entry point from link.json
fn parse_entry_point(ep: &Value) -> Result<EntryPoint> {
    let ep_obj = ep.as_object()
        .ok_or_else(|| eyre::eyre!("Entry point is not an object"))?;

    let command = ep_obj.get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| eyre::eyre!("Missing 'command' in entry point"))?
        .to_string();

    let func_str = ep_obj.get("func")
        .or_else(|| ep_obj.get("function"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| eyre::eyre!("Missing 'func' or 'function' in entry point"))?;

    // Parse "module:function" format
    let (module, function) = if let Some((m, f)) = func_str.split_once(':') {
        (m.to_string(), f.to_string())
    } else {
        // Handle "module.function" format
        let parts: Vec<&str> = func_str.split('.').collect();
        if parts.len() >= 2 {
            let mod_parts = &parts[..parts.len() - 1];
            let func = parts.last().unwrap();
            (mod_parts.join("."), func.to_string())
        } else {
            return Err(eyre::eyre!("Invalid function format: {}", func_str));
        }
    };

    Ok(EntryPoint {
        command,
        module,
        function,
    })
}

/// Read link.json from conda package (for Python entry points)
fn read_link_json(package_dir: &Path) -> Result<Option<Vec<EntryPoint>>> {
    let link_path = package_dir.join("info/conda/link.json");

    if !link_path.exists() {
        return Ok(None);
    }

    let link_data: Value = crate::io::read_json_file(&link_path)
        .wrap_err_with(|| format!("Failed to read link.json from {}", package_dir.display()))?;

    // Check if this is a noarch: python package
    let noarch = link_data.get("noarch")
        .and_then(|v| v.as_str());

    if noarch != Some("python") {
        return Ok(None);
    }

    let entry_points = link_data.get("entry_points")
        .and_then(|v| v.as_array())
        .ok_or_else(|| eyre::eyre!("Missing entry_points in link.json"))?;

    let mut result = Vec::new();

    for ep in entry_points {
        result.push(parse_entry_point(ep)?);
    }

    Ok(Some(result))
}

/// Get Python version from installed Python package
fn get_python_version_from_installed() -> Option<(u64, u64)> {
    use crate::models::PACKAGE_CACHE;
    use crate::package;

    let pkgkey_opt = {
        let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
        installed.iter()
            .find(|(pkgkey, _pkg)| {
                if let Ok(pkgname) = package::pkgkey2pkgname(pkgkey) {
                    pkgname == "python"
                } else {
                    false
                }
            })
            .map(|(pkgkey, _)| pkgkey.clone())
    };

    if let Some(pkgkey) = pkgkey_opt {
        if let Ok(version_str) = package::pkgkey2version(&pkgkey) {
            // Version might be like "3.13.0" or "3.13.0-1"
            let version_clean = version_str.split('-').next().unwrap_or(&version_str);
            return extract_python_version_from_string(version_clean);
        }
    }

    None
}

/// Get Python version from index.json site-packages path
fn get_python_version_from_index(index_json: &IndexJson) -> Option<(u64, u64)> {
    if let Some(sp_path) = &index_json.python_site_packages_path {
        return extract_python_version_from_path(sp_path);
    }
    None
}

/// Get Python info from installed Python package or environment
fn get_python_info(index_json: &IndexJson) -> Result<Option<PythonInfo>> {
    let (major, minor) = get_python_version_from_installed()
        .or_else(|| get_python_version_from_index(index_json))
        .unwrap_or_else(|| {
            log::warn!("Could not determine Python version, defaulting to {}", DEFAULT_PYTHON_VERSION_STR);
            DEFAULT_PYTHON_VERSION
        });

    let path = PathBuf::from(format!("bin/python{major}.{minor}"));
    let site_packages_path = index_json.python_site_packages_path
        .as_ref()
        .map(|s| PathBuf::from(s))
        .unwrap_or_else(|| PathBuf::from(format!("lib/python{major}.{minor}/site-packages")));
    let bin_dir = PathBuf::from("bin");

    Ok(Some(PythonInfo {
        short_version: (major, minor),
        path,
        site_packages_path,
        bin_dir,
    }))
}

/// Extract Python version from version string (e.g., "3.13.0" -> (3, 13))
fn extract_python_version_from_string(version: &str) -> Option<(u64, u64)> {
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() >= 2 {
        let major = parts[0].parse().ok()?;
        let minor = parts[1].parse().ok()?;
        return Some((major, minor));
    }
    None
}

/// Extract Python version from site-packages path
fn extract_python_version_from_path(path: &str) -> Option<(u64, u64)> {
    // Match patterns like "lib/python3.13/site-packages" or "Lib/site-packages" (Windows)
    let re = regex::Regex::new(r"python(\d+)\.(\d+)").ok()?;
    if let Some(captures) = re.captures(path) {
        let major = captures.get(1)?.as_str().parse().ok()?;
        let minor = captures.get(2)?.as_str().parse().ok()?;
        return Some((major, minor));
    }
    None
}

/// Compute final paths for noarch Python packages
fn compute_paths(
    index_json: &IndexJson,
    paths_entries: &[PathsEntry],
    python_info: Option<&PythonInfo>,
) -> Vec<(PathsEntry, PathBuf)> {
    let mut final_paths = Vec::new();

    let is_noarch_python = index_json.noarch.as_ref()
        .map(|n| n == "python")
        .unwrap_or(false);

    for entry in paths_entries {
        let path = if is_noarch_python {
            if let Some(py_info) = python_info {
                remap_noarch_path(&entry.relative_path, py_info)
            } else {
                entry.relative_path.clone()
            }
        } else {
            entry.relative_path.clone()
        };

        final_paths.push((entry.clone(), path));
    }

    final_paths
}

/// Remap noarch Python package paths
fn remap_noarch_path(relative_path: &Path, python_info: &PythonInfo) -> PathBuf {
    // Remap site-packages/ -> lib/python{major}.{minor}/site-packages/
    if let Ok(rest) = relative_path.strip_prefix("site-packages/") {
        return python_info.site_packages_path.join(rest);
    }

    // Remap python-scripts/ -> bin/ (Unix) or Scripts/ (Windows)
    if let Ok(rest) = relative_path.strip_prefix("python-scripts/") {
        return python_info.bin_dir.join(rest);
    }

    // No remapping needed
    relative_path.to_path_buf()
}

/// Replace prefix in shebang, handling long shebangs and spaces
fn replace_shebang<'a>(
    shebang: Cow<'a, str>,
    old_prefix: &str,
    new_prefix: &str,
) -> Cow<'a, str> {
    assert!(
        shebang.starts_with("#!"),
        "Shebang does not start with #! ({})",
        shebang
    );

    // If the new prefix contains a space, convert to /usr/bin/env format
    if new_prefix.contains(' ') {
        if !shebang.contains(old_prefix) {
            return shebang;
        }
        // Convert the shebang without spaces to a new shebang, and only then replace
        // which is relevant for the Python case
        let new_shebang = convert_shebang_to_env(shebang).replace(old_prefix, new_prefix);
        return Cow::Owned(new_shebang);
    }

    let shebang: Cow<'_, str> = shebang.replace(old_prefix, new_prefix).into();

    if !shebang.starts_with("#!") {
        log::warn!("Shebang does not start with #! ({})", shebang);
        return shebang;
    }

    if is_valid_shebang_length(&shebang) {
        shebang
    } else {
        convert_shebang_to_env(shebang)
    }
}

/// Copy file content and replace prefix placeholder in text files
/// Handles shebang lines specially (may need conversion to /usr/bin/env)
fn copy_replace_textual_placeholder(
    source_path: &Path,
    target_path: &Path,
    prefix_placeholder: &str,
    target_prefix: &str,
) -> Result<()> {
    let source_bytes = fs::read(source_path)
        .wrap_err_with(|| format!("Failed to read source file: {}", source_path.display()))?;

    let mut target_file = lfs::file_create(target_path)?;

    let old_prefix = prefix_placeholder.as_bytes();
    let new_prefix = target_prefix.as_bytes();
    let mut source_bytes = source_bytes.as_slice();

    // Check if we have a shebang. We need to handle it differently because it has a maximum length
    // that can be exceeded in very long target prefix's.
    #[cfg(unix)]
    {
        if source_bytes.starts_with(b"#!") {
            // Extract first line
            let newline_pos = source_bytes.iter().position(|&c| c == b'\n').unwrap_or(source_bytes.len());
            let (first, rest) = source_bytes.split_at(newline_pos);
            let first_line = String::from_utf8_lossy(first);
            let new_shebang = replace_shebang(
                first_line,
                prefix_placeholder,
                target_prefix,
            );
            target_file.write_all(new_shebang.as_bytes())
                .wrap_err_with(|| format!("Failed to write shebang to {}", target_path.display()))?;
            source_bytes = rest;
        }
    }

    // Replace prefix in remaining content
    let mut last_match = 0;

    for index in memmem::find_iter(source_bytes, old_prefix) {
        target_file.write_all(&source_bytes[last_match..index])
            .wrap_err_with(|| format!("Failed to write to {}", target_path.display()))?;
        target_file.write_all(new_prefix)
            .wrap_err_with(|| format!("Failed to write to {}", target_path.display()))?;
        last_match = index + old_prefix.len();
    }

    // Write remaining bytes
    if last_match < source_bytes.len() {
        target_file.write_all(&source_bytes[last_match..])
            .wrap_err_with(|| format!("Failed to write to {}", target_path.display()))?;
    }

    Ok(())
}

/// Copy file content and replace prefix placeholder in binary files (C-strings)
/// Maintains original file length with null padding
fn copy_replace_cstring_placeholder(
    source_path: &Path,
    target_path: &Path,
    prefix_placeholder: &str,
    target_prefix: &str,
) -> Result<()> {
    let source_bytes = fs::read(source_path)
        .wrap_err_with(|| format!("Failed to read binary file: {}", source_path.display()))?;

    let mut target_file = lfs::file_create(target_path)?;

    let old_prefix = prefix_placeholder.as_bytes();
    let new_prefix = target_prefix.as_bytes();
    let mut source_bytes = source_bytes.as_slice();

    let finder = memmem::Finder::new(old_prefix);

    loop {
        if let Some(index) = finder.find(source_bytes) {
            // Write all bytes up to the old prefix
            target_file.write_all(&source_bytes[..index])
                .wrap_err_with(|| format!("Failed to write to {}", target_path.display()))?;

            // Find the end of the C-style string (the nul terminator)
            let mut end = index + old_prefix.len();
            while end < source_bytes.len() && source_bytes[end] != b'\0' {
                end += 1;
            }

            // Extract the C-string that contains the placeholder
            let mut out = Vec::new();
            let mut old_bytes = &source_bytes[index..end];
            let old_len = old_bytes.len();

            // Replace all occurrences of the old prefix with the new prefix within this C-string
            while let Some(sub_index) = finder.find(old_bytes) {
                out.write_all(&old_bytes[..sub_index])
                    .wrap_err_with(|| format!("Failed to write to {}", target_path.display()))?;
                out.write_all(new_prefix)
                    .wrap_err_with(|| format!("Failed to write to {}", target_path.display()))?;
                old_bytes = &old_bytes[sub_index + old_prefix.len()..];
            }
            out.write_all(old_bytes)
                .wrap_err_with(|| format!("Failed to write to {}", target_path.display()))?;

            // Write the replaced string, truncating if necessary to maintain original length
            if out.len() > old_len {
                target_file.write_all(&out[..old_len])
                    .wrap_err_with(|| format!("Failed to write to {}", target_path.display()))?;
            } else {
                target_file.write_all(&out)
                    .wrap_err_with(|| format!("Failed to write to {}", target_path.display()))?;
            }

            // Compute the padding required when replacing the old prefix(es) with the new one.
            // If the old prefix is longer than the new one we need to add padding to ensure
            // that the entire part will hold the same number of bytes. We do this by adding
            // '\0's (nul terminators). This ensures that the text will remain a valid
            // nul-terminated string.
            let padding = old_len.saturating_sub(out.len());
            if padding > 0 {
                target_file.write_all(&vec![0u8; padding])
                    .wrap_err_with(|| format!("Failed to write to {}", target_path.display()))?;
            }

            // Continue with the rest of the bytes
            source_bytes = &source_bytes[end..];
        } else {
            // The old prefix was not found in the (remaining) source bytes.
            // Write the rest of the bytes
            target_file.write_all(source_bytes)
                .wrap_err_with(|| format!("Failed to write to {}", target_path.display()))?;
            return Ok(());
        }
    }
}

/// Create Unix Python entry point script for conda packages.
///
/// This function generates executable shell scripts that serve as command-line entry points
/// for Python packages. These scripts allow Python package functionality to be invoked
/// directly from the command line as standalone executables.
///
/// # Use Cases
///
/// 1. **Conda noarch:python packages**: When installing conda packages with `noarch: python`,
///    entry points defined in `link.json` need to be converted into executable scripts in
///    the environment's `bin/` directory. Examples include:
///    - `pip` command from the pip package
///    - `jupyter` command from jupyter-core
///    - `conda` command from conda itself
///    - Any package that defines console_scripts entry points
///
/// 2. **Python console scripts**: Packages that use setuptools' `console_scripts` entry point
///    mechanism need executable wrappers that properly invoke the underlying Python module
///    and function. This function creates those wrappers.
///
/// 3. **Cross-platform compatibility**: The generated scripts handle shebang limitations
///    (max 127 characters on some systems) by using an exec wrapper when the Python path
///    is too long or contains spaces, ensuring compatibility across different Unix systems.
///
/// 4. **Environment isolation**: Entry points ensure that commands invoke the correct Python
///    interpreter from the conda environment, maintaining proper dependency isolation.
///
/// # Example
///
/// For an entry point with command="pip", module="pip._internal.cli.main", function="main",
/// this function creates a script at `bin/pip` that:
/// - Has a shebang pointing to the environment's Python interpreter
/// - Imports and calls `pip._internal.cli.main.main()`
/// - Is executable and can be run directly from the command line
fn create_unix_python_entry_point(
    target_dir: &Path,
    target_prefix: &str,
    entry_point: &EntryPoint,
    python_info: &PythonInfo,
) -> Result<PathBuf> {
    let relative_path = python_info.bin_dir.join(&entry_point.command);
    let script_path = target_dir.join(&relative_path);

    // Create parent directory
    if let Some(parent) = script_path.parent() {
        lfs::create_dir_all(parent)?;
    }

    // Generate shebang
    let python_path = Path::new(target_prefix).join(&python_info.path);
    let python_path_str = python_path.to_string_lossy().replace('\\', "/");
    let shebang = if python_path_str.len() > 125 || python_path_str.contains(' ') {
        // Use exec wrapper for long shebangs or paths with spaces
        format!("#!/bin/sh\n'''exec' \"{}\" \"$0\" \"$@\" #'''", python_path_str)
    } else {
        format!("#!{}", python_path_str)
    };

    // Generate entry point script content
    let (import_name, _) = entry_point.function.split_once('.')
        .unwrap_or((&entry_point.function, ""));

    let script_content = format!(
        "{}\n\
        # -*- coding: utf-8 -*-\n\
        import re\n\
        import sys\n\n\
        from {} import {}\n\n\
        if __name__ == '__main__':\n\
        \tsys.argv[0] = re.sub(r'(-script\\.pyw?|\\.exe)?$', '', sys.argv[0])\n\
        \tsys.exit({}())\n",
        shebang,
        entry_point.module,
        import_name,
        entry_point.function
    );

    lfs::write(&script_path, script_content)?;

    // Make executable
    utils::set_executable_permissions(&script_path, 0o775)?;

    log::debug!("Created Python entry point: {}", script_path.display());
    Ok(relative_path)
}

/// Prepare conda package metadata (index.json, paths.json, Python info)
/// Returns empty paths vector if paths.json does not exist (caller should fall back to generic linking)
fn prepare_conda_package_metadata(
    package_dir: &Path,
) -> Result<(IndexJson, Vec<PathsEntry>, Option<PythonInfo>)> {
    let index_json = read_index_json(package_dir)
        .wrap_err_with(|| format!("Failed to read index.json from {}", package_dir.display()))?;

    let paths_entries = read_paths_json(package_dir)
        .wrap_err_with(|| format!("Failed to read paths.json from {}", package_dir.display()))?;

    // Get Python info if this is a noarch Python package
    let python_info = if index_json.noarch.as_ref().map(|n| n == "python").unwrap_or(false) {
        get_python_info(&index_json)
            .wrap_err_with(|| "Failed to get Python info for noarch package")?
    } else {
        None
    };

    Ok((index_json, paths_entries, python_info))
}

/// Link a file that requires prefix placeholder replacement
fn copy_file_with_prefix_replacement(
    source_path: &Path,
    target_path: &Path,
    placeholder_info: &PrefixPlaceholder,
    target_prefix: &str,
) -> Result<()> {
    match placeholder_info.file_mode {
        FileMode::Text => {
            copy_replace_textual_placeholder(
                source_path,
                target_path,
                &placeholder_info.placeholder,
                target_prefix,
            )?;
        }
        FileMode::Binary => {
            copy_replace_cstring_placeholder(
                source_path,
                target_path,
                &placeholder_info.placeholder,
                target_prefix,
            )?;
        }
    }

    crate::utils::preserve_file_permissions(source_path, target_path)?;
    Ok(())
}

/// Link a file without prefix replacement, using path_type or plan.link
fn link_file_without_prefix_replacement(
    plan: &InstallationPlan,
    source_path: &Path,
    target_path: &Path,
    path_type: &str,
    fhs_file: &Path,
) -> Result<()> {
    // Detect if source is a symlink
    let is_link = fs::symlink_metadata(source_path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);

    // Determine link type based on path_type and plan capabilities
    let link_type = if path_type == "hardlink" && plan.can_hardlink {
        LinkType::Hardlink
    } else if path_type == "softlink" && plan.can_symlink {
        LinkType::Symlink
    } else {
        // Default to reflink or copy
        LinkType::Reflink
    };

    // Single call to mirror_file
    mirror_file(source_path, target_path, fhs_file, is_link, link_type, plan.can_reflink)?;
    Ok(())
}

/// Link conda package files with prefix replacement and path type handling
fn link_conda_files(
    plan: &InstallationPlan,
    store_fs_dir: &PathBuf,
    final_paths: Vec<(PathsEntry, PathBuf)>,
    target_prefix: &str,
) -> Result<()> {
    for (entry, computed_path) in final_paths {
        // Skip directories and no_link files
        if entry.path_type == "directory" || entry.no_link {
            continue;
        }

        let source_path = store_fs_dir.join(&entry.relative_path);
        let target_path = plan.env_root.join(&computed_path);

        // Create parent directory
        if let Some(parent) = target_path.parent() {
            lfs::create_dir_all(parent)?;
        }

        // Handle prefix placeholder replacement
        if let Some(placeholder_info) = &entry.prefix_placeholder {
            copy_file_with_prefix_replacement(
                &source_path,
                &target_path,
                placeholder_info,
                target_prefix,
            )?;
        } else {
            link_file_without_prefix_replacement(
                plan,
                &source_path,
                &target_path,
                &entry.path_type,
                &computed_path,
            )?;
        }
    }

    Ok(())
}

/// Create Python entry points for noarch Python packages
fn create_conda_entry_points(
    plan: &InstallationPlan,
    package_dir: &Path,
    python_info: &PythonInfo,
    target_prefix: &str,
) -> Result<()> {
    if let Ok(Some(entry_points)) = read_link_json(package_dir) {
        for entry_point in entry_points {
            create_unix_python_entry_point(
                &plan.env_root,
                target_prefix,
                &entry_point,
                python_info,
            )?;
        }
    }
    Ok(())
}

/// Link conda package with prefix replacement, noarch remapping, and entry points
pub fn link_conda_package(plan: &InstallationPlan, store_fs_dir: &PathBuf) -> Result<()> {
    // Get package directory (parent of fs/)
    let package_dir = store_fs_dir.parent()
        .ok_or_else(|| eyre::eyre!("Invalid store_fs_dir path: {}", store_fs_dir.display()))?;

    // Prepare metadata
    let (index_json, paths_entries, python_info) = prepare_conda_package_metadata(package_dir)?;

    // If paths.json is missing (empty entries), fall back to generic linking
    if paths_entries.is_empty() {
        log::info!("paths.json missing or empty for {}, falling back to generic linking", package_dir.display());
        return link_package_generic(plan, store_fs_dir);
    }

    // Compute final paths (with noarch remapping if needed)
    let final_paths = compute_paths(&index_json, &paths_entries, python_info.as_ref());

    // Get target prefix
    let target_prefix = plan.env_root.to_string_lossy().to_string();

    // Link files
    link_conda_files(plan, store_fs_dir, final_paths, &target_prefix)?;

    // Create Python entry points if this is a noarch Python package
    if let Some(py_info) = python_info {
        create_conda_entry_points(plan, package_dir, &py_info, &target_prefix)?;
    }

    Ok(())
}
