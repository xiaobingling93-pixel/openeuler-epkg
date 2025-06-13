use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use regex::Regex;
use flate2::read::GzDecoder;
use liblzma::read::XzDecoder;
use zstd::stream::read::Decoder as ZstdDecoder;
use crossbeam_channel::{bounded, Receiver};
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
                    search_filelists(&repo_dir.join(&filelists.filename), options)
                        .with_context(|| format!("Failed to search filelists in {}", repo_index.repodata_name))?;
                    any_filelists = true;
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

fn search_filelists(filelists_path: &Path, options: &SearchOptions) -> Result<()> {
    if !filelists_path.exists() {
        warn!("Filelists not found at {}", filelists_path.display());
        return Ok(());
    }

    // Determine if we're dealing with RPM XML format or simple format
    let is_rpm_format = filelists_path.to_str().unwrap_or("").contains(".xml");

    // Prepare the pattern based on options
    let mut pattern_str = options.pattern.clone();
    if options.regexp && options.files && pattern_str.starts_with('/') {
        pattern_str = format!("^{}", &pattern_str[1..]);
    }

    // Create channels for producer/consumer pattern
    let (tx, rx) = bounded(1000);

    // Start the producer thread to read the file
    let producer_handle = start_filelists_producer(filelists_path, tx)?;

    // Process the file contents based on pattern type and file format
    if options.regexp {
        process_filelists_with_regex(rx, &pattern_str, is_rpm_format, options)?;
    } else {
        process_filelists_with_aho_corasick(rx, pattern_str, is_rpm_format, options)?;
    }

    // Wait for producer thread to complete
    producer_handle.join().unwrap()?;

    Ok(())
}

// Start a producer thread to read and send file contents line by line
fn start_filelists_producer(filelists_path: &Path, tx: crossbeam_channel::Sender<String>) -> Result<thread::JoinHandle<Result<()>>> {
    // Clone the path to avoid lifetime issues with the thread
    let filelists_path_owned = filelists_path.to_path_buf();

    // Spawn producer thread to read and decode the file
    let producer_handle = thread::spawn(move || -> Result<()> {
        let file = File::open(&filelists_path_owned)?;
        let reader = match filelists_path_owned.extension().and_then(|ext| ext.to_str()) {
            Some("gz") => Box::new(BufReader::new(GzDecoder::new(file))) as Box<dyn BufRead>,
            Some("xz") => Box::new(BufReader::new(XzDecoder::new(file))) as Box<dyn BufRead>,
            Some("zst") => Box::new(BufReader::new(ZstdDecoder::new(file)?)) as Box<dyn BufRead>,
            _ => Box::new(BufReader::new(file)) as Box<dyn BufRead>,
        };

        for line in reader.lines() {
            let line_content = line?;
            tx.send(line_content).map_err(|e| eyre!("Channel send error: {}", e))?
        }
        Ok(())
    });

    Ok(producer_handle)
}

// Process filelists using regex pattern matching
fn process_filelists_with_regex(
    rx: Receiver<String>,
    pattern_str: &str,
    is_rpm_format: bool,
    options: &SearchOptions
) -> Result<()> {
    // Extract literal prefix if possible for optimization
    let literal_prefix = extract_literal_prefix(pattern_str);
    let regex = Arc::new(Regex::new(pattern_str).map_err(|e| eyre!("Invalid regex pattern: {}", e))?);
    let options = Arc::new(options);

    if is_rpm_format {
        if let Some(prefix) = literal_prefix {
            process_rpm_filelists_with_prefix(rx, regex, prefix, &options)?
        } else {
            process_rpm_filelists_with_regex(rx, regex, &options)?
        }
    } else {
        if let Some(prefix) = literal_prefix {
            process_simple_filelists_with_prefix(rx, regex, prefix, &options)?
        } else {
            process_simple_filelists_with_regex(rx, regex, &options)?
        }
    }

    Ok(())
}

// Process filelists using Aho-Corasick pattern matching
fn process_filelists_with_aho_corasick(
    rx: Receiver<String>,
    pattern_str: String,
    is_rpm_format: bool,
    options: &SearchOptions
) -> Result<()> {
    let pattern_bytes = pattern_str.as_bytes().to_vec();
    let options = Arc::new(options);

    if is_rpm_format {
        process_rpm_filelists_with_aho_corasick(rx, pattern_bytes, &options)?
    } else {
        process_simple_filelists_with_aho_corasick(rx, pattern_bytes, &options)?
    }

    Ok(())
}

