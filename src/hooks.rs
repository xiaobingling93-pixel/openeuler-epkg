use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use color_eyre::eyre::{Result, Context};
use crate::models::PackageFormat;
use crate::package::pkgkey2pkgname;
use crate::models::PACKAGE_CACHE;
use crate::utils::get_package_files;
use crate::plan::{InstallationPlan, pkgkey2pkgline};
use shlex;
use glob::Pattern;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum HookOperation {
    Install = 1 << 0,
    Upgrade = 1 << 1,
    Remove = 1 << 2,
}

impl HookOperation {
    /// Convert to bit flag value
    #[inline]
    pub fn as_flag(self) -> u8 {
        self as u8
    }
}

/// Trait extension for u8 to check if a HookOperation flag is set
trait HookOperationFlags {
    fn is_set(self, op: HookOperation) -> bool;
}

impl HookOperationFlags for u8 {
    /// Check if a bit flag is set
    #[inline]
    fn is_set(self, op: HookOperation) -> bool {
        (self & op.as_flag()) != 0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum HookType {
    Path,
    Package,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HookWhen {
    PreTransaction,
    PostTransaction,
    PostUnTrans, // RPM
    // Per-package phases (used for Arch-style hooks and RPM/Deb mapping)
    PreInstall,
    PreInstall2,
    PostInstall,
    PostInstall2,
    PreRemove,
    PreRemove2,
    PostRemove,
    PostRemove2,
    PreUpgrade,
    PostUpgrade,
}

#[derive(Debug, Clone)]
pub struct HookTrigger {
    pub operations: u8, // Bit flags for HookOperation
    pub hook_type: HookType,
    pub targets: Vec<String>, // Can contain glob patterns and negations (!)
    pub positive_targets: Vec<String>,
    pub negative_targets: Vec<String>,
    pub positive_patterns: Vec<Pattern>,
    pub negative_patterns: Vec<Pattern>,
    pub type_set: bool, // for input validation
}

#[derive(Debug, Clone)]
pub struct HookAction {
    pub description: Option<String>,
    pub when: HookWhen,
    pub exec: String,
    pub depends: Vec<String>,
    pub abort_on_fail: bool,
    pub needs_targets: bool,
    pub priority: u32,
}

#[derive(Debug, Clone)]
pub struct Hook {
    pub triggers: Vec<HookTrigger>,
    pub action: HookAction,
    pub hook_name: String,  // File name without .hook suffix, used as lookup/sort key
    pub file_path: String,  // Full path to the hook file, used for log messages
    pub pkgkey: Option<String>,  // Package key if this hook belongs to a package, None for global hooks
}

/// Extract file name from path and strip .hook suffix
fn extract_hook_file_name(hook_path: &Path) -> String {
    let file_name_full = hook_path.file_name()
        .and_then(|n| n.to_str())
        .unwrap()
        .to_string();
    file_name_full.strip_suffix(".hook").unwrap_or(&file_name_full).to_string()
}

/// Parse a hook file from disk
/// Reference: _alpm_hook_parse_cb in hook.c
fn parse_hook_file(hook_path: &Path) -> Result<Hook> {
    let content = fs::read_to_string(hook_path)
        .with_context(|| format!("Failed to read hook file: {}", hook_path.display()))?;

    let mut current_triggers = Vec::new();
    let mut current_action: Option<HookAction> = None;
    let mut in_trigger = false;
    let mut in_action = false;

    let hook_name = extract_hook_file_name(hook_path);

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
                operations: 0,
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
            // Start new action
            current_action = Some(HookAction {
                description: None,
                when: HookWhen::PostTransaction, // Default
                exec: String::new(),
                depends: Vec::new(),
                abort_on_fail: false,
                needs_targets: false,
                priority: 1_000_000,
            });
            continue;
        }

        if in_trigger {
            parse_trigger_line(line, &mut current_triggers, hook_path, line_num)?;
        } else if in_action {
            parse_action_line(line, current_action.as_mut().unwrap(), hook_path, line_num)?;
        } else {
            // Invalid: option outside of section
            return Err(color_eyre::eyre::eyre!(
                "hook {} line {}: invalid option {} (not in a section)",
                hook_path.display(), line_num, line
            ));
        }
    }

    let hook = Hook {
        triggers: current_triggers,
        action: current_action.unwrap(),
        hook_name,
        file_path: hook_path.to_string_lossy().to_string(),
        pkgkey: None,  // Will be set in register_hook_to_plan() if this is a package hook
    };

    validate_hook(&hook, hook_path)?;

    Ok(hook)
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
        if trigger.operations == 0 {
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

                trigger.operations |= operation.as_flag();
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

fn populate_hook_target_cache(hook: &mut Hook) {
    for trigger in &mut hook.triggers {
        let (positive_targets, negative_targets) = split_hook_targets(&trigger.targets);
        trigger.positive_patterns = compile_patterns(&positive_targets);
        trigger.negative_patterns = compile_patterns(&negative_targets);
        trigger.positive_targets = positive_targets;
        trigger.negative_targets = negative_targets;
    }
}

/// Compile string patterns into Pattern objects, filtering out invalid ones
fn compile_patterns(patterns: &[String]) -> Vec<Pattern> {
    patterns.iter()
        .filter_map(|p| Pattern::new(p).ok())
        .collect()
}

fn parse_when_value(raw: &str, file: &Path, line_num: usize) -> Result<HookWhen> {
    let v = raw.trim();
    let when = match v {
        // Arch / generic
        "PreTransaction"    => HookWhen::PreTransaction,
        "PostTransaction"   => HookWhen::PostTransaction,
        // RPM-style aliases for transaction phases
        "PostUnTrans"       => HookWhen::PostUnTrans,
        // Per-package phases (used for RPM/Deb/Arch mapping)
        "PreInstall"        => HookWhen::PreInstall,
        "PreInstall2"       => HookWhen::PreInstall2,
        "PostInstall"       => HookWhen::PostInstall,
        "PostInstall2"      => HookWhen::PostInstall2,
        "PreRemove"         => HookWhen::PreRemove,
        "PreRemove2"        => HookWhen::PreRemove2,
        "PostRemove"        => HookWhen::PostRemove,
        "PostRemove2"       => HookWhen::PostRemove2,
        "PreUpgrade"        => HookWhen::PreUpgrade,
        "PostUpgrade"       => HookWhen::PostUpgrade,
        _ => {
            return Err(color_eyre::eyre::eyre!(
                "hook {} line {}: invalid When value {}",
                file.display(),
                line_num,
                raw
            ));
        }
    };
    Ok(when)
}

fn parse_action_line(line: &str, action: &mut HookAction, file: &Path, line_num: usize) -> Result<()> {
    if let Some((key, value)) = line.split_once('=') {
        let key = key.trim();
        let value = value.trim();

        match key {
            "When" => {
                // Warn if overwriting (pacman behavior)
                if action.when != HookWhen::PostTransaction {
                    log::warn!(
                        "hook {} line {}: overwriting previous definition of When",
                        file.display(),
                        line_num
                    );
                }
                action.when = parse_when_value(value, file, line_num)?;
            }
            "Description" => {
                // Warn if overwriting (pacman behavior)
                if action.description.is_some() {
                    log::warn!("hook {} line {}: overwriting previous definition of Description", file.display(), line_num);
                }
                action.description = Some(value.to_string());
            }
            "Depends" => {
                action.depends.extend(
                    value.split_whitespace().map(|s| s.to_string())
                );
            }
            "Exec" => {
                // Warn if overwriting (pacman behavior)
                if !action.exec.is_empty() {
                    log::warn!(
                        "hook {} line {}: overwriting previous definition of Exec",
                        file.display(),
                        line_num
                    );
                }
                action.exec = value.to_string();
            }
            "Priority" => {
                let prio = value.parse::<u32>().map_err(|e| {
                    color_eyre::eyre::eyre!(
                        "hook {} line {}: invalid Priority {} ({})",
                        file.display(),
                        line_num,
                        value,
                        e
                    )
                })?;
                action.priority = prio;
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


/// Read and sort entries from a hook directory
fn read_hook_directory_entries(hook_dir: &Path) -> Option<Vec<fs::DirEntry>> {
    let entries: Vec<_> = match fs::read_dir(hook_dir) {
        Ok(dir) => match dir.collect::<std::result::Result<Vec<_>, _>>() {
            Ok(entries) => entries,
            Err(e) => {
                log::warn!("Failed to read hook directory entries: {}", e);
                return None;
            }
        },
        Err(e) => {
            log::warn!("Failed to read hook directory {}: {}", hook_dir.display(), e);
            return None;
        }
    };

    Some(entries)
}

/// Update an existing global hook with a package pkgkey
/// Returns true if the hook was updated (and caller should return early), false otherwise
fn update_existing_hook_pkgkey(
    hook_name: &str,
    pkgkey: &str,
    path: &Path,
    plan: &mut InstallationPlan,
) -> bool {
    // Get mutable reference to the existing hook Arc
    if let Some(existing_hook_arc) = plan.hooks_by_name.get_mut(hook_name) {
        if existing_hook_arc.pkgkey.is_none() {
            let existing_file_path = existing_hook_arc.file_path.clone();

            // Mutate the hook in place using Arc::make_mut (clones only if there are multiple references)
            Arc::make_mut(existing_hook_arc).pkgkey = Some(pkgkey.to_string());

            log::info!(
                "hook '{}' from package {} ({}) is overriding global hook ({})",
                hook_name,
                pkgkey,
                path.display(),
                existing_file_path
            );
            return true;
        }
    }
    false
}

/// Register a hook to the plan structures
/// Only adds to hooks_by_name. Other indices (hooks_by_when, hooks_by_pkgkey) are built later.
fn register_hook_to_plan(
    mut hook: Hook,
    hook_name: String,
    pkgkey: Option<&str>,
    plan: &mut InstallationPlan,
) {
    // Set pkgkey on the hook if provided
    hook.pkgkey = pkgkey.map(|s| s.to_string());

    // Wrap hook in Arc for sharing across indices
    let hook_arc = Arc::new(hook);

    // Only save to hooks_by_name during loading
    plan.hooks_by_name.insert(hook_name, hook_arc);
}

/// Build hooks_by_when and hooks_by_pkgkey indices from hooks_by_name
fn build_hook_indices(plan: &mut InstallationPlan) {
    plan.hooks_by_when.clear();
    plan.hooks_by_pkgkey.clear();

    for hook_arc in plan.hooks_by_name.values() {
        // Add to hooks_by_when
        plan.hooks_by_when
            .entry(hook_arc.action.when.clone())
            .or_insert_with(Vec::new)
            .push(Arc::clone(hook_arc));

        // Add to hooks_by_pkgkey if it's a package hook
        if let Some(ref pkgkey) = hook_arc.pkgkey {
            plan.hooks_by_pkgkey
                .entry(pkgkey.clone())
                .or_insert_with(Vec::new)
                .push(Arc::clone(hook_arc));
        }
    }
}

/// Process a single hook file path and save it to plan structures
fn load_hook_file(
    path: &Path,
    plan: &mut InstallationPlan,
    pkgkey: Option<&str>,
) {

    // Only process .hook files
    if path.extension().and_then(|e| e.to_str()) != Some("hook") {
        log::debug!("skipping non-hook file {}", path.display());
        return;
    }

    let hook_name = extract_hook_file_name(&path);

    // Check if hook with same name already exists
    // Since we load global hooks first, then package hooks, if we find an existing hook
    // it means a package hook is overriding a global hook. Simply update the pkgkey and return.
    if let Some(pkgkey) = pkgkey {
        if update_existing_hook_pkgkey(&hook_name, pkgkey, path, plan) {
            return;
        }
    }

    // Skip symlinks to /dev/null (disabled hooks)
    if let Ok(link_target) = fs::read_link(&path) {
        if link_target == PathBuf::from("/dev/null") {
            log::debug!("Skipping disabled hook: {}", path.display());
            return;
        }
    }

    // Check if it's a directory (skip)
    if path.is_dir() {
        log::debug!("skipping directory {}", path.display());
        return;
    }

    match parse_hook_file(&path) {
        Ok(mut hook) => {
            populate_hook_target_cache(&mut hook);
            register_hook_to_plan(hook, hook_name, pkgkey, plan);
        }
        Err(e) => {
            log::warn!("Failed to parse hook file {}: {}", path.display(), e);
        }
    }
}

/// Sort hook references by file name (reference: _alpm_hook_cmp)
fn sort_hook_refs<T: AsRef<Hook>>(hooks: &mut [T]) {
    hooks.sort_by(|a, b| {
        let a_hook = a.as_ref();
        let b_hook = b.as_ref();
        a_hook.hook_name.cmp(&b_hook.hook_name)
            .then_with(|| a_hook.hook_name.len().cmp(&b_hook.hook_name.len()))
    });
}

/// Load hooks from a hook directory
/// Processes all .hook files in the given directory and adds them to the plan
fn load_hooks_from_directory(
    plan: &mut InstallationPlan,
    hook_dir: &Path,
    pkgkey: Option<&str>,
) -> Result<()> {
    if !hook_dir.exists() {
        return Ok(());
    }

    let entries = match read_hook_directory_entries(hook_dir) {
        Some(entries) => entries,
        None => return Ok(()),
    };

    for entry in entries {
        load_hook_file(&entry.path(), plan, pkgkey);
    }

    Ok(())
}

/// Load hooks from a package's hook directory
/// Scans the appropriate per-package hook directory for hooks.
/// - Pacman: $store_root/$pkgline/fs/usr/share/libalpm/hooks/
/// - Other formats (Rpm/Deb/etc): $store_root/$pkgline/info/install/
pub fn load_package_hooks(plan: &mut InstallationPlan, pkgkey: &str) -> Result<()> {
    let pkgline = pkgkey2pkgline(plan, pkgkey);

    if pkgline.is_empty() {
        log::debug!("Package {} has no pkgline, skipping hook loading", pkgkey);
        return Ok(());
    }

    let hook_dir = match plan.package_format {
        crate::models::PackageFormat::Pacman => {
            plan.store_root
                .join(&pkgline)
                .join("fs")
                .join("usr/share/libalpm/hooks")
        }
        _ => {
            plan.store_root
                .join(&pkgline)
                .join("info/install")
        }
    };

    load_hooks_from_directory(plan, &hook_dir, Some(pkgkey))
}

/// Load initial hooks (from installed packages and etc/pacman.d/hooks/)
/// Note: We load global hooks first, then package hooks. Package hooks can override global hooks
/// by updating the pkgkey on the existing hook.
pub fn load_initial_hooks(plan: &mut InstallationPlan) -> Result<()> {
    // Global hooks from etc/pacman.d/hooks/ are only for Pacman format
    if plan.package_format == PackageFormat::Pacman {
        let etc_hooks_dir = plan.env_root.join("etc/pacman.d/hooks");
        load_hooks_from_directory(plan, &etc_hooks_dir, None)?;
    }

    // Load hooks from installed packages (package hooks can override global hooks)
    let pkgkeys: Vec<String> = {
        let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
        installed.keys().cloned().collect()
    };
    for pkgkey in pkgkeys {
        load_package_hooks(plan, &pkgkey)?;
    }

    // Build hooks_by_when and hooks_by_pkgkey indices from hooks_by_name
    build_hook_indices(plan);

    Ok(())
}

/// Load hooks for packages in the current batch
pub fn load_batch_hooks(plan: &mut InstallationPlan) -> Result<()> {
    let pkgkeys: Vec<String> = plan.batch.new_pkgkeys.iter().cloned().collect();
    for pkgkey in pkgkeys {
        load_package_hooks(plan, &pkgkey)?;
    }

    // Build hooks_by_when and hooks_by_pkgkey indices from hooks_by_name
    build_hook_indices(plan);

    Ok(())
}

/// Match Path trigger (reference: _alpm_hook_trigger_match_file)
/// Returns (matched, aggregated_targets)
fn match_path_trigger(
    plan: &InstallationPlan,
    trigger: &HookTrigger,
    needs_targets: bool,
    pkgkey_filter: Option<&str>,
) -> Result<(bool, Vec<String>)> {
    // If there are no positive targets, we can't match
    if trigger.positive_targets.is_empty() {
        return Ok((false, Vec::new()));
    }

    let mut matched_targets = Vec::new();

    let upgrades_new_set =
        collect_matching_files(plan, trigger, needs_targets, pkgkey_filter, &plan.batch.upgrades_new)?;
    let upgrades_old_set =
        collect_matching_files(plan, trigger, needs_targets, pkgkey_filter, &plan.batch.upgrades_old)?;

    let wants_install = trigger.operations.is_set(HookOperation::Install);
    if wants_install {
        let installed_set =
            collect_matching_files(plan, trigger, needs_targets, pkgkey_filter, &plan.installed)?;
        let fresh_install_set =
            collect_matching_files(plan, trigger, needs_targets, pkgkey_filter, &plan.batch.fresh_installs)?;
        matched_targets.extend(installed_set.into_iter());
        matched_targets.extend(fresh_install_set.into_iter());
        matched_targets.extend(upgrades_new_set.difference(&upgrades_old_set).cloned());
    }

    let wants_remove = trigger.operations.is_set(HookOperation::Remove);
    if wants_remove {
        let old_remove_set =
            collect_matching_files(plan, trigger, needs_targets, pkgkey_filter, &plan.batch.old_removes)?;
        matched_targets.extend(old_remove_set.into_iter());
        matched_targets.extend(upgrades_old_set.difference(&upgrades_new_set).cloned());
    }

    let wants_upgrade = trigger.operations.is_set(HookOperation::Upgrade);
    if wants_upgrade {
        matched_targets.extend(upgrades_old_set.intersection(&upgrades_new_set).cloned());
    }

    Ok((!matched_targets.is_empty(), matched_targets))
}

/// Collect matching files for a batch of pkgkeys by looking up package info from the plan.
fn collect_matching_files(
    plan: &InstallationPlan,
    trigger: &HookTrigger,
    needs_targets: bool,
    pkgkey_filter: Option<&str>,
    pkgkeys: &HashSet<String>,
) -> Result<HashSet<String>> {
    let mut out = HashSet::new();

    if let Some(filter) = pkgkey_filter {
        if pkgkeys.contains(filter) {
            out = collect_matching_files_for_pkg(plan, trigger, needs_targets, filter)?;
        }
        return Ok(out);
    }

    for pkgkey in pkgkeys {
        let pkg_matches = collect_matching_files_for_pkg(plan, trigger, needs_targets, pkgkey)?;
        if !pkg_matches.is_empty() {
            if !needs_targets {
                return Ok(pkg_matches);
            }
            out.extend(pkg_matches.into_iter());
        }
    }

    Ok(out)
}

/// Collect matching files for a single pkgkey by looking up package info from the plan.
fn collect_matching_files_for_pkg(
    plan: &InstallationPlan,
    trigger: &HookTrigger,
    needs_targets: bool,
    pkgkey: &str,
) -> Result<HashSet<String>> {
    let store_root = &plan.store_root;
    let mut out = HashSet::new();

    if let Some(info) = crate::plan::pkgkey2installinfo(plan, pkgkey) {
        let files = get_package_files(store_root, &info)?;
        for file in &files {
            if matches_patterns(
                file,
                &trigger.positive_patterns,
                &trigger.negative_patterns,
            ) {
                out.insert(file.clone());
                if !needs_targets {
                    return Ok(out);
                }
            }
        }
    }

    Ok(out)
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

/// Check if any compiled pattern matches a string
fn matches_any_pattern(text: &str, patterns: &[Pattern]) -> bool {
    patterns.iter().any(|pattern| pattern.matches(text))
}

/// Match Package trigger (reference: _alpm_hook_trigger_match_pkg)
fn match_package_trigger(
    trigger: &HookTrigger,
    plan: &InstallationPlan,
    pkgkey_filter: Option<&str>,
) -> Result<(bool, Vec<String>, Vec<String>, Vec<String>)> {
    let mut install_pkgs = Vec::new();
    let mut upgrade_pkgs = Vec::new();
    let mut remove_pkgs = Vec::new();

    if trigger.positive_targets.is_empty() {
        return Ok((false, Vec::new(), Vec::new(), Vec::new()));
    }

    let wants_install = trigger.operations.is_set(HookOperation::Install);
    let wants_upgrade = trigger.operations.is_set(HookOperation::Upgrade);
    let wants_remove = trigger.operations.is_set(HookOperation::Remove);

    if wants_install {
        install_pkgs = collect_matching_pkg_names(&plan.batch.fresh_installs, trigger, pkgkey_filter);
    }
    if wants_upgrade {
        upgrade_pkgs = collect_matching_pkg_names(&plan.batch.upgrades_new, trigger, pkgkey_filter);
    }
    if wants_remove {
        remove_pkgs = collect_matching_pkg_names(&plan.batch.old_removes, trigger, pkgkey_filter);
    }

    let matched = (wants_install && !install_pkgs.is_empty())
        || (wants_upgrade && !upgrade_pkgs.is_empty())
        || (wants_remove && !remove_pkgs.is_empty());

    Ok((matched, install_pkgs, upgrade_pkgs, remove_pkgs))
}

/// Collect package names from a set of pkgkeys that match the trigger patterns and optional filter.
fn collect_matching_pkg_names(
    pkgkeys: &HashSet<String>,
    trigger: &HookTrigger,
    pkgkey_filter: Option<&str>,
) -> Vec<String> {
    let mut matched = Vec::new();

    if let Some(filter) = pkgkey_filter {
        // Only process the filtered pkgkey if it exists in the set
        if pkgkeys.contains(filter) {
            add_matching_pkgname(filter, trigger, &mut matched);
        }
    } else {
        // Process all pkgkeys
        for pkgkey in pkgkeys {
            add_matching_pkgname(pkgkey, trigger, &mut matched);
        }
    }

    matched
}

/// Helper function to check if a package matches the trigger patterns and add it to the matched vector.
fn add_matching_pkgname(
    pkgkey: &str,
    trigger: &HookTrigger,
    matched: &mut Vec<String>,
) {
    if let Ok(pkgname) = pkgkey2pkgname(pkgkey) {
        if matches_patterns(&pkgname, &trigger.positive_patterns, &trigger.negative_patterns) {
            matched.push(pkgname);
        }
    }
}

/// Check if a package dependency is satisfied
/// Reference: _alpm_hook_run_hook uses alpm_find_satisfier
fn check_dependency(
    fresh_installs: &HashSet<String>,
    dep: &str,
) -> bool {
    // Check installed and freshly installed package names for the dependency
    // This is a simplified check - in full implementation we'd need to check
    // provides, version constraints, etc.
    fn pkgkey_matches_dep(pkgkey: &str, dep: &str) -> bool {
        matches!(pkgkey2pkgname(pkgkey), Ok(pkgname) if pkgname == dep)
    }

    let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
    installed.iter().any(|(pkgkey, _)| pkgkey_matches_dep(pkgkey, dep))
        || fresh_installs.iter().any(|pkgkey| pkgkey_matches_dep(pkgkey, dep))
}

/// Execute a hook
/// Reference: _alpm_hook_run_hook
fn execute_hook(
    hook: &Hook,
    plan: &InstallationPlan,
    matched_targets: &[String],
) -> Result<()> {
    let env_root = &plan.env_root;

    // Check dependencies (reference: checks before execution)
    for dep in &hook.action.depends {
        if !check_dependency(&plan.batch.fresh_installs, dep) {
            return Err(color_eyre::eyre::eyre!(
                "unable to run hook {}: could not satisfy dependencies",
                hook.file_path
            ));
        }
    }

    // Parse exec command using shlex (reference: wordsplit)
    let exec_parts = match shlex::split(&hook.action.exec) {
        Some(parts) => {
            if parts.is_empty() {
                return Err(color_eyre::eyre::eyre!("Empty Exec in hook {}", hook.file_path));
            }
            parts
        }
        None => {
            return Err(color_eyre::eyre::eyre!(
                "hook {}: invalid Exec value {}",
                hook.file_path, hook.action.exec
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

    log::info!("Executing hook {}: {}", hook.file_path, hook.action.exec);

    let env_vars = HashMap::new();
    let stdin_data = if hook.action.needs_targets {
        Some(matched_targets.join("\n").into_bytes())
    } else {
        None
    };

    // Execute the hook
    let run_options = crate::run::RunOptions {
        command: command.to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
        env_vars,
        stdin: stdin_data,
        no_exit: !hook.action.abort_on_fail,
        chdir_to_env_root: true,
        timeout: 300, // 5 minute timeout for hooks
        ..Default::default()
    };

    match crate::run::fork_and_execute(env_root, &run_options, &command_path) {
        Ok(()) => {
            log::debug!("Hook {} executed successfully", hook.file_path);
            Ok(())
        }
        Err(e) => {
            if hook.action.abort_on_fail {
                Err(e).with_context(|| format!("Hook {} failed and AbortOnFail is set", hook.file_path))
            } else {
                log::warn!("Hook {} failed: {}", hook.file_path, e);
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

/// Check if we should reduce path trigger evaluation based on package count
fn should_reduce_path_triggers(plan: &InstallationPlan) -> bool {
    plan.batch.fresh_installs.len() + plan.batch.upgrades_new.len() >= 20
}

/// Calculate the recent cutoff time for path trigger optimization
fn calculate_recent_cutoff(should_reduce: bool) -> SystemTime {
    if should_reduce {
        SystemTime::now()
            .checked_sub(Duration::from_secs(3600))
            .unwrap_or(SystemTime::UNIX_EPOCH)
    } else {
        SystemTime::UNIX_EPOCH
    }
}

/// Check if a hook trigger matches the transaction
fn check_trigger_match(
    trigger: &HookTrigger,
    plan: &InstallationPlan,
    needs_targets: bool,
    should_reduce_path_triggers: bool,
    recent_cutoff: SystemTime,
    pkgkey_filter: Option<&str>,
) -> Result<(bool, Vec<String>)> {
    let env_root = &plan.env_root;
    if should_reduce_path_triggers && trigger.hook_type == HookType::Path && pkgkey_filter.is_none() {
        // Skip expensive trigger evaluation when none of its target files
        // were touched recently.
        if !path_trigger_has_recent_match(trigger, env_root, recent_cutoff)? {
            return Ok((false, Vec::new()));
        }
    }

    match trigger.hook_type {
        HookType::Path => {
            match_path_trigger(
                plan,
                trigger,
                needs_targets,
                pkgkey_filter,
            )
        }
        HookType::Package => {
            let (pmatched, install_targets, upgrade_targets, remove_targets) =
                match_package_trigger(trigger, plan, pkgkey_filter)?;

            let mut matched_targets = Vec::new();
            if pmatched && needs_targets {
                matched_targets.extend(install_targets);
                matched_targets.extend(upgrade_targets);
                matched_targets.extend(remove_targets);
            }

            Ok((pmatched, matched_targets))
        }
    }
}

/// Find all triggered hooks for the transaction
/// Returns a vector of (hook, matched_targets) tuples
fn find_triggered_hooks<'a>(
    relevant_hooks: &'a [&'a Arc<Hook>],
    plan: &InstallationPlan,
    should_reduce_path_triggers: bool,
    recent_cutoff: SystemTime,
    pkgkey_filter: Option<&str>,
) -> Result<Vec<(&'a Arc<Hook>, Vec<String>)>> {
    let mut triggered_hooks = Vec::new();

    for hook in relevant_hooks {
        // Special case: triggerless hooks are never triggered
        if hook.triggers.is_empty() {
            continue;
        }

        let mut hook_matched = false;
        let mut all_matched_targets = Vec::new();

        for trigger in &hook.triggers {
            let (matched, matched_targets) = check_trigger_match(
                trigger,
                plan,
                hook.action.needs_targets,
                should_reduce_path_triggers,
                recent_cutoff,
                pkgkey_filter,
            )?;

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

            triggered_hooks.push((*hook, all_matched_targets));
        }
    }

    Ok(triggered_hooks)
}

/// Execute all triggered hooks
fn execute_triggered_hooks(
    triggered_hooks: Vec<(&Arc<Hook>, Vec<String>)>,
    plan: &InstallationPlan,
    when: &HookWhen,
) -> Result<()> {
    for (hook, matched_targets) in triggered_hooks {
        log::info!("running '{}'...", hook.file_path);

        if let Err(e) = execute_hook(
            hook.as_ref(),
            plan,
            &matched_targets,
        ) {
            if hook.action.abort_on_fail {
                return Err(e).with_context(|| format!("failed to run transaction hooks"));
            }
        }

        // If PreTransaction and error occurred, stop (pacman behavior)
        if *when == HookWhen::PreTransaction {
            // Error already handled above
        }
    }

    Ok(())
}

/// Run hooks for a transaction
/// Reference: _alpm_hook_run
#[allow(dead_code)]
pub fn run_hooks(
    plan: &InstallationPlan,
    when: HookWhen,
) -> Result<()> {
    // Early return if no packages to process
    if plan.batch.new_pkgkeys.is_empty() {
        return Ok(());
    }

    // Get hooks for this When timing
    let relevant_hooks = match plan.hooks_by_when.get(&when) {
        Some(hooks) => hooks,
        None => return Ok(()),
    };

    if relevant_hooks.is_empty() {
        return Ok(());
    }

    // Sort hooks by name (reference: _alpm_hook_cmp)
    let mut sorted_hooks: Vec<&Arc<Hook>> = relevant_hooks.iter().collect();
    sort_hook_refs(&mut sorted_hooks);

    let should_reduce = should_reduce_path_triggers(plan);
    let recent_cutoff = calculate_recent_cutoff(should_reduce);

    // Find triggered hooks (reference: _alpm_hook_triggered)
    let triggered_hooks = find_triggered_hooks(
        &sorted_hooks,
        plan,
        should_reduce,
        recent_cutoff,
        None,
    )?;

    // Execute triggered hooks (reference: executes in order)
    execute_triggered_hooks(triggered_hooks, plan, &when)?;

    Ok(())
}

/// Run a single named hook over all packages in the current batch.
#[allow(dead_code)]
pub fn run_hook(
    plan: &InstallationPlan,
    hook_name: &str,
) -> Result<()> {
    // Early return if no packages to process
    if plan.batch.new_pkgkeys.is_empty() {
        return Ok(());
    }

    let hook_arc = match plan.hooks_by_name.get(hook_name) {
        Some(h) => h,
        None => return Ok(()),
    };

    let mut sorted_hooks: Vec<&Arc<Hook>> = vec![hook_arc];
    sort_hook_refs(&mut sorted_hooks);

    let should_reduce = should_reduce_path_triggers(plan);
    let recent_cutoff = calculate_recent_cutoff(should_reduce);

    let triggered_hooks = find_triggered_hooks(
        &sorted_hooks,
        plan,
        should_reduce,
        recent_cutoff,
        None,
    )?;

    execute_triggered_hooks(triggered_hooks, plan, &hook_arc.action.when)?;

    Ok(())
}

/// Run hooks belonging to a specific pkgkey over all packages.
pub fn run_pkgkey_hooks(
    plan: &InstallationPlan,
    when: &HookWhen,
    pkgkey: &str,
) -> Result<()> {
    // Early return if no packages to process
    if plan.batch.new_pkgkeys.is_empty() {
        return Ok(());
    }

    let pkg_hooks = match plan.hooks_by_pkgkey.get(pkgkey) {
        Some(h) => h,
        None => return Ok(()),
    };

    let mut relevant: Vec<&Arc<Hook>> = pkg_hooks
        .iter()
        .filter(|h| h.action.when == *when)
        .collect();

    if relevant.is_empty() {
        return Ok(());
    }

    sort_hook_refs(&mut relevant);

    let should_reduce = should_reduce_path_triggers(plan);
    let recent_cutoff = calculate_recent_cutoff(should_reduce);

    let triggered_hooks = find_triggered_hooks(
        &relevant,
        plan,
        should_reduce,
        recent_cutoff,
        None,
    )?;

    execute_triggered_hooks(triggered_hooks, plan, when)?;

    Ok(())
}

/// Run both:
/// - hooks belonging to `pkgkey` over all packages (`run_pkgkey_hooks`)
/// - all hooks on `pkgkey` with triggers restricted to that pkg (`run_hooks_on_pkgkey`)
pub fn run_pkgkey_hooks_pair(
    plan: &InstallationPlan,
    when: HookWhen,
    pkgkey: &str,
) -> Result<()> {
    run_pkgkey_hooks(plan, &when, pkgkey)?;
    run_hooks_on_pkgkey(plan, &when, pkgkey)?;
    Ok(())
}

/// Run all hooks on a specific pkgkey, restricting trigger evaluation to that pkgkey.
pub fn run_hooks_on_pkgkey(
    plan: &InstallationPlan,
    when: &HookWhen,
    pkgkey: &str,
) -> Result<()> {
    // Early return if no packages to process
    if plan.batch.new_pkgkeys.is_empty() {
        return Ok(());
    }

    let pkg_hooks = match plan.hooks_by_pkgkey.get(pkgkey) {
        Some(h) => h,
        None => return Ok(()),
    };

    let mut relevant: Vec<&Arc<Hook>> = pkg_hooks
        .iter()
        .filter(|h| h.action.when == *when)
        .collect();

    if relevant.is_empty() {
        return Ok(());
    }

    sort_hook_refs(&mut relevant);

    let should_reduce = should_reduce_path_triggers(plan);
    let recent_cutoff = calculate_recent_cutoff(should_reduce);

    let triggered_hooks = find_triggered_hooks(
        &relevant,
        plan,
        should_reduce,
        recent_cutoff,
        Some(pkgkey),
    )?;

    execute_triggered_hooks(triggered_hooks, plan, when)?;

    Ok(())
}

