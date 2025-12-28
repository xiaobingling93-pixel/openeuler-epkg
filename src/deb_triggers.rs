use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::collections::{HashMap, HashSet};
use color_eyre::Result;
use color_eyre::eyre::{self, Context};
use crate::models::InstalledPackageInfo;
use crate::package::pkgkey2pkgname;

/// Cycle detection state for hare-and-tortoise algorithm
/// Reference: /c/package-managers/dpkg/src/main/trigproc.c
struct TriggerCycleNode {
    /// Package that was processed at this step
    processed_pkgkey: String,
    /// Pending triggers for each package at this step
    /// HashMap<pkgkey, Vec<trigger_name>>
    pending_triggers: HashMap<String, Vec<String>>,
    /// Next node in the cycle detection chain
    next: Option<Box<TriggerCycleNode>>,
}

/// Global cycle detection state (hare-and-tortoise algorithm)
struct CycleDetectionState {
    tortoise: Option<Box<TriggerCycleNode>>,
    hare: Option<Box<TriggerCycleNode>>,
    tortoise_advance: bool,
}

impl CycleDetectionState {
    fn new() -> Self {
        CycleDetectionState {
            tortoise: None,
            hare: None,
            tortoise_advance: false,
        }
    }

    #[allow(dead_code)]
    fn reset(&mut self) {
        self.tortoise = None;
        self.hare = None;
        self.tortoise_advance = false;
    }

    /// Check for cycles using hare-and-tortoise algorithm
    /// Returns Some(pkgkey) if cycle detected (package to mark as failed), None otherwise
    /// Reference: /c/package-managers/dpkg/src/main/trigproc.c check_trigger_cycle()
    fn check_cycle(
        &mut self,
        processing_pkgkey: &str,
        all_pending_triggers: &HashMap<String, Vec<String>>,
    ) -> Option<String> {
        // Create new cycle node with current state
        let new_node = TriggerCycleNode {
            processed_pkgkey: processing_pkgkey.to_string(),
            pending_triggers: all_pending_triggers.clone(),
            next: None,
        };

        // First node - initialize tortoise and hare
        if self.hare.is_none() {
            let node = Box::new(new_node);
            // For first node, both point to the same node
            // We need to create two separate nodes with same data
            let node_data = TriggerCycleNode {
                processed_pkgkey: processing_pkgkey.to_string(),
                pending_triggers: all_pending_triggers.clone(),
                next: None,
            };
            self.tortoise = Some(Box::new(node_data));
            self.hare = Some(node);
            return None;
        }

        // Add new node to hare chain
        let node = Box::new(new_node);
        if let Some(ref mut hare) = self.hare {
            // Store the next pointer before moving
            let next_node = node;
            hare.next = Some(next_node);
            // Move hare to the new node
            if let Some(next) = hare.next.take() {
                self.hare = Some(next);
            }
        } else {
            self.hare = Some(node);
        }

        // Advance tortoise every other step
        if self.tortoise_advance {
            if let Some(ref mut tortoise) = self.tortoise {
                if let Some(next) = tortoise.next.take() {
                    self.tortoise = Some(next);
                }
            }
        }
        self.tortoise_advance = !self.tortoise_advance;

        // Check if hare's pending triggers are a superset of tortoise's
        // If so, we have a cycle
        if let (Some(ref tortoise), Some(ref hare)) = (&self.tortoise, &self.hare) {
            // Check if all packages in tortoise have their triggers in hare
            for (tortoise_pkgkey, tortoise_triggers) in &tortoise.pending_triggers {
                // Get hare's triggers for this package
                if let Some(hare_triggers) = hare.pending_triggers.get(tortoise_pkgkey) {
                    // Check if all tortoise triggers are in hare
                    for tortoise_trigger in tortoise_triggers {
                        if !hare_triggers.contains(tortoise_trigger) {
                            // Not a superset - no cycle yet
                            return None;
                        }
                    }
                } else {
                    // Package not in hare - no cycle
                    return None;
                }
            }

            // Cycle detected! Return the earliest package in the cycle to mark as failed
            // Get the first package from tortoise's pending triggers
            if let Some((first_pkgkey, _)) = tortoise.pending_triggers.iter().next() {
                log::warn!(
                    "Trigger cycle detected! Chain: {} -> ... -> {}",
                    tortoise.processed_pkgkey,
                    processing_pkgkey
                );
                return Some(first_pkgkey.clone());
            }
        }

        None
    }
}

