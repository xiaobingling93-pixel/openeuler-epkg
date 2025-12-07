use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use color_eyre::eyre::{Result, Context};
use crate::models::InstalledPackageInfo;
use crate::utils::list_package_files;
use shlex;
use glob::Pattern;

#[derive(Debug, Clone, PartialEq)]
pub enum HookOperation {
    Install,
    Upgrade,
    Remove,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HookType {
    Path,
    Package,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HookWhen {
    PreTransaction,
    PostTransaction,
}

#[derive(Debug, Clone)]
pub struct HookTrigger {
    pub operations: Vec<HookOperation>,
    pub hook_type: HookType,
    pub targets: Vec<String>, // Can contain glob patterns and negations (!)
    pub positive_targets: Vec<String>,
    pub negative_targets: Vec<String>,
    pub positive_patterns: Vec<Pattern>,
    pub negative_patterns: Vec<Pattern>,
    pub type_set: bool,
}

#[derive(Debug, Clone)]
pub struct HookAction {
    pub description: Option<String>,
    pub when: HookWhen,
    pub exec: String,
    pub depends: Vec<String>,
    pub abort_on_fail: bool,
    pub needs_targets: bool,
}

#[derive(Debug, Clone)]
pub struct Hook {
    pub triggers: Vec<HookTrigger>,
    pub action: HookAction,
    pub file_name: String,
}

/// Parse a hook file from disk
/// Reference: _alpm_hook_parse_cb in hook.c
fn parse_hook_file(hook_path: &Path) -> Result<Vec<Hook>> {
    let content = fs::read_to_string(hook_path)
        .with_context(|| format!("Failed to read hook file: {}", hook_path.display()))?;

    let mut hooks = Vec::new();
    let mut current_triggers = Vec::new();
    let mut current_action: Option<HookAction> = None;
    let mut in_trigger = false;
    let mut in_action = false;

    let file_name = hook_path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    let mut line_num = 0;

    for line in content.lines() {
        line_num += 1;
        let line = line.trim();

        // Skip comments and empty lines
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Section headers
        if line == "[Trigger]" {
            in_trigger = true;
            in_action = false;
            // Start a new trigger section
            current_triggers.push(HookTrigger {
                operations: Vec::new(),
                hook_type: HookType::Path, // Default
                targets: Vec::new(),
                positive_targets: Vec::new(),
                negative_targets: Vec::new(),
                positive_patterns: Vec::new(),
                negative_patterns: Vec::new(),
                type_set: false,
            });
            continue;
        } else if line == "[Action]" {
            in_action = true;
            in_trigger = false;
            // If we have an action already, create a hook (allow triggerless hooks)
            if current_action.is_some() {
                hooks.push(Hook {
                    triggers: current_triggers.clone(),
                    action: current_action.clone().unwrap(),
                    file_name: file_name.clone(),
                });
                current_triggers.clear();
            }
            // Start new action
            current_action = Some(HookAction {
                description: None,
                when: HookWhen::PostTransaction, // Default
                exec: String::new(),
                depends: Vec::new(),
                abort_on_fail: false,
                needs_targets: false,
            });
            continue;
        }

        if in_trigger {
            parse_trigger_line(line, &mut current_triggers, hook_path, line_num)?;
        } else if in_action {
            if current_action.is_none() {
                current_action = Some(HookAction {
                    description: None,
                    when: HookWhen::PostTransaction, // Default
                    exec: String::new(),
                    depends: Vec::new(),
                    abort_on_fail: false,
                    needs_targets: false,
                });
            }
            parse_action_line(line, current_action.as_mut().unwrap(), hook_path, line_num)?;
        } else {
            // Invalid: option outside of section
            return Err(color_eyre::eyre::eyre!(
                "hook {} line {}: invalid option {} (not in a section)",
                hook_path.display(), line_num, line
            ));
        }
    }

    // Handle last hook if file doesn't end with empty line
    if current_action.is_some() {
        hooks.push(Hook {
            triggers: current_triggers,
            action: current_action.unwrap(),
            file_name,
        });
    }

    // Validate hooks
    for hook in &mut hooks {
        validate_hook(hook, hook_path)?;
    }

    Ok(hooks)
}

/// Validate a hook (reference: _alpm_hook_validate)
fn validate_hook(hook: &Hook, file: &Path) -> Result<()> {
    // Special case: allow triggerless hooks as a way of creating dummy hooks
    // that can be used to mask lower priority hooks
    if hook.triggers.is_empty() {
        return Ok(());
    }

    // Validate each trigger
    for trigger in &hook.triggers {
        if trigger.targets.is_empty() {
            return Err(color_eyre::eyre::eyre!(
                "Missing trigger targets in hook: {}",
                file.display()
            ));
        }
        if trigger.operations.is_empty() {
            return Err(color_eyre::eyre::eyre!(
                "Missing trigger operation in hook: {}",
                file.display()
            ));
        }
    }

    // Validate action
    if hook.action.exec.is_empty() {
        return Err(color_eyre::eyre::eyre!(
            "Missing Exec option in hook: {}",
            file.display()
        ));
    }

    // When defaults to PostTransaction, so we check if it's still the default
    // Actually, we require it to be explicitly set
    // But the reference allows default, so we'll just warn about AbortOnFail
    if hook.action.when == HookWhen::PostTransaction && hook.action.abort_on_fail {
        log::warn!(
            "AbortOnFail set for PostTransaction hook: {}",
            file.display()
        );
    }

    Ok(())
}

fn parse_trigger_line(line: &str, triggers: &mut Vec<HookTrigger>, file: &Path, line_num: usize) -> Result<()> {
    if let Some((key, value)) = line.split_once('=') {
        let key = key.trim();
        let value = value.trim();

        if triggers.is_empty() {
            triggers.push(HookTrigger {
                operations: Vec::new(),
                hook_type: HookType::Path,
                targets: Vec::new(),
                positive_targets: Vec::new(),
                negative_targets: Vec::new(),
                positive_patterns: Vec::new(),
                negative_patterns: Vec::new(),
                type_set: false,
            });
        }

        let trigger = triggers.last_mut().unwrap();

        match key {
            "Operation" => {
                let operation = match value {
                    "Install" => HookOperation::Install,
                    "Upgrade" => HookOperation::Upgrade,
                    "Remove" => HookOperation::Remove,
                    _ => return Err(color_eyre::eyre::eyre!(
                        "hook {} line {}: invalid value {}",
                        file.display(), line_num, value
                    )),
                };

                if !trigger.operations.contains(&operation) {
                    trigger.operations.push(operation);
                }
            }
            "Type" => {
                // Warn if overwriting (pacman behavior)
                if trigger.type_set {
                    log::warn!("hook {} line {}: overwriting previous definition of Type", file.display(), line_num);
                }
                trigger.type_set = true;
                trigger.hook_type = match value {
                    "Path" | "File" => HookType::Path,
                    "Package" => HookType::Package,
                    _ => return Err(color_eyre::eyre::eyre!(
                        "hook {} line {}: invalid value {}",
                        file.display(), line_num, value
                    )),
                };
            }
            "Target" => {
                trigger.targets.push(value.to_string());
            }
            _ => {
                return Err(color_eyre::eyre::eyre!(
                    "hook {} line {}: invalid option {}",
                    file.display(), line_num, key
                ));
            }
        }
    } else {
        return Err(color_eyre::eyre::eyre!(
            "hook {} line {}: invalid option {}",
            file.display(), line_num, line
        ));
    }
    Ok(())
}

fn split_hook_targets(targets: &[String]) -> (Vec<String>, Vec<String>) {
    let mut positive_targets = Vec::new();
    let mut negative_targets = Vec::new();

    for target in targets {
        if let Some(stripped) = target.strip_prefix('!') {
            negative_targets.push(stripped.to_string());
        } else {
            positive_targets.push(target.clone());
        }
    }

    (positive_targets, negative_targets)
}

fn populate_hook_target_caches(hook: &mut Hook) {
    for trigger in &mut hook.triggers {
        let (positive_targets, negative_targets) = split_hook_targets(&trigger.targets);
        trigger.positive_patterns = compile_patterns(&positive_targets);
        trigger.negative_patterns = compile_patterns(&negative_targets);
        trigger.positive_targets = positive_targets;
        trigger.negative_targets = negative_targets;
    }
}

fn parse_action_line(line: &str, action: &mut HookAction, file: &Path, line_num: usize) -> Result<()> {
    if let Some((key, value)) = line.split_once('=') {
        let key = key.trim();
        let value = value.trim();

        match key {
            "When" => {
                // Warn if overwriting (pacman behavior)
                if action.when != HookWhen::PostTransaction {
                    log::warn!("hook {} line {}: overwriting previous definition of When", file.display(), line_num);
                }
                action.when = match value {
                    "PreTransaction" => HookWhen::PreTransaction,
                    "PostTransaction" => HookWhen::PostTransaction,
                    _ => return Err(color_eyre::eyre::eyre!(
                        "hook {} line {}: invalid value {}",
                        file.display(), line_num, value
                    )),
                };
            }
            "Description" => {
                // Warn if overwriting (pacman behavior)
                if action.description.is_some() {
                    log::warn!("hook {} line {}: overwriting previous definition of Description", file.display(), line_num);
                }
                action.description = Some(value.to_string());
            }
            "Depends" => {
                action.depends.push(value.to_string());
            }
            "Exec" => {
                // Warn if overwriting (pacman behavior)
                if !action.exec.is_empty() {
                    log::warn!("hook {} line {}: overwriting previous definition of Exec", file.display(), line_num);
                }
                action.exec = value.to_string();
            }
            _ => {
                return Err(color_eyre::eyre::eyre!(
                    "hook {} line {}: invalid option {}",
                    file.display(), line_num, key
                ));
            }
        }
    } else {
        // Boolean flags without values
        match line {
            "AbortOnFail" => {
                action.abort_on_fail = true;
            }
            "NeedsTargets" => {
                action.needs_targets = true;
            }
            _ => {
                return Err(color_eyre::eyre::eyre!(
                    "hook {} line {}: invalid option {}",
                    file.display(), line_num, line
                ));
            }
        }
    }
    Ok(())
}

/// Load all hooks from the system hook directory
/// Reference: _alpm_hook_run - scans directories in reverse order, hooks with same name override
pub fn load_hooks(env_root: &Path) -> Result<Vec<Hook>> {
    let mut hooks: HashMap<String, Hook> = HashMap::new(); // Map by file name for overriding

    // Standard hook directories (reference scans in reverse order)
    let hook_dirs = vec![
        env_root.join("usr/share/libalpm/hooks"),
        env_root.join("etc/pacman.d/hooks"),
    ];

    // Process directories in reverse order (last directory overrides first)
    for hook_dir in hook_dirs.iter().rev() {
        if !hook_dir.exists() {
            continue;
        }

        let mut entries: Vec<_> = fs::read_dir(hook_dir)
            .with_context(|| format!("Failed to read hook directory: {}", hook_dir.display()))?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        // Sort entries by name for consistent processing
        entries.sort_by_key(|e| e.file_name());

        for entry in entries {
            let path = entry.path();

            // Only process .hook files
            if path.extension().and_then(|e| e.to_str()) != Some("hook") {
                log::debug!("skipping non-hook file {}", path.display());
                continue;
            }

            let file_name = path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string();

            // Check if hook with same name already exists (skip if so - override behavior)
            if hooks.contains_key(&file_name) {
                log::debug!("skipping overridden hook {}", path.display());
                continue;
            }

            // Skip symlinks to /dev/null (disabled hooks)
            if let Ok(link_target) = fs::read_link(&path) {
                if link_target == PathBuf::from("/dev/null") {
                    log::debug!("Skipping disabled hook: {}", path.display());
                    continue;
                }
            }

            // Check if it's a directory (skip)
            if path.is_dir() {
                log::debug!("skipping directory {}", path.display());
                continue;
            }

            match parse_hook_file(&path) {
                Ok(file_hooks) => {
                    // For now, we only support one hook per file (first one)
                    // The reference implementation also creates one hook per file
                    if let Some(mut hook) = file_hooks.first().cloned() {
                        populate_hook_target_caches(&mut hook);
                        hooks.insert(file_name, hook);
                    }
                }
                Err(e) => {
                    log::warn!("Failed to parse hook file {}: {}", path.display(), e);
                }
            }
        }
    }

    // Convert to Vec and sort by file name (reference: _alpm_hook_cmp)
    let mut hooks_vec: Vec<Hook> = hooks.into_values().collect();
    hooks_vec.sort_by(|a, b| {
        // Custom comparison: exclude .hook suffix from comparison
        let suflen = ".hook".len();
        let a_name = &a.file_name;
        let b_name = &b.file_name;

        let a_len = if a_name.len() >= suflen && a_name.ends_with(".hook") {
            a_name.len() - suflen
        } else {
            a_name.len()
        };
        let b_len = if b_name.len() >= suflen && b_name.ends_with(".hook") {
            b_name.len() - suflen
        } else {
            b_name.len()
        };

        let a_prefix = &a_name[..a_len.min(a_name.len())];
        let b_prefix = &b_name[..b_len.min(b_name.len())];

        let ret = a_prefix.cmp(b_prefix);
        if ret == std::cmp::Ordering::Equal && a_len != b_len {
            if a_len < b_len {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            }
        } else {
            ret
        }
    });

    Ok(hooks_vec)
}

/// Check if any compiled pattern matches a string
fn matches_any_pattern(text: &str, patterns: &[Pattern]) -> bool {
    patterns.iter().any(|pattern| pattern.matches(text))
}

/// Check if text matches positive patterns but not negative patterns (using compiled patterns)
fn matches_patterns(
    text: &str,
    positive_patterns: &[Pattern],
    negative_patterns: &[Pattern],
) -> bool {
    matches_any_pattern(text, positive_patterns)
        && !matches_any_pattern(text, negative_patterns)
}

/// Compile string patterns into Pattern objects, filtering out invalid ones
fn compile_patterns(patterns: &[String]) -> Vec<Pattern> {
    patterns.iter()
        .filter_map(|p| Pattern::new(p).ok())
        .collect()
}

/// Collect package files matching trigger targets into the provided buffer.
fn collect_matching_package_files<'a, I>(
    packages: I,
    store_root: &Path,
    positive_patterns: &[Pattern],
    negative_patterns: &[Pattern],
    output: &mut Vec<String>,
) -> Result<()>
where
    I: Iterator<Item = &'a InstalledPackageInfo>,
{
    for info in packages {
        let files = get_package_files(store_root, info)?;
        for file in &files {
            let file_str = file.to_string_lossy();
            if matches_patterns(&file_str, positive_patterns, negative_patterns) {
                output.push(file_str.to_string());
            }
        }
    }

    Ok(())
}

/// Match Path trigger (reference: _alpm_hook_trigger_match_file)
/// Returns (matched, aggregated_targets)
fn match_path_trigger(
    trigger: &HookTrigger,
    fresh_installs: &HashMap<String, InstalledPackageInfo>,
    upgrades_new: &HashMap<String, InstalledPackageInfo>,
    upgrades_old: &HashMap<String, InstalledPackageInfo>,
    old_removes: &HashMap<String, InstalledPackageInfo>,
    store_root: &Path,
    needs_targets: bool,
) -> Result<(bool, Vec<String>)> {
    // If there are no positive targets, we can't match
    if trigger.positive_targets.is_empty() {
        return Ok((false, Vec::new()));
    }

    let wants_install = trigger.operations.contains(&HookOperation::Install);
    let wants_upgrade = trigger.operations.contains(&HookOperation::Upgrade);
    let wants_remove = trigger.operations.contains(&HookOperation::Remove);

    if !needs_targets {
        let matched = match_path_trigger_no_targets(
            fresh_installs,
            upgrades_new,
            upgrades_old,
            old_removes,
            store_root,
            &trigger.positive_patterns,
            &trigger.negative_patterns,
            wants_install,
            wants_upgrade,
            wants_remove,
        )?;
        return Ok((matched, Vec::new()));
    }

    match_path_trigger_with_targets(
        fresh_installs,
        upgrades_new,
        upgrades_old,
        old_removes,
        store_root,
        &trigger.positive_patterns,
        &trigger.negative_patterns,
        wants_install,
        wants_upgrade,
        wants_remove,
    )
}

/// Fast-path match for path triggers when targets are not needed. Short-circuits
/// as soon as any matching condition is detected to avoid full set construction.
fn match_path_trigger_no_targets(
    fresh_installs: &HashMap<String, InstalledPackageInfo>,
    upgrades_new: &HashMap<String, InstalledPackageInfo>,
    upgrades_old: &HashMap<String, InstalledPackageInfo>,
    old_removes: &HashMap<String, InstalledPackageInfo>,
    store_root: &Path,
    positive_patterns: &[Pattern],
    negative_patterns: &[Pattern],
    wants_install: bool,
    wants_upgrade: bool,
    wants_remove: bool,
) -> Result<bool> {
    let file_matches = |packages: &HashMap<String, InstalledPackageInfo>| -> Result<bool> {
        for info in packages.values() {
            for file in get_package_files(store_root, info)? {
                let file_str = file.to_string_lossy();
                if matches_patterns(&file_str, &positive_patterns, &negative_patterns) {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    };

    if wants_install && file_matches(fresh_installs)? {
        return Ok(true);
    }

    if wants_remove && file_matches(old_removes)? {
        return Ok(true);
    }

    // For upgrades and upgrade-related diffs we need to compare old/new sets, but still
    // return early as soon as a decisive condition is found.
    let mut upgrades_old_set = HashSet::new();
    if wants_upgrade || wants_remove || wants_install {
        for info in upgrades_old.values() {
            for file in get_package_files(store_root, info)? {
                let file_str = file.to_string_lossy();
                if matches_patterns(&file_str, &positive_patterns, &negative_patterns) {
                    upgrades_old_set.insert(file_str.to_string());
                }
            }
        }
    }

    let mut upgrades_new_set = HashSet::new();
    if wants_upgrade || wants_remove || wants_install {
        for info in upgrades_new.values() {
            for file in get_package_files(store_root, info)? {
                let file_str = file.to_string_lossy();
                if matches_patterns(&file_str, &positive_patterns, &negative_patterns) {
                    let file_str = file_str.to_string();
                    // Upgrade match: found in both old and new
                    if wants_upgrade && upgrades_old_set.contains(&file_str) {
                        return Ok(true);
                    }
                    // Install via upgrade addition
                    if wants_install && !upgrades_old_set.contains(&file_str) {
                        return Ok(true);
                    }
                    upgrades_new_set.insert(file_str);
                }
            }
        }
    }

    // Remove via upgrade disappearance: any old file missing from new set.
    if wants_remove {
        for old_file in &upgrades_old_set {
            if !upgrades_new_set.contains(old_file) {
                return Ok(true);
            }
        }
    }

    Ok(false)
}

/// Full match path trigger when targets are required; collects and returns them.
fn match_path_trigger_with_targets(
    fresh_installs: &HashMap<String, InstalledPackageInfo>,
    upgrades_new: &HashMap<String, InstalledPackageInfo>,
    upgrades_old: &HashMap<String, InstalledPackageInfo>,
    old_removes: &HashMap<String, InstalledPackageInfo>,
    store_root: &Path,
    positive_patterns: &[Pattern],
    negative_patterns: &[Pattern],
    wants_install: bool,
    wants_upgrade: bool,
    wants_remove: bool,
) -> Result<(bool, Vec<String>)> {
    let mut matched_targets = Vec::new();

    let mut fresh_install_files = Vec::new();
    let mut upgrades_new_files = Vec::new();
    let mut upgrades_old_files = Vec::new();
    let mut old_remove_files = Vec::new();

    if wants_install || wants_upgrade {
        collect_matching_package_files(
            fresh_installs.values(),
            store_root,
            positive_patterns,
            negative_patterns,
            &mut fresh_install_files,
        )?;
    }

    if wants_install || wants_upgrade || wants_remove {
        collect_matching_package_files(
            upgrades_new.values(),
            store_root,
            positive_patterns,
            negative_patterns,
            &mut upgrades_new_files,
        )?;

        collect_matching_package_files(
            upgrades_old.values(),
            store_root,
            positive_patterns,
            negative_patterns,
            &mut upgrades_old_files,
        )?;
    }

    if wants_remove {
        collect_matching_package_files(
            old_removes.values(),
            store_root,
            positive_patterns,
            negative_patterns,
            &mut old_remove_files,
        )?;
    }

    let fresh_install_set: HashSet<_> = fresh_install_files.into_iter().collect();
    let upgrades_new_set: HashSet<_> = upgrades_new_files.into_iter().collect();
    let upgrades_old_set: HashSet<_> = upgrades_old_files.into_iter().collect();
    let old_remove_set: HashSet<_> = old_remove_files.into_iter().collect();

    if wants_install {
        matched_targets.extend(fresh_install_set.iter().cloned());
        matched_targets.extend(upgrades_new_set.difference(&upgrades_old_set).cloned());
    }

    if wants_remove {
        matched_targets.extend(old_remove_set.iter().cloned());
        matched_targets.extend(upgrades_old_set.difference(&upgrades_new_set).cloned());
    }

    if wants_upgrade {
        matched_targets.extend(upgrades_old_set.intersection(&upgrades_new_set).cloned());
    }

    let matched = !matched_targets.is_empty();
    Ok((matched, matched_targets))
}

/// Match Package trigger (reference: _alpm_hook_trigger_match_pkg)
fn match_package_trigger(
    trigger: &HookTrigger,
    fresh_installs: &HashMap<String, InstalledPackageInfo>,
    upgrades_new: &HashMap<String, InstalledPackageInfo>,
    _upgrades_old: &HashMap<String, InstalledPackageInfo>,
    old_removes: &HashMap<String, InstalledPackageInfo>,
) -> Result<(bool, Vec<String>, Vec<String>, Vec<String>)> {
    let mut install_pkgs = Vec::new();
    let mut upgrade_pkgs = Vec::new();
    let mut remove_pkgs = Vec::new();

    if trigger.positive_targets.is_empty() {
        return Ok((false, Vec::new(), Vec::new(), Vec::new()));
    }

    // Check install/upgrade operations
    if trigger.operations.contains(&HookOperation::Install) || trigger.operations.contains(&HookOperation::Upgrade) {
        for (pkgkey, _) in fresh_installs {
            if let Ok(pkgname) = crate::package::pkgkey2pkgname(pkgkey) {
                if matches_patterns(&pkgname, &trigger.positive_patterns, &trigger.negative_patterns) {
                    if trigger.operations.contains(&HookOperation::Install) {
                        install_pkgs.push(pkgname);
                    }
                }
            }
        }

        for (pkgkey, _) in upgrades_new {
            if let Ok(pkgname) = crate::package::pkgkey2pkgname(pkgkey) {
                if matches_patterns(&pkgname, &trigger.positive_patterns, &trigger.negative_patterns) {
                    if trigger.operations.contains(&HookOperation::Upgrade) {
                        upgrade_pkgs.push(pkgname);
                    }
                }
            }
        }
    }

    // Check remove operations (reference: excludes packages being upgraded)
    if trigger.operations.contains(&HookOperation::Remove) {
        for (pkgkey, _) in old_removes {
            // Exclude packages that are being upgraded (reference: checks if in add list)
            if upgrades_new.contains_key(pkgkey) {
                continue;
            }

            if let Ok(pkgname) = crate::package::pkgkey2pkgname(pkgkey) {
                if matches_patterns(&pkgname, &trigger.positive_patterns, &trigger.negative_patterns) {
                    remove_pkgs.push(pkgname);
                }
            }
        }
    }

    let matched = (trigger.operations.contains(&HookOperation::Install) && !install_pkgs.is_empty())
        || (trigger.operations.contains(&HookOperation::Upgrade) && !upgrade_pkgs.is_empty())
        || (trigger.operations.contains(&HookOperation::Remove) && !remove_pkgs.is_empty());

    Ok((matched, install_pkgs, upgrade_pkgs, remove_pkgs))
}

/// Get all files from a package
fn get_package_files(
    store_root: &Path,
    package_info: &InstalledPackageInfo,
) -> Result<Vec<PathBuf>> {
    let store_fs_dir = store_root.join(&package_info.pkgline).join("fs");
    if !store_fs_dir.exists() {
        return Ok(Vec::new());
    }

    let files = list_package_files(store_fs_dir.to_str()
        .ok_or_else(|| color_eyre::eyre::eyre!("Invalid store fs path"))?)?;

    // Convert to relative paths (without leading /)
    Ok(files.iter()
        .filter_map(|p| p.strip_prefix(&store_fs_dir).ok())
        .map(|p| p.to_path_buf())
        .collect())
}

/// Check if a package dependency is satisfied
/// Reference: _alpm_hook_run_hook uses alpm_find_satisfier
fn check_dependency(
    installed_packages: &HashMap<String, InstalledPackageInfo>,
    fresh_installs: &HashMap<String, InstalledPackageInfo>,
    dep: &str,
) -> bool {
    // Check installed and freshly installed package names for the dependency
    // This is a simplified check - in full implementation we'd need to check
    // provides, version constraints, etc.
    fn pkgkey_matches_dep(pkgkey: &str, dep: &str) -> bool {
        matches!(crate::package::pkgkey2pkgname(pkgkey), Ok(pkgname) if pkgname == dep)
    }

    installed_packages.keys().any(|pkgkey| pkgkey_matches_dep(pkgkey, dep))
        || fresh_installs.keys().any(|pkgkey| pkgkey_matches_dep(pkgkey, dep))
}

/// Execute a hook
/// Reference: _alpm_hook_run_hook
fn execute_hook(
    hook: &Hook,
    env_root: &Path,
    matched_targets: &[String],
    installed_packages: &HashMap<String, InstalledPackageInfo>,
    fresh_installs: &HashMap<String, InstalledPackageInfo>,
) -> Result<()> {
    // Check dependencies (reference: checks before execution)
    for dep in &hook.action.depends {
        if !check_dependency(installed_packages, fresh_installs, dep) {
            return Err(color_eyre::eyre::eyre!(
                "unable to run hook {}: could not satisfy dependencies",
                hook.file_name
            ));
        }
    }

    // Parse exec command using shlex (reference: wordsplit)
    let exec_parts = match shlex::split(&hook.action.exec) {
        Some(parts) => {
            if parts.is_empty() {
                return Err(color_eyre::eyre::eyre!("Empty Exec in hook {}", hook.file_name));
            }
            parts
        }
        None => {
            return Err(color_eyre::eyre::eyre!(
                "hook {}: invalid Exec value {}",
                hook.file_name, hook.action.exec
            ));
        }
    };

    let command = &exec_parts[0];
    let args = &exec_parts[1..];

    // Build command path (reference: _alpm_run_chroot handles path resolution)
    let command_path = if command.starts_with('/') {
        env_root.join(command.strip_prefix('/').unwrap_or(command))
    } else {
        // Try common paths
        let common_paths = vec![
            env_root.join("usr/bin").join(command),
            env_root.join("usr/sbin").join(command),
            env_root.join("bin").join(command),
            env_root.join("sbin").join(command),
        ];

        common_paths.iter()
            .find(|p| p.exists())
            .cloned()
            .unwrap_or_else(|| env_root.join("usr/bin").join(command))
    };

    log::info!("Executing hook {}: {}", hook.file_name, hook.action.exec);

    let env_vars = std::collections::HashMap::new();
    let stdin_data = if hook.action.needs_targets {
        Some(matched_targets.join("\n").into_bytes())
    } else {
        None
    };

    // Execute the hook
    let run_options = crate::run::RunOptions {
        mount_dirs: Vec::new(),
        user: None,
        command: command.to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
        env_vars,
        stdin: stdin_data,
        no_exit: !hook.action.abort_on_fail,
        chdir_to_env_root: true,
        skip_namespace_isolation: false,
        timeout: 300, // 5 minute timeout for hooks
        builtin: false,
    };

    match crate::run::fork_and_execute(env_root, &run_options, &command_path) {
        Ok(()) => {
            log::debug!("Hook {} executed successfully", hook.file_name);
            Ok(())
        }
        Err(e) => {
            if hook.action.abort_on_fail {
                Err(e).with_context(|| format!("Hook {} failed and AbortOnFail is set", hook.file_name))
            } else {
                log::warn!("Hook {} failed: {}", hook.file_name, e);
                Ok(())
            }
        }
    }
}

/// Check if a Path hook trigger has any matching file modified after `cutoff`.
/// This is used to trim the number of triggers we evaluate when a transaction
/// touches many packages.
fn path_trigger_has_recent_match(
    trigger: &HookTrigger,
    env_root: &Path,
    cutoff: SystemTime,
) -> Result<bool> {
    if trigger.positive_targets.is_empty() {
        return Ok(false);
    }

    for target in &trigger.positive_targets {
        if target_has_recent_match(target, env_root, cutoff)? {
            return Ok(true);
        }
    }

    Ok(false)
}

fn target_has_recent_match(target: &str, env_root: &Path, cutoff: SystemTime) -> Result<bool> {
    let parent_prefix = parent_prefix_before_wildcard(target);

    // Targets are relative to env_root; strip a leading slash to avoid
    // escaping the environment root.
    let absolute_pattern = if target.starts_with('/') {
        env_root.join(&target[1..])
    } else {
        env_root.join(target)
    };

    let pattern_str = absolute_pattern.to_string_lossy();

    if let Some(parent) = parent_prefix {
        let absolute_parent = if parent.starts_with('/') {
            env_root.join(&parent[1..])
        } else {
            env_root.join(&parent)
        };

        match absolute_parent.metadata().and_then(|m| m.modified()) {
            Ok(modified) if modified >= cutoff => return Ok(true),
            Ok(_) => {}
            Err(e) => {
                log::debug!(
                    "failed to read mtime for parent {}: {}",
                    absolute_parent.display(),
                    e
                );
            }
        }
    }

    match glob::glob(&pattern_str) {
        Ok(paths) => {
            for path_result in paths {
                let path = match path_result {
                    Ok(p) => p,
                    Err(e) => {
                        log::debug!("path trigger glob error for {}: {}", pattern_str, e);
                        continue;
                    }
                };

                match path.metadata().and_then(|m| m.modified()) {
                    Ok(modified) if modified >= cutoff => return Ok(true),
                    Ok(_) => {}
                    Err(e) => {
                        log::debug!("failed to read mtime for {}: {}", path.display(), e);
                    }
                }
            }
        }
        Err(e) => {
            log::debug!("failed to expand glob {}: {}", pattern_str, e);
        }
    }

    Ok(false)
}

fn parent_prefix_before_wildcard(target: &str) -> Option<String> {
    if let Some(star_idx) = target.find('*') {
        if let Some(slash_idx) = target[..star_idx].rfind('/') {
            let prefix = &target[..=slash_idx]; // include trailing slash
            if !prefix.is_empty() {
                return Some(prefix.to_string());
            }
        }
    }
    None
}

/// Run hooks for a transaction
/// Reference: _alpm_hook_run
pub fn run_hooks(
    hooks: &[Hook],
    env_root: &Path,
    store_root: &Path,
    when: HookWhen,
    fresh_installs: &HashMap<String, InstalledPackageInfo>,
    upgrades_new: &HashMap<String, InstalledPackageInfo>,
    upgrades_old: &HashMap<String, InstalledPackageInfo>,
    old_removes: &HashMap<String, InstalledPackageInfo>,
    installed_packages: &HashMap<String, InstalledPackageInfo>,
) -> Result<()> {
    let should_reduce_path_triggers = fresh_installs.len() + upgrades_new.len() >= 20;
    let recent_cutoff = if should_reduce_path_triggers {
        SystemTime::now()
            .checked_sub(Duration::from_secs(3600))
            .unwrap_or(SystemTime::UNIX_EPOCH)
    } else {
        SystemTime::UNIX_EPOCH
    };

    // Filter hooks by When
    let relevant_hooks: Vec<&Hook> = hooks.iter()
        .filter(|h| h.action.when == when)
        .collect();

    if relevant_hooks.is_empty() {
        return Ok(());
    }

    // Find triggered hooks (reference: _alpm_hook_triggered)
    let mut triggered_hooks = Vec::new();

    for hook in relevant_hooks {
        // Special case: triggerless hooks are never triggered
        if hook.triggers.is_empty() {
            continue;
        }

        let mut hook_matched = false;
        let mut all_matched_targets = Vec::new();

        for trigger in &hook.triggers {
            if should_reduce_path_triggers && trigger.hook_type == HookType::Path {
                // Skip expensive trigger evaluation when none of its target files
                // were touched recently.
                if !path_trigger_has_recent_match(trigger, env_root, recent_cutoff)? {
                    continue;
                }
            }

            let (matched, matched_targets) = match trigger.hook_type {
                HookType::Path => {
                    match_path_trigger(
                        trigger,
                        fresh_installs,
                        upgrades_new,
                        upgrades_old,
                        old_removes,
                        store_root,
                        hook.action.needs_targets,
                    )?
                }
                HookType::Package => {
                    let (pmatched, install_targets, upgrade_targets, remove_targets) =
                        match_package_trigger(trigger, fresh_installs, upgrades_new, upgrades_old, old_removes)?;

                    if pmatched && hook.action.needs_targets {
                        all_matched_targets.extend(install_targets);
                        all_matched_targets.extend(upgrade_targets);
                        all_matched_targets.extend(remove_targets);
                    }

                    (pmatched, Vec::new())
                }
            };

            if matched {
                hook_matched = true;

                // Path trigger already handles needs_targets internally. For package triggers,
                // targets were added above when needed.
                if hook.action.needs_targets {
                    all_matched_targets.extend(matched_targets);
                } else {
                    break;
                }
            }
        }

        if hook_matched {
            // Sort and deduplicate targets (reference: _alpm_strlist_dedup)
            all_matched_targets.sort();
            all_matched_targets.dedup();

            triggered_hooks.push((hook, all_matched_targets));
        }
    }

    // Execute triggered hooks (reference: executes in order)
    for (hook, matched_targets) in triggered_hooks {
        log::info!("running '{}'...", hook.file_name);

        if let Err(e) = execute_hook(
            hook,
            env_root,
            &matched_targets,
            installed_packages,
            fresh_installs,
        ) {
            if hook.action.abort_on_fail {
                return Err(e).with_context(|| format!("failed to run transaction hooks"));
            }
        }

        // If PreTransaction and error occurred, stop (pacman behavior)
        if when == HookWhen::PreTransaction {
            // Error already handled above
        }
    }

    Ok(())
}
