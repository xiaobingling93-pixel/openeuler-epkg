use std::path::Path;
use std::collections::HashMap;

use color_eyre::eyre::{self, Result, WrapErr, eyre};
use crate::models::*;
use crate::models;
use crate::dirs;
use crate::mmio;
use crate::store;
use crate::utils;
use crate::package;
use crate::download;
use crate::scriptlets;
use crate::rpm_triggers;
use crate::plan::InstallationPlan;
use crate::models::PACKAGE_CACHE;
use crate::link::compute_link_type_and_reflink;
use crate::io::{load_world, save_installed_packages, save_world};
use crate::world::{apply_no_install_changes, apply_delta_world, add_essential_packages_to_delta_world, create_delta_world_from_specs};
use crate::depends::resolve_and_install_packages;
use crate::risks::validate_installation_plan;
use crate::plan::prompt_and_confirm_install_plan;
use crate::history::{create_new_generation_with_root, record_history, update_current_generation_symlink_with_root};
use crate::steps::{execute_removals, process_installation_results};
use crate::expose::{execute_unexpose_operations, execute_expose_operations};
use crate::aur::{build_and_install_aur_packages, is_aur_package};
use crate::download::{enqueue_package_downloads, get_package_file_path};

/// Installs specified packages and their dependencies.
pub fn install_packages(package_specs: Vec<String>) -> Result<InstallationPlan> {
    load_world()?;

    // Apply no-install changes from CLI to world.json
    apply_no_install_changes()?;

    // Process package specs: handle local files/URLs, return all package specs ready for installation
    let processed_specs = process_url_package_specs(package_specs)?;

    // Create delta_world from processed specs (in case local files were converted to specs)
    let mut delta_world = create_delta_world_from_specs(&processed_specs);
    apply_delta_world(&delta_world);

    // Prepare user_request_world BEFORE adding essential packages
    // user_request_world should only contain explicitly user-requested packages, not essential ones
    // user_request_world will be used for setting ebin_exposure
    let user_request_world = Some(delta_world.clone());

    // Add essential packages to delta_world if not already in world
    // this extended delta_world won't be saved to disk
    if !models::config().install.no_install_essentials {
        add_essential_packages_to_delta_world(&mut delta_world)?;
    }

    resolve_and_install_packages(
        &mut delta_world,
        user_request_world.as_ref(),
    )
}

// ============================================================================
// Package spec handling:
// - remote url
// - local package file
// ============================================================================

/// Process package specs: separate regular specs from files/URLs, download URLs, process local files
fn process_url_package_specs(package_specs: Vec<String>) -> Result<Vec<String>> {
    use store::detect_package_format;
    use crate::mirror::{Mirrors, UrlProtocol};

    // Separate package specs into regular package names and local files/URLs
    let mut regular_specs = Vec::new();
    let mut remote_urls = Vec::new();
    let mut local_files = Vec::new();

    for spec in package_specs {
        // Use detect_url_proto_path to determine if it's a remote URL or local path
        match Mirrors::detect_url_proto_path(&spec, "") {
            Ok((UrlProtocol::Http, _)) => {
                // It's a remote URL (HTTP/HTTPS or special patterns)
                remote_urls.push(spec);
            }
            Ok((UrlProtocol::Local, local_path)) => {
                // It's a local path - verify it's a valid package file
                let path = std::path::Path::new(&local_path);
                if path.exists() && path.is_file() {
                    // Use detect_package_format to check if it's a supported package file
                    if detect_package_format(path).is_ok() {
                        local_files.push(local_path.to_string_lossy().to_string());
                    } else {
                        regular_specs.push(spec);
                    }
                } else {
                    regular_specs.push(spec);
                }
            }
            Err(_) => {
                // Neither remote nor local - treat as regular spec
                regular_specs.push(spec);
            }
        }
    }

    // Download all remote URLs in parallel (if any)
    if !remote_urls.is_empty() {
        use download::download_urls;
        let download_results = download_urls(remote_urls);
        for result in download_results {
            match result {
                Ok(path) => local_files.push(path),
                Err(e) => return Err(e).with_context(|| format!("Failed to download remote package URLs")),
            }
        }
    }

    // Process local package files (unpack, load metadata, add to cache)
    if !local_files.is_empty() {
        let package_specs_from_files = process_local_package_files(local_files)?;
        regular_specs.extend(package_specs_from_files);
    }

    Ok(regular_specs)
}

