use std::fs;
use std::io::Seek;
use std::io::Write;
use std::io::SeekFrom;
use std::io::BufRead;
use std::path::Path;
use std::path::PathBuf;
use std::collections::HashMap;
use std::os::unix::fs::symlink;
use std::os::unix::fs::PermissionsExt;
use anyhow::Result;
use anyhow::anyhow;
use crate::dirs;
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
fn remove_duplicates(
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
            eprintln!("{} ({} packages)", warn, duplicates.len());
        }

        // Remove duplicates from `b`
        for package_name in duplicates {
            // appbin_flag 变更需要处理
            if a.get(&package_name).unwrap().appbin_flag ==
               b.get(&package_name).unwrap().appbin_flag {
                b.remove(&package_name);
            }
        }
    }
}

fn handle_elf(target_path: &Path, env_root: &Path, fs_file: &Path) -> Result<()> {
    let id1 = "{{SOURCE_ENV_DIR LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9}}";
    let id2 = "{{TARGET_ELF_PATH LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9}}";

    fs::copy(env_root.join("/usr/bin/elf-loader"), &target_path)?;
    replace_string(&target_path, id1, &env_root.to_string_lossy())?;
    replace_string(&target_path, id2, &fs_file.to_string_lossy())?;
    Ok(())
}

fn replace_string(binary_file: &Path, long_id: &str, replacement: &str) -> Result<()> {
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

fn mirror_dir(env_root: &Path, store_fs_dir: &Path, fs_files: &[PathBuf]) -> Result<()> {
    for fs_file in fs_files {
        let fhs_file = fs_file.strip_prefix(store_fs_dir)?;
        let target_path = env_root.join(fhs_file);

        // Create parent directory if it doesn't exist
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent)?;
        }

        if fs_file.is_dir() {
            fs::create_dir_all(&target_path)?;
            continue;
        }

        if target_path.exists() {
            eprintln!("Warning: File {} already exists, overwriting", target_path.display());
            fs::remove_file(&target_path)?;
        }

        let metadata = fs::symlink_metadata(fs_file)?;
        if metadata.file_type().is_symlink() {
            shortcut_symlink(store_fs_dir, fs_file, fhs_file, &target_path);
        } else {
            if fhs_file.starts_with("etc/") {
                fs::copy(fs_file, &target_path)?;
            } else {
                symlink(fs_file, &target_path)?;
            }
        }
    }
    Ok(())
}

// like symlink() but removes one level of indirection
fn shortcut_symlink(store_fs_dir: &Path, fs_file: &Path, fhs_file: &Path, target_path: &Path) -> Result<()> {
    if let Ok(link_target) = fs::read_link(fs_file) {
        // Get new_link_target
        // python3.11:
        //     /root/.epkg/store/2h652gawx5zjpazx83ep2jkcv2kkp0xm__python3__3.11.6__2.oe2403/fs/usr/bin/python3 -> python3.11
        // ../bin/pidof:
        //     /root/.epkg/store/dkaz2ks577dhyg3gz8n414xvq52x7e9g__procps-ng__4.0.4__5.oe2403/fs/usr/sbin/pidof -> /usr/bin/pidof
        // ../libexec/qemu-kvm:
        //     /root/.epkg/store/pbaknz0skh99y3mmdwcs2xxhay5mzbgj__qemu__8.2.0__13.oe2403/fs/usr/bin/qemu-kvm -> /usr/libexec/qemu-kvm
        // ../../lib64/ld-linux-x86-64.so.2:
        //     /root/.epkg/store/3ajbdnc50knwxw39j3bgaw86nxs3kt0w__glibc-common__2.38__29.oe2403/fs/usr/bin/ld.so -> ../../lib64/ld-linux-x86-64.so.2
        let new_link_target = if link_target.is_absolute() {
            store_fs_dir.join(link_target)
        } else if link_target.starts_with("../") {
            normalize_join(store_fs_dir, &link_target)
        } else {
            fs_file.parent()
                .ok_or_else(|| anyhow!("Failed to get parent directory for {}", fs_file.display()))?
                .join(link_target)
        };

        symlink(&new_link_target, target_path)?;
    }
    Ok(())
}

fn normalize_join(base: &Path, subpath: &Path) -> PathBuf {
    let mut components: Vec<_> = base.components().collect();

    for component in subpath.components() {
        match component {
            std::path::Component::ParentDir if !components.is_empty() => {
                components.pop();
            },
            std::path::Component::CurDir => {},
            _ => components.push(component),
        }
    }

    components.iter().collect()
}

fn create_ebin_wrappers(env_root: &Path, fs_files: &[PathBuf]) -> Result<()> {
    for fs_file in fs_files {
        let path_str = fs_file.to_string_lossy();

        if !path_str.contains("/bin/") && !path_str.contains("/sbin/") && !path_str.contains("/libexec/") {
            continue;
        }

        let lib_regex = regex::Regex::new(r"\.(?:so|so\.\d+)$").unwrap();
        if lib_regex.is_match(&path_str) {
            continue;
        }

        // Skip if not executable
        let metadata = fs::metadata(fs_file)?;
        let mode = metadata.permissions().mode();
        if mode & 0o111 == 0 {
            continue;
        }

        create_ebin_wrapper(env_root, fs_file)?;
    }
    Ok(())
}

