use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::io::Read;
use color_eyre::eyre::Result;
use color_eyre::eyre;
use liblzma;
use flate2::read::GzDecoder;

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
        m.insert("Prefer-Variant",              "preferVariant");
        m.insert("Commands",                    "commands");

        m.insert("Ruby-Versions",               "rubyVersions");
        m.insert("Lua-Versions",                "luaVersions");
        m.insert("Python-Version",              "pythonVersion");
        m.insert("Python-Egg-Name",             "pythonEggName");
        m.insert("Built-Using",                 "builtUsing");
        m.insert("Static-Built-Using",          "staticBuiltUsing");
        m.insert("Javascript-Built-Using",      "javascriptBuiltUsing");
        m.insert("X-Cargo-Built-Using",         "xCargoBuiltUsing");
        m.insert("X-Rocm-Gpu-Architecture",     "xRocmGpuArchitecture");
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

/// Filter release items to prefer .xz over .gz when both exist for the same location
fn filter_packages_by_compression(release_items: Vec<RepoReleaseItem>) -> Vec<RepoReleaseItem> {
    let mut filtered_items = Vec::new();
    let mut seen_locations = std::collections::HashSet::new();

    // First pass: collect all .xz files and non-Packages items
    for item in &release_items {
        if item.location.ends_with("/Packages.xz") {
            // Strip the compression suffix to get the base location
            if let Some(base_location) = item.location.strip_suffix("/Packages.xz") {
                seen_locations.insert(base_location.to_string());
                filtered_items.push(item.clone());
            }
        } else if item.location.ends_with("/Packages.gz") {
            // Skip .gz files for now, will handle in second pass
            continue;
        } else {
            // Keep all non-Packages items (Contents files, etc.)
            filtered_items.push(item.clone());
        }
    }

    // Second pass: add .gz files only if no .xz file exists for that location
    for item in &release_items {
        if item.location.ends_with("/Packages.gz") {
            // Strip the compression suffix to get the base location
            if let Some(base_location) = item.location.strip_suffix("/Packages.gz") {
                if !seen_locations.contains(base_location) {
                    filtered_items.push(item.clone());
                }
            }
        }
    }

    filtered_items
}

