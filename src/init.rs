use std::fs;
use std::env;
use std::path::Path;
use std::os::unix::fs::PermissionsExt;
use anyhow::{Result, Context};
use crate::models::*;
use crate::download::download_urls;
use crate::utils;

impl PackageManager {

    #[allow(dead_code)]
    pub fn check_init(&mut self) -> Result<()> {
        if !self.get_env_root("main".to_string())?.exists() {
            self.init()?;
        }

        Ok(())
    }

    pub fn init(&mut self) -> Result<()> {
        if !self.get_env_root("common".to_string())?.exists() {
            self.install_epkg()?;
        }

        // Check if already initialized
        if self.get_env_root("main".to_string())?.exists() {
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
        // Validate architecture
        self.validate_architecture()?;

        // Set up installation paths
        fs::create_dir_all(&dirs().epkg_cache)
            .context("Failed to create cache directory")?;
        fs::create_dir_all(&dirs().epkg_pkg_cache)
            .context("Failed to create package cache directory")?;
        fs::create_dir_all(&dirs().epkg_channel_cache)
            .context("Failed to create channel cache directory")?;

        // Download required files
        self.download_required_files()?;

        // Set up common environment
        self.setup_common_environment()?;

        // Update shell configuration
        self.update_shell_rc()?;

        Ok(())
    }

    fn validate_architecture(&self) -> Result<()> {
        let arch = env::consts::ARCH;
        match arch {
            "x86_64" | "aarch64" | "riscv64" | "loongarch64" => Ok(()),
            _ => Err(anyhow::anyhow!("Unsupported architecture: {}", arch))
        }
    }

    fn download_required_files(&self) -> Result<()> {
        let arch = env::consts::ARCH;

        // Set up URLs
        let epkg_url = "https://repo.oepkgs.net/openeuler/epkg/rootfs/";
        let epkg_version = &config().init.version;
        let epkg_manager_url = format!("https://gitee.com/openeuler/epkg/repository/archive/{}.tar.gz", epkg_version);
        let elf_loader = "elf-loader";

        println!("Downloading epkg manager from \n- {}\n- {}", epkg_manager_url, epkg_url);

        // Download files
        let urls = vec![
            epkg_manager_url.clone(),
            format!("{}{}-{}",          epkg_url, elf_loader, arch),
            format!("{}{}-{}.sha256",   epkg_url, elf_loader, arch),
        ];

        // Download with better error handling
        download_urls(urls, &dirs().epkg_cache.to_str().unwrap(), 6, 6, None)
            .context("Failed to download required files")?;

        // Verify the downloaded files exist
        let epkg_manager_tar = dirs().epkg_cache.join(format!("{}.tar.gz", epkg_version));
        if !epkg_manager_tar.exists() {
            return Err(anyhow::anyhow!("Failed to download epkg manager tar file from {}", epkg_manager_url));
        }

        // Verify checksums
        utils::verify_sha256sum(&dirs().epkg_cache.join(format!("{}-{}.sha256", elf_loader, arch)))?;

        Ok(())
    }

    fn setup_common_environment(&mut self) -> Result<()> {
        let common_env_root = self.get_env_root("common".to_string())?;

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

        if let Err(e) = std::os::unix::fs::symlink(&epkg_extracted_dir, &epkg_manager_dir) {
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
            .ok_or_else(|| anyhow::anyhow!("Invalid shell path"))?;

        let rc_path = match shell {
            "bash" => env::var("HOME")? + "/.bashrc",
            "zsh" => env::var("HOME")? + "/.zshrc",
            _ => return Err(anyhow::anyhow!("Unsupported shell: {}", shell)),
        };

        // Get the common environment root path using get_env_root
        let common_env_root = self.get_env_root("common".to_string())?;
        let rc_content = format!(
            "\n# epkg begin\nsource {}/usr/opt/epkg-manager/lib/epkg-rc.sh\n# epkg end\n",
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
