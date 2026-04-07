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

#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
#[cfg(unix)]
use std::time::SystemTime;
use color_eyre::Result;
use std::sync::Arc;
#[cfg(unix)]
use color_eyre::eyre::WrapErr;
#[cfg(unix)]
use crate::lfs;
use crate::models::{PackageFormat, InstalledPackageInfo};
use crate::models::PACKAGE_CACHE;
use crate::plan::{InstallationPlan, PackageOperation, OperationType, remove_package_from_cache};
use crate::hooks;
use crate::hooks::{run_hooks, run_pkgkey_hooks_pair, HookWhen};
#[cfg(unix)]
use crate::utils;
use crate::scriptlets::{run_scriptlet, run_trans_scriptlets, ScriptletType};
#[cfg(unix)]
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
    let store_root = plan.store_root.clone();
    let env_root = plan.env_root.clone();

    match action {
        PackageAction::PreInstall => {
            run_pkgkey_hooks_pair(plan, HookWhen::PreInstall, pkgkey)?;
            run_scriptlet(plan,    ScriptletType::PreInstall, pkgkey, pkg_info.as_ref(), old_pkgkey)?;
            run_pkgkey_hooks_pair(plan, HookWhen::PreInstall2, pkgkey)?;
        }

        PackageAction::LinkFiles => {
            // Files are already linked during download/unpack phase, but we may need to handle diff linking for upgrades
            let new_files_union = &plan.batch.new_files;
            crate::link::unlink_package_diff(old_pkgkey, old_pkg_info, pkg_info, &store_root, &env_root, new_files_union)?;
            // Note: Actual linking happens earlier in the download/unpack phase

            PACKAGE_CACHE.installed_packages.write().unwrap().insert(pkgkey.to_string(), Arc::clone(pkg_info));
            PACKAGE_CACHE.pkgline2installed.write().unwrap().insert(pkg_info.pkgline.clone(), Arc::clone(pkg_info));
        }

        PackageAction::PostInstall => {
            run_scriptlet(plan, ScriptletType::PostInstall, pkgkey, pkg_info.as_ref(), old_pkgkey)?;
            run_pkgkey_hooks_pair(plan, HookWhen::PostInstall, pkgkey)?;
            run_pkgkey_hooks_pair(plan, HookWhen::PostInstall2, pkgkey)?;

            // DEB trigger processing: noawait triggers from Unincorp (immediate, per-package processing)
            crate::deb_triggers::run_debian_unincorp_triggers(plan, HookWhen::PostInstall)?;
        }

        PackageAction::PreRemove => {
            run_pkgkey_hooks_pair(plan, HookWhen::PreRemove, pkgkey)?;
            run_scriptlet(plan,    ScriptletType::PreRemove, pkgkey, pkg_info.as_ref(), old_pkgkey)?;
            run_pkgkey_hooks_pair(plan, HookWhen::PreRemove2, pkgkey)?;
        }

        PackageAction::UnlinkFiles => {
            if old_pkgkey.is_none() {   // Upgrade is handled by unlink_package_diff()
                unlink_package(pkgkey, &pkg_info.pkgline, &store_root, &env_root)?;
            }
            remove_package_from_cache(pkgkey, pkg_info);
        }

        PackageAction::PostRemove => {
            run_scriptlet(plan,    ScriptletType::PostRemove, pkgkey, pkg_info.as_ref(), old_pkgkey)?;
            run_pkgkey_hooks_pair(plan, HookWhen::PostRemove, pkgkey)?;
            run_pkgkey_hooks_pair(plan, HookWhen::PostRemove2, pkgkey)?;
        }

        PackageAction::PreUpgrade => {
            run_pkgkey_hooks_pair(plan, HookWhen::PreUpgrade, pkgkey)?;
            run_scriptlet(plan,    ScriptletType::PreUpgrade, pkgkey, pkg_info.as_ref(), old_pkgkey)?;
        }

        PackageAction::PostUpgrade => {
            run_scriptlet(plan,    ScriptletType::PostUpgrade, pkgkey, pkg_info.as_ref(), old_pkgkey)?;
            run_pkgkey_hooks_pair(plan, HookWhen::PostUpgrade, pkgkey)?;
        }

        PackageAction::ExposeExecutables => {
            let store_fs_dir = plan.store_root.join(&pkg_info.pkgline).join("fs");
            crate::expose::expose_package(plan, &store_fs_dir, &pkgkey)?;
        }

        PackageAction::UnexposeExecutables => {
            crate::expose::unexpose_package(plan, &env_root, pkgkey)?;
        }
    }
    Ok(())
}

