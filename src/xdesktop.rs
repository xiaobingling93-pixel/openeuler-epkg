//! Desktop integration for epkg
//!
//! This module handles symlinking desktop files, icons, and other desktop integration
//! components from the package environment to the user's home directory.
//!
//!  Core Problem
//!
//!  Distro package hooks (like update-desktop-database, gtk-update-icon-cache) run inside the
//!  environment namespace where / = $env_root. They'll write to $env_root/etc/ and $env_root/usr/share/,
//!  which isn't visible to the host X11 desktop. epkg should avoid modifying host /etc/ but can modify $HOME.
//!
//!  Required Desktop Integration Work
//!
//!  1. Desktop File Registration
//!
//!  - Source: $env_root/usr/share/applications/*.desktop
//!  - Target: ~/.local/share/applications/
//!  - Adjustments: Modify Exec field to point to ebin wrapper ($env_root/usr/ebin/<app>)
//!  - Update: Run update-desktop-database ~/.local/share/applications
//!
//!  2. Icon Theme Integration
//!
//!  - Source: $env_root/usr/share/icons/*/
//!  - Target: ~/.local/share/icons/hicolor/ (symlink entire directories)
//!  - Update: Run gtk-update-icon-cache ~/.local/share/icons/hicolor
//!
//!  3. MIME Type Registration
//!
//!  - Source: $env_root/usr/share/mime/packages/*.xml
//!  - Target: ~/.local/share/mime/packages/
//!  - Update: Run update-mime-database ~/.local/share/mime
//!
//!  4. Font Registration
//!
//!  - Source: $env_root/usr/share/fonts/*/
//!  - Target: ~/.local/share/fonts/
//!  - Update: Run fc-cache -f ~/.local/share/fonts
//!
//!  5. DBus Services
//!
//!  - Source: $env_root/usr/share/dbus-1/services/*.service
//!  - Target: ~/.local/share/dbus-1/services/
//!
//!  6. Autostart Entries
//!
//!  - Source: $env_root/etc/xdg/autostart/*.desktop
//!  - Target: ~/.config/autostart/ (adjust Exec like regular desktop files)
//!
//! Note: we currently check store_fs_dir per expose/unexposed package, which does not exist
//! in LinkType=Move case.

use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use color_eyre::Result;
use color_eyre::eyre::{self, WrapErr};
use glob;
use log;


/// Adjust Exec line to use ebin wrapper
fn adjust_exec_line(exec_line: &str, env_root: &Path) -> Result<String> {
    // Format: Exec=command %f
    // or: Exec=/usr/bin/command %f
    let (prefix, rest) = exec_line.split_once('=')
        .ok_or_else(|| eyre::eyre!("Invalid Exec line: {}", exec_line))?;

    // Find first whitespace to separate command from arguments
    let command_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let command = &rest[..command_end];
    let args = &rest[command_end..];

    // Extract binary name (strip path)
    let binary_name = Path::new(command)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| command.to_string());

    // Check if ebin wrapper exists
    let ebin_wrapper = env_root.join("usr/ebin").join(&binary_name);
    let new_command = if ebin_wrapper.exists() {
        ebin_wrapper.to_string_lossy().to_string()
    } else {
        // If no ebin wrapper, maybe it's a script or binary in package's bin/
        // Try to find it in package's filesystem
        log::debug!("No ebin wrapper found for {}, using original command", binary_name);
        command.to_string()
    };

    // Reconstruct Exec line
    Ok(format!("{}={}{}", prefix, new_command, args))
}

/// Adjust Icon line to convert absolute paths to icon names
fn adjust_icon_line(icon_line: &str) -> Result<String> {
    let (prefix, value) = icon_line.split_once('=')
        .ok_or_else(|| eyre::eyre!("Invalid Icon line: {}", icon_line))?;

    // If value is an absolute path under /usr/share/icons/, convert to icon name
    if value.starts_with("/usr/share/icons/") {
        let path = Path::new(value);
        if let Some(stem) = path.file_stem() {
            // Return just the icon name (filename without extension)
            return Ok(format!("{}={}", prefix, stem.to_string_lossy()));
        }
    }

    // Otherwise keep original
    Ok(icon_line.to_string())
}

