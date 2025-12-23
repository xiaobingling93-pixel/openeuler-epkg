use std::fs;
use std::env;
use std::path::Path;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::symlink;
use color_eyre::Result;
use color_eyre::eyre::WrapErr;
use color_eyre::eyre;
use crate::models::*;
use crate::download::download_urls;
use crate::utils;
use crate::dirs::{find_env_root, get_env_root};
use crate::models::dirs;
use crate::mirror;
use crate::deinit::remove_epkg_from_rc_file;
use std::fs::OpenOptions;
use std::io::Write as IoWrite;
use std::path::PathBuf;
use nix::unistd::{fork, ForkResult};
use serde::{Deserialize, Serialize};

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

impl PackageManager {

    // After root run `epkg self install --store=shared`, /usr/local/bin/epkg will be created and exposed
    // to normal users. Then everyone can run "epkg install". To make it user friendly, here we'll
    // auto trigger light_init() seemlessly at first invocation.
    pub fn try_light_init(&mut self) -> Result<()> {
        if matches!(config().subcommand,
              EpkgCommand::Unpack
            | EpkgCommand::Convert
            | EpkgCommand::Hash
            | EpkgCommand::Repo
            | EpkgCommand::SelfInstall
            | EpkgCommand::SelfUpgrade
            | EpkgCommand::SelfRemove
            | EpkgCommand::Run
            | EpkgCommand::None
        ) {
            return Ok(());
        }

        if find_env_root(MAIN_ENV).is_some() {
            return Ok(());
        }

        self.light_init()?;

        Ok(())
    }

    pub fn light_init(&mut self) -> Result<()> {
        // Create main environment
        self.create_environment(MAIN_ENV)?;

        // Load the environment config that was just created and register it
        let env_config = crate::io::deserialize_env_config_for(MAIN_ENV.to_string())?;
        self.register_environment_for(MAIN_ENV, env_config)?;

        // Update shell configuration
        self.update_shell_rc()?;

        println!("Notice: for changes to take effect, close and re-open your current shell.");
        Ok(())
    }

