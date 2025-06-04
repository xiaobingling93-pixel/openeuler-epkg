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
use crate::dirs::find_env_root;
use std::fs::OpenOptions;
use std::io::Write as IoWrite;

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

        println!("Notice: For changes to take effect, close and re-open your current shell.");
        Ok(())
    }

    pub fn install_epkg(&mut self) -> Result<()> {
        // Set up installation paths
        fs::create_dir_all(&dirs().epkg_cache)
            .context("Failed to create cache directory")?;
        fs::create_dir_all(&dirs().epkg_downloads_cache)
            .context("Failed to create downloads directory")?;
        fs::create_dir_all(&dirs().epkg_channel_cache)
            .context("Failed to create channel directory")?;

        // Set up common environment
        self.setup_common_environment()?;

        // Update shell configuration
        self.update_shell_rc()?;

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
        let epkg_manager_tar = dirs.epkg_cache.join(format!("{}.tar.gz", epkg_version));
        let elf_loader_path = dirs.epkg_cache.join(format!("{}-{}", elf_loader, arch));
        let elf_loader_sha = dirs.epkg_cache.join(format!("{}-{}.sha256", elf_loader, arch));

        // Check if running from git repo
        let current_exe = std::env::current_exe()
            .context("Failed to get current executable path")?;
        let repo_root = current_exe.parent().unwrap().parent().unwrap().parent().unwrap();
        let git_dir = repo_root.join(".git");
        let mut need_download_epkg_manager: bool = false;

        // Collect urls for downloading in parallel
        let mut urls = Vec::new();

        if git_dir.exists() {
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
        let local_loader = repo_root.parent().unwrap()
            .join("elf-loader/src/loader");

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

        download_urls(urls, &dirs.epkg_cache, 6, false)
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
        let epkg_manager_tar = dirs().epkg_cache.join(format!("{}.tar.gz", epkg_version));

        if epkg_manager_dir.exists() {
            return Ok(());
        }

        println!("Extracting epkg manager from {}", epkg_manager_tar.display());

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
            eprintln!("Warning: Failed to create symlink from epkg-manager to {}: {}",
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
            &dirs().epkg_cache.join(format!("elf-loader-{}", arch)),
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

        Ok(())
    }

    fn update_shell_rc(&mut self) -> Result<()> {
        let shell_rc_infos = crate::dirs::get_shell_rc()?;

        if shell_rc_infos.is_empty() {
            // No specific shell found via SHELL var, and no common rc files detected.
            // A warning would have been printed by get_shell_rc in this case.
            return Ok(());
        }

        let common_env_root = self.get_env_root("common".to_string())?;

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

                println!("Updated shell configuration file: {}", shell_rc_info.rc_file_path);
            } else {
                println!("epkg configuration already present in {}. Skipping.", shell_rc_info.rc_file_path);
            }
        }

        Ok(())
    }
}
