//! Brew post_install execution support
//!
//! Homebrew formulae often define `post_install` method that runs after package
//! installation to perform setup tasks like creating directories or running
//! setup commands. This module provides minimal support for executing these
//! without requiring the full Homebrew Library.

use std::fs;
use std::path::Path;
use std::process::Command;
use color_eyre::Result;
use color_eyre::eyre::WrapErr;

/// Check if a formula defines a post_install method.
///
/// Simple text scan - no Ruby parsing needed.
/// Looks for "def post_install" not inside a comment.
pub fn detect_post_install(formula_content: &str) -> bool {
    formula_content.lines().any(|line| {
        let trimmed = line.trim();
        trimmed.starts_with("def post_install") ||
        trimmed.starts_with("def post_install(") ||
        // Also match method definitions with block syntax
        (trimmed.contains("post_install") && trimmed.starts_with("def "))
    })
}

/// Run post_install for a brew package.
///
/// Creates minimal Ruby stub environment and executes the formula's
/// post_install method using portable-ruby.
///
/// # Arguments
/// * `env_root` - Environment root directory
/// * `store_dir` - Store directory path (contains info/brew/.brew/)
/// * `pkgname` - Package name
/// * `version` - Package version (from Cellar, without bottle revision)
pub fn run_post_install(env_root: &Path, store_dir: &Path, pkgname: &str, version: &str) -> Result<()> {
    // Formula file is at store_dir/info/brew/.brew/pkgname.rb
    let formula_path = store_dir.join("info/brew/.brew").join(format!("{}.rb", pkgname));
    if !formula_path.exists() {
        log::debug!("Formula not found at {}, skipping post_install", formula_path.display());
        return Ok(());
    }

    // Check if formula has post_install
    let formula_content = fs::read_to_string(&formula_path)
        .wrap_err_with(|| format!("Failed to read formula: {}", formula_path.display()))?;

    if !detect_post_install(&formula_content) {
        log::debug!("{} has no post_install method", pkgname);
        return Ok(());
    }

    log::info!("Running post_install for {}", pkgname);

    // Ruby executable path
    let ruby_path = env_root.join("Homebrew/Library/Homebrew/vendor/portable-ruby/current/bin/ruby");
    if !ruby_path.exists() {
        log::warn!("portable-ruby not found, skipping post_install");
        return Ok(());
    }

    // Assets directory path (from epkg source directory)
    let assets_dir = crate::dirs::path_join(
        crate::dirs::get_epkg_src_path().as_path(),
        &["assets", "homebrew"]
    );

    let stub_path = assets_dir.join("formula_stub.rb");
    let runner_path = assets_dir.join("postinstall_runner.rb");

    if !stub_path.exists() || !runner_path.exists() {
        log::warn!("Ruby stub/runner not found at {}, skipping post_install", assets_dir.display());
        return Ok(());
    }

    // Execute with portable-ruby
    // Arguments: runner.rb <stub_path> <formula_path> <pkgname> <version>
    //
    // For Linux brew, use HOMEBREW_SHORT_PREFIX (/home/linuxbrew/.LB) as HOMEBREW_PREFIX.
    // This ensures gcc specs file and other post_install outputs reference namespace-compatible
    // paths. The SHORT_PREFIX is also used for placeholder replacement in brew_pkg.rs.
    #[cfg(target_os = "linux")]
    let homebrew_prefix = crate::brew_pkg::HOMEBREW_SHORT_PREFIX;

    #[cfg(target_os = "macos")]
    let homebrew_prefix = env_root.display().to_string();

    let status = Command::new(&ruby_path)
        .arg("--disable=gems,rubyopt")
        .arg(&runner_path)
        .arg(&stub_path)
        .arg(&formula_path)
        .arg(pkgname)
        .arg(version)
        .env("HOMEBREW_PREFIX", homebrew_prefix)
        .env("HOMEBREW_CELLAR", env_root.join("Cellar"))
        .env("HOMEBREW_LIBRARY", env_root.join("Homebrew/Library"))
        .env("TMPDIR", env_root.join("tmp"))
        .env("HOMEBREW_TEMP", env_root.join("tmp"))
        .env("PATH", format!("{}:/usr/bin:/bin", env_root.join("bin").display()))
        .current_dir(env_root)
        .status();

    match status {
        Ok(s) if s.success() => {
            log::info!("post_install for {} completed successfully", pkgname);
            Ok(())
        }
        Ok(s) => {
            log::warn!("post_install for {} failed with exit code {}", pkgname, s.code().unwrap_or(-1));
            // Don't fail the install - post_install errors are non-critical
            Ok(())
        }
        Err(e) => {
            log::warn!("Failed to execute post_install for {}: {}", pkgname, e);
            // Don't fail the install
            Ok(())
        }
    }
}