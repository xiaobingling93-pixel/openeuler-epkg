use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::Path;
use regex::Regex;
use flate2::read::GzDecoder;
use liblzma::read::XzDecoder;
use zstd::stream::read::Decoder as ZstdDecoder;
use crossbeam_channel::Receiver;
use std::thread;
use std::sync::Arc;
use std::path::PathBuf;
use color_eyre::eyre::{Result, Context};
use color_eyre::eyre::eyre;
use crate::models::{repodata_indice};
use memmap2::Mmap;
use log::warn;
// Import regex::bytes::Regex with an alias to avoid name conflict
use regex::bytes::Regex as BytesRegex;
use aho_corasick::AhoCorasick;
use regex::bytes::RegexBuilder;
use memchr::{memchr, memrchr};

pub struct SearchOptions {
    pub files: bool,
    pub paths: bool,
    pub regexp: bool,
    pub pattern: String,
}

pub fn search_repo_cache(options: &SearchOptions) -> Result<()> {
    let repodata_indice = repodata_indice();
    let mut any_filelists = false;

    for repo_index in repodata_indice.values() {
        let repo_dir = PathBuf::from(&repo_index.repo_dir_path);

        for shard in repo_index.repo_shards.values() {
            if options.files || options.paths {
                if let Some(filelists) = &shard.filelists {
                    let filelists_path = repo_dir.join(&filelists.filename);
                    if filelists_path.exists() {
                        search_filelists(vec![filelists_path], options)
                            .with_context(|| format!("Failed to search filelists in {}", repo_index.repodata_name))?;
                        any_filelists = true;
                    } else {
                        warn!("Filelists not found at {}", filelists_path.display());
                    }
                }
            } else {
                let filename = shard.packages.filename.clone();
                search_packages_fast(&repo_dir.join(&filename), options)
                    .with_context(|| format!("Failed to search package info in {}", repo_index.repodata_name))?;
            }
        }
    }

    if !any_filelists && (options.files || options.paths) {
        warn!("No filelists found in any repository");
    }

    Ok(())
}

pub fn search_filelists(filelists: Vec<PathBuf>, options: &SearchOptions) -> Result<()> {
    // Create a channel for communication between producer and consumer
    let (tx, rx) = crossbeam_channel::unbounded();

    // Start the producer thread with chunked processing
    let producer_handle = start_filelists_producer_chunked(filelists.clone(), tx);

    // Process the filelists based on the options
    if options.regexp {
        // For regex searches, try to extract a literal prefix for optimization
        process_filelists_with_regex_chunked(rx, filelists, options)?
    } else {
        // For non-regex searches, use Aho-Corasick for efficient pattern matching
        process_filelists_with_aho_corasick_chunked(rx, filelists, options)?
    }

    // Wait for the producer to finish
    producer_handle.join().unwrap()?;

    Ok(())
}

// Start a producer thread to read and send file contents in chunks
fn start_filelists_producer_chunked(filelists: Vec<PathBuf>, tx: crossbeam_channel::Sender<Vec<u8>>) -> thread::JoinHandle<Result<()>> {
    thread::spawn(move || {
        // Use a reasonably sized buffer for chunks (32KB)
        const CHUNK_SIZE: usize = 32 * 1024;

        for filelist in filelists {
            let file = File::open(&filelist)?;
            let mut reader: Box<dyn std::io::Read> = if filelist.to_string_lossy().ends_with(".gz") {
                Box::new(GzDecoder::new(file))
            } else if filelist.to_string_lossy().ends_with(".xz") {
                Box::new(XzDecoder::new(file))
            } else if filelist.to_string_lossy().ends_with(".zst") {
                Box::new(ZstdDecoder::new(file)?)
            } else {
                Box::new(file)
            };

            // Use a buffer for reading and a separate buffer for leftover data
            let mut buffer = vec![0; CHUNK_SIZE];
            let mut leftover = Vec::new();

            // Read chunks and send them through the channel
            loop {
                // Read into the buffer after any leftover data
                let bytes_read = reader.read(&mut buffer[leftover.len()..])?;
                if bytes_read == 0 && leftover.is_empty() {
                    break; // End of file and no leftover data
                }

                // Calculate the total data we have
                let total_data = leftover.len() + bytes_read;

                // If we have no data, we're done
                if total_data == 0 {
                    break;
                }

                // Find the last newline in the data
                let mut end_pos = total_data;

                // Only look for line boundaries if we have a full chunk or we're not at EOF
                if bytes_read == CHUNK_SIZE - leftover.len() {
                    for i in (0..total_data).rev() {
                        if buffer[i] == b'\n' {
                            end_pos = i + 1; // Include the newline
                            break;
                        }
                    }
                }

                // Send the chunk up to the last complete line
                if end_pos > 0 {
                    let chunk = buffer[0..end_pos].to_vec();
                    if tx.send(chunk).is_err() {
                        // Channel is closed, receiver has terminated
                        return Ok(());
                    }
                }

                // Save any leftover data for the next iteration
                if end_pos < total_data {
                    leftover = buffer[end_pos..total_data].to_vec();
                    // Copy leftover to the beginning of the buffer
                    buffer[0..leftover.len()].copy_from_slice(&leftover);
                } else {
                    leftover.clear();
                }

                // If we're at EOF and have sent all data, we're done
                if bytes_read == 0 {
                    break;
                }
            }
        }
        Ok(())
    })
}

