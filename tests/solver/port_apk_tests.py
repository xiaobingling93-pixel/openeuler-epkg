#!/usr/bin/env python3
"""
Script to port tests from apk-tools format to epkg YAML format.
Converts .test, .repo, and .installed files to YAML format.
"""

import os
import re
import yaml
from pathlib import Path
from typing import Dict, List, Optional, Tuple

SOURCE_DIR = Path("/c/package-managers/apk-tools/test/solver")
TARGET_DIR = Path("/c/epkg/tests/solver")

def parse_apk_index_file(file_path: Path) -> List[Dict]:
    """Parse an APK index format file (.repo or .installed) and return list of packages."""
    packages = []
    current_pkg = {}

    with open(file_path, 'r') as f:
        for line in f:
            line = line.strip()
            if not line:
                if current_pkg:
                    packages.append(current_pkg)
                    current_pkg = {}
                continue

            if ':' in line:
                key, value = line.split(':', 1)
                key = key.strip()
                value = value.strip()

                if key == 'C':
                    # Checksum - skip
                    continue
                elif key == 'P':
                    current_pkg['pkgname'] = value
                elif key == 'V':
                    current_pkg['version'] = value
                elif key == 'A':
                    current_pkg['arch'] = value
                elif key == 'S':
                    # Size - skip
                    continue
                elif key == 'I':
                    # Install size - skip
                    continue
                elif key == 'D':
                    # Dependencies/requires
                    if 'requires' not in current_pkg:
                        current_pkg['requires'] = []
                    # Handle conflicts (!xxx)
                    deps = value.split()
                    for dep in deps:
                        if dep.startswith('!'):
                            # Convert !xxx to conflicts
                            conflict = dep[1:]  # Remove !
                            if 'conflicts' not in current_pkg:
                                current_pkg['conflicts'] = []
                            current_pkg['conflicts'].append(conflict)
                        else:
                            current_pkg['requires'].append(dep)
                elif key == 'p':
                    # Provides
                    if 'provides' not in current_pkg:
                        current_pkg['provides'] = []
                    current_pkg['provides'].append(value)
                elif key == 'k':
                    # Key - skip
                    continue
                elif key == 'i':
                    # Install-if - skip for now
                    continue

    if current_pkg:
        packages.append(current_pkg)

    return packages

def convert_package_to_yaml(pkg: Dict) -> Dict:
    """Convert a parsed package dict to YAML format."""
    result = {
        'pkgname': pkg.get('pkgname', ''),
        'version': pkg.get('version', '1'),
        'arch': pkg.get('arch', 'x86_64'),
    }

    if 'requires' in pkg and pkg['requires']:
        result['requires'] = pkg['requires']
    # Don't include empty requires list - will be cleaned later anyway

    if 'conflicts' in pkg and pkg['conflicts']:
        result['conflicts'] = pkg['conflicts']

    # Handle provides
    arch = result['arch']
    result['provides'] = []

    # Add custom provides if any
    if 'provides' in pkg and pkg['provides']:
        for provide in pkg['provides']:
            result['provides'].append(provide)
            # For versioned provides like "cmd:b=2", don't add arch variant
            # For unversioned provides, add arch variant
            if '=' not in provide:
                result['provides'].append(f"{provide}({arch}) = {result['version']}")

    # Always add default provides
    result['provides'].append(f"{result['pkgname']} = {result['version']}")
    result['provides'].append(f"{result['pkgname']}({arch}) = {result['version']}")

    return result

