use std::env;
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use crate::models::*;
use crate::repo::RepoRevise;
use crate::utils;
use crate::userdb;
use color_eyre::eyre::{self};
use color_eyre::Result;

impl EPKGDirs {
    pub fn build_dirs(options: &EPKGConfig) -> Result<Self> {
        let opt_epkg = PathBuf::from("/opt/epkg");
        let home = get_home()?;
        let home_epkg = PathBuf::from(&home).join(".epkg");
        let private_cache = PathBuf::from(&home).join(".cache/epkg");

        let (epkg_store, epkg_cache) = if options.init.shared_store {
            // Shared store/cache live under /opt/epkg
            (opt_epkg.join("store"), opt_epkg.join("cache"))
        } else {
            // Non-shared store uses user home, cache uses private_cache
            (home_epkg.join("store"), private_cache.clone())
        };

        // Get username - if it fails, the error will propagate upward
        let username = get_username()?;

        let user_envs = if options.init.shared_store {
            // /opt/epkg/envs/$USER
            opt_epkg.join(format!("envs/{}", username))
        } else {
            // $HOME/.epkg/envs
            home_epkg.join("envs")
        };

        let user_aur_builds = if options.init.shared_store {
            // /opt/epkg/cache/aur_builds/$USER
            epkg_cache.join("aur_builds").join(&username)
        } else {
            // $HOME/.cache/epkg/aur_builds
            private_cache.join("aur_builds")
        };

        Ok(Self {
            opt_epkg,
            home_epkg,
            user_envs,
            user_aur_builds,
            epkg_downloads_cache: epkg_cache.join("downloads"),
            epkg_channels_cache: epkg_cache.join("channels"),
            epkg_store,
            epkg_cache,
        })
    }
}

