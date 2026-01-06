use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::collections::HashMap;
use color_eyre::Result;
use color_eyre::eyre::Context;
use crate::models::InstalledPackageInfo;
use crate::package::pkgkey2pkgname;

// Constants matching dpkg's structure
pub const TRIGGERSDIR: &str = "var/lib/dpkg/triggers";
pub const TRIGGERSDEFERREDFILE: &str = "Unincorp";

/// Get the triggers directory path in the environment
fn get_triggers_dir(env_root: &Path) -> PathBuf {
    env_root.join(TRIGGERSDIR)
}

/// Get the Unincorp (deferred triggers) file path
fn get_unincorp_path(env_root: &Path) -> PathBuf {
    get_triggers_dir(env_root).join(TRIGGERSDEFERREDFILE)
}

/// Ensure triggers directory exists
pub fn ensure_triggers_dir(env_root: &Path) -> Result<()> {
    let triggers_dir = get_triggers_dir(env_root);
    fs::create_dir_all(&triggers_dir)
        .with_context(|| format!("Failed to create triggers directory: {}", triggers_dir.display()))?;
    Ok(())
}

/// Parse trigger name and options (e.g., "trigger-name/noawait")
/// Returns (trigger_name, await_mode)
fn parse_trigger_with_options(trigger_str: &str) -> (String, bool) {
    if let Some(pos) = trigger_str.find("/noawait") {
        (trigger_str[..pos].to_string(), false)
    } else if let Some(pos) = trigger_str.find("/await") {
        (trigger_str[..pos].to_string(), true)
    } else {
        (trigger_str.to_string(), true) // Default to await
    }
}

/// Read trigger interests from package metadata
/// Returns (explicit_interests, file_interests)
/// explicit_interests: HashMap<trigger_name, Vec<(pkgname, await_mode)>>
/// file_interests: Vec<(file_path, pkgname, await_mode)>
pub fn read_package_trigger_interests(
    pkgkey: &str,
    store_root: &Path,
) -> Result<(HashMap<String, Vec<(String, bool)>>, Vec<(String, String, bool)>)> {
    let pkgname = pkgkey2pkgname(pkgkey).unwrap_or_else(|_| pkgkey.to_string());
    let install_dir = store_root.join(format!("{}/info/install", pkgkey));
    let interest_file = install_dir.join("deb_interest.triggers");

    let mut explicit_interests: HashMap<String, Vec<(String, bool)>> = HashMap::new();
    let mut file_interests: Vec<(String, String, bool)> = Vec::new();

    if !interest_file.exists() {
        return Ok((explicit_interests, file_interests));
    }

    let content = fs::read_to_string(&interest_file)
        .with_context(|| format!("Failed to read interest file: {}", interest_file.display()))?;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let (trigger_name, await_mode) = parse_trigger_with_options(line);

        // Check if it's a file trigger (starts with /)
        if trigger_name.starts_with('/') {
            file_interests.push((trigger_name, pkgname.clone(), await_mode));
        } else {
            // Explicit trigger
            explicit_interests
                .entry(trigger_name)
                .or_insert_with(Vec::new)
                .push((pkgname.clone(), await_mode));
        }
    }

    Ok((explicit_interests, file_interests))
}

/// Read activate triggers from package metadata
pub fn read_package_activate_triggers(
    pkgkey: &str,
    store_root: &Path,
) -> Result<Vec<(String, bool)>> {
    let install_dir = store_root.join(format!("{}/info/install", pkgkey));
    let activate_file = install_dir.join("deb_activate.triggers");

    if !activate_file.exists() {
        return Ok(Vec::new());
    }

    let content = fs::read_to_string(&activate_file)
        .with_context(|| format!("Failed to read activate file: {}", activate_file.display()))?;

    let mut triggers = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (trigger_name, await_mode) = parse_trigger_with_options(line);
        triggers.push((trigger_name, await_mode));
    }

    Ok(triggers)
}

/// Activate a trigger (add to Unincorp file)
/// Reference: dpkg-trigger main.c do_trigger()
/// Used by dpkg-trigger command
pub fn activate_trigger(
    env_root: &Path,
    trigger_name: &str,
    activating_package: Option<&str>,
    no_await: bool,
) -> Result<()> {
    ensure_triggers_dir(env_root)?;
    let unincorp_path = get_unincorp_path(env_root);

    // Read existing Unincorp
    let mut existing_activations: HashMap<String, Vec<String>> = HashMap::new();
    if unincorp_path.exists() {
        if let Ok(file) = fs::File::open(&unincorp_path) {
            let reader = BufReader::new(file);
            for line in reader.lines() {
                let line = line?;
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let parts: Vec<&str> = line.split_whitespace().collect();
                if !parts.is_empty() {
                    let trigger = parts[0].to_string();
                    let packages: Vec<String> = parts[1..].iter().map(|s| s.to_string()).collect();
                    existing_activations.insert(trigger, packages);
                }
            }
        }
    }

    // Add new activation
    let awaiter = if no_await {
        "-".to_string()
    } else {
        activating_package.map(|s| s.to_string()).unwrap_or_else(|| "-".to_string())
    };

    let packages = existing_activations.entry(trigger_name.to_string())
        .or_insert_with(Vec::new);

    if !packages.contains(&awaiter) {
        packages.push(awaiter);
    }

    // Write updated Unincorp
    let mut content = String::new();
    for (trigger, packages) in &existing_activations {
        if !packages.is_empty() {
            content.push_str(trigger);
            for pkg in packages {
                content.push(' ');
                content.push_str(pkg);
            }
            content.push('\n');
        }
    }

    fs::write(&unincorp_path, content)
        .with_context(|| format!("Failed to write Unincorp file: {}", unincorp_path.display()))?;

    Ok(())
}