def parse_test_file(file_path: Path) -> Dict:
    """Parse a .test file and return test metadata."""
    test_data = {
        'args': '',
        'repo': [],
        'installed': None,
        'world': None,
        'expect': []
    }

    current_section = None
    with open(file_path, 'r') as f:
        for line in f:
            line = line.strip()
            if line.startswith('@ARGS'):
                test_data['args'] = line[5:].strip()
            elif line.startswith('@REPO'):
                repo_line = line[5:].strip()
                # Handle @REPO @tag filename.repo format
                parts = repo_line.split()
                if len(parts) >= 2 and parts[0].startswith('@'):
                    tag = parts[0][1:]  # Remove @
                    repo_file = parts[1]
                    test_data['repo'].append((tag, repo_file))
                else:
                    test_data['repo'].append((None, repo_line))
            elif line.startswith('@CACHE'):
                # Treat @CACHE the same as @REPO
                cache_line = line[6:].strip()
                # Handle @CACHE @tag filename.repo format
                parts = cache_line.split()
                if len(parts) >= 2 and parts[0].startswith('@'):
                    tag = parts[0][1:]  # Remove @
                    repo_file = parts[1]
                    test_data['repo'].append((tag, repo_file))
                else:
                    test_data['repo'].append((None, cache_line))
            elif line.startswith('@INSTALLED'):
                test_data['installed'] = line[10:].strip()
            elif line.startswith('@WORLD'):
                test_data['world'] = line[7:].strip()
            elif line.startswith('@EXPECT'):
                current_section = 'expect'
            elif current_section == 'expect' and line:
                test_data['expect'].append(line)

    return test_data

def clean_yaml_data(data):
    """Recursively clean YAML data by removing trivial/default values.

    Removes:
    - arch: x86_64 (default arch)
    - depend_depth: 0 (default depth)
    - install_time: 1000000000 (default time)
    - ebin_exposure
    - Empty arrays: rdepends: [], depends: [], ebin_links: []
    - skip: false (default skip value)
    - Converts empty dicts to None (for plan entries, so they output as `key:` instead of `key: {}`)
    """
    if isinstance(data, dict):
        cleaned = {}
        for key, value in data.items():
            # Skip trivial/default values
            if key == 'arch' and value == 'x86_64':
                continue
            if key == 'depend_depth' and value == 0:
                continue
            if key == 'install_time' and value == 1000000000:
                continue
            if key == 'ebin_exposure' and value in (True, False):
                continue
            if key == 'skip' and value is False:
                continue
            # Skip empty arrays
            if isinstance(value, list) and len(value) == 0:
                continue
            # Recursively clean nested structures
            cleaned_value = clean_yaml_data(value)
            # Convert empty dicts to None (for plan entries, so they output as `key:` instead of `key: {}`)
            if cleaned_value == {}:
                cleaned_value = None
            # Convert None to empty dict initially, but we'll convert back to None for plan entries
            if cleaned_value is None:
                cleaned_value = {}
            # Only include non-empty values (but allow empty dicts for plan entries)
            if cleaned_value != '':
                # Always include dicts (even empty ones, for plan entries), non-empty lists, and non-empty non-collection values
                if isinstance(cleaned_value, dict) or (isinstance(cleaned_value, list) and len(cleaned_value) > 0) or (not isinstance(cleaned_value, (list, dict)) and cleaned_value):
                    cleaned[key] = cleaned_value
        return cleaned
    elif isinstance(data, list):
        return [clean_yaml_data(item) for item in data]
    else:
        return data

def convert_empty_dicts_to_none(data, in_plan=False):
    """Convert empty dicts to None for plan entries so they output as `key:` instead of `key: {}`."""
    if isinstance(data, dict):
        result = {}
        for key, value in data.items():
            # Check if we're in a plan section
            is_plan_section = in_plan or key in ('fresh_installs', 'upgrades_new', 'upgrades_old', 'old_removes')
            if isinstance(value, dict):
                if len(value) == 0 and is_plan_section:
                    # Convert empty dict to None for plan entries
                    result[key] = None
                else:
                    result[key] = convert_empty_dicts_to_none(value, is_plan_section)
            elif isinstance(value, list):
                result[key] = [convert_empty_dicts_to_none(item, is_plan_section) for item in value]
            else:
                result[key] = value
        return result
    elif isinstance(data, list):
        return [convert_empty_dicts_to_none(item, in_plan) for item in data]
    else:
        return data

