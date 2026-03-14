use std::fs;
use std::path::Path;
use std::collections::HashMap;
use std::io::{Read, Seek};
use std::process::Command;
use tar::Archive;
use log;
use lazy_static::lazy_static;
use color_eyre::Result;
use color_eyre::eyre::{self, WrapErr};
use bzip2::read::BzDecoder;
use zstd::stream::read::Decoder as ZstdDecoder;
use zip::ZipArchive;
use regex;
use crate::utils;
use crate::lfs;

/// Separator used to combine version and build_string in pkgkey for virtual packages
/// Format: version-build_string (e.g., "1-skylake_avx512")
/// This allows us to encode build_string in the version field without conflicting with pkgkey's '__' separator
pub const VERSION_BUILD_SEPARATOR: &str = "-";

lazy_static! {
    /// Mapping from Conda package metadata fields to common field names
    /// Based on conda-package-streaming implementation and conda index.json format
    pub static ref PACKAGE_KEY_MAPPING: HashMap<&'static str, &'static str> = {
        let mut m = HashMap::new();

        // Core package metadata from index.json (conda-package-streaming standard)
        m.insert("name",            "pkgname");
        m.insert("version",         "version");
        m.insert("summary",         "summary");
        m.insert("description",     "description");
        m.insert("url",             "homepage");
        m.insert("license",         "license");
        m.insert("license_family",  "licenseFamily");
        m.insert("build",           "buildString");
        m.insert("build_number",    "buildNumber");
        m.insert("timestamp",       "buildTime");
        m.insert("size",            "size");
        m.insert("arch",            "arch");
        m.insert("platform",        "platform");
        m.insert("subdir",          "subdir");

        // Dependencies and relationships (conda-package-streaming spec)
        m.insert("depends",         "requires");
        m.insert("constrains",      "constrains");
        m.insert("track_features",  "trackFeatures");
        m.insert("features",        "features");

        // File checksums
        m.insert("md5",             "md5sum");
        m.insert("sha256",          "sha256");

        // Conda-specific fields
        m.insert("noarch",                      "noarch");
        m.insert("preferred_env",               "preferredEnv");
        //       "python_site_packages_path": "lib/python3.13t/site-packages",
        m.insert("python_site_packages_path",   "pythonSitePackagesPath");

        m
    };

    /// Scriptlet mapping for Conda packages
    /// Based on conda-package-streaming link/unlink script handling
    pub static ref SCRIPT_MAPPING: HashMap<&'static str, &'static str> = {
        let mut m = HashMap::new();

        // Conda link scripts (executed when package is installed/linked)
        // Note: Conda uses the same pre-link/post-link scripts for both installs and upgrades
        // During upgrades, PreInstall/PostInstall actions are used (not PreUpgrade/PostUpgrade)
        m.insert("pre-link.sh",     "pre_install.sh");
        m.insert("post-link.sh",    "post_install.sh");

        // Conda unlink scripts (executed when package is removed/unlinked)
        m.insert("pre-unlink.sh",   "pre_remove.sh");
        m.insert("post-unlink.sh",  "post_remove.sh");

        // Conda environment activation/deactivation scripts
        m.insert("activate.sh",     "activate.sh");
        m.insert("deactivate.sh",   "deactivate.sh");

        // Windows equivalents
        // Note: Conda uses the same pre-link/post-link scripts for both installs and upgrades
        m.insert("pre-link.bat",    "pre_install.bat");
        m.insert("post-link.bat",   "post_install.bat");
        m.insert("pre-unlink.bat",  "pre_remove.bat");
        m.insert("post-unlink.bat", "post_remove.bat");
        m.insert("activate.bat",    "activate.bat");
        m.insert("deactivate.bat",  "deactivate.bat");

        m
    };
}

