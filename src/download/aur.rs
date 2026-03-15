// ============================================================================
// DOWNLOAD AUR - Arch User Repository Download Handling
//
// This module provides specialized handling for downloading packages from the
// Arch User Repository (AUR). Unlike regular HTTP downloads, AUR packages are
// downloaded using git clone/fetch operations to retrieve the source snapshots.
//
// Key Features:
// - AUR URL pattern recognition and validation
// - Git-based download using clone/fetch operations
// - Package name extraction from AUR snapshot URLs
// - Integration with the broader download system
// ============================================================================

#[cfg(unix)]
use std::path::PathBuf;

#[cfg(unix)]
use color_eyre::eyre::eyre;
#[cfg(unix)]
use color_eyre::eyre::{Result, WrapErr};

#[cfg(unix)]
use crate::dirs;
#[cfg(unix)]
use crate::run;
#[cfg(unix)]
use crate::utils;

/// AUR base URL for package downloads
pub const AUR_BASE_URL: &str = "https://aur.archlinux.org/cgit/aur.git/snapshot/";
/// AUR domain for git operations
#[cfg(unix)]
pub const AUR_DOMAIN: &str = "aur.archlinux.org";

/// Handles AUR package downloads using git clone/fetch
///
/// Returns Ok(()) if URL is AUR and git download was successful.
/// Returns Err if URL is not AUR or git is not available (caller should fall back to regular download).
#[cfg(unix)]
pub fn handle_aur_git_download(
    url: &str,
) -> Result<()> {
    // Check if URL matches AUR pattern
    if !url.starts_with(AUR_BASE_URL) {
        return Err(eyre!("Not an AUR URL"));
    }

    // Extract package name from URL: https://aur.archlinux.org/cgit/aur.git/snapshot/{package}.tar.gz
    let pkgbase = url
        .strip_prefix(AUR_BASE_URL)
        .ok_or_else(|| eyre!("Invalid AUR URL format: {}", url))?
        .strip_prefix("/")
        .ok_or_else(|| eyre!("Invalid AUR URL format: {}", url))?
        .strip_suffix(".tar.gz")
        .ok_or_else(|| eyre!("AUR URL should end with .tar.gz: {}", url))?;

    // Check if git is available and determine which one to use
    let (git_path, is_host_git) = find_git_command()?;

    log::info!("Downloading AUR package {} using git", pkgbase);

    // Place git directory in build directory (same location as extracted build dir)
    // This matches the layout: ~/.cache/epkg/aur_builds/{pkgbase}
    let build_dir = dirs().user_aur_builds.clone();
    let clone_dir = build_dir.join(pkgbase);

    // Create build directory if needed
    std::fs::create_dir_all(&build_dir)
        .with_context(|| format!("Failed to create build directory: {}", build_dir.display()))?;

    // Clone or fetch the repository
    clone_or_fetch_aur_repo(&git_path, pkgbase, &clone_dir, is_host_git)?;

    log::info!("Successfully downloaded AUR package {} to git directory {}", pkgbase, clone_dir.display());
    Ok(())
}

/// Find git command, preferring host OS over environment
/// Returns (git_path, is_host_git)
#[cfg(unix)]
pub fn find_git_command() -> Result<(PathBuf, bool)> {
    // Try host OS first
    if let Some(git_path) = utils::find_command_in_paths("git") {
        return Ok((git_path, true));
    }

    // Fall back to environment
    let env_root = dirs::get_default_env_root()?;
    let git_path = run::find_command_in_env_path("git", &env_root)
        .map_err(|_| eyre!("git command not found in host OS or environment"))?;

    Ok((git_path, false))
}

/// Clone or fetch AUR git repository
#[cfg(unix)]
pub fn clone_or_fetch_aur_repo(
    git_path: &std::path::Path,
    pkgbase: &str,
    clone_dir: &std::path::Path,
    is_host_git: bool,
) -> Result<()> {
    let git_url = format!("https://{}/{}.git", AUR_DOMAIN, pkgbase);
    // If the target dir exists but is not a git repo (e.g., leftover from a failed extract),
    // clean it up so clone can succeed.
    if clone_dir.exists() && !clone_dir.join(".git").exists() {
        log::warn!(
            "Cleaning non-git directory before cloning AUR repo: {}",
            clone_dir.display()
        );
        std::fs::remove_dir_all(clone_dir)
            .with_context(|| format!("Failed to remove non-git dir {}", clone_dir.display()))?;
    }
    let repo_exists = clone_dir.join(".git").exists();

    let env_root = dirs::get_default_env_root().unwrap_or_else(|_| PathBuf::from("/"));
    let base_run_options = run::RunOptions {
        command: git_path.to_string_lossy().to_string(),
        skip_namespace_isolation: is_host_git,
        timeout: 300, // 5 minute timeout
        ..Default::default()
    };

    if repo_exists {
        log::info!("Git repository exists at {}, fetching updates", clone_dir.display());
        // Fetch updates using -C to change directory
        let fetch_options = run::RunOptions {
            args: vec![
                "-C".to_string(),
                clone_dir.to_string_lossy().to_string(),
                "fetch".to_string(),
                "origin".to_string(),
            ],
            ..base_run_options.clone()
        };
        run::fork_and_execute(&env_root, &fetch_options)
            .with_context(|| format!("Failed to fetch git repository: {}", git_url))?;

        // Checkout HEAD using -C to change directory
        let checkout_options = run::RunOptions {
            args: vec![
                "-C".to_string(),
                clone_dir.to_string_lossy().to_string(),
                "checkout".to_string(),
                "HEAD".to_string(),
            ],
            ..base_run_options.clone()
        };
        if let Err(e) = run::fork_and_execute(&env_root, &checkout_options) {
            log::warn!(
                "Checkout HEAD failed in {}: {}. Trying git reset --hard + checkout.",
                clone_dir.display(),
                e
            );
            let reset_options = run::RunOptions {
                args: vec![
                    "-C".to_string(),
                    clone_dir.to_string_lossy().to_string(),
                    "reset".to_string(),
                    "--hard".to_string(),
                ],
                ..base_run_options.clone()
            };
            run::fork_and_execute(&env_root, &reset_options)
                .with_context(|| format!("Failed to reset repository: {}", clone_dir.display()))?;
            run::fork_and_execute(&env_root, &checkout_options)
                .with_context(|| format!("Failed to checkout HEAD in repository: {}", clone_dir.display()))?;
        }
    } else {
        log::info!("Cloning git repository from {}", git_url);
        // Clone repository - run from parent directory
        let clone_parent = clone_dir.parent()
            .ok_or_else(|| eyre!("clone_dir has no parent: {}", clone_dir.display()))?;
        let clone_options = run::RunOptions {
            args: vec![
                "-C".to_string(),
                clone_parent.to_string_lossy().to_string(),
                "clone".to_string(),
                "-q".to_string(),
                "-c".to_string(),
                "init.defaultBranch=master".to_string(),
                git_url.clone(),
                clone_dir.to_string_lossy().to_string(),
            ],
            ..base_run_options
        };
        run::fork_and_execute(&env_root, &clone_options)
            .with_context(|| format!("Failed to clone git repository: {}", git_url))?;
    }

    Ok(())
}
