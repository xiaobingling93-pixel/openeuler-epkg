//! Installation plan module
//!
//! This module defines the InstallationPlan structure that tracks packages to be installed,
//! upgraded, removed, and exposed during package operations.

use std::collections::HashMap;
use std::sync::Arc;
use std::path::PathBuf;
use color_eyre::Result;
use color_eyre::eyre::WrapErr;
use crate::models::{PACKAGE_CACHE, InstalledPackageInfo, InstalledPackagesMap, LinkType, PackageFormat, channel_config};
use crate::package;
use crate::mmio;
use crate::aur::is_aur_package;
use crate::dirs;

/// Package operation flags
pub mod op_flags {
    /// Whether new package should be exposed (ebin_exposure)
    pub const SHOULD_EXPOSE:    u8 = 1 << 0;
    /// Whether old package should be unexposed (was exposed, now being removed)
    pub const SHOULD_UNEXPOSE:  u8 = 1 << 1;
    /// Whether new_pkg is already in store (has non-empty pkgline)
    pub const IN_STORE:         u8 = 1 << 2;
    /// Whether new_pkg is an AUR package
    pub const IS_AUR:           u8 = 1 << 3;
}

/// Package operation type (L2: Package Operations)
/// Represents a single package operation in a transaction
#[derive(Debug, Clone)]
pub struct PackageOperation {
    /// New package being installed/upgraded (None for pure removals)
    pub new_pkg: Option<(String, Arc<InstalledPackageInfo>)>, // (pkgkey, info)
    /// Old package being removed/upgraded (None for fresh installs)
    pub old_pkg: Option<(String, Arc<InstalledPackageInfo>)>, // (pkgkey, info)
    /// Operation type
    pub op_type: OperationType,
    /// Operation flags (see op_flags module)
    pub flags: u8,
    /// Dependency depth for ordering (from pkgkey_to_depth)
    pub depend_depth: u16,
}

impl PackageOperation {
    /// Check if a flag is set
    pub fn has_flag(&self, flag: u8) -> bool {
        (self.flags & flag) != 0
    }

    /// Set a flag
    #[allow(dead_code)]
    pub fn set_flag(&mut self, flag: u8) {
        self.flags |= flag;
    }

    /// Check if should_expose flag is set
    pub fn should_expose(&self) -> bool {
        self.has_flag(op_flags::SHOULD_EXPOSE)
    }

    /// Check if should_unexpose flag is set
    pub fn should_unexpose(&self) -> bool {
        self.has_flag(op_flags::SHOULD_UNEXPOSE)
    }

    /// Check if in_store flag is set
    #[allow(dead_code)]
    pub fn in_store(&self) -> bool {
        self.has_flag(op_flags::IN_STORE)
    }

    /// Check if is_aur flag is set
    #[allow(dead_code)]
    pub fn is_aur(&self) -> bool {
        self.has_flag(op_flags::IS_AUR)
    }
}

/// Package operation type (L2: Package Operations)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationType {
    /// Fresh installation (new_pkg is Some, old_pkg is None)
    FreshInstall,
    /// Upgrade (both new_pkg and old_pkg are Some)
    Upgrade,
    /// Pure removal (new_pkg is None, old_pkg is Some)
    Removal,
}

/// Filesystem information from statvfs
#[derive(Debug, Clone, Default)]
pub struct FilesystemInfo {
    pub fsid: u64,          // Filesystem ID from statvfs.f_fsid
    pub free_space: u64,    // Free space in bytes
    pub free_inodes: u64,   // Free inodes (u64::MAX if unlimited)
}

#[derive(Debug, Clone)]
pub struct InstallationPlan {
    // Uniform ordered structure (L1: Transaction -> Vec<PackageOperation>)
    pub ordered_operations: Vec<PackageOperation>,

    pub link: LinkType,
    pub can_reflink: bool,

    pub total_download: u64,
    pub total_install: u64,

    pub store_root_fs:      Option<FilesystemInfo>,
    pub env_root_fs:        Option<FilesystemInfo>,
    pub download_cache_fs:  Option<FilesystemInfo>,

    pub store_pkglines_by_pkgname: std::collections::HashMap<String, Vec<String>>,

    // Skipped reinstalls (packages that are already installed with same version)
    pub skipped_reinstalls: InstalledPackagesMap,

    // Cache fields for ordered_operations
    pub fresh_installs: InstalledPackagesMap,
    pub old_removes:    InstalledPackagesMap,
    pub upgrades_new:   InstalledPackagesMap,
    pub upgrades_old:   InstalledPackagesMap,
    pub upgrade_map_old_to_new: HashMap<String, String>,

