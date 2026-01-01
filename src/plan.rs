//! Installation plan module
//!
//! This module defines the InstallationPlan structure that tracks packages to be installed,
//! upgraded, removed, and exposed during package operations.

use std::collections::HashMap;
use color_eyre::Result;
use crate::models::{InstalledPackageInfo, InstalledPackagesMap, LinkType};
use crate::package;
use crate::models::PACKAGE_CACHE;
use crate::mmio;
use crate::aur::is_aur_package;

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct InstallationPlan {
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub fresh_installs: InstalledPackagesMap,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub upgrades_new: InstalledPackagesMap,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub upgrades_old: InstalledPackagesMap,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub upgrade_map_old_to_new: HashMap<String, String>, // old_pkgkey -> new_pkgkey
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub skipped_reinstalls: InstalledPackagesMap,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub old_removes: InstalledPackagesMap,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub new_exposes: InstalledPackagesMap,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub del_exposes: InstalledPackagesMap,
    #[serde(default)]
    pub link: LinkType,
    #[serde(default)]
    pub can_reflink: bool,
    #[serde(skip)]
    pub store_pkglines_by_pkgname: std::collections::HashMap<String, Vec<String>>,
}

impl<'de> serde::Deserialize<'de> for InstallationPlan {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        struct Helper {
            #[serde(default, deserialize_with = "deserialize_pkgkey_hashmap")]
            fresh_installs: InstalledPackagesMap,
            #[serde(default, deserialize_with = "deserialize_pkgkey_hashmap")]
            upgrades_new: InstalledPackagesMap,
            #[serde(default, deserialize_with = "deserialize_pkgkey_hashmap")]
            upgrades_old: InstalledPackagesMap,
            #[serde(default)]
            upgrade_map_old_to_new: HashMap<String, String>,
            #[serde(default, deserialize_with = "deserialize_pkgkey_hashmap")]
            skipped_reinstalls: InstalledPackagesMap,
            #[serde(default, deserialize_with = "deserialize_pkgkey_hashmap")]
            old_removes: InstalledPackagesMap,
            #[serde(default, deserialize_with = "deserialize_pkgkey_hashmap")]
            new_exposes: InstalledPackagesMap,
            #[serde(default, deserialize_with = "deserialize_pkgkey_hashmap")]
            del_exposes: InstalledPackagesMap,
            #[serde(default)]
            link: LinkType,
            #[serde(default)]
            can_reflink: bool,
        }