/// Adjust desktop file content for environment isolation
///
/// Modifies the Exec line to point to ebin wrapper and adjusts Icon paths if needed.
fn adjust_desktop_file(desktop_path: &Path, env_root: &Path) -> Result<String> {
    let file = fs::File::open(desktop_path)
        .with_context(|| format!("Failed to open desktop file {}", desktop_path.display()))?;

    let reader = BufReader::new(file);
    let mut output = String::new();

    for line in reader.lines() {
        let line = line?;

        if line.starts_with("Exec=") {
            // Parse Exec line and adjust path
            let adjusted = adjust_exec_line(&line, env_root)?;
            output.push_str(&adjusted);
            output.push('\n');
        } else if line.starts_with("Icon=") {
            // Adjust icon path if it's a relative path to /usr/share/icons
            let adjusted = adjust_icon_line(&line)?;
            output.push_str(&adjusted);
            output.push('\n');
        } else {
            output.push_str(&line);
            output.push('\n');
        }
    }

    Ok(output)
}

/// Generic function to transform and copy desktop files from source to destination
///
/// # Arguments
/// * `src_dir` - Source directory to read desktop files from
/// * `dst_dir` - Destination directory to write processed files to
/// * `env_root` - Environment root path for adjusting desktop file content
/// * `file_extension_filter` - Optional file extension filter (e.g., ".desktop")
fn create_desktop_files_generic(
    src_dir: &Path,
    dst_dir: &Path,
    env_root: &Path,
    file_extension_filter: Option<&str>,
) -> Result<Vec<PathBuf>> {
    let mut processed_files = Vec::new();

    if !src_dir.exists() {
        return Ok(processed_files);
    }

    fs::create_dir_all(dst_dir)
        .with_context(|| format!("Failed to create directory {}", dst_dir.display()))?;

    for entry in fs::read_dir(src_dir)
        .with_context(|| format!("Failed to read directory {}", src_dir.display()))?
    {
        let entry = entry?;
        let src_path = entry.path();

        if !src_path.is_file() {
            continue;
        }

        let filename = src_path.file_name()
            .ok_or_else(|| eyre::eyre!("Failed to get filename for {}", src_path.display()))?;

        // Apply extension filter if provided
        if let Some(ext) = file_extension_filter {
            if !filename.to_string_lossy().ends_with(ext) {
                continue;
            }
        }

        let dst_path = dst_dir.join(filename);

        // Parse and adjust desktop file
        let adjusted_content = adjust_desktop_file(&src_path, env_root)?;

        // Write adjusted content to destination
        fs::write(&dst_path, adjusted_content)
            .with_context(|| format!("Failed to write desktop file {}", dst_path.display()))?;

        processed_files.push(dst_path.clone());

        log::debug!("Processed desktop file: {} -> {}", src_path.display(), dst_path.display());
    }

    Ok(processed_files)
}

/// Generic function to remove desktop files created from source directory
///
/// # Arguments
/// * `src_dir` - Source directory where files originated from
/// * `dst_dir` - Destination directory where files were created
/// * `file_extension_filter` - Optional file extension filter
fn remove_desktop_files_generic(
    src_dir: &Path,
    dst_dir: &Path,
    file_extension_filter: Option<&str>,
) -> Result<Vec<PathBuf>> {
    let mut removed_files = Vec::new();

    if !src_dir.exists() {
        return Ok(removed_files);
    }

    for entry in fs::read_dir(src_dir)
        .with_context(|| format!("Failed to read directory {}", src_dir.display()))?
    {
        let entry = entry?;
        let src_path = entry.path();

        if !src_path.is_file() {
            continue;
        }

        let filename = src_path.file_name()
            .ok_or_else(|| eyre::eyre!("Failed to get filename for {}", src_path.display()))?;

        // Apply extension filter if provided
        if let Some(ext) = file_extension_filter {
            if !filename.to_string_lossy().ends_with(ext) {
                continue;
            }
        }

        let dst_path = dst_dir.join(filename);

        // Remove the file if it exists
        if dst_path.exists() {
            fs::remove_file(&dst_path)
                .with_context(|| format!("Failed to remove desktop file {}", dst_path.display()))?;
            removed_files.push(dst_path.clone());
            log::debug!("Removed desktop file: {}", dst_path.display());
        }
    }

    Ok(removed_files)
}