    // Cache fields for execution (stored early in prepare_installation_plan)
    pub env_root: PathBuf,
    pub store_root: PathBuf,
    pub package_format: PackageFormat,

    // Batch maps (stored in build_completed_maps)
    pub completed_packages: InstalledPackagesMap,
    pub fresh_installs_completed: InstalledPackagesMap,
    pub upgrades_new_completed: InstalledPackagesMap,
    pub upgrades_old_completed: InstalledPackagesMap,
    pub old_removes_completed: InstalledPackagesMap,
}

impl Default for InstallationPlan {
    fn default() -> Self {
        Self {
            ordered_operations: Vec::new(),
            link: LinkType::Symlink,
            can_reflink: false,
            total_download: 0,
            total_install: 0,
            store_root_fs: None,
            env_root_fs: None,
            download_cache_fs: None,
            store_pkglines_by_pkgname: HashMap::new(),
            skipped_reinstalls: InstalledPackagesMap::new(),
            fresh_installs: InstalledPackagesMap::new(),
            old_removes: InstalledPackagesMap::new(),
            upgrades_new: InstalledPackagesMap::new(),
            upgrades_old: InstalledPackagesMap::new(),
            upgrade_map_old_to_new: HashMap::new(),
            // Initialize with defaults - will be set properly in prepare_installation_plan()
            env_root: PathBuf::new(),
            store_root: PathBuf::new(),
            package_format: PackageFormat::default(),
            completed_packages: InstalledPackagesMap::new(),
            fresh_installs_completed: InstalledPackagesMap::new(),
            upgrades_new_completed: InstalledPackagesMap::new(),
            upgrades_old_completed: InstalledPackagesMap::new(),
            old_removes_completed: InstalledPackagesMap::new(),
        }
    }
}

/// Calculate operation flags for a package operation
/// Computes flags based on new_pkg and old_pkg directly
pub fn calculate_op_flags(
    new_pkg: Option<&(String, Arc<InstalledPackageInfo>)>,
    old_pkg: Option<&(String, Arc<InstalledPackageInfo>)>,
) -> u8 {
    let mut flags = 0u8;

    // SHOULD_EXPOSE: new package should be exposed if it has ebin_exposure
    if let Some((_pkgkey, new_pkg_info)) = new_pkg {
        if new_pkg_info.ebin_exposure {
            flags |= op_flags::SHOULD_EXPOSE;
        }
        if !new_pkg_info.pkgline.is_empty() {
            flags |= op_flags::IN_STORE;
        }
        if is_aur_package(&_pkgkey) {
            flags |= op_flags::IS_AUR;
        }
    }

    // SHOULD_UNEXPOSE: old package should be unexposed if it was exposed
    if let Some((_pkgkey, old_pkg_info)) = old_pkg {
        if old_pkg_info.ebin_exposure {
            flags |= op_flags::SHOULD_UNEXPOSE;
        }
    }

    flags
}

/// Create a PackageOperation with calculated flags
pub fn create_package_operation(
    new_pkg: Option<(String, Arc<InstalledPackageInfo>)>,
    old_pkg: Option<(String, Arc<InstalledPackageInfo>)>,
    op_type: OperationType,
) -> PackageOperation {
    let flags = calculate_op_flags(
        new_pkg.as_ref(),
        old_pkg.as_ref(),
    );
    // Calculate depend_depth: use new_pkg.depend_depth if available, otherwise old_pkg.depend_depth
    let depend_depth = if let Some((_, info)) = new_pkg.as_ref() {
        info.depend_depth
    } else if let Some((_, info)) = old_pkg.as_ref() {
        info.depend_depth
    } else {
        0
    };
    PackageOperation {
        new_pkg,
        old_pkg,
        op_type,
        flags,
        depend_depth,
    }
}

/// Sort operations by dependency depth
/// Upgrades and fresh installs are sorted by depth (lowest first)
/// Removals are sorted by reverse depth (highest first, so dependents before dependencies)
pub fn sort_operations_by_depth(operations: &mut [PackageOperation]) {
    operations.sort_by(|a, b| {
        let depth_a = match a.op_type {
            OperationType::Upgrade => a.depend_depth,
            OperationType::Removal => u16::MAX - a.depend_depth,
            OperationType::FreshInstall => a.depend_depth,
        };
        let depth_b = match b.op_type {
            OperationType::Upgrade => b.depend_depth,
            OperationType::Removal => u16::MAX - b.depend_depth,
            OperationType::FreshInstall => b.depend_depth,
        };
        depth_a.cmp(&depth_b)
    });
}

