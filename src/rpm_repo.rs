use std::fs;
use std::path::PathBuf;
use crate::models::{FileInfo, RepoIndex, RepoShard};
use crate::repo::{RepoRevise, url_to_cache_path};
use std::time::{SystemTime, Duration};
use std::collections::HashMap;
use sha2::{Sha256, Digest};
use time::OffsetDateTime;
use time::macros::format_description;
use hex;
use crate::download::download_urls;
use crate::dirs;
use color_eyre::eyre;
use color_eyre::eyre::{Result, eyre};
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use std::io::BufReader;

const PACKAGE_KEY_MAPPING: &[(&str, &str)] = &[
    ("name", "pkgname"),
    ("version", "version"),
    ("release", "release"),
    ("arch", "arch"),
    ("summary", "summary"),
    ("description", "description"),
    ("location", "location"),
    ("size", "size"),
    ("checksum", "sha256"),
];

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct RpmReleaseItem {
    pub repo_name: String,
    pub need_revise: bool,
    pub url: String,
    pub hash_type: String,
    pub hash: String,
    pub size: u64,
    pub path: PathBuf,
    pub arch: String,
}

pub fn revise_repodata(repo: &RepoRevise) -> Result<()> {
    let repo_dir = dirs::get_repo_dir(&repo).unwrap();
    let repomd_path = url_to_cache_path(&repo.index_url)?;

    // Check if already updated in last 1 day
    if repomd_path.exists() {
        let metadata = fs::metadata(&repomd_path)?;
        let modified = metadata.modified()?;
        let now = SystemTime::now();
        if let Ok(duration) = now.duration_since(modified) {
            if duration < Duration::from_secs(24 * 60 * 60) {
                return Ok(());
            }
        }
    }

    // Download repomd.xml file
    let release_dir = repomd_path.parent().unwrap();
    download_urls(vec![repo.index_url.clone()], dirs().epkg_downloads_cache.to_str().unwrap(), 6, false)?;

    // Parse repomd.xml file
    let release_content = fs::read_to_string(&repomd_path)?;
    let info = parse_repomd_file(&repo.repo_name, &release_content, &release_dir.to_path_buf(), &repo.index_url)?;

    if info.is_empty() {
        return Ok(());
    }

    // Download and process files
    download_revises(&info)?;
    convert_revises(&info, &repo_dir)?;

    Ok(())
}

