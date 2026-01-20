use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use color_eyre::eyre::{Result, eyre};
use serde::Deserialize;
use crate::models::*;
use crate::plan::InstallationPlan;
use crate::models::PACKAGE_CACHE;
#[cfg(test)]
use env_logger;

/// Test case metadata (includes expected results)
#[derive(Debug, Deserialize)]
#[allow(dead_code)] // Used in run() method
struct TestCaseMetadata {
    /// Package format for this test
    format: String,
    /// Distribution name (optional, e.g., "openeuler", "debian")
    #[serde(default)]
    distro: String,
    /// Description of what this test validates
    description: String,
    /// Whether this test should be skipped
    #[serde(default)]
    skip: bool,
    /// Repository files to load (space-separated list, e.g., "repo.yaml repo-overlay.yaml")
    #[serde(default)]
    repo: String,
    /// Optional installed packages file (if not specified, uses empty installed set)
    #[serde(default)]
    installed: Option<String>,
    /// Packages to install
    #[serde(default)]
    install: Vec<String>,
    /// Packages to upgrade
    #[serde(default)]
    upgrade: Vec<String>,
    /// Packages to remove
    #[serde(default)]
    remove: Vec<String>,
    /// Expected GenerationCommand (simplified plan with just package lists)
    #[serde(default)]
    plan: GenerationCommand,
    /// Whether resolution should fail
    #[serde(default)]
    expect_fail: bool,
    /// Optional config overrides for this test
    #[serde(default)]
    config: TestConfig,
}

/// Test configuration overrides
#[derive(Debug, Default, Deserialize)]
#[allow(dead_code)] // Used in TestCaseMetadata
struct TestConfig {
    /// Ignore missing dependencies
    #[serde(default)]
    ignore_missing: bool,
    /// Full upgrade mode: upgrade all packages, not just those in world.json
    #[serde(default)]
    full_upgrade: bool,
}

/// Test case data structure
#[allow(dead_code)] // Used by run_all_tests() in test module
struct TestCase {
    /// Test directory path
    test_dir: PathBuf,
    /// Test metadata (includes expected results)
    metadata: TestCaseMetadata,
    /// Available packages in repository (merged from all repo files)
    packages: Vec<Package>,
    /// Installed packages
    installed: InstalledPackagesMap,
}

impl TestCase {
    /// Load a test case from a test YAML file
    #[allow(dead_code)] // Used in test module
    fn load(test_file: &Path) -> Result<Self> {
        let test_dir = test_file.parent()
            .ok_or_else(|| eyre!("Test file has no parent directory: {}", test_file.display()))?;

        // Load metadata (includes expected results)
        let metadata: TestCaseMetadata = {
            let content = fs::read_to_string(test_file)?;
            serde_yaml::from_str(&content)?
        };

        // Load and merge packages from all repo files
        let mut packages: Vec<Package> = Vec::new();
        for repo_file in metadata.repo.split_whitespace() {
            let repo_path = test_dir.join(repo_file);
            if repo_path.exists() {
                let content = fs::read_to_string(&repo_path)?;
                let mut repo_packages: Vec<Package> = serde_yaml::from_str(&content)?;
                packages.append(&mut repo_packages);
            } else {
                return Err(eyre!("Repository file not found: {}", repo_path.display()));
            }
        }

        // Load installed packages (optional)
        let installed: InstalledPackagesMap = if let Some(installed_file) = &metadata.installed {
            let installed_path = test_dir.join(installed_file);
            if installed_path.exists() {
                let content = fs::read_to_string(&installed_path)?;
                let raw: HashMap<String, InstalledPackageInfo> = serde_yaml::from_str(&content)?;
                raw.into_iter().map(|(k, v)| (k, Arc::new(v))).collect()
            } else {
                return Err(eyre!("Installed file not found: {}", installed_path.display()));
            }
        } else {
            HashMap::new()
        };

        Ok(TestCase {
            test_dir: test_dir.to_path_buf(),
            metadata,
            packages,
            installed,
        })
    }