    pub fn upgrade_epkg(&mut self) -> Result<()> {
        // Check if self environment exists
        if find_env_root(SELF_ENV).is_none() {
            eprintln!("epkg is not installed. Please run 'epkg self install' first.");
            return Ok(());
        }

        // Check for available updates and get initialization plan
        match check_for_updates() {
            Ok(init_plan) => {
                // Check if upgrade is needed
                if init_plan.new.epkg_version != init_plan.current.epkg_version ||
                    init_plan.new.elf_loader_version != init_plan.current.elf_loader_version {
                    println!("Upgrading epkg installation...");
                    self.download_setup_files(&init_plan)?;
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

    pub fn install_epkg(&mut self) -> Result<()> {
        fixup_host_lib64_symlink()
            .unwrap_or_else(|e| {
                log::debug!("Could not fixup /lib64 symlink: {}", e);
            });

        // Set up installation paths
        fs::create_dir_all(&dirs().epkg_downloads_cache.join("epkg"))
            .context("Failed to create epkg downloads directory")?;

        print_banner();

        // Pre-populate country cache in background thread to speed up later invocations
        pre_populate_country_cache();

        // For fresh install, create a basic init plan
        let init_plan = check_for_updates()?;
        self.download_setup_files(&init_plan)?;

        self.create_environment(SELF_ENV)?;

        Ok(())
    }

    fn download_package_manager_files(&self, init_plan: &InitPlan) -> Result<()> {
        // Collect urls for downloading in parallel
        let mut urls = Vec::new();

        // Handle epkg source code (local repo or download)
        if init_plan.need_download_epkg_src {
            println!("Downloading epkg source code from {}", init_plan.epkg_src_url);
            urls.push(init_plan.epkg_src_url.clone());
        }

        // Download epkg binary if upgrading
        if init_plan.need_download_epkg_binary {
            println!("Downloading epkg binary from {}", init_plan.epkg_binary_url);
            urls.extend(vec![
                init_plan.epkg_binary_url.clone(),
                init_plan.epkg_binary_sha_url.clone()
            ]);
        }

        // Check for local elf-loader
        if let Some(ref local_loader) = init_plan.local_elf_loader_path {
            // Ensure parent directory exists before copying
            if let Some(parent) = init_plan.elf_loader_path.parent() {
                fs::create_dir_all(parent)
                    .context(format!("Failed to create parent directory for {}", init_plan.elf_loader_path.display()))?;
            }
            fs::copy(local_loader, &init_plan.elf_loader_path)
                .context(format!("Failed to copy local elf-loader from {} to {}",
                    local_loader.display(), init_plan.elf_loader_path.display()))?;
            println!("Using local elf-loader from {}", local_loader.display());
        } else if init_plan.need_download_elf_loader {
            println!("Downloading elf-loader from {}", init_plan.elf_loader_url);
            urls.extend(vec![
                init_plan.elf_loader_url.clone(),
                init_plan.elf_loader_sha_url.clone()
            ]);
        }

        if urls.is_empty() {
            return Ok(());
        }

        // Delete .sha256 files first: gitee.com HTTP headers have no file timestamp,
        // so download.rs would think "File unchanged" based on file size matching.
        let sha256_files_to_delete = vec![&init_plan.elf_loader_sha_path, &init_plan.epkg_binary_sha_path];
        for sha256_path in sha256_files_to_delete {
            if sha256_path.exists() {
                log::debug!("Deleting existing .sha256 file: {}", sha256_path.display());
                if let Err(e) = fs::remove_file(sha256_path) {
                    log::warn!("Failed to delete existing .sha256 file {}: {}", sha256_path.display(), e);
                }
            }
        }

        // Download to the new epkg subdirectory within downloads cache
        // Use the base directory - download_urls will construct nested paths internally
        let download_results = download_urls(urls);
        for result in download_results {
            result.with_context(|| "Failed to download package manager files")?;
        }

        // Verify checksums
        if init_plan.local_elf_loader_path.is_none() && init_plan.need_download_elf_loader {
            utils::verify_sha256sum(&init_plan.elf_loader_sha_path)
                .context("Failed to verify elf-loader checksum")?;
        }

        if init_plan.need_download_epkg_binary {
            utils::verify_sha256sum(&init_plan.epkg_binary_sha_path)
                .context("Failed to verify epkg binary checksum")?;
        }

        if init_plan.need_download_epkg_src && !init_plan.epkg_src_path.exists() {
            return Err(eyre::eyre!("Failed to download epkg source code tar file from {}", init_plan.epkg_src_url));
        }

        Ok(())
    }

    fn download_setup_files(&mut self, init_plan: &InitPlan) -> Result<()> {
        let self_env_root = dirs().user_envs.join(SELF_ENV);

        self.download_package_manager_files(init_plan)
            .context("Failed to download required files for self environment")?;

        self.setup_epkg_src(&self_env_root, init_plan)?;
        self.setup_common_binaries(&self_env_root, init_plan)?;

        Ok(())
    }

    fn setup_epkg_src(&self, env_root: &Path, init_plan: &InitPlan) -> Result<()> {
        let usr_src = env_root.join("usr/src");
        let epkg_src = usr_src.join("epkg");

        // Check if we're using a local repository
        if init_plan.using_local_repo {
            // Create symlink directly to git working directory
            if !usr_src.exists() {
                fs::create_dir_all(&usr_src)
                    .context("Failed to create usr/src directory in environment")?;
            }

            if !epkg_src.exists() {
                let repo_root = find_repo_root()?;
                symlink(repo_root.to_str().unwrap(), &epkg_src)
                    .context("Failed to create symlink to local repository")?;
            }

            println!("Using local git repository for epkg source code");
            return Ok(());
        }

        // Extract epkg source code tar for remote repository
        let epkg_extracted_dir = format!("epkg-{}", init_plan.new.epkg_version);
        let epkg_extracted_path = usr_src.join(&epkg_extracted_dir);

        println!("Extracting epkg source code to: {}", usr_src.display());

        if epkg_extracted_path.exists() {
            fs::remove_dir_all(&epkg_extracted_path)?;
        } else {
            fs::create_dir_all(&usr_src)
                .context(format!("Failed to create opt directory at {}", usr_src.display()))?;
        }

        // Extract tar.gz file with error handling
        utils::extract_tar_gz(&init_plan.epkg_src_path, &usr_src)
            .context("Failed to extract epkg source code tar file")?;

        // Create a symlink from epkg to epkg-master (or epkg-$version)
        if let Err(e) = utils::force_symlink(&epkg_extracted_dir, &epkg_src) {
            eprintln!("[WARN] Failed to create symlink {} -> {}: {}",
                     epkg_src.display(), epkg_extracted_dir, e);
        }

        Ok(())
    }

    fn setup_common_binaries(&self, env_root: &Path, init_plan: &InitPlan) -> Result<()> {
        let usr_bin = env_root.join("usr/bin");

        fs::create_dir_all(&usr_bin)
            .context(format!("Failed to create usr/bin directory at {}", usr_bin.display()))?;

        let target_epkg = usr_bin.join("epkg");

        // Determine epkg binary source based on whether we're upgrading or installing
        let epkg_source = if config().init.upgrade {
            // Use downloaded epkg binary for upgrades
            if !init_plan.epkg_binary_path.exists() {
                return Err(eyre::eyre!("Downloaded epkg binary not found at {}", init_plan.epkg_binary_path.display()));
            }
            init_plan.epkg_binary_path.clone()
        } else {
            // Use current executable for normal installs
            std::env::current_exe()
                .context("Failed to get current executable path")?
        };

        // Copy epkg binary using atomic operation
        self.copy_epkg_binary_atomically(&epkg_source, &target_epkg, true)?;

        // Copy elf-loader binary using atomic operation
        let elf_loader_target = usr_bin.join("elf-loader");
        self.copy_epkg_binary_atomically(&init_plan.elf_loader_path, &elf_loader_target, false)?;

        // Create symlink to epkg binary in the first valid PATH component
        self.create_epkg_symlink(&target_epkg)
            .context("Failed to create epkg symlink in PATH")?;

        Ok(())
    }

    /// Safely copy a binary using atomic operations to avoid conflicts with running processes
    fn copy_epkg_binary_atomically(&self, source: &Path, target: &Path, is_epkg: bool) -> Result<()> {
        // Check if we're trying to copy the epkg binary to itself or to a location that would conflict
        if is_epkg {
            if source == target {
                log::info!("Target epkg binary is the same as current executable, skipping copy");
                return Ok(());
            } else if target.exists() {
                // Check if target is a symlink pointing to current executable
                if let Ok(target_metadata) = fs::symlink_metadata(target) {
                    if target_metadata.file_type().is_symlink() {
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
                    } else {
                        // Target exists and is not a symlink, proceed with copy
                        log::info!("Target epkg binary exists, proceeding with copy");
                    }
                } else {
                    log::warn!("Failed to get target metadata, proceeding with copy");
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
            if let Err(e) = fs::remove_file(&temp_target) {
                log::warn!("Failed to remove existing temporary file {}: {}", temp_target.display(), e);
            }
        }

        // Copy to temporary file first
        fs::copy(source, &temp_target)
            .context(format!("Failed to copy binary to temporary file: {} -> {}",
                source.display(), temp_target.display()))?;

        // Set permissions on temporary file before rename
        let mode = if is_epkg && config().init.shared_store {
            0o4755 // setuid + rwxr-xr-x for epkg in shared store mode
        } else {
            0o755 // rwxr-xr-x for standard permissions
        };
        fs::set_permissions(&temp_target, fs::Permissions::from_mode(mode))
            .context(format!("Failed to set permissions on temporary binary"))?;

        // Atomically rename temporary file to target
        match fs::rename(&temp_target, target) {
            Ok(_) => {
                log::debug!("Successfully copied binary using atomic operation: {} -> {}",
                    source.display(), target.display());
                Ok(())
            }
            Err(e) => {
                // Clean up temporary file on failure
                if let Err(cleanup_err) = fs::remove_file(&temp_target) {
                    log::warn!("Failed to clean up temporary file {} after rename failure: {}",
                        temp_target.display(), cleanup_err);
                }
                Err(eyre::eyre!("Failed to atomically rename binary: {} -> {}: {}",
                    temp_target.display(), target.display(), e))
            }
        }
    }

    /// Create symlinks to the epkg binary for user convenience and system-wide access.
    ///
    /// This function ensures that the 'epkg' binary is easily accessible from the command line by creating symlinks in multiple locations:
    ///
    /// 1. Always creates a symlink in the main environment's 'usr/ebin' directory:
    ///    - This directory is prepended to the user's PATH by default, ensuring 'epkg' is available in new shells.
    ///    - This provides a reliable entry point for the binary.
    ///
    /// 2. Best-effort symlink in $HOME/bin (if present in PATH):
    ///    - If the user's PATH contains $HOME/bin, a symlink is created there.
    ///    - This allows immediate access to 'epkg' in the current shell session without requiring a shell restart.
    ///    - Only attempts to create $HOME/bin if it does not already exist.
    ///
    /// 3. Best-effort symlink in /usr/local/bin (if running as root):
    ///    - If the process is running as root and /usr/local/bin exists, a symlink is created there.
    ///    - This makes 'epkg' available system-wide for all users.
    ///    - Does not attempt to create /usr/local/bin if it does not exist.
    fn create_epkg_symlink(&self, epkg_binary_path: &Path) -> Result<()> {
        if config().init.upgrade {
            return Ok(());
        }

        // Try to create symlink in $HOME/bin if it's in PATH
        let home = crate::dirs::get_home().wrap_err("Failed to get HOME directory")?;
        let home_bin = PathBuf::from(&home).join("bin");
        let path_var = env::var("PATH")
            .unwrap_or_else(|_| "".to_string());

        if path_var.contains(&*home_bin.to_string_lossy()) {
            if home_bin.exists() {
                println!("Creating symlink: {}/epkg -> {}", home_bin.display(), epkg_binary_path.display());
                if let Err(e) = utils::force_symlink(epkg_binary_path, &home_bin.join("epkg")) {
                    log::warn!("Failed to create epkg symlink in {}: {}", home_bin.display(), e);
                }
            }
        }

        // Try to create symlink in /usr/local/bin if running as root
        if utils::is_running_as_root() {
            let usr_local_bin = PathBuf::from("/usr/local/bin");
            fs::create_dir_all(&usr_local_bin)
                .context(format!("Failed to create /usr/local/bin directory at {}", usr_local_bin.display()))?;
            println!("Creating symlink: {}/epkg -> {}", usr_local_bin.display(), epkg_binary_path.display());
            if let Err(e) = utils::force_symlink(epkg_binary_path, &usr_local_bin.join("epkg")) {
                log::warn!("Failed to create epkg symlink in {}: {}", usr_local_bin.display(), e);
            }
        }

        Ok(())
    }

    fn update_shell_rc(&mut self) -> Result<()> {
        let shell_rc_infos = crate::dirs::get_shell_rc()?;

        if shell_rc_infos.is_empty() {
            // No specific shell found via SHELL var, and no common rc files detected.
            // A warning would have been printed by get_shell_rc in this case.
            return Ok(());
        }

        let self_env_root = get_env_root(SELF_ENV.to_string())?;

        for shell_rc_info in shell_rc_infos {
            let rc_content = format!(r#"
# epkg begin
epkg_rc='{base_path}/usr/src/epkg/lib/{script_name}'
test -r "$epkg_rc" && . "$epkg_rc"
# epkg end
"#,
                base_path = self_env_root.display(),
                script_name = shell_rc_info.source_script_name
            );

            // Remove any existing epkg configuration and get the cleaned content
            let existing_content = remove_epkg_from_rc_file(&shell_rc_info.rc_file_path)?;

            // Append the new configuration
            println!("Adding epkg to shell RC file: {}", shell_rc_info.rc_file_path);

            let mut file = OpenOptions::new()
                .append(true)
                .create(true) // Create if it doesn't exist
                .open(&shell_rc_info.rc_file_path)
                .with_context(|| format!("Failed to open or create shell rc file: {}", shell_rc_info.rc_file_path))?;

            // If the file was empty or didn't end with a newline, add one before our content for neatness.
            if !existing_content.is_empty() && !existing_content.ends_with('\n') {
                file.write_all(b"\n")
                    .with_context(|| format!("Failed to write newline to shell rc file: {}", shell_rc_info.rc_file_path))?;
            }

            file.write_all(rc_content.as_bytes())
                .with_context(|| format!("Failed to write to shell rc file: {}", shell_rc_info.rc_file_path))?;
        }

        Ok(())
    }
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
        find_env_root(SELF_ENV),
        // Also check root's self environment directly
        Some(dirs().opt_epkg.join("envs").join("root").join(SELF_ENV))
            .filter(|p| p.exists()),
    ];

    for self_env_root_opt in possible_self_envs {
        if let Some(self_env_root) = self_env_root_opt {
            let epkg_src_symlink = self_env_root.join("usr/src/epkg");
            if epkg_src_symlink.exists() {
                // Check if it's a symlink
                if let Ok(metadata) = fs::symlink_metadata(&epkg_src_symlink) {
                    if metadata.file_type().is_symlink() {
                        // Follow the symlink to get the actual repo root
                        // Use canonicalize on the symlink itself to handle both absolute and relative paths
                        if let Ok(canonical_path) = fs::canonicalize(&epkg_src_symlink) {
                            if is_valid_local_repo(&canonical_path) {
                                return Ok(canonical_path);
                            }
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
    repo_root.join("lib/epkg-rc.sh").exists()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GiteeRelease {
    tag_name: String,
    name: String,
    created_at: String,
    assets: Vec<GiteeAsset>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GiteeAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EpkgVersionInfo {
    epkg_version: String,
    elf_loader_version: String,
}

#[derive(Debug, Clone)]
struct InitPlan {
    current: EpkgVersionInfo,
    new: EpkgVersionInfo,
    // File paths and URLs
    epkg_binary_url: String,
    epkg_binary_sha_url: String,
    epkg_src_url: String,
    elf_loader_url: String,
    elf_loader_sha_url: String,
    // Local file paths
    epkg_binary_path: std::path::PathBuf,
    epkg_binary_sha_path: std::path::PathBuf,
    epkg_src_path: std::path::PathBuf,
    elf_loader_path: std::path::PathBuf,
    elf_loader_sha_path: std::path::PathBuf,
    // Flags
    need_download_epkg_binary: bool,
    need_download_epkg_src: bool,
    need_download_elf_loader: bool,
    using_local_repo: bool,
    // Local elf-loader info
    local_elf_loader_path: Option<std::path::PathBuf>,
}

/// Fetch the latest release information from Gitee API
fn fetch_latest_release(owner: &str, repo: &str) -> Result<GiteeRelease> {
    let url = format!("https://gitee.com/api/v5/repos/{}/{}/releases/latest", owner, repo);

    // Create an agent with timeout configuration for better error handling
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(std::time::Duration::from_secs(15)))
        .timeout_recv_response(Some(std::time::Duration::from_secs(30)))
        .build()
        .into();

    // Make the HTTP request with detailed error context
    let mut response = agent.get(&url)
        .call()
        .map_err(|e| {
            match e {
                ureq::Error::StatusCode(code) => {
                    eyre::eyre!(
                        "HTTP {} error when fetching release info from {}",
                        code,
                        url
                    )
                }
                ureq::Error::Io(io_err) => {
                    eyre::eyre!(
                        "Network I/O error when fetching release info from {}: {}",
                        url,
                        io_err
                    )
                }
                _ => {
                    eyre::eyre!(
                        "General error when fetching release info from {}: {}",
                        url,
                        e
                    )
                }
            }
        })?;

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

    // Log response body for debugging (first 500 chars)
    let debug_body = if body.len() > 500 {
        format!("{}...", &body[..500])
    } else {
        body.clone()
    };

    log::debug!("Response body: {}", debug_body);

    let release: GiteeRelease = serde_json::from_str(&body)
        .with_context(|| format!(
            "Failed to parse release information from response body: {}",
            debug_body
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

/// Get version from epkg binary
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

    // Try to find elf-loader in common locations
    let env_root = find_env_root(SELF_ENV);
    let possible_elf_loader_paths = [
        env_root.as_ref().map(|root| root.join("usr/bin/elf-loader")).unwrap_or_else(|| PathBuf::new()),
        dirs().epkg_downloads_cache.join(format!("epkg/elf-loader-{}", &config().common.arch)),
        PathBuf::from("./elf-loader"),
    ];

    let elf_loader_version = possible_elf_loader_paths
        .iter()
        .find_map(|path| get_elf_loader_version(path).ok())
        .unwrap_or_else(|| "unknown".to_string());

    Ok(EpkgVersionInfo {
        epkg_version,
        elf_loader_version,
    })
}

/// Check for updates and return initialization plan
fn check_for_updates() -> Result<InitPlan> {
    println!("Checking for updates...");

    let current_version = get_current_epkg_version_info()?;

    // Determine if this is an upgrade or fresh install
    let is_upgrade = config().init.upgrade;
    let arch = &config().common.arch;
    let dirs = dirs();
    let epkg_download_dir = dirs.epkg_downloads_cache.join("epkg");

    // Check for local repo and local elf-loader BEFORE making API calls
    let repo_root = find_repo_root()?;
    let using_local_repo = is_valid_local_repo(&repo_root);
    let local_elf_loader_path = repo_root.join("elf-loader/src/loader");
    let has_local_elf_loader = local_elf_loader_path.exists();

    // If both local repo and local elf-loader are detected, skip API calls
    let new_version = if using_local_repo && has_local_elf_loader {
        // Use current version as new version when using local development binaries
        current_version.clone()
    } else {
        // Fetch latest epkg version
        let epkg_release = fetch_latest_release("openeuler", "epkg")
            .context("Failed to fetch epkg release info")?;

        // Fetch latest elf-loader version
        let elf_loader_release = fetch_latest_release("openeuler", "elf-loader")
            .context("Failed to fetch elf-loader release info")?;

        EpkgVersionInfo {
            epkg_version: epkg_release.tag_name.clone(),
            elf_loader_version: elf_loader_release.tag_name.clone(),
        }
    };

    // Always show version information
    println!("  epkg: {} → {}", current_version.epkg_version, new_version.epkg_version);
    println!("  elf-loader: {} → {}", current_version.elf_loader_version, new_version.elf_loader_version);

    // Set up URLs first (needed for path resolution)
    let (epkg_binary_url, elf_loader_url) = get_versioned_urls(&new_version.epkg_version, &new_version.elf_loader_version, arch);
    let epkg_binary_sha_url = format!("{}.sha256", epkg_binary_url);
    let epkg_src_url = format!("https://gitee.com/openeuler/epkg/repository/archive/{}.tar.gz", new_version.epkg_version);
    let elf_loader_sha_url = format!("{}.sha256", elf_loader_url);

    // Set up file paths using the same resolution logic as the download system
    // This ensures paths match where files are actually downloaded
    let epkg_binary_path      = mirror::Mirrors::remote_url_to_path(&epkg_binary_url,       &epkg_download_dir, "epkg")?;
    let epkg_binary_sha_path  = mirror::Mirrors::remote_url_to_path(&epkg_binary_sha_url,   &epkg_download_dir, "epkg")?;
    let epkg_src_path         = mirror::Mirrors::remote_url_to_path(&epkg_src_url,          &epkg_download_dir, "epkg")?;
    let elf_loader_path       = mirror::Mirrors::remote_url_to_path(&elf_loader_url,        &epkg_download_dir, "epkg")?;
    let elf_loader_sha_path   = mirror::Mirrors::remote_url_to_path(&elf_loader_sha_url,    &epkg_download_dir, "epkg")?;

    // Determine what needs to be downloaded
    let need_download_epkg_binary = is_upgrade;
    let need_download_epkg_src = !using_local_repo;
    let need_download_elf_loader = !has_local_elf_loader;

    Ok(InitPlan {
        current: current_version,
        new: new_version,
        epkg_binary_url,
        epkg_binary_sha_url,
        epkg_src_url,
        elf_loader_url,
        elf_loader_sha_url,
        epkg_binary_path,
        epkg_binary_sha_path,
        epkg_src_path,
        elf_loader_path,
        elf_loader_sha_path,
        need_download_epkg_binary,
        need_download_epkg_src,
        need_download_elf_loader,
        using_local_repo,
        local_elf_loader_path: if has_local_elf_loader { Some(local_elf_loader_path) } else { None },
    })
}

/// Generate versioned download URLs
fn get_versioned_urls(epkg_version: &str, elf_loader_version: &str, arch: &str) -> (String, String) {
    let epkg_url = format!("https://gitee.com/openeuler/epkg/releases/download/{}/epkg-{}", epkg_version, arch);
    let elf_loader_url = format!("https://gitee.com/openeuler/elf-loader/releases/download/{}/elf-loader-{}", elf_loader_version, arch);

    (epkg_url, elf_loader_url)
}

/// Fix up /lib64 symlink in the host OS.
/// - If /lib64 already exists as a symlink to usr/lib64: fine and return
/// - If /lib64 already exists as a symlink to usr/lib (archlinux host): remove it or warn 'rpm/deb guest os may not work'
/// - If /lib64 not exists (alpine host): create symlink to usr/lib64 or warn 'guest os other than alpine/archlinux/conda may not work'
/// Only works when running as root.
fn fixup_host_lib64_symlink() -> Result<()> {
    let lib64_path = Path::new("/lib64");
    let usr_lib64_target = Path::new("usr/lib64");

    // Check if /lib64 already exists as a symlink
    if let Ok(metadata) = fs::symlink_metadata(lib64_path) {
        if metadata.file_type().is_symlink() {
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
                        fs::remove_file(lib64_path)
                            .wrap_err_with(|| format!("Failed to remove existing /lib64 symlink"))?;
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
        } else {
            // /lib64 exists but is not a symlink (directory or file)
            return Err(eyre::eyre!("/lib64 exists but is not a symlink, cannot fix"));
        }
    }

    // /lib64 doesn't exist (or was just removed), need to create it
    if !utils::is_running_as_root() {
        eprintln!("WARNING: /lib64 -> usr/lib64 symlink does not exist and cannot be created (not running as root). Guest OS other than Alpine/ArchLinux/Conda may not work.");
        return Err(eyre::eyre!("Cannot create /lib64 symlink: not running as root"));
    }

    // Create the symlink using relative path
    symlink(usr_lib64_target, lib64_path)
        .wrap_err_with(|| format!("Failed to create /lib64 -> usr/lib64 symlink"))?;

    Ok(())
}
