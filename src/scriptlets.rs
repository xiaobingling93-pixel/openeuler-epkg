use color_eyre::eyre::{Result, eyre};
use crate::models::{InstalledPackageInfo, PackageFormat};
use crate::plan::InstallationPlan;
use crate::deb_triggers::setup_deb_env_vars;
use crate::rpm_triggers::setup_rpm_env_vars;
use crate::package;
use crate::run::{RunOptions, setup_namespace_and_mounts};
use nix::unistd::{fork, ForkResult};
use nix::sys::wait::{waitpid, WaitStatus};
use crate::shebang::strip_shebang;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScriptletType {
    PreInstall,
    PostInstall,
    PreUpgrade,
    PostUpgrade,
    PreRemove,
    PostRemove,
    // Transaction scriptlets (RPM-specific)
    PreTrans,      // %pretrans - before transaction starts
    PostTrans,     // %posttrans - after transaction completes
    PreUnTrans,    // %preuntrans - before uninstall transaction starts
    PostUnTrans,   // %postuntrans - after uninstall transaction completes
}

impl ScriptletType {
    /// Get the script filenames for this scriptlet type
    fn get_script_names(&self, package_format: PackageFormat) -> Vec<String> {
        let name = match (self, package_format) {
            // For other scriptlet types, use direct mapping
            (ScriptletType::PreInstall, _)  => "pre_install",
            (ScriptletType::PostInstall, _) => "post_install",
            (ScriptletType::PreUpgrade, _)  => "pre_upgrade",
            (ScriptletType::PostUpgrade, _) => "post_upgrade",
            (ScriptletType::PreRemove, _)   => "pre_remove",
            (ScriptletType::PostRemove, _)  => "post_remove",

            // Transaction scriptlets (RPM-specific)
            // These use distinct filenames to avoid conflicts with regular upgrade scriptlets
            (ScriptletType::PreTrans, PackageFormat::Rpm) => "pre_trans",
            (ScriptletType::PostTrans, PackageFormat::Rpm) => "post_trans",
            (ScriptletType::PreUnTrans, PackageFormat::Rpm) => "pre_untrans",
            (ScriptletType::PostUnTrans, PackageFormat::Rpm) => "post_untrans",

            // Transaction scriptlets not supported for other formats
            (ScriptletType::PreTrans, _) | (ScriptletType::PostTrans, _) |
            (ScriptletType::PreUnTrans, _) | (ScriptletType::PostUnTrans, _) => return Vec::new(),
        };

        match package_format {
            PackageFormat::Rpm => vec![format!("{}.sh", name), format!("{}.lua", name)],
            _ => vec![format!("{}.sh", name)]
        }
    }

    /// Get the parameters to pass to the script based on package format and scenario
    /// For RPM, $1 represents the number of installed instances AFTER the operation completes.
    fn get_script_params(
        &self,
        package_format: PackageFormat,
        is_upgrade: bool,
        old_version: Option<&str>,
        new_version: Option<&str>,
    ) -> Vec<String> {
        match package_format {
            PackageFormat::Rpm => {
                self.get_rpm_script_params(is_upgrade)
            }
            PackageFormat::Deb => {
                self.get_deb_script_params(is_upgrade, old_version, new_version)
            }
            PackageFormat::Pacman | PackageFormat::Apk => {
                // Both Pacman and APK use the same scriptlet parameter format:
                // - pre_install/post_install: <new-version>
                // - pre_upgrade/post_upgrade: <new-version> <old-version>
                // - pre_remove/post_remove: <old-version>
                self.get_pacman_script_params(old_version, new_version)
            }
            PackageFormat::Conda => {
                // Conda scripts receive no command-line parameters
                // Only environment variables are set: PREFIX, PKG_NAME, PKG_VERSION, PKG_BUILDNUM
                // Reference: conda/core/link.py run_script() function
                vec![]
            }
            // For other formats, no parameters are typically used
            _ => vec![]
        }
    }

