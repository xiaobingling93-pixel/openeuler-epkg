use std::fs;
use std::io::Seek;
use std::io::Write;
use std::io::SeekFrom;
use std::path::Path;
use color_eyre::eyre::eyre;
use std::path::PathBuf;
use std::collections::HashMap;
use std::os::unix::fs::symlink;
use std::os::unix::fs::PermissionsExt;
use color_eyre::eyre::{self, Result, Context};
use crate::models::*;
use crate::utils::*;
use crate::dirs::find_env_root;
use crate::package;

fn print_packages_by_depend_depth(packages: &HashMap<String, InstalledPackageInfo>) {
    // Convert HashMap to a Vec of tuples (pkgkey, info)
    let mut packages_vec: Vec<(&String, &InstalledPackageInfo)> = packages.iter().collect();

    // Sort by depend_depth
    packages_vec.sort_by(|a, b| a.1.depend_depth.cmp(&b.1.depend_depth));

    // Print the header
    println!("{:<12} {:<10}", "depend_depth", "package");

    // Print each package
    for (pkgkey, info) in packages_vec {
        println!("{:<12} {:<10}", info.depend_depth, pkgkey);
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
            log::info!("{} {:?}", warn, duplicates);
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
    // Constants for placeholder strings in elf-loader
    const SOURCE_ENV_DIR_PLACEHOLDER: &str = "{{SOURCE_ENV_DIR LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9}}";
    const TARGET_ELF_PATH_PLACEHOLDER: &str = "{{TARGET_ELF_PATH LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9}}";

    // Get common environment root path
    let common_env_root = find_env_root("common")
        .ok_or_else(|| eyre::eyre!("Common environment not found"))?;

    // Copy elf-loader from common environment
    let elf_loader_path = common_env_root.join("usr/bin/elf-loader");
    fs::copy(&elf_loader_path, target_path)
        .with_context(|| format!(
            "Failed to copy elf-loader from {} to {}",
            elf_loader_path.display(),
            target_path.display()
        ))?;

    // Replace placeholder strings with actual paths
    replace_string(target_path, SOURCE_ENV_DIR_PLACEHOLDER, &env_root.to_string_lossy())
        .with_context(|| format!("Failed to replace SOURCE_ENV_DIR_PLACEHOLDER in {}", target_path.display()))?;
    replace_string(target_path, TARGET_ELF_PATH_PLACEHOLDER, &fs_file.to_string_lossy())
        .with_context(|| format!("Failed to replace TARGET_ELF_PATH_PLACEHOLDER in {}", target_path.display()))?;

    log::debug!(
        "handle_elf target_path={}, env_root={}, fs_file={}",
        target_path.display(),
        env_root.display(),
        fs_file.display()
    );
    Ok(())
}

fn replace_string(binary_file: &Path, long_id: &str, replacement: &str) -> Result<()> {
    let data = fs::read(binary_file)
        .with_context(|| format!("Failed to read {} for replace_string", binary_file.display()))?;
    let pattern = long_id.as_bytes();

    if let Some(pos) = data.windows(pattern.len()).position(|window| window == pattern) {
        let mut file = fs::OpenOptions::new().write(true).open(binary_file)
            .with_context(|| format!("Failed to open {} for replace_string", binary_file.display()))?;
        file.seek(SeekFrom::Start(pos as u64))
            .with_context(|| format!("Failed to seek to position {} in {}", pos, binary_file.display()))?;
        // Write the replacement followed by a null terminator.
        file.write_all(format!("{}\0", replacement).as_bytes())?;
    }

    Ok(())
}

fn mirror_dir(env_root: &Path, store_fs_dir: &Path, fs_files: &[PathBuf]) -> Result<()> {
    for fs_file in fs_files {
        let fhs_file = fs_file.strip_prefix(store_fs_dir)
            .with_context(|| format!("Failed to strip prefix {} from {}", store_fs_dir.display(), fs_file.display()))?;
        let target_path = env_root.join(fhs_file);

        // Create parent directory if it doesn't exist
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create parent directory {}", parent.display()))?;
        }

        if fs_file.is_dir() {
            fs::create_dir_all(&target_path)
                .with_context(|| format!("Failed to create directory {}", target_path.display()))?;
            continue;
        }

        if fs::symlink_metadata(&target_path).is_ok() {
            log::info!("Warning: File already exists, overwriting {} with {}", target_path.display(), fs_file.display());
            fs::remove_file(&target_path)
                .with_context(|| format!("Failed to remove {} for mirror_dir", target_path.display()))?;
        }

        let metadata = fs::symlink_metadata(fs_file)
            .with_context(|| format!("Failed to get metadata for {} for mirror_dir", fs_file.display()))?;
        if metadata.file_type().is_symlink() {
            shortcut_symlink(store_fs_dir, fs_file, &target_path)
                .with_context(|| format!("Failed to shortcut_symlink from {} to {}", fs_file.display(), target_path.display()))?;
        } else {
            if fhs_file.starts_with("etc/") {
                fs::copy(fs_file, &target_path)
                    .with_context(|| format!("Failed to copy {} to {}", fs_file.display(), target_path.display()))?;
            } else {
                symlink(fs_file, &target_path)
                    .with_context(|| format!("Failed to create symlink from {} to {}", fs_file.display(), target_path.display()))?;
            }
        }
    }
    Ok(())
}