/// Generic function to symlink files or directories from source base directory with glob pattern to destination
///
/// # Arguments
/// * `src_base` - Base source directory
/// * `glob_pattern` - Glob pattern relative to src_base for files/directories to symlink
/// * `dst_dir` - Destination directory to create symlinks in
fn symlink_generic(
    src_base: &Path,
    glob_pattern: &str,
    dst_dir: &Path,
) -> Result<Vec<PathBuf>> {
    let mut linked_items = Vec::new();

    if !src_base.exists() {
        return Ok(linked_items);
    }

    fs::create_dir_all(dst_dir)
        .with_context(|| format!("Failed to create directory {}", dst_dir.display()))?;

    // Construct full glob pattern by joining base path with glob pattern
    let full_glob = src_base.join(glob_pattern).to_string_lossy().to_string();

    // Use glob to find matching paths
    for entry in glob::glob(&full_glob)
        .with_context(|| format!("Failed to parse glob pattern {}", full_glob))?
    {
        let src_path = entry
            .with_context(|| format!("Failed to read glob entry for pattern {}", full_glob))?;

        // Determine the relative path from the base directory to compose destination
        let src_path_str = src_path.to_string_lossy();
        let base_str = src_base.to_string_lossy();

        // Strip the base directory from the source path to get the relative part
        let relative_path = if src_path_str.starts_with(&*base_str) {
            &src_path_str[base_str.len()..]
        } else {
            &src_path_str
        };

        // Remove leading path separator if present
        let relative_path = relative_path.strip_prefix(std::path::MAIN_SEPARATOR).unwrap_or(relative_path);

        // Compose destination path
        let dst_path = dst_dir.join(relative_path);

        // Create parent directories if they don't exist
        if let Some(parent) = dst_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create parent directory {}", parent.display()))?;
        }

        // Handle existing destination
        if dst_path.exists() {
            if dst_path.is_symlink() {
                fs::remove_file(&dst_path)?;
            } else {
                continue;
            }
        }

        // Create symlink
        symlink(&src_path, &dst_path)
            .with_context(|| format!("Failed to create symlink {} -> {}", dst_path.display(), src_path.display()))?;

        linked_items.push(dst_path.clone());

        let item_type = if src_path.is_dir() { "directory" } else { "file" };
        log::debug!("Linked {}: {} -> {}", item_type, src_path.display(), dst_path.display());
    }

    Ok(linked_items)
}

/// Generic function to remove symlinks from destination directory based on source base directory and glob pattern
///
/// # Arguments
/// * `src_base` - Base source directory (used to determine what was originally linked)
/// * `glob_pattern` - Glob pattern relative to src_base for files/directories that were symlinked
/// * `dst_dir` - Destination directory where symlinks were created
fn unlink_generic(
    src_base: &Path,
    glob_pattern: &str,
    dst_dir: &Path,
) -> Result<Vec<PathBuf>> {
    let mut unlinked_items = Vec::new();

    if !src_base.exists() {
        return Ok(unlinked_items);
    }

    // Construct full glob pattern by joining base path with glob pattern
    let full_glob = src_base.join(glob_pattern).to_string_lossy().to_string();

    // Use glob to find matching paths that were originally linked
    for entry in glob::glob(&full_glob)
        .with_context(|| format!("Failed to parse glob pattern {}", full_glob))?
    {
        let src_path = entry
            .with_context(|| format!("Failed to read glob entry for pattern {}", full_glob))?;

        // Determine the relative path from the base directory to compose destination
        let src_path_str = src_path.to_string_lossy();
        let base_str = src_base.to_string_lossy();

        // Strip the base directory from the source path to get the relative part
        let relative_path = if src_path_str.starts_with(&*base_str) {
            &src_path_str[base_str.len()..]
        } else {
            &src_path_str
        };

        // Remove leading path separator if present
        let relative_path = relative_path.strip_prefix(std::path::MAIN_SEPARATOR).unwrap_or(relative_path);

        // Compose destination path
        let dst_path = dst_dir.join(relative_path);

        // Remove the symlink if it exists
        if dst_path.exists() {
            if dst_path.is_symlink() {
                fs::remove_file(&dst_path)
                    .with_context(|| format!("Failed to remove symlink {}", dst_path.display()))?;
                unlinked_items.push(dst_path.clone());
                log::debug!("Removed symlink: {}", dst_path.display());
            } else {
                log::warn!("Expected symlink at {} but found regular file/directory, skipping removal", dst_path.display());
            }
        }
    }

    Ok(unlinked_items)
}