def parse_installed_file(file_path: Path) -> Dict:
    """Parse an .installed file and return installed packages dict."""
    packages = parse_apk_index_file(file_path)
    installed = {}

    for pkg in packages:
        pkgname = pkg.get('pkgname', '')
        version = pkg.get('version', '1')
        arch = pkg.get('arch', 'x86_64')
        pkgkey = f"{pkgname}__{version}__{arch}"

        installed[pkgkey] = {
            #  'pkgline': f"fake_hash__{pkgname}__{version}__{arch}",
            'arch': arch,
            'depend_depth': 0,
            'install_time': 1000000000,
            'ebin_exposure': True,
            'rdepends': [],
            'depends': [],
            'ebin_links': []
        }

    # Clean the installed data
    installed = clean_yaml_data(installed)
    return installed

def extract_request_from_args(args: str) -> Tuple[Dict[str, List[str]], Dict]:
    """Extract package requests from @ARGS line.
    Returns a tuple of (request_dict, flags_dict) where:
    - request_dict has 'install', 'upgrade', 'remove' fields
    - flags_dict has 'force' (for --force), 'upgrade_flag' (for --upgrade), 'ignore' (list of ignored packages)
    Returns None if the command is 'fix' (should skip this test).
    """
    parts = args.split()
    request = {
        'install': [],
        'upgrade': [],
        'remove': []
    }
    flags = {
        'force': False,
        'upgrade_flag': False,
        'ignore': []
    }
    current_command = None
    i = 0

    while i < len(parts):
        if parts[i] == 'add':
            current_command = 'install'
            i += 1
        elif parts[i] == 'upgrade':
            current_command = 'upgrade'
            i += 1
        elif parts[i] == 'fix':
            # Skip tests with 'fix' command
            return None
        elif parts[i] == 'del':
            current_command = 'remove'
            i += 1
        elif parts[i] == '--force':
            flags['force'] = True
            i += 1
        elif parts[i] == '--upgrade':
            flags['upgrade_flag'] = True
            # When --upgrade is used with 'add', use upgrade instead of install
            if current_command == 'install':
                current_command = 'upgrade'
            i += 1
        elif parts[i] == '--ignore':
            # --ignore flag: collect packages after --ignore until next flag or end
            i += 1
            while i < len(parts) and not parts[i].startswith('--'):
                flags['ignore'].append(parts[i])
                i += 1
        elif parts[i] == '--no-network':
            # Ignore --no-network option
            i += 1
            continue
        elif parts[i] == '-a':
            # Ignore -a option (short flag)
            i += 1
            continue
        elif parts[i].startswith('--'):
            # Ignore other unknown options
            i += 1
            continue
        elif parts[i].startswith('-') and len(parts[i]) > 1:
            # Ignore other short flags (like -a, -b, etc.)
            i += 1
            continue
        else:
            if current_command:
                request[current_command].append(parts[i])
            i += 1

    return request, flags

def extract_plan_from_expect(expect: List[str]) -> Tuple[Dict, List[str], bool]:
    """Extract InstallationPlan from @EXPECT section."""
    plan = {}
    missing = []
    expect_fail = False

    fresh_installs = {}
    upgrades_new = {}
    upgrades_old = {}
    old_removes = {}

    for line in expect:
        if 'ERROR:' in line or 'breaks:' in line:
            expect_fail = True
            continue
        # Match lines like "(1/2) Installing a (2)" - fresh install
        match = re.search(r'Installing\s+(\S+)\s+\((\S+)\)', line)
        if match:
            pkgname = match.group(1)
            version = match.group(2)
            pkgkey = f"{pkgname}__{version}__x86_64"
            fresh_installs[pkgkey] = {}
        # Match lines like "Replacing a (1 -> 2)" - upgrade
        match = re.search(r'Replacing\s+(\S+)\s+\((\S+)\s+->\s+(\S+)\)', line)
        if match:
            pkgname = match.group(1)
            old_version = match.group(2)
            new_version = match.group(3)
            old_pkgkey = f"{pkgname}__{old_version}__x86_64"
            new_pkgkey = f"{pkgname}__{new_version}__x86_64"
            upgrades_old[old_pkgkey] = {}
            upgrades_new[new_pkgkey] = {}
        # Match lines like "Upgrading a (1 -> 2)" - upgrade
        match = re.search(r'Upgrading\s+(\S+)\s+\((\S+)\s+->\s+(\S+)\)', line)
        if match:
            pkgname = match.group(1)
            old_version = match.group(2)
            new_version = match.group(3)
            old_pkgkey = f"{pkgname}__{old_version}__x86_64"
            new_pkgkey = f"{pkgname}__{new_version}__x86_64"
            upgrades_old[old_pkgkey] = {}
            upgrades_new[new_pkgkey] = {}
        # Match lines like "Purging a (2)" - removal
        match = re.search(r'Purging\s+(\S+)\s+\((\S+)\)', line)
        if match:
            pkgname = match.group(1)
            version = match.group(2)
            pkgkey = f"{pkgname}__{version}__x86_64"
            old_removes[pkgkey] = {}

    # Only include non-empty fields
    if fresh_installs:
        plan['fresh_installs'] = fresh_installs
    if upgrades_new:
        plan['upgrades_new'] = upgrades_new
    if upgrades_old:
        plan['upgrades_old'] = upgrades_old
    if old_removes:
        plan['old_removes'] = old_removes

    return plan, missing, expect_fail

