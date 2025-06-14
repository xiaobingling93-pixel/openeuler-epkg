use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::io::Read;
use color_eyre::eyre::Result;
use color_eyre::eyre;
use liblzma;

use crate::models::*;
use crate::dirs;
use crate::repo::*;
use crate::packages_stream;

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
        m.insert("Task",            "task");
        m.insert("Section",         "section");
        m.insert("Priority",        "priority");
        m.insert("Filename",        "location");
        m.insert("Size",            "size");
        m.insert("MD5sum",          "md5sum");
        m.insert("SHA256",          "sha256");
        m.insert("SHA512",          "sha512");
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
        m.insert("Modaliases",      "modaliases");
        m.insert("Pmaliases",       "pmaliases");

        m.insert("Origin",          "");    // trivial repeating info
        m.insert("Bugs",            "");    // trivial repeating info
        m.insert("SHA1",            "");    // skip: already has sha256
        m.insert("Support",         "");

        m.insert("Phased-Update-Percentage",    "phasedUpdatePercentage");
        m.insert("Original-Vcs-Git",            "originalVcsGit");
        m.insert("Original-Vcs-Browser",        "originalVcsBrowser");
        m.insert("Auto-Built-Package",          "autoBuiltPackage");
        m.insert("Ubuntu-Oem-Kernel-Flavour",   "ubuntuOemKernelFlavour");

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

fn parent_parent_parent(path: &str) -> Option<String> {
    path.rsplitn(4, '/')  // Split from the right, max 4 parts (last 3 + rest)
        .last()           // Take the remaining part (before the last 3)
        .map(|s| s.to_string())
}

pub fn parse_release_file(repo: &RepoRevise, content: &str, release_dir: &PathBuf) -> Result<Vec<RepoReleaseItem>> {
    let mut release_items = Vec::new();
    let mut current_hash_type = String::new();

    // Map Debian architecture to standard architecture
    let map_architecture = |arch: &str| -> String {
        match arch {
            "arm64" => "aarch64".to_string(),
            "amd64" => "x86_64".to_string(),
            _ => arch.to_string(),
        }
    };

    // This could be in last line, so must whole-file-match in the beginning
    let acquire_by_hash = content.contains("Acquire-By-Hash: yes");

    // Single pass: collect files with their best hash type
    for line in content.lines() {
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
                let size = parts[1].parse::<usize>().unwrap_or(0);
                let location = parts[2].to_string();

                if location.contains("/debian-installer/") {
                    continue;
                }

                // Check if this is a file we're interested in
                let is_packages = location.contains("/binary-") && location.ends_with("/Packages.xz");
                let is_contents = location.contains("Contents-") && location.ends_with(".gz");

                // Only process entries that match the Debian repo metadata files of interest
                if is_packages || is_contents {
                    // repo_name: e.g. "main" from "main/binary-amd64/Packages.xz"
                    let mut repo_name = location.split('/').next().unwrap_or("").to_string();
                    if repo_name == location && repo.repo_name == "main" {
                        repo_name = repo.repo_name.clone();
                        // Ubuntu has a single Contents file outside of a specific repo
                        // As a workaround, attribute it to the "main" repo
                    } else if repo_name != repo.repo_name {
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
                        repo_dir.join(format!("filelists-{}.gz", arch))
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
                        std::path::Path::new(&location).parent().unwrap().join(
                            format!(
                                "by-hash/{}/{}",
                                current_hash_type, // e.g. "SHA256"
                                hash // e.g. "aaa"
                            )
                        ).display().to_string()
                    } else {
                        location.clone() // Use original location
                    };

                    // Check if we need to revise by checking if the file exists
                    let download_path = &release_dir.join(&download_location);
                    let need_download = !download_path.exists();
                    let need_convert = !output_path.exists();

                    let mut package_baseurl = repo.index_url.clone();

                    // Construct the download URL
                    let baseurl = if repo.index_url.ends_with("/Release") {
                        if let Some(parent_url) = parent_parent_parent(&repo.index_url) {
                            package_baseurl = parent_url;
                        }
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
                        package_baseurl: package_baseurl,
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
    log::debug!("Starting to process packages content for {} (hash: {})", revise.location, revise.hash);

    let mut derived_files = packages_stream::PackagesStreamline::new(revise, repo_dir, process_line)
        .map_err(|e| eyre::eyre!("Failed to initialize PackagesStreamline for {}: {}", revise.location, e))?;

    // Always use automatic hash validation by passing the expected hash
    let reader = packages_stream::ReceiverHasher::new_with_size(data_rx, revise.hash.clone(), revise.size.try_into().unwrap());

    log::debug!("Using XZ decoder for {}", revise.location);
    let mut decoder = liblzma::read::XzDecoder::new(reader);
    let mut unpack_buf = vec![0u8; 65536];
    let mut chunk_count = 0;

    // Collect data and calculate hash incrementally
    loop {
        let read_result = decoder.read(&mut unpack_buf);
        chunk_count += 1;

        if chunk_count % 100 == 0 {
            log::trace!("Processed {} chunks for {}", chunk_count, revise.location);
        }

        match derived_files.handle_chunk(read_result, &unpack_buf)
            .map_err(|e| eyre::eyre!("Failed to handle chunk {} for {}: {}", chunk_count, revise.location, e))?
        {
            true => continue,
            false => {
                log::debug!("Finished processing after {} chunks for {}", chunk_count, revise.location);
                break;
            }
        }
    }

    log::debug!("Finalizing processing for {}", revise.location);
    derived_files.on_finish(revise)
        .map_err(|e| eyre::eyre!("Failed to finalize processing for {}: {}", revise.location, e))
}

// Helper function to process a single line
fn process_line(line: &str,
                derived_files: &mut packages_stream::PackagesStreamline) -> Result<()> {
    if line.is_empty() {
        // Only trigger new block if we have a current package
        if !derived_files.current_pkgname.is_empty() {
            derived_files.output.push_str("\n");
            derived_files.on_new_paragraph();
        }
    } else if line.starts_with(" ") {
        // This is a continuation line, append it to the previous line
        derived_files.output.push_str(line);
    } else if let Some((key, value)) = line.split_once(": ") {
        if let Some(mapped_key) = PACKAGE_KEY_MAPPING.get(key) {
            if !mapped_key.is_empty() {
                derived_files.output.push_str(&format!("\n{}: {}", mapped_key, value));
            }

            if key == "Package" {
                // Start tracking the new package
                derived_files.on_new_pkgname(value);
            } else if key == "Essential" && value.trim().eq_ignore_ascii_case("yes") {
                derived_files.on_essential();
                derived_files.output.push_str("\npriority: essential");
            } else if key == "Important" && value.trim().eq_ignore_ascii_case("yes") {
                derived_files.output.push_str("\npriority: important");
            } else if key == "Provides" {
                // Example value: "nvidia-open-kernel-535.247.01, nvidia-open-kernel-dkms-any (= 535.247.01)"
                let provides: Vec<&str> = value.split(", ")
                    .map(|s| s.split_whitespace().next().unwrap_or(""))
                    .filter(|s| !s.is_empty())
                    .collect();
                derived_files.on_provides(provides);
            }
        } else {
            log::warn!("Unexpected key in line -- {}: {}", key, value);
        }
    } else if line.ends_with(":") {
        // "X-Cargo-Built-Using:" no space and value part, so the above split_once failed
    } else {
        log::warn!("Unexpected line format: {}", line);
    }
    Ok(())
}
