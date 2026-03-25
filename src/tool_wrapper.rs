//! Tool mirror acceleration module
//!
//! This module handles automatic injection of mirror environment variables
//! for common package managers (pip, npm, gem, go, cargo).
//!
//! Cross-platform: same install flow on Linux, macOS, and Windows. Host config links use
//! [`crate::lfs::symlink`] / [`crate::lfs::symlink_to_directory`] /
//! [`crate::lfs::symlink_to_file`] (Windows: shared libkrun policy — native symlink / junction /
//! hardlink+copy / LX reparse). Wrapper
//! install uses [`crate::utils::set_permissions_from_mode`] (no-op on Windows). Path matching
//! for newly installed binaries normalizes `\\` to `/` so detection works on Windows.
//!
//! **Windows launchers**: Native Conda/CMD does not honor shebangs. We still write one
//! extensionless script (same as Unix) for MSYS2/Git Bash, and copy a template from
//! `assets/tool/cmd_shims/` to `usr/local/bin/{tool}.cmd` (`python.cmd`, `ruby.cmd`, or
//! `posix_shell.cmd` by shebang). Templates use `%~dp0%~n0` so the paired extensionless
//! script is same basename as the `.cmd` file. We use **`.cmd`** (not `.bat`) for new shims. Do **not** duplicate as
//! `tool.sh` — the extensionless file is already the POSIX script.
//!
//! ★ 注意：文件存在性检查 ★
//! ═══════════════════════════════════════════════════════════════════════════
//!
//! 本模块涉及两类路径检查：
//!   1. Host 配置路径（如 ~/.pip/pip.conf）→ 使用 lfs::exists_on_host()
//!   2. Env 内部路径（如 env_root/usr/local/bin/）→ 使用 lfs::exists_in_env()
//!
//! 禁止直接使用 .exists()！使用 lfs 模块的显式函数。
//! ═══════════════════════════════════════════════════════════════════════════

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use color_eyre::eyre::{self, Context, Result};
use log;

use crate::lfs;
use crate::dirs;
use crate::plan::InstallationPlan;

/// Supported tools that can have mirror acceleration
const SUPPORTED_TOOLS: &[&str] = &[
    "pip", "pip3", "npm", "node", "npx", "gem", "bundle",
    "go", "cargo", "mvn",
];

/// User config file paths for each tool (on host OS)
const TOOL_CONFIG_FILES: &[(&str, &[&str])] = &[
    ("pip", &["~/.pip/pip.conf", "~/.config/pip/pip.conf"]),
    ("npm", &["~/.npmrc"]),
    ("node", &[]), // Node uses env vars, not config files
    ("npx", &[]),  // Inherits from npm
    ("gem", &["~/.gemrc"]),
    ("bundle", &["~/.bundle/config"]),
    ("go",  &[]), // Go uses env vars, not config files
    ("cargo", &["~/.cargo/config.toml", "~/.cargo/config"]),
    ("mvn", &["~/.m2/settings.xml"]),
];

/// Environment variables to check for each tool
const TOOL_ENV_VARS: &[(&str, &[&str])] = &[
    ("pip", &["PIP_INDEX_URL", "PIP_INDEX_HOST"]),
    ("npm", &["npm_config_registry", "NPM_CONFIG_REGISTRY"]),
    ("node", &["npm_config_registry", "NODEJS_ORG_MIRROR"]),
    ("npx", &["npm_config_registry"]),
    ("gem", &["BUNDLE_MIRROR__HTTPS://RUBYGEMS__ORG/"]),
    ("bundle", &["BUNDLE_MIRROR__HTTPS://RUBYGEMS__ORG/", "BUNDLE_RUBYGEMS__ORG_MIRROR"]),
    ("go",  &["GOPROXY"]),
    ("cargo", &["RUSTUP_DIST_SERVER", "CARGO_REGISTRIES_CRATES_INDEX"]),
    ("mvn", &["MAVEN_CENTRAL_MIRROR", "MAVEN_REPO_LOCAL"]),
];

