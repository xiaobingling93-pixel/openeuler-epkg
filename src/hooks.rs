use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use color_eyre::eyre::{Result, Context};
use crate::models::PackageFormat;
use crate::package::{pkgkey2pkgname, pkgkey2version};
use crate::models::PACKAGE_CACHE;
use crate::version_constraint::check_version_constraint;
use crate::package_cache::map_pkgline2filelist;
use crate::plan::InstallationPlan;
use crate::plan::pkgkey2pkgline;
use crate::parse_requires::VersionConstraint;
use crate::rpm_triggers::{parse_rpm_trigger_condition, RPMTRIGGER_DEFAULT_PRIORITY};
use crate::run::{fork_and_execute, RunOptions};
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
    pub positive_prefixes: Vec<String>,
    pub positive_packages: HashMap<String, Vec<VersionConstraint>>, // Package name -> version constraints
    pub positive_patterns: Vec<Pattern>,
    pub negative_patterns: Vec<Pattern>,
    pub type_set: bool, // for input validation
}

impl Default for HookTrigger {
    fn default() -> Self {
        Self {
            operations: 0,
            hook_type: HookType::Path,
            targets: Vec::new(),
            positive_targets: Vec::new(),
            negative_targets: Vec::new(),
            positive_prefixes: Vec::new(),
            positive_packages: HashMap::new(),
            positive_patterns: Vec::new(),
            negative_patterns: Vec::new(),
            type_set: false,
        }
    }
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
    pub script_order: u32,  // for applets/rpm.rs query: in-package list order, not exec order
}

