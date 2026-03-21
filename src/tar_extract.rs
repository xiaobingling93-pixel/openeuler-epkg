use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use tar::Archive;
use color_eyre::Result;
use crate::lfs;
use crate::utils;

/// Configuration for tar extraction
#[derive(Debug, Clone)]
pub struct ExtractConfig {
    /// The base directory where files will be extracted
    pub target_dir: PathBuf,
    /// Number of leading path components to strip
    pub strip_components: usize,
    /// Whether to collect hard links for deferred creation
    pub handle_hard_links: bool,
    /// Directory for metadata files starting with "." (e.g., ".PKGINFO")
    pub meta_dir: Option<PathBuf>,
}

impl ExtractConfig {
    /// Create a new extract configuration with the target directory
    pub fn new<P: AsRef<Path>>(target_dir: P) -> Self {
        Self {
            target_dir: target_dir.as_ref().to_path_buf(),
            strip_components: 0,
            handle_hard_links: true,
            meta_dir: None,
        }
    }

    /// Set the number of components to strip from paths
    #[allow(dead_code)]
    pub fn strip_components(mut self, count: usize) -> Self {
        self.strip_components = count;
        self
    }

    /// Set whether to handle hard links
    pub fn handle_hard_links(mut self, handle: bool) -> Self {
        self.handle_hard_links = handle;
        self
    }

    /// Set the metadata directory for dot files
    pub fn meta_dir<P: AsRef<Path>>(mut self, dir: P) -> Self {
        self.meta_dir = Some(dir.as_ref().to_path_buf());
        self
    }
}

/// Path classification policy function type
///
/// This function is called for each tar entry to determine:
/// - Where the entry should be extracted (target path)
/// - Whether the entry should be skipped (return None)
///
/// # Arguments
///
/// * `path` - The original path from the tar entry
/// * `is_hard_link` - Whether this entry is a hard link
/// * `store_tmp_dir` - The base store directory
///
/// # Returns
///
/// * `Some(PathBuf)` - The target path where the entry should be extracted
/// * `None` - Skip this entry (don't extract)
pub type PathPolicy =
    Box<dyn Fn(&Path, bool, &Path) -> Option<PathBuf>>;

