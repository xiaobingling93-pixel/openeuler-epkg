use std::fs;
use std::path::{Path, PathBuf};
use std::collections::{HashMap, HashSet};
use color_eyre::Result;
use crate::models::{InstalledPackageInfo, InstalledPackagesMap, PackageFormat};
use crate::utils::get_package_files;
use crate::package::pkgkey2pkgname;
use crate::parse_provides::parse_provides;
use crate::scriptlets::{get_interpreters_for_script, ScriptletType};
use crate::deb_triggers::setup_deb_env_vars;

/// Collect files from packages using get_package_files
/// Converts relative paths to strings and collects them into the appropriate collection type
fn collect_package_files_to_vec<'a>(
    store_root: &Path,
    packages: impl Iterator<Item = &'a InstalledPackageInfo>,
) -> Vec<String> {
    let mut files = Vec::new();
    for pkg_info in packages {
        if let Ok(rel_files) = get_package_files(store_root, pkg_info) {
            files.extend(rel_files);
        }
    }
    files
}

/// Collect files from packages with pkgkey into a HashMap
fn collect_package_files_to_map<'a>(
    store_root: &Path,
    packages: impl Iterator<Item = (&'a String, &'a InstalledPackageInfo)>,
) -> HashMap<String, Vec<String>> {
    let mut result = HashMap::new();
    for (pkgkey, pkg_info) in packages {
        if let Ok(rel_files) = get_package_files(store_root, pkg_info) {
            if !rel_files.is_empty() {
                result.insert(pkgkey.clone(), rel_files);
            }
        }
    }
    result
}

/// Collect files from packages into a HashSet
fn collect_package_files_to_set<'a>(
    store_root: &Path,
    packages: impl Iterator<Item = &'a InstalledPackageInfo>,
) -> HashSet<String> {
    let mut files = HashSet::new();
    for pkg_info in packages {
        if let Ok(rel_files) = get_package_files(store_root, pkg_info) {
            files.extend(rel_files);
        }
    }
    files
}

/// Match files against trigger paths using prefix matching
/// Returns a vector of matching file paths
fn match_files_against_trigger_paths(
    files: &[String],
    trigger_paths: &[String],
) -> Vec<String> {
    let mut matching_files = Vec::new();
    for file in files {
        // File triggers use prefix matching, not glob patterns
        for trigger_path in trigger_paths {
            // RPM file triggers match paths that start with the trigger prefix
            // Normalize both paths for comparison
            let file_normalized = if file.starts_with('/') {
                file.as_str()
            } else {
                // Prepend / to match absolute trigger paths
                let normalized = format!("/{}", file);
                if normalized.starts_with(trigger_path) {
                    matching_files.push(file.clone());
                    break;
                }
                continue;
            };

            let trigger_normalized = if trigger_path.starts_with('/') {
                trigger_path.as_str()
            } else {
                // Try matching relative trigger path
                if file.starts_with(trigger_path) {
                    matching_files.push(file.clone());
                    break;
                }
                continue;
            };

            if file_normalized.starts_with(trigger_normalized) {
                matching_files.push(file.clone());
                break; // File matches at least one trigger path
            }
        }
    }
    matching_files
}

/// Match files against trigger paths using prefix matching with priorities
/// Returns vectors of matching file paths and their corresponding priorities
fn match_files_against_trigger_paths_with_priorities(
    files: &[String],
    trigger_paths: &[String],
    priorities: &[u32],
    default_priority: u32,
) -> (Vec<String>, Vec<u32>) {
    let mut matching_files = Vec::new();
    let mut trigger_priorities = Vec::new();
    for file in files {
        // File triggers use prefix matching, not glob patterns
        for (idx, trigger_path) in trigger_paths.iter().enumerate() {
            // RPM file triggers match paths that start with the trigger prefix
            // Normalize both paths for comparison
            let file_normalized = if file.starts_with('/') {
                file.as_str()
            } else {
                // Prepend / to match absolute trigger paths
                let normalized = format!("/{}", file);
                if normalized.starts_with(trigger_path) {
                    matching_files.push(file.clone());
                    trigger_priorities.push(priorities.get(idx).copied().unwrap_or(default_priority));
                    break;
                }
                continue;
            };

            let trigger_normalized = if trigger_path.starts_with('/') {
                trigger_path.as_str()
            } else {
                // Try matching relative trigger path
                if file.starts_with(trigger_path) {
                    matching_files.push(file.clone());
                    trigger_priorities.push(priorities.get(idx).copied().unwrap_or(default_priority));
                    break;
                }
                continue;
            };

            if file_normalized.starts_with(trigger_normalized) {
                matching_files.push(file.clone());
                trigger_priorities.push(priorities.get(idx).copied().unwrap_or(default_priority));
                break; // File matches at least one trigger path
            }
        }
    }
    (matching_files, trigger_priorities)
}