/// Global cycle detection state (thread-local for safety, but in practice epkg is single-threaded)
#[allow(static_mut_refs)]
static mut CYCLE_DETECTION: Option<CycleDetectionState> = None;

/// Reset cycle detection state (call at start of trigger processing)
pub fn reset_cycle_detection() {
    unsafe {
        CYCLE_DETECTION = Some(CycleDetectionState::new());
    }
}

/// Check for trigger cycles
/// Returns Some(pkgkey) if cycle detected, None otherwise
#[allow(static_mut_refs)]
pub fn check_trigger_cycle(
    processing_pkgkey: &str,
    all_pending_triggers: &HashMap<String, Vec<String>>,
) -> Option<String> {
    unsafe {
        if CYCLE_DETECTION.is_none() {
            CYCLE_DETECTION = Some(CycleDetectionState::new());
        }
        if let Some(ref mut state) = CYCLE_DETECTION {
            state.check_cycle(processing_pkgkey, all_pending_triggers)
        } else {
            None
        }
    }
}

// Constants matching dpkg's structure
pub const TRIGGERSDIR: &str = "var/lib/dpkg/triggers";
pub const TRIGGERSDEFERREDFILE: &str = "Unincorp";
const TRIGGERSFILEFILE: &str = "File";
const TRIGGERSLOCKFILE: &str = "Lock";

/// Get the triggers directory path in the environment
fn get_triggers_dir(env_root: &Path) -> PathBuf {
    env_root.join(TRIGGERSDIR)
}

/// Get the File triggers file path
fn get_triggers_file_path(env_root: &Path) -> PathBuf {
    get_triggers_dir(env_root).join(TRIGGERSFILEFILE)
}

/// Get the Unincorp (deferred triggers) file path
fn get_unincorp_path(env_root: &Path) -> PathBuf {
    get_triggers_dir(env_root).join(TRIGGERSDEFERREDFILE)
}

/// Get the lock file path
#[allow(dead_code)]
fn get_lock_path(env_root: &Path) -> PathBuf {
    get_triggers_dir(env_root).join(TRIGGERSLOCKFILE)
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

/// Update explicit trigger interest file in /var/lib/dpkg/triggers/<trigger-name>
fn update_explicit_trigger_interest(
    env_root: &Path,
    trigger_name: &str,
    interested_packages: &[(String, bool)],
) -> Result<()> {
    ensure_triggers_dir(env_root)?;
    let trigger_file = get_triggers_dir(env_root).join(trigger_name);

    if interested_packages.is_empty() {
        // Remove file if no packages are interested
        if trigger_file.exists() {
            fs::remove_file(&trigger_file)?;
        }
        return Ok(());
    }

    // Write package list (format: "package[/noawait]")
    let mut content = String::new();
    for (pkgname, await_mode) in interested_packages {
        if *await_mode {
            content.push_str(pkgname);
        } else {
            content.push_str(&format!("{}/noawait", pkgname));
        }
        content.push('\n');
    }

    fs::write(&trigger_file, content)
        .with_context(|| format!("Failed to write trigger interest file: {}", trigger_file.display()))?;

    Ok(())
}

/// Update file trigger interest file in /var/lib/dpkg/triggers/File
/// Format: "/path/to/file package[/noawait]"
fn update_file_trigger_interests(
    env_root: &Path,
    file_interests: &[(String, String, bool)], // (file_path, pkgname, await_mode)
) -> Result<()> {
    ensure_triggers_dir(env_root)?;
    let file_path = get_triggers_file_path(env_root);

    if file_interests.is_empty() {
        // Remove file if no file triggers
        if file_path.exists() {
            fs::remove_file(&file_path)?;
        }
        return Ok(());
    }

    // Write file trigger interests
    let mut content = String::new();
    for (file_path, pkgname, await_mode) in file_interests {
        if *await_mode {
            content.push_str(&format!("{} {}\n", file_path, pkgname));
        } else {
            content.push_str(&format!("{} {}/noawait\n", file_path, pkgname));
        }
    }

    fs::write(&file_path, content)
        .with_context(|| format!("Failed to write file triggers file: {}", file_path.display()))?;

    Ok(())
}

/// Helper function to filter and update a trigger file, removing it if empty
fn filter_and_update_trigger_file(file_path: &Path, filter_fn: impl Fn(&str) -> bool) -> Result<()> {
    if !file_path.exists() {
        return Ok(());
    }

    let content = fs::read_to_string(file_path)?;
    let updated_lines: Vec<String> = content.lines()
        .filter(|line| {
            let line = line.trim();
            !line.is_empty() && filter_fn(line)
        })
        .map(|s| s.to_string())
        .collect();

    if updated_lines.is_empty() {
        fs::remove_file(file_path)?;
    } else {
        fs::write(file_path, updated_lines.join("\n") + "\n")?;
    }

    Ok(())
}

/// Helper function to read existing explicit trigger interests from a file
fn read_explicit_trigger_interests(trigger_file: &Path) -> Result<Vec<(String, bool)>> {
    let mut existing_packages: Vec<(String, bool)> = Vec::new();

    if trigger_file.exists() {
        if let Ok(content) = fs::read_to_string(trigger_file) {
            for line in content.lines() {
                let line = line.trim();
                if !line.is_empty() {
                    let (pkgname, await_mode) = parse_trigger_with_options(line);
                    existing_packages.push((pkgname, await_mode));
                }
            }
        }
    }

    Ok(existing_packages)
}

/// Helper function to read existing file trigger interests from a file
fn read_file_trigger_interests(file_path: &Path) -> Result<Vec<(String, String, bool)>> {
    let mut existing_file_interests: Vec<(String, String, bool)> = Vec::new();

    if file_path.exists() {
        if let Ok(content) = fs::read_to_string(file_path) {
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    let file_path = parts[0].to_string();
                    let (pkgname, await_mode) = parse_trigger_with_options(parts[1]);
                    existing_file_interests.push((file_path, pkgname, await_mode));
                }
            }
        }
    }

    Ok(existing_file_interests)
}