/// Process local package files: unpack packages, load metadata, and add to cache
/// Returns package specs that can be used with install_packages()
fn process_local_package_files(local_files: Vec<String>) -> Result<Vec<String>> {
    use std::sync::Arc;
    use store::detect_package_format;
    use mmio::deserialize_package;

    let mut package_specs = Vec::new();

    // Process each local package file
    for package_file in local_files {
        // Verify the file exists and is a package file
        let path = std::path::Path::new(&package_file);
        if !path.exists() {
            return Err(eyre::eyre!("Package file not found: {}", package_file));
        }
        if !path.is_file() {
            return Err(eyre::eyre!("Path is not a file: {}", package_file));
        }

        // Unpack the package to the store
        let final_dir = store::unpack_mv_package(&package_file, None, None)
            .with_context(|| format!("Failed to unpack package: {}", package_file))?;

        // Extract caHash from the pkgline (directory name)
        // Format: {ca_hash}__{pkgname}__{version}__{arch}
        let pkgline = final_dir.file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| eyre::eyre!("Invalid package directory name: {}", final_dir.display()))?;
        let parsed_pkgline = package::parse_pkgline(pkgline)
            .with_context(|| format!("Failed to parse pkgline: {}", pkgline))?;

        // Read package.txt from the unpacked package
        let package_txt_path = final_dir.join("info/package.txt");
        if !package_txt_path.exists() {
            return Err(eyre::eyre!("Package metadata not found: {}", package_txt_path.display()));
        }

        let package_content = std::fs::read_to_string(&package_txt_path)
            .with_context(|| format!("Failed to read package.txt: {}", package_txt_path.display()))?;

        // Deserialize package metadata
        let mut package = deserialize_package(&package_content)
            .with_context(|| format!("Failed to deserialize package from: {}", package_txt_path.display()))?;

        // Set caHash from the parsed pkgline (unpack_mv_package ensures it's in the directory name)
        package.ca_hash = Some(parsed_pkgline.ca_hash.clone());

        // Set repodata_name to "local" for locally installed packages
        package.repodata_name = "local".to_string();

        // Set location to the absolute local file path so install_packages can use it
        let abs_path = std::path::Path::new(&package_file).canonicalize()
            .with_context(|| format!("Failed to canonicalize path: {}", package_file))?;
        package.location = abs_path.to_string_lossy().to_string();
        package.package_baseurl = String::new(); // Empty baseurl indicates local file

        // Generate pkgkey from package metadata (using caHash from pkgline)
        package.pkgkey = package::pkgline2pkgkey(pkgline)
            .unwrap_or_else(|_| format!("{}={}", package.pkgname, package.version));

        // Detect package format from file extension
        let format = detect_package_format(std::path::Path::new(&package_file))
            .with_context(|| format!("Failed to detect package format for: {}", package_file))?;

        // Add package to cache
        crate::package_cache::add_package_to_cache(Arc::new(package.clone()), format);

        // Create package spec (pkgname=version)
        let spec = format!("{}={}", package.pkgname, package.version);
        package_specs.push(spec);
    }

    Ok(package_specs)
}

