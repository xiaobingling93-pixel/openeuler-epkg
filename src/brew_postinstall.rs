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
use color_eyre::eyre::{self, WrapErr};
use crate::lfs;

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
/// * `pkgname` - Package name
/// * `version` - Package version (from Cellar, without bottle revision)
pub fn run_post_install(env_root: &Path, pkgname: &str, version: &str) -> Result<()> {
    // Find formula file in store
    let formula_path = find_formula_in_store(env_root, pkgname)
        .wrap_err_with(|| format!("Cannot find formula for {}", pkgname))?;

    // Check if formula has post_install
    let formula_content = fs::read_to_string(&formula_path)
        .wrap_err_with(|| format!("Failed to read formula: {}", formula_path.display()))?;

    if !detect_post_install(&formula_content) {
        log::debug!("{} has no post_install method", pkgname);
        return Ok(());
    }

    log::info!("Running post_install for {}", pkgname);

    // Ensure stub directory exists
    let stub_dir = env_root.join("Homebrew/Library/Homebrew");
    if !stub_dir.exists() {
        lfs::create_dir_all(&stub_dir)?;
    }

    // Copy or create stub file
    let stub_path = stub_dir.join("epkg_formula_stub.rb");
    if !stub_path.exists() {
        create_formula_stub(&stub_path)?;
    }

    // Ruby executable path
    let ruby_path = env_root.join("Homebrew/Library/Homebrew/vendor/portable-ruby/current/bin/ruby");
    if !ruby_path.exists() {
        log::warn!("portable-ruby not found, skipping post_install");
        return Ok(());
    }

    // Build Ruby script
    // Use load instead of require to avoid caching issues
    // String concatenation to avoid format! macro parsing Ruby's #{...}
    let script = [
        "begin",
        &format!("  load '{}'", stub_path.display()),
        &format!("  load '{}'", formula_path.display()),
        "",
        "  # Find the formula class (last defined class inheriting from Formula)",
        "  formula_class = ObjectSpace.each_object(Class).select { |c| c < Formula && c != Formula }.last",
        "",
        "  if formula_class",
        &format!("    formula = formula_class.new('{}', '{}')", pkgname, version),
        "    if formula.method(:post_install).owner != Formula",
        &format!("      puts \"==> Running post_install for {}\"", pkgname),
        "      formula.post_install",
        "      puts \"==> post_install completed\"",
        "    end",
        "  else",
        "    puts \"Warning: No Formula class found\"",
        "  end",
        "rescue Exception => e",
        "  puts \"Error: #{e.class}: #{e.message}\"",
        "  puts e.backtrace.first(5).join(\"\\n\")",
        "  exit 1",
        "end",
    ].join("\n");

    // Execute with portable-ruby
    let status = Command::new(&ruby_path)
        .args(["--disable=gems,rubyopt", "-e", &script])
        .env("HOMEBREW_PREFIX", env_root)
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

/// Find formula file for a package in the store.
///
/// Formula is stored at info/brew/.brew/pkgname.rb
fn find_formula_in_store(env_root: &Path, pkgname: &str) -> Result<PathBuf> {
    // Formula could be in multiple store directories
    // Look for the most recent one
    let store_base = env_root.parent()
        .unwrap_or(env_root)
        .join("store");

    if !store_base.exists() {
        return Err(eyre::eyre!("Store directory not found"));
    }

    // Search for package in store
    for entry in fs::read_dir(&store_base)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();

        // Match pkgname in store directory name
        // Format: hash__pkgname__version__arch
        if name.contains(&format!("__{}__", pkgname)) || name.ends_with(&format!("__{}", pkgname)) {
            let formula_path = entry.path().join("info/brew/.brew").join(format!("{}.rb", pkgname));
            if formula_path.exists() {
                return Ok(formula_path);
            }
        }
    }

    Err(eyre::eyre!("Formula not found for {}", pkgname))
}

/// Create the minimal Formula stub file.
///
/// This provides the essential Formula class and helper methods
/// without requiring the full Homebrew Library.
fn create_formula_stub(stub_path: &Path) -> Result<()> {
    // Load stub from assets directory (embedded at compile time)
    let stub_content = include_str!("../assets/homebrew/epkg_formula_stub.rb");

    fs::write(stub_path, stub_content)
        .wrap_err_with(|| format!("Failed to write stub: {}", stub_path.display()))?;

    log::trace!("Created Formula stub at {}", stub_path.display());
    Ok(())
}

use std::path::PathBuf;