/// Incorporate package trigger interests into /var/lib/dpkg/triggers/
/// Called during package unpack/remove
pub fn incorporate_package_trigger_interests(
    pkgkey: &str,
    store_root: &Path,
    env_root: &Path,
    is_removal: bool,
) -> Result<()> {
    if is_removal {
        // On removal, we need to remove this package from all trigger interest files
        let pkgname = pkgkey2pkgname(pkgkey).unwrap_or_else(|_| pkgkey.to_string());

        // Remove from explicit trigger files
        let triggers_dir = get_triggers_dir(env_root);
        if triggers_dir.exists() {
            for entry in fs::read_dir(&triggers_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.is_file() && path.file_name() != Some(TRIGGERSFILEFILE.as_ref())
                    && path.file_name() != Some(TRIGGERSDEFERREDFILE.as_ref())
                    && path.file_name() != Some(TRIGGERSLOCKFILE.as_ref()) {
                    // This is an explicit trigger interest file
                    filter_and_update_trigger_file(&path, |line| {
                        !line.starts_with(&format!("{}/", pkgname)) && line != &pkgname
                    })?;
                }
            }
        }

        // Remove from file triggers
        let file_path = get_triggers_file_path(env_root);
        filter_and_update_trigger_file(&file_path, |line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                let pkg_part = parts[1];
                !pkg_part.starts_with(&format!("{}/", pkgname)) && pkg_part != &pkgname
            } else {
                true
            }
        })?;
    } else {
        // On install/upgrade, add interests
        let (explicit_interests, file_interests) = read_package_trigger_interests(pkgkey, store_root)?;

        // Update explicit trigger interest files
        for (trigger_name, packages) in explicit_interests {
            // Read existing interests
            let triggers_dir = get_triggers_dir(env_root);
            let trigger_file = triggers_dir.join(&trigger_name);
            let existing_packages = read_explicit_trigger_interests(&trigger_file)?;

            // Merge with new interests (avoid duplicates)
            let mut all_packages = existing_packages;
            for (pkgname, await_mode) in packages {
                if !all_packages.iter().any(|(p, _)| p == &pkgname) {
                    all_packages.push((pkgname, await_mode));
                }
            }

            update_explicit_trigger_interest(env_root, &trigger_name, &all_packages)?;
        }

        // Update file trigger interests
        let file_path = get_triggers_file_path(env_root);
        let existing_file_interests = read_file_trigger_interests(&file_path)?;

        // Merge with new file interests
        let mut all_file_interests = existing_file_interests;
        for (file_path, pkgname, await_mode) in file_interests {
            if !all_file_interests.iter().any(|(f, p, _)| f == &file_path && p == &pkgname) {
                all_file_interests.push((file_path, pkgname, await_mode));
            }
        }

        update_file_trigger_interests(env_root, &all_file_interests)?;
    }

    Ok(())
}

