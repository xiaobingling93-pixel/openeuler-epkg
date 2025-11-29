use lazy_static::lazy_static;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::io::{Read, Write};
use std::fs::OpenOptions;
use color_eyre::eyre::Result;
use color_eyre::eyre;
use flate2::read::GzDecoder;
use tar::Archive;
use zstd::stream::write::Encoder as ZstdEncoder;
use zstd::stream::read::Decoder as ZstdDecoder;
use sha2::{Sha256, Digest};
use std::fs;
use hex;
use liblzma::read::XzDecoder;

use crate::models::*;
use crate::repo::RepoReleaseItem;
use crate::packages_stream;
use crate::repo;
use crate::utils;

// wfg /c/os/archlinux/repodata/core.files% g -h '^%.*%$' */desc|sc
//     266 %VERSION%
//     266 %URL%
//     266 %SHA256SUM%
//     266 %PGPSIG%
//     266 %PACKAGER%
//     266 %NAME%
//     266 %LICENSE%
//     266 %ISIZE%
//     266 %FILENAME%
//     266 %DESC%
//     266 %CSIZE%
//     266 %BUILDDATE%
//     266 %BASE%
//     266 %ARCH%
//     244 %DEPENDS%
//     187 %MAKEDEPENDS%
//     115 %PROVIDES%
//      48 %OPTDEPENDS%
//      47 %CHECKDEPENDS%
//      40 %REPLACES%
//      35 %MD5SUM%
//      34 %CONFLICTS%

lazy_static! {
    pub static ref PACKAGE_KEY_MAPPING: std::collections::HashMap<&'static str, &'static str> = {
        let mut m = std::collections::HashMap::new();

        // Map Arch Linux package fields to common field names
        // Based on the desc file format specification
        m.insert("FILENAME",        "location");        // filename
        m.insert("NAME",            "pkgname");         // package name
        m.insert("BASE",            "source");          // base package name
        m.insert("VERSION",         "version");         // package version
        m.insert("DESC",            "summary");         // description
        m.insert("CSIZE",           "size");            // compressed size
        m.insert("ISIZE",           "installedSize");   // installed size
        m.insert("SHA256SUM",       "sha256");          // SHA256 checksum
        m.insert("PGPSIG",          "pgpSig");          // PGP signature
        m.insert("URL",             "homepage");        // homepage URL
        m.insert("LICENSE",         "license");         // license
        m.insert("ARCH",            "arch");            // architecture
        m.insert("BUILDDATE",       "buildTime");       // build time
        m.insert("PACKAGER",        "maintainer");      // packager/maintainer
        m.insert("DEPENDS",         "requires");        // dependencies
        m.insert("OPTDEPENDS",      "suggests");        // optional dependencies
        m.insert("MAKEDEPENDS",     "buildRequires");   // build dependencies
        m.insert("CHECKDEPENDS",    "checkRequires");   // check dependencies
        m.insert("PROVIDES",        "provides");        // provides
        m.insert("CONFLICTS",       "conflicts");       // conflicts
        m.insert("REPLACES",        "replaces");        // replaces

        m
    };
}

/// Generic decoder that can handle gz, xz, and zst compression
enum GenericDecoder<R: Read> {
    Gz(GzDecoder<R>),
    Xz(XzDecoder<R>),
    Zst(Box<dyn Read>),
}

impl<R: Read> Read for GenericDecoder<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            GenericDecoder::Gz(decoder) => decoder.read(buf),
            GenericDecoder::Xz(decoder) => decoder.read(buf),
            GenericDecoder::Zst(decoder) => decoder.read(buf),
        }
    }
}

/// Determine compression type based on file extension
fn get_compression_type(filename: &str) -> &'static str {
    if filename.ends_with(".zst") {
        "zst"
    } else if filename.ends_with(".xz") {
        "xz"
    } else if filename.ends_with(".gz") {
        "gz"
    } else {
        "gz" // default to gz for backward compatibility
    }
}