/// Execute an InstallationPlan by performing the actual installation/removal operations.
/// This function can be reused by both install and remove operations.
/// If config().common.dry_run is true, will return the plan without executing it.
/// The target environment is determined by config().common.env.
pub fn execute_installation_plan(mut plan: InstallationPlan) -> Result<InstallationPlan> {
    // --- USER PROMPT AND PRE-EXECUTION CHECKS ---
    let go_on = prompt_and_confirm_install_plan(&plan)?;
    if !go_on {
        return Ok(plan);
    }

    if models::config().common.env.is_empty() {
        return Err(eyre::eyre!("Environment name not specified for installation plan"));
    }

    let env_root = dirs::get_default_env_root()?;
    let generations_root = dirs::get_default_generations_root()?;

    let new_generation = create_new_generation_with_root(&generations_root)?;
    let env_root = env_root.clone();
    let store_root = dirs().epkg_store.clone();

    // Load channel config from the environment
    let package_format = channel_config().format;

    // Copy link type from EnvConfig to InstallationPlan
    // Downgrade hardlink to symlink if store and env are on different filesystems
    // Check for reflink support if using hardlink
    // Fail early if Move or Runpath link types require cross-filesystem rename
    let (link_type, can_reflink) = compute_link_type_and_reflink(
        env_config().link,
        &store_root,
        &env_root,
    )?;
    plan.link = link_type;
    plan.can_reflink = can_reflink;

    // Validate transaction (disk space, conflicts, etc.) for RPM packages
    if package_format == PackageFormat::Rpm {
        if let Err(e) = validate_installation_plan(&plan, &store_root, &env_root) {
            log::warn!("Transaction validation failed: {}", e);
            // Continue anyway - validation is advisory for now
        }
    }

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
            let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
            if let Err(e) = scriptlets::run_scriptlets_with_context(
                &pretrans_packages,
                &store_root,
                &env_root,
                package_format,
                scriptlets::ScriptletType::PreTrans,
                !plan.upgrades_new.is_empty(), // is_upgrade if there are upgrades
                Some(&installed),
                Some(&plan.fresh_installs),
                Some(&plan.old_removes),
            ) {
                drop(installed);
                log::warn!("Failed to run %pretrans scriptlets: {}", e);
            }
        }

        // %preuntrans of packages being removed (runs after %pretrans, before removals)
        if !plan.old_removes.is_empty() {
            let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
            if let Err(e) = scriptlets::run_scriptlets_with_context(
                &plan.old_removes,
                &store_root,
                &env_root,
                package_format,
                scriptlets::ScriptletType::PreUnTrans,
                false, // is_upgrade - removals are separate from upgrades
                Some(&installed),
                Some(&plan.fresh_installs),
                Some(&plan.old_removes),
            ) {
                drop(installed);
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
                &store_root,
                &env_root,
            ) {
                log::warn!("Failed to run RPM transfiletriggerun triggers: {}", e);
            }
        }
    }

    // Execute removals
    execute_removals(&plan, &store_root, &env_root, package_format)?;

    // Execute installations and upgrades
    execute_installations(&mut plan, &store_root, &env_root, package_format)?;

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
            let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
            if let Err(e) = scriptlets::run_scriptlets_with_context(
                &posttrans_packages,
                &store_root,
                &env_root,
                package_format,
                scriptlets::ScriptletType::PostTrans,
                !plan.upgrades_new.is_empty(), // is_upgrade if there are upgrades
                Some(&installed),
                Some(&plan.fresh_installs),
                Some(&plan.old_removes),
            ) {
                drop(installed);
                log::warn!("Failed to run %posttrans scriptlets: {}", e);
            }
        }

        // Execute transaction scriptlets: %postuntrans of packages being removed
        // This runs AFTER %posttrans, AFTER uninstall transaction completes (RPM behavior)
        // Order: %posttrans → %postuntrans → %transfiletriggerpostun → %transfiletriggerin
        if !plan.old_removes.is_empty() {
            let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
            if let Err(e) = scriptlets::run_scriptlets_with_context(
                &plan.old_removes,
                &store_root,
                &env_root,
                package_format,
                scriptlets::ScriptletType::PostUnTrans,
                false, // is_upgrade - removals are separate from upgrades
                Some(&installed),
                Some(&plan.fresh_installs),
                Some(&plan.old_removes),
            ) {
                drop(installed);
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
                &store_root,
                &env_root,
            ) {
                log::warn!("Failed to run RPM transfiletriggerpostun triggers: {}", e);
            }
        }

        // RPM transaction file triggers (transfiletriggerin) - LAST, after %posttrans, %postuntrans, and %transfiletriggerpostun
        // Order: %posttrans → %postuntrans → %transfiletriggerpostun → %transfiletriggerin
        let mut posttrans_packages = HashMap::new();
        for (k, v) in plan.fresh_installs.iter() {
            posttrans_packages.insert(k.clone(), v.clone());
        }
        for (k, v) in plan.upgrades_new.iter() {
            posttrans_packages.insert(k.clone(), v.clone());
        }
        if !posttrans_packages.is_empty() {
            let mut all_installed = HashMap::new();
            for (k, v) in PACKAGE_CACHE.installed_packages.read().unwrap().iter() {
                all_installed.insert(k.clone(), v.clone());
            }
            for (k, v) in posttrans_packages.iter() {
                all_installed.insert(k.clone(), v.clone());
            }
            if let Err(e) = rpm_triggers::run_rpm_transaction_file_triggers(
                "transfiletriggerin",
                &all_installed,
                &plan.fresh_installs,
                &plan.upgrades_new,
                &HashMap::new(),
                &store_root,
                &env_root,
            ) {
                log::warn!("Failed to run RPM transfiletriggerin triggers: {}", e);
            }
        }
    }

    // Execute exposure changes
    execute_unexpose_operations(&plan, &env_root)?;
    execute_expose_operations(&plan, &store_root, &env_root)?;

    // Update metadata for skipped reinstalls (uses plan.skipped_reinstalls as the source
    // of session_info).
    update_skipped_reinstalls_metadata(&plan)?;

    record_history(&new_generation, Some(&plan))?;
    save_installed_packages(&new_generation)?;
    save_world(&new_generation)?;
    update_current_generation_symlink_with_root(&generations_root, new_generation)?;

    Ok(plan)
}