/// Execute transaction scriptlets and triggers before file operations.
/// Runs %pretrans, %preuntrans, and transfiletriggerun scriptlets/triggers.
fn begin_transaction(
    plan: &InstallationPlan,
) -> Result<()> {
    let package_format = plan.package_format;
    // Execute transaction scriptlets at transaction boundaries (RPM behavior)
    // Order: %pretrans of new, then %preuntrans of old (before any file operations)
    if package_format == PackageFormat::Rpm {
        // %pretrans of packages being installed/upgraded
        run_trans_scriptlets(plan, ScriptletType::PreTrans)?;

        // %preuntrans of packages being removed (runs after %pretrans, before removals)
        run_trans_scriptlets(plan, ScriptletType::PreUnTrans)?;
    }

    // Hook: PreTransaction
    run_hooks(plan, HookWhen::PreTransaction)?;

    Ok(())
}

/// Execute transaction scriptlets and triggers after file operations.
/// Runs %posttrans, %postuntrans, transfiletriggerpostun, and transfiletriggerin scriptlets/triggers.
fn end_transaction(
    plan: &mut InstallationPlan,
) -> Result<()> {
    let package_format = plan.package_format;

    // Execute transaction scriptlets: %posttrans of packages being installed/upgraded
    // This runs AFTER all file operations complete (RPM behavior)
    if package_format == PackageFormat::Rpm {
        // %posttrans of packages being installed/upgraded
        run_trans_scriptlets(plan, ScriptletType::PostTrans)?;

        // Execute transaction scriptlets: %postuntrans of packages being removed
        // This runs AFTER %posttrans, AFTER uninstall transaction completes (RPM behavior)
        // Order: %posttrans → %postuntrans → %transfiletriggerpostun → %transfiletriggerin
        run_trans_scriptlets(plan, ScriptletType::PostUnTrans)?;
    }

    // Hooks: PostUnTrans then PostTransaction
    run_hooks(plan, HookWhen::PostUnTrans)?;
    run_hooks(plan, HookWhen::PostTransaction)?;

    // DEB trigger processing: await triggers from Unincorp (batched, after all packages are processed)
    crate::deb_triggers::run_debian_unincorp_triggers(plan, HookWhen::PostTransaction)?;

    Ok(())
}


/// Build maps for hooks from ordered_operations
/// Stores the maps in plan.batch members
fn build_batch_maps(plan: &mut InstallationPlan) {
    plan.batch.fresh_installs.clear();
    plan.batch.upgrades_new.clear();
    plan.batch.upgrades_old.clear();
    plan.batch.old_removes.clear();

    for op in &plan.ordered_operations {
        match op.op_type {
            OperationType::FreshInstall => {
                if let Some(pkgkey) = &op.new_pkgkey {
                    if plan.batch.new_pkgkeys.contains(pkgkey) {
                        plan.batch.fresh_installs.insert(pkgkey.clone());
                    }
                }
            }
            OperationType::Upgrade => {
                if let (Some(new_pkgkey), Some(old_pkgkey)) = (&op.new_pkgkey, &op.old_pkgkey) {
                    if plan.batch.new_pkgkeys.contains(new_pkgkey) {
                        plan.batch.upgrades_new.insert(new_pkgkey.clone());
                        plan.batch.upgrades_old.insert(old_pkgkey.clone());
                    }
                }
            }
            OperationType::Removal => {
                if let Some(pkgkey) = &op.old_pkgkey {
                    plan.batch.old_removes.insert(pkgkey.clone());
                }
            }
        }
    }
}