/// Create appropriate decoder based on compression type
fn create_decoder<R: Read + 'static>(reader: R, compression_type: &str) -> Result<GenericDecoder<R>> {
    match compression_type {
        "zst" => {
            log::debug!("Creating zstd decoder");
            let boxed_reader: Box<dyn Read> = Box::new(reader);
            Ok(GenericDecoder::Zst(Box::new(ZstdDecoder::new(boxed_reader)?)))
        }
        "xz" => {
            log::debug!("Creating xz decoder");
            Ok(GenericDecoder::Xz(XzDecoder::new(reader)))
        }
        "gz" => {
            log::debug!("Creating gzip decoder");
            Ok(GenericDecoder::Gz(GzDecoder::new(reader)))
        }
        _ => Err(eyre::eyre!("Unsupported compression type: {}", compression_type))
    }
}

pub fn process_packages_content(data_rx: Receiver<Vec<u8>>, repo_dir: &PathBuf, revise: &RepoReleaseItem) -> Result<PackagesFileInfo> {
    log::debug!("Starting to process Arch packages content for {} (hash: {}, size: {})", revise.location, revise.hash, revise.size);

    // Validate download path
    validate_download_path(revise)?;

    // Initialize files
    let filelists_path = repo_dir.join("filelists.txt.zst");
    if filelists_path.exists() {
        std::fs::remove_file(&filelists_path)
            .map_err(|e| eyre::eyre!("Failed to remove existing filelists.txt.zst: {}", e))?;
    }

    let mut derived_files = packages_stream::PackagesStreamline::new(revise, repo_dir, process_line)
        .map_err(|e| eyre::eyre!("Failed to initialize PackagesStreamline for {}: {}", revise.location, e))?;

    // Create streaming reader from receiver with hash validation
    log::debug!("Creating ReceiverHasher with hash='{}', size={}", revise.hash, revise.size);
    let receiver_reader = packages_stream::ReceiverHasher::new_with_size(
        data_rx,
        revise.hash.clone(),
        revise.size.try_into().map_err(|e| eyre::eyre!("Failed to convert size {} to u64: {}", revise.size, e))?
    );

    // Determine compression type and create appropriate decoder
    let compression_type = get_compression_type(&revise.location);
    log::debug!("Detected compression type: {} for {}", compression_type, revise.location);
    let decoder = create_decoder(receiver_reader, compression_type)?;

    log::debug!("Creating tar archive for {}", revise.location);
    let mut archive = Archive::new(decoder);
    archive.set_ignore_zeros(true);

    // Create zstd encoder for filelists compression
    let filelists_file_handle = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&filelists_path)
        .map_err(|e| eyre::eyre!("Failed to open filelists.txt.zst: {}", e))?;

    let mut filelists_encoder = ZstdEncoder::new(filelists_file_handle, 3)?; // compression level 3

    // Process tar entries
    let mut packages: std::collections::HashMap<String, PackageFiles> = std::collections::HashMap::new();
    process_tar_entries(&mut archive, &mut packages, &mut filelists_encoder, &mut derived_files, revise)?;

    // Process any remaining incomplete packages (desc only)
    for (package_name, package_files) in packages {
        if package_files.desc.is_some() {
            process_complete_package(&package_files, &mut filelists_encoder, &mut derived_files)
                .map_err(|e| eyre::eyre!("Failed to process remaining package {}: {}", package_name, e))?;
            log::debug!("Processed remaining package: {}", package_name);
        }
    }

    // Finalize processing
    finalize_processing(filelists_encoder, &mut derived_files, repo_dir, revise)
}

/// Validate download path for already downloaded files
fn validate_download_path(revise: &RepoReleaseItem) -> Result<()> {
    // Check if the download path exists and is readable (for already downloaded files)
    if !revise.need_download && revise.need_convert {
        log::debug!("Processing already downloaded file: {}", revise.download_path.display());
        if !revise.download_path.exists() {
            return Err(eyre::eyre!("Downloaded file does not exist: {}", revise.download_path.display()));
        }

        let metadata = std::fs::metadata(&revise.download_path)
            .map_err(|e| eyre::eyre!("Failed to get metadata for {}: {}", revise.download_path.display(), e))?;

        if metadata.len() == 0 {
            return Err(eyre::eyre!("Downloaded file is empty: {}", revise.download_path.display()));
        }

        log::debug!("Downloaded file size: {} bytes", metadata.len());
    }

    Ok(())
}