/// Extract a tar archive with a custom path policy function
///
/// This is a more flexible version of `extract_archive()` that allows
/// the caller to provide a custom function for path classification.
///
/// # Example
///
/// ```rust,ignore
/// // Custom policy for brew packages
/// let policy = Box::new(|path, is_hard_link, store_tmp_dir| {
///     // Skip top-level entries (package_name/, version/)
///     let components: Vec<_> = path.components().collect();
///     if components.len() < 3 {
///         return None; // Skip
///     }
///
///     // Strip first two components (package_name/version/)
///     let stripped: PathBuf = components.iter().skip(2).collect();
///
///     // Handle .brew/ directory specially
///     if stripped.starts_with(".brew") {
///         return Some(crate::dirs::path_join(store_tmp_dir, &["info", "brew", ".brew"]).join(
///             stripped.strip_prefix(".brew").unwrap_or(&stripped)));
///     }
///
///     // Regular files go to fs/
///     Some(store_tmp_dir.join("fs").join(stripped))
/// });
///
/// let entries = extract_archive_with_policy(&mut archive, &config, policy)?;
/// ```
pub fn extract_archive_with_policy<R: Read>(
    archive: &mut Archive<R>,
    config: &ExtractConfig,
    path_policy: PathPolicy,
) -> Result<usize> {
    let mut entries_processed = 0;
    let mut hard_links: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut created_dirs: HashSet<PathBuf> = HashSet::new();

    for entry_result in archive.entries()? {
        let mut entry = entry_result?;
        let path = entry.path()?.to_path_buf();
        entries_processed += 1;

        log::trace!(
            "Processing tar entry #{}: {}",
            entries_processed,
            path.display()
        );

        // Check if this is a hard link entry
        let header = entry.header();
        let is_hard_link = matches!(header.entry_type(), tar::EntryType::Link);
        let is_symlink = matches!(header.entry_type(), tar::EntryType::Symlink);
        let mode = header.mode().unwrap_or(0o644);
        let is_dir = matches!(header.entry_type(), tar::EntryType::Directory);

        // Apply path policy, then map Unix-style names to Win32-safe paths (PUA encoding, etc.)
        let target_path = match (path_policy)(&path, is_hard_link, &config.target_dir) {
            Some(tp) => lfs::sanitize_path_for_windows(&tp),
            None => continue, // Skip this entry
        };

        // Handle hard links
        if config.handle_hard_links && is_hard_link {
            if let Ok(Some(link_path)) = entry.link_name() {
                let source_path = match (path_policy)(&link_path, false, &config.target_dir) {
                    Some(sp) => lfs::sanitize_path_for_windows(&sp),
                    None => continue,
                };

                log::trace!(
                    "Queued hard link: {} -> {}",
                    target_path.display(),
                    source_path.display()
                );

                hard_links.push((source_path, target_path));
                continue;
            }
        }

        // Ensure parent directory exists (with caching to avoid redundant calls)
        if let Some(parent) = target_path.parent() {
            if !created_dirs.contains(parent) {
                lfs::create_dir_all(parent)?;
                created_dirs.insert(parent.to_path_buf());
            }
        }

        // Extract the file
        entry.unpack(&target_path)?;
        // Skip permission fixup for symlinks - their permissions are meaningless (always lrwxrwxrwx)
        // and on Windows, setting EA on a symlink might affect the target instead
        if !is_symlink {
            utils::fixup_file_permissions_with_mode(&target_path, mode, is_dir);
        }

        // Cache directory path for future entries
        if is_dir {
            created_dirs.insert(target_path.clone());
        }
    }

    // Create hard links after all files are extracted
    create_hard_links(&hard_links)?;

    Ok(entries_processed)
}

