//! Installation steps execution module
//!
//! This module handles the execution of installation steps including pre/post scriptlets,
//! trigger processing, and hook execution for package installations and upgrades.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::fs;
use std::time::SystemTime;
use color_eyre::Result;
use color_eyre::eyre::{eyre, WrapErr};
use crate::models::{PackageFormat, InstalledPackageInfo, InstalledPackagesMap};
use crate::models::PACKAGE_CACHE;
use crate::plan::InstallationPlan;
use crate::hooks;
use crate::rpm_triggers;
use crate::deb_triggers;
use crate::deb_triggers::process_deb_triggers;
use crate::scriptlets::{run_scriptlets, ScriptletType};
use crate::package;
use crate::run;
use crate::remove::unlink_package;
use log;


/// Process installation results (upgrades and fresh installations)
/// https://rpm-software-management.github.io/rpm/man/rpm-scriptlets.7#EXECUTION_ORDER
pub fn process_installation_results(
    plan: &InstallationPlan,
    completed_packages: &InstalledPackagesMap,
    store_root: &Path,
    env_root: &Path,
    package_format: PackageFormat,
) -> Result<()> {
    // Load hooks for Arch Linux (Pacman format)
    let hooks = if package_format == PackageFormat::Pacman {
        match hooks::load_hooks(env_root) {
            Ok(hooks) => {
                log::debug!("Loaded {} hooks", hooks.len());
                Some(hooks)
            }
            Err(e) => {
                log::warn!("Failed to load hooks: {}", e);
                None
            }
        }
    } else {
        None
    };

    // Build completed package maps once (reused for both PreTransaction and PostTransaction hooks)
    let mut fresh_installs_completed = HashMap::new();
    let mut upgrades_new_completed = HashMap::new();

    for (pkgkey, info) in completed_packages.iter() {
        if plan.fresh_installs.contains_key(pkgkey) {
            fresh_installs_completed.insert(pkgkey.clone(), info.clone());
        }
        if plan.upgrades_new.contains_key(pkgkey) {
            upgrades_new_completed.insert(pkgkey.clone(), info.clone());
        }
    }

    // Run PreTransaction hooks
    if let Some(ref hooks) = hooks {
        if !fresh_installs_completed.is_empty() || !upgrades_new_completed.is_empty() || !plan.old_removes.is_empty() {
            hooks::run_hooks(
                hooks,
                env_root,
                store_root,
                hooks::HookWhen::PreTransaction,
                &fresh_installs_completed,
                &upgrades_new_completed,
                &plan.upgrades_old,
                &plan.old_removes,
            )?;
        }
    }

    // Process upgrades
    if !upgrades_new_completed.is_empty() {
        log::info!("Processing {} upgrades", upgrades_new_completed.len());
        process_upgrades(
            &plan.upgrades_old,
            &upgrades_new_completed,
            &plan.upgrade_map_old_to_new,
            store_root,
            env_root,
            package_format,
        )?;
    }

    // Process fresh installations
    if !fresh_installs_completed.is_empty() {
        log::info!("Processing {} fresh installations", fresh_installs_completed.len());
        process_fresh_installs(&fresh_installs_completed, store_root, env_root, package_format)?;
    }

    // Run PostTransaction hooks
    if let Some(ref hooks) = hooks {
        if !fresh_installs_completed.is_empty() || !upgrades_new_completed.is_empty() || !plan.old_removes.is_empty() {
            hooks::run_hooks(
                hooks,
                env_root,
                store_root,
                hooks::HookWhen::PostTransaction,
                &fresh_installs_completed,
                &upgrades_new_completed,
                &plan.upgrades_old,
                &plan.old_removes,
            )?;
        }
    }


    Ok(())
}

