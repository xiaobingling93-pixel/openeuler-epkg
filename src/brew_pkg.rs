use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use tar::Archive;
use log;
use color_eyre::Result;
use color_eyre::eyre::{self, WrapErr};
use flate2::read::GzDecoder;
use crate::lfs;
use crate::tar_extract::{create_package_dirs, ExtractConfig, extract_archive_with_policy};

/// Homebrew placeholder prefixes that need to be rewritten in dylib/interpreter paths
const HOMEBREW_PLACEHOLDER_PREFIXES: &[&str] = &[
    "@@HOMEBREW_CELLAR@@",
    "@@HOMEBREW_PREFIX@@",
];

/// Short prefix used for rewriting Homebrew paths (.LB = 3 chars vs .linuxbrew = 10 chars)
/// This ensures rewritten paths fit in placeholder buffers without overflow.
const HOMEBREW_SHORT_PREFIX: &str = "/home/linuxbrew/.LB";

/// Homebrew preferred installation prefixes.
///
/// Homebrew bottles are precompiled binaries that expect to be installed at specific
/// prefixes. Using the correct prefix is required for most bottles to work.
///
/// See: https://docs.brew.sh/Homebrew-on-Linux
/// See: https://docs.brew.sh/Installation
pub mod prefix {
    use std::path::PathBuf;

    /// Linux preferred prefix: /home/linuxbrew/.linuxbrew
    /// This avoids writing to system-owned directories while allowing bottles to work.
    #[cfg(not(target_os = "macos"))]
    pub const LINUX: &str = "/home/linuxbrew/.linuxbrew";

    /// macOS ARM (Apple Silicon) preferred prefix: /opt/homebrew
    #[cfg(target_os = "macos")]
    pub const MACOS_ARM: &str = "/opt/homebrew";

    /// macOS Intel preferred prefix: /usr/local
    #[cfg(target_os = "macos")]
    pub const MACOS_INTEL: &str = "/usr/local";

    /// Get the preferred HOMEBREW_PREFIX for the current platform.
    pub fn preferred() -> &'static str {
        #[cfg(target_os = "macos")]
        {
            // Detect Apple Silicon vs Intel
            if is_apple_silicon() {
                MACOS_ARM
            } else {
                MACOS_INTEL
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            LINUX
        }
    }

    /// Get the preferred prefix as a PathBuf
    pub fn preferred_path() -> PathBuf {
        PathBuf::from(preferred())
    }

    #[cfg(target_os = "macos")]
    fn is_apple_silicon() -> bool {
        // Check for ARM64 architecture
        std::env::consts::ARCH == "aarch64"
    }
}

/// Global lock to serialize install_name_tool/patchelf rewrites.
/// Parallel package linking can trigger concurrent rewrite passes on the same env files.
static BREW_DYLIB_REWRITE_LOCK: std::sync::LazyLock<std::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(()));

/// Common metadata file prefixes (base names) that brew packages include at root level
/// These should be moved to info/brew/ to avoid conflicts between packages
const BREW_META_FILE_PREFIXES: &[&str] = &[
    "AUTHORS",
    "CHANGELOG",
    "CHANGES",
    "COPYING",
    "HISTORY",
    "LICENSE",
    "NEWS",
    "README",
    "RELEASE",
    "TODO",
];

/// Specific metadata file names to check
const BREW_META_FILE_NAMES: &[&str] = &[
    "ChangeLog",
    "RELEASE_NOTES",
    "sbom.spdx.json",
];

/// Check if a path component is a brew metadata file
/// Matches files at root level with ALL CAPS names (possibly with extensions)
fn is_brew_meta_file(name: &str) -> bool {
    // Check specific file names
    if BREW_META_FILE_NAMES.iter().any(|&meta| name == meta) {
        return true;
    }

    // Check if name starts with one of the known prefixes
    // Handles: LICENSE, LICENSE.md, README.rst, etc.
    for prefix in BREW_META_FILE_PREFIXES {
        if name == *prefix {
            return true;
        }
        // Check with extension: PREFIX.ext (e.g., README.md, CHANGELOG.md)
        if name.starts_with(prefix) && name.len() > prefix.len() {
            let rest = &name[prefix.len()..];
            // Match .ext patterns where ext is lowercase letters
            if rest.starts_with('.') && rest[1..].chars().all(|c| c.is_ascii_lowercase()) {
                return true;
            }
        }
    }

    // Check for ALL CAPS names (possibly with extensions)
    // e.g., AUTHORS, CONTRIBUTORS, PATENTS, etc.
    let base_name = name.split('.').next().unwrap_or(name);
    if base_name.chars().all(|c| c.is_ascii_uppercase() || c == '_' || c == '-') && base_name.len() > 2 {
        // Must have at least some uppercase letters (not just ___ or ---)
        if base_name.chars().any(|c| c.is_ascii_uppercase()) {
            return true;
        }
    }

    false
}

/// Unpacks a Brew bottle to the specified directory
///
/// Brew bottles are gzipped tar archives containing precompiled binaries.
/// The structure is:
///   package_name/version/  (e.g., jq/1.7.1/)
///     bin/
///     lib/
///     share/
///     ...
///
/// We extract to fs/ and create info files separately from formula metadata
pub fn unpack_package<P: AsRef<Path>>(bottle_file: P, store_tmp_dir: P, pkgkey: Option<&str>) -> Result<()> {
    let bottle_file = bottle_file.as_ref();
    let store_tmp_dir = store_tmp_dir.as_ref();

    // Create the required directory structure following project pattern
    create_package_dirs(store_tmp_dir, "brew")?;

    log::debug!("Unpacking Brew bottle: {}", bottle_file.display());

    // Validate file exists and is readable
    let metadata = lfs::metadata_on_host(bottle_file)
        .wrap_err_with(|| format!("Failed to read file metadata: {}", bottle_file.display()))?;

    let file_size = metadata.len();
    if file_size == 0 {
        return Err(eyre::eyre!(
            "File is empty (0 bytes): {}. The download may be incomplete or the file may be corrupted.",
            bottle_file.display()
        ));
    }

    // Open and decompress the bottle
    let file = fs::File::open(bottle_file)
        .wrap_err_with(|| format!("Failed to open bottle file: {}", bottle_file.display()))?;
    let decoder = GzDecoder::new(file);
    let archive = Archive::new(decoder);

    // Use policy-based extraction for Brew bottles
    extract_brew_contents(archive, store_tmp_dir, pkgkey)?;

    // Note: Dylib path rewriting is done at link time (for Move link type)
    // because paths need to be absolute and point to the specific environment.
    // The store files retain the original placeholder paths until linking.

    // Create package.txt from pkgkey
    // pkgkey format: {pkgname}__{version}__{arch}
    if let Some(key) = pkgkey {
        create_package_txt_from_pkgkey(store_tmp_dir, key)
            .wrap_err_with(|| format!("Failed to create package.txt for {}", store_tmp_dir.display()))?;
    } else {
        return Err(eyre::eyre!("pkgkey is required for Brew package unpacking"));
    }

    // Generate filelist.txt following project pattern
    crate::store::create_filelist_txt(store_tmp_dir)
        .wrap_err_with(|| format!("Failed to create filelist.txt for {}", store_tmp_dir.display()))?;

    Ok(())
}

/// Path policy for Brew bottles
///
/// Brew bottles have a top-level directory like "package_name/version/"
/// - Skip top-level entries (package_name/, package_name/version/)
/// - .brew/ directory goes to info/brew/.brew/
/// - Root-level metadata files go to info/brew/
/// - Regular files go to fs/Cellar/package_name/tar_version/... (Homebrew-style layout)
///
/// Note: The version from tar path (e.g., "2.3.2") is used for Cellar directory,
/// NOT the pkgkey version with bottle revision (e.g., "2.3.2_0").
/// This matches vanilla Homebrew's Cellar layout: Cellar/pkgname/VERSION/
/// where VERSION is the formula version, not the bottle revision.
/// Homebrew bottles have hardcoded paths referencing Cellar/pkgname/VERSION/,
/// so we must match this layout for dylib path resolution to work correctly.
fn brew_path_policy_with_pkgkey(path: &Path, _is_hard_link: bool, store_tmp_dir: &Path, _pkgkey_version: &str) -> Option<PathBuf> {
    // Path structure: "package_name/version/..." (e.g., "jq/1.7.1/bin/jq")
    // We want: "Cellar/package_name/tar_version/..." for regular files
    // tar_version is the version from the tar path (without bottle revision)
    // This matches vanilla Homebrew: Cellar/jq/1.7.1/bin/jq
    let components: Vec<_> = path.components().collect();
    if components.len() < 3 {
        // Skip top-level entries (package_name/, package_name/version/)
        return None;
    }

    // Get package name and version from tar path
    // components[0] = package_name (e.g., "jq" or "python@3.13")
    // components[1] = tar_version (e.g., "1.7.1" or "3.13.13") - WITHOUT bottle revision
    let pkgname = components[0].as_os_str().to_str().unwrap_or("");
    let tar_version = components[1].as_os_str().to_str().unwrap_or("");

    // Reconstruct path without first two components (package_name and version)
    let stripped_components: Vec<_> = components.iter().skip(2).collect();

    // Check for .brew/ directory (contains formula with post_install etc)
    let is_brew_dir = stripped_components.first()
        .and_then(|c| c.as_os_str().to_str())
        .map(|s| s == ".brew")
        .unwrap_or(false);

    // Check for root-level metadata files
    let is_root_meta_file = !is_brew_dir && stripped_components.len() == 1 &&
        stripped_components[0].as_os_str().to_str()
            .map(|s| is_brew_meta_file(s))
            .unwrap_or(false);

    let target_path = if is_brew_dir {
        // Move .brew/ directory to info/brew/.brew/ (contains formula with post_install etc)
        stripped_components.iter().skip(1).fold(
            crate::dirs::path_join(store_tmp_dir, &["info", "brew", ".brew"]),
            |acc, comp| acc.join(comp.as_os_str())
        )
    } else if is_root_meta_file {
        // Move metadata files to info/brew/ to avoid conflicts
        crate::dirs::path_join(store_tmp_dir, &["info", "brew"]).join(
            stripped_components[0].as_os_str()
        )
    } else {
        // Regular files go to fs/Cellar/package_name/tar_version/... (Homebrew-style layout)
        // Use tar_version from tar path (without bottle revision)
        // This matches the vanilla Homebrew directory structure:
        // /opt/homebrew/Cellar/jq/1.7.1/bin/jq
        // IMPORTANT: Homebrew bottles have hardcoded paths referencing Cellar/pkgname/VERSION
        // where VERSION is the formula version (without bottle revision).
        // If we use version with bottle revision (e.g., "3.13.13_0"), hardcoded paths
        // like "Cellar/python@3.13/3.13.13/Frameworks/..." in the dylib won't match.
        let cellar_base = crate::dirs::path_join(store_tmp_dir, &["fs", "Cellar", pkgname, tar_version]);
        stripped_components.iter().fold(
            cellar_base,
            |acc, comp| acc.join(comp.as_os_str())
        )
    };

    Some(target_path)
}

