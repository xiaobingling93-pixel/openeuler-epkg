use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::collections::HashMap;
use color_eyre::Result;
use color_eyre::eyre::{Context, eyre};
use crate::models::{InstalledPackageInfo, PACKAGE_CACHE, PackageFormat};
use std::sync::Arc;
use crate::plan::InstallationPlan;
use crate::hooks::{Hook, HookWhen};

// Constants matching dpkg's structure
pub const TRIGGERSDIR: &str = "var/lib/dpkg/triggers";
pub const TRIGGERSDEFERREDFILE: &str = "Unincorp";

#[derive(Debug, Clone)]
pub(crate) struct TriggerEntry {
    name: String,
    await_mode: bool, // true = await, false = noawait
}

/// Get the triggers directory path in the environment
fn get_triggers_dir(env_root: &Path) -> PathBuf {
    env_root.join(TRIGGERSDIR)
}

/// Get the Unincorp (deferred triggers) file path
fn get_unincorp_path(env_root: &Path) -> PathBuf {
    get_triggers_dir(env_root).join(TRIGGERSDEFERREDFILE)
}

/// Convert a deb trigger name to a filename-safe string.
/// For file triggers (starting with '/'), replaces '/' with '__'.
/// For explicit triggers, returns the name as-is.
fn trigger_name_to_filename(name: &str) -> String {
    if name.starts_with('/') {
        // File trigger: replace '/' with '__'
        name.replace('/', "__").to_string()
    } else {
        // Explicit trigger: use trigger name as-is
        name.to_string()
    }
}

/// Ensure triggers directory exists
pub fn ensure_triggers_dir(env_root: &Path) -> Result<()> {
    let triggers_dir = get_triggers_dir(env_root);
    fs::create_dir_all(&triggers_dir)
        .with_context(|| format!("Failed to create triggers directory: {}", triggers_dir.display()))?;
    Ok(())
}

/// Process triggers for a single package (both interest and activate triggers)
/// Helper function used by load_initial_deb_triggers and load_batch_deb_triggers
fn load_deb_package_triggers(
    plan: &mut InstallationPlan,
    pkgkey: &str,
    pkgline: &str,
) -> Result<()> {
    if pkgline.is_empty() {
        return Ok(());
    }

    // Read triggers from info/deb/triggers using pkgline (packages in store are stored by pkgline)
    let package_dir = plan.store_root.join(pkgline);
    let (interest_triggers, activate_triggers) = read_package_triggers(&package_dir)?;

    add_interest_triggers_to_maps(plan, pkgkey, interest_triggers);
    add_activate_triggers_to_maps(plan, pkgkey, activate_triggers);

    Ok(())
}

/// Add triggers to bidirectional maps (pkgkey -> trigger names, trigger name -> pkgkeys)
/// Common helper used by add_interest_triggers_to_maps and add_activate_triggers_to_maps
fn add_triggers_to_bidirectional_maps(
    pkgkey: &str,
    trigger_names: Vec<String>,
    pkg_to_triggers: &mut HashMap<String, Vec<String>>,
    trigger_to_pkgs: &mut HashMap<String, Vec<String>>,
) {
    let pkgkey_string = pkgkey.to_string();
    for trigger_name in trigger_names {
        // Map: pkgkey -> trigger names
        let pkg_entry = pkg_to_triggers
            .entry(pkgkey_string.clone())
            .or_insert_with(Vec::new);
        if !pkg_entry.contains(&trigger_name) {
            pkg_entry.push(trigger_name.clone());
        }

        // Map: trigger name -> pkgkeys
        let name_entry = trigger_to_pkgs
            .entry(trigger_name.clone())
            .or_insert_with(Vec::new);
        if !name_entry.contains(&pkgkey_string) {
            name_entry.push(pkgkey_string.clone());
        }
    }
}

