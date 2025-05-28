use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, Duration};
use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use rayon::prelude::*;

use std::io::Read;
use crate::models::FileInfo;
use crate::repo::{url_to_cache_path, RepoRevise};
use crate::download::download_urls;
use crate::dirs;
use color_eyre::eyre;
use color_eyre::eyre::Result;
use color_eyre::eyre::WrapErr;
use sha2::{Sha256, Digest};
use hex;
use crate::download::DownloadTask;
use crate::download::submit_download_task;

const PACKAGE_KEY_MAPPING: &[(&str, &str)] = &[
    ("Package", "pkgname"),
    ("Version", "version"),
    ("Installed-Size", "installedSize"),
    ("Maintainer", "maintainer"),
    ("Architecture", "arch"),
    ("Depends", "requires"),
    ("Pre-Depends", "requiresPre"),
    ("Description", "summary"),
    ("Homepage", "homepage"),
    ("Tag", "tag"),
    ("Section", "section"),
    ("Priority", "priority"),
    ("Filename", "location"),
    ("Size", "size"),
    ("MD5sum", "md5sum"),
    ("SHA256", "sha256"),
];

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct DebianReleaseItem {
    pub repo_name: String,
    pub need_revise: bool,
    pub arch: String,
    pub url: String,
    pub hash_type: String,
    pub hash: String,
    pub size: u64,
    pub path: String,
    pub download_path: PathBuf,
}

pub fn refresh_download(path: &PathBuf, repo: &RepoRevise) -> Result<()> {
    // Check if already updated in last 1 day
    if path.exists() {
        let metadata = fs::metadata(&path)?;
        let modified = metadata.modified()?;
        let now = SystemTime::now();
        if let Ok(duration) = now.duration_since(modified) {
            if duration < Duration::from_secs(24 * 60 * 60) {
                return Ok(());
            }
        }
    }

    // Download Release file
    download_urls(vec![repo.index_url.clone()], dirs().epkg_downloads_cache.to_str().unwrap(), 6, false)?;
    Ok(())
}

