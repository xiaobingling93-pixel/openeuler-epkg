use lazy_static::lazy_static;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::io::Read;
use color_eyre::eyre::Result;
use color_eyre::eyre;
use flate2::read::GzDecoder;
use tar::Archive;

use crate::models::*;
use crate::repo::RepoReleaseItem;
use crate::packages_stream;

lazy_static! {
    pub static ref PACKAGE_KEY_MAPPING: std::collections::HashMap<&'static str, &'static str> = {
        let mut m = std::collections::HashMap::new();

        // Map APK APKINDEX field names to common field names
        // Based on the APKINDEX format specification
        m.insert("P", "pkgname");       // package name
        m.insert("V", "version");       // package version
        m.insert("A", "arch");          // architecture
        m.insert("S", "size");          // size of entire package
        m.insert("I", "installedSize"); // installed size
        m.insert("T", "summary");       // description
        m.insert("U", "homepage");      // url
        m.insert("L", "license");       // license
        m.insert("o", "source");        // origin
        m.insert("m", "maintainer");    // maintainer
        m.insert("t", "buildTime");     // build time
        m.insert("c", "commit");        // commit
        m.insert("k", "provider_priority"); // provider priority
        m.insert("D", "requires");      // dependencies
        m.insert("p", "provides");      // provides
        m.insert("i", "suggests");      // install if
        m.insert("C", "sha1");          // checksum

        m
    };
}

pub fn process_packages_content(data_rx: Receiver<Vec<u8>>, repo_dir: &PathBuf, revise: &RepoReleaseItem) -> Result<PackagesFileInfo> {
    log::debug!("Starting to process APK packages content for {} (hash: {}, size: {})", revise.location, revise.hash, revise.size);

    let mut derived_files = packages_stream::PackagesStreamline::new(revise, repo_dir, process_line)
        .map_err(|e| eyre::eyre!("Failed to initialize PackagesStreamline for {}: {}", revise.location, e))?;

    // Create streaming reader from receiver with hash validation
    // Note: Use size if available, otherwise fall back to hash-only validation
    let receiver_reader = if revise.size > 0 {
        packages_stream::ReceiverHasher::new_with_size(
            data_rx,
            revise.hash.clone(),
            revise.size.try_into().unwrap()
        )
    } else {
        log::warn!("No size available for {}, using hash-only validation", revise.location);
        packages_stream::ReceiverHasher::new(data_rx, revise.hash.clone())
    };

    // Process concatenated gzip streams with streaming approach
    process_concatenated_gzip_streams_streaming(receiver_reader, &mut derived_files, &revise.location)?;

    log::debug!("Finalizing processing for {}", revise.location);
    derived_files.on_finish(revise)
        .map_err(|e| eyre::eyre!("Failed to finalize processing for {}: {}", revise.location, e))
}

/// Process concatenated gzip streams using streaming approach
fn process_concatenated_gzip_streams_streaming(
    mut receiver_reader: packages_stream::ReceiverHasher,
    derived_files: &mut packages_stream::PackagesStreamline,
    location: &str
) -> Result<()> {
    // Alpine APKINDEX files contain two concatenated gzip streams:
    // 1. First stream: signature data (contains .SIGN files)
    // 2. Second stream: actual APKINDEX tarball (contains DESCRIPTION and APKINDEX)

    log::debug!("Processing concatenated gzip streams for {}", location);

    // Buffer all data first since APK format requires parsing complete concatenated streams
    let mut all_data = Vec::new();
    let mut read_buf = vec![0u8; 65536];

    loop {
        match receiver_reader.read(&mut read_buf) {
            Ok(0) => break, // EOF
            Ok(n) => {
                all_data.extend_from_slice(&read_buf[..n]);
                log::trace!("Read {} bytes, total buffer now has {} bytes", n, all_data.len());
            }
            Err(e) => return Err(eyre::eyre!("Failed to read from receiver: {}", e)),
        }
    }

    if all_data.is_empty() {
        return Err(eyre::eyre!("No data received for {}", location));
    }

    log::debug!("Processing {} bytes of concatenated gzipped data for {}", all_data.len(), location);

    // Process each gzip stream in the concatenated file
    let mut cursor_position = 0;
    let mut stream_count = 0;

    while cursor_position < all_data.len() {
        // Find next gzip stream
        let gzip_position = find_next_gzip_stream(&all_data, cursor_position)?;
        if gzip_position.is_none() {
            log::debug!("No more gzip streams found at position {}", cursor_position);
            break;
        }

        let gzip_pos = gzip_position.unwrap();
        log::debug!("Found gzip stream {} at position {} for {}", stream_count, gzip_pos, location);

        // Try to process this gzip stream
        if process_single_gzip_stream(&all_data, gzip_pos, derived_files, location, stream_count)? {
            // Found and processed APKINDEX successfully
            log::debug!("Successfully found and processed APKINDEX in stream {}", stream_count);
            return Ok(());
        }

        // Move to search for next gzip stream
        cursor_position = gzip_pos + 1;
        stream_count += 1;
    }

    Err(eyre::eyre!("APKINDEX file not found in any tar archive in: {}. Processed {} gzip streams.", location, stream_count))
}

/// Find the next gzip stream starting from the given position
fn find_next_gzip_stream(data: &[u8], start_pos: usize) -> Result<Option<usize>> {
    if start_pos >= data.len() {
        return Ok(None);
    }

    for pos in start_pos..data.len().saturating_sub(1) {
        if data[pos] == 0x1f && data[pos + 1] == 0x8b {
            return Ok(Some(pos));
        }
    }

    Ok(None)
}