/// Extract a tar archive with support for hard links and path stripping
///
/// This function handles the common pattern across package formats:
/// 1. Strip top-level directory prefixes (e.g., "package_name/version/")
/// 2. Detect hard links and collect them for deferred creation
/// 3. Extract regular files
/// 4. Create hard links after all files are extracted
///
/// # Path Routing Recommendation
///
/// For complex path routing needs (e.g., moving `.brew/` to `info/brew/.brew/`,
/// metadata files to `info/`), we recommend doing simple extraction first,
/// then performing post-extraction moves:
///
/// ```rust,ignore
/// // 1. Extract with simple path stripping
/// let config = ExtractConfig::new(store_tmp_dir.join("fs"))
///     .strip_components(2);  // Strip "pkgname/version/"
/// extract_archive(&mut archive, &config)?;
///
/// // 2. Post-extraction path routing via fs operations
/// move_brew_metadata(store_tmp_dir)?;  // Custom per-format logic
/// ```
///
/// This approach keeps the extraction logic simple and reusable, while
/// allowing each package format to implement its own path routing rules.
///
/// # Arguments
///
/// * `archive` - The tar archive to extract
/// * `config` - Configuration for extraction
///
/// # Returns
///
/// Returns the number of entries processed
pub fn extract_archive<R: Read>(
    archive: &mut Archive<R>,
    config: &ExtractConfig,
) -> Result<usize> {
    let mut entries_processed = 0;
    let mut hard_links: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut created_dirs: HashSet<PathBuf> = HashSet::new();

    for entry_result in archive.entries()? {
        let mut entry = entry_result?;
        let path = entry.path()?.to_path_buf();
        entries_processed += 1;

        log::trace!("Processing tar entry #{}: {}", entries_processed, path.display());

        // Calculate the target path
        let target_path = calculate_target_path(&path, config)?;

        // Get header info for hard link detection and permission fixup
        let header = entry.header();
        let is_hard_link = matches!(header.entry_type(), tar::EntryType::Link);
        let is_symlink = matches!(header.entry_type(), tar::EntryType::Symlink);
        let mode = header.mode().unwrap_or(0o644);
        let is_dir = matches!(header.entry_type(), tar::EntryType::Directory);

        // Check if this is a hard link entry
        if config.handle_hard_links {
            if is_hard_link {
                if let Ok(Some(link_path)) = entry.link_name() {
                    // Calculate the source path (the file being linked to)
                    let source_path = calculate_target_path(&link_path, config)?;

                    log::trace!(
                        "Queued hard link: {} -> {}",
                        target_path.display(),
                        source_path.display()
                    );

                    hard_links.push((source_path, target_path));
                    continue;
                }
            }
        }

        // Ensure parent directory exists (with caching to avoid redundant calls)
        if let Some(parent) = target_path.parent() {
            if !created_dirs.contains(parent) {
                lfs::create_dir_all(parent)?;
                created_dirs.insert(parent.to_path_buf());
            }
        }

        // Extract the file
        entry.unpack(&target_path)?;
        // Skip permission fixup for symlinks - their permissions are meaningless (always lrwxrwxrwx)
        // and on Windows, setting EA on a symlink might affect the target instead
        if !is_symlink {
            utils::fixup_file_permissions_with_mode(&target_path, mode, is_dir);
        }

        // Cache directory path for future entries
        if is_dir {
            created_dirs.insert(target_path.clone());
        }
    }

    // Now create all hard links after all files have been extracted
    for (source_path, target_path) in hard_links {
        // Ensure parent directory exists for the hard link target
        if let Some(parent) = target_path.parent() {
            if let Err(e) = lfs::create_dir_all(parent) {
                log::warn!("Failed to create directory {} for hard link: {}", parent.display(), e);
                continue;
            }
        }

        // Create the hard link if the source file exists
        if lfs::exists_on_host(&source_path) {
            // Remove existing file if present (in case of re-extraction)
            if lfs::exists_on_host(&target_path) {
                if let Err(e) = lfs::remove_file(&target_path) {
                    log::warn!("Failed to remove existing file {}: {}", target_path.display(), e);
                }
            }

            if let Err(e) = fs::hard_link(&source_path, &target_path) {
                log::warn!("Failed to create hard link from {} to {}: {}",
                    source_path.display(), target_path.display(), e);
            } else {
                log::trace!("Created hard link: {} -> {}", target_path.display(), source_path.display());
            }
        } else {
            log::warn!("Cannot create hard link {}: source file {} does not exist",
                target_path.display(), source_path.display());
        }
    }

    Ok(entries_processed)
}

/// Calculate the target path for a tar entry, applying path stripping and
/// metadata directory redirection
fn calculate_target_path(path: &Path, config: &ExtractConfig) -> Result<PathBuf> {
    // Strip leading components from the path
    let stripped_path = strip_path_components(path, config.strip_components);

    // Handle metadata routing:
    // - Dot files (metadata files starting with ".") go to meta_dir
    // - For conda packages, "info/*" directory contents also go to meta_dir
    let result = if let Some(ref meta_dir) = config.meta_dir {
        // Check if this is a dot file
        if let Some(file_name) = stripped_path.file_name() {
            let name_str = file_name.to_string_lossy();
            if name_str.starts_with('.') {
                // This is a metadata file, place it in meta_dir
                return Ok(meta_dir.join(stripped_path));
            }
        }

        // Check if this is an info/ directory entry (for conda packages)
        // Route info/* to meta_dir (e.g., info/index.json -> meta_dir/index.json)
        if stripped_path.starts_with("info") {
            let relative = stripped_path.strip_prefix("info").unwrap_or(&stripped_path);
            return Ok(meta_dir.join(relative));
        }

        config.target_dir.join(&stripped_path)
    } else {
        config.target_dir.join(&stripped_path)
    };

    Ok(lfs::sanitize_path_for_windows(&result))
}

/// Strip leading components from a path
///
/// Returns a new path with the specified number of leading components removed.
/// If the path has fewer components than requested, returns the original path.
fn strip_path_components(path: &Path, components_to_strip: usize) -> PathBuf {
    let components: Vec<_> = path.components().collect();

    if components.len() <= components_to_strip {
        return PathBuf::from(".");
    }

    components
        .into_iter()
        .skip(components_to_strip)
        .collect()
}