// Process filelists using regex pattern matching with chunked data
fn process_filelists_with_regex_chunked(rx: Receiver<Vec<u8>>, filelists: Vec<PathBuf>, options: &SearchOptions) -> Result<()> {
    // Create a regex from the pattern
    let regex = Arc::new(Regex::new(&options.pattern)?);

    // Try to extract a literal prefix from the regex for optimization
    let prefix = extract_literal_prefix(&options.pattern);

    // Check if we have a useful prefix and use it for optimization
    if let Some(prefix) = prefix {
        // Process based on filelist format
        if filelists.iter().any(|p| p.to_string_lossy().contains("filelists.xml")) {
            // RPM XML format
            process_rpm_filelists_with_prefix_chunked(rx, regex, prefix, options)
        } else {
            // Simple format
            process_simple_filelists_with_prefix_chunked(rx, regex, prefix, options)
        }
    } else {
        // No useful prefix, fall back to full regex matching
        if filelists.iter().any(|p| p.to_string_lossy().contains("filelists.xml")) {
            // RPM XML format
            process_rpm_filelists_with_regex_chunked(rx, regex, options)
        } else {
            // Simple format
            process_simple_filelists_with_regex_chunked(rx, regex, options)
        }
    }
}

// Process filelists using Aho-Corasick pattern matching with chunked data
fn process_filelists_with_aho_corasick_chunked(rx: Receiver<Vec<u8>>, filelists: Vec<PathBuf>, options: &SearchOptions) -> Result<()> {
    // Convert pattern to bytes for Aho-Corasick
    let pattern = options.pattern.as_bytes().to_vec();

    // Process based on filelist format
    if filelists.iter().any(|p| p.to_string_lossy().contains("filelists.xml")) {
        // RPM XML format
        process_rpm_filelists_with_aho_corasick_chunked(rx, pattern, options)
    } else {
        // Simple format
        process_simple_filelists_with_aho_corasick_chunked(rx, pattern, options)
    }
}

// Helper function to find line boundaries in a chunk of data
fn find_line_boundaries(data: &[u8], start_pos: usize, end_pos: usize) -> (usize, usize) {
    // Find start of line (previous newline + 1 or 0)
    let line_start = if start_pos == 0 {
        0
    } else {
        let mut pos = start_pos;
        while pos > 0 {
            pos -= 1;
            if data[pos] == b'\n' {
                return (pos + 1, end_pos);
            }
        }
        0
    };

    // Find end of line (next newline or end of data)
    let mut line_end = end_pos;
    while line_end < data.len() {
        if data[line_end] == b'\n' {
            break;
        }
        line_end += 1;
    }

    (line_start, line_end)
}

// Extract a line from a chunk as a string
fn extract_line(data: &[u8], start: usize, end: usize) -> String {
    String::from_utf8_lossy(&data[start..end]).to_string()
}