impl Default for HookAction {
    fn default() -> Self {
        Self {
            description: None,
            when: HookWhen::PostTransaction,
            exec: String::new(),
            depends: Vec::new(),
            abort_on_fail: false,
            needs_targets: false,
            priority: RPMTRIGGER_DEFAULT_PRIORITY,
            script_order: 0,
        }
    }
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
/// Append -pkgkey to avoid conflicts since both old/new pkg triggers may be loaded
fn extract_hook_file_name(hook_path: &Path, pkgkey: Option<&str>) -> String {
    let file_name_full = hook_path.file_name()
        .and_then(|n| n.to_str())
        .unwrap()
        .to_string();
    let base_name = file_name_full.strip_suffix(".hook").unwrap_or(&file_name_full).to_string();
    if let Some(pkgkey) = pkgkey {
        format!("{}-{}", base_name, pkgkey)
    } else {
        base_name
    }
}

/// Parse a hook file from disk
/// Reference: _alpm_hook_parse_cb in hook.c
pub fn parse_hook_file(hook_path: &Path, pkgkey: Option<&str>) -> Result<Hook> {
    let content = fs::read_to_string(hook_path)
        .with_context(|| format!("Failed to read hook file: {}", hook_path.display()))?;

    let mut current_triggers = Vec::new();
    let mut current_action: Option<HookAction> = None;
    let mut in_trigger = false;
    let mut in_action = false;

    let hook_name = extract_hook_file_name(hook_path, pkgkey);

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
            current_triggers.push(HookTrigger::default());
            continue;
        } else if line == "[Action]" {
            in_action = true;
            in_trigger = false;
            // Start new action
            current_action = Some(HookAction::default());
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
        // Check for leading '/' (should be absolute/path, not /absolute/path)
        for pattern in &trigger.positive_prefixes {
            // ! from Archlinux for negative patterns
            // + from Alpine
            let check_pattern = pattern.strip_prefix('!').unwrap_or(pattern.strip_prefix('+').unwrap_or(pattern));
            if check_pattern.starts_with('/') {
                return Err(color_eyre::eyre::eyre!(
                    "Target value '{}' in hook {} has leading '/', should be '{}'",
                    pattern,
                    file.display(),
                    check_pattern.strip_prefix('/').unwrap_or(check_pattern)
                ));
            }
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

/// Check if a character is a glob character
#[inline]
fn is_glob_char(c: char) -> bool {
    matches!(c, '*' | '?' | '[' | ']')
}

/// Extract prefix from a pattern if it's a prefix pattern (no glob or only '*' at the end)
/// Returns Some(prefix) if it's a prefix pattern, None otherwise
/// Converts
/// - "usr/share/fonts" -> Some("usr/share/fonts")
/// - "usr/share/fonts/*" -> Some("usr/share/fonts/")
/// - "usr/share/icons/*/" -> None
fn extract_prefix(pattern: &str) -> Option<&str> {
    // First try to strip '*' from the end
    let candidate = pattern.strip_suffix('*').unwrap_or(pattern);

    // Check any glob characters
    if !candidate.chars().any(is_glob_char) {
        return Some(candidate);
    }

    None
}

/// Separate positive targets into prefixes and patterns
fn separate_prefixes_and_patterns(positive_targets: &[String]) -> (Vec<String>, Vec<String>) {
    let mut prefixes = Vec::new();
    let mut patterns = Vec::new();

    for target in positive_targets {
        // Deb/Apk Path trigger targets have leading '/'
        if let Some(prefix) = extract_prefix(target.strip_prefix('/').unwrap_or(target)) {
            prefixes.push(prefix.to_string());
        } else {
            patterns.push(target.clone());
        }
    }

    (prefixes, patterns)
}

/// Compile string patterns into Pattern objects, filtering out invalid ones
fn compile_patterns(patterns: &[String]) -> Vec<Pattern> {
    patterns.iter()
        .filter_map(|p| Pattern::new(p).ok())
        .collect()
}

/// Populate cache for a single trigger
fn populate_trigger_cache(trigger: &mut HookTrigger) {
    let (positive_targets, negative_targets) = split_hook_targets(&trigger.targets);

    if trigger.hook_type == HookType::Package {
        // positive_targets => positive_patterns OR positive_packages
        for target in &positive_targets {
            if target.chars().any(is_glob_char) { // Archlinux may have package name pattern
                match Pattern::new(target) {
                    Ok(pattern) => {
                        trigger.positive_patterns.push(pattern);
                    }
                    Err(e) => {
                        log::warn!("Failed to compile pattern '{}' for Package trigger: {}", target, e);
                    }
                }
            } else {
                // RPM/DEB style package triggers
                // RPM, DEB and some Archlinux targets are suitable for quick hash lookup in matches_any_package()
                let packages = parse_rpm_trigger_condition(target);
                for (pkg_name, constraints) in packages {
                    // Merge constraints if package already exists
                    let entry = trigger.positive_packages.entry(pkg_name.clone()).or_insert_with(Vec::new);
                    entry.extend(constraints);
                }
            }
        }
    } else { // HookType::Path
        // positive_targets => positive_patterns OR positive_prefixes
        let (prefixes, patterns) = separate_prefixes_and_patterns(&positive_targets);
        trigger.positive_prefixes = prefixes;
        trigger.positive_patterns = compile_patterns(&patterns);
    }

    trigger.negative_patterns = compile_patterns(&negative_targets);
    trigger.negative_targets = negative_targets;
    trigger.positive_targets = positive_targets;
}

fn populate_hook_target_cache(hook: &mut Hook) {
    for trigger in &mut hook.triggers {
        populate_trigger_cache(trigger);
    }
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
            "ScriptOrder" => {
                let order = value.parse::<u32>().map_err(|e| {
                    color_eyre::eyre::eyre!(
                        "hook {} line {}: invalid ScriptOrder {} ({})",
                        file.display(),
                        line_num,
                        value,
                        e
                    )
                })?;
                action.script_order = order;
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
        log::trace!("skipping non-hook file {}", path.display());
        return;
    }

    // Check if hook with same name already exists
    // Since we load global hooks first, then package hooks, if we find an existing hook
    // it means a package hook is overriding a global hook. Simply update the pkgkey and return.
    // Use the base name (without pkgkey suffix) for the override check
    let global_hook_name = extract_hook_file_name(&path, None);
    if let Some(pkgkey) = pkgkey {
        if update_existing_hook_pkgkey(&global_hook_name, pkgkey, path, plan) {
            return;
        }
    }

    // Extract hook name with pkgkey suffix appended if pkgkey is provided
    let hook_name = extract_hook_file_name(&path, pkgkey);

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

    match parse_hook_file(&path, pkgkey) {
        Ok(mut hook) => {
            populate_hook_target_cache(&mut hook);
            register_hook_to_plan(hook, hook_name, pkgkey, plan);
        }
        Err(e) => {
            log::warn!("Failed to parse hook file {}: {}", path.display(), e);
        }
    }
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
fn load_package_hooks(plan: &mut InstallationPlan, pkgkey: &str) -> Result<()> {
    let pkgline = pkgkey2pkgline(plan, pkgkey);

    if pkgline.is_empty() {
        log::debug!("Package {} has no pkgline, skipping hook loading", pkgkey);
        return Ok(());
    }

    let hook_dir = match plan.package_format {
        crate::models::PackageFormat::Pacman => {
            crate::dirs::path_join(
                &plan.store_root.join(&pkgline).join("fs"),
                &["usr", "share", "libalpm", "hooks"],
            )
        }
        _ => {
            crate::dirs::path_join(&plan.store_root.join(&pkgline), &["info", "install"])
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
        let etc_hooks_dir = crate::dirs::path_join(&plan.env_root, &["etc", "pacman.d", "hooks"]);
        load_hooks_from_directory(plan, &etc_hooks_dir, None)?;
    }

    // Global hooks from etc/apk/commit_hooks.d/ are only for APK format
    if plan.package_format == PackageFormat::Apk {
        let etc_hooks_dir = crate::dirs::path_join(&plan.env_root, &["etc", "apk", "commit_hooks.d"]);
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

    // Add systemd hooks if systemd package is not installed
    add_systemd_hooks_if_needed(plan)?;

    // Build hooks_by_when and hooks_by_pkgkey indices from hooks_by_name
    build_hook_indices(plan);

    Ok(())
}

/// Add systemd hooks if systemd package is not installed.  Since we skipped systemd in
/// no_install_packages[] but still need running systemd-sysusers and systemd-tmpfiles.
///
/// Only needed for RPM/ArchLinux distros.
///
/// RPM: auto run by triggers, which is not installed, so need run here
///   wfg /c/os/fedora/systemd% grep '^%' triggers.systemd
///   %transfiletriggerin -P 1000600 -- /usr/lib/tmpfiles.d/
///   %transfiletriggerin -P 1000700 -- /usr/lib/sysusers.d/
///
///   %transfiletriggerin -P 1000500 -- /usr/lib/sysctl.d/
///   %transfiletriggerin -P 1000700 -- /usr/lib/binfmt.d/
///
///   %transfiletriggerin -P 1000600 udev -- /usr/lib/udev/rules.d/
///   %transfiletriggerin -P 1000700 udev -- /usr/lib/udev/hwdb.d/
///
///   %transfiletriggerin -P 1000700 -- /usr/lib/systemd/catalog/
///   %transfiletriggerin -P  900899 -- /usr/lib/systemd/user/       /etc/systemd/user/
///   %transfiletriggerin -P  900900 -- /usr/lib/systemd/system/     /etc/systemd/system/
///   %transfiletriggerpostun -P 1000099 -- /usr/lib/systemd/user/   /etc/systemd/user/
///   %transfiletriggerpostun -P   10000 -- /usr/lib/systemd/system/ /etc/systemd/system/
///   %transfiletriggerpostun -P 1000100 -- /usr/lib/systemd/system/ /etc/systemd/system/
///   %transfiletriggerpostun -P    9999 -- /usr/lib/systemd/user/   /etc/systemd/user/
///
/// Archlinux: need run
///   % epkg -e archlinux search --paths usr/share/libalpm/hooks/|grep systemd
///   systemd /usr/share/libalpm/hooks/20-systemd-sysusers.hook
///   ...
///   wfg ~/.epkg/store/wdmnsebhj53mrqa224ulxaqur4s4nfyg__systemd__259-2__x86_64/fs/usr/share/libalpm/hooks% ls
///   20-systemd-sysusers.hook  25-systemd-binfmt.hook   25-systemd-hwdb.hook    30-systemd-daemon-reload-system.hook  35-systemd-restart-marked.hook  35-systemd-update.hook
///   21-systemd-tmpfiles.hook  25-systemd-catalog.hook  25-systemd-sysctl.hook  30-systemd-daemon-reload-user.hook    35-systemd-udev-reload.hook
///
///   wfg ~/.epkg/store/wdmnsebhj53mrqa224ulxaqur4s4nfyg__systemd__259-2__x86_64/fs/usr/share/libalpm/hooks% cat 20-systemd-sysusers.hook
///   [Trigger]
///   Type = Path
///   Operation = Install
///   Operation = Upgrade
///   Target = usr/lib/sysusers.d/*.conf
///   [Action]
///   Description = Creating system user accounts...
///   When = PostTransaction
///   Exec = /usr/share/libalpm/scripts/systemd-hook sysusers
///
/// DEB: adduser/chown/chmod are explicitly handled by the package postinst script:
/// - either run 'adduser', e.g. sddm
/// - or run 'sysusers-sysusers' on its provided sysusers.d/*.conf
///   wfg /var/lib/dpkg/info% grep 'systemd-sysusers ' *.p*|wc -l
///   13
///   wfg /var/lib/dpkg/info% grep 'usr/lib/sysusers.d/' *.list|wc -l
///   13
///   wfg /var/lib/dpkg/info% grep /usr/lib/tmpfiles.d *triggers
///   wfg /var/lib/dpkg/info% grep /usr/lib/sysusers.d  *triggers
///   wfg /var/lib/dpkg/info% cat systemd.triggers
///   interest-noawait /usr/lib/systemd/catalog
///   interest-noawait /usr/lib/binfmt.d
///   interest-noawait /usr/lib/sysctl.d
///   interest-noawait libc-upgrade
///   wfg /var/lib/dpkg/info% grep -B2 systemd-sysusers *post*
///   cron-daemon-common.postinst-# Automatically added by dh_installsysusers/13.24.2
///   cron-daemon-common.postinst-if [ "$1" = "configure" ] || [ "$1" = "abort-upgrade" ] || [ "$1" = "abort-deconfigure" ] || [ "$1" = "abort-remove" ] ; then
///   cron-daemon-common.postinst:   systemd-sysusers ${DPKG_ROOT:+--root="$DPKG_ROOT"} cron-daemon-common.conf
///   ---
///   ... many more, ditto for systemd-tmpfiles
/// However systemd package carries some .conf files (e.g. basic.conf, tmp.conf) necessary for
/// setup the base system, so we need provide equivalent setup when systemd is not installed in env:
///   systemd-sysusers ${DPKG_ROOT:+--root="$DPKG_ROOT"} basic.conf systemd-journal.conf systemd-network.conf
///   systemd-tmpfiles ${DPKG_ROOT:+--root="$DPKG_ROOT"} --create 20-systemd-shell-extra.conf 20-systemd-ssh-generator.conf 20-systemd-stub.conf credstore.conf debian.conf home.conf journal-nocow.conf legacy.conf provision.conf systemd-network.conf systemd-nologin.conf systemd-pstore.conf systemd-tmp.conf systemd.conf tmp.conf var.conf x11.conf || true
///
/// Alpine: no sysusers.d; explicit adduser in scriptlets
///   % grep sysusers.d ~/.epkg/store/*nginx*/info/filelist.txt
///   % grep -r sysusers ~/.epkg/store/*/info/apk
///   % grep -r adduser ~/.epkg/store/*/info/apk
///   /home/wfg/.epkg/store/46ipnaixxgm2ljjh2bbsknlkbtuwhair__redis__8.0.4-r0__x86_64/info/apk/.pre-install:adduser -S -D -H -h /var/lib/redis -s /sbin/nologin -G redis -g redis redis 2>/dev/null
///   /home/wfg/.epkg/store/d4qfx4aefytxdjfdjmgoknxzsnorb2ot__nginx__1.28.0-r3__x86_64/info/apk/.pre-install:adduser -S -D -H -h /var/lib/nginx -s /sbin/nologin -G nginx -g nginx nginx 2>/dev/null
///   /home/wfg/.epkg/store/lc6hz4yb6hw2a3z2yju4ul5apk7g66qg__busybox__1.37.0-r20__x86_64/info/apk/.post-install:adduser -S -D -H -h /dev/null -s /sbin/nologin -G klogd -g klogd klogd 2>/dev/null
///   /home/wfg/.epkg/store/lc6hz4yb6hw2a3z2yju4ul5apk7g66qg__busybox__1.37.0-r20__x86_64/info/apk/.post-upgrade:adduser -S -D -H -h /dev/null -s /sbin/nologin -G klogd -g klogd klogd 2>/dev/null
///
/// Conda: no triggers, no sysusers.d

/// Check if systemd package is installed (present in world)
fn is_systemd_installed() -> bool {
    let world = PACKAGE_CACHE.world.read().unwrap();
    world.contains_key("systemd")
}

/// Check if systemd package is in the no-install list
fn is_systemd_in_no_install() -> bool {
    let world = PACKAGE_CACHE.world.read().unwrap();
    world.get("no-install")
        .map(|s| s.split_whitespace().any(|pkg| pkg == "systemd"))
        .unwrap_or(false)
}

fn add_systemd_hooks_if_needed(plan: &mut InstallationPlan) -> Result<()> {

    if is_systemd_installed() {
        return Ok(());
    }

    if !is_systemd_in_no_install() {
        return Ok(());
    }

    // Check if systemd package is being installed in this transaction
    // Disabled: the above world[] checks shall be enough
    // let systemd_in_new_pkgs = plan.new_pkgs.keys().any(|pkgkey| {
    //     pkgkey2pkgname(pkgkey).map(|name| name == "systemd").unwrap_or(false)
    // });
    // if systemd_in_new_pkgs {
    //     return Ok(());
    // }

    log::debug!("systemd is in no-install list, adding systemd hooks");

    create_deb_sysusers(plan);

    // Create sysusers hook
    create_systemd_hook(
        "usr/lib/sysusers.d/*.conf",
        "Creating system user accounts...",
        "systemd-sysusers",
        1000700,
        "20-systemd-sysusers",
        HookWhen::PostTransaction,
        plan,
    )?;

    // Create tmpfiles hook
    create_systemd_hook(
        "usr/lib/tmpfiles.d/*.conf",
        "Creating temporary files...",
        "systemd-tmpfiles --create",
        1000600,
        "21-systemd-tmpfiles",
        HookWhen::PostTransaction,
        plan,
    )?;

    Ok(())
}

// Helper to create and register a systemd hook
fn create_systemd_hook(
    target_pattern: &str,
    description: &str,
    exec: &str,
    priority: u32,
    hook_name: &str,
    when: HookWhen,
    plan: &mut InstallationPlan,
) -> Result<()> {
    let mut hook = Hook {
        triggers: vec![HookTrigger {
            operations: HookOperation::Install.as_flag() | HookOperation::Upgrade.as_flag(),
            hook_type: HookType::Path,
            targets: vec![target_pattern.to_string()],
            type_set: true,
            ..Default::default()
        }],
        action: HookAction {
            description: Some(description.to_string()),
            when,
            exec: exec.to_string(),
            priority,
            ..Default::default()
        },
        hook_name: hook_name.to_string(),
        file_path: format!("VirtualFile({})", hook_name),
        pkgkey: None,
    };

    populate_hook_target_cache(&mut hook);
    register_hook_to_plan(hook, hook_name.to_string(), None, plan);

    Ok(())
}

// When skipped installing systemd package, we need provide its
// usr/lib/sysusers.d/basic.conf file to create system users.
fn create_deb_sysusers(plan: &InstallationPlan)
{
    if plan.package_format != crate::models::PackageFormat::Deb {
        return;
    }

    // Write basic.conf if missing
    let basic_conf_path = crate::dirs::path_join(&plan.env_root, &["usr", "lib", "sysusers.d", "basic.conf"]);
    if basic_conf_path.exists() {
        return;
    }

    if let Some(parent) = basic_conf_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let content = r#"g adm        4     -
g tty        5     -
g disk       6     -
g man        12    -
g kmem       15    -
g dialout    20    -
g fax        21    -
g voice      22    -
g cdrom      24    -
g floppy     25    -
g tape       26    -
g sudo       27    -
g audio      29    -
g dip        30    -
g operator   37    -
g src        40    -
g shadow     42    -
g utmp       43    -
g video      44    -
g sasl       45    -
g plugdev    46    -
g staff      50    -
g games      60    -
g users      100   -
g nogroup    65534 -

u root       0       - /root                /bin/bash
u daemon     1       - /usr/sbin            /usr/sbin/nologin
u bin        2       - /bin                 /usr/sbin/nologin
u sys        3       - /dev                 /usr/sbin/nologin
u sync       4:65534 - /bin                 /bin/sync
u games      5:60    - /usr/games           /usr/sbin/nologin
u man        6:12    - /var/cache/man       /usr/sbin/nologin
u lp         7       - /var/spool/lpd       /usr/sbin/nologin
u mail       8       - /var/mail            /usr/sbin/nologin
u news       9       - /var/spool/news      /usr/sbin/nologin
u uucp       10      - /var/spool/uucp      /usr/sbin/nologin
u proxy      13      - /bin                 /usr/sbin/nologin
u www-data   33      - /var/www             /usr/sbin/nologin
u backup     34      - /var/backups         /usr/sbin/nologin
u list       38      - /var/list            /usr/sbin/nologin
u irc        39      - /run/ircd            /usr/sbin/nologin
u _apt       42:65534 - /nonexistent         /usr/sbin/nologin
u nobody     65534:65534 - /nonexistent         /usr/sbin/nologin"#;

    if let Err(e) = fs::write(&basic_conf_path, content) {
        log::warn!("Failed to write {}: {}", basic_conf_path.display(), e);
    } else {
        log::debug!("Created missing {}", basic_conf_path.display());
        run_in_env(&plan.env_root, "systemd-sysusers", &["basic.conf"]);
    }
}

fn run_in_env(env_root: &Path, cmd: &str, args: &[&str])
{
    let run_options = RunOptions {
        command: cmd.to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
        no_exit: true,
        chdir_to_env_root: true,
        timeout: 30,
        ..Default::default()
    };
    if let Err(e) = fork_and_execute(env_root, &run_options) {
        log::warn!("Failed to run {}: {}", cmd, e);
    }
}

/// Load hooks for packages in the current batch
pub fn load_batch_hooks(plan: &mut InstallationPlan) -> Result<()> {
    let pkgkeys: Vec<String> = plan.batch.new_pkgkeys.iter().cloned().collect();
    for pkgkey in pkgkeys {
        load_package_hooks(plan, &pkgkey)?;
    }

    // Build hooks_by_when and hooks_by_pkgkey indices from hooks_by_name
    build_hook_indices(plan);

    log::trace!("hooks after batch load: {:#?}", plan.hooks_by_name);

    Ok(())
}

/// Match Path trigger (reference: _alpm_hook_trigger_match_file)
/// Returns (matched, aggregated_targets)
/// For DEB trigger hooks (is_deb_trigger=true), matched_targets contains trigger names instead of file paths
/// For DEB file triggers, trigger-name is positive_targets item (or positive_prefixes)
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

    let matched = !matched_targets.is_empty();

    // For DEB/APK file hooks, replace file paths with trigger names
    if matched && (
        plan.package_format == PackageFormat::Deb ||
        plan.package_format == PackageFormat::Apk
    ) {
        let trigger_names: Vec<String> = trigger.positive_targets.clone();
        Ok((true, trigger_names))
    } else {
        Ok((matched, matched_targets))
    }
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
        let files = map_pkgline2filelist(store_root, &info.pkgline)?;
        for file in &files {
            // For file paths, pass empty string for pkgkey (not used for Path triggers)
            if matches_patterns(file, trigger, pkgkey, plan) {
                out.insert(file.clone());
                if !needs_targets {
                    return Ok(out);
                }
            }
        }
    }

    Ok(out)
}

/// Check if text matches any of the given prefixes
fn matches_any_prefix(text: &str, prefixes: &[String]) -> bool {
    prefixes.iter().any(|prefix| text.starts_with(prefix))
}

/// Check if any compiled pattern matches a string
fn matches_any_pattern(text: &str, patterns: &[Pattern]) -> bool {
    patterns.iter().any(|pattern| pattern.matches(text))
}

/// Check if a package name matches any entry in positive_packages with version constraints
/// Returns true if the package name is in positive_packages and all version constraints are satisfied
fn matches_any_package(
    pkgname: &str,
    pkgkey: &str,
    trigger: &HookTrigger,
    plan: &InstallationPlan,
) -> bool {
    if trigger.positive_packages.is_empty() {
        return false;
    }

    if let Some(constraints) = trigger.positive_packages.get(pkgname) {
        // If there are version constraints, check all of them
        if !constraints.is_empty() {
            if let Ok(pkg_version) = pkgkey2version(pkgkey) {
                // All constraints must be satisfied (AND logic)
                for constraint in constraints {
                    match check_version_constraint(&pkg_version, constraint, plan.package_format) {
                        Ok(true) => {
                            // This constraint is satisfied, continue checking others
                        }
                        Ok(false) => {
                            // This constraint is not satisfied, fail
                            return false;
                        }
                        Err(e) => {
                            log::warn!("Failed to check version constraint for {} {}: {}", pkgname, pkg_version, e);
                            // On error, fail open (treat as satisfied for this constraint)
                        }
                    }
                }
                // All constraints satisfied
                return true;
            } else {
                // Failed to get version, fail
                return false;
            }
        } else {
            // No version constraints, package name matches
            return true;
        }
    }

    false
}

/// Check if text matches positive patterns but not negative patterns (using compiled patterns and prefixes)
/// For Package triggers, also checks positive_packages with version constraints using pkgkey and plan
fn matches_patterns(
    text: &str,
    trigger: &HookTrigger,
    pkgkey: &str,
    plan: &InstallationPlan,
) -> bool {
    // Must match positive and not match negative
    (
        matches_any_package(text, pkgkey, trigger, plan) ||
        matches_any_prefix(text, &trigger.positive_prefixes) ||
        matches_any_pattern(text, &trigger.positive_patterns)
    ) &&
        !matches_any_pattern(text, &trigger.negative_patterns)
}

/// Match Package trigger (reference: _alpm_hook_trigger_match_pkg)
/// Returns (matched, combined_targets)
/// For DEB trigger hooks (is_deb_trigger=true), returns trigger names instead of package names
/// For DEB explicit triggers, trigger-name is positive_targets item (the trigger name)
fn match_package_trigger(
    trigger: &HookTrigger,
    plan: &InstallationPlan,
    pkgkey_filter: Option<&str>,
) -> Result<(bool, Vec<String>)> {
    let is_deb_trigger = plan.package_format == PackageFormat::Deb;

    if trigger.positive_targets.is_empty() {
        return Ok((false, Vec::new()));
    }

    let wants_install = trigger.operations.is_set(HookOperation::Install);
    let wants_upgrade = trigger.operations.is_set(HookOperation::Upgrade);
    let wants_remove = trigger.operations.is_set(HookOperation::Remove);

    let mut matched_targets = Vec::new();

    if wants_install {
        let install_pkgs = collect_matching_pkg_names(&plan.batch.fresh_installs, trigger, plan, pkgkey_filter);
        matched_targets.extend(install_pkgs);
    }
    if wants_upgrade {
        let upgrade_pkgs = collect_matching_pkg_names(&plan.batch.upgrades_new, trigger, plan, pkgkey_filter);
        matched_targets.extend(upgrade_pkgs);
    }
    if wants_remove {
        let remove_pkgs = collect_matching_pkg_names(&plan.batch.old_removes, trigger, plan, pkgkey_filter);
        matched_targets.extend(remove_pkgs);
    }

    let matched = !matched_targets.is_empty();

    // For DEB trigger hooks, replace package names with trigger names
    if is_deb_trigger && matched {
        // For DEB explicit triggers, trigger-name is positive_targets item (the trigger name)
        // positive_targets contains the trigger names (e.g., "mime-support", "update-menus")
        let trigger_names = trigger.positive_targets.clone();
        Ok((true, trigger_names))
    } else {
        Ok((matched, matched_targets))
    }
}

/// Collect package names from a set of pkgkeys that match the trigger patterns and optional filter.
fn collect_matching_pkg_names(
    pkgkeys: &HashSet<String>,
    trigger: &HookTrigger,
    plan: &InstallationPlan,
    pkgkey_filter: Option<&str>,
) -> Vec<String> {
    let mut matched = Vec::new();

    if let Some(filter) = pkgkey_filter {
        // Only process the filtered pkgkey if it exists in the set
        if pkgkeys.contains(filter) {
            add_matching_pkgname(filter, trigger, plan, &mut matched);
        }
    } else {
        // Process all pkgkeys
        for pkgkey in pkgkeys {
            add_matching_pkgname(pkgkey, trigger, plan, &mut matched);
        }
    }

    matched
}

/// Helper function to check if a package matches the trigger patterns and add it to the matched vector.
/// For deb packages, matches trigger-name instead of pkgname.
fn add_matching_pkgname(
    pkgkey: &str,
    trigger: &HookTrigger,
    plan: &InstallationPlan,
    matched: &mut Vec<String>,
) {
    if plan.package_format == PackageFormat::Deb {
        // For deb packages, match trigger-name instead of pkgname
        if let Some(trigger_names) = plan.deb_activate_triggers_by_pkg.get(pkgkey) {
            for trigger_name in trigger_names {
                if matches_patterns(trigger_name, trigger, pkgkey, plan) {
                    matched.push(trigger_name.clone());
                }
            }
        }
        return;
    }
    // For non-deb packages, match pkgname as before
    if let Ok(pkgname) = pkgkey2pkgname(pkgkey) {
        // matches_patterns() now handles positive_packages uniformly
        if matches_patterns(&pkgname, trigger, pkgkey, plan) {
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
///
/// Count installed instances of a package by name
/// Returns the number of installed packages with the given pkgname
fn count_installed_instances_by_name(pkgname: &str) -> u32 {
    let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
    let mut count = 0u32;
    for (pkgkey, _) in installed.iter() {
        if let Ok(name) = pkgkey2pkgname(pkgkey) {
            if name == pkgname {
                count += 1;
            }
        }
    }
    count
}

/// Get triggering package name from matched targets or transaction
/// For package triggers, matched_targets contains package names
/// For file triggers, just return None
fn get_triggering_pkgname(
    hook: &Hook,
    matched_targets: &[String],
    _plan: &InstallationPlan,
) -> Option<String> {
    // For package triggers, matched_targets contains package names
    if !hook.triggers.is_empty() && hook.triggers[0].hook_type == HookType::Package {
        return matched_targets.first().cloned();
    }

    // It seems no real world RPM file triggers using $2
    None
}

/// Add DEB trigger arguments ($2) to the args vector
/// For DEB hooks, adds trigger names as a single argument:
/// - $2 = "<space-separated trigger names>"
/// Note: $1 = "triggered" is already part of the Exec command
fn add_deb_trigger_args(
    _hook: &Hook,
    matched_targets: &[String],
    plan: &InstallationPlan,
    args: &mut Vec<String>,
) {
    if plan.package_format != PackageFormat::Deb {
        return;
    }
    // matched_targets contains trigger names for DEB trigger hooks
    if !matched_targets.is_empty() {
        // Join trigger names with space and add as a single argument
        // This matches dpkg's behavior: postinst triggered "trigger-name trigger-name ..."
        args.push(matched_targets.join(" "));
    }
}

/// Add RPM trigger instance count arguments ($1 and $2) to the args vector
/// For RPM format, adds two arguments:
/// - $1: Number of installed instances of the triggered package (package containing the trigger scriptlet)
/// - $2: Number of installed instances of the triggering package (package that set off the trigger)
fn add_rpm_trigger_instance_args(
    hook: &Hook,
    matched_targets: &[String],
    plan: &InstallationPlan,
    args: &mut Vec<String>,
) {
    if plan.package_format != PackageFormat::Rpm {
        return;
    }

    // hooks by create_systemd_hook() do not accept RPM $1 $2 params
    if hook.pkgkey.is_none() {
        return;
    }

    // $1: Number of installed instances of the triggered package (package containing the trigger scriptlet)
    let triggered_pkgname = hook.pkgkey.as_ref()
        .and_then(|pkgkey| pkgkey2pkgname(pkgkey).ok());
    let triggered_count = if let Some(ref name) = triggered_pkgname {
        count_installed_instances_by_name(name)
    } else {
        0
    };

    // $2: Number of installed instances of the triggering package (package that set off the trigger)
    let triggering_pkgname = get_triggering_pkgname(hook, matched_targets, plan);
    let triggering_count = if let Some(ref name) = triggering_pkgname {
        count_installed_instances_by_name(name)
    } else {
        triggered_count
    };

    args.push(triggered_count.to_string());
    args.push(triggering_count.to_string());
}

/// Check hook dependencies before execution
fn check_hook_dependencies(hook: &Hook, plan: &InstallationPlan) -> Result<()> {
    for dep in &hook.action.depends {
        if !check_dependency(&plan.batch.fresh_installs, dep) {
            return Err(color_eyre::eyre::eyre!(
                "unable to run hook {}: could not satisfy dependencies",
                hook.file_path
            ));
        }
    }
    Ok(())
}

/// Parse hook Exec command, handling %PKGINFO_DIR placeholder and shell quoting
/// Returns (command, args) tuple
fn parse_hook_exec(hook: &Hook) -> Result<(String, Vec<String>)> {
    // Replace %PKGINFO_DIR placeholder with the actual package info directory path
    // The hook file is at: store_dir/pkgline/info/install/hook_name.hook
    // So pkginfo_dir is: hook.file_path.parent().parent() (== store_dir/pkgline/info)
    let exec_command = if hook.action.exec.contains("%PKGINFO_DIR") {
        let pkginfo_dir = std::path::Path::new(&hook.file_path)
            .parent() // install/
            .and_then(|p| p.parent()) // info/
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| {
                log::warn!("Could not determine PKGINFO_DIR for hook {}", hook.file_path);
                String::new()
            });
        hook.action.exec.replace("%PKGINFO_DIR", &pkginfo_dir)
    } else {
        hook.action.exec.clone()
    };

    let exec_parts = match shlex::split(&exec_command) {
        Some(parts) => {
            if parts.is_empty() {
                return Err(color_eyre::eyre::eyre!("Empty Exec in hook {}", hook.file_path));
            }
            parts
        }
        None => {
            return Err(color_eyre::eyre::eyre!(
                "hook {}: invalid Exec value {}",
                hook.file_path, exec_command
            ));
        }
    };

    let command = exec_parts[0].clone();
    let args = exec_parts[1..].iter().map(|s| s.to_string()).collect();
    Ok((command, args))
}

/// This function handles various Exec command formats found in real-world hooks:
/// - Direct execution:
///     `/usr/bin/appstreamcli refresh-cache --force`
/// - Shell commands with -c and nested quotes:
///     `/bin/sh -c 'while read -r f; do install-info "$f" /usr/share/info/dir 2> /dev/null; done'`
///     `/bin/sh -c 'killall -q -s USR1 gvfsd || true'`
/// - Commands with quoted arguments:
///     `/usr/bin/vim -es --cmd ":helptags /usr/share/vim/vimfiles/doc" --cmd ":q"`
pub fn execute_hook(
    hook: &Hook,
    plan: &InstallationPlan,
    matched_targets: &[String],
) -> Result<()> {
    let env_root = &plan.env_root;

    // Check dependencies (reference: checks before execution)
    check_hook_dependencies(hook, plan)?;

    // Parse exec command using shlex (reference: wordsplit)
    let (command, mut args) = parse_hook_exec(hook)?;

    if plan.package_format == PackageFormat::Deb {
        // For DEB format, add trigger names
        add_deb_trigger_args(hook, matched_targets, plan, &mut args);
    } else if plan.package_format == PackageFormat::Rpm {
        // For RPM format, add $1 and $2 arguments for trigger instance counts
        add_rpm_trigger_instance_args(hook, matched_targets, plan, &mut args);
    }

    log::info!("Executing hook {}: {} {:?}", hook.file_path, command, args);

    let env_vars = HashMap::new();
    // For hooks with NeedsTargets, matched file paths are passed via stdin
    // Example: texinfo hooks read file paths from stdin in their 'while read -r f' loops
    let stdin_data = if hook.action.needs_targets {
        Some(matched_targets.join("\n").into_bytes())
    } else {
        None
    };

    // Execute the hook
    let run_options = crate::run::RunOptions {
        command,
        args,
        env_vars,
        stdin: stdin_data,
        no_exit: !hook.action.abort_on_fail,
        chdir_to_env_root: true,
        timeout: 300, // 5 minute timeout for hooks
        ..Default::default()
    };

    match crate::run::fork_and_execute(env_root, &run_options) {
        Ok(None) => {
            log::debug!("Hook {} executed successfully", hook.file_path);
            Ok(())
        }
        Ok(Some(_)) => {
            unreachable!("Foreground process should not return PID")
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

/// Check if a hook trigger matches the transaction
/// For DEB trigger hooks, matched_targets contains trigger names instead of file paths/package names
fn check_trigger_match(
    trigger: &HookTrigger,
    plan: &InstallationPlan,
    needs_targets: bool,
    pkgkey_filter: Option<&str>,
) -> Result<(bool, Vec<String>)> {
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
            let (pmatched, matched_targets) =
                match_package_trigger(trigger, plan, pkgkey_filter)?;

            if pmatched && needs_targets {
                Ok((true, matched_targets))
            } else {
                Ok((pmatched, Vec::new()))
            }
        }
    }
}

/// Sort triggered hooks by hook name (reference: _alpm_hook_cmp)
/// For RPM format, sort by priority first (lower priority = earlier execution)
fn sort_triggered_hooks(
    triggered_hooks: &mut [(&Arc<Hook>, Vec<String>)],
    package_format: PackageFormat,
) {
    triggered_hooks.sort_by(|a, b| {
        let a_hook = a.0;
        let b_hook = b.0;

        // For RPM format, sort by priority first (lower priority = earlier execution)
        if package_format == PackageFormat::Rpm {
            a_hook.action.priority.cmp(&b_hook.action.priority)
                .then_with(|| a_hook.hook_name.cmp(&b_hook.hook_name))
                .then_with(|| a_hook.hook_name.len().cmp(&b_hook.hook_name.len()))
        } else {
            a_hook.hook_name.cmp(&b_hook.hook_name)
                .then_with(|| a_hook.hook_name.len().cmp(&b_hook.hook_name.len()))
        }
    });
}

/// Find all triggered hooks for the transaction
/// Returns a vector of (hook, matched_targets) tuples
/// Sorts triggered hooks before returning
fn find_triggered_hooks<'a>(
    relevant_hooks: &'a [Arc<Hook>],
    plan: &InstallationPlan,
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

            triggered_hooks.push((hook, all_matched_targets));
        }
    }

    // Sort triggered hooks (only the ones that will be executed)
    sort_triggered_hooks(&mut triggered_hooks, plan.package_format);

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

pub fn run_hooks(
    plan: &InstallationPlan,
    when: HookWhen,
) -> Result<()> {
    if plan.batch.is_first {
        run_trans_hooks(plan, when)?;
    } else {
        for pkgkey in &plan.batch.new_pkgkeys {
            run_pkgkey_hooks_pair(plan, when.clone(), pkgkey)?;
        }
    }
    Ok(())
}

/// Run hooks for a transaction
/// Reference: _alpm_hook_run
fn run_trans_hooks(
    plan: &InstallationPlan,
    when: HookWhen,
) -> Result<()> {
    // Get hooks for this When timing
    let relevant_hooks = match plan.hooks_by_when.get(&when) {
        Some(hooks) => hooks,
        None => return Ok(()),
    };

    // Find triggered hooks (reference: _alpm_hook_triggered)
    // Sorting is done inside find_triggered_hooks on triggered hooks only
    let triggered_hooks = find_triggered_hooks(relevant_hooks, plan, None)?;

    // Execute triggered hooks (reference: executes in order)
    execute_triggered_hooks(triggered_hooks, plan, &when)?;

    Ok(())
}

/// Filter hooks by when timing and clone them
fn filter_hooks_by_when(hooks: &[Arc<Hook>], when: &HookWhen) -> Vec<Arc<Hook>> {
    hooks
        .iter()
        .filter(|h| h.action.when == *when)
        .cloned()
        .collect()
}

/// Run hooks belonging to a specific pkgkey over all packages.
fn run_pkgkey_hooks(
    plan: &InstallationPlan,
    when: &HookWhen,
    pkgkey: &str,
) -> Result<()> {
    let pkg_hooks = match plan.hooks_by_pkgkey.get(pkgkey) {
        Some(h) => h,
        None => return Ok(()),
    };

    let relevant_hooks = filter_hooks_by_when(pkg_hooks, when);

    let triggered_hooks = find_triggered_hooks(&relevant_hooks, plan, None)?;

    execute_triggered_hooks(triggered_hooks, plan, when)?;

    Ok(())
}

/// Run all hooks on a specific pkgkey, restricting trigger evaluation to that pkgkey.
fn run_hooks_on_pkgkey(
    plan: &InstallationPlan,
    when: &HookWhen,
    pkgkey: &str,
) -> Result<()> {
    // Get hooks for this When timing
    let relevant_hooks = match plan.hooks_by_when.get(&when) {
        Some(hooks) => hooks,
        None => return Ok(()),
    };

    let triggered_hooks = find_triggered_hooks(&relevant_hooks, plan, Some(pkgkey))?;

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
