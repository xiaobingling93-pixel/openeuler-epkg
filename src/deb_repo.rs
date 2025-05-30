use std::fs;
use std::path::PathBuf;
use std::collections::HashMap;
use std::collections::HashSet;
use std::time::SystemTime;
use std::sync::mpsc;
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::io::Read;
use std::io::Write;
use crate::models::*;
use crate::repo::{url_to_cache_path, RepoRevise};
use crate::dirs;
use color_eyre::eyre;
use color_eyre::eyre::Result;
use color_eyre::eyre::WrapErr;
use sha2::{Sha256, Digest};
use hex;
use crate::download::DownloadTask;
use crate::download::submit_download_task;
use crate::download::DOWNLOAD_MANAGER;
use crate::repo;
use crate::config;

use lazy_static::lazy_static;
lazy_static! {
    pub static ref PACKAGE_KEY_MAPPING: std::collections::HashMap<&'static str, &'static str> = {
        let mut m = std::collections::HashMap::new();

        m.insert("Package",         "pkgname");
        m.insert("Source",          "source");
        m.insert("Version",         "version");
        m.insert("Installed-Size",  "installedSize");
        m.insert("Maintainer",      "maintainer");
        m.insert("Architecture",    "arch");
        m.insert("Depends",         "requires");
        m.insert("Pre-Depends",     "requiresPre");
        m.insert("Recommends",      "recommends");
        m.insert("Enhances",        "enhances");
        m.insert("Provides",        "provides");
        m.insert("Description",     "summary");
        m.insert("Description-md5", "descriptionMd5");
        m.insert("Multi-Arch",      "multiArch");
        m.insert("Homepage",        "homepage");
        m.insert("Tag",             "tag");
        m.insert("Section",         "section");
        m.insert("Priority",        "priority");
        m.insert("Filename",        "location");
        m.insert("Size",            "size");
        m.insert("MD5sum",          "md5sum");
        m.insert("SHA256",          "sha256");
        m.insert("Suggests",        "suggests");
        m.insert("Breaks",          "breaks");
        m.insert("Replaces",        "replaces");
        m.insert("Conflicts",       "conflicts");
        m.insert("Protected",       "protected");
        m.insert("Essential",       "essential");
        m.insert("Important",       "important");
        m.insert("Build-Essential", "buildEssential");
        m.insert("Build-Ids",       "buildIds");
        m.insert("Comment",         "comment");

        m.insert("Ruby-Versions",               "rubyVersions");
        m.insert("Lua-Versions",                "luaVersions");
        m.insert("Python-Version",              "pythonVersion");
        m.insert("Python-Egg-Name",             "pythonEggName");
        m.insert("Built-Using",                 "builtUsing");
        m.insert("Static-Built-Using",          "staticBuiltUsing");
        m.insert("Javascript-Built-Using",      "javascriptBuiltUsing");
        m.insert("X-Cargo-Built-Using", 	"xCargoBuiltUsing");
        m.insert("Built-Using-Newlib-Source",   "builtUsingNewlibSource");
        m.insert("Go-Import-Path",              "goImportPath");
        m.insert("Ghc-Package",                 "ghcPackage");
        m.insert("Original-Maintainer",         "originalMaintainer");
        m.insert("Efi-Vendor",                  "efiVendor");
        m.insert("Cnf-Ignore-Commands",         "cnfIgnoreCommands");
        m.insert("Cnf-Visible-Pkgname",         "cnfVisiblePkgname");
        m.insert("Cnf-Extra-Commands",          "cnfExtraCommands");
        m.insert("Gstreamer-Version",           "gstreamerVersion");
        m.insert("Gstreamer-Elements",          "gstreamerElements");
        m.insert("Gstreamer-Uri-Sources",       "gstreamerUriSources");
        m.insert("Gstreamer-Uri-Sinks",         "gstreamerUriSinks");
        m.insert("Gstreamer-Encoders",          "gstreamerEncoders");
        m.insert("Gstreamer-Decoders",          "gstreamerDecoders");
        m.insert("Postgresql-Catversion",       "postgresqlCatversion");

        m
    };
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct DebianReleaseItem {
    pub repo_name: String,
    pub need_download: bool,
    pub need_convert: bool,
    pub arch: String,
    pub url: String,
    pub hash_type: String,
    pub hash: String,
    pub size: u64,
    pub path: String,
    pub download_path: PathBuf,
    pub output_path: PathBuf,
}

fn process_revises_parallel(
    revises: Vec<DebianReleaseItem>,
    repo_dir: Arc<PathBuf>,
    info_clone2: Vec<DebianReleaseItem>,
    result_tx: mpsc::Sender<Vec<PathBuf>>
) {
    std::thread::spawn(move || {
        let mut handles = Vec::new();

        // Process files in parallel std::thread
        for revise in revises {
            let repo_dir = Arc::clone(&repo_dir);
            let revise = revise.clone();

            let handle = std::thread::spawn(move || {
                let (data_tx, data_rx) = channel();

                // Create and submit download task
                let task = DownloadTask::new(
                    revise.url.clone(),
                    dirs().epkg_downloads_cache.to_str().unwrap().to_string(),
                    6
                ).with_data_channel(data_tx);

                // Submit download task
                if let Err(e) = submit_download_task(task) {
                    return Err(e);
                }

                let _ = &DOWNLOAD_MANAGER.start_processing()?;

                log::debug!("process_data for {:?}", revise);
                // Process data blocks as they arrive
                process_data(data_rx, &repo_dir, &revise)
            });

            handles.push(handle);
        }

        // Wait for all threads to complete
        for handle in handles {
            let _ = handle.join().unwrap();
        }

        let mut packages_metafiles = Vec::new();
        for revise in info_clone2 {
            if revise.path.ends_with("/Packages.xz") {
                packages_metafiles.push(repo_dir.join(format!(".packages-{}.json", revise.arch)));
            }
        }
        log::debug!("sending packages_metafiles {:?}", packages_metafiles);
        let _ = result_tx.send(packages_metafiles);
    });
}

fn process_revises_sequential(
    revises: Vec<DebianReleaseItem>,
    repo_dir: &PathBuf,
    info_clone2: Vec<DebianReleaseItem>,
    result_tx: mpsc::Sender<Vec<PathBuf>>
) -> Result<()> {
    // Process files sequentially
    for revise in revises {
        let (data_tx, data_rx) = channel();

        // Create and submit download task
        let task = DownloadTask::new(
            revise.url.clone(),
            dirs().epkg_downloads_cache.to_str().unwrap().to_string(),
            6
        ).with_data_channel(data_tx);

        // Submit download task
        if let Err(e) = submit_download_task(task) {
            return Err(e);
        }

        let _ = &DOWNLOAD_MANAGER.start_processing()?;

        log::debug!("process_data for {:?}", revise);
        // Process data blocks as they arrive
        process_data(data_rx, repo_dir, &revise)?;
    }

    let mut packages_metafiles = Vec::new();
    for revise in info_clone2 {
        if revise.path.ends_with("/Packages.xz") {
            packages_metafiles.push(repo_dir.join(format!(".packages-{}.json", revise.arch)));
        }
    }
    log::debug!("sending packages_metafiles {:?}", packages_metafiles);
    let _ = result_tx.send(packages_metafiles);
    Ok(())
}

pub fn revise_repodata(repo: &RepoRevise, result_tx: &mpsc::Sender<Vec<PathBuf>>) -> Result<bool> {
    let repo_dir = dirs::get_repo_dir(&repo).unwrap();
    let release_path = url_to_cache_path(&repo.index_url)?;

    repo::refresh_download(&release_path, &repo)?;

    // Parse Release file
    let release_content = fs::read_to_string(&release_path)
        .with_context(|| format!("Failed to read Release file: {}", release_path.display()))?;
    let release_dir = release_path.parent().unwrap();
    let info = parse_release_file(&repo, &release_content, &release_dir.to_path_buf())?;

    if info.is_empty() {
        return Ok(false);
    }

    let repo_dir = Arc::new(repo_dir.clone());

    // Filter out items that don't need revision
    let info_clone = info.clone();
    let revises: Vec<_> = info_clone.iter()
        .filter(|revise| revise.need_download || revise.need_convert)
        .cloned()
        .collect();

    if revises.is_empty() {
        return Ok(false);
    }

    log::debug!("repo: {:?}", repo);
    log::debug!("revises: {:#?}", revises);

    let info_clone2 = info.clone();
    let result_tx = result_tx.clone();

    if config().common.parallel_processing {
        process_revises_parallel(revises, repo_dir, info_clone2, result_tx);
    } else {
        process_revises_sequential(revises, &repo_dir, info_clone2, result_tx)?;
    }
    Ok(true)
}

fn parse_release_file(repo: &RepoRevise, content: &str, release_dir: &PathBuf) -> Result<Vec<DebianReleaseItem>> {
    let mut info = Vec::new();
    let mut acquire_by_hash = false;
    let mut current_hash_type = String::new();

    // Map Debian architecture to standard architecture
    let map_architecture = |arch: &str| -> String {
        match arch {
            "arm64" => "aarch64".to_string(),
            "amd64" => "x86_64".to_string(),
            _ => arch.to_string(),
        }
    };

    // Single pass: collect files with their best hash type
    for line in content.lines() {
        if line.starts_with("Acquire-By-Hash:") {
            acquire_by_hash = line.contains("yes");
            continue;
        }

        if line.starts_with("SHA256:") {
            current_hash_type = "SHA256".to_string();
            continue;
        } else if line.starts_with("SHA1:") {
            current_hash_type = "SHA1".to_string();
            continue;
        } else if line.starts_with("MD5Sum:") {
            current_hash_type = "MD5".to_string();
            continue;
        }

        if !current_hash_type.is_empty() && !line.trim().is_empty() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 {
                let hash = parts[0].to_string();
                let size = parts[1].parse::<u64>().unwrap_or(0);
                let path = parts[2].to_string();

                if path.contains("/debian-installer/") {
                    continue;
                }

                // Check if this is a file we're interested in
                let is_packages = path.contains("/binary-") && path.ends_with("/Packages.xz");
                let is_contents = path.contains("/Contents-") && path.ends_with(".gz");

                // Only process entries that match the Debian repo metadata files of interest
                if is_packages || is_contents {
                    // repo_name: e.g. "main" from "main/binary-amd64/Packages.xz"
                    let repo_name = path.split('/').next().unwrap_or("").to_string();
                    if repo_name != repo.repo_name {
                        continue;
                    }

                    // arch: e.g. "amd64" from "main/binary-amd64/Packages.xz" or "main/Contents-amd64.gz"
                    let arch = if is_packages {
                        let deb_arch = path.split("binary-").nth(1).unwrap_or("").split('/').next().unwrap_or("").to_string();
                        map_architecture(&deb_arch)
                    } else {
                        let deb_arch = path.split("Contents-").nth(1).unwrap_or("").split('.').next().unwrap_or("").to_string();
                        map_architecture(&deb_arch)
                    };

                    // Skip if architecture doesn't match and isn't 'all'
                    if arch != "all" && arch != repo.arch {
                        continue;
                    }

                    let repo_dir = dirs::get_repo_dir(&repo).unwrap();
                    let output_path = if is_packages {
                        repo_dir.join(format!("packages-{}.txt", arch))
                    } else {
                        repo_dir.join(format!("filelist-{}.gz", arch))
                    };

                    // --- EXAMPLES FOR PATH AND URL CONSTRUCTION ---
                    // Given:
                    //   repo.index_url = "$mirror/debian/dists/$version/Release"
                    //   current_hash_type = "SHA256"
                    //   hash = "aaa"
                    //   path = "main/binary-amd64/Packages.xz"
                    //   path = "main/Contents-amd64.gz"
                    //
                    // For Packages.xz:
                    //   path = "main/binary-amd64/Packages.xz"
                    //   path.rsplitn(2, '/').nth(1).unwrap() == "main/binary-amd64"
                    //   URL: http://mirrors.163.com/debian/dists/trixie///main/binary-amd64/by-hash/SHA256/aaa
                    //
                    // For Contents-amd64.gz:
                    //   path = "main/Contents-amd64.gz"
                    //   path.rsplitn(2, '/').nth(1).unwrap() == "main"
                    //   URL: http://mirrors.163.com/debian/dists/trixie///main/by-hash/SHA256/ccc
                    // ------------------------------------------------

                    // Construct the location path based on acquire_by_hash setting
                    // If acquire_by_hash is true, use the by-hash path
                    //   e.g. main/binary-amd64/by-hash/SHA256/aaa  # for Packages file
                    //   or   main/by-hash/SHA256/ccc               # for Contents file
                    // If acquire_by_hash is false, use the original path
                    //   e.g. main/binary-amd64/Packages.xz
                    //   or   main/Contents-amd64.gz
                    let location = if acquire_by_hash {
                        format!(
                            "{}/by-hash/{}/{}",
                            path.rsplitn(2, '/').nth(1).unwrap(), // = path.parent()
                            current_hash_type, // e.g. "SHA256"
                            hash // e.g. "aaa"
                        )
                    } else {
                        path.clone() // Use original path
                    };

                    // Check if we need to revise by checking if the file exists
                    let download_path = &release_dir.join(&location);
                    let need_download = !download_path.exists();
                    let need_convert = !output_path.exists();

                    // Construct the download URL
                    let baseurl = if repo.index_url.ends_with("/Release") {
                        repo.index_url.trim_end_matches("/Release")
                    } else if repo.index_url.ends_with('/') {
                        repo.index_url.trim_end_matches('/')
                    } else {
                        &repo.index_url
                    };
                    let url = format!("{}/{}", baseurl, location);

                    // Example output for info vector:
                    // DebianReleaseItem {
                    //     repo_name: "main",
                    //     need_download: true,
                    //     arch: "x86_64",
                    //     url: "http://mirrors.163.com///debian/dists/trixie/main/binary-amd64/by-hash/SHA256/aaa",
                    //     hash_type: "SHA256",
                    //     hash: "aaa",
                    //     size: 9680256,
                    //     path: "main/binary-amd64/Packages.xz",
                    //     download_path: "$HOME/.cache/epkg/downloads/debian/dists/trixie/main/binary-amd64/by-hash/SHA256/aaa"
                    // }

                    info.push(DebianReleaseItem {
                        repo_name,
                        need_download,
                        need_convert,
                        arch,
                        url,
                        hash_type: current_hash_type.clone(),
                        hash,
                        size,
                        path,
                        output_path,
                        download_path: download_path.to_path_buf(),
                    });
                }
            }
        }
    }

    // Remove entries with lower priority hash types
    info.retain(|revise| {
        let priority = match revise.hash_type.as_str() {
            "SHA256" => 3,
            "MD5" => 2,
            "SHA1" => 1,
            _ => 0,
        };
        priority == 3 // Keep only SHA256 entries
    });

    Ok(info)
}