/// Add interest triggers to the plan's trigger maps
/// Helper function used by load_deb_package_triggers()
fn add_interest_triggers_to_maps(
    plan: &mut InstallationPlan,
    pkgkey: &str,
    triggers: Vec<TriggerEntry>,
) {
    let trigger_names: Vec<String> = triggers.into_iter().map(|t| t.name).collect();
    add_triggers_to_bidirectional_maps(
        pkgkey,
        trigger_names,
        &mut plan.deb_explicit_triggers_by_pkg,
        &mut plan.deb_explicit_triggers_by_name,
    );
}

/// Add activate triggers to the plan's trigger maps
/// Helper function used by load_deb_package_triggers()
fn add_activate_triggers_to_maps(
    plan: &mut InstallationPlan,
    pkgkey: &str,
    triggers: Vec<TriggerEntry>,
) {
    let trigger_names: Vec<String> = triggers.into_iter().map(|t| t.name).collect();
    add_triggers_to_bidirectional_maps(
        pkgkey,
        trigger_names,
        &mut plan.deb_activate_triggers_by_pkg,
        &mut plan.deb_activate_triggers_by_name,
    );
}

/// Unincorp file format (Deferred Triggers)
///
/// The Unincorp file stores deferred trigger activations that need to be processed
/// after package operations complete. This matches dpkg's behavior for handling
/// triggers that are activated during package installation/removal.
///
/// File Format:
/// ============
/// - Location: `{env_root}/var/lib/dpkg/triggers/Unincorp`
/// - Format: One trigger per line
/// - Line format: `<trigger-name> <activating-package-1> [<activating-package-2> ...]`
/// - Comments: Lines starting with `#` are ignored
/// - Empty lines: Skipped during parsing
///
/// Trigger Name:
/// =============
/// - Must be printable ASCII characters (0x21-0x7e)
/// - Terminated by whitespace or end of line
///
/// Activating Package Values:
/// ==========================
/// - `"-"` (single dash): Indicates a noawait trigger (processed immediately at PostInstall)
/// - Package name: Indicates an await trigger (processed at PostTransaction)
///   The package name is the package that activated the trigger
///
/// Package Name Format:
/// ===================
/// - Must start with: digit, lowercase letter, or `-`
/// - Can contain: digits, lowercase letters, `-`, `:`, `+`, `.`
/// - Special case: `-` alone is valid (noawait), but `-something` is invalid
///
/// Examples:
/// ========
/// ```
/// mime-support package1 package2
/// menu - package3
/// ```
///
/// In the above example:
/// - `mime-support` trigger was activated by `package1` and `package2` (await mode)
/// - `menu` trigger has one noawait activation (`-`) and one await activation (`package3`)
///
/// Processing:
/// ==========
/// - noawait triggers (`-`) are processed at PostInstall (immediate, per-package)
/// - await triggers (package names) are processed at PostTransaction (batched)
/// - After processing, processed triggers are removed from the file
///
/// Reference: dpkg source code (lib/dpkg/trigdeferred.c, src/trigger/main.c)
///
/// Read and parse the Unincorp file
/// Returns a HashMap mapping trigger names to their activating packages
fn read_unincorp_file(unincorp_path: &Path) -> Result<HashMap<String, Vec<String>>> {
    let mut activations: HashMap<String, Vec<String>> = HashMap::new();

    if !unincorp_path.exists() {
        return Ok(activations);
    }

    if let Ok(file) = fs::File::open(unincorp_path) {
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
                activations.insert(trigger, packages);
            }
        }
    }

    Ok(activations)
}

