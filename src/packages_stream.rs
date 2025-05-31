use std::path::PathBuf;
use std::collections::HashMap;
use std::collections::HashSet;
use std::time::SystemTime;
use std::sync::mpsc::Receiver;
use std::fs;
use std::fs::{OpenOptions, File};
use std::io::BufWriter;
use std::io::Write;
use color_eyre::eyre;
use color_eyre::eyre::Result;
use color_eyre::eyre::WrapErr;
use sha2::{Sha256, Digest};
use hex;
use crate::models::*;
use crate::repo::*;
use crate::mmio;

pub struct ReceiverHasher {
    receiver: Receiver<Vec<u8>>,
    current_chunk: Vec<u8>,
    position: usize,
    pub hasher: Sha256,
    pub sha256sum: String,
}

impl ReceiverHasher {
    pub fn new(receiver: Receiver<Vec<u8>>) -> Self {
        Self {
            receiver,
            current_chunk: Vec::new(),
            position: 0,
            hasher: Sha256::new(),
            sha256sum: String::new(),
        }
    }
}

impl std::io::Read for ReceiverHasher {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.position >= self.current_chunk.len() {
            match self.receiver.recv() {
                Ok(chunk) => {
                    self.hasher.update(&chunk);
                    self.current_chunk = chunk;
                    self.position = 0;
                }
                Err(_) => {
                    self.sha256sum = hex::encode(self.hasher.finalize_reset());
                    return Ok(0); // End of stream
                }
            }
        }

        let remaining = self.current_chunk.len() - self.position;
        let to_copy = std::cmp::min(remaining, buf.len());
        buf[..to_copy].copy_from_slice(&self.current_chunk[self.position..self.position + to_copy]);
        self.position += to_copy;
        Ok(to_copy)
    }
}


pub struct PackagesStreamline {
    pub provide2pkgnames: HashMap<String, Vec<String>>,
    pub essential_pkgnames: HashSet<String>,
    pub pkgname2ranges: HashMap<String, Vec<PackageRange>>,
    pub output_path: PathBuf,
    pub json_path: PathBuf,
    pub provide2pkgnames_path: PathBuf,
    pub essential_pkgnames_path: PathBuf,
    pub pkgname2ranges_path: PathBuf,
    pub new_hasher: Sha256,
    pub current_pkgname: String,
    pub output: String,
    pub output_offset: usize,
    pub package_begin_offset: usize,
    pub writer: BufWriter<File>,
    pub partial_line: String,
    pub process_line: fn(line: &str, derived_files: &mut PackagesStreamline) -> Result<()>,
}

impl PackagesStreamline {
    pub fn new(revise: &RepoReleaseItem,
                repo_dir: &PathBuf,
                process_line_fn: fn(&str, &mut PackagesStreamline) -> Result<()>) -> Result<Self> {
        let output_path = repo_dir.join(format!("packages-{}.txt", revise.arch));
        let json_path = repo_dir.join(format!(".packages-{}.json", revise.arch));
        let provide2pkgnames_path = repo_dir.join(format!("provide2pkgnames-{}.yaml", revise.arch));
        let essential_pkgnames_path = repo_dir.join(format!("essential_pkgnames-{}.txt", revise.arch));
        let pkgname2ranges_path = repo_dir.join(format!("packages-{}.idx", revise.arch));

        log::debug!("Output paths - txt: {:?}, json: {:?}, idx: {:?}", output_path, json_path, pkgname2ranges_path);

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&output_path)
            .context(format!("Failed to open output file: {:?}", output_path))?;
        let writer = BufWriter::new(file);

