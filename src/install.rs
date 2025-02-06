use std::collections::HashMap;
use clap::parser::ValuesRef;
use anyhow::Result;
use crate::models::*;
use std::path::Path;
use std::process::Command;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::io::Error;
use std::io::SeekFrom;
use std::io::Read;
use std::io::Seek;
use std::io::Write;
use anyhow::anyhow;
use anyhow::Context;
use pathdiff::diff_paths;

fn print_packages_by_depend_depth(packages: &HashMap<String, InstalledPackageInfo>) {
    // Convert HashMap to a Vec of tuples (pkgline, info)
    let mut packages_vec: Vec<(&String, &InstalledPackageInfo)> = packages.iter().collect();

    // Sort by depend_depth
    packages_vec.sort_by(|a, b| a.1.depend_depth.cmp(&b.1.depend_depth));

    // Print the header
    println!("{:<12} {:<10}", "depend_depth", "package");

    // Print each package
    for (pkgline, info) in packages_vec {
        println!("{:<12} {:<10}", info.depend_depth, pkgline);
    }
}

/// Finds duplicates between `a` and `b`,
/// shows a warning about the duplicates, and removes them from `b`.
pub fn remove_duplicates(
    a: &HashMap<String, InstalledPackageInfo>,
    b: &mut HashMap<String, InstalledPackageInfo>,
    warn: &str) {

    let duplicates: Vec<_> = b
        .keys()
        .filter(|&package_name| a.contains_key(package_name))
        .cloned()
        .collect();

    if !duplicates.is_empty() {
        if !warn.is_empty() {
            eprintln!("{}", warn);
            for package_name in &duplicates {
                eprintln!("- {}", package_name);
            }
        }

        // Remove duplicates from `b`
        for package_name in duplicates {
            b.remove(&package_name);
        }
    }
}

// List package/fs files
pub fn list_package_files(package_fs_dir: &str) -> Result<Vec<PathBuf>> {
    let dir = Path::new(package_fs_dir);
    let mut paths = Vec::new();

    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;

        if file_type.is_file() || file_type.is_symlink() {
            paths.push(path.clone());
        } else if file_type.is_dir() {
            paths.extend(list_package_files(path.to_str().unwrap())?);
        }
    }

    Ok(paths)
}

// Get file type
pub fn get_file_type(file: &Path) -> Result<String> {
    let output = Command::new("file").arg(file).output()?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())

}

pub fn handle_exec(fs_dir: &Path, fs_file: &Path, rfs_file: &Path, symlink_dir: &Path, target_path: &Path) -> Result<()> {
    let file_type = get_file_type(fs_file)?;
    let elfloader_exec = Path::new("/root/.epkg/envs/common/profile-1/usr/bin/elf-loader");

    if file_type.contains("ELF 64-bit LSB") {
        handle_elf(elfloader_exec, target_path, symlink_dir, fs_file)?;
    } else if file_type.contains("ASCII text executable") {
        let target_path = symlink_dir.join(rfs_file);
        fs::copy(fs_file, &target_path)?;
    } else if file_type.contains("Perl script text executable") {
        let target_path = symlink_dir.join(rfs_file);
        if target_path.exists() {
            fs::remove_file(&target_path)?;
        }
        symlink(fs_file, &target_path)?;
    } else if file_type.contains("symbolic link") {
        if let Ok(ln_fs_file) = fs::canonicalize(fs_file) {
            // fs_file's target symbolic link relative path to "fs_dir"
            let ln_store_relative = ln_fs_file.strip_prefix(fs_dir)?;
            handle_symlink(ln_store_relative, rfs_file, symlink_dir)?;
        }
    }

    Ok(())
}

pub fn handle_symlink(ln_store_relative: &Path, rfs_file: &Path, symlink_dir: &Path) -> Result<()> {
    // Get ln_fs_file relative path within the env directory, relative to rfs_file.
    let joined_path = symlink_dir.join(rfs_file);
    let ln_env_dirname = joined_path.parent().ok_or_else(|| anyhow::anyhow!("Failed to get parent directory"))?;
    let ln_env_relative = pathdiff::diff_paths(symlink_dir.join(ln_store_relative), ln_env_dirname).ok_or_else(|| anyhow::anyhow!("Failed to compute relative path"))?;
    // symlink
    let target_symlink_path = symlink_dir.join(rfs_file);
    if target_symlink_path.exists() {
        fs::remove_file(&target_symlink_path)?;
    }
    symlink(&ln_env_relative, &target_symlink_path)?;

    Ok(())
}

pub fn handle_elf(elfloader_exec: &Path, target_path: &Path, symlink_dir: &Path, fs_file: &Path) -> Result<()> {
    let id1 = "{{SOURCE_ENV_DIR LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9}}";
    let id2 = "{{TARGET_ELF_PATH LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9}}";

    fs::copy(elfloader_exec, &target_path)?;
    replace_string(&target_path, id1, &symlink_dir.to_string_lossy())?;
    replace_string(&target_path, id2, &fs_file.to_string_lossy())?;
    Ok(())
}