// Add this struct before using XzDecoder
struct ReceiverReader<'a> {
    receiver: Receiver<Vec<u8>>,
    current_chunk: Vec<u8>,
    position: usize,
    hasher: Option<&'a mut Sha256>,
}

impl<'a> ReceiverReader<'a> {
    fn new(receiver: Receiver<Vec<u8>>) -> Self {
        Self {
            receiver,
            current_chunk: Vec::new(),
            position: 0,
            hasher: None,
        }
    }

    fn with_hasher(mut self, hasher: &'a mut Sha256) -> Self {
        self.hasher = Some(hasher);
        self
    }
}

impl<'a> std::io::Read for ReceiverReader<'a> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.position >= self.current_chunk.len() {
            match self.receiver.recv() {
                Ok(chunk) => {
                    if let Some(hasher) = &mut self.hasher {
                        hasher.update(&chunk);
                    }
                    self.current_chunk = chunk;
                    self.position = 0;
                }
                Err(_) => return Ok(0), // End of stream
            }
        }

        let remaining = self.current_chunk.len() - self.position;
        let to_copy = std::cmp::min(remaining, buf.len());
        buf[..to_copy].copy_from_slice(&self.current_chunk[self.position..self.position + to_copy]);
        self.position += to_copy;
        Ok(to_copy)
    }
}

