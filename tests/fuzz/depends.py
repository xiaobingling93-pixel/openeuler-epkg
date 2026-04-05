#!/usr/bin/env python3

import argparse
import os
import re
import subprocess
import sys
import time
from typing import List, Optional

# Default values
DEFAULT_BATCH_M = 10
DEFAULT_BATCH_N = 100
ALL_OSES = ['openeuler', 'opensuse', 'fedora', 'debian', 'ubuntu', 'archlinux', 'alpine', 'conda']


def script_name() -> str:
    """Get the script name for display in usage and debug output."""
    return sys.argv[0]


def debug_solve(os_name: str, pkg_to_install: str, grep_pkg: str = None):
    """Run debug solve with RUST_LOG=debug and output to /tmp/dd.

    Usage: depends.py --debug --os=<OS> <pkg_to_install> [--grep=<PKG>]

    Example:
        depends.py --debug --os=fedora vtk
        depends.py --debug --os=fedora vtk --grep=hdf5
    """
    debug_log = '/tmp/dd'
    cmd = ['epkg', '--assume-no', '-e', os_name, 'install', '--no-install-essentials', pkg_to_install]

    # Run with RUST_LOG=debug
    import os as os_module
    env = os_module.environ.copy()
    env['RUST_LOG'] = 'debug'

    result = subprocess.run(cmd, capture_output=True, text=True, env=env)
    # Strip DEBUG prefix from log lines
    debug_content = re.sub(r'^.*DEBUG ', '', result.stderr, flags=re.MULTILINE)

    # Write to debug log file
    with open(debug_log, 'w') as f:
        f.write(debug_content)
        f.write(f"\n\nreproduce command:\n")
        f.write(f"    RUST_LOG=debug epkg --assume-no -e {os_name} install --no-install-essentials {pkg_to_install}\n")
        f.write(f"\npackage metadata query command:\n")
        f.write(f"    epkg -e {os_name} info {pkg_to_install}\n")

        if grep_pkg:
            f.write(f"\n    epkg -e {os_name} info {grep_pkg}\n")
            f.write(f"\ndebug log grep command:\n")
            f.write(f"    grep -F -C3 '{grep_pkg}' {debug_log}\n")

    print(f"Debug log written to {debug_log}")

    if grep_pkg:
        # Show grep output for the problematic package
        grep_result = subprocess.run(
            ['grep', '-F', '-C3', grep_pkg, debug_log],
            capture_output=True, text=True
        )
        if grep_result.stdout:
            print(f"\nGrep output for '{grep_pkg}':")
            print(grep_result.stdout)

    # Show the log with less
    subprocess.run(['less', debug_log])


def load_error_whitelist() -> List[str]:
    """Load error whitelist patterns from a text file."""
    # Get the directory where this script is located
    script_dir = os.path.dirname(os.path.abspath(__file__))
    whitelist_file = os.path.join(script_dir, 'whitelist.txt')

    whitelist = []
    try:
        with open(whitelist_file, 'r', encoding='utf-8') as f:
            for line in f:
                line = line.strip()
                # Skip empty lines and comments
                if line and not line.startswith('#'):
                    whitelist.append(line)
    except FileNotFoundError:
        # If file doesn't exist, return empty list (no whitelist entries)
        pass
    except Exception as e:
        print(f"Warning: Failed to load error whitelist from {whitelist_file}: {e}", file=sys.stderr)

    return whitelist


# Load error whitelist from file
ERROR_WHITELIST = load_error_whitelist()

