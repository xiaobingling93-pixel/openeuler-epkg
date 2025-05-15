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
            eprintln!("epkg was already initialized for user {}", env::var("USER")?);
            return Ok(());
        }

        // Create necessary directories
        fs::create_dir_all(&dirs().home_config.join("path.d"))?;

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
        fs::create_dir_all(&dirs().epkg_pkg_cache)
            .context("Failed to create package cache directory")?;
        fs::create_dir_all(&dirs().epkg_channel_cache)
            .context("Failed to create channel cache directory")?;

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
        let current_exe = std::env::current_exe()?;
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
                fs::create_dir_all(env_opt)?;
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
            fs::copy(&local_loader, &elf_loader_path)?;
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

        download_urls(urls, dirs.epkg_cache.to_str().unwrap(), 6, false)
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

        self.download_required_files(&common_env_root)?;

        self.setup_epkg_manager(&common_env_root)?;
        self.setup_common_binaries(&common_env_root)?;

        self.create_environment("common")?;

        Ok(())
    }

    // input tar file:
    // wfg /tmp% wget http://gitee.com/openeuler/epkg/repository/archive/master.tar.gz
    // wfg /tmp% less master.tar.gz|head
    // drwxrwxr-x root/root         0 2025-04-29 10:56 epkg-master/
    // -rw-rw-r-- root/root        22 2025-04-29 10:56 epkg-master/.gitignore
    // -rw-rw-r-- root/root     45157 2025-04-29 10:56 epkg-master/Cargo.lock
    // -rw-rw-r-- root/root      1184 2025-04-29 10:56 epkg-master/Cargo.toml
    // -rw-rw-r-- root/root      4163 2025-04-29 10:56 epkg-master/Makefile
    // drwxrwxr-x root/root         0 2025-04-29 10:56 epkg-master/bin/
    // -rw-rw-r-- root/root      7609 2025-04-29 10:56 epkg-master/bin/epkg-installer.sh
    // -rw-rw-r-- root/root      2196 2025-04-29 10:56 epkg-master/bin/epkg-uninstaller.sh
    // drwxrwxr-x root/root         0 2025-04-29 10:56 epkg-master/build/
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
        fs::create_dir_all(&env_opt)?;

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

        fs::create_dir_all(&usr_bin)?;

        // Copy binaries (special handling for common environment)
        fs::copy(
            std::env::current_exe()?,
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
        fs::set_permissions(&usr_bin.join("epkg"), fs::Permissions::from_mode(mode))?;
        fs::set_permissions(&usr_bin.join("elf-loader"), fs::Permissions::from_mode(0o755))?;

        Ok(())
    }

    fn update_shell_rc(&mut self) -> Result<()> {
        let shell = env::var("SHELL")?;
        let shell = Path::new(&shell)
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| eyre::eyre!("Invalid shell path"))?;

        let rc_path = match shell {
            "bash" => env::var("HOME")? + "/.bashrc",
            "zsh" => env::var("HOME")? + "/.zshrc",
            _ => return Err(eyre::eyre!("Unsupported shell: {}", shell)),
        };

        let common_env_root = self.get_env_root("common".to_string())?;
        let rc_content = format!(
            "\n# epkg begin\nsource {}/opt/epkg-manager/lib/epkg-rc.sh\n# epkg end\n",
            common_env_root.display()
        );

        // Read existing content
        let existing_content = fs::read_to_string(&rc_path)
            .unwrap_or_default();

        // Only append if epkg begin line doesn't exist
        if !existing_content.contains("# epkg begin") {
            let full_content = existing_content + &rc_content;
            fs::write(&rc_path, full_content)
                .context("Failed to update shell rc file")?;
        }

        Ok(())
    }
}