        fn deserialize_pkgkey_hashmap<'de, D>(
            deserializer: D,
        ) -> Result<InstalledPackagesMap, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            use serde::de::{MapAccess, Visitor};
            use std::fmt;

            struct PkgkeyHashMapVisitor;

            impl<'de> Visitor<'de> for PkgkeyHashMapVisitor {
                type Value = InstalledPackagesMap;

                fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                    formatter.write_str("a map of pkgkey to InstalledPackageInfo or null")
                }

                fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
                where
                    M: MapAccess<'de>,
                {
                    let mut result = HashMap::new();
                    while let Some(key) = map.next_key::<String>()? {
                        // Try to deserialize as Option<InstalledPackageInfo>
                        // null values will deserialize as None, which we convert to a default
                        let pkgkey = key.clone();
                        let info = match map.next_value::<Option<InstalledPackageInfo>>() {
                            Ok(Some(info)) => info,
                            Ok(None) => {
                                // null value - create minimal default (we only compare keys anyway)
                                InstalledPackageInfo {
                                    pkgline: format!("fake_hash__{}", pkgkey),
                                    arch: "x86_64".to_string(),
                                    depend_depth: 0,
                                    install_time: 1000000000,
                                    ebin_exposure: true,
                                    rdepends: Vec::new(),
                                    depends: Vec::new(),
                                    bdepends: Vec::new(),
                                    rbdepends: Vec::new(),
                                    ebin_links: Vec::new(),
                                    pending_triggers: Vec::new(),
                                    triggers_awaited: false,
                                    config_failed: false,
                                }
                            }
                            Err(_) => {
                                // Deserialization error - also create default
                                InstalledPackageInfo {
                                    pkgline: format!("fake_hash__{}", pkgkey),
                                    arch: "x86_64".to_string(),
                                    depend_depth: 0,
                                    install_time: 1000000000,
                                    ebin_exposure: true,
                                    rdepends: Vec::new(),
                                    depends: Vec::new(),
                                    bdepends: Vec::new(),
                                    rbdepends: Vec::new(),
                                    ebin_links: Vec::new(),
                                    pending_triggers: Vec::new(),
                                    triggers_awaited: false,
                                    config_failed: false,
                                }
                            }
                        };
                        result.insert(key, info);
                    }
                    Ok(result)
                }
            }

            deserializer.deserialize_map(PkgkeyHashMapVisitor)
        }

        let helper = Helper::deserialize(deserializer)?;
        Ok(InstallationPlan {
            fresh_installs: helper.fresh_installs,
            upgrades_new: helper.upgrades_new,
            upgrades_old: helper.upgrades_old,
            upgrade_map_old_to_new: helper.upgrade_map_old_to_new,
            skipped_reinstalls: helper.skipped_reinstalls,
            old_removes: helper.old_removes,
            new_exposes: helper.new_exposes,
            del_exposes: helper.del_exposes,
            link: helper.link,
            can_reflink: helper.can_reflink,
            store_pkglines_by_pkgname: std::collections::HashMap::new(),
        })
    }
}


pub fn prepare_installation_plan(
    all_packages_for_session: &InstalledPackagesMap,
) -> Result<InstallationPlan> {
    let mut plan = InstallationPlan::default();
    for (session_pkgkey, session_pkg_info) in all_packages_for_session {
        let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
        if installed.contains_key(session_pkgkey) {
            plan.skipped_reinstalls.insert(session_pkgkey.clone(), session_pkg_info.clone());
            continue;
        }

        let (is_upgrade, old_pkgkey) = find_upgrade_target(
            session_pkgkey,
            session_pkg_info,
            &installed,
        );
        if is_upgrade {
            plan.upgrades_new.insert(session_pkgkey.clone(), session_pkg_info.clone());
            plan.upgrades_old.insert(old_pkgkey.clone(), installed.get(&old_pkgkey).unwrap().clone());
            // Directly build deterministic old->new mapping for upgrades (used in UI and processing).
            // For AUR packages, find_upgrade_target() already applies AUR-aware matching logic
            // (matching by pkgname+version and allowing arch changes from "any" to actual arch).
            plan.upgrade_map_old_to_new.insert(old_pkgkey.clone(), session_pkgkey.clone());
        } else {
            plan.fresh_installs.insert(session_pkgkey.clone(), session_pkg_info.clone());
        }
    }

    // Find and add orphaned packages to removals
    add_orphans_to_removes(&mut plan)?;

    // Auto-populate expose plan based on installation/removal actions
    auto_populate_expose_plan(&mut plan);

    Ok(plan)
}