    /// Get script parameters for RPM format
    /// RPM scriptlets only accept $1 (package count), never $2
    /// $1 represents the number of instances of the package that will be installed AFTER the scriptlet completes
    fn get_rpm_script_params(
        &self,
        is_upgrade: bool,
    ) -> Vec<String> {
        // Calculate $1 based on scriptlet type and upgrade status
        // Based on RPM scriptlet execution order in rpm-scriptlets.7.scd and process_package_operation()
        let package_count = match self {
            ScriptletType::PreInstall => {
                // %pre of new: $1 = npkgs_installed + 1 (will be 1 after install)
                // For both fresh install and upgrade, the new package is not installed yet,
                // so npkgs_installed = 0, and after install it will be 1
                1
            }
            ScriptletType::PreUpgrade => {
                // %preupgrade: same as %pre for upgrades, $1 = npkgs_installed + 1 (will be 1 after install)
                // Note: This scriptlet type may not be used in current flow (PreInstall is used instead)
                1
            }
            ScriptletType::PostInstall => {
                // %post of new: new installed, old still installed (if upgrade)
                if is_upgrade { 2 } else { 1 }
            }
            ScriptletType::PostUpgrade => {
                // %postupgrade: same as %post for upgrades, both old and new installed
                2
            }

            ScriptletType::PreRemove => {
                // %preun of old: new installed, old still installed (if upgrade)
                if is_upgrade { 2 } else { 1 }
            }
            ScriptletType::PostRemove => {
                // %postun of old: new installed, old removed (if upgrade)
                if is_upgrade { 1 } else { 0 }
            }

            ScriptletType::PreTrans => {
                // %pretrans of new: $1 = npkgs_installed + 1 (will be 1 after install)
                // Same as PreInstall - for both fresh install and upgrade, the new package
                // is not installed yet, so npkgs_installed = 0, and after install it will be 1
                1
            }
            ScriptletType::PostTrans => {
                // %posttrans of new: new installed, old removed (if upgrade)
                1
            }

            ScriptletType::PreUnTrans => {
                // %preuntrans of old:
                // Upgrade: $1 = npkgs_installed (old version still installed) = 1
                // Removal: $1 = npkgs_installed - 1 (will be 0 after removal) = 0
                if is_upgrade { 1 } else { 0 }
            }
            ScriptletType::PostUnTrans => {
                // %postuntrans of old: old removed
                0
            }
        };
        vec![package_count.to_string()]
    }

    /// Get script parameters for DEB format
    fn get_deb_script_params(
        &self,
        is_upgrade: bool,
        old_version: Option<&str>,
        new_version: Option<&str>,
    ) -> Vec<String> {
        match self {
            ScriptletType::PreInstall => {
                if is_upgrade {
                    let mut params = vec!["upgrade".to_string()];
                    if let Some(old_ver) = old_version {
                        params.push(old_ver.to_string()); // $2=old_version
                    }
                    if let Some(new_ver) = new_version {
                        params.push(new_ver.to_string()); // $3=new_version
                    }
                    params
                } else {
                    vec!["install".to_string()]
                }
            }
            ScriptletType::PostInstall => {
                if is_upgrade {
                    let mut params = vec!["configure".to_string()];
                    if let Some(old_ver) = old_version {
                        params.push(old_ver.to_string()); // $2=old_version
                    }
                    params
                } else {
                    vec!["configure".to_string()]
                }
            }
            ScriptletType::PreUpgrade => {
                // For DEB, PreUpgrade maps to pre_install.sh with upgrade parameters
                let mut params = vec!["upgrade".to_string()];
                if let Some(old_ver) = old_version {
                    params.push(old_ver.to_string()); // $2=old_version
                }
                params
            }
            ScriptletType::PostUpgrade => {
                // For DEB, PostUpgrade maps to post_install.sh with configure parameters
                let mut params = vec!["configure".to_string()];
                if let Some(old_ver) = old_version {
                    params.push(old_ver.to_string()); // $2=old_version
                }
                params
            }
            ScriptletType::PreRemove => {
                if is_upgrade {
                    let mut params = vec!["upgrade".to_string()];
                    if let Some(new_ver) = new_version {
                        params.push(new_ver.to_string()); // $2=new_version
                    }
                    params
                } else {
                    vec!["remove".to_string()]
                }
            }
            ScriptletType::PostRemove => {
                if is_upgrade {
                    let mut params = vec!["upgrade".to_string()];
                    if let Some(new_ver) = new_version {
                        params.push(new_ver.to_string()); // $2=new_version
                    }
                    params
                } else {
                    vec!["remove".to_string()]
                }
            }
            // Transaction scriptlets not supported for DEB
            ScriptletType::PreTrans | ScriptletType::PostTrans |
            ScriptletType::PreUnTrans | ScriptletType::PostUnTrans => {
                Vec::new()
            }
        }
    }