/// Write the Unincorp file from a HashMap of trigger activations
fn write_unincorp_file(
    unincorp_path: &Path,
    activations: &HashMap<String, Vec<String>>,
) -> Result<()> {
    let mut content = String::new();
    for (trigger, packages) in activations {
        if !packages.is_empty() {
            content.push_str(trigger);
            for pkg in packages {
                content.push(' ');
                content.push_str(pkg);
            }
            content.push('\n');
        }
    }

    fs::write(unincorp_path, content)
        .with_context(|| format!("Failed to write Unincorp file: {}", unincorp_path.display()))?;

    Ok(())
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
    let mut existing_activations = read_unincorp_file(&unincorp_path)?;

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
    write_unincorp_file(&unincorp_path, &existing_activations)?;

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

/// Read triggers from package metadata (info/deb/triggers file)
/// Returns (interest_triggers, activate_triggers)
/// Both are Vec<TriggerEntry> containing trigger names and await_mode
///
/// Takes the full path to the package directory (e.g., store_root/pkgline)
pub fn read_package_triggers<P: AsRef<Path>>(
    package_dir: P,
) -> Result<(Vec<TriggerEntry>, Vec<TriggerEntry>)> {
    let triggers_path = package_dir.as_ref().join("info/deb/triggers");
    if !triggers_path.exists() {
        return Ok((Vec::new(), Vec::new()));
    }

    let triggers_content = fs::read_to_string(&triggers_path)
        .with_context(|| format!("Failed to read triggers file: {}", triggers_path.display()))?;

    let mut interest_triggers: Vec<TriggerEntry> = Vec::new();
    let mut activate_triggers: Vec<TriggerEntry> = Vec::new();

    for (line_num, line) in triggers_content.lines().enumerate() {
        let line = line.trim();
        let line_num = line_num + 1; // 1-based line numbers

        // Skip comments and empty lines
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Parse trigger directives
        // Format: "<directive> <trigger-name>"
        // Directives: interest, interest-await, interest-noawait, activate, activate-await, activate-noawait
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }

        let directive = parts[0];
        let trigger_name = parts[1..].join(" ");

        match directive {
            "interest" | "interest-await" => {
                interest_triggers.push(TriggerEntry { name: trigger_name, await_mode: true });
            }
            "interest-noawait" => {
                interest_triggers.push(TriggerEntry { name: trigger_name, await_mode: false });
            }
            "activate" | "activate-await" => {
                activate_triggers.push(TriggerEntry { name: trigger_name, await_mode: true });
            }
            "activate-noawait" => {
                activate_triggers.push(TriggerEntry { name: trigger_name, await_mode: false });
            }
            _ => {
                return Err(eyre!(
                    "Unknown trigger directive '{}' in triggers file '{}' at line {}",
                    directive,
                    triggers_path.display(),
                    line_num
                ));
            }
        }
    }

    Ok((interest_triggers, activate_triggers))
}

/// Generate Arch-style .hook files under info/install/ for Debian triggers.
///
/// Current mapping (conservative, file-trigger only):
/// - For each interest trigger whose name starts with '/', we create a Path hook:
///   - [Trigger]:
///     - Operation = Install|Upgrade|Remove
///     - Type = Path
///     - Target = <trigger path>
///   - [Action]:
///     - When = PostTransaction
///     - Exec = /bin/true          (no-op placeholder for now)
///
/// This allows the generic hooks engine to see where Debian file triggers
/// would conceptually fire, without changing the existing dpkg-style trigger
/// runtime in `deb_triggers.rs`.
pub fn write_deb_trigger_hooks<P: AsRef<Path>>(
    interest_triggers: &[TriggerEntry],
    activate_triggers: &[TriggerEntry],
    store_tmp_dir: P,
) -> Result<()> {
    use std::fmt::Write as FmtWrite;

    let store_tmp_dir = store_tmp_dir.as_ref();
    let install_dir = store_tmp_dir.join("info/install");

    if interest_triggers.is_empty() && activate_triggers.is_empty() {
        return Ok(());
    }

    fs::create_dir_all(&install_dir)?;

    // Generate hooks for interest triggers
    // These hooks will run when matching packages activate the trigger
    for entry in interest_triggers {
        let name = entry.name.trim();

        let mut buf = String::new();

        // Map await mode to When phase:
        // - noawait -> PostInstall (immediate, per-package processing)
        // - await -> PostTransaction (batched, after all packages are processed)
        let when_phase = if entry.await_mode {
            "PostTransaction"
        } else {
            "PostInstall"
        };

        // Determine trigger type: file trigger (starts with '/') or explicit trigger
        let (hook_type, target) = if name.starts_with('/') {
            // File trigger: Path type
            ("Path", name)
        } else {
            // Explicit trigger: Package type
            ("Package", name)
        };

        // [Trigger]
        buf.push_str("[Trigger]\n");
        buf.push_str("Operation = Install\n");
        buf.push_str("Operation = Upgrade\n");
        buf.push_str("Operation = Remove\n");
        writeln!(buf, "Type = {}", hook_type)?;
        writeln!(buf, "Target = {}", target)?;

        // [Action]
        buf.push_str("\n[Action]\n");
        writeln!(buf, "When = {}", when_phase)?;
        writeln!(
            buf,
            "Description = DEB {} trigger for {} (defer_mode={})",
            if hook_type == "Path" { "file" } else { "explicit" },
            target,
            if entry.await_mode { "await" } else { "noawait" }
        )?;
        // Exec will call the package's postinst with "triggered" argument
        // The postinst script is in the same directory as the hook file
        // Use %PKGINFO_DIR placeholder that will be replaced at runtime with the actual package info directory
        // The hook engine will add "triggered" and trigger names as arguments:
        // postinst triggered <trigger-name>...
        writeln!(buf, "Exec = %PKGINFO_DIR/deb/postinst triggered")?;

        // For DEB triggers, use the trigger name itself as the hook name
        // Sanitize the trigger name for use as a filename (replace '/' with '__' for file triggers)
        let hook_name = trigger_name_to_filename(name);
        let hook_path = install_dir.join(format!("{}.hook", hook_name));
        fs::write(&hook_path, buf)
            .with_context(|| format!("Failed to write DEB hook file {}", hook_path.display()))?;
    }

    // Note: activate_triggers don't generate hooks directly - they are used
    // to match against interest triggers. The hook engine will need to check
    // which packages activate which triggers and match them against interest hooks.

    Ok(())
}