        Ok(Self {
            provide2pkgnames: HashMap::new(),
            essential_pkgnames: HashSet::new(),
            pkgname2ranges: HashMap::new(),
            output_path,
            json_path,
            provide2pkgnames_path,
            essential_pkgnames_path,
            pkgname2ranges_path,
            new_hasher: Sha256::new(),
            current_pkgname: String::new(),
            output: String::new(),
            output_offset: 0,
            package_begin_offset: 0,
            writer,
            partial_line: String::new(),
            process_line: process_line_fn,
        })
    }

    pub fn on_new_paragraph(&mut self) {
        if !self.current_pkgname.is_empty() {
            let current_offset = self.output_offset + self.output.len();
            self.pkgname2ranges.entry(self.current_pkgname.clone())
                .or_insert(Vec::new())
                .push(PackageRange {
                    begin: self.package_begin_offset,
                    len: current_offset - self.package_begin_offset,
                });
            self.package_begin_offset = current_offset;
            self.current_pkgname.clear();
        }
    }

    pub fn on_new_pkgname(&mut self, value: &str) {
        self.current_pkgname.clear();
        self.current_pkgname.push_str(value);
    }

    pub fn on_essential(&mut self) {
        self.essential_pkgnames.insert(self.current_pkgname.clone());
    }

    pub fn on_provides(&mut self, provides: Vec<&str>) {
        for provide in provides {
            self.provide2pkgnames
                .entry(provide.to_string())
                .or_insert(Vec::new())
                .push(self.current_pkgname.clone());
        }
    }

    pub fn on_output(&mut self) -> Result<()> {
        if !self.output.is_empty() {
            self.new_hasher.update(self.output.as_bytes());
            self.writer.write_all(self.output.as_bytes())
                .context(format!("Failed to append to output file: {:?}", self.output_path))?;
            self.output_offset += self.output.len();
            self.output.clear();
        }
        Ok(())
    }

    pub fn on_finish(&mut self, revise: &RepoReleaseItem, calculated_hash: String) -> Result<FileInfo> {
        // Get the final hash from origin_hasher
        let expected_hash = &revise.hash;
        if calculated_hash != *expected_hash {
            log::error!("Hash verification failed for {} - calculated: {}, expected: {}",
                revise.location, calculated_hash, expected_hash);
            return Err(eyre::eyre!("Hash verification failed for {}: calculated {}, expected {}",
                    revise.location, calculated_hash, expected_hash));
        }

        self.writer.flush()
            .context(format!("Failed to flush output file: {:?}", self.output_path))?;

        // Save package offsets to index file
        mmio::serialize_pkgname2ranges(&self.pkgname2ranges_path, &self.pkgname2ranges)?;
        mmio::serialize_provide2pkgnames(&self.provide2pkgnames_path, &self.provide2pkgnames)?;
        mmio::serialize_essential_pkgnames(&self.essential_pkgnames_path, &self.essential_pkgnames)?;

        let sha256sum = hex::encode(self.new_hasher.finalize_reset());
        save_file_metadata(&self.output_path, &self.json_path, sha256sum)
    }

    pub fn handle_chunk(&mut self, result: std::io::Result<usize>, unpack_buf: &[u8]) -> Result<bool> {
        match result {
            Ok(0) => {
                log::debug!("Reached EOF after processing {} bytes", self.output_offset);
                let line = self.partial_line.clone();
                let process_line = self.process_line;
                process_line(&line, self)?;
                // Ensure we properly close the last package
                if !self.current_pkgname.is_empty() {
                    self.on_new_paragraph();
                }
                self.on_output()?;
                Ok(false) // Signal to stop processing
            }
            Ok(n) => {
                let content = &unpack_buf[..n];
                let mut pos = 0;

                while pos < content.len() {
                    // Find the next newline
                    if let Some(newline_pos) = content[pos..].iter().position(|&b| b == b'\n') {
                        let newline_pos = pos + newline_pos;

                        // Get the line content up to the newline
                        let line_content = &content[pos..newline_pos];
                        let line = if self.partial_line.is_empty() {
                            String::from_utf8_lossy(line_content).to_string()
                        } else {
                            let full_line = self.partial_line.clone() + &String::from_utf8_lossy(line_content);
                            self.partial_line.clear();
                            full_line
                        };

                        // Process the complete line
                        let process_line = self.process_line;
                        process_line(&line, self)?;
                        pos = newline_pos + 1;
                    } else {
                        // No more newlines, save the rest as partial
                        self.partial_line.push_str(&String::from_utf8_lossy(&content[pos..]));
                        break;
                    }
                }

                // Write accumulated output to file
                self.on_output()?;
                Ok(true) // Continue processing
            }
            Err(e) => {
                log::error!("Decompression error: {}", e);
                Err(eyre::eyre!("Failed to decompress file: {}", e))
            }
        }
    }
}

fn save_file_metadata(output_path: &PathBuf, json_path: &PathBuf, sha256sum: String) -> Result<FileInfo> {
    let metadata = fs::metadata(output_path)
        .context(format!("Failed to get metadata for file: {:?}", output_path))?;
    let file_info = FileInfo {
        filename: output_path.file_name().unwrap().to_string_lossy().into_owned(),
        sha256sum: sha256sum,
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