/// Execute package installations and upgrades
fn execute_installations(plan: &mut InstallationPlan, store_root: &Path, env_root: &Path, package_format: PackageFormat) -> Result<()> {
    if plan.fresh_installs.is_empty() && plan.upgrades_new.is_empty() {
        return Ok(());
    }

    // Remove old versions of upgraded packages from installed_packages *before* downloads
    let mut installed = PACKAGE_CACHE.installed_packages.write().unwrap();
    for (old_pkgkey_to_remove, _) in plan.upgrades_old.iter() {
        installed.remove(old_pkgkey_to_remove);
    }
    drop(installed);

    // Step 1: Prepare packages for download and processing
    let (packages_to_download_and_process, packages_with_pkglines) = prepare_packages_for_installation(plan)?;

    // Step 2: Download (binary + AUR) and unpack+link (binary) packages
    let (mut completed_packages, aur_packages) = download_and_install_packages(
        &packages_to_download_and_process,
        &packages_with_pkglines,
        store_root,
        env_root,
        plan.link,
        plan.can_reflink,
        &plan.store_pkglines_by_pkgname,
    )?;

    // Step 3: Process upgrades and fresh installations
    // Pass HashMap directly to process_installation_results
    process_installation_results(plan, &completed_packages, store_root, env_root, package_format)?;

    // Step 4: Build and install AUR packages (build with makepkg)
    if !aur_packages.is_empty() {
        let aur_completed = build_and_install_aur_packages(&aur_packages, plan, store_root, env_root, package_format)?;
        for (k, v) in aur_completed {
            completed_packages.insert(k, v);
        }
    }

    // Step 5: Update installed packages metadata
    let mut installed = PACKAGE_CACHE.installed_packages.write().unwrap();
    for (k, v) in completed_packages.iter() {
        installed.insert(k.clone(), v.clone());
    }
    drop(installed);

    Ok(())
}

/// Prepare packages for download and processing
/// Returns: (packages_to_download, packages_with_pkglines)
fn prepare_packages_for_installation(plan: &InstallationPlan) -> Result<(InstalledPackagesMap, InstalledPackagesMap)> {
    // Separate packages into those with pkglines (already in store) and those without (need download)
    // AUR packages are included in both categories and will be filtered later
    let mut packages_to_download = HashMap::new();
    let mut packages_with_pkglines = HashMap::new();

    // Process fresh_installs
    for (pkgkey, package_info) in plan.fresh_installs.iter() {
        if !package_info.pkgline.is_empty() {
            packages_with_pkglines.insert(pkgkey.clone(), package_info.clone());
        } else {
            packages_to_download.insert(pkgkey.clone(), package_info.clone());
        }
    }

    // Process upgrades_new
    for (pkgkey, package_info) in plan.upgrades_new.iter() {
        if !package_info.pkgline.is_empty() {
            packages_with_pkglines.insert(pkgkey.clone(), package_info.clone());
        } else {
            packages_to_download.insert(pkgkey.clone(), package_info.clone());
        }
    }

    Ok((packages_to_download, packages_with_pkglines))
}

/// Download and install packages
/// Returns: (completed_packages, aur_packages)
fn download_and_install_packages(
    packages_to_download_and_process: &InstalledPackagesMap,
    packages_with_pkglines: &InstalledPackagesMap,
    store_root: &Path,
    env_root: &Path,
    link_type: LinkType,
    can_reflink: bool,
    store_pkglines_by_pkgname: &HashMap<String, Vec<String>>,
) -> Result<(InstalledPackagesMap, InstalledPackagesMap)> {
    // Submit download tasks first (includes both binary and AUR packages)
    let url_to_pkgkeys = enqueue_package_downloads(packages_to_download_and_process)?;
    let pending_urls: Vec<String> = url_to_pkgkeys.keys().cloned().collect();

    // While downloading, link packages that already exist in the store (have non-empty pkgline)
    // For AUR packages that already have a pkgline, we also treat them as completed here
    // (they were built before and have a valid store path), so we don't rebuild them.
    let mut completed_packages = HashMap::new();
    let mut aur_packages = HashMap::new();
    for (pkgkey, package_info) in packages_with_pkglines {
        // Link the package from store to env_root
        let store_fs_dir = store_root.join(&package_info.pkgline).join("fs");
        crate::link::link_package(&store_fs_dir, &env_root.to_path_buf(), link_type, can_reflink)
            .with_context(|| format!("Failed to link existing package {}", pkgkey))?;

        // Add to completed packages (including AUR packages that already exist in the store)
        completed_packages.insert(pkgkey.clone(), package_info.clone());
    }

    // Process downloads for packages that needed to be downloaded
    // wait_downloads_and_unpack_link will filter out AUR packages and return them separately
    let mut mutable_packages_for_processing = packages_to_download_and_process.clone();
    let (downloaded_packages, downloaded_aur_packages) = wait_downloads_and_unpack_link(
        &url_to_pkgkeys,
        pending_urls,
        &mut mutable_packages_for_processing,
        store_root,
        env_root,
        link_type,
        can_reflink,
        store_pkglines_by_pkgname,
    )?;

    // Merge downloaded packages with already-linked packages
    completed_packages.extend(downloaded_packages);
    aur_packages.extend(downloaded_aur_packages);

    utils::fixup_env_links(env_root)?;

    Ok((completed_packages, aur_packages))
}

