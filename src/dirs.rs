use std::env;
use std::path::PathBuf;
use std::io::{self, ErrorKind};
use crate::models::*;
use crate::repo::RepoRevise;
use color_eyre::Result;
use color_eyre::eyre;

#[derive(Default)]
pub struct EPKGDirsBuilder {
    options: Option<EPKGConfig>,
    custom_home: Option<PathBuf>,
    custom_opt: Option<PathBuf>,
}

impl EPKGDirs {
    pub fn builder() -> EPKGDirsBuilder {
        EPKGDirsBuilder::default()
    }

    // Helper method to create dirs using proper path joining
    fn build_dirs(options: &EPKGConfig, home_epkg: &PathBuf, opt_epkg: &PathBuf) -> io::Result<Self> {
        let (store_root, cache_root) = if options.init.shared_store {
            (opt_epkg.join("store"), opt_epkg.join("cache"))
        } else {
            (home_epkg.join("store"), get_xdg_cache()?.join("epkg"))
        };

        // Get username - if it fails, the error will propagate upward
        let username = get_username()?;
        let user_pubenvs = opt_epkg.join(format!("envs/{}", username));

        Ok(Self {
            opt_epkg: opt_epkg.clone(),
            home_epkg: home_epkg.clone(),
            home_config: home_epkg.join("config"),
            private_envs: home_epkg.join("envs"),
            public_envs: user_pubenvs,
            epkg_store: store_root,
            epkg_cache: cache_root.clone(),
            epkg_channel_cache: cache_root.join("channel"),
            epkg_downloads_cache: cache_root.join("downloads"),
        })
    }
}

impl EPKGDirsBuilder {
    pub fn with_options(mut self, options: EPKGConfig) -> Self {
        self.options = Some(options);
        self
    }

    #[allow(dead_code)]
    pub fn with_custom_home(mut self, path: PathBuf) -> Self {
        self.custom_home = Some(path);
        self
    }

    pub fn build(self) -> io::Result<EPKGDirs> {
        let options = self.options.unwrap_or_default();

        let home_epkg = match self.custom_home {
            Some(path) => path,
            None => get_home_epkg_path()
        };

        let opt_epkg = self.custom_opt.unwrap_or_else(|| PathBuf::from("/opt/epkg"));

        EPKGDirs::build_dirs(&options, &home_epkg, &opt_epkg)
    }
}

impl PackageManager {

    pub fn get_env_root(&mut self, env_name: String) -> Result<PathBuf> {
        let env_config = self.get_env_config(env_name)?;
        Ok(PathBuf::from(&env_config.env_root))
    }

    pub fn get_default_env_root(&mut self) -> Result<PathBuf> {
        self.get_env_root(config().common.env.clone())
    }

    pub fn get_generations_root(&mut self, env_name: &str) -> Result<PathBuf> {
        let env_root = self.get_env_root(env_name.to_string())?;
        Ok(env_root.join("generations"))
    }

    pub fn get_default_generations_root(&mut self) -> Result<PathBuf> {
        self.get_generations_root(&config().common.env)
    }
}

// Find the path to an environment's root directory, the path is canonicalized and exists
// Returns None if the environment is not found
pub fn find_env_root(env_name: &str) -> Option<PathBuf> {
    // Check private env first
    let private_env_base = dirs().home_epkg.join("envs").join(env_name);
    if private_env_base.exists() {
        return std::fs::canonicalize(private_env_base).ok();
    }

    // Check public env
    if let Ok(username) = get_username() {
        let public_env_base = dirs().opt_epkg.join("envs").join(username).join(env_name);
        if public_env_base.exists() {
            return std::fs::canonicalize(public_env_base).ok();
        }
    }

    None
}