/// Build Debian trigger initial maps for the plan.
pub fn load_initial_deb_triggers(plan: &mut InstallationPlan) -> Result<()> {
    if plan.package_format != PackageFormat::Deb {
        return Ok(());
    }

    let installed = PACKAGE_CACHE.installed_packages.read().unwrap();

    for (pkgkey, info) in installed.iter() {
        load_deb_package_triggers(plan, pkgkey, &info.pkgline)?;
    }

    Ok(())
}

/// Load Debian triggers for packages in the current batch.
/// Extends plan trigger maps with triggers from packages in plan.batch.new_pkgkeys.
pub fn load_batch_deb_triggers(plan: &mut InstallationPlan) -> Result<()> {
    if plan.package_format != PackageFormat::Deb {
        return Ok(());
    }

    let pkgkeys: Vec<String> = plan.batch.new_pkgkeys.iter().cloned().collect();
    for pkgkey in pkgkeys {
        let pkgline = crate::plan::pkgkey2pkgline(plan, &pkgkey);
        load_deb_package_triggers(plan, &pkgkey, &pkgline)?;
    }

    Ok(())
}

/// Separate triggers into noawait and await categories, then split based on when
/// Returns (triggers_to_consume, triggers_remaining)
/// If any activating_package is '-', classify the entire trigger as noawait
fn separate_unincorp_triggers(
    trigger_activations: HashMap<String, Vec<String>>,
    when: HookWhen,
) -> Result<(HashMap<String, Vec<String>>, HashMap<String, Vec<String>>)> {
    let mut noawait_triggers: HashMap<String, Vec<String>> = HashMap::new();
    let mut await_triggers: HashMap<String, Vec<String>> = HashMap::new();

    for (trigger_name, activating_packages) in trigger_activations {
        // If any activating_package is '-', classify as noawait
        if activating_packages.iter().any(|pkg| pkg == "-") {
            noawait_triggers.insert(trigger_name, activating_packages);
        } else {
            await_triggers.insert(trigger_name, activating_packages);
        }
    }

    // Split based on when parameter
    let (triggers_to_consume, triggers_remaining) = match when {
        HookWhen::PostInstall => (noawait_triggers, await_triggers),
        HookWhen::PostTransaction => (await_triggers, noawait_triggers),
        _ => {
            // Only PostInstall and PostTransaction are valid
            return Err(color_eyre::eyre::eyre!("Invalid HookWhen for unincorp triggers"));
        }
    };

    Ok((triggers_to_consume, triggers_remaining))
}

