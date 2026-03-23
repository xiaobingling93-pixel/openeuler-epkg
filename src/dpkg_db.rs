//! Generate dpkg-compatible metadata for environments
//!
//! This module creates the `/var/lib/dpkg/status` file and `/var/lib/dpkg/info/` symlinks
//! so that the real dpkg/dpkg-query commands can see packages installed by epkg.
//!
//! # dpkg Database Structure
//!
//! The dpkg database is located at `/var/lib/dpkg/` and contains:
//! - `status` - list of all installed packages with their status
//! - `info/{pkgname}.*` - control files for each package (conffiles, templates, etc.)
//! - `diversions` - package diversions
//! - `alternatives/` - alternatives database
//!
//! # On-disk Layout Examples
//!
//! ## /var/lib/dpkg/status file format
//!
//! ```text
//! Package: gcc
//! Status: install ok installed
//! Priority: optional
//! Section: devel
//! Installed-Size: 36
//! Maintainer: Debian GCC Maintainers <debian-gcc@lists.debian.org>
//! Architecture: amd64
//! Source: gcc-defaults (1.220)
//! Version: 4:14.2.0-1
//! Provides: c-compiler
//! Depends: cpp (= 4:14.2.0-1), cpp-x86-64-linux-gnu (= 4:14.2.0-1), gcc-14 (>= 14.2.0-6~)
//! Description: GNU C compiler
//!  This is the GNU C compiler, a fairly portable optimizing compiler for C.
//!
//! Package: g++
//! Status: install ok installed
//! ...
//! ```
//!
//! ## /var/lib/dpkg/info/ directory layout
//!
//! ```text
//! /var/lib/dpkg/info/
//! ├── gcc.conffiles      # symlink -> $HOME/.epkg/store/.../info/deb/conffiles
//! ├── gcc.md5sums        # symlink -> $HOME/.epkg/store/.../info/deb/md5sums
//! ├── gcc.postinst       # symlink -> $HOME/.epkg/store/.../info/deb/postinst
//! ├── gcc.postrm         # symlink -> $HOME/.epkg/store/.../info/deb/postrm
//! ├── gcc.prerm          # symlink -> $HOME/.epkg/store/.../info/deb/prerm
//! ├── gcc.list           # (not created - epkg uses store-based file tracking)
//! └── ...
//! ```
//!
//! ## Flow during installation
//!
//! 1. Before running scriptlets: generate dpkg status for installed + pending packages
//! 2. Scriptlets run and can use `dpkg --status <pkg>` to check if packages exist
//! 3. After installation completes: regenerate dpkg status with final package list
//!
//! ## Why symlinks instead of copying
//!
//! The info files are symlinked to the store location rather than copied because:
//! - Saves disk space (no duplication)
//! - Files are read-only anyway
//! - When package is removed, symlink just gets deleted

use color_eyre::Result;
use std::path::PathBuf;

use crate::models::{InstalledPackageInfo, PACKAGE_CACHE};
use crate::dirs;
use crate::lfs;

/// Get the dpkg admin directory path for the current environment
fn get_dpkg_admindir() -> Result<PathBuf> {
    let env_root = dirs::get_env_root(crate::models::config().common.env_name.clone())?;
    Ok(crate::dirs::path_join(&env_root, &["var", "lib", "dpkg"]))
}

/// Generate dpkg status entry for a package
fn generate_status_entry(
    pkgname: &str,
    version: &str,
    arch: &str,
    info: &InstalledPackageInfo,
    control_content: Option<&str>,
) -> String {
    let mut entry = String::new();

    // Package header
    entry.push_str(&format!("Package: {}\n", pkgname));
    entry.push_str("Status: install ok installed\n");

    // Track which fields we've already seen
    let mut seen_depends = false;

    // Parse control file for additional fields if available
    if let Some(control) = control_content {
        for line in control.lines() {
            // Skip Package, Status, Version, Architecture as we set them ourselves
            if line.starts_with("Package:") ||
               line.starts_with("Status:") ||
               line.starts_with("Version:") ||
               line.starts_with("Architecture:") {
                continue;
            }
            // Track if we've seen Depends
            if line.starts_with("Depends:") {
                seen_depends = true;
            }
            // Skip empty lines
            if line.is_empty() {
                continue;
            }
            entry.push_str(line);
            entry.push('\n');
        }
    }

    // Essential fields
    entry.push_str(&format!("Version: {}\n", version));
    entry.push_str(&format!("Architecture: {}\n", arch));

    // Add depends if available and not already in control
    if !seen_depends && !info.depends.is_empty() {
        // Convert pkgkey format to dpkg format
        let deps: Vec<String> = info.depends.iter()
            .map(|pkgkey| {
                let name = crate::package::pkgkey2pkgname(pkgkey).unwrap_or_else(|_| pkgkey.clone());
                // For now, just use the package name without version constraint
                name
            })
            .collect();
        entry.push_str(&format!("Depends: {}\n", deps.join(", ")));
    }

    entry.push('\n');

    entry
}

/// Read control file from package store
fn read_package_control(pkgline: &str) -> Option<String> {
    let store_path = dirs().epkg_store.join(pkgline);
    let control_path = crate::dirs::path_join(&store_path, &["info", "deb", "control"]);
    std::fs::read_to_string(&control_path).ok()
}