fn process_rpm_filelists_with_prefix_chunked(rx: Receiver<Vec<u8>>, regex: Arc<Regex>, prefix: Vec<u8>, _options: &SearchOptions) -> Result<()> {
    let mut current_pkgname = String::new();

    // Create patterns for Aho-Corasick
    let patterns = vec![b"<package".to_vec(), b"  <file>".to_vec(), prefix];
    let ac = match AhoCorasick::new(&patterns) {
        Ok(ac) => ac,
        Err(e) => return Err(eyre!("Failed to create Aho-Corasick automaton: {}", e)),
    };

    while let Ok(chunk) = rx.recv() {
        let mut matches = ac.find_iter(&chunk);

        while let Some(mat) = matches.next() {
            let pattern_idx = mat.pattern().as_usize();
            let match_start = mat.start();
            let match_end = mat.end();

            // Find the line boundaries for this match
            let (line_start, line_end) = find_line_boundaries(&chunk, match_start, match_end);
            let line = extract_line(&chunk, line_start, line_end);

            match pattern_idx {
                0 => {
                    // pkgname pattern
                    if let Some(name_start) = line.find("name=\"") {
                        if let Some(name_end) = line[name_start + 6..].find("\"") {
                            current_pkgname = line[name_start + 6..name_start + 6 + name_end].to_string();
                        }
                    }
                },
                1 => {
                    // file pattern
                    let file_path = line.trim_start_matches("  <file>").trim_end_matches("</file>");

                    if regex.is_match(file_path) {
                        println!("{} {}", current_pkgname, file_path);
                    }
                },
                2 => {
                    // Our literal prefix matched, verify with full regex
                    if regex.is_match(&line) {
                        if let Some((pkgname, path)) = line.split_once(' ') {
                            println!("{} {}", pkgname, path);
                        }
                    }
                },
                _ => unreachable!()
            }
        }
    }

    Ok(())
}

fn process_rpm_filelists_with_regex_chunked(rx: Receiver<Vec<u8>>, regex: Arc<Regex>, options: &SearchOptions) -> Result<()> {
    let mut current_pkgname = String::new();

    // Create patterns to quickly identify important lines
    let package_pattern = b"<package";
    let file_pattern = b"  <file>";

    while let Ok(chunk) = rx.recv() {
        // First pass: find all package and file lines
        let mut i = 0;
        while i < chunk.len() {
            // Look for package lines
            if i + package_pattern.len() <= chunk.len() && &chunk[i..i+package_pattern.len()] == package_pattern {
                // Found a package line, extract the line
                let (line_start, line_end) = find_line_boundaries(&chunk, i, i + package_pattern.len());
                let line = extract_line(&chunk, line_start, line_end);

                // Extract package name
                if let Some(name_start) = line.find("name=\"") {
                    if let Some(name_end) = line[name_start + 6..].find("\"") {
                        current_pkgname = line[name_start + 6..name_start + 6 + name_end].to_string();
                    }
                }

                i = line_end;
            }
            // Look for file lines
            else if i + file_pattern.len() <= chunk.len() && &chunk[i..i+file_pattern.len()] == file_pattern {
                // Found a file line, extract the line
                let (line_start, line_end) = find_line_boundaries(&chunk, i, i + file_pattern.len());
                let line = extract_line(&chunk, line_start, line_end);

                // Process file path
                let file_path = line.trim_start_matches("  <file>").trim_end_matches("</file>");

                let matches = if options.files {
                    let filename = Path::new(file_path).file_name().unwrap_or_default().to_str().unwrap_or_default();
                    regex.is_match(filename)
                } else {
                    regex.is_match(file_path)
                };

                if matches {
                    println!("{} {}", current_pkgname, file_path);
                }

                i = line_end;
            } else {
                i += 1;
            }
        }
    }

    Ok(())
}

/* Example input:
<package pkgid="e01a85beb0abfbb377f060882d281d3052e0cbadf77d67c9ff1d4533c42f0d17" name="CUnit" arch="x86_64">
  <version epoch="0" ver="2.1.3" rel="24.oe2403"/>
  <file>/etc/ima/digest_lists.tlv/0-metadata_list-compact_tlv-CUnit-2.1.3-24.oe2403.x86_64</file>
  <file>/etc/ima/digest_lists/0-metadata_list-compact-CUnit-2.1.3-24.oe2403.x86_64</file>
  <file>/usr/lib64/libcunit.so.1</file>
  <file>/usr/lib64/libcunit.so.1.0.1</file>
  <file type="dir">/usr/share/CUnit</file>
  <file>/usr/share/CUnit/CUnit-List.dtd</file>
  <file>/usr/share/CUnit/CUnit-List.xsl</file>
*/
// Constants for XML tags we need to find
const PACKAGE_TAG: &[u8] = b"<package";
const FILE_TAG: &[u8] = b"<file>";
const FILE_END_TAG: &[u8] = b"</file>";
const NAME_ATTR: &[u8] = b"name=\"";
const QUOTE: u8 = b'\"';
const NEWLINE: u8 = b'\n';

