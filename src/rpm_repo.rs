use std::path::PathBuf;
use std::collections::HashMap;
use std::sync::mpsc::Receiver;
use std::io::Read;
use color_eyre::eyre::{eyre, WrapErr, Result};
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
        m.insert("arch", "arch");
        m.insert("summary", "summary");
        m.insert("description", "description");
        m.insert("url", "homepage");
        m.insert("license", "license");
        m.insert("vendor", "vendor");
        m.insert("group", "group");
        m.insert("buildhost", "buildHost");
        m.insert("sourcerpm", "source");
        m.insert("packager", "maintainer");
        m.insert("size", "size");
        m.insert("installed-size", "installedSize");
        m.insert("location", "location");
        m.insert("checksum", "sha256");
        m.insert("time", "buildTime");
        m.insert("requires", "requires");
        m.insert("recommends", "recommends");
        m.insert("provides", "provides");
        m.insert("files", "files");

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
                            .find(|attr_result| {
                                match attr_result {
                                    Ok(attr) => attr.key.as_ref() == b"type",
                                    Err(_) => false
                                }
                            })
                            .and_then(|attr_result| attr_result.ok())
                            .and_then(|attr| String::from_utf8(attr.value.into_owned())
                                .map_err(|e| {
                                    log::warn!("Failed to convert attribute value to UTF-8: {}", e);
                                    e
                                }).ok()) {
                            current_data_type = data_type;
                        } else {
                            log::warn!("Failed to find 'type' attribute in 'data' element");
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(ref e)) => {
                // Handle self-closing elements like <location href="..."/>
                if in_data && e.name().as_ref() == b"location" {
                    if let Some(href) = e.attributes()
                        .find(|attr_result| {
                            match attr_result {
                                Ok(attr) => attr.key.as_ref() == b"href",
                                Err(_) => false
                            }
                        })
                        .and_then(|attr_result| attr_result.ok())
                        .and_then(|attr| String::from_utf8(attr.value.into_owned())
                            .map_err(|e| {
                                log::warn!("Failed to convert href attribute value to UTF-8: {}", e);
                                e
                            }).ok()) {
                        current_location = href;
                    }
                }
            }
            Ok(Event::Text(e)) => {
                if in_data {
                    let text = e.unescape()
                        .map_err(|e| eyre!("XML unescape error: Failed to unescape XML text: {}", e))
                        .unwrap_or_default()
                        .to_string()
                        .trim()
                        .to_string();

                    match current_element.as_str() {
                        "checksum" => current_checksum = text,
                        "size" => current_size = text.parse().unwrap_or_else(|e| {
                            log::warn!("Failed to parse size value '{}': {}", text, e);
                            0
                        }),
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
                            let local_path = url_to_cache_path(&url)
                                .with_context(|| format!("Failed to convert URL to cache path: {}", url))?;
                            let need_download = !local_path.exists();

                            let is_packages = current_data_type == "primary";
                            let repo_dir = dirs::get_repo_dir(&repo)
                                .map_err(|e| eyre!("Failed to get repository directory for {}: {}", repo.repo_name, e))?;
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
            Err(e) => return Err(eyre!("XML parsing error: Error at position {}: {:?}", reader.buffer_position(), e)),
            _ => {}
        }
        buf.clear();
    }

    Ok(info)
}

