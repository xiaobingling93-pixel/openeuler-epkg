//! Package cache management module
//!
//! This module provides functions for managing the package cache, including adding packages
//! to the cache and updating various indexes (pkgkey2package, pkgname2packages, provide2pkgnames).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use color_eyre::Result;
use crate::models::{Package, PackageFormat, PackageCache, PACKAGE_CACHE, InstalledPackagesMap};
use crate::parse_provides::parse_provides;

impl PackageCache {
    pub fn new() -> Self {
        Self {
            pkgkey2package: RwLock::new(HashMap::new()),
            pkgline2package: RwLock::new(HashMap::new()),
            pkgname2packages: RwLock::new(HashMap::new()),
            provide2pkgnames: RwLock::new(HashMap::new()),
            installed_packages: RwLock::<InstalledPackagesMap>::default(),
            pkgline2installed: RwLock::<InstalledPackagesMap>::default(),
            world: RwLock::new(HashMap::new()),
            pkgline2filelist: RwLock::new(HashMap::new()),
            installed_path_lookup_for_unpack: RwLock::new(None),
        }
    }

    /// Clear all caches (useful for tests to ensure clean state)
    pub fn clear(&self) {
        self.pkgkey2package.write().unwrap().clear();
        self.pkgline2package.write().unwrap().clear();
        self.pkgname2packages.write().unwrap().clear();
        self.provide2pkgnames.write().unwrap().clear();
        self.installed_packages.write().unwrap().clear();
        self.pkgline2installed.write().unwrap().clear();
        self.world.write().unwrap().clear();
        self.pkgline2filelist.write().unwrap().clear();
        *self.installed_path_lookup_for_unpack.write().unwrap() = None;
    }
}

/// Create a virtual package with given parameters
pub fn create_virtual_package(
    pkgname: &str,
    version: &str,
    pkgkey_version: Option<&str>,
    format: PackageFormat,
) -> Package {
    use crate::package;

    let arch = std::env::consts::ARCH.to_string();
    let pkgkey_version = pkgkey_version.unwrap_or(version);
    let pkgkey = package::format_pkgkey(pkgname, pkgkey_version, &arch);

    let summary = format!("Virtual package: {}", pkgname);
    let description = Some(format!("System virtual package for {}", pkgname));

    Package {
        pkgname: pkgname.to_string(),
        version: version.to_string(),
        arch,
        summary,
        description,
        format,
        pkgkey,
        repodata_name: "virtual".to_string(),
        ..Default::default()
    }
}

/// Helper to add a package to cache and update indexes
pub fn add_package_to_cache(package: Arc<Package>, format: PackageFormat) {
    let pkgkey = package.pkgkey.clone();
    let pkgname = package.pkgname.clone();

    // Add to pkgkey2package
    PACKAGE_CACHE.pkgkey2package.write().unwrap().insert(pkgkey.clone(), Arc::clone(&package));

    // Update pkgname2packages index
    PACKAGE_CACHE
        .pkgname2packages
        .write()
        .unwrap()
        .entry(pkgname.clone())
        .or_insert_with(Vec::new)
        .push(Arc::clone(&package));

    // Update provide2pkgnames index
    // IMPORTANT: Provides are in the form cap_with_arch=version (e.g., "libfoo(x86-64)=2.0")
    // cap_with_arch is an atomic tag that should NEVER be split. The provide2pkgnames
    // index is keyed by cap_with_arch (e.g., "libfoo(x86-64)"), not by cap alone.
    // When doing lookups, always use cap_with_arch directly, never strip the arch.
    for provide in &package.provides {
        // Parse provides string and extract names with optional versions
        // parse_provides preserves cap_with_arch (e.g., "libfoo(x86-64)")
        let provide_map = parse_provides(provide, format);
        for (provide_name, _version) in provide_map {
            // provide_name is cap_with_arch (atomic, never split)
            // version is available but not currently used for indexing
            PACKAGE_CACHE
                .provide2pkgnames
                .write()
                .unwrap()
                .entry(provide_name)
                .or_insert_with(HashSet::new)
                .insert(pkgname.clone());
        }
    }
}

/// Add Conda virtual packages to cache
pub fn add_conda_virtual_packages_to_cache() -> Result<()> {
    match crate::conda_pkg::detect_conda_virtual_packages() {
        Ok(virtual_packages) => {
            for virtual_pkg in virtual_packages {
                log::debug!("Adding virtual package to cache: {}={}", virtual_pkg.pkgname, virtual_pkg.version);
                add_package_to_cache(Arc::new(virtual_pkg), PackageFormat::Conda);
            }
            Ok(())
        }
        Err(e) => {
            log::warn!("Failed to detect Conda virtual packages: {}", e);
            Err(e)
        }
    }
}


