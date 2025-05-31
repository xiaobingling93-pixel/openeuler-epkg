use std::fs;
use std::fs::File;
use std::ops::Range;
use std::path::PathBuf;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, BufWriter, Write};
use memmap2::Mmap;
use color_eyre::eyre::{Result, WrapErr};
use crate::models::*;
use crate::repo::RepoRevise;

#[derive(Debug)]
pub struct FileMapper {
    file: File,
    mmap: Mmap,
}

impl FileMapper {
    pub fn new(file_path: &str) -> std::io::Result<Self> {
        let file = File::open(file_path)?;
        // Memory map the file (unsafe because we must ensure the file isn't modified externally)
        let mmap = unsafe { Mmap::map(&file)? };
        Ok(Self { file, mmap })
    }

    /// Get the entire mapped data
    pub fn data(&self) -> &[u8] {
        &self.mmap
    }

    /// Get a specific range of the mapped data
    /// Panics if range is out of bounds
    pub fn range(&self, range: Range<usize>) -> &[u8] {
        &self.mmap[range]
    }

    /// Safe range access with bounds checking
    pub fn checked_range(&self, range: Range<usize>) -> Option<&[u8]> {
        if range.end <= self.mmap.len() {
            Some(&self.mmap[range])
        } else {
            None
        }
    }
}

// // Example usage
// fn main() -> std::io::Result<()> {
//     let mapper = FileMapper::new("example.txt")?;
//
//     // Access first 100 bytes
//     if let Some(data) = mapper.checked_range(0..100) {
//         println!("First 100 bytes: {:?}", data);
//     }
//
//     // Process the entire file in chunks
//     let chunk_size = 4096;
//     for chunk in mapper.data().chunks(chunk_size) {
//         // Process each chunk
//         println!("Chunk length: {}", chunk.len());
//     }
//
//     Ok(())
// }

/// Deserializes essential package names from a file
pub fn deserialize_repoindex(file_path: &PathBuf) -> Result<RepoIndex> {
    let contents = fs::read_to_string(&file_path)
        .with_context(|| format!("Failed to read file: {}", file_path.display()))?;

    let repoindex: RepoIndex = serde_json::from_str(&contents)
        .with_context(|| format!("Failed to parse JSON from file: {}", file_path.display()))?;

    Ok(repoindex)
}

pub fn populate_repoindex_data(repo: &RepoRevise, mut repo_index: RepoIndex) -> Result<()> {
    let repo_dir = crate::dirs::get_repo_dir(&repo)?;
    for (_, shard) in &mut repo_index.repo_shards {
        let filename = shard.packages.filename.clone();
        let packages_path = repo_dir.join(&filename);
        let provide2pkgnames_path = repo_dir.join(filename.replace("packages", "provide2pkgnames"));
        let essential_pkgnames_path = repo_dir.join(filename.replace("packages", "essential_pkgnames"));
        let pkgname2ranges_path = repo_dir.join(filename.replace("packages", "pkgname2ranges"));
        shard.packages_mmap = Some(FileMapper::new(packages_path.to_str().unwrap())?);
        shard.provide2pkgnames = deserialize_provide2pkgnames(&provide2pkgnames_path)?;
        shard.essential_pkgnames = deserialize_essential_pkgnames(&essential_pkgnames_path)?;
        shard.pkgname2ranges = deserialize_pkgname2ranges(&pkgname2ranges_path)?;
    }
    {
        let mut repodata_indice = repodata_indice();
        repodata_indice.insert(repo.repodata_name.clone(), repo_index);
    }
    Ok(())
}

/// Serializes essential package names to a file
pub fn serialize_essential_pkgnames(path: &PathBuf, pkgnames: &HashSet<String>) -> Result<()> {
    let file = File::create(path)?;
    let mut writer = BufWriter::new(file);

    let mut sorted_names: Vec<_> = pkgnames.iter().collect();
    sorted_names.sort();

    for item in sorted_names {
        writeln!(writer, "{}", item)?;
    }

    Ok(())
}

/// Deserializes essential package names from a file
pub fn deserialize_essential_pkgnames(file_path: &PathBuf) -> Result<HashSet<String>> {
    let file = File::open(file_path)?;
    let reader = BufReader::new(file);
    let mut hashset: HashSet<String> = HashSet::new();

    for line in reader.lines() {
        let line = line?;
        hashset.insert(line);
    }

    Ok(hashset)
}

/// Serializes package provides mapping to a file
pub fn serialize_provide2pkgnames(path: &PathBuf, provide2pkgnames: &HashMap<String, Vec<String>>) -> Result<()> {
    let file = File::create(path)?;
    let mut writer = BufWriter::new(file);

    let mut sorted_names: Vec<_> = provide2pkgnames.iter().collect();
    sorted_names.sort_by(|a, b| a.0.cmp(&b.0));

    for (key, values) in sorted_names {
        let line = format!("{}: {}", key, values.join(" "));
        writeln!(writer, "{}", line)?;
    }

    Ok(())
}

/// Deserializes package provides mapping from a file
pub fn deserialize_provide2pkgnames(file_path: &PathBuf) -> Result<HashMap<String, Vec<String>>> {
    let file = File::open(file_path)?;
    let reader = BufReader::new(file);
    let mut map: HashMap<String, Vec<String>> = HashMap::new();

    for (line_num, line_result) in reader.lines().enumerate() {
        let line = line_result.context(format!("Failed to read line {} from {}", line_num + 1, file_path.display()))?;
        if let Some((key, values)) = line.split_once(": ") {
            let values: Vec<String> = values.split(" ").map(|s| s.to_string()).collect();
            map.insert(key.to_string(), values);
        }
    }

    Ok(map)
}

// Function to serialize pkgname2ranges to a file
pub fn serialize_pkgname2ranges(path: &PathBuf, pkgname2ranges: &HashMap<String, Vec<PackageRange>>) -> Result<()> {
    let mut file = fs::File::create(path)
        .with_context(|| format!("Failed to create index file: {}", path.display()))?;

    // Sort package names before writing
    let mut sorted_packages: Vec<_> = pkgname2ranges.iter().collect();
    sorted_packages.sort_by(|a, b| a.0.cmp(b.0));

    for (pkgname, offsets) in sorted_packages {
        let offset_str = offsets.iter()
            .map(|o| format!("{:x} {:x}", o.begin, o.len))
            .collect::<Vec<_>>()
            .join(" ");
        writeln!(file, "{}: {}", pkgname, offset_str)
            .with_context(|| format!("Failed to write to index file: {}", path.display()))?;
    }
    Ok(())
}

// Function to deserialize pkgname2ranges from a file
pub fn deserialize_pkgname2ranges(path: &PathBuf) -> Result<HashMap<String, Vec<PackageRange>>> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read index file: {}", path.display()))?;

    let mut pkgname2ranges = HashMap::new();
    for line in content.lines() {
        if let Some((pkgname, offsets_str)) = line.split_once(": ") {
            let offsets: Vec<PackageRange> = offsets_str
                .split_whitespace()
                .collect::<Vec<_>>()
                .chunks(2)
                .filter_map(|chunk| {
                    if chunk.len() == 2 {
                        let begin = usize::from_str_radix(chunk[0], 16).ok()?;
                        let len = usize::from_str_radix(chunk[1], 16).ok()?;
                        Some(PackageRange {
                            begin,
                            len,
                        })
                    } else {
                        None
                    }
                })
                .collect();
            if !offsets.is_empty() {
                pkgname2ranges.insert(pkgname.to_string(), offsets);
            }
        }
    }
    Ok(pkgname2ranges)
}