/// Unpacks a Conda package to the specified directory
///
/// Conda packages can be in two formats:
/// 1. Legacy .tar.bz2 format (traditional conda packages)
/// 2. Modern .conda format (ZIP archive with separate info-*.tar.zst and pkg-*.tar.zst)
///
/// Based on conda-package-streaming implementation
pub fn unpack_package<P: AsRef<Path>>(conda_file: P, store_tmp_dir: P, pkgkey: Option<&str>) -> Result<()> {
    let conda_file = conda_file.as_ref();
    let store_tmp_dir = store_tmp_dir.as_ref();

    // Create the required directory structure following project pattern
    fs::create_dir_all(store_tmp_dir.join("fs"))?;
    fs::create_dir_all(store_tmp_dir.join("info/conda"))?;
    fs::create_dir_all(store_tmp_dir.join("info/install"))?;

    log::debug!("Unpacking Conda package: {}", conda_file.display());

    // Determine package format and extract accordingly
    // Following conda-package-streaming package detection logic
    let file_name = conda_file.file_name().and_then(|n| n.to_str()).unwrap_or("");

    if file_name.ends_with(".conda") {
        // Modern .conda format (ZIP archive with zstd-compressed tar components)
        unpack_conda_format(conda_file, store_tmp_dir)
            .wrap_err_with(|| format!("Failed to unpack .conda format: {}", conda_file.display()))?;
    } else if file_name.ends_with(".tar.bz2") {
        // Legacy .tar.bz2 format (single bzip2-compressed tar archive)
        unpack_tar_bz2_format(conda_file, store_tmp_dir)
            .wrap_err_with(|| format!("Failed to unpack .tar.bz2 format: {}", conda_file.display()))?;
    } else {
        return Err(eyre::eyre!("Unsupported Conda package format: {}", file_name));
    }

    // Generate filelist.txt following project pattern
    crate::store::create_filelist_txt(store_tmp_dir)
        .wrap_err_with(|| format!("Failed to create filelist.txt for {}", store_tmp_dir.display()))?;

    // Create package.txt from metadata
    create_package_txt(store_tmp_dir, pkgkey)
        .wrap_err_with(|| format!("Failed to create package.txt for {}", store_tmp_dir.display()))?;

    // Create scriptlets (if any)
    create_scriptlets(store_tmp_dir)
        .wrap_err_with(|| format!("Failed to create scriptlets for {}", store_tmp_dir.display()))?;

    Ok(())
}

/// Unpacks modern .conda format packages (ZIP archives with separate info/pkg components)
/// Based on conda-package-streaming.package_streaming.stream_conda_component implementation
fn unpack_conda_format<P: AsRef<Path>>(conda_file: P, store_tmp_dir: &Path) -> Result<()> {
    let conda_file = conda_file.as_ref();

    // Validate file exists and is readable
    let metadata = lfs::metadata_on_host(conda_file)
        .wrap_err_with(|| format!("Failed to read file metadata: {}", conda_file.display()))?;

    let file_size = metadata.len();
    if file_size == 0 {
        return Err(eyre::eyre!(
            "File is empty (0 bytes): {}. The download may be incomplete or the file may be corrupted.",
            conda_file.display()
        ));
    }

    // Check ZIP magic bytes (PK header) to verify it's a ZIP file
    let mut file = fs::File::open(conda_file)
        .wrap_err_with(|| format!("Failed to open file: {}", conda_file.display()))?;

    let mut magic_bytes = [0u8; 2];
    file.read_exact(&mut magic_bytes)
        .wrap_err_with(|| format!("Failed to read file header: {}", conda_file.display()))?;
    file.seek(std::io::SeekFrom::Start(0))
        .wrap_err_with(|| format!("Failed to seek to start: {}", conda_file.display()))?;

    // ZIP files start with "PK" (0x50 0x4B)
    if magic_bytes != [0x50, 0x4B] {
        return Err(eyre::eyre!(
            "File does not appear to be a valid ZIP archive (missing PK header): {}. File size: {} bytes. The file may be corrupted or incomplete.",
            conda_file.display(),
            file_size
        ));
    }

    // Try to open the ZIP archive
    // The central directory end is at the end of the file, so if it's missing, the download is incomplete
    let mut archive = match ZipArchive::new(file) {
        Ok(archive) => archive,
        Err(e) => {
            // Check if the error is about missing central directory end
            let error_msg = e.to_string();
            if error_msg.contains("central directory end") || error_msg.contains("Could not find") {
                return Err(eyre::eyre!(
                    "Incomplete .conda package file: {}\n\
                    File size: {} bytes\n\
                    The ZIP archive is missing the central directory end, which indicates the download did not complete.\n\
                    \n\
                    Solution: Delete the incomplete file and re-download the package:\n\
                    1. Delete: {}\n\
                    2. Re-run the install command to re-download the package",
                    conda_file.display(),
                    file_size,
                    conda_file.display()
                ));
            }
            return Err(eyre::eyre!(
                "Failed to open .conda archive: {}\n\
                File size: {} bytes\n\
                Error: {}\n\
                The ZIP archive may be corrupted or incomplete.",
                conda_file.display(),
                file_size,
                e
            ));
        }
    };

    // Extract stem name for component identification
    // Following conda-package-streaming logic: info-{stem}.tar.zst and pkg-{stem}.tar.zst
    let package_stem = conda_file.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("package");

    let mut info_component = None;
    let mut pkg_component = None;

    // Find info and pkg components within the ZIP
    // Based on conda-package-streaming component detection
    for i in 0..archive.len() {
        let entry = archive.by_index(i)?;
        let name = entry.name();

        if name.starts_with(&format!("info-{}", package_stem)) && name.ends_with(".tar.zst") {
            info_component = Some(name.to_string());
        } else if name.starts_with(&format!("pkg-{}", package_stem)) && name.ends_with(".tar.zst") {
            pkg_component = Some(name.to_string());
        }
    }

    // Extract info component to info/conda/
    // Following conda-package-streaming.extract.extract_stream pattern
    // Strip "info/" prefix from paths since the info component tar contains paths like "info/index.json"
    if let Some(info_name) = info_component {
        let info_reader = archive.by_name(&info_name)?;
        extract_zstd_tar_stream(info_reader, &store_tmp_dir.join("info/conda"), Some("info/"))
            .wrap_err_with(|| format!("Failed to extract info component: {} for {}", info_name, conda_file.display()))?;
    } else {
        return Err(eyre::eyre!("No info component found in .conda package"));
    }

    // Extract pkg component to fs/
    // Following conda-package-streaming component extraction logic
    if let Some(pkg_name) = pkg_component {
        let pkg_reader = archive.by_name(&pkg_name)?;
        extract_zstd_tar_stream(pkg_reader, &store_tmp_dir.join("fs"), None)
            .wrap_err_with(|| format!("Failed to extract pkg component: {} for {}", pkg_name, conda_file.display()))?;
    } else {
        return Err(eyre::eyre!("No pkg component found in .conda package"));
    }

    Ok(())
}