/// Update metadata for packages that were already installed but involved in this session
fn update_skipped_reinstalls_metadata(plan: &InstallationPlan) -> Result<()> {
    let mut installed = PACKAGE_CACHE.installed_packages.write().unwrap();
    for (pkgkey, session_info) in plan.skipped_reinstalls.iter() {
        if let Some(installed_info) = installed.get_mut(pkgkey) {
            // Only update fields that can change between sessions.
            // Crucially, DO NOT overwrite `pkgline` or `install_time`.
            installed_info.depend_depth = session_info.depend_depth;
            installed_info.ebin_exposure = session_info.ebin_exposure;
            installed_info.depends = session_info.depends.clone();
            installed_info.rdepends = session_info.rdepends.clone();
        }
    }
    Ok(())
}

/// Process downloads and install packages as they complete
fn wait_downloads_and_unpack_link(
    url_to_pkgkeys: &HashMap<String, Vec<String>>,
    mut pending_urls: Vec<String>,
    packages_to_install: &mut InstalledPackagesMap,
    store_root: &Path,
    env_root: &Path,
    link_type: LinkType,
    can_reflink: bool,
    store_pkglines_by_pkgname: &HashMap<String, Vec<String>>,
) -> Result<(InstalledPackagesMap, InstalledPackagesMap)> {
    let mut completed_packages: InstalledPackagesMap = HashMap::new();
    let mut aur_packages: InstalledPackagesMap = HashMap::new();
    // Collect unpacked packages that need linking after all downloads complete
    let mut packages_to_link: Vec<(String, InstalledPackageInfo)> = Vec::new();

    // Unpack packages as downloads complete
    while !pending_urls.is_empty() {
        // Wait for any download to complete
        if let Some(completed_url) = download::wait_for_any_download_task(&pending_urls)? {
            // Get the package key for this completed URL
            if let Some(pkgkeys) = url_to_pkgkeys.get(&completed_url) {
                // Remove from pending list (we only track URLs here)
                pending_urls.retain(|url| *url != completed_url);

                // Process all pkgkeys that share this URL (SPLITPKG or shared artifacts)
                for pkgkey in pkgkeys {
                    let pkgkey = pkgkey.clone();

                    // Check if this is an AUR package
                    if is_aur_package(&pkgkey) {
                        // For AUR packages, just add to aur_packages (they will be built later)
                        if let Some(package_info) = packages_to_install.remove(&pkgkey) {
                            aur_packages.insert(pkgkey, package_info);
                        }
                    } else {
                        // For binary packages, unpack (but don't link yet)
                        // Get the downloaded file path
                        let file_path = get_package_file_path(&pkgkey)?;

                        // Get package info from the map
                        let package_info = packages_to_install.remove(&pkgkey)
                            .ok_or_else(|| eyre!("Package key not found: {}", pkgkey))?;

                        // Unpack the package (without linking)
                        let (actual_pkgkey, package_info) = crate::store::unpack_package(
                            &file_path,
                            &pkgkey,
                            package_info,
                            store_pkglines_by_pkgname,
                        )?;

                        // Store for later linking (clone package_info since we need it in both places)
                        packages_to_link.push((actual_pkgkey.clone(), package_info.clone()));
                        // Also store in completed_packages for return value
                        completed_packages.insert(actual_pkgkey, package_info);
                    }
                }
            } else {
                log::warn!("Could not find package key for completed URL: {}", completed_url);
            }
        }
    }

    // Now that all downloads have completed successfully, link all unpacked packages
    for (actual_pkgkey, package_info) in packages_to_link {
        let store_fs_dir = store_root.join(package_info.pkgline.clone()).join("fs");
        crate::link::link_package(&store_fs_dir, &env_root.to_path_buf(), link_type, can_reflink)
            .with_context(|| format!("Failed to link package {}", actual_pkgkey))?;
    }

    Ok((completed_packages, aur_packages))
}
