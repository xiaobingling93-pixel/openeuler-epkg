//! # Path layout (`dirs`) at startup
//!
//! How `EPKGDirs` is filled before the rest of epkg runs:
//!
//! 1. **Load YAML** — `load_config_from_matches` deserializes `options.yaml` (or `--config`) into
//!    `EPKGConfig`. The `dirs` field may be empty or only partially set.
//! 2. **Common options** — `parse_options_common` applies global CLI flags, arch, and
//!    `init.shared_store` (except `self install`, which sets store mode from its own flags).
//! 3. **Subcommand** — `parse_options_subcommand` runs the subcommand parser, then
//!    `determine_environment_final` and `validate_env_name`.
//! 4. **Finalize dirs** — [`init_config_dirs`] runs `EPKGDirs::build_dirs` (OS + `shared_store`),
//!    merges into `config.dirs`, then moves that merged value into a process-global [`OnceLock`]
//!    (YAML overrides preserved). The `dirs` field on the stored [`crate::models::EPKGConfig`] is
//!    left empty after the move.
//! 5. **Readers** — [`crate::models::dirs`] and [`dirs_ref`] return `&'static EPKGDirs` pointing at
//!    that single `OnceLock` allocation (no per-call clone).

use std::env;
use std::sync::OnceLock;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use crate::models::*;
use crate::repo::RepoRevise;
#[cfg(unix)]
use crate::utils;
#[cfg(unix)]
use crate::userdb;
use color_eyre::eyre::{self};
use color_eyre::Result;

/// Holds the merged [`EPKGDirs`] after [`init_config_dirs`] (exactly one `set`; read-only thereafter).
static DIRS: OnceLock<EPKGDirs> = OnceLock::new();

/// Read-only path layout for this process (same backing store as [`crate::models::dirs`]).
///
/// Prefer [`crate::models::dirs`] at call sites so [`crate::models::config`] is initialized first;
/// use `dirs_ref` only when you know parsing has already run.
#[inline]
pub fn dirs_ref() -> &'static EPKGDirs {
    DIRS.get()
        .expect("dirs not initialized (init_config_dirs must run before dirs_ref)")
}

/// Compute `user_envs` path without relying on `dirs_ref()`.
/// This can be called during config initialization.
///
/// - If shared_store: /opt/epkg/envs/$USER
/// - If !shared_store: $HOME/.epkg/envs
pub fn compute_user_envs(shared_store: bool) -> Result<PathBuf> {
    if shared_store {
        let username = get_username()?;
        Ok(PathBuf::from("/opt/epkg/envs").join(&username))
    } else {
        let home = get_home()?;
        Ok(PathBuf::from(&home).join(".epkg").join("envs"))
    }
}

/// Join `parts` onto `base` with one `Path::join` per component (avoids `.join("a/b")` mixing
/// separators on Windows.)
#[inline]
pub fn path_join<P: AsRef<Path> + ?Sized>(base: &P, parts: &[&str]) -> PathBuf {
    let mut p = base.as_ref().to_path_buf();
    for s in parts {
        p = p.join(s);
    }
    p
}

/// Basename of the epkg binary under each environment's `usr/bin/` (matches `init` copy target).
#[cfg(windows)]
pub const EPKG_USR_BIN_NAME: &str = "epkg.exe";
#[cfg(not(windows))]
pub const EPKG_USR_BIN_NAME: &str = "epkg";