def usage():
    """Print usage information."""
    script_name = sys.argv[0]
    print(f"""Usage: {script_name} [OPTIONS] [PACKAGES...]

Test package dependencies across different OS environments.

Options:
  --debug               Debug mode: run RUST_LOG=debug for single package
  --grep=<PKG>          In debug mode, grep this package in debug log
  --solver=<SOLVER>     Dependency solver to use: 'constraints' or 'simple'
  --os=<OS_LIST>        Limit testing to specified OSes (comma-separated, e.g., debian,ubuntu)
  --batch=MxN           Customize batch size: M packages per batch, N batches (default: {DEFAULT_BATCH_M}x{DEFAULT_BATCH_N})
  --seed=<SEED>         Random seed for package selection (default: current time)

Arguments:
  PACKAGES...           Space-separated list of packages to test
                        If not specified, randomly selects packages from available list per OS

Examples:
  {script_name}                                    # Test random packages (10x100 batches) on all OSes
  {script_name} --os=debian,ubuntu                 # Test random packages on debian and ubuntu only
  {script_name} --batch=5x20 --os=fedora           # Test 5x20 random packages on fedora only
  {script_name} --seed=12345                       # Test with specific random seed
  {script_name} bash curl                          # Test bash and curl on all OSes
  {script_name} --solver=constraints bash curl     # Test with constraints solver
  {script_name} --os=debian --solver=simple wine   # Test wine on debian with simple solver
  {script_name} --debug --os=fedora vtk            # Debug mode: run with RUST_LOG=debug
  {script_name} --debug --os=fedora vtk --grep=hdf5  # Debug + grep for hdf5

Typical Stress Test + Debug Workflow:

  Run these in parallel in separate terminals:
    {script_name} --os=debian
    {script_name} --os=ubuntu
    {script_name} --os=openeuler
    {script_name} --os=opensuse
    {script_name} --os=fedora
    {script_name} --os=archlinux
    {script_name} --os=alpine
    {script_name} --os=conda

  On test error:
    1. If it's a real package missing/conflicting issue:
       vim tests/fuzz/whitelist.txt
       # Add the broken package pattern to ignore

    2. If it's a solver bug:
       # Run the debug_solve() function shown in the error output
       # Or manually run with RUST_LOG=debug and grep for pkg_no_candidate

    3. In rare cases, you may need to grep and check repodata:
       grep <pkg_no_candidate> ~/.cache/epkg/channels/<os>-*/*/*/provide2pkgnames.yaml
       grep <pkg_no_candidate> ~/.cache/epkg/channels/<os>-*/*/*/packages.txt
""")


def parse_args(args: List[str]) -> argparse.Namespace:
    """Parse command line arguments."""
    parser = argparse.ArgumentParser(
        description='Test package dependencies across different OS environments.',
        add_help=False
    )

    parser.add_argument('-h', '--help', action='store_true', help='Show help message')
    parser.add_argument('--debug', action='store_true', help='Debug mode: run RUST_LOG=debug for single package')
    parser.add_argument('--grep', type=str, help='In debug mode, grep this package in debug log')
    parser.add_argument('--solver', type=str, choices=['constraints', 'simple'],
                       help="Dependency solver to use: 'constraints' or 'simple'")
    parser.add_argument('--os', type=str,
                       help='Limit testing to specified OSes (comma-separated, e.g., debian,ubuntu)')
    parser.add_argument('--batch', type=str, default=f'{DEFAULT_BATCH_M}x{DEFAULT_BATCH_N}',
                       help=f'Customize batch size: M packages per batch, N batches (default: {DEFAULT_BATCH_M}x{DEFAULT_BATCH_N})')
    parser.add_argument('--seed', type=int, default=int(time.time()),
                       help='Random seed for package selection (default: current time)')
    parser.add_argument('packages', nargs='*', help='Packages to test')

    parsed = parser.parse_args(args)

    # Parse batch format
    if parsed.batch:
        try:
            m_str, n_str = parsed.batch.split('x')
            parsed.batch_m = int(m_str)
            parsed.batch_n = int(n_str)
        except ValueError:
            print(f"Error: Invalid batch format '{parsed.batch}'. Must be MxN (e.g., 10x100)", file=sys.stderr)
            sys.exit(1)
    else:
        parsed.batch_m = DEFAULT_BATCH_M
        parsed.batch_n = DEFAULT_BATCH_N

    # Parse OS list
    if parsed.os:
        parsed.selected_oses = [os.strip() for os in parsed.os.split(',')]
    else:
        parsed.selected_oses = ALL_OSES

    # Determine if using random packages
    parsed.use_random_packages = len(parsed.packages) == 0

    return parsed


def get_available_packages(os: str) -> List[str]:
    """Get available packages for an OS."""
    try:
        result = subprocess.run(
            ['epkg', '-e', os, 'list', '--available'],
            capture_output=True,
            text=True,
            check=False
        )

        packages = []
        for line in result.stdout.split('\n'):
            # Skip header lines (first 3 lines) and only process lines starting with A_
            if line.startswith('A'):
                parts = line.split()
                if len(parts) >= 8:
                    packages.append(parts[4])

        return sorted(set(packages))
    except Exception as e:
        print(f"Warning: Failed to get available packages for {os}: {e}", file=sys.stderr)
        return []