/// Classify packages into fresh installs and upgrades
fn classify_packages(
    all_packages_for_session: &InstalledPackagesMap,
    plan: &mut InstallationPlan,
) -> Result<()> {
    // Ensure PACKAGE_CACHE.installed_packages is loaded
    crate::io::load_installed_packages()?;
    let installed = &*PACKAGE_CACHE.installed_packages.read().unwrap();

    // First pass: classify packages
    for (session_pkgkey, session_pkg_info) in all_packages_for_session {
        if installed.contains_key(session_pkgkey) {
            plan.skipped_reinstalls.insert(session_pkgkey.clone(), Arc::clone(session_pkg_info));
            continue;
        }

        let (is_upgrade, old_pkgkey) = find_upgrade_target(
            session_pkgkey,
            installed,
        );
        if is_upgrade {
            plan.upgrades_new.insert(session_pkgkey.clone(), Arc::clone(session_pkg_info));
            plan.upgrades_old.insert(old_pkgkey.clone(), Arc::clone(installed.get(&old_pkgkey).unwrap()));
            plan.upgrade_map_old_to_new.insert(old_pkgkey.clone(), session_pkgkey.clone());
        } else {
            plan.fresh_installs.insert(session_pkgkey.clone(), Arc::clone(session_pkg_info));
        }
    }

    Ok(())
}


/// Build ordered operations from classified packages
fn build_ordered_operations(plan: &mut InstallationPlan) {
    let mut operations: Vec<PackageOperation> = Vec::new();

    // Add fresh installs
    for (pkgkey, pkg_info) in plan.fresh_installs.iter() {
        operations.push(create_package_operation(
            Some((pkgkey.clone(), Arc::clone(pkg_info))),
            None,
            OperationType::FreshInstall,
        ));
    }

    // Add upgrades
    for (old_pkgkey, old_pkg_info) in plan.upgrades_old.iter() {
        if let Some(new_pkgkey) = plan.upgrade_map_old_to_new.get(old_pkgkey) {
            if let Some(new_pkg_info) = plan.upgrades_new.get(new_pkgkey) {
                operations.push(create_package_operation(
                    Some((new_pkgkey.clone(), Arc::clone(new_pkg_info))),
                    Some((old_pkgkey.clone(), Arc::clone(old_pkg_info))),
                    OperationType::Upgrade,
                ));
            }
        }
    }

    // Add pure removals
    for (pkgkey, pkg_info) in plan.old_removes.iter() {
        // Skip if this is part of an upgrade
        if plan.upgrades_old.contains_key(pkgkey) {
            continue;
        }

        operations.push(create_package_operation(
            None,
            Some((pkgkey.clone(), Arc::clone(pkg_info))),
            OperationType::Removal,
        ));
    }

    // Sort operations by depend_depth
    sort_operations_by_depth(&mut operations);

    plan.ordered_operations = operations;
}

/// Convert an InstallationPlan to a GenerationCommand
/// Extracts package lists and expose/unexpose operations from the plan
pub fn plan_to_generation_command(plan: &InstallationPlan) -> crate::models::GenerationCommand {
    use crate::models::GenerationCommand;

    // Extract expose/unexpose operations from ordered_operations
    let mut new_exposes = Vec::new();
    let mut del_exposes = Vec::new();

    for op in &plan.ordered_operations {
        if op.should_expose() {
            if let Some((pkgkey, _)) = &op.new_pkg {
                new_exposes.push(pkgkey.clone());
            }
        }
        if op.should_unexpose() {
            if let Some((old_pkgkey, _)) = &op.old_pkg {
                del_exposes.push(old_pkgkey.clone());
            }
        }
    }

    GenerationCommand {
        timestamp:      String::new(),  // Will be set by caller
        action:         String::new(),  // Will be set by caller
        command_line:   String::new(),  // Will be set by caller
        fresh_installs: plan.fresh_installs.keys().cloned().collect(),
        upgrades_new:   plan.upgrades_new.keys().cloned().collect(),
        upgrades_old:   plan.upgrades_old.keys().cloned().collect(),
        old_removes:    plan.old_removes.keys().cloned().collect(),
        new_exposes,
        del_exposes,
    }
}

