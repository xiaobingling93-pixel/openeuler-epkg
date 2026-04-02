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
//!  - Adjustments: Modify Exec field to point to ebin wrapper ($env_root/ebin/<app>)
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

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use color_eyre::Result;
use color_eyre::eyre::{self, WrapErr};
use log;
use crate::lfs;


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
    let ebin_wrapper = env_root.join("ebin").join(&binary_name);
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
/// * `env_root` - Environment root path for adjusting desktop file content and creating symlink targets
/// * `filelist` - List of relative file paths from package filesystem
/// * `src_prefix` - Source directory prefix to filter files (e.g., "usr/share/applications")
/// * `dst_subdir` - Destination subdirectory under home directory (e.g., ".local/share/applications")
fn create_desktop_files(
    env_root: &Path,
    filelist: &[String],
    src_prefix: &str,
    dst_subdir: &str,
) -> Result<Vec<PathBuf>> {
    let mut processed_files = Vec::new();

    let home = crate::dirs::get_home().ok();
    let home_path = if let Some(ref home_str) = home {
        Path::new(home_str)
    } else {
        return Ok(processed_files);
    };
    let dst_dir = home_path.join(dst_subdir);

    lfs::create_dir_all(&dst_dir)?;

    // Filter filelist for files in the source directory that end with .desktop
    let desktop_files: Vec<&String> = filelist
        .iter()
        .filter(|path| {
            path.starts_with(src_prefix) &&
            path.ends_with(".desktop")
        })
        .collect();

    for file_path in desktop_files {
        let host_rel = lfs::host_path_from_manifest_rel_path(file_path.trim_start_matches('/'));
        let filename = host_rel
            .file_name()
            .ok_or_else(|| eyre::eyre!("Failed to get filename for {}", file_path))?;

        let dst_path = dst_dir.join(filename);

        // Construct the source path under env_root (where the file has been moved)
        let src_path = env_root.join(&host_rel);

        // Parse and adjust desktop file
        let adjusted_content = adjust_desktop_file(&src_path, env_root)?;

        // Write adjusted content to destination
        lfs::write(&dst_path, adjusted_content)?;

        processed_files.push(dst_path.clone());

        log::debug!("Processed desktop file: {} -> {}", src_path.display(), dst_path.display());
    }

    Ok(processed_files)
}