/// Creates the standard package store directory structure
///
/// Creates the following directories under `store_tmp_dir`:
/// - `fs/` - for extracted package files
/// - `info/{format}/` - for format-specific metadata (e.g., "deb", "rpm")
/// - `info/install/` - for installation scripts/hooks
///
/// # Example
///
/// ```rust,ignore
/// // For a Debian package
/// create_package_dirs(store_tmp_dir, "deb")?;
/// // Creates: fs/, info/deb/, info/install/
/// ```
pub fn create_package_dirs<P: AsRef<Path>>(
    store_tmp_dir: P,
    format: &str,
) -> Result<()> {
    let store_tmp_dir = store_tmp_dir.as_ref();
    lfs::create_dir_all(store_tmp_dir.join("fs"))?;
    lfs::create_dir_all(crate::dirs::path_join(store_tmp_dir, &["info", format]))?;
    lfs::create_dir_all(crate::dirs::path_join(store_tmp_dir, &["info", "install"]))?;
    Ok(())
}

/// Create hard links from a list of (source, target) pairs
///
/// This is a shared helper for creating hard links after all files have been extracted.
/// It handles directory creation, existing file removal, and proper error logging.
///
/// # Arguments
///
/// * `links` - A slice of (source_path, target_path) tuples
///
/// # Example
///
/// ```rust,ignore
/// let mut hard_links: Vec<(PathBuf, PathBuf)> = Vec::new();
/// // ... collect hard links during extraction ...
/// create_hard_links(&hard_links)?;
/// ```
/// Unpack a tar archive to `dest`.
///
/// On Windows, each entry path is passed through [`lfs::sanitize_path_for_windows`]
/// so POSIX names with `:` and other illegal characters extract successfully
/// (same behavior as `utils` tar paths). On other platforms this delegates to
/// [`tar::Archive::unpack`].
pub fn unpack_tar_archive<R: Read>(archive: &mut Archive<R>, dest: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        lfs::create_dir_all(dest)?;
        for entry_result in archive.entries()? {
            let mut entry = entry_result?;
            let entry_path = entry.path()?.to_path_buf();
            let sanitized_path = lfs::sanitize_path_for_windows(&entry_path);
            if entry_path != sanitized_path {
                log::debug!(
                    "Sanitized tar entry path: '{}' -> '{}'",
                    entry_path.display(),
                    sanitized_path.display()
                );
            }
            let dest_path = dest.join(&sanitized_path);
            if let Some(parent) = dest_path.parent() {
                lfs::create_dir_all(parent)?;
            }
            entry.unpack(&dest_path)?;
        }
    }
    #[cfg(not(windows))]
    {
        archive.unpack(dest)?;
    }
    Ok(())
}

pub fn create_hard_links(links: &[(PathBuf, PathBuf)]) -> Result<()> {
    for (source_path, target_path) in links {
        // Ensure parent directory exists for the hard link target
        if let Some(parent) = target_path.parent() {
            if let Err(e) = lfs::create_dir_all(parent) {
                log::warn!("Failed to create directory {} for hard link: {}", parent.display(), e);
                continue;
            }
        }

        // Create the hard link if the source file exists
        if lfs::exists_on_host(source_path) {
            // Remove existing file if present (in case of re-extraction)
            if lfs::exists_on_host(target_path) {
                if let Err(e) = lfs::remove_file(target_path) {
                    log::warn!("Failed to remove existing file {}: {}", target_path.display(), e);
                }
            }

            if let Err(e) = fs::hard_link(source_path, target_path) {
                log::warn!("Failed to create hard link from {} to {}: {}",
                    source_path.display(), target_path.display(), e);
            } else {
                log::trace!("Created hard link: {} -> {}", target_path.display(), source_path.display());
            }
        } else {
            log::warn!("Cannot create hard link {}: source file {} does not exist",
                target_path.display(), source_path.display());
        }
    }

    Ok(())
}