/// Recursively find orphaned packages and add them to plan.old_removes
/// An orphaned package is one that has no remaining reverse dependencies
/// (i.e., no other installed package depends on it)
/// Packages with depend_depth=0 (user-requested packages) are never considered orphans
/// Essential packages are never considered orphans
fn add_orphans_to_removes(
    plan: &mut InstallationPlan,
) -> Result<()> {
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
            !plan.skipped_reinstalls.contains_key(*pkgkey) &&
            !plan.upgrades_old.contains_key(*pkgkey) &&
            pkg_info.depend_depth > 0 &&  // Exclude user-requested packages (depend_depth=0)
            !is_essential(pkgkey)  // Exclude essential packages
        })
        .map(|(pkgkey, _)| pkgkey.clone())
        .collect();

    if possible_orphans.is_empty() {
        return Ok(());
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
                    let is_being_removed = plan.old_removes.contains_key(*rdep_pkgkey);
                    let is_old_upgrade = plan.upgrades_old.contains_key(*rdep_pkgkey);

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

        // Add orphans to plan.old_removes
        let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
        for orphan_pkgkey in &orphan_pkgkeys {
            if let Some(pkg_info) = installed.get(orphan_pkgkey) {
                plan.old_removes.insert(orphan_pkgkey.clone(), pkg_info.clone());
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

    Ok(())
}

/// Auto-add items to plan.del_exposes/new_exposes based on ebin_exposure status
/// This function automatically populates the expose fields based on the installation/removal plan
pub fn auto_populate_expose_plan(plan: &mut InstallationPlan) {
    // Track exposure changes for packages being removed
    for (pkgkey, pkg_info) in plan.old_removes.iter() {
        if pkg_info.ebin_exposure {
            // Package being removed was exposed - will be unexposed
            plan.del_exposes.insert(pkgkey.clone(), pkg_info.clone());
        }
    }

    // Track exposure changes for packages being upgraded (old versions)
    // Also ensure new versions inherit exposure from old versions
    for (old_pkgkey, old_pkg_info) in plan.upgrades_old.iter() {
        if old_pkg_info.ebin_exposure {
            // Old version being upgraded was exposed - will be unexposed
            plan.del_exposes.insert(old_pkgkey.clone(), old_pkg_info.clone());

            // Find the corresponding new version and ensure it's also exposed
            if let Some(new_pkgkey) = plan.upgrade_map_old_to_new.get(old_pkgkey) {
                if let Some(new_pkg_info) = plan.upgrades_new.get(new_pkgkey) {
                    // Found matching new version - ensure it's exposed
                    plan.new_exposes.insert(new_pkgkey.clone(), new_pkg_info.clone());
                }
            }
        }
    }

    // Track exposure changes for new packages being installed
    for (pkgkey, pkg_info) in plan.fresh_installs.iter() {
        if pkg_info.ebin_exposure {
            // New package being installed should be exposed
            plan.new_exposes.insert(pkgkey.clone(), pkg_info.clone());
        }
    }

    // Track exposure changes for packages being upgraded (new versions)
    // Note: This handles cases where new version has ebin_exposure set but old version didn't
    for (pkgkey, pkg_info) in plan.upgrades_new.iter() {
        if pkg_info.ebin_exposure {
            // New version being upgraded should be exposed
            plan.new_exposes.insert(pkgkey.clone(), pkg_info.clone());
        }
    }

    // Track additional exposure changes for skipped_reinstalls
    process_skipped_reinstalls_exposure(plan);
}

/// Track exposure changes for packages in skipped_reinstalls
/// This function handles cases where packages exist in both old and new states but have exposure changes.
/// IMPORTANT: For skipped reinstalls (same version), we preserve the existing exposure status.
/// We only change exposure if the user explicitly requested it (which would be handled elsewhere).
fn process_skipped_reinstalls_exposure(
    plan: &mut InstallationPlan,
) {
    // Collect keys first to avoid borrow checker issues
    let pkgkeys: Vec<String> = plan.skipped_reinstalls.keys().cloned().collect();
    for pkgkey in pkgkeys {
        // Extract values we need before any mutable borrows
        let (new_ebin_exposure, new_info_clone) = {
            let new_info = plan.skipped_reinstalls.get(&pkgkey).unwrap();
            (new_info.ebin_exposure, new_info.clone())
        };

        let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
        if let Some(old_info) = installed.get(&pkgkey) {
            // Package exists in both - check for exposure changes
            // For skipped reinstalls (same version), preserve existing exposure status
            // The new_ebin_exposure might be false due to resolution logic (e.g., user_request_world=None),
            // but we should preserve the installed package's exposure status
            if new_ebin_exposure != old_info.ebin_exposure {
                // Only change exposure if the new exposure is explicitly true (user requested)
                // If old was exposed and new is false, preserve the exposure (don't unexpose)
                // If old was not exposed and new is true, expose it (user requested)
                if new_ebin_exposure && !old_info.ebin_exposure {
                    // Package will be newly exposed (user requested)
                    plan.new_exposes.insert(pkgkey.clone(), new_info_clone);
                }
                // If old was exposed but new session says false, preserve exposure (don't unexpose)
                // This happens when user_request_world is None during full upgrade
            }
        }
    }
}

/// Determine if a package is an upgrade by comparing package names and architectures
/// Returns (is_upgrade, old_pkgkey) if it's an upgrade, (false, "") otherwise
///
/// For AUR packages, matches by pkgname+version only (ignoring arch).
/// For non-AUR packages, matches by pkgname+arch (version is compared separately if needed).
pub fn find_upgrade_target(
    new_pkgkey: &str,
    _new_pkg_info: &InstalledPackageInfo,
    old_packages: &InstalledPackagesMap,
) -> (bool, String) {
    let (new_pkgname, new_version, new_arch) = match package::parse_pkgkey(new_pkgkey) {
        Ok(parts) => parts,
        Err(_) => return (false, String::new()),
    };

    let is_aur = is_aur_package(new_pkgkey);

    for (old_pkgkey, _) in old_packages.iter() {
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

    if !plan.fresh_installs.is_empty() {
        actions_planned = true;
        println!("Packages to be freshly installed:");
        print_packages_by_depend_depth(&plan.fresh_installs);
    }

    if !plan.upgrades_new.is_empty() {
        actions_planned = true;
        println!("Packages to be upgraded:");
        for (old_pkgkey, _pkg_info) in plan.upgrades_old.iter() {
            let new_pkgkey_display = plan.upgrade_map_old_to_new
                .get(old_pkgkey)
                .map(|s| s.as_str())
                .unwrap_or("unknown new version");
            println!("- {} (replacing {})", new_pkgkey_display, old_pkgkey);
        }
    }

    if !plan.old_removes.is_empty() {
        actions_planned = true;
        println!("Packages to be removed:");
        for (pkgkey, _pkg_info) in plan.old_removes.iter() {
            println!("- {}", pkgkey);
        }
    }

    if !plan.new_exposes.is_empty() {
        actions_planned = true;
        println!("Packages to be exposed:");
        for (pkgkey, _pkg_info) in plan.new_exposes.iter() {
            println!("- {}", pkgkey);
        }
    }

    if !plan.del_exposes.is_empty() {
        actions_planned = true;
        println!("Packages to be unexposed:");
        for (pkgkey, _pkg_info) in plan.del_exposes.iter() {
            println!("- {}", pkgkey);
        }
    }

    actions_planned
}

fn print_packages_by_depend_depth(packages: &InstalledPackagesMap) {
    // Convert HashMap to a Vec of tuples (pkgkey, info)
    let mut packages_vec: Vec<(&String, &InstalledPackageInfo)> = packages.iter().map(|(k, v)| (k, v)).collect();

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
fn print_download_requirements(plan: &InstallationPlan) -> Result<()> {
    // Sum sizes for downloads
    let mut total_download: u64 = 0;
    let mut total_install: u64 = 0;
    for pkgkey in plan.fresh_installs.keys().chain(plan.upgrades_new.keys()) {
        if let Ok(pkginfo) = crate::package_cache::load_package_info(pkgkey) {
            total_download += pkginfo.size as u64;
            total_install += pkginfo.installed_size as u64;
        }
    }

    if total_download > 0 {
        println!(
            "Need to get {} archives.",
            crate::utils::format_size(total_download)
        );
        println!(
            "After this operation, {} of additional disk space will be used.",
            crate::utils::format_size(total_install)
        );
    }

    Ok(())
}