/// Generate the dpkg status file for all installed packages
pub fn generate_dpkg_status() -> Result<()> {
    let admindir = get_dpkg_admindir()?;
    let status_path = admindir.join("status");

    // Ensure directory exists
    lfs::create_dir_all(&admindir)?;

    // Load installed packages
    crate::io::load_installed_packages()?;
    let installed = PACKAGE_CACHE.installed_packages.read().unwrap();

    // Build status entries sorted by package name
    let mut entries: Vec<(String, String)> = Vec::new();

    for (pkgkey, info) in installed.iter() {
        let pkgname = crate::package::pkgkey2pkgname(pkgkey)
            .or_else(|_| crate::package::parse_pkgline(&info.pkgline).map(|p| p.pkgname))
            .unwrap_or_default();
        let version = crate::package::pkgkey2version(pkgkey)
            .or_else(|_| crate::package::parse_pkgline(&info.pkgline).map(|p| p.version))
            .unwrap_or_default();
        let arch = info.arch.clone();

        let control = read_package_control(&info.pkgline);
        let entry = generate_status_entry(&pkgname, &version, &arch, info, control.as_deref());

        entries.push((pkgname, entry));
    }

    // Sort by package name
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    // Write status file
    let count = entries.len();
    let mut content = String::new();
    for (_, entry) in entries {
        content.push_str(&entry);
    }

    lfs::write(&status_path, content)?;
    log::debug!("Generated dpkg status file with {} packages at {}", count, status_path.display());

    Ok(())
}

/// Append pending packages to the dpkg status file
/// This is used before running scriptlets so that dpkg can see packages being installed
pub fn append_pending_to_dpkg_status(pending: &crate::models::InstalledPackagesMap) -> Result<()> {
    if pending.is_empty() {
        return Ok(());
    }

    let admindir = get_dpkg_admindir()?;
    let status_path = admindir.join("status");

    // Ensure directory exists
    lfs::create_dir_all(&admindir)?;

    // Read existing status content
    let mut content = if status_path.exists() {
        std::fs::read_to_string(&status_path)?
    } else {
        String::new()
    };

    // Track which packages are already in the status file
    let mut existing_packages: std::collections::HashSet<String> = std::collections::HashSet::new();
    for line in content.lines() {
        if line.starts_with("Package: ") {
            let pkgname = line.strip_prefix("Package: ").unwrap_or("");
            existing_packages.insert(pkgname.to_string());
        }
    }

    // Add pending packages that aren't already in the status file
    for (pkgkey, info) in pending.iter() {
        let pkgname = crate::package::pkgkey2pkgname(pkgkey)
            .or_else(|_| crate::package::parse_pkgline(&info.pkgline).map(|p| p.pkgname))
            .unwrap_or_default();

        // Skip if already in status file
        if existing_packages.contains(&pkgname) {
            continue;
        }

        let version = crate::package::pkgkey2version(pkgkey)
            .or_else(|_| crate::package::parse_pkgline(&info.pkgline).map(|p| p.version))
            .unwrap_or_default();
        let arch = info.arch.clone();

        let control = read_package_control(&info.pkgline);
        let entry = generate_status_entry(&pkgname, &version, &arch, info, control.as_deref());

        content.push_str(&entry);
    }

    lfs::write(&status_path, content)?;
    log::debug!("Appended {} pending packages to dpkg status", pending.len());

    Ok(())
}

/// Create symlinks in /var/lib/dpkg/info/ for a package
pub fn create_dpkg_info_symlinks(pkgname: &str, pkgline: &str) -> Result<()> {
    let admindir = get_dpkg_admindir()?;
    let info_dir = admindir.join("info");

    lfs::create_dir_all(&info_dir)?;

    let store_info_path =
        crate::dirs::path_join(&dirs().epkg_store.join(pkgline), &["info", "deb"]);

    // Skip if no deb info directory
    if !store_info_path.exists() {
        return Ok(());
    }

    // Create symlinks for each info file
    for entry in std::fs::read_dir(&store_info_path)? {
        let entry = entry?;
        let filename = entry.file_name();
        let filename_str = filename.to_string_lossy();

        // Skip control file (not needed in info directory)
        if filename_str == "control" {
            continue;
        }

        // Create symlink: {pkgname}.{filetype}
        let link_name = format!("{}.{}", pkgname, filename_str);
        let link_name = lfs::sanitize_path_for_windows(std::path::Path::new(&link_name));
        let link_path = info_dir.join(&link_name);

        // Remove existing symlink if present
        if link_path.exists() || link_path.symlink_metadata().is_ok() {
            lfs::remove_file(&link_path)?;
        }

        // Create relative symlink to store path
        // The path will be resolved inside the namespace
        let target = entry.path();
        lfs::symlink(&target, &link_path)?;
    }

    Ok(())
}

/// Create symlinks for all installed packages
pub fn create_all_dpkg_info_symlinks() -> Result<()> {
    crate::io::load_installed_packages()?;
    let installed = PACKAGE_CACHE.installed_packages.read().unwrap();

    for (pkgkey, info) in installed.iter() {
        let pkgname = crate::package::pkgkey2pkgname(pkgkey)
            .or_else(|_| crate::package::parse_pkgline(&info.pkgline).map(|p| p.pkgname))
            .unwrap_or_default();
        if let Err(e) = create_dpkg_info_symlinks(&pkgname, &info.pkgline) {
            log::warn!("Failed to create dpkg info symlinks for {}: {}", pkgname, e);
        }
    }

    Ok(())
}

/// Generate complete dpkg database (status + info symlinks)
pub fn generate_dpkg_database() -> Result<()> {
    generate_dpkg_status()?;
    create_all_dpkg_info_symlinks()?;
    Ok(())
}
