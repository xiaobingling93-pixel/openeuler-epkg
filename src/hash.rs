use std::fs;
use std::path::{Path, PathBuf};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::fs::MetadataExt;
use base32;
use base32::Alphabet;
use walkdir::WalkDir;
use sha2::{Sha256, Digest};

fn main() {
    let epkg_path = std::env::args().nth(1).expect("Please provide a path as an argument");
    let base32_result = cal_path_hash(&epkg_path);
    println!("{}", base32_result.to_lowercase());
}

pub fn cal_path_hash(epkg_path: &String) -> String {
    let dir = Path::new(&epkg_path);
    let mut hasher = Sha256::new();

    // 收集所有文件和目录的相对路径
    let mut relative_entries: Vec<PathBuf> = WalkDir::new(dir)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path().strip_prefix(dir).unwrap_or(entry.path()).to_path_buf())
        .collect();
    relative_entries.sort();

    for entry in &relative_entries {
        // hasher add path
        hasher.update(path_to_bytes(entry));
        // hasher add file_type & other param
        let absolute_path = dir.join(entry);
        let (entry_content, entry_type) = get_entry_hash_param(&absolute_path);
        hasher.update(entry_type.as_bytes());
        hasher.update(&entry_content);
    }

    let hash_result = hasher.finalize();
    let compressed_hash = xor_compress_to_20_bytes(&hash_result);
    let base32_result = base32::encode(Alphabet::Crockford, &compressed_hash);
    base32_result.to_lowercase()
}

fn get_entry_hash_param(entry: &Path) -> (Vec<u8>, String) {
    match fs::symlink_metadata(entry) {
        Ok(metadata) => match metadata.file_type() {
            ft if ft.is_symlink() => (path_to_bytes(&fs::read_link(entry).unwrap()), "S_IFLNK".to_string()),
            ft if ft.is_file() => (fs::read(entry).unwrap(), "S_IFREG".to_string()),
            ft if ft.is_block_device() => (metadata.dev().to_ne_bytes().into(), "S_IFBLK".to_string()), // metadata.dev() -> u64
            ft if ft.is_char_device() => (metadata.dev().to_ne_bytes().into(), "S_IFCHR".to_string()),  // high32-major  low32-minor
            ft if ft.is_dir() => (Vec::new(), "S_IFDIR".to_string()),
            ft if ft.is_socket() => (Vec::new(), "S_IFSOCK".to_string()),
            ft if ft.is_fifo() => (Vec::new(), "S_IFIFO".to_string()),
            _ => panic!("Encountered an unknown file type at: {}", entry.display()),
        },
        Err(e) => panic!("Failed to get metadata for {}: {}", entry.display(), e),
    }
}

fn path_to_bytes(path: &Path) -> Vec<u8> {
    path.to_string_lossy().as_bytes().to_vec()
}

fn xor_compress_to_20_bytes(hash: &[u8]) -> Vec<u8> {
    assert_eq!(hash.len(), 32, "Hash must be 32 bytes for SHA-256");
    let mut compressed = vec![0u8; 20];
    for i in 0..20 {
        compressed[i] = hash[i] ^ hash[i + 12]; // 前 20 字节和后 12 字节依次 XOR
    }
    compressed
}