/// Process tar entries
///
/// This function does more than just extract tar entries - it performs intelligent
/// package processing by:
///
/// 1. **Path filtering**: Only processes entries matching "package-name/desc" or
///    "package-name/files" patterns. Example tar file contents: one dir and 1-2 files per package
///        ...
///        drwxr-xr-x polyzen/users      0 2025-04-15 04:26 alsa-lib-1.2.14-1/
///        -rw-r--r-- polyzen/users    703 2025-04-15 04:26 alsa-lib-1.2.14-1/desc
///        -rw-r--r-- polyzen/users   4862 2025-04-15 04:26 alsa-lib-1.2.14-1/files
///        drwxr-xr-x polyzen/users      0 2024-07-12 06:19 alsa-oss-1.1.8-6/
///        -rw-r--r-- polyzen/users    702 2024-07-12 06:19 alsa-oss-1.1.8-6/desc
///        -rw-r--r-- polyzen/users    320 2024-07-12 06:19 alsa-oss-1.1.8-6/files
///        ...
/// 2. **Content extraction**: Reads the full content of desc and files entries
/// 3. **Package accumulation**: Collects desc and files content for each package
///    in a HashMap, handling packages that may have their entries split across
///    multiple tar entries
/// 4. **Immediate processing**: As soon as both desc and files are available for
///    a package, it immediately processes the complete package (converts to
///    packages.txt format and extracts file lists), then removes it from memory
///    to optimize memory usage during streaming
/// 5. **Error resilience**: Continues processing even if individual entries fail,
///    logging errors but not stopping the entire process
///
/// This streaming approach allows processing large repository archives without
/// loading everything into memory at once.
fn process_tar_entries(
    archive: &mut Archive<GenericDecoder<packages_stream::ReceiverHasher>>,
    packages: &mut std::collections::HashMap<String, PackageFiles>,
    filelists_encoder: &mut ZstdEncoder<std::fs::File>,
    derived_files: &mut packages_stream::PackagesStreamline,
    revise: &RepoReleaseItem,
) -> Result<()> {
    let mut entry_count = 0;
    log::debug!("Starting to process tar entries for {}", revise.location);

    let entries = archive.entries()
        .map_err(|e| eyre::eyre!("Failed to create tar entries iterator for {}: {}", revise.location, e))?;

    let full_path = &revise.download_path;

    for entry_result in entries {
        entry_count += 1;
        let mut entry = match entry_result {
            Ok(entry) => entry,
            Err(e) => {
                log::error!("Failed to read tar entry #{} for {}: {}", entry_count, revise.location, e);
                utils::mark_file_bad(&full_path)?;
                return Err(eyre::eyre!("Failed to read tar entry #{} for {}: {} (file marked as .bad)",
                    entry_count, revise.location, e));
            }
        };

        let path = match entry.path() {
            Ok(path) => path.display().to_string(),
            Err(e) => {
                log::error!("Failed to get path for tar entry #{} for {}: {}", entry_count, revise.location, e);
                utils::mark_file_bad(&full_path)?;
                return Err(eyre::eyre!("Failed to get path for tar entry #{} for {}: {} (file marked as .bad)",
                    entry_count, revise.location, e));
            }
        };

        log::trace!("Processing tar entry #{}: {}", entry_count, path);

        // Parse path: package-name/desc or package-name/files
        let path_parts: Vec<&str> = path.split('/').collect();
        log::trace!("Path parts: {:?} (length: {})", path_parts, path_parts.len());

        if path_parts.len() == 2 {
            let package_name = path_parts[0].to_string();
            let file_type = path_parts[1];

            // Only process desc and files, skip directories
            if file_type == "desc" || file_type == "files" {
                log::trace!("Found package file: {} -> {} (type: {})", path, package_name, file_type);

                let mut content = String::new();
                match entry.read_to_string(&mut content) {
                    Ok(_) => {
                        log::trace!("Read {} bytes from {}/{}", content.len(), package_name, file_type);
                    }
                    Err(e) => {
                        log::error!("Failed to read content from {}/{}: {}", package_name, file_type, e.to_string());
                        // If it's a corrupt compression stream, mark the entire file as bad
                        if e.to_string().contains("corrupt deflate stream") || e.to_string().contains("corrupt xz stream") {
                            utils::mark_file_bad(&full_path)?;
                            return Err(eyre::eyre!("Corrupt compression stream detected in {}/{} for {}: {} (file marked as .bad)",
                                package_name, file_type, revise.location, e));
                        }
                        continue;
                    }
                }

                let package_files = packages.entry(package_name.clone()).or_insert(PackageFiles::default());

                if file_type == "desc" {
                    package_files.desc = Some(content);
                    log::trace!("Set desc for package: {}", package_name);
                } else if file_type == "files" {
                    package_files.files = Some(content);
                    log::trace!("Set files for package: {}", package_name);
                }

                // Process package immediately if complete
                if package_files.desc.is_some() && package_files.files.is_some() {
                    let complete_package = packages.remove(&package_name).unwrap();
                    log::trace!("Processing complete package: {}", package_name);
                    match process_complete_package(&complete_package, filelists_encoder, derived_files) {
                        Ok(_) => log::trace!("Successfully processed complete package: {}", package_name),
                        Err(e) => {
                            log::error!("Failed to process complete package {}: {}", package_name, e);
                            return Err(eyre::eyre!("Failed to process package {}: {}", package_name, e));
                        }
                    }
                }
            }
        } else {
            log::trace!("Skipping entry (doesn't match pattern): {}", path);
        }
    }

    log::info!("Processed {} total tar entries", entry_count);
    Ok(())
}

