//! Installation plan module
//!
//! This module defines the InstallationPlan structure that tracks packages to be installed,
//! upgraded, removed, and exposed during package operations.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::path::PathBuf;
use color_eyre::Result;
use color_eyre::eyre::WrapErr;
use crate::models::{PACKAGE_CACHE, InstalledPackageInfo, InstalledPackagesMap, LinkType, PackageFormat, channel_config};
use crate::package;
use crate::mmio;
use crate::aur::is_aur_package;
use crate::dirs;
use crate::hooks::{Hook, HookWhen};

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
    /// New package key being installed/upgraded (None for pure removals)
    pub new_pkgkey: Option<String>,
    /// Old package key being removed/upgraded (None for fresh installs)
    pub old_pkgkey: Option<String>,
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

    // Single source of truth for new packages
    // = (all_packages_for_session - skipped_reinstalls)
    // = (fresh_installs + upgrades_new)
    pub new_pkgs: InstalledPackagesMap,

    /// Set of currently installed package keys (snapshot at plan creation).
    pub installed:      HashSet<String>,

    // Cache fields for ordered_operations (pkgkey sets)
    pub fresh_installs: HashSet<String>,
    pub old_removes:    HashSet<String>,
    pub upgrades_new:   HashSet<String>,
    pub upgrades_old:   HashSet<String>,
    pub upgrade_map_old_to_new: HashMap<String, String>,

    // Cache fields for execution (stored early in prepare_installation_plan)
    pub env_root: PathBuf,
    pub store_root: PathBuf,
    pub package_format: PackageFormat,

    // Batch maps (stored in build_completed_maps)
    pub batch: InstallBatch,

    // Hook data structures (indexed for efficient lookup)
    pub hooks_by_when: HashMap<HookWhen, Vec<Arc<Hook>>>,
    pub hooks_by_pkgkey: HashMap<String, Vec<Arc<Hook>>>,
    pub hooks_by_name: HashMap<String, Arc<Hook>>,

    /// Debian explicit trigger interests (non-file triggers)
    /// - deb_explicit_triggers_by_pkg: pkgkey -> trigger names this package is interested in
    /// - deb_explicit_triggers_by_name: trigger name -> pkgkeys interested in it
    pub deb_explicit_triggers_by_pkg: HashMap<String, Vec<String>>,
    pub deb_explicit_triggers_by_name: HashMap<String, Vec<String>>,

    /// Debian activate triggers (triggers that packages activate)
    /// - deb_activate_triggers_by_pkg: pkgkey -> trigger names this package activates
    /// - deb_activate_triggers_by_name: trigger name -> pkgkeys that activate it
    pub deb_activate_triggers_by_pkg: HashMap<String, Vec<String>>,
    pub deb_activate_triggers_by_name: HashMap<String, Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct InstallBatch {
    pub new_pkgkeys:    HashSet<String>,
    pub fresh_installs: HashSet<String>,
    pub upgrades_new:   HashSet<String>,
    pub upgrades_old:   HashSet<String>,
    pub old_removes:    HashSet<String>,
    pub is_first:       bool,
}

impl Default for InstallBatch {
    fn default() -> Self {
        Self {
            new_pkgkeys:    HashSet::new(),
            fresh_installs: HashSet::new(),
            upgrades_new:   HashSet::new(),
            upgrades_old:   HashSet::new(),
            old_removes:    HashSet::new(),
            is_first:       true,
        }
    }
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
            new_pkgs: InstalledPackagesMap::new(),
            fresh_installs: HashSet::new(),
            old_removes: HashSet::new(),
            upgrades_new: HashSet::new(),
            upgrades_old: HashSet::new(),
            upgrade_map_old_to_new: HashMap::new(),
            // Initialize with defaults - will be set properly in prepare_installation_plan()
            env_root: PathBuf::new(),
            store_root: PathBuf::new(),
            package_format: PackageFormat::default(),
            batch: InstallBatch::default(),
            hooks_by_when: HashMap::new(),
            hooks_by_pkgkey: HashMap::new(),
            hooks_by_name: HashMap::new(),
            installed: HashSet::new(),
            deb_explicit_triggers_by_pkg: HashMap::new(),
            deb_explicit_triggers_by_name: HashMap::new(),
            deb_activate_triggers_by_pkg: HashMap::new(),
            deb_activate_triggers_by_name: HashMap::new(),
        }
    }
}

/// Get InstalledPackageInfo for a new package key (from plan.new_pkgs)
/// Use this when you know the package is being installed/upgraded in this transaction
pub fn pkgkey2new_pkg_info(plan: &InstallationPlan, pkgkey: &str) -> Option<Arc<InstalledPackageInfo>> {
    plan.new_pkgs.get(pkgkey).map(|info| Arc::clone(info))
}

/// Get InstalledPackageInfo for an installed package key (from PACKAGE_CACHE.installed_packages)
/// Use this when you know the package is already installed (old packages, removals, etc.)
pub fn pkgkey2installed_pkg_info(pkgkey: &str) -> Option<Arc<InstalledPackageInfo>> {
    let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
    let result = installed.get(pkgkey).map(|i| Arc::clone(i));
    drop(installed);
    result
}

