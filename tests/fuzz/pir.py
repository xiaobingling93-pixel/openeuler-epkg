#!/usr/bin/env python3
"""
PIR (Package Install/Restore) Fuzz Test

Long-running 7x24 fuzz test for epkg install/remove/restore cycles.
Designed to run in a dedicated user account with optimized filesystem layout.

Usage:
    CACHE_DIR=/path/to/large/disk python3 pir.py setup
    CACHE_DIR=/path/to/large/disk python3 pir.py run --os=openeuler
    CACHE_DIR=/path/to/large/disk python3 pir.py --os=openeuler  # setup + run
"""

import argparse
import os
import platform
import random
import subprocess
import sys
import time
from datetime import datetime
from pathlib import Path
from typing import Optional

# Default configuration
DEFAULT_BATCH_SIZE = 10
DEFAULT_MAX_ERRORS = 100
TMPFS_SIZE_GB = 4  # Target tmpfs size, or use < half system memory

# Environment variables
EPKG_BIN = os.environ.get('EPKG_BIN', None)
CACHE_DIR = os.environ.get('CACHE_DIR', None)
BAD_CASES_DIR = None  # Set after CACHE_DIR is validated

# Platform detection
IS_MACOS = platform.system() == 'Darwin'
IS_LINUX = platform.system() == 'Linux'
HOME = Path.home()
USER = os.environ.get('USER', 'unknown')


def log(msg: str):
    """Log message with timestamp."""
    timestamp = datetime.now().strftime('%Y-%m-%d %H:%M:%S')
    print(f"[{timestamp}] {msg}", flush=True)


def get_system_memory_gb() -> float:
    """Get system total memory in GB."""
    if IS_MACOS:
        result = subprocess.run(['sysctl', '-n', 'hw.memsize'], capture_output=True)
        return int(result.stdout.strip()) / (1024 ** 3)
    elif IS_LINUX:
        with open('/proc/meminfo') as f:
            for line in f:
                if line.startswith('MemTotal'):
                    return int(line.split()[1]) / (1024 ** 2)
    return 16  # Default fallback


def get_tmpfs_target_size_gb() -> int:
    """Calculate tmpfs target size: 4GB or < half system memory."""
    mem_gb = get_system_memory_gb()
    half_mem = int(mem_gb * 0.45)  # Slightly less than half
    return min(TMPFS_SIZE_GB, half_mem)


def get_tmpfs_path() -> Path:
    """Get tmpfs base path for epkg."""
    return Path(f"/tmp/epkg-{USER}")


def get_epkg_symlink_path() -> Path:
    """Get .epkg symlink path."""
    return HOME / ".epkg"


def get_cache_symlink_path() -> Path:
    """Get cache symlink path based on platform."""
    if IS_MACOS:
        return HOME / "Library" / "Caches" / "epkg"
    else:
        return HOME / ".cache" / "epkg"


def is_path_mounted(path: Path) -> bool:
    """Check if a path is a mount point."""
    result = subprocess.run(['mount'], capture_output=True, text=True)
    for line in result.stdout.splitlines():
        if str(path) in line:
            return True
    return False


def setup_macos_ramdisk(tmpfs_path: Path):
    """Create and mount RamDisk on macOS."""
    if tmpfs_path.exists() and is_path_mounted(tmpfs_path):
        log(f"RamDisk already mounted at {tmpfs_path}")
        return True

    # Calculate number of 512-byte blocks: GB * 2097152
    size_gb = get_tmpfs_target_size_gb()
    num_blocks = size_gb * 2097152

    log(f"Creating macOS RamDisk: {size_gb}GB ({num_blocks} blocks)")

    # Create RAM disk device
    result = subprocess.run(
        ['hdid', '-nomount', f'ram://{num_blocks}'],
        capture_output=True, text=True
    )
    if result.returncode != 0:
        log(f"ERROR: hdid failed: {result.stderr}")
        return False

    device = result.stdout.strip().split()[0]
    log(f"RAM device: {device}")

    # Format as HFS+
    result = subprocess.run(
        ['newfs_hfs', '-v', f'epkg-{USER}', device],
        capture_output=True, text=True
    )
    if result.returncode != 0:
        log(f"ERROR: newfs_hfs failed: {result.stderr}")
        subprocess.run(['hdiutil', 'detach', device], capture_output=True)
        return False

    # Create mount point and mount
    tmpfs_path.mkdir(parents=True, exist_ok=True)

    result = subprocess.run(
        ['mount', '-t', 'hfs', device, str(tmpfs_path)],
        capture_output=True, text=True
    )
    if result.returncode != 0:
        log(f"ERROR: mount failed: {result.stderr}")
        subprocess.run(['hdiutil', 'detach', device], capture_output=True)
        return False

    log(f"RamDisk mounted at {tmpfs_path}")
    return True