#[cfg(unix)]
impl EPKGDirs {
    /// Computes the full default path layout for `options` (shared vs private store, home, username).
    ///
    /// **Steps:**
    ///
    /// 1. Called from `init_config_dirs` before `merge_from`.
    /// 2. [`init_config_dirs`] merges this into `config.dirs`, then moves the result into the
    ///    global `OnceLock` (see that function).
    pub fn build_dirs(options: &EPKGConfig) -> Result<Self> {
        let opt_epkg = PathBuf::from("/opt/epkg");
        let home = get_home()?;
        let home_epkg = PathBuf::from(&home).join(".epkg");
        let home_cache = path_join(&PathBuf::from(&home), &[".cache", "epkg"]);

        let (epkg_store, epkg_cache) = if options.init.shared_store {
            // Shared store/cache live under /opt/epkg
            (opt_epkg.join("store"), opt_epkg.join("cache"))
        } else {
            // Non-shared store uses user home, cache uses home_cache
            (home_epkg.join("store"), home_cache.clone())
        };

        // Get username - if it fails, the error will propagate upward
        let username = get_username()?;

        let user_envs = if options.init.shared_store {
            // /opt/epkg/envs/$USER
            opt_epkg.join("envs").join(&username)
        } else {
            // $HOME/.epkg/envs
            home_epkg.join("envs")
        };

        let user_aur_builds = if options.init.shared_store {
            // /opt/epkg/cache/aur_builds/$USER
            epkg_cache.join("aur_builds").join(&username)
        } else {
            // $HOME/.cache/epkg/aur_builds
            home_cache.join("aur_builds")
        };

        Ok(Self {
            opt_epkg,
            home_epkg,
            home_cache,
            user_envs,
            user_aur_builds,
            epkg_downloads_cache: epkg_cache.join("downloads"),
            epkg_channels_cache: epkg_cache.join("channels"),
            epkg_store,
            epkg_cache,
        })
    }
}

/// Windows shared-install root: `D:\epkg` when drive `D:` exists, otherwise `C:\epkg`.
///
/// This only applies when you use a shared store install on Windows; the D: drive check avoids
/// putting a large store on a small system C: when a second disk is available.
#[cfg(windows)]
fn windows_global_epkg_root() -> PathBuf {
    let d = Path::new("D:\\");
    if d.exists() {
        PathBuf::from(r"D:\epkg")
    } else {
        PathBuf::from(r"C:\epkg")
    }
}

#[cfg(windows)]
impl EPKGDirs {
    /// Computes the full default path layout for `options` (shared vs private store, `%USERPROFILE%`, etc.).
    ///
    /// **Steps:**
    ///
    /// 1. Called from `init_config_dirs` before `merge_from`.
    /// 2. [`init_config_dirs`] merges this into `config.dirs`, then moves the result into the
    ///    global `OnceLock` (see that function).
    pub fn build_dirs(options: &EPKGConfig) -> Result<Self> {
        let opt_epkg = windows_global_epkg_root();

        // Per-user (private) layout: %USERPROFILE%\.epkg\ — store, envs, and cache live under
        // .epkg (no separate %USERPROFILE%\.cache\epkg).
        let user_profile = env::var("USERPROFILE")
            .map(PathBuf::from)
            .or_else(|_| env::var("HOME").map(PathBuf::from))
            .unwrap_or_else(|_| PathBuf::from("."));

        let home_epkg = user_profile.join(".epkg");
        let home_cache = home_epkg.join("cache");

        let (epkg_store, epkg_cache) = if options.init.shared_store {
            // Shared store/cache under C:\epkg\
            (opt_epkg.join("store"), opt_epkg.join("cache"))
        } else {
            // User-private: under %USERPROFILE%\.epkg\
            (home_epkg.join("store"), home_cache.clone())
        };

        // Get username
        let username = env::var("USERNAME")
            .or_else(|_| env::var("USER"))
            .unwrap_or_else(|_| "user".to_string());

        let user_envs = if options.init.shared_store {
            // C:\epkg\envs\<username>
            opt_epkg.join("envs").join(&username)
        } else {
            // %USERPROFILE%\.epkg\envs
            home_epkg.join("envs")
        };

        // AUR builds not applicable on Windows, but keep for struct completeness
        let user_aur_builds = epkg_cache.join("aur_builds").join(&username);

        Ok(Self {
            opt_epkg,
            home_epkg,
            home_cache,
            user_envs,
            user_aur_builds,
            epkg_downloads_cache: epkg_cache.join("downloads"),
            epkg_channels_cache: epkg_cache.join("channels"),
            epkg_store,
            epkg_cache,
        })
    }
}