// Find the line boundaries containing a match
fn find_line_boundaries_memchr(data: &[u8], match_pos: usize) -> (usize, usize) {
    // Find start of line (previous newline or start of data)
    let line_start = match memrchr(NEWLINE, &data[..match_pos]) {
        Some(pos) => pos + 1, // Skip the newline
        None => 0,
    };

    // Find end of line (next newline or end of data)
    let line_end = match memchr(NEWLINE, &data[match_pos..]) {
        Some(pos) => match_pos + pos,
        None => data.len(),
    };

    (line_start, line_end)
}

// Extract file path from a line containing a file element
fn extract_file_path(line: &[u8]) -> Option<String> {
    if let Some(file_start) = memchr::memmem::find(line, FILE_TAG) {
        let file_start = file_start + FILE_TAG.len();

        if let Some(file_end) = memchr::memmem::find(&line[file_start..], FILE_END_TAG) {
            return Some(String::from_utf8_lossy(&line[file_start..file_start + file_end]).to_string());
        }
    }
    None
}

// Find package name in a chunk of data, searching backward from a given position
fn find_package_name(data: &[u8], search_end: usize) -> Option<String> {
    let mut pkg_search_end = search_end;

    while pkg_search_end > 0 {
        if let Some(pkg_pos) = memchr::memmem::rfind(&data[..pkg_search_end], PACKAGE_TAG) {
            // Found a package tag, extract the name
            if let Some(name_pos) = memchr::memmem::find(&data[pkg_pos..pkg_search_end], NAME_ATTR) {
                let name_start = pkg_pos + name_pos + NAME_ATTR.len();
                if let Some(name_end) = memchr(QUOTE, &data[name_start..pkg_search_end]) {
                    return Some(String::from_utf8_lossy(&data[name_start..name_start + name_end]).to_string());
                }
            }
            // If we couldn't extract the name, move search position before this package tag
            pkg_search_end = pkg_pos;
        } else {
            // No more package tags found
            break;
        }
    }

    None
}

// Check if a file path should be printed based on search options
fn should_print_file(file_path: &str, pattern: &[u8], options: &SearchOptions) -> bool {
    if options.files {
        let filename = Path::new(file_path).file_name()
            .unwrap_or_default().to_str().unwrap_or_default();
        filename.as_bytes().windows(pattern.len()).any(|window| window == pattern)
    } else {
        true // We already found the pattern in the line
    }
}

fn process_rpm_filelists_with_aho_corasick_chunked(rx: Receiver<Vec<u8>>, pattern: Vec<u8>, options: &SearchOptions) -> Result<()> {
    // Buffer to store partial XML elements across chunks
    let mut buffer = Vec::new();
    // Keep the previous chunk for backward package search
    let mut prev_chunk: Option<Vec<u8>> = None;

    while let Ok(mut chunk) = rx.recv() {
        // If we have data in the buffer from a previous chunk, prepend it
        if !buffer.is_empty() {
            let mut combined = buffer.clone();
            combined.extend_from_slice(&chunk);
            chunk = combined;
            buffer.clear();
        }

        // Start position for our search
        let mut pos = 0;

        // Process the chunk
        while pos < chunk.len() {
            // First search for our pattern
            if let Some(pattern_pos) = memchr::memmem::find(&chunk[pos..], &pattern) {
                let pattern_pos = pos + pattern_pos;

                // Find the line containing this pattern
                let (line_start, line_end) = find_line_boundaries_memchr(&chunk, pattern_pos);
                let line = &chunk[line_start..line_end];

                // Check if this is a file element line and extract the file path
                if memchr::memmem::find(line, FILE_TAG).is_some() {
                    if let Some(file_path) = extract_file_path(line) {
                        // Check if we should print this file based on options
                        if should_print_file(&file_path, &pattern, options) {
                            // Try to find package name in current chunk
                            let mut found_pkgname = false;

                            // Search in current chunk
                            if let Some(pkgname) = find_package_name(&chunk, line_start) {
                                println!("{} {}", pkgname, file_path);
                                found_pkgname = true;
                            }
                            // If not found, search in previous chunk
                            else if let Some(prev) = &prev_chunk {
                                if let Some(pkgname) = find_package_name(prev, prev.len()) {
                                    println!("{} {}", pkgname, file_path);
                                    found_pkgname = true;
                                }
                            }

                            // If we still couldn't find a package name, use a placeholder
                            if !found_pkgname {
                                println!("unknown-package {}", file_path);
                            }
                        }
                    }
                }

                // Move position past this match
                pos = line_end + 1;
            } else {
                // No more matches in this chunk
                // Save the last part of the chunk that might contain a partial match
                if chunk.len() - pos > pattern.len() {
                    buffer.extend_from_slice(&chunk[chunk.len() - pattern.len()..]);
                } else {
                    buffer.extend_from_slice(&chunk[pos..]);
                }
                break;
            }
        }

        // Store current chunk as previous before moving to next chunk
        prev_chunk = Some(chunk);
    }

    Ok(())
}