/// Generic function to symlink files or directories from filelist to destination
///
/// # Arguments
/// * `env_root` - Environment root path where files have been moved, used as symlink target
/// * `filelist` - List of relative file paths from package filesystem
/// * `src_prefix` - Base source directory prefix to filter files (e.g., "usr/share/icons")
/// * `dst_subdir` - Destination subdirectory under home directory (e.g., ".local/share/icons")
fn symlink_desktop_files(
    env_root: &Path,
    filelist: &[String],
    src_prefix: &str,
    dst_subdir: &str,
) -> Result<Vec<PathBuf>> {
    let mut linked_items = Vec::new();

    let home = crate::dirs::get_home().ok();
    let home_path = if let Some(ref home_str) = home {
        Path::new(home_str)
    } else {
        return Ok(linked_items);
    };
    let dst_dir = home_path.join(dst_subdir);

    lfs::create_dir_all(&dst_dir)?;

    // Filter filelist for files/directories in the source base directory
    let matching_items: Vec<&String> = filelist
        .iter()
        .filter(|path| path.starts_with(src_prefix))
        .collect();

    for file_path in matching_items {
        // Get the relative path from the base prefix
        let relative_path = &file_path[src_prefix.len()..];
        let relative_path = relative_path.strip_prefix('/').unwrap_or(relative_path);

        // Compose destination path (POSIX manifest segments → host names on Windows)
        let dst_path = dst_dir.join(crate::lfs::host_path_from_manifest_rel_path(relative_path));

        // Create parent directories if they don't exist
        if let Some(parent) = dst_path.parent() {
            crate::utils::safe_mkdir_p(parent)?;
        }

        // Handle existing destination
        if lfs::is_symlink(&dst_path) {
            lfs::remove_file(&dst_path)?;
        } else if dst_path.exists() {
            // Regular file or directory - skip
            continue;
        }

        // Create symlink to the path under env_root (where the file has been moved)
        let target_path = env_root.join(crate::lfs::host_path_from_manifest_rel_path(file_path));

        // Skip directories
        if target_path.is_dir() {
            continue;
        }

        lfs::symlink_file_for_virtiofs(&target_path, &dst_path)?;

        linked_items.push(dst_path.clone());

        log::debug!("Linked desktop file: {} -> {}", target_path.display(), dst_path.display());
    }

    Ok(linked_items)
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
    let xdg_data_home = crate::dirs::path_join(home, &[".local", "share"]);
    let dir_to_update = xdg_data_home.join(subdir);

    // Check if directory exists
    if !dir_to_update.exists() {
        log::debug!("{} directory {} does not exist, skipping {}", command_name, dir_to_update.display(), command);
        return Ok(());
    }

    // Prepare RunOptions for fork_and_execute
    // Inherit VM settings from active VM reuse session during install/upgrade.
    // This allows desktop update commands to reuse the same VM that was created for the main command.
    // The VM reuse logic is handled in fork_and_execute() -> prepare_run_options_for_command().
    let mut run_options = crate::run::RunOptions {
        command: command.to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    };

    // Append dir_to_update to args
    run_options.args.push(dir_to_update.display().to_string());

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
                run_options.skip_namespace_isolation = true;
                match crate::run::fork_and_execute(Path::new("/"), &run_options) {
                    Ok(_) => {
                        log::debug!("Updated {} (retried with /)", command_name.to_lowercase());
                    }
                    Err(retry_e) => {
                        // This can happen in container
                        log::info!("{} not work even after retry: {}", command, retry_e);
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

/// Perform desktop integration for a package and return the created links
pub fn expose_package_xdesktop(env_root: &Path, filelist: &[String], desktop_integration_occurred: &mut DesktopIntegrationFlags) -> Result<Vec<String>> {
    let mut links = Vec::new();

    /// Macro to check if desktop integration function processed any files and collect links
    macro_rules! collect_integration_links {
        ($flag_field:ident, $func:expr) => {
            let processed_files = $func?;
            if !processed_files.is_empty() {
                desktop_integration_occurred.$flag_field = true;
            }
            links.extend(processed_files.into_iter().map(|p| p.to_string_lossy().into_owned()));
        };
    }

    // Check autostart entries and desktop files (both contribute to desktop_files)
    collect_integration_links!(desktop_files, create_desktop_files(env_root, filelist, "etc/xdg/autostart", ".config/autostart"));
    collect_integration_links!(desktop_files, create_desktop_files(env_root, filelist, "usr/share/applications", ".local/share/applications"));

    // Check other integration types
    collect_integration_links!(icons,      symlink_desktop_files(env_root, filelist, "usr/share/icons", ".local/share/icons"));
    collect_integration_links!(fonts,      symlink_desktop_files(env_root, filelist, "usr/share/fonts", ".local/share/fonts"));
    // MIME: Only symlink source XML files from packages/, not:
    // - Generated cache files (mime.cache, globs, aliases, etc.)
    // - Generated type definition files in subdirs (application/, text/, image/, video/, inode/, etc.)
    // These are all created by update-mime-database from the source XML files.
    collect_integration_links!(mime_files, symlink_desktop_files(env_root, filelist, "usr/share/mime/packages", ".local/share/mime/packages"));

    // DBus services don't have a corresponding database update, so we don't track them
    let _ = symlink_desktop_files(env_root, filelist, "usr/share/dbus-1/services", ".local/share/dbus-1/services");

    Ok(links)
}


/// Desktop files created by env have their Exec line modified to point to the ebin wrapper
/// under the environment root.
fn is_env_desktop_file(desktop_path: &Path, env_root: &Path) -> Result<bool> {
    let file = match fs::File::open(desktop_path) {
        Ok(f) => f,
        Err(_) => return Ok(false), // File doesn't exist or can't be read
    };

    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = line?;
        if line.starts_with("Exec=") {
            // Check if Exec line points to env_root
            let env_prefix = env_root.to_string_lossy().to_string();
            if line.contains(&env_prefix) {
                return Ok(true);
            }
            break;
        }
    }

    Ok(false)
}

/// Remove a single symlinked desktop integration file (icons, fonts, mime files, etc.)
fn remove_symlinked_file(
    link_path: &Path,
    link_path_str: &str,
    env_root: &Path,
    desktop_integration_occurred: &mut DesktopIntegrationFlags,
) -> Result<()> {
    // Verify this is a symlink and points to our environment or store
    if let Ok(target_path) = fs::read_link(link_path) {
        // Check if the symlink target starts with env_root
        if target_path.starts_with(env_root) {
            // Safe to remove - this symlink points to our environment or store
            if let Err(e) = lfs::remove_file(link_path) {
                log::warn!("Failed to remove desktop integration file {}: {}", link_path.display(), e);
            } else {
                log::debug!("Removed desktop integration file: {}", link_path.display());

                // Update flags based on what was removed
                if link_path_str.contains("icons") {
                    desktop_integration_occurred.icons = true;
                } else if link_path_str.contains("fonts") {
                    desktop_integration_occurred.fonts = true;
                } else if link_path_str.contains("mime") {
                    desktop_integration_occurred.mime_files = true;
                }
            }
        } else {
            log::warn!("Desktop integration file {} points outside environment or store ({}), not removing",
                link_path.display(), target_path.display());
        }
    } else {
        log::warn!("Desktop integration file {} is not a symlink or cannot read link target, not removing",
            link_path.display());
    }

    Ok(())
}

/// Remove a single desktop file that points to env_root
fn remove_desktop_file(
    link_path: &Path,
    env_root: &Path,
    desktop_integration_occurred: &mut DesktopIntegrationFlags,
) -> Result<()> {
    match is_env_desktop_file(link_path, env_root) {
        Ok(true) => {
            if let Err(e) = lfs::remove_file(link_path) {
                log::warn!("Failed to remove desktop integration file {}: {}", link_path.display(), e);
            } else {
                log::debug!("Removed desktop integration file: {}", link_path.display());
                desktop_integration_occurred.desktop_files = true;
            }
        }
        Ok(false) => {
            log::warn!("Desktop file {} not pointing to env_root {}, not removing",
                link_path.display(),
                env_root.display(),
                );
        }
        Err(e) => {
            log::warn!("Failed to check if desktop file {} was created by epkg: {}, not removing",
                link_path.display(), e);
        }
    }

    Ok(())
}

/// Remove desktop integration files based on stored links, with env validation
///
/// This function removes desktop integration files that were previously created during package exposure.
/// It validates that each file to be removed was created by epkg before removal:
/// - Symlinks are checked to ensure they point to the environment or store
/// - Desktop files are checked to ensure their Exec line points to the ebin wrapper
///
/// # Arguments
/// * `xdesktop_links` - List of paths to desktop integration files that were created
/// * `env_root` - Environment root path to validate file targets against
/// * `desktop_integration_occurred` - Mutable reference to flags tracking which integration types occurred
pub fn unexpose_package_xdesktop(
    xdesktop_links: &[String],
    env_root: &Path,
    desktop_integration_occurred: &mut DesktopIntegrationFlags,
) -> Result<()> {
    for link_path_str in xdesktop_links {
        let link_path = Path::new(link_path_str);

        // Check if the file exists
        if !link_path.exists() {
            log::debug!("Desktop integration file does not exist, skipping: {}", link_path.display());
            continue;
        }

        // Check if this is a desktop file by extension
        if lfs::is_symlink(link_path) {
            remove_symlinked_file(link_path, link_path_str, env_root, desktop_integration_occurred)?;
        } else if link_path_str.ends_with(".desktop") {
            remove_desktop_file(link_path, env_root, desktop_integration_occurred)?;
        } else {
            log::warn!("Desktop integration file {} is not a symlink or .desktop file, not removing", link_path.display());
        }
    }

    Ok(())
}

/// Update desktop databases based on which types of integration occurred
pub fn update_desktop_databases(env_root: &Path, desktop_integration_occurred: &DesktopIntegrationFlags) {
    /// Macro to update desktop database if integration occurred
    macro_rules! update_database_if_needed {
        ($flag_field:ident, $update_func:expr) => {
            if desktop_integration_occurred.$flag_field {
                let _ = $update_func;
            }
        };
    }

    let home = crate::dirs::get_home().ok();
    if let Some(home_path) = home {
        let home_path = Path::new(&home_path);
        update_database_if_needed!(desktop_files, update_desktop_database(env_root, home_path));
        update_database_if_needed!(icons,         update_icon_cache(env_root, home_path));
        update_database_if_needed!(fonts,         update_font_cache(env_root, home_path));
        update_database_if_needed!(mime_files,    update_mime_database(env_root, home_path));
    }
}
