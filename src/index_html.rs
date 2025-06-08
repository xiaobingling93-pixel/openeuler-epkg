use std::path::PathBuf;
use color_eyre::eyre::Result;
use color_eyre::eyre;
use color_eyre::eyre::WrapErr;
use std::fs;
use regex::Regex;

use crate::models::*;
use crate::dirs;
use crate::repo::*;
use crate::packages_stream;
use crate::download::download_urls;

/*
 * REPOSITORY ARCHITECTURE OVERVIEW - HTML Directory Index Handler
 *
 * This module handles Type 3 repositories in epkg's three-tier repository architecture:
 *
 * TYPE 1: Release/repomd.xml Repositories (Structured Metadata)
 * ============================================================
 * Examples: Debian, Ubuntu, CentOS, RHEL, Fedora
 * Structure:
 *   - Primary metadata file (Release, repomd.xml) contains:
 *     * Package file locations with SHA256/MD5 hashes
 *     * Size information for integrity verification
 *     * Cryptographic signatures for security
 *   - Package database files (Packages.xz, primary.xml.gz) contain:
 *     * Rich metadata per package (dependencies, descriptions, etc.)
 *     * Controlled by hash-based content addressing
 * Benefits: Reliable, consistent, tamper-proof downloads with rich package info
 * Handler: deb_repo.rs, rpm_repo.rs
 *
 * TYPE 2: Direct Package Database Files (Rich Info, No Metadata Layer)
 * ===================================================================
 * Examples: Alpine APK repositories, some custom package servers
 * Structure:
 *   - Single package database file (APKINDEX.tar.gz)
 *   - Contains rich package information directly
 *   - No separate metadata layer with hashes
 * Benefits: Simpler structure, still rich package info
 * Limitations: No hash verification, potential consistency issues
 * Handler: sync_from_package_database() in repo.rs
 *
 * TYPE 3: Plain HTML Directory Listings (Minimal Info, Maximum Compatibility)
 * =========================================================================
 * Examples: Simple HTTP servers, basic mirrors, custom package directories
 * Structure:
 *   - HTTP directory listing showing package files
 *   - Filename-based package information extraction
 *   - No structured metadata or hash verification
 * Benefits: Works with any HTTP server, maximum compatibility
 * Limitations: Minimal package info, no integrity verification, parsing fragility
 * Handler: THIS MODULE (index_html.rs)
 *
 * This module (Type 3) extracts package information from HTML directory listings
 * by parsing filenames using format-specific regex patterns. It's the fallback
 * option for repositories that don't provide structured metadata.
 */

// index_url: https://some/dir/  (must end with /)
pub fn sync_from_directory_index(format: PackageFormat, repo: &RepoRevise, release_path: &PathBuf) -> Result<bool> {
    let repo_dir = dirs::get_repo_dir(&repo)
        .with_context(|| format!("Failed to get repository directory for: {}", repo.repo_name))?;

    // Download index.html
    let index_html_url = format!("{}index.html", repo.index_url);
    let index_html_path = release_path.join("index.html");

    // Create parent directory if it doesn't exist
    if let Some(parent) = index_html_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
    }

    // Check if we need to refresh the index.html file
    let status = should_refresh_release_file(&index_html_path, repo)
        .with_context(|| "Failed to check if index.html needs refreshing")?;

    // Only download if needed
    if status != ReleaseStatus::FineExist && status != ReleaseStatus::FineRecent {
        // Download the index.html file
        download_urls(vec![index_html_url], &dirs().epkg_downloads_cache, 6, false)
            .with_context(|| "Failed to download index.html")?;
    }

    // Read the index.html content
    let html_content = fs::read_to_string(&index_html_path)
        .with_context(|| format!("Failed to read index.html from: {}", index_html_path.display()))?;

    // Create packages streamline for processing
    let output_path = repo_dir.join("packages.txt");

    // Create a dummy RepoReleaseItem for PackagesStreamline
    let revise = RepoReleaseItem {
        format: format,
        repo_name: repo.repo_name.clone(),
        repodata_name: repo.repodata_name.clone(),
        need_download: false,
        need_convert: status == ReleaseStatus::NeedConvert || !output_path.exists(),
        arch: repo.arch.clone(),
        url: repo.index_url.clone(),
        package_baseurl: repo.index_url.clone(),
        hash_type: "".to_string(),
        hash: "".to_string(),
        size: 0,
        location: "index.html".to_string(),
        is_packages: true,
        download_path: index_html_path.clone(),
        output_path: output_path.clone(),
    };

    let mut derived_files = packages_stream::PackagesStreamline::new(&revise, &repo_dir, process_line)
        .with_context(|| "Failed to initialize PackagesStreamline for index.html processing")?;

    // Parse HTML content and extract package information
    parse_html_and_write_packages(&html_content, format, &repo.arch, &mut derived_files)
        .with_context(|| "Failed to parse HTML and write packages")?;

    // Finalize the processing
    derived_files.on_finish(&revise)
        .with_context(|| "Failed to finalize packages processing")?;

    // Create and load repository index
    let release_items = vec![revise];
    create_load_repoindex(&repo, false, &repo_dir, release_items)
        .with_context(|| format!("Failed to create and load repository index for: {}", repo.repo_name))?;

    Ok(true)
}

