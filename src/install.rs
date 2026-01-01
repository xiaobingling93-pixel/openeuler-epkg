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
use crate::plan::InstallationPlan;
use crate::models::PACKAGE_CACHE;
use crate::link::compute_link_type_and_reflink;
use crate::io::{load_world, save_installed_packages, save_world};
use crate::world::{apply_no_install_changes, apply_delta_world, add_essential_packages_to_delta_world, create_delta_world_from_specs};
use crate::depends::resolve_and_install_packages;
use crate::plan::prompt_and_confirm_install_plan;
use crate::history::{create_new_generation_with_root, record_history, update_current_generation_symlink_with_root};
use crate::transaction::{run_transaction_batch, begin_transaction, end_transaction};
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
    let download_cache = dirs().epkg_downloads_cache.clone();

    // Load channel config from the environment
    let package_format = channel_config().format;

    // Get filesystem info for all mount points and store in plan
    plan.store_root_fs = crate::risks::get_filesystem_info(&store_root).ok();
    plan.env_root_fs = crate::risks::get_filesystem_info(&env_root).ok();
    plan.download_cache_fs = crate::risks::get_filesystem_info(&download_cache).ok();

    // Copy link type from EnvConfig to InstallationPlan
    // Downgrade hardlink to symlink if store and env are on different filesystems
    // Check for reflink support if using hardlink
    // Fail early if Move or Runpath link types require cross-filesystem rename
    let (link_type, can_reflink) = compute_link_type_and_reflink(
        env_config().link,
        &store_root,
        &env_root,
        &plan,
    )?;
    plan.link = link_type;
    plan.can_reflink = can_reflink;

    // Calculate download requirements and store in plan
    crate::risks::calculate_plan_sizes(&mut plan)?;

    // Validate transaction (disk space, conflicts, etc.)
    if let Err(e) = crate::risks::check_disk_space_for_plan(&plan, &store_root, &download_cache) {
        log::warn!("Transaction validation failed: {}", e);
        // Continue anyway - validation is advisory for now
    }

    // Execute installations and upgrades (also processes removals via run_transaction_batch)
    execute_installations(&mut plan, &store_root, &env_root, package_format)?;

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
    if plan.ordered_operations.is_empty() {
        return Ok(());
    }

    // Step 1: Prepare packages for download and processing
    let (packages_to_download_and_process, packages_with_pkglines) = prepare_packages_for_installation(plan)?;

    // Step 2: Download and unpack packages (but do not link yet)
    let (aur_packages, packages_to_link) = download_and_unpack_packages(
        &packages_to_download_and_process,
        &packages_with_pkglines,
        &plan.store_pkglines_by_pkgname,
    )?;

    // Step 2a: Check risks for all packages before linking
    crate::risks::validate_before_linking(&packages_to_link, store_root, env_root, plan)
        .with_context(|| "Risk check failed - aborting before any linking to keep environment clean")?;

    // Step 2b: Link all packages (after risk checks pass)
    let mut completed_packages = link_packages(
        packages_to_link,
        store_root,
        env_root,
        plan.link,
        plan.can_reflink,
    )?;

    // Use cached maps from plan
    let has_upgrades = !plan.upgrades_new.is_empty();

    // Execute transaction scriptlets at transaction boundaries (RPM behavior)
    begin_transaction(&plan, &store_root, &env_root, package_format, has_upgrades)?;

    // Step 3: Process upgrades and fresh installations
    // Pass HashMap directly to run_transaction_batch
    run_transaction_batch(plan, &completed_packages, store_root, env_root, package_format)?;

    // Step 4: Build and install AUR packages (build with makepkg)
    if !aur_packages.is_empty() {
        let aur_completed = build_and_install_aur_packages(&aur_packages, plan, store_root, env_root, package_format)?;
        for (k, v) in aur_completed {
            completed_packages.insert(k, v);
        }
    }

    // Execute transaction scriptlets: %posttrans of packages being installed/upgraded
    // This runs AFTER all file operations complete (RPM behavior)
    end_transaction(&plan, &store_root, &env_root, package_format, has_upgrades)?;

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
fn prepare_packages_for_installation(
    plan: &InstallationPlan,
) -> Result<(InstalledPackagesMap, InstalledPackagesMap)> {
    // Separate packages into those with pkglines (already in store) and those without (need download)
    // AUR packages are included in both categories and will be filtered later
    let mut packages_to_download = HashMap::new();
    let mut packages_with_pkglines = HashMap::new();

    // Extract fresh installs and upgrades from ordered_operations
    for op in &plan.ordered_operations {
        if let Some((pkgkey, package_info)) = &op.new_pkg {
            if !package_info.pkgline.is_empty() {
                packages_with_pkglines.insert(pkgkey.clone(), package_info.clone());
            } else {
                packages_to_download.insert(pkgkey.clone(), package_info.clone());
            }
        }
    }

    Ok((packages_to_download, packages_with_pkglines))
}