/// Activate a trigger (add to Unincorp file)
/// Reference: dpkg-trigger main.c do_trigger()
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

/// Build a cached index of file trigger paths for efficient matching
/// Returns HashSet of trigger paths
pub fn build_file_trigger_index(env_root: &Path) -> Result<HashSet<String>> {
    let file_path_trigger = get_triggers_file_path(env_root);
    if !file_path_trigger.exists() {
        return Ok(HashSet::new());
    }

    let content = fs::read_to_string(&file_path_trigger)?;
    let mut trigger_paths = HashSet::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if !parts.is_empty() {
            trigger_paths.insert(parts[0].to_string());
        }
    }

    Ok(trigger_paths)
}

/// Check if a file path matches a trigger path
/// File triggers match if the trigger path is a directory prefix of the file path
/// e.g., /usr/share/omf matches /usr/share/omf/file but NOT /usr/share/omf2/file
fn file_path_matches_trigger(file_path: &str, trigger_path: &str) -> bool {
    if !file_path.starts_with(trigger_path) {
        return false;
    }

    // Check boundary: next character must be '/' or end of string
    let remaining = &file_path[trigger_path.len()..];
    remaining.is_empty() || remaining.starts_with('/')
}

/// Activate file trigger for a file path
/// Uses cached trigger index for efficiency
///
/// Note: dpkg calls trig_file_activate_parents(), however it looks not necessary to
/// walk parent dirs -- if there are 2 dirs in trigger_index where one is another's parent,
/// they'll both be found by file_path_matches_trigger().
pub fn activate_file_trigger(
    env_root: &Path,
    file_path: &str,
    activating_package: Option<&str>,
    trigger_index: &HashSet<String>,
) -> Result<()> {
    if trigger_index.is_empty() {
        return Ok(()); // No file triggers registered
    }

    let mut matched_triggers = HashSet::new();

    // Check if file matches any file trigger
    // File triggers match if the trigger path is a prefix of the file path
    for trigger_path in trigger_index {
        if file_path_matches_trigger(file_path, trigger_path) {
            matched_triggers.insert(trigger_path.clone());
        }
    }

    // Activate all matched triggers
    for trigger in matched_triggers {
        activate_trigger(env_root, &trigger, activating_package, false)?;
    }

    Ok(())
}

/// Result of trigger incorporation
#[derive(Debug, Clone)]
pub struct TriggerIncorporationResult {
    /// Packages with pending triggers to process: HashMap<pkgname, Vec<trigger_name>>
    pub pending_triggers: HashMap<String, Vec<String>>,
    /// Packages that should be marked as triggers-awaited: HashSet<pkgname>
    pub awaiting_packages: HashSet<String>,
}