pub fn revise_repodata(repo: &RepoRevise, result_tx: &mpsc::Sender<Vec<PathBuf>>) -> Result<bool> {
    let repo_dir = dirs::get_repo_dir(&repo).unwrap();
    let release_path = url_to_cache_path(&repo.index_url)?;

    refresh_download(&release_path, &repo)?;

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
        .filter(|revise| revise.need_revise)
        .cloned()
        .collect();

    if revises.is_empty() {
        return Ok(false);
    }

    let info_clone2 = info.clone();
    let result_tx = result_tx.clone();
    std::thread::spawn(move || {
        // Process items in parallel using Rayon
        let _results: Vec<Result<FileInfo>> = revises.par_iter()
            .map(|revise| {
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

                // Process data blocks as they arrive
                process_data(data_rx, &repo_dir, &revise)
            })
            .collect();

        let mut packages_metafiles = Vec::new();
        for revise in info_clone2 {
            if revise.path.ends_with("/Packages.xz") {
                packages_metafiles.push(repo_dir.join(format!(".packages-{}.json", revise.arch)));
            }
        }
        let _ = result_tx.send(packages_metafiles);
    });
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
                    let need_revise = !download_path.exists();

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
                    //     need_revise: true,
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
                        need_revise,
                        arch,
                        url,
                        hash_type: current_hash_type.clone(),
                        hash,
                        size,
                        path,
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
struct ReceiverReader {
    receiver: Receiver<Vec<u8>>,
    current_chunk: Vec<u8>,
    position: usize,
}

impl ReceiverReader {
    fn new(receiver: Receiver<Vec<u8>>) -> Self {
        Self {
            receiver,
            current_chunk: Vec::new(),
            position: 0,
        }
    }
}

impl std::io::Read for ReceiverReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.position >= self.current_chunk.len() {
            match self.receiver.recv() {
                Ok(chunk) => {
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
    let output_path = repo_dir.join(format!("packages-{}.txt", revise.arch));
    let json_path = repo_dir.join(format!(".packages-{}.json", revise.arch));

    let mut origin_hasher = Sha256::new();
    let mut new_hasher = Sha256::new();
    let reader = ReceiverReader::new(data_rx);
    let mut decoder = xz2::read::XzDecoder::new(reader);
    let mut decompressed = vec![0u8; 8192];
    let mut current_package = HashMap::new();
    let mut current_key: Option<String> = None;

    // Collect data and calculate hash incrementally
    loop {
        match decoder.read(&mut decompressed) {
            Ok(0) => break, // EOF
            Ok(n) => {
                origin_hasher.update(&decompressed[..n]);
                let content = String::from_utf8_lossy(&decompressed[..n]);
                let mut output = String::new();
                for line in content.lines() {
                    if line.trim().is_empty() {
                        if let Some(key) = current_key.take() {
                            let _key_ref = &key;
                            if let Some(new_key) = PACKAGE_KEY_MAPPING.iter().find(|(k, _)| *k == key) {
                                output.push_str(&format!("{}: {}\n", new_key.1, current_package[&key]));
                            }
                        }
                        if !current_package.is_empty() {
                            output.push_str("\n");
                            current_package.clear();
                        }
                    } else if let Some((key, value)) = line.split_once(':') {
                        if current_key.is_some() {
                            current_package.insert(current_key.take().unwrap(), value.trim().to_string());
                        }
                        current_key = Some(key.trim().to_string());
                    } else if let Some(key) = current_key.take() {
                        current_package.entry(key.to_string()).or_insert_with(String::new).push_str(line.trim());
                    }
                }
                new_hasher.update(output.as_bytes());
                fs::write(&output_path, output)?;
            }
            Err(e) => return Err(eyre::eyre!("Decompression error: {}", e)),
        }
    }

    // Verify hash
    let calculated_hash = hex::encode(origin_hasher.finalize());
    let expected_hash = &revise.hash;
    if calculated_hash != *expected_hash {
        return Err(eyre::eyre!("Hash verification failed for {}", revise.path));
    }

    // Compute sha256sum of processed content
    let new_hash = new_hasher.finalize();
    let metadata = fs::metadata(&output_path)?;
    let file_info = FileInfo {
        filename: output_path.file_name().unwrap().to_string_lossy().into_owned(),
        sha256sum: hex::encode(new_hash),
        datetime: metadata.modified()?.duration_since(SystemTime::UNIX_EPOCH)?.as_secs().to_string(),
        size: metadata.len(),
    };
    let json_content = serde_json::to_string_pretty(&file_info)?;
    fs::write(&json_path, json_content)?;
    Ok(file_info)
}

fn process_filelist_content(data_rx: Receiver<Vec<u8>>, repo_dir: &PathBuf, revise: &DebianReleaseItem) -> Result<FileInfo> {
    let mut hasher = Sha256::new();

    // Process data and calculate hash incrementally
    while let Ok(data) = data_rx.recv() {
        hasher.update(&data);
    }

    // Verify hash
    let calculated_hash = hex::encode(hasher.finalize());
    if calculated_hash != revise.hash {
        return Err(eyre::eyre!("Hash verification failed for {}", revise.path));
    }

    // Create symbolic link from contents_path to repo_dir
    // "Contents-all.gz"
    let output_path = repo_dir.join(format!("filelist-{}.gz", revise.arch));
    let json_path = repo_dir.join(format!(".filelist-{}.json", revise.arch));
    if output_path.exists() {
	fs::remove_file(&output_path)?;
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(revise.download_path.clone(), &output_path)?;
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(revise.download_path, &output_path)?;

    let metadata = fs::metadata(&output_path)?;
    let file_info = FileInfo {
        filename: output_path.file_name().unwrap().to_string_lossy().into_owned(),
        sha256sum: calculated_hash,
        datetime: metadata.modified()?.duration_since(SystemTime::UNIX_EPOCH)?.as_secs().to_string(),
        size: metadata.len(),
    };
    let json_content = serde_json::to_string_pretty(&file_info)?;
    fs::write(&json_path, json_content)?;
    Ok(file_info)
}