/// Execute package removals
pub fn execute_removals(plan: &InstallationPlan, store_root: &Path, env_root: &Path, package_format: PackageFormat) -> Result<()> {
    if plan.old_removes.is_empty() {
        return Ok(());
    }

    // Note: %preuntrans is executed earlier in execute_installation_plan, before removals start

    // Update rdepends of packages that depended on the removed packages
    for (removed_pkg_key, removed_pkg_info) in plan.old_removes.iter() {
        for dep_on_key in &removed_pkg_info.depends {
            // If the dependency itself is NOT being removed
            if !plan.old_removes.contains_key(dep_on_key) {
                // Get the mutable info of this dependency from the main installed_packages map
                let mut installed = PACKAGE_CACHE.installed_packages.write().unwrap();
                if let Some(dep_pkg_info_mut) = installed.get_mut(dep_on_key) {
                    let initial_rdep_count = dep_pkg_info_mut.rdepends.len();
                    dep_pkg_info_mut.rdepends.retain(|r| r != removed_pkg_key);
                    if dep_pkg_info_mut.rdepends.len() < initial_rdep_count {
                        log::debug!("Updated rdepends for '{}': removed '{}' (was one of its rdepends)", dep_on_key, removed_pkg_key);
                    } else {
                        log::trace!("Checked rdepends for '{}': '{}' was not found as an rdepend (or already removed)", dep_on_key, removed_pkg_key);
                    }
                }
            }
        }
    }

    // RPM file triggers (filetriggerun, high priority) - BEFORE preun
    // RPM execution order: filetriggerun (high) -> triggerun -> preun -> filetriggerun (low) -> remove files
    if package_format == PackageFormat::Rpm {
        let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
        if let Err(e) = rpm_triggers::run_rpm_file_triggers(
            "filetriggerun",
                &installed,
            &HashMap::new(),
            &HashMap::new(),
            &plan.old_removes,
            store_root,
            env_root,
            1, // High priority (>= 10000)
        ) {
            log::warn!("Failed to run RPM filetriggerun triggers (high priority): {}", e);
        }
    }

    // RPM package triggers (triggerun) - before preun
    if package_format == PackageFormat::Rpm {
        let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
        if let Err(e) = rpm_triggers::run_rpm_package_triggers(
            "triggerun",
                &installed,
            &HashMap::new(),
            &HashMap::new(),
            &plan.old_removes,
            store_root,
            env_root,
        ) {
            log::warn!("Failed to run RPM triggerun triggers: {}", e);
        }
    }

    // Run pre-remove scriptlets
    run_scriptlets(
        &plan.old_removes,
        store_root,
        env_root,
        package_format,
        ScriptletType::PreRemove,
        false, // is_upgrade
    )?;

    // RPM file triggers (filetriggerun, low priority) - AFTER preun, BEFORE file removal
    if package_format == PackageFormat::Rpm {
        let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
        if let Err(e) = rpm_triggers::run_rpm_file_triggers(
            "filetriggerun",
                &installed,
            &HashMap::new(),
            &HashMap::new(),
            &plan.old_removes,
            store_root,
            env_root,
            2, // Low priority (< 10000)
        ) {
            log::warn!("Failed to run RPM filetriggerun triggers (low priority): {}", e);
        }
    }

    // Unlink packages
    for (pkgkey, pkg_info) in plan.old_removes.iter() {
        // Ensure pkgline is valid for path construction
        if pkg_info.pkgline.is_empty() || pkg_info.pkgline.contains("/") || pkg_info.pkgline.contains("..") {
            log::error!("Invalid pkgline for {}: '{}'. Skipping unlink.", pkgkey, pkg_info.pkgline);
            return Err(eyre!("Invalid pkgline for {}: '{}'", pkgkey, pkg_info.pkgline));
        }
        let pkg_store_path = store_root.join(&pkg_info.pkgline);
        log::info!("Unlinking files for package: {} from store path {}", pkgkey, pkg_store_path.display());

        // Remove DEB trigger interests before unlinking
        // Check if this is a DEB package by looking for trigger interest file
        let install_dir = store_root.join(&pkg_info.pkgline).join("info/install");
        let interest_file = install_dir.join("deb_interest.triggers");
        if interest_file.exists() {
            if let Err(e) = deb_triggers::incorporate_package_trigger_interests(
                pkgkey,
                store_root,
                env_root,
                true, // is_removal
            ) {
                log::warn!("Failed to remove trigger interests for {}: {}", pkgkey, e);
            }
        }

        unlink_package(&pkg_store_path, &env_root.to_path_buf())
            .with_context(|| format!("Failed to unlink package {} (store path: {})", pkgkey, pkg_store_path.display()))?;
        PACKAGE_CACHE.installed_packages.write().unwrap().remove(pkgkey);
    }

    // RPM file triggers (filetriggerpostun, high priority) - AFTER file removal, BEFORE postun
    // RPM execution order: remove files -> filetriggerpostun (high) -> postun -> triggerpostun -> filetriggerpostun (low)
    if package_format == PackageFormat::Rpm {
        let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
        if let Err(e) = rpm_triggers::run_rpm_file_triggers(
            "filetriggerpostun",
                &installed,
            &HashMap::new(),
            &HashMap::new(),
            &plan.old_removes,
            store_root,
            env_root,
            1, // High priority (>= 10000)
        ) {
            log::warn!("Failed to run RPM filetriggerpostun triggers (high priority): {}", e);
        }
    }

    // Run post-remove scriptlets
    run_scriptlets(
        &plan.old_removes,
        store_root,
        env_root,
        package_format,
        ScriptletType::PostRemove,
        false, // is_upgrade
    )?;

    // RPM package triggers (triggerpostun) - after triggering package is removed
    if package_format == PackageFormat::Rpm {
        let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
        if let Err(e) = rpm_triggers::run_rpm_package_triggers(
            "triggerpostun",
                &installed,
            &HashMap::new(),
            &HashMap::new(),
            &plan.old_removes,
            store_root,
            env_root,
        ) {
            log::warn!("Failed to run RPM triggerpostun triggers: {}", e);
        }
    }

    // RPM file triggers (filetriggerpostun, low priority) - AFTER postun
    if package_format == PackageFormat::Rpm {
        let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
        if let Err(e) = rpm_triggers::run_rpm_file_triggers(
            "filetriggerpostun",
                &installed,
            &HashMap::new(),
            &HashMap::new(),
            &plan.old_removes,
            store_root,
            env_root,
            2, // Low priority (< 10000)
        ) {
            log::warn!("Failed to run RPM filetriggerpostun triggers (low priority): {}", e);
        }
    }

    Ok(())
}

