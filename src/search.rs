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

    let mut pattern = options.pattern.clone();
    let pattern = if options.regexp {
        if options.files && pattern.starts_with('/') {
            pattern = format!("^{}", &pattern[1..]);
        }
        Regex::new(&pattern).map_err(|e| eyre!("Invalid regex pattern: {}", e))?
    } else {
        Regex::new(&regex::escape(&pattern)).map_err(|e| eyre!("Invalid pattern: {}", e))?
    };

    // Create channels for producer/consumer pattern
    let (tx, rx) = bounded(1000);
    let pattern = Arc::new(pattern);
    let options = Arc::new(options);

    // Clone the path to avoid lifetime issues with the thread
    let filelists_path_owned = filelists_path.to_path_buf();
    let is_rpm_format = filelists_path.to_str().unwrap_or("").contains(".xml");

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
            tx.send(line_content).map_err(|e| eyre!("Channel send error: {}", e))?;
        }
        Ok(())
    });

    if is_rpm_format {
        process_rpm_filelists(rx, &pattern, &options)?;
    } else {
        process_simple_filelists(rx, &pattern, &options)?;
    }

    // Wait for producer thread to complete
    producer_handle.join().unwrap()?;

    Ok(())
}

fn process_rpm_filelists(rx: Receiver<String>, pattern: &Regex, options: &SearchOptions) -> Result<()> {
    let mut current_pkgname = String::new();
    let mut in_package = false;

    while let Ok(line) = rx.recv() {
        if line.starts_with("<package") {
            if let Some(name_start) = line.find("name=\"") {
                if let Some(name_end) = line[name_start + 6..].find("\"") {
                    current_pkgname = line[name_start + 6..name_start + 6 + name_end].to_string();
                    in_package = true;
                }
            }
        } else if line.starts_with("  <file>") {
            if !in_package {
                continue;
            }

            let file_path = line.trim_start_matches("  <file>").trim_end_matches("</file>");

            let matches = if options.files {
                let filename = Path::new(file_path).file_name().unwrap_or_default().to_str().unwrap_or_default();
                pattern.is_match(filename)
            } else {
                pattern.is_match(file_path)
            };

            if matches {
                println!("{} {}", current_pkgname, file_path);
            }
        } else if line.starts_with("</package>") {
            in_package = false;
        }
    }

    Ok(())
}

fn process_simple_filelists(rx: Receiver<String>, pattern: &Regex, options: &SearchOptions) -> Result<()> {
    while let Ok(line) = rx.recv() {
        if let Some((pkgname, path)) = line.split_once(' ') {
            let matches = if options.files {
                let filename = Path::new(path).file_name().unwrap_or_default().to_str().unwrap_or_default();
                pattern.is_match(filename)
            } else {
                pattern.is_match(path)
            };

            if matches {
                println!("{} {}", pkgname, path);
            }
        }
    }

    Ok(())
}

pub fn search_packages_fast(packages_path: &Path, options: &SearchOptions) -> Result<()> {
    // Memory map the file
    let file = File::open(packages_path)?;
    let mmap = unsafe { Mmap::map(&file)? };

    // Choose the fastest matcher
    let searcher: Box<dyn Fn(&[u8]) -> bool> = if options.regexp {
        let regex = BytesRegex::new(&options.pattern)?;
        Box::new(move |line| regex.is_match(line))
    } else {
        // Simple substring search without aho_corasick
        let pattern_bytes = options.pattern.as_bytes();
        Box::new(move |line| line.windows(pattern_bytes.len()).any(|window| window == pattern_bytes))
    };

    let mut current_pkgname = &b""[..];
    let mut current_summary = &b""[..];
    let mut stdout = BufWriter::new(std::io::stdout());

    // Iterate lines manually (faster than .split())
    let mut start = 0;
    for (i, &byte) in mmap.iter().enumerate() {
        if byte != b'\n' { continue; }

        let line = &mmap[start..i];
        start = i + 1;

        if line.is_empty() { continue; }

        if let Some(rest) = strip_prefix(line, b"pkgname: ") {
            current_pkgname = rest;
        } else if let Some(rest) = strip_prefix(line, b"summary: ") {
            current_summary = rest;
        } else if searcher(line) {
            writeln!(
                stdout,
                "{} - {}",
                String::from_utf8_lossy(current_pkgname),
                String::from_utf8_lossy(current_summary)
            )?;
        }
    }

    Ok(())
}

#[inline]
fn strip_prefix<'a>(haystack: &'a [u8], needle: &[u8]) -> Option<&'a [u8]> {
    if haystack.starts_with(needle) {
        Some(&haystack[needle.len()..])
    } else {
        None
    }
}