/// Finishes your `dirs` configuration after CLI and `options.yaml` are loaded.
///
/// You may have set only some paths under `dirs:`; this fills every remaining empty field
/// using the same rules as a fresh install ([`EPKGDirs::build_dirs`]), then installs the merged
/// snapshot as the process-wide read-only path roots (see module docs).
///
/// **Steps:**
///
/// 1. `computed = EPKGDirs::build_dirs(config)?` — full default layout for current `EPKGConfig`.
/// 2. `config.dirs.merge_from(&computed)` — YAML paths win; empty slots filled from `computed`.
/// 3. `mem::take(&mut config.dirs)` into the global `OnceLock` — `config.dirs` becomes default-empty.
pub fn init_config_dirs(config: &mut EPKGConfig) -> Result<()> {
    if DIRS.get().is_some() {
        return Err(eyre::eyre!("init_config_dirs: already initialized"));
    }
    let computed = EPKGDirs::build_dirs(config)?;
    config.dirs.merge_from(&computed);
    let merged = std::mem::take(&mut config.dirs);
    DIRS.set(merged)
        .map_err(|_| eyre::eyre!("init_config_dirs: DIRS.set failed (duplicate init)"))?;
    Ok(())
}

impl EPKGDirs {
    /// Keeps paths you already set in `options.yaml` and copies defaults from `computed` only into empty slots.
    ///
    /// So your explicit overrides win; anything you left out gets the usual defaults for your OS
    /// and shared/private store mode.
    ///
    /// **Steps:**
    ///
    /// 1. For each path field, if `self` is empty, copy from the same field in `computed` (built by `build_dirs`).
    /// 2. Non-empty `self` values (from YAML) are left unchanged.
    ///
    /// [`init_config_dirs`] then moves `self` out into the global `OnceLock`; callers read paths via
    /// [`crate::models::dirs`] / [`dirs_ref`], not via the `dirs` field on [`EPKGConfig`] after init.
    pub fn merge_from(&mut self, computed: &EPKGDirs) {
        let m = |p: &mut PathBuf, q: &PathBuf| {
            if p.as_os_str().is_empty() {
                *p = q.clone();
            }
        };
        m(&mut self.opt_epkg, &computed.opt_epkg);
        m(&mut self.home_epkg, &computed.home_epkg);
        m(&mut self.home_cache, &computed.home_cache);
        m(&mut self.user_envs, &computed.user_envs);
        m(&mut self.user_aur_builds, &computed.user_aur_builds);
        m(&mut self.epkg_store, &computed.epkg_store);
        m(&mut self.epkg_cache, &computed.epkg_cache);
        m(&mut self.epkg_downloads_cache, &computed.epkg_downloads_cache);
        m(&mut self.epkg_channels_cache, &computed.epkg_channels_cache);
    }
}

/// Get the base path to unpack package temporarily
pub fn unpack_basedir() -> PathBuf {
    dirs_ref().epkg_store.join("unpack")
}

pub fn get_env_root(env_name: String) -> Result<PathBuf> {
    let current_env = config().common.env_name.clone();
    // Only use the cached env config if we're asking for the current environment
    if !current_env.is_empty() && current_env == env_name {
        let current_env_root = config().common.env_root.clone();
        if !current_env_root.is_empty() {
            Ok(current_env_root.into())
        } else {
            let env_config = env_config();
            Ok(PathBuf::from(&env_config.env_root))
        }
    } else {
        let env_config = crate::io::deserialize_env_config_for(env_name)?;
        Ok(PathBuf::from(&env_config.env_root))
    }
}

pub fn get_default_env_root() -> Result<PathBuf> {
    get_env_root(config().common.env_name.clone())
}

pub fn get_generations_root(env_name: &str) -> Result<PathBuf> {
    let env_root = get_env_root(env_name.to_string())?;
    Ok(env_root.join("generations"))
}

pub fn get_default_generations_root() -> Result<PathBuf> {
    get_generations_root(&config().common.env_name)
}