/// Extract Brew bottle contents using policy-based extraction
fn extract_brew_contents<R: Read>(
    archive: Archive<R>,
    store_tmp_dir: &Path,
    pkgkey: Option<&str>,
) -> Result<usize> {
    // Extract version from pkgkey: {pkgname}__{version}__{arch}
    // The version from pkgkey includes bottle revision (e.g., "2.3.2_0")
    // which is different from tar path version (e.g., "2.3.2")
    // Use Box::leak to get a static reference for the closure
    let pkgkey_version: &'static str = match pkgkey {
        Some(key) => {
            let (_, version, _) = crate::package::parse_pkgkey_parts(key)
                .wrap_err_with(|| format!("Invalid pkgkey format: {}", key))?;
            Box::leak(version.to_string().into_boxed_str())
        }
        None => {
            return Err(eyre::eyre!("pkgkey is required for Brew package extraction"));
        }
    };

    let config = ExtractConfig::new(store_tmp_dir)
        .handle_hard_links(true);

    // Use closure with static pkgkey_version reference
    let policy: crate::tar_extract::PathPolicy = Box::new(|path, is_hard_link, store_tmp_dir| {
        brew_path_policy_with_pkgkey(path, is_hard_link, store_tmp_dir, pkgkey_version)
    });
    let mut archive = archive;
    extract_archive_with_policy(&mut archive, &config, policy)
}

/// Creates package.txt from pkgkey
fn create_package_txt_from_pkgkey<P: AsRef<Path>>(store_tmp_dir: P, pkgkey: &str) -> Result<()> {
    let store_tmp_dir = store_tmp_dir.as_ref();

    // Parse pkgkey: {pkgname}__{version}__{arch}
    let (pkgname, version, arch) = crate::package::parse_pkgkey_parts(pkgkey)
        .wrap_err_with(|| format!("Invalid pkgkey format: {}", pkgkey))?;

    // Create package fields
    let mut package_fields: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    package_fields.insert("pkgname".to_string(), pkgname.to_string());
    package_fields.insert("version".to_string(), version.to_string());
    package_fields.insert("arch".to_string(), arch.to_string());
    package_fields.insert("format".to_string(), "brew".to_string());

    // Save the package.txt file using the common store function
    crate::store::save_package_txt(package_fields, store_tmp_dir, Some(pkgkey))
        .wrap_err("Failed to save package.txt")?;

    Ok(())
}

/// Rewrites Homebrew placeholder dylib paths in Mach-O binaries after linking.
///
/// Homebrew bottles contain placeholder paths like:
/// - @@HOMEBREW_CELLAR@@/pkgname/version/lib/libfoo.dylib
/// - @@HOMEBREW_PREFIX@@/opt/dependency/lib/libbar.dylib
///
/// These are rewritten to absolute paths under the environment root:
/// - @@HOMEBREW_PREFIX@@/opt/pkgname/lib/libfoo.dylib -> env_root/lib/libfoo.dylib
/// - @@HOMEBREW_CELLAR@@/pkgname/version/lib/libfoo.dylib -> env_root/lib/libfoo.dylib
///
/// This function is called after files are moved to the environment (LinkType::Move).
/// Each environment gets its own copy with paths specific to that environment.
/// Create Homebrew-style symlinks from env_root to Cellar directory.
///
/// Homebrew uses a Cellar layout where actual files are stored in:
///   Cellar/pkgname/version/bin/file
///   Cellar/pkgname/version/lib/libfile.dylib
///   Cellar/pkgname/version/share/subdir/
///
/// And symlinks are created at the top level:
///   bin/file -> ../Cellar/pkgname/version/bin/file
///   lib/libfile.dylib -> ../Cellar/pkgname/version/lib/libfile.dylib
///   lib/subdir -> ../Cellar/pkgname/version/lib/subdir
///   share/pkgname -> ../Cellar/pkgname/version/share/pkgname
///   opt/pkgname -> ../Cellar/pkgname/version
///
/// This function scans the Cellar directory and creates these top-level symlinks.
/// It should be called after files have been moved/linked to env_root/Cellar/.
///
/// Note: For brew environments, we need real directories (bin/, lib/, share/) instead
/// of usr-merge symlinks (bin -> usr/bin). This function removes usr-merge symlinks
/// and creates real directories to match vanilla Homebrew layout.
///
/// # Arguments
/// * `env_root` - Environment root directory
/// * `store_dir` - Store directory path (for finding formula file)
/// * `pkgkey` - Package key in format "{pkgname}__{version}__{arch}" (version includes bottle revision)
pub fn create_cellar_symlinks(env_root: &Path, store_dir: &Path, pkgkey: &str) -> Result<()> {
    // Parse pkgkey to get package name
    // pkgkey format: {pkgname}__{version}__{arch}
    let parts: Vec<&str> = pkgkey.rsplitn(3, "__").collect();
    if parts.len() != 3 {
        return Err(eyre::eyre!("Invalid pkgkey format, expected 3 parts: {}", pkgkey));
    }
    let pkgname = parts[2];

    // Discover the actual version from Cellar directory
    // Cellar uses version without bottle revision (e.g., "3.13.13" not "3.13.13_0")
    let version = match discover_cellar_version(env_root, pkgname) {
        Ok(v) => v,
        Err(_) => {
            log::debug!("Cellar package base not found for {}", pkgname);
            return Ok(());
        }
    };

    let cellar_pkg_dir = env_root.join("Cellar").join(pkgname).join(&version);

    // Ensure Cellar directory exists
    let cellar_dir = env_root.join("Cellar");
    if !cellar_dir.exists() {
        crate::lfs::create_dir_all(&cellar_dir)?;
    }

    // Create opt/pkgname symlink
    create_opt_package_symlink(env_root, pkgname, &version)?;

    // Special handling for portable-ruby: create Homebrew vendor symlinks
    // Homebrew expects Ruby at Homebrew/Library/Homebrew/vendor/portable-ruby/current/
    if pkgname == "portable-ruby" {
        create_portable_ruby_vendor_symlinks(env_root, &version)?;
    }

    // Create file-level symlinks for bin/, lib/, sbin/
    for dir_name in ["bin", "lib", "sbin"] {
        let env_dir = ensure_real_directory(env_root, dir_name)?;
        let cellar_subdir = cellar_pkg_dir.join(dir_name);
        if cellar_subdir.exists() {
            create_cellar_file_symlinks(&cellar_subdir, &env_dir, pkgname, &version, dir_name)?;
        }
    }

    // Handle libexec/bin/ -> top-level bin/ symlinks (unversioned commands)
    let cellar_libexec_bin = cellar_pkg_dir.join("libexec").join("bin");
    if cellar_libexec_bin.exists() {
        let env_bin_dir = ensure_real_directory(env_root, "bin")?;
        create_cellar_file_symlinks(&cellar_libexec_bin, &env_bin_dir, pkgname, &version, "libexec/bin")?;
    }

    // Handle Python site-packages symlinks
    link_python_site_packages(env_root, &cellar_pkg_dir, pkgname, &version)?;

    // On Linux, ensure lib64/ld-linux-x86-64.so.2 exists for ELF interpreter
    // The system dynamic linker path is /lib64/ld-linux-x86-64.so.2
    // Homebrew glibc provides this in Cellar/glibc/version/lib/
    #[cfg(target_os = "linux")]
    {
        // Check if glibc is installed (provides ld-linux-x86-64.so.2)
        let glibc_cellar = env_root.join("Cellar").join("glibc");
        if glibc_cellar.exists() {
            // Find glibc version
            if let Ok(glibc_version) = discover_cellar_version(env_root, "glibc") {
                let glibc_ld = glibc_cellar.join(&glibc_version).join("lib").join("ld-linux-x86-64.so.2");
                if glibc_ld.exists() {
                    // Ensure lib64 directory exists (may be usr-merge symlink)
                    let lib64_dir = ensure_real_directory(env_root, "lib64")?;

                    // Create symlink: lib64/ld-linux-x86-64.so.2 -> ../lib/ld-linux-x86-64.so.2
                    let lib64_ld = lib64_dir.join("ld-linux-x86-64.so.2");
                    if crate::lfs::symlink_metadata(&lib64_ld).is_ok() {
                        crate::lfs::remove_file(&lib64_ld)?;
                    }
                    // Use relative path to lib/ld-linux-x86-64.so.2
                    crate::lfs::symlink_file_for_virtiofs(Path::new("../lib/ld-linux-x86-64.so.2"), &lib64_ld)?;
                    log::trace!("Created lib64 ld.so symlink: {} -> ../lib/ld-linux-x86-64.so.2", lib64_ld.display());

                    // Create symlink: lib/ld.so -> ld-linux-x86-64.so.2
                    // This is needed for bottles that use @@HOMEBREW_PREFIX@@/lib/ld.so interpreter
                    // which is rewritten to /home/linuxbrew/.LB/lib/ld.so (28 chars, fits in 30-char buffer)
                    let lib_dir = env_root.join("lib");
                    if lib_dir.exists() {
                        let lib_ld_so = lib_dir.join("ld.so");
                        if crate::lfs::symlink_metadata(&lib_ld_so).is_ok() {
                            crate::lfs::remove_file(&lib_ld_so)?;
                        }
                        crate::lfs::symlink_file_for_virtiofs(Path::new("ld-linux-x86-64.so.2"), &lib_ld_so)?;
                        log::trace!("Created lib/ld.so symlink: {} -> ld-linux-x86-64.so.2", lib_ld_so.display());
                    }
                }
            }
        }
    }

    // Create directory-level symlinks for share/, Frameworks/, include/
    for dir_name in ["Frameworks", "share", "include"] {
        let env_dir = if dir_name == "share" || dir_name == "include" {
            ensure_real_directory(env_root, dir_name)?
        } else {
            env_root.join(dir_name)
        };

        if !env_dir.exists() {
            crate::lfs::create_dir_all(&env_dir)?;
        }

        let cellar_subdir = cellar_pkg_dir.join(dir_name);
        if cellar_subdir.exists() {
            create_cellar_dir_symlinks(&cellar_subdir, &env_dir, pkgname, &version, dir_name)?;
        }
    }

    log::info!("Created Cellar symlinks for {} {} in {}", pkgname, version, env_root.display());

    // Replace placeholder paths in Python configuration files
    // Python's _sysconfigdata_*.py contains @@HOMEBREW_PREFIX@@ placeholders
    // that need to be replaced for proper sys.path calculation
    replace_python_config_placeholders(env_root, pkgname, &version)?;

    // Run post_install if defined in formula
    // Uses minimal Ruby stub instead of full Homebrew Library
    crate::brew_postinstall::run_post_install(env_root, store_dir, pkgname, &version)?;

    Ok(())
}

