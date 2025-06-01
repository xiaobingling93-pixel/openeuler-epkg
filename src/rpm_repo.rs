use std::path::PathBuf;
use std::collections::HashMap;
use std::sync::mpsc::Receiver;
use std::io::Read;
use color_eyre::eyre;
use color_eyre::eyre::{Result, eyre};
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use crate::models::*;
use crate::repo::*;
use crate::dirs;
use crate::packages_stream;
use lazy_static::lazy_static;
use flate2::read::GzDecoder;
use zstd;

lazy_static! {
    pub static ref PACKAGE_KEY_MAPPING: std::collections::HashMap<&'static str, &'static str> = {
        let mut m = std::collections::HashMap::new();

        m.insert("name", "pkgname");
        m.insert("version", "version");
        m.insert("release", "release");
        m.insert("arch", "arch");
        m.insert("summary", "summary");
        m.insert("description", "description");
        m.insert("url", "homepage");
        m.insert("license", "license");
        m.insert("vendor", "vendor");
        m.insert("group", "group");
        m.insert("buildhost", "buildhost");
        m.insert("sourcerpm", "sourcerpm");
        m.insert("headerstart", "headerstart");
        m.insert("headerend", "headerend");
        m.insert("packager", "packager");
        m.insert("size", "size");
        m.insert("archive-size", "archiveSize");
        m.insert("installed-size", "installedSize");
        m.insert("package-size", "packageSize");
        m.insert("location", "location");
        m.insert("checksum", "sha256");
        m.insert("time", "time");

        m
    };
}