pub fn process_packages_content(data_rx: Receiver<Vec<u8>>, repo_dir: &PathBuf, revise: &RepoReleaseItem) -> Result<FileInfo> {
    log::debug!("Starting to process packages content for {} (hash: {})", revise.location, revise.hash);

    let mut derived_files = packages_stream::PackagesStreamline::new(revise, repo_dir, process_xml_package)
        .map_err(|e| eyre!("Failed to initialize PackagesStreamline for {}: {}", revise.location, e))?;

    // Always use automatic hash validation by passing the expected hash
    let reader = packages_stream::ReceiverHasher::new(data_rx, revise.hash.clone());

    // Detect compression type from file extension and use appropriate decoder
    let mut unpack_buf = vec![0u8; 65536];

    if revise.location.ends_with(".zst") {
        log::debug!("Using zstd decoder for {} (expected hash: {})", revise.location, revise.hash);

        // Use zstd decoder for .zst files
        let zst_decoder_result = zstd::stream::read::Decoder::new(reader);

        // Handle decoder initialization error explicitly
        let mut zst_decoder = match zst_decoder_result {
            Ok(decoder) => {
                log::debug!("Successfully created zstd decoder for {}", revise.location);
                decoder
            },
            Err(e) => {
                let err_msg = format!("Failed to create zstd decoder for {}: {}", revise.location, e);
                log::error!("{}", err_msg);
                return Err(eyre!(err_msg));
            }
        };

        // Process the XML stream directly
        let mut chunk_count = 0;
        let mut total_bytes = 0;

        loop {
            // Clear buffer before each read to avoid potential data corruption
            unpack_buf.fill(0);

            let read_result = zst_decoder.read(&mut unpack_buf);
            match read_result {
                Ok(0) => {
                    log::debug!("Reached EOF after processing {} chunks ({} bytes) for {}",
                              chunk_count, total_bytes, revise.location);
                    break; // EOF
                }
                Ok(n) => {
                    chunk_count += 1;
                    total_bytes += n;

                    if n == 0 {
                        log::warn!("Read 0 bytes in chunk {} for {}, this might indicate a problem",
                                 chunk_count, revise.location);
                        continue;
                    }

                    if chunk_count == 1 {
                        // Log first few bytes of first chunk to help with debugging
                        let preview_size = std::cmp::min(n, 32);
                        let preview = hex::encode(&unpack_buf[..preview_size]);
                        log::debug!("First {} bytes of first chunk: {}", preview_size, preview);
                    }

                    if chunk_count % 100 == 0 {
                        log::debug!("Processed {} chunks ({} bytes) for {}",
                                  chunk_count, total_bytes, revise.location);
                    }

                    // Try to process the XML chunk and provide detailed error context if it fails
                    match process_xml_chunk(&unpack_buf[..n], &mut derived_files) {
                        Ok(_) => (),
                        Err(e) => {
                            let err_msg = format!("Failed to process XML chunk {} ({} bytes) for {}: {}", chunk_count, n, revise.location, e);
                            log::error!("{}", err_msg);

                            // If this is the first chunk, log more details to help diagnose
                            if chunk_count == 1 {
                                let preview = String::from_utf8_lossy(&unpack_buf[..std::cmp::min(n, 200)]);
                                log::error!("First chunk content preview: {}", preview);
                            }

                            return Err(eyre!("XML processing error: {}", err_msg));
                        }
                    };
                }
                Err(e) => {
                    let err_msg = format!("Decompression error for {} at chunk {}: {}", revise.location, chunk_count, e);
                    log::error!("{}", err_msg);
                    return Err(eyre!("Decompression error: {}", err_msg));
                }
            }
        }

        derived_files.on_finish(revise)
            .map_err(|e| eyre!("Failed to finalize processing for {}: {}", revise.location, e))
    } else {
        log::debug!("Using gzip decoder for {}", revise.location);
        // Default to gzip decoder for .gz files or other formats
        let mut xml_decoder = GzDecoder::new(reader);

        // Process the XML stream directly without using handle_chunk (since it's for line-based processing)
        let mut chunk_count = 0;
        loop {
            let read_result = xml_decoder.read(&mut unpack_buf);
            match read_result {
                Ok(0) => {
                    log::debug!("Reached EOF after processing {} chunks for {}", chunk_count, revise.location);
                    break; // EOF
                }
                Ok(n) => {
                    chunk_count += 1;
                    if chunk_count % 100 == 0 {
                        log::trace!("Processed {} chunks for {}", chunk_count, revise.location);
                    }
                    process_xml_chunk(&unpack_buf[..n], &mut derived_files)
                        .map_err(|e| eyre!("Failed to process XML chunk {} ({} bytes) for {}: {}", chunk_count, n, revise.location, e))?;
                }
                Err(e) => {
                    log::error!("Decompression error for {}: {}", revise.location, e);
                    return Err(eyre!("Decompression error: Failed to decompress file {}: {}", revise.location, e));
                }
            }
        }

        derived_files.on_finish(revise)
            .map_err(|e| eyre!("Failed to finalize processing for {}: {}", revise.location, e))
    }
}

