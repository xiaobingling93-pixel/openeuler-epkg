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

    // 收集所有文件和目录的相对路径
    let mut relative_entries: Vec<PathBuf> = WalkDir::new(dir)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path().strip_prefix(dir).unwrap_or(entry.path()).to_path_buf())
        .collect();

    // 按照字典顺序排序
    relative_entries.sort();

    for entry in &relative_entries {
        let absolute_path = dir.join(entry);
        // println!("Processing entry: {}", absolute_path.display());  // 打印当前条目路径
        let (entry_path, entry_content) = get_entry_content(&absolute_path, &mut hasher, &mut hashed_files, dir);
        hasher.update(&entry_path);
        hasher.update(&entry_content);
    }

    let hash_result = hasher.finalize();
    let compressed_hash = xor_compress_to_20_bytes(&hash_result);
    let base32_result = base32::encode(Alphabet::Crockford, &compressed_hash);
    base32_result.to_lowercase()
}

fn get_entry_content(entry: &Path, hasher: &mut Sha256, hashed_files: &mut HashSet<PathBuf>, base_dir: &Path,) -> (Vec<u8>, Vec<u8>) {
    match fs::symlink_metadata(entry) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                let target_path = fs::read_link(entry).unwrap_or_else(|_| entry.to_path_buf());
                let relative_target = target_path.strip_prefix(base_dir).unwrap_or(&target_path);

                // 获取链接的内容
                let target_content = path_to_bytes(relative_target)

                // 将符号链接的相对路径和内容都加入到哈希计算中
                (path_to_bytes(entry.strip_prefix(base_dir).unwrap_or(entry)), target_content)
            } else if metadata.is_file() {
                let content = fs::read(entry).unwrap_or_else(|_| Vec::new());
                (path_to_bytes(entry.strip_prefix(base_dir).unwrap_or(entry)), content)
            } else if metadata.is_dir() {
                // 对于目录，仅返回路径，不包括内容
                (path_to_bytes(entry.strip_prefix(base_dir).unwrap_or(entry)), Vec::new())
            } else {
                // 如果是其他类型的条目（例如socket, device等），我们只返回路径
                (path_to_bytes(entry.strip_prefix(base_dir).unwrap_or(entry)), Vec::new())
            }
        }
        Err(_) => (path_to_bytes(entry.strip_prefix(base_dir).unwrap_or(entry)), Vec::new()),
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