/// Discover the version directory in Cellar for a package.
///
/// Homebrew Cellar structure: Cellar/pkgname/VERSION/
/// There should be exactly one version directory.
///
/// # Arguments
/// * `env_root` - Environment root directory
/// * `pkgname` - Package name
///
/// # Returns
/// The version string found in Cellar, or error if not found.
fn discover_cellar_version(env_root: &Path, pkgname: &str) -> Result<String> {
    let cellar_pkg_base = env_root.join("Cellar").join(pkgname);
    if !cellar_pkg_base.exists() {
        return Err(eyre::eyre!("Cellar package base not found: {}", cellar_pkg_base.display()));
    }

    for entry in std::fs::read_dir(&cellar_pkg_base)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            return Ok(entry.file_name().to_string_lossy().into_owned());
        }
    }

    Err(eyre::eyre!("No version directory found in Cellar for {}", pkgname))
}

/// Ensure a real directory exists (remove usr-merge symlink if present).
///
/// For Brew Cellar layout, we need real directories (bin/, lib/, share/)
/// instead of usr-merge symlinks (bin -> usr/bin) to properly create
/// Cellar-style symlinks.
///
/// # Arguments
/// * `env_root` - Environment root directory
/// * `dir_name` - Directory name (e.g., "bin", "lib", "share")
///
/// # Returns
/// Path to the real directory.
fn ensure_real_directory(env_root: &Path, dir_name: &str) -> Result<PathBuf> {
    let env_dir = env_root.join(dir_name);

    // Remove usr-merge symlink if present
    if crate::lfs::symlink_metadata(&env_dir)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        log::debug!("Removing usr-merge symlink {} for Cellar layout", env_dir.display());
        crate::lfs::remove_file(&env_dir)?;
    }

    // Create real directory if needed
    if !env_dir.exists() {
        crate::lfs::create_dir_all(&env_dir)?;
    }

    Ok(env_dir)
}

/// Create opt/pkgname symlink pointing to Cellar.
///
/// Homebrew-style symlink: opt/pkgname -> ../Cellar/pkgname/version
/// This allows self-reference paths like @@HOMEBREW_PREFIX@@/opt/pkgname to work.
///
/// # Arguments
/// * `env_root` - Environment root directory
/// * `pkgname` - Package name
/// * `version` - Package version from Cellar
fn create_opt_package_symlink(env_root: &Path, pkgname: &str, version: &str) -> Result<()> {
    let opt_dir = env_root.join("opt");
    if !opt_dir.exists() {
        crate::lfs::create_dir_all(&opt_dir)?;
    }

    let opt_pkg_link = opt_dir.join(pkgname);
    let opt_target = PathBuf::from("../Cellar").join(pkgname).join(version);

    // Remove existing symlink if present
    if crate::lfs::symlink_metadata(&opt_pkg_link).is_ok() {
        crate::lfs::remove_file(&opt_pkg_link)?;
    }

    crate::lfs::symlink_dir_for_virtiofs(&opt_target, &opt_pkg_link)?;
    log::trace!("Created opt symlink: {} -> {}", opt_pkg_link.display(), opt_target.display());

    Ok(())
}

/// Create Homebrew vendor symlinks for portable-ruby.
///
/// Homebrew expects Ruby at Homebrew/Library/Homebrew/vendor/portable-ruby/current/
/// This creates the directory structure and symlinks pointing to Cellar.
///
/// # Arguments
/// * `env_root` - Environment root directory
/// * `version` - portable-ruby version (e.g., "4.0.2_1")
fn create_portable_ruby_vendor_symlinks(env_root: &Path, version: &str) -> Result<()> {
    // Create Homebrew/Library/Homebrew/vendor/portable-ruby/ directory
    let vendor_dir = env_root
        .join("Homebrew")
        .join("Library")
        .join("Homebrew")
        .join("vendor")
        .join("portable-ruby");

    if !vendor_dir.exists() {
        crate::lfs::create_dir_all(&vendor_dir)?;
        log::trace!("Created portable-ruby vendor directory: {}", vendor_dir.display());
    }

    // Create version symlink: vendor/portable-ruby/version -> ../../../../../Cellar/portable-ruby/version
    // Path: env_root/Homebrew/Library/Homebrew/vendor/portable-ruby/version
    //       -> ../../../../../Cellar/portable-ruby/version (5 levels up to env_root)
    let version_link = vendor_dir.join(version);
    let cellar_target = PathBuf::from("../../../../../Cellar/portable-ruby").join(version);

    // Remove existing symlink if present
    if crate::lfs::symlink_metadata(&version_link).is_ok() {
        crate::lfs::remove_file(&version_link)?;
    }

    crate::lfs::symlink_dir_for_virtiofs(&cellar_target, &version_link)?;
    log::trace!("Created portable-ruby version symlink: {} -> {}", version_link.display(), cellar_target.display());

    // Create current symlink: vendor/portable-ruby/current -> version
    let current_link = vendor_dir.join("current");

    if crate::lfs::symlink_metadata(&current_link).is_ok() {
        crate::lfs::remove_file(&current_link)?;
    }

    crate::lfs::symlink_dir_for_virtiofs(Path::new(version), &current_link)?;
    log::trace!("Created portable-ruby current symlink: {} -> {}", current_link.display(), version);

    Ok(())
}

/// Link Python site-packages from Cellar to top-level lib/.
///
/// For Python packages in Cellar, creates symlinks from
/// Cellar/pkgname/version/lib/python3.x/site-packages/ to
/// env_root/lib/python3.x/site-packages/.
///
/// # Arguments
/// * `env_root` - Environment root directory
/// * `cellar_pkg_dir` - Cellar package directory (Cellar/pkgname/version)
/// * `pkgname` - Package name
/// * `version` - Package version
fn link_python_site_packages(env_root: &Path, cellar_pkg_dir: &Path, pkgname: &str, version: &str) -> Result<()> {
    let cellar_lib = cellar_pkg_dir.join("lib");
    if !cellar_lib.exists() || !cellar_lib.is_dir() {
        return Ok(());
    }

    for py_entry in std::fs::read_dir(&cellar_lib)?.filter_map(|e| e.ok()) {
        let py_name = py_entry.file_name();
        let py_name_str = py_name.to_string_lossy();
        if !py_name_str.starts_with("python3.") {
            continue;
        }

        let cellar_site_packages = cellar_lib.join(&py_name).join("site-packages");
        if !cellar_site_packages.exists() || !cellar_site_packages.is_dir() {
            continue;
        }

        let env_site_packages = env_root.join("lib").join(&py_name).join("site-packages");
        if !env_site_packages.exists() {
            crate::lfs::create_dir_all(&env_site_packages)?;
        }

        let py_base_dir = format!("lib/{}", py_name_str);
        create_python_site_packages_symlinks(
            &cellar_site_packages,
            &env_site_packages,
            pkgname,
            version,
            &py_base_dir,
        )?;
    }

    Ok(())
}

/// Create file-level symlinks for bin/ and lib/ directories.
///
/// Each file under cellar_dir gets a symlink at env_dir level.
/// For subdirectories under lib/, create directory symlinks.
///
/// # Arguments
/// * `cellar_dir` - Directory under Cellar (e.g., Cellar/jq/1.7.1/bin)
/// * `env_dir` - Corresponding top-level directory (e.g., env_root/bin)
/// * `pkgname` - Package name
/// * `version` - Package version
/// * `base_dir` - Base directory name (bin, lib)
fn create_cellar_file_symlinks(
    cellar_dir: &Path,
    env_dir: &Path,
    pkgname: &str,
    version: &str,
    base_dir: &str,
) -> Result<()> {
    for entry in walkdir::WalkDir::new(cellar_dir).min_depth(1).max_depth(2).into_iter().filter_map(|e| e.ok()) {
        let cellar_path = entry.path();
        let rel_path = cellar_path.strip_prefix(cellar_dir)?;

        // Create parent directory in env_dir if needed
        let env_path = env_dir.join(rel_path);
        if let Some(parent) = env_path.parent() {
            if !parent.exists() {
                crate::lfs::create_dir_all(parent)?;
            }
        }

        if entry.file_type().is_dir() {
            // For subdirectories under lib/ (e.g., lib/guile/), create directory symlink
            // lib/guile -> ../Cellar/pkgname/version/lib/guile
            // But NOT for Python version directories (python3.x) - these should be real directories
            // so that site-packages symlinks are created correctly
            if base_dir == "lib" && rel_path.components().count() == 1 {
                let dir_name = rel_path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("");

                // Skip Python version directories (python3.x)
                // These need to be real directories, not symlinks, so that
                // site-packages can contain symlinks to Cellar
                if dir_name.starts_with("python3.") {
                    // Create real directory instead of symlink
                    if !env_path.exists() {
                        crate::lfs::create_dir_all(&env_path)?;
                    }
                    continue;
                }

                // Remove existing symlink/dir if present
                if crate::lfs::symlink_metadata(&env_path).is_ok() {
                    if crate::lfs::symlink_metadata(&env_path)?.is_dir() {
                        crate::lfs::remove_dir_all(&env_path)?;
                    } else {
                        crate::lfs::remove_file(&env_path)?;
                    }
                }

                let symlink_target = PathBuf::from("..")
                    .join("Cellar")
                    .join(pkgname)
                    .join(version)
                    .join(base_dir)
                    .join(rel_path);

                crate::lfs::symlink_dir_for_virtiofs(&symlink_target, &env_path)?;
                log::trace!("Created Cellar dir symlink: {} -> {}", env_path.display(), symlink_target.display());
            }
            continue;
        }

        // For symlinks in Cellar, also create symlink pointing to Cellar (not copying symlink target)
        // This ensures symlink chain is preserved:
        // env_root/bin/python3 -> ../Cellar/pkg/version/bin/python3 -> ../Frameworks/...
        // The relative path ../Frameworks/... in Cellar resolves correctly to Cellar internal path

        // Remove existing file/symlink if present
        if crate::lfs::symlink_metadata(&env_path).is_ok() {
            crate::lfs::remove_file(&env_path)?;
        }

        // Calculate relative symlink target:
        // env_root/bin/jq -> ../Cellar/jq/1.7.1/bin/jq
        let symlink_target = PathBuf::from("..")
            .join("Cellar")
            .join(pkgname)
            .join(version)
            .join(base_dir)
            .join(rel_path);

        crate::lfs::symlink_file_for_virtiofs(&symlink_target, &env_path)?;
        log::trace!("Created Cellar symlink: {} -> {}", env_path.display(), symlink_target.display());
    }

    Ok(())
}