def cmd_setup(dry_run: bool = False) -> bool:
    """
    Setup filesystem layout for fuzz testing.

    Layout:
    - $HOME/.epkg -> /tmp/epkg-$USER (tmpfs)
    - $HOME/.cache/epkg or Library/Caches/epkg -> $CACHE_DIR
    """
    tmpfs_path = get_tmpfs_path()
    epkg_link = get_epkg_symlink_path()
    cache_link = get_cache_symlink_path()

    if not CACHE_DIR:
        log("ERROR: CACHE_DIR environment variable is required")
        return False

    cache_dir = Path(CACHE_DIR)
    if not cache_dir.exists():
        log(f"Creating CACHE_DIR: {cache_dir}")
        if not dry_run:
            cache_dir.mkdir(parents=True, exist_ok=True)

    global BAD_CASES_DIR
    BAD_CASES_DIR = cache_dir / "bad-cases"
    if not BAD_CASES_DIR.exists() and not dry_run:
        BAD_CASES_DIR.mkdir(parents=True, exist_ok=True)

    log(f"Layout configuration:")
    log(f"  TMPFS path: {tmpfs_path}")
    log(f"  .epkg link: {epkg_link} -> {tmpfs_path}")
    log(f"  cache link: {cache_link} -> {cache_dir}")
    log(f"  bad-cases:  {BAD_CASES_DIR}")

    if dry_run:
        return True

    # Setup tmpfs
    if IS_MACOS:
        setup_macos_ramdisk(tmpfs_path)
    elif IS_LINUX:
        tmpfs_path.mkdir(parents=True, exist_ok=True)

    # Setup .epkg symlink
    if epkg_link.exists():
        if epkg_link.is_symlink():
            # On macOS, /tmp -> /private/tmp, so compare resolved paths
            current_target = epkg_link.resolve()
            expected_target = tmpfs_path.resolve()
            if current_target != expected_target:
                log(f"WARNING: .epkg links to {current_target}, expected {expected_target}")
        else:
            log(f"ERROR: .epkg exists but is not a symlink")
            return False
    else:
        log(f"Creating symlink: {epkg_link} -> {tmpfs_path}")
        epkg_link.symlink_to(tmpfs_path)

    # Setup cache symlink
    if cache_link.exists():
        if cache_link.is_symlink():
            # Compare resolved paths for consistency
            current_target = cache_link.resolve()
            expected_target = cache_dir.resolve()
            if current_target != expected_target:
                log(f"WARNING: cache links to {current_target}, expected {expected_target}")
        else:
            log(f"ERROR: cache exists but is not a symlink")
            return False
    else:
        log(f"Creating symlink: {cache_link} -> {cache_dir}")
        cache_link.parent.mkdir(parents=True, exist_ok=True)
        cache_link.symlink_to(cache_dir)

    # Ensure epkg binary is available (run self install if needed)
    try:
        epkg_bin = find_epkg_binary()
        log(f"epkg binary: {epkg_bin}")
    except RuntimeError as e:
        log(f"ERROR: {e}")
        return False

    return True


