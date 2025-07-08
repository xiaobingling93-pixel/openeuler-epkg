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

fn print_banner() {
    println!(r#"         ____  _  ______   "#);
    println!(r#"   ____ |  _ \| |/ / ___|  "#);
    println!(r#"  ( ___)| |_) | ' / |  _   "#);
    println!(r#"   )__) |  __/| . \ |_| |  "#);
    println!(r#"  (____)|_|   |_|\_\____|  "#);
}

impl PackageManager {

    #[allow(dead_code)]
    pub fn check_init(&mut self) -> Result<()> {
        if find_env_root("main").is_none() {
            self.init()?;
        }

        Ok(())
    }

    pub fn init(&mut self) -> Result<()> {
        if find_env_root("common").is_none() {
            self.install_epkg()?;
        }

        // Check if already initialized
        if find_env_root("main").is_some() {
            eprintln!("epkg was already initialized for current user");
            return Ok(());
        }

        // Create necessary directories
        fs::create_dir_all(&dirs().home_config.join("path.d"))
            .context("Failed to create path.d directory in home config")?;

        // Create main environment
        self.create_environment("main")?;
        self.register_environment("main")?;

        // Update shell configuration
        self.update_shell_rc()?;

        println!("Notice: for changes to take effect, close and re-open your current shell.");
        Ok(())
    }

    pub fn install_epkg(&mut self) -> Result<()> {
        // Set up installation paths
        fs::create_dir_all(&dirs().epkg_downloads_cache.join("epkg"))
            .context("Failed to create epkg downloads directory")?;

        print_banner();

        // Set up common environment
        self.setup_common_environment()?;

        Ok(())
    }

    fn download_required_files(&self, env_root: &Path) -> Result<()> {
        let arch = &config().common.arch;
        let epkg_version = &config().init.version;
        let dirs = dirs();

        // Set up URLs and paths
        let epkg_url = "https://repo.oepkgs.net/openeuler/epkg/rootfs/";
        let epkg_manager_url = format!("https://gitee.com/openeuler/epkg/repository/archive/{}.tar.gz", epkg_version);
        let elf_loader = "elf-loader";
        let epkg_download_dir = dirs.epkg_downloads_cache.join("epkg");
        let epkg_manager_tar = epkg_download_dir.join(format!("{}.tar.gz", epkg_version));
        let elf_loader_path = epkg_download_dir.join(format!("{}-{}", elf_loader, arch));
        let elf_loader_sha = epkg_download_dir.join(format!("{}-{}.sha256", elf_loader, arch));

        let mut need_download_epkg_manager: bool = false;

        // Collect urls for downloading in parallel
        let mut urls = Vec::new();

        let repo_root = find_repo_root()?;
        if is_valid_local_repo(&repo_root) {
            // Create symlink directly to git working directory
            let env_opt = env_root.join("opt");
            let epkg_manager_dir = env_opt.join("epkg-manager");
            if !env_opt.exists() {
                fs::create_dir_all(env_opt)
                    .context("Failed to create opt directory in environment")?;
                symlink(repo_root.to_str().unwrap(), &epkg_manager_dir)?;
            }
            println!("Using local git repository for epkg manager");
        } else {
            println!("Downloading epkg manager from {}", epkg_manager_url);
            urls.push(epkg_manager_url.clone());
            need_download_epkg_manager = true;
        }

        // Check for local elf-loader
        let local_loader = match repo_root.parent() {
            Some(parent) => parent.join("elf-loader/src/loader"),
            None => repo_root.join("elf-loader/src/loader"),
        };

        if local_loader.exists() {
            fs::copy(&local_loader, &elf_loader_path)
                .context(format!("Failed to copy local elf-loader from {} to {}",
                    local_loader.display(), elf_loader_path.display()))?;
            println!("Using local elf-loader from {}", local_loader.display());
        } else {
            println!("Downloading elf-loader from {}", epkg_url);
            urls.extend(vec![
                format!("{}{}-{}",        epkg_url, elf_loader, arch),
                format!("{}{}-{}.sha256", epkg_url, elf_loader, arch)
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

        if need_download_epkg_manager && !epkg_manager_tar.exists() {
            return Err(eyre::eyre!("Failed to download epkg manager tar file from {}", epkg_manager_url));
        }

        Ok(())
    }

    fn setup_common_environment(&mut self) -> Result<()> {
        let common_env_root = self.new_env_base("common");

        self.download_required_files(&common_env_root)
            .context("Failed to download required files for common environment")?;

        self.setup_epkg_manager(&common_env_root)?;
        self.setup_common_binaries(&common_env_root)?;

        self.create_environment("common")?;

        Ok(())
    }

    fn setup_epkg_manager(&self, env_root: &Path) -> Result<()> {
        let epkg_version = &config().init.version;

        // Extract epkg-manager tar
        let env_opt = env_root.join("opt");
        let epkg_manager_dir = env_opt.join("epkg-manager");
        let epkg_extracted_dir = format!("epkg-{}", epkg_version);
        let epkg_manager_tar = dirs().epkg_downloads_cache.join("epkg").join(format!("{}.tar.gz", epkg_version));

        if epkg_manager_dir.exists() {
            return Ok(());
        }

        println!("Extracting epkg manager to: {}", env_opt.display());

        // Create opt directory if it doesn't exist
        fs::create_dir_all(&env_opt)
            .context(format!("Failed to create opt directory at {}", env_opt.display()))?;

        // Extract tar.gz file with error handling
        utils::extract_tar_gz(&epkg_manager_tar, &env_opt)
            .context("Failed to extract epkg manager tar file")?;

        // Create a symlink from epkg-manager to epkg-master (or epkg-$version)
        if epkg_manager_dir.exists() {
            fs::remove_file(&epkg_manager_dir).ok();
        }

        if let Err(e) = symlink(&epkg_extracted_dir, &epkg_manager_dir) {
            eprintln!("[WARN] Failed to create symlink from epkg-manager to {}: {}",
                     epkg_extracted_dir, e);
        }

        Ok(())
    }

    fn setup_common_binaries(&self, env_root: &Path) -> Result<()> {
        let arch = env::consts::ARCH;
        let usr_bin = env_root.join("usr/bin");

        fs::create_dir_all(&usr_bin)
            .context(format!("Failed to create usr/bin directory at {}", usr_bin.display()))?;

        // Copy binaries (special handling for common environment)
        fs::copy(
            std::env::current_exe()
                .context("Failed to get current executable path")?,
            &usr_bin.join("epkg")
        ).context("Failed to copy epkg binary")?;

        fs::copy(
            &dirs().epkg_downloads_cache.join("epkg").join(format!("elf-loader-{}", arch)),
            &usr_bin.join("elf-loader")
        ).context("Failed to copy elf-loader binary")?;

        // Set permissions based on installation mode
        let mode = if config().init.shared_store {
            0o4755 // setuid + rwxr-xr-x
        } else {
            0o755 // rwxr-xr-x
        };
        // Set permissions on epkg binary - uses setuid (4755) for shared store mode or standard (755) for single-user mode
        log::debug!("Setting epkg binary permissions to mode {:o}", mode);
        fs::set_permissions(&usr_bin.join("epkg"), fs::Permissions::from_mode(mode))
            .context(format!("Failed to set permissions (mode {:o}) on epkg binary at {}", mode, usr_bin.join("epkg").display()))?;

        // Set standard executable permissions (755) on elf-loader binary
        log::debug!("Setting elf-loader binary permissions to mode 755");
        fs::set_permissions(&usr_bin.join("elf-loader"), fs::Permissions::from_mode(0o755))
            .context(format!("Failed to set permissions (mode 755) on elf-loader binary at {}", usr_bin.join("elf-loader").display()))?;

        // Create symlink to epkg binary in the first valid PATH component
        self.create_epkg_symlink(env_root, &usr_bin.join("epkg"))
            .context("Failed to create epkg symlink in PATH")?;

        Ok(())
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

        let common_env_root = get_env_root("common".to_string())?;

        for shell_rc_info in shell_rc_infos {
            let rc_content = format!(
                "\n# epkg begin\nsource {}/opt/epkg-manager/lib/{}\n# epkg end\n",
                common_env_root.display(),
                shell_rc_info.source_script_name
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