/// Unpacks legacy .tar.bz2 format Conda packages
/// Based on conda-package-streaming.package_streaming implementation
fn unpack_tar_bz2_format<P: AsRef<Path>>(conda_file: P, store_tmp_dir: &Path) -> Result<()> {
    let conda_file = conda_file.as_ref();

    let file = fs::File::open(conda_file)?;
    let decoder = BzDecoder::new(file);
    let mut archive = Archive::new(decoder);

    let mut entries_processed = 0;
    let mut found_index_json = false;

    // Extract all contents, following conda-package-streaming logic
    // .tar.bz2 format contains everything in a single tar archive
    for entry_result in archive.entries()? {
        let mut entry = entry_result?;
        let path = entry.path()?.to_path_buf();
        entries_processed += 1;

        log::trace!("Processing tar entry #{}: {}", entries_processed, path.display());

        // Determine target location based on path
        // Following conda-package-streaming path classification
        let target_path = if path.starts_with("info/") {
            // Metadata files go to info/conda/ (following project pattern)
            if path.ends_with("index.json") {
                found_index_json = true;
            }
            store_tmp_dir.join("info/conda").join(path.strip_prefix("info/").unwrap())
        } else {
            // Regular files go to fs/ (following project pattern)
            store_tmp_dir.join("fs").join(&path)
        };

        // Ensure parent directory exists
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Extract the file
        entry.unpack(&target_path)?;
        utils::fixup_file_permissions(&target_path);
    }

    // Verify required metadata exists
    if !found_index_json {
        return Err(eyre::eyre!("No index.json found in Conda package"));
    }

    log::debug!("Successfully unpacked .tar.bz2 Conda package with {} entries", entries_processed);
    Ok(())
}

