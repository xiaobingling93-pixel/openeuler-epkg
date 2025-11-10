# Solver Test Framework

This directory contains data-driven tests for the dependency solver, inspired by apk-tools test structure.

## Test Structure

Each test case is defined by a `test*.yaml` file (e.g., `test.yaml`, `test1.yaml`, `test-basic.yaml`).
Test files can be placed directly in subdirectories of the tests/solver directory.

A test directory typically contains:
- `test*.yaml` - Test definition file (required)
- `repo.yaml` - Base repository packages (referenced by test.yaml)
- `repo-*.yaml` - Additional repository overlay files (optional, referenced by test.yaml)
- `installed.yaml` - Currently installed packages (optional, referenced by test.yaml)

## Running Tests

Tests are automatically run via the `solver_tests::tests::test_solver` test function:

```bash
cargo test solver_tests::tests::test_solver
```

Or run with output:

```bash
cargo test solver_tests::tests::test_solver -- --nocapture
```

## Test File Format

### test.yaml

The test file contains all test configuration and expected results:

```yaml
format: apk  # rpm, deb, apk, epkg, conda, python, pacman
description: Test description
skip: false  # Optional, default false
repo: repo.yaml repo-overlay.yaml  # Space-separated list of repo files to load and merge
installed: installed.yaml  # Optional, installed packages file (if not specified, uses empty set)

# Operation to perform (one of install, upgrade, or remove)
install:
  - package1
  - package2
# OR
upgrade:
  - package1
# OR
remove:
  - package1

# Expected InstallationPlan (optional)
plan:
  fresh_installs:
    package1__1.0__x86_64: {}
    package2__2.0__x86_64: {}
  upgrades_new:
    package1__2.0__x86_64: {}
  upgrades_old:
    package1__1.0__x86_64: {}
  old_removes:
    package1__1.0__x86_64: {}

expect_fail: false  # Optional, default false. Set to true if test is expected to fail
config:
  ignore_missing: false  # Optional, default false. Override ignore_missing config option
```

### repo.yaml

List of packages available in the repository. Each package should have:
- `pkgname` - Package name
- `version` - Package version
- `arch` - Architecture
- `requires` - List of requirements (optional)
- `provides` - List of provided capabilities (optional)
- `obsoletes` - List of obsoleted packages (optional)
- Other fields as needed

Multiple repo files can be specified in the `repo:` field, and they will be merged together.

### installed.yaml

YAML object mapping pkgkey to InstalledPackageInfo (can be empty `{}`).
This file is optional - if not specified in `test.yaml`, an empty installed set is used.

## Test Behavior

### Operations

The test framework supports three operations:
- **install**: Install specified packages and their dependencies
- **upgrade**: Upgrade specified packages to their latest versions
- **remove**: Remove specified packages (and optionally their dependents)

Only one operation type should be specified per test. If multiple are specified, `install` takes precedence over `upgrade`, which takes precedence over `remove`.

### Expected Results

Tests can specify an expected `InstallationPlan` in the `plan:` field. The plan should contain:
- `fresh_installs`: Map of pkgkeys to install (keys only, values are empty objects)
- `upgrades_new`: Map of new package versions (keys only)
- `upgrades_old`: Map of old package versions being replaced (keys only)
- `old_removes`: Map of packages to remove (keys only)

If no plan is specified, the test will only verify that the operation succeeds (unless `expect_fail: true`).

### Expected Failures

Set `expect_fail: true` for tests that are expected to fail (e.g., missing dependencies, conflicts). The test will pass if an error occurs and fail if it succeeds.

### Config Overrides

Tests can override configuration options via the `config:` field:
- `ignore_missing`: If `true`, missing dependencies won't cause the operation to fail immediately (useful for testing error handling)

Note: When `expect_fail: true`, the test framework automatically sets `ignore_missing: true` internally to allow error propagation instead of process termination.

## Multiple Test Files

You can have multiple `test*.yaml` files in a subdirectory. Each file will be run as a separate test case. This allows you to:
- Share repository files (`repo.yaml`) across multiple tests
- Organize related tests together
- Test different scenarios with the same repository setup

Example structure:
```
tests/solver/
  basic/
    repo.yaml
    installed.yaml
    test1.yaml
    test2.yaml
    test10.yaml
  complex/
    repo.yaml
    repo-overlay.yaml
    test1.yaml
    test2.yaml
```

## Test Execution

Each test:
1. Resets all global state (config, channel config format) to defaults
2. Loads repository packages from specified repo files
3. Loads installed packages (if specified)
4. Sets the channel config format based on the test's `format:` field
5. Applies any config overrides
6. Creates a fresh PackageManager with empty caches
7. Executes the specified operation (install/upgrade/remove)
8. Validates the result against the expected plan (if provided) or expected success/failure