/// Country to region mapping for mirror selection
/// EU covers many European countries
static COUNTRY_TO_REGION: LazyLock<std::collections::HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    let mut map = std::collections::HashMap::new();

    // China
    map.insert("CN", "cn");

    // EU countries
    let eu_countries = [
        "AT", "BE", "BG", "HR", "CY", "CZ", "DK", "EE", "FI", "FR",
        "DE", "GR", "HU", "IE", "IT", "LV", "LT", "LU", "MT", "NL",
        "PL", "PT", "RO", "SK", "SI", "ES", "SE",
    ];
    for cc in eu_countries {
        map.insert(cc, "eu");
    }

    // US
    map.insert("US", "us");

    // UK (not in EU but close)
    map.insert("GB", "eu");

    // Other major regions
    map.insert("JP", "us"); // Japan uses US mirrors typically
    map.insert("KR", "us"); // Korea
    map.insert("AU", "us"); // Australia
    map.insert("CA", "us"); // Canada
    map.insert("NZ", "us"); // New Zealand

    map
});

/// Map country code to region code for mirror selection
pub fn country_to_region(country_code: &str) -> Option<&'static str> {
    COUNTRY_TO_REGION.get(country_code).copied()
}

/// Get the region code for current location
pub fn get_region_code() -> Option<String> {
    crate::location::get_country_code()
        .ok()
        .and_then(|cc| country_to_region(&cc).map(|s| s.to_string()))
}

/// Get the tool config directory path (~/.config/epkg/tool)
fn get_tool_config_dir() -> Result<PathBuf> {
    let home = dirs::get_home()?;
    Ok(crate::dirs::path_join(&PathBuf::from(home), &[".config", "epkg", "tool"]))
}

/// Get the env_vars directory path
fn get_env_vars_dir() -> Result<PathBuf> {
    let epkg_src = dirs::get_epkg_src_path();
    Ok(crate::dirs::path_join(&epkg_src, &["assets", "tool", "env_vars"]))
}

/// Setup tool config symlinks on `epkg self install`
/// Creates:
/// - ~/.config/epkg/tool/env_vars -> $EPKG_SRC/assets/tool/env_vars
/// - ~/.config/epkg/tool/my_region -> cn/eu/us/etc.
pub fn setup_tool_config_symlinks() -> Result<()> {
    let config_dir = get_tool_config_dir()?;
    lfs::create_dir_all(&config_dir)?;

    // Create env_vars symlink
    let env_vars_link = config_dir.join("env_vars");
    let env_vars_target = get_env_vars_dir()?;

    if lfs::exists_on_host(&env_vars_target) {
        // Use exists_no_follow to check if link file itself exists (including broken symlinks)
        if lfs::exists_no_follow(&env_vars_link) {
            lfs::remove_file(&env_vars_link)?;
        }
        // Use symlink_dir_for_native because env_vars_target is a directory
        // and these symlinks need to be readable from Windows host
        lfs::symlink_dir_for_native(&env_vars_target, &env_vars_link)?;
        log::info!("Created symlink: {} -> {}", env_vars_link.display(), env_vars_target.display());
    }

    // Create my_region symlink based on region
    let iploc_link = config_dir.join("my_region");

    // Remove existing link - use exists_no_follow to catch broken symlinks too
    if lfs::exists_no_follow(&iploc_link) {
        lfs::remove_file(&iploc_link)?;
    }

    // Get region and create symlink
    if let Some(region) = get_region_code() {
        let iploc_target = config_dir.join("env_vars").join(&region);
        if lfs::exists_on_host(&iploc_target) {
            // Use symlink_dir_for_native because iploc_target is a directory
            lfs::symlink_dir_for_native(&iploc_target, &iploc_link)?;
            log::info!("Created my_region symlink: {} -> {} (region: {})",
                      iploc_link.display(), iploc_target.display(), region);
        } else {
            log::debug!("Region config dir {} does not exist, skipping my_region symlink", iploc_target.display());
        }
    } else {
        log::debug!("Could not determine region, skipping my_region symlink");
    }

    Ok(())
}

