use std::collections::HashMap;
use std::sync::Arc;
use std::thread::JoinHandle;

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
use crate::io::load_world;
use crate::io::{save_pending_packages, remove_pending_packages};
use crate::io::{save_installed_packages, save_world};
use crate::repo::sync_channel_metadata;
use crate::world::{apply_no_install_changes, apply_delta_world, add_essential_packages_to_delta_world, create_delta_world_from_specs};
use crate::depends::resolve_and_install_packages;
use crate::plan::prompt_and_confirm_install_plan;
#[cfg(target_os = "linux")]
#[allow(unused_imports)]
use crate::{risks, deb_triggers};
use crate::history::{create_new_generation_with_root, update_current_generation_symlink_with_root, record_history};
use crate::transaction::run_transaction_batch;
#[cfg(target_os = "linux")]
use crate::aur::is_aur_package;
#[cfg(target_os = "linux")]
use crate::aur::build_and_install_aur_packages;
use crate::download::{enqueue_package_downloads, get_package_file_path};
use crate::lfs;

/// Installs specified packages and their dependencies.
pub fn install_packages(package_specs: Vec<String>) -> Result<InstallationPlan> {
    load_world()?;

    // Apply no-install changes from CLI to world.json
    apply_no_install_changes()?;

    // handle local files/URLs, return all package specs ready for installation
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
        // Load repo indexes first so get_essential_pkgnames() can read essential_pkgnames from shards
        sync_channel_metadata()?;
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
pub(crate) fn process_url_package_specs(package_specs: Vec<String>) -> Result<Vec<String>> {
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
                if lfs::exists_on_host(&local_path) && path.is_file() {
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
        if !lfs::exists_on_host(&package_file) {
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
            .and_then(|n: &std::ffi::OsStr| n.to_str())
            .ok_or_else(|| eyre::eyre!("Invalid package directory name: {}", final_dir.display()))?;
        let parsed_pkgline = package::parse_pkgline(pkgline)
            .with_context(|| format!("Failed to parse pkgline: {}", pkgline))?;

        // Read package.txt from the unpacked package
        let package_txt_path = crate::dirs::path_join(&final_dir, &["info", "package.txt"]);
        if !lfs::exists_on_host(&package_txt_path) {
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
/// The target environment is determined by config().common.env_name.
pub fn execute_installation_plan(mut plan: InstallationPlan) -> Result<InstallationPlan> {
    // Calculate download requirements and store in plan before prompting
    #[cfg(unix)]
    {
        crate::risks::calculate_plan_sizes(&mut plan)?;
    }

    // --- USER PROMPT AND PRE-EXECUTION CHECKS ---
    let go_on = prompt_and_confirm_install_plan(&plan)?;

    // Even if user didn't confirm (or no changes planned), still need to expose
    // skipped reinstalls that have ebin_exposure=true
    if !go_on {
        let has_reinstalls_to_expose = plan.skipped_reinstalls.iter()
            .any(|(_, info)| info.ebin_exposure);

        if has_reinstalls_to_expose {
            log::info!("No operations planned, but {} skipped reinstalls need exposure",
                plan.skipped_reinstalls.iter().filter(|(_, info)| info.ebin_exposure).count());
            expose_packages(&mut plan)?;
        }
        return Ok(plan);
    }

    if models::config().common.env_name.is_empty() {
        return Err(eyre::eyre!("Environment name not specified for installation plan"));
    }

    let env_root = plan.env_root.clone();
    let store_root = plan.store_root.clone();
    let download_cache = dirs().epkg_downloads_cache.clone();

    // Ensure store and download cache exist so statvfs can report correct fsid/space
    lfs::create_dir_all(&store_root)?;
    lfs::create_dir_all(&download_cache)?;

    // Get filesystem info for all mount points and store in plan
    #[cfg(unix)]
    {
        plan.env_root_fs = crate::risks::get_filesystem_info(&env_root);
    }
    #[cfg(not(unix))]
    let _ = env_root; // suppress unused warning on non-Unix platforms
    #[cfg(unix)]
    {
        plan.store_root_fs = crate::risks::get_filesystem_info(&store_root);
    }
    #[cfg(unix)]
    {
        plan.download_cache_fs = crate::risks::get_filesystem_info(&download_cache);
    }

    // Copy link type from EnvConfig to InstallationPlan
    // Downgrade hardlink to symlink if store and env are on different filesystems
    // Check for reflink support if using hardlink
    // Fail early if Move or Runpath link types require cross-filesystem rename
    compute_link_type_and_reflink(&mut plan)?;

    // Validate transaction (disk space, conflicts, etc.)
    #[cfg(unix)]
    {
        if let Err(e) = crate::risks::check_disk_space_for_plan(&plan, &store_root, &download_cache) {
            log::warn!("Transaction validation failed: {}", e);
            // Continue anyway - validation is advisory for now
        }
    }

    // Execute installations and upgrades (also processes removals via run_transaction_batch)
    execute_installations(&mut plan)?;

    // Update metadata for skipped reinstalls (uses plan.skipped_reinstalls as the source
    // of session_info).
    update_skipped_reinstalls_metadata(&plan)?;

    let generations_root = dirs::get_default_generations_root()?;
    let new_generation = create_new_generation_with_root(&generations_root)?;
    record_history(&new_generation, Some(&plan))?;
    save_installed_packages(&new_generation)?;
    save_world(&new_generation)?;
    update_current_generation_symlink_with_root(&generations_root, new_generation)?;

    // Generate dpkg database for Debian/Ubuntu environments
    // This allows the real dpkg/dpkg-query commands to see installed packages
    #[cfg(target_os = "linux")]
    if plan.package_format == crate::models::PackageFormat::Deb {
        if let Err(e) = crate::dpkg_db::generate_dpkg_database() {
            log::warn!("Failed to generate dpkg database: {}", e);
        }
    }

    Ok(plan)
}

/// Execute package installations and upgrades
fn execute_installations(plan: &mut InstallationPlan) -> Result<()> {
    // Even if no operations planned, still need to expose skipped reinstalls
    // that have ebin_exposure=true (user-requested packages already installed)
    if plan.ordered_operations.is_empty() {
        // Check if any skipped reinstalls need exposure
        let has_reinstalls_to_expose = plan.skipped_reinstalls.iter()
            .any(|(_, info)| info.ebin_exposure);

        log::debug!("No ordered operations, checking skipped_reinstalls: has_reinstalls_to_expose={}, skipped_count={}",
            has_reinstalls_to_expose, plan.skipped_reinstalls.len());

        for (pkgkey, info) in plan.skipped_reinstalls.iter() {
            log::debug!("  skipped_reinstall: {} ebin_exposure={}", pkgkey, info.ebin_exposure);
        }

        if has_reinstalls_to_expose {
            expose_packages(plan)?;
        }
        return Ok(());
    }

    // Step 1: Download and unpack packages (but do not link yet)
    // This populates plan.batch.new_pkgkeys with packages ready to link
    #[allow(unused)]
    let aur_packages = download_and_unpack_packages(plan)?;

    // Step 2a: Check risks for all packages before linking
    #[cfg(unix)]
    {
        crate::risks::validate_before_linking(plan)
            .with_context(|| "Risk check failed - aborting before any linking to keep environment clean")?;
    }

    // Step 2b: Link all packages (after risk checks pass)
    link_packages(plan)?;

    // Step 2c: Setup tool wrappers for mirror acceleration
    // Called after link_packages so new_files is populated by build_batch_file_union below
    // Note: setup_tool_wrappers is now called in run_transaction_batch after build_batch_file_union

    // Build trigger indices used by hooks/trigger mapping.
    crate::deb_triggers::load_initial_deb_triggers(plan)?;

    // Load initial hooks (from installed packages and etc/pacman.d/hooks/)
    crate::hooks::load_initial_hooks(plan)?;

    // Step 3: Process upgrades and fresh installations
    // Set is_first flag for the first batch
    plan.batch.is_first = true;

    // Save pending packages so dpkg-query can see packages being installed
    save_pending_packages(&plan.new_pkgs)?;

    // Generate dpkg database for Debian/Ubuntu environments before running scriptlets
    // This allows maintainer scripts to query packages being installed
    #[cfg(target_os = "linux")]
    if plan.package_format == crate::models::PackageFormat::Deb {
        // First generate status for already installed packages
        if let Err(e) = crate::dpkg_db::generate_dpkg_status() {
            log::warn!("Failed to generate initial dpkg status: {}", e);
        }
        // Then add pending packages to the dpkg status
        if let Err(e) = crate::dpkg_db::append_pending_to_dpkg_status(&plan.new_pkgs) {
            log::warn!("Failed to append pending packages to dpkg status: {}", e);
        }
    }

    // Run transaction batch for all platforms.
    // On Windows/macOS with Linux-format packages, scriptlets run via VM (libkrun).
    // On Windows/macOS with native formats (conda/brew/msys2), skip_namespace_isolation
    // is set, allowing direct execution.
    run_transaction_batch(plan)?;

    // Clean up pending packages after transaction completes
    remove_pending_packages()?;

    // Step 4: Build and install AUR packages (build with makepkg)
    #[cfg(target_os = "linux")]
    {
        if !aur_packages.is_empty() {
            // Will call run_transaction_batch() once for each round of build
            build_and_install_aur_packages(plan, &aur_packages)?;
        }
    }

    // Step 5: Expose packages with ebin_exposure=true
    // This includes both skipped reinstalls and dependencies of meta-packages
    {
        expose_packages(plan)?;
    }

    // Step 6: update X11 desktop database
    #[cfg(target_os = "linux")]
    crate::xdesktop::update_desktop_databases(&plan.env_root, &plan.desktop_integration_occurred);

    Ok(())
}

/// Download and unpack packages (but do not link them yet)
/// Populates plan.batch.new_pkgkeys with packages that are ready to link
/// Returns: aur_packages
fn download_and_unpack_packages(
    plan: &mut InstallationPlan,
) -> Result<InstalledPackagesMap> {
    // Separate packages into those with pkglines (already in store) and those without (need download)
    // AUR packages are included in both categories and will be filtered later
    let mut packages_to_download: HashMap<String, Arc<InstalledPackageInfo>> = HashMap::new();
    let mut packages_with_pkglines: HashMap<String, Arc<InstalledPackageInfo>> = HashMap::new();

    // Extract fresh installs and upgrades from ordered_operations
    for op in &plan.ordered_operations {
        if let Some(pkgkey) = &op.new_pkgkey {
            if let Some(package_info) = crate::plan::pkgkey2new_pkg_info(plan, pkgkey) {
                if !package_info.pkgline.is_empty() {
                    packages_with_pkglines.insert(pkgkey.clone(), package_info);
                } else {
                    packages_to_download.insert(pkgkey.clone(), package_info);
                }
            }
        }
    }

    // Submit download tasks first (includes both binary and AUR packages)
    let url_to_pkgkeys = enqueue_package_downloads(&packages_to_download)?;
    let mut pending_urls: Vec<String> = url_to_pkgkeys.keys().cloned().collect();

    // Process downloads for packages that needed to be downloaded
    // wait_downloads_and_unpack will filter out AUR packages and return them separately
    // IMPORTANT: We unpack packages but do NOT link them yet - we need to check all risks first
    let downloaded_aur_packages = wait_downloads_and_unpack(
        plan,
        &url_to_pkgkeys,
        &mut pending_urls,
        &packages_to_download,
    )?;

    // Build all_pkgs from ordered_operations (filters: is_aur() == false || in_store() == true)
    crate::plan::build_all_pkgs_from_operations(plan);

    Ok(downloaded_aur_packages)
}


/// Link all packages to the environment (without exposing)
/// This should only be called after all risk checks have passed.
fn link_packages(plan: &mut InstallationPlan) -> Result<()> {
    let link_workers = link_worker_count();
    let plan_ref: &InstallationPlan = &*plan;
    let pkgkeys = plan.batch.new_pkgkeys.clone();

    if link_workers > 1 {
        std::thread::scope(|scope| -> Result<()> {
            let mut handles: Vec<(String, std::thread::ScopedJoinHandle<'_, Result<()>>)> = Vec::new();

            for pkgkey in pkgkeys {
                while handles.len() >= link_workers {
                    let (running_pkgkey, handle) = handles.remove(0);
                    handle
                        .join()
                        .map_err(|e| eyre!("Link worker thread panicked for {}: {:?}", running_pkgkey, e))??;
                }

                let pkgkey_for_worker = pkgkey.clone();
                let handle = scope.spawn(move || link_one_package(plan_ref, &pkgkey_for_worker));
                handles.push((pkgkey, handle));
            }

            while let Some((running_pkgkey, handle)) = handles.pop() {
                handle
                    .join()
                    .map_err(|e| eyre!("Link worker thread panicked for {}: {:?}", running_pkgkey, e))??;
            }

            Ok(())
        })?;
    } else {
        for pkgkey in &pkgkeys {
            link_one_package(plan_ref, pkgkey)?;
        }
    }

    utils::fixup_env_links(&plan.env_root)?;

    Ok(())
}

fn link_worker_count() -> usize {
    if models::config().common.parallel_processing > 1 {
        models::config().common.parallel_processing.min(8)
    } else {
        1
    }
}

fn link_one_package(plan: &InstallationPlan, pkgkey: &str) -> Result<()> {
    if let Some(package_info) = crate::plan::pkgkey2new_pkg_info(plan, pkgkey) {
        if package_info.pkgline.is_empty() {
            return Err(eyre::eyre!(
                "Package {} has empty pkgline, cannot link. This indicates the package wasn't properly unpacked.",
                pkgkey
            ));
        }
        let store_fs_dir = plan.store_root.join(&package_info.pkgline).join("fs");
        crate::link::link_package(plan, &store_fs_dir)?;

        // Create symlinks in usr/bin/ for libexec/bin/ executables.
        // Homebrew formulas (e.g., python@3.14, node) pre-create unversioned
        // command symlinks in libexec/bin/ during the build phase. These are
        // included in the bottle tarball (not created by post_install).
        // We create corresponding symlinks in usr/bin/ for epkg run.
        if let Err(e) = crate::expose::create_libexec_bin_symlinks(&plan.env_root, &store_fs_dir) {
            log::debug!("Failed to create libexec bin symlinks for {}: {}", pkgkey, e);
        }

        // Generate service files for brew packages with service definition
        #[cfg(unix)]
        if plan.package_format == crate::models::PackageFormat::Brew {
            if let Some(package) = crate::package_cache::load_package_info(pkgkey).ok() {
                if let Some(ref service_json) = package.service_json {
                    if let Ok(service) = serde_json::from_str::<crate::brew_repo::BrewService>(service_json) {
                        let pkgname = crate::package::pkgkey2pkgname(pkgkey).unwrap_or_default();
                        if let Err(e) = crate::brew_service::generate_service_files(&plan.env_root, &pkgname, &service) {
                            log::warn!("Failed to generate service files for {}: {}", pkgkey, e);
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

/// Expose all packages that should be exposed based on ordered_operations
/// - ebin wrappers
/// - create/symlink X11 desktop files
/// - update X11 desktop database
#[allow(dead_code)]
fn expose_packages(plan: &mut InstallationPlan) -> Result<()> {
    // Collect pkgkeys that need exposure to avoid borrowing issues
    // Include both new/updated packages and skipped reinstalls with ebin_exposure=true
    let mut pkgkeys_to_expose: Vec<String> = Vec::new();

    // First, collect from ordered_operations
    pkgkeys_to_expose.extend(
        plan.ordered_operations
            .iter()
            .filter(|op| op.should_expose())
            .filter_map(|op| op.new_pkgkey.clone())
    );

    // Also expose skipped reinstalls that have ebin_exposure=true
    // These are packages already installed but user requested (e.g., "cargo" was already installed
    // but requested again; the ebin wrapper might need updating)
    for (pkgkey, info) in plan.skipped_reinstalls.iter() {
        if info.ebin_exposure {
            pkgkeys_to_expose.push(pkgkey.clone());
        }
    }

    // Also collect dependencies of meta-packages for exposure
    // This handles meta-packages like default-jdk that depend on packages providing executables
    let meta_exposures = crate::depends::get_meta_package_exposures(&plan.new_pkgs)?;
    pkgkeys_to_expose.extend(meta_exposures);

    // Remove duplicates while preserving order
    let mut seen = std::collections::HashSet::new();
    pkgkeys_to_expose.retain(|k| seen.insert(k.clone()));

    for pkgkey in pkgkeys_to_expose {
        log::info!("Exposing package: {}", pkgkey);

        // Get the package info to find the store_fs_dir
        // First try new packages, then skipped reinstalls
        let (store_fs_dir, pkgline) = if let Some(package_info) = crate::plan::pkgkey2new_pkg_info(plan, &pkgkey) {
            (plan.store_root.join(&package_info.pkgline).join("fs"), package_info.pkgline.clone())
        } else if let Some(installed_info) = get_skipped_reinstall_pkg_info(plan, &pkgkey) {
            (plan.store_root.join(&installed_info.pkgline).join("fs"), installed_info.pkgline.clone())
        } else {
            log::warn!("Package {} not found in plan for exposure", pkgkey);
            continue;
        };

        // Skip exposure if pkgline is empty (e.g., test data without real packages in store)
        if pkgline.is_empty() {
            log::debug!("Skipping exposure for {} - empty pkgline (package not in store)", pkgkey);
            continue;
        }

        crate::expose::expose_package(plan, &store_fs_dir, &pkgkey)?;
    }

    Ok(())
}

/// Helper to get package info from skipped_reinstalls
fn get_skipped_reinstall_pkg_info(
    plan: &InstallationPlan,
    pkgkey: &str,
) -> Option<crate::models::InstalledPackageInfo> {
    plan.skipped_reinstalls
        .get(pkgkey)
        .map(|arc| (**arc).clone())
}

/// Update metadata for packages that were already installed but involved in this session
fn update_skipped_reinstalls_metadata(plan: &InstallationPlan) -> Result<()> {
    let mut installed = PACKAGE_CACHE.installed_packages.write().unwrap();
    for (pkgkey, session_info) in plan.skipped_reinstalls.iter() {
        if let Some(installed_info_arc) = installed.get_mut(pkgkey) {
            // Only update fields that can change between sessions.
            // Crucially, DO NOT overwrite `pkgline` or `install_time`.
            let info = Arc::make_mut(installed_info_arc);
            info.depend_depth = session_info.depend_depth;
            info.ebin_exposure = session_info.ebin_exposure;
            info.depends = session_info.depends.clone();
            info.rdepends = session_info.rdepends.clone();
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
/// * `plan` - Installation plan (modified in place to update pkgline in plan.new_pkgs)
/// * `url_to_pkgkeys` - Mapping from download URLs to package keys that use that URL
/// * `pending_urls` - List of URLs that are still being downloaded (modified in place)
/// * `packages_to_install` - Packages to process (immutable, used for checking existence)
///
/// # Returns
/// * `aur_packages` - AUR packages that need to be built separately
/// * `packages_to_link` - Binary packages that have been unpacked and are ready for linking
fn wait_downloads_and_unpack(
    plan: &mut InstallationPlan,
    url_to_pkgkeys: &HashMap<String, Vec<String>>,
    pending_urls: &mut Vec<String>,
    packages_to_install: &InstalledPackagesMap,
) -> Result<InstalledPackagesMap> {
    let mut aur_packages: InstalledPackagesMap = HashMap::new();
    let mut unpack_handles: Vec<(String, JoinHandle<Result<(String, String)>>)> = Vec::new();
    let store_pkglines_by_pkgname = Arc::new(plan.store_pkglines_by_pkgname.clone());
    let unpack_workers = unpack_worker_count();

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
                    process_downloaded_pkgkey(
                        plan,
                        pkgkey,
                        packages_to_install,
                        &mut aur_packages,
                        &mut unpack_handles,
                        &store_pkglines_by_pkgname,
                        unpack_workers,
                    )?;
                }
            } else {
                log::warn!("Could not find package key for completed URL: {}", completed_url);
            }
        }
    }

    // Wait for remaining unpack workers.
    while let Some(completed) = unpack_handles.pop() {
        collect_unpack_result_for_plan(plan, completed)?;
    }

    // Return packages that need linking (but don't link them here - that happens after all risk checks)
    // This allows us to check ALL packages before linking ANY, keeping the environment clean
    // Note: packages_to_link is not returned here - they are added to plan.batch.new_pkgkeys later
    Ok(aur_packages)
}

fn unpack_worker_count() -> usize {
    if models::config().common.parallel_processing > 1 {
        models::config().common.parallel_processing.min(6)
    } else {
        1
    }
}

#[cfg(target_os = "linux")]
fn is_aur_pkgkey(pkgkey: &str) -> bool {
    is_aur_package(pkgkey)
}

#[cfg(not(target_os = "linux"))]
fn is_aur_pkgkey(_pkgkey: &str) -> bool {
    false
}

fn collect_unpack_result_for_plan(
    plan: &mut InstallationPlan,
    (pkgkey, handle): (String, JoinHandle<Result<(String, String)>>),
) -> Result<()> {
    let (_actual_pkgkey, pkgline) = handle
        .join()
        .map_err(|e| eyre!("Unpack worker thread panicked for {}: {:?}", pkgkey, e))??;
    if let Some(plan_pkg_info) = plan.new_pkgs.get_mut(&pkgkey) {
        Arc::make_mut(plan_pkg_info).pkgline = pkgline;
    }
    Ok(())
}

fn process_downloaded_pkgkey(
    plan: &mut InstallationPlan,
    pkgkey: &str,
    packages_to_install: &InstalledPackagesMap,
    aur_packages: &mut InstalledPackagesMap,
    unpack_handles: &mut Vec<(String, JoinHandle<Result<(String, String)>>)>,
    store_pkglines_by_pkgname: &Arc<HashMap<String, Vec<String>>>,
    unpack_workers: usize,
) -> Result<()> {
    if is_aur_pkgkey(pkgkey) {
        if let Some(package_info) = packages_to_install.get(pkgkey) {
            aur_packages.insert(pkgkey.to_string(), Arc::clone(package_info));
        }
        return Ok(());
    }

    if !packages_to_install.contains_key(pkgkey) {
        return Err(eyre!("Package key not found: {}", pkgkey));
    }

    if unpack_workers > 1 {
        while unpack_handles.len() >= unpack_workers {
            let completed = unpack_handles.remove(0);
            collect_unpack_result_for_plan(plan, completed)?;
        }
        spawn_unpack_worker(pkgkey, unpack_handles, Arc::clone(store_pkglines_by_pkgname))?;
    } else {
        unpack_binary_package_sync(plan, pkgkey, store_pkglines_by_pkgname)?;
    }

    Ok(())
}

fn spawn_unpack_worker(
    pkgkey: &str,
    unpack_handles: &mut Vec<(String, JoinHandle<Result<(String, String)>>)>,
    store_pkglines_by_pkgname: Arc<HashMap<String, Vec<String>>>,
) -> Result<()> {
    let file_path = get_package_file_path(pkgkey)?;
    let pkgkey_for_worker = pkgkey.to_string();
    let handle = std::thread::spawn(move || -> Result<(String, String)> {
        let package = crate::package_cache::load_package_info(&pkgkey_for_worker)
            .map_err(|e| eyre!("Failed to load package info for {}: {}", pkgkey_for_worker, e))?;
        crate::store::unpack_package(
            &file_path,
            &pkgkey_for_worker,
            &store_pkglines_by_pkgname,
            Some(package.format),
        )
    });
    unpack_handles.push((pkgkey.to_string(), handle));
    Ok(())
}

fn unpack_binary_package_sync(
    plan: &mut InstallationPlan,
    pkgkey: &str,
    store_pkglines_by_pkgname: &HashMap<String, Vec<String>>,
) -> Result<()> {
    let file_path = get_package_file_path(pkgkey)?;
    let package = crate::package_cache::load_package_info(pkgkey)
        .map_err(|e| eyre!("Failed to load package info for {}: {}", pkgkey, e))?;
    let (_actual_pkgkey, pkgline) = crate::store::unpack_package(
        &file_path,
        pkgkey,
        store_pkglines_by_pkgname,
        Some(package.format),
    )?;
    if let Some(plan_pkg_info) = plan.new_pkgs.get_mut(pkgkey) {
        Arc::make_mut(plan_pkg_info).pkgline = pkgline;
    }
    Ok(())
}