/// Extracts a zstd-compressed tar stream
/// Based on conda-package-streaming's zstandard decompression approach
///
/// If `strip_prefix` is provided, paths starting with that prefix will have it stripped.
fn extract_zstd_tar_stream<R: Read>(reader: R, target_dir: &Path, strip_prefix: Option<&str>) -> Result<()> {
    fs::create_dir_all(target_dir)?;

    let decoder = ZstdDecoder::new(reader)
        .wrap_err("Failed to create zstd decoder")?;
    let mut archive = Archive::new(decoder);

    // Extract with proper permission handling
    for entry_result in archive.entries()? {
        let mut entry = entry_result?;
        let mut path = entry.path()?.to_path_buf();

        // Strip prefix if specified
        if let Some(prefix) = strip_prefix {
            let prefix_path = Path::new(prefix);
            if let Ok(stripped) = path.strip_prefix(prefix_path) {
                path = stripped.to_path_buf();
            }
        }

        let target_path = target_dir.join(&path);

        // Ensure parent directory exists
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Extract the file
        entry.unpack(&target_path)?;
        utils::fixup_file_permissions(&target_path);
    }

    Ok(())
}

/// Creates package.txt from Conda metadata files (index.json, etc.)
/// Based on conda-package-streaming metadata extraction approach
fn create_package_txt<P: AsRef<Path>>(store_tmp_dir: P, pkgkey: Option<&str>) -> Result<()> {
    let store_tmp_dir = store_tmp_dir.as_ref();
    let conda_info_dir = store_tmp_dir.join("info/conda");

    // Try to read index.json (primary metadata file)
    let index_json_path = conda_info_dir.join("index.json");
    let index_data: serde_json::Value = crate::io::read_json_file(&index_json_path)?;

    let mut package_fields: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    // Extract fields from index.json and map them
    // Following conda-package-streaming metadata extraction pattern
    if let Some(object) = index_data.as_object() {
        for (key, value) in object {
            let mapped_key = PACKAGE_KEY_MAPPING
                .get(key.as_str())
                .unwrap_or(&key.as_str())
                .to_string();

            let string_value = match value {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Bool(b) => b.to_string(),
                serde_json::Value::Array(arr) => {
                    // Join array elements with commas (for dependencies, etc.)
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                }
                _ => continue,
            };

            if !string_value.is_empty() {
                package_fields.insert(mapped_key, string_value);
            }
        }
    }

    // Try to read additional metadata files if they exist
    // Following conda-package-streaming metadata completeness approach
    let files_path = conda_info_dir.join("files");
    if lfs::exists_on_host(&files_path) {
        log::debug!("Found files metadata");
        // Could add file count or other file-related metadata here
    }

    let recipe_path = conda_info_dir.join("recipe");
    if lfs::exists_on_host(&recipe_path) {
        log::debug!("Found recipe metadata");
        // Could extract additional recipe information here
    }

    // Conda's version in index.json is upstream version, need append buildString to match
    // the version used in repo package and encoded in online conda package file name.
    if let Some(build_string) = package_fields.get("buildString").cloned() {
        if let Some(version) = package_fields.get_mut("version") {
            version.push_str(VERSION_BUILD_SEPARATOR);
            version.push_str(&build_string);
        }
    }

    package_fields.insert("format".to_string(), "conda".to_string());

    // Save the package.txt file using the common store function
    // Following project pattern for package.txt generation
    crate::store::save_package_txt(package_fields, store_tmp_dir, pkgkey)
        .wrap_err("Failed to save package.txt")?;

    Ok(())
}

/// Creates standardized scriptlets from Conda package scripts
/// Based on conda-package-streaming script handling and project script mapping pattern
fn create_scriptlets<P: AsRef<Path>>(store_tmp_dir: P) -> Result<()> {
    let store_tmp_dir = store_tmp_dir.as_ref();
    let conda_info_dir = store_tmp_dir.join("info/conda");
    let install_dir = store_tmp_dir.join("info/install");

    crate::utils::copy_scriptlets_by_mapping(&SCRIPT_MAPPING, &conda_info_dir, &install_dir, false)?;

    Ok(())
}