fn create_ebin_wrapper(env_root: &Path, fs_file: &Path) -> Result<()> {
    let file_type = get_file_type(fs_file)?;
    let basename = fs_file.file_name()
        .ok_or_else(|| anyhow!("Failed to get filename for {}", fs_file.display()))?;
    let ebin_path = env_root.join("usr/ebin").join(basename);

    // Create ebin directory if it doesn't exist
    if let Some(parent) = ebin_path.parent() {
        fs::create_dir_all(parent)?;
    }

    match file_type {
        FileType::Elf => {
            handle_elf(&ebin_path, env_root, fs_file)?;
        }
        FileType::ShellScript | FileType::PerlScript | FileType::PythonScript | FileType::RubyScript | FileType::NodeScript | FileType::LuaScript => {
            let file = fs::File::open(fs_file)?;
            let mut reader = std::io::BufReader::new(file);
            let mut first_line = String::new();
            reader.read_line(&mut first_line)?;

            let env_shell_bang_line = if first_line.starts_with("#!") {
                let interpreter_with_params = first_line[2..].trim();
                let interpreter_path = interpreter_with_params.split_whitespace().next()
                    .ok_or_else(|| anyhow!("Failed to parse interpreter path from shebang"))?;
                let interpreter_basename = Path::new(interpreter_path).file_name()
                    .ok_or_else(|| anyhow!("Failed to get interpreter basename"))?
                    .to_string_lossy();
                format!("#!{}/usr/ebin/{}\n", env_root.display(), interpreter_basename)
            } else {
                String::new()
            };

            let mut wrapper = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&ebin_path)?;

            if !env_shell_bang_line.is_empty() {
                wrapper.write_all(env_shell_bang_line.as_bytes())?;
            }

            // Add language-specific exec command
            let exec_cmd = match file_type {
                FileType::ShellScript => format!("exec {:?}\n", fs_file),
                FileType::PythonScript => format!("exec(open({:?}).read())\n", fs_file),
                FileType::RubyScript => format!("load({:?})\n", fs_file),
                FileType::LuaScript => format!("dofile({:?})\n", fs_file),
                _ => format!("exec {:?}\n", fs_file),
            };
            wrapper.write_all(exec_cmd.as_bytes())?;

            // Make the wrapper executable
            let mut perms = fs::metadata(&ebin_path)?.permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&ebin_path, perms)?;
        }
        _ => {}
    }
    Ok(())
}

impl PackageManager {

    // link files from env_root to store_fs_dir
    pub fn new_package(&self, store_fs_dir: &PathBuf, env_root: &PathBuf, appbin_flag: bool) -> Result<()> {
        let fs_files = list_package_files(store_fs_dir)?;
        mirror_dir(Path::new(env_root), Path::new(store_fs_dir), &fs_files)?;
        if appbin_flag {
            create_ebin_wrappers(Path::new(env_root), &fs_files)?;
        }
        Ok(())
    }

    pub fn install_packages(&mut self, package_specs: Vec<String>) -> Result<()> {
        self.load_store_paths()?;
        self.load_installed_packages()?;

        let mut packages_to_install = self.resolve_package_info(package_specs.clone());
        self.record_appbin_source(&mut packages_to_install);
        self.collect_essential_packages(&mut packages_to_install)?;
        self.collect_recursive_depends(&mut packages_to_install)?;
        remove_duplicates(&self.installed_packages, &mut packages_to_install, "Warning: Some packages are already installed and will be skipped:");
        if packages_to_install.is_empty() {
            return Err(anyhow!("No packages to install"));
        }
        self.install_pkglines(packages_to_install)
    }

    pub fn install_pkglines(&mut self, packages_to_install: HashMap<String, InstalledPackageInfo>) -> Result<()> {
        if self.options.verbose {
            println!("Packages to install:");
            print_packages_by_depend_depth(&packages_to_install);
        }

        let files = self.download_packages(&packages_to_install)?;
        self.unpack_packages(files)?;
        self.change_appbin_flag_same_source(&mut packages_to_install)?;
        self.create_new_generation()?;

        let mut appbin_count = 0;
        let mut appbin_packages = Vec::new();
        let env_root = self.get_default_env_root();
        let store_root = self.dirs.epkg_store;
        for (pkgline, _package_info) in &packages_to_install {
            let mut appbin_flag = false;
            let mut pkg_name = String::new();
            // appbin_source check
            if let Some(spec) = self.pkghash2spec.get(&pkgline[0..32]) {
                appbin_flag = _package_info.appbin_flag;
                pkg_name = spec.name.clone();
                if appbin_flag {
                    appbin_count += 1;
                    appbin_packages.push(pkg_name.clone());
                }
            }
            self.new_package(store_root.join(pkgline).join("fs"), env_root, appbin_flag)?;
        }

        // Save installed packages
        self.installed_packages.extend(packages_to_install.clone());
        self.save_installed_packages()?;
        self.record_history("install", packages_to_install.keys().cloned().collect(), vec![])?;

        println!("Installation successful - Total packages: {}, AppBin packages: {}", packages_to_install.len(), appbin_count);
        if !appbin_packages.is_empty() {
            println!("AppBin package list: {}", appbin_packages.join(", "));
        }

        Ok(())
    }

}