fn parse_html_and_write_packages(
    html_content: &str,
    format: PackageFormat,
    arch: &str,
    derived_files: &mut packages_stream::PackagesStreamline,
) -> Result<()> {
    // Create regex to match file entries in the HTML
    // Pattern matches: filename, size, date (flexible format)
    let file_regex = Regex::new(r#"(?m)^([^\s]+\.(?:rpm|deb|pkg\.tar\.xz|apk))\s+(\d{4}/\d{1,2}/\d{1,2}\s+\d{1,2}:\d{1,2})\s+([0-9.]+\s*[kKmMgG]?[bB]?)"#)
        .map_err(|e| eyre::eyre!("Failed to compile regex: {}", e))?;

    let suffix = match format {
        PackageFormat::Rpm => "rpm",
        PackageFormat::Deb => "deb",
        PackageFormat::Apk => "apk",
        PackageFormat::Pacman => "pkg.tar.xz",
        _ => return Err(eyre::eyre!("Unsupported package format: {:?}", format)),
    };

    for captures in file_regex.captures_iter(html_content) {
        let filename = captures.get(1).unwrap().as_str();

        // Only process files with the correct package format suffix
        if !filename.ends_with(&format!(".{}", suffix)) {
            continue;
        }

        // Parse package information from filename
        if let Some((pkgname, version, file_arch)) = parse_package_filename(filename, format) {
            // Skip if architecture doesn't match (unless it's 'all' or 'noarch')
            if file_arch != "all" && file_arch != "noarch" && file_arch != arch {
                continue;
            }

            // Generate package information and write to derived_files
            write_package_info(&pkgname, &version, &file_arch, filename, derived_files)?;
        }
    }

    Ok(())
}

fn parse_package_filename(filename: &str, format: PackageFormat) -> Option<(String, String, String)> {
    match format {
        PackageFormat::Rpm => {
            // Example: crystal1.0-1.0.0-2.1.x86_64.rpm
            // Pattern: name-version-release.arch.rpm
            let re = Regex::new(r"^(.+)-([^-]+)-([^-]+)\.([^.]+)\.rpm$").ok()?;
            if let Some(caps) = re.captures(filename) {
                let pkgname = caps.get(1)?.as_str().to_string();
                let version = format!("{}-{}", caps.get(2)?.as_str(), caps.get(3)?.as_str());
                let arch = caps.get(4)?.as_str().to_string();
                return Some((pkgname, version, arch));
            }
        },
        PackageFormat::Deb => {
            // Example: package_1.0.0-1_amd64.deb
            // Pattern: name_version_arch.deb
            let re = Regex::new(r"^(.+)_([^_]+)_([^_]+)\.deb$").ok()?;
            if let Some(caps) = re.captures(filename) {
                let pkgname = caps.get(1)?.as_str().to_string();
                let version = caps.get(2)?.as_str().to_string();
                let arch = caps.get(3)?.as_str().to_string();
                return Some((pkgname, version, arch));
            }
        },
        PackageFormat::Apk => {
            // Example: package-1.0.0-r1.x86_64.apk
            let re = Regex::new(r"^(.+)-([^-]+-r[0-9]+)\.([^.]+)\.apk$").ok()?;
            if let Some(caps) = re.captures(filename) {
                let pkgname = caps.get(1)?.as_str().to_string();
                let version = caps.get(2)?.as_str().to_string();
                let arch = caps.get(3)?.as_str().to_string();
                return Some((pkgname, version, arch));
            }
        },
        _ => return None,
    }
    None
}

fn write_package_info(
    pkgname: &str,
    version: &str,
    arch: &str,
    filename: &str,
    derived_files: &mut packages_stream::PackagesStreamline,
) -> Result<()> {
    // Start a new package paragraph
    derived_files.on_new_paragraph();
    derived_files.on_new_pkgname(pkgname);

    // Write package information in the expected format
    derived_files.output.push_str(&format!("\npkgname: {}", pkgname));
    derived_files.output.push_str(&format!("\nversion: {}", version));
    derived_files.output.push_str(&format!("\narch: {}", arch));
    derived_files.output.push_str(&format!("\nlocation: {}", filename));

    Ok(())
}

// Helper function to process a single line (required by PackagesStreamline)
fn process_line(_line: &str, _derived_files: &mut packages_stream::PackagesStreamline) -> Result<()> {
    // For index.html processing, we handle the content directly in parse_html_and_write_packages
    // This function is required by PackagesStreamline but won't be used in this context
    Ok(())
}