/// Detect glibc version using ldd --version
/// Returns (family, version) tuple, e.g., ("glibc", "2.35")
#[cfg(target_os = "linux")]
pub fn detect_glibc_version() -> Result<Option<(String, String)>> {
    {
        let output = match Command::new("ldd").arg("--version").output() {
            Err(_) => {
                log::debug!("Failed to execute `ldd --version`. Assuming glibc is not available.");
                return Ok(None);
            }
            Ok(output) => output,
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        if let Some(version) = parse_glibc_ldd_version(&stdout)? {
            return Ok(Some(("glibc".to_string(), version)));
        }
        Ok(None)
    }

}

/// Parse glibc version from ldd output
#[cfg(target_os = "linux")]
fn parse_glibc_ldd_version(input: &str) -> Result<Option<String>> {
    // Match patterns like "ldd (Ubuntu GLIBC 2.35-0ubuntu3.1) 2.35" or "ldd (GNU libc) 2.31" or
    // "ldd (Debian GLIBC 2.41-12) 2.41"
    // Skip content in parentheses and match version at the end
    let re = regex::Regex::new(r"\)\s*([0-9]+\.[0-9a-b.]+)").unwrap();

    if let Some(captures) = re.captures(input) {
        if let Some(version_match) = captures.get(1) {
            return Ok(Some(version_match.as_str().to_string()));
        }
    }
    Ok(None)
}

/// Detect Linux kernel version from /proc/version or uname
/// Returns version string, e.g., "5.10.102"
pub fn detect_linux_version() -> Result<Option<String>> {
    #[cfg(target_os = "linux")]
    {
        // Try /proc/version first
        if let Ok(proc_version) = fs::read_to_string("/proc/version") {
            if let Some(version) = extract_linux_version_from_proc(&proc_version) {
                return Ok(Some(version));
            }
        }

        // Fallback to uname -r
        if let Ok(output) = Command::new("uname").arg("-r").output() {
            let version_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if let Some(version) = extract_linux_version_part(&version_str) {
                return Ok(Some(version));
            }
        }
        Ok(None)
    }

    #[cfg(not(target_os = "linux"))]
    {
        Ok(None)
    }
}

/// Extract Linux version from /proc/version content
#[cfg(target_os = "linux")]
fn extract_linux_version_from_proc(proc_version: &str) -> Option<String> {
    // /proc/version format: "Linux version 5.10.102.1-microsoft-standard-WSL2 (buildd@lgw01-amd64-060) (gcc (Ubuntu 9.4.0-1ubuntu1~20.04.1) 9.4.0, GNU ld (GNU Binutils for Ubuntu) 2.34) #1 SMP Thu Mar 17 19:48:32 UTC 2022"
    // We want to extract "5.10.102.1" or at least "5.10.102"
    let re = regex::Regex::new(r"Linux version ([0-9]+\.[0-9]+(?:\.[0-9]+)*(?:\.[0-9]+)?)").ok()?;
    if let Some(captures) = re.captures(proc_version) {
        if let Some(version_match) = captures.get(1) {
            return extract_linux_version_part(version_match.as_str());
        }
    }
    None
}

/// Extract first 2-4 version components from Linux version string
/// Takes "5.10.102.1-microsoft-standard-WSL2" and returns "5.10.102.1"
#[cfg(target_os = "linux")]
fn extract_linux_version_part(version_str: &str) -> Option<String> {
    // Match up to 4 version components: major.minor.patch.patch2
    let re = regex::Regex::new(r"^([0-9]+\.[0-9]+(?:\.[0-9]+)?(?:\.[0-9]+)?)").ok()?;
    if let Some(captures) = re.captures(version_str) {
        if let Some(version_match) = captures.get(1) {
            return Some(version_match.as_str().to_string());
        }
    }
    None
}

/// Detect CUDA version using nvidia-smi or library detection
/// Returns version string, e.g., "11.8"
pub fn detect_cuda_version() -> Result<Option<String>> {
    // Try nvidia-smi first (works on musl and other systems)
    if let Ok(output) = Command::new("nvidia-smi")
        .arg("--query")
        .arg("-u")
        .arg("-x")
        .env_remove("CUDA_VISIBLE_DEVICES")
        .output()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let re = regex::Regex::new(r"<cuda_version>(.*?)</cuda_version>").unwrap();
        if let Some(captures) = re.captures(&stdout) {
            if let Some(version_match) = captures.get(1) {
                return Ok(Some(version_match.as_str().to_string()));
            }
        }
    }

    // Could add library-based detection here if needed
    // For now, return None if nvidia-smi doesn't work
    Ok(None)
}

/// Check if running on Unix-like system
pub fn is_unix() -> bool {
    cfg!(unix)
}