def find_epkg_binary() -> Path:
    """Find epkg binary path, run self install if needed."""
    candidates = []

    if EPKG_BIN:
        candidates.append(Path(EPKG_BIN))

    # Find relative to script location (tests/fuzz/pir.py -> project_root)
    script_dir = Path(__file__).parent
    project_root = script_dir.parent.parent

    self_epkg = HOME / ".epkg" / "envs" / "self" / "usr" / "bin" / "epkg"
    self_assets = HOME / ".epkg" / "envs" / "self" / "usr" / "src" / "epkg" / "assets" / "repos"
    debug_epkg = project_root / "target" / "debug" / "epkg"
    release_epkg = project_root / "target" / "release" / "epkg"

    # Check if self env exists and is complete (has assets/repos)
    if self_epkg.exists() and self_assets.exists():
        return self_epkg.resolve()

    candidates.extend([debug_epkg, release_epkg])

    # Find a built binary
    built_epkg = None
    for path in candidates:
        if path.exists() and path.is_file():
            built_epkg = path
            break

    if built_epkg:
        log(f"Running epkg self install (self env not found or incomplete)")
        result = subprocess.run(
            [str(built_epkg), 'self', 'install'],
            capture_output=True, text=True
        )
        if result.returncode != 0:
            log(f"ERROR: epkg self install failed: {result.stderr}")
            raise RuntimeError("epkg self install failed")
        if self_epkg.exists():
            return self_epkg.resolve()
        return built_epkg.resolve()

    raise RuntimeError("epkg binary not found. Set EPKG_BIN or build the project.")


def run_epkg(args: list, env_name: str, capture_output: bool = True) -> subprocess.CompletedProcess:
    """
    Run epkg command with logging.

    Environment variables:
    - RUST_LOG=warn
    - RUST_BACKTRACE=1
    """
    epkg_bin = find_epkg_binary()
    cmd = [str(epkg_bin), '-e', env_name] + args

    env = os.environ.copy()
    env['RUST_LOG'] = 'warn'
    env['RUST_BACKTRACE'] = '1'

    log(f"Running: {' '.join(cmd)}")

    if capture_output:
        return subprocess.run(cmd, capture_output=True, text=True, env=env)
    else:
        return subprocess.run(cmd, env=env)


def get_available_packages(os_name: str, env_name: str) -> list:
    """Get list of available packages for the OS."""
    log(f"Getting available packages for {os_name}")

    result = run_epkg(['list', '--available'], env_name)
    if result.returncode != 0:
        log(f"ERROR: Failed to list packages: {result.stderr}")
        return []

    packages = []
    for line in result.stdout.splitlines():
        parts = line.split()
        if len(parts) >= 5 and parts[0].startswith(('A', '_')):
            packages.append(parts[4])

    log(f"Found {len(packages)} available packages")
    return packages


def get_installed_executables(env_name: str) -> list:
    """Get list of installed executables in the environment."""
    env_path = get_epkg_symlink_path() / "envs" / env_name
    bin_path = env_path / "usr" / "bin"

    if not bin_path.exists():
        return []

    executables = []
    for exe in bin_path.iterdir():
        if exe.is_file() and not exe.is_symlink():
            executables.append(f"/usr/bin/{exe.name}")

    return executables


def test_executable_help(env_name: str, exe_path: str) -> tuple[bool, str]:
    """Test if executable can run with --help or --version."""
    result = run_epkg(['run', '--', exe_path, '--help'], env_name)

    if result.returncode == 0:
        return True, result.stdout + result.stderr

    output = result.stdout + result.stderr
    if any(keyword in output for keyword in ['Usage', '--help', 'Options']):
        return True, output

    if any(keyword in output for keyword in [
        'error while loading shared libraries',
        'cannot open shared object file',
        'No such file or directory'
    ]):
        return False, output

    # Try --version as fallback
    result = run_epkg(['run', '--', exe_path, '--version'], env_name)
    output = result.stdout + result.stderr

    if result.returncode == 0 or any(keyword in output for keyword in ['version', 'Version', 'Copyright']):
        return True, output

    if any(keyword in output for keyword in [
        'error while loading shared libraries',
        'cannot open shared object file',
        'No such file or directory'
    ]):
        return False, output

    return True, output