/// Process upgrade flow for packages
fn process_upgrades(
    old_packages: &InstalledPackagesMap,
    new_packages: &InstalledPackagesMap,
    upgrade_map_old_to_new: &HashMap<String, String>,
    store_root: &Path,
    env_root: &Path,
    package_format: PackageFormat,
) -> Result<()> {
    // Process each package upgrade individually
    for (old_pkgkey, old_package_info) in old_packages.iter() {
        let new_pkgkey = upgrade_map_old_to_new.get(old_pkgkey);
        let new_package_info = new_pkgkey.and_then(|k| new_packages.get(k.as_str()));

        if let (Some(new_pkgkey), Some(new_package_info)) = (new_pkgkey, new_package_info) {
            log::info!("Upgrading package: {} (from {})", new_pkgkey, old_pkgkey);
            process_single_package_upgrade(
                new_pkgkey,
                old_pkgkey,
                old_package_info,
                new_package_info,
                store_root,
                env_root,
                package_format,
            )?;
        } else {
            log::warn!("New package info not found for upgrade from: {}", old_pkgkey);
        }
    }
    Ok(())
}

/// Process upgrade flow for a single package pair
fn process_single_package_upgrade(
    new_pkgkey: &str,
    old_pkgkey: &str,
    old_package_info: &InstalledPackageInfo,
    new_package_info: &InstalledPackageInfo,
    store_root: &Path,
    env_root: &Path,
    package_format: PackageFormat,
) -> Result<()> {
    use crate::scriptlets::{run_scriptlet, ScriptletType};

    // Extract version information
    let old_version = package::pkgkey2version(old_pkgkey).ok();
    let new_version = package::pkgkey2version(new_pkgkey).ok();

    log::debug!(
        "Processing upgrade for {}: {} -> {}",
        new_pkgkey,
        old_version.as_deref().unwrap_or("unknown"),
        new_version.as_deref().unwrap_or("unknown")
    );

    // Step 1: New package pre-upgrade (with old version info)
    run_scriptlet(
        new_pkgkey,
        new_package_info,
        store_root,
        env_root,
        package_format,
        ScriptletType::PreUpgrade,
        true, // is_upgrade
        old_version.as_deref(),
        new_version.as_deref(),
    )?;

    // Step 1.5: RPM package triggers (triggerprein) for upgrade
    if package_format == PackageFormat::Rpm {
        let mut upgrades_new_map = HashMap::new();
        upgrades_new_map.insert(new_pkgkey.to_string(), new_package_info.clone());
        let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
        if let Err(e) = rpm_triggers::run_rpm_package_triggers(
            "triggerprein",
                &installed,
            &upgrades_new_map,
            &HashMap::new(),
            &HashMap::new(),
            store_root,
            env_root,
        ) {
            log::warn!("Failed to run RPM triggerprein triggers during upgrade: {}", e);
        }
    }

    // Step 1.6: RPM package triggers (triggerun) for upgrade - BEFORE preun
    // RPM execution order: triggerun (old) -> triggerun (rpmdb) -> filetriggerun (high) -> preun
    if package_format == PackageFormat::Rpm {
        let mut old_removes_map = HashMap::new();
        old_removes_map.insert(old_pkgkey.to_string(), old_package_info.clone());
        let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
        if let Err(e) = rpm_triggers::run_rpm_package_triggers(
            "triggerun",
                &installed,
            &HashMap::new(),
            &HashMap::new(),
            &old_removes_map,
            store_root,
            env_root,
        ) {
            log::warn!("Failed to run RPM triggerun triggers during upgrade: {}", e);
        }
    }

    // Step 1.7: RPM file triggers (filetriggerun, high priority) - BEFORE preun
    if package_format == PackageFormat::Rpm {
        let mut old_removes_map = HashMap::new();
        old_removes_map.insert(old_pkgkey.to_string(), old_package_info.clone());
        let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
        if let Err(e) = rpm_triggers::run_rpm_file_triggers(
            "filetriggerun",
                &installed,
            &HashMap::new(),
            &HashMap::new(),
            &old_removes_map,
            store_root,
            env_root,
            1, // High priority (>= 10000)
        ) {
            log::warn!("Failed to run RPM filetriggerun triggers (high priority) during upgrade: {}", e);
        }
    }

    // Step 2: Old package pre-remove (with new version info)
    run_scriptlet(
        new_pkgkey,
        old_package_info,
        store_root,
        env_root,
        package_format,
        ScriptletType::PreRemove,
        true, // is_upgrade
        old_version.as_deref(),
        new_version.as_deref(),
    )?;

    // Step 2.5: RPM file triggers (filetriggerun, low priority) - AFTER preun, BEFORE file removal
    if package_format == PackageFormat::Rpm {
        let mut old_removes_map = HashMap::new();
        old_removes_map.insert(old_pkgkey.to_string(), old_package_info.clone());
        let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
        if let Err(e) = rpm_triggers::run_rpm_file_triggers(
            "filetriggerun",
                &installed,
            &HashMap::new(),
            &HashMap::new(),
            &old_removes_map,
            store_root,
            env_root,
            2, // Low priority (< 10000)
        ) {
            log::warn!("Failed to run RPM filetriggerun triggers (low priority) during upgrade: {}", e);
        }
    }

    // Step 3: Link new package files to env
    // Done in wait_downloads_and_unpack_link() via unpack_package() for now
    // let new_store_fs_dir = store_root.join(&new_package_info.pkgline).join("fs");
    // link_package(&new_store_fs_dir, &env_root.to_path_buf())
    //     .with_context(|| format!("Failed to link new package {}", new_package_info.pkgline))?;

    // Step 3.5: DEB trigger handling (incorporate interests, build index, activate file triggers)
    if package_format == PackageFormat::Deb {
        // Use helper function for single package
        let packages = [(new_pkgkey, new_package_info)];
        process_deb_triggers(&packages, store_root, env_root)?;
    }

    // Step 4: Unlink old package unique files (files in old_pkg but not in new_pkg)
    crate::link::unlink_package_diff(old_package_info, new_package_info, store_root, env_root)
        .with_context(|| format!("Failed to unlink old package files for {}", old_pkgkey))?;

    // Step 5: New package post-upgrade (with old version info)
    run_scriptlet(
        new_pkgkey,
        new_package_info,
        store_root,
        env_root,
        package_format,
        ScriptletType::PostUpgrade,
        true, // is_upgrade
        old_version.as_deref(),
        new_version.as_deref(),
    )?;

    // Step 5.5: RPM package triggers (triggerin) for upgrade
    if package_format == PackageFormat::Rpm {
        let mut upgrades_new_map = HashMap::new();
        upgrades_new_map.insert(new_pkgkey.to_string(), new_package_info.clone());
        let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
        if let Err(e) = rpm_triggers::run_rpm_package_triggers(
            "triggerin",
                &installed,
            &upgrades_new_map,
            &HashMap::new(),
            &HashMap::new(),
            store_root,
            env_root,
        ) {
            log::warn!("Failed to run RPM triggerin triggers during upgrade: {}", e);
        }
    }

    // Step 6: Old package post-remove (with new version info)
    run_scriptlet(
        new_pkgkey,
        old_package_info,
        store_root,
        env_root,
        package_format,
        ScriptletType::PostRemove,
        true, // is_upgrade
        old_version.as_deref(),
        new_version.as_deref(),
    )?;

    // Step 6.5: RPM package triggers (triggerpostun) for upgrade
    if package_format == PackageFormat::Rpm {
        let mut old_removes_map = HashMap::new();
        old_removes_map.insert(old_pkgkey.to_string(), old_package_info.clone());
        let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
        if let Err(e) = rpm_triggers::run_rpm_package_triggers(
            "triggerpostun",
                &installed,
            &HashMap::new(),
            &HashMap::new(),
            &old_removes_map,
            store_root,
            env_root,
        ) {
            log::warn!("Failed to run RPM triggerpostun triggers during upgrade: {}", e);
        }
    }

    log::info!("Successfully upgraded package: {}", new_pkgkey);
    Ok(())
}