    /// Get script parameters for Pacman (Arch Linux) and APK (Alpine Linux) formats
    /// According to:
    /// - Pacman: https://man.archlinux.org/man/PKGBUILD.5#INSTALL/UPGRADE/REMOVE_SCRIPTING
    /// - APK: https://wiki.alpinelinux.org/wiki/APKBUILD_Reference and apk-package.5.scd
    /// Both formats use the same parameter format:
    /// - pre_install: one argument (new package full version string)
    /// - post_install: one argument (new package full version string)
    /// - pre_upgrade: two arguments (new package full version string, old package full version string)
    /// - post_upgrade: two arguments (new package full version string, old package full version string)
    /// - pre_remove/pre_deinstall: one argument (old package full version string)
    /// - post_remove/post_deinstall: one argument (old package full version string)
    /// Note: During upgrade operations, only pre_upgrade and post_upgrade are called, not install/remove functions.
    /// Reference code:
    /// - Pacman: _alpm_runscriptlet() calls and proto.install template in http://gitlab.archlinux.org/pacman/pacman
    /// - APK: apk_ipkg_run_script() in /c/package-managers/apk-tools/src/package.c
    fn get_pacman_script_params(
        &self,
        old_version: Option<&str>,
        new_version: Option<&str>,
    ) -> Vec<String> {
        match self {
            ScriptletType::PreInstall => {
                // pre_install: one argument (new package full version string)
                if let Some(new_ver) = new_version {
                    vec![new_ver.to_string()]
                } else {
                    vec![]
                }
            }
            ScriptletType::PostInstall => {
                // post_install: one argument (new package full version string)
                if let Some(new_ver) = new_version {
                    vec![new_ver.to_string()]
                } else {
                    vec![]
                }
            }
            ScriptletType::PreUpgrade => {
                // pre_upgrade: two arguments (new package full version string, old package full version string)
                let mut params = Vec::new();
                if let Some(new_ver) = new_version {
                    params.push(new_ver.to_string());
                }
                if let Some(old_ver) = old_version {
                    params.push(old_ver.to_string());
                }
                params
            }
            ScriptletType::PostUpgrade => {
                // post_upgrade: two arguments (new package full version string, old package full version string)
                let mut params = Vec::new();
                if let Some(new_ver) = new_version {
                    params.push(new_ver.to_string());
                }
                if let Some(old_ver) = old_version {
                    params.push(old_ver.to_string());
                }
                params
            }
            ScriptletType::PreRemove => {
                // pre_remove: one argument (old package full version string)
                if let Some(old_ver) = old_version {
                    vec![old_ver.to_string()]
                } else {
                    vec![]
                }
            }
            ScriptletType::PostRemove => {
                // post_remove: one argument (old package full version string)
                if let Some(old_ver) = old_version {
                    vec![old_ver.to_string()]
                } else {
                    vec![]
                }
            }
            // Transaction scriptlets not supported for Pacman
            ScriptletType::PreTrans | ScriptletType::PostTrans |
            ScriptletType::PreUnTrans | ScriptletType::PostUnTrans => {
                Vec::new()
            }
        }
    }
}

