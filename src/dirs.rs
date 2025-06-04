use std::env;
use std::path::{Path, PathBuf};

use crate::models::*;
use crate::repo::RepoRevise;
use color_eyre::Result;
use color_eyre::eyre::{self, WrapErr};

#[derive(Debug, Clone, PartialEq)] // Reverted derives
pub struct ShellRcInfo {
    pub rc_file_path: String,
    pub _shell_name: String,
    pub source_script_name: String,
}

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
    fn build_dirs(options: &EPKGConfig, home_epkg: &PathBuf, opt_epkg: &PathBuf) -> Result<Self> {
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

    pub fn build(self) -> Result<EPKGDirs> {
        let options = self.options.unwrap_or_default();

        let home_epkg = match self.custom_home {
            Some(path) => path,
            None => get_home_epkg_path()?
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
pub fn get_home_epkg_path() -> Result<PathBuf> {
    let home = get_home().wrap_err("Failed to get HOME directory for .epkg path")?;
    Ok(PathBuf::from(home).join(".epkg"))
}

fn get_xdg_cache() -> Result<PathBuf> {
    match env::var("XDG_CACHE_HOME") {
        Ok(path_str) if !path_str.is_empty() => Ok(PathBuf::from(path_str)),
        _ => { // Covers Err cases and empty Ok string
            let home_str = get_home().wrap_err("XDG_CACHE_HOME not found or invalid, and failed to get HOME directory for fallback cache")?;
            Ok(PathBuf::from(home_str).join(".cache"))
        }
    }
}

fn determine_rc_and_script_for_shell(shell_name: &str, home_dir: &Path) -> Option<ShellRcInfo> {
    let (rc_file_sub_path, script_name_str) = match shell_name {
        "bash" => (".bashrc", "epkg-rc.sh"),
        "zsh" => (".zshrc", "epkg-rc.sh"),
        "ksh" => (".kshrc", "epkg-rc.sh"),
        "fish" => (".config/fish/config.fish", "epkg-rc.sh"), // Assuming epkg-rc.sh is fish-compatible
        "csh" => (".cshrc", "epkg-rc.csh"),
        "tcsh" => (".tcshrc", "epkg-rc.csh"),
        _ => return None, // Shell name not supported
    };

    let full_rc_path = home_dir.join(rc_file_sub_path.trim_start_matches('/'));

    if full_rc_path.exists() {
        Some(ShellRcInfo {
            rc_file_path: full_rc_path.to_string_lossy().into_owned(),
            _shell_name: shell_name.to_string(), // Use the input shell_name
            source_script_name: script_name_str.to_string(),
        })
    } else {
        None // RC file does not exist for this supported shell
    }
}

pub fn get_shell_rc() -> Result<Vec<ShellRcInfo>> {
    let home_path_str = get_home().wrap_err("Failed to get home directory.")?;
    let home_dir = PathBuf::from(home_path_str);
    let mut infos: Vec<ShellRcInfo> = Vec::new(); // Use Vec

    match env::var("SHELL") {
        Ok(shell_env_var) => {
            if let Some(detected_shell_name_str) = Path::new(&shell_env_var).file_name().and_then(|s| s.to_str()) {
                if let Some(shell_rc_info) = determine_rc_and_script_for_shell(detected_shell_name_str, &home_dir) {
                    // SHELL var is valid, points to a supported shell, and its rc file exists.
                    // Use it exclusively and return early.
                    infos.push(shell_rc_info);
                    return Ok(infos); // Early return with Vec
                } else {
                    // SHELL var specified a shell, but it was unsupported or its rc file didn't exist.
                    // Print warning and proceed to fallback scan.
                    eprintln!(
                        "Warning: SHELL variable is '{}'. Could not derive a valid, existing rc file from it (either unsupported shell type or its specific rc file was not found). Falling back to scanning for common rc files.",
                        shell_env_var
                    );
                }
            } else {
                // SHELL var path was invalid. Print warning and proceed to fallback scan.
                eprintln!(
                    "Warning: SHELL variable ('{}') has an invalid path. Could not extract filename. Falling back to scanning for common rc files.",
                    shell_env_var
                );
            }
        }
        Err(_) => {
            // SHELL var not set. Print warning and proceed to fallback scan.
            eprintln!("Warning: SHELL environment variable not set. Attempting to detect common shell configuration files.");
        }
    }

    // If we reach here, it means either SHELL var was not set, was invalid,
    // or didn't lead to a usable rc file. So, perform fallback scan.
    let common_shell_names = ["bash", "zsh", "fish", "ksh", "csh", "tcsh"];
    for shell_name_str in common_shell_names.iter() {
        if let Some(shell_rc_info) = determine_rc_and_script_for_shell(shell_name_str, &home_dir) {
            infos.push(shell_rc_info);
        }
    }

    if infos.is_empty() {
        eprintln!("Warning: Could not identify any usable shell configuration files to update.");
    }

    Ok(infos)
}

fn get_username() -> Result<String> {
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
    let home = get_home()?;
        let path = PathBuf::from(home);
        if let Some(username) = path.file_name().and_then(|n| n.to_str()) {
            if !username.is_empty() {
                return Ok(username.to_string());
            }
        }

    // If all else fails, return a descriptive error
    Err(eyre::eyre!("Could not determine username. Please ensure either USER or USERNAME environment variables are set. This is required to set up the public environments directory."))
}