/// Build union of all files from new packages in batch for diff calculation during upgrades
fn build_batch_file_union(plan: &mut InstallationPlan) -> Result<()> {
    plan.batch.new_files.clear();
    for pkgkey in &plan.batch.new_pkgkeys {
        if let Some(package_info) = crate::plan::pkgkey2new_pkg_info(plan, pkgkey) {
            match crate::package_cache::map_pkgline2filelist(&plan.store_root, &package_info.pkgline) {
                Ok(files) => {
                    for file in files {
                        plan.batch.new_files.insert(PathBuf::from(file));
                    }
                }
                Err(e) => {
                    log::warn!("Failed to get file list for {} (pkgline {}): {}", pkgkey, package_info.pkgline, e);
                }
            }
        }
    }
    log::debug!("Batch file union contains {} files from {} packages", plan.batch.new_files.len(), plan.batch.new_pkgkeys.len());
    Ok(())
}

/// Level 0: Transaction handler (rpmtsRun style)
/// Executes the entire transaction by processing all package operations in order
/// https://rpm-software-management.github.io/rpm/man/rpm-scriptlets.7#EXECUTION_ORDER
pub fn run_transaction_batch(
    plan: &mut InstallationPlan,
) -> Result<()> {
    // Build maps for hooks from ordered_operations
    build_batch_maps(plan);
    // Build union of all files from new packages in batch for diff calculation during upgrades
    build_batch_file_union(plan)?;

    // Setup tool wrappers for newly installed tools (after new_files is populated)
    crate::tool_wrapper::setup_tool_wrappers(plan)?;

    // Execute transaction scriptlets at transaction boundaries (RPM behavior)
    begin_transaction(&plan)?;

    // Load hooks for batch packages (incremental loading)
    hooks::load_batch_hooks(plan)?;

    // Load Debian triggers for batch packages (incremental loading)
    crate::deb_triggers::load_batch_deb_triggers(plan)?;

    // Run PreTransaction hooks
    run_hooks(plan, HookWhen::PreTransaction)?;

    // Process each package operation in order (rpmtsProcess style)
    log::debug!("run_transaction_batch: starting process_package_operations");
    process_package_operations(plan)?;
    log::debug!("run_transaction_batch: process_package_operations completed, now running PostTransaction hooks");

    // Run PostTransaction hooks
    log::debug!("run_transaction_batch: about to run PostTransaction hooks");
    run_hooks(plan, HookWhen::PostTransaction)?;
    log::debug!("run_transaction_batch: PostTransaction hooks completed");

    // Run ldconfig if needed (after all package operations complete) - Unix only
    #[cfg(unix)]
    run_ldconfig_if_needed(&plan.env_root)?;

    // Execute transaction scriptlets: %posttrans of packages being installed/upgraded
    // This runs AFTER all file operations complete (RPM behavior)
    end_transaction(plan)?;

    #[cfg(feature = "libkrun")]
    {
        crate::libkrun::shutdown_vm_reuse_session_if_active()?;
    }

    // Follow-up batches will see is_first=false
    plan.batch.is_first = false;

    log::debug!("run_transaction_batch: completed successfully");
    Ok(())
}

