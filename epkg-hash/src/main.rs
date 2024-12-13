use std::fs;
use std::path::Path;
use sha2::{Sha256, Digest};
use base32;
use base32::Alphabet;
use walkdir::WalkDir;


fn main() {
    let epkg_path = std::env::args().nth(1).expect("Please provide a path as an argument");
    let base32_result = cal_path_hash(&epkg_path);
    println!("{}", base32_result.to_lowercase());
}


pub fn cal_path_hash(epkg_path: &String) -> String {
    let dir = Path::new(&epkg_path);
    let mut hasher = Sha256::new();

    let mut files = Vec::new();
    for entry in WalkDir::new(dir) {
        let entry = entry.unwrap();
        let path = entry.path();
        let metadata = fs::metadata(&path).unwrap();

        if metadata.is_file() || metadata.is_dir() {
            files.push(path.to_path_buf());
            
        }
    }
    files.sort();
    // for file in &files {
    //     println!("{:?}", file);
    // }

    for file in files {
        let file_content = if file.is_dir() {
            file.to_str().unwrap().as_bytes().to_vec()
        } else {
            fs::read(&file).unwrap()
        };
        hasher.update(&file_content);
    }

    let hash_result = hasher.finalize();
    let compressed_hash = xor_compress_to_20_bytes(&hash_result);
    let base32_result = base32::encode(Alphabet::Crockford, &compressed_hash);
    base32_result
}

fn xor_compress_to_20_bytes(hash: &[u8]) -> Vec<u8> {
    assert_eq!(hash.len(), 32, "Hash must be 32 bytes for SHA-256");
    let mut compressed = vec![0u8; 20];
    for i in 0..20 {
        compressed[i] = hash[i] ^ hash[i + 12]; // 前 20 字节和后 12 字节依次 XOR
    }
    compressed
}