// Process simple filelists with regex and literal prefix optimization (chunked version)
fn process_simple_filelists_with_prefix_chunked(rx: Receiver<Vec<u8>>, regex: Arc<Regex>, prefix: Vec<u8>, options: &SearchOptions) -> Result<()> {
    // Create the Aho-Corasick automaton with the prefix
    let patterns = vec![prefix.clone()];
    let ac = match AhoCorasick::new(&patterns) {
        Ok(ac) => ac,
        Err(e) => return Err(eyre!("Failed to create Aho-Corasick automaton: {}", e)),
    };

    while let Ok(chunk) = rx.recv() {
        // Find all matches of the prefix in the chunk
        for mat in ac.find_iter(&chunk) {
            // Extract the line containing the match
            let (line_start, line_end) = find_line_boundaries(&chunk, mat.start(), mat.end());
            let line = extract_line(&chunk, line_start, line_end);

            // Verify with full regex
            if let Some((pkgname, path)) = line.split_once(' ') {
                let matches = if options.files {
                    let filename = Path::new(path).file_name().unwrap_or_default().to_str().unwrap_or_default();
                    regex.is_match(filename)
                } else {
                    regex.is_match(path)
                };

                if matches {
                    println!("{} {}", pkgname, path);
                }
            }
        }
    }

    Ok(())
}

// Process simple filelists with full regex matching (chunked version)
fn process_simple_filelists_with_regex_chunked(rx: Receiver<Vec<u8>>, regex: Arc<Regex>, options: &SearchOptions) -> Result<()> {
    // Use a newline pattern to find line boundaries
    let newline = b'\n';

    while let Ok(chunk) = rx.recv() {
        let mut start = 0;

        // Process each line in the chunk
        for i in 0..chunk.len() {
            if chunk[i] == newline || i == chunk.len() - 1 {
                // Extract the line
                let end = if i == chunk.len() - 1 && chunk[i] != newline { i + 1 } else { i };
                let line = extract_line(&chunk, start, end);

                // Process the line
                if let Some((pkgname, path)) = line.split_once(' ') {
                    let matches = if options.files {
                        let filename = Path::new(path).file_name().unwrap_or_default().to_str().unwrap_or_default();
                        regex.is_match(filename)
                    } else {
                        regex.is_match(path)
                    };

                    if matches {
                        println!("{} {}", pkgname, path);
                    }
                }

                start = i + 1;
            }
        }
    }

    Ok(())
}

// Process simple filelists using Aho-Corasick for non-regex pattern matching (chunked version)
fn process_simple_filelists_with_aho_corasick_chunked(rx: Receiver<Vec<u8>>, pattern: Vec<u8>, options: &SearchOptions) -> Result<()> {
    // Create an Aho-Corasick automaton for the pattern
    let ac = match AhoCorasick::new(&[pattern.clone()]) {
        Ok(ac) => ac,
        Err(e) => return Err(eyre!("Failed to create Aho-Corasick automaton: {}", e)),
    };

    while let Ok(chunk) = rx.recv() {
        // Find all matches in the chunk
        for mat in ac.find_iter(&chunk) {
            // Extract the line containing the match
            let (line_start, line_end) = find_line_boundaries(&chunk, mat.start(), mat.end());
            let line = extract_line(&chunk, line_start, line_end);

            // Process the line
            if let Some((pkgname, path)) = line.split_once(' ') {
                if options.files {
                    // For --files option, check if the pattern matches the filename only
                    let filename = Path::new(path).file_name().unwrap_or_default().to_str().unwrap_or_default();
                    if filename.as_bytes().windows(pattern.len()).any(|window| window == pattern.as_slice()) {
                        println!("{} {}", pkgname, path);
                    }
                } else {
                    // Pattern already matched in the path
                    println!("{} {}", pkgname, path);
                }
            }
        }
    }

    Ok(())
}