/// Create a virtual package Package struct
pub fn create_virtual_package(
    pkgname: &str,
    version: &str,
    build_string: Option<&str>,
) -> crate::models::Package {
    use crate::models::PackageFormat;

    // For __archspec, encode build_string in pkgkey if provided
    // pkgkey format: pkgname__version__arch
    // For __archspec with build_string, we encode it as: version@build_string
    // This avoids conflict with pkgkey's '__' separator
    let version_for_pkgkey = if let Some(build) = build_string {
        format!("{}{}{}", version, VERSION_BUILD_SEPARATOR, build)
    } else {
        version.to_string()
    };

    crate::package_cache::create_virtual_package(
        pkgname,
        version,
        Some(&version_for_pkgkey),
        PackageFormat::Conda,
    )
}

/// Detect CPU microarchitecture for __archspec
/// Returns the microarchitecture name (e.g., "skylake_avx512", "x86_64_v4", "cascadelake")
pub fn detect_archspec() -> Option<String> {
    // For now, use a simple detection based on CPU architecture
    // In the future, this could use the archspec library like rattler does
    let arch = std::env::consts::ARCH;

    // Map common architectures to generic microarchitecture names
    // This is a simplified version - a full implementation would use archspec library
    match arch {
        "x86_64" => {
            // Try to detect more specific microarchitecture via /proc/cpuinfo
            #[cfg(target_os = "linux")]
            {
                if let Ok(cpuinfo) = fs::read_to_string("/proc/cpuinfo") {
                    // Look for model name or flags that indicate specific microarchitecture
                    for line in cpuinfo.lines() {
                        if line.starts_with("model name") {
                            let model = line.split(':').nth(1)?.trim().to_lowercase();
                            // Try to extract microarchitecture from model name
                            if model.contains("skylake") || model.contains("skx") {
                                return Some("skylake_avx512".to_string());
                            }
                            if model.contains("cascade") {
                                return Some("cascadelake".to_string());
                            }
                            if model.contains("sapphire") {
                                return Some("sapphirerapids".to_string());
                            }
                            if model.contains("ice lake") || model.contains("icelake") {
                                return Some("icelake".to_string());
                            }
                            if model.contains("zen") {
                                // Try to detect Zen generation
                                if model.contains("zen 4") || model.contains("zen4") {
                                    return Some("zen4".to_string());
                                }
                                if model.contains("zen 3") || model.contains("zen3") {
                                    return Some("zen3".to_string());
                                }
                                if model.contains("zen 2") || model.contains("zen2") {
                                    return Some("zen2".to_string());
                                }
                            }
                        }
                        // Check for AVX-512 support
                        if line.starts_with("flags") && line.contains("avx512") {
                            // Default to x86_64_v4 if AVX-512 is available
                            return Some("x86_64_v4".to_string());
                        }
                    }
                }
            }
            // Fallback to generic x86_64
            Some("x86_64".to_string())
        }
        "aarch64" | "arm64" => Some("aarch64".to_string()),
        "powerpc64le" => Some("power10le".to_string()),
        _ => Some(arch.to_string()),
    }
}

/// Detect and create virtual packages for Conda
/// Returns a vector of Package structs for detected virtual packages
pub fn detect_conda_virtual_packages() -> Result<Vec<crate::models::Package>> {
    let mut virtual_packages = Vec::new();

    // Detect __unix (always available on Unix systems)
    if is_unix() {
        virtual_packages.push(create_virtual_package("__unix", "0", None));
    }

    // Detect __linux
    if let Ok(Some(linux_version)) = detect_linux_version() {
        virtual_packages.push(create_virtual_package("__linux", &linux_version, None));
        log::debug!("Detected __linux version: {}", linux_version);
    }

    // Detect __glibc (only on Linux)
    #[cfg(target_os = "linux")]
    {
        if let Ok(Some((_family, version))) = detect_glibc_version() {
            // Virtual package name is __glibc (lowercase family name with __ prefix)
            virtual_packages.push(create_virtual_package("__glibc", &version, None));
            log::debug!("Detected __glibc version: {}", version);
        }
    }

    // Detect __cuda
    if let Ok(Some(cuda_version)) = detect_cuda_version() {
        virtual_packages.push(create_virtual_package("__cuda", &cuda_version, None));
        log::debug!("Detected __cuda version: {}", cuda_version);
    }

    // Detect __archspec
    if let Some(archspec_name) = detect_archspec() {
        virtual_packages.push(create_virtual_package("__archspec", "1", Some(&archspec_name)));
        log::debug!("Detected __archspec: {}", archspec_name);
    }

    log::info!("Detected {} Conda virtual packages", virtual_packages.len());
    Ok(virtual_packages)
}