pub fn parse_repomd_file(repo: &RepoRevise, content: &str, _release_dir: &PathBuf) -> Result<Vec<RepoReleaseItem>> {
    let index_url = &repo.index_url;
    let mut info = Vec::new();
    let mut reader = Reader::from_str(content);

    let mut buf = Vec::new();
    let mut current_data_type = String::new();
    let mut current_location = String::new();
    let mut current_checksum = String::new();
    let mut current_size = 0u64;
    let mut in_data = false;
    let mut current_element = String::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let element_name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                current_element = element_name.clone();

                match e.name().as_ref() {
                    b"data" => {
                        in_data = true;
                        // Reset values for new data element
                        current_location.clear();
                        current_checksum.clear();
                        current_size = 0;

                        if let Some(data_type) = e.attributes()
                            .find(|attr| attr.as_ref().unwrap().key.as_ref() == b"type")
                            .and_then(|attr| attr.ok())
                            .and_then(|attr| String::from_utf8(attr.value.into_owned()).ok()) {
                            current_data_type = data_type;
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(ref e)) => {
                // Handle self-closing elements like <location href="..."/>
                if in_data && e.name().as_ref() == b"location" {
                    if let Some(href) = e.attributes()
                        .find(|attr| attr.as_ref().unwrap().key.as_ref() == b"href")
                        .and_then(|attr| attr.ok())
                        .and_then(|attr| String::from_utf8(attr.value.into_owned()).ok()) {
                        current_location = href;
                    }
                }
            }
            Ok(Event::Text(e)) => {
                if in_data {
                    let text = e.unescape().unwrap_or_default().to_string().trim().to_string();

                    match current_element.as_str() {
                        "checksum" => current_checksum = text,
                        "size" => current_size = text.parse().unwrap_or(0),
                        _ => {}
                    }
                }
            }
            Ok(Event::End(ref e)) => {
                match e.name().as_ref() {
                    b"data" => {
                        if current_data_type == "primary" || current_data_type == "filelists" {
                            let baseurl = if index_url.ends_with("/repomd.xml") {
                                index_url.trim_end_matches("/repomd.xml").trim_end_matches("/repodata")
                            } else {
                                index_url.trim_end_matches('/')
                            };
                            let url = format!("{}/{}", baseurl, current_location);
                            let local_path = url_to_cache_path(&url)?;
                            let need_download = !local_path.exists();

                            let is_packages = current_data_type == "primary";
                            let repo_dir = dirs::get_repo_dir(&repo).unwrap();
                            let output_path = if is_packages {
                                repo_dir.join(format!("packages.txt"))
                            } else {
                                repo_dir.join(format!("filelist.xml.zst"))
                            };
                            let need_convert = !output_path.exists();

                            info.push(RepoReleaseItem {
                                format: PackageFormat::Rpm,
                                repo_name: repo.repo_name.to_string(),
                                repodata_name: repo.repodata_name.to_string(),
                                need_download,
                                need_convert,
                                arch: repo.arch.clone(),
                                url: url.clone(),
                                package_baseurl: baseurl.to_string(),
                                hash_type: "SHA256".to_string(),
                                hash: current_checksum.clone(),
                                size: current_size,
                                location: current_location.clone(),
                                is_packages,
                                output_path: output_path,
                                download_path: local_path,
                            });
                        }
                        in_data = false;
                        current_data_type.clear();
                    }
                    _ => {
                        // Clear current_element when we finish an element
                        current_element.clear();
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(eyre!("Error at position {}: {:?}", reader.buffer_position(), e)),
            _ => {}
        }
        buf.clear();
    }

    Ok(info)
}

pub fn process_packages_content(data_rx: Receiver<Vec<u8>>, repo_dir: &PathBuf, revise: &RepoReleaseItem) -> Result<FileInfo> {
    log::debug!("Starting to process packages content for {}", revise.location);

    let mut derived_files = packages_stream::PackagesStreamline::new(revise, repo_dir, process_xml_package)?;

    // Always use automatic hash validation by passing the expected hash
    let reader = packages_stream::ReceiverHasher::new(data_rx, revise.hash.clone());

    // Detect compression type from file extension and use appropriate decoder
    let mut unpack_buf = vec![0u8; 65536];

    if revise.location.ends_with(".zst") {
        // Use zstd decoder for .zst files
        let mut zst_decoder = zstd::stream::read::Decoder::new(reader)?;

        // Process the XML stream directly
        loop {
            let read_result = zst_decoder.read(&mut unpack_buf);
            match read_result {
                Ok(0) => break, // EOF
                Ok(n) => {
                    process_xml_chunk(&unpack_buf[..n], &mut derived_files)?;
                }
                Err(e) => {
                    log::error!("Decompression error: {}", e);
                    return Err(eyre::eyre!("Failed to decompress zst file: {}", e));
                }
            }
        }

        derived_files.on_finish(revise)
    } else {
        // Default to gzip decoder for .gz files or other formats
        let mut xml_decoder = GzDecoder::new(reader);

        // Process the XML stream directly without using handle_chunk (since it's for line-based processing)
        loop {
            let read_result = xml_decoder.read(&mut unpack_buf);
            match read_result {
                Ok(0) => break, // EOF
                Ok(n) => {
                    process_xml_chunk(&unpack_buf[..n], &mut derived_files)?;
                }
                Err(e) => {
                    log::error!("Decompression error: {}", e);
                    return Err(eyre::eyre!("Failed to decompress file: {}", e));
                }
            }
        }

        derived_files.on_finish(revise)
    }
}

// Helper function to process XML chunks for RPM packages
fn process_xml_chunk(chunk: &[u8], derived_files: &mut packages_stream::PackagesStreamline) -> Result<()> {
    let mut xml_reader = Reader::from_reader(chunk);
    let mut buf = Vec::new();
    let mut package_info = HashMap::new();
    let mut in_package = false;
    let mut current_tag = String::new();

    loop {
        match xml_reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                match e.name().as_ref() {
                    b"package" => {
                        in_package = true;
                        package_info.clear();
                    }
                    _ => {
                        if in_package {
                            current_tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                        }
                    }
                }
            }
            Ok(Event::Text(e)) => {
                if in_package {
                    let text = e.unescape().unwrap_or_default().to_string();
                    package_info.insert(current_tag.clone(), text);
                }
            }
            Ok(Event::Empty(ref e)) => {
                if in_package {
                    for attr in e.attributes() {
                        if let Ok(attr) = attr {
                            let key = String::from_utf8_lossy(attr.key.as_ref()).to_string();
                            let value = String::from_utf8_lossy(&attr.value).to_string();
                            match key.as_str() {
                                "ver" => package_info.insert("version".to_string(), value),
                                "rel" => package_info.insert("release".to_string(), value),
                                "href" => package_info.insert("location".to_string(), value),
                                _ => None,
                            };
                        }
                    }
                }
            }
            Ok(Event::End(ref e)) => {
                if e.name().as_ref() == b"package" {
                    // Process the complete package
                    process_package_info(&package_info, derived_files)?;
                    in_package = false;
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(eyre!("Error parsing XML at position {}: {:?}", xml_reader.buffer_position(), e)),
            _ => {}
        }
        buf.clear();
    }
    Ok(())
}

// Helper function to process a single package and call the appropriate on_xxx methods
fn process_package_info(package_info: &HashMap<String, String>, derived_files: &mut packages_stream::PackagesStreamline) -> Result<()> {
    // Start a new package paragraph
    derived_files.on_new_paragraph();

    // Process package name first
    if let Some(pkgname) = package_info.get("name") {
        derived_files.on_new_pkgname(pkgname);
        derived_files.output.push_str(&format!("\npkgname: {}", pkgname));
    }

    // Process other package fields
    for (key, value) in package_info {
        if let Some(mapped_key) = PACKAGE_KEY_MAPPING.get(key.as_str()) {
            if key != "name" { // Already processed above
                derived_files.output.push_str(&format!("\n{}: {}", mapped_key, value));
            }
        }
    }

    derived_files.output.push_str("\n");
    derived_files.on_output()?;
    Ok(())
}

// Dummy process line function since we're processing XML directly
fn process_xml_package(_line: &str, _derived_files: &mut packages_stream::PackagesStreamline) -> Result<()> {
    // Not used for XML processing - we process chunks directly
    Ok(())
}