// Helper function to process XML chunks for RPM packages
fn process_xml_chunk(chunk: &[u8], derived_files: &mut packages_stream::PackagesStreamline) -> Result<()> {
    if chunk.is_empty() {
        log::warn!("Received empty XML chunk to process");
        return Ok(());
    }

    // Check if the chunk looks like XML (starts with <?xml or <)
    let is_xml_like = chunk.starts_with(b"<?xml") || chunk.starts_with(b"<");
    if !is_xml_like {
        let preview = String::from_utf8_lossy(&chunk[..std::cmp::min(chunk.len(), 50)]);
        log::warn!("Chunk doesn't appear to be XML. First bytes: {}", preview);
        // Continue anyway to get more specific errors
    }

    let mut xml_reader = Reader::from_reader(chunk);
    let mut buf = Vec::new();
    let mut in_package = false;
    let mut current_tag = String::new();
    let mut packages_processed = 0;

    // Current package state for streaming output
    let mut in_dependency_section = String::new(); // Track which dependency section we're in
    let mut dependency_lists: HashMap<String, (Vec<String>, Vec<String>)> = HashMap::new();
    let mut files: Vec<String> = Vec::new();

    // Initialize dependency lists
    dependency_lists.insert("requires".to_string(),     (Vec::new(), Vec::new())); // (regular, pre)
    dependency_lists.insert("provides".to_string(),     (Vec::new(), Vec::new()));
    dependency_lists.insert("recommends".to_string(),   (Vec::new(), Vec::new()));
    dependency_lists.insert("supplements".to_string(),  (Vec::new(), Vec::new()));
    dependency_lists.insert("enhances".to_string(),     (Vec::new(), Vec::new()));
    dependency_lists.insert("suggests".to_string(),     (Vec::new(), Vec::new()));
    dependency_lists.insert("conflicts".to_string(),    (Vec::new(), Vec::new()));
    dependency_lists.insert("obsoletes".to_string(),    (Vec::new(), Vec::new()));

    loop {
        let event_result = xml_reader.read_event_into(&mut buf);
        match event_result {
            Ok(Event::Start(ref e)) => {
                match e.name().as_ref() {
                    b"package" => {
                        in_package = true;
                        derived_files.on_new_paragraph();
                        // Clear all dependency lists for new package
                        for (_, (regular, pre)) in dependency_lists.iter_mut() {
                            regular.clear();
                            pre.clear();
                        }
                        files.clear();
                    }
                    b"rpm:requires"     => in_dependency_section = "requires".to_string(),
                    b"rpm:provides"     => in_dependency_section = "provides".to_string(),
                    b"rpm:recommends"   => in_dependency_section = "recommends".to_string(),
                    b"rpm:supplements"  => in_dependency_section = "supplements".to_string(),
                    b"rpm:enhances"     => in_dependency_section = "enhances".to_string(),
                    b"rpm:suggests"     => in_dependency_section = "suggests".to_string(),
                    b"rpm:conflicts"    => in_dependency_section = "conflicts".to_string(),
                    b"rpm:obsoletes"    => in_dependency_section = "obsoletes".to_string(),
                    b"checksum" => {
                        if in_package {
                            current_tag = "checksum".to_string();
                        }
                    }
                    b"file" => {
                        if in_package {
                            current_tag = "file".to_string();
                        }
                    }
                    _ => {
                        if in_package {
                            current_tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                        }
                    }
                }
            }
            Ok(Event::Text(e)) => {
                if in_package && !current_tag.is_empty() {
                    match e.unescape().map_err(|e| eyre!("XML unescape error: Failed to unescape XML text: {}", e)) {
                        Ok(text) => {
                            let text_str = text.to_string().trim().to_string();
                            if !text_str.is_empty() {
                                // Use PACKAGE_KEY_MAPPING for common fields
                                if let Some(mapped_key) = PACKAGE_KEY_MAPPING.get(current_tag.as_str()) {
                                    if current_tag == "name" {
                                        derived_files.on_new_pkgname(&text_str);
                                        derived_files.output.push_str(&format!("{}: {}\n", mapped_key, text_str));
                                    } else {
                                        derived_files.output.push_str(&format!("{}: {}\n", mapped_key, text_str));
                                    }
                                } else {
                                    // Handle special cases not in the mapping
                                    match current_tag.as_str() {
                                        "checksum" => {
                                            derived_files.output.push_str(&format!("sha256: {}\n", text_str));
                                        }
                                        "file" => {
                                            files.push(text_str);
                                        }
                                        "rpm:license" => {
                                            derived_files.output.push_str(&format!("license: {}\n", text_str));
                                        }
                                        "rpm:vendor" => {
                                            derived_files.output.push_str(&format!("vendor: {}\n", text_str));
                                        }
                                        "rpm:group" => {
                                            derived_files.output.push_str(&format!("group: {}\n", text_str));
                                        }
                                        "rpm:buildhost" => {
                                            derived_files.output.push_str(&format!("buildHost: {}\n", text_str));
                                        }
                                        "rpm:sourcerpm" => {
                                            derived_files.output.push_str(&format!("source: {}\n", text_str));
                                        }
                                        _ => {
                                            // Log unknown fields for debugging
                                            log::debug!("Unknown text field in package: {} = {}", current_tag, text_str);
                                        }
                                    }
                                }
                            }
                        },
                        Err(err) => {
                            log::warn!("Failed to unescape XML text for tag {}: {}", current_tag, err);
                        }
                    }
                }
            }
            Ok(Event::Empty(ref e)) => {
                if in_package {
                    match e.name().as_ref() {
                        b"version" => {
                            // Handle version formatting: epoch:ver-rel
                            let mut epoch = String::new();
                            let mut ver = String::new();
                            let mut rel = String::new();

                            for attr in e.attributes() {
                                if let Ok(attr) = attr {
                                    let key = String::from_utf8_lossy(attr.key.as_ref());
                                    if let Ok(value) = String::from_utf8(attr.value.to_vec())
                                        .map_err(|e| {
                                            log::warn!("Failed to convert attribute value to UTF-8: {}", e);
                                            e
                                        }) {
                                        match key.as_ref() {
                                            "epoch" => epoch = value,
                                            "ver" => ver = value,
                                            "rel" => rel = value,
                                            _ => {}
                                        }
                                    }
                                }
                            }

                            // Format version string
                            let version_str = if epoch == "0" {
                                format!("{}-{}", ver, rel)
                            } else {
                                format!("{}:{}-{}", epoch, ver, rel)
                            };
                            derived_files.output.push_str(&format!("version: {}\n", version_str));
                        }
                        b"location" => {
                            for attr in e.attributes() {
                                if let Ok(attr) = attr {
                                    let key = String::from_utf8_lossy(attr.key.as_ref());
                                    if let Ok(value) = String::from_utf8(attr.value.to_vec()) {
                                        if key == "href" {
                                            derived_files.output.push_str(&format!("location: {}\n", value));
                                        }
                                    }
                                }
                            }
                        }
                        b"size" => {
                            for attr in e.attributes() {
                                if let Ok(attr) = attr {
                                    let key = String::from_utf8_lossy(attr.key.as_ref());
                                    if let Ok(value) = String::from_utf8(attr.value.to_vec()) {
                                        match key.as_ref() {
                                            "package" => {
                                                derived_files.output.push_str(&format!("size: {}\n", value));
                                            }
                                            "installed" => {
                                                derived_files.output.push_str(&format!("installedSize: {}\n", value));
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                            }
                        }
                        b"time" => {
                            for attr in e.attributes() {
                                if let Ok(attr) = attr {
                                    let key = String::from_utf8_lossy(attr.key.as_ref());
                                    if let Ok(value) = String::from_utf8(attr.value.to_vec()) {
                                        if key == "build" {
                                            derived_files.output.push_str(&format!("buildTime: {}\n", value));
                                        }
                                    }
                                }
                            }
                        }
                        b"file" => {
                            // Handle file elements with attributes
                            let _file_path = String::new();
                            let mut _file_type = String::new();

                            for attr in e.attributes() {
                                if let Ok(attr) = attr {
                                    let key = String::from_utf8_lossy(attr.key.as_ref());
                                    if let Ok(value) = String::from_utf8(attr.value.to_vec()) {
                                        match key.as_ref() {
                                            "type" => _file_type = value,
                                            _ => {}
                                        }
                                    }
                                }
                            }

                            // File path will come in text content or this might be self-closing
                            // If it's self-closing, we'll need to get path from content
                        }
                        b"rpm:entry" => {
                            if !in_dependency_section.is_empty() {
                                let mut name = String::new();
                                let mut is_pre = false;
                                let mut _flags = String::new();
                                let mut epoch = String::new();
                                let mut ver = String::new();
                                let mut rel = String::new();

                                for attr_result in e.attributes() {
                                    if let Ok(attr) = attr_result.map_err(|e| eyre!("XML attribute error: Failed to process XML attribute: {}", e)) {
                                        let key = String::from_utf8_lossy(attr.key.as_ref());
                                        if let Ok(value) = String::from_utf8(attr.value.to_vec())
                                            .map_err(|e| {
                                                log::warn!("Failed to convert attribute value to UTF-8: {}", e);
                                                e
                                            }) {
                                            match key.as_ref() {
                                                "name"  => name = value,
                                                "pre"   => is_pre = value == "1",
                                                "flags" => _flags = value,
                                                "epoch" => epoch = value,
                                                "ver"   => ver = value,
                                                "rel"   => rel = value,
                                                _ => {}
                                            }
                                        }
                                    }
                                }

                                // Format entry with version if available
                                let formatted_entry = if !ver.is_empty() && !rel.is_empty() {
                                    if epoch.is_empty() || epoch == "0" {
                                        format!("{}={}-{}", name, ver, rel)
                                    } else {
                                        format!("{}={}:{}-{}", name, epoch, ver, rel)
                                    }
                                } else {
                                    name
                                };

                                // Add to appropriate list
                                if let Some((regular, pre)) = dependency_lists.get_mut(&in_dependency_section) {
                                    if is_pre {
                                        pre.push(formatted_entry);
                                    } else {
                                        regular.push(formatted_entry);
                                    }
                                } else {
                                    log::warn!("Unknown dependency section: {}", in_dependency_section);
                                }
                            } else {
                                log::warn!("Found rpm:entry outside of known dependency section");
                            }
                        }
                        _ => {}
                    }
                }
            }
            Ok(Event::End(ref e)) => {
                match e.name().as_ref() {
                    b"package" => {
                        // Emit all dependency lists
                        for (section_name, (regular, pre)) in &dependency_lists {
                            if section_name == "requires" {
                                // Special handling for requires - emit requiresPre separately
                                if !pre.is_empty() {
                                    derived_files.output.push_str(&format!("requiresPre: {}\n", pre.join(", ")));
                                }
                                if !regular.is_empty() {
                                    derived_files.output.push_str(&format!("requires: {}\n", regular.join(", ")));
                                }
                            } else {
                                // For other dependency types, combine pre and regular
                                let mut all_entries = pre.clone();
                                all_entries.extend(regular.iter().cloned());
                                if !all_entries.is_empty() {
                                    derived_files.output.push_str(&format!("{}: {}\n", section_name, all_entries.join(", ")));
                                }
                            }
                        }

                        // Emit files if any
                        if !files.is_empty() {
                            derived_files.output.push_str(&format!("files: {}\n", files.join(", ")));
                        }

                        // End package processing
                        derived_files.output.push_str("\n");
                        derived_files.on_output()
                            .with_context(|| "Failed to process packages content")?;
                        in_package = false;
                        packages_processed += 1;
                        if packages_processed % 1000 == 0 {
                            log::debug!("Processed {} packages", packages_processed);
                        }
                    }
                    b"rpm:requires"     | b"rpm:provides" | b"rpm:recommends" |
                    b"rpm:supplements"  | b"rpm:enhances" | b"rpm:suggests" |
                    b"rpm:conflicts"    | b"rpm:obsoletes" => {
                        // Clear dependency section when we exit it
                        in_dependency_section.clear();
                    }
                    _ => {
                        // Clear current_tag when we finish an element
                        current_tag.clear();
                    }
                }
            }
            Ok(Event::Eof) => {
                log::debug!("Reached end of XML chunk, processed {} packages", packages_processed);
                break;
            },
            Err(e) => {
                let position = xml_reader.buffer_position() as usize;
                let context = if position < chunk.len() {
                    let start = position.saturating_sub(20);
                    let end = std::cmp::min(position + 20, chunk.len());
                    format!("Context: {}", String::from_utf8_lossy(&chunk[start..end]))
                } else {
                    "Position beyond chunk length".to_string()
                };

                log::error!("Error parsing XML at position {}: {:?}. {}", position, e, context);
                return Err(eyre!("Error parsing XML at position {}: {:?}", position, e));
            },
            _ => {}
        }
        buf.clear();
    }

    Ok(())
}

// Dummy process line function since we're processing XML directly
fn process_xml_package(_line: &str, _derived_files: &mut packages_stream::PackagesStreamline) -> Result<()> {
    // Not used for XML processing - we process chunks directly
    Ok(())
}