/// Create directory-level symlinks for share/, libexec/, Frameworks/, include/.
///
/// Each subdirectory under cellar_dir gets its own symlink at env_dir level.
/// For share/doc, share/info, share/man, share/aclocal - these are shared across
/// packages and should be handled specially (not symlinked).
///
/// # Arguments
/// * `cellar_dir` - Directory under Cellar (e.g., Cellar/git/2.53.0/share)
/// * `env_dir` - Corresponding top-level directory (e.g., env_root/share)
/// * `pkgname` - Package name
/// * `version` - Package version
/// * `base_dir` - Base directory name (share, libexec, etc.)
fn create_cellar_dir_symlinks(
    cellar_dir: &Path,
    env_dir: &Path,
    pkgname: &str,
    version: &str,
    base_dir: &str,
) -> Result<()> {
    // Directories that are shared across packages (not symlinked, files copied instead)
    // These are managed separately by the package manager
    let shared_dirs = ["doc", "info", "man", "man1", "man2", "man3", "man4", "man5", "man6", "man7", "man8", "aclocal"];

    for entry in std::fs::read_dir(cellar_dir)?.filter_map(|e| e.ok()) {
        let cellar_subdir = entry.path();
        let subdir_name = cellar_subdir.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");

        // Skip shared directories - these should be actual directories with copied files
        if shared_dirs.contains(&subdir_name) {
            // For shared directories, we copy files instead of symlinking
            // This is handled by the regular mirror_dir function
            continue;
        }

        // Skip if not a directory
        if !entry.file_type()?.is_dir() && !entry.file_type()?.is_symlink() {
            continue;
        }

        // Create symlink: share/pkgname -> ../Cellar/pkgname/version/share/pkgname
        let env_subdir = env_dir.join(subdir_name);

        // Remove existing symlink/dir if present
        if crate::lfs::symlink_metadata(&env_subdir).is_ok() {
            if crate::lfs::symlink_metadata(&env_subdir)?.file_type().is_dir() {
                crate::lfs::remove_dir_all(&env_subdir)?;
            } else {
                crate::lfs::remove_file(&env_subdir)?;
            }
        }

        let symlink_target = PathBuf::from("..")
            .join("Cellar")
            .join(pkgname)
            .join(version)
            .join(base_dir)
            .join(subdir_name);

        // Use symlink_dir for directory symlinks
        crate::lfs::symlink_dir_for_virtiofs(&symlink_target, &env_subdir)?;
        log::trace!("Created Cellar dir symlink: {} -> {}", env_subdir.display(), symlink_target.display());
    }

    Ok(())
}

/// Create file-level symlinks for Python site-packages directory.
///
/// This handles Python packages installed via Homebrew. Files in Cellar's site-packages
/// are symlinked to the top-level lib/python3.x/site-packages/ directory.
///
/// Unlike regular lib/ files, Python packages need file-level symlinks for each
/// package directory (e.g., numpy/, numpy-2.4.4.dist-info/) so Python can import them.
///
/// # Arguments
/// * `cellar_site_packages` - Cellar site-packages directory (e.g., Cellar/numpy/.../site-packages)
/// * `env_site_packages` - Top-level site-packages directory (e.g., env_root/lib/python3.13/site-packages)
/// * `pkgname` - Package name (e.g., "numpy")
/// * `version` - Package version (e.g., "2.4.4_0")
/// * `py_base_dir` - Python base dir for relative path (e.g., "lib/python3.13")
fn create_python_site_packages_symlinks(
    cellar_site_packages: &Path,
    env_site_packages: &Path,
    pkgname: &str,
    version: &str,
    py_base_dir: &str,
) -> Result<()> {
    log::debug!("create_python_site_packages_symlinks: cellar={} env={}",
                cellar_site_packages.display(), env_site_packages.display());

    // Safety check: only process if cellar_site_packages is a real directory
    if !cellar_site_packages.is_dir() {
        log::warn!("Skipping Python symlink creation: cellar_site_packages is not a real directory: {}",
                   cellar_site_packages.display());
        return Ok(());
    }

    // Scan Cellar site-packages and create symlinks for each package/file
    for entry in std::fs::read_dir(cellar_site_packages)?.filter_map(|e: std::io::Result<std::fs::DirEntry>| e.ok()) {
        let cellar_item: PathBuf = entry.path();
        let item_name: &str = cellar_item.file_name()
            .and_then(|n: &std::ffi::OsStr| n.to_str())
            .unwrap_or("");

        // Skip if name is empty
        if item_name.is_empty() {
            continue;
        }

        // Skip if cellar_item is a symlink (Cellar should have real files)
        let entry_ft: std::fs::FileType = entry.file_type()?;
        if entry_ft.is_symlink() {
            log::warn!("Skipping symlink in Cellar site-packages: {}", cellar_item.display());
            continue;
        }

        let env_item: PathBuf = env_site_packages.join(item_name);
        log::debug!("Creating Python symlink: env_item={} cellar_item={}", env_item.display(), cellar_item.display());

        // Remove existing symlink/dir if present
        if crate::lfs::symlink_metadata(&env_item).is_ok() {
            if crate::lfs::symlink_metadata(&env_item)?.file_type().is_dir() {
                crate::lfs::remove_dir_all(&env_item)?;
            } else {
                crate::lfs::remove_file(&env_item)?;
            }
        }

        // Create symlink: env_site_packages/numpy -> ../../../Cellar/numpy/.../site-packages/numpy
        // Relative path: from env_root/lib/python3.13/site-packages/numpy
        //               to env_root/Cellar/numpy/2.4.4_0/lib/python3.13/site-packages/numpy
        // Need 3 levels up: site-packages -> python3.13 -> lib -> env_root
        //                  env_root contains Cellar, so path is env_root/Cellar/...
        let symlink_target: PathBuf = PathBuf::from("..")
            .join("..")
            .join("..")
            .join("Cellar")
            .join(pkgname)
            .join(version)
            .join(py_base_dir)
            .join("site-packages")
            .join(item_name);

        // Use symlink_dir for directories (Python packages)
        // Use symlink_file for single files
        if entry_ft.is_dir() {
            crate::lfs::symlink_dir_for_virtiofs(&symlink_target, &env_item)?;
        } else {
            crate::lfs::symlink_file_for_virtiofs(&symlink_target, &env_item)?;
        }
        log::trace!("Created Python package symlink: {} -> {}", env_item.display(), symlink_target.display());
    }

    Ok(())
}

/// Rewrite dylib/interpreter paths for all binaries in the environment.
/// Rewrite dylib/interpreter paths in binary files for a Homebrew environment.
///
/// # Policy: Dynamic Linking Path Rewriting for Homebrew Bottles
///
/// Homebrew bottles contain placeholder paths (e.g., `@@HOMEBREW_PREFIX@@`) that must be
/// rewritten to the actual installation path for the binaries to work. This is because
/// bottles are built on CI machines with hardcoded paths that differ from the user's
/// installation location.
///
/// ## Linux ELF Rewriting
///
/// Rewrites two types of paths in ELF binaries:
/// 1. **PT_INTERP** (dynamic linker): Homebrew bottles use `@@HOMEBREW_PREFIX@@/lib/ld.so`
///    as the interpreter path. We rewrite this to the system's dynamic linker
///    (e.g., `/lib64/ld-linux-x86-64.so.2`).
///
/// 2. **RPATH/RUNPATH**: Library search paths containing `@@HOMEBREW_PREFIX@@` or
///    `@@HOMEBREW_CELLAR@@` are rewritten to the actual HOMEBREW_PREFIX.
///
/// Example:
/// ```
/// # Before: PT_INTERP = "@@HOMEBREW_PREFIX@@/lib/ld.so"
/// # After:  PT_INTERP = "/lib64/ld-linux-x86-64.so.2"
///
/// # Before: RPATH = "@@HOMEBREW_PREFIX@@/lib:@@HOMEBREW_CELLAR@@/gcc/14.2.0/lib"
/// # After:  RPATH = "/home/linuxbrew/.linuxbrew/lib:/home/linuxbrew/.linuxbrew/Cellar/gcc/14.2.0/lib"
/// ```
///
/// ## macOS Mach-O Rewriting
///
/// Uses `install_name_tool` to rewrite dylib load commands and rpaths in Mach-O binaries.
/// Similar placeholder replacement is performed for macOS paths.
///
/// ## Algorithm
///
/// 1. Collect all binary files from standard directories (bin/, lib/, libexec/, Frameworks/)
/// 2. Deduplicate using canonicalized paths (handles symlinks pointing to same file)
/// 3. For each binary:
///    - Parse the binary format (ELF/Mach-O)
///    - Extract current paths
///    - Rewrite placeholders to actual paths
///    - Write back modified binary
///
/// # Locking
///
/// Uses a global mutex to serialize rewrites. Parallel package installation can trigger
/// concurrent rewrites on the same environment files.
pub fn rewrite_dylib_paths_for_env(env_root: &Path) -> Result<()> {
    let _rewrite_guard = BREW_DYLIB_REWRITE_LOCK
        .lock()
        .map_err(|e| eyre::eyre!("Failed to acquire brew dylib rewrite lock: {}", e))?;

    #[cfg(target_os = "macos")]
    {
        rewrite_mach_o_dylib_paths(env_root)
    }

    #[cfg(target_os = "linux")]
    {
        rewrite_elf_interpreter_paths(env_root)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        // Other platforms: nothing to do
        Ok(())
    }
}

