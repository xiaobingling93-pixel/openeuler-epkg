//! Installation steps execution module
//!
//! This module handles the execution of installation steps including pre/post scriptlets,
//! trigger processing, and hook execution for package installations and upgrades.
//!
//! Structure (3-level hierarchy inspired by RPM transaction model):
//!
//! ## Level 0: Transaction (rpmtsRun style)
//! Executes the entire transaction by processing all package operations in order.
//! Functions:
//! - `run_transaction_batch()` - Main transaction handler, processes all operations in `ordered_operations`
//! - `run_ldconfig_if_needed()` - Updates library cache after transaction completion
//!
//! ## Level 1: Package pair operation (rpmtsProcess/rpmteProcess style)
//! Processes a single package operation (install/upgrade/remove) by executing a sequence of actions.
//! Functions:
//! - `process_package_operation()` - Handles a single `PackageOperation`, calls `run_action()` for each step
//!
//! ## Level 2: Package actions entrypoint (runGoal style)
//! Executes a single action for a package operation.
//! Functions:
//! - `run_action()` - Executes one `PackageAction` (PreInstall, LinkFiles, PostInstall, etc.)
//!
//! ## Level 3a: Install/remove actions (rpmPackageInstall/rpmPackageErase style)
//! Performs actual file operations and system state changes.
//! Actions (handled within `run_action()`):
//! - `LinkFiles` - Links package files to environment (handles upgrade diff unlinking)
//! - `UnlinkFiles` - Unlinks package files from environment
//! - `ExposeExecutables` - Exposes executables (ebin exposure)
//! - `UnexposeExecutables` - Unexposes executables (ebin unexposure)
//!
//! ## Level 3b: Scriptlet/triggers actions
//! Executes scriptlets and triggers at various stages of package operations.
//! Actions (handled within `run_action()`):
//! - `PreInstall` - Runs pre-install scriptlets, RPM triggerprein, DEB activate triggers
//! - `PostInstall` - Runs post-install scriptlets, RPM triggerin/filetriggerin, DEB trigger processing
//! - `PreRemove` - Runs pre-remove scriptlets, RPM triggerun/filetriggerun (for upgrades)
//! - `PostRemove` - Runs post-remove scriptlets, RPM triggerpostun/filetriggerpostun (for upgrades)
//! - `PreUpgrade` - Runs pre-upgrade scriptlets, RPM triggerprein
//! - `PostUpgrade` - Runs post-upgrade scriptlets, RPM triggerin, DEB trigger processing
//!
//! ## Helper/Utility Functions
//! - Operation maps are cached in `InstallationPlan` (fresh_installs, upgrades_new, upgrades_old, old_removes)
//!
//! ## Obsolete Functions (kept for reference, marked with `#[allow(dead_code)]`)
//! - `process_upgrades()` - Replaced by `process_package_operation()` with integrated triggers
//! - `process_single_package_upgrade()` - Replaced by `run_action()` with integrated triggers
//! - `process_fresh_installs()` - Replaced by `process_package_operation()` with integrated triggers

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::fs;
use std::time::SystemTime;
use color_eyre::Result;
use std::sync::Arc;
use color_eyre::eyre::{eyre, WrapErr};
use crate::models::{PackageFormat, InstalledPackageInfo, InstalledPackagesMap};
use crate::models::PACKAGE_CACHE;
use crate::plan::{InstallationPlan, PackageOperation, OperationType};
use crate::hooks;
use crate::rpm_triggers;
use crate::deb_triggers;
use crate::deb_triggers::process_deb_triggers;
use crate::scriptlets;
use crate::scriptlets::{run_scriptlet, run_scriptlets, ScriptletType};
use crate::package;
use crate::run;
use crate::remove::unlink_package;
use log;

/// Package actions (L3: Package Actions)
/// Represents individual actions that compose a package operation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageAction {
    /// Pre-install scriptlet (before files are linked)
    PreInstall,
    /// Link files to environment
    LinkFiles,
    /// Post-install scriptlet (after files are linked)
    PostInstall,
    /// Pre-remove scriptlet (before files are unlinked)
    PreRemove,
    /// Unlink files from environment
    UnlinkFiles,
    /// Post-remove scriptlet (after files are unlinked)
    PostRemove,
    /// Pre-upgrade scriptlet (for new package, before old is removed)
    PreUpgrade,
    /// Post-upgrade scriptlet (for new package, after old is removed)
    PostUpgrade,
    /// Expose executables (ebin exposure)
    ExposeExecutables,
    /// Unexpose executables (ebin unexposure)
    UnexposeExecutables,
}

