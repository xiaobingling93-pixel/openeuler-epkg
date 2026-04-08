#[cfg(unix)]
use std::env;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write as IoWrite;
use std::path::{Path, PathBuf};

use color_eyre::eyre::{self, Context};
use color_eyre::Result;
#[cfg(unix)]
use nix::unistd::{fork, ForkResult};
use serde::{Deserialize, Serialize};

use crate::dirs::{find_env_base, get_env_root};
use crate::download::download_urls;
use crate::mirror;
use crate::models::*;
use crate::models::dirs;
use crate::utils;
use crate::environment::{create_environment, register_environment_for};
use crate::lfs;
#[cfg(target_os = "linux")]
use crate::apparmor;

const GITEE_API_BASE:   &str = &"https://gitee.com/api/v5";
const GITEE_OWNER:      &str = &"wu_fengguang";
const REPO_EPKG:        &str = &"epkg";
const REPO_ELF_LOADER:  &str = &"elf-loader";
#[cfg(feature = "libkrun")]
const REPO_VMLINUX:     &str = &"sandbox-kernel";

fn print_banner() {
    println!(r#"         ____  _  ______   "#);
    println!(r#"   ____ |  _ \| |/ / ___|  "#);
    println!(r#"  ( ___)| |_) | ' / |  _   "#);
    println!(r#"   )__) |  __/| . \ |_| |  "#);
    println!(r#"  (____)|_|   |_|\_\____|  "#);
}

/// Pre-populate the country cache in a background process to speed up later epkg install invocations.
/// This function forks a child process that runs independently and won't be affected by main process exit.
///
/// Behavior when main thread exits:
/// - The child process continues running independently in the background
/// - The cache population will complete even if the main process exits
/// - The child process will automatically clean up when it finishes
/// - This ensures the cache is properly populated for future epkg operations
#[cfg(unix)]
fn pre_populate_country_cache() {
    match unsafe { fork() } {
        Ok(ForkResult::Parent { child, .. }) => {
            // Parent process: continue with installation, don't wait for child
            log::debug!("Started background process (PID: {}) to pre-populate country cache", child);
        }
        Ok(ForkResult::Child) => {
            // Child process: populate cache and exit
            if let Err(e) = crate::location::get_country_code_from_ip() {
                log::debug!("Failed to pre-populate country cache: {}", e);
            } else {
                log::debug!("Successfully pre-populated country cache");
            }
            std::process::exit(0);
        }
        Err(e) => {
            log::warn!("Failed to fork background process for cache population: {}", e);
        }
    }
}

// After root run `epkg self install --store=shared`, /usr/local/bin/epkg will be created and
// exposed to normal users. Then everyone can run "epkg install". To make it user friendly,
// here we'll auto trigger light_init() seemlessly at first invocation.
pub fn try_light_init() -> Result<()> {
    if !config().init.shared_store {
        return Ok(());
    }

    // When running inside an environment (e.g. chroot), do not try to create the main env.
    if config().common.in_env_root {
        return Ok(());
    }

    if matches!(config().subcommand,
          EpkgCommand::Unpack
        | EpkgCommand::Convert
        | EpkgCommand::Hash
        | EpkgCommand::Repo
        | EpkgCommand::List
        | EpkgCommand::SelfInstall
        | EpkgCommand::SelfUpgrade
        | EpkgCommand::SelfRemove
        | EpkgCommand::Upgrade
        | EpkgCommand::Remove
        | EpkgCommand::Run
        | EpkgCommand::Busybox
        | EpkgCommand::EnvPath
        | EpkgCommand::None
    ) {
        return Ok(());
    }

    if find_env_base(MAIN_ENV).is_none() {
        light_init()?;
    }

    // Setup tool config symlinks for mirror acceleration
    crate::tool_wrapper::setup_tool_config_symlinks();

    Ok(())
}

pub fn light_init() -> Result<()> {
    // Create main environment
    create_environment(MAIN_ENV)?;

    // Load the environment config that was just created and register it
    let env_config = crate::io::deserialize_env_config_for(MAIN_ENV.to_string())?;
    register_environment_for(MAIN_ENV, env_config)?;

    // Setup tool config symlinks for mirror acceleration
    crate::tool_wrapper::setup_tool_config_symlinks();

    update_shell_profile().unwrap_or_else(|e| {
        log::warn!("Could not update shell profile: {}", e);
    });

    println!("Notice: for changes to take effect, close and re-open your current shell.");
    Ok(())
}

pub fn upgrade_epkg() -> Result<()> {
    // Check if self environment exists
    if find_env_base(SELF_ENV).is_none() {
        eprintln!("epkg is not installed. Please run 'epkg self install' first.");
        return Ok(());
    }

    // Check for available updates and get initialization plan
    match check_for_updates() {
        Ok(init_plan) => {
            // Check if upgrade is needed (platform-neutral: elf_loader_version is `None` off-Linux).
            let need_upgrade = init_plan.new.epkg_version != init_plan.current.epkg_version
                || init_plan.new.elf_loader_version != init_plan.current.elf_loader_version;

            if need_upgrade {
                println!("Upgrading epkg installation...");
                download_setup_files(&init_plan)?;
            } else {
                println!("epkg is already up to date.");
            }
        }
        Err(e) => {
            eprintln!("Warning: Failed to check for updates: {}", e);
            return Ok(());
        }
    }

    Ok(())
}

pub fn install_epkg_with_force(force: bool) -> Result<()> {
    // Fix up /lib64 symlink on Unix systems
    #[cfg(unix)]
    fixup_host_lib64_symlink()
        .unwrap_or_else(|e| {
            log::debug!("Could not fixup /lib64 symlink: {}", e);
        });

    // When force is specified, rename (don't delete) old self env first
    // This allows us to copy the running binary from the old location before cleanup
    let old_self_env = if force {
        let self_env_base = crate::dirs::get_env_base_path(SELF_ENV);
        if self_env_base.exists() {
            let old_path = self_env_base.parent().unwrap().join(".self-old");
            // Remove any stale backup from previous interrupted install
            if old_path.exists() {
                log::info!("Removing stale backup: {}", old_path.display());
                lfs::remove_dir_all(&old_path)?;
            }
            println!("Moving old self environment to: {}", old_path.display());
            lfs::rename(&self_env_base, &old_path)?;
            Some(old_path)
        } else {
            None
        }
    } else {
        None
    };

    // Set up installation paths
    lfs::create_dir_all(&dirs().epkg_downloads_cache.join("epkg"))?;

    print_banner();

    // Pre-populate country cache in background thread to speed up later invocations
    #[cfg(unix)]
    pre_populate_country_cache();

    // Download and setup package manager files
    let init_plan = check_for_updates()?;
    download_setup_files(&init_plan)?;

    // Create self environment
    create_environment(SELF_ENV)?;

    // Clean up old self environment after successful installation
    if let Some(old_env) = old_self_env {
        if old_env.exists() {
            log::info!("Cleaning up old self environment: {}", old_env.display());
            if let Err(e) = lfs::remove_dir_all(&old_env) {
                log::warn!("Failed to remove old self environment: {}", e);
            }
        }
    }

    // Install AppArmor profile to allow epkg to use namespaces and mounts
    // This is required on Ubuntu and other systems with strict AppArmor policies
    #[cfg(target_os = "linux")]
    if let Err(e) = apparmor::install_apparmor_profile() {
        log::warn!("Failed to install AppArmor profile: {}", e);
        log::warn!("epkg may not function correctly on systems with strict AppArmor policies");
        // Continue anyway - don't fail the installation
    }

    // Setup tool config symlinks for mirror acceleration
    crate::tool_wrapper::setup_tool_config_symlinks();

    println!("Installation complete!");

    update_shell_profile().unwrap_or_else(|e| {
        log::warn!("Could not update shell profile: {}", e);
    });

    Ok(())
}

#[allow(dead_code)]
fn download_and_install_epkg_binary_from_release(
    release: &GiteeRelease,
    binary_asset_name: &str,
    target_epkg: &Path,
) -> Result<()> {
    // Assets are paired as:
    // - `${binary_asset_name}`
    // - `${binary_asset_name}.sha256`
    let sha_asset_name = format!("{}.sha256", binary_asset_name);

    let epkg_binary_url = release.find_asset_url(binary_asset_name)?;
    let epkg_binary_sha_url = release.find_asset_url(&sha_asset_name)?;

    let epkg_download_dir = &dirs().epkg_downloads_cache;

    let epkg_binary_path =
        mirror::Mirrors::remote_url_to_path(&epkg_binary_url, epkg_download_dir, "epkg")?;
    let epkg_binary_sha_path =
        mirror::Mirrors::remote_url_to_path(&epkg_binary_sha_url, epkg_download_dir, "epkg")?;

    // Delete sha file first: HTTP timestamps may be missing and we want fresh checksum content.
    if epkg_binary_sha_path.exists() {
        lfs::remove_file(&epkg_binary_sha_path)?;
    }

    let download_results =
        download_urls(vec![epkg_binary_url.clone(), epkg_binary_sha_url.clone()]);
    for result in download_results {
        result.with_context(|| "Failed to download epkg binary for upgrade")?;
    }

    utils::verify_sha256sum(&epkg_binary_sha_path)
        .context("Failed to verify epkg binary checksum")?;

    copy_epkg_binary_atomically(&epkg_binary_path, target_epkg, true)?;
    Ok(())
}

fn remove_stale_init_sha256_files(init_plan: &InitPlan, epkg_download_dir: &Path) -> Result<()> {
    #[allow(unused_mut)]
    let mut sha256_files_to_delete: Vec<std::path::PathBuf> = if let Some(ref epkg_plan) = init_plan.epkg_binary {
        vec![epkg_plan.sha_path(epkg_download_dir)?]
    } else {
        Vec::new()
    };

    if let ElfLoaderPlan::Download { url, .. } = &init_plan.elf_loader {
        let elf_plan = AssetDownloadPlan { url: url.clone(), path: std::path::PathBuf::new() };
        sha256_files_to_delete.push(elf_plan.sha_path(epkg_download_dir)?);
    }
    #[cfg(feature = "libkrun")]
    if let Some(ref vmlinux_plan) = init_plan.vmlinux {
        sha256_files_to_delete.push(vmlinux_plan.sha_path(epkg_download_dir)?);
    }
    if let Some(ref epkg_linux_plan) = init_plan.epkg_linux {
        if epkg_linux_plan.is_download() {
            sha256_files_to_delete.push(epkg_linux_plan.sha_path(epkg_download_dir)?);
        }
    }
    for sha256_path in &sha256_files_to_delete {
        if sha256_path.exists() {
            lfs::remove_file(sha256_path)?;
        }
    }
    Ok(())
}

fn verify_init_download_checksums(init_plan: &InitPlan, epkg_download_dir: &Path) -> Result<()> {
    if let ElfLoaderPlan::Download { url, .. } = &init_plan.elf_loader {
        let elf_plan = AssetDownloadPlan { url: url.clone(), path: std::path::PathBuf::new() };
        let elf_sha_path = elf_plan.sha_path(epkg_download_dir)?;
        utils::verify_sha256sum(&elf_sha_path)
            .context("Failed to verify elf-loader checksum")?;
    }

    if let Some(ref epkg_plan) = init_plan.epkg_binary {
        let epkg_sha_path = epkg_plan.sha_path(epkg_download_dir)?;
        utils::verify_sha256sum(&epkg_sha_path)
            .context("Failed to verify epkg binary checksum")?;
    }

    if let Some(ref epkg_linux_plan) = init_plan.epkg_linux {
        if epkg_linux_plan.is_download() {
            let epkg_linux_sha_path = epkg_linux_plan.sha_path(epkg_download_dir)?;
            utils::verify_sha256sum(&epkg_linux_sha_path)
                .context("Failed to verify epkg-linux checksum")?;
        }
    }

    Ok(())
}

fn download_package_manager_files(init_plan: &InitPlan) -> Result<()> {
    // Collect urls for downloading in parallel
    let mut urls = Vec::new();
    let dirs = dirs();
    let epkg_download_dir = &dirs.epkg_downloads_cache;

    // Handle epkg source code (local repo or download)
    if init_plan.need_download_epkg_src {
        println!("Downloading epkg source code from {}", init_plan.epkg_src_url);
        urls.push(init_plan.epkg_src_url.clone());
    }

    // Download epkg binary only when version differs.
    if let Some(ref epkg_plan) = init_plan.epkg_binary {
        println!("Downloading epkg binary from {}", epkg_plan.url);
        let sha_url = epkg_plan.sha_url();
        urls.extend(vec![epkg_plan.url.clone(), sha_url]);
    }

    // Handle elf-loader based on the explicit plan
    match &init_plan.elf_loader {
        ElfLoaderPlan::LocalCopy { source, target } => {
            log::debug!(
                "Copying local elf-loader from {} to {}",
                source.display(),
                target.display()
            );
            if let Some(parent) = target.parent() {
                lfs::create_dir_all(parent)?;
            }
            lfs::copy(source, target)?;
            println!("Using local elf-loader from {}", source.display());
        }
        ElfLoaderPlan::Download { url, target } => {
            println!("Downloading elf-loader from {}", url);
            let elf_plan = AssetDownloadPlan { url: url.clone(), path: target.clone() };
            let sha_url = elf_plan.sha_url();
            urls.extend(vec![url.clone(), sha_url]);
        }
        ElfLoaderPlan::None => {}
    }

    // Download vmlinux if built with libkrun feature
    #[cfg(feature = "libkrun")]
    {
        if let Some(ref vmlinux_plan) = init_plan.vmlinux {
            println!("Downloading vmlinux from {}", vmlinux_plan.url);
            let sha_url = vmlinux_plan.sha_url();
            let config_url = vmlinux_plan.vmlinux_config_url()?;
            urls.extend(vec![
                vmlinux_plan.url.clone(),
                sha_url,
            ]);
            urls.push(config_url);
        }
    }

    // Download epkg-linux for VM usage on Windows/macOS hosts
    if let Some(ref epkg_linux_plan) = init_plan.epkg_linux {
        if epkg_linux_plan.is_download() {
            println!("Downloading epkg-linux (for VM) from {}", epkg_linux_plan.url);
            let sha_url = epkg_linux_plan.sha_url();
            urls.extend(vec![epkg_linux_plan.url.clone(), sha_url]);
        } else {
            println!(
                "Using local epkg-linux (for VM) from {}",
                epkg_linux_plan.path.display()
            );
        }
    }

    if urls.is_empty() {
        return Ok(());
    }

    // Delete .sha256 files first: gitee.com HTTP headers have no file timestamp,
    // so download.rs would think "File unchanged" based on file size matching.
    remove_stale_init_sha256_files(init_plan, epkg_download_dir)?;

    // Download to the new epkg subdirectory within downloads cache
    // Use the base directory - download_urls will construct nested paths internally
    let download_results = download_urls(urls);
    for result in download_results {
        result.with_context(|| "Failed to download package manager files")?;
    }

    verify_init_download_checksums(init_plan, epkg_download_dir)?;

    if init_plan.need_download_epkg_src && !init_plan.epkg_src_path.exists() {
        return Err(eyre::eyre!("Failed to download epkg source code tar file from {}", init_plan.epkg_src_url));
    }

    // Install vmlinux if downloaded
    #[cfg(feature = "libkrun")]
    {
        if let (Some(ref vmlinux_plan), Some(ref version)) = (&init_plan.vmlinux, &init_plan.vmlinux_version) {
            if vmlinux_plan.path.exists() {
                let arch = &config().common.arch;
                let cfg_path = vmlinux_plan.vmlinux_config_path(epkg_download_dir)?;
                install_kernel(
                    &vmlinux_plan.path,
                    Some(&cfg_path),
                    version,
                    arch,
                )?;
            }
        }
    }

    Ok(())
}

fn download_setup_files(init_plan: &InitPlan) -> Result<()> {
    let self_env_root = dirs().user_envs.join(SELF_ENV);

    download_package_manager_files(init_plan)
        .context("Failed to download required files for self environment")?;

    setup_epkg_src(&self_env_root, init_plan)?;
    setup_common_binaries(&self_env_root, init_plan)?;

    Ok(())
}

fn setup_epkg_src(env_root: &Path, init_plan: &InitPlan) -> Result<()> {
    let usr_src = crate::dirs::path_join(env_root, &["usr", "src"]);
    let epkg_src = usr_src.join("epkg");

    // Check if we're using a local repository
    if init_plan.using_local_repo {
        // Create symlink directly to git working directory
        if !usr_src.exists() {
            lfs::create_dir_all(&usr_src)?;
        }

        if lfs::is_symlink_or_junction(&epkg_src) {
            lfs::remove_file(&epkg_src)?;
        }
        // If directory exists but is not a symlink/junction, remove it (may be incomplete/stale)
        if epkg_src.exists() {
            log::debug!("Removing existing epkg src directory: {}", epkg_src.display());
            lfs::remove_dir_all(&epkg_src)?;
        }
        let repo_root = find_repo_root()?;
        // On Windows, use symlink_dir_for_native() to create a native Windows-accessible symlink.
        // The regular symlink() creates LX reparse points that only work with virtiofs.
        #[cfg(windows)]
        lfs::symlink_dir_for_native(&repo_root, &epkg_src)?;
        #[cfg(not(windows))]
        lfs::symlink_dir_for_native(&repo_root, &epkg_src)?;

        println!("Using local git repository for epkg source code");
        return Ok(());
    }

    // Extract epkg source code tar for remote repository
    let epkg_extracted_dir = format!("epkg-{}", init_plan.new.epkg_version);
    let epkg_extracted_path = usr_src.join(&epkg_extracted_dir);

    println!("Extracting epkg source code to: {}", usr_src.display());

    if epkg_extracted_path.exists() {
        lfs::remove_dir_all(&epkg_extracted_path)?;
    } else {
        lfs::create_dir_all(&usr_src)?;
    }

    // Extract tar.gz file with error handling
    utils::extract_tar_gz(&init_plan.epkg_src_path, &usr_src)
        .context("Failed to extract epkg source code tar file")?;

    // Create a symlink from epkg to epkg-master (or epkg-$version)
    if let Err(e) = utils::force_symlink_dir_for_native(&epkg_extracted_dir, &epkg_src) {
        eprintln!("[WARN] Failed to create symlink {} -> {}: {}",
                 epkg_src.display(), epkg_extracted_dir, e);
    }

    Ok(())
}

/// Install epkg binaries to the self environment's usr/bin directory.
///
/// See the architecture documentation in environment.rs for the dual-binary mechanism.
///
/// This function is called during `epkg self install` and sets up:
///
/// | Platform | Binary            | Purpose                          |
/// |----------|-------------------|----------------------------------|
/// | Linux    | epkg              | Main package manager binary      |
/// | Linux    | elf-loader        | Dynamic linker for glibc packages|
/// | macOS    | epkg              | Native binary for Conda/Brew     |
/// | macOS    | epkg-linux-$arch  | Linux binary for VM execution    |
/// | Windows  | epkg.exe          | Native binary for Conda/msys2    |
/// | Windows  | epkg-linux-$arch  | Linux binary for VM execution    |
///
/// The self environment layout after installation:
/// ```
/// $ENVS/self/usr/bin/
/// ├── epkg[.exe]          # Native host binary
/// ├── elf-loader          # Linux only
/// └── epkg-linux-$arch    # Windows/macOS only
/// ```
fn setup_common_binaries(env_root: &Path, init_plan: &InitPlan) -> Result<()> {
    let usr_bin = crate::dirs::path_join(env_root, &["usr", "bin"]);

    lfs::create_dir_all(&usr_bin)?;

    let target_epkg = usr_bin.join(crate::dirs::EPKG_USR_BIN_NAME);

    // Use downloaded epkg binary only when a version differs and we decided to download it.
    let epkg_source = if let Some(ref epkg_plan) = init_plan.epkg_binary {
        if !epkg_plan.path.exists() {
            return Err(eyre::eyre!(
                "Downloaded epkg binary not found at {}",
                epkg_plan.path.display()
            ));
        }
        epkg_plan.path.clone()
    } else {
        std::env::current_exe().context("Failed to get current executable path")?
    };

    // Copy epkg binary using atomic operation
    copy_epkg_binary_atomically(&epkg_source, &target_epkg, true)?;

    // Copy elf-loader binary using atomic operation
    // On Linux: always needed for running glibc packages
    // On Windows/macOS with libkrun: needed for Linux distros running in VM
    let elf_loader_source = match &init_plan.elf_loader {
        ElfLoaderPlan::LocalCopy { target, .. } => Some(target.clone()),
        ElfLoaderPlan::Download { target, .. } => Some(target.clone()),
        ElfLoaderPlan::None => None,
    };
    if let Some(elf_loader_path) = elf_loader_source {
        if elf_loader_path.exists() {
            let elf_loader_target = usr_bin.join("elf-loader");
            copy_epkg_binary_atomically(&elf_loader_path, &elf_loader_target, false)?;
            log::info!("Installed elf-loader: {}", elf_loader_target.display());
        } else {
            log::warn!("elf-loader binary not found at {}", elf_loader_path.display());
        }
    }

    // Copy epkg-linux binary for VM usage on Windows/macOS hosts
    // On Linux, epkg-linux is a hardlink to epkg (same inode, saves disk space)
    #[cfg(target_os = "linux")]
    {
        let arch = &config().common.arch;
        let epkg_linux_target = usr_bin.join(format!("epkg-linux-{}", arch));
        // Create hardlink to epkg (same inode, atomic with epkg installation)
        if target_epkg.exists() {
            // Remove existing file first (hardlink requires target to not exist)
            if epkg_linux_target.exists() {
                lfs::remove_file(&epkg_linux_target)?;
            }
            std::fs::hard_link(&target_epkg, &epkg_linux_target)
                .context(format!("Failed to create hardlink {} -> {}",
                    epkg_linux_target.display(), target_epkg.display()))?;
            log::info!("Installed epkg-linux for VM (hardlink): {}", epkg_linux_target.display());
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let arch = &config().common.arch;
        let epkg_linux_target = usr_bin.join(format!("epkg-linux-{}", arch));
        if let Some(ref epkg_linux_plan) = init_plan.epkg_linux {
            if epkg_linux_plan.path.exists() {
                copy_epkg_binary_atomically(&epkg_linux_plan.path, &epkg_linux_target, false)?;
                log::info!("Installed epkg-linux for VM: {}", epkg_linux_target.display());
            } else {
                log::warn!("epkg-linux binary not found at {}", epkg_linux_plan.path.display());
            }
        }
    }

    // Short paths under ~/.epkg/: bin -> self usr/bin, assets -> self usr/src/epkg/assets
    ensure_home_epkg_symlinks(env_root, &usr_bin)?;

    // Update hardlinks in all envs to point to the new epkg-linux binary.
    // This ensures that all environments share the same inode for epkg/init,
    // saving disk space and ensuring consistency.
    // Note: On Windows/macOS, this updates files for virtiofs export to VM.
    {
        let arch = &config().common.arch;
        let self_epkg_linux = usr_bin.join(format!("epkg-linux-{}", arch));
        if self_epkg_linux.exists() {
            update_all_env_hardlinks(&self_epkg_linux, arch)?;
        }
    }

    // Create symlink to epkg binary in the first valid PATH component (Unix only)
    #[cfg(unix)]
    create_epkg_symlink(&target_epkg)
        .context("Failed to create epkg symlink in PATH")?;

    // Codesign the downloaded binary with hypervisor entitlements on macOS
    // This is required for libkrun VM support
    #[cfg(target_os = "macos")]
    {
        let epkg_src = crate::dirs::path_join(env_root, &["usr", "src", "epkg"]);
        let entitlements = epkg_src.join("assets/macos/epkg.entitlements");
        if entitlements.exists() {
            codesign_with_entitlements(&target_epkg, &entitlements)?;
            // Also sign epkg-linux if it exists
            let arch = &config().common.arch;
            let epkg_linux_target = usr_bin.join(format!("epkg-linux-{}", arch));
            if epkg_linux_target.exists() {
                codesign_with_entitlements(&epkg_linux_target, &entitlements)?;
            }
        } else {
            log::warn!("Entitlements file not found at {}", entitlements.display());
        }
    }

    Ok(())
}

/// Update hardlinks in all environments to point to the new epkg-linux binary.
///
/// This function is called after `epkg self install` to ensure that all environments
/// have hardlinks pointing to the newly installed binary. This:
/// 1. Saves disk space (all envs share the same inode)
/// 2. Ensures consistency (all envs use the same binary version)
/// 3. Makes updates atomic (single binary replacement updates all envs)
///
/// Files updated in each environment:
/// - `usr/bin/epkg` -> hardlink to self's epkg-linux-$arch
/// - `usr/bin/init` -> hardlink to self's epkg-linux-$arch
///
/// Note: On Windows/macOS hosts, we copy files instead of hardlinks because:
/// - Windows/macOS host filesystems cannot use Linux ELF binaries directly
/// - The hardlinks are only meaningful inside the VM (virtiofs exports them)
/// - We still update the files to ensure consistency across environments
fn update_all_env_hardlinks(self_epkg_linux: &Path, _arch: &str) -> Result<()> {
    #[cfg(not(target_os = "linux"))]
    {
        // On Windows/macOS, we cannot create hardlinks for Linux ELF binaries
        // because the host filesystem doesn't support executing them.
        // However, we still need to copy the binary to all environments
        // so that virtiofs can export them to the VM guest.
        log::debug!(
            "Skipping hardlink update on non-Linux host: {}",
            self_epkg_linux.display()
        );
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    {
        let envs_dir = dirs().user_envs.clone();
        if !envs_dir.exists() {
            return Ok(());
        }

        let entries = match std::fs::read_dir(&envs_dir) {
            Ok(e) => e,
            Err(e) => {
                log::warn!("Failed to read envs directory {}: {}", envs_dir.display(), e);
                return Ok(());
            }
        };

        let mut updated_count = 0;
        for entry in entries.flatten() {
            let env_name = entry.file_name().to_string_lossy().to_string();
            // Skip self environment - it already has the correct binary
            if env_name == SELF_ENV {
                continue;
            }

            let env_usr_bin = envs_dir.join(&env_name).join("usr").join("bin");
            if !env_usr_bin.exists() {
                continue;
            }

            // Update epkg and init hardlinks
            for filename in ["epkg", "init"] {
                let target_path = env_usr_bin.join(filename);
                if target_path.exists() {
                    // Remove existing file (whether hardlink or not) and create new hardlink
                    if let Err(e) = lfs::remove_file(&target_path) {
                        log::warn!("Failed to remove {}: {}", target_path.display(), e);
                        continue;
                    }
                }
                if let Err(e) = std::fs::hard_link(self_epkg_linux, &target_path) {
                    log::warn!("Failed to create hardlink {} -> {}: {}",
                        target_path.display(), self_epkg_linux.display(), e);
                    continue;
                }
                log::debug!("Updated hardlink: {} -> {}", target_path.display(), self_epkg_linux.display());
                updated_count += 1;
            }
        }

        if updated_count > 0 {
            log::info!("Updated {} hardlinks across all environments", updated_count);
        }

        Ok(())
    }
}

/// Symlinks under `home_epkg` (`$HOME/.epkg` or `%USERPROFILE%\\.epkg`) into the self env:
/// - `bin` -> self `usr/bin` (short path to `epkg` and other tools)
/// - `assets` -> self `usr/src/epkg/assets` (short path to shipped assets)
fn ensure_home_epkg_symlinks(self_env_root: &Path, self_usr_bin: &Path) -> Result<()> {
    let home_epkg = dirs().home_epkg.clone();

    let assets = crate::dirs::path_join(self_env_root, &["usr", "src", "epkg", "assets"]);
    for (name, target) in [("bin", self_usr_bin), ("assets", assets.as_path())] {
        link_home_epkg_subdir(&home_epkg, name, target);
    }

    Ok(())
}

/// `home_epkg/<name>` -> directory `target`.
fn link_home_epkg_subdir(home_epkg: &Path, name: &str, target: &Path) {
    let link = home_epkg.join(name);

    // Ensure parent directory exists before creating symlink
    if let Err(e) = lfs::create_dir_all(home_epkg) {
        log::warn!(
            "Failed to create directory {}: {}",
            home_epkg.display(),
            e
        );
        return;
    }

    println!("Creating symlink: {} -> {}", link.display(), target.display());
    if let Err(e) = utils::force_symlink_dir_for_native(target, &link) {
        log::warn!(
            "Failed to create symlink {} -> {}: {}",
            link.display(),
            target.display(),
            e
        );
    }
}

/// Codesign a binary with hypervisor entitlements on macOS.
/// This is required for libkrun VM support.
#[cfg(target_os = "macos")]
fn codesign_with_entitlements(binary: &Path, entitlements: &Path) -> Result<()> {
    use std::process::Command;

    log::info!("Codesigning {} with entitlements {}", binary.display(), entitlements.display());

    let output = Command::new("codesign")
        .arg("--force")
        .arg("--sign")
        .arg("-")
        .arg("--entitlements")
        .arg(entitlements)
        .arg(binary)
        .output()
        .context("Failed to execute codesign command")?;

    if output.status.success() {
        log::info!("Successfully signed {} with hypervisor entitlements", binary.display());
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        log::warn!("Failed to codesign {}: {}", binary.display(), stderr);
    }

    Ok(())
}

/// Safely copy a binary using atomic operations to avoid conflicts with running processes
fn copy_epkg_binary_atomically(source: &Path, target: &Path, is_epkg: bool) -> Result<()> {
    // Check if we're trying to copy the epkg binary to itself or to a location that would conflict
    if is_epkg {
        if source == target {
            log::info!("Target epkg binary is the same as current executable, skipping copy");
            return Ok(());
        } else if target.exists() {
            // Check if target is a symlink pointing to current executable
            #[cfg(unix)]
            if lfs::is_symlink(target) {
                if let Ok(target_link) = fs::read_link(target) {
                    if target_link == source {
                        log::info!("Target epkg binary is a symlink to current executable, skipping copy");
                        return Ok(());
                    } else {
                        log::info!("Target epkg binary is a symlink to different location, proceeding with copy");
                    }
                } else {
                    log::warn!("Failed to read target symlink, proceeding with copy");
                }
            }
            #[cfg(not(unix))]
            {
                // Target exists, proceed with copy
                log::info!("Target epkg binary exists, proceeding with copy");
            }
        } else {
            // Target doesn't exist, proceed with copy
            log::info!("Target epkg binary doesn't exist, proceeding with copy");
        }
    }

    // Create a temporary file in the same directory as the target
    let temp_target = target.with_extension("tmp");

    // Clean up any existing temporary file
    if temp_target.exists() {
        if let Err(e) = lfs::remove_file(&temp_target) {
            log::warn!("Failed to remove existing temporary file {}: {}", temp_target.display(), e);
        }
    }

    // Copy to temporary file first
    lfs::copy(source, &temp_target)?;

    // Set permissions on temporary file before rename (Unix only)
    #[cfg(unix)]
    {
        let mode = if is_epkg && config().init.shared_store {
            0o755
        } else {
            0o755
        };
        utils::set_permissions_from_mode(&temp_target, mode)
            .context(format!("Failed to set permissions on temporary binary"))?;
    }

    // Set POSIX mode on Windows for VM execution (virtiofs needs EA for execute bit)
    #[cfg(windows)]
    {
        let _ = is_epkg; // suppress unused warning
        const S_IFREG: u32 = 0o100000; // Regular file type bit
        let mode = S_IFREG | 0o755; // 0o100755 = regular executable binary
        if let Err(e) = crate::ntfs_ea::set_posix_mode(&temp_target, mode, false) {
            log::warn!("Failed to set POSIX mode on {}: {}", temp_target.display(), e);
        }
    }

    // Atomically rename temporary file to target
    match lfs::rename(&temp_target, target) {
        Ok(_) => {
            Ok(())
        }
        Err(_) => {
            // Clean up temporary file on failure
            lfs::remove_file(&temp_target)
        }
    }
}

/// Create a symlink to the epkg binary for user convenience and system-wide access.
///
/// This function ensures that the 'epkg' binary is easily accessible from the command line.
/// The behavior depends on the installation mode:
///
/// - **Upgrade mode** (`epkg self upgrade`): Returns immediately without creating any symlinks (upgrades preserve existing symlinks).
///
/// - **Shared store mode** (`--store=shared`):
///   - Creates a symlink in `/usr/local/bin/epkg` pointing to the epkg binary.
///   - Creates the `/usr/local/bin` directory if it does not exist.
///   - This makes 'epkg' available system-wide for all users.
///
/// - **User store mode** (default):
///   - Creates a symlink in `$HOME/bin/epkg` if `$HOME/bin` exists and is present in PATH.
///   - This allows immediate access to 'epkg' in the current shell session without requiring a shell restart.
///   - Does not create `$HOME/bin` if it does not exist.
#[cfg(unix)]
fn create_epkg_symlink(epkg_binary_path: &Path) -> Result<()> {
    if config().subcommand == EpkgCommand::SelfUpgrade {
        return Ok(());
    }

    // Symlink to /usr/local/bin
    if config().init.shared_store {
        let usr_local_bin = PathBuf::from("/usr/local/bin");
        lfs::create_dir_all(&usr_local_bin)?;
        println!("Creating symlink: {}/epkg -> {}", usr_local_bin.display(), epkg_binary_path.display());
        if let Err(e) = utils::force_symlink_file_for_native(epkg_binary_path, &usr_local_bin.join("epkg")) {
            log::warn!("Failed to create epkg symlink in {}: {}", usr_local_bin.display(), e);
        }
        return Ok(());
    }

    // Symlink to $HOME/bin if in PATH
    let home = crate::dirs::get_home().wrap_err("Failed to get HOME directory")?;
    let home_bin = PathBuf::from(&home).join("bin");
    let path_var = env::var("PATH")
        .unwrap_or_else(|_| "".to_string());

    if path_var.contains(&*home_bin.to_string_lossy()) {
        if home_bin.exists() {
            println!("Creating symlink: {}/epkg -> {}", home_bin.display(), epkg_binary_path.display());
            if let Err(e) = utils::force_symlink_file_for_native(epkg_binary_path, &home_bin.join("epkg")) {
                log::warn!("Failed to create epkg symlink in {}: {}", home_bin.display(), e);
            }
        }
    }

    Ok(())
}

/// Update shell profile files with epkg configuration.
/// Works on both Unix (bash/zsh rc files) and Windows (PowerShell profiles).
fn update_shell_profile() -> Result<()> {
    let self_env_root = get_env_root(SELF_ENV.to_string())?;
    let profile_paths = shell_profile_paths();

    if profile_paths.is_empty() {
        return Ok(());
    }

    let block = build_epkg_block(&self_env_root);

    for profile_path in profile_paths {
        // Ensure parent directory exists
        if let Some(parent) = profile_path.parent() {
            lfs::create_dir_all(parent)?;
        }
        append_epkg_block_to_text_file(&profile_path, &block)?;
    }

    Ok(())
}

/// Get the list of shell profile paths to update.
#[cfg(unix)]
fn shell_profile_paths() -> Vec<PathBuf> {
    let paths: Vec<String> = if config().init.shared_store {
        crate::dirs::get_global_shell_rc().unwrap_or_default()
    } else {
        let home_path_str = crate::dirs::get_home().unwrap_or_default();
        let home_dir = PathBuf::from(home_path_str);
        crate::dirs::get_user_shell_rc(&home_dir).unwrap_or_default()
    };
    paths.into_iter().map(PathBuf::from).collect()
}

#[cfg(windows)]
fn shell_profile_paths() -> Vec<PathBuf> {
    crate::dirs::powershell_profile_paths()
}

/// Build the epkg block that will be appended to profile files.
#[cfg(unix)]
fn build_epkg_block(self_env_root: &Path) -> String {
    format!(
        r#"
# epkg begin
epkg_rc='{base_path}/usr/src/epkg/assets/shell/epkg.sh'
test -r "$epkg_rc" && . "$epkg_rc"
# epkg end
"#,
        base_path = self_env_root.display(),
    )
}

#[cfg(windows)]
fn build_epkg_block(self_env_root: &Path) -> String {
    let ps1_path = self_env_root
        .join("usr")
        .join("src")
        .join("epkg")
        .join("assets")
        .join("shell")
        .join("epkg.ps1");
    let escaped = ps1_path.display().to_string().replace('\'', "''");
    format!(
        r#"
# epkg begin
$epkg_ps1 = '{escaped}'
if (Test-Path -LiteralPath $epkg_ps1) {{ . $epkg_ps1 }}
# epkg end
"#,
        escaped = escaped
    )
}

fn append_epkg_block_to_text_file(path: &Path, block_content: &str) -> Result<()> {
    // Read existing content and check if it already has the correct block
    let existing_content = if lfs::exists_on_host(path) {
        fs::read_to_string(path)
            .wrap_err_with(|| format!("Failed to read file: {}", path.display()))?
    } else {
        String::new()
    };

    // Check if the existing epkg block matches the new block
    if existing_content.contains(block_content.trim()) {
        // Block already exists with correct content, no need to update
        return Ok(());
    }

    // Remove existing epkg block if present (without printing "Removed" message)
    let cleaned_content = if existing_content.contains("# epkg begin") && existing_content.contains("# epkg end") {
        let lines: Vec<&str> = existing_content.lines().collect();
        let mut new_lines = Vec::new();
        let mut in_epkg_block = false;

        for line in lines {
            if line.contains("# epkg begin") {
                in_epkg_block = true;
                continue;
            }
            if line.contains("# epkg end") {
                in_epkg_block = false;
                continue;
            }
            if !in_epkg_block {
                new_lines.push(line);
            }
        }
        new_lines.join("\n")
    } else {
        existing_content.clone()
    };

    // Write the cleaned content back if we modified it
    if cleaned_content != existing_content {
        lfs::write(path, &cleaned_content)?;
    }

    println!("Adding epkg to: {}", path.display());

    let mut file = OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .with_context(|| format!("Failed to open or create file: {}", path.display()))?;

    if !cleaned_content.is_empty() && !cleaned_content.ends_with('\n') {
        file
            .write_all(b"\n")
            .with_context(|| format!("Failed to write newline to: {}", path.display()))?;
    }

    file
        .write_all(block_content.as_bytes())
        .with_context(|| format!("Failed to write to: {}", path.display()))?;

    Ok(())
}

fn find_repo_root() -> Result<std::path::PathBuf> {
    // Check if running from git repo
    let current_exe = std::env::current_exe()
        .context("Failed to get current executable path")?;

    let repo_root = if let Some(mut path) = current_exe.parent() {
        // Look for .git directory in current directory or up to 3 levels up
        for _ in 0..4 {
            let git_dir = path.join(".git");
            if git_dir.exists() {
                break;
            }
            if let Some(parent) = path.parent() {
                path = parent;
            } else {
                // Reached root directory without finding .git
                break;
            }
        }
        path.to_path_buf()
    } else {
        // If current_exe has no parent, use the current directory
        std::env::current_dir()
            .context("Failed to get current directory")?
    };

    // If we found a valid repo from the executable path, return it
    if is_valid_local_repo(&repo_root) {
        return Ok(repo_root);
    }

    // Fallback: Check if self environment has a symlink to the repo
    // This handles the case where root installed epkg and created a symlink at
    // /opt/epkg/envs/root/self/usr/src/epkg -> /c/epkg, but normal users
    // running the installed epkg don't have the repo in their executable path.
    // We need to check both the current user's self env and root's self env.
    let possible_self_envs = vec![
        crate::dirs::find_env_root(SELF_ENV),
        // Also check root's self environment directly
        Some(dirs().opt_epkg.join("envs").join("root").join(SELF_ENV))
            .filter(|p| p.exists()),
    ];

    for self_env_root_opt in possible_self_envs {
        if let Some(self_env_root) = self_env_root_opt {
            let epkg_src_symlink = crate::dirs::path_join(&self_env_root, &["usr", "src", "epkg"]);
            if epkg_src_symlink.exists() {
                // Check if it's a symlink
                if lfs::is_symlink_or_junction(&epkg_src_symlink) {
                    // Follow the symlink to get the actual repo root
                    // Use canonicalize on the symlink itself to handle both absolute and relative paths
                    if let Ok(canonical_path) = std::fs::canonicalize(&epkg_src_symlink) {
                        if is_valid_local_repo(&canonical_path) {
                            return Ok(canonical_path);
                        }
                    }
                }
            }
        }
    }

    Ok(repo_root)
}

fn is_valid_local_repo(repo_root: &std::path::Path) -> bool {
    repo_root.join(".git").exists() &&
    crate::dirs::path_join(repo_root, &["assets", "shell", "epkg.sh"]).exists()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GiteeRelease {
    tag_name: String,
    prerelease: bool,
    name: String,
    created_at: String,
    assets: Vec<GiteeAsset>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GiteeAsset {
    name: String,
    browser_download_url: String,
}

impl GiteeRelease {
    fn find_asset_url(&self, name: &str) -> Result<String> {
        self.assets.iter()
            .find(|asset| asset.name == name)
            .map(|asset| asset.browser_download_url.clone())
            .ok_or_else(|| {
                eyre::eyre!("Asset '{}' not found in release: {:#?}", name, self)
            })
    }

    fn find_asset_urls_for_arch(&self, prefix: &str, arch: &str) -> Result<(String, String)> {
        let binary_name = format!("{}-{}", prefix, arch);
        let sha_name = format!("{}.sha256", binary_name);
        let binary_url = self.find_asset_url(&binary_name)?;
        let sha_url = self.find_asset_url(&sha_name)?;
        Ok((binary_url, sha_url))
    }

    fn find_asset_urls_for_arch_with_prefixes(
        &self,
        prefixes: &[&str],
        arch: &str,
    ) -> Result<(String, String)> {
        let mut last_err: Option<color_eyre::eyre::Report> = None;
        for prefix in prefixes {
            match self.find_asset_urls_for_arch(prefix, arch) {
                Ok(v) => return Ok(v),
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap_or_else(|| eyre::eyre!("No asset URLs found for arch {}", arch)))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EpkgVersionInfo {
    epkg_version: String,
    // Present only when we can reasonably detect elf-loader version.
    // On non-Linux platforms we keep this as `None` to avoid large `cfg` forks.
    elf_loader_version: Option<String>,
}

#[derive(Debug, Clone)]
struct AssetDownloadPlan {
    // Main asset
    url: String,
    path: std::path::PathBuf,
}

impl AssetDownloadPlan {
    fn is_download(&self) -> bool {
        !self.url.is_empty()
    }

    fn sha_url(&self) -> String {
        format!("{}.sha256", self.url)
    }

    fn sha_path(&self, epkg_download_dir: &std::path::Path) -> Result<std::path::PathBuf> {
        // Derive sha local path using the same cache mapping as other downloads.
        mirror::Mirrors::remote_url_to_path(&self.sha_url(), epkg_download_dir, "epkg")
    }

    #[cfg(feature = "libkrun")]
    fn file_name(&self) -> Result<String> {
        Ok(self.path
            .file_name()
            .ok_or_else(|| eyre::eyre!("Asset path missing file name: {}", self.path.display()))?
            .to_string_lossy()
            .to_string())
    }

    /// Derive `config-*` URL from the `vmlinux-<ver>-<arch>.zst` or `Image-<ver>-<arch>.zst` URL.
    /// Supports both naming conventions:
    /// - x86_64: vmlinux-{version}-{arch}.zst (ELF format)
    /// - aarch64/riscv64: Image-{version}-{arch}.zst (Raw format)
    #[cfg(feature = "libkrun")]
    fn vmlinux_config_url(&self) -> Result<String> {
        let file_name = self.file_name()?;

        // Determine kernel prefix and validate file name
        let (prefix, inner) = if file_name.starts_with("vmlinux-") && file_name.ends_with(".zst") {
            let inner = file_name
                .strip_prefix("vmlinux-")
                .and_then(|s| s.strip_suffix(".zst"))
                .ok_or_else(|| eyre::eyre!("Failed to parse vmlinux asset name: {}", file_name))?;
            ("vmlinux", inner)
        } else if file_name.starts_with("Image-") && file_name.ends_with(".zst") {
            let inner = file_name
                .strip_prefix("Image-")
                .and_then(|s| s.strip_suffix(".zst"))
                .ok_or_else(|| eyre::eyre!("Failed to parse Image asset name: {}", file_name))?;
            ("Image", inner)
        } else {
            return Err(eyre::eyre!("Unexpected kernel asset name (expected vmlinux-*.zst or Image-*.zst): {}", file_name));
        };

        let (version, arch) = inner
            .rsplit_once('-')
            .ok_or_else(|| eyre::eyre!("Failed to split {} name: {}", prefix, file_name))?;

        let config_asset_name = format!("config-{}-{}", version, arch);
        let config_url = self
            .url
            .strip_suffix(&file_name)
            .map(|p| format!("{}{}", p, config_asset_name))
            .ok_or_else(|| eyre::eyre!("Failed to derive kernel config url from {}", self.url))?;

        Ok(config_url)
    }

    #[cfg(feature = "libkrun")]
    fn vmlinux_config_path(&self, epkg_download_dir: &std::path::Path) -> Result<std::path::PathBuf> {
        let config_url = self.vmlinux_config_url()?;
        mirror::Mirrors::remote_url_to_path(&config_url, epkg_download_dir, "epkg")
    }
}

/// Elf-loader handling plan: explicitly models the three possible scenarios
#[derive(Debug, Clone)]
enum ElfLoaderPlan {
    /// Copy from local source to target path (development mode)
    LocalCopy { source: std::path::PathBuf, target: std::path::PathBuf },
    /// Download from URL to target path
    Download { url: String, target: std::path::PathBuf },
    /// No elf-loader needed
    None,
}

#[derive(Debug, Clone)]
struct InitPlan {
    current: EpkgVersionInfo,
    new: EpkgVersionInfo,
    // File paths and URLs
    epkg_src_url: String,
    epkg_src_path: std::path::PathBuf,

    // Self-update assets (epkg + optional elf-loader + optional vmlinux)
    epkg_binary: Option<AssetDownloadPlan>,
    elf_loader: ElfLoaderPlan,
    /// Linux ELF epkg binary for VM usage on Windows/macOS hosts
    epkg_linux: Option<AssetDownloadPlan>,
    #[cfg(feature = "libkrun")]
    vmlinux: Option<AssetDownloadPlan>,
    #[cfg(feature = "libkrun")]
    vmlinux_version: Option<String>,
    // Flags
    need_download_epkg_src: bool,
    using_local_repo: bool,
}

/// Fetch the latest release information from Gitee API
fn fetch_latest_release(owner: &str, repo: &str) -> Result<GiteeRelease> {
    let url = format!("{}/repos/{}/{}/releases/latest", GITEE_API_BASE, owner, repo);

    log::debug!("Request url: {}", url);

    // Create an agent with timeout configuration for better error handling
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(std::time::Duration::from_secs(15)))
        .timeout_recv_response(Some(std::time::Duration::from_secs(30)))
        .build()
        .into();

    // Make the HTTP request with detailed error context
    let mut response = agent.get(&url).call()
        .with_context(|| format!("Failed to fetch release from {}", url))?;

    let status = response.status();
    if status != 200 {
        let body = response.body_mut().read_to_string().unwrap_or_else(|_| "Failed to read error response body".to_string());
        return Err(eyre::eyre!(
            "HTTP {} error when fetching release info from {}: {}",
            status,
            url,
            body
        ));
    }

    let body = response.body_mut().read_to_string()
        .context("Failed to read response body")?;

    // Log full response body for debugging
    log::debug!("Response body: {}", body);

    let release: GiteeRelease = serde_json::from_str(&body)
        .with_context(|| format!(
            "Failed to parse release information from response body: {}",
            body
        ))?;

    Ok(release)
}

/// Parse version from --version output
fn parse_version_from_output(version_output: &str) -> Option<String> {
    // Look for pattern: "... version $version_tag (build date $build_date, commit $git_hash)"
    let re = regex::Regex::new(r"version\s+([^\s]+)\s+\(").ok()?;
    let captures = re.captures(version_output)?;
    Some(captures.get(1)?.as_str().to_string())
}

/// Fetch a specific release by tag name from Gitee.
#[allow(dead_code)]
fn fetch_release_by_tag(owner: &str, repo: &str, tag_name: &str) -> Result<GiteeRelease> {
    let url = format!(
        "{}/repos/{}/{}/releases/tags/{}",
        GITEE_API_BASE, owner, repo, tag_name
    );

    log::debug!("Request url: {}", url);

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(std::time::Duration::from_secs(15)))
        .timeout_recv_response(Some(std::time::Duration::from_secs(30)))
        .build()
        .into();

    let mut response = agent.get(&url).call()
        .with_context(|| format!("Failed to fetch release by tag from {}", url))?;

    let status = response.status();
    if status != 200 {
        let body = response.body_mut().read_to_string().unwrap_or_else(|_| "Failed to read error response body".to_string());
        return Err(eyre::eyre!(
            "HTTP {} error when fetching release info from {}: {}",
            status,
            url,
            body
        ));
    }

    let body = response.body_mut()
        .read_to_string()
        .context("Failed to read response body")?;

    let release: GiteeRelease = serde_json::from_str(&body)
        .with_context(|| format!(
            "Failed to parse release information from response body: {}",
            body
        ))?;

    Ok(release)
}

/// Get version from epkg binary
#[allow(dead_code)]
fn get_epkg_version() -> Result<String> {
    // If this is the running epkg program, use the build-time version
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(file_name) = current_exe.file_name() {
            if let Some(name_str) = file_name.to_str() {
                if name_str.contains("epkg") {
                    return Ok(env!("EPKG_VERSION_TAG").to_string());
                }
            }
        }
    }

    // Try to run epkg --version
    let output = std::process::Command::new("epkg")
        .arg("--version")
        .output()
        .context("Failed to run epkg --version")?;

    if !output.status.success() {
        return Err(eyre::eyre!("epkg --version failed"));
    }

    let version_output = String::from_utf8(output.stdout)
        .context("Failed to parse epkg --version output")?;

    parse_version_from_output(&version_output)
        .ok_or_else(|| eyre::eyre!("Failed to parse version from epkg --version output"))
}

/// Get version from elf-loader binary
fn get_elf_loader_version(elf_loader_path: &Path) -> Result<String> {
    if !elf_loader_path.exists() {
        return Err(eyre::eyre!("elf-loader binary not found"));
    }

    let output = std::process::Command::new(elf_loader_path)
        .arg("--version")
        .output()
        .context("Failed to run elf-loader --version")?;

    if !output.status.success() {
        return Err(eyre::eyre!("elf-loader --version failed"));
    }

    let version_output = String::from_utf8(output.stdout)
        .context("Failed to parse elf-loader --version output")?;

    parse_version_from_output(&version_output)
        .ok_or_else(|| eyre::eyre!("Failed to parse version from elf-loader --version output"))
}

/// Get the current installed version information
fn get_current_epkg_version_info() -> Result<EpkgVersionInfo> {
    let epkg_version = get_epkg_version().unwrap_or_else(|_| env!("EPKG_VERSION_TAG").to_string());

    // Try to find elf-loader in common locations on Linux.
    // On non-Linux platforms we keep it as `None`.
    let elf_loader_version = if std::env::consts::OS == "linux" {
        // Try to find elf-loader in common locations
        let env_root = crate::dirs::find_env_root(SELF_ENV);
        let possible_elf_loader_paths = [
            env_root.as_ref().map(|root| crate::dirs::path_join(root, &["usr", "bin", "elf-loader"])).unwrap_or_else(|| PathBuf::new()),
            dirs()
                .epkg_downloads_cache
                .join("epkg")
                .join(format!("elf-loader-{}", &config().common.arch)),
            PathBuf::from("./elf-loader"),
        ];

        possible_elf_loader_paths
            .iter()
            .find_map(|path| get_elf_loader_version(path).ok())
            .map(Some)
            .unwrap_or(None)
    } else {
        None
    };

    Ok(EpkgVersionInfo {
        epkg_version,
        elf_loader_version,
    })
}

/// Path to the default VM kernel image (from kernel release, written during `epkg self install`).
/// Shared by libkrun and qemu.
/// Uses uniform "kernel" symlink pointing to the actual kernel file:
/// - x86_64: vmlinux-{version}-{arch} (ELF format)
/// - aarch64/riscv64: Image-{version}-{arch} (Raw format)
///
/// Note: The kernel is always loaded from the `self` environment (`~/.epkg/envs/self/boot/kernel`),
/// not from individual target environments. This is because:
/// 1. The kernel is a shared resource used by all VM instances
/// 2. Individual environments (e.g., debian, fedora) don't need their own kernel copies
/// 3. The self environment contains epkg's own binaries and the kernel is part of that
#[cfg(any(feature = "libkrun", target_os = "linux"))]
fn default_kernel_path() -> PathBuf {
    // Uniform kernel symlink name
    // Note: Always use kernel from self env, not from the target environment
    dirs().user_envs.join(SELF_ENV).join("boot").join("kernel")
}

/// Returns the default kernel path as a string if the file exists; otherwise None.
/// Used by libkrun and qemu.
#[cfg(any(feature = "libkrun", target_os = "linux"))]
pub fn default_kernel_path_if_exists() -> Option<String> {
    let default = default_kernel_path();
    // Debug: trace kernel path checking on Windows
    #[cfg(target_os = "windows")]
    crate::debug_epkg!("init: checking default kernel path: {}", default.display());
    if default.exists() {
        #[cfg(target_os = "windows")]
        crate::debug_epkg!("init: default kernel path exists: yes");
        default.into_os_string().into_string().ok()
    } else {
        #[cfg(target_os = "windows")]
        crate::debug_epkg!("init: default kernel path exists: no");
        None
    }
}

/// Get vmlinux download URL for the current architecture from Gitee releases.
/// Returns (url, sha256_url, config_url, version) tuple.
#[cfg(feature = "libkrun")]
fn get_vmlinux_url() -> Result<Option<(String, String, String, String)>> {
    let arch = &config().common.arch;

    // Check if vmlinux is available for this architecture
    match arch.as_str() {
        "x86_64" | "aarch64" | "riscv64" => {}
        "loongarch64" => {
            log::debug!("vmlinux not available for loongarch64, VM feature won't be usable");
            return Ok(None);
        }
        _ => {
            log::debug!("vmlinux not available for {}, VM feature won't be usable", arch);
            return Ok(None);
        }
    };

    // Fetch latest release from Gitee
    let release = fetch_latest_release(GITEE_OWNER, REPO_VMLINUX)?;

    // Find the vmlinux asset for this architecture
    // Format: vmlinux-$kver-$arch.zst (e.g. vmlinux-6.19.6-x86_64.zst)
    let suffix = format!("-{}.zst", arch);
    let asset = release.assets.iter()
        .find(|a| a.name.starts_with("vmlinux-") && a.name.ends_with(&suffix))
        .ok_or_else(|| eyre::eyre!("No vmlinux asset found for architecture {}", arch))?;

    let version = asset.name
        .strip_prefix("vmlinux-")
        .and_then(|s| s.strip_suffix(&suffix))
        .ok_or_else(|| eyre::eyre!("Failed to parse version from asset name: {}", asset.name))?
        .to_string();

    let url = asset.browser_download_url.clone();
    let sha_url = format!("{}.sha256", url);

    // Find config file: config-$kver-$arch (e.g. config-6.19.6-x86_64)
    let config_name = format!("config-{}-{}", version, arch);
    let config_url = release.assets.iter()
        .find(|a| a.name == config_name)
        .map(|a| a.browser_download_url.clone())
        .ok_or_else(|| eyre::eyre!("No config asset found for architecture {}", arch))?;

    Ok(Some((url, sha_url, config_url, version)))
}

/// Install kernel from downloaded .zst file to self/boot directory.
/// Uses unified naming convention:
/// - x86_64: vmlinux-{version}-{arch} (ELF format)
/// - aarch64/riscv64: Image-{version}-{arch} (Raw format)
/// Creates "kernel" symlink pointing to the actual kernel file.
#[cfg(feature = "libkrun")]
fn install_kernel(zst_path: &Path, config_path: Option<&Path>, version: &str, arch: &str) -> Result<()> {
    let self_env_root = dirs().user_envs.join(SELF_ENV);
    let boot_dir = self_env_root.join("boot");
    lfs::create_dir_all(&boot_dir)?;

    // Decompress .zst file
    println!("  Decompressing kernel-{}-{}...", version, arch);
    let kernel_data = zstd_decompress_file(zst_path)?;

    // Determine kernel name prefix based on architecture
    // x86_64 uses vmlinux (ELF), aarch64/riscv64 use Image (Raw)
    let kernel_prefix = if arch == "x86_64" { "vmlinux" } else { "Image" };
    let kernel_name = format!("{}-{}-{}", kernel_prefix, version, arch);
    let kernel_path = boot_dir.join(&kernel_name);
    lfs::write(&kernel_path, &kernel_data)?;

    // Create/update "kernel" symlink pointing to actual kernel file
    let kernel_link = boot_dir.join("kernel");
    if kernel_link.exists() || lfs::is_symlink(&kernel_link) {
        lfs::remove_file(&kernel_link)?;
    }
    // On Windows, use symlink_file_for_native() because libkrun reads the kernel
    // from the Windows host, and LX symlinks are not readable by native Windows.
    #[cfg(windows)]
    lfs::symlink_file_for_native(&kernel_name, &kernel_link)?;
    #[cfg(not(windows))]
    lfs::symlink_file_for_native(&kernel_name, &kernel_link)?;

    println!("  Installed kernel: {} ({} bytes)", kernel_path.display(), kernel_data.len());

    // Install config file
    if let Some(cfg_path) = config_path {
        if cfg_path.exists() {
            let config_name = format!("config-{}-{}", version, arch);
            let config_dest = boot_dir.join(&config_name);
            lfs::copy(cfg_path, &config_dest)?;
            println!("  Installed config: {}", config_dest.display());
        }
    }

    Ok(())
}

/// Decompress a .zst file and return the decompressed data.
#[cfg(feature = "libkrun")]
fn zstd_decompress_file(path: &Path) -> Result<Vec<u8>> {
    use std::io::Read;
    use zstd::stream::Decoder;

    let file = std::fs::File::open(path)
        .context("Failed to open zst file")?;
    let mut decoder = Decoder::new(file)
        .context("Failed to create zstd decoder")?;
    let mut data = Vec::new();
    decoder.read_to_end(&mut data)
        .context("Failed to decompress zst file")?;
    Ok(data)
}

/// Check for updates and return initialization plan
struct ResolvedAssets {
    new_version:      EpkgVersionInfo,
    epkg_binary_url:  String,
    elf_loader_url:   Option<String>,
    /// Linux ELF epkg binary for VM usage on Windows/macOS hosts
    epkg_linux_url:   Option<String>,
}

fn resolve_assets_for_os(
    current_version: &EpkgVersionInfo,
    arch: &str,
    os: &str,
    is_linux: bool,
    using_local_repo: bool,
) -> Result<ResolvedAssets> {
    // Local development mode: running from git repo
    // - On Linux: skip binary download, use local elf-loader if available
    // - On Windows/macOS: skip native binary download, still need epkg-linux for VM
    if using_local_repo {
        let new_version = current_version.clone();
        let epkg_release = fetch_release_by_tag(GITEE_OWNER, REPO_EPKG, &new_version.epkg_version)?;

        // On Linux, we still need to resolve the binary URL for reference
        // On Windows/macOS, skip native binary URL (we're running local build)
        let epkg_binary_url = if is_linux {
            let (bin_url, _sha_url) =
                epkg_release.find_asset_urls_for_arch_with_prefixes(&["epkg-linux", "epkg"], arch)?;
            bin_url
        } else {
            // Windows/macOS: dummy URL, won't be used for download
            String::new()
        };

        // Local elf-loader: no need to fetch URL if local exists, otherwise download latest
        let elf_loader_url = if is_linux {
            match &new_version.elf_loader_version {
                Some(elf_loader_tag) => {
                    let elf_loader_release =
                        fetch_release_by_tag(GITEE_OWNER, REPO_ELF_LOADER, elf_loader_tag)?;
                    let (loader_url, _loader_sha_url) =
                        elf_loader_release.find_asset_urls_for_arch("elf-loader", arch)?;
                    Some(loader_url)
                }
                None => {
                    // No local elf-loader found, download latest from remote
                    log::info!("No local elf-loader found, will download from remote");
                    let elf_loader_release = fetch_latest_release(GITEE_OWNER, REPO_ELF_LOADER)?;
                    match elf_loader_release.find_asset_urls_for_arch("elf-loader", arch) {
                        Ok((loader_url, _)) => Some(loader_url),
                        Err(e) => {
                            log::warn!("Could not resolve elf-loader binary: {}", e);
                            None
                        }
                    }
                }
            }
        } else if cfg!(feature = "libkrun") {
            // On Windows/macOS with libkrun, still need elf-loader for Linux distros running in VM
            // Fetch the latest elf-loader release if no version is specified
            let elf_loader_release = fetch_latest_release(GITEE_OWNER, REPO_ELF_LOADER)?;
            match elf_loader_release.find_asset_urls_for_arch("elf-loader", arch) {
                Ok((loader_url, _)) => Some(loader_url),
                Err(e) => {
                    log::warn!("Could not resolve elf-loader binary: {}", e);
                    None
                }
            }
        } else {
            None
        };

        // On Windows/macOS, still need epkg-linux for VM usage
        let epkg_linux_url = if !is_linux {
            let (linux_url, _sha_url) =
                epkg_release.find_asset_urls_for_arch_with_prefixes(&["epkg-linux", "epkg"], arch)?;
            Some(linux_url)
        } else {
            None
        };

        return Ok(ResolvedAssets {
            new_version,
            epkg_binary_url,
            elf_loader_url,
            epkg_linux_url,
        });
    }

    // Production mode - fetch releases and resolve URLs from assets.
    let epkg_release = fetch_latest_release(GITEE_OWNER, REPO_EPKG)?;
    let epkg_version = epkg_release.tag_name.clone();

    let (elf_loader_version, elf_loader_url) = if is_linux {
        let elf_loader_release = fetch_latest_release(GITEE_OWNER, REPO_ELF_LOADER)?;
        let elf_loader_version = Some(elf_loader_release.tag_name.clone());
        let (loader_url, _loader_sha_url) =
            elf_loader_release.find_asset_urls_for_arch("elf-loader", arch)?;
        (elf_loader_version, Some(loader_url))
    } else if cfg!(feature = "libkrun") {
        // On Windows/macOS with libkrun, still need elf-loader for Linux distros running in VM
        let elf_loader_release = fetch_latest_release(GITEE_OWNER, REPO_ELF_LOADER)?;
        let elf_loader_version = Some(elf_loader_release.tag_name.clone());
        let (loader_url, _loader_sha_url) =
            elf_loader_release.find_asset_urls_for_arch("elf-loader", arch)?;
        (elf_loader_version, Some(loader_url))
    } else {
        (None, None)
    };

    let new_version = EpkgVersionInfo {
        epkg_version,
        elf_loader_version,
    };

    let epkg_binary_url = if is_linux {
        let (bin_url, _sha_url) =
            epkg_release.find_asset_urls_for_arch_with_prefixes(&["epkg-linux", "epkg"], arch)?;
        bin_url
    } else if os == "windows" {
        let binary_name = format!("epkg-windows-{}.exe", arch);
        epkg_release.find_asset_url(&binary_name)?
    } else if os == "macos" {
        let (bin_url, _sha_url) = epkg_release.find_asset_urls_for_arch("epkg-macos", arch)?;
        bin_url
    } else {
        return Err(eyre::eyre!("Unsupported OS for asset resolution: {}", os));
    };

    // On Windows/macOS, also resolve epkg-linux-$arch for VM usage.
    // This binary will be used as /usr/bin/init inside the Linux VM.
    let epkg_linux_url = if !is_linux {
        match epkg_release.find_asset_urls_for_arch_with_prefixes(&["epkg-linux", "epkg"], arch) {
            Ok((linux_url, _)) => {
                log::debug!("Resolved epkg-linux URL for VM: {}", linux_url);
                Some(linux_url)
            }
            Err(e) => {
                log::warn!("Could not resolve epkg-linux binary for VM: {}", e);
                None
            }
        }
    } else {
        None
    };

    Ok(ResolvedAssets {
        new_version,
        epkg_binary_url,
        elf_loader_url,
        epkg_linux_url,
    })
}

#[cfg(feature = "libkrun")]
fn resolve_vmlinux_plan(
    epkg_download_dir: &Path,
) -> Result<(Option<AssetDownloadPlan>, Option<String>)> {
    match get_vmlinux_url() {
        Ok(Some((url, _sha_url, _config_url, version))) => {
            let path = mirror::Mirrors::remote_url_to_path(&url, epkg_download_dir, "epkg")?;
            Ok((Some(AssetDownloadPlan { url, path }), Some(version)))
        }
        Ok(None) => Ok((None, None)),
        Err(e) => {
            log::warn!("Failed to get vmlinux URL: {}", e);
            Ok((None, None))
        }
    }
}

#[cfg(not(feature = "libkrun"))]
fn resolve_vmlinux_plan(
    _epkg_download_dir: &Path,
) -> Result<(Option<AssetDownloadPlan>, Option<String>)> {
    Ok((None, None))
}

struct UpdateContext {
    arch: String,
    os: String,
    is_linux: bool,
    epkg_download_dir: PathBuf,
    repo_root: PathBuf,
    using_local_repo: bool,
    local_elf_loader_path: Option<PathBuf>,
}

fn resolve_update_context() -> Result<(UpdateContext, EpkgVersionInfo)> {
    let current_version = get_current_epkg_version_info()?;

    let arch = config().common.arch.clone();
    let os = std::env::consts::OS.to_string();
    let is_linux = os == "linux";

    let dirs = dirs();
    let epkg_download_dir = dirs.epkg_downloads_cache.clone();

    // Check for local repo BEFORE making API calls
    let repo_root = find_repo_root()?;
    let using_local_repo = is_valid_local_repo(&repo_root);

    let local_elf_loader_path = if is_linux {
        Some(crate::dirs::path_join(
            &repo_root,
            &["git", "elf-loader", "src", "loader"],
        ))
    } else {
        None
    };

    Ok((
        UpdateContext {
            arch,
            os,
            is_linux,
            epkg_download_dir,
            repo_root,
            using_local_repo,
            local_elf_loader_path,
        },
        current_version,
    ))
}

fn print_version_info(current: &EpkgVersionInfo, new: &EpkgVersionInfo, is_linux: bool) {
    println!("  epkg: {} → {}", current.epkg_version, new.epkg_version);
    if is_linux || cfg!(feature = "libkrun") {
        if new.elf_loader_version.is_some() {
            println!(
                "  elf-loader: {:?} → {:?}",
                current.elf_loader_version, new.elf_loader_version
            );
        }
    }
}

fn build_epkg_src_url(version: &str) -> String {
    format!(
        "https://gitee.com/{}/{}/repository/archive/{}.tar.gz",
        GITEE_OWNER, REPO_EPKG, version
    )
}

fn resolve_elf_loader_plan(
    arch: &str,
    epkg_download_dir: &Path,
    local_elf_loader_path: &Option<PathBuf>,
    elf_loader_url: &Option<String>,
) -> Result<ElfLoaderPlan> {
    Ok(if let Some(ref source) = local_elf_loader_path {
        if source.exists() {
            // Local elf-loader exists: copy to default target path
            let target = epkg_download_dir.join(format!("elf-loader-{}", arch));
            log::debug!(
                "elf-loader plan: LocalCopy from {} to {}",
                source.display(),
                target.display()
            );
            ElfLoaderPlan::LocalCopy {
                source: source.clone(),
                target,
            }
        } else if let Some(ref url) = elf_loader_url {
            // Local path specified but file doesn't exist: download instead
            let target = mirror::Mirrors::remote_url_to_path(url, epkg_download_dir, "epkg")?;
            log::debug!("elf-loader plan: Download from {} to {}", url, target.display());
            ElfLoaderPlan::Download {
                url: url.clone(),
                target,
            }
        } else {
            ElfLoaderPlan::None
        }
    } else if let Some(ref url) = elf_loader_url {
        // No local elf-loader: download from remote
        let target = mirror::Mirrors::remote_url_to_path(url, epkg_download_dir, "epkg")?;
        log::debug!("elf-loader plan: Download from {} to {}", url, target.display());
        ElfLoaderPlan::Download {
            url: url.clone(),
            target,
        }
    } else {
        ElfLoaderPlan::None
    })
}

fn resolve_epkg_linux_plan(
    arch: &str,
    is_linux: bool,
    repo_root: &Path,
    epkg_download_dir: &Path,
    epkg_linux_url: Option<String>,
) -> Result<Option<AssetDownloadPlan>> {
    Ok(if !is_linux {
        // Prefer cross-compiled Linux targets over native build.
        // The native `target/debug/epkg` is a macOS/Windows binary, not Linux ELF.
        // Only use it as a fallback when no cross-compiled version exists and
        // we need to check if it's actually a Linux ELF (e.g., symlink to cross-build).
        let candidates = [
            repo_root
                .join("target")
                .join(format!("{}-unknown-linux-musl", arch))
                .join("debug")
                .join("epkg"),
            repo_root
                .join("target")
                .join(format!("{}-unknown-linux-gnu", arch))
                .join("debug")
                .join("epkg"),
            // Native build path - only valid if it's actually a Linux ELF
            // (e.g., when cross-compiling and creating a symlink)
            repo_root.join("target").join("debug").join("epkg"),
        ];

        // Find first candidate that exists AND is a Linux ELF binary
        let local_epkg_linux = candidates.into_iter().find(|p| {
            if p.exists() {
                // Check if it's a Linux ELF binary by reading magic bytes
                if let Ok(data) = std::fs::read(p) {
                    // ELF magic: 0x7f 'E' 'L' 'F'
                    data.len() >= 4 && data[0] == 0x7f && data[1] == b'E' && data[2] == b'L' && data[3] == b'F'
                } else {
                    false
                }
            } else {
                false
            }
        });
        if let Some(local_epkg_linux) = local_epkg_linux {
            log::debug!("epkg-linux plan: local copy from {}", local_epkg_linux.display());
            Some(AssetDownloadPlan {
                // Empty URL means local-copy mode (skip download + checksum flow).
                url: String::new(),
                path: local_epkg_linux,
            })
        } else if let Some(linux_url) = epkg_linux_url {
            let linux_path = mirror::Mirrors::remote_url_to_path(
                &linux_url,
                epkg_download_dir,
                "epkg",
            )?;
            log::debug!("epkg-linux plan: download {} -> {}", linux_url, linux_path.display());
            Some(AssetDownloadPlan { url: linux_url, path: linux_path })
        } else {
            None
        }
    } else {
        None
    })
}

fn check_for_updates() -> Result<InitPlan> {
    println!("Checking for updates...");

    let (ctx, current_version) = resolve_update_context()?;

    let ResolvedAssets {
        new_version,
        epkg_binary_url,
        elf_loader_url,
        epkg_linux_url,
    } = resolve_assets_for_os(
        &current_version,
        &ctx.arch,
        &ctx.os,
        ctx.is_linux,
        ctx.using_local_repo,
    )?;

    // Always show version information
    print_version_info(&current_version, &new_version, ctx.is_linux);

    let epkg_src_url = build_epkg_src_url(&new_version.epkg_version);

    // Set up file paths using the same resolution logic as the download system.
    // This ensures paths match where files are actually downloaded.
    // Note: epkg_binary_url may be empty on Windows/macOS with local dev mode
    let epkg_binary_path = if epkg_binary_url.is_empty() {
        PathBuf::new() // Won't be used
    } else {
        mirror::Mirrors::remote_url_to_path(&epkg_binary_url, &ctx.epkg_download_dir, "epkg")?
    };
    let epkg_src_path =
        mirror::Mirrors::remote_url_to_path(&epkg_src_url, &ctx.epkg_download_dir, "epkg")?;

    // Determine what needs to be downloaded.
    // epkg binary is downloaded only when:
    // - version differs AND
    // - we have a URL (non-empty, i.e., not local dev mode on Windows/macOS)
    let need_download_epkg_binary = new_version.epkg_version != current_version.epkg_version
        && !epkg_binary_url.is_empty();
    let need_download_epkg_src = !ctx.using_local_repo;

    let elf_loader_plan = resolve_elf_loader_plan(
        &ctx.arch,
        &ctx.epkg_download_dir,
        &ctx.local_elf_loader_path,
        &elf_loader_url,
    )?;

    // Optional addon (libkrun).
    #[cfg(feature = "libkrun")]
    let (vmlinux_plan, vmlinux_version) = resolve_vmlinux_plan(&ctx.epkg_download_dir)?;
    #[cfg(not(feature = "libkrun"))]
    let _ = resolve_vmlinux_plan(&ctx.epkg_download_dir)?;

    let epkg_binary_plan: Option<AssetDownloadPlan> = if need_download_epkg_binary {
        Some(AssetDownloadPlan {
            url: epkg_binary_url.clone(),
            path: epkg_binary_path,
        })
    } else {
        None
    };

    let epkg_linux_plan = resolve_epkg_linux_plan(
        &ctx.arch,
        ctx.is_linux,
        &ctx.repo_root,
        &ctx.epkg_download_dir,
        epkg_linux_url,
    )?;

    let init_plan = InitPlan {
        current: current_version,
        new: new_version,
        epkg_src_url,
        epkg_src_path,
        epkg_binary: epkg_binary_plan,
        elf_loader: elf_loader_plan,
        epkg_linux: epkg_linux_plan,
        #[cfg(feature = "libkrun")]
        vmlinux: vmlinux_plan,
        #[cfg(feature = "libkrun")]
        vmlinux_version,
        need_download_epkg_src,
        using_local_repo: ctx.using_local_repo,
    };

    // Debug print the InitPlan
    log::debug!(
        "InitPlan: current.epkg={}, new.epkg={}, \
         epkg_binary={:?}, elf_loader={:?}, \
         need_download_epkg_src={}, using_local_repo={}",
        init_plan.current.epkg_version,
        init_plan.new.epkg_version,
        init_plan.epkg_binary.as_ref().map(|p| &p.path),
        &init_plan.elf_loader,
        init_plan.need_download_epkg_src,
        init_plan.using_local_repo,
    );

    Ok(init_plan)
}


/// Fix up /lib64 symlink in the host OS.
/// - If /lib64 already exists as a symlink to usr/lib64: fine and return
/// - If /lib64 already exists as a symlink to usr/lib (archlinux host): remove it or warn 'rpm/deb guest os may not work'
/// - If /lib64 not exists (alpine host): create symlink to usr/lib64 or warn 'guest os other than alpine/archlinux/conda may not work'
/// Only works when running as root.
#[cfg(unix)]
fn fixup_host_lib64_symlink() -> Result<()> {
    let lib64_path = Path::new("/lib64");
    let usr_lib64_target = Path::new("usr/lib64");

    // Check if /lib64 already exists as a symlink
    if lfs::is_symlink(lib64_path) {
        if let Ok(target) = fs::read_link(lib64_path) {
            // Check if it points to usr/lib64 (correct)
            if target == usr_lib64_target {
                // Already correct, nothing to do
                return Ok(());
            }

            // Check if it points to usr/lib (needs fixing on usr-merge systems like Arch)
            let usr_lib_target = Path::new("usr/lib");
            if target == usr_lib_target {
                if utils::is_running_as_root() {
                    // Remove the old symlink so we can create the correct one
                    lfs::remove_file(lib64_path)?;
                    // Fall through to create the correct symlink
                } else {
                    // Not root, can't fix it
                    eprintln!("WARNING: /lib64 -> usr/lib symlink exists but cannot be fixed to usr/lib64 (not running as root). RPM/Debian guest OS may not work.");
                    return Err(eyre::eyre!("/lib64 -> usr/lib exists but cannot be fixed: not running as root"));
                }
            } else {
                // Points to something else, don't touch it
                return Err(eyre::eyre!("/lib64 exists as symlink pointing to {:?}, not fixing", target));
            }
        }
    } else if lib64_path.exists() {
        // /lib64 exists but is not a symlink (directory or file)
        return Err(eyre::eyre!("/lib64 exists but is not a symlink, cannot fix"));
    }

    // /lib64 doesn't exist (or was just removed), need to create it
    if !utils::is_running_as_root() {
        eprintln!("WARNING: /lib64 -> usr/lib64 symlink does not exist and cannot be created (not running as root). Guest OS other than Alpine/ArchLinux/Conda may not work.");
        return Err(eyre::eyre!("Cannot create /lib64 symlink: not running as root"));
    }

    // Create the symlink using relative path
    lfs::symlink_dir_for_native(usr_lib64_target, lib64_path)?;

    Ok(())
}