def check_log_for_errors(log_content: str) -> list:
    """Check log content for error patterns."""
    errors = []
    for line in log_content.splitlines():
        if 'ERROR' in line:
            errors.append(f"ERROR: {line}")

        if 'WARN' in line and any(keyword in line for keyword in [
            'failed', 'error', 'cannot', 'missing', 'broken'
        ]):
            errors.append(f"WARN: {line}")

        if 'panic' in line.lower() or 'thread panicked' in line.lower():
            errors.append(f"PANIC: {line}")

    return errors


def get_tmpfs_usage_percent() -> float:
    """Get current tmpfs usage percentage."""
    tmpfs_path = get_tmpfs_path()

    result = subprocess.run(['df', '-k', str(tmpfs_path)], capture_output=True, text=True)
    for line in result.stdout.splitlines():
        if str(tmpfs_path) in line or 'epkg' in line:
            parts = line.split()
            if len(parts) >= 5:
                used = int(parts[2])
                total = int(parts[1])
                if total > 0:
                    return (used / total) * 100

    return 0.0


def load_whitelist() -> list:
    """Load error whitelist from tests/test_depends-whitelist.txt."""
    script_dir = Path(__file__).parent
    project_root = script_dir.parent.parent
    whitelist_file = project_root / "tests" / "test_depends-whitelist.txt"

    patterns = []
    if whitelist_file.exists():
        with open(whitelist_file) as f:
            for line in f:
                line = line.strip()
                # Skip comments and empty lines
                if line and not line.startswith('#'):
                    patterns.append(line)

    log(f"Loaded {len(patterns)} whitelist patterns")
    return patterns


def error_matches_whitelist(error_msg: str, whitelist: list) -> bool:
    """Check if error message matches any whitelist pattern."""
    for pattern in whitelist:
        if pattern in error_msg:
            return True
    return False


def save_bad_case(os_name: str, commands: list, log_content: str, error_type: str,
                   error_msg: str = "", is_depends_error: bool = False):
    """Save bad case artifacts for later analysis."""
    if not BAD_CASES_DIR:
        log("ERROR: BAD_CASES_DIR not set")
        return

    timestamp = datetime.now().strftime('%Y%m%d_%H%M%S')
    case_dir = BAD_CASES_DIR / f"{timestamp}_{os_name}_{error_type}"
    case_dir.mkdir(parents=True, exist_ok=True)

    env_name = f"fuzz-{os_name}"
    epkg_bin = find_epkg_binary()

    # Write reproduce.sh (no set -e, follow tests/README.md principle)
    reproduce_sh = case_dir / "reproduce.sh"
    with open(reproduce_sh, 'w') as f:
        f.write("#!/bin/sh\n")
        f.write(f"# Generated by {__file__}\n")
        f.write("#\n")
        f.write("# ============================================================\n")
        f.write("# Problem Analysis Principles (问题分析原则):\n")
        f.write("#   1. 分析问题的性质 (nature) - 是代码bug、配置问题、还是repo问题?\n")
        f.write("#   2. 分析问题的本质 (essence) - 根本原因是什么?\n")
        f.write("#   3. 确定妥善处理方法 (proper solution):\n")
        f.write("#      - 代码bug: 修复代码，不要workaround\n")
        f.write("#      - repo问题: 添加whitelist并记录原因\n")
        f.write("#      - 配置问题: 调整配置或文档\n")
        f.write("#\n")
        f.write("# Key principles (关键原则):\n")
        f.write("#   - \"do proper fix\" - 正确修复，不要临时方案\n")
        f.write("#   - \"no fix for sake of fix\" - 不要为了修而修\n")
        f.write("#   - 反虚假声明: 不确定的地方说不确定，没验证就不要暗示通过了\n")
        f.write("# ============================================================\n")
        f.write("#\n")
        f.write("# AI troubleshooting guide:\n")
        f.write("#   1. Turn on MORE debug log: RUST_LOG=trace\n")
        f.write("#   2. Refer to below commands to reproduce the error\n")
        f.write("#   3. Redirect log output to file, then analyze relevant lines like grep -B3 -A3 ...\n")
        if is_depends_error:
            f.write("#\n")
            f.write("# Whitelist handling:\n")
            f.write("#   If this is a repo dependency issue (not epkg bug), add pattern to\n")
            f.write("#   tests/test_depends-whitelist.txt with a comment explaining:\n")
            f.write("#     - Package name and version\n")
            f.write("#     - Why it's unresolvable (e.g., broken deps in upstream repo)\n")
        f.write("#\n")
        f.write(f"# OS: {os_name}\n")
        f.write(f"# Error type: {error_type}\n")
        if error_msg:
            # Truncate long error messages
            error_preview = error_msg[:500] if len(error_msg) > 500 else error_msg
            f.write(f"# Error preview: {error_preview}\n")
        f.write(f"# Generated: {timestamp}\n\n")
        f.write("set -x\n\n")
        f.write(f"EPKG_BIN='{epkg_bin}'\n")
        f.write(f"ENV_NAME='{env_name}'\n\n")
        f.write(f"$EPKG_BIN env remove $ENV_NAME 2>/dev/null  # ignore error: may fail on first run\n")
        f.write(f"$EPKG_BIN env create $ENV_NAME -c {os_name}\n\n")
        for cmd in commands:
            f.write(f"{cmd.replace('epkg', '$EPKG_BIN')}\n")

    reproduce_sh.chmod(0o755)

    # Write epkg.log
    epkg_log = case_dir / "epkg.log"
    with open(epkg_log, 'w') as f:
        f.write(log_content)

    log(f"Saved bad case to: {case_dir}")
    log(f"  reproduce.sh: {len(commands)} commands")
    log(f"  epkg.log: {len(log_content)} bytes")