/// Collect binary files from standard directories.
///
/// Scans the given directories for files matching the predicate (e.g., is_elf_file, is_mach_o_file).
/// Returns a deduplicated list of canonicalized paths (handles symlinks pointing to same file).
///
/// For Brew packages, files are in Cellar/ directory with symlinks in bin/, lib/ etc.
/// We scan Cellar/ directly to find actual files for dylib rewriting.
///
/// # Arguments
/// * `env_root` - Environment root directory
/// * `dirs` - List of directory names to scan (e.g., ["Cellar", "bin", "lib", "libexec"])
/// * `is_binary` - Predicate function to check if a file is a binary of the target type
fn collect_binary_files(env_root: &Path, dirs: &[&str], is_binary: fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

    for dir_name in dirs {
        let dir = env_root.join(dir_name);
        if !dir.exists() {
            continue;
        }

        for entry in walkdir::WalkDir::new(&dir).into_iter().filter_map(|e| e.ok()) {
            let path = entry.path();

            // Skip symlinks to avoid processing the same file multiple times
            if entry.file_type().is_symlink() {
                continue;
            }

            // Check if it's a regular file and matches the binary predicate
            if path.is_file() && is_binary(path) {
                let real_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
                if seen.insert(real_path.clone()) {
                    files.push(real_path);
                }
            }
        }
    }

    files
}

/// Rewrite dylib paths in Mach-O files (macOS).
///
/// Collects all Mach-O binaries from Cellar/ directory (where actual files are stored),
/// then rewrites their dylib load commands using install_name_tool.
///
/// Note: For Brew Cellar layout, files are in Cellar/pkgname/version/bin/, lib/, etc.
/// The env_root/bin/, lib/ directories contain symlinks to Cellar, so we scan Cellar directly.
#[cfg(target_os = "macos")]
fn rewrite_mach_o_dylib_paths(env_root: &Path) -> Result<()> {
    // Collect all potential Mach-O files from Cellar directory
    // Cellar/ contains the actual binary files for brew packages
    let cellar_dir = env_root.join("Cellar");
    if !cellar_dir.exists() {
        log::debug!("No Cellar directory found in {}", env_root.display());
        return Ok(());
    }

    // Scan Cellar/ for all Mach-O files (binaries and dylibs)
    let mach_o_files = collect_binary_files(env_root, &["Cellar"], is_mach_o_file);

    if mach_o_files.is_empty() {
        log::debug!("No Mach-O files found in Cellar {}", env_root.display());
        return Ok(());
    }

    log::info!("Rewriting dylib paths in {} Mach-O files for env {}", mach_o_files.len(), env_root.display());

    for mach_o_path in &mach_o_files {
        if let Err(e) = rewrite_dylib_paths_for_file_in_env(mach_o_path, env_root) {
            log::warn!("Failed to rewrite dylib paths for {}: {}", mach_o_path.display(), e);
        }
    }

    Ok(())
}

/// Check if a file is a Mach-O binary (not a text file, etc.)
#[cfg(target_os = "macos")]
fn is_mach_o_file(path: &Path) -> bool {
    use std::io::Read;

    // Check file extension - skip common non-binary extensions
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if ["txt", "md", "json", "xml", "html", "py", "sh", "pl", "rb"].contains(&ext) {
        return false;
    }

    // Check magic number for Mach-O
    if let Ok(mut file) = std::fs::File::open(path) {
        let mut magic = [0u8; 4];
        if file.read_exact(&mut magic).is_ok() {
            // Mach-O magic numbers:
            // 0xFEEDFACE (MH_MAGIC - 32-bit little endian)
            // 0xFEEDFACF (MH_MAGIC_64 - 64-bit little endian)
            // 0xCAFEFEED (MH_BUNDLE - universal binary)
            // 0xBEBAFECA (JAVA_CLASS)
            let magic_u32 = u32::from_ne_bytes(magic);
            return magic_u32 == 0xFEEDFACE ||
                   magic_u32 == 0xFEEDFACF ||
                   magic_u32 == 0xCAFEBABE ||
                   magic == [0xCA, 0xFE, 0xBA, 0xBE];
        }
    }
    false
}

/// Rewrite dylib paths for a single Mach-O file using install_name_tool
#[cfg(target_os = "macos")]
fn rewrite_dylib_paths_for_file_in_env(mach_o_path: &Path, env_root: &Path) -> Result<()> {
    use std::process::Command;

    // Get current dylib paths using otool -L
    let output = Command::new("otool")
        .arg("-L")
        .arg(mach_o_path)
        .output()
        .wrap_err_with(|| format!("Failed to run otool -L on {}", mach_o_path.display()))?;

    if !output.status.success() {
        return Err(eyre::eyre!("otool -L failed: {}", String::from_utf8_lossy(&output.stderr)));
    }

    let homebrew_prefix = env_root.display().to_string();
    let otool_output = String::from_utf8_lossy(&output.stdout);

    // Collect all path changes needed
    let changes = collect_dylib_path_changes(&otool_output, env_root, &homebrew_prefix);

    if changes.is_empty() {
        return Ok(());
    }

    // Remove code signature before modifying Mach-O
    let _ = Command::new("codesign")
        .arg("--remove-signature")
        .arg(mach_o_path)
        .status();

    // Build install_name_tool command with all changes
    let mut install_name_cmd = Command::new("install_name_tool");

    // Handle dylib ID changes
    let (id_changed, has_placeholder, old_dylib_id) =
        handle_dylib_id_changes(&mut install_name_cmd, mach_o_path, env_root, &homebrew_prefix);

    // Skip if no placeholders and already rewritten
    if !has_placeholder && changes.is_empty() {
        log::debug!("Skipping already-rewritten file: {}", mach_o_path.display());
        return Ok(());
    }

    // Add load command changes, skipping dylib's own ID
    add_load_command_changes(&mut install_name_cmd, &changes, &old_dylib_id);

    // Execute install_name_tool
    if !changes.is_empty() || id_changed {
        install_name_cmd.arg(mach_o_path);
        let status = install_name_cmd
            .status()
            .wrap_err_with(|| format!("Failed to run install_name_tool on {}", mach_o_path.display()))?;

        if !status.success() {
            log::warn!("install_name_tool failed for {}", mach_o_path.display());
        }
    }

    Ok(())
}

/// Parse dylib paths from otool -L output and collect needed changes.
///
/// # Arguments
/// * `otool_output` - Output from otool -L
/// * `env_root` - Environment root directory
/// * `homebrew_prefix` - The HOMEBREW_PREFIX path string
#[cfg(target_os = "macos")]
fn collect_dylib_path_changes(
    otool_output: &str,
    env_root: &Path,
    homebrew_prefix: &str,
) -> Vec<(String, String)> {
    let mut changes = Vec::new();

    for line in otool_output.lines() {
        let line = line.trim();

        // Parse dylib path: "	/path/to/lib.dylib (compatibility version...)"
        if let Some(path_end) = line.find(" (") {
            let dylib_path = &line[..path_end];

            // Check for Homebrew placeholder
            for prefix in HOMEBREW_PLACEHOLDER_PREFIXES {
                if dylib_path.starts_with(prefix) {
                    if let Some(new_path) = resolve_homebrew_dylib_path_for_env(dylib_path, prefix, env_root) {
                        log::debug!("Rewriting placeholder: {} -> {}", dylib_path, new_path);
                        changes.push((dylib_path.to_string(), new_path));
                    }
                    break;
                }
            }

            // Check for Cellar path that needs rewriting
            if dylib_path.starts_with(homebrew_prefix) && dylib_path.contains("/Cellar/") {
                if let Some(new_path) = rewrite_cellar_path_to_top_level(dylib_path, env_root) {
                    log::debug!("Rewriting Cellar path: {} -> {}", dylib_path, new_path);
                    changes.push((dylib_path.to_string(), new_path));
                }
            }
        }
    }

    changes
}

/// Handle dylib ID changes from otool -D output.
///
/// Returns (id_changed, has_placeholder, old_dylib_id).
#[cfg(target_os = "macos")]
fn handle_dylib_id_changes(
    install_name_cmd: &mut std::process::Command,
    mach_o_path: &Path,
    env_root: &Path,
    homebrew_prefix: &str,
) -> (bool, bool, Option<String>) {
    use std::process::Command;

    let id_output = Command::new("otool")
        .arg("-D")
        .arg(mach_o_path)
        .output();

    let mut id_changed = false;
    let mut has_placeholder = false;
    let mut old_dylib_id: Option<String> = None;

    if let Ok(output) = id_output {
        if output.status.success() {
            let id_text = String::from_utf8_lossy(&output.stdout);

            for line in id_text.lines().skip(1) { // Skip header line
                let dylib_id = line.trim();

                // Check for placeholder presence
                if dylib_id.contains("@@HOMEBREW_PREFIX@@") || dylib_id.contains("@@HOMEBREW_CELLAR@@") {
                    has_placeholder = true;
                }

                // Handle placeholder in dylib ID
                for prefix in HOMEBREW_PLACEHOLDER_PREFIXES {
                    if dylib_id.starts_with(prefix) {
                        if let Some(new_id) = resolve_homebrew_dylib_path_for_env(dylib_id, prefix, env_root) {
                            log::debug!("Adding dylib ID change: {} -> {}", dylib_id, new_id);
                            install_name_cmd.arg("-id").arg(&new_id);
                            id_changed = true;
                        }
                        break;
                    }
                }

                // Handle Cellar path in dylib ID
                if dylib_id.starts_with(homebrew_prefix) && dylib_id.contains("/Cellar/") {
                    if let Some(new_id) = rewrite_cellar_path_to_top_level(dylib_id, env_root) {
                        log::debug!("Adding Cellar dylib ID change: {} -> {}", dylib_id, new_id);
                        install_name_cmd.arg("-id").arg(&new_id);
                        id_changed = true;
                        has_placeholder = true;
                    }
                }

                old_dylib_id = Some(dylib_id.to_string());
                break;
            }
        }
    }

    (id_changed, has_placeholder, old_dylib_id)
}