/// Symlink autostart entries from package to user's autostart directory
fn create_autostart_entries(
    store_fs_dir: &Path,
    env_root: &Path,
    home: &Path,
) -> Result<Vec<PathBuf>> {
    let autostart_src = store_fs_dir.join("etc/xdg/autostart");
    let autostart_dst = home.join(".config/autostart");
    create_desktop_files_generic(&autostart_src, &autostart_dst, env_root, None)
}

/// Symlink desktop files from package to user's applications directory
///
/// # Arguments
/// * `store_fs_dir` - Path to package's extracted files (e.g., /opt/epkg/store/.../fs)
/// * `env_root` - Path to environment root (e.g., /opt/epkg/envs/main)
/// * `home` - User's home directory path
fn create_desktop_files(
    store_fs_dir: &Path,
    env_root: &Path,
    home: &Path,
) -> Result<Vec<PathBuf>> {
    let applications_src = store_fs_dir.join("usr/share/applications");
    let applications_dst = home.join(".local/share/applications");
    create_desktop_files_generic(&applications_src, &applications_dst, env_root, Some(".desktop"))
}

/// Symlink icon directories from package to user's icons directory
fn symlink_icons(
    store_fs_dir: &Path,
    home: &Path,
) -> Result<Vec<PathBuf>> {
    let icons_src = store_fs_dir.join("usr/share/icons");
    let icons_dst = home.join(".local/share/icons");
    symlink_generic(&icons_src, "**/*", &icons_dst)
}

/// Symlink font directories from package to user's fonts directory
fn symlink_fonts(
    store_fs_dir: &Path,
    home: &Path,
) -> Result<Vec<PathBuf>> {
    let fonts_src = store_fs_dir.join("usr/share/fonts");
    let fonts_dst = home.join(".local/share/fonts");
    symlink_generic(&fonts_src, "**/*", &fonts_dst)
}

/// Symlink MIME type files from package to user's mime directory
fn symlink_mime_files(
    store_fs_dir: &Path,
    home: &Path,
) -> Result<Vec<PathBuf>> {
    let mime_src = store_fs_dir.join("usr/share/mime/packages");
    let mime_dst = home.join(".local/share/mime/packages");
    symlink_generic(&mime_src, "*", &mime_dst)
}

/// Symlink DBus service files from package to user's DBus directory
fn symlink_dbus_services(
    store_fs_dir: &Path,
    home: &Path,
) -> Result<Vec<PathBuf>> {
    let dbus_src = store_fs_dir.join("usr/share/dbus-1/services");
    let dbus_dst = home.join(".local/share/dbus-1/services");
    symlink_generic(&dbus_src, "*", &dbus_dst)
}

/// Remove autostart entries created by package
fn remove_autostart_entries(
    store_fs_dir: &Path,
    home: &Path,
) -> Result<Vec<PathBuf>> {
    let autostart_src = store_fs_dir.join("etc/xdg/autostart");
    let autostart_dst = home.join(".config/autostart");
    remove_desktop_files_generic(&autostart_src, &autostart_dst, None)
}

/// Remove desktop files created by package
fn remove_desktop_files(
    store_fs_dir: &Path,
    home: &Path,
) -> Result<Vec<PathBuf>> {
    let applications_src = store_fs_dir.join("usr/share/applications");
    let applications_dst = home.join(".local/share/applications");
    remove_desktop_files_generic(&applications_src, &applications_dst, Some(".desktop"))
}