def create_environment(os_name: str) -> str:
    """Create fuzz test environment for the OS."""
    env_name = f"fuzz-{os_name}"
    epkg_bin = find_epkg_binary()
    env_path = get_epkg_symlink_path() / "envs" / env_name

    env_vars = os.environ.copy()
    env_vars['RUST_LOG'] = 'warn'
    env_vars['RUST_BACKTRACE'] = '1'

    # Remove existing env only if it exists
    if env_path.exists():
        log(f"Removing existing environment: {env_name}")
        result = subprocess.run(
            [str(epkg_bin), '-e', 'self', 'env', 'remove', env_name],
            capture_output=True, text=True, env=env_vars
        )
        if result.returncode != 0:
            log(f"env remove failed: {result.stderr[:200]}")

    log(f"Creating environment: {env_name}")
    result = subprocess.run(
        [str(epkg_bin), '-e', 'self', 'env', 'create', env_name, '-c', os_name],
        capture_output=True, text=True, env=env_vars
    )

    if result.returncode != 0:
        raise RuntimeError(f"Failed to create environment: {result.stderr}")

    return env_name


def run_fuzz_iteration(os_name: str, env_name: str, packages: list,
                        batch_size: int, whitelist: list) -> tuple[str, bool]:
    """
    Run single fuzz iteration: install, test executables, restore.

    Returns:
        tuple: (error_type or None, has_error)
    """
    loop_commands = []
    loop_log = ""

    # Check disk space before install
    usage = get_tmpfs_usage_percent()
    if usage > 90:
        log(f"Disk usage {usage:.1f}% > 90%, skipping iteration to avoid space issues")
        # Run gc and return without error
        result = run_epkg(['gc'], env_name='self')
        log(f"GC output: {result.stdout[:200]}")
        time.sleep(1)
        return None, False

    # Select random packages
    batch = random.sample(packages, min(batch_size, len(packages)))
    batch_str = ' '.join(batch)
    log(f"Selected packages: {batch_str}")

    # Install packages
    cmd_str = f"epkg -e {env_name} install --assume-yes --ignore-file-conflicts {batch_str}"
    loop_commands.append(cmd_str)

    result = run_epkg(['install', '--assume-yes', '--ignore-file-conflicts'] + batch, env_name)
    loop_log += f"=== INSTALL ===\n{result.stdout}\n{result.stderr}\n"

    install_error = result.returncode != 0
    log_errors = check_log_for_errors(result.stdout + result.stderr)

    # Check for disk space error - should exit gracefully
    if "Insufficient disk space" in result.stderr or "Insufficient disk space" in result.stdout:
        log(f"Insufficient disk space, running gc and exiting gracefully")
        result = run_epkg(['gc'], env_name='self')
        log(f"GC output: {result.stdout[:200]}")
        return None, False

    # Check for dependency resolution errors matching whitelist
    # These are repo issues, not epkg bugs - skip saving as bad case
    combined_output = result.stdout + result.stderr
    if "Dependency resolution failed" in combined_output:
        if error_matches_whitelist(combined_output, whitelist):
            log(f"Dependency resolution error matches whitelist - skipping (repo issue)")
            return None, False
        # Also check individual package names against whitelist
        # Example: "No candidates were found for alpine-base alpine-base"
        for pattern in whitelist:
            if pattern in combined_output:
                log(f"Dependency error matches whitelist pattern '{pattern}' - skipping (repo issue)")
                return None, False

    if install_error or log_errors:
        error_type = "install_fail" if install_error else "install_warn"
        error_msg = result.stderr if install_error else str(log_errors[:3])
        # Check if this is a dependency resolution error
        is_depends_error = "Dependency resolution failed" in combined_output
        save_bad_case(os_name, loop_commands, loop_log, error_type, error_msg, is_depends_error)
        log(f"Install error detected")
        # Skip restore when install failed - generation may not have advanced
        return error_type, True

    # Test executables
    executables = get_installed_executables(env_name)
    log(f"Testing {len(executables)} executables")

    exe_errors = []
    failed_outputs = []  # Only record failed outputs to reduce log size
    for exe in executables:
        cmd_str = f"epkg -e {env_name} run {exe} --help"
        loop_commands.append(cmd_str)

        success, output = test_executable_help(env_name, exe)

        if not success:
            exe_errors.append(exe)
            failed_outputs.append(f"=== RUN {exe} (FAILED) ===\n{output}\n")
            log(f"Executable test failed: {exe}")

    if exe_errors:
        # Only include failed outputs in log to reduce size
        loop_log += "".join(failed_outputs)
        save_bad_case(os_name, loop_commands, loop_log, "exe_fail", f"Failed executables: {exe_errors}")
        log(f"Executable errors detected")
        return "exe_fail", True

    # Restore to generation 1
    log("Restoring to generation 1")
    cmd_str = f"epkg -e {env_name} restore 1"
    loop_commands.append(cmd_str)

    result = run_epkg(['restore', '1'], env_name)
    loop_log += f"=== RESTORE ===\n{result.stdout}\n{result.stderr}\n"

    if result.returncode != 0:
        save_bad_case(os_name, loop_commands, loop_log, "restore_fail", result.stderr)
        log(f"Restore error detected")
        return "restore_fail", True

    # Check tmpfs usage
    usage = get_tmpfs_usage_percent()
    log(f"TMPFS usage: {usage:.1f}%")

    if usage > 80:
        log("Running epkg gc to free memory")
        result = run_epkg(['gc'], env_name='self')
        log(f"GC output: {result.stdout[:100]}")

    time.sleep(1)
    return None, False


