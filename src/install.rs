use std::fs;
use std::io::Seek;
use std::io::Write;
use std::io::SeekFrom;
use std::path::Path;
use std::collections::HashMap;
use std::os::unix::fs::symlink;
use anyhow::Result;
use anyhow::anyhow;
use crate::paths;
use crate::utils::*;
use crate::models::*;

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

pub fn handle_exec(fs_dir: &Path, fs_file: &Path, rfs_file: &Path, symlink_dir: &Path, target_path: &Path, appbin_flag: bool) -> Result<()> {
    let file_type = get_file_type(fs_file)?;

    if file_type.contains("ELF 64-bit LSB") {
        handle_elf(target_path, symlink_dir, fs_file)?;
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
        if let Ok(link_target) = fs::read_link(fs_file) {
            let ln_store_relative = if link_target.is_absolute() {
                link_target.strip_prefix("/").unwrap().to_path_buf()
            } else {
                link_target
            };
            handle_symlink(&ln_store_relative, rfs_file, symlink_dir)?;
        } else {
            return Err(anyhow!("handle_exec failed handle symbolic link {:?}", fs_file));
        }
    }

    // Add app-bin path
    if appbin_flag && rfs_file.starts_with("usr/bin/") {
        let rfs_file_appbin = rfs_file.to_string_lossy().replace("/bin", "/app-bin");
        let parent_dir_appbin = Path::new(&rfs_file_appbin).parent().unwrap();
        let symlink_dir_appbin = symlink_dir.join(parent_dir_appbin);
        if !symlink_dir_appbin.exists() {
            fs::create_dir_all(&symlink_dir_appbin)?;
        }

        let rfs_rel_path = pathdiff::diff_paths(symlink_dir.join(rfs_file), &symlink_dir_appbin).unwrap();
        let appbin_target_path = symlink_dir.join(rfs_file_appbin);

        if fs::symlink_metadata(&appbin_target_path).is_ok() {
            fs::remove_file(&appbin_target_path).unwrap();
        }
        symlink(rfs_rel_path, appbin_target_path).unwrap();
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
    if fs::symlink_metadata(&target_symlink_path).is_ok() {
        fs::remove_file(&target_symlink_path).unwrap();
    }
    symlink(&ln_env_relative, &target_symlink_path).unwrap();

    Ok(())
}

pub fn handle_elf(target_path: &Path, symlink_dir: &Path, fs_file: &Path) -> Result<()> {
    let id1 = "{{SOURCE_ENV_DIR LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9}}";
    let id2 = "{{TARGET_ELF_PATH LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9}}";

    fs::copy(&paths::instance.elfloader_exec, &target_path)?;
    replace_string(&target_path, id1, &symlink_dir.to_string_lossy())?;
    replace_string(&target_path, id2, &fs_file.to_string_lossy())?;
    Ok(())
}

pub fn replace_string(binary_file: &Path, long_id: &str, replacement: &str) -> Result<()> {
    let data = fs::read(binary_file)?;
    let pattern = long_id.as_bytes();
    
    if let Some(pos) = data.windows(pattern.len()).position(|window| window == pattern) {
        let mut file = fs::OpenOptions::new().write(true).open(binary_file)?;
        file.seek(SeekFrom::Start(pos as u64))?;
        // Write the replacement followed by a null terminator.
        file.write_all(format!("{}\0", replacement).as_bytes())?;
    }
    
    Ok(())
}

impl PackageManager {

    pub fn postinstall_scriptlet(&self, pkg_name: &str, symlink_dir: &Path) -> Result<()> {
        match pkg_name {
            "golang" => {
                // usr/bin
                symlink(symlink_dir.join("usr/lib/golang/bin/go"), symlink_dir.join("usr/bin/go"))?;
                symlink(symlink_dir.join("usr/lib/golang/bin/gofmt"), symlink_dir.join("usr/bin/gofmt"))?;
                // usr/app-bin
                symlink(Path::new("../bin/go"), symlink_dir.join("usr/app-bin/go"))?;
                symlink(Path::new("../bin/gofmt"), symlink_dir.join("usr/app-bin/gofmt"))?;
            }
            "ca-certificates" => {
                fs::copy(
                    paths::instance.epkg_common_root.join("profile-current/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem"),
                    symlink_dir.join("etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem"),
                )?;
            }
            "maven" => {
                // usr/bin
                symlink(symlink_dir.join("usr/share/maven/bin/mvn"), symlink_dir.join("usr/bin/mvn"))?;
                // usr/app-bin
                symlink(Path::new("../bin/mvn"), symlink_dir.join("usr/app-bin/mvn"))?;
            }
            "python3-pip" => {
                for file in &["pip", "pip3", "pip3.11"] {
                    let path = symlink_dir.join(format!("usr/bin/{}", file));
                    let content = fs::read_to_string(&path)?;
                    let new_content = content.replacen("#!/usr/bin/python", "#!/usr/bin/env python3", 1);
                    fs::write(&path, new_content)?;
                }
            }
            "ruby" => {
                let path = symlink_dir.join("usr/bin/erb");
                let content = fs::read_to_string(&path)?;
                let new_content = content.replacen("#!/usr/bin/ruby", "#!/usr/bin/env ruby", 1);
                fs::write(&path, new_content)?;
            }
            "rubygems" => {
                let path = symlink_dir.join("usr/bin/gem");
                let content = fs::read_to_string(&path)?;
                let new_content = content.replacen("#!/usr/bin/ruby", "#!/usr/bin/env ruby", 1);
                fs::write(&path, new_content)?;
            }
            _ => {}
        }

        Ok(())
    }

    pub fn new_package(&self, fs_dir: &str, symlink_dir: &str, appbin_flag: bool) -> Result<()> {
        let fs_files = list_package_files(&fs_dir).unwrap();
        for fs_file in fs_files {
            let rfs_file = fs_file.strip_prefix(&fs_dir).unwrap();
            let target_path = Path::new(&symlink_dir).join(rfs_file);
            
            // println!("fs_file: {:?}\nrfs_file: {:?}\ntarget_path: {:?}", fs_file, rfs_file, target_path);
            // Create empty directory
            if fs_file.is_dir() {
                fs::create_dir_all(&target_path).unwrap();
                continue;
            }

            // If it's a symlink | the target doesn't exist | appbin_flag=false, exists() will return false
            if !fs_file.exists() && !fs_file.is_symlink() && !appbin_flag {
                continue;
            }

            // Create parent directory (if it doesn't exist)
            let symlink_parent_dir = Path::new(&symlink_dir).join(rfs_file.parent().unwrap_or(Path::new("")));
            fs::create_dir_all(&symlink_parent_dir).unwrap();

            // Check if the path contains "/bin/"
            if fs_file.to_string_lossy().contains("/bin/") {
                handle_exec(Path::new(&fs_dir), &fs_file, Path::new(&rfs_file), Path::new(&symlink_dir), &target_path, appbin_flag)?;
                continue; 
            }
            
            // Check if the path contains "/sbin/"
            if fs_file.to_string_lossy().contains("/sbin/") {
                handle_exec(Path::new(&fs_dir), &fs_file, Path::new(&rfs_file), Path::new(&symlink_dir), &target_path, appbin_flag)?;
                continue; 
            }

            // Check if the path contains "/etc/"
            if fs_file.to_string_lossy().contains("/etc/") {
                if !fs::symlink_metadata(&fs_file).map(|metadata| metadata.file_type().is_symlink()).unwrap() {
                    fs::copy(fs_file, target_path).unwrap();
                    continue; 
                }
            }

            // If it is a symbolic link, copy the symbolic link itself; otherwise, create a symbolic link.
            if fs::symlink_metadata(&target_path).is_ok() {
                fs::remove_file(&target_path).unwrap();
            }
            symlink(fs::read_link(&fs_file).unwrap_or(fs_file.to_path_buf()), &target_path).unwrap();
        }

        Ok(())
    }

    pub fn install_packages(&mut self, package_specs: Vec<String>, command_line: &str) -> Result<()> {
        self.load_store_paths().unwrap();
        self.load_installed_packages().unwrap();

        let mut packages_to_install = self.resolve_package_info(package_specs.clone());
        self.resolve_appbin_source(&mut packages_to_install);
        self.collect_recursive_depends(&mut packages_to_install)?;
        remove_duplicates(&self.installed_packages, &mut packages_to_install, "Warning: The following packages are already installed and will be skipped:");
        if packages_to_install.is_empty() {
            return Err(anyhow!("No packages to install"));
        }

        if self.options.verbose {
            println!("appbin_source: {:?}", self.appbin_source);
            println!("Packages to install:");
            print_packages_by_depend_depth(&packages_to_install);
        }

        let files = self.download_packages(&packages_to_install)?;
        self.unpack_packages(files).unwrap();

        // create symlinks
        let symlink_dir = self.get_current_profile()?;
        let mut appbin_count = 0;
        let mut appbin_packages = Vec::new();
        for (pkgline, _package_info) in &packages_to_install {
            let mut appbin_flag = false;
            let mut pkg_name = String::new();
            // appbin_source check
            if let Some(spec) = self.pkghash2spec.get(&pkgline[0..32]) {
                appbin_flag = package_specs.contains(&spec.name) || spec.source.as_ref().map_or(false, |source| self.appbin_source.contains(source));
                pkg_name = spec.name.clone();
                if appbin_flag {
                    appbin_count += 1;
                    appbin_packages.push(pkg_name.clone());
                }
            }
            // install files
            let fs_dir = format!("{}/{}/fs", paths::instance.epkg_store_root.display(), pkgline);
            self.new_package(&fs_dir, &symlink_dir, appbin_flag).unwrap();
            // postinstall
            self.postinstall_scriptlet(&pkg_name, Path::new(&symlink_dir)).unwrap();
        }

        // Save installed packages
        self.installed_packages.extend(packages_to_install.clone());
        self.save_installed_packages().unwrap();
        self.record_history("install", packages_to_install.keys().cloned().collect(), vec![], command_line)?;
        
        println!("Installation successful - Total packages: {}, AppBin packages: {}", packages_to_install.len(), appbin_count);
        if !appbin_packages.is_empty() {
            println!("AppBin package list: {}", appbin_packages.join(", "));
        }

        Ok(())
    }

}