def filter_available_packages(os: str, packages: List[str]) -> List[str]:
    """Filter packages that are available in the OS."""
    available_packages = set(get_available_packages(os))
    return [pkg for pkg in packages if pkg in available_packages]


def select_random_packages(os: str, m: int, n: int, seed: int) -> List[str]:
    """Select random packages from available list using Python random."""
    import random

    total_needed = m * n

    available_packages = get_available_packages(os)

    if not available_packages:
        print(f"Warning: No available packages found for {os}", file=sys.stderr)
        return []

    available_count = len(available_packages)

    if available_count < total_needed:
        print(f"Warning: Only {available_count} packages available for {os}, but {total_needed} needed", file=sys.stderr)
        total_needed = available_count

    # Use Python random with seed for reproducible random selection
    random.seed(seed)
    return random.sample(available_packages, total_needed)


def split_into_batches(m: int, packages: List[str]) -> List[List[str]]:
    """Split packages into batches."""
    batches = []
    for i in range(0, len(packages), m):
        batches.append(packages[i:i + m])
    return batches


def is_whitelisted_error(stdout: str, stderr: str) -> Optional[str]:
    """Check if error output matches any pattern in the whitelist.

    Returns the matched pattern if found, None otherwise.
    """
    combined_output = stdout + stderr
    for pattern in ERROR_WHITELIST:
        if pattern in combined_output:
            return pattern
    return None


def parse_dependency_error(stdout: str, stderr: str) -> Optional[tuple[str, str]]:
    """Parse dependency resolution error to extract package names.

    Returns a tuple of (pkg_to_install, pkg_no_candidate) if found, None otherwise.
    - pkg_to_install: Package from first error line (e.g., "qdldl-python")
    - pkg_no_candidate: Package or capability from last error line (e.g., "pybind11-abi")
    """
    combined_output = stdout + stderr
    lines = combined_output.split('\n')

    pkg_to_install = None
    pkg_no_candidate = None

    # Find first error line with "Dependency resolution failed for"
    for i, line in enumerate(lines):
        if "Dependency resolution failed for" in line:
            # Format: "0: Dependency resolution failed for 2nd pass (REQUIRES only):"
            # Package name is on the next line(s)
            # Look for pattern like "  libstdc++6-pp-gcc13-32bit libstdc++6-pp-gcc13-32bit cannot..."
            # in the following lines
            for j in range(i + 1, min(i + 5, len(lines))):
                next_line = lines[j]
                # Match pattern: "  package-name package-name cannot be installed"
                match = re.search(r'\s+(\S+)\s+(\S+)\s+cannot', next_line)
                if match:
                    pkg_to_install = match.group(1)
                    break
            break

    # Find last line that indicates the final unsatisfied / conflicting dependency.
    for line in reversed(lines):
        if (
            "for which no candidates were found" in line
            or "which cannot be installed because there are no viable options" in line
            or "which conflicts with any installable versions previously reported" in line
        ):
            # Extract package name from the final problematic line.
            # Examples:
            #   "└─ pybind11-abi pybind11-abi(=4), for which no candidates were found."
            #   "└─ numpy-base numpy-base(=1.26.4=py39hb5e798b_0), for which no candidates were found."
            #   "└─ eject eject(>2.1.0), which conflicts with any installable versions previously reported"
            match = re.search(
                r'\s+(\S+)\s+(\S+),\s+',
                line,
            )
            if match:
                pkg_no_candidate = match.group(1)
            break

    if pkg_to_install and pkg_no_candidate:
        return (pkg_to_install, pkg_no_candidate)
    return None