/// Process fresh install flow for packages
fn process_fresh_installs(
    fresh_installs: &InstalledPackagesMap,
    store_root: &Path,
    env_root: &Path,
    package_format: PackageFormat,
) -> Result<()> {
    // Fresh install flow:
    // 1. triggerprein (RPM package triggers)
    // 2. pre_install  (check dependencies/conflicts)
    // 3. install files (link packages)
    // 4. filetriggerin (high priority)
    // 5. post_install (start services/update config)
    // 6. triggerin
    // 7. filetriggerin (low priority)

    // Step 0.5: RPM package triggers (triggerprein) - BEFORE pre
    // RPM execution order: sysusers -> triggerprein (rpmdb) -> triggerprein (new) -> pre
    // Pre-compute trigger data once for reuse across multiple trigger calls in this function
    let rpm_trigger_data = if package_format == PackageFormat::Rpm {
        // Include both installed packages and fresh installs to check for triggers
        // Create a temporary HashMap for the merged data
        let mut all_installed = HashMap::new();
        for (k, v) in PACKAGE_CACHE.installed_packages.read().unwrap().iter() {
            all_installed.insert(k.clone(), v.clone());
        }
        for (k, v) in fresh_installs.iter() {
            all_installed.insert(k.clone(), v.clone());
        }
        Some(rpm_triggers::prepare_rpm_trigger_data(
            &all_installed,
            fresh_installs,
            &HashMap::new(),
            &HashMap::new(),
        ))
    } else {
        None
    };

    if package_format == PackageFormat::Rpm {
        let mut all_installed = HashMap::new();
        for (k, v) in PACKAGE_CACHE.installed_packages.read().unwrap().iter() {
            all_installed.insert(k.clone(), v.clone());
        }
        for (k, v) in fresh_installs.iter() {
            all_installed.insert(k.clone(), v.clone());
        }
        if let Some((ref triggering_packages, ref all_packages)) = rpm_trigger_data {
            if let Err(e) = rpm_triggers::run_rpm_package_triggers_with_data(
                "triggerprein",
                &all_installed,
                fresh_installs,
                &HashMap::new(),
                &HashMap::new(),
                triggering_packages,
                all_packages,
                store_root,
                env_root,
            ) {
                log::warn!("Failed to run RPM triggerprein triggers: {}", e);
            }
        }
    }

    // Step 1: Pre-install
    run_scriptlets(
        fresh_installs,
        store_root,
        env_root,
        package_format,
        ScriptletType::PreInstall,
        false, // is_upgrade
    )?;

    // Step 1.5: Activate DEB activate triggers (status-change activation)
    // These are triggers declared with "activate" directive that fire on package status changes
    if package_format == PackageFormat::Deb {
        for (pkgkey, _pkg_info) in fresh_installs.iter() {
            let activate_triggers = deb_triggers::read_package_activate_triggers(pkgkey, store_root)
                .unwrap_or_default();
            let pkgname = package::pkgkey2pkgname(pkgkey).ok();
            for (trigger_name, await_mode) in activate_triggers {
                if let Err(e) = deb_triggers::activate_trigger(
                    env_root,
                    &trigger_name,
                    pkgname.as_deref(),
                    !await_mode, // no_await is inverse of await_mode
                ) {
                    log::warn!("Failed to activate trigger {} for package {}: {}", trigger_name, pkgkey, e);
                }
            }
        }
    }

    // Step 2: Install files (link packages)
    // This is moved earlier to wait_downloads_and_unpack_link() via unpack_package(), so that scriptlets have command to run.
    // for (_, package_info) in fresh_installs {
    //     let store_fs_dir = store_root.join(&package_info.pkgline).join("fs");
    //     link_package(&store_fs_dir, &env_root.to_path_buf())
    //         .with_context(|| format!("Failed to link package {}", package_info.pkgline))?;
    // }

    // Step 2.3: DEB trigger handling (incorporate interests, build index, activate file triggers)
    if package_format == PackageFormat::Deb {
        // Collect packages into a Vec for the helper function
        let packages: Vec<_> = fresh_installs
            .iter()
            .map(|(k, v)| (k.as_str(), v))
            .collect();
        process_deb_triggers(&packages, store_root, env_root)?;
    }

    // Step 2.5: RPM file triggers (filetriggerin, high priority) - AFTER file unpack, BEFORE postin
    // RPM execution order: unpack -> filetriggerin (high) -> postin -> triggerin -> filetriggerin (low)
    if package_format == PackageFormat::Rpm {
        let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
        let mut all_installed = HashMap::new();
        for (k, v) in installed.iter() {
            all_installed.insert(k.clone(), v.clone());
        }
        for (k, v) in fresh_installs.iter() {
            all_installed.insert(k.clone(), v.clone());
        }
        drop(installed);
        if let Err(e) = rpm_triggers::run_rpm_file_triggers(
            "filetriggerin",
            &all_installed,
            fresh_installs,
            &HashMap::new(),
            &HashMap::new(),
            store_root,
            env_root,
            1, // High priority (>= 10000)
        ) {
            log::warn!("Failed to run RPM filetriggerin triggers (high priority): {}", e);
        }
    }

    // Step 3: Post-install
    run_scriptlets(
        fresh_installs,
        store_root,
        env_root,
        package_format,
        ScriptletType::PostInstall,
        false, // is_upgrade
    )?;

    // Step 3.3: RPM package triggers (triggerin) - after triggering package is installed
    // Also runs when your package is installed and target is already installed
    if package_format == PackageFormat::Rpm {
        // Include both installed packages and fresh installs to check for triggers
        let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
        let mut all_installed = HashMap::new();
        for (k, v) in installed.iter() {
            all_installed.insert(k.clone(), v.clone());
        }
        for (k, v) in fresh_installs.iter() {
            all_installed.insert(k.clone(), v.clone());
        }
        drop(installed);
        // Reuse pre-computed trigger data from earlier
        if let Some((ref triggering_packages, ref all_packages)) = rpm_trigger_data {
            if let Err(e) = rpm_triggers::run_rpm_package_triggers_with_data(
                "triggerin",
                &all_installed,
                fresh_installs,
                &HashMap::new(),
                &HashMap::new(),
                triggering_packages,
                all_packages,
                store_root,
                env_root,
            ) {
                log::warn!("Failed to run RPM triggerin triggers: {}", e);
            }
        }
    }

    // Step 3.4: RPM file triggers (filetriggerin, low priority) - AFTER postin
    if package_format == PackageFormat::Rpm {
        let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
        let mut all_installed = HashMap::new();
        for (k, v) in installed.iter() {
            all_installed.insert(k.clone(), v.clone());
        }
        for (k, v) in fresh_installs.iter() {
            all_installed.insert(k.clone(), v.clone());
        }
        drop(installed);
        if let Err(e) = rpm_triggers::run_rpm_file_triggers(
            "filetriggerin",
            &all_installed,
            fresh_installs,
            &HashMap::new(),
            &HashMap::new(),
            store_root,
            env_root,
            2, // Low priority (< 10000)
        ) {
            log::warn!("Failed to run RPM filetriggerin triggers (low priority): {}", e);
        }
    }

    // Step 3.5: Incorporate and process DEB triggers
    // Incorporate triggers from Unincorp into package status, then process pending triggers
    if package_format == PackageFormat::Deb {
        let mut all_packages: InstalledPackagesMap = PACKAGE_CACHE.installed_packages.read().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        for (k, v) in fresh_installs.iter() {
            all_packages.insert(k.clone(), v.clone());
        }

        // Incorporate triggers from Unincorp
        let incorporation_result = deb_triggers::incorporate_triggers(env_root, &all_packages, store_root)
            .unwrap_or_else(|_| deb_triggers::TriggerIncorporationResult {
                pending_triggers: HashMap::new(),
                awaiting_packages: HashSet::new(),
            });

        // Update package states based on incorporation results
        // Mark packages with pending triggers
        // Collect pkgkeys first to avoid borrow checker issues
        let mut pending_pkgkeys: Vec<(String, Vec<String>)> = Vec::new();
        for (pkgname, trigger_names) in &incorporation_result.pending_triggers {
            if let Some((pkgkey, _)) = all_packages.iter()
                .find(|(k, _)| package::pkgkey2pkgname(k).unwrap_or_default() == *pkgname) {
                pending_pkgkeys.push((pkgkey.clone(), trigger_names.clone()));
            }
        }
        for (pkgkey, trigger_names) in pending_pkgkeys {
            if let Some(info) = all_packages.get_mut(&pkgkey) {
                info.pending_triggers = trigger_names.clone();
            }
            if let Some(info) = PACKAGE_CACHE.installed_packages.write().unwrap().get_mut(&pkgkey) {
                info.pending_triggers = trigger_names.clone();
            }
            let pkgname = package::pkgkey2pkgname(&pkgkey).unwrap_or_default();
            log::debug!("Package {} has pending triggers: {:?}", pkgname, trigger_names);
        }

        // Mark packages that should await trigger processing
        // Collect pkgkeys first to avoid borrow checker issues
        let mut awaiting_pkgkeys: Vec<String> = Vec::new();
        for pkgname in &incorporation_result.awaiting_packages {
            if let Some((pkgkey, _)) = all_packages.iter()
                .find(|(k, _)| package::pkgkey2pkgname(k).unwrap_or_default() == *pkgname) {
                awaiting_pkgkeys.push(pkgkey.clone());
            }
        }
        for pkgkey in awaiting_pkgkeys {
            if let Some(info) = all_packages.get_mut(&pkgkey) {
                info.triggers_awaited = true;
            }
            if let Some(info) = PACKAGE_CACHE.installed_packages.write().unwrap().get_mut(&pkgkey) {
                info.triggers_awaited = true;
            }
            let pkgname = package::pkgkey2pkgname(&pkgkey).unwrap_or_default();
            log::debug!("Package {} is awaiting trigger processing", pkgname);
        }

        // Process triggers for packages with pending triggers
        // Reset cycle detection at start
        deb_triggers::reset_cycle_detection();

        // Collect pkgkeys first to avoid borrow checker issues
        let mut processing_queue: Vec<(String, String, Vec<String>)> = Vec::new();
        for (pkgname, trigger_names) in incorporation_result.pending_triggers {
            if let Some((pkgkey, _)) = all_packages.iter()
                .find(|(k, _)| package::pkgkey2pkgname(k).unwrap_or_default() == pkgname) {
                // Skip packages that are already in config-failed state
                if let Some(info) = all_packages.get(pkgkey) {
                    if info.config_failed {
                        log::debug!("Skipping package {} - already in config-failed state", pkgname);
                        continue;
                    }
                }
                processing_queue.push((pkgkey.clone(), pkgname.clone(), trigger_names));
            }
        }

        // Process triggers with cycle detection
        for (pkgkey, pkgname, trigger_names) in processing_queue {
            // Build map of all pending triggers for cycle detection
            let mut all_pending_triggers: HashMap<String, Vec<String>> = HashMap::new();
            for (k, info) in all_packages.iter() {
                if !info.pending_triggers.is_empty() && !info.config_failed {
                    all_pending_triggers.insert(k.clone(), info.pending_triggers.clone());
                }
            }

            // Check for cycles before processing
            if let Some(cycle_pkgkey) = deb_triggers::check_trigger_cycle(&pkgkey, &all_pending_triggers) {
                log::warn!("Trigger cycle detected! Marking package {} as config-failed to break cycle", cycle_pkgkey);
                // Mark the cycle-breaking package as config-failed
                if let Some(info) = all_packages.get_mut(&cycle_pkgkey) {
                    info.config_failed = true;
                    info.pending_triggers.clear(); // Clear pending triggers
                }
                if let Some(info) = PACKAGE_CACHE.installed_packages.write().unwrap().get_mut(&cycle_pkgkey) {
                    info.config_failed = true;
                    info.pending_triggers.clear();
                }
                // Skip processing this package if it's the one we're marking as failed
                if cycle_pkgkey == pkgkey {
                    continue;
                }
            }

            if let Some(pkg_info) = all_packages.get(&pkgkey).cloned() {
                match deb_triggers::process_package_triggers(
                    &pkgkey,
                    &pkg_info,
                    &trigger_names,
                    store_root,
                    env_root,
                ) {
                    Ok(_) => {
                        // Clear pending triggers after successful processing
                        if let Some(info) = all_packages.get_mut(&pkgkey) {
                            info.pending_triggers.clear();
                            info.config_failed = false; // Clear config-failed if processing succeeded
                        }
                        if let Some(info) = PACKAGE_CACHE.installed_packages.write().unwrap().get_mut(&pkgkey) {
                            info.pending_triggers.clear();
                            info.config_failed = false;
                        }
                        log::debug!("Successfully processed triggers for package {}", pkgname);
                    }
                    Err(e) => {
                        log::warn!("Failed to process triggers for package {}: {}", pkgname, e);
                        // Set package to config-failed state
                        if let Some(info) = all_packages.get_mut(&pkgkey) {
                            info.config_failed = true;
                            // Keep pending triggers - they won't be reattempted until explicitly requested
                        }
                        if let Some(info) = PACKAGE_CACHE.installed_packages.write().unwrap().get_mut(&pkgkey) {
                            info.config_failed = true;
                        }
                        log::warn!("Package {} marked as config-failed due to trigger processing failure", pkgname);
                    }
                }
            }
        }
    }

    // Run ldconfig if needed
    run_ldconfig_if_needed(env_root)?;

    Ok(())
}