fn process_rpm_filelists_with_prefix(rx: Receiver<String>, regex: Arc<Regex>, prefix: Vec<u8>, _options: &SearchOptions) -> Result<()> {
    let mut current_pkgname = String::new();

    // Create patterns for Aho-Corasick
    let patterns = vec![b"<package".to_vec(), b"  <file>".to_vec(), prefix];
    let ac = match AhoCorasick::new(&patterns) {
        Ok(ac) => ac,
        Err(e) => return Err(eyre!("Failed to create Aho-Corasick automaton: {}", e)),
    };

    while let Ok(line) = rx.recv() {
        let mut matches = ac.find_iter(line.as_bytes());

        while let Some(mat) = matches.next() {
            match mat.pattern().as_usize() {
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

fn process_rpm_filelists_with_regex(rx: Receiver<String>, regex: Arc<Regex>, options: &SearchOptions) -> Result<()> {
    let mut current_pkgname = String::new();

    while let Ok(line) = rx.recv() {
        if line.starts_with("<package") {
            if let Some(name_start) = line.find("name=\"") {
                if let Some(name_end) = line[name_start + 6..].find("\"") {
                    current_pkgname = line[name_start + 6..name_start + 6 + name_end].to_string();
                }
            }
        } else if line.starts_with("  <file>") {
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
fn process_rpm_filelists_with_aho_corasick(rx: Receiver<String>, pattern: Vec<u8>, options: &SearchOptions) -> Result<()> {
    let mut current_pkgname = String::new();

    // Create patterns for Aho-Corasick
    let patterns = vec![
        b"<package".to_vec(),
        pattern.clone()
    ];

    // Create the Aho-Corasick automaton
    let ac = match AhoCorasick::new(&patterns) {
        Ok(ac) => ac,
        Err(e) => return Err(eyre!("Failed to create Aho-Corasick automaton: {}", e)),
    };

    while let Ok(line) = rx.recv() {
        let line_bytes = line.as_bytes();
        let mut has_pattern_match = false;

        for mat in ac.find_iter(line_bytes) {
            match mat.pattern().as_usize() {
                0 => { // <package pattern
                    if let Some(name_start) = line.find("name=\"") {
                        if let Some(name_end) = line[name_start + 6..].find("\"") {
                            current_pkgname = line[name_start + 6..name_start + 6 + name_end].to_string();
                        }
                    }
                },
                1 => { // User pattern
                    has_pattern_match = true;
                },
                _ => unreachable!()
            }
        }

        // Process file line with pattern match
        if has_pattern_match {
            let file_path = line.trim_start_matches("  <file>").trim_end_matches("</file>");

            // For --files option, check if the pattern matches the filename only
            if options.files {
                let filename = Path::new(file_path).file_name().unwrap_or_default().to_str().unwrap_or_default();
                if filename.as_bytes().windows(pattern.len()).any(|window| window == pattern.as_slice()) {
                    println!("{} {}", current_pkgname, file_path);
                }
            } else {
                println!("{} {}", current_pkgname, file_path);
            }
        }
    }

    Ok(())
}

// Process simple filelists with regex and literal prefix optimization
fn process_simple_filelists_with_prefix(rx: Receiver<String>, regex: Arc<Regex>, prefix: Vec<u8>, options: &SearchOptions) -> Result<()> {
    // Create the Aho-Corasick automaton with the prefix
    let patterns = vec![prefix.clone()];
    let ac = match AhoCorasick::new(&patterns) {
        Ok(ac) => ac,
        Err(e) => return Err(eyre!("Failed to create Aho-Corasick automaton: {}", e)),
    };

    while let Ok(line) = rx.recv() {
        // Fast pre-filtering with Aho-Corasick
        if ac.is_match(line.as_bytes()) {
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

// Process simple filelists with full regex matching
fn process_simple_filelists_with_regex(rx: Receiver<String>, regex: Arc<Regex>, options: &SearchOptions) -> Result<()> {
    while let Ok(line) = rx.recv() {
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

    Ok(())
}

// Process simple filelists using Aho-Corasick for non-regex pattern matching
fn process_simple_filelists_with_aho_corasick(rx: Receiver<String>, pattern: Vec<u8>, options: &SearchOptions) -> Result<()> {
    while let Ok(line) = rx.recv() {
        if let Some((pkgname, path)) = line.split_once(' ') {
            let target = if options.files {
                let filename = Path::new(path).file_name().unwrap_or_default().to_str().unwrap_or_default();
                filename
            } else {
                path
            };

            // Simple substring search
            if target.as_bytes().windows(pattern.len()).any(|window| window == pattern.as_slice()) {
                println!("{} {}", pkgname, path);
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