fn process_data(data_rx: Receiver<Vec<u8>>, repo_dir: &PathBuf, revise: &DebianReleaseItem) -> Result<FileInfo> {
    if revise.path.ends_with("Packages.xz") {
        process_packages_content(data_rx, repo_dir, revise)
    } else if revise.path.contains("/Contents-") {
        process_filelist_content(data_rx, repo_dir, revise)
    } else {
        Err(eyre::eyre!("Unknown file type: {}", revise.path))
    }
}

fn process_packages_content(data_rx: Receiver<Vec<u8>>, repo_dir: &PathBuf, revise: &DebianReleaseItem) -> Result<FileInfo> {
    log::debug!("Starting to process packages content for {}", revise.path);
    let output_path = repo_dir.join(format!("packages-{}.txt", revise.arch));
    let json_path = repo_dir.join(format!(".packages-{}.json", revise.arch));
    let provide2pkgnames_path = repo_dir.join(format!("provide2pkgnames-{}.yaml", revise.arch));
    let essential_pkgnames_path = repo_dir.join(format!("essential_pkgnames-{}.txt", revise.arch));
    let index_path = repo_dir.join(format!("packages-{}.idx", revise.arch));
    log::debug!("Output paths - txt: {:?}, json: {:?}, idx: {:?}", output_path, json_path, index_path);

    let mut origin_hasher = Sha256::new();
    let mut new_hasher = Sha256::new();
    let reader = ReceiverReader::new(data_rx).with_hasher(&mut origin_hasher);
    let mut decoder = xz2::read::XzDecoder::new(reader);
    let mut decompressed = vec![0u8; 65536];

    let mut current_pkgname: String = String::new();
    let mut provide2pkgnames = HashMap::new();
    let mut essential_pkgnames: HashSet<String> = HashSet::new();
    let mut pkgname2ranges: HashMap<String, Vec<PackageRange>> = HashMap::new();
    let mut total_bytes = 0;
    let mut output = String::new();
    let mut partial_line = String::new();
    let mut output_offset = 0;
    let mut package_begin_offset = 0;

    // Open output file for appending before the loop
    use std::fs::OpenOptions;
    use std::io::BufWriter;
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&output_path)
        .context(format!("Failed to open output file: {:?}", output_path))?;
    let mut writer = BufWriter::new(file);

    // Collect data and calculate hash incrementally
    loop {
        match decoder.read(&mut decompressed) {
            Ok(0) => {
                log::debug!("Reached EOF after processing {} bytes", total_bytes);
                // Process any remaining partial line
                process_line(&partial_line,
                    &mut current_pkgname,
                    &mut provide2pkgnames,
                    &mut essential_pkgnames,
                    &mut output,
                    &mut pkgname2ranges,
                    &mut output_offset,
                    &mut package_begin_offset);
                break;
            }
            Ok(n) => {
                total_bytes += n;
                let content = &decompressed[..n];
                let mut pos = 0;

                while pos < content.len() {
                    // Find the next newline
                    if let Some(newline_pos) = content[pos..].iter().position(|&b| b == b'\n') {
                        let newline_pos = pos + newline_pos;

                        // If we have a partial line, combine it with the content up to the newline
                        if partial_line.is_empty() {
                            // No partial line, just process the line up to the newline
                            let line = String::from_utf8_lossy(&content[pos..newline_pos]);
                            process_line(&line,
                                &mut current_pkgname,
                                &mut provide2pkgnames,
                                &mut essential_pkgnames,
                                &mut output,
                                &mut pkgname2ranges,
                                &mut output_offset,
                                &mut package_begin_offset);
                        } else {
                            let line = String::from_utf8_lossy(&content[pos..newline_pos]);
                            let full_line = partial_line.clone() + &line;
                            process_line(&full_line,
                                &mut current_pkgname,
                                &mut provide2pkgnames,
                                &mut essential_pkgnames,
                                &mut output,
                                &mut pkgname2ranges,
                                &mut output_offset,
                                &mut package_begin_offset);
                            partial_line.clear();
                        }

                        pos = newline_pos + 1;
                    } else {
                        // No more newlines, save the rest as partial
                        partial_line.push_str(&String::from_utf8_lossy(&content[pos..]));
                        break;
                    }
                }

                new_hasher.update(output.as_bytes());
                writer.write_all(output.as_bytes())
                    .context(format!("Failed to append to output file: {:?}", output_path))?;
                output_offset += output.len();
                output.clear();
            }
            Err(e) => {
                log::error!("Decompression error: {}", e);
                return Err(eyre::eyre!("Failed to decompress file {}: {}", revise.path, e));
            }
        }
    }

    // Get the final hash from the ReceiverReader
    writer.flush().context(format!("Failed to flush output file: {:?}", output_path))?;
    let calculated_hash = hex::encode(origin_hasher.finalize());
    let expected_hash = &revise.hash;
    if calculated_hash != *expected_hash {
        log::error!("Hash verification failed for {} - calculated: {}, expected: {}", revise.path, calculated_hash, expected_hash);
        return Err(eyre::eyre!("Hash verification failed for {}: calculated {}, expected {}",
            revise.path, calculated_hash, expected_hash));
    }

    // Save package offsets to index file
    repo::serialize_pkgname2offsets(&index_path, &pkgname2ranges)?;

    // Compute final hash and save metadata
    let new_hash = new_hasher.finalize();
    let metadata = fs::metadata(&output_path)
        .context(format!("Failed to get metadata for file: {:?}", output_path))?;
    let file_info = FileInfo {
        filename: output_path.file_name().unwrap().to_string_lossy().into_owned(),
        sha256sum: hex::encode(new_hash),
        datetime: metadata.modified()?.duration_since(SystemTime::UNIX_EPOCH)?.as_secs().to_string(),
        size: metadata.len(),
    };
    let json_content = serde_json::to_string_pretty(&file_info)
        .context("Failed to serialize file info to JSON")?;
    fs::write(&json_path, json_content)
        .context(format!("Failed to write JSON metadata to file: {:?}", json_path))?;
    repo::serialize_provide2pkgnames(&provide2pkgnames_path, &provide2pkgnames)?;
    repo::serialize_essential_pkgnames(&essential_pkgnames_path, &essential_pkgnames)?;
    log::debug!("Successfully processed packages content");
    Ok(file_info)
}