/// Check if any file matches against trigger paths using prefix matching
/// Returns true if at least one file matches
fn has_matching_files_against_trigger_paths(
    files: &[String],
    trigger_paths: &[String],
) -> bool {
    for file in files {
        for trigger_path in trigger_paths {
            // RPM file triggers match paths that start with the trigger prefix
            // Normalize both paths for comparison
            let file_normalized = if file.starts_with('/') {
                file.as_str()
            } else {
                let normalized = format!("/{}", file);
                if normalized.starts_with(trigger_path) {
                    return true;
                }
                continue;
            };

            let trigger_normalized = if trigger_path.starts_with('/') {
                trigger_path.as_str()
            } else {
                if file.starts_with(trigger_path) {
                    return true;
                }
                continue;
            };

            if file_normalized.starts_with(trigger_normalized) {
                return true;
            }
        }
    }
    false
}

/// Find trigger script path (.sh or .lua) for a given trigger type
/// Returns the path to the script if it exists, None otherwise
fn find_trigger_script_path(install_dir: &Path, trigger_type: &str) -> Option<PathBuf> {
    let trigger_script = install_dir.join(format!("{}.sh", trigger_type));
    let trigger_lua = install_dir.join(format!("{}.lua", trigger_type));

    if trigger_script.exists() {
        Some(trigger_script)
    } else if trigger_lua.exists() {
        Some(trigger_lua)
    } else {
        None
    }
}

/// Read trigger paths from metadata file
/// Returns a vector of trigger paths, or None if the file doesn't exist or is empty
fn read_trigger_paths(trigger_metadata: &Path) -> Option<Vec<String>> {
    if !trigger_metadata.exists() {
        return None;
    }

    if let Ok(metadata_content) = fs::read_to_string(trigger_metadata) {
        let trigger_paths: Vec<String> = metadata_content.lines().map(|s| s.trim().to_string()).collect();
        if trigger_paths.is_empty() {
            None
        } else {
            Some(trigger_paths)
        }
    } else {
        None
    }
}

/// Calculate arg1 (number of installed instances of triggered package) for trigger scriptlets
fn calculate_trigger_arg1(
    pkgkey: &str,
    installed_packages: &InstalledPackagesMap,
    fresh_installs: &InstalledPackagesMap,
    old_removes: &InstalledPackagesMap,
) -> u32 {
    let triggered_pkgname = pkgkey2pkgname(pkgkey).unwrap_or_default();
    count_installed_packages_by_name(
        &triggered_pkgname,
        installed_packages,
        fresh_installs,
        old_removes,
    )
}

/// Set up environment variables for RPM package scriptlets
/// Sets RPM_INSTALL_PREFIX and RPM_INSTALL_PREFIX_N for relocatable packages
pub fn setup_rpm_env_vars(
    env_vars: &mut std::collections::HashMap<String, String>,
    _pkgkey: &str,
    package_info: &InstalledPackageInfo,
    store_root: &Path,
) {
    // Try to read install prefixes from package metadata
    // RPM stores install prefixes in RPMTAG_INSTPREFIXES
    // For now, we'll try to read from a metadata file if it exists
    let install_dir = store_root.join(&package_info.pkgline).join("info/install");
    let prefixes_file = install_dir.join("install_prefixes.txt");

    if let Ok(prefixes_content) = std::fs::read_to_string(&prefixes_file) {
        let prefixes: Vec<&str> = prefixes_content.lines().filter(|s| !s.trim().is_empty()).collect();
        for (index, prefix) in prefixes.iter().enumerate() {
            let var_name = if index == 0 {
                "RPM_INSTALL_PREFIX".to_string()
            } else {
                format!("RPM_INSTALL_PREFIX_{}", index)
            };
            env_vars.insert(var_name, prefix.to_string());
        }
        // Also set RPM_INSTALL_PREFIX0 for compatibility
        if !prefixes.is_empty() {
            env_vars.insert("RPM_INSTALL_PREFIX0".to_string(), prefixes[0].to_string());
        }
    }
    // If no prefixes file exists, that's okay - package is not relocatable
}