/// Get InstalledPackageInfo for a package key (fallback lookup)
/// First looks up in plan.new_pkgs, then falls back to PACKAGE_CACHE.installed_packages
/// Use this only when you don't know which source the package comes from
pub fn pkgkey2installinfo(plan: &InstallationPlan, pkgkey: &str) -> Option<Arc<InstalledPackageInfo>> {
    // First try plan.new_pkgs (new packages being installed)
    if let Some(info) = plan.new_pkgs.get(pkgkey) {
        return Some(Arc::clone(info));
    }
    // Fall back to installed packages
    pkgkey2installed_pkg_info(pkgkey)
}

/// Get pkgline for a package key
/// Uses pkgkey2installinfo() to find the package info
pub fn pkgkey2pkgline(plan: &InstallationPlan, pkgkey: &str) -> String {
    pkgkey2installinfo(plan, pkgkey)
        .map(|info| info.pkgline.clone())
        .unwrap_or_default()
}

/// Calculate operation flags for a package operation
/// Computes flags based on new_pkgkey and old_pkgkey
pub fn calculate_op_flags(
    plan: &InstallationPlan,
    new_pkgkey: Option<&str>,
    old_pkgkey: Option<&str>,
) -> u8 {
    let mut flags = 0u8;

    // SHOULD_EXPOSE: new package should be exposed if it has ebin_exposure
    if let Some(pkgkey) = new_pkgkey {
        if let Some(new_pkg_info) = pkgkey2new_pkg_info(plan, pkgkey) {
            if new_pkg_info.ebin_exposure {
                flags |= op_flags::SHOULD_EXPOSE;
            }
            if !new_pkg_info.pkgline.is_empty() {
                flags |= op_flags::IN_STORE;
            }
            if is_aur_package(pkgkey) {
                flags |= op_flags::IS_AUR;
            }
        }
    }

    // SHOULD_UNEXPOSE: old package should be unexposed if it was exposed
    if let Some(pkgkey) = old_pkgkey {
        if let Some(old_pkg_info) = pkgkey2installed_pkg_info(pkgkey) {
            if old_pkg_info.ebin_exposure {
                flags |= op_flags::SHOULD_UNEXPOSE;
            }
        }
    }

    flags
}

/// Create a PackageOperation with calculated flags
pub fn create_package_operation(
    plan: &InstallationPlan,
    new_pkgkey: Option<String>,
    old_pkgkey: Option<String>,
    op_type: OperationType,
) -> PackageOperation {
    let flags = calculate_op_flags(
        plan,
        new_pkgkey.as_deref(),
        old_pkgkey.as_deref(),
    );
    // Calculate depend_depth: use new_pkg.depend_depth if available, otherwise old_pkg.depend_depth
    let depend_depth = if let Some(ref pkgkey) = new_pkgkey {
        pkgkey2new_pkg_info(plan, pkgkey)
            .map(|info| info.depend_depth)
            .unwrap_or(0)
    } else if let Some(ref pkgkey) = old_pkgkey {
        pkgkey2installed_pkg_info(pkgkey)
            .map(|info| info.depend_depth)
            .unwrap_or(0)
    } else {
        0
    };
    PackageOperation {
        new_pkgkey,
        old_pkgkey,
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
        plan.new_pkgs.insert(session_pkgkey.clone(), Arc::clone(session_pkg_info));

        let (is_upgrade, old_pkgkey) = find_upgrade_target(
            session_pkgkey,
            installed,
        );
        if is_upgrade {
            plan.upgrades_new.insert(session_pkgkey.clone());
            plan.upgrades_old.insert(old_pkgkey.clone());
            plan.upgrade_map_old_to_new.insert(old_pkgkey.clone(), session_pkgkey.clone());
        } else {
            plan.fresh_installs.insert(session_pkgkey.clone());
        }
    }

    Ok(())
}