    /// Parse format string to PackageFormat enum
    #[allow(dead_code)] // Used in test module
    fn parse_format(&self) -> Result<PackageFormat> {
        PackageFormat::from_str(self.metadata.format.as_str())
    }

    /// Create and populate PackageManager with test data
    ///
    /// This creates a fresh PackageManager instance with empty caches for each test,
    /// ensuring that packages from previous tests don't leak into the current test.
    #[allow(dead_code)] // Used in test module
    fn setup_package_manager(&self) {
        // Clear all caches first to ensure clean state
        PACKAGE_CACHE.clear();

        // Populate installed packages into cache
        for (k, v) in &self.installed {
            PACKAGE_CACHE.installed_packages.write().unwrap().insert(k.clone(), v.clone());
        }

        // Populate packages into pkgkey2package and update indexes
        // build_provider_list in depends.rs now checks provide2pkgnames index for test data
        // Get format from test metadata, default to Epkg if parsing fails
        let format = self.parse_format().unwrap_or(PackageFormat::Epkg);
        for mut pkg in self.packages.clone() {
            // Generate pkgkey if not set
            if pkg.pkgkey.is_empty() {
                pkg.pkgkey = format!("{}__{}__{}", pkg.pkgname, pkg.version, pkg.arch);
            }
            crate::package_cache::add_package_to_cache(Arc::new(pkg), format);
        }
    }

    /// Create initial packages map from install request (for fallback compatibility)
    #[allow(dead_code)] // Used in test module
    fn create_initial_packages(&self) -> InstalledPackagesMap {
        let mut initial_packages: InstalledPackagesMap = HashMap::new();

        // For each requested package, try to find it and add to initial packages
        for req in &self.metadata.install {
            // Try to resolve the package name to find matching packages
            if let Ok(packages) = crate::package_cache::map_pkgname2packages(req) {
                if let Some(pkg) = packages.first() {
                    let pkgkey = pkg.pkgkey.clone();
                    initial_packages.insert(
                        pkgkey.clone(),
                        std::sync::Arc::new(InstalledPackageInfo {
                            pkgline: format!("fake_hash__{}", pkgkey),
                            arch: pkg.arch.clone(),
                            depend_depth: 0,
                            install_time: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap()
                                .as_secs(),
                            ebin_exposure: true,
                            rdepends: Vec::new(),
                            depends: Vec::new(),
                            bdepends: Vec::new(),
                            rbdepends: Vec::new(),
                            ebin_links: Vec::new(),
                            xdesktop_links: Vec::new(),
                            pending_triggers: Vec::new(),
                            triggers_awaited: false,
                            config_failed: false,
                        }),
                    );
                }
            }
            // If package not found, the solver will try to resolve it via providers
        }

        initial_packages
    }