/// Download and unpack packages (but do not link them yet)
/// Returns: (aur_packages, packages_to_link)
fn download_and_unpack_packages(
    packages_to_download_and_process: &InstalledPackagesMap,
    packages_with_pkglines: &InstalledPackagesMap,
    store_pkglines_by_pkgname: &HashMap<String, Vec<String>>,
) -> Result<(InstalledPackagesMap, Vec<(String, InstalledPackageInfo)>)> {
    // Submit download tasks first (includes both binary and AUR packages)
    let url_to_pkgkeys = enqueue_package_downloads(packages_to_download_and_process)?;
    let mut pending_urls: Vec<String> = url_to_pkgkeys.keys().cloned().collect();

    // Process downloads for packages that needed to be downloaded
    // wait_downloads_and_unpack will filter out AUR packages and return them separately
    // IMPORTANT: We unpack packages but do NOT link them yet - we need to check all risks first
    let mut mutable_packages_for_processing = packages_to_download_and_process.clone();
    let (downloaded_aur_packages, mut all_packages_to_link) = wait_downloads_and_unpack(
        &url_to_pkgkeys,
        &mut pending_urls,
        &mut mutable_packages_for_processing,
        store_pkglines_by_pkgname,
    )?;

    // Add packages that already exist in the store
    for (pkgkey, package_info) in packages_with_pkglines {
        all_packages_to_link.push((pkgkey.clone(), package_info.clone()));
    }

    Ok((downloaded_aur_packages, all_packages_to_link))
}


/// Link all packages to the environment
/// This should only be called after all risk checks have passed.
fn link_packages(
    packages_to_link: Vec<(String, InstalledPackageInfo)>,
    store_root: &Path,
    env_root: &Path,
    link_type: LinkType,
    can_reflink: bool,
) -> Result<InstalledPackagesMap> {
    let mut completed_packages = HashMap::new();

    for (pkgkey, package_info) in packages_to_link {
        let store_fs_dir = store_root.join(&package_info.pkgline).join("fs");
        crate::link::link_package(&store_fs_dir, &env_root.to_path_buf(), link_type, can_reflink)
            .with_context(|| format!("Failed to link package {}", pkgkey))?;

        // Add to completed packages after successful linking
        completed_packages.insert(pkgkey.clone(), package_info.clone());
    }

    utils::fixup_env_links(env_root)?;

    Ok(completed_packages)
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

/// Wait for downloads to complete and unpack packages as they become available.
///
/// This function processes downloads asynchronously, unpacking binary packages as they complete.
/// AUR packages are separated out and returned separately (they will be built later).
///
/// **Important**: This function does NOT link packages. Linking happens later after all risk checks
/// have been performed, keeping the environment clean in case we need to abort.
///
/// # Arguments
/// * `url_to_pkgkeys` - Mapping from download URLs to package keys that use that URL
/// * `pending_urls` - List of URLs that are still being downloaded (modified in place)
/// * `packages_to_install` - Packages to process (modified in place, packages are removed as processed)
/// * `store_pkglines_by_pkgname` - Mapping for resolving package lines by package name
///
/// # Returns
/// * `aur_packages` - AUR packages that need to be built separately
/// * `packages_to_link` - Binary packages that have been unpacked and are ready for linking
fn wait_downloads_and_unpack(
    url_to_pkgkeys: &HashMap<String, Vec<String>>,
    pending_urls: &mut Vec<String>,
    packages_to_install: &mut InstalledPackagesMap,
    store_pkglines_by_pkgname: &HashMap<String, Vec<String>>,
) -> Result<(InstalledPackagesMap, Vec<(String, InstalledPackageInfo)>)> {
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

                        // Store for later linking (don't add to completed_packages yet - that happens after linking)
                        packages_to_link.push((actual_pkgkey, package_info));
                    }
                }
            } else {
                log::warn!("Could not find package key for completed URL: {}", completed_url);
            }
        }
    }

    // Return packages that need linking (but don't link them here - that happens after all risk checks)
    // This allows us to check ALL packages before linking ANY, keeping the environment clean
    Ok((aur_packages, packages_to_link))
}
