# epkg list - Enhanced Package Listing Command

## Overview

The `epkg list` command provides a comprehensive way to list packages based on different scopes and advanced filtering options. It displays packages in a formatted table with detailed status information, including installation status, dependency depth, and upgrade availability.

## CLI Interface

```bash
epkg list [--installed] [--upgradable] [--available] [--all] [GLOB_PATTERN]
```

### Scope Options

Only one scope option can be used at a time. The default scope is `--installed` if no scope is specified.

- `--installed` - List only installed packages (default)
- `--available` - List only packages that are available but not installed
- `--upgradable` - List only packages that have available updates
- `--all` - List all packages (installed, available, and upgradable)

### Advanced Pattern Filtering

`GLOB_PATTERN` supports comprehensive glob-style pattern matching for filtering package names:

#### Pattern Types

1. **No wildcards**: `bash` - Substring matching (matches "bash", "bash-completion", "mybash")
2. **Prefix matching**: `bash*` - Matches packages starting with "bash"
3. **Suffix matching**: `*bash` - Matches packages ending with "bash"
4. **Contains matching**: `*bash*` - Matches packages containing "bash" anywhere
5. **Complex patterns**: `b*sh` - Matches packages starting with "b" and ending with "sh"
6. **Multiple wildcards**: `java*openjdk*headless` - Matches complex patterns
7. **Universal match**: `*` - Matches all packages
8. **Empty pattern**: Lists all packages in the specified scope

#### Pattern Examples

```bash
# Substring matching (no wildcards)
epkg list bash          # Matches: bash, bash-completion, mybash

# Prefix matching
epkg list bash*         # Matches: bash, bash-completion, bashrc
epkg list java*         # Matches: java-8-openjdk, java-11-openjdk

# Suffix matching
epkg list *dev          # Matches: lib-dev, python3-dev
epkg list *bash         # Matches: mybash, zsh-bash

# Contains matching
epkg list *bash*        # Matches: bash, mybash, bash-completion, my-bash-script

# Complex patterns
epkg list b*sh          # Matches: bash, bush, brush
epkg list java*jdk*     # Matches: java-8-openjdk, java-11-openjdk-headless
```

## Data Sources & Architecture

### Installed Packages
- **Source**: `~/.epkg/envs/{env}/generations/current/installed-packages.json`
- **Access**: `self.load_installed_packages()` method
- **Format**: `HashMap<String, InstalledPackageInfo>` where key is pkgkey
- **Caching**: Loaded once per command execution

### Available Packages
- **Source**: Repository indices packages.idx (`pkgname2ranges.keys()` from all RepoShard instances)
- **Access**: Streaming iteration through repository shards
- **Format**: Direct iteration over package names from repository metadata
- **Performance**: Only essential metadata loaded for list command

### Package Details
- **Source**: packages.txt repository metadata and package.txt from installed local store
- **Repository packages**: `self.map_pkgname2packages(pkgname)`
- **Local packages**: `self.map_pkgline2package(pkgline)` with caching
- **Caching**: Both pkgname2package and pkgline2package caches implemented

## Performance Optimizations

### Streaming Architecture
The implementation uses a **two-step streaming approach** that eliminates memory bloat:

#### OLD: Inefficient Batch Processing
```rust
// Collect ALL package names first (memory intensive)
let package_names = self.collect_package_names_by_scope(&scope)?;
// Filter ALL names (large intermediate collections)
let filtered_names = self.apply_pattern_filter(package_names, pattern);
// Complex mixed logic for details
let package_items = self.collect_package_details(filtered_names, &scope)?;
```

#### NEW: Efficient Streaming Processing
```rust
match scope {
    ListScope::Installed => self.process_installed_packages(&mut items, pattern, false)?,
    ListScope::Available => self.process_available_packages(&mut items, pattern)?,
    ListScope::Upgradable => self.process_installed_packages(&mut items, pattern, true)?,
    ListScope::All => {
        self.process_installed_packages(&mut items, pattern, false)?;
        self.process_available_packages(&mut items, pattern)?;
    }
}
```

### Memory Efficiency Benefits
- **No Large Collections**: Eliminated intermediate `HashSet<String>` collections
- **Early Filtering**: Pattern matching applied during iteration, not after
- **Streaming Output**: Package items created and added immediately
- **Separated Concerns**: Clean separation between installed and available logic
- **Reduced Complexity**: No mixed-mode processing logic

### Conditional Data Loading
The list command implements performance optimizations by skipping expensive data structures:

```rust
// In populate_repoindex_data()
let load_provides = config().subcommand != "list";
if load_provides {
    shard.provide2pkgnames = deserialize_provide2pkgnames(&path)?;
} else {
    shard.provide2pkgnames = HashMap::new(); // Skip for list command
}
```

### Efficient Pattern Matching
- **Algorithm**: Split pattern by '*' and match segments in order
- **Optimization**: Early termination for non-matching patterns
- **Test Coverage**: 7 comprehensive test functions covering all pattern types