/// Add load command changes to install_name_tool command.
///
/// Skips changes that match the dylib's own ID (handled separately by -id).
#[cfg(target_os = "macos")]
fn add_load_command_changes(
    install_name_cmd: &mut std::process::Command,
    changes: &[(String, String)],
    old_dylib_id: &Option<String>,
) {
    for (old_path, new_path) in changes {
        // Skip if this is the dylib's own ID (already handled by -id)
        if let Some(ref old_id) = old_dylib_id {
            if old_path == old_id {
                log::debug!("Skipping load change for dylib ID: {}", old_path);
                continue;
            }
        }
        log::debug!("Adding load change: {} -> {}", old_path, new_path);
        install_name_cmd.arg("-change").arg(old_path).arg(new_path);
    }
}

/// Resolve a Homebrew placeholder dylib path to an absolute path under env_root.
///
/// For Cellar layout:
/// - @@HOMEBREW_CELLAR@@/jq/1.8.1/lib/libjq.1.dylib -> env_root/Cellar/jq/1.8.1/lib/libjq.1.dylib
/// - @@HOMEBREW_PREFIX@@/opt/oniguruma/lib/libonig.5.dylib -> env_root/Cellar/oniguruma/version/lib/libonig.5.dylib
#[cfg(target_os = "macos")]
fn resolve_homebrew_dylib_path_for_env(placeholder_path: &str, prefix: &str, env_root: &Path) -> Option<String> {
    // Extract the path after the placeholder prefix
    let rest = &placeholder_path[prefix.len()..];

    match prefix {
        "@@HOMEBREW_PREFIX@@" => {
            // Format: /opt/pkgname/lib/libfoo.dylib or /lib/libfoo.dylib
            // For Cellar layout, libraries are in Cellar/pkgname/version/lib/
            // Try to find in Cellar first, then fall back to env_root
            extract_lib_path_and_resolve_cellar(rest, env_root)
        }
        "@@HOMEBREW_CELLAR@@" => {
            // Format: /pkgname/version/lib/libfoo.dylib
            // The path structure is: /<pkgname>/<version>/<actual_path>
            // For Cellar layout, this directly maps to env_root/Cellar/pkgname/version/lib/
            let parts: Vec<&str> = rest.splitn(4, '/').collect();
            if parts.len() >= 4 {
                // parts[0] is empty (before first /), parts[1] is pkgname, parts[2] is version
                // parts[3] is the rest of the path like "lib/libfoo.dylib"
                let pkgname = parts[1];
                let version = parts[2];
                let rest_path = parts[3];

                // Try exact version first
                let cellar_path = env_root.join("Cellar").join(pkgname).join(version).join(rest_path);
                if cellar_path.exists() {
                    return Some(cellar_path.display().to_string());
                }

                // Try with bottle revision suffix (_0, _1, etc.)
                for rev in 0..10 {
                    let version_with_rev = format!("{}_{}", version, rev);
                    let cellar_path = env_root.join("Cellar").join(pkgname).join(&version_with_rev).join(rest_path);
                    if cellar_path.exists() {
                        return Some(cellar_path.display().to_string());
                    }
                }

                // Fall back to general resolution
                extract_lib_path_and_resolve_cellar(&format!("/{}", rest_path), env_root)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Extract library path and resolve under env_root/Cellar.
/// For Cellar layout, libraries are in Cellar/pkgname/version/lib/
#[cfg(target_os = "macos")]
fn extract_lib_path_and_resolve_cellar(rest: &str, env_root: &Path) -> Option<String> {
    // Try /lib/ path resolution
    if let Some(lib_pos) = rest.find("/lib/") {
        let lib_rel_path = &rest[lib_pos + 5..]; // "libfoo.dylib" or nested path

        // Search Cellar packages first
        if let Some(path) = search_path_in_cellar(env_root, "lib", lib_rel_path) {
            return Some(path);
        }

        // Try direct lib path under env_root
        let full_path = env_root.join("lib").join(lib_rel_path);
        if full_path.exists() {
            return Some(full_path.display().to_string());
        }
    }

    // Try /Frameworks/ path resolution
    if let Some(fw_pos) = rest.find("/Frameworks/") {
        let fw_rel_path = &rest[fw_pos + 13..]; // "Python.framework/..."

        // Search Cellar packages first
        if let Some(path) = search_path_in_cellar(env_root, "Frameworks", fw_rel_path) {
            return Some(path);
        }

        // Try env_root/Frameworks/ (symlinked directory)
        let full_path = env_root.join("Frameworks").join(fw_rel_path);
        if full_path.exists() {
            return Some(full_path.display().to_string());
        }
    }

    // Try /opt/pkgname/lib/ pattern
    if rest.starts_with("/opt/") {
        if let Some(path) = resolve_opt_package_lib(env_root, rest) {
            return Some(path);
        }
    }

    // Fallback: search by library name
    let lib_name = rest.rsplit('/').next()?;
    search_library_by_name(env_root, lib_name)
}

/// Search for a path in all Cellar packages under a specific subdirectory.
///
/// Iterates through Cellar/pkgname/version/subdir/ to find the given relative path.
///
/// # Arguments
/// * `env_root` - Environment root directory
/// * `subdir` - Subdirectory name (e.g., "lib", "Frameworks")
/// * `rel_path` - Relative path under subdir (e.g., "libfoo.dylib")
#[cfg(target_os = "macos")]
fn search_path_in_cellar(env_root: &Path, subdir: &str, rel_path: &str) -> Option<String> {
    let cellar_dir = env_root.join("Cellar");
    if !cellar_dir.exists() {
        return None;
    }

    for pkg_entry in std::fs::read_dir(&cellar_dir)?.filter_map(|e| e.ok()) {
        let pkg_dir = pkg_entry.path();
        if !pkg_dir.is_dir() {
            continue;
        }

        for ver_entry in std::fs::read_dir(&pkg_dir)?.filter_map(|e| e.ok()) {
            let ver_dir = ver_entry.path();
            let target_path = ver_dir.join(subdir).join(rel_path);
            if target_path.exists() {
                return Some(target_path.display().to_string());
            }
        }
    }

    None
}

/// Resolve /opt/pkgname/lib/ pattern to Cellar path.
///
/// # Arguments
/// * `env_root` - Environment root directory
/// * `rest` - Path after placeholder (e.g., "/opt/pkgname/lib/libfoo.dylib")
#[cfg(target_os = "macos")]
fn resolve_opt_package_lib(env_root: &Path, rest: &str) -> Option<String> {
    let parts: Vec<&str> = rest.split('/').collect();
    if parts.len() < 4 {
        return None;
    }

    // /opt/pkgname/lib/libfoo.dylib -> parts = ["", "opt", "pkgname", "lib", ...]
    let pkgname = parts[2];
    let rest_path = parts[3..].join("/");

    let cellar_pkg_dir = env_root.join("Cellar").join(pkgname);
    if !cellar_pkg_dir.exists() {
        return None;
    }

    for ver_entry in std::fs::read_dir(&cellar_pkg_dir)?.filter_map(|e| e.ok()) {
        let ver_dir = ver_entry.path();
        let lib_path = ver_dir.join(&rest_path);
        if lib_path.exists() {
            return Some(lib_path.display().to_string());
        }
    }

    None
}

/// Search for a library by name in Cellar packages and env_root/lib.
#[cfg(target_os = "macos")]
fn search_library_by_name(env_root: &Path, lib_name: &str) -> Option<String> {
    // Try env_root/lib first
    let lib_path = env_root.join("lib").join(lib_name);
    if lib_path.exists() {
        return Some(lib_path.display().to_string());
    }

    // Search Cellar packages
    search_path_in_cellar(env_root, "lib", lib_name)
}

/// Rewrite a Cellar dylib path to use top-level symlinked paths.
///
/// For Brew Cellar layout, dylib paths in binaries may reference Cellar directly.
/// This function rewrites such paths to use top-level symlinks (Frameworks/, lib/, etc.)
/// which ensures proper prefix detection for programs like Python.
///
/// Examples:
/// - Cellar/python@3.13/3.13.13_0/Frameworks/Python.framework/Versions/3.13/Python
///   -> Frameworks/Python.framework/Versions/3.13/Python
/// - Cellar/libffi/3.4.2/lib/libffi.8.dylib
///   -> lib/libffi.8.dylib
///
/// This rewrite is needed because:
/// 1. Python's prefix detection uses the dylib path to find its home
/// 2. If dylib points to Cellar, Python thinks prefix is Cellar/...
/// 3. If dylib points to top-level Frameworks/, Python correctly finds /opt/homebrew
#[cfg(target_os = "macos")]
fn rewrite_cellar_path_to_top_level(cellar_path: &str, env_root: &Path) -> Option<String> {
    // Parse path to find the part after Cellar/pkgname/version/
    // Expected format: /opt/homebrew/Cellar/pkgname/version/rest/of/path
    let homebrew_prefix = env_root.display().to_string();
    if !cellar_path.starts_with(&homebrew_prefix) {
        return None;
    }

    let after_prefix = &cellar_path[homebrew_prefix.len()..];
    if !after_prefix.starts_with("/Cellar/") {
        return None;
    }

    // Find the path after Cellar/pkgname/version/
    // Format: /Cellar/pkgname/version/rest
    let parts: Vec<&str> = after_prefix.split('/').collect();
    if parts.len() < 5 {
        // Need at least: ["", "Cellar", "pkgname", "version", "rest..."]
        return None;
    }

    // parts[0] = "", parts[1] = "Cellar", parts[2] = pkgname, parts[3] = version
    // parts[4..] = rest of path like ["Frameworks", "Python.framework", ...]
    let rest_parts: Vec<&str> = parts[4..].to_vec();

    // Check if this is a Frameworks path (should use top-level Frameworks symlink)
    if rest_parts.first() == Some(&"Frameworks") {
        let top_level_path = homebrew_prefix.clone() + "/" + &rest_parts.join("/");
        // Verify the symlink exists at top level
        let top_level_full = env_root.join(rest_parts.join("/"));
        if top_level_full.exists() || crate::lfs::symlink_metadata(&top_level_full).is_ok() {
            return Some(top_level_path);
        }
    }

    // Check if this is a lib path (should use top-level lib directory)
    if rest_parts.first() == Some(&"lib") {
        let top_level_path = homebrew_prefix.clone() + "/" + &rest_parts.join("/");
        // Verify the path exists at top level (may be symlinked)
        let top_level_full = env_root.join(rest_parts.join("/"));
        if top_level_full.exists() || crate::lfs::symlink_metadata(&top_level_full).is_ok() {
            return Some(top_level_path);
        }
    }

    // For other paths, we don't rewrite
    None
}

/// Replace placeholder paths in Python configuration files.
///
/// Python's `_sysconfigdata__darwin_darwin.py` (or similar for other platforms)
/// contains `@@HOMEBREW_PREFIX@@` placeholders that need to be replaced with the
/// actual HOMEBREW_PREFIX path. This file is used by sysconfig and site modules
/// to determine Python's paths, including site-packages directories.
///
/// Without this replacement, Python cannot find packages installed in
/// `$HOMEBREW_PREFIX/lib/python3.x/site-packages/`.
///
/// # Arguments
/// * `env_root` - Environment root directory (HOMEBREW_PREFIX)
/// * `pkgname` - Package name (e.g., "python@3.13")
/// * `version` - Package version (e.g., "3.13.13_0")
fn replace_python_config_placeholders(env_root: &Path, pkgname: &str, version: &str) -> Result<()> {
    let cellar_pkg_dir = env_root.join("Cellar").join(pkgname).join(version);

    // Find Python config files
    let config_files = find_python_config_files(&cellar_pkg_dir)?;

    if config_files.is_empty() {
        log::debug!("No Python _sysconfigdata files found for {}", pkgname);
        return Ok(());
    }

    // Replace placeholders in each config file
    let homebrew_prefix = env_root.display().to_string();
    for config_file in &config_files {
        replace_homebrew_placeholders_in_file(config_file, &homebrew_prefix)?;
    }

    Ok(())
}

/// Find Python configuration files in Cellar package directory.
///
/// Searches both framework structure (Frameworks/Python.framework) and
/// non-framework structure (lib/python3.x/) for _sysconfigdata*.py and
/// sitecustomize.py files.
///
/// # Arguments
/// * `cellar_pkg_dir` - Cellar package directory (Cellar/pkgname/version)
fn find_python_config_files(cellar_pkg_dir: &Path) -> Result<Vec<PathBuf>> {
    // Try framework structure first (macOS)
    let framework_lib = cellar_pkg_dir
        .join("Frameworks")
        .join("Python.framework")
        .join("Versions");

    if framework_lib.exists() {
        return scan_framework_for_config_files(&framework_lib);
    }

    // Non-framework structure: look in lib/python3.x/
    let lib_dir = cellar_pkg_dir.join("lib");
    if lib_dir.exists() {
        return scan_lib_for_config_files(&lib_dir);
    }

    Ok(Vec::new())
}

/// Scan framework structure for Python config files.
fn scan_framework_for_config_files(framework_lib: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    for version_entry in std::fs::read_dir(framework_lib)?.filter_map(|e| e.ok()) {
        let lib_dir = version_entry.path().join("lib");
        if lib_dir.exists() {
            files.extend(scan_lib_for_config_files(&lib_dir)?);
        }
    }

    Ok(files)
}

/// Scan lib/ directory for Python config files (_sysconfigdata*.py, sitecustomize.py).
fn scan_lib_for_config_files(lib_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    for py_entry in std::fs::read_dir(lib_dir)?.filter_map(|e| e.ok()) {
        let py_lib_dir = py_entry.path();
        let py_name = py_lib_dir.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");

        if !py_name.starts_with("python3.") {
            continue;
        }

        for config_entry in std::fs::read_dir(&py_lib_dir)?.filter_map(|e| e.ok()) {
            let config_file = config_entry.path();
            let file_name = config_file.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");

            if (file_name.starts_with("_sysconfigdata") && file_name.ends_with(".py"))
                || file_name == "sitecustomize.py"
            {
                files.push(config_file);
            }
        }
    }

    Ok(files)
}

/// Replace Homebrew placeholders in a single file.
///
/// Replaces @@HOMEBREW_PREFIX@@, @@HOMEBREW_CELLAR@@, and @@HOMEBREW_LIBRARY@@
/// with actual paths.
///
/// # Arguments
/// * `config_file` - Path to the config file
/// * `homebrew_prefix` - The actual HOMEBREW_PREFIX path
fn replace_homebrew_placeholders_in_file(config_file: &Path, homebrew_prefix: &str) -> Result<()> {
    let content = std::fs::read_to_string(config_file)
        .wrap_err_with(|| format!("Failed to read {}", config_file.display()))?;

    // Check if there are placeholders to replace
    if !content.contains("@@HOMEBREW_PREFIX@@")
        && !content.contains("@@HOMEBREW_CELLAR@@")
        && !content.contains("@@HOMEBREW_LIBRARY@@")
    {
        return Ok(());
    }

    log::debug!("Replacing placeholders in {}", config_file.display());

    let homebrew_cellar = format!("{}/Cellar", homebrew_prefix);
    let homebrew_library = format!("{}/Cellar", homebrew_prefix);

    let new_content = content
        .replace("@@HOMEBREW_PREFIX@@", homebrew_prefix)
        .replace("@@HOMEBREW_CELLAR@@", &homebrew_cellar)
        .replace("@@HOMEBREW_LIBRARY@@", &homebrew_library);

    std::fs::write(config_file, &new_content)
        .wrap_err_with(|| format!("Failed to write {}", config_file.display()))?;

    log::info!("Replaced placeholders in {}", config_file.display());
    Ok(())
}

// ============================================================================
// Linux ELF Interpreter Path Rewriting
// ============================================================================

/// Rewrite ELF interpreter paths for all binaries in the environment (Linux).
/// Homebrew Linux bottles use @@HOMEBREW_PREFIX@@ as a placeholder in the
/// ELF interpreter path (PT_INTERP). This function replaces
/// Rewrite ELF interpreter and RPATH in ELF binaries (Linux).
///
/// Collects all ELF binaries from bin/, lib/, and libexec/ directories,
/// then rewrites their PT_INTERP (interpreter) and RPATH entries.
///
/// # Policy
///
/// Homebrew Linux bottles contain placeholder paths like `@@HOMEBREW_PREFIX@@` in:
/// - PT_INTERP (dynamic linker path): typically `@@HOMEBREW_PREFIX@@/lib/ld.so`
/// - RPATH/RUNPATH: library search paths with placeholders
///
/// This function rewrites:
/// - PT_INTERP to the system's dynamic linker (e.g., `/lib64/ld-linux-x86-64.so.2`)
/// - RPATH placeholders to actual HOMEBREW_PREFIX paths
///
/// # Example
///
/// ```
/// # Before rewriting:
/// # PT_INTERP = "@@HOMEBREW_PREFIX@@/lib/ld.so"
/// # RPATH     = "@@HOMEBREW_PREFIX@@/lib:@@HOMEBREW_CELLAR@@/gcc/14.2.0/lib"
///
/// # After rewriting:
/// # PT_INTERP = "/lib64/ld-linux-x86-64.so.2"
/// # RPATH     = "/home/linuxbrew/.linuxbrew/lib:/home/linuxbrew/.linuxbrew/Cellar/gcc/14.2.0/lib"
/// ```
#[cfg(target_os = "linux")]
fn rewrite_elf_interpreter_paths(env_root: &Path) -> Result<()> {
    // Collect all ELF files from Cellar directory (Brew package layout)
    // Cellar/ contains the actual binary files for brew packages
    let cellar_dir = env_root.join("Cellar");
    if !cellar_dir.exists() {
        log::debug!("No Cellar directory found in {}", env_root.display());
        return Ok(());
    }

    // Scan Cellar/ for all ELF files (binaries and libraries)
    let elf_files = collect_binary_files(env_root, &["Cellar"], is_elf_file);

    if elf_files.is_empty() {
        log::debug!("No ELF files found in {}", env_root.display());
        return Ok(());
    }

    log::info!("Checking ELF paths in {} files for env {}", elf_files.len(), env_root.display());

    // Homebrew Linux bottles may use system interpreter /lib64/ld-linux-x86-64.so.2
    // or placeholder @@HOMEBREW_PREFIX@@/lib/ld.so.
    // Replace with short .ld.so symlink path that fits in tight buffers.
    // /home/linuxbrew/.ld.so (22 chars + null = 23 bytes) fits in 28-byte buffer (gcc-15).
    // Symlink chain: .ld.so -> lib/ld.so -> ld-linux-x86-64.so.2 -> Cellar/glibc/...
    let new_interpreter = "/home/linuxbrew/.ld.so";

    for elf_path in &elf_files {
        if let Err(e) = rewrite_elf_interpreter_for_file(elf_path, &new_interpreter) {
            log::warn!("Failed to rewrite ELF interpreter for {}: {}", elf_path.display(), e);
        }
    }

    Ok(())
}

/// Check if a file is an ELF binary.
#[cfg(target_os = "linux")]
fn is_elf_file(path: &Path) -> bool {
    use std::io::Read;

    // Check file extension - skip common non-binary extensions
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if ["txt", "md", "json", "xml", "html", "py", "sh", "pl", "rb"].contains(&ext) {
        return false;
    }

    // Check magic number for ELF
    if let Ok(mut file) = std::fs::File::open(path) {
        let mut magic = [0u8; 4];
        if file.read_exact(&mut magic).is_ok() {
            // ELF magic: 0x7F 'E' 'L' 'F'
            return magic == [0x7F, 0x45, 0x4C, 0x46];
        }
    }
    false
}

/// Rewrite ELF interpreter and RPATH for a single file using goblin.
/// Homebrew Linux bottles use @@HOMEBREW_PREFIX@@ as a placeholder in the
/// ELF interpreter path (PT_INTERP) and RPATH. This function replaces them.
#[cfg(target_os = "linux")]
/// Rewrite ELF interpreter (PT_INTERP) and RPATH for a single ELF file.
///
/// Uses the `goblin` crate to parse ELF and perform in-place modifications.
/// This is a low-level file processor called by `rewrite_elf_interpreter_paths()`.
///
/// # Arguments
/// * `elf_path` - Path to the ELF file to modify
/// * `new_interpreter` - New interpreter path (e.g., "/lib64/ld-linux-x86-64.so.2")
///
/// # Process
/// 1. Parse ELF structure using goblin
/// 2. Extract PT_INTERP segment info (offset, current string, max length)
/// 3. Extract RPATH/RUNPATH dynamic entries from .dynstr section
/// 4. Modify PT_INTERP if it contains Homebrew placeholders
/// 5. Modify RPATH entries by replacing placeholders with actual paths
/// 6. Write modified content back to file
///
/// # Safety
/// - Preserves file structure (does not resize)
/// - New strings must fit within original buffer sizes
/// - Logs warnings if new strings are too long
#[cfg(target_os = "linux")]
fn rewrite_elf_interpreter_for_file(elf_path: &Path, new_interpreter: &str) -> Result<()> {
    use goblin::elf::Elf;
    use std::io::{Seek, SeekFrom, Read, Write};

    // Read the file
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(elf_path)
        .wrap_err_with(|| format!("Failed to open ELF file {}", elf_path.display()))?;

    let mut content = Vec::new();
    file.read_to_end(&mut content)?;

    // Parse ELF - this borrows from content, so we extract info first
    let elf = match Elf::parse(&content) {
        Ok(elf) => elf,
        Err(e) => {
            log::debug!("Not a valid ELF file {}: {}", elf_path.display(), e);
            return Ok(());
        }
    };

    // Extract info before modifying content
    let rpath_info = extract_rpath_info(&elf, &content, elf_path);
    let interp_info = extract_interp_info(&elf, &content, elf_path);

    // Determine if this ELF needs Homebrew interpreter replacement.
    // Key insight: Some Homebrew bottles (like GCC) use system interpreter
    // /lib64/ld-linux-x86-64.so.2 without placeholders, but their RPATH
    // contains Homebrew placeholders that get rewritten to .LB paths.
    // These binaries need Homebrew's ld.so to resolve .LB RPATH correctly.
    // We check for:
    // 1. Interpreter has placeholder (needs rewrite)
    // 2. RPATH has placeholder OR already uses .LB prefix (needs Homebrew ld.so)
    let has_placeholder_interpreter = if let Some((_, ref old_interp, _)) = interp_info {
        HOMEBREW_PLACEHOLDER_PREFIXES.iter().any(|p| old_interp.contains(p))
    } else {
        false
    };

    let needs_homebrew_interpreter = has_placeholder_interpreter ||
        rpath_info.iter().any(|(_, rpath, _)| {
            // RPATH needs Homebrew interpreter if:
            // - Contains placeholder (will be rewritten to .LB)
            // - Already uses .LB prefix (was rewritten, needs Homebrew ld.so)
            HOMEBREW_PLACEHOLDER_PREFIXES.iter().any(|p| rpath.contains(p)) ||
            rpath.contains(HOMEBREW_SHORT_PREFIX)
        });

    // Modify interpreter if needed
    if needs_homebrew_interpreter {
        if let Some((offset, old_str, max_len)) = interp_info {
            log::trace!("Interpreter for {}: '{}' (offset={}, max_len={})", elf_path.display(), old_str, offset, max_len);
            if modify_string_in_buffer(&mut content, offset, max_len, new_interpreter) {
                log::info!("Rewrote ELF interpreter for {}: {} -> {}",
                    elf_path.display(), old_str, new_interpreter);
            } else {
                log::warn!("Failed to rewrite interpreter for {}: '{}' too long for buffer (max {})",
                    elf_path.display(), new_interpreter, max_len);
            }
        }
    }

    // Modify RPATH entries with Homebrew placeholders
    for (str_offset, old_rpath, max_len) in rpath_info {
        if HOMEBREW_PLACEHOLDER_PREFIXES.iter().any(|p| old_rpath.contains(p)) {
            // Use short prefix (18 chars) that always fits in placeholder buffer (22 chars)
            let new_rpath = replace_homebrew_placeholders_with_short_prefix(&old_rpath);
            if modify_string_in_buffer(&mut content, str_offset, max_len, &new_rpath) {
                log::info!("Rewrote ELF RPATH for {}: {} -> {}",
                    elf_path.display(), old_rpath, new_rpath);
            }
        }
    }

    // Write back to file
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&content)?;
    file.set_len(content.len() as u64)?;

    Ok(())
}

/// Modify a null-terminated string in a content buffer.
///
/// Zero-fills the buffer segment and copies the new string,
/// ensuring null termination within the max length.
///
/// # Arguments
/// * `content` - Mutable content buffer
/// * `offset` - Start offset of the string in buffer
/// * `max_len` - Maximum length of buffer segment (including null terminator)
/// * `new_str` - New string to write
///
/// # Returns
/// true if successful, false if new string too long
#[cfg(target_os = "linux")]
fn modify_string_in_buffer(content: &mut [u8], offset: usize, max_len: usize, new_str: &str) -> bool {
    let new_bytes = new_str.as_bytes();
    if new_bytes.len() + 1 > max_len {
        log::warn!("New string too long: need {} bytes, have {} bytes", new_bytes.len() + 1, max_len);
        return false;
    }

    let slice = &mut content[offset..offset + max_len];
    slice.fill(0);
    slice[..new_bytes.len()].copy_from_slice(new_bytes);
    slice[new_bytes.len()] = 0; // null terminator
    true
}

/// Replace Homebrew placeholders with short prefix that always fits in original buffer.
///
/// Uses `/home/linuxbrew/.LB` (18 chars) instead of full `/home/linuxbrew/.linuxbrew` (26 chars).
/// This is shorter than the placeholder `@@HOMEBREW_PREFIX@@` (22 chars), so it always fits
/// without needing ELF resize or worrying about overflow.
///
/// A symlink `/home/linuxbrew/.LB -> .linuxbrew` is created in both host and env,
/// making both paths work in all isolation modes including skip_namespace_isolation.
///
/// # Example
/// `@@HOMEBREW_PREFIX@@/Cellar/jq/lib` → `/home/linuxbrew/.LB/Cellar/jq/lib`
#[cfg(target_os = "linux")]
fn replace_homebrew_placeholders_with_short_prefix(s: &str) -> String {
    // Use short prefix that fits in 22-char placeholder buffer
    // /home/linuxbrew/.LB = 18 chars (fits with 4-char margin)
    const HOMEBREW_SHORT_PREFIX: &str = "/home/linuxbrew/.LB";
    let mut result = s.to_string();
    for placeholder in HOMEBREW_PLACEHOLDER_PREFIXES {
        result = result.replace(placeholder, HOMEBREW_SHORT_PREFIX);
    }
    result
}

/// Extract interpreter information from ELF.
/// Returns (offset, current_string, max_length) if PT_INTERP exists.
#[cfg(target_os = "linux")]
fn extract_interp_info(elf: &goblin::elf::Elf, content: &[u8], _elf_path: &Path) -> Option<(usize, String, usize)> {
    let interp_phdr = elf.program_headers.iter().find(|ph| ph.p_type == goblin::elf::program_header::PT_INTERP)?;

    let offset = interp_phdr.p_offset as usize;
    let filesz = interp_phdr.p_filesz as usize;

    if offset == 0 || filesz == 0 {
        return None;
    }

    let current_str = read_null_terminated_string(content, offset)?;
    Some((offset, current_str, filesz))
}

/// Extract RPATH information from ELF.
/// Returns Vec of (string_offset, current_string, max_length) for each RPATH/RUNPATH entry.
#[cfg(target_os = "linux")]
/// Extract RPATH information from ELF.
/// Returns Vec of (string_offset, current_string, max_length) for each RPATH/RUNPATH entry.
#[cfg(target_os = "linux")]
fn extract_rpath_info(elf: &goblin::elf::Elf, content: &[u8], elf_path: &Path) -> Vec<(usize, String, usize)> {
    let mut result = Vec::new();

    let dyn_section = match elf.dynamic.as_ref() {
        Some(d) => d,
        None => {
            log::debug!("No dynamic section in {}", elf_path.display());
            return result;
        }
    };

    // Find all RPATH/RUNPATH entries first
    let rpath_entries: Vec<_> = dyn_section.dyns.iter()
        .filter(|e| e.d_tag == goblin::elf::dynamic::DT_RPATH || e.d_tag == goblin::elf::dynamic::DT_RUNPATH)
        .collect();

    if rpath_entries.is_empty() {
        log::trace!("No RPATH/RUNPATH entries in {}", elf_path.display());
        return result;
    }

    // Get dynstr file offset from section headers
    let dynstr_file_offset = elf.section_headers.iter()
        .find(|sh| elf.shdr_strtab.get_at(sh.sh_name).map(|name| name == ".dynstr").unwrap_or(false))
        .map(|sh| sh.sh_offset as usize);

    match dynstr_file_offset {
        Some(offset) => {
            log::trace!("dynstr_file_offset={} for {}", offset, elf_path.display());
            for entry in &rpath_entries {
                let str_offset = offset + entry.d_val as usize;
                if let Some(current_str) = read_null_terminated_string(content, str_offset) {
                    let max_len = find_string_max_length(content, str_offset);
                    result.push((str_offset, current_str, max_len));
                }
            }
        }
        None => {
            log::debug!("No .dynstr section header in {} (sections: {})", elf_path.display(), elf.section_headers.len());
            // Fallback: use elf.dynstrtab to get strings, then search for them in content
            for entry in &rpath_entries {
                let str_idx = entry.d_val as usize;
                if let Some(rpath_str) = elf.dynstrtab.get_at(str_idx) {
                    // Search for this string in file content
                    if let Some(file_offset) = find_string_in_content(content, rpath_str) {
                        let max_len = find_string_max_length(content, file_offset);
                        result.push((file_offset, rpath_str.to_string(), max_len));
                        log::trace!("Found RPATH '{}' at offset {} by content search", rpath_str, file_offset);
                    }
                }
            }
        }
    }

    result
}

/// Find a string's offset in file content by searching for exact match.
#[cfg(target_os = "linux")]
fn find_string_in_content(content: &[u8], target: &str) -> Option<usize> {
    let target_bytes = target.as_bytes();
    content.windows(target_bytes.len())
        .position(|window| window == target_bytes)
}
/// Find the maximum length of a string at the given offset (until next string or end of content).
#[cfg(target_os = "linux")]
fn find_string_max_length(content: &[u8], offset: usize) -> usize {
    if offset >= content.len() {
        return 0;
    }
    // Find the null terminator
    let end = content[offset..].iter().position(|&b| b == 0).unwrap_or(content.len() - offset);
    // The max length includes the null terminator
    end + 1
}

/// Read a null-terminated string from a byte slice at the given offset.
#[cfg(target_os = "linux")]
fn read_null_terminated_string(content: &[u8], offset: usize) -> Option<String> {
    if offset >= content.len() {
        return None;
    }
    let end = content[offset..].iter().position(|&b| b == 0)?;
    Some(String::from_utf8_lossy(&content[offset..offset + end]).into_owned())
}