def run_install_test(os: str, batch_packages: List[str], solver_option: Optional[str]) -> bool:
    """Run install test for a batch."""
    # Build command line
    cmd = ['epkg', '--assume-no', '-e', os, 'install', '--no-install-essentials']
    if solver_option:
        cmd.append(solver_option)
    cmd.extend(batch_packages)

    cmdline = ' '.join(cmd)

    # Run command and capture output
    try:
        result = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            check=False
        )

        # Only show output if failed
        if result.returncode != 0:
            # Check if error matches whitelist
            matched_pattern = is_whitelisted_error(result.stdout, result.stderr)
            if matched_pattern:
                print(f"  Failed command: {cmdline}")
                print(f"    (Error matches whitelist: '{matched_pattern}', continuing...)")
                return True

            print(f"  Failed command:\n{cmdline}")
            for line in result.stdout.split('\n'):
                if line.strip():
                    print(f"    {line}")
            for line in result.stderr.split('\n'):
                if line.strip():
                    print(f"    {line}")

            # Try to parse dependency error and show debug command
            parsed_error = parse_dependency_error(result.stdout, result.stderr)
            if parsed_error:
                pkg_to_install, pkg_no_candidate = parsed_error
                # Quote package names to handle special characters (e.g., python3.13dist(numpy))
                print(f"Debug command:")
                print(f"epkg -e {os} info '{pkg_no_candidate}'")
                print(f"{script_name()} --debug --os={os} '{pkg_to_install}' --grep='{pkg_no_candidate}'")

            return False
        else:
            return True
    except Exception as e:
        print(f"  Failed command: {cmdline}")
        print(f"    Exception: {e}")
        return False


def test_os(os: str, args: argparse.Namespace) -> bool:
    """Test packages for a specific OS."""
    print(f"Testing {os}")

    # Create environment
    try:
        subprocess.run(
            ['epkg', 'env', 'create', os, '-c', os],
            capture_output=True,
            check=False
        )
    except Exception as e:
        print(f"  Warning: Failed to create environment: {e}", file=sys.stderr)

    if args.use_random_packages:
        # Select random packages
        packages_to_test = select_random_packages(os, args.batch_m, args.batch_n, args.seed)

        if not packages_to_test:
            print(f"  Skipping {os}: no packages available")
            return True
    else:
        # Use specified packages, filter by availability
        packages_to_test = filter_available_packages(os, args.packages)

        if not packages_to_test:
            print(f"  Skipping {os}: no specified packages available")
            return True

    # Split into batches
    batches = split_into_batches(args.batch_m, packages_to_test)

    # Run tests for each batch
    total_packages_tested = 0
    for batch_num, batch_packages in enumerate(batches, 1):
        solver_opt = f"--solver={args.solver}" if args.solver else None
        if run_install_test(os, batch_packages, solver_opt):
            total_packages_tested += len(batch_packages)
            # Show total packages tested, updating on same line
            print(f"\r  Batch {total_packages_tested} packages", end='', flush=True)
        else:
            print()  # New line after failure
            print(f"  ERROR: Batch {batch_num} failed for {os}")
            return False

    print()  # New line after all batches complete
    print(f"  Completed {os}: {len(batches)} batches tested")
    return True


def main():
    """Main function."""
    args = parse_args(sys.argv[1:])

    if args.help:
        usage()
        sys.exit(0)

    # Handle debug mode
    if args.debug:
        if not args.packages:
            print("Error: --debug requires PACKAGE argument", file=sys.stderr)
            sys.exit(1)
        if not args.selected_oses or len(args.selected_oses) != 1:
            print("Error: --debug requires single --os argument", file=sys.stderr)
            sys.exit(1)
        os_name = args.selected_oses[0]
        pkg_to_install = args.packages[0]
        pkg_no_candidate = args.grep
        debug_solve(os_name, pkg_to_install, args.grep)
        return

    if args.help:
        usage()
        sys.exit(0)

    print("Configuration:")
    print(f"  OSes: {', '.join(args.selected_oses)}")
    print(f"  Batch size: {args.batch_m}x{args.batch_n}")
    print(f"  Random seed: {args.seed}")
    if args.use_random_packages:
        print("  Mode: Random package selection")
    else:
        print(f"  Mode: Specified packages: {' '.join(args.packages)}")
    if args.solver:
        print(f"  Solver: --solver={args.solver}")
    print()

    # Test each OS
    for os in args.selected_oses:
        if not test_os(os, args):
            print(f"ERROR: Testing failed for {os}", file=sys.stderr)
            sys.exit(1)
        print()

    print("All tests completed successfully")


if __name__ == '__main__':
    main()