// Helper function to process a single line
fn process_line(line: &str,
                current_pkgname: &mut String,
                provide2pkgnames: &mut HashMap<String, Vec<String>>,
                essential_pkgnames: &mut HashSet<String>,
                output: &mut String,
                pkgname2ranges: &mut HashMap<String, Vec<PackageRange>>,
                output_offset: &mut usize,
                package_begin_offset: &mut usize) {
    if line.is_empty() {
        output.push_str("\n");
        // If we hit an empty line and have a current package, record its end offset
        if !current_pkgname.is_empty() {
            let current_offset = *output_offset + output.len();
            pkgname2ranges.entry(current_pkgname.clone()).or_insert(Vec::new()).push(PackageRange {
                begin: *package_begin_offset,
                len: current_offset - *package_begin_offset,
            });
            *package_begin_offset = current_offset;
        }
        current_pkgname.clear();
    } else if line.starts_with(" ") {
        // This is a continuation line, append it to the previous line
        output.push_str(line);
    } else if let Some((key, value)) = line.split_once(": ") {
        if let Some(mapped_key) = PACKAGE_KEY_MAPPING.get(key) {
            output.push_str(&format!("\n{}: {}", mapped_key, value));
            if key == "Package" {
                // Start tracking the new package
                current_pkgname.clear();
                current_pkgname.push_str(value);
            } else if key == "Essential" {
                output.push_str(&format!("\n{}: {}", "priority", "essential"));
                essential_pkgnames.insert(current_pkgname.clone());
            } else if key == "Important" {
                output.push_str(&format!("\n{}: {}", "priority", "important"));
            } else if key == "Provides" {
                // Example value: "nvidia-open-kernel-535.247.01, nvidia-open-kernel-dkms-any (= 535.247.01)"
                let provides: Vec<&str> = value.split(", ")
                    .map(|s| s.split_whitespace().next().unwrap_or(""))
                    .filter(|s| !s.is_empty())
                    .collect();
                for provide in provides {
                    provide2pkgnames.entry(provide.to_string()).or_insert(Vec::new()).push(current_pkgname.clone());
                }
            }
        } else {
            log::warn!("Unexpected key in line -- {}: {}", key, value);
        }
    } else {
        log::warn!("Unexpected line format: {}", line);
    }
}