/// Set up APK (Alpine Linux) environment variables for scriptlets
/// According to https://wiki.alpinelinux.org/wiki/APKBUILD_Reference and apk.8.scd:
/// - APK_PACKAGE: Package name (package scripts only)
/// - APK_SCRIPT: Set to one of the package script types
/// Reference: apk_ipkg_run_script() and apk_script_types[] in /c/package-managers/apk-tools/src/package.c
pub fn setup_apk_env_vars(
    env_vars: &mut std::collections::HashMap<String, String>,
    pkgkey: &str,
    _package_info: &InstalledPackageInfo,
    scriptlet_type: ScriptletType,
) {
    use crate::package::pkgkey2pkgname;

    // Set APK_SCRIPT to the script type name
    // APK script types: pre-install, post-install, pre-upgrade, post-upgrade, pre-deinstall, post-deinstall
    let script_type = match scriptlet_type {
        ScriptletType::PreInstall   => "pre-install",
        ScriptletType::PostInstall  => "post-install",
        ScriptletType::PreUpgrade   => "pre-upgrade",
        ScriptletType::PostUpgrade  => "post-upgrade",
        ScriptletType::PreRemove    => "pre-deinstall",
        ScriptletType::PostRemove   => "post-deinstall",
        _ => {
            // Transaction scriptlets not used for APK
            return;
        }
    };
    env_vars.insert("APK_SCRIPT".to_string(), script_type.to_string());

    // Set APK_PACKAGE to package name
    // Reference: apk sets this for package scripts (not commit scripts)
    if let Ok(package_name) = pkgkey2pkgname(pkgkey) {
        env_vars.insert("APK_PACKAGE".to_string(), package_name);
    }
}

/// Set up Conda environment variables for scriptlets
/// According to conda documentation:
/// - PREFIX: The install prefix (environment root)
/// - PKG_NAME: The name of the package
/// - PKG_VERSION: The version of the package (without build string)
/// - PKG_BUILDNUM: The build number of the package
/// Reference: conda/core/link.py and rattler/src/install/link_script.rs
pub fn setup_conda_env_vars(
    env_vars: &mut std::collections::HashMap<String, String>,
    pkgkey: &str,
    package_info: &InstalledPackageInfo,
    store_root: &std::path::Path,
    env_root: &std::path::Path,
) {
    use crate::package::{pkgkey2pkgname, pkgkey2version};
    use crate::conda_pkg::VERSION_BUILD_SEPARATOR;
    use std::fs;

    // Set PREFIX to the environment root (install prefix)
    env_vars.insert("PREFIX".to_string(), env_root.to_string_lossy().to_string());

    // Set PKG_NAME to package name
    if let Ok(package_name) = pkgkey2pkgname(pkgkey) {
        env_vars.insert("PKG_NAME".to_string(), package_name);
    }

    // Extract version from pkgkey (may include build string separated by VERSION_BUILD_SEPARATOR)
    let version_with_build = pkgkey2version(pkgkey).unwrap_or_default();
    // Split version and build string (version is the part before the separator)
    let pkg_version = version_with_build
        .splitn(2, VERSION_BUILD_SEPARATOR)
        .next()
        .unwrap_or(&version_with_build)
        .to_string();
    env_vars.insert("PKG_VERSION".to_string(), pkg_version);

    // Try to read PKG_BUILDNUM from package.txt
    // Build number is stored as "buildNumber" in package.txt
    let mut build_num = "0".to_string(); // Default to "0" if not found
    let package_txt_path = store_root.join(&package_info.pkgline).join("info/package.txt");
    if package_txt_path.exists() {
        if let Ok(content) = fs::read_to_string(&package_txt_path) {
            for line in content.lines() {
                if let Some((key, value)) = line.split_once(": ") {
                    if key == "buildNumber" {
                        build_num = value.to_string();
                        break;
                    }
                }
            }
        }
    }
    env_vars.insert("PKG_BUILDNUM".to_string(), build_num);
}