### Caching Strategy
- **Repository packages**: `pkgname2package: HashMap<String, Vec<Arc<Package>>>`
- **Local packages**: `pkgline2package: HashMap<String, Arc<Package>>`
- **Installation info**: Single load of installed-packages.json per command

## Version Comparison System

### Advanced Version Parsing
Implements comprehensive version comparison supporting both RPM and Debian formats:

```rust
// Format: [epoch:]upstream_version[-revision]
// Examples:
// "1.0" -> epoch=0, upstream="1.0", revision="0"
// "2:1.0-rc1" -> epoch=2, upstream="1.0-rc1", revision="0"
// "1.0-rc1-5" -> epoch=0, upstream="1.0-rc1", revision="5"
```

### Parsing Rules
- **Epoch**: Optional number before colon (:), defaults to 0
- **Upstream**: Main version part, may contain dashes for pre-release markers (e.g., `"1.0-rc1"`)
- **Revision**: Optional part after last dash that **STARTS WITH A DIGIT**
  - Debian: debian_revision (e.g., `"1.0-5"`, `"1.0-1ubuntu2"`)
  - RPM: release (e.g., `"1.0-2.el8"`, `"1.0-1.fc35"`)

### Pre-release Handling
Correctly handles pre-release versions:
- `"1.0-rc1"` < `"1.0"` (pre-release < final)
- `"1.0-beta"` < `"1.0-rc1"` (alphabetical for same base)
- `"2.0-rc1"` > `"1.0-rc2"` (version precedence)

### Comparison Priority
1. **Epoch** (highest priority)
2. **Upstream version** (supports semantic versioning + Debian rules)
3. **Revision** (lowest priority)

## Output Format

### Display Layout
```
Installation=Exposed/Installed/Available
| Depth=0-9/Essential/_(not-installed)
|/ Upgrade=Upgradable/ (no-upgrade-available)
||/ Name                            Version                        Arch         Repo                 Description
+++-===============================-==============================-============-====================-========================================
I1  bash                            5.2.15-2+b2                   amd64        debian:bookworm      GNU Bourne Again SHell
E0U vim                             9.0.1378-2                    amd64        debian:bookworm      Vi IMproved - enhanced vi editor
A__ python3-dev                     3.11.2-1+b1                   amd64        debian:bookworm      Header files for Python 3.x
```

### Status Column (3 characters)

#### Position 1 - Installation/Exposure Status
- **`E`** - Exposed package (appbin_flag == true, in ebin/)
- **`I`** - Installed package
- **`A`** - Available (not installed)

#### Position 2 - Depth/Essential Status
- **`0-9`** - Installation depth (for installed packages)
- **`E`** - Essential package (system-critical)
- **`_`** - Not installed

#### Position 3 - Upgrade Status
- **`U`** - Upgradable (newer version available)
- **` `** - No upgrade available or not applicable

### Column Details
- **Name**: Package name (max 31 chars)
- **Version**: Package version (max 30 chars)
- **Arch**: Architecture (max 12 chars)
- **Repo**: Repository/source name (max 20 chars)
- **Description**: Package summary (max 60 chars, truncated with "..")

## Implementation Architecture

### Core Components

#### Main Entry Point
```rust
pub fn list_packages_with_scope(&mut self, scope: ListScope, pattern: &str) -> Result<()>
```
- Coordinates entire listing process using **streaming architecture**
- Loads installed packages once
- Delegates to streaming data collection methods
- Sorts and displays results

#### Streaming Data Collection Pipeline
The new architecture uses a **two-step streaming approach** for optimal performance:

1. **Streaming Collection**: `collect_package_items_streaming(scope, pattern)`
   - Processes each data source independently
   - Applies pattern filtering **early** during iteration
   - Avoids large intermediate collections
   - Reduces memory usage significantly

2. **Scope-Specific Processing**:
   - **Installed/Upgradable**: `process_installed_packages()` - streams through installed packages
   - **Available**: `process_available_packages()` - streams through repository indices
   - **All**: Combines both streaming processes

#### Streaming Processing Methods

**For Installed Packages**:
```rust
fn process_installed_packages(&mut self, items: &mut Vec<PackageListItem>, pattern: &str, upgradable_only: bool) -> Result<()>
```
- **Data Source**: `self.installed_packages` (single load per command)
- **Streaming**: Iterates directly over installed packages
- **Early Filtering**: Pattern matching applied during iteration
- **Upgrade Detection**: Optional filtering for upgradable packages only
- **Memory Efficient**: No intermediate collections

**For Available Packages**:
```rust
fn process_available_packages(&mut self, items: &mut Vec<PackageListItem>, pattern: &str) -> Result<()>
```
- **Data Source**: Repository shard indices (`pkgname2ranges.keys()`)
- **Streaming**: Iterates through all repository shards
- **Early Filtering**: Pattern matching + installation status checks during iteration
- **Architecture Filtering**: Skip incompatible architectures
- **Exclusion**: Automatically excludes already installed packages