pub fn prepare_installation_plan(
    all_packages_for_session: &InstalledPackagesMap,
    explicit_removes: Option<InstalledPackagesMap>,
) -> Result<InstallationPlan> {
    // Get paths and format early
    let env_root = dirs::get_default_env_root()?;
    let store_root = dirs().epkg_store.clone();
    let package_format = channel_config().format;

    let mut plan = InstallationPlan::default();
    plan.env_root = env_root;
    plan.store_root = store_root;
    plan.package_format = package_format;

    // Classify packages into fresh installs and upgrades
    classify_packages(all_packages_for_session, &mut plan)?;

    // Determine which packages should be removed
    if let Some(old_removes) = explicit_removes {
        plan.old_removes = old_removes;
    } else {
        plan.old_removes = find_orphaned_packages(&plan.upgrades_old, &plan.skipped_reinstalls)?;
    }

    // Build ordered operations from classified packages
    build_ordered_operations(&mut plan);

    // Fill pkglines for packages that already exist in the store
    crate::store::fill_pkglines_in_plan(&mut plan)
        .with_context(|| "Failed to find existing packages in store")?;

    Ok(plan)
}

/// Find orphaned packages that should be removed
/// An orphaned package is one that has no remaining reverse dependencies
/// (i.e., no other installed package depends on it)
/// Packages with depend_depth=0 (user-requested packages) are never considered orphans
/// Essential packages are never considered orphans
fn find_orphaned_packages(
    upgrades_old: &InstalledPackagesMap,
    skipped_reinstalls: &InstalledPackagesMap,
) -> Result<InstalledPackagesMap> {
    let mut old_removes = InstalledPackagesMap::new();
    // Helper function to check if a package is essential
    let is_essential = |pkgkey: &str| -> bool {
        if let Ok(pkgname) = package::pkgkey2pkgname(pkgkey) {
            mmio::is_essential_pkgname(&pkgname)
        } else {
            false
        }
    };

    // Calculate possible orphans: installed packages that are not being skipped or upgraded
    // Exclude packages with depend_depth=0 (user-requested packages) and essential packages
    let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
    let possible_orphans: Vec<String> = installed
        .iter()
        .filter(|(pkgkey, pkg_info)| {
            !skipped_reinstalls.contains_key(*pkgkey) &&
            !upgrades_old.contains_key(*pkgkey) &&
            pkg_info.depend_depth > 0 &&    // Exclude user-requested packages (depend_depth=0)
            !is_essential(pkgkey)           // Exclude essential packages
        })
        .map(|(pkgkey, _)| pkgkey.clone())
        .collect();

    if possible_orphans.is_empty() {
        return Ok(old_removes);
    }

    // Build pkgkey_to_depends for possible orphans
    let mut pkgkey_to_depends: HashMap<String, Vec<String>> = HashMap::new();
    for pkgkey in &possible_orphans {
        let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
        if let Some(pkg_info) = installed.get(pkgkey.as_str()) {
            pkgkey_to_depends.insert(pkgkey.clone(), pkg_info.depends.clone());
        }
        drop(installed);
    }

    // Build remaining_rdepends for each possible orphan
    // Filter out rdepends that are being removed or upgraded (old version)
    // Keep rdepends that are staying installed (skipped_reinstalls, upgrades_new, fresh_installs)
    let mut remaining_rdepends: HashMap<String, Vec<String>> = HashMap::new();
    let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
    for pkgkey in &possible_orphans {
        if let Some(pkg_info) = installed.get(pkgkey.as_str()) {
            let filtered_rdepends: Vec<String> = pkg_info.rdepends
                .iter()
                .filter(|rdep_pkgkey| {
                    // Filter out rdepends that are being removed or upgraded (old version)
                    // These won't keep the package alive
                    let is_being_removed = old_removes.contains_key(*rdep_pkgkey);
                    let is_old_upgrade = upgrades_old.contains_key(*rdep_pkgkey);

                    // Keep rdepends that are staying installed:
                    // - skipped_reinstalls: staying as-is
                    // - upgrades_new: new version staying
                    // - fresh_installs: being installed
                    // Also ensure the rdepend is still in installed_packages (not already removed)
                    !is_being_removed && !is_old_upgrade &&
                    installed.contains_key(*rdep_pkgkey)
                })
                .cloned()
                .collect();
            remaining_rdepends.insert(pkgkey.clone(), filtered_rdepends);
        }
    }
    drop(installed);

    // Ensure all possible orphans have an entry in remaining_rdepends
    for pkgkey in &possible_orphans {
        remaining_rdepends.entry(pkgkey.clone()).or_insert_with(Vec::new);
    }

    // Loop to find orphans recursively (similar to calculate_pkgkey_to_depth)
    loop {
        let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
        // Find packages with empty remaining_rdepends => these are orphans
        // Exclude packages with depend_depth=0 (user-requested packages) and essential packages
        let orphan_pkgkeys: Vec<String> = remaining_rdepends
            .iter()
            .filter(|(pkgkey, rdepends)| {
                rdepends.is_empty() && {
                    // Double-check that it's not a user-requested package or essential package
                    if let Some(pkg_info) = installed.get(*pkgkey) {
                        pkg_info.depend_depth > 0 && !is_essential(pkgkey)
                    } else {
                        false
                    }
                }
            })
            .map(|(pkgkey, _)| pkgkey.clone())
            .collect();
        drop(installed);

        if orphan_pkgkeys.is_empty() {
            // No more orphans found
            break;
        }

        log::debug!("Found {} orphaned packages", orphan_pkgkeys.len());

        // Add orphans to old_removes
        let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
        for orphan_pkgkey in &orphan_pkgkeys {
            if let Some(pkg_info) = installed.get(orphan_pkgkey) {
                old_removes.insert(orphan_pkgkey.clone(), Arc::clone(pkg_info));
                log::debug!("Added orphaned package '{}' to old_removes", orphan_pkgkey);
            }
        }
        drop(installed);

        // Remove orphan nodes from remaining_rdepends and update reverse dependencies
        for orphan_pkgkey in &orphan_pkgkeys {
            // Remove this orphan from remaining_rdepends
            remaining_rdepends.remove(orphan_pkgkey);

            // Remove this orphan from reverse dependency lists of packages it depends on
            // If orphan_pkgkey depends on dep_pkgkey, then orphan_pkgkey is in remaining_rdepends[dep_pkgkey]
            // We need to remove orphan_pkgkey from remaining_rdepends[dep_pkgkey]
            if let Some(depends_list) = pkgkey_to_depends.get(orphan_pkgkey) {
                for dep_pkgkey in depends_list {
                    if let Some(rdepends) = remaining_rdepends.get_mut(dep_pkgkey) {
                        rdepends.retain(|x| x != orphan_pkgkey);
                    }
                }
            }
        }
    }

    Ok(old_removes)
}