// Common structure to hold the state during search operations
struct PackagesSearchState<'a> {
    current_pkgname: &'a [u8],
    current_summary: &'a [u8],
    stdout: BufWriter<std::io::Stdout>,
}

impl<'a> PackagesSearchState<'a> {
    fn new() -> Self {
        PackagesSearchState {
            current_pkgname: &b""[..],
            current_summary: &b""[..],
            stdout: BufWriter::new(std::io::stdout()),
        }
    }

    fn print_match(&mut self) -> Result<()> {
        writeln!(
            self.stdout,
            "{} - {}",
            String::from_utf8_lossy(self.current_pkgname),
            String::from_utf8_lossy(self.current_summary)
        )?;
        Ok(())
    }
}

pub fn search_packages_fast(packages_path: &Path, options: &SearchOptions) -> Result<()> {
    // Memory map the file
    let file = File::open(packages_path)?;
    let mmap = unsafe { Mmap::map(&file)? };

    // Create patterns for Aho-Corasick
    let pkgname_pattern = b"pkgname: ";
    let summary_pattern = b"summary: ";

    // Choose the fastest matcher based on options
    if options.regexp {
        search_with_regex(&mmap, options, pkgname_pattern, summary_pattern)?
    } else {
        search_with_aho_corasick(&mmap, options, pkgname_pattern, summary_pattern)?
    }

    Ok(())
}

// Search using regex with potential literal prefix optimization
fn search_with_regex(mmap: &Mmap, options: &SearchOptions, pkgname_pattern: &[u8], summary_pattern: &[u8]) -> Result<()> {
    // Create regex for pattern matching
    let regex = RegexBuilder::new(&options.pattern)
        .unicode(false)
        .build()?;

    // Since regex.prefixes() isn't available in this version, we'll use a simpler approach
    // Try to extract a literal prefix if the pattern starts with a literal
    let literal_prefix = extract_literal_prefix(&options.pattern);

    if let Some(prefix) = literal_prefix {
        // We have a literal prefix to use for optimization
        search_with_regex_prefix(mmap, &regex, prefix, pkgname_pattern, summary_pattern)
    } else {
        // No literal prefix available, use full regex search
        search_with_full_regex(mmap, &regex, pkgname_pattern, summary_pattern)
    }
}

// Helper function to extract a literal prefix from a regex pattern if possible
fn extract_literal_prefix(pattern: &str) -> Option<Vec<u8>> {
    // Very simple heuristic: if the pattern starts with non-special characters, use them as prefix
    let special_chars = ['.', '*', '+', '?', '|', '(', ')', '[', ']', '{', '}', '^', '$', '\\'];

    // Get the first few characters that are not regex special characters
    let prefix: String = pattern.chars()
        .take_while(|&c| !special_chars.contains(&c))
        .collect();

    if prefix.is_empty() {
        None
    } else {
        Some(prefix.into_bytes())
    }
}

// Search using a single regex prefix with Aho-Corasick for pre-filtering
fn search_with_regex_prefix(
    mmap: &Mmap,
    regex: &BytesRegex,
    prefix: Vec<u8>,
    pkgname_pattern: &[u8],
    summary_pattern: &[u8],
) -> Result<()> {
    // Create patterns for Aho-Corasick
    let patterns = vec![pkgname_pattern, summary_pattern, &prefix];

    // Create the Aho-Corasick automaton
    let ac = match AhoCorasick::new(patterns) {
        Ok(ac) => ac,
        Err(e) => return Err(eyre!("Failed to create Aho-Corasick automaton: {}", e)),
    };

    let mut state = PackagesSearchState::new();

    // Iterate lines manually (faster than .split())
    let mut start = 0;
    for (i, &byte) in mmap.iter().enumerate() {
        if byte != b'\n' { continue; }

        let line = &mmap[start..i];
        start = i + 1;

        if line.is_empty() { continue; }

        // Find all matches in the current line
        let mut matches = ac.find_iter(line);

        // Check what kind of match we have
        let mut is_pkgname = false;
        let mut is_summary = false;
        let mut has_pattern_match = false;

        while let Some(mat) = matches.next() {
            match mat.pattern().as_usize() {
                0 => {
                    // pkgname pattern
                    state.current_pkgname = &line[mat.end()..];
                    is_pkgname = true;
                },
                1 => {
                    // summary pattern
                    state.current_summary = &line[mat.end()..];
                    is_summary = true;
                },
                2 => {
                    // Our literal prefix matched, verify with full regex
                    if regex.is_match(line) {
                        has_pattern_match = true;
                    }
                },
                _ => unreachable!()
            }
        }

        // If we didn't find a pkgname or summary pattern but found a potential match
        if !is_pkgname && !is_summary && has_pattern_match {
            state.print_match()?;
        }
    }

    Ok(())
}