/// Get the base path for an environment
/// Location is determined by InitOptions.shared_store:
///   - shared_store=false: $HOME/.epkg/envs/$env_name
///   - shared_store=true:
///     - self:   /opt/epkg/envs/root/self (special env for package manager files only)
///     - others: /opt/epkg/envs/$USER/$env_name
/// Supports both 'env_name' and 'owner/env_name' formats
/// Note: EnvConfig.public only controls visibility/permissions, not location
pub fn get_env_base_path(env_name: &str) -> PathBuf {
    if env_name.is_empty() {
        panic!("env_name is empty in get_env_base_path");
    }
    // Visit other's /opt/epkg/envs/$owner/$name (public envs)
    if let Some(slash_pos) = env_name.find('/') {
        if !matches!(config().subcommand,
              EpkgCommand::Run
            | EpkgCommand::Info
            | EpkgCommand::List
            | EpkgCommand::EnvList
            | EpkgCommand::Search
        ) {
            use std::process::exit;
            eprintln!("Can only read-only visit others public env via `epkg run|info|search|list|env list`");
            exit(1);
        }
        let owner = &env_name[..slash_pos];
        let name = &env_name[slash_pos + 1..];
        return public_envs_path().join(owner).join(name);
    }

    // No slash: own environment
    if config().init.shared_store {
        if env_name == SELF_ENV {
            return public_envs_path().join("root").join(SELF_ENV);
        }
    }

    // Visit my own env
    dirs_ref().user_envs.join(env_name)
}

/// Get the relative path to epkg environment config (etc/epkg/env.yaml)
fn env_config_relative_path() -> PathBuf {
    path_join(Path::new(""), &["etc", "epkg", "env.yaml"])
}

/// `$env_root/etc/epkg` built with per-component joins (consistent separators on Windows).
#[inline]
pub fn env_root_etc_epkg(env_root: &Path) -> PathBuf {
    path_join(env_root, &["etc", "epkg"])
}

#[inline]
pub fn env_root_channel_yaml(env_root: &Path) -> PathBuf {
    env_root_etc_epkg(env_root).join("channel.yaml")
}

#[inline]
pub fn env_root_repos_d(env_root: &Path) -> PathBuf {
    env_root_etc_epkg(env_root).join("repos.d")
}

#[inline]
pub fn env_root_env_yaml(env_root: &Path) -> PathBuf {
    env_root_etc_epkg(env_root).join("env.yaml")
}

/// Get the path to an environment's configuration file
pub fn get_env_config_path(env_name: &str) -> PathBuf {
    let cfg = crate::config();
    // When env_root is set (e.g. create with --root, install --root), config lives at env_root/etc/epkg/env.yaml.
    // Check before in_env_root so that create with --root writes to the target path, not /etc.
    if !cfg.common.env_root.is_empty() && env_name == cfg.common.env_name {
        return env_root_env_yaml(Path::new(&cfg.common.env_root));
    }
    // If we're running inside an environment root (chroot/bind mount), the config is at /etc/epkg/env.yaml
    if cfg.common.in_env_root && env_name == cfg.common.env_name {
        return env_root_env_yaml(Path::new("/"));
    }
    get_env_base_path(env_name).join(env_config_relative_path())
}