/// Run ldconfig if the library cache needs updating
fn run_ldconfig_if_needed(env_root: &Path) -> Result<()> {
    let ld_so_cache = env_root.join("etc/ld.so.cache");
    let lib_dirs = [
        env_root.join("etc/ld.so.conf.d"),
        env_root.join("lib"),
        env_root.join("lib64"),
        env_root.join("usr/lib"),
        env_root.join("usr/lib64"),
    ];

    // Get mtime of ld.so.cache if it exists
    let cache_mtime = if ld_so_cache.exists() {
        fs::metadata(&ld_so_cache)
            .with_context(|| format!("Failed to get metadata for {}", ld_so_cache.display()))?
            .modified()
            .with_context(|| format!("Failed to get modification time for {}", ld_so_cache.display()))?
    } else {
        // If cache doesn't exist, we need to run ldconfig
        SystemTime::UNIX_EPOCH
    };

    // Check if any lib directory has been modified more recently than the cache
    let needs_update = lib_dirs.iter().any(|dir| {
        if !dir.exists() {
            return false;
        }
        match fs::metadata(dir) {
            Ok(metadata) => {
                match metadata.modified() {
                    Ok(dir_mtime) => dir_mtime > cache_mtime,
                    Err(_) => true, // If we can't get mtime, assume update needed
                }
            }
            Err(_) => false, // If we can't get metadata, skip this directory
        }
    });

    if needs_update {
        log::info!("Library cache needs updating, running ldconfig");

        // Check if ldconfig exists in the environment before trying to run it
        match run::find_command_in_env_path("ldconfig", env_root) {
            Ok(ldconfig_path) => {
                let run_options = run::RunOptions {
                    command: "ldconfig".to_string(),
                    no_exit: true,
                    chdir_to_env_root: true, // ldconfig should run relative to environment root
                    ..Default::default()
                };

                // Execute ldconfig
                run::fork_and_execute(env_root, &run_options, &ldconfig_path)?;
            }
            Err(_) => {
                log::warn!("ldconfig command not found in environment, skipping library cache update");
            }
        }
    } else {
        log::debug!("Library cache is up to date, skipping ldconfig");
    }

    Ok(())
}