/// Level 2: Package actions entrypoint (runGoal style)
/// Executes a single action for a package
/// For upgrades, old_pkgkey and old_pkg_info are provided
fn run_action(
    plan: &mut InstallationPlan,
    action: PackageAction,
    pkgkey: &str,
    pkg_info: &Arc<InstalledPackageInfo>,
    old_pkgkey: Option<&str>,
    old_pkg_info: Option<&Arc<InstalledPackageInfo>>,
) -> Result<()> {
    let store_root = &plan.store_root;
    let env_root = &plan.env_root;
    let package_format = plan.package_format;
    let is_upgrade = old_pkgkey.is_some();
    let old_version = old_pkgkey.and_then(|k| package::pkgkey2version(k).ok());
    let new_version = package::pkgkey2version(pkgkey).ok();

    match action {
        PackageAction::PreInstall => {
            // Level 3b: Trigger and scriptlet actions
            let mut single_pkg: InstalledPackagesMap = HashMap::new();
            single_pkg.insert(pkgkey.to_string(), Arc::clone(pkg_info));

            // RPM triggerprein - BEFORE pre scriptlet
            if package_format == PackageFormat::Rpm {
                let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
                let mut all_installed: InstalledPackagesMap = HashMap::new();
                for (k, v) in installed.iter() {
                    all_installed.insert(k.clone(), Arc::clone(v));
                }
                all_installed.insert(pkgkey.to_string(), Arc::clone(pkg_info));
                drop(installed);
                if let Err(e) = rpm_triggers::run_rpm_package_triggers(
                    "triggerprein",
                    &all_installed,
                    &single_pkg,
                    &HashMap::new(),
                    &HashMap::new(),
                    store_root,
                    env_root,
                ) {
                    log::warn!("Failed to run RPM triggerprein triggers for {}: {}", pkgkey, e);
                }
            }

            // Pre-install scriptlet
            run_scriptlets(&single_pkg, plan, ScriptletType::PreInstall, is_upgrade)?;

            // DEB activate triggers - AFTER pre scriptlet
            if package_format == PackageFormat::Deb {
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

        PackageAction::LinkFiles => {
            // Level 3a: Install action
            // Files are already linked during download/unpack phase, but we may need to handle diff linking for upgrades
            if is_upgrade {
                if let (Some(old_key), Some(old_info)) = (old_pkgkey, old_pkg_info) {
                    // For upgrades, link new files and unlink old unique files
                    crate::link::unlink_package_diff(old_info, pkg_info, store_root, env_root)
                        .with_context(|| format!("Failed to unlink old package files for {}", old_key))?;
                }
            }
            // Note: Actual linking happens earlier in the download/unpack phase

            // DEB trigger handling (incorporate interests, build index, activate file triggers) - AFTER linking
            if package_format == PackageFormat::Deb {
                let packages = [(pkgkey, pkg_info.as_ref())];
                if let Err(e) = process_deb_triggers(&packages, store_root, env_root) {
                    log::warn!("Failed to process DEB triggers for {}: {}", pkgkey, e);
                }
            }

            // RPM file triggers (filetriggerin, high priority) - AFTER file linking, BEFORE postin
            if package_format == PackageFormat::Rpm {
                let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
                let mut all_installed: InstalledPackagesMap = HashMap::new();
                for (k, v) in installed.iter() {
                    all_installed.insert(k.clone(), Arc::clone(v));
                }
                all_installed.insert(pkgkey.to_string(), Arc::clone(pkg_info));
                drop(installed);
                let mut single_pkg = HashMap::new();
                single_pkg.insert(pkgkey.to_string(), pkg_info.clone());
                if let Err(e) = rpm_triggers::run_rpm_file_triggers(
                    "filetriggerin",
                    &all_installed,
                    &single_pkg,
                    &HashMap::new(),
                    &HashMap::new(),
                    store_root,
                    env_root,
                    1, // High priority (>= 10000)
                ) {
                    log::warn!("Failed to run RPM filetriggerin triggers (high priority) for {}: {}", pkgkey, e);
                }
            }
        }

        PackageAction::PostInstall => {
            // Level 3b: Scriptlet and trigger actions
            let mut single_pkg: InstalledPackagesMap = HashMap::new();
            single_pkg.insert(pkgkey.to_string(), Arc::clone(pkg_info));

            // Post-install scriptlet
            run_scriptlets(&single_pkg, plan, ScriptletType::PostInstall, is_upgrade)?;

            // RPM package triggers (triggerin) - AFTER postin
            if package_format == PackageFormat::Rpm {
                let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
                let mut all_installed: InstalledPackagesMap = HashMap::new();
                for (k, v) in installed.iter() {
                    all_installed.insert(k.clone(), Arc::clone(v));
                }
                all_installed.insert(pkgkey.to_string(), Arc::clone(pkg_info));
                drop(installed);
                if let Err(e) = rpm_triggers::run_rpm_package_triggers(
                    "triggerin",
                    &all_installed,
                    &single_pkg,
                    &HashMap::new(),
                    &HashMap::new(),
                    store_root,
                    env_root,
                ) {
                    log::warn!("Failed to run RPM triggerin triggers for {}: {}", pkgkey, e);
                }
            }

            // RPM file triggers (filetriggerin, low priority) - AFTER postin
            if package_format == PackageFormat::Rpm {
                let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
                let mut all_installed: InstalledPackagesMap = HashMap::new();
                for (k, v) in installed.iter() {
                    all_installed.insert(k.clone(), Arc::clone(v));
                }
                all_installed.insert(pkgkey.to_string(), Arc::clone(pkg_info));
                drop(installed);
                if let Err(e) = rpm_triggers::run_rpm_file_triggers(
                    "filetriggerin",
                    &all_installed,
                    &single_pkg,
                    &HashMap::new(),
                    &HashMap::new(),
                    store_root,
                    env_root,
                    2, // Low priority (< 10000)
                ) {
                    log::warn!("Failed to run RPM filetriggerin triggers (low priority) for {}: {}", pkgkey, e);
                }
            }

            // DEB trigger incorporation and processing - AFTER postin
            if package_format == PackageFormat::Deb {
                let mut all_packages: InstalledPackagesMap = PACKAGE_CACHE.installed_packages.read().unwrap()
                    .iter()
                    .map(|(k, v)| (k.clone(), Arc::clone(v)))
                    .collect();
                all_packages.insert(pkgkey.to_string(), Arc::clone(pkg_info));

                // Incorporate triggers from Unincorp
                let incorporation_result = deb_triggers::incorporate_triggers(env_root, &all_packages, store_root)
                    .unwrap_or_else(|_| deb_triggers::TriggerIncorporationResult {
                        pending_triggers: HashMap::new(),
                        awaiting_packages: HashSet::new(),
                    });

                // Update package states based on incorporation results for ALL packages
                // Collect pkgkeys first to avoid borrow checker issues
                let mut pending_pkgkeys: Vec<(String, Vec<String>)> = Vec::new();
                for (pkgname, trigger_names) in &incorporation_result.pending_triggers {
                    if let Some((pkgkey, _)) = all_packages.iter()
                        .find(|(k, _)| package::pkgkey2pkgname(k).unwrap_or_default() == *pkgname) {
                        pending_pkgkeys.push((pkgkey.clone(), trigger_names.clone()));
                    }
                }
                for (pkgkey, trigger_names) in &pending_pkgkeys {
                    if let Some(info) = all_packages.get_mut(pkgkey) {
                        Arc::make_mut(info).pending_triggers = trigger_names.clone();
                    }
                    if let Some(info) = PACKAGE_CACHE.installed_packages.write().unwrap().get_mut(pkgkey) {
                        Arc::make_mut(info).pending_triggers = trigger_names.clone();
                    }
                }

                // Mark packages that should await trigger processing
                let mut awaiting_pkgkeys: Vec<String> = Vec::new();
                for pkgname in &incorporation_result.awaiting_packages {
                    if let Some((pkgkey, _)) = all_packages.iter()
                        .find(|(k, _)| package::pkgkey2pkgname(k).unwrap_or_default() == *pkgname) {
                        awaiting_pkgkeys.push(pkgkey.clone());
                    }
                }
                for pkgkey in &awaiting_pkgkeys {
                    if let Some(info) = all_packages.get_mut(pkgkey) {
                        Arc::make_mut(info).triggers_awaited = true;
                    }
                    if let Some(info) = PACKAGE_CACHE.installed_packages.write().unwrap().get_mut(pkgkey) {
                        Arc::make_mut(info).triggers_awaited = true;
                    }
                }

                // Process triggers for ALL packages with pending triggers (not just current package)
                // Reset cycle detection at start
                deb_triggers::reset_cycle_detection();

                // Collect pkgkeys first to avoid borrow checker issues
                let mut processing_queue: Vec<(String, String, Vec<String>)> = Vec::new();
                for (pkgkey, trigger_names) in &pending_pkgkeys {
                    let pkgname = package::pkgkey2pkgname(pkgkey).unwrap_or_default();
                    // Skip packages that are already in config-failed state
                    if let Some(info) = all_packages.get(pkgkey) {
                        if info.config_failed {
                            log::debug!("Skipping package {} - already in config-failed state", pkgname);
                            continue;
                        }
                    }
                    processing_queue.push((pkgkey.clone(), pkgname, trigger_names.clone()));
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
                        if let Some(info) = all_packages.get_mut(&cycle_pkgkey) {
                            let info_mut = Arc::make_mut(info);
                            info_mut.config_failed = true;
                            info_mut.pending_triggers.clear();
                        }
                        if let Some(info) = PACKAGE_CACHE.installed_packages.write().unwrap().get_mut(&cycle_pkgkey) {
                            let info_mut = Arc::make_mut(info);
                            info_mut.config_failed = true;
                            info_mut.pending_triggers.clear();
                        }
                        // Skip processing this package if it's the one we're marking as failed
                        if cycle_pkgkey == pkgkey {
                            continue;
                        }
                    }

                    if let Some(pkg_info_ref) = all_packages.get(&pkgkey).cloned() {
                        match deb_triggers::process_package_triggers(
                            &pkgkey,
                            &pkg_info_ref,
                            &trigger_names,
                            store_root,
                            env_root,
                        ) {
                            Ok(_) => {
                                // Clear pending triggers after successful processing
                                if let Some(info) = all_packages.get_mut(&pkgkey) {
                                    let info_mut = Arc::make_mut(info);
                                    info_mut.pending_triggers.clear();
                                    info_mut.config_failed = false;
                                }
                                if let Some(info) = PACKAGE_CACHE.installed_packages.write().unwrap().get_mut(&pkgkey) {
                                    let info_mut = Arc::make_mut(info);
                                    info_mut.pending_triggers.clear();
                                    info_mut.config_failed = false;
                                }
                                log::debug!("Successfully processed triggers for package {}", pkgname);
                            }
                            Err(e) => {
                                log::warn!("Failed to process triggers for package {}: {}", pkgname, e);
                                if let Some(info) = all_packages.get_mut(&pkgkey) {
                                    Arc::make_mut(info).config_failed = true;
                                }
                                if let Some(info) = PACKAGE_CACHE.installed_packages.write().unwrap().get_mut(&pkgkey) {
                                    Arc::make_mut(info).config_failed = true;
                                }
                            }
                        }
                    }
                }
            }
        }

        PackageAction::PreRemove => {
            // Level 3b: Trigger and scriptlet actions
            let mut single_pkg = HashMap::new();
            if let Some(old_info) = old_pkg_info {
                single_pkg.insert(pkgkey.to_string(), old_info.clone());
            } else {
                single_pkg.insert(pkgkey.to_string(), pkg_info.clone());
            }

            // RPM package triggers (triggerun) - BEFORE preun
            if package_format == PackageFormat::Rpm && is_upgrade {
                let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
                let mut all_installed = HashMap::new();
                for (k, v) in installed.iter() {
                    all_installed.insert(k.clone(), v.clone());
                }
                drop(installed);
                if let Err(e) = rpm_triggers::run_rpm_package_triggers(
                    "triggerun",
                    &all_installed,
                    &HashMap::new(),
                    &HashMap::new(),
                    &single_pkg,
                    store_root,
                    env_root,
                ) {
                    log::warn!("Failed to run RPM triggerun triggers during upgrade for {}: {}", pkgkey, e);
                }
            }

            // RPM file triggers (filetriggerun, high priority) - BEFORE preun
            if package_format == PackageFormat::Rpm && is_upgrade {
                let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
                let mut all_installed = HashMap::new();
                for (k, v) in installed.iter() {
                    all_installed.insert(k.clone(), v.clone());
                }
                drop(installed);
                if let Err(e) = rpm_triggers::run_rpm_file_triggers(
                    "filetriggerun",
                    &all_installed,
                    &HashMap::new(),
                    &HashMap::new(),
                    &single_pkg,
                    store_root,
                    env_root,
                    1, // High priority (>= 10000)
                ) {
                    log::warn!("Failed to run RPM filetriggerun triggers (high priority) during upgrade for {}: {}", pkgkey, e);
                }
            }

            // Pre-remove scriptlet
            run_scriptlets(&single_pkg, plan, ScriptletType::PreRemove, is_upgrade)?;

            // RPM file triggers (filetriggerun, low priority) - AFTER preun, BEFORE file removal
            if package_format == PackageFormat::Rpm && is_upgrade {
                let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
                let mut all_installed = HashMap::new();
                for (k, v) in installed.iter() {
                    all_installed.insert(k.clone(), v.clone());
                }
                drop(installed);
                if let Err(e) = rpm_triggers::run_rpm_file_triggers(
                    "filetriggerun",
                    &all_installed,
                    &HashMap::new(),
                    &HashMap::new(),
                    &single_pkg,
                    store_root,
                    env_root,
                    2, // Low priority (< 10000)
                ) {
                    log::warn!("Failed to run RPM filetriggerun triggers (low priority) during upgrade for {}: {}", pkgkey, e);
                }
            }
        }

        PackageAction::UnlinkFiles => {
            // Level 3a: Remove action
            if pkg_info.pkgline.is_empty() || pkg_info.pkgline.contains("/") || pkg_info.pkgline.contains("..") {
                log::error!("Invalid pkgline for {}: '{}'. Skipping unlink.", pkgkey, pkg_info.pkgline);
                return Err(eyre!("Invalid pkgline for {}: '{}'", pkgkey, pkg_info.pkgline));
            }
            let pkg_store_path = store_root.join(&pkg_info.pkgline);
            log::info!("Unlinking files for package: {} from store path {}", pkgkey, pkg_store_path.display());

            // Remove DEB trigger interests before unlinking
            if package_format == PackageFormat::Deb {
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
            }

            unlink_package(&pkg_store_path, &env_root.to_path_buf())
                .with_context(|| format!("Failed to unlink package {} (store path: {})", pkgkey, pkg_store_path.display()))?;
            PACKAGE_CACHE.installed_packages.write().unwrap().remove(pkgkey);
        }

        PackageAction::PostRemove => {
            // Level 3b: Scriptlet action
            let mut single_pkg = HashMap::new();
            if let Some(old_info) = old_pkg_info {
                single_pkg.insert(pkgkey.to_string(), old_info.clone());
            } else {
                single_pkg.insert(pkgkey.to_string(), pkg_info.clone());
            }
            run_scriptlets(&single_pkg, plan, ScriptletType::PostRemove, is_upgrade)?;

            // RPM package triggers (triggerpostun) for upgrade - AFTER postun
            if package_format == PackageFormat::Rpm && is_upgrade {
                let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
                let mut all_installed = HashMap::new();
                for (k, v) in installed.iter() {
                    all_installed.insert(k.clone(), v.clone());
                }
                drop(installed);
                if let Err(e) = rpm_triggers::run_rpm_package_triggers(
                    "triggerpostun",
                    &all_installed,
                    &HashMap::new(),
                    &HashMap::new(),
                    &single_pkg,
                    store_root,
                    env_root,
                ) {
                    log::warn!("Failed to run RPM triggerpostun triggers during upgrade for {}: {}", pkgkey, e);
                }
            }

            // Update rdepends
            for dep_on_key in &pkg_info.depends {
                let mut installed = PACKAGE_CACHE.installed_packages.write().unwrap();
                if let Some(dep_pkg_info_mut) = installed.get_mut(dep_on_key) {
                    Arc::make_mut(dep_pkg_info_mut).rdepends.retain(|r| r != pkgkey);
                }
            }

        }

        PackageAction::PreUpgrade => {
            // Level 3b: Trigger and scriptlet actions
            let mut single_pkg: InstalledPackagesMap = HashMap::new();
            single_pkg.insert(pkgkey.to_string(), Arc::clone(pkg_info));

            // RPM package triggers (triggerprein) for upgrade - BEFORE preupgrade
            if package_format == PackageFormat::Rpm {
                let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
                let mut all_installed: InstalledPackagesMap = HashMap::new();
                for (k, v) in installed.iter() {
                    all_installed.insert(k.clone(), Arc::clone(v));
                }
                all_installed.insert(pkgkey.to_string(), Arc::clone(pkg_info));
                drop(installed);
                if let Err(e) = rpm_triggers::run_rpm_package_triggers(
                    "triggerprein",
                    &all_installed,
                    &single_pkg,
                    &HashMap::new(),
                    &HashMap::new(),
                    store_root,
                    env_root,
                ) {
                    log::warn!("Failed to run RPM triggerprein triggers during upgrade for {}: {}", pkgkey, e);
                }
            }

            // Pre-upgrade scriptlet
            run_scriptlet(
                pkgkey,
                pkg_info.as_ref(),
                plan,
                ScriptletType::PreUpgrade,
                true, // is_upgrade
                old_version.as_deref(),
                new_version.as_deref(),
            )?;
        }

        PackageAction::PostUpgrade => {
            // Level 3b: Scriptlet and trigger actions
            let mut single_pkg: InstalledPackagesMap = HashMap::new();
            single_pkg.insert(pkgkey.to_string(), Arc::clone(pkg_info));

            // Post-upgrade scriptlet
            run_scriptlet(
                pkgkey,
                pkg_info.as_ref(),
                plan,
                ScriptletType::PostUpgrade,
                true, // is_upgrade
                old_version.as_deref(),
                new_version.as_deref(),
            )?;

            // RPM package triggers (triggerin) for upgrade - AFTER postupgrade
            if package_format == PackageFormat::Rpm {
                let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
                let mut all_installed: InstalledPackagesMap = HashMap::new();
                for (k, v) in installed.iter() {
                    all_installed.insert(k.clone(), Arc::clone(v));
                }
                all_installed.insert(pkgkey.to_string(), Arc::clone(pkg_info));
                drop(installed);
                if let Err(e) = rpm_triggers::run_rpm_package_triggers(
                    "triggerin",
                    &all_installed,
                    &single_pkg,
                    &HashMap::new(),
                    &HashMap::new(),
                    store_root,
                    env_root,
                ) {
                    log::warn!("Failed to run RPM triggerin triggers during upgrade for {}: {}", pkgkey, e);
                }
            }

            // DEB trigger handling for upgrades
            if package_format == PackageFormat::Deb {
                let packages = [(pkgkey, pkg_info.as_ref())];
                if let Err(e) = process_deb_triggers(&packages, store_root, env_root) {
                    log::warn!("Failed to process DEB triggers for upgrade {}: {}", pkgkey, e);
                }
            }
        }

        PackageAction::ExposeExecutables => {
            // Level 3a: Expose action
            // Note: Actual exposure is handled in execute_expose_operations
            // This action is a placeholder for the action sequence
            log::debug!("Expose executables action for {}", pkgkey);
        }

        PackageAction::UnexposeExecutables => {
            // Level 3a: Unexpose action
            // Note: Actual unexposure is handled in execute_unexpose_operations
            // This action is a placeholder for the action sequence
            log::debug!("Unexpose executables action for {}", pkgkey);
        }
    }
    Ok(())
}

/// Execute transaction scriptlets and triggers before file operations.
/// Runs %pretrans, %preuntrans, and transfiletriggerun scriptlets/triggers.
pub fn begin_transaction(
    plan: &InstallationPlan,
) -> Result<()> {
    let store_root = &plan.store_root;
    let env_root = &plan.env_root;
    let package_format = plan.package_format;
    let has_upgrades = !plan.upgrades_new.is_empty();
    // Execute transaction scriptlets at transaction boundaries (RPM behavior)
    // Order: %pretrans of new, then %preuntrans of old (before any file operations)
    if package_format == PackageFormat::Rpm {
        // %pretrans of packages being installed/upgraded
        let mut pretrans_packages = HashMap::new();
        for (k, v) in plan.fresh_installs.iter() {
            pretrans_packages.insert(k.clone(), v.clone());
        }
        for (k, v) in plan.upgrades_new.iter() {
            pretrans_packages.insert(k.clone(), v.clone());
        }
        if !pretrans_packages.is_empty() {
            if let Err(e) = scriptlets::run_scriptlets(
                &pretrans_packages,
                plan,
                scriptlets::ScriptletType::PreTrans,
                has_upgrades,
            ) {
                log::warn!("Failed to run %pretrans scriptlets: {}", e);
            }
        }

        // %preuntrans of packages being removed (runs after %pretrans, before removals)
        if !plan.old_removes.is_empty() {
            if let Err(e) = scriptlets::run_scriptlets(
                &plan.old_removes,
                plan,
                scriptlets::ScriptletType::PreUnTrans,
                false, // is_upgrade - removals are separate from upgrades
            ) {
                log::warn!("Failed to run %preuntrans scriptlets: {}", e);
            }
        }

        // RPM transaction file triggers (transfiletriggerun) - after %preuntrans, before removals
        // Runs ONCE per transaction for all matching removed files
        if !plan.old_removes.is_empty() {
            let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
            if let Err(e) = rpm_triggers::run_rpm_transaction_file_triggers(
                "transfiletriggerun",
                &installed,
                &HashMap::new(),
                &HashMap::new(),
                &plan.old_removes,
                store_root,
                env_root,
            ) {
                log::warn!("Failed to run RPM transfiletriggerun triggers: {}", e);
            }
        }
    }

    Ok(())
}

/// Execute transaction scriptlets and triggers after file operations.
/// Runs %posttrans, %postuntrans, transfiletriggerpostun, and transfiletriggerin scriptlets/triggers.
pub fn end_transaction(
    plan: &InstallationPlan,
) -> Result<()> {
    let store_root = &plan.store_root;
    let env_root = &plan.env_root;
    let package_format = plan.package_format;
    let has_upgrades = !plan.upgrades_new.is_empty();
    // Execute transaction scriptlets: %posttrans of packages being installed/upgraded
    // This runs AFTER all file operations complete (RPM behavior)
    if package_format == PackageFormat::Rpm {
        let mut posttrans_packages = HashMap::new();
        for (k, v) in plan.fresh_installs.iter() {
            posttrans_packages.insert(k.clone(), v.clone());
        }
        for (k, v) in plan.upgrades_new.iter() {
            posttrans_packages.insert(k.clone(), v.clone());
        }
        if !posttrans_packages.is_empty() {
            if let Err(e) = scriptlets::run_scriptlets(
                &posttrans_packages,
                plan,
                scriptlets::ScriptletType::PostTrans,
                has_upgrades,
            ) {
                log::warn!("Failed to run %posttrans scriptlets: {}", e);
            }
        }

        // Execute transaction scriptlets: %postuntrans of packages being removed
        // This runs AFTER %posttrans, AFTER uninstall transaction completes (RPM behavior)
        // Order: %posttrans → %postuntrans → %transfiletriggerpostun → %transfiletriggerin
        if !plan.old_removes.is_empty() {
            if let Err(e) = scriptlets::run_scriptlets(
                &plan.old_removes,
                plan,
                scriptlets::ScriptletType::PostUnTrans,
                false, // is_upgrade - removals are separate from upgrades
            ) {
                log::warn!("Failed to run %postuntrans scriptlets: {}", e);
            }
        }

        // RPM transaction file triggers (transfiletriggerpostun) - after %posttrans and %postuntrans
        // Order: %posttrans → %postuntrans → %transfiletriggerpostun → %transfiletriggerin
        if !plan.old_removes.is_empty() {
            let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
            if let Err(e) = rpm_triggers::run_rpm_transaction_file_triggers(
                "transfiletriggerpostun",
                &installed,
                &HashMap::new(),
                &HashMap::new(),
                &plan.old_removes,
                store_root,
                env_root,
            ) {
                log::warn!("Failed to run RPM transfiletriggerpostun triggers: {}", e);
            }
        }

        // RPM transaction file triggers (transfiletriggerin) - LAST, after %posttrans, %postuntrans, and %transfiletriggerpostun
        // Order: %posttrans → %postuntrans → %transfiletriggerpostun → %transfiletriggerin
        if !posttrans_packages.is_empty() {
            let mut all_installed: InstalledPackagesMap = HashMap::new();
            for (k, v) in PACKAGE_CACHE.installed_packages.read().unwrap().iter() {
                all_installed.insert(k.clone(), Arc::clone(v));
            }
            for (k, v) in posttrans_packages.iter() {
                all_installed.insert(k.clone(), Arc::clone(v));
            }
            if let Err(e) = rpm_triggers::run_rpm_transaction_file_triggers(
                "transfiletriggerin",
                &all_installed,
                &plan.fresh_installs,
                &plan.upgrades_new,
                &HashMap::new(),
                store_root,
                env_root,
            ) {
                log::warn!("Failed to run RPM transfiletriggerin triggers: {}", e);
            }
        }
    }

    Ok(())
}


/// Build maps for hooks from ordered_operations
/// Stores the maps in plan.batch members
fn build_completed_maps(
    plan: &mut InstallationPlan,
) {
    plan.fresh_installs_completed.clear();
    plan.upgrades_new_completed.clear();
    plan.upgrades_old_completed.clear();
    plan.old_removes_completed.clear();

    for op in &plan.ordered_operations {
        match op.op_type {
            OperationType::FreshInstall => {
                if let Some((pkgkey, _)) = &op.new_pkg {
                    if let Some(info) = plan.completed_packages.get(pkgkey) {
                        plan.fresh_installs_completed.insert(pkgkey.clone(), Arc::clone(info));
                    }
                }
            }
            OperationType::Upgrade => {
                if let (Some((new_pkgkey, _)), Some((old_pkgkey, old_info))) = (&op.new_pkg, &op.old_pkg) {
                    if let Some(new_info) = plan.completed_packages.get(new_pkgkey) {
                        plan.upgrades_new_completed.insert(new_pkgkey.clone(), Arc::clone(new_info));
                        plan.upgrades_old_completed.insert(old_pkgkey.clone(), Arc::clone(old_info));
                    }
                }
            }
            OperationType::Removal => {
                if let Some((pkgkey, pkg_info)) = &op.old_pkg {
                    plan.old_removes_completed.insert(pkgkey.clone(), Arc::clone(pkg_info));
                }
            }
        }
    }
}

/// Process each package operation in order (rpmtsProcess style)
fn process_package_operations(
    plan: &mut InstallationPlan,
) -> Result<()> {
    // Clone operations to avoid borrow checker issues
    let operations: Vec<_> = plan.ordered_operations.clone();
    for op in &operations {
        // Skip operations that don't have completed packages yet (for installs/upgrades)
        if let Some((new_pkgkey, _)) = &op.new_pkg {
            if !plan.completed_packages.contains_key(new_pkgkey) {
                // Package not yet completed (e.g., AUR packages being built)
                continue;
            }
        }

        // Level 1: Process package pair operation
        process_package_operation(plan, op)?;
    }
    Ok(())
}

/// Level 0: Transaction handler (rpmtsRun style)
/// Executes the entire transaction by processing all package operations in order
/// https://rpm-software-management.github.io/rpm/man/rpm-scriptlets.7#EXECUTION_ORDER
pub fn run_transaction_batch(
    plan: &mut InstallationPlan,
    completed_packages: &InstalledPackagesMap,
) -> Result<()> {
    let package_format = plan.package_format;

    // Store completed_packages in plan
    plan.completed_packages.clear();
    plan.completed_packages.extend(completed_packages.iter().map(|(k, v)| (k.clone(), Arc::clone(v))));

    // Load hooks for Arch Linux (Pacman format)
    let hooks = hooks::load_hooks(&plan.env_root, package_format);

    // Build maps for hooks from ordered_operations
    build_completed_maps(plan);

    // Run PreTransaction hooks
    hooks::run_hooks(hooks.as_deref(), plan, hooks::HookWhen::PreTransaction)?;

    // Process each package operation in order (rpmtsProcess style)
    process_package_operations(plan)?;

    // Run PostTransaction hooks
    hooks::run_hooks(hooks.as_deref(), plan, hooks::HookWhen::PostTransaction)?;

    // Run ldconfig if needed (after all package operations complete)
    run_ldconfig_if_needed(&plan.env_root)?;

    Ok(())
}

/// Level 1: Package pair operation handler (rpmtsProcess/rpmteProcess style)
/// Processes a single package operation by executing a sequence of actions
pub fn process_package_operation(
    plan: &mut InstallationPlan,
    op: &PackageOperation,
) -> Result<()> {
    let package_format = plan.package_format;
    match op.op_type {
        OperationType::FreshInstall => {
            if let Some((pkgkey, _)) = &op.new_pkg {
                if let Some(completed_info) = plan.completed_packages.get(pkgkey) {
                    let completed_info = Arc::clone(completed_info);
                    // Execute actions for fresh install
                    run_action(plan, PackageAction::PreInstall,   pkgkey, &completed_info, None, None)?;
                    run_action(plan, PackageAction::LinkFiles,    pkgkey, &completed_info, None, None)?;
                    run_action(plan, PackageAction::PostInstall,  pkgkey, &completed_info, None, None)?;
                    if op.should_expose() {
                        run_action(plan, PackageAction::ExposeExecutables, pkgkey, &completed_info, None, None)?;
                    }
                }
            }
        }
        OperationType::Upgrade => {
            if let (Some((new_pkgkey, _)), Some((old_pkgkey, old_info))) = (&op.new_pkg, &op.old_pkg) {
                if let Some(new_info) = plan.completed_packages.get(new_pkgkey) {
                    let new_info = Arc::clone(new_info);
                    let old_info = Arc::clone(old_info);
                    match package_format {
                        PackageFormat::Rpm | PackageFormat::Conda => {
                            // Order matches RPM scriptlet execution sequence https://rpm-software-management.github.io/rpm/man/rpm-scriptlets.7
                            // Also matches conda's execution order (conda/core/link.py _execute method)
                            // 1. %pre of _new_     | pre-link of _new_     (PreInstall)
                            // 2. (unpack _new_ files)                      (LinkFiles)
                            // 3. %post of _new_    | post-link of _new_    (PostInstall)
                            //
                            // 4. %preun of _old_   | pre-unlink of _old_   (PreRemove)
                            // 5. (erase _old_ files)                       (UnlinkFiles)
                            // 6. %postun of _old_  | post-unlink of _old_  (PostRemove)
                            run_action(plan, PackageAction::PreInstall,   new_pkgkey, &new_info, Some(old_pkgkey), Some(&old_info))?;
                            run_action(plan, PackageAction::LinkFiles,    new_pkgkey, &new_info, Some(old_pkgkey), Some(&old_info))?;
                            run_action(plan, PackageAction::PostInstall,  new_pkgkey, &new_info, Some(old_pkgkey), Some(&old_info))?;

                            run_action(plan, PackageAction::PreRemove,    old_pkgkey, &old_info, Some(new_pkgkey), Some(&new_info))?;
                            run_action(plan, PackageAction::UnlinkFiles,  old_pkgkey, &old_info, Some(new_pkgkey), Some(&new_info))?;
                            run_action(plan, PackageAction::PostRemove,   old_pkgkey, &old_info, Some(new_pkgkey), Some(&new_info))?;
                        },
                        PackageFormat::Deb => {
                            // Order matches Debian maintainer script execution sequence https://www.debian.org/doc/debian-policy/ch-maintainerscripts.html
                            // Based on dpkg/src/main/unpack.c process_archive() and configure.c:
                            // 1. prerm of _old_         (PreRemove) with "upgrade new-version"
                            //    - Called at line 1459: maintscript_run_old_or_new(..., PRERMFILE, "upgrade", ...)
                            //    - Args: $1="upgrade", $2=new_version (from script.c:347-348)
                            // 2. preinst of _new_       (PreInstall) with "upgrade old-version new-version"
                            //    - Called at line 1520: maintscript_run_new(..., PREINSTFILE, "upgrade", old_ver, new_ver, NULL)
                            //    - Args: $1="upgrade", $2=old_version, $3=new_version
                            // 3. (unpack _new_ files)   (LinkFiles) - extracts new files, also calls unlink_package_diff()
                            //    - Unpacking happens at lines 1614-1650
                            //    - unlink_package_diff() called here (equivalent to pkg_remove_old_files() at line 1686)
                            // 4. postrm of _old_        (PostRemove) with "upgrade new-version"
                            //    - Called at line 1663: maintscript_run_old_or_new(..., POSTRMFILE, "upgrade", ...)
                            //    - Args: $1="upgrade", $2=new_version (only if old was HALFINSTALLED/UNPACKED, but we always call it)
                            // 5. (unlink _old_ files)   (UnlinkFiles) - completely unlink old package from environment
                            //    - Needed for store-based system, not in dpkg (dpkg replaces files during unpack)
                            // 6. postinst of _new_      (PostInstall) with "configure old-version"
                            //    - Called in configure.c:679: maintscript_postinst(..., "configure", old_version, NULL)
                            //    - Args: $1="configure", $2=old_version
                            run_action(plan, PackageAction::PreRemove,    old_pkgkey, &old_info, Some(new_pkgkey), Some(&new_info))?;
                            run_action(plan, PackageAction::PreInstall,   new_pkgkey, &new_info, Some(old_pkgkey), Some(&old_info))?;
                            run_action(plan, PackageAction::LinkFiles,    new_pkgkey, &new_info, Some(old_pkgkey), Some(&old_info))?;
                            run_action(plan, PackageAction::PostRemove,   old_pkgkey, &old_info, Some(new_pkgkey), Some(&new_info))?;
                            run_action(plan, PackageAction::UnlinkFiles,  old_pkgkey, &old_info, Some(new_pkgkey), Some(&new_info))?;
                            run_action(plan, PackageAction::PostInstall,  new_pkgkey, &new_info, Some(old_pkgkey), Some(&old_info))?;
                        },
                        PackageFormat::Pacman | PackageFormat::Apk => {
                            // Archlinux https://man.archlinux.org/man/PKGBUILD.5#INSTALL/UPGRADE/REMOVE_SCRIPTING
                            // Alpine https://wiki.alpinelinux.org/wiki/APKBUILD_Reference
                            run_action(plan, PackageAction::PreUpgrade,   new_pkgkey, &new_info, Some(old_pkgkey), Some(&old_info))?;
                            run_action(plan, PackageAction::LinkFiles,    new_pkgkey, &new_info, Some(old_pkgkey), Some(&old_info))?;
                            run_action(plan, PackageAction::UnlinkFiles,  old_pkgkey, &old_info, Some(new_pkgkey), Some(&new_info))?;
                            run_action(plan, PackageAction::PostUpgrade,  new_pkgkey, &new_info, Some(old_pkgkey), Some(&old_info))?;
                        },
                        PackageFormat::Epkg | PackageFormat::Python => { todo!() },
                    }

                    if op.should_unexpose() {
                        run_action(plan, PackageAction::UnexposeExecutables, old_pkgkey, &old_info, None, None)?;
                    }
                    if op.should_expose() {
                        run_action(plan, PackageAction::ExposeExecutables, new_pkgkey, &new_info, None, None)?;
                    }
                }
            }
        }
        OperationType::Removal => {
            if let Some((pkgkey, pkg_info)) = &op.old_pkg {
                // Execute actions for removal
                run_action(plan, PackageAction::PreRemove,        pkgkey, pkg_info, None, None)?;
                run_action(plan, PackageAction::UnlinkFiles,      pkgkey, pkg_info, None, None)?;
                run_action(plan, PackageAction::PostRemove,       pkgkey, pkg_info, None, None)?;
                if op.should_unexpose() {
                    run_action(plan, PackageAction::UnexposeExecutables, pkgkey, pkg_info, None, None)?;
                }
            }
        }
    }
    Ok(())
}

/// Run ldconfig if the library cache needs updating
/// Called after all package operations complete
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