def get_cache_dir_from_symlink() -> Optional[Path]:
    """Get CACHE_DIR from existing cache symlink."""
    cache_link = get_cache_symlink_path()
    if cache_link.exists() and cache_link.is_symlink():
        return cache_link.resolve()
    return None


def cmd_run(os_name: str, batch_size: int, max_errors: int):
    """
    Main fuzz test loop.

    Loop:
    - Install random packages
    - Test executables
    - Restore to generation 1
    - Check tmpfs usage, gc if needed
    - Save bad cases on errors
    """
    # Get CACHE_DIR from env or existing symlink
    cache_dir = None
    if CACHE_DIR:
        cache_dir = Path(CACHE_DIR)
    else:
        cache_dir = get_cache_dir_from_symlink()

    if not cache_dir:
        log("ERROR: CACHE_DIR not set and cache symlink not found")
        log("       Run 'pir.py setup' first or set CACHE_DIR environment variable")
        return

    global BAD_CASES_DIR
    BAD_CASES_DIR = cache_dir / "bad-cases"
    if not BAD_CASES_DIR.exists():
        BAD_CASES_DIR.mkdir(parents=True, exist_ok=True)

    # Load whitelist for dependency resolution errors
    whitelist = load_whitelist()

    env_name = create_environment(os_name)
    packages = get_available_packages(os_name, env_name)

    if not packages:
        log("ERROR: No packages available")
        return

    error_count = 0
    loop_count = 0

    log(f"Starting fuzz loop: batch_size={batch_size}, max_errors={max_errors}")

    while error_count < max_errors:
        loop_count += 1
        log(f"=== Loop {loop_count} ===")

        error_type, has_error = run_fuzz_iteration(
            os_name, env_name, packages, batch_size, whitelist
        )
        if has_error:
            error_count += 1
            log(f"Error count: {error_count}/{max_errors}")

    log(f"Fuzz test completed: {loop_count} loops, {error_count} errors")