/// Determine if a package is an upgrade by comparing package names and architectures
/// Returns (is_upgrade, old_pkgkey) if it's an upgrade, (false, "") otherwise
///
/// For AUR packages, matches by pkgname+version only (ignoring arch).
/// For non-AUR packages, matches by pkgname+arch (version is compared separately if needed).
pub fn find_upgrade_target(
    new_pkgkey: &str,
    installed: &InstalledPackagesMap,
) -> (bool, String) {
    let (new_pkgname, new_version, new_arch) = match package::parse_pkgkey(new_pkgkey) {
        Ok(parts) => parts,
        Err(_) => return (false, String::new()),
    };

    let is_aur = is_aur_package(new_pkgkey);

    for (old_pkgkey, _) in installed.iter() {
        if old_pkgkey == new_pkgkey {
            continue;
        }

        match package::parse_pkgkey(old_pkgkey) {
            Ok((old_pkgname, old_version, old_arch)) => {
                if is_aur {
                    // For AUR packages: match by pkgname+version only (arch may change from "any" to actual arch)
                    if new_pkgname == old_pkgname && new_version == old_version {
                        return (true, old_pkgkey.clone());
                    }
                } else {
                    // For non-AUR packages: match by pkgname+arch (version comparison handled separately)
                    if new_pkgname == old_pkgname && new_arch == old_arch {
                        return (true, old_pkgkey.clone());
                    }
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

/// Prompt the user with the installation plan and confirm before proceeding.
/// Returns actions_planned
pub fn prompt_and_confirm_install_plan(
    plan: &InstallationPlan,
) -> Result<bool> {
    let actions_planned = display_installation_plan(plan);

    if !actions_planned {
        println!("\nNo changes planned based on the current request.");
        return Ok(false);
    }

    print_installation_summary(plan);
    print_download_requirements(plan)?;

    crate::utils::user_prompt_and_confirm()
}

/// Display the installation plan details to the user
fn display_installation_plan(plan: &InstallationPlan) -> bool {
    let mut actions_planned = false;
    let mut fresh_installs = Vec::new();
    let mut upgrades = Vec::new();
    let mut removals = Vec::new();
    let mut exposes = Vec::new();
    let mut unexposes = Vec::new();

    for op in &plan.ordered_operations {
        match op.op_type {
            OperationType::FreshInstall => {
                if let Some((pkgkey, pkg_info)) = &op.new_pkg {
                    fresh_installs.push((pkgkey.clone(), Arc::clone(pkg_info)));
                }
            }
            OperationType::Upgrade => {
                if let (Some((new_pkgkey, _)), Some((old_pkgkey, _))) = (&op.new_pkg, &op.old_pkg) {
                    upgrades.push((old_pkgkey.clone(), new_pkgkey.clone()));
                }
            }
            OperationType::Removal => {
                if let Some((pkgkey, _)) = &op.old_pkg {
                    removals.push(pkgkey.clone());
                }
            }
        }
        if op.should_expose() {
            if let Some((pkgkey, _)) = &op.new_pkg {
                exposes.push(pkgkey.clone());
            }
        }
        if op.should_unexpose() {
            if let Some((pkgkey, _)) = &op.old_pkg {
                unexposes.push(pkgkey.clone());
            }
        }
    }

    if !fresh_installs.is_empty() {
        actions_planned = true;
        println!("Packages to be freshly installed:");
        let mut fresh_map = HashMap::new();
        for (k, v) in fresh_installs {
            fresh_map.insert(k, v);
        }
        print_packages_by_depend_depth(&fresh_map);
    }

    if !upgrades.is_empty() {
        actions_planned = true;
        println!("Packages to be upgraded:");
        for (old_pkgkey, new_pkgkey) in upgrades {
            println!("- {} (replacing {})", new_pkgkey, old_pkgkey);
        }
    }

    if !removals.is_empty() {
        actions_planned = true;
        println!("Packages to be removed:");
        for pkgkey in removals {
            println!("- {}", pkgkey);
        }
    }

    if !exposes.is_empty() {
        actions_planned = true;
        println!("Packages to be exposed:");
        for pkgkey in exposes {
            println!("- {}", pkgkey);
        }
    }

    if !unexposes.is_empty() {
        actions_planned = true;
        println!("Packages to be unexposed:");
        for pkgkey in unexposes {
            println!("- {}", pkgkey);
        }
    }

    actions_planned
}

fn print_packages_by_depend_depth(packages: &InstalledPackagesMap) {
    // Convert HashMap to a Vec of tuples (pkgkey, info)
    let mut packages_vec: Vec<(&String, &Arc<InstalledPackageInfo>)> = packages.iter().map(|(k, v)| (k, v)).collect();

    // Sort by depend_depth
    packages_vec.sort_by(|a, b| a.1.depend_depth.cmp(&b.1.depend_depth));

    // Print the header
    println!("{:<5} {:>10}  {:<30}", "DEPTH", "SIZE", "PACKAGE");

    // Print each package
    for (pkgkey, info) in packages_vec {
        // Try to load package info to get size
        let size_str = match crate::package_cache::load_package_info(pkgkey) {
            Ok(package) => {
                format!("{}", crate::utils::format_size(package.size as u64))
            }
            Err(_) => "".to_string(),
        };

        println!("{:<5} {:>10}  {:<30}", info.depend_depth, size_str, pkgkey);
    }
}

/// Print summary statistics for the installation plan
fn print_installation_summary(plan: &InstallationPlan) {
    let mut num_upgraded = 0;
    let mut num_new = 0;
    let mut num_remove = 0;
    let mut num_expose = 0;
    let mut num_unexpose = 0;

    for op in &plan.ordered_operations {
        match op.op_type {
            OperationType::FreshInstall => num_new += 1,
            OperationType::Upgrade => num_upgraded += 1,
            OperationType::Removal => num_remove += 1,
        }
        if op.should_expose() {
            num_expose += 1;
        }
        if op.should_unexpose() {
            num_unexpose += 1;
        }
    }

    println!(
        "\n{} upgraded, {} newly installed, {} to remove, {} to expose, {} to unexpose.",
        num_upgraded, num_new, num_remove, num_expose, num_unexpose
    );
}

/// Calculate and print download and disk space requirements
fn print_download_requirements(plan: &InstallationPlan) -> Result<()> {
    if plan.total_download > 0 {
        println!(
            "Need to get {} archives.",
            crate::utils::format_size(plan.total_download)
        );
        println!(
            "After this operation, {} of additional disk space will be used.",
            crate::utils::format_size(plan.total_install)
        );
    }

    Ok(())
}
