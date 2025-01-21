use std::fs;
use std::path::{Path, PathBuf};
use std::io::Read;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::fs::MetadataExt;
use base32;
use walkdir::WalkDir;
use sha1::Digest;
use sha2;
use anyhow::Result;


pub fn b32_hash(content: &str) -> String {
    // Compute the SHA1 hash of the input string
    let mut hasher = sha1::Sha1::new();
    hasher.update(content.as_bytes());
    let sha1_hash = hasher.finalize();

    // Encode the SHA1 hash in base32
    let b32sum = base32::encode(base32::Alphabet::Rfc4648 { padding: false }, &sha1_hash);

    // Convert the base32 hash to lowercase
    b32sum.to_lowercase()
}

pub fn epkg_store_hash(epkg_path: &str) -> Result<String> {
    let dir = Path::new(&epkg_path);

    let fs_path = dir.join("fs");
    let install_path = dir.join("info").join("install");

    // 收集所有文件和目录的路径
    let mut paths: Vec<PathBuf> = WalkDir::new(dir)
        .into_iter()
        .filter_map(|entry| entry.ok()) // Skip errors
        .map(|entry| entry.into_path())
        .filter(|entry| entry.starts_with(&fs_path) || entry.starts_with(&install_path))
        .collect();

    paths.sort();

    let mut info: Vec<String> = Vec::new();

    for path in &paths {
        if path == dir { continue; } // this is where rust WalkDir differs from python os.walk
        let (fsize, ftype, fdata) = get_path_info(&path)?;
        info.push(path.strip_prefix(dir)?.to_string_lossy().into_owned());
        info.push(ftype.to_string());
        info.push(fsize.to_string());
        info.push(fdata);
    }

    let mut hasher = sha2::Sha256::new();
    let all_info = info.join("\n");
    // println!("{}", all_info);

    hasher.update(all_info);
    let sha256_sum = format!("{:x}", hasher.finalize());
    Ok(b32_hash(&sha256_sum))
}

fn get_path_info(path: &Path) -> Result<(u64, &str, String)> {
    let metadata = fs::symlink_metadata(path)?;

    let (ftype, fdata) = match metadata.file_type() {
        ft if ft.is_symlink()       => ("S_IFLNK", fs::read_link(path)?.to_string_lossy().into_owned()),
        ft if ft.is_file()          => ("S_IFREG", file_sha256_chunks(path)?.join(" ")),
        ft if ft.is_block_device()  => ("S_IFBLK", metadata.dev().to_string()),  // u64
        ft if ft.is_char_device()   => ("S_IFCHR", metadata.dev().to_string()),  // high32-major  low32-minor
        ft if ft.is_dir()           => ("S_IFDIR", "".to_string()),
        ft if ft.is_fifo()          => ("S_IFIFO", "".to_string()),
        ft if ft.is_socket()        => ("S_IFSOCK", "".to_string()),
        _ => panic!("Encountered an unknown file type at: {}", path.display()),
    };

    Ok((metadata.len(), ftype, fdata))
}

/// Compute the SHA-256 hash for every 16 KB chunk of a file.
/// One-shot computation could consume too much memory for large files.
fn file_sha256_chunks(file_path: &Path) -> Result<Vec<String>> {
    const CHUNK_SIZE: usize = 16<<10; // 16 KB

    let mut file = fs::File::open(file_path)?;
    let mut buffer = vec![0; CHUNK_SIZE];
    let mut hashes = Vec::new();

    loop {
        let bytes_read = file.read(&mut buffer)?;
        if bytes_read == 0 {
            break; // End of file
        }

        // Compute the SHA-256 hash of the chunk
        let mut hasher = sha2::Sha256::new();
        hasher.update(&buffer[..bytes_read]);
        let hash = format!("{:x}", hasher.finalize());
        hashes.push(hash);
    }

    Ok(hashes)
}