/// Process each package operation in order (rpmtsProcess style)
fn process_package_operations(
    plan: &mut InstallationPlan,
) -> Result<()> {
    // Clone operations to avoid borrow checker issues
    let operations: Vec<_> = plan.ordered_operations.clone();
    for op in &operations {
        // Skip operations that don't have completed packages yet (for installs/upgrades)
        if let Some(new_pkgkey) = &op.new_pkgkey {
            if !plan.batch.new_pkgkeys.contains(new_pkgkey) {
                // Package not yet completed (e.g., AUR packages being built)
                continue;
            }
        }

        // Level 1: Process package pair operation
        process_package_operation(plan, op)?;
    }
    log::debug!("process_package_operations: completed {} operations", operations.len());
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
            if let Some(pkgkey) = &op.new_pkgkey {
                if plan.batch.new_pkgkeys.contains(pkgkey) {
                    if let Some(completed_info) = crate::plan::pkgkey2new_pkg_info(plan, pkgkey) {
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
        }
        OperationType::Upgrade => {
            if let (Some(new_pkgkey), Some(old_pkgkey)) = (&op.new_pkgkey, &op.old_pkgkey) {
                if plan.batch.new_pkgkeys.contains(new_pkgkey) {
                    if let Some(new_info) = crate::plan::pkgkey2new_pkg_info(plan, new_pkgkey) {
                        if let Some(old_info) = crate::plan::pkgkey2installed_pkg_info(old_pkgkey) {
                            if op.should_unexpose() {
                                run_action(plan, PackageAction::UnexposeExecutables, old_pkgkey, &old_info, None, None)?;
                            }
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
                                PackageFormat::Epkg | PackageFormat::Python | PackageFormat::Brew => { todo!() },
                            }

                            if op.should_expose() {
                                run_action(plan, PackageAction::ExposeExecutables, new_pkgkey, &new_info, None, None)?;
                            }
                        }
                    }
                }
            }
        }
        OperationType::Removal => {
            if let Some(pkgkey) = &op.old_pkgkey {
                if let Some(pkg_info) = crate::plan::pkgkey2installed_pkg_info(pkgkey) {
                    if op.should_unexpose() {
                        run_action(plan, PackageAction::UnexposeExecutables, pkgkey, &pkg_info, None, None)?;
                    }
                    // Execute actions for removal
                    run_action(plan, PackageAction::PreRemove,        pkgkey, &pkg_info, None, None)?;
                    run_action(plan, PackageAction::UnlinkFiles,      pkgkey, &pkg_info, None, None)?;
                    run_action(plan, PackageAction::PostRemove,       pkgkey, &pkg_info, None, None)?;
                    crate::deb_triggers::run_debian_unincorp_triggers(plan, HookWhen::PostInstall)?;
                }
            }
        }
    }
    Ok(())
}

/// Run ldconfig if the library cache needs updating
/// Called after all package operations complete
#[cfg(unix)]
fn run_ldconfig_if_needed(env_root: &Path) -> Result<()> {
    let ld_so_cache = crate::dirs::path_join(env_root, &["etc", "ld.so.cache"]);
    let lib_dirs = [
        crate::dirs::path_join(env_root, &["etc", "ld.so.conf.d"]),
        env_root.join("lib"),
        env_root.join("lib64"),
        crate::dirs::path_join(env_root, &["usr", "lib"]),
        crate::dirs::path_join(env_root, &["usr", "lib64"]),
    ];

    // Get mtime of ld.so.cache if it exists
    let cache_mtime = if lfs::exists_or_any_symlink(&ld_so_cache) {
        lfs::symlink_metadata(&ld_so_cache)
            .with_context(|| format!("Failed to get metadata for {}", ld_so_cache.display()))?
            .modified()
            .with_context(|| format!("Failed to get modification time for {}", ld_so_cache.display()))?
    } else {
        // If cache doesn't exist, we need to run ldconfig
        SystemTime::UNIX_EPOCH
    };

    // Check if any lib directory has been modified more recently than the cache
    let needs_update = lib_dirs.iter().any(|dir| {
        if !lfs::exists_or_any_symlink(dir) {
            return false;
        }
        match lfs::symlink_metadata(dir) {
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
                // Inherit VM settings from active VM reuse session during install/upgrade.
                // This allows ldconfig to reuse the same VM that was created for the main command.
                // The VM reuse logic is handled in fork_and_execute() -> prepare_run_options_for_command().
                let mut run_options = run::RunOptions {
                    command: ldconfig_path.to_string_lossy().to_string(),
                    no_exit: true,
                    chdir_to_env_root: true, // ldconfig should run relative to environment root
                    ..Default::default()
                };

                // Nested under `epkg run --isolate=vm` (e.g. e2e): libc::clone with namespace flags
                // can return EPERM; ldconfig only needs the env tree, not an extra namespace.
                if utils::e2e_backend_is_vm() {
                    run_options.skip_namespace_isolation = true;
                }

                // Execute ldconfig
                run::fork_and_execute(env_root, &run_options)?;
            }
            Err(_) => {
                log::info!("ldconfig command not found in environment, skipping library cache update");
            }
        }
    } else {
        log::debug!("Library cache is up to date, skipping ldconfig");
    }

    Ok(())
}