/// Process a single gzip stream and return true if APKINDEX was found and processed
fn process_single_gzip_stream(
    all_data: &[u8],
    gzip_position: usize,
    derived_files: &mut packages_stream::PackagesStreamline,
    location: &str,
    stream_num: usize
) -> Result<bool> {
    let stream_data = &all_data[gzip_position..];
    let cursor = std::io::Cursor::new(stream_data);
    let gz_decoder = GzDecoder::new(cursor);
    let mut tar_archive = Archive::new(gz_decoder);

    // Enable ignore_zeros to handle potential padding issues
    tar_archive.set_ignore_zeros(true);

    let entries = tar_archive.entries()
        .map_err(|e| eyre::eyre!("Failed to read tar archive entries at position {} in stream {}: {}", gzip_position, stream_num, e))?;

    log::debug!("Successfully got tar entries iterator for stream {} at position {}", stream_num, gzip_position);

    for (entry_idx, entry_result) in entries.enumerate() {
        let entry = entry_result
            .map_err(|e| eyre::eyre!("Failed to read tar entry {} in stream {}: {}", entry_idx, stream_num, e))?;

        if process_tar_entry(entry, entry_idx, gzip_position, derived_files, location, stream_num)? {
            // Found and processed APKINDEX
            return Ok(true);
        }
    }

    // This stream didn't contain APKINDEX
    log::debug!("Stream {} at position {} didn't contain APKINDEX", stream_num, gzip_position);
    Ok(false)
}

/// Process a single tar entry, return true if it was the APKINDEX file that was processed
fn process_tar_entry(
    entry: tar::Entry<GzDecoder<std::io::Cursor<&[u8]>>>,
    entry_idx: usize,
    gzip_position: usize,
    derived_files: &mut packages_stream::PackagesStreamline,
    location: &str,
    stream_num: usize
) -> Result<bool> {
    let path = entry.path()
        .map_err(|e| eyre::eyre!("Failed to get path for tar entry {} in stream {}: {}", entry_idx, stream_num, e))?;

    let path_str = path.to_string_lossy();
    log::debug!("Found tar entry {}: '{}' in stream {} at position {}", entry_idx, path_str, stream_num, gzip_position);

    // Check if this is the APKINDEX file
    let is_apkindex = path.file_name()
        .and_then(|name| name.to_str())
        == Some("APKINDEX");

    if !is_apkindex {
        log::trace!("Skipping tar entry: '{}' in stream {} at position {}", path_str, stream_num, gzip_position);
        return Ok(false);
    }

    log::debug!("Found APKINDEX file in tar archive at entry {} in stream {} at position {}", entry_idx, stream_num, gzip_position);

    // Process the APKINDEX content
    process_apkindex_content_streaming(entry, derived_files, location)?;

    Ok(true)
}

/// Process the content of the APKINDEX file using streaming approach
fn process_apkindex_content_streaming(
    mut entry: tar::Entry<GzDecoder<std::io::Cursor<&[u8]>>>,
    derived_files: &mut packages_stream::PackagesStreamline,
    location: &str
) -> Result<()> {
    let mut unpack_buf = vec![0u8; 65536];
    let mut chunk_count = 0;

    log::debug!("Processing APKINDEX content for {}", location);

    loop {
        let read_result = entry.read(&mut unpack_buf);
        chunk_count += 1;

        if chunk_count % 100 == 0 {
            log::trace!("Processed {} chunks for {}", chunk_count, location);
        }

        let should_continue = derived_files.handle_chunk(read_result, &unpack_buf)
            .map_err(|e| eyre::eyre!("Failed to handle chunk {} for {}: {}", chunk_count, location, e))?;

        if !should_continue {
            log::debug!("Finished processing APKINDEX after {} chunks for {}", chunk_count, location);
            break;
        }
    }

    Ok(())
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
    } else if let Some((key, value)) = line.split_once(':') {
        // APK format uses single letter keys followed by colon
        let key = key.trim();
        let value = value.trim();

        if let Some(mapped_key) = PACKAGE_KEY_MAPPING.get(key) {
            if !mapped_key.is_empty() {
                derived_files.output.push_str(&format!("\n{}: {}", mapped_key, value));
            }

            match key {
                "P" => {
                    // Start tracking the new package
                    derived_files.on_new_pkgname(value);
                }
                "p" => {
                    // Provides field - split by spaces and extract only package names
                    let provides: Vec<&str> = value.split_whitespace()
                        .filter(|s| !s.is_empty())
                        .map(|s| {
                            // Remove version part after '=' when followed by a digit
                            // Examples:
                            //   boost-atomic=1.84.0-r3
                            //   so:libc.musl-x86_64.so.1=1
                            //   so:libcairo-gobject.so.2=2.11804.4
                            if let Some(i) = s.find('=') {
                                if i + 1 < s.len() && s[i+1..].chars().next().map_or(false, |c| c.is_ascii_digit()) {
                                    &s[..i]
                                } else {
                                    s
                                }
                            } else {
                                s
                            }
                        })
                        .collect();
                    derived_files.on_provides(provides);
                }
                _ => {}
            }
        } else {
            log::warn!("Unexpected key in APK line -- {}: {}", key, value);
        }
    }

    Ok(())
}