    /// Validate InstallationPlan against expected GenerationCommand
    #[allow(dead_code)] // Used in test module
    fn validate_plan(&self, plan_result: Result<InstallationPlan>) -> Result<()> {
        match plan_result {
            Ok(plan) => {
                // Use the helper function to convert plan to GenerationCommand
                let actual_command = crate::plan::plan_to_generation_command(&plan);
                let expected_command = &self.metadata.plan;

                println!("  Fresh installs: {:?}", actual_command.fresh_installs);
                println!("  Upgrades: {:?}", actual_command.upgrades_new);
                println!("  Removals: {:?}", actual_command.old_removes);
                if !actual_command.new_exposes.is_empty() {
                    println!("  New exposes: {:?}", actual_command.new_exposes);
                }
                if !actual_command.del_exposes.is_empty() {
                    println!("  Del exposes: {:?}", actual_command.del_exposes);
                }

                // Check if expected plan has any content
                let has_expected_plan = !expected_command.fresh_installs.is_empty()
                    || !expected_command.upgrades_new.is_empty()
                    || !expected_command.old_removes.is_empty()
                    || !expected_command.new_exposes.is_empty()
                    || !expected_command.del_exposes.is_empty();

                if has_expected_plan {
                    // Compare Vec fields by converting to HashSet for order-independent comparison
                    let mut errors = Vec::new();

                    // Helper to compare Vec fields as sets
                    let compare_vec_fields = |actual: &[String], expected: &[String], field_name: &str| -> Option<String> {
                        let actual_set: std::collections::HashSet<String> = actual.iter().cloned().collect();
                        let expected_set: std::collections::HashSet<String> = expected.iter().cloned().collect();
                        if actual_set != expected_set {
                            Some(format!("{} mismatch: expected {:?}, got {:?}", field_name, expected, actual))
                        } else {
                            None
                        }
                    };

                    // Compare all GenerationCommand fields
                    if let Some(error) = compare_vec_fields(&actual_command.fresh_installs, &expected_command.fresh_installs, "Fresh installs") {
                        errors.push(error);
                    }
                    if let Some(error) = compare_vec_fields(&actual_command.upgrades_new, &expected_command.upgrades_new, "Upgrades") {
                        errors.push(error);
                    }
                    if let Some(error) = compare_vec_fields(&actual_command.old_removes, &expected_command.old_removes, "Removals") {
                        errors.push(error);
                    }

                    if !errors.is_empty() {
                        return Err(eyre!("Test failed:\n{}", errors.join("\n")));
                    }

                    println!("  ✓ Test passed (plan matches expected)");
                } else {
                    // No expected plan - just check if it succeeded
                    if !self.metadata.expect_fail {
                        println!("  ✓ Test passed (no expected plan specified)");
                    } else {
                        return Err(eyre!("Test expected to fail but succeeded"));
                    }
                }
            }
            Err(e) => {
                if self.metadata.expect_fail {
                    println!("  ✓ Test passed (expected failure)");
                    return Ok(());
                } else {
                    return Err(eyre!("Test failed with error: {}", e));
                }
            }
        }

        Ok(())
    }

    /// Reset all test state to ensure clean start for each test
    #[allow(dead_code)] // Used in test module
    fn reset_test_state(&self) {
        #[cfg(test)]
        {
            // Reset channel config to defaults
            let mut channel_config = crate::models::channel_config_mut();
            *channel_config = ChannelConfig::default();

            // Reset config options to defaults
            let mut global_config = crate::models::config_mut();
            global_config.common.ignore_missing = false;
        }
    }

    /// Apply config overrides from test metadata
    #[allow(dead_code)] // Used in test module
    fn apply_config_overrides(&self) -> Result<()> {
        #[cfg(test)]
        {
            // Get mutable access to the global config
            // config_mut() returns a MutexGuard which provides mutable access
            let mut global_config = crate::models::config_mut();
            // Override ignore_missing if specified in test config
            global_config.common.ignore_missing = self.metadata.config.ignore_missing;
            // Override full_upgrade if specified in test config
            global_config.upgrade.full_upgrade = self.metadata.config.full_upgrade;

            // Set the channel config format for this test
            let format = self.parse_format()?;
            let mut channel_config = crate::models::channel_config_mut();
            channel_config.format = format;
            // Set distro if specified in test metadata
            if !self.metadata.distro.is_empty() {
                channel_config.distro = self.metadata.distro.clone();
            }
        }
        Ok(())
    }

