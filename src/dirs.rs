use std::env;
use std::path::{Path, PathBuf};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use crate::models::*;
use crate::repo::RepoRevise;
use crate::utils;
use color_eyre::Result;
use color_eyre::eyre::{self, WrapErr};

#[derive(Debug, Clone, PartialEq)] // Reverted derives
pub struct ShellRcInfo {
    pub rc_file_path: String,
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
        // Private cache is always under XDG cache (e.g. $HOME/.cache/epkg)
        let private_cache = get_xdg_cache()?.join("epkg");

        let (store_root, cache_root) = if options.init.shared_store {
            // Shared store/cache live under /opt/epkg, but AUR builds still use private_cache
            (opt_epkg.join("store"), opt_epkg.join("cache"))
        } else {
            // Non-shared store uses user home, cache uses private_cache
            (home_epkg.join("store"), private_cache.clone())
        };

        // Get username - if it fails, the error will propagate upward
        let username = get_username()?;
        let user_pubenvs = opt_epkg.join(format!("envs/{}", username));

        Ok(Self {
            opt_epkg: opt_epkg.clone(),
            home_epkg: home_epkg.clone(),
            private_envs: home_epkg.join("envs"),
            public_envs: user_pubenvs,
            epkg_store: store_root,
            epkg_cache: cache_root.clone(),
            epkg_downloads_cache: cache_root.join("downloads"),
            epkg_channel_cache: cache_root.join("channel"),
            // AUR builds always go under the private cache, never under /opt/epkg
            epkg_aur_builds: private_cache.join("aur_builds"),
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
        let options = self.options.unwrap();

        let home_epkg = match self.custom_home {
            Some(path) => path,
            None => get_home_epkg_path()?
        };

        let opt_epkg = self.custom_opt.unwrap_or_else(|| PathBuf::from("/opt/epkg"));

        EPKGDirs::build_dirs(&options, &home_epkg, &opt_epkg)
    }
}

pub fn get_env_root(env_name: String) -> Result<PathBuf> {
    let env_config = crate::models::env_config();
    if env_config.name == env_name {
        Ok(PathBuf::from(&env_config.env_root))
    } else {
        let env_config = crate::io::deserialize_env_config_for(env_name)?;
        Ok(PathBuf::from(&env_config.env_root))
    }
}

pub fn get_default_env_root() -> Result<PathBuf> {
    get_env_root(config().common.env.clone())
}

pub fn get_generations_root(env_name: &str) -> Result<PathBuf> {
    let env_root = get_env_root(env_name.to_string())?;
    Ok(env_root.join("generations"))
}

pub fn get_default_generations_root() -> Result<PathBuf> {
    get_generations_root(&config().common.env)
}

/// Get the base path for an environment
///   - private: $HOME/.epkg/envs/$env_name
///   - public: /opt/epkg/envs/$username/$env_name
pub fn get_env_base_path(env_name: &str, public: bool) -> Result<PathBuf> {
    if public {
        let username = get_username()?;
        Ok(dirs().opt_epkg.join("envs").join(username).join(env_name))
    } else {
        Ok(dirs().home_epkg.join("envs").join(env_name))
    }
}

pub fn find_env_base(env_name: &str) -> Option<PathBuf> {
    // Check private env first
    let private_env_base = dirs().home_epkg.join("envs").join(env_name);
    // Checking $private_env_base is not enough: `epkg run` could mkdir $private_env_base/opt_real/
    if private_env_base.join("etc/epkg/env.yaml").exists() {
        return Some(private_env_base);
    }

    // Check public envs - search through all users' public environment directories
    let public_envs_parent = dirs().opt_epkg.join("envs");
    if let Ok(entries) = fs::read_dir(&public_envs_parent) {
        for entry in entries {
            if let Ok(entry) = entry {
                let public_env_base = entry.path().join(env_name);
                if public_env_base.join("etc/epkg/env.yaml").exists() {
                    return Some(public_env_base);
                }
            }
        }
    }

    None
}

pub fn find_env_config_path(env_name: &str) -> Option<PathBuf> {
    find_env_base(env_name).map(|base| base.join("etc/epkg/env.yaml"))
}

/// Get the path to an environment's configuration file
pub fn get_env_config_path(env_config: &EnvConfig) -> PathBuf {
    get_env_base_path(&env_config.name, env_config.public)
        .expect("Failed to get env base path")
        .join("etc/epkg/env.yaml")
}

/// Find the root directory for an environment by searching both private and public locations
/// Returns None if the environment is not found
pub fn find_env_root(env_name: &str) -> Option<PathBuf> {
    if let Some(env_base) = find_env_base(env_name) {
        // Canonicalize the base to resolve symlinks and get the real path
        return std::fs::canonicalize(env_base).ok();
    }
    None
}

/// Find the first existing dir:
/// - $HOME/.epkg/envs/self/usr/src/epkg
/// - /opt/epkg/envs/root/self/usr/src/epkg
pub fn get_epkg_src_path() -> Result<PathBuf> {
    let self_env_root = find_env_root(SELF_ENV)
                .ok_or_else(|| eyre::eyre!("Self environment not found"))?;
    Ok(self_env_root.join("usr/src/epkg"))
}

/// Retrieves the home directory path, trying multiple methods.
pub fn get_home() -> Result<String> {
    // Try HOME environment variable first
    if let Ok(home) = env::var("HOME") {
        // fixup bare docker HOME=/
        if home == "/" && utils::is_running_as_root() {
            return Ok("/root".to_string());
        }

        return Ok(home);
    }

    // bare docker may have HOME=/
    if utils::is_running_as_root() {
        return Ok("/root".to_string());
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

/// Check if a shell binary is installed and executable
fn is_shell_installed(shell_name: &str) -> bool {
    // Common locations where shell binaries are typically installed
    let shell_paths = [
        format!("/bin/{}", shell_name),
        format!("/usr/bin/{}", shell_name),
        format!("/usr/local/bin/{}", shell_name),
    ];

    for path in &shell_paths {
        if let Ok(metadata) = fs::metadata(path) {
            // Check if the file is executable
            #[cfg(unix)]
            {
                if metadata.permissions().mode() & 0o111 != 0 {
                    return true;
                }
            }
            #[cfg(not(unix))]
            {
                // On non-Unix systems, just check if the file exists
                return true;
            }
        }
    }
    false
}

fn determine_rc_and_script_for_shell(shell_name: &str, home_dir: &Path) -> Option<ShellRcInfo> {
    // First check if the shell binary is actually installed
    if !is_shell_installed(shell_name) {
        return None; // Shell is not installed, don't update its config
    }

    let (rc_file_sub_path, script_name_str) = match shell_name {
        "bash" => (".bashrc", "epkg-rc.sh"),
        "zsh" => (".zshrc", "epkg-rc.sh"),
        "ksh" => (".kshrc", "epkg-rc.sh"),
        "csh" => (".cshrc", "epkg-rc.csh"),
        "tcsh" => (".tcshrc", "epkg-rc.csh"),
        "fish" => (".config/fish/config.fish", "epkg-rc.sh"), // Assuming epkg-rc.sh is fish-compatible
        _ => return None, // Shell name not supported
    };

    let full_rc_path = home_dir.join(rc_file_sub_path.trim_start_matches('/'));

    if full_rc_path.exists() {
        Some(ShellRcInfo {
            rc_file_path: full_rc_path.to_string_lossy().into_owned(),
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
                    println!(
                        "Notice: SHELL variable is '{}'. Could not derive a valid, existing rc file from it (either unsupported shell type or its specific rc file was not found). Falling back to scanning for common rc files.",
                        shell_env_var
                    );
                }
            } else {
                // SHELL var path was invalid. Print warning and proceed to fallback scan.
                println!(
                    "Notice: SHELL variable ('{}') has an invalid path. Could not extract filename. Falling back to scanning for common rc files.",
                    shell_env_var
                );
            }
        }
        Err(_) => {
            // SHELL var not set. Print warning and proceed to fallback scan.
            println!("Notice: SHELL environment variable not set. Attempting to detect common shell configuration files.");
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

pub fn get_username() -> Result<String> {
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

/// Get the path to user's private environments directory
pub fn user_private_envs(user_home: &str) -> PathBuf {
    PathBuf::from(user_home).join(".epkg/envs")
}

/// Get the path to user's public environments directory
pub fn user_public_envs(username: &str) -> PathBuf {
    dirs().opt_epkg.join("envs").join(username)
}