pub fn find_env_base(env_name: &str) -> Option<PathBuf> {
    // Find environment based on current shared_store setting
    let base = get_env_base_path(env_name);
    if base.join(env_config_relative_path()).exists() {
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
/// Get the epkg source path for accessing assets (repos, mirrors, etc.)
///
/// This function is safe to call during config parsing because it does not
/// call `config()` which could cause a deadlock during initialization.
/// It uses `determine_shared_store()` and `compute_user_envs()` directly.
pub fn get_epkg_src_path() -> PathBuf {
    // Use determine_shared_store() to check store mode without calling config()
    let shared_store = crate::utils::determine_shared_store().unwrap_or(false);

    // Compute user_envs without calling get_env_base_path() which calls config()
    let user_envs = compute_user_envs(shared_store).unwrap_or_else(|_| {
        // Fallback: use home-based path
        let home = std::env::var("HOME").unwrap_or_default();
        PathBuf::from(home).join(".epkg").join("envs")
    });

    let user_path = path_join(&user_envs, &[SELF_ENV, "usr", "src", "epkg"]);
    if user_path.exists() {
        log::debug!("Using user's epkg source path: {:?}", user_path);
        return user_path;
    }

    let root_path = path_join(&public_envs_path(), &["root", "self", "usr", "src", "epkg"]);
    if root_path.exists() {
        log::debug!("Using root's epkg source path: {:?}", root_path);
        return root_path;
    }

    log::debug!("Neither user nor root epkg source path exists, returning user path anyway: {:?}", user_path);
    user_path
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
        #[cfg(unix)]
        if home == "/" && utils::is_running_as_root() {
            return Ok("/root".to_string());
        }

        return Ok(home);
    }

    // bare docker may have HOME=/
    #[cfg(unix)]
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
    let repodata_name = repo.repodata_name.clone();
    #[cfg(windows)]
    let repodata_name = repodata_name.replace('/', "\\");
    dirs_ref()
        .epkg_channels_cache
        .join(&repo.channel)
        .join(repodata_name)
        .join(repo.arch.clone())
}

/// Check if a shell binary is installed and executable
#[cfg(unix)]
fn is_shell_installed(shell_name: &str) -> bool {
    // Common locations where shell binaries are typically installed
    let shell_paths = [
        format!("/bin/{}", shell_name),
        format!("/usr/bin/{}", shell_name),
        format!("/usr/local/bin/{}", shell_name),
    ];

    for path in &shell_paths {
        if let Ok(_metadata) = fs::metadata(path) {
            // Check if the file is executable
            #[cfg(unix)]
            {
                if _metadata.permissions().mode() & 0o111 != 0 {
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
#[cfg(unix)]
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
#[cfg(unix)]
pub fn get_global_shell_rc() -> Result<Vec<String>> {
    let entries = [
        ("/etc/bash.bashrc", "bash"),
        ("/etc/zsh/zshrc", "zsh"),
    ];

    Ok(collect_shell_rc_paths(&entries, None))
}

/// Get per-user shell RC files under `home_dir` for installed shells only.
#[cfg(unix)]
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

/// Get per-user shell RC files (Windows stub - returns empty list)
#[cfg(not(unix))]
pub fn get_user_shell_rc(_home_dir: &Path) -> Result<Vec<String>> {
    // Windows doesn't use shell RC files in the same way
    Ok(Vec::new())
}

/// PowerShell profile paths for `epkg.ps1` integration.
///
/// On Windows, pwsh uses `%USERPROFILE%\Documents\PowerShell\Microsoft.PowerShell_profile.ps1`.
/// On non-Windows platforms we intentionally skip PowerShell profile integration.
#[cfg(windows)]
pub fn powershell_profile_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(up) = env::var("USERPROFILE") {
        paths.push(
            PathBuf::from(up)
                .join("Documents")
                .join("PowerShell")
                .join("Microsoft.PowerShell_profile.ps1"),
        );
    }
    paths
}

#[cfg(not(windows))]
pub fn powershell_profile_paths() -> Vec<PathBuf> {
    Vec::new()
}

/// Get username from environment variables.
/// On Unix, validates environment variables against real UID when running as setuid.
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
    dirs_ref().opt_epkg.join("envs")
}

/// Walk all directories in the given parent directory and call the callback for each.
///
/// This helper function walks the "bottom" directory level, calling the callback
/// for each subdirectory found with (env_path, owner_opt).
pub fn walk_bottom_dir<F>(parent_path: &Path, owner_opt: Option<&str>, callback: &mut F) -> Result<()>
where
    F: FnMut(&Path, Option<&str>) -> Result<()>,
{
    log::debug!("walk_bottom_dir: parent_path='{}'", parent_path.display());
    match fs::read_dir(parent_path) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let env_path = entry.path();
                if env_path.is_dir() {
                    log::debug!("walk_bottom_dir: found env_path='{}'", env_path.display());
                    callback(&env_path, owner_opt)?;
                }
            }
        }
        Err(e) if e.kind() == ErrorKind::PermissionDenied => {
            log::debug!("walk_bottom_dir: skipping '{}' (permission denied)", parent_path.display());
        }
        Err(e) => {
            log::debug!("walk_bottom_dir: skipping '{}' ({})", parent_path.display(), e);
        }
    }
    Ok(())
}

/// Walk all public environments under /opt/epkg/envs/*/*
/// Calls the callback for each environment found with (env_path, Some(owner)).
fn walk_public_envs<F>(callback: &mut F) -> Result<()>
where
    F: FnMut(&Path, Option<&str>) -> Result<()>,
{
    let allusers_envs_base = PathBuf::from("/opt/epkg/envs");
    match fs::read_dir(&allusers_envs_base) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let owner_path = entry.path();
                if owner_path.is_dir() {
                    let owner = owner_path.file_name()
                        .and_then(|n| n.to_str());
                    walk_bottom_dir(&owner_path, owner, callback)?;
                }
            }
        }
        Err(e) if e.kind() == ErrorKind::PermissionDenied => {
            log::debug!("walk_public_envs: skipping '{}' (permission denied)", allusers_envs_base.display());
        }
        Err(e) => {
            log::debug!("walk_public_envs: skipping '{}' ({})", allusers_envs_base.display(), e);
        }
    }
    Ok(())
}