/// Finalize processing
///
/// This function completes the package processing pipeline and generates final outputs:
///
/// **What it does:**
/// 1. **Last package indexing**: Ensures any remaining package in the pipeline gets
///    properly indexed and written to packages.txt
/// 2. **Compression finalization**: Closes the zstd encoder, finalizing the compressed
///    filelists.txt.zst file
/// 3. **Hash calculation**: Computes SHA-256 hash of the final filelists.txt.zst file
///    for integrity verification
/// 4. **Metadata generation**: Creates and writes filelists metadata file containing
///    hash and other repository metadata
/// 5. **Pipeline completion**: Calls the derived_files.on_finish() to complete the
///    packages.txt processing and cleanup
///
/// **Outputs generated:**
/// - `packages.txt.zst`: Compressed package metadata in standard repository format
/// - `filelists.txt.zst`: Compressed file listings for all packages
///
/// **Returns:** PackagesFileInfo containing details about the processed repository files
fn finalize_processing(
    filelists_encoder: ZstdEncoder<std::fs::File>,
    derived_files: &mut packages_stream::PackagesStreamline,
    repo_dir: &PathBuf,
    revise: &RepoReleaseItem
) -> Result<PackagesFileInfo> {
    // Ensure the last package gets indexed
    if !derived_files.current_pkgname.is_empty() {
        log::debug!("Finalizing last package: {}", derived_files.current_pkgname);
        derived_files.on_new_paragraph();
    }

    log::debug!("Finalizing processing for {}", revise.location);

    // Finish compression and get the file handle back
    filelists_encoder.finish()
        .map_err(|e| eyre::eyre!("Failed to finish filelists compression: {}", e))?;

    // Calculate SHA-256 hash of the filelists file
    let filelists_path = repo_dir.join("filelists.txt.zst");
    let filelists_content = fs::read(&filelists_path)
        .map_err(|e| eyre::eyre!("Failed to read filelists file for hash calculation: {}", e))?;
    let mut hasher = Sha256::new();
    hasher.update(&filelists_content);
    let calculated_hash = hex::encode(hasher.finalize());

    // Generate and write metadata for filelists
    repo::generate_and_write_filelists_metadata(&filelists_path, calculated_hash)
        .map_err(|e| eyre::eyre!("Failed to generate filelists metadata: {}", e))?;

    // Finalize processing
    derived_files.on_finish(revise)
        .map_err(|e| eyre::eyre!("Failed to finalize processing for {}: {}", revise.location, e))
}

#[derive(Default)]
struct PackageFiles {
    desc: Option<String>,
    files: Option<String>,
}