/// Special marker for embedded Lua interpreter
const EMBEDDED_LUA: &str = "<embedded-lua>";

/// Resolve a symlink path to its target within the environment context
/// This properly handles symlinks that point to absolute paths by checking if the target exists
/// within the environment root, not on the host system.
///
/// Returns Some(target_path) where target_path is the resolved absolute path within the environment
/// that actually contains the executable, or None if the path is invalid or the target doesn't exist.
///
/// Examples:
/// - Regular file:     ~/.epkg/envs/alpine/usr/bin/bash exists         -> Some(~/.epkg/envs/alpine/usr/bin/bash)
/// - Absolute symlink: ~/.epkg/envs/alpine/usr/bin/sh -> /usr/bin/bash -> Some(~/.epkg/envs/alpine/usr/bin/bash)
/// - Relative symlink: ~/.epkg/envs/alpine/usr/bin/sh -> bash          -> Some(~/.epkg/envs/alpine/usr/bin/bash)
/// - Invalid symlink: symlink points to non-existent target -> None
pub fn resolve_symlink_in_env(symlink_path: &std::path::Path, env_root: &std::path::Path) -> Option<std::path::PathBuf> {
    // First check if the symlink file itself exists (as a regular file or symlink)
    if symlink_path.exists() && !symlink_path.is_symlink() {
        // It's a regular file, not a symlink
        // Example: ~/.epkg/envs/alpine/usr/bin/bash is a regular executable file
        // Return: Some(~/.epkg/envs/alpine/usr/bin/bash) - the resolved target path (same as input)
        return Some(symlink_path.to_path_buf());
    }

    // If it's a symlink, read the target and check if the target exists within the environment
    if let Ok(link_target) = std::fs::read_link(symlink_path) {
        if link_target.is_absolute() {
            // Absolute symlink: check if env_root + target exists
            // This avoids checking host system paths that might coincidentally exist
            // Example: ~/.epkg/envs/alpine/usr/bin/sh -> /usr/bin/bash -> Some(~/.epkg/envs/alpine/usr/bin/bash)
            let target_in_env = env_root.join(link_target.strip_prefix("/").unwrap_or(&link_target));
            if target_in_env.exists() {
                return Some(target_in_env);
            }
        } else {
            // Relative symlink: resolve relative to the symlink's directory
            // Example: ~/.epkg/envs/alpine/usr/bin/sh -> bash -> Some(~/.epkg/envs/alpine/usr/bin/bash)
            let symlink_dir = symlink_path.parent()?;
            let resolved_path = symlink_dir.join(&link_target);
            if resolved_path.exists() {
                return Some(resolved_path);
            }
        }
    }

    // Return: None - symlink_path doesn't exist on host, symlink target doesn't exist in environment, or symlink couldn't be read
    None
}

/// Get interpreters to try for a given script file extension
/// For .lua files, try embedded Lua first, then external lua interpreter
pub fn get_interpreters_for_script(script_name: &str) -> Vec<&'static str> {
    if script_name.ends_with(".sh") {
        vec!["bash", "sh"]
    } else if script_name.ends_with(".lua") {
        vec![EMBEDDED_LUA, "lua"]  // Try embedded Lua first
    } else if script_name.ends_with(".py") {
        vec!["python3", "python"]
    } else if script_name.ends_with(".pl") {
        vec!["perl"]
    } else {
        // Default to shell interpreters for unknown extensions
        vec!["bash", "sh"]
    }
}