/// Walk environments based on shared_store setting:
/// - If !shared_store: walk $HOME/.epkg/envs/* (private) and also /opt/epkg/envs/*/* (public)
/// - If  shared_store: walk /opt/epkg/envs/*/*
///
/// Calls the callback for each environment found with (env_path, owner_opt).
/// owner_opt is Some(owner) for shared_store, None for private.
///
/// **Note**: This function takes `shared_store` and `user_envs` as parameters to avoid calling
/// `config()` and `dirs_ref()` which would cause deadlock during config initialization.
pub fn walk_environments<F>(shared_store: bool, user_envs: &Path, mut callback: F) -> Result<()>
where
    F: FnMut(&Path, Option<&str>) -> Result<()>,
{
    if shared_store {
        // Walk /opt/epkg/envs/*/*
        walk_public_envs(&mut callback)?;
    } else {
        // Walk $HOME/.epkg/envs/* (private envs)
        walk_bottom_dir(user_envs, None, &mut callback)?;

        // Also walk public envs so users can see others' public environments
        walk_public_envs(&mut callback)?;
    }

    Ok(())
}

/// Search for a `.eenv` directory starting from `start_path` and moving upward.
/// Searches start_path/.eenv, then parent directories up to root or 100 levels.
/// Returns the first existing `.eenv` directory path, or None if not found.
pub fn find_nearest_dot_eenv(start_path: &Path) -> Option<PathBuf> {
    // Convert relative start path to absolute if possible
    let mut current = if start_path.is_absolute() {
        start_path.to_path_buf()
    } else {
        // Try to get absolute path by joining with current directory
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(start_path),
            Err(_) => start_path.to_path_buf(), // fallback to relative
        }
    };
    log::debug!("find_nearest_dot_eenv: start_path='{}', current='{}'",
                start_path.display(), current.display());
    let mut depth = 0;
    while depth < 10 {
        let dot_eenv = current.join(".eenv");
        log::trace!("find_nearest_dot_eenv: checking {} (depth={})", dot_eenv.display(), depth);
        if dot_eenv.exists() && dot_eenv.is_dir() {
            log::debug!("find_nearest_dot_eenv: found .eenv at {}", dot_eenv.display());
            return Some(dot_eenv);
        }
        // Move to parent directory
        if !current.pop() {
            break;
        }
        depth += 1;
    }
    log::trace!("find_nearest_dot_eenv: no .eenv found");
    None
}
