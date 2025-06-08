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
use sha2::{Sha256, Digest};
use std::fs;
use hex;

use crate::models::*;
use crate::repo::RepoReleaseItem;
use crate::packages_stream;
use crate::repo;

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
        m.insert("PROVIDES",        "provides");        // provides
        m.insert("CONFLICTS",       "conflicts");       // conflicts
        m.insert("REPLACES",        "replaces");        // replaces

        m
    };
}

pub fn process_packages_content(data_rx: Receiver<Vec<u8>>, repo_dir: &PathBuf, revise: &RepoReleaseItem) -> Result<FileInfo> {
    log::debug!("Starting to process Arch packages content for {} (hash: {}, size: {})", revise.location, revise.hash, revise.size);

    // Initialize files
    let filelists_path = repo_dir.join("filelists.txt.zst");
    if filelists_path.exists() {
        std::fs::remove_file(&filelists_path)
            .map_err(|e| eyre::eyre!("Failed to remove existing filelists.txt.zst: {}", e))?;
    }

    let mut derived_files = packages_stream::PackagesStreamline::new(revise, repo_dir, process_line)
        .map_err(|e| eyre::eyre!("Failed to initialize PackagesStreamline for {}: {}", revise.location, e))?;

    // Create streaming reader from receiver with hash validation
    let receiver_reader = packages_stream::ReceiverHasher::new_with_size(
        data_rx,
        revise.hash.clone(),
        revise.size.try_into().unwrap()
    );
    let gz_decoder = GzDecoder::new(receiver_reader);
    let mut archive = Archive::new(gz_decoder);
    archive.set_ignore_zeros(true);

    // Create zstd encoder for filelists compression
    let filelists_file_handle = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&filelists_path)
        .map_err(|e| eyre::eyre!("Failed to open filelists.txt.zst: {}", e))?;

    let mut filelists_encoder = ZstdEncoder::new(filelists_file_handle, 3)?; // compression level 3

    let mut packages: std::collections::HashMap<String, PackageFiles> = std::collections::HashMap::new();

    // Process tar entries as they stream in
    let mut entry_count = 0;
    for entry_result in archive.entries()? {
        entry_count += 1;
        let mut entry = entry_result?;
        let path = entry.path()?.display().to_string();
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
                        log::error!("Failed to read content from {}/{}: {}", package_name, file_type, e);
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
                    match process_complete_package(&complete_package, &mut filelists_encoder, &mut derived_files) {
                        Ok(_) => log::trace!("Successfully processed complete package: {}", package_name),
                        Err(e) => log::error!("Failed to process complete package {}: {}", package_name, e),
                    }
                }
            }
        } else {
            log::trace!("Skipping entry (doesn't match pattern): {}", path);
        }
    }

    log::info!("Processed {} total tar entries", entry_count);

    // Process any remaining incomplete packages (desc only)
    for (package_name, package_files) in packages {
        if package_files.desc.is_some() {
            process_complete_package(&package_files, &mut filelists_encoder, &mut derived_files)?;
            log::debug!("Processed complete package: {}", package_name);
        }
    }

    log::debug!("Finalizing processing for {}", revise.location);

    // Finish compression and get the file handle back
    filelists_encoder.finish()?;

    // Calculate SHA-256 hash of the filelists file
    let filelists_path = repo_dir.join("filelists.txt.zst");
    let filelists_content = fs::read(&filelists_path)?;
    let mut hasher = Sha256::new();
    hasher.update(&filelists_content);
    let calculated_hash = hex::encode(hasher.finalize());

    // Generate and write metadata for filelists
    repo::generate_and_write_filelists_metadata(&filelists_path, calculated_hash)?;

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
                    // Split provides by spaces and clean up version requirements
                    let provides: Vec<&str> = section_content.iter()
                        .flat_map(|line| line.split_whitespace())
                        .filter(|s| !s.is_empty())
                        .map(|s| {
                            // Remove version part after >, <, >=, <=, =
                            if let Some(i) = s.find('>') {
                                &s[..i]
                            } else if let Some(i) = s.find('<') {
                                &s[..i]
                            } else if let Some(i) = s.find('=') {
                                &s[..i]
                            } else {
                                s
                            }
                        })
                        .collect();

                    if !provides.is_empty() {
                        derived_files.on_provides(provides);
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