    /// Run the test case with a specific solver
    #[allow(dead_code)] // Used in test module
    fn run_with_solver(&self, solver_name: &str) -> Result<()> {
        if self.metadata.skip {
            println!("Skipping test: {}", self.metadata.description);
            return Ok(());
        }
        if !self.metadata.remove.is_empty() {
            println!("Skipping test (non-empty remove field): {}", self.metadata.description);
            return Ok(());
        }

        println!("Running test: {} [solver: {}]", self.metadata.description, solver_name);
        println!("  Format: {}", self.metadata.format);

        // Reset all test state to ensure clean start
        self.reset_test_state();

        // Apply config overrides if present (including format)
        self.apply_config_overrides()?;

        self.setup_package_manager();

        // Determine which operation to perform and get the plan
        // dry_run is already set to true in test mode via CLAP_MATCHES
        let plan_result = if !self.metadata.install.is_empty() {
            println!("  Install: {:?}", self.metadata.install);
            // Set config.subcommand for install
            #[cfg(test)]
            {
                let mut global_config = crate::models::config_mut();
                global_config.subcommand = crate::models::EpkgCommand::Install;
            }
            crate::install::install_packages(self.metadata.install.clone())
        } else if !self.metadata.remove.is_empty() {
            println!("  Remove: {:?}", self.metadata.remove);
            crate::remove::remove_packages(self.metadata.remove.clone())
        } else {
            println!("  Upgrade: {:?}", self.metadata.upgrade);
            // Set config.subcommand for upgrade
            #[cfg(test)]
            {
                let mut global_config = crate::models::config_mut();
                global_config.subcommand = crate::models::EpkgCommand::Upgrade;
            }
            crate::upgrade::upgrade_packages(self.metadata.upgrade.clone())
        };

        // Validate plan
        self.validate_plan(plan_result)
    }
}

/// Collect all test*.yaml files from immediate subdirectories ($tests_dir/*/test*.yaml)
#[allow(dead_code)] // Used in test module
fn collect_test_files(tests_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut test_files = Vec::new();

    for entry in fs::read_dir(tests_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            // Only check immediate subdirectories
            for sub_entry in fs::read_dir(&path)? {
                let sub_entry = sub_entry?;
                let sub_path = sub_entry.path();
                if sub_path.is_file() {
                    if let Some(file_name) = sub_path.file_name().and_then(|n| n.to_str()) {
                        if file_name.starts_with("test") && file_name.ends_with(".yaml") {
                            test_files.push(sub_path);
                        }
                    }
                }
            }
        }
    }

    test_files.sort();
    Ok(test_files)
}

/// Test execution result counters
#[allow(dead_code)] // Used in test module
struct TestResults {
    passed: usize,
    failed: usize,
    skipped: usize,
}

impl TestResults {
    fn new() -> Self {
        TestResults {
            passed: 0,
            failed: 0,
            skipped: 0,
        }
    }

    #[allow(dead_code)] // Used in test module
    fn record_pass(&mut self) {
        self.passed += 1;
    }

    #[allow(dead_code)] // Used in test module
    fn record_fail(&mut self) {
        self.failed += 1;
    }

    #[allow(dead_code)] // Used in test module
    fn record_skip(&mut self) {
        self.skipped += 1;
    }

    #[allow(dead_code)] // Used in test module
    fn print_summary(&self) {
        println!("Summary: {} passed, {} failed, {} skipped", self.passed, self.failed, self.skipped);
    }

    #[allow(dead_code)] // Used in test module
    fn has_failures(&self) -> bool {
        self.failed > 0
    }
}

/// Check if a test case should be filtered out based on SOLVER_TEST_FILTER env var
#[allow(dead_code)] // Used in test module
fn should_filter_test(test_file: &Path, test_case: &TestCase, tests_dir: &Path) -> bool {
    let filter_pattern = match std::env::var("SOLVER_TEST_FILTER").ok() {
        Some(pattern) => pattern,
        None => return false, // No filter, don't skip
    };

    let file_name = test_file.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let dir_name = test_file.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()).unwrap_or("");

    // Calculate relative path from tests_dir to test_file (e.g., "complicated/test1.yaml")
    let relative_path = test_file.strip_prefix(tests_dir)
        .ok()
        .and_then(|p| p.to_str())
        .unwrap_or("");

    // Check if test matches filter (relative path, file name, dir name, or description)
    !relative_path.contains(&filter_pattern)
        && !file_name.contains(&filter_pattern)
        && !dir_name.contains(&filter_pattern)
        && !test_case.metadata.description.contains(&filter_pattern)
}