def get_test_category(test_name: str) -> str:
    """Determine the category/subdirectory for a test."""
    if test_name.startswith('basic'):
        return 'basic'
    elif test_name.startswith('complicated'):
        return 'complicated'
    elif test_name.startswith('conflict'):
        return 'conflict'
    elif test_name.startswith('error'):
        return 'error'
    elif test_name.startswith('fuzzy'):
        return 'fuzzy'
    elif test_name.startswith('installif'):
        return 'installif'
    elif test_name.startswith('pinning'):
        return 'pinning'
    elif test_name.startswith('provides'):
        return 'provides'
    elif test_name.startswith('selfupgrade'):
        return 'selfupgrade'
    elif test_name.startswith('upgrade'):
        return 'upgrade'
    elif test_name.startswith('fix'):
        return 'fix'
    else:
        return 'misc'

def get_test_number(test_name: str) -> int:
    """Extract test number from test name."""
    # Extract number from names like basic1, basic17, error10, etc.
    match = re.search(r'(\d+)$', test_name)
    if match:
        return int(match.group(1))
    return 1

def port_test(test_file: Path):
    """Port a single test file."""
    test_name = test_file.stem
    category = get_test_category(test_name)
    test_num = get_test_number(test_name)

    # Create target directory
    target_dir = TARGET_DIR / category
    target_dir.mkdir(parents=True, exist_ok=True)

    # Parse test file
    test_data = parse_test_file(test_file)

    # Convert repo files
    repo_files = []
    for tag, repo_file in test_data['repo']:
        repo_path = SOURCE_DIR / repo_file
        if repo_path.exists():
            packages = parse_apk_index_file(repo_path)
            yaml_packages = [convert_package_to_yaml(pkg) for pkg in packages]
            # Clean the repo packages
            yaml_packages = clean_yaml_data(yaml_packages)

            # Determine repo filename - use original repo filename without extension
            # But if it's the standard "category.repo", use "repo.yaml" for consistency
            repo_base = Path(repo_file).stem  # Remove .repo extension
            if repo_base == category or repo_base == f"{category}1":
                # Standard repo file, use repo.yaml
                if tag:
                    repo_filename = f"repo_{tag}.yaml"
                else:
                    repo_filename = "repo.yaml"
            else:
                # Custom repo file, use its name
                if tag:
                    repo_filename = f"{repo_base}_{tag}.yaml"
                else:
                    repo_filename = f"{repo_base}.yaml"

            repo_target = target_dir / repo_filename
            with open(repo_target, 'w') as f:
                yaml.dump(yaml_packages, f, default_flow_style=False, sort_keys=False)

            repo_files.append(repo_filename)

    # Convert installed file if present
    installed_file = None
    if test_data['installed']:
        installed_path = SOURCE_DIR / test_data['installed']
        if installed_path.exists():
            installed = parse_installed_file(installed_path)
            installed_filename = "installed.yaml"
            installed_target = target_dir / installed_filename
            with open(installed_target, 'w') as f:
                yaml.dump(installed, f, default_flow_style=False, sort_keys=False)
            installed_file = installed_filename

    # Create test YAML
    result = extract_request_from_args(test_data['args'])

    # Skip tests with 'fix' command
    if result is None:
        print(f"Skipping {test_file.name} (contains 'fix' command)")
        return

    request, flags = result

    if test_data['world']:
        # @WORLD represents packages that should remain installed
        world_pkgs = test_data['world'].split()
        # If there's a remove command, remove those packages from @WORLD first
        if request['remove']:
            # Remove packages being deleted from @WORLD
            world_pkgs = [pkg for pkg in world_pkgs if pkg not in request['remove']]

        # Add remaining @WORLD packages to the appropriate list based on the command
        # Check the original args to determine if the command was 'upgrade' (not 'add --upgrade')
        args_lower = test_data['args'].lower()
        args_parts = args_lower.split()
        is_upgrade_command = args_parts and args_parts[0] == 'upgrade'

        if world_pkgs:
            if is_upgrade_command:
                # If the command is 'upgrade', add @WORLD packages to upgrade
                request['upgrade'].extend(world_pkgs)
                request['upgrade'] = list(dict.fromkeys(request['upgrade']))
            else:
                # Otherwise, add to install (to ensure they remain installed)
                request['install'].extend(world_pkgs)
                request['install'] = list(dict.fromkeys(request['install']))

    # Remove ignored packages from upgrade list (after @WORLD has been processed)
    if flags.get('ignore') and request['upgrade']:
        request['upgrade'] = [pkg for pkg in request['upgrade'] if pkg not in flags['ignore']]

    plan, missing, expect_fail = extract_plan_from_expect(test_data['expect'])

    test_yaml = {
        'format': 'apk',
        'description': f"{category} test {test_num}",
        'skip': False,
        'repo': ' '.join(repo_files),
    }

    if installed_file:
        test_yaml['installed'] = installed_file

    # Handle --force flag: add config.ignore_missing: true
    if flags['force']:
        test_yaml['config'] = {
            'ignore_missing': True
        }

    # Handle --upgrade flag: if --upgrade was used with 'add', packages should already be in upgrade
    # (The parsing logic handles this by changing current_command when --upgrade is encountered)
    # No additional processing needed here since packages are already in the correct list

    # Only include non-empty request fields
    if request['install']:
        test_yaml['install'] = request['install']
    if request['upgrade']:
        test_yaml['upgrade'] = request['upgrade']
    if request['remove']:
        test_yaml['remove'] = request['remove']

    # Only include plan if it has any content
    if plan:
        test_yaml['plan'] = plan

    # Only include missing if it has content
    if missing:
        test_yaml['missing'] = missing

    # Only include expect_fail if it's True
    if expect_fail:
        test_yaml['expect_fail'] = expect_fail

    # Determine test filename
    test_filename = f"test{test_num}.yaml"
    test_target = target_dir / test_filename

    # Remove empty fields before dumping
    test_yaml = {k: v for k, v in test_yaml.items() if v not in ([], {}, None, '')}

    # Clean the test YAML data
    test_yaml = clean_yaml_data(test_yaml)

    # Convert empty dicts to None in plan entries so they output as `key:` instead of `key: {}`
    test_yaml = convert_empty_dicts_to_none(test_yaml)

    # Dump to string first, then post-process to replace `: null` with `:`
    yaml_str = yaml.dump(test_yaml, default_flow_style=False, sort_keys=False, allow_unicode=True)
    # Replace `: null` with `:` (but not `: null` in the middle of a value)
    yaml_str = re.sub(r': null$', ':', yaml_str, flags=re.MULTILINE)

    with open(test_target, 'w') as f:
        f.write(yaml_str)

    print(f"Ported {test_file.name} -> {test_target}")

def main():
    """Main function to port all tests."""
    # Get all .test files
    test_files = sorted(SOURCE_DIR.glob("*.test"))

    for test_file in test_files:
        try:
            port_test(test_file)
        except Exception as e:
            print(f"Error porting {test_file.name}: {e}")
            import traceback
            traceback.print_exc()

if __name__ == '__main__':
    main()