// like symlink() but removes one level of indirection
fn shortcut_symlink(store_fs_dir: &Path, fs_file: &Path, target_path: &Path) -> Result<()> {
    if let Ok(link_target) = fs::read_link(fs_file) {
        // Handle different types of symlinks:
        // 1. Absolute paths: e.g. /usr/bin/python3 -> /usr/bin/python3.11
        //    Join with store_fs_dir to make it relative to the package root
        // 2. Parent-relative paths: e.g. ../bin/pidof -> /usr/bin/pidof
        //    Use normalize_join to resolve the ../ components against store_fs_dir
        // 3. Sibling-relative paths: e.g. python3 -> python3.11
        //    Join with the parent directory of the source file
        let new_link_target = if link_target.is_absolute() {
            // For absolute paths like /usr/bin/python3.11, make them relative to store_fs_dir
            // Note: Using Path.join() here would incorrectly handle absolute paths by discarding the base path
            PathBuf::from(format!("{}/{}", store_fs_dir.display(), link_target.display()))
        } else if link_target.starts_with("../") {
            // For parent-relative paths like ../bin/pidof, normalize against fs_file
            normalize_join(fs_file.parent().ok_or_else(|| eyre::eyre!("Failed to get parent directory for {}", fs_file.display()))?,
                           &link_target)
        } else {
            // For sibling-relative paths like python3.11, join with source file's parent
            fs_file.parent()
                .ok_or_else(|| eyre::eyre!("Failed to get parent directory for {}", fs_file.display()))?
                .join(link_target)
        };

        symlink(&new_link_target, target_path)
            .with_context(|| format!("Failed to create symlink from {} to {}", fs_file.display(), target_path.display()))?;
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
    log::debug!("Creating ebin wrappers for {} files in {}", fs_files.len(), env_root.display());
    for fs_file in fs_files {
        let path_str = fs_file.to_string_lossy();

        if !path_str.contains("/bin/") && !path_str.contains("/sbin/") && !path_str.contains("/libexec/") {
            continue;
        }

        let lib_regex = regex::Regex::new(r"\.(so|so\.\d+)$").unwrap();
        if lib_regex.is_match(&path_str) {
            continue;
        }

        // Skip if not executable or is directory
        let metadata = fs::symlink_metadata(fs_file)
            .with_context(|| format!("Failed to get metadata for {} for create_ebin_wrappers", fs_file.display()))?;
        let mode = metadata.permissions().mode();
        if mode & 0o111 == 0 || metadata.is_dir() {
            continue;
        }

        create_ebin_wrapper(env_root, fs_file)
            .with_context(|| format!("Failed to create ebin wrapper for {}", fs_file.display()))?;
    }
    Ok(())
}

fn create_ebin_wrapper(env_root: &Path, fs_file: &Path) -> Result<()> {
    let (file_type, first_line) = get_file_type(fs_file)
        .with_context(|| format!("Failed to determine file type for {}", fs_file.display()))?;
    let basename = fs_file.file_name()
        .ok_or_else(|| eyre::eyre!("Failed to get filename for {}", fs_file.display()))?;
    let ebin_path = env_root.join("usr/ebin").join(basename);

    log::debug!(
        "Creating ebin wrapper: ebin_path={}, fs_file={}, file_type={:?}, first_line={:?}",
        ebin_path.display(),
        fs_file.display(),
        file_type,
        first_line
    );
    match file_type {
        FileType::Elf => {
            handle_elf(&ebin_path, env_root, fs_file)
                .with_context(|| format!("Failed to handle elf for {}", ebin_path.display()))?;
        }
        FileType::ShellScript
        | FileType::PerlScript
        | FileType::PythonScript
        | FileType::RubyScript
        | FileType::NodeScript
        | FileType::LuaScript => {
            create_script_wrapper(env_root, fs_file, &ebin_path, file_type, &first_line)
                .with_context(|| format!("Failed to create script wrapper for {}", fs_file.display()))?;
        }
        _ => {}
    }
    Ok(())
}

fn create_script_wrapper(
    env_root: &Path,
    fs_file: &Path,
    ebin_path: &Path,
    file_type: FileType,
    first_line: &str,
) -> Result<()> {
    let env_shell_bang_line = create_shebang_line(env_root, first_line)?;
    let exec_cmd = get_exec_command(&file_type, fs_file);

    let mut wrapper = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(ebin_path)
        .with_context(|| format!("Failed to open {} for create_script_wrapper", ebin_path.display()))?;

    if !env_shell_bang_line.is_empty() {
        wrapper.write_all(env_shell_bang_line.as_bytes())
            .with_context(|| format!("Failed to write shebang line to {}", ebin_path.display()))?;
    }

    wrapper.write_all(exec_cmd.as_bytes())
        .with_context(|| format!("Failed to write exec command to {}", ebin_path.display()))?;

    set_wrapper_permissions(ebin_path)?;

    log::debug!(
        "Created script wrapper: ebin_path={}, fs_file={}, file_type={:?}, first_line={:?}",
        ebin_path.display(),
        fs_file.display(),
        file_type,
        first_line
    );
    Ok(())
}

/// Parse a shebang line into interpreter path and parameters
fn parse_shebang_line(first_line: &str) -> Result<(String, String)> {
    if !first_line.starts_with("#!") {
        return Err(eyre::eyre!("No shebang line found"));
    }

    let interpreter_with_params = first_line[2..].trim();
    // Example: interpreter_with_params = "/bin/sh"
    let (interpreter_path, params) = match interpreter_with_params.split_once(' ') {
        Some((path, params)) => (path.to_string(), params.to_string()),  // Example: path="/usr/bin/env", params="python3"
        None => (interpreter_with_params.to_string(), String::new()),    // Example: path="/bin/sh", params=""
    };
    log::debug!("interpreter_path: '{}', params: '{}'", interpreter_path, params);

    Ok((interpreter_path, params))
}

/// Find and link the appropriate interpreter if it doesn't exist
fn find_link_interpreter(interpreter_in_env: &Path, interpreter_basename: &str) -> Result<()> {
    if interpreter_in_env.exists() {
        return Ok(());
    }

    // if the soft link is broken, delete it
    if let Ok(metadata) = fs::symlink_metadata(interpreter_in_env) {
        if metadata.file_type().is_symlink() {
            if fs::read_link(interpreter_in_env).map(|t| !t.exists()).unwrap_or(false) {
                fs::remove_file(interpreter_in_env)?
            } else {
                return Ok(());
            }
        } else {
            return Ok(());
        }
    }

    // Get the parent directory to search in
    let parent = interpreter_in_env.parent()
        .ok_or_else(|| eyre::eyre!("Failed to get parent directory of {}", interpreter_in_env.display()))?;

    // Find candidate interpreters based on the type
    let targets = match interpreter_basename {
        // For shell scripts, look for bash or dash as alternatives
        "sh" => glob::glob(&format!("{}/{{bash,dash}}", parent.display()))
            .with_context(|| "Failed to glob for shell interpreters")?,

        // For other interpreters (python, ruby etc), look for versioned variants
        // e.g. python3.8, python3.9 etc
        _ => glob::glob(&format!("{}?*", interpreter_in_env.display()))
            .with_context(|| format!("Failed to glob for {} interpreters", interpreter_basename))?
    };

    // Find the "latest" interpreter by comparing filenames
    let target = targets
        .filter_map(Result::ok)
        .max_by(|a, b| {
            let a_name = a.file_name().unwrap_or_default().to_string_lossy();
            let b_name = b.file_name().unwrap_or_default().to_string_lossy();
            a_name.cmp(&b_name)
        })
        .ok_or_else(|| eyre::eyre!("No suitable interpreter found for {}", interpreter_basename))?;

    // Create a symlink from the found interpreter to the expected location
    symlink(&target, interpreter_in_env)
        .with_context(|| format!("Failed to create symlink from {} to {}",
            target.display(), interpreter_in_env.display()))?;

    Ok(())
}

/// Create the wrapper for the interpreter in the ebin directory
fn create_interpreter_wrapper(env_root: &Path, interpreter_path: &str, interpreter_basename: &str) -> Result<String> {
    // Example: env_interpreter_path = "/home/wfg/.epkg/envs/main/ebin/sh"
    let env_interpreter_path = format!("{}/ebin/{}", env_root.display(), interpreter_basename);
    let env_interpreter = Path::new(&env_interpreter_path);

    if !env_interpreter.exists() {
        // Example: interpreter_in_env = "/home/wfg/.epkg/envs/main/bin/sh"
        // Which is a symlink to: "/home/wfg/.epkg/store/twktsyye3ksj068w2fx9pz5fefwy70mw__bash__5.2.15__9.oe2403/fs/usr/bin/bash"
        // use format!() instead of Path::join() to enforce simple string operation
        let interpreter_in_env = format!("{}{}", env_root.display(), interpreter_path);
        let interpreter_in_env = Path::new(&interpreter_in_env);

        // Find and link the interpreter if needed
        match find_link_interpreter(interpreter_in_env, interpreter_basename) {
            Ok(()) => {},
            Err(e) => {
                if interpreter_basename.ends_with("sh") {
                    println!("Shell interpreter {} is not found in environment. you can install it later.", interpreter_basename);
                    return Ok("".to_string());
                } else {
                    return Err(e);
                }
            }
        }

        // Example: store_interpreter = "/home/wfg/.epkg/store/twktsyye3ksj068w2fx9pz5fefwy70mw__bash__5.2.15__9.oe2403/fs/usr/bin/bash"
        // Create the wrapper
        let store_interpreter = fs::canonicalize(interpreter_in_env)
            .with_context(|| format!("Failed to resolve interpreter path: {}", interpreter_in_env.display()))?;

        log::debug!("handle_elf params: env_interpreter={:?}, env_root={:?}, store_interpreter={:?}, interpreter_in_env={:?}",
            env_interpreter, env_root, store_interpreter, interpreter_in_env);
        // Example output:
        // handle_elf params:
        // env_interpreter="/home/wfg/.epkg/envs/main/ebin/sh",
        // env_root="/home/wfg/.epkg/envs/main",
        // store_interpreter="/home/wfg/.epkg/store/twktsyye3ksj068w2fx9pz5fefwy70mw__bash__5.2.15__9.oe2403/fs/usr/bin/bash",
        // interpreter_in_env="/home/wfg/.epkg/envs/main/bin/sh"
        handle_elf(env_interpreter, env_root, &store_interpreter)?;
    }

    Ok(env_interpreter_path)
}

fn create_shebang_line(env_root: &Path, first_line: &str) -> Result<String> {
    let (interpreter_path, params) = parse_shebang_line(first_line)
        .with_context(|| format!("Failed to parse shebang line: '{}'", first_line))?;

    let interpreter_basename = Path::new(&interpreter_path).file_name()
        .ok_or_else(|| eyre::eyre!("Failed to get interpreter basename"))?
        .to_string_lossy();

    let env_interpreter_path = match create_interpreter_wrapper(env_root, &interpreter_path, &interpreter_basename)
        .with_context(|| format!("Failed to create interpreter wrapper for {} with basename {}", interpreter_path, interpreter_basename))
    {
        Ok(path) => {
            if path == "" {
                return Ok(first_line.to_string());
            }
            path
        },
        Err(e) => return Err(e),
    };

    // Example output: "#!/home/wfg/.epkg/envs/main/ebin/sh "
    Ok(format!("#!{} {}\n", env_interpreter_path, params))
}

fn get_exec_command(file_type: &FileType, fs_file: &Path) -> String {
    match file_type {
        FileType::ShellScript => format!("exec {:?} \"$@\"\n", fs_file),
        FileType::PythonScript => format!("exec(open({:?}).read())\n", fs_file),
        FileType::RubyScript => format!("load({:?})\n", fs_file),
        FileType::LuaScript => format!("dofile({:?})\n", fs_file),
        _ => format!("exec {:?} \"$@\"\n", fs_file),
    }
}

fn set_wrapper_permissions(ebin_path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = fs::Permissions::from_mode(0o755);
    fs::set_permissions(ebin_path, perms)
        .with_context(|| format!("Failed to set permissions for {}", ebin_path.display()))?;
    Ok(())
}

impl PackageManager {

    // link files from env_root to store_fs_dir
    pub fn link_package(&self, store_fs_dir: &PathBuf, env_root: &PathBuf) -> Result<()> {
        let fs_files = list_package_files(store_fs_dir.to_str().ok_or_else(|| eyre::eyre!("Invalid store_fs_dir path: {}", store_fs_dir.display()))?)
            .with_context(|| format!("Failed to list package files in {}", store_fs_dir.display()))?;
        mirror_dir(env_root, store_fs_dir, &fs_files)
            .with_context(|| format!("Failed to mirror directory from {} to {}", store_fs_dir.display(), env_root.display()))?;
        Ok(())
    }

    // - run post-install scriptlets
    // - create ebin wrappers
    pub fn expose_package(&self, store_fs_dir: &PathBuf, env_root: &PathBuf) -> Result<()> {
        log::debug!("expose_package {}", store_fs_dir.display());
        let fs_files = list_package_files(store_fs_dir.to_str().ok_or_else(|| eyre::eyre!("Invalid store_fs_dir path"))?)?;
        create_ebin_wrappers(env_root, &fs_files)?;
        Ok(())
    }

    pub fn install_packages(&mut self, package_specs: Vec<String>) -> Result<()> {
        self.load_installed_packages()?;

        let mut packages_to_install = self.resolve_package_info(package_specs.clone());
        let mut packages_to_install_clone = packages_to_install.clone();
        let mut depends_pkg : HashMap<String, InstalledPackageInfo> = HashMap::new();
        let channel_config = self.get_channel_config(config().common.env.clone())?;
        let repo_format = channel_config.format;
        for (pkgline, pkginfo) in packages_to_install_clone.drain() {
            let mut tmp_pkg = HashMap::new();
            tmp_pkg.insert(pkgline, pkginfo);
            let mut tmp_depends : HashMap<String, InstalledPackageInfo> = HashMap::new();
            self.collect_depends(&mut tmp_pkg, &mut tmp_depends, 1,  repo_format)?;
            depends_pkg.extend(tmp_depends);
        }

        for pkg in depends_pkg.keys() {
            if let Some(info) = packages_to_install.get_mut(pkg) {
                info.depend_depth = 1;
            }
        }

        let current_installed: Vec<String> = packages_to_install
            .keys()
            .filter(|name| self.installed_packages.contains_key(*name))
            .cloned()
            .collect();
        if packages_to_install.len() == current_installed.len() &&
           packages_to_install.keys().all(|k| current_installed.contains(k)) {
           println!("All packages input have already installed");
           return Ok(());
        }
        if current_installed.len() > 0 {
            println!("These packages have already been installed: {}", current_installed.join(","));
        }

        self.record_appbin_source(&mut packages_to_install)?;
        self.collect_essential_packages(&mut packages_to_install)?;
        let current_env_name_ref = &config().common.env;
        let channel_config = self.channels_config.get(current_env_name_ref)
            .ok_or_else(|| eyre::eyre!(
                "Channel configuration not found for environment '{}'. Ensure environment is initialized and linked to a channel.",
                current_env_name_ref
            ))?;
        let repo_format = channel_config.format;
        self.collect_recursive_depends(&mut packages_to_install, repo_format)?;
        remove_duplicates(&self.installed_packages, &mut packages_to_install, "Warning: Some packages are already installed and will be skipped:");
        if packages_to_install.is_empty() {
            println!("No packages to install");
            return Ok(());
        }
        self.install_pkgkeys(packages_to_install)
    }

    pub fn install_pkgkeys(&mut self, mut packages_to_install: HashMap<String, InstalledPackageInfo>) -> Result<()> {
        if config().common.verbose {
            println!("Packages to install:");
            print_packages_by_depend_depth(&packages_to_install);
        }
        if config().common.simulate {
            return Ok(());
        }
        let files = self.download_packages(&packages_to_install, false)?;
        let pkgidlines = crate::store::unpack_packages(files)?;
        for pkgidline in pkgidlines {
            let (id, line) = pkgidline.split_once("__")
                .ok_or_else(|| eyre!("Invalid package line format: {}", pkgidline))?;
            let pkgline = package::parse_pkgline(line)
                .map_err(|e| eyre!("Failed to parse package line: {}", e))?;
            let pkgkey = package::format_pkgkey(&pkgline.pkgname, id);
            packages_to_install.get_mut(&pkgkey)
                .ok_or_else(|| eyre!("Package key not found: {}", pkgkey))?
                .pkgline = line.to_string();
        }

        self.change_appbin_flag_same_source(&mut packages_to_install)?;
        let new_generation = self.create_new_generation()?;

        let mut appbin_count = 0;
        let mut appbin_packages = Vec::new();
        let env_root = self.get_default_env_root()?.clone();
        let store_root = dirs().epkg_store.clone();

        // First phase: Link all packages
        for (pkgkey, package_info) in &packages_to_install {
            let store_fs_dir = store_root.join(package_info.pkgline.clone()).join("fs");
            self.link_package(&store_fs_dir, &env_root)
                .with_context(|| format!("Failed to link package {}", pkgkey))?;
        }

        // Second phase: Expose packages and handle appbin flags
        for (pkgkey, package_info) in &packages_to_install {
            let appbin_flag = package_info.appbin_flag;
            if appbin_flag {
                appbin_count += 1;
                appbin_packages.push(pkgkey.clone());
                let store_fs_dir = store_root.join(package_info.pkgline.clone()).join("fs");
                self.expose_package(&store_fs_dir, &env_root)
                    .with_context(|| format!("Failed to expose package {}", pkgkey))?;
            }
        }

        // Save installed packages
        self.installed_packages.extend(packages_to_install.clone());
        self.save_installed_packages(&new_generation)?;
        self.record_history(&new_generation, "install", packages_to_install.keys().cloned().collect(), vec![])?;

        // Last step: update current symlink to point to the new generation
        self.update_current_generation_symlink(new_generation)?;

        println!("Installation successful - Total packages: {}, AppBin packages: {}", packages_to_install.len(), appbin_count);
        if !appbin_packages.is_empty() {
            println!("AppBin package list: {}", appbin_packages.join(", "));
        }

        Ok(())
    }

}