/// Get the base path to unpack package temporarily
pub fn unpack_basedir() -> PathBuf {
    dirs().epkg_store.join("unpack")
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
/// Location is determined by InitOptions.shared_store:
///   - shared_store=false: $HOME/.epkg/envs/$env_name
///   - shared_store=true:
///     - self:   /opt/epkg/envs/root/self
///     - others: /opt/epkg/envs/$USER/$env_name
/// Supports both 'env_name' and 'owner/env_name' formats
/// Note: EnvConfig.public only controls visibility/permissions, not location
fn get_env_base_path(env_name: &str) -> PathBuf {
    if config().init.shared_store {
        if env_name == SELF_ENV {
            return public_envs_path().join("root").join(SELF_ENV);
        }

        // Visit other's /opt/epkg/envs/$owner/$name
        if let Some(slash_pos) = env_name.find('/') {
            if !matches!(config().subcommand,
                  EpkgCommand::Run
                | EpkgCommand::Info
                | EpkgCommand::List
                | EpkgCommand::Search
            ) {
                use std::process::exit;
                eprintln!("Can only read-only visit others public env via `epkg run|info|search`");
                exit(1);
            }

            let owner = &env_name[..slash_pos];
            let name = &env_name[slash_pos + 1..];
            return dirs().opt_epkg.join("envs").join(owner).join(name);
        }
    }

    // Visit my own env
    dirs().user_envs.join(env_name)
}

/// Get the path to an environment's configuration file
pub fn get_env_config_path(env_name: &str) -> PathBuf {
    get_env_base_path(&env_name)
        .join("etc/epkg/env.yaml")
}

pub fn find_env_base(env_name: &str) -> Option<PathBuf> {
    // Find environment based on current shared_store setting
    let base = get_env_base_path(env_name);
    if base.join("etc/epkg/env.yaml").exists() {
        return Some(base);
    }
    None
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
pub fn get_epkg_src_path() -> PathBuf {
    get_env_base_path(SELF_ENV).join("usr/src/epkg")
}

/// Retrieves the home directory path, trying multiple methods.
/// When running as setuid, validates environment variables against real UID for security.
pub fn get_home() -> Result<String> {
    // Security check: if running as setuid, get home from real UID and validate env vars
    #[cfg(unix)]
    if utils::is_suid() {
        let real_home = utils::get_home_from_uid()?;

        // Validate HOME environment variable if set
        if let Ok(env_home) = env::var("HOME") {
            if env_home != real_home {
                return Err(eyre::eyre!(
                    "Security violation: HOME environment variable ('{}') does not match real UID home ('{}'). \
                    Environment variables cannot be trusted when running as setuid.",
                    env_home, real_home
                ));
            }
        }

        // fixup bare docker HOME=/
        if real_home == "/" && utils::is_running_as_root() {
            return Ok("/root".to_string());
        }

        return Ok(real_home);
    }

    // Try HOME environment variable first (only when not setuid)
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

    // Try using /etc/passwd directly (works in statically linked binaries)
    // getpwuid() doesn't work reliably in static builds due to NSS limitations
    #[cfg(unix)]
    {
        let uid = unsafe { libc::getuid() };
        if let Ok(home) = userdb::get_home_by_uid(uid, None) {
            return Ok(home);
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

pub fn get_repo_dir(repo: &RepoRevise) -> PathBuf {
    dirs()
        .epkg_channels_cache
        .join(&repo.channel)
        .join(repo.repodata_name.clone())
        .join(repo.arch.clone())
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

/// Common helper to collect shell RC file paths for both global and user scopes.
/// It first checks if the shell binary is installed via `is_shell_installed()`,
/// then verifies that the RC file actually exists before returning the path.
/// If `home_dir` is `None`, treats paths as global (absolute). Otherwise, treats them as relative to `home_dir`.
fn collect_shell_rc_paths(
    entries: &[(&str, &str)],
    home_dir: Option<&Path>,
) -> Vec<String> {
    let mut rc_paths = Vec::new();

    for (rc_sub_path, shell_name) in entries {
        if !is_shell_installed(shell_name) {
            continue;
        }

        let rc_path = if let Some(home) = home_dir {
            home.join(rc_sub_path)
        } else {
            PathBuf::from(rc_sub_path)
        };

        if rc_path.exists() {
            rc_paths.push(rc_path.to_string_lossy().into_owned());
        }
    }

    rc_paths
}

/// Get global shell RC files (e.g., `/etc/bash.bashrc`, `/etc/zsh/zshrc`)
/// for installed shells only.
pub fn get_global_shell_rc() -> Result<Vec<String>> {
    let entries = [
        ("/etc/bash.bashrc", "bash"),
        ("/etc/zsh/zshrc", "zsh"),
    ];

    Ok(collect_shell_rc_paths(&entries, None))
}

/// Get per-user shell RC files under `home_dir` for installed shells only.
pub fn get_user_shell_rc(home_dir: &Path) -> Result<Vec<String>> {
    let entries = [
        (".bashrc", "bash"),
        (".zshrc", "zsh"),
        (".kshrc", "ksh"),
        (".cshrc", "csh"),
        (".tcshrc", "tcsh"),
        (".config/fish/config.fish", "fish"),
    ];

    Ok(collect_shell_rc_paths(&entries, Some(home_dir)))
}

/// Get username, with security validation when running as setuid.
/// When running as setuid, validates environment variables against real UID for security.
pub fn get_username() -> Result<String> {
    // Security check: if running as setuid, get username from real UID and validate env vars
    #[cfg(unix)]
    if utils::is_suid() {
        let real_username = utils::get_username_from_uid()?;

        // Validate USER environment variable if set
        if let Ok(env_user) = env::var("USER") {
            if !env_user.is_empty() && env_user != real_username {
                return Err(eyre::eyre!(
                    "Security violation: USER environment variable ('{}') does not match real UID username ('{}'). \
                    Environment variables cannot be trusted when running as setuid.",
                    env_user, real_username
                ));
            }
        }

        return Ok(real_username);
    }

    // Try USER environment variable first (common on Unix/Linux) - only when not setuid
    if let Ok(username) = env::var("USER") {
        if !username.is_empty() {
            return Ok(username);
        }
    }

    // Try USERNAME environment variable (common on Windows) - only when not setuid
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

    // Fallback: try to get username from real UID (Unix only)
    #[cfg(unix)]
    {
        if let Ok(username) = utils::get_username_from_uid() {
            return Ok(username);
        }
    }

    // If all else fails, return a descriptive error
    Err(eyre::eyre!("Could not determine username. Please ensure either USER or USERNAME environment variables are set. This is required to set up the public environments directory."))
}

/// Get the base path to users' public environments directories
pub fn public_envs_path() -> PathBuf {
    dirs().opt_epkg.join("envs")
}

/// Walk all directories in the given parent directory and call the callback for each.
///
/// This helper function walks the "bottom" directory level, calling the callback
/// for each subdirectory found with (env_path, owner_opt).
pub fn walk_bottom_dir<F>(parent_path: &Path, owner_opt: Option<&str>, callback: &mut F) -> Result<()>
where
    F: FnMut(&Path, Option<&str>) -> Result<()>,
{
    if let Ok(entries) = fs::read_dir(parent_path) {
        for entry in entries.flatten() {
            let env_path = entry.path();
            if env_path.is_dir() {
                callback(&env_path, owner_opt)?;
            }
        }
    }
    Ok(())
}

/// Walk environments based on shared_store setting:
/// - If !shared_store: walk $HOME/.epkg/envs/*
/// - If  shared_store: walk /opt/epkg/envs/*/*
///
/// Calls the callback for each environment found with (env_path, owner_opt).
/// owner_opt is Some(owner) for shared_store, None for private.
pub fn walk_environments<F>(mut callback: F) -> Result<()>
where
    F: FnMut(&Path, Option<&str>) -> Result<()>,
{
    if config().init.shared_store {
        // Walk /opt/epkg/envs/*/*
        let allusers_envs_base = public_envs_path();
        if let Ok(entries) = fs::read_dir(&allusers_envs_base) {
            for entry in entries.flatten() {
                let owner_path = entry.path();
                if owner_path.is_dir() {
                    let owner = owner_path.file_name()
                        .and_then(|n| n.to_str());
                    walk_bottom_dir(&owner_path, owner, &mut callback)?;
                }
            }
        }
    } else {
        // Walk $HOME/.epkg/envs/*
        let personal_envs_root = &dirs().user_envs;
        walk_bottom_dir(personal_envs_root, None, &mut callback)?;
    }

    Ok(())
}
