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

    // After root run `epkg init --store=shared`, /usr/local/bin/epkg will be created and exposed
    // to normal users. Then everyone can run "epkg install". To make it user friendly, here we'll
    // auto trigger light_init() seemlessly at first invocation.
    pub fn try_light_init(&mut self) -> Result<()> {
        if matches!(config().subcommand,
              EpkgCommand::Unpack
            | EpkgCommand::Convert
            | EpkgCommand::Hash
            | EpkgCommand::Repo
            | EpkgCommand::Init
            | EpkgCommand::Deinit
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

    fn light_init(&mut self) -> Result<()> {
        // Create necessary directories
        fs::create_dir_all(&dirs().home_config.join("path.d"))
            .context("Failed to create path.d directory in home config")?;

        // Create main environment
        self.create_environment(MAIN_ENV)?;

        // Load the environment config that was just created and register it
        let env_config = crate::io::deserialize_env_config_for(MAIN_ENV.to_string())?;
        self.register_environment_for(MAIN_ENV, &env_config)?;

        // Update shell configuration
        self.update_shell_rc()?;

        println!("Notice: for changes to take effect, close and re-open your current shell.");
        Ok(())
    }

    pub fn command_init(&mut self) -> Result<()> {
        if config().init.upgrade {
            self.upgrade_epkg()?;
            return Ok(());
        }

        if find_env_root(BASE_ENV).is_none() {
            self.install_epkg()?;
        }

        // Check if already initialized
        if find_env_root(MAIN_ENV).is_some() {
            eprintln!("epkg was already initialized for current user");
            return Ok(());
        }

        self.light_init()?;
        Ok(())
    }

    pub fn upgrade_epkg(&mut self) -> Result<()> {
        // Check if base environment exists
        if find_env_root(BASE_ENV).is_none() {
            eprintln!("epkg is not installed. Please run 'epkg init' first.");
            return Ok(());
        }

        println!("Checking for updates...");

        // Check for available updates
        match check_for_updates() {
            Ok(Some(new_version)) => {
                println!("New versions available:");
                println!("  epkg: {}", new_version.epkg_version);
                println!("  elf-loader: {}", new_version.elf_loader_version);

                println!("Upgrading epkg installation...");
                self.download_setup_files()?;
                println!("epkg upgrade completed successfully.");
            }
            Ok(None) => {
                println!("epkg is already up to date.");
            }
            Err(e) => {
                eprintln!("Warning: Failed to check for updates: {}", e);
            }
        }

        Ok(())
    }

    pub fn install_epkg(&mut self) -> Result<()> {
        // Set up installation paths
        fs::create_dir_all(&dirs().epkg_downloads_cache.join("epkg"))
            .context("Failed to create epkg downloads directory")?;

        print_banner();

        // Pre-populate country cache in background thread to speed up later invocations
        pre_populate_country_cache();

        self.download_setup_files()?;

        self.create_environment(BASE_ENV)?;

        Ok(())
    }

    fn download_required_files(&self, _env_root: &Path) -> Result<()> {
        let arch = &config().common.arch;
        let dirs = dirs();

        // Determine versions to use
        let epkg_version = if config().init.upgrade {
            // For upgrades, try to get current version first, then fetch latest if needed
            get_epkg_version().unwrap_or_else(|_| {
                fetch_latest_release("openeuler", "epkg")
                    .map(|release| release.tag_name)
                    .unwrap_or_else(|_| env!("EPKG_VERSION_TAG").to_string())
            })
        } else {
            // For fresh installs, use the build-time version
            env!("EPKG_VERSION_TAG").to_string()
        };

        let elf_loader_version = {
            let repo_root = find_repo_root()?;
            let local_loader = repo_root.join("elf-loader/src/loader");

            if local_loader.exists() {
                // Use local elf-loader version
                get_elf_loader_version(&local_loader)
                    .unwrap_or_else(|_| "unknown".to_string())
            } else {
                // Fetch latest elf-loader version
                fetch_latest_release("openeuler", "elf-loader")
                    .map(|release| release.tag_name)
                    .unwrap_or_else(|_| "unknown".to_string())
            }
        };

        // Set up versioned URLs
        let (epkg_binary_url, elf_loader_url) = get_versioned_urls(&epkg_version, &elf_loader_version, arch);
        let epkg_src_url = format!("https://gitee.com/openeuler/epkg/repository/archive/{}.tar.gz", epkg_version);

        let elf_loader = "elf-loader";
        let epkg_static = "epkg";
        let epkg_download_dir = dirs.epkg_downloads_cache.join("epkg");
        let epkg_src_tar = epkg_download_dir.join(format!("{}.tar.gz", epkg_version));
        let elf_loader_path = epkg_download_dir.join(format!("{}-{}", elf_loader, arch));
        let elf_loader_sha = epkg_download_dir.join(format!("{}-{}.sha256", elf_loader, arch));
        let epkg_binary_sha = epkg_download_dir.join(format!("{}-{}.sha256", epkg_static, arch));

        let mut need_download_epkg_src: bool = false;
        let mut need_download_epkg_binary: bool = false;

        // Collect urls for downloading in parallel
        let mut urls = Vec::new();

        let repo_root = find_repo_root()?;

        // Handle epkg source code (local repo or download)
        let using_local_repo = is_valid_local_repo(&repo_root);
        if !using_local_repo {
            println!("Downloading epkg source code from {}", epkg_src_url);
            urls.push(epkg_src_url.clone());
            need_download_epkg_src = true;
        }

        // Download epkg binary if upgrading
        if config().init.upgrade {
            println!("Downloading epkg binary from {}", epkg_binary_url);
            urls.extend(vec![
                epkg_binary_url.clone(),
                format!("{}.sha256", epkg_binary_url)
            ]);
            need_download_epkg_binary = true;
        }

        // Check for local elf-loader
        let local_loader = repo_root.join("elf-loader/src/loader");

        if local_loader.exists() {
            fs::copy(&local_loader, &elf_loader_path)
                .context(format!("Failed to copy local elf-loader from {} to {}",
                    local_loader.display(), elf_loader_path.display()))?;
            println!("Using local elf-loader from {}", local_loader.display());
        } else {
            println!("Downloading elf-loader from {}", elf_loader_url);
            urls.extend(vec![
                elf_loader_url.clone(),
                format!("{}.sha256", elf_loader_url)
            ]);
        }

        if urls.is_empty() {
            return Ok(());
        }

        // Download to the new epkg subdirectory within downloads cache
        let epkg_download_dir = dirs.epkg_downloads_cache.join("epkg");
        download_urls(urls, &epkg_download_dir, 6, false)
            .context("Failed to download required files")?;

        // Verify checksums
        if !local_loader.exists() {
            utils::verify_sha256sum(&elf_loader_sha)
                .context("Failed to verify elf-loader checksum")?;
        }

        if need_download_epkg_binary {
            utils::verify_sha256sum(&epkg_binary_sha)
                .context("Failed to verify epkg binary checksum")?;
        }

        if need_download_epkg_src && !epkg_src_tar.exists() {
            return Err(eyre::eyre!("Failed to download epkg source code tar file from {}", epkg_src_url));
        }

        Ok(())
    }

    fn download_setup_files(&mut self) -> Result<()> {
        let base_env_root = self.new_env_base(BASE_ENV);

        self.download_required_files(&base_env_root)
            .context("Failed to download required files for base environment")?;

        self.setup_epkg_src(&base_env_root)?;
        self.setup_common_binaries(&base_env_root)?;

        Ok(())
    }

    fn setup_epkg_src(&self, env_root: &Path) -> Result<()> {
        let epkg_version = if config().init.upgrade {
            get_epkg_version().unwrap_or_else(|_| env!("EPKG_VERSION_TAG").to_string())
        } else {
            env!("EPKG_VERSION_TAG").to_string()
        };
        let repo_root = find_repo_root()?;
        let usr_src = env_root.join("usr/src");
        let epkg_src = usr_src.join("epkg");

        // Check if we're using a local repository
        if is_valid_local_repo(&repo_root) {
            // Create symlink directly to git working directory
            if !usr_src.exists() {
                fs::create_dir_all(&usr_src)
                    .context("Failed to create usr/src directory in environment")?;
            }

            if !epkg_src.exists() {
                symlink(repo_root.to_str().unwrap(), &epkg_src)
                    .context("Failed to create symlink to local repository")?;
            }

            println!("Using local git repository for epkg source code");
            return Ok(());
        }

        // Extract epkg source code tar for remote repository
        let epkg_extracted_dir = format!("epkg-{}", epkg_version);
        let epkg_extracted_path = usr_src.join(&epkg_extracted_dir);
        let epkg_src_tar = dirs().epkg_downloads_cache.join("epkg").join(format!("{}.tar.gz", epkg_version));

        println!("Extracting epkg source code to: {}", usr_src.display());

        if epkg_extracted_path.exists() {
            fs::remove_dir_all(&epkg_extracted_path)?;
        } else {
            fs::create_dir_all(&usr_src)
                .context(format!("Failed to create opt directory at {}", usr_src.display()))?;
        }

        // Extract tar.gz file with error handling
        utils::extract_tar_gz(&epkg_src_tar, &usr_src)
            .context("Failed to extract epkg source code tar file")?;

        // Create a symlink from epkg to epkg-master (or epkg-$version)
        if let Err(e) = utils::force_symlink(&epkg_extracted_dir, &epkg_src) {
            eprintln!("[WARN] Failed to create symlink {} -> {}: {}",
                     epkg_src.display(), epkg_extracted_dir, e);
        }

        Ok(())
    }

    fn setup_common_binaries(&self, env_root: &Path) -> Result<()> {
        let arch = env::consts::ARCH;
        let usr_bin = env_root.join("usr/bin");

        fs::create_dir_all(&usr_bin)
            .context(format!("Failed to create usr/bin directory at {}", usr_bin.display()))?;

        let target_epkg = usr_bin.join("epkg");

        // Determine epkg binary source based on whether we're upgrading or installing
        let epkg_source = if config().init.upgrade {
            // Use downloaded epkg binary for upgrades
            let epkg_binary_path = dirs().epkg_downloads_cache.join("epkg").join(format!("epkg-{}", arch));
            if !epkg_binary_path.exists() {
                return Err(eyre::eyre!("Downloaded epkg binary not found at {}", epkg_binary_path.display()));
            }
            epkg_binary_path
        } else {
            // Use current executable for normal installs
            std::env::current_exe()
                .context("Failed to get current executable path")?
        };

        // Copy epkg binary using atomic operation
        self.copy_epkg_binary_atomically(&epkg_source, &target_epkg, true)?;

        // Copy elf-loader binary using atomic operation
        let elf_loader_source = dirs().epkg_downloads_cache.join("epkg").join(format!("elf-loader-{}", arch));
        let elf_loader_target = usr_bin.join("elf-loader");
        self.copy_epkg_binary_atomically(&elf_loader_source, &elf_loader_target, false)?;

        // Create symlink to epkg binary in the first valid PATH component
        self.create_epkg_symlink(env_root, &target_epkg)
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
    fn create_epkg_symlink(&self, env_root: &Path, epkg_binary_path: &Path) -> Result<()> {
        if config().init.upgrade {
            return Ok(());
        }

        let main_ebin = env_root.join("main/usr/ebin");

        fs::create_dir_all(&main_ebin)
            .context(format!("Failed to create usr/ebin directory at {}", main_ebin.display()))?;

        println!("Creating symlink: {}/epkg -> {}", main_ebin.display(), epkg_binary_path.display());
        utils::force_symlink(epkg_binary_path, &main_ebin.join("epkg"))
            .context(format!("Failed to create symlink from {} to {}",
                epkg_binary_path.display(), main_ebin.join("epkg").display()))?;

        // Try to create symlink in $HOME/bin if it's in PATH
        let home = crate::dirs::get_home().wrap_err("Failed to get HOME directory")?;
        let home_bin = PathBuf::from(&home).join("bin");
        let path_var = env::var("PATH")
            .unwrap_or_else(|_| "".to_string());

        if path_var.contains(home_bin.to_string_lossy().as_ref()) {
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
            if usr_local_bin.exists() {
                println!("Creating symlink: {}/epkg -> {}", usr_local_bin.display(), epkg_binary_path.display());
                if let Err(e) = utils::force_symlink(epkg_binary_path, &usr_local_bin.join("epkg")) {
                    log::warn!("Failed to create epkg symlink in {}: {}", usr_local_bin.display(), e);
                }
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

        let base_env_root = get_env_root(BASE_ENV.to_string())?;

        for shell_rc_info in shell_rc_infos {
            let rc_content = format!(r#"
# epkg begin
epkg_rc='{base_path}/usr/src/epkg/lib/{script_name}'
test -r "$epkg_rc" && . "$epkg_rc"
# epkg end
"#,
                base_path = base_env_root.display(),
                script_name = shell_rc_info.source_script_name
            );

            // Read existing content
            let existing_content = match fs::read_to_string(&shell_rc_info.rc_file_path) {
                Ok(content) => content,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // If the rc file doesn't exist, it will be created by OpenOptions
                    String::new()
                }
                Err(e) => {
                    return Err(eyre::eyre!("Failed to read shell rc file {}: {}", shell_rc_info.rc_file_path, e));
                }
            };

            // Only append if epkg begin line doesn't exist
            if !existing_content.contains("# epkg begin") {
                println!("Updating shell RC file: {}", shell_rc_info.rc_file_path);

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
            } else {
                println!("epkg configuration already present in {}. Skipping.", shell_rc_info.rc_file_path);
            }
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
    published_at: String,
    assets: Vec<GiteeAsset>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GiteeAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct VersionInfo {
    epkg_version: String,
    elf_loader_version: String,
}

/// Fetch the latest release information from Gitee API
fn fetch_latest_release(owner: &str, repo: &str) -> Result<GiteeRelease> {
    let url = format!("https://gitee.com/api/v5/repos/{}/{}/releases/latest", owner, repo);

    let mut response = ureq::get(&url)
        .call()
        .context("Failed to fetch release information from Gitee")?;

    let body = response.body_mut().read_to_string()
        .context("Failed to read response body")?;
    let release: GiteeRelease = serde_json::from_str(&body)
        .context("Failed to parse release information")?;

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
fn get_current_version_info() -> Result<VersionInfo> {
    let epkg_version = get_epkg_version().unwrap_or_else(|_| env!("EPKG_VERSION_TAG").to_string());

    // Try to find elf-loader in common locations
    let possible_elf_loader_paths = [
        find_env_root(BASE_ENV).unwrap_or_else(|| PathBuf::new()).join("usr/bin/elf-loader"),
        dirs().epkg_downloads_cache.join(format!("epkg/elf-loader-{}", &config().common.arch)),
        PathBuf::from("./elf-loader"),
    ];

    let elf_loader_version = possible_elf_loader_paths
        .iter()
        .find_map(|path| get_elf_loader_version(path).ok())
        .unwrap_or_else(|| "unknown".to_string());

    Ok(VersionInfo {
        epkg_version,
        elf_loader_version,
    })
}

/// Check for updates and return new version information if available
fn check_for_updates() -> Result<Option<VersionInfo>> {
    let current_version = get_current_version_info()?;

    // Fetch latest epkg version
    let epkg_release = fetch_latest_release("openeuler", "epkg")
        .context("Failed to fetch epkg release info")?;

    // Fetch latest elf-loader version
    let elf_loader_release = fetch_latest_release("openeuler", "elf-loader")
        .context("Failed to fetch elf-loader release info")?;

    let new_version = VersionInfo {
        epkg_version: epkg_release.tag_name,
        elf_loader_version: elf_loader_release.tag_name,
    };

    // Check if we have newer versions
    if new_version.epkg_version != current_version.epkg_version ||
        new_version.elf_loader_version != current_version.elf_loader_version {
        Ok(Some(new_version))
    } else {
        Ok(None)
    }
}

/// Generate versioned download URLs
fn get_versioned_urls(epkg_version: &str, elf_loader_version: &str, arch: &str) -> (String, String) {
    let epkg_url = format!("https://gitee.com/openeuler/epkg/releases/download/{}/epkg-{}", epkg_version, arch);
    let elf_loader_url = format!("https://gitee.com/openeuler/elf-loader/releases/download/{}/elf-loader-{}", elf_loader_version, arch);

    (epkg_url, elf_loader_url)
}
