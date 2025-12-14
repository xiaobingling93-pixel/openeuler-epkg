use std::path::PathBuf;
use std::collections::HashMap;
use std::sync::mpsc::Receiver;
use std::io::Read;
use color_eyre::eyre::{self, eyre, WrapErr, Result};
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use crate::models::*;
use crate::repo::{RepoRevise, RepoReleaseItem};
use regex::Regex;
use crate::dirs;
use crate::packages_stream;
use lazy_static::lazy_static;
use flate2::read::GzDecoder;
use zstd;
use std::error::Error;

lazy_static! {
    pub static ref PACKAGE_KEY_MAPPING: std::collections::HashMap<&'static str, &'static str> = {
        let mut m = std::collections::HashMap::new();

        m.insert("name",           "pkgname");
        m.insert("version",        "version");
        m.insert("arch",           "arch");
        m.insert("summary",        "summary");
        m.insert("description",    "description");
        m.insert("url",            "homepage");
        m.insert("license",        "license");
        m.insert("vendor",         "vendor");
        m.insert("group",          "section");
        m.insert("buildhost",      "buildHost");
        m.insert("sourcerpm",      "source");
        m.insert("packager",       "maintainer");
        m.insert("size",           "size");
        m.insert("installed-size", "installedSize");
        m.insert("location",       "location");
        m.insert("checksum",       "sha256");
        m.insert("time",           "buildTime");
        m.insert("requires",       "requires");
        m.insert("recommends",     "recommends");
        m.insert("provides",       "provides");
        m.insert("files",          "files");

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
    let mut current_checksum_type = String::new();
    let mut current_size = 0;
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
                        current_checksum_type.clear();
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
                    b"checksum" => {
                        // Extract checksum type from the checksum element
                        if let Some(checksum_type) = e.attributes()
                            .find(|attr_result| {
                                match attr_result {
                                    Ok(attr) => attr.key.as_ref() == b"type",
                                    Err(_) => false
                                }
                            })
                            .and_then(|attr_result| attr_result.ok())
                            .and_then(|attr| String::from_utf8(attr.value.into_owned())
                                .map_err(|e| {
                                    log::warn!("Failed to convert checksum type attribute value to UTF-8: {}", e);
                                    e
                                }).ok()) {
                            current_checksum_type = checksum_type.to_uppercase();
                        } else {
                            log::warn!("Failed to find 'type' attribute in 'checksum' element, defaulting to SHA256");
                            current_checksum_type = "SHA256".to_string();
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
                            let download_path = crate::mirror::Mirrors::url_to_cache_path(&url, &repo.repodata_name)
                                .with_context(|| format!("Failed to convert URL to cache path: {}", url))?;
                            let need_download = !download_path.exists();

                            let is_packages = current_data_type == "primary";
                            let repo_dir = dirs::get_repo_dir(&repo)
                                .map_err(|e| eyre!("Failed to get repository directory for {}: {}", repo.repo_name, e))?;
                            let output_path = if is_packages {
                                repo_dir.join(format!("packages.txt"))
                            } else {
                                if let Some(ext) = current_location.split('.').last() {
                                    repo_dir.join(format!("filelists.xml.{}", ext))
                                } else {
                                    eyre::bail!("File has no extension: {} check {}", current_location, download_path.display())
                                }
                            };
                            // Check if we need to convert by checking both the output file and its JSON metadata
                            let need_convert = if !output_path.exists() {
                                true // Output file doesn't exist, definitely need to convert
                            } else {
                                // Output file exists, but check if RepoIndex.json exists
                                let repoindex_path = repo_dir.join("RepoIndex.json");
                                !repoindex_path.exists()
                            };

                            info.push(RepoReleaseItem {
                                repo_revise: repo.clone(),
                                need_download,
                                need_convert,
                                arch: repo.arch.clone(),
                                url: url.clone(),
                                package_baseurl: baseurl.to_string(),
                                hash_type: current_checksum_type.clone(),
                                hash: current_checksum.clone(),
                                size: current_size,
                                location: current_location.clone(),
                                is_packages,
                                is_adb: false,
                                output_path,
                                download_path,
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

// Process chunks of data from a reader with proper error handling and logging
fn process_chunks<R: Read>(
    mut reader: R,
    xml_processor: &mut StreamingXmlProcessor,
    unpack_buf: &mut Vec<u8>,
    revise: &RepoReleaseItem,
    decoder_type: &str,
) -> Result<()> {
    log::debug!("process_chunks starting for {} using {} decoder", revise.location, decoder_type);

    loop {
        match reader.read(unpack_buf) {
            Ok(0) => {
                log::debug!("process_chunks EOF reached for {}", revise.location);
                break;
            }
            Ok(n) => {
                log::trace!("process_chunks read {} bytes for {}", n, revise.location);
                if let Err(e) = xml_processor.process_chunk(&unpack_buf[..n]) {
                    let err_msg = format!("Failed to process XML chunk ({} bytes) for {}: {}",
                                       n, revise.location, e);
                    log::error!("{}", err_msg);

                    return Err(eyre!("XML processing error: {}", err_msg));
                }
            }
            Err(e) => {
                // Check if this is due to incomplete download first
                if e.kind() == std::io::ErrorKind::UnexpectedEof {
                    if let Some(incomplete_err) = e.source().and_then(|s| s.downcast_ref::<crate::packages_stream::IncompleteDownloadError>()) {
                        log::info!("Decompression stopped for {} due to incomplete download: {}", revise.location, incomplete_err);
                        return Err(eyre!("Incomplete download: {}", incomplete_err));
                    } else {
                        log::debug!("Decompression stopped for {} due to EOF (expected for incomplete downloads)", revise.location);
                        return Err(eyre!("Decompression error: EOF during decompression for {} (likely due to incomplete download)", revise.location));
                    }
                }

                let err_msg = format!("Decompression error for {} using {} decoder: {}",
                                   revise.location, decoder_type, e);
                log::error!("{}", err_msg);
                return Err(eyre!("Decompression error: {}", err_msg));
            }
        }
    }

    log::debug!("process_chunks completed successfully for {}", revise.location);
    // Finalize the XML processor
    xml_processor.finalize()
        .map_err(|e| eyre!("Failed to finalize XML processor for {}: {}", revise.location, e))?;

    Ok(())
}

pub fn process_packages_content(data_rx: Receiver<Vec<u8>>, repo_dir: &PathBuf, revise: &RepoReleaseItem) -> Result<PackagesFileInfo> {
    log::debug!("[process_packages_content] Starting to process packages content for {} (hash: {}, size: {}, hash_type: {})", revise.location, revise.hash, revise.size, revise.hash_type);

    let mut derived_files = packages_stream::PackagesStreamline::new(revise, repo_dir, process_xml_package)
        .wrap_err_with(|| format!("[process_packages_content] Failed to initialize PackagesStreamline for {}", revise.location))?;

    // Always use automatic hash validation by passing the expected hash and size
    let reader = packages_stream::ReceiverHasher::new_with_hash_type(data_rx, revise.hash.clone(), revise.size.try_into().unwrap(), revise.hash_type.clone());

    // Detect compression type from file extension and use appropriate decoder
    let mut unpack_buf = vec![0u8; 65536];
    let mut xml_processor = StreamingXmlProcessor::new(&mut derived_files);

    if revise.location.ends_with(".zst") {
        log::debug!("Using zstd decoder for {} (expected hash: {}, size: {})", revise.location, revise.hash, revise.size);

        // Use zstd decoder for .zst files
        let zst_decoder_result = zstd::stream::read::Decoder::new(reader);

        // Handle decoder initialization error explicitly
        let zst_decoder = match zst_decoder_result {
            Ok(decoder) => {
                log::debug!("Successfully created zstd decoder for {}", revise.location);
                decoder
            },
            Err(e) => {
                // Check if this is due to incomplete download
                if let Some(io_err) = e.source().and_then(|s| s.downcast_ref::<std::io::Error>()) {
                    if io_err.kind() == std::io::ErrorKind::UnexpectedEof {
                        if let Some(incomplete_err) = io_err.source().and_then(|s| s.downcast_ref::<crate::packages_stream::IncompleteDownloadError>()) {
                            log::info!("Skipping decompression for {} due to incomplete download: {}", revise.location, incomplete_err);
                            return Err(eyre!("Incomplete download: {}", incomplete_err));
                        }
                    }
                }
                let err_msg = format!("Failed to create zstd decoder for {}: {}", revise.location, e);
                log::error!("{}", err_msg);
                return Err(eyre!(err_msg));
            }
        };

        // Process chunks using the zstd decoder
        process_chunks(zst_decoder, &mut xml_processor, &mut unpack_buf, revise, "zst")?;
    } else {
        log::debug!("Using gzip decoder for {} (expected size: {})", revise.location, revise.size);
        // Default to gzip decoder for .gz files or other formats
        let xml_decoder = GzDecoder::new(reader);

        // Process chunks using the gzip decoder
        process_chunks(xml_decoder, &mut xml_processor, &mut unpack_buf, revise, "gz")?;
    }

    // Finalize processing for both decoders
    log::debug!("[process_packages_content] Finalizing processing for {}", revise.location);
    derived_files.on_finish(revise)
        .wrap_err_with(|| format!("[process_packages_content] Failed to finalize processing for {}", revise.location))
}

// Streaming XML processor that maintains state across chunks
pub struct StreamingXmlProcessor<'a> {
    numeric_char_ref_regex: Regex,
    xml_buffer: String,
    derived_files: &'a mut packages_stream::PackagesStreamline,

    // Parser state
    current_tag: String,
    packages_processed: usize,
    in_dependency_section: String,
    dependency_lists: HashMap<String, (Vec<String>, Vec<String>)>,
    files: Vec<String>,
}

impl<'a> StreamingXmlProcessor<'a> {
    fn new(derived_files: &'a mut packages_stream::PackagesStreamline) -> Self {
        let mut dependency_lists = HashMap::new();
        let numeric_char_ref_regex = Regex::new(r"&#x([0-9a-fA-F]+);|&#([0-9]+);").unwrap();

        // Initialize dependency lists
        dependency_lists.insert("requires".to_string(),     (Vec::new(), Vec::new())); // (regular, pre)
        dependency_lists.insert("provides".to_string(),     (Vec::new(), Vec::new()));
        dependency_lists.insert("recommends".to_string(),   (Vec::new(), Vec::new()));
        dependency_lists.insert("supplements".to_string(),  (Vec::new(), Vec::new()));
        dependency_lists.insert("enhances".to_string(),     (Vec::new(), Vec::new()));
        dependency_lists.insert("suggests".to_string(),     (Vec::new(), Vec::new()));
        dependency_lists.insert("conflicts".to_string(),    (Vec::new(), Vec::new()));
        dependency_lists.insert("obsoletes".to_string(),    (Vec::new(), Vec::new()));

        Self {
            xml_buffer: String::new(),
            derived_files,
            current_tag: String::new(),
            numeric_char_ref_regex,
            packages_processed: 0,
            in_dependency_section: String::new(),
            dependency_lists,
            files: Vec::new(),
        }
    }

    fn decode_html_entities(&self, text: &str) -> String {
        // First, handle standard named entities
        let named_entities_transformed = text.replace("&lt;", "<")
             .replace("&gt;", ">")
             .replace("&amp;", "&")
             .replace("&quot;", "\"")
             .replace("&apos;", "'");

        // Then, handle numeric character references (decimal and hex)
        self.numeric_char_ref_regex.replace_all(&named_entities_transformed, |caps: &regex::Captures| {
            let val_hex = caps.get(1).map(|m| u32::from_str_radix(m.as_str(), 16));
            let val_dec = caps.get(2).map(|m| m.as_str().parse::<u32>());

            match (val_hex, val_dec) {
                (Some(Ok(code_point)), _) | (_, Some(Ok(code_point))) => {
                    std::char::from_u32(code_point).map_or_else(
                        || caps.get(0).unwrap().as_str().to_string(), // If invalid char, return original sequence
                        |c| c.to_string()
                    )
                }
                _ => caps.get(0).unwrap().as_str().to_string(), // If parsing fails, return original sequence
            }
        }).into_owned()
    }

    fn process_chunk(&mut self, chunk: &[u8]) -> Result<()> {
        if chunk.is_empty() {
            return Ok(());
        }

        // Convert chunk to string and append to buffer
        let chunk_str = String::from_utf8_lossy(chunk);
        self.xml_buffer.push_str(&chunk_str);

        // Process complete packages
        self.process_complete_packages()
    }

    fn finalize(&mut self) -> Result<()> {
        // Process any remaining complete packages in the buffer
        self.process_complete_packages().context("Failed to process remaining complete packages during finalization")?;

        // Ensure the last package is saved to the index
        // on_new_paragraph() is normally called when a new package starts (to save the previous one),
        // but the last package never triggers a new package start, so we need to save it here
        if !self.derived_files.current_pkgname.is_empty() {
            log::debug!("Saving last package to index: {}", self.derived_files.current_pkgname);
            self.derived_files.on_new_paragraph();
        }

        // Log final statistics
        log::info!("StreamingXmlProcessor finished: processed {} packages total", self.packages_processed);
        Ok(())
    }

    fn process_complete_packages(&mut self) -> Result<()> {
        // Keep looking for complete packages until we can't find any more
        loop {
            // Find the next complete package
            if let Some(package_start) = self.xml_buffer.find("<package type=\"rpm\">") {
                if let Some(package_end_offset) = self.xml_buffer[package_start..].find("</package>") {
                    let package_end = package_start + package_end_offset + "</package>".len();

                    // Extract the complete package XML (clone to avoid borrowing issues)
                    let package_xml = self.xml_buffer[package_start..package_end].to_string();

                    // Process this package
                    self.process_single_package(&package_xml).with_context(|| format!("Failed to process package XML of size {}", package_xml.len()))?;

                    // Remove processed package from buffer
                    self.xml_buffer = self.xml_buffer[package_end..].to_string();
                } else {
                    // No complete package found, keep current buffer for next chunk
                    break;
                }
            } else {
                // No package start found, clear buffer up to a reasonable point
                // but keep some data in case a package tag spans across chunks
                if self.xml_buffer.len() > 100000 {
                    // Keep only the last 1000 characters to avoid memory issues
                    let keep_from = self.xml_buffer.len().saturating_sub(1000);
                    self.xml_buffer = self.xml_buffer[keep_from..].to_string();
                }
                break;
            }
        }
        Ok(())
    }

    fn process_single_package(&mut self, package_xml: &str) -> Result<()> {
        use quick_xml::Reader;
        use quick_xml::events::Event;

        let mut reader = Reader::from_str(package_xml);
        let mut buf = Vec::new();

        // Reset package-level state
        self.current_tag.clear();
        self.in_dependency_section.clear();

        // Clear all dependency lists for new package
        for (_, (regular, pre)) in self.dependency_lists.iter_mut() {
            regular.clear();
            pre.clear();
        }
        self.files.clear();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    let event_clone = e.clone();
                    self.handle_start_event(&event_clone).with_context(|| format!("Failed to handle start event for tag: {}", String::from_utf8_lossy(event_clone.name().as_ref())))?;
                }
                Ok(Event::Text(e)) => {
                    self.handle_text_event(&e).context("Failed to handle text event")?;
                }
                Ok(Event::Empty(ref e)) => {
                    let event_clone = e.clone();
                    self.handle_empty_event(&event_clone).with_context(|| format!("Failed to handle empty event for tag: {}", String::from_utf8_lossy(event_clone.name().as_ref())))?;
                }
                Ok(Event::End(ref e)) => {
                    let event_clone = e.clone();
                    self.handle_end_event(&event_clone).with_context(|| format!("Failed to handle end event for tag: {}", String::from_utf8_lossy(event_clone.name().as_ref())))?;
                }
                Ok(Event::Eof) => break,
                Err(e) => {
                    log::error!("Error parsing package XML: {:?}", e);
                    log::error!("Package XML was: {}", package_xml);
                    return Err(eyre!("Error parsing package XML: {:?}", e));
                }
                _ => {}
            }
            buf.clear();
        }

        self.packages_processed += 1;
        if self.packages_processed % 1000 == 0 {
            log::trace!("Processed {} packages", self.packages_processed);
        }

        Ok(())
    }

    fn handle_start_event(&mut self, e: &quick_xml::events::BytesStart) -> Result<()> {
        match e.name().as_ref() {
            b"package" => {
                self.derived_files.on_new_paragraph();
            }
            b"rpm:requires"     => self.in_dependency_section = "requires".to_string(),
            b"rpm:provides"     => self.in_dependency_section = "provides".to_string(),
            b"rpm:recommends"   => self.in_dependency_section = "recommends".to_string(),
            b"rpm:supplements"  => self.in_dependency_section = "supplements".to_string(),
            b"rpm:enhances"     => self.in_dependency_section = "enhances".to_string(),
            b"rpm:suggests"     => self.in_dependency_section = "suggests".to_string(),
            b"rpm:conflicts"    => self.in_dependency_section = "conflicts".to_string(),
            b"rpm:obsoletes"    => self.in_dependency_section = "obsoletes".to_string(),
            b"checksum" => {
                self.current_tag = "checksum".to_string();
            }
            b"file" => {
                self.current_tag = "file".to_string();
            }
            _ => {
                self.current_tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
            }
        }
        Ok(())
    }

    fn handle_text_event(&mut self, e: &quick_xml::events::BytesText) -> Result<()> {
        if !self.current_tag.is_empty() {
            match e.unescape().map_err(|e| eyre!("XML unescape error: Failed to unescape XML text: {}", e)) {
                Ok(text) => {
                    let text_str = text.to_string().trim().to_string();
                    if !text_str.is_empty() {
                        // Use PACKAGE_KEY_MAPPING for common fields
                        if let Some(mapped_key) = PACKAGE_KEY_MAPPING.get(self.current_tag.as_str()) {
                            if self.current_tag == "name" {
                                self.derived_files.on_new_pkgname(&text_str);
                                self.derived_files.output.push_str(&format!("{}: {}\n", mapped_key, text_str));
                            } else {
                                // Format multi-line text with indentation for follow-up lines
                                let formatted_text = if text_str.contains('\n') {
                                    text_str.replace("\n", "\n ")
                                } else {
                                    text_str
                                };
                                self.derived_files.output.push_str(&format!("{}: {}\n", mapped_key, formatted_text));
                            }
                        } else {
                            // Handle special cases not in the mapping
                            match self.current_tag.as_str() {
                                "checksum" => {
                                    self.derived_files.output.push_str(&format!("sha256: {}\n", text_str));
                                }
                                "file" => {
                                    self.files.push(text_str);
                                }
                                "rpm:license" => {
                                    self.derived_files.output.push_str(&format!("license: {}\n", text_str));
                                }
                                "rpm:vendor" => {
                                    self.derived_files.output.push_str(&format!("vendor: {}\n", text_str));
                                }
                                "rpm:group" => {
                                    if text_str != "Unspecified" {
                                        self.derived_files.output.push_str(&format!("section: {}\n", text_str));
                                    }
                                }
                                "rpm:buildhost" => {
                                    self.derived_files.output.push_str(&format!("buildHost: {}\n", text_str));
                                }
                                "rpm:sourcerpm" => {
                                    self.derived_files.output.push_str(&format!("source: {}\n", text_str));
                                }
                                _ => {
                                    // Log unknown fields for debugging
                                    log::debug!("Unknown text field in package: {} = {}", self.current_tag, text_str);
                                }
                            }
                        }
                    }
                },
                Err(err) => {
                    log::warn!("Failed to unescape XML text for tag {}: {}", self.current_tag, err);
                }
            }
        }
        Ok(())
    }

    fn handle_empty_event(&mut self, e: &quick_xml::events::BytesStart) -> Result<()> {
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
                self.derived_files.output.push_str(&format!("version: {}\n", version_str));
            }
            b"location" => {
                for attr in e.attributes() {
                    if let Ok(attr) = attr {
                        let key = String::from_utf8_lossy(attr.key.as_ref());
                        if let Ok(value) = String::from_utf8(attr.value.to_vec()) {
                            if key == "href" {
                                self.derived_files.output.push_str(&format!("location: {}\n", value));
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
                                    self.derived_files.output.push_str(&format!("size: {}\n", value));
                                }
                                "installed" => {
                                    self.derived_files.output.push_str(&format!("installedSize: {}\n", value));
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
                                self.derived_files.output.push_str(&format!("buildTime: {}\n", value));
                            }
                        }
                    }
                }
            }
            b"rpm:entry" => {
                if !self.in_dependency_section.is_empty() {
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

                    // Store a copy of name for later use
                    let name_copy = name.clone();

                    // Format entry with version if available
                    let formatted_entry = if !ver.is_empty() {
                        // Convert flags to appropriate symbol
                        let flag_symbol = match _flags.as_str() {
                            "EQ" => " = ", // Added spaces to distinguish from "font(:lang=yap)",
                                           // "gstreamer1(decoder-video/x-dv)(systemstream=true)()(64bit)",
                                           // etc. values at split time
                            "GE" => " >= ",
                            "GT" => " > ",
                            "LE" => " <= ",
                            "LT" => " < ",
                            unknown => {
                                log::warn!(
                                    "Encountered unknown rpm dependency flag '{}' in <rpm:entry> (name: '{}', section: '{}'). Defaulting to '='.",
                                    unknown,
                                    name,
                                    self.in_dependency_section
                                );
                                " = "
                            }
                        };

                        // Format version part
                        let version_part = if !rel.is_empty() {
                            if epoch.is_empty() || epoch == "0" {
                                format!("{}-{}", ver, rel)
                            } else {
                                format!("{}:{}-{}", epoch, ver, rel)
                            }
                        } else {
                            if epoch.is_empty() || epoch == "0" {
                                ver.clone()
                            } else {
                                format!("{}:{}", epoch, ver)
                            }
                        };

                        format!("{}{}{}", name, flag_symbol, version_part)
                    } else {
                        name
                    };

                    // Add to appropriate list
                    if let Some((regular, pre)) = self.dependency_lists.get_mut(&self.in_dependency_section) {
                        if is_pre {
                            pre.push(formatted_entry);
                        } else {
                            regular.push(formatted_entry);
                        }
                        if self.in_dependency_section == "provides" {
                            // Decode HTML entities in the name before passing to on_provides
                            let decoded_name = self.decode_html_entities(&name_copy);
                            self.derived_files.on_provides(&decoded_name, PackageFormat::Rpm);
                        }
                    } else {
                        log::warn!("Unknown dependency section: {}", self.in_dependency_section);
                    }
                } else {
                    log::warn!("Found rpm:entry outside of known dependency section");
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_end_event(&mut self, e: &quick_xml::events::BytesEnd) -> Result<()> {
        match e.name().as_ref() {
            b"package" => {
                // Emit all dependency lists in a specific, predictable order
                let dependency_order = [
                    "provides", "requires", "recommends", "suggests",
                    "enhances", "supplements", "conflicts", "obsoletes"
                ];

                // Transform all dependency entries first (before mutably borrowing output)
                // This avoids borrow checker issues with the closure
                let mut transformed_dependency_lists: HashMap<String, (Vec<String>, Vec<String>)> = HashMap::new();
                for (section_name, (regular, pre)) in &self.dependency_lists {
                    let transformed_regular: Vec<String> = regular.iter()
                        .map(|entry| self.decode_html_entities(entry))
                        .collect();
                    let transformed_pre: Vec<String> = pre.iter()
                        .map(|entry| self.decode_html_entities(entry))
                        .collect();
                    transformed_dependency_lists.insert(section_name.clone(), (transformed_regular, transformed_pre));
                }

                // Helper function to output a list of entries
                let output_list = |entries: &[String], key: &str, output: &mut String| {
                    if !entries.is_empty() {
                        output.push_str(&format!("{}: {}\n", key, entries.join(", ")));
                    }
                };

                for section_name in &dependency_order {
                    if let Some((regular, pre)) = transformed_dependency_lists.get(*section_name) {
                        if *section_name == "requires" {
                            // Special handling for requires - emit requiresPre separately
                            output_list(pre, "requiresPre", &mut self.derived_files.output);
                            output_list(regular, "requires", &mut self.derived_files.output);
                        } else {
                            // For other dependency types, combine pre and regular
                            let mut all_entries = pre.clone();
                            all_entries.extend(regular.iter().cloned());
                            output_list(&all_entries, section_name, &mut self.derived_files.output);
                        }
                    }
                }

                // Emit files if any
                if !self.files.is_empty() {
                    let files_str = self.files.join(", ");
                    self.derived_files.output.push_str(&format!("files: {}\n", files_str));
                    // Files are provides (for file-based provides)
                    // Join with comma and pass to on_provides - parse_provides handles comma-separated lists
                    self.derived_files.on_provides(&files_str, PackageFormat::Rpm);
                }

                // End package processing
                self.derived_files.output.push_str("\n");
                self.derived_files.on_output()
                    .with_context(|| "Failed to process packages content")?;
                self.packages_processed += 1;
                if self.packages_processed % 1000 == 0 {
                    log::debug!("Processed {} packages", self.packages_processed);
                }
            }
            b"rpm:requires"     | b"rpm:provides" | b"rpm:recommends" |
            b"rpm:supplements"  | b"rpm:enhances" | b"rpm:suggests" |
            b"rpm:conflicts"    | b"rpm:obsoletes" => {
                // Clear dependency section when we exit it
                self.in_dependency_section.clear();
            }
            _ => {
                // Clear current_tag when we finish an element
                self.current_tag.clear();
            }
        }
        Ok(())
    }
}

// Dummy process line function since we're processing XML directly
fn process_xml_package(_line: &str, _derived_files: &mut packages_stream::PackagesStreamline) -> Result<()> {
    // Not used for XML processing - we process chunks directly
    Ok(())
}