/// Add Debian virtual packages to cache
pub fn add_deb_virtual_packages_to_cache() -> Result<()> {
    // Add virtual packages to satisfy systemd | systemd-standalone-sysusers | systemd-sysusers
    let virtual_packages = [
        "systemd-sysusers",
        "systemd-standalone-sysusers",
    ];
    for pkgname in virtual_packages {
        let virtual_pkg = create_virtual_package(
            pkgname,
            "1",
            None,
            PackageFormat::Deb,
        );
        log::debug!("Adding Debian virtual package to cache: {}={}", virtual_pkg.pkgname, virtual_pkg.version);
        add_package_to_cache(Arc::new(virtual_pkg), PackageFormat::Deb);
    }
    Ok(())
}

pub fn map_pkgname2packages(pkgname: &str) -> Result<Vec<Package>> {
    // First check if we have packages in pkgname2packages index (for testing)
    if let Some(cached_packages) = PACKAGE_CACHE.pkgname2packages.read().unwrap().get(pkgname) {
        if !cached_packages.is_empty() {
            return Ok(cached_packages.iter().map(|pkg_arc| (**pkg_arc).clone()).collect());
        }
    }

    // Fall back to mmio lookup (for production)
    match crate::mmio::map_pkgname2packages(pkgname) {
        Ok(packages_list) => {
            for package in &packages_list {
                // cache for later references and update indexes
                log::trace!("Caching package: {}", package.pkgkey);
                let format = package.format;
                add_package_to_cache(Arc::new(package.clone()), format);
            }
            return Ok(packages_list);
        },
        Err(e) => Err(e)
    }
}

pub fn map_pkgline2package(pkgline: &str) -> Result<Arc<Package>> {
    // Check cache first
    if let Some(package) = PACKAGE_CACHE.pkgline2package.read().unwrap().get(pkgline) {
        log::trace!("Found cached package info for pkgline '{}'", pkgline);
        return Ok(Arc::clone(package));
    }

    // Load from mmio function
    match crate::mmio::map_pkgline2package(pkgline) {
        Ok(package) => {
            log::trace!("Caching package from pkgline: {}", pkgline);
            let arc_package = Arc::new(package);
            PACKAGE_CACHE.pkgline2package.write().unwrap().insert(pkgline.to_string(), Arc::clone(&arc_package));
            Ok(arc_package)
        },
        Err(e) => Err(e)
    }
}

pub fn load_package_info(pkgkey: &str) -> Result<Arc<Package>> {
    log::trace!("Loading package info for '{}'", pkgkey);
    // Try to find in cache first
    if let Some(package) = PACKAGE_CACHE.pkgkey2package.read().unwrap().get(pkgkey) {
        log::trace!("Found cached package info for '{}'", pkgkey);
        return Ok(Arc::clone(package));
    }

    // Query info in packages.txt
    log::debug!("Package '{}' not in cache, loading from repository", pkgkey);
    match crate::mmio::map_pkgkey2package(pkgkey) {
        Ok(package) => {
            let format = package.format;
            let arc_package = Arc::new(package);
            // Cache the package for future use and update indexes
            add_package_to_cache(Arc::clone(&arc_package), format);
            Ok(arc_package)
        }
        Err(e) => {
            Err(e)
        }
    }
}


/// Get filelist for a package, either from cache or from store
/// Fills the cache if it wasn't already there
/// Returns relative paths from `filelist.txt` (files and directories; dirs end with `/`).
pub fn map_pkgline2filelist(
    store_root: &std::path::Path,
    pkgline: &str,
) -> color_eyre::Result<Vec<String>> {
    use color_eyre::eyre::Context;

    // Check cache first
    {
        let cache = PACKAGE_CACHE.pkgline2filelist.read().unwrap();
        if let Some(cached_filelist) = cache.get(pkgline) {
            return Ok(cached_filelist.clone());
        }
    }

    // Not in cache, get from store using get_package_files (reads filelist.txt; dirs have trailing `/`)
    let file_list = crate::utils::get_package_files(store_root, pkgline)
        .with_context(|| format!("Failed to get filelist for {}/{}/info/filelist.txt", store_root.display(), pkgline))?;

    // Cache it for future use
    PACKAGE_CACHE.pkgline2filelist.write().unwrap()
        .insert(pkgline.to_string(), file_list.clone());

    Ok(file_list)
}

/// Get file list with type information for a package from store.
/// Returns Vec<MtreeFileInfo> with file type (file/dir/link) for accurate disk space estimation.
pub fn map_pkgline2filelist_with_info(
    store_root: &std::path::Path,
    pkgline: &str,
) -> color_eyre::Result<Vec<crate::mtree::MtreeFileInfo>> {
    use color_eyre::eyre::Context;

    let store_fs_dir = store_root.join(pkgline).join("fs");
    if !crate::lfs::exists_on_host(&store_fs_dir) {
        return Ok(Vec::new());
    }

    let file_infos = crate::utils::list_package_files_with_info(store_fs_dir.to_str()
        .ok_or_else(|| color_eyre::eyre::eyre!("Invalid store fs path"))?)
        .with_context(|| format!("Failed to get filelist for {}/{}/info/filelist.txt", store_root.display(), pkgline))?;

    Ok(file_infos)
}