pub fn parse_release_file(repo: &RepoRevise, content: &str, release_dir: &PathBuf) -> Result<Vec<RepoReleaseItem>> {
    let mut release_items = Vec::new();
    let mut current_hash_type = String::new();
    let mut components = Vec::new();

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

        if line.starts_with("Components:") {
            let components_line = line.strip_prefix("Components:").unwrap_or("").trim();
            components = components_line.split_whitespace().map(|s| s.to_string()).collect();
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
                let is_packages = location.contains("/binary-") && (location.ends_with("/Packages.xz") || location.ends_with("/Packages.gz")) ||
                                 location.ends_with("Packages.gz");
                let is_contents = location.contains("Contents-") && location.ends_with(".gz");

                // Only process entries that match the Debian repo metadata files of interest
                if is_packages || is_contents {
                    // Case 1: the common situation
                    // e.g. http://deb.debian.org/debian/dists/trixie/InRelease
                    //
                    // Origin: mongodb
                    // Architectures: amd64 arm64
                    // Components: main
                    // MD5Sum:
                    //  963904bcd0ba3fac6446e1a036478bab           399738 main/binary-amd64/Packages
                    //  4a0c6bc7e925cea11e96df40e3b49258            44458 main/binary-amd64/Packages.gz
                    //  de176d56dcab6fb85563f1d075d23bb1            91824 main/binary-arm64/Packages
                    //  8037ce831b3957701298c27c5882ef4a            20944 main/binary-arm64/Packages.gz
                    //  => component_name = "main"

                    // Case 2: OBS or CUDA repositories don't use components or subdirs in location,
                    // e.g. https://developer.download.nvidia.cn/compute/cuda/repos/ubuntu2404/x86_64/Release
                    //
                    // Archive: Debian_12
                    // Origin: obs://build.opensuse.org/devel:languages:crystal/Debian_12
                    // Label: devel:languages:crystal
                    // Architectures: i386 amd64
                    // Description: Crystal (Debian_12)
                    // MD5Sum:
                    //  1246e515893d375c5da48a8cae4f6175 7134 Packages.gz
                    //  45f461e8c85b7e33be9bf7953d5e2473 6150 Sources.gz
                    //  => component_name = ""
                    // For root-level entries (no '/', e.g. Ubuntu's "Contents-amd64.gz") leave
                    // component_name empty so the workaround below can attribute them to "main"
                    // and repodata_name won't become weird things like 'Contents-amd64.gz-security'.
                    let mut component_name: String = if location.contains('/') {
                        location.split('/').next().unwrap_or("").to_string()
                    } else {
                        String::new()
                    };

                    // Ubuntu has a single Contents file outside of any specific components
                    // As a workaround, attribute it to the "main" repo
                    //
                    // Origin: Ubuntu
                    // Architectures: amd64 arm64 armhf i386 ppc64el riscv64 s390x
                    // Components: main restricted universe multiverse
                    // Description: Ubuntu Noble 24.04
                    // MD5Sum:
                    //  2fc7d01e0a1c7b351738abcd571eec59         51301092 Contents-amd64.gz
                    //  d9a7b09989b1804788068aa3fc437fbe          1401160 main/binary-amd64/Packages.xz
                    //  e76d3250b16471773a8760583f955010           269224 multiverse/binary-amd64/Packages.xz
                    if is_contents && !location.contains('/') && !components.is_empty() {
                        component_name = components.first().cloned().unwrap_or_else(|| "main".to_string());
                        // log::debug!("Adapt Ubuntu {} to component {}", location, component_name);
                    }

                    // Filter components based on repo.components - if components list is not empty,
                    // only include components that are in the list
                    if !repo.components.is_empty() {
                        if !repo.components.contains(&component_name) {
                            continue;
                        }
                    }

                    // arch: e.g. "amd64" from "main/binary-amd64/Packages.xz" or "main/Contents-amd64.gz"
                    // or from "Packages" (for repositories like CUDA that don't use binary- subdirectories)
                    let arch = if is_packages {
                        if location.contains("/binary-") {
                            let deb_arch = location.split("binary-").nth(1).unwrap_or("").split('/').next().unwrap_or("").to_string();
                            map_architecture(&deb_arch)
                        } else {
                            // For repositories like CUDA that don't use binary- subdirectories,
                            // use the repository's architecture
                            repo.arch.clone()
                        }
                    } else {
                        let deb_arch = location.split("Contents-").nth(1).unwrap_or("").split('.').next().unwrap_or("").to_string();
                        map_architecture(&deb_arch)
                    };

                    // Skip if architecture doesn't match and isn't 'all'
                    if arch != "all" && arch != repo.arch {
                        continue;
                    }

                    // Augment repodata_name from 'repo-suffix' to 'repo-component-suffix' format
                    let joined_names = vec![repo.repo_name.as_str(), component_name.as_str()].join("-");
                    let with_component_name = joined_names.trim_end_matches('-');
                    let repodata_name = repo.repodata_name
                        // Official => Official-main
                        // Official-updates => Official-main-updates
                        // Official-security => Official-main-security
                        .replace(&repo.repo_name, &with_component_name)
                        // => main
                        // => main-updates
                        // => main-security
                        .replace("Official-", "");  // matches the "Official" in sources/debian.yaml

                    // Create a new RepoRevise object with augmented repodata_name with component
                    let component_repo = crate::repo::RepoRevise {
                        repodata_name: repodata_name,
                        components: vec![component_name.clone()],
                        arch: arch.clone(),
                        ..repo.clone()
                    };

                    let repo_dir = dirs::get_repo_dir(&component_repo);
                    let output_path = if is_packages {
                        repo_dir.join("packages.txt")
                    } else {
                        repo_dir.join("filelists.gz")
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
                    // Check if we need to convert by checking both the output file and its JSON metadata
                    let need_convert = if !output_path.exists() {
                        true // Output file doesn't exist, definitely need to convert
                    } else {
                        // Output file exists, check if metadata JSON file exists
                        let metadata_path = if is_packages {
                            output_path.with_extension("json").to_str()
                                .map(|s| s.replace("packages", ".packages"))
                        } else {
                            output_path.with_extension("").with_extension("json").to_str()
                                .map(|s| s.replace("filelists", ".filelists"))
                        };
                        // If we can't determine metadata path or it doesn't exist, need to convert
                        metadata_path.map(|p| !std::path::Path::new(&p).exists()).unwrap_or(true)
                    };

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
                        repo_revise: component_repo,
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
                        is_adb: false,
                        output_path,
                        download_path: download_path.to_path_buf(),
                    });
                    // log::debug!("Release line: {}\n {:?}", line, release_items.last());
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

    Ok(filter_packages_by_compression(release_items))
}

pub fn process_packages_content(data_rx: Receiver<Vec<u8>>, repo_dir: &PathBuf, revise: &RepoReleaseItem) -> Result<PackagesFileInfo> {
    log::debug!("Starting to process packages content for {} (hash: {})", revise.location, revise.hash);

    let mut derived_files = packages_stream::PackagesStreamline::new(revise, repo_dir, process_line)
        .map_err(|e| eyre::eyre!("Failed to initialize PackagesStreamline for {}: {}", revise.location, e))?;

    // Always use automatic hash validation by passing the expected hash
    let reader = packages_stream::ReceiverHasher::new_with_size(data_rx, revise.hash.clone(), revise.size.try_into().unwrap());

    let mut unpack_buf = vec![0u8; 65536];
    let mut chunk_count = 0;

    // Detect compression type and use appropriate decoder
    if revise.location.ends_with(".xz") {
        log::debug!("Using XZ decoder for {}", revise.location);
        let mut decoder = liblzma::read::XzDecoder::new(reader);

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
    } else if revise.location.ends_with(".gz") {
        log::debug!("Using GZIP decoder for {}", revise.location);
        let mut decoder = GzDecoder::new(reader);

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
    } else {
        return Err(eyre::eyre!("Unsupported compression format for {}: expected .xz or .gz", revise.location));
    }

    log::debug!("Finalizing processing for {}", revise.location);
    derived_files.on_essential("mawk".to_string());
    derived_files.on_essential("dpkg".to_string());
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
                if *mapped_key == "installedSize" {
                    derived_files.output.push_str("000");   // Debian original Installed-Size is in KB.
                }
            }

            if key == "Package" {
                // Start tracking the new package
                derived_files.on_new_pkgname(value);
            } else if key == "Essential" && value.trim().eq_ignore_ascii_case("yes") {
                derived_files.on_essential(derived_files.current_pkgname.clone());
                derived_files.output.push_str("\npriority: essential");
            } else if key == "Important" && value.trim().eq_ignore_ascii_case("yes") {
                derived_files.output.push_str("\npriority: important");
            } else if key == "Provides" {
                // on_provides handles parsing internally
                derived_files.on_provides(value, PackageFormat::Deb);
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