/// Process a complete package
fn process_complete_package(
    package_files: &PackageFiles,
    filelists_encoder: &mut ZstdEncoder<std::fs::File>,
    derived_files: &mut packages_stream::PackagesStreamline
) -> Result<()> {
    // Extract actual package name from desc content
    let mut actual_package_name = String::new();

    // Process desc content for packages.txt
    if let Some(desc_content) = &package_files.desc {
        process_desc_to_packages(desc_content, derived_files)?;
        actual_package_name = derived_files.current_pkgname.clone();
    }

    // Process files content for filelists.txt
    if let Some(files_content) = &package_files.files {
        if !actual_package_name.is_empty() {
            process_files_to_filelists(&actual_package_name, files_content, filelists_encoder)?;
        }
    }

    Ok(())
}

/// Process files content and write to filelists.txt
fn process_files_to_filelists(package_name: &str, files_content: &str, filelists_encoder: &mut ZstdEncoder<std::fs::File>) -> Result<()> {
    let mut in_files_section = false;
    let mut files_to_write = Vec::new();

    for line in files_content.lines() {
        let line = line.trim();

        if line == "%FILES%" {
            in_files_section = true;
            continue;
        }

        if in_files_section && !line.is_empty() {
            // Skip directories (lines ending with /)
            if !line.ends_with('/') {
                files_to_write.push(format!("{} {}\n", package_name, line));
            }
        }
    }

    // Write all files for this package
    for file_line in &files_to_write {
        filelists_encoder.write_all(file_line.as_bytes())
            .map_err(|e| eyre::eyre!("Failed to write to filelists.txt.zst: {}", e))?;
    }

    // Add blank line after package
    if !files_to_write.is_empty() {
        filelists_encoder.write_all(b"\n")
            .map_err(|e| eyre::eyre!("Failed to write separator to filelists.txt.zst: {}", e))?;
    }

    log::trace!("Wrote {} files for package: {}", files_to_write.len(), package_name);
    Ok(())
}

/// Helper function to process a desc section and handle special fields
fn process_desc_section(
    section_name: &str,
    section_content: &[String],
    derived_files: &mut packages_stream::PackagesStreamline
) -> Result<()> {
    if let Some(mapped_key) = PACKAGE_KEY_MAPPING.get(section_name) {
        if !mapped_key.is_empty() {
            let value = if section_content.len() == 1 {
                &section_content[0]
            } else {
                // For multi-line sections, join with spaces
                &section_content.join(" ")
            };

            derived_files.output.push_str(&format!("{}: {}\n", mapped_key, value));

            // Handle special processing for certain fields
            match section_name {
                "NAME" => {
                    derived_files.on_new_pkgname(value);
                }
                "PROVIDES" => {
                    // on_provides handles parsing internally
                    let provides_str = section_content.join(" ");
                    if !provides_str.trim().is_empty() {
                        derived_files.on_provides(&provides_str, PackageFormat::Pacman);
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// Process desc content and write to packages.txt
fn process_desc_to_packages(desc_content: &str, derived_files: &mut packages_stream::PackagesStreamline) -> Result<()> {
    let mut current_section = String::new();
    let mut section_content = Vec::new();

    // Add blank line to start package
    derived_files.output.push_str("\n");
    derived_files.on_new_paragraph();

    for line in desc_content.lines() {
        let line = line.trim();

        if line.starts_with('%') && line.ends_with('%') {
            // Process previous section if any
            if !current_section.is_empty() && !section_content.is_empty() {
                process_desc_section(current_section.as_str(), &section_content, derived_files)?;
            }

            // Start new section
            current_section = line[1..line.len()-1].to_string(); // Remove % markers
            section_content.clear();
        } else if !line.is_empty() {
            section_content.push(line.to_string());
        }
    }

    // Process the last section
    if !current_section.is_empty() && !section_content.is_empty() {
        process_desc_section(current_section.as_str(), &section_content, derived_files)?;
    }

    // Flush the accumulated output to packages.txt file
    derived_files.on_output()
        .map_err(|e| eyre::eyre!("Failed to write package output: {}", e))?;

    log::trace!("Processed package for derived_files");
    Ok(())
}

/// Helper function to process a single line (placeholder for consistency)
fn process_line(_line: &str, _derived_files: &mut packages_stream::PackagesStreamline) -> Result<()> {
    // This function is required by PackagesStreamline but for Arch Linux packages,
    // we process the data differently (by parsing tar entries rather than line by line)
    // so this is mostly a placeholder that handles empty lines to trigger paragraph breaks
    Ok(())
}