fn parse_repomd_file(repo_name: &str, content: &str, release_dir: &PathBuf, index_url: &str) -> Result<Vec<RpmReleaseItem>> {
    let mut info = Vec::new();
    let mut reader = Reader::from_str(content);

    let mut buf = Vec::new();
    let mut current_data_type = String::new();
    let mut current_location = String::new();
    let mut current_checksum = String::new();
    let mut current_size = 0u64;
    let mut current_arch = String::new();
    let mut in_data = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                match e.name().as_ref() {
                    b"data" => {
                        in_data = true;
                        if let Some(data_type) = e.attributes()
                            .find(|attr| attr.as_ref().unwrap().key.as_ref() == b"type")
                            .and_then(|attr| attr.ok())
                            .and_then(|attr| String::from_utf8(attr.value.into_owned()).ok()) {
                            current_data_type = data_type;
                        }
                    }
                    b"location" => {
                        if in_data {
                            if let Some(href) = e.attributes()
                                .find(|attr| attr.as_ref().unwrap().key.as_ref() == b"href")
                                .and_then(|attr| attr.ok())
                                .and_then(|attr| String::from_utf8(attr.value.into_owned()).ok()) {
                                current_location = href;
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(e)) => {
                if in_data {
                    let text = e.unescape().unwrap_or_default().to_string();
                    let parent = reader.buffer_position() as usize;
                    let parent_name = reader.get_ref()[parent..].split(|&b| b == b'<').nth(1)
                        .and_then(|s| s.split(|&b| b == b'>').next())
                        .unwrap_or(b"");

                    match parent_name {
                        b"checksum" => current_checksum = text,
                        b"size" => current_size = text.parse().unwrap_or(0),
                        b"arch" => current_arch = text,
                        _ => {}
                    }
                }
            }
            Ok(Event::End(ref e)) => {
                if e.name().as_ref() == b"data" {
                    if current_data_type == "primary" || current_data_type == "filelists" {
                        let need_revise = !release_dir.file_name()
                            .map(|name| if name == "repodata" { release_dir.parent() } else { Some(release_dir.as_path()) })
                            .flatten()
                            .unwrap_or(release_dir)
                            .join(&current_location)
                            .exists();

                        let baseurl = if index_url.ends_with("repomd.xml") {
                            index_url.trim_end_matches("repomd.xml")
                        } else if index_url.ends_with('/') {
                            index_url.trim_end_matches('/')
                        } else {
                            &index_url
                        };
                        let url = format!("{}{}", baseurl, current_location);
                        let local_path = url_to_cache_path(&url)?;

                        info.push(RpmReleaseItem {
                            repo_name: repo_name.to_string(),
                            need_revise,
                            url: url.clone(),
                            hash_type: "SHA256".to_string(),
                            hash: current_checksum.clone(),
                            size: current_size,
                            path: local_path,
                            arch: current_arch.clone(),
                        });
                    }
                    in_data = false;
                    current_data_type.clear();
                    current_location.clear();
                    current_checksum.clear();
                    current_size = 0;
                    current_arch.clear();
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(eyre!("Error at position {}: {:?}", reader.buffer_position(), e)),
            _ => {}
        }
        buf.clear();
    }

    Ok(info)
}

fn download_revises(info: &[RpmReleaseItem]) -> Result<()> {
    // Download all files
    let urls: Vec<String> = info.iter().map(|r| r.url.clone()).collect();
    download_urls(urls, dirs().epkg_downloads_cache.to_str().unwrap(), 1, false)?;

    // Unpack compressed files
    unpack_compressed_files(info)?;
    Ok(())
}

fn unpack_compressed_files(info: &[RpmReleaseItem]) -> Result<()> {
    for revise in info {
        let input_path = PathBuf::from(&revise.path);
        let output_path = input_path.with_extension("");
        let extension = input_path.extension()
            .and_then(|ext| ext.to_str())
            .ok_or_else(|| eyre::eyre!("Failed to get extension from path: {}", revise.path.display()))?;
        crate::utils::decompress_file(&input_path, &output_path, extension)?;
    }
    Ok(())
}

fn convert_revises(info: &[RpmReleaseItem], repo_dir: &PathBuf) -> Result<()> {
    let mut packages_info = None;
    let mut filelist_info = None;

    for revise in info {
        let unpacked_path = PathBuf::from(&revise.path).with_extension("");
        let path_str = revise.path.to_string_lossy();
        if path_str.contains("primary.xml") {
            packages_info = Some(convert_packages(&unpacked_path, repo_dir)?);
        } else if path_str.contains("filelists.xml") {
            filelist_info = Some(convert_filelist(&unpacked_path, repo_dir, &revise)?);
        }
    }

    save_repo_index_json(packages_info, filelist_info, repo_dir)?;
    Ok(())
}

fn convert_packages(packages_path: &PathBuf, repo_dir: &PathBuf) -> Result<FileInfo> {
    let file = fs::File::open(packages_path)?;
    let reader = BufReader::new(file);
    let mut xml_reader = Reader::from_reader(reader);

    let mut output = String::new();
    let mut buf = Vec::new();
    let mut package_info = HashMap::new();
    let mut in_package = false;
    let mut current_tag = String::new();

    loop {
        match xml_reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                match e.name().as_ref() {
                    b"package" => {
                        in_package = true;
                        package_info.clear();
                    }
                    _ => {
                        if in_package {
                            current_tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                        }
                    }
                }
            }
            Ok(Event::Text(e)) => {
                if in_package {
                    let text = e.unescape().unwrap_or_default().to_string();
                    package_info.insert(current_tag.clone(), text);
                }
            }
            Ok(Event::Empty(ref e)) => {
                if in_package {
                    for attr in e.attributes() {
                        if let Ok(attr) = attr {
                            let key = String::from_utf8_lossy(attr.key.as_ref()).to_string();
                            let value = String::from_utf8_lossy(&attr.value).to_string();
                            match key.as_str() {
                                "ver" => package_info.insert("version".to_string(), value),
                                "rel" => package_info.insert("release".to_string(), value),
                                "href" => package_info.insert("location".to_string(), value),
                                _ => None,
                            };
                        }
                    }
                }
            }
            Ok(Event::End(ref e)) => {
                if e.name().as_ref() == b"package" {
                    for (key, value) in &package_info {
                        if let Some(new_key) = PACKAGE_KEY_MAPPING.iter().find(|(k, _)| *k == key) {
                            output.push_str(&format!("{}: {}\n", new_key.1, value));
                        }
                    }
                    output.push_str("\n");
                    in_package = false;
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(eyre!("Error at position {}: {:?}", xml_reader.buffer_position(), e)),
            _ => {}
        }
        buf.clear();
    }

    // Use provided local time for YYMMDD
    let date_str = match OffsetDateTime::now_local() {
        Ok(dt) => dt.format(&format_description!("[year][month][day]")).unwrap_or_else(|_| "<time_fmt_err>".to_string()),
        Err(_) => "<local_time_err>".to_string(),
    };

    // Compute sha256sum of file
    let mut hasher = Sha256::new();
    hasher.update(&output);
    let hash = hasher.finalize();
    let short_hash = hex::encode(&hash)[..6].to_string();
    let output_path = repo_dir.join(format!("packages-{}-{}.txt", date_str, short_hash));
    fs::write(&output_path, output)?;

    let metadata = fs::metadata(&output_path)?;
    Ok(FileInfo {
        filename: "packages.txt".to_string(),
        sha256sum: hex::encode(hash),
        datetime: metadata.modified()?.duration_since(SystemTime::UNIX_EPOCH)?.as_secs().to_string(),
        size: metadata.len(),
    })
}

fn convert_filelist(contents_path: &PathBuf, repo_dir: &PathBuf, revise: &RpmReleaseItem) -> Result<FileInfo> {
    // Create symbolic link from contents_path to repo_dir
    let output_path = repo_dir.join(contents_path.file_name().unwrap());
    if output_path.exists() {
        fs::remove_file(&output_path)?;
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(contents_path, &output_path)?;
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(contents_path, &output_path)?;

    let metadata = fs::metadata(contents_path)?;
    Ok(FileInfo {
        filename: contents_path.file_name().unwrap().to_string_lossy().to_string(),
        sha256sum: revise.hash.clone(),
        datetime: metadata.modified()?.duration_since(SystemTime::UNIX_EPOCH)?.as_secs().to_string(),
        size: metadata.len(),
    })
}

fn save_repo_index_json(
    packages_info: Option<FileInfo>,
    filelist_info: Option<FileInfo>,
    repo_dir: &PathBuf
) -> Result<()> {
    let repo_index = RepoIndex {
        repo_shards: vec![RepoShard {
            essential_pkgnames: std::collections::HashSet::new(),
            provide2pkgnames: std::collections::HashMap::new(),
            packages: packages_info.expect("packages_info should not be None"),
            filelist: filelist_info,
        }],
    };

    let index_path = repo_dir.join("RepoIndex.json");
    fs::write(index_path, serde_json::to_string_pretty(&repo_index)?)?;

    Ok(())
}

