use std::path::Path;
use color_eyre::eyre::Result;
use crate::models::{InstalledPackageInfo, PackageFormat};
use std::collections::HashMap;
use crate::deb_triggers::setup_deb_env_vars;
use crate::rpm_triggers::{setup_rpm_env_vars, count_installed_packages_by_name};

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
            // For RPM and DEB, upgrade scriptlets reuse install/remove scripts
            (ScriptletType::PreUpgrade, PackageFormat::Rpm) | (ScriptletType::PreUpgrade, PackageFormat::Deb) => "pre_install",
            (ScriptletType::PostUpgrade, PackageFormat::Rpm) | (ScriptletType::PostUpgrade, PackageFormat::Deb) => "post_install",

            // For other scriptlet types, use direct mapping
            (ScriptletType::PreInstall, _) => "pre_install",
            (ScriptletType::PostInstall, _) => "post_install",
            (ScriptletType::PreUpgrade, _) => "pre_upgrade",
            (ScriptletType::PostUpgrade, _) => "post_upgrade",
            (ScriptletType::PreRemove, _) => "pre_remove",
            (ScriptletType::PostRemove, _) => "post_remove",

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
    /// This function now accepts an optional package_count parameter for accurate calculation.
    fn get_script_params(
        &self,
        package_format: PackageFormat,
        is_upgrade: bool,
        old_version: Option<&str>,
        new_version: Option<&str>,
        package_count: Option<u32>, // Number of installed instances BEFORE operation
    ) -> Vec<String> {
        match package_format {
            PackageFormat::Rpm => {
                match self {
                    ScriptletType::PreInstall | ScriptletType::PreTrans => {
                        // PKG_INSTALL/PKG_PRETRANS: $1 = npkgs_installed + 1 (will be 1 after install)
                        let arg1 = package_count.map(|c| c + 1).unwrap_or(1);
                        let mut params = vec![arg1.to_string()];
                        if is_upgrade {
                            if let Some(old_ver) = old_version {
                                params.push(old_ver.to_string()); // $2=old_version
                            }
                        }
                        params
                    }
                    ScriptletType::PostInstall | ScriptletType::PostTrans => {
                        // PKG_POSTTRANS: $1 = npkgs_installed + isUpdate (1 if upgrade, 0 if fresh install)
                        // For fresh install: npkgs_installed + 1 = 1
                        // For upgrade: npkgs_installed + 1 = 1 (old removed, new installed, net +1)
                        let arg1 = if is_upgrade {
                            package_count.map(|c| c + 1).unwrap_or(1) // Upgrade: count stays same but we add 1
                        } else {
                            package_count.map(|c| c + 1).unwrap_or(1) // Fresh install: 0 + 1 = 1
                        };
                        vec![arg1.to_string()]
                    }
                    ScriptletType::PreUpgrade => {
                        // For RPM, PreUpgrade maps to pre_install.sh with upgrade parameters
                        // Same as PreInstall for upgrades
                        let arg1 = package_count.map(|c| c + 1).unwrap_or(1);
                        let mut params = vec![arg1.to_string()];
                        if let Some(old_ver) = old_version {
                            params.push(old_ver.to_string()); // $2=old_version
                        }
                        params
                    }
                    ScriptletType::PostUpgrade => {
                        // Same as PostInstall for upgrades
                        let arg1 = package_count.map(|c| c + 1).unwrap_or(1);
                        vec![arg1.to_string()]
                    }
                    ScriptletType::PreRemove | ScriptletType::PreUnTrans => {
                        // PKG_ERASE/PKG_PREUNTRANS:
                        // If upgrade (rpmteDependsOn): $1 = npkgs_installed (old version still installed)
                        // If removal: $1 = npkgs_installed - 1 (will be 0 after removal)
                        let arg1 = if is_upgrade {
                            package_count.unwrap_or(1) // Upgrade: old version count
                        } else {
                            package_count.map(|c| c.saturating_sub(1)).unwrap_or(0) // Removal: will be 0
                        };
                        let mut params = vec![arg1.to_string()];
                        if is_upgrade {
                            if let Some(new_ver) = new_version {
                                params.push(new_ver.to_string()); // $2=new_version
                            }
                        }
                        params
                    }
                    ScriptletType::PostRemove | ScriptletType::PostUnTrans => {
                        // PKG_POSTUNTRANS: $1 = npkgs_installed (after removal, will be 0 for complete removal)
                        let arg1 = if is_upgrade {
                            package_count.map(|c| c.saturating_sub(1)).unwrap_or(0) // Upgrade: old removed, new installed
                        } else {
                            0 // Complete removal: will be 0
                        };
                        let mut params = vec![arg1.to_string()];
                        if is_upgrade {
                            if let Some(new_ver) = new_version {
                                params.push(new_ver.to_string()); // $2=new_version
                            }
                        }
                        params
                    }
                }
            }
            PackageFormat::Deb => {
                match self {
                    ScriptletType::PreInstall => {
                        if is_upgrade {
                            let mut params = vec!["upgrade".to_string()];
                            if let Some(old_ver) = old_version {
                                params.push(old_ver.to_string()); // $2=old_version
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
            // For other formats (Arch, Alpine, etc.), no parameters are typically used
            _ => vec![]
        }
    }
}

/// Get interpreters to try for a given script file extension
pub fn get_interpreters_for_script(script_name: &str) -> Vec<&'static str> {
    if script_name.ends_with(".sh") {
        vec!["bash", "sh"]
    } else if script_name.ends_with(".lua") {
        vec!["lua"]
    } else if script_name.ends_with(".py") {
        vec!["python3", "python"]
    } else if script_name.ends_with(".pl") {
        vec!["perl"]
    } else {
        // Default to shell interpreters for unknown extensions
        vec!["bash", "sh"]
    }
}

/// Run scriptlets for multiple packages
pub fn run_scriptlets(
    completed_packages: &HashMap<String, InstalledPackageInfo>,
    store_root: &Path,
    env_root: &Path,
    package_format: PackageFormat,
    scriptlet_type: ScriptletType,
    is_upgrade: bool,
) -> Result<()> {
    run_scriptlets_with_context(
        completed_packages,
        store_root,
        env_root,
        package_format,
        scriptlet_type,
        is_upgrade,
        None, // installed_packages
        None, // fresh_installs
        None, // old_removes
    )
}

/// Run scriptlets for multiple packages with package count context
pub fn run_scriptlets_with_context(
    completed_packages: &HashMap<String, InstalledPackageInfo>,
    store_root: &Path,
    env_root: &Path,
    package_format: PackageFormat,
    scriptlet_type: ScriptletType,
    is_upgrade: bool,
    installed_packages: Option<&HashMap<String, InstalledPackageInfo>>,
    fresh_installs: Option<&HashMap<String, InstalledPackageInfo>>,
    old_removes: Option<&HashMap<String, InstalledPackageInfo>>,
) -> Result<()> {
    // Convert HashMap to a Vec of tuples (pkgkey, info) and sort by depend_depth in descending order
    // This ensures packages with higher depend_depth are processed first
    let mut packages_vec: Vec<(&String, &InstalledPackageInfo)> = completed_packages.iter().collect();
    packages_vec.sort_by(|a, b| b.1.depend_depth.cmp(&a.1.depend_depth));

    // Process packages in sorted order
    for (pkgkey, package_info) in packages_vec {
        // Calculate package count for RPM scriptlet arguments
        let package_count = if package_format == PackageFormat::Rpm {
            if let (Some(installed), Some(fresh), Some(old)) = (installed_packages, fresh_installs, old_removes) {
                if let Ok(pkgname) = crate::package::pkgkey2pkgname(pkgkey) {
                    Some(count_installed_packages_by_name(
                        &pkgname,
                        installed,
                        fresh,
                        old,
                    ))
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        if let Err(e) = run_scriptlet_with_count(
            pkgkey,
            package_info,
            store_root,
            env_root,
            package_format,
            scriptlet_type,
            is_upgrade,
            None, // old_version
            None, // new_version
            package_count,
        ) {
            log::warn!("Failed to run {:?} scriptlet for package {}: {}", scriptlet_type, pkgkey, e);
        }
    }
    Ok(())
}

/// Run a single scriptlet for one package
pub fn run_scriptlet(
    pkgkey: &str,
    package_info: &InstalledPackageInfo,
    store_root: &Path,
    env_root: &Path,
    package_format: PackageFormat,
    scriptlet_type: ScriptletType,
    is_upgrade: bool,
    old_version: Option<&str>,
    new_version: Option<&str>,
) -> Result<()> {
    run_scriptlet_with_count(
        pkgkey,
        package_info,
        store_root,
        env_root,
        package_format,
        scriptlet_type,
        is_upgrade,
        old_version,
        new_version,
        None, // package_count - will use fallback
    )
}

/// Run a single scriptlet for one package with package count
pub fn run_scriptlet_with_count(
    pkgkey: &str,
    package_info: &InstalledPackageInfo,
    store_root: &Path,
    env_root: &Path,
    package_format: PackageFormat,
    scriptlet_type: ScriptletType,
    is_upgrade: bool,
    old_version: Option<&str>,
    new_version: Option<&str>,
    package_count: Option<u32>,
) -> Result<()> {
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

            for interpreter in interpreters {
                let interpreter_path = env_root.join("usr/bin").join(interpreter);

                // Check if interpreter exists
                if !interpreter_path.exists() {
                    log::debug!(
                        "Interpreter {} not found for scriptlet {}, trying next interpreter",
                        interpreter_path.display(),
                        script_path.display()
                    );
                    continue;
                }

                // Get parameters based on package format and scenario
                let params = scriptlet_type.get_script_params(package_format, is_upgrade, old_version, new_version, package_count);

                // Prepare script arguments: [script_path, param1, param2, ...]
                let mut script_args = vec![script_path.to_string_lossy().to_string()];
                script_args.extend(params);

                // Create RunOptions for scriptlet execution with namespace isolation
                // Set up environment variables required by package scripts
                let mut env_vars = std::collections::HashMap::new();

                // Add environment variables for package scripts based on format
                if package_format == PackageFormat::Deb {
                    setup_deb_env_vars(&mut env_vars, pkgkey, package_info, scriptlet_type, env_root);
                } else if package_format == PackageFormat::Rpm {
                    setup_rpm_env_vars(&mut env_vars, pkgkey, package_info, store_root);
                }

                let run_options = crate::run::RunOptions {
                    command: interpreter.to_string(),
                    args: script_args,
                    env_vars,
                    no_exit: true,           // Don't exit on scriptlet failures, just warn
                    chdir_to_env_root: true, // Scriptlets should run relative to environment root
                    timeout: 60,             // 60 second timeout for scriptlets
                    ..Default::default()
                };

                // Execute the scriptlet using fork_and_execute for namespace isolation
                match crate::run::fork_and_execute(env_root, &run_options, &interpreter_path) {
                    Ok(()) => {
                        log::debug!(
                            "{:?} scriptlet completed successfully for package {} using {}",
                            scriptlet_type,
                            pkgkey,
                            interpreter
                        );
                        script_executed = true;
                        break; // Successfully executed, no need to try other interpreters
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