/// Set up environment variables for Debian package scripts
/// Matches dpkg's behavior as seen in dpkg source code (main.c, script.c)
pub fn setup_deb_env_vars(
    env_vars: &mut std::collections::HashMap<String, String>,
    pkgkey: &str,
    package_info: &InstalledPackageInfo,
    scriptlet_type: crate::scriptlets::ScriptletType,
    _env_root: &std::path::Path,
) {
    use crate::package::{pkgkey2pkgname, pkgkey2version, pkgkey2arch};

    // Set DPKG_MAINTSCRIPT_NAME based on scriptlet type
    // Reference: dpkg src/main/script.c:199 setenv("DPKG_MAINTSCRIPT_NAME", cmd->argv[0], 1)
    let script_type = match scriptlet_type {
        crate::scriptlets::ScriptletType::PreInstall | crate::scriptlets::ScriptletType::PreUpgrade => "preinst",
        crate::scriptlets::ScriptletType::PostInstall | crate::scriptlets::ScriptletType::PostUpgrade => "postinst",
        crate::scriptlets::ScriptletType::PreRemove => "prerm",
        crate::scriptlets::ScriptletType::PostRemove => "postrm",
        crate::scriptlets::ScriptletType::PreTrans | crate::scriptlets::ScriptletType::PostTrans |
        crate::scriptlets::ScriptletType::PreUnTrans | crate::scriptlets::ScriptletType::PostUnTrans => {
                        // Transaction scriptlets not used for DEB
                        return;
                    }
    };
    env_vars.insert("DPKG_MAINTSCRIPT_NAME".to_string(), script_type.to_string());

    // Set DPKG_MAINTSCRIPT_PACKAGE to package name
    // Reference: dpkg src/main/script.c:195 setenv("DPKG_MAINTSCRIPT_PACKAGE", pkg->set->name, 1)
    if let Ok(package_name) = pkgkey2pkgname(pkgkey) {
        env_vars.insert("DPKG_MAINTSCRIPT_PACKAGE".to_string(), package_name);
    }

    // Set DPKG_MAINTSCRIPT_ARCH using pkgkey2arch
    // Reference: dpkg src/main/script.c:197 setenv("DPKG_MAINTSCRIPT_ARCH", pkgbin->arch->name, 1)
    if let Ok(arch) = pkgkey2arch(pkgkey) {
        env_vars.insert("DPKG_MAINTSCRIPT_ARCH".to_string(), arch);
    } else {
        // Fallback to the arch field from package_info
        env_vars.insert("DPKG_MAINTSCRIPT_ARCH".to_string(), package_info.arch.clone());
    }

    // Set DPKG_MAINTSCRIPT_VERSION using pkgkey2version
    // Note: This is not set by dpkg in script.c, but may be useful for scripts
    if let Ok(version) = pkgkey2version(pkgkey) {
        env_vars.insert("DPKG_MAINTSCRIPT_VERSION".to_string(), version);
    }

    // Set DPKG_MAINTSCRIPT_PACKAGE_REFCOUNT
    // Reference: dpkg src/main/script.c:196 setenv("DPKG_MAINTSCRIPT_PACKAGE_REFCOUNT", pkg_count, 1)
    // For now, we'll set it to 1 as a default value
    env_vars.insert("DPKG_MAINTSCRIPT_PACKAGE_REFCOUNT".to_string(), "1".to_string());

    // Set DPKG_ADMINDIR - dpkg database directory
    // Reference: dpkg src/main/main.c:805 setenv("DPKG_ADMINDIR", dpkg_db_get_dir(), 1)
    // Reference: dpkg src/main/script.c:116 setenv("DPKG_ADMINDIR", admindir + instdirlen, 1)
    // Scripts run inside env_root which is mounted as "/", so use "/var/lib/dpkg"
    env_vars.insert("DPKG_ADMINDIR".to_string(), "/var/lib/dpkg".to_string());

    // Set DPKG_ROOT - root filesystem directory
    // Reference: dpkg src/main/main.c:807 setenv("DPKG_ROOT", dpkg_fsys_get_dir(), 1)
    // Reference: dpkg src/main/script.c:118 setenv("DPKG_ROOT", "", 1)
    // When running scripts, dpkg sets this to "" (empty), but we use "/" since env_root is mounted as "/"
    env_vars.insert("DPKG_ROOT".to_string(), "/".to_string());

    // Set DPKG_RUNNING_VERSION - version of dpkg running the script
    // Reference: dpkg src/main/script.c:200 setenv("DPKG_RUNNING_VERSION", PACKAGE_VERSION, 1)
    // Use a reasonable dpkg version that scripts might expect
    env_vars.insert("DPKG_RUNNING_VERSION".to_string(), "1.21.22".to_string());

    // Set DPKG_MAINTSCRIPT_DEBUG if RUST_DEBUG is defined
    // Reference: dpkg src/main/script.c:199 setenv("DPKG_MAINTSCRIPT_DEBUG", maintscript_debug, 1)
    if std::env::var("RUST_DEBUG").is_ok() {
        env_vars.insert("DPKG_MAINTSCRIPT_DEBUG".to_string(), "1".to_string());
    }

    // Suppress debconf interactive prompts and warnings
    // Note: These are not set by dpkg, but are useful for non-interactive operation
    env_vars.insert("DEBIAN_FRONTEND".to_string(), "noninteractive".to_string());
    env_vars.insert("DEBCONF_NONINTERACTIVE_SEEN".to_string(), "true".to_string());
}