def main():
    parser = argparse.ArgumentParser(
        description='PIR (Package Install/Restore) Fuzz Test for epkg',
        usage='python3 pir.py [setup|run] [--os OS] [--batch N] [--max-err N]'
    )
    subparsers = parser.add_subparsers(dest='command', help='Commands')

    # setup subcommand
    setup_parser = subparsers.add_parser('setup', help='Setup filesystem layout only')
    setup_parser.add_argument('--dry-run', action='store_true', help='Show layout without creating')

    # run subcommand
    run_parser = subparsers.add_parser('run', help='Run fuzz test loop (assumes layout already setup)')
    run_parser.add_argument('--os', required=True, help='Target OS')
    run_parser.add_argument('--batch', type=int, default=DEFAULT_BATCH_SIZE, help='Batch size')
    run_parser.add_argument('--max-err', type=int, default=DEFAULT_MAX_ERRORS, help='Max errors')

    # Default (no subcommand): setup + run
    parser.add_argument('--os', help='Target OS (for default mode: setup + run)')
    parser.add_argument('--batch', type=int, default=DEFAULT_BATCH_SIZE, help='Batch size')
    parser.add_argument('--max-err', type=int, default=DEFAULT_MAX_ERRORS, help='Max errors')
    parser.add_argument('--dry-run', action='store_true', help='Dry run (setup only)')

    args = parser.parse_args()

    log("PIR (Package Install/Restore) Fuzz Test")
    log(f"Platform: {platform.system()}")

    if args.command == 'setup':
        log("Mode: setup only")
        if not cmd_setup(args.dry_run):
            sys.exit(1)
        log("Setup complete")

    elif args.command == 'run':
        log(f"Mode: run only (assumes layout already setup)")
        log(f"OS target: {args.os}")
        log(f"Batch size: {args.batch}")
        log(f"Max errors: {args.max_err}")
        try:
            cmd_run(args.os, args.batch, args.max_err)
        except KeyboardInterrupt:
            log("Interrupted by user")
        except Exception as e:
            log(f"ERROR: {e}")
            sys.exit(1)

    else:
        # Default mode: setup + run
        if not args.os:
            parser.error("--os is required for default mode")

        log(f"Mode: setup + run")
        log(f"OS target: {args.os}")
        log(f"Batch size: {args.batch}")
        log(f"Max errors: {args.max_err}")

        if not cmd_setup(args.dry_run):
            sys.exit(1)

        if args.dry_run:
            log("Dry run complete")
            sys.exit(0)

        try:
            cmd_run(args.os, args.batch, args.max_err)
        except KeyboardInterrupt:
            log("Interrupted by user")
        except Exception as e:
            log(f"ERROR: {e}")
            sys.exit(1)


if __name__ == '__main__':
    main()