/*
 * Trigger name → hook lookup for Unincorp execution
 *
 * Hooks are stored in plan.hooks_by_name under two shapes:
 * - Global hook: key = base name (e.g. "update-ca-certificates").
 * - Package hook (when no global exists): key = base_name + "-" + pkgkey
 *   (e.g. "update-ca-certificates-ca-certificates__20250419__all").
 *
 * So we look up by exact trigger-derived name first; if missing, we collect
 * any key that equals the name or starts with "name-", and prefer the hook
 * from a package in the current batch (newly installed).
 */
/// Find a hook for a trigger by base name. Package hooks are registered as "base_name-pkgkey";
/// this looks up exact name first, then any key starting with "base_name-", preferring a hook
/// from a package in the current batch (newly installed).
fn find_hook_for_trigger<'a>(
    plan: &'a InstallationPlan,
    hook_name: &str,
) -> Option<&'a Arc<Hook>> {
    if let Some(hook) = plan.hooks_by_name.get(hook_name) {
        return Some(hook);
    }
    let prefix = format!("{}-", hook_name);
    let candidates: Vec<&Arc<Hook>> = plan
        .hooks_by_name
        .iter()
        .filter(|(k, _)| k.as_str() == hook_name || k.starts_with(&prefix))
        .map(|(_, h)| h)
        .collect();
    if candidates.is_empty() {
        return None;
    }
    if candidates.len() == 1 {
        return Some(candidates[0]);
    }
    // Prefer hook from a package in the current batch (newly installed)
    candidates
        .iter()
        .find(|h| h.pkgkey.as_ref().map_or(false, |pk| plan.batch.new_pkgkeys.contains(pk)))
        .copied()
        .or_else(|| Some(candidates[0]))
}

/// Directly find and execute hooks for unincorp triggers without modifying the plan
/// For each trigger name, finds the hook by name and executes it directly
fn run_unincorp_trigger_hooks(
    plan: &InstallationPlan,
    triggers_to_process: &HashMap<String, Vec<String>>,
) -> Result<()> {
    for trigger_name in triggers_to_process.keys() {
        let hook_name = trigger_name_to_filename(trigger_name);

        if let Some(hook) = find_hook_for_trigger(plan, &hook_name) {
            let matched_targets = vec![trigger_name.clone()];
            crate::hooks::execute_hook(hook.as_ref(), plan, &matched_targets)?;
        } else {
            log::warn!("No hook found for trigger '{}' (hook name: '{}')", trigger_name, hook_name);
            let mut available: Vec<&str> = plan.hooks_by_name.keys().map(String::as_str).collect();
            available.sort();
            log::debug!("Available hooks: {:?}", available);
        }
    }

    Ok(())
}

/// Process triggers from Unincorp file
/// Reads the Unincorp file and directly finds/executes matching hooks.
///
/// - noawait records (activating_package == "-") -> PostInstall (immediate, per-package processing)
/// - await records (activating_package != "-") -> PostTransaction (batched, after all packages are processed)
pub fn run_debian_unincorp_triggers(
    plan: &mut InstallationPlan,
    when: HookWhen,
) -> Result<()> {
    if plan.package_format != PackageFormat::Deb {
        return Ok(());
    }

    let unincorp_path = get_unincorp_path(&plan.env_root);

    // Read Unincorp file
    let trigger_activations = read_unincorp_file(&unincorp_path)?;
    if trigger_activations.is_empty() {
        return Ok(());
    }

    // Separate triggers and split based on when parameter
    let (triggers_to_consume, triggers_remaining) = separate_unincorp_triggers(trigger_activations, when)?;
    if triggers_to_consume.is_empty() {
        return Ok(());
    }

    // Directly find and execute hooks for unincorp triggers without modifying the plan
    run_unincorp_trigger_hooks(plan, &triggers_to_consume)?;

    // Write remaining triggers back to Unincorp
    write_unincorp_file(&unincorp_path, &triggers_remaining)?;

    Ok(())
}