/// Fork and execute a closure in the child process.
/// Returns Ok(()) if child exits successfully, otherwise returns an error.
fn fork_and_call<F>(f: F) -> Result<()>
where
    F: FnOnce() -> Result<()>,
{
    match unsafe { fork() } {
        Ok(ForkResult::Parent { child }) => {
            match waitpid(child, None) {
                Ok(WaitStatus::Exited(_, 0)) => Ok(()),
                Ok(WaitStatus::Exited(_, code)) => Err(eyre!("child exited with code {}", code)),
                Ok(WaitStatus::Signaled(_, signal, _)) => Err(eyre!("child killed by signal {:?}", signal)),
                Ok(_) => Err(eyre!("child ended with unexpected status")),
                Err(e) => Err(eyre!("failed to wait for child: {}", e)),
            }
        }
        Ok(ForkResult::Child) => {
            match f() {
                Ok(()) => std::process::exit(0),
                Err(_) => std::process::exit(1),
            }
        }
        Err(e) => Err(eyre!("fork failed: {}", e)),
    }
}

/// Execute Lua scriptlet using embedded Lua interpreter.
/// Uses the global cached Lua state with extensions pre-registered.
fn execute_lua_scriptlet(
    script_path: &std::path::Path,
    args: &[String],
    env_root: &std::path::Path,
) -> Result<()> {
    // Get the global cached Lua state (with extensions pre-registered)
    let lua = crate::lua::get_cached_lua_state();

    // Read script content
    let script_content = std::fs::read_to_string(script_path)
        .map_err(|e| eyre!("Failed to read Lua scriptlet {}: {}", script_path.display(), e))?;
    let stripped_content = strip_shebang(&script_content);

    // Setup scriptlet environment (arg table) - this changes per scriptlet
    crate::lua::setup_arg_table(&lua, args)
        .map_err(|e| eyre!("Failed to setup scriptlet environment: {}", e))?;

    // Change working directory to env_root
    std::env::set_current_dir(env_root)
        .map_err(|e| eyre!("Failed to change directory to {}: {}", env_root.display(), e))?;

    // Execute the script
    let script_name = script_path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("<lua>");

    lua.load(stripped_content)
        .set_name(script_name)
        .exec()
        .map_err(|e| eyre!("Lua scriptlet execution failed: {}", e))?;

    Ok(())
}

/// Run transaction scriptlets for multiple packages
/// Iterates over plan.ordered_operations and runs transaction scriptlets for suitable packages
/// based on scriptlet_type, with per-package is_upgrade determination.
/// Only handles transaction scriptlets: PreTrans, PostTrans, PreUnTrans, PostUnTrans
pub fn run_trans_scriptlets(
    plan: &InstallationPlan,
    scriptlet_type: ScriptletType,
) -> Result<()> {
    for op in &plan.ordered_operations {
        match scriptlet_type {
            // Transaction scriptlets for new packages (fresh installs and upgrades)
            ScriptletType::PreTrans | ScriptletType::PostTrans => {
                if let Some(new_pkgkey) = &op.new_pkgkey {
                    if !plan.batch.new_pkgkeys.contains(new_pkgkey) {
                        continue;
                    }
                    if let Some(new_pkg_info) = crate::plan::pkgkey2new_pkg_info(plan, new_pkgkey) {
                        run_scriptlet(plan, scriptlet_type, new_pkgkey, new_pkg_info.as_ref(), op.old_pkgkey.as_deref())?;
                    }
                }
            }
            // Transaction scriptlets for old packages being removed
            ScriptletType::PreUnTrans | ScriptletType::PostUnTrans => {
                if let Some(old_pkgkey) = &op.old_pkgkey {
                    if !plan.batch.old_removes.contains(old_pkgkey) &&
                       !plan.batch.upgrades_old.contains(old_pkgkey) {
                        continue;
                    }
                    if let Some(old_pkg_info) = crate::plan::pkgkey2installed_pkg_info(old_pkgkey) {
                        run_scriptlet(plan, scriptlet_type, old_pkgkey, old_pkg_info.as_ref(), op.new_pkgkey.as_deref())?;
                    }
                }
            }
            // Other scriptlet types should not be called through this function
            _ => {
                return Err(eyre!(
                    "run_trans_scriptlets() called with non-transaction scriptlet type: {:?}",
                    scriptlet_type
                ));
            }
        }
    }
    Ok(())
}