/// Check if any env var for the tool is already set
fn check_env_var_set(tool: &str) -> bool {
    for (t, vars) in TOOL_ENV_VARS {
        if *t == tool {
            for var in *vars {
                if std::env::var(var).is_ok() {
                    log::debug!("Env var {} is already set for tool {}", var, tool);
                    return true;
                }
            }
        }
    }
    false
}

/// Expand ~ in path
fn expand_tilde(path: &str) -> PathBuf {
    if path.starts_with("~/") {
        if let Ok(home) = dirs::get_home() {
            let rest = &path[2..];
            #[cfg(windows)]
            {
                let rest = rest.replace('/', "\\");
                return PathBuf::from(home).join(&rest);
            }
            #[cfg(not(windows))]
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

/// Check if user config file exists on host OS
/// Uses exists_on_host since we're checking regular config files on host
fn check_user_config_exists(tool: &str) -> bool {
    for (t, paths) in TOOL_CONFIG_FILES {
        if *t == tool {
            for path in *paths {
                let expanded = expand_tilde(path);
                if lfs::exists_on_host(&expanded) {
                    log::debug!("User config file exists for tool {}: {}", tool, expanded.display());
                    return true;
                }
            }
        }
    }
    false
}

/// Check if wrapper should be created for tool
/// Note: wrapper_path is in env_root, checked from host context
fn should_create_wrapper(tool: &str, env_root: &Path) -> bool {
    // Check if tool is supported
    if !SUPPORTED_TOOLS.contains(&tool) {
        return false;
    }

    // Check if env var is already set
    if check_env_var_set(tool) {
        log::debug!("Skipping wrapper for {}: env var already set", tool);
        return false;
    }

    // Check if user config exists
    if check_user_config_exists(tool) {
        log::debug!("Skipping wrapper for {}: user config exists", tool);
        return false;
    }

    // Check if wrapper already exists
    // Use exists_in_env because wrapper_path is in env_root and may be a broken symlink
    // (symlink target exists in guest namespace but not on host)
    let wrapper_path = crate::dirs::path_join(env_root, &["usr", "local", "bin"]).join(tool);
    if lfs::exists_in_env(&wrapper_path) {
        log::debug!("Wrapper already exists for {}: {}", tool, wrapper_path.display());
        return false;
    }

    true
}

/// Detect installed tools from plan's new files
fn detect_installed_tools(plan: &InstallationPlan) -> Vec<String> {
    let mut tools = Vec::new();

    // Default paths checked for all tools (usr/bin and bin are standard)
    const DEFAULT_PATHS: &[&str] = &["usr/bin/{}", "bin/{}"];

    // Alternative (non-standard) paths for specific tools
    // Format: tool_name -> &[alternative_paths]
    const TOOL_ALT_PATHS: &[(&str, &[&str])] = &[
        // Go language (Alpine: usr/bin/go, some distros: usr/lib/go/bin/go or usr/lib/golang/bin/go)
        ("go",    &["usr/lib/go/bin/go", "usr/lib/golang/bin/go"]),
        // Rust language
        ("cargo", &["usr/lib/rust/bin/cargo"]),
        // Python
        ("pip",   &["usr/lib/python3/bin/pip"]),
        ("pip3",  &["usr/lib/python3/bin/pip3"]),
        // Node.js
        ("npm",   &["usr/share/nodejs/bin/npm"]),
        ("node",  &["usr/lib/nodejs/bin/node"]),
        ("npx",   &["usr/share/nodejs/bin/npx"]),
        // Ruby
        ("gem",   &["usr/lib/ruby/bin/gem"]),
        ("bundle", &["usr/lib/ruby/bin/bundle"]),
        // Maven
        ("mvn",   &["usr/share/maven/bin/mvn"]),
    ];

    for file in &plan.batch.new_files {
        // Normalize separators so matching works on Windows (usr\bin\pip vs usr/bin/pip)
        let file_str = file.to_string_lossy().replace('\\', "/");
        log::debug!("Checking new_file: {}", file_str);

        // Check if file matches any tool's alternative paths
        for (tool, alt_paths) in TOOL_ALT_PATHS {
            for path in *alt_paths {
                if file_str == *path {
                    log::debug!("Detected tool: {} from path: {}", tool, path);
                    if !tools.contains(&tool.to_string()) {
                        tools.push(tool.to_string());
                    }
                    break;
                }
            }
        }

        // Check if file matches default paths (usr/bin/, bin/) for any supported tool
        for tool in SUPPORTED_TOOLS {
            for path_template in DEFAULT_PATHS {
                let expected_path = path_template.replace("{}", tool);
                if file_str == expected_path {
                    log::debug!("Detected tool: {} from default path: {}", tool, expected_path);
                    if !tools.contains(&tool.to_string()) {
                        tools.push(tool.to_string());
                    }
                    break;
                }
            }
        }
    }

    log::debug!("Detected tools: {:?}", tools);
    tools
}

/// Get wrapper script content for a tool
/// Note: Filesystem symlinks (e.g., node->npm) are auto-followed by read_to_string()
fn get_wrapper_content(tool: &str) -> Result<String> {
    let epkg_src = dirs::get_epkg_src_path();
    let base = crate::dirs::path_join(&epkg_src, &["assets", "tool", "wrappers"]);
    let wrapper_path = base.join(tool);

    if lfs::exists_on_host(&wrapper_path) {
        let content = std::fs::read_to_string(&wrapper_path)
            .with_context(|| format!("Failed to read wrapper script: {}", wrapper_path.display()))?;
        return Ok(content);
    }

    let generic = base.join("shell-wrapper.sh");
    if lfs::exists_on_host(&generic) {
        let content = std::fs::read_to_string(&generic)
            .with_context(|| format!("Failed to read generic shell wrapper: {}", generic.display()))?;
        return Ok(content);
    }

    Err(eyre::eyre!("No wrapper script found for tool: {}", tool))
}

/// How to invoke the extensionless wrapper from a `tool.cmd` shim on Windows.
#[cfg(windows)]
#[derive(Clone, Copy)]
enum WindowsShimKind {
    Python,
    Ruby,
    PosixShell,
}

#[cfg(windows)]
fn detect_windows_shim_kind(content: &str) -> Option<WindowsShimKind> {
    let shebang = content.lines().next()?.trim();
    if shebang.contains("python") {
        return Some(WindowsShimKind::Python);
    }
    if shebang.contains("ruby") {
        return Some(WindowsShimKind::Ruby);
    }
    if shebang.contains("/bin/sh") || shebang.contains("/bin/bash") || shebang.contains("/usr/bin/env sh") {
        return Some(WindowsShimKind::PosixShell);
    }
    None
}

#[cfg(windows)]
fn cmd_shim_template_filename(kind: WindowsShimKind) -> &'static str {
    match kind {
        WindowsShimKind::Python => "python.cmd",
        WindowsShimKind::Ruby => "ruby.cmd",
        WindowsShimKind::PosixShell => "posix_shell.cmd",
    }
}

#[cfg(windows)]
fn load_windows_cmd_shim_template(kind: WindowsShimKind) -> Result<String> {
    let epkg_src = dirs::get_epkg_src_path();
    let path = crate::dirs::path_join(
        &epkg_src,
        &["assets", "tool", "cmd_shims", cmd_shim_template_filename(kind)],
    );
    if !lfs::exists_on_host(&path) {
        return Err(eyre::eyre!(
            "Windows CMD shim template missing: {} (expected under EPKG_SRC assets)",
            path.display()
        ));
    }
    std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read CMD shim template {}", path.display()))
}

#[cfg(windows)]
fn write_windows_cmd_shim(wrapper_dir: &Path, tool: &str, script_content: &str) -> Result<()> {
    let Some(kind) = detect_windows_shim_kind(script_content) else {
        log::debug!(
            "No Windows .cmd shim for tool {} (unrecognized shebang); MSYS2/Git Bash can still run the extensionless script",
            tool
        );
        return Ok(());
    };
    let body = load_windows_cmd_shim_template(kind)?;
    let cmd_path = wrapper_dir.join(format!("{}.cmd", tool));
    lfs::write(&cmd_path, body.as_bytes())
        .with_context(|| format!("Failed to write {}", cmd_path.display()))?;
    log::info!("Created Windows CMD launcher: {}", cmd_path.display());
    Ok(())
}

/// Create wrapper script for a tool
fn create_tool_wrapper(tool: &str, env_root: &Path) -> Result<()> {
    let wrapper_dir = crate::dirs::path_join(env_root, &["usr", "local", "bin"]);
    lfs::create_dir_all(&wrapper_dir)?;

    let wrapper_path = wrapper_dir.join(tool);
    let content = get_wrapper_content(tool)?;

    // Write wrapper script
    lfs::write(&wrapper_path, &content)?;

    // Unix: chmod; Windows: no-op (see utils::set_permissions_from_mode)
    crate::utils::set_permissions_from_mode(&wrapper_path, 0o755)
        .with_context(|| format!("Failed to set permissions for {}", wrapper_path.display()))?;

    #[cfg(windows)]
    write_windows_cmd_shim(&wrapper_dir, tool, &content)?;

    log::info!("Created tool wrapper: {}", wrapper_path.display());
    Ok(())
}

/// Remove wrapper script for a tool
#[allow(dead_code)]
fn remove_tool_wrapper(tool: &str, env_root: &Path) -> Result<()> {
    let wrapper_dir = crate::dirs::path_join(env_root, &["usr", "local", "bin"]);
    let wrapper_path = wrapper_dir.join(tool);

    // Use exists_in_env because wrapper_path is in env_root and may be a broken symlink
    if lfs::exists_in_env(&wrapper_path) {
        lfs::remove_file(&wrapper_path)?;
        log::info!("Removed tool wrapper: {}", wrapper_path.display());
    }

    #[cfg(windows)]
    {
        let cmd_path = wrapper_dir.join(format!("{}.cmd", tool));
        if lfs::exists_in_env(&cmd_path) {
            lfs::remove_file(&cmd_path)?;
            log::info!("Removed Windows CMD launcher: {}", cmd_path.display());
        }
    }

    Ok(())
}

/// Setup tool wrappers for newly installed tools
/// Called from execute_installations() after link_packages()
pub fn setup_tool_wrappers(plan: &InstallationPlan) -> Result<()> {
    log::debug!("setup_tool_wrappers: checking for newly installed tools");
    log::debug!("setup_tool_wrappers: new_files count = {}", plan.batch.new_files.len());
    for f in &plan.batch.new_files {
        log::debug!("setup_tool_wrappers: new_file = {}", f.display());
    }
    let env_root = PathBuf::from(&plan.env_root);

    // Detect newly installed tools
    let tools = detect_installed_tools(plan);

    if tools.is_empty() {
        log::debug!("setup_tool_wrappers: no supported tools detected in new files");
        return Ok(());
    }

    log::debug!("Detected newly installed tools: {:?}", tools);

    for tool in &tools {
        if should_create_wrapper(tool, &env_root) {
            create_tool_wrapper(tool, &env_root)?;
        }
    }

    Ok(())
}

/// Remove tool wrappers when tools are removed
#[allow(dead_code)]
pub fn remove_tool_wrappers(_plan: &InstallationPlan) -> Result<()> {
    // TODO: Implement when removal tracking is available
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_country_to_region() {
        assert_eq!(country_to_region("CN"), Some("cn"));
        assert_eq!(country_to_region("US"), Some("us"));
        assert_eq!(country_to_region("DE"), Some("eu"));
        assert_eq!(country_to_region("FR"), Some("eu"));
        assert_eq!(country_to_region("GB"), Some("eu"));
        assert_eq!(country_to_region("JP"), Some("us"));
        assert_eq!(country_to_region("XX"), None);
    }
}