/// Find triggers that match triggering packages
/// Returns: (pkgkey, pkg_info, script_path, trigger_index, triggering_pkgname, triggering_pkgkey)
fn find_matching_triggers(
    trigger_type: &str,
    candidate_packages: &InstalledPackagesMap,
    triggering_packages: &HashMap<String, (String, String)>, // name -> (version, pkgkey)
    all_packages: &InstalledPackagesMap, // All packages for provides checking
    store_root: &Path,
) -> Vec<(String, InstalledPackageInfo, PathBuf, usize, String, String)> {
    use crate::parse_requires::parse_requires;
    use crate::version_constraint::check_version_constraint;

    let mut triggered_packages = Vec::new();

    for (pkgkey, pkg_info) in candidate_packages.iter() {
        let install_dir = store_root.join(&pkg_info.pkgline).join("info/install");
        let trigger_script = install_dir.join(format!("{}.sh", trigger_type));
        let trigger_lua = install_dir.join(format!("{}.lua", trigger_type));
        let trigger_metadata = install_dir.join(format!("{}.triggers", trigger_type));

        // Check if trigger scriptlet exists
        let script_path = if trigger_script.exists() {
            Some(trigger_script)
        } else if trigger_lua.exists() {
            Some(trigger_lua)
        } else {
            None
        };

        if let Some(script_path) = script_path {
            // Read trigger metadata (package names/conditions that trigger this)
            if trigger_metadata.exists() {
                if let Ok(metadata_content) = fs::read_to_string(&trigger_metadata) {
                    let trigger_conditions: Vec<String> = metadata_content.lines().map(|s| s.to_string()).collect();

                    // Check each trigger condition (indexed by trigger index)
                    for (trigger_index, condition) in trigger_conditions.iter().enumerate() {
                        // Parse condition: could be "name" or "name version" or "name op version"
                        let parts: Vec<&str> = condition.split_whitespace().collect();
                        if parts.is_empty() {
                            continue;
                        }
                        let trigger_name = parts[0];

                        // Check if any triggering package matches (name or provides)
                        for (triggering_name, (triggering_version, triggering_pkgkey)) in triggering_packages.iter() {
                            // Check if triggering package name matches
                            let name_matches = triggering_name == trigger_name;

                            // Also check if triggering package provides the capability
                            // Load triggering package info to check provides
                            let provides_matches = if let Some(triggering_pkg_info) =
                                all_packages.get(triggering_pkgkey) {
                                package_provides_capability(
                                    triggering_pkgkey,
                                    triggering_pkg_info,
                                    store_root,
                                    trigger_name,
                                )
                            } else {
                                false
                            };

                            if !name_matches && !provides_matches {
                                continue;
                            }

                            // If condition has version info, check it
                            let version_matches = if parts.len() >= 3 {
                                // Format: "name op version" or "name version"
                                let op_str = if parts.len() >= 3 && ["<", ">", "=", "<=", ">="].contains(&parts[1]) {
                                    parts[1]
                                } else {
                                    // No operator, just version - treat as exact match
                                    "="
                                };
                                let cond_version = if parts.len() >= 3 && ["<", ">", "=", "<=", ">="].contains(&parts[1]) {
                                    parts[2..].join(" ")
                                } else if parts.len() >= 2 {
                                    parts[1..].join(" ")
                                } else {
                                    String::new()
                                };

                                if !cond_version.is_empty() {
                                    // Parse as dependency constraint
                                    let constraint_str = format!("{} {} {}", trigger_name, op_str, cond_version);
                                    if let Ok(constraints) = parse_requires(crate::models::PackageFormat::Rpm, &constraint_str) {
                                        // Check if triggering version satisfies constraint
                                        let mut matches = false;
                                        for or_group in &constraints {
                                            for dep in or_group {
                                                if dep.capability == trigger_name {
                                                    for vc in &dep.constraints {
                                                        if let Ok(satisfies) = check_version_constraint(
                                                            triggering_version,
                                                            vc,
                                                            crate::models::PackageFormat::Rpm,
                                                        ) {
                                                            if satisfies {
                                                                matches = true;
                                                                break;
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        matches
                                    } else {
                                        true // If parsing fails, assume it matches (fallback)
                                    }
                                } else {
                                    true // No version constraint, just name match
                                }
                            } else {
                                true // No version in condition, just name match
                            };

                            if version_matches {
                                triggered_packages.push((
                                    pkgkey.clone(),
                                    pkg_info.clone(),
                                    script_path.clone(),
                                    trigger_index,
                                    triggering_name.clone(),
                                    triggering_pkgkey.clone(),
                                ));
                                break; // Only trigger once per condition
                            }
                        }
                    }
                }
            }
        }
    }

    triggered_packages
}

/// Check if a package provides a given capability (name or provides)
/// This matches RPM's rpmdsAnyMatchesDep behavior
fn package_provides_capability(
    pkgkey: &str,
    _pkg_info: &InstalledPackageInfo,
    _store_root: &Path,
    capability: &str,
) -> bool {
    // First check if package name matches
    if let Ok(pkgname) = pkgkey2pkgname(pkgkey) {
        if pkgname == capability {
            return true;
        }
    }

    // Then check if package provides the capability
    // Load package using map_pkgkey2package
    if let Ok(package) = crate::mmio::map_pkgkey2package(pkgkey) {
        // Check all provides entries
        for provide_str in &package.provides {
            let provide_map = parse_provides(provide_str, PackageFormat::Rpm);
            // Check if any provide matches the capability (with or without version)
            for (provide_name, _version) in provide_map {
                // Strip version from capability if present for comparison
                let cap_base = if let Some(eq_pos) = capability.find('=') {
                    &capability[..eq_pos].trim_end()
                } else {
                    capability
                };

                if provide_name == cap_base || provide_name == capability {
                    return true;
                }
            }
        }
    }

    false
}

/// Count installed instances of a package by name
pub fn count_installed_packages_by_name(
    pkgname: &str,
    installed_packages: &InstalledPackagesMap,
    fresh_installs: &InstalledPackagesMap,
    old_removes: &InstalledPackagesMap,
) -> u32 {
    let mut count = 0u32;

    // Count from installed packages
    for (pkgkey, _) in installed_packages.iter() {
        if let Ok(name) = pkgkey2pkgname(pkgkey) {
            if name == pkgname {
                count += 1;
            }
        }
    }

    // Add fresh installs (will be installed)
    for (pkgkey, _) in fresh_installs.iter() {
        if let Ok(name) = pkgkey2pkgname(pkgkey) {
            if name == pkgname {
                count += 1;
            }
        }
    }

    // Subtract old removes (will be removed)
    for (pkgkey, _) in old_removes.iter() {
        if let Ok(name) = pkgkey2pkgname(pkgkey) {
            if name == pkgname {
                count = count.saturating_sub(1);
            }
        }
    }

    count
}

/// Pre-compute reusable data structures for RPM package triggers
/// This can be called once and reused across multiple trigger type calls
pub fn prepare_rpm_trigger_data(
    installed_packages: &InstalledPackagesMap,
    fresh_installs: &InstalledPackagesMap,
    upgrades_new: &InstalledPackagesMap,
    old_removes: &InstalledPackagesMap,
) -> (
    HashMap<String, (String, String)>, // triggering_packages: name -> (version, pkgkey)
    InstalledPackagesMap, // all_packages: merged map for provides checking
) {
    // Collect all package names and versions that are being installed/upgraded/removed
    let mut triggering_packages: HashMap<String, (String, String)> = HashMap::new(); // name -> (version, pkgkey)
    for (pkgkey, _) in fresh_installs.iter() {
        if let (Ok(pkgname), Ok(version)) = (pkgkey2pkgname(pkgkey), crate::package::pkgkey2version(pkgkey)) {
            triggering_packages.insert(pkgname, (version, pkgkey.clone()));
        }
    }
    for (pkgkey, _) in upgrades_new.iter() {
        if let (Ok(pkgname), Ok(version)) = (pkgkey2pkgname(pkgkey), crate::package::pkgkey2version(pkgkey)) {
            triggering_packages.insert(pkgname, (version, pkgkey.clone()));
        }
    }
    for (pkgkey, _) in old_removes.iter() {
        if let (Ok(pkgname), Ok(version)) = (pkgkey2pkgname(pkgkey), crate::package::pkgkey2version(pkgkey)) {
            triggering_packages.insert(pkgname, (version, pkgkey.clone()));
        }
    }

    // Build all_packages map for provides checking
    let mut all_packages: InstalledPackagesMap = installed_packages.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    for (k, v) in fresh_installs.iter() {
        all_packages.insert(k.clone(), v.clone());
    }
    for (k, v) in upgrades_new.iter() {
        all_packages.insert(k.clone(), v.clone());
    }
    for (k, v) in old_removes.iter() {
        all_packages.insert(k.clone(), v.clone());
    }

    (triggering_packages, all_packages)
}

/// Execute RPM package triggers for packages being installed/upgraded/removed
/// Reference: https://rpm-software-management.github.io/rpm/man/rpm-scriptlets.7
///
/// This function now separates triggers into two phases to match RPM behavior:
/// 1. Triggers in OTHER packages (runTriggers equivalent)
/// 2. Triggers in THIS package (runImmedTriggers equivalent)
///
/// For better performance when calling multiple times with the same parameters,
/// use `run_rpm_package_triggers_with_data` and pre-compute the data once.
pub fn run_rpm_package_triggers(
    trigger_type: &str, // "triggerprein", "triggerin", "triggerun", "triggerpostun"
    installed_packages: &InstalledPackagesMap,
    fresh_installs: &InstalledPackagesMap,
    upgrades_new: &InstalledPackagesMap,
    old_removes: &InstalledPackagesMap,
    store_root: &Path,
    env_root: &Path,
) -> Result<()> {
    // Pre-compute reusable data structures
    let (triggering_packages, all_packages) = prepare_rpm_trigger_data(
        installed_packages,
        fresh_installs,
        upgrades_new,
        old_removes,
    );

    run_rpm_package_triggers_with_data(
        trigger_type,
        installed_packages,
        fresh_installs,
        upgrades_new,
        old_removes,
        &triggering_packages,
        &all_packages,
        store_root,
        env_root,
    )
}

/// Execute RPM package triggers with pre-computed data structures.
/// Use this when calling multiple times with the same parameters to avoid recomputing.
pub fn run_rpm_package_triggers_with_data(
    trigger_type: &str, // "triggerprein", "triggerin", "triggerun", "triggerpostun"
    installed_packages: &InstalledPackagesMap,
    fresh_installs: &InstalledPackagesMap,
    upgrades_new: &InstalledPackagesMap,
    old_removes: &InstalledPackagesMap,
    triggering_packages: &HashMap<String, (String, String)>, // Pre-computed: name -> (version, pkgkey)
    all_packages: &InstalledPackagesMap,    // Pre-computed: merged map for provides checking
    store_root: &Path,
    env_root: &Path,
) -> Result<()> {
    // Determine execution order based on trigger type and RPM behavior
    // For install: other packages first, then this package
    // For erase: this package first (triggerun), or only other packages (triggerpostun)
    let execute_other_first = match trigger_type {
        "triggerprein" | "triggerin" => true,   // Install: other packages first
        "triggerun" => false,                   // Erase: this package first
        "triggerpostun" => true,                // Erase: only other packages (this package already removed)
        _ => true,
    };

    // Phase 1: Triggers in OTHER packages (equivalent to RPM's runTriggers)
    // These are packages that are NOT being installed/removed in this transaction
    // Build set of transaction package keys for efficient lookup
    let transaction_pkgkeys: HashSet<&String> = fresh_installs.keys()
        .chain(upgrades_new.keys())
        .chain(old_removes.keys())
        .collect();

    // Build other_packages more efficiently by filtering instead of cloning and removing
    let other_packages: InstalledPackagesMap = installed_packages.iter()
        .filter(|(k, _)| !transaction_pkgkeys.contains(k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    let mut other_triggers = find_matching_triggers(
        trigger_type,
        &other_packages,
        &triggering_packages,
        &all_packages,
        store_root,
    );

    // Phase 2: Triggers in THIS package (equivalent to RPM's runImmedTriggers)
    // These are packages that ARE being installed/removed in this transaction
    let mut this_packages = HashMap::new();
    for (pkgkey, pkg_info) in fresh_installs.iter() {
        this_packages.insert(pkgkey.clone(), pkg_info.clone());
    }
    for (pkgkey, pkg_info) in upgrades_new.iter() {
        if !this_packages.contains_key(pkgkey) {
            this_packages.insert(pkgkey.clone(), pkg_info.clone());
        }
    }
    // For triggerun/triggerpostun, include packages being removed
    if trigger_type == "triggerun" || trigger_type == "triggerpostun" {
        for (pkgkey, pkg_info) in old_removes.iter() {
            this_packages.insert(pkgkey.clone(), pkg_info.clone());
        }
    }

    let mut this_triggers = find_matching_triggers(
        trigger_type,
        &this_packages,
        &triggering_packages,
        &all_packages,
        store_root,
    );

    // Track executed trigger indices to avoid duplicates (RPM behavior)
    let mut executed_indices: HashSet<(String, usize)> = HashSet::new();

    // Execute triggers in the correct order
    let mut all_triggers = if execute_other_first {
        // Other packages first, then this package
        other_triggers.append(&mut this_triggers);
        other_triggers
    } else {
        // This package first, then other packages
        this_triggers.append(&mut other_triggers);
        this_triggers
    };

    // Sort by dependency depth (higher depth first)
    all_triggers.sort_by(|a, b| b.1.depend_depth.cmp(&a.1.depend_depth));

    // Execute triggers
    for (pkgkey, pkg_info, script_path, trigger_index, _triggering_name, triggering_pkgkey) in all_triggers {
        // Skip if this trigger index has already been executed for this package
        if executed_indices.contains(&(pkgkey.clone(), trigger_index)) {
            continue;
        }
        executed_indices.insert((pkgkey.clone(), trigger_index));

        log::info!("Running RPM {} trigger for package {} (trigger index {})", trigger_type, pkgkey, trigger_index);

        // Calculate $1: number of instances of triggered package (the one containing the trigger) after operation
        // Use actual count, not just 0 or 1
        let triggered_pkgname = pkgkey2pkgname(&pkgkey).unwrap_or_default();
        let mut arg1 = count_installed_packages_by_name(
            &triggered_pkgname,
            installed_packages,
            fresh_installs,
            old_removes,
        ) as i32;

        // Apply count correction based on operation
        let count_correction = match trigger_type {
            "triggerprein" | "triggerin" => {
                // For install: if package is being installed, count is already correct (no correction needed)
                0
            }
            "triggerun" | "triggerpostun" => {
                // For erase: if package is being removed, decrement count
                if old_removes.contains_key(&pkgkey) {
                    -1
                } else {
                    0
                }
            }
            _ => 0,
        };
        arg1 += count_correction;
        let arg1 = arg1.max(0) as u32; // Ensure non-negative

        // Calculate $2: number of instances of triggering package (the one that set off the trigger) after operation
        let triggering_pkgname = pkgkey2pkgname(&triggering_pkgkey).unwrap_or_default();
        let arg2 = match trigger_type {
            "triggerprein" | "triggerin" => {
                // Trigger runs when triggering package is being installed/upgraded
                count_installed_packages_by_name(
                    &triggering_pkgname,
                    installed_packages,
                    fresh_installs,
                    old_removes,
                )
            }
            "triggerun" | "triggerpostun" => {
                // Trigger runs when triggering package is being removed
                // Count should be 0 after removal
                0
            }
            _ => 1, // Default
        };

        execute_trigger_scriptlet(&script_path, &pkgkey, &pkg_info, store_root, env_root, PackageFormat::Rpm, arg1, arg2)?;
    }

    Ok(())
}

/// Execute RPM file triggers based on installed files
/// Reference: https://rpm-software-management.github.io/rpm/man/rpm-scriptlets.7
/// File triggers execute once per triggering package and receive matching paths via stdin
///
/// Priority classes:
/// - priority_class = 1: High priority triggers (>= 10000) - executed before postin/preun
/// - priority_class = 2: Low priority triggers (< 10000) - executed after postin/preun
/// - priority_class = 0: All triggers (default, for backward compatibility)
pub fn run_rpm_file_triggers(
    trigger_type: &str, // "filetriggerin", "filetriggerun", "filetriggerpostun"
    installed_packages: &InstalledPackagesMap,
    fresh_installs: &InstalledPackagesMap,
    upgrades_new: &InstalledPackagesMap,
    old_removes: &InstalledPackagesMap,
    store_root: &Path,
    env_root: &Path,
    priority_class: u32, // 0 = all, 1 = high (>= 10000), 2 = low (< 10000)
) -> Result<()> {
    const TRIGGER_PRIORITY_BOUND: u32 = 10000;
    const DEFAULT_PRIORITY: u32 = 1000000;

    // Collect files per triggering package (the package that contains the files)
    // For filetriggerin: files from packages being installed/upgraded
    // For filetriggerun/filetriggerpostun: files from packages being removed
    let triggering_packages_files = match trigger_type {
        "filetriggerin" => {
            // Files from packages being installed/upgraded
            collect_package_files_to_map(
                store_root,
                fresh_installs.iter().map(|(k, v)| (k, v)).chain(upgrades_new.iter().map(|(k, v)| (k, v))),
            )
        }
        "filetriggerun" | "filetriggerpostun" => {
            // Files from packages being removed
            collect_package_files_to_map(
                store_root,
                old_removes.iter().map(|(k, v)| (k, v)),
            )
        }
        _ => return Ok(()),
    };

    // Find packages with file triggers (triggered packages) and match files
    // Package file triggers execute once per triggering package
    let mut triggered_packages: Vec<(String, InstalledPackageInfo, PathBuf, String, Vec<String>, u32)> = Vec::new();
    // Structure: (triggered_pkgkey, triggered_pkg_info, script_path, triggering_pkgkey, matching_files, priority)

    // Iterate over installed packages
    for (triggered_pkgkey, triggered_pkg_info) in installed_packages.iter().chain(fresh_installs.iter()) {
        let install_dir = store_root.join(&triggered_pkg_info.pkgline).join("info/install");
        let trigger_metadata = install_dir.join(format!("{}.triggers", trigger_type));

        let script_path = find_trigger_script_path(&install_dir, trigger_type);

        if let Some(script_path) = script_path {
            if let Some(trigger_paths) = read_trigger_paths(&trigger_metadata) {
                // Load priorities for this trigger type
                let priorities_path = install_dir.join(format!("{}.priorities", trigger_type));
                let priorities: Vec<u32> = if priorities_path.exists() {
                    if let Ok(priorities_content) = fs::read_to_string(&priorities_path) {
                        priorities_content.lines()
                            .map(|s| s.trim().parse::<u32>().unwrap_or(DEFAULT_PRIORITY))
                            .collect()
                    } else {
                        vec![DEFAULT_PRIORITY; trigger_paths.len()]
                    }
                } else {
                    vec![DEFAULT_PRIORITY; trigger_paths.len()]
                };

                // Check each triggering package's files against trigger paths
                for (triggering_pkgkey, triggering_files) in &triggering_packages_files {
                    let (matching_files, trigger_priorities) = match_files_against_trigger_paths_with_priorities(
                        triggering_files,
                        &trigger_paths,
                        &priorities,
                        DEFAULT_PRIORITY,
                    );

                    if !matching_files.is_empty() {
                        // Special case for filetriggerpostun: skip if the trigger package is being removed
                        // RPM behavior: filetriggerpostun is NOT executed when the package containing
                        // the trigger is removed (per rpm-scriptlets.7.scd)
                        if trigger_type == "filetriggerpostun" {
                            // Check if the triggered package (the one with the trigger) is being removed
                            if old_removes.contains_key(triggered_pkgkey) {
                                log::debug!("Skipping filetriggerpostun for package {} (package is being removed)", triggered_pkgkey);
                                continue;
                            }
                        }

                        // Determine priority for this trigger instance (use highest priority of matching paths)
                        let trigger_priority = trigger_priorities.iter().max().copied().unwrap_or(DEFAULT_PRIORITY);

                        // Filter by priority class if specified
                        let matches_priority_class = match priority_class {
                            0 => true, // All triggers
                            1 => trigger_priority >= TRIGGER_PRIORITY_BOUND, // High priority
                            2 => trigger_priority < TRIGGER_PRIORITY_BOUND,  // Low priority
                            _ => true,
                        };

                        if matches_priority_class {
                            triggered_packages.push((
                                triggered_pkgkey.clone(),
                                triggered_pkg_info.clone(),
                                script_path.clone(),
                                triggering_pkgkey.clone(),
                                matching_files,
                                trigger_priority,
                            ));
                        }
                    }
                }
            }
        }
    }

    // Execute triggers sorted by priority (descending), then dependency depth, then triggering package
    triggered_packages.sort_by(|a, b| {
        // Sort by priority (descending - higher priority first)
        b.5.cmp(&a.5)
            // Then by triggered package dependency depth (deeper first)
            .then_with(|| b.1.depend_depth.cmp(&a.1.depend_depth))
            // Then by triggering package key for consistency
            .then_with(|| a.3.cmp(&b.3))
    });

    for (triggered_pkgkey, triggered_pkg_info, script_path, triggering_pkgkey, matching_files, _priority) in triggered_packages {
        log::info!("Running RPM {} trigger for package {} (triggered by package {})",
                   trigger_type, triggered_pkgkey, triggering_pkgkey);

        // Calculate $1: number of installed instances of triggered package after operation
        let arg1 = calculate_trigger_arg1(
            &triggered_pkgkey,
            installed_packages,
            fresh_installs,
            old_removes,
        );

        // Calculate $2: number of installed instances of triggering package after operation
        let triggering_pkgname = pkgkey2pkgname(triggering_pkgkey.as_str()).unwrap_or_default();
        let arg2 = count_installed_packages_by_name(
            &triggering_pkgname,
            installed_packages,
            fresh_installs,
            old_removes,
        );

        // Pass matching files via stdin (one per line)
        let stdin_data = Some(matching_files.join("\n").into_bytes());

        execute_file_trigger_scriptlet(
            &script_path,
            &triggered_pkgkey,
            &triggered_pkg_info,
            store_root,
            env_root,
            PackageFormat::Rpm,
            arg1,
            arg2,
            stdin_data,
        )?;
    }

    Ok(())
}

/// Execute RPM transaction file triggers (transaction-level file triggers)
/// Reference: https://rpm-software-management.github.io/rpm/man/rpm-scriptlets.7
/// Transaction file triggers execute once per transaction, not per package
///
/// Note: For transfiletriggerpostun, matching is done against INSTALLED files from packages
/// being removed (not removed files). This matches RPM's behavior where it checks
/// RPMFILE_IS_INSTALLED during removal phase preparation.
pub fn run_rpm_transaction_file_triggers(
    trigger_type: &str, // "transfiletriggerin", "transfiletriggerun", "transfiletriggerpostun"
    installed_packages: &InstalledPackagesMap,
    fresh_installs: &InstalledPackagesMap,
    upgrades_new: &InstalledPackagesMap,
    old_removes: &InstalledPackagesMap,
    store_root: &Path,
    env_root: &Path,
) -> Result<()> {
    // For transfiletriggerin: collect all matching installed files (from transaction or previously installed)
    // For transfiletriggerun: collect all matching removed files
    // For transfiletriggerpostun: match against INSTALLED files from packages being removed (not removed files!)
    // This is counterintuitive but matches RPM's behavior (rpmtriggersPrepPostUnTransFileTrigs checks RPMFILE_IS_INSTALLED)
    let files_to_collect = match trigger_type {
        "transfiletriggerin" => {
            // Collect from fresh installs, upgrades, and all installed packages
            let files = collect_package_files_to_set(
                store_root,
                fresh_installs.values().chain(upgrades_new.values()).chain(installed_packages.values()),
            );
            Some(files.into_iter().collect())
        }
        "transfiletriggerun" => {
            // Collect from packages being removed
            let files = collect_package_files_to_vec(store_root, old_removes.values());
            Some(files)
        }
        "transfiletriggerpostun" => {
            // CRITICAL: Match against INSTALLED files from packages being removed, not removed files!
            // This matches RPM's behavior: rpmtriggersPrepPostUnTransFileTrigs checks RPMFILE_IS_INSTALLED
            // Reference: rpmtriggers.cc:136-170 (rpmtriggersPrepPostUnTransFileTrigs)
            //
            // The files in the store's fs/ directory represent the installed files from the package.
            // Even though files are unlinked from env_root during removal, the store's fs/ directory
            // still contains the installed files, which is what we match against.
            // This is counterintuitive but matches RPM's documented behavior.
            let files = collect_package_files_to_set(store_root, old_removes.values());
            // Return as Vec for consistency with other trigger types
            Some(files.into_iter().collect())
        }
        _ => return Ok(()),
    };

    // Find packages with transaction file triggers
    // Skip packages in the current transaction (they run via immediate triggers instead)
    // This matches RPM's skipFileTrigger logic for transaction triggers
    // Reference: rpmtriggers.cc:519-554 (skipFileTrigger)
    let mut trigger_packages: Vec<(String, InstalledPackageInfo, PathBuf, Vec<String>)> = Vec::new();
    // Structure: (pkgkey, pkg_info, script_path, matching_files)

    // Build set of packages in current transaction for skip logic
    // RPM skips packages in installedPackages or removedPackages hashes
    // Packages in transaction run their triggers via runImmedFileTriggers instead
    let transaction_packages: HashSet<String> = fresh_installs.keys()
        .chain(upgrades_new.keys())
        .chain(old_removes.keys())
        .cloned()
        .collect();

    // Iterate over all installed packages (including those being installed in this transaction)
    // but skip those in the transaction - they run via immediate triggers
    for (pkgkey, pkg_info) in installed_packages.iter().chain(fresh_installs.iter()) {
        // Skip packages in current transaction (RPM behavior: skipFileTrigger for transaction triggers)
        // These packages will have their transaction file triggers executed via immediate triggers
        // (runImmedFileTriggers equivalent) during package processing
        if transaction_packages.contains(pkgkey) {
            log::debug!("Skipping transaction file trigger {} for package {} (in current transaction)", trigger_type, pkgkey);
            continue;
        }
        let install_dir = store_root.join(&pkg_info.pkgline).join("info/install");
        let trigger_metadata = install_dir.join(format!("{}.triggers", trigger_type));

        let script_path = find_trigger_script_path(&install_dir, trigger_type);

        if let Some(script_path) = script_path {
            if let Some(trigger_paths) = read_trigger_paths(&trigger_metadata) {
                // Match files against trigger paths
                if let Some(ref files_to_check) = files_to_collect {
                    let matching_files = match_files_against_trigger_paths(files_to_check, &trigger_paths);

                    // Only add if there are matching files (except for transfiletriggerpostun)
                    if !matching_files.is_empty() || trigger_type == "transfiletriggerpostun" {
                        trigger_packages.push((pkgkey.clone(), pkg_info.clone(), script_path, matching_files));
                    }
                } else if trigger_type == "transfiletriggerpostun" {
                    // transfiletriggerpostun executes if there were matching installed files from packages being removed
                    // but doesn't receive the file list via stdin
                    // Check if there are any matching files (even though we won't pass them)
                    if let Some(ref files_to_check) = files_to_collect {
                        if has_matching_files_against_trigger_paths(files_to_check, &trigger_paths) {
                            trigger_packages.push((pkgkey.clone(), pkg_info.clone(), script_path, Vec::new()));
                        }
                    }
                }
            }
        }
    }

    // Execute each trigger once (deduplicate by package)
    trigger_packages.sort_by(|a, b| b.1.depend_depth.cmp(&a.1.depend_depth));
    trigger_packages.dedup_by(|a, b| a.0 == b.0);

    for (pkgkey, pkg_info, script_path, matching_files) in trigger_packages {
        log::info!("Running RPM {} trigger for package {}", trigger_type, pkgkey);

        // Transaction file triggers receive $1 (triggered package instances) but not $2
        // $1 = number of installed instances of triggered package after operation
        let arg1 = calculate_trigger_arg1(
            &pkgkey,
            installed_packages,
            fresh_installs,
            old_removes,
        );

        // Pass matching files via stdin (one per line), except for transfiletriggerpostun
        let stdin_data = if trigger_type == "transfiletriggerpostun" {
            None // transfiletriggerpostun doesn't receive file list
        } else {
            Some(matching_files.join("\n").into_bytes())
        };

        execute_file_trigger_scriptlet(
            &script_path,
            &pkgkey,
            &pkg_info,
            store_root,
            env_root,
            PackageFormat::Rpm,
            arg1,
            0, // $2 not used for transaction file triggers
            stdin_data,
        )?;
    }

    Ok(())
}

/// Execute a trigger scriptlet
/// $1: number of installed instances of the triggered package (the one containing the trigger)
/// $2: number of installed instances of the triggering package (the one that set off the trigger)
fn execute_trigger_scriptlet(
    script_path: &Path,
    pkgkey: &str,
    package_info: &InstalledPackageInfo,
    _store_root: &Path,
    env_root: &Path,
    package_format: PackageFormat,
    arg1: u32, // Number of instances of triggered package
    arg2: u32, // Number of instances of triggering package
) -> Result<()> {
    execute_file_trigger_scriptlet(script_path, pkgkey, package_info, _store_root, env_root, package_format, arg1, arg2, None)
}

/// Execute a file trigger scriptlet with optional stdin data
/// File triggers receive matching paths via standard input, one per line
fn execute_file_trigger_scriptlet(
    script_path: &Path,
    pkgkey: &str,
    package_info: &InstalledPackageInfo,
    _store_root: &Path,
    env_root: &Path,
    package_format: PackageFormat,
    arg1: u32, // Number of instances of triggered package
    arg2: u32, // Number of instances of triggering package (0 for transaction file triggers)
    stdin_data: Option<Vec<u8>>, // Matching file paths, one per line
) -> Result<()> {
    let script_name = script_path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");
    let interpreters = get_interpreters_for_script(script_name);

    for interpreter in interpreters {
        let interpreter_path = env_root.join("usr/bin").join(interpreter);
        if !interpreter_path.exists() {
            continue;
        }

        let mut env_vars = std::collections::HashMap::new();
        if package_format == PackageFormat::Deb {
            setup_deb_env_vars(&mut env_vars, pkgkey, package_info, ScriptletType::PostInstall, env_root);
        }

        // Pass $1 and $2 as arguments to the script
        // For shell scripts, these will be available as $1 and $2
        // For other interpreters, they're passed as command-line arguments
        let run_options = crate::run::RunOptions {
            command: interpreter.to_string(),
            args: vec![
                script_path.to_string_lossy().to_string(),
                arg1.to_string(), // $1
                arg2.to_string(), // $2
            ],
            env_vars,
            stdin: stdin_data.clone(), // Pass matching file paths via stdin
            no_exit: true,
            chdir_to_env_root: true,
            timeout: 60,
            ..Default::default()
        };

        match crate::run::fork_and_execute(env_root, &run_options, &interpreter_path) {
            Ok(()) => return Ok(()),
            Err(e) => {
                log::warn!("Failed to execute trigger scriptlet {}: {}", script_path.display(), e);
                continue;
            }
        }
    }

    Ok(())
}