/// Remove symlinks for icon directories created by package
fn unlink_icons(
    store_fs_dir: &Path,
    home: &Path,
) -> Result<Vec<PathBuf>> {
    let icons_src = store_fs_dir.join("usr/share/icons");
    let icons_dst = home.join(".local/share/icons");
    unlink_generic(&icons_src, "**/*", &icons_dst)
}

/// Remove symlinks for font directories created by package
fn unlink_fonts(
    store_fs_dir: &Path,
    home: &Path,
) -> Result<Vec<PathBuf>> {
    let fonts_src = store_fs_dir.join("usr/share/fonts");
    let fonts_dst = home.join(".local/share/fonts");
    unlink_generic(&fonts_src, "**/*", &fonts_dst)
}

/// Remove symlinks for MIME type files created by package
fn unlink_mime_files(
    store_fs_dir: &Path,
    home: &Path,
) -> Result<Vec<PathBuf>> {
    let mime_src = store_fs_dir.join("usr/share/mime/packages");
    let mime_dst = home.join(".local/share/mime/packages");
    unlink_generic(&mime_src, "*", &mime_dst)
}

/// Remove symlinks for DBus service files created by package
fn unlink_dbus_services(
    store_fs_dir: &Path,
    home: &Path,
) -> Result<Vec<PathBuf>> {
    let dbus_src = store_fs_dir.join("usr/share/dbus-1/services");
    let dbus_dst = home.join(".local/share/dbus-1/services");
    unlink_generic(&dbus_src, "*", &dbus_dst)
}

/// Common function to run a desktop database update command
///
/// # Arguments
/// * `env_root` - Environment root path
/// * `command` - The command name to run
/// * `args` - Arguments to pass to the command
/// * `home` - User's home directory path
/// * `subdir` - Subdirectory under XDG_DATA_HOME to check for existence
/// * `command_name` - Human-readable name for logging
fn run_desktop_update_command(
    home: &Path,
    env_root: &Path,
    command: &str,
    args: &[&str],
    subdir: &str,
    command_name: &str,
) -> Result<()> {
    // Compute XDG_DATA_HOME and the directory to check for existence
    let xdg_data_home = home.join(".local/share");
    let check_dir = xdg_data_home.join(subdir);

    // Check if directory exists
    if !check_dir.exists() {
        log::debug!("{} directory {} does not exist, skipping {}", command_name, check_dir.display(), command);
        return Ok(());
    }

    // Prepare RunOptions for fork_and_execute
    let mut run_options = crate::run::RunOptions {
        command: command.to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    };

    // Append subdir to args
    run_options.args.push(subdir.to_string());

    // Add XDG_DATA_HOME environment variable
    run_options.env_vars.insert("XDG_DATA_HOME".to_string(), xdg_data_home.to_string_lossy().to_string());

    // Execute the command using fork_and_execute
    match crate::run::fork_and_execute(env_root, &run_options) {
        Ok(_) => {
            log::debug!("Updated {}", command_name.to_lowercase());
        }
        Err(e) => {
            if e.to_string().contains("not found") || e.to_string().contains("ENOENT") {
                // Retry with root filesystem
                match crate::run::fork_and_execute(Path::new("/"), &run_options) {
                    Ok(_) => {
                        log::debug!("Updated {} (retried with /)", command_name.to_lowercase());
                    }
                    Err(retry_e) => {
                        log::warn!("{} failed even after retry: {}", command, retry_e);
                    }
                }
            } else {
                log::warn!("{} failed: {}", command, e);
            }
        }
    }

    Ok(())
}

/// Update desktop database (applications menu)
fn update_desktop_database(env_root: &Path, home: &Path) -> Result<()> {
    run_desktop_update_command(
        home,
        env_root,
        "update-desktop-database",
        &[],
        "applications",
        "desktop database"
    )
}