// Search using full regex when no literal prefixes are available
fn search_with_full_regex(
    mmap: &Mmap,
    regex: &BytesRegex,
    pkgname_pattern: &[u8],
    summary_pattern: &[u8],
) -> Result<()> {
    let mut state = PackagesSearchState::new();

    let mut start = 0;
    for (i, &byte) in mmap.iter().enumerate() {
        if byte != b'\n' { continue; }

        let line = &mmap[start..i];
        start = i + 1;

        if line.is_empty() { continue; }

        if let Some(rest) = strip_prefix(line, pkgname_pattern) {
            state.current_pkgname = rest;
        } else if let Some(rest) = strip_prefix(line, summary_pattern) {
            state.current_summary = rest;
        } else if regex.is_match(line) {
            state.print_match()?;
        }
    }

    Ok(())
}

// Search using Aho-Corasick for non-regex patterns
fn search_with_aho_corasick(
    mmap: &Mmap,
    options: &SearchOptions,
    pkgname_pattern: &[u8],
    summary_pattern: &[u8],
) -> Result<()> {
    let user_pattern = options.pattern.as_bytes();

    // Create patterns for Aho-Corasick
    // Add the newline pattern to detect line boundaries
    let patterns = vec![pkgname_pattern, summary_pattern, user_pattern, b"\n"];

    // Create the Aho-Corasick automaton with proper error handling
    let ac = match AhoCorasick::new(patterns) {
        Ok(ac) => ac,
        Err(e) => return Err(eyre!("Failed to create Aho-Corasick automaton: {}", e)),
    };

    let mut state = PackagesSearchState::new();
    let mut current_line_start = 0;

    // Track state for the current line
    let mut is_pkgname = false;
    let mut is_summary = false;
    let mut has_pattern_match = false;

    // Process the entire mmap at once
    for mat in ac.find_iter(mmap) {
        match mat.pattern().as_usize() {
            0 => {
                // pkgname pattern - must be at start of line
                if mat.start() == current_line_start {
                    state.current_pkgname = &mmap[mat.end()..find_next_newline(mmap, mat.end())];
                    is_pkgname = true;
                }
            },
            1 => {
                // summary pattern - must be at start of line
                if mat.start() == current_line_start {
                    state.current_summary = &mmap[mat.end()..find_next_newline(mmap, mat.end())];
                    is_summary = true;
                }
            },
            2 => {
                // user pattern - check if it's not at start of line or not a pkgname/summary line
                let line_end = find_next_newline(mmap, mat.start());
                let current_line = &mmap[current_line_start..line_end];

                // Only consider it a match if it's not a pkgname or summary line
                if !current_line.starts_with(pkgname_pattern) && !current_line.starts_with(summary_pattern) {
                    has_pattern_match = true;
                }
            },
            3 => {
                // Newline - process the completed line
                if has_pattern_match && !is_pkgname && !is_summary {
                    state.print_match()?;
                }

                // Reset line state
                current_line_start = mat.end();
                is_pkgname = false;
                is_summary = false;
                has_pattern_match = false;
            },
            _ => unreachable!()
        }
    }

    Ok(())
}

// Helper function to find the next newline character
#[inline]
fn find_next_newline(data: &[u8], start: usize) -> usize {
    for i in start..data.len() {
        if data[i] == b'\n' {
            return i;
        }
    }
    data.len()
}

#[inline]
fn strip_prefix<'a>(haystack: &'a [u8], needle: &[u8]) -> Option<&'a [u8]> {
    if haystack.starts_with(needle) {
        Some(&haystack[needle.len()..])
    } else {
        None
    }
}