/// Build ordered operations from classified packages
fn build_ordered_operations(plan: &mut InstallationPlan) {
    let mut operations: Vec<PackageOperation> = Vec::new();

    // Add fresh installs
    for pkgkey in plan.fresh_installs.iter() {
        operations.push(create_package_operation(
            plan,
            Some(pkgkey.clone()),
            None,
            OperationType::FreshInstall,
        ));
    }

    // Add upgrades
    for old_pkgkey in plan.upgrades_old.iter() {
        if let Some(new_pkgkey) = plan.upgrade_map_old_to_new.get(old_pkgkey) {
            if plan.upgrades_new.contains(new_pkgkey) {
                operations.push(create_package_operation(
                    plan,
                    Some(new_pkgkey.clone()),
                    Some(old_pkgkey.clone()),
                    OperationType::Upgrade,
                ));
            }
        }
    }

    // Add pure removals
    for pkgkey in plan.old_removes.iter() {
        // Skip if this is part of an upgrade
        if plan.upgrades_old.contains(pkgkey) {
            continue;
        }

        operations.push(create_package_operation(
            plan,
            None,
            Some(pkgkey.clone()),
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
            if let Some(pkgkey) = &op.new_pkgkey {
                new_exposes.push(pkgkey.clone());
            }
        }
        if op.should_unexpose() {
            if let Some(old_pkgkey) = &op.old_pkgkey {
                del_exposes.push(old_pkgkey.clone());
            }
        }
    }

    GenerationCommand {
        timestamp:      String::new(),  // Will be set by caller
        action:         String::new(),  // Will be set by caller
        command_line:   String::new(),  // Will be set by caller
        fresh_installs: plan.fresh_installs.iter().cloned().collect(),
        upgrades_new:   plan.upgrades_new.iter().cloned().collect(),
        upgrades_old:   plan.upgrades_old.iter().cloned().collect(),
        old_removes:    plan.old_removes.iter().cloned().collect(),
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

    // Snapshot currently installed package keys for later use.
    let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
    plan.installed = installed.keys().cloned().collect();

    // Determine which packages should be removed
    if let Some(old_removes) = explicit_removes {
        plan.old_removes = old_removes.keys().cloned().collect();
    } else {
        let old_removes_map = find_orphaned_packages(&plan.upgrades_old, &plan.skipped_reinstalls)?;
        plan.old_removes = old_removes_map.keys().cloned().collect();
    }

    // Build ordered operations from classified packages
    build_ordered_operations(&mut plan);

    // Fill pkglines for packages that already exist in the store
    crate::store::fill_pkglines_in_plan(&mut plan)
        .with_context(|| "Failed to find existing packages in store")?;

    // Build trigger indices used by hooks/trigger mapping.
    crate::deb_triggers::load_initial_deb_triggers(&mut plan)?;

    // Load initial hooks (from installed packages and etc/pacman.d/hooks/)
    crate::hooks::load_initial_hooks(&mut plan)?;

    Ok(plan)
}

/// Find orphaned packages that should be removed
/// An orphaned package is one that has no remaining reverse dependencies
/// (i.e., no other installed package depends on it)
/// Packages with depend_depth=0 (user-requested packages) are never considered orphans
/// Essential packages are never considered orphans
fn find_orphaned_packages(
    upgrades_old: &HashSet<String>,
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
            !upgrades_old.contains(*pkgkey) &&
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
                    let is_old_upgrade = upgrades_old.contains(*rdep_pkgkey);

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
                if let Some(pkgkey) = &op.new_pkgkey {
                    fresh_installs.push(pkgkey.clone());
                }
            }
            OperationType::Upgrade => {
                if let (Some(new_pkgkey), Some(old_pkgkey)) = (&op.new_pkgkey, &op.old_pkgkey) {
                    upgrades.push((old_pkgkey.clone(), new_pkgkey.clone()));
                }
            }
            OperationType::Removal => {
                if let Some(pkgkey) = &op.old_pkgkey {
                    removals.push(pkgkey.clone());
                }
            }
        }
        if op.should_expose() {
            if let Some(pkgkey) = &op.new_pkgkey {
                exposes.push(pkgkey.clone());
            }
        }
        if op.should_unexpose() {
            if let Some(pkgkey) = &op.old_pkgkey {
                unexposes.push(pkgkey.clone());
            }
        }
    }

    if !fresh_installs.is_empty() {
        actions_planned = true;
        println!("Packages to be freshly installed:");
        let mut fresh_map = InstalledPackagesMap::new();
        for pkgkey in fresh_installs {
            if let Some(info) = pkgkey2new_pkg_info(plan, &pkgkey) {
                fresh_map.insert(pkgkey, info);
            }
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

/// Build new_pkgkeys from ordered_operations
/// Filters packages where is_aur() == false || in_store() == true
pub fn build_all_pkgs_from_operations(plan: &mut InstallationPlan) {
    plan.batch.new_pkgkeys.clear();
    for op in &plan.ordered_operations {
        if let Some(pkgkey) = &op.new_pkgkey {
            // Include if: not AUR OR in store
            if !op.is_aur() || op.in_store() {
                plan.batch.new_pkgkeys.insert(pkgkey.clone());
            }
        }
    }
}

/// Remove a package from the installed packages cache and update reverse dependencies.
///
/// This function:
/// 1. Removes the package from `PACKAGE_CACHE.installed_packages`
/// 2. Updates rdepends for all packages that depend on the removed package
///
/// # Arguments
/// * `pkgkey` - Package key to remove
/// * `pkg_info` - Package info containing the depends list
pub fn remove_package_from_cache(pkgkey: &str, pkg_info: &InstalledPackageInfo) {
    PACKAGE_CACHE.installed_packages.write().unwrap().remove(pkgkey);
    // Update rdepends
    for dep_on_key in &pkg_info.depends {
        let mut installed = PACKAGE_CACHE.installed_packages.write().unwrap();
        if let Some(dep_pkg_info_mut) = installed.get_mut(dep_on_key) {
            Arc::make_mut(dep_pkg_info_mut).rdepends.retain(|r| r != pkgkey);
        }
    }
}