fn process_filelist_content(data_rx: Receiver<Vec<u8>>, repo_dir: &PathBuf, revise: &DebianReleaseItem) -> Result<FileInfo> {
    log::debug!("Processing filelist content for arch: {:?}", revise);
    let mut hasher = Sha256::new();

    // Process data and calculate hash incrementally
    while let Ok(data) = data_rx.recv() {
        hasher.update(&data);
    }

    // Verify hash
    let calculated_hash = hex::encode(hasher.finalize());
    if calculated_hash != revise.hash {
        log::error!("Hash verification failed for {}: expected {}, got {}",
            revise.path, revise.hash, calculated_hash);
        return Err(eyre::eyre!("Hash verification failed for {}: expected {}, got {}",
            revise.path, revise.hash, calculated_hash));
    }
    log::debug!("Hash verification successful for {}", revise.path);

    // Create symbolic link from contents_path to repo_dir
    // "Contents-all.gz"
    let output_path = repo_dir.join(format!("filelist-{}.gz", revise.arch));
    let json_path = repo_dir.join(format!(".filelist-{}.json", revise.arch));
    if output_path.exists() {
        log::debug!("Removing existing filelist at {}", output_path.display());
        fs::remove_file(&output_path)
            .with_context(|| format!("Failed to remove existing filelist at {}", output_path.display()))?;
    }

    log::debug!("Creating symlink from {} to {}", revise.download_path.display(), output_path.display());
    #[cfg(unix)]
    std::os::unix::fs::symlink(revise.download_path.clone(), &output_path)
        .with_context(|| format!("Failed to create symlink from {} to {}",
            revise.download_path.display(), output_path.display()))?;
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(revise.download_path, &output_path)
        .with_context(|| format!("Failed to create symlink from {} to {}",
            revise.download_path.display(), output_path.display()))?;

    let metadata = fs::metadata(&output_path)
        .with_context(|| format!("Failed to get metadata for {}", output_path.display()))?;
    let file_info = FileInfo {
        filename: output_path.file_name()
            .ok_or_else(|| eyre::eyre!("Failed to get filename from path: {}", output_path.display()))?
            .to_string_lossy()
            .into_owned(),
        sha256sum: calculated_hash,
        datetime: metadata.modified()?
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_secs()
            .to_string(),
        size: metadata.len(),
    };

    log::debug!("Writing filelist metadata to {}", json_path.display());
    let json_content = serde_json::to_string_pretty(&file_info)
        .with_context(|| format!("Failed to serialize file info to JSON for {}", output_path.display()))?;
    fs::write(&json_path, json_content)
        .with_context(|| format!("Failed to write JSON metadata to {}", json_path.display()))?;

    log::debug!("Successfully processed filelist content for arch: {}", revise.arch);
    Ok(file_info)
}