/// Retrieves the home directory path, trying multiple methods.
pub fn get_home() -> Result<String> {
    // Try HOME environment variable first
    if let Ok(home) = env::var("HOME") {
        return Ok(home);
    }

    // Try using getpwuid on Unix systems
    #[cfg(unix)]
    {
        use std::ffi::CStr;
        use std::os::raw::{c_char, c_int};

        extern "C" {
            fn getuid() -> c_int;
            fn getpwuid(uid: c_int) -> *mut libc::passwd;
        }

        unsafe {
            let uid = getuid();
            let passwd = getpwuid(uid);
            if !passwd.is_null() {
                let home_dir = (*passwd).pw_dir as *const c_char;
                if !home_dir.is_null() {
                    if let Ok(home) = CStr::from_ptr(home_dir).to_str() {
                        return Ok(home.to_string());
                    }
                }
            }
        }
    }

    // Try matching path patterns to find home directory
    if let Ok(current_dir) = std::env::current_dir() {
        let path_str = current_dir.to_string_lossy();

        // Check if path starts with /home/username
        if let Some(captures) = regex::Regex::new(r"^(/home/[^/]+)").ok().and_then(|re| re.captures(&path_str)) {
            if let Some(home_match) = captures.get(1) {
                return Ok(home_match.as_str().to_string());
            }
        }

        // Check if path is in /root
        if path_str.starts_with("/root") {
            return Ok("/root".to_string());
        }
    }

    Err(eyre::eyre!("Could not determine home directory"))
}

pub fn get_repo_dir(repo: &RepoRevise) -> Result<PathBuf> {
    let channel_dir = dirs().epkg_cache.join("channel");
    let repo_dir = channel_dir.join(&repo.channel).join(repo.repodata_name.clone()).join(repo.arch.clone());
    Ok(repo_dir)
}

/// Find the first existing dir:
/// - $HOME/.epkg/envs/common/opt/epkg-manager
/// - /opt/epkg/envs/root/common/opt/epkg-manager
pub fn get_epkg_manager_path() -> Result<PathBuf> {
    let common_env_root = find_env_root("common")
                .ok_or_else(|| eyre::eyre!("Common environment not found"))?;
    Ok(common_env_root.join("opt/epkg-manager"))
}

/// Get the path to an environment's configuration file
/// $HOME/.epkg/config/envs/$env.yaml
pub fn get_env_config_path(env_name: &str) -> PathBuf {
    dirs().home_config.join("envs").join(format!("{}.yaml", env_name))
}

/// $HOME/.epkg
pub fn get_home_epkg_path() -> PathBuf {
    let home = get_home().expect("HOME environment variable not set");
    PathBuf::from(home).join(".epkg")
}

fn get_xdg_cache() -> io::Result<PathBuf> {
    // Try XDG_CACHE_HOME first
    match env::var("XDG_CACHE_HOME") {
        Ok(xdg_cache_str) => Ok(PathBuf::from(xdg_cache_str)),
        Err(_) => { // XDG_CACHE_HOME not set or other VarError
            // Try HOME/.cache
            match get_home() { // This returns color_eyre::Result<String>
                Ok(home_str) => Ok(PathBuf::from(home_str).join(".cache")),
                Err(eyre_report) => {
                    // Convert eyre::Report to io::Error
                    Err(io::Error::new(ErrorKind::NotFound, format!("Could not determine cache directory: XDG_CACHE_HOME not set and failed to get HOME: {}", eyre_report)))
                }
            }
        }
    }
}

fn get_username() -> io::Result<String> {
    // Try USER environment variable first (common on Unix/Linux)
    if let Ok(username) = env::var("USER") {
        if !username.is_empty() {
            return Ok(username);
        }
    }

    // Try USERNAME environment variable (common on Windows)
    if let Ok(username) = env::var("USERNAME") {
        if !username.is_empty() {
            return Ok(username);
        }
    }

    // Try to get username from HOME path
    if let Ok(home) = get_home() {
        let path = PathBuf::from(home);
        if let Some(username) = path.file_name().and_then(|n| n.to_str()) {
            if !username.is_empty() {
                return Ok(username.to_string());
            }
        }
    }

    // If all else fails, return a descriptive error
    Err(io::Error::new(
        ErrorKind::NotFound,
        "Could not determine username. Please ensure either USER or USERNAME environment variables are set. \
         This is required to set up the public environments directory."
    ))
}