/// Run a single scriptlet for one package
pub fn run_scriptlet(
    plan: &InstallationPlan,
    scriptlet_type: ScriptletType,
    pkgkey: &str,
    package_info: &InstalledPackageInfo,
    old_pkgkey: Option<&str>,
) -> Result<()> {
    let store_root = &plan.store_root;
    let env_root = &plan.env_root;
    let package_format = plan.package_format;
    // Extract versions from pkgkeys
    let old_version = old_pkgkey.and_then(|k| package::pkgkey2version(k).ok());
    let new_version = package::pkgkey2version(pkgkey).ok();
    // Calculate is_upgrade: both old_pkgkey and new_version must be Some for an upgrade
    let is_upgrade = old_pkgkey.is_some() && new_version.is_some();
    // Skip all fakeroot scriptlets as post_install runs ldconfig -r . which removes ld-linux-x86-64.so.2
    let pkgname = crate::package::pkgkey2pkgname(pkgkey).unwrap_or_default();
    if pkgname == "fakeroot" {
        log::info!(
            "Skipping {:?} scriptlet for package {} (fakeroot scriptlets run ldconfig -r . which removes critical system files)",
            scriptlet_type,
            pkgkey
        );
        return Ok(());
    }

    let script_base_path = store_root.join(&package_info.pkgline).join("info/install");

    // Get the script names to try for this scriptlet type
    let script_names = scriptlet_type.get_script_names(package_format);

    for script_name in &script_names {
        let script_path = script_base_path.join(script_name);
        if script_path.exists() {
            log::info!(
                "Running {:?} scriptlet for package {}: {}",
                scriptlet_type,
                pkgkey,
                script_path.display()
            );

            // Get interpreters to try for this script
            let interpreters = get_interpreters_for_script(script_name);
            let mut script_executed = false;

            // Get parameters based on package format and scenario
            let params = scriptlet_type.get_script_params(package_format, is_upgrade, old_version.as_deref(), new_version.as_deref());

            for interpreter in interpreters {
                // Check if this is embedded Lua
                if interpreter == EMBEDDED_LUA {
                    // Prepare script arguments for Lua (1-indexed: arg[1], arg[2], ...)
                    // arg[1] is typically empty or scriptlet name
                    // arg[2] is the first parameter (package count for RPM)
                    let mut lua_args = vec!["".to_string()]; // arg[1] - usually empty
                    lua_args.extend(params.clone());

                    log::debug!(
                        "Trying embedded Lua interpreter for scriptlet {}",
                        script_path.display()
                    );

                    // Run embedded Lua inside a forked child with namespace + mount setup,
                    // so that operations like ldconfig affect the environment, not the host.
                    let result = fork_and_call(|| {
                        let run_options = RunOptions {
                            chdir_to_env_root: true,
                            ..Default::default()
                        };
                        setup_namespace_and_mounts(&env_root, &run_options)?;
                        execute_lua_scriptlet(&script_path, &lua_args, &env_root)
                    });

                    match result {
                        Ok(()) => {
                            log::debug!(
                                "{:?} scriptlet completed successfully for package {} using embedded Lua",
                                scriptlet_type,
                                pkgkey
                            );
                            script_executed = true;
                            break; // Successfully executed
                        }
                        Err(e) => {
                            log::debug!(
                                "Failed to execute {:?} scriptlet for package {} using embedded Lua: {}, trying next interpreter",
                                scriptlet_type,
                                pkgkey,
                                e
                            );
                            continue; // Try external lua interpreter
                        }
                    }
                }

                // Scriptlets run in namespace isolation, so only environment paths are accessible.
                // System paths (/usr/bin/*) are not available since we're in a chroot environment.
                // We validate symlinks properly to handle cases where environment symlinks point to valid targets.
                let interpreter_path = env_root.join("usr/bin").join(interpreter);
                if resolve_symlink_in_env(&interpreter_path, env_root).is_none() {
                    log::debug!(
                        "Interpreter {} not found in environment, trying next interpreter",
                        interpreter
                    );
                    continue;
                }

                // Prepare script arguments: [script_path, param1, param2, ...]
                let mut script_args = vec![script_path.to_string_lossy().to_string()];
                script_args.extend(params.clone());

                // Create RunOptions for scriptlet execution with namespace isolation
                // Set up environment variables required by package scripts
                let mut env_vars = std::collections::HashMap::new();

                // Add environment variables for package scripts based on format
                if package_format == PackageFormat::Deb {
                    setup_deb_env_vars(&mut env_vars, pkgkey, package_info, scriptlet_type, env_root);
                } else if package_format == PackageFormat::Rpm {
                    setup_rpm_env_vars(&mut env_vars, pkgkey, package_info, store_root);
                } else if package_format == PackageFormat::Apk {
                    setup_apk_env_vars(&mut env_vars, pkgkey, package_info, scriptlet_type);
                } else if package_format == PackageFormat::Conda {
                    setup_conda_env_vars(&mut env_vars, pkgkey, package_info, store_root, env_root);
                }

                let run_options = crate::run::RunOptions {
                    command: interpreter_path.to_string_lossy().to_string(),
                    args: script_args,
                    env_vars,
                    no_exit: true,           // Don't exit on scriptlet failures, just warn
                    chdir_to_env_root: true, // Scriptlets should run relative to environment root
                    timeout: 60,             // 60 second timeout for scriptlets
                    ..Default::default()
                };

                // Execute the scriptlet using fork_and_execute for namespace isolation
                match crate::run::fork_and_execute(env_root, &run_options) {
                    Ok(None) => {
                        log::debug!(
                            "{:?} scriptlet completed successfully for package {} using {}",
                            scriptlet_type,
                            pkgkey,
                            interpreter
                        );
                        script_executed = true;
                        break; // Successfully executed, break out of paths loop
                    }
                    Ok(Some(_)) => {
                        unreachable!("Foreground process should not return PID")
                    }
                    Err(e) => {
                        // Check if this is a diversion conflict or other known recoverable error
                        let error_msg = format!("{}", e);
                        if error_msg.contains("dpkg-divert") && error_msg.contains("clashes") {
                            log::warn!(
                                "Diversion conflict in {:?} scriptlet for package {}: {}. Continuing installation.",
                                scriptlet_type,
                                pkgkey,
                                e
                            );
                            script_executed = true;
                            break; // Treat diversion conflicts as non-fatal
                        } else if should_ignore_scriptlet_error(&error_msg, scriptlet_type) {
                            log::warn!(
                                "Ignoring recoverable error in {:?} scriptlet for package {}: {}",
                                scriptlet_type,
                                pkgkey,
                                e
                            );
                            script_executed = true;
                            break;
                        } else {
                            log::debug!(
                                "Failed to execute {:?} scriptlet for package {} using {}: {}, trying next interpreter",
                                scriptlet_type,
                                pkgkey,
                                interpreter,
                                e
                            );
                            continue; // Try next interpreter
                        }
                    }
                }
            }

            if !script_executed {
                log::warn!(
                    "No suitable interpreter found for {:?} scriptlet {} for package {}",
                    scriptlet_type,
                    script_name,
                    pkgkey
                );
            } else {
                // Successfully executed a scriptlet, return early
                return Ok(());
            }
        }
    }

    Ok(())
}

/// Check if a scriptlet error should be ignored to allow installation to continue
fn should_ignore_scriptlet_error(error_msg: &str, scriptlet_type: ScriptletType) -> bool {
    // Known patterns of recoverable errors
    let recoverable_patterns = [
        "dpkg-divert: error: ",
        // Add more patterns as needed
    ];

    // Only ignore certain errors in postinst scripts to be conservative
    if matches!(scriptlet_type, ScriptletType::PostInstall | ScriptletType::PostUpgrade) {
        for pattern in &recoverable_patterns {
            if error_msg.contains(pattern) {
                return true;
            }
        }
    }

    false
}