/// Incorporate triggers from Unincorp into package status
/// Returns packages that need trigger processing and packages that should await
pub fn incorporate_triggers(
    env_root: &Path,
    installed_packages: &HashMap<String, InstalledPackageInfo>,
    store_root: &Path,
) -> Result<TriggerIncorporationResult> {
    let unincorp_path = get_unincorp_path(env_root);
    if !unincorp_path.exists() {
        return Ok(TriggerIncorporationResult {
            pending_triggers: HashMap::new(),
            awaiting_packages: HashSet::new(),
        });
    }

    // Read Unincorp
    // Format: <trigger-name> <activating-package> ...
    // The activating packages are the ones that triggered the trigger
    let mut trigger_activations: HashMap<String, Vec<String>> = HashMap::new();
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
                trigger_activations.insert(trigger, packages);
            }
        }
    }

    // Find interested packages and add triggers to their pending list
    // Check both explicit and file trigger interests
    // Also track which triggering packages should await
    let mut packages_with_pending_triggers: HashMap<String, Vec<String>> = HashMap::new();
    let mut packages_that_should_await: HashSet<String> = HashSet::new();

    // Build a map of triggering packages for each trigger
    // This helps us determine which packages activated which triggers
    let mut trigger_to_activators: HashMap<String, Vec<String>> = HashMap::new();
    for (trigger_name, activating_packages) in &trigger_activations {
        for activating_pkg in activating_packages {
            if activating_pkg != "-" {
                trigger_to_activators
                    .entry(trigger_name.clone())
                    .or_insert_with(Vec::new)
                    .push(activating_pkg.clone());
            }
        }
    }

    for (pkgkey, _pkg_info) in installed_packages {
        let (explicit_interests, file_interests) = read_package_trigger_interests(pkgkey, store_root)?;
        let pkgname = pkgkey2pkgname(pkgkey).unwrap_or_else(|_| pkgkey.to_string());

        for (trigger_name, activating_packages) in &trigger_activations {
            let mut interested = false;
            let mut interested_await_mode = true; // Default to await

            // Check explicit interests
            if let Some(interested_packages) = explicit_interests.get(trigger_name) {
                // Get await mode from the first interested package entry
                // All entries for the same trigger should have the same await mode
                if let Some((_, await_mode)) = interested_packages.first() {
                    interested = true;
                    interested_await_mode = *await_mode;
                }
            }

            // Check file interests - trigger_name might be a file path
            if !interested {
                for (file_path, _, await_mode) in &file_interests {
                    if file_path == trigger_name {
                        interested = true;
                        interested_await_mode = *await_mode;
                        break;
                    }
                }
            }

            if interested {
                // Add trigger to pending list for this interested package
                packages_with_pending_triggers
                    .entry(pkgname.clone())
                    .or_insert_with(Vec::new)
                    .push(trigger_name.clone());

                // Determine if triggering packages should await
                // Rule: If interested package uses interest-noawait, triggering packages do NOT await
                //       Otherwise, triggering packages await (if they activated with await mode)
                if interested_await_mode {
                    // Interested package requires await - mark triggering packages as awaiting
                    // (unless they used --no-await, which is indicated by "-" in Unincorp)
                    for activating_pkg in activating_packages {
                        if activating_pkg != "-" {
                            packages_that_should_await.insert(activating_pkg.clone());
                        }
                    }
                }
                // If interested_await_mode is false (interest-noawait), triggering packages don't await
            }
        }
    }

    // Clear Unincorp after incorporation
    if unincorp_path.exists() {
        fs::write(&unincorp_path, "")?;
    }

    Ok(TriggerIncorporationResult {
        pending_triggers: packages_with_pending_triggers,
        awaiting_packages: packages_that_should_await,
    })
}

/// Process triggers for a package (call postinst with "triggered" argument)
pub fn process_package_triggers(
    pkgkey: &str,
    package_info: &InstalledPackageInfo,
    trigger_names: &[String],
    store_root: &Path,
    env_root: &Path,
) -> Result<()> {
    let install_dir = store_root.join(&package_info.pkgline).join("info/install");
    let postinst = install_dir.join("post_install.sh");

    if !postinst.exists() {
        log::warn!("Package {} has pending triggers but no postinst script", pkgkey);
        return Ok(());
    }

    // Call postinst with "triggered" and space-separated trigger list
    let trigger_list = trigger_names.join(" ");

    // Get interpreters to try
    let interpreters = crate::scriptlets::get_interpreters_for_script("post_install.sh");

    for interpreter in interpreters {
        let interpreter_path = env_root.join("usr/bin").join(interpreter);
        if !interpreter_path.exists() {
            continue;
        }

        // Prepare script arguments: [script_path, "triggered", "trigger1 trigger2 ..."]
        let script_args = vec![
            postinst.to_string_lossy().to_string(),
            "triggered".to_string(),
            trigger_list.clone(),
        ];

        // Set up environment variables
        let mut env_vars = std::collections::HashMap::new();
        setup_deb_env_vars(&mut env_vars, pkgkey, package_info, crate::scriptlets::ScriptletType::PostInstall, env_root);

        let run_options = crate::run::RunOptions {
            mount_dirs: Vec::new(),
            user: None,
            command: interpreter.to_string(),
            args: script_args,
            env_vars,
            stdin: None,
            no_exit: true,           // Don't exit on scriptlet failures, just warn
            chdir_to_env_root: true, // Scriptlets should run relative to environment root
            skip_namespace_isolation: false,
            timeout: 0,
        };

        log::info!("Processing triggers for package {}: {}", pkgkey, trigger_list);

        // Find interpreter path
        let interpreter_path = env_root.join("usr/bin").join(interpreter);
        match crate::run::fork_and_execute(env_root, &run_options, &interpreter_path) {
            Ok(_) => {
                log::info!("Successfully processed triggers for package {}", pkgkey);
                return Ok(());
            }
            Err(e) => {
                log::warn!("Failed to execute postinst for triggers in package {}: {}", pkgkey, e);
                // Try next interpreter
                continue;
            }
        }
    }

    Err(eyre::eyre!("Failed to execute postinst for triggers: no suitable interpreter found"))
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