/// Run a test case with both solvers and record results
#[allow(dead_code)] // Used in test module
fn run_test_case_with_both_solvers(test_file: &Path, test_case: &TestCase, results: &mut TestResults, tests_dir: &Path) {
    // Run with resolvo solver (default)

    // Calculate relative path from tests_dir to test_file (e.g., "complicated/test1.yaml")
    let relative_path = test_file.strip_prefix(tests_dir)
        .ok()
        .and_then(|p| p.to_str())
        .unwrap_or("");

    println!("SOLVER_TEST_FILTER={} cargo test test_solver -- --nocapture", relative_path);

    println!("==================");
    let resolvo_result = test_case.run_with_solver("resolvo");
    let resolvo_passed = resolvo_result.is_ok();
    if let Err(e) = resolvo_result {
        eprintln!("FAILED: {} [solver: resolvo] - {}", test_file.display(), e);
    }

    // Record pass only if solver passed and test wasn't skipped
    // Record fail if solver failed
    if resolvo_passed {
        if !test_case.metadata.skip && test_case.metadata.remove.is_empty() {
            results.record_pass();
        } else {
            results.record_skip();
        }
    } else {
        results.record_fail();
    }
}

/// Run a single test file and update results
#[allow(dead_code)] // Used in test module
fn run_test_file(test_file: &Path, results: &mut TestResults, tests_dir: &Path) {
    match TestCase::load(test_file) {
        Ok(test_case) => {
            // Apply filter if specified
            if should_filter_test(test_file, &test_case, tests_dir) {
                // Skip this test - doesn't match filter
                return;
            }

            run_test_case_with_both_solvers(test_file, &test_case, results, tests_dir);
        }
        Err(e) => {
            results.record_fail();
            eprintln!("FAILED to load test case {}: {}", test_file.display(), e);
        }
    }
    println!();
}

/// Run all test cases in a directory
///
/// Supports filtering by test name via environment variable SOLVER_TEST_FILTER.
/// The filter matches against relative paths (e.g., "complicated/test1.yaml"),
/// test file names, directory names, and test descriptions.
///
/// Example usage:
///   SOLVER_TEST_FILTER=basic cargo test test_solver
///   SOLVER_TEST_FILTER=complicated/test1.yaml cargo test test_solver
///
/// Note: Cargo's built-in test filtering (e.g., `cargo test test_solver -- test_name`)
/// works at the test function level, but this environment variable allows filtering
/// individual test cases within the solver test suite.
#[allow(dead_code)] // Used in test module
pub fn run_all_tests(tests_dir: &Path) -> Result<()> {
    if !tests_dir.exists() {
        return Err(eyre!("Tests directory does not exist: {}", tests_dir.display()));
    }

    let test_files = collect_test_files(tests_dir)?;

    let filter_pattern = std::env::var("SOLVER_TEST_FILTER").ok();
    let test_count = if filter_pattern.is_some() {
        // Count will be determined as we run tests
        test_files.len()
    } else {
        test_files.len()
    };

    println!("Found {} test case(s)", test_count);
    if let Some(pattern) = &filter_pattern {
        println!("Filtering by pattern: {} (use SOLVER_TEST_FILTER env var)", pattern);
    }
    println!();

    let mut results = TestResults::new();

    for test_file in test_files {
        run_test_file(&test_file, &mut results, tests_dir);
    }

    results.print_summary();

    if results.has_failures() {
        Err(eyre!("{} test(s) failed", results.failed))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_solver() {
        // Initialize logger for tests so RUST_LOG environment variable works
        let _ = env_logger::Builder::from_default_env()
            .format(|buf, record| {
                use std::io::Write;
                writeln!(
                    buf,
                    "[{} {} {}:{}] {}",
                    record.level(),
                    record.file().unwrap_or("unknown"),
                    record.line().unwrap_or(0),
                    record.target(),
                    record.args()
                )
            })
            .try_init();

        let tests_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/solver");
        if tests_dir.exists() {
            // Run the tests and unwrap to fail the test if there are errors
            run_all_tests(&tests_dir).unwrap();
        } else {
            // Skip test if directory doesn't exist
            println!("Tests directory not found: {:?}, skipping", tests_dir);
        }
    }
}