### Upgrade Detection System

#### Version Comparison
```rust
fn is_package_upgradable(&mut self, pkgname: &str, installed_info: &InstalledPackageInfo) -> Result<bool>
```
- **Process**:
  1. Extract installed version from pkgline using `parse_package_line()`
  2. Get available packages with `map_pkgname2packages()`
  3. Compare versions using `crate::version::is_version_newer()`
  4. Check architecture compatibility

#### Architecture Handling
- **Priority**: Exact architecture matches preferred
- **Fallback**: Allow empty architecture as compatible
- **Filter**: Skip incompatible architectures

### Enhanced Status Determination

#### For Installed Packages
```rust
fn determine_status_for_installed(&mut self, pkgname: &str, installed_info: &InstalledPackageInfo) -> Result<String>
```
- **Position 1**: Check appbin_flag for exposed status
- **Position 2**: Essential check via `is_essential_pkgname()`, then depth
- **Position 3**: Upgrade check via `is_package_upgradable()`

#### For Available Packages
```rust
fn determine_status_for_available(&self, pkgname: &str) -> Result<String>
```
- **Fixed format**: "A_ " (Available, Not installed, No upgrade status)

### Local Package Support

#### Orphaned Package Handling
- **Detection**: Installed packages not found in repositories
- **Source**: Load from local store via `map_pkgline2package()`
- **Display**: Show as "local" or "orphaned" repository
- **Fallback**: Basic info if local loading fails

#### Store Integration
```rust
pub fn map_pkgline2package(&mut self, pkgline: &str) -> Result<Arc<Package>>
```
- **Path**: `${EPKG_STORE}/${pkgline}/info/package.txt`
- **Caching**: Implemented with `pkgline2package` HashMap
- **Error handling**: Graceful fallback for missing local data

## Error Handling & Robustness

### Graceful Degradation
- **Missing installed packages**: Empty list instead of error
- **Repository unavailability**: Continue with available data
- **Version parsing failures**: Fall back to string comparison
- **Local store access**: Fall back to basic package info

### Error Recovery
- **Invalid patterns**: No matches returned instead of crash
- **Memory mapped file issues**: Graceful error reporting
- **Network/cache issues**: Use cached data when possible

### Logging Strategy
- **Debug**: Pattern matching details, version comparisons
- **Info**: Package counts, scope selections
- **Warn**: Missing data, parsing failures
- **Error**: Critical failures with context

## Test Coverage

### Pattern Matching Tests (7 functions)
- `test_matches_glob_pattern_no_wildcards` - Substring matching
- `test_matches_glob_pattern_prefix` - Prefix patterns (`bash*`)
- `test_matches_glob_pattern_suffix` - Suffix patterns (`*dev`)
- `test_matches_glob_pattern_contains` - Contains patterns (`*bash*`)
- `test_matches_glob_pattern_complex` - Multi-wildcard patterns
- `test_matches_glob_pattern_edge_cases` - Edge cases and empty patterns
- `test_matches_glob_pattern_real_world_examples` - Real package names

### Version Comparison Tests (13 functions)
- **Epoch comparison**: Priority testing
- **Tilde precedence**: Pre-release handling
- **Numeric comparison**: Proper numeric vs lexicographic
- **Revision comparison**: Debian/RPM revision handling
- **Complex versions**: Real-world Debian examples
- **Character precedence**: Debian character ordering rules
- **Missing components**: Default value handling
- **Upstream with dashes**: Pre-release vs revision distinction
- **Parsing accuracy**: Component extraction verification
- **Semantic versions**: Integration with versions crate

## Usage Examples

### Basic Listing
```bash
# List all installed packages (default)
epkg list

# List with pattern matching
epkg list vim*          # All packages starting with "vim"
epkg list *-dev         # All development packages
epkg list *python*      # All packages containing "python"
```

### Scope-based Listing
```bash
# Available packages only
epkg list --available
epkg list --available *java*

# Upgradable packages
epkg list --upgradable
epkg list --upgradable vim*

# All packages (installed + available)
epkg list --all
epkg list --all *kernel*
```

### Advanced Patterns
```bash
# Complex glob patterns
epkg list b*sh                    # bash, bush, brush
epkg list java*openjdk*headless   # java-X-openjdk-headless variants
epkg list lib*-dev                # library development packages

# Everything
epkg list --all "*"              # All packages in all repos
```

### Output Interpretation
```bash
# Status examples:
I1  package-name  # Installed, depth 1, no upgrade
E0U another-pkg   # Exposed, depth 0 (direct), upgrade available
A__ third-pkg     # Available only, not installed
IE  essential     # Installed essential package
```

## Future Enhancements

### Planned Features
- **Sorting options**: By name, version, size, installation date
- **Column customization**: User-selectable columns
- **Output formats**: JSON, YAML, CSV export options
- **Pagination**: For large package lists
- **Search highlighting**: Highlight pattern matches in output
