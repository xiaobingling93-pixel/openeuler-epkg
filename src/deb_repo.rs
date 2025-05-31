use std::fs;
use std::path::PathBuf;
use std::collections::HashMap;
use std::collections::HashSet;
use std::time::SystemTime;
use std::sync::mpsc::Receiver;
use std::io::Read;
use std::io::Write;
use color_eyre::eyre;
use color_eyre::eyre::Result;
use color_eyre::eyre::WrapErr;
use sha2::{Sha256, Digest};
use hex;
use crate::models::*;
use crate::dirs;
use crate::repo::*;
use crate::mmio;

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
        m.insert("X-Cargo-Built-Using",         "xCargoBuiltUsing");
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

pub fn parse_release_file(repo: &RepoRevise, content: &str, release_dir: &PathBuf) -> Result<Vec<RepoReleaseItem>> {
    let mut release_items = Vec::new();
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
                let location = parts[2].to_string();

                if location.contains("/debian-installer/") {
                    continue;
                }

                // Check if this is a file we're interested in
                let is_packages = location.contains("/binary-") && location.ends_with("/Packages.xz");
                let is_contents = location.contains("/Contents-") && location.ends_with(".gz");

                // Only process entries that match the Debian repo metadata files of interest
                if is_packages || is_contents {
                    // repo_name: e.g. "main" from "main/binary-amd64/Packages.xz"
                    let repo_name = location.split('/').next().unwrap_or("").to_string();
                    if repo_name != repo.repo_name {
                        continue;
                    }

                    // arch: e.g. "amd64" from "main/binary-amd64/Packages.xz" or "main/Contents-amd64.gz"
                    let arch = if is_packages {
                        let deb_arch = location.split("binary-").nth(1).unwrap_or("").split('/').next().unwrap_or("").to_string();
                        map_architecture(&deb_arch)
                    } else {
                        let deb_arch = location.split("Contents-").nth(1).unwrap_or("").split('.').next().unwrap_or("").to_string();
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
                    //   location = "main/binary-amd64/Packages.xz"
                    //   location = "main/Contents-amd64.gz"
                    //
                    // For Packages.xz:
                    //   location = "main/binary-amd64/Packages.xz"
                    //   location.rsplitn(2, '/').nth(1).unwrap() == "main/binary-amd64"
                    //   URL: http://mirrors.163.com/debian/dists/trixie///main/binary-amd64/by-hash/SHA256/aaa
                    //
                    // For Contents-amd64.gz:
                    //   location = "main/Contents-amd64.gz"
                    //   location.rsplitn(2, '/').nth(1).unwrap() == "main"
                    //   URL: http://mirrors.163.com/debian/dists/trixie///main/by-hash/SHA256/ccc
                    // ------------------------------------------------

                    // Construct the location path based on acquire_by_hash setting
                    // If acquire_by_hash is true, use the by-hash location
                    //   e.g. main/binary-amd64/by-hash/SHA256/aaa  # for Packages file
                    //   or   main/by-hash/SHA256/ccc               # for Contents file
                    // If acquire_by_hash is false, use the original location
                    //   e.g. main/binary-amd64/Packages.xz
                    //   or   main/Contents-amd64.gz
                    let download_location = if acquire_by_hash {
                        format!(
                            "{}/by-hash/{}/{}",
                            location.rsplitn(2, '/').nth(1).unwrap(), // = location.parent()
                            current_hash_type, // e.g. "SHA256"
                            hash // e.g. "aaa"
                        )
                    } else {
                        location.clone() // Use original location
                    };

                    // Check if we need to revise by checking if the file exists
                    let download_path = &release_dir.join(&download_location);
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
                    let url = format!("{}/{}", baseurl, download_location);

                    // Example output for release_items vector:
                    // RepoReleaseItem {
                    //     repo_name: "main",
                    //     need_download: true,
                    //     arch: "x86_64",
                    //     url: "http://mirrors.163.com///debian/dists/trixie/main/binary-amd64/by-hash/SHA256/aaa",
                    //     hash_type: "SHA256",
                    //     hash: "aaa",
                    //     size: 9680256,
                    //     location: "main/binary-amd64/Packages.xz",
                    //     download_path: "$HOME/.cache/epkg/downloads/debian/dists/trixie/main/binary-amd64/by-hash/SHA256/aaa"
                    // }

                    release_items.push(RepoReleaseItem {
                        format: PackageFormat::Deb,
                        repo_name,
                        repodata_name: repo.repodata_name.to_string(),
                        need_download,
                        need_convert,
                        arch,
                        url,
                        hash_type: current_hash_type.clone(),
                        hash,
                        size,
                        location,
                        is_packages,
                        output_path,
                        download_path: download_path.to_path_buf(),
                    });
                }
            }
        }
    }

    // Remove entries with lower priority hash types
    release_items.retain(|revise| {
        let priority = match revise.hash_type.as_str() {
            "SHA256" => 3,
            "MD5" => 2,
            "SHA1" => 1,
            _ => 0,
        };
        priority == 3 // Keep only SHA256 entries
    });

    Ok(release_items)
}

pub fn process_packages_content(data_rx: Receiver<Vec<u8>>, repo_dir: &PathBuf, revise: &RepoReleaseItem) -> Result<FileInfo> {
    log::debug!("Starting to process packages content for {}", revise.location);
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
                return Err(eyre::eyre!("Failed to decompress file {}: {}", revise.location, e));
            }
        }
    }

    // Get the final hash from the ReceiverReader
    writer.flush().context(format!("Failed to flush output file: {:?}", output_path))?;
    let calculated_hash = hex::encode(origin_hasher.finalize());
    let expected_hash = &revise.hash;
    if calculated_hash != *expected_hash {
        log::error!("Hash verification failed for {} - calculated: {}, expected: {}", revise.location, calculated_hash, expected_hash);
        return Err(eyre::eyre!("Hash verification failed for {}: calculated {}, expected {}",
            revise.location, calculated_hash, expected_hash));
    }

    // Save package offsets to index file
    mmio::serialize_pkgname2ranges(&index_path, &pkgname2ranges)?;
    mmio::serialize_provide2pkgnames(&provide2pkgnames_path, &provide2pkgnames)?;
    mmio::serialize_essential_pkgnames(&essential_pkgnames_path, &essential_pkgnames)?;

    save_file_metadata(&output_path, &json_path, new_hasher)
}

fn save_file_metadata(output_path: &PathBuf, json_path: &PathBuf, new_hasher: Sha256) -> Result<FileInfo> {
    // Compute final hash and save metadata
    let new_hash = new_hasher.finalize();

    let metadata = fs::metadata(output_path)
        .context(format!("Failed to get metadata for file: {:?}", output_path))?;
    let file_info = FileInfo {
        filename: output_path.file_name().unwrap().to_string_lossy().into_owned(),
        sha256sum: hex::encode(new_hash),
        datetime: metadata.modified()?.duration_since(SystemTime::UNIX_EPOCH)?.as_secs().to_string(),
        size: metadata.len(),
    };
    let json_content = serde_json::to_string_pretty(&file_info)
        .context("Failed to serialize file info to JSON")?;
    fs::write(json_path, json_content)
        .context(format!("Failed to write JSON metadata to file: {:?}", json_path))?;

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