/// Update icon cache
fn update_icon_cache(env_root: &Path, home: &Path) -> Result<()> {
    run_desktop_update_command(
        home,
        env_root,
        "gtk-update-icon-cache",
        &["-f", "-t"],
        "icons/hicolor",
        "icon cache"
    )
}

/// Update MIME database
fn update_mime_database(env_root: &Path, home: &Path) -> Result<()> {
    run_desktop_update_command(
        home,
        env_root,
        "update-mime-database",
        &[],
        "mime",
        "MIME database"
    )
}

/// Update font cache
fn update_font_cache(env_root: &Path, home: &Path) -> Result<()> {
    run_desktop_update_command(
        home,
        env_root,
        "fc-cache",
        &["-f"],
        "fonts",
        "font cache"
    )
}

/// Desktop integration flags to track which types occurred during expose operations
#[derive(Debug, Clone, Default)]
pub struct DesktopIntegrationFlags {
    pub desktop_files: bool,  // Applications and/or autostart desktop files were processed
    pub icons: bool,          // Icon directories were symlinked
    pub mime_files: bool,     // MIME type files were symlinked
    pub fonts: bool,          // Font directories were symlinked
}

/// Macro to check if desktop integration function processed any files and update flags accordingly
macro_rules! check_integration {
    ($flags:expr, $field:ident, $func:expr) => {
        if !$func?.is_empty() {
            $flags.$field = true;
        }
    };
}

/// Macro to update desktop database if integration occurred
macro_rules! update_database_if_needed {
    ($flags:expr, $flag_field:ident, $update_func:expr) => {
        if $flags.$flag_field {
            let _ = $update_func;
        }
    };
}

/// Perform desktop integration for a package
pub fn expose_desktop_integration(store_fs_dir: &Path, env_root: &Path, home: &Path, flags: &mut DesktopIntegrationFlags) -> Result<()> {
    // Check autostart entries and desktop files (both contribute to desktop_files)
    check_integration!(flags, desktop_files, create_autostart_entries(store_fs_dir, env_root, home));
    check_integration!(flags, desktop_files, create_desktop_files(store_fs_dir, env_root, home));

    // Check other integration types
    check_integration!(flags, icons,        symlink_icons(store_fs_dir, home));
    check_integration!(flags, fonts,        symlink_fonts(store_fs_dir, home));
    check_integration!(flags, mime_files,   symlink_mime_files(store_fs_dir, home));

    // DBus services don't have a corresponding database update, so we don't track them
    let _ = symlink_dbus_services(store_fs_dir, home);

    Ok(())
}

/// Remove desktop integration for a package
pub fn unexpose_desktop_integration(store_fs_dir: &Path, home: &Path, flags: &mut DesktopIntegrationFlags) -> Result<()> {
    // Remove autostart entries and desktop files (both contribute to desktop_files)
    check_integration!(flags, desktop_files, remove_autostart_entries(store_fs_dir, home));
    check_integration!(flags, desktop_files, remove_desktop_files(store_fs_dir, home));

    // Remove other integration types
    check_integration!(flags, icons,        unlink_icons(store_fs_dir, home));
    check_integration!(flags, fonts,        unlink_fonts(store_fs_dir, home));
    check_integration!(flags, mime_files,   unlink_mime_files(store_fs_dir, home));

    // DBus services don't have a corresponding database update, so we don't track them
    let _ = unlink_dbus_services(store_fs_dir, home);

    Ok(())
}

/// Update desktop databases based on which types of integration occurred
pub fn update_desktop_databases(env_root: &Path, desktop_integration_occurred: &DesktopIntegrationFlags) {
    let home = crate::dirs::get_home().ok();
    if let Some(home_path) = home {
        let home_path = Path::new(&home_path);
        update_database_if_needed!(desktop_integration_occurred, desktop_files, update_desktop_database(env_root, home_path));
        update_database_if_needed!(desktop_integration_occurred, icons,         update_icon_cache(env_root, home_path));
        update_database_if_needed!(desktop_integration_occurred, fonts,         update_font_cache(env_root, home_path));
        update_database_if_needed!(desktop_integration_occurred, mime_files,    update_mime_database(env_root, home_path));
    }
}