pub fn replace_string(binary_file: &Path, long_id: &str, replacement: &str) -> Result<()> {
    // println!("Replacing '{}' with '{}' in file {:?}", long_id, replacement, binary_file);
    // read file data
    let mut file = fs::OpenOptions::new().read(true).write(true).open(binary_file)?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer)?;

    // trans long_id & replacement to bytes
    let long_id_bytes = long_id.as_bytes();
    let replacement_bytes = replacement.as_bytes();

    while let Some(position) = buffer.windows(long_id_bytes.len()).position(|window| window == long_id_bytes) {
        // println!("Replacement successful at position {}", position);

        // length: replacement > long_id_bytes, extend buffer
        if replacement_bytes.len() > long_id_bytes.len() {
            buffer.resize(buffer.len() + replacement_bytes.len() - long_id_bytes.len(), 0);
            buffer[position + replacement_bytes.len()..].rotate_right(replacement_bytes.len() - long_id_bytes.len());
        }

        // replace
        buffer[position..position + replacement_bytes.len()].copy_from_slice(replacement_bytes);

        // length: replacement <long_id_bytes, delete redundant bytes
        if replacement_bytes.len() < long_id_bytes.len() {
            buffer[position + replacement_bytes.len()..].rotate_left(long_id_bytes.len() - replacement_bytes.len());
            buffer.truncate(buffer.len() - (long_id_bytes.len() - replacement_bytes.len()));
        }
    }

    // write to file
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&buffer)?;
    file.set_len(buffer.len() as u64)?;

    Ok(())
}

impl PackageManager {

    pub fn process_package_files(&self, package_path: &str) -> Result<()> {
        let home_dir = std::env::var("HOME")?;
        let symlink_dir = format!("{}/.epkg/envs/{}/profile-current", home_dir, self.options.env);
        let fs_dir = format!("{}/fs", package_path);
        let fs_files = list_package_files(&fs_dir)?;

        println!("Details:\nsymlink_dir: {:?}\nfs_dir: {:?}\nfs_files: {:?}", symlink_dir, fs_dir, fs_files);

        for fs_file in fs_files {
            let rfs_file = fs_file.strip_prefix(&fs_dir)?;
            let target_path = Path::new(&symlink_dir).join(rfs_file);
            
            println!("fs_file: {:?}\nrfs_file: {:?}\ntarget_path: {:?}", fs_file, rfs_file, target_path);

            // If it's a symlink and the target doesn't exist, exists() will return false
            if !fs_file.exists() && !fs_file.is_symlink() {
                continue;
            }

            if rfs_file.to_string_lossy().contains("/etc/yum.repos.d") {
                continue;
            }

            // Create parent directory (if it doesn't exist)
            let symlink_parent_dir = Path::new(&symlink_dir).join(rfs_file.parent().unwrap_or(Path::new("")));
            fs::create_dir_all(&symlink_parent_dir)?;

            // Check if the path contains "/bin/"
            if fs_file.to_string_lossy().contains("/bin/") {
                handle_exec(Path::new(&fs_dir), &fs_file, Path::new(&rfs_file), Path::new(&symlink_dir), &target_path)?;
                continue; 
            }
            
            // Check if the path contains "/sbin/"
            if fs_file.to_string_lossy().contains("/sbin/") {
                handle_exec(Path::new(&fs_dir), &fs_file, Path::new(&rfs_file), Path::new(&symlink_dir), &target_path)?;
                continue; 
            }

            // Check if the path contains "/etc/"
            if fs_file.to_string_lossy().contains("/etc/") {
                fs::copy(fs_file, target_path)?;
                continue; 
            }
            
            // If it is a symbolic link, copy the symbolic link itself; otherwise, create a symbolic link.
            if fs_file.is_symlink() {
                let link_target = fs::read_link(fs_file).unwrap();
                symlink(link_target, &target_path).unwrap();
            } else {
                if target_path.exists() {
                    fs::remove_file(&target_path).unwrap();
                }
                symlink(fs_file, &target_path).unwrap();
            }
        }

        Ok(())
    }
    
    pub fn install_packages(&mut self, package_specs: ValuesRef<String>) -> Result<()> {

        self.load_store_paths()?;
        self.load_installed_packages()?;

        let mut packages_to_install = self.resolve_package_info(package_specs);
        remove_duplicates(&self.installed_packages, &mut packages_to_install, "Warning: The following packages are already installed and will be skipped:");

        self.collect_recursive_depends(&mut packages_to_install)?;
        remove_duplicates(&self.installed_packages, &mut packages_to_install, "");

        if self.options.verbose {
            println!("Packages to install:");
            print_packages_by_depend_depth(&packages_to_install);
        }

        let files = self.download_packages(&packages_to_install)?;
        self.unpack_packages(files)?;
        self.installed_packages.extend(packages_to_install);

        // Filter self.installed_packages to retain only keys containing "tree"
        self.installed_packages.retain(|key, _| key.contains("tree"));
        println!("Installed packages:{:?}", self.installed_packages);

        // create symlinks
        let home_dir = std::env::var("HOME").expect("HOME environment variable not set");
        let store_dir = format!("{}/.epkg/store", home_dir);
        for (package_name, _package_info) in &self.installed_packages {
            let package_path = format!("{}/{}", store_dir, package_name);
            self.process_package_files(&package_path)?;
        }
        // self.save_installed_packages()?;

        Ok(())
    }

}
