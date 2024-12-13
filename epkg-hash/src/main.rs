use std::fs;
use std::path::{Path, PathBuf};
use sha2::{Sha256, Digest};
use base32;
use base32::Alphabet;
use walkdir::WalkDir;
use std::collections::HashSet;

fn main() {
    let epkg_path = std::env::args().nth(1).expect("Please provide a path as an argument");
    let base32_result = cal_path_hash(&epkg_path);
    println!("{}", base32_result.to_lowercase());
}

pub fn cal_path_hash(epkg_path: &String) -> String {
    let dir = Path::new(&epkg_path);
    let mut hasher = Sha256::new();
    let mut hashed_files = HashSet::new();

    let mut files = Vec::new();
    for entry in WalkDir::new(dir) {
        let entry = entry.unwrap();
        let path = entry.path();

        if entry.file_type().is_file() || entry.file_type().is_dir() || entry.file_type().is_symlink() {
            files.push(path.to_path_buf());
        }
    }
    files.sort();

    for file in &files {
        // println!("Processing file: {}", file.display());  // 打印当前文件路径
        let file_content = get_file_content(file, &mut hasher, &mut hashed_files);
        hasher.update(&file_content);
    }

    let hash_result = hasher.finalize();
    let compressed_hash = xor_compress_to_20_bytes(&hash_result);
    let base32_result = base32::encode(Alphabet::Crockford, &compressed_hash);
    base32_result
}

fn get_file_content(file: &Path, hasher: &mut Sha256, hashed_files: &mut HashSet<PathBuf>) -> Vec<u8> {
    match fs::symlink_metadata(file) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                let target_path = fs::read_link(file).unwrap_or_else(|_| file.to_path_buf());
                hasher.update(&path_to_bytes(file));
                if !hashed_files.contains(&target_path) {
                    hashed_files.insert(target_path.clone());
                    if let Ok(content) = fs::read(&target_path) {
                        return content;
                    }
                }
                path_to_bytes(file)
            } else if metadata.is_file() {
                let content = fs::read(file).unwrap_or_else(|_| Vec::new());
                if content.is_empty() {
                    path_to_bytes(file)
                } else {
                    content
                }
            } else if metadata.is_dir() {
                path_to_bytes(file)
            } else {
                path_to_bytes(file)
            }
        }
        Err(_) => {
            path_to_bytes(file)
        }
    }
}

fn path_to_bytes(path: &Path) -> Vec<u8> {
    path.as_os_str().to_string_lossy().as_bytes().to_vec()
}

fn xor_compress_to_20_bytes(hash: &[u8]) -> Vec<u8> {
    assert_eq!(hash.len(), 32, "Hash must be 32 bytes for SHA-256");
    let mut compressed = vec![0u8; 20];
    for i in 0..20 {
        compressed[i] = hash[i] ^ hash[i + 12]; // 前 20 字节和后 12 字节依次 XOR
    }
    compressed
}