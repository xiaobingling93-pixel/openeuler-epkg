#!/usr/bin/env python3
import json
import os
import subprocess
import time
import sys
from urllib.parse import urlparse

# Import common utilities
from common import debug_print

# Define paths
BASE_DIR = os.path.dirname(os.path.abspath(__file__))
LS_MIRRORS_INPUT_PATH = os.path.join(BASE_DIR, 'ls-mirrors.json')
OFFICIAL_MIRRORS_INPUT_PATH = os.path.join(BASE_DIR, 'official-mirrors.json')
PROBE_MIRRORS_OUTPUT_PATH = os.path.join(BASE_DIR, 'probe-mirrors.json')
NOREACH_MIRRORS_OUTPUT_PATH = os.path.join(BASE_DIR, 'noreach-mirrors.txt')
NOCONTENT_MIRRORS_OUTPUT_PATH = os.path.join(BASE_DIR, 'nocontent-mirrors.txt')

# List of possible distribution directories to probe, ordered by frequency
POSSIBLE_DISTRO_DIRS = [
    "ubuntu", "debian", "archlinux", "rocky", "fedora", "centos",
    "almalinux", "epel", "debian-security", "opensuse", "raspbian",
    "linuxmint", "centos-stream", "alpine", "manjaro", "fedora-epel",
    "ubuntu-ports", "rpmfusion", "deepin", "centos-vault",
    "debian-multimedia", "mxlinux", "CentOS", "openeuler", "arch",
    "mint", "endeavouros", "ubuntu-archive", "archlinuxarm",
    "fedora-secondary", "rocky-linux", "archlinuxcn", "armbian",
    "deb-multimedia", "raspberrypi", "openSUSE", "anaconda", "conda",
    "artixlinux", "alpinelinux", "cachyos", "msys2",
]

def load_official_mirrors_data():
    """Load official-mirrors.json data."""
    if os.path.exists(OFFICIAL_MIRRORS_INPUT_PATH):
        try:
            with open(OFFICIAL_MIRRORS_INPUT_PATH, 'r') as f:
                return json.load(f)
        except Exception as e:
            print(f"Error loading official-mirrors.json: {e}")
            return {}
    return {}

def load_ls_mirrors_data():
    """Load ls-mirrors.json data."""
    if os.path.exists(LS_MIRRORS_INPUT_PATH):
        try:
            with open(LS_MIRRORS_INPUT_PATH, 'r') as f:
                return json.load(f)
        except Exception as e:
            print(f"Error loading ls-mirrors.json: {e}")
            return {}
    return {}

def run_lftp_cls(mirror_url, distro_dir, timeout=4):
    """Run lftp command to list directory contents using 'cls' command with && style."""
    try:
        # Set comprehensive lftp timeout options and then run the command
        # net:timeout - connection and data transfer timeout
        # net:max-retries - disable retries for faster failure detection
        # net:reconnect-interval-base/multiplier - disable reconnection delays
        lftp_cmd = [
            'lftp', '-c',
            f'set net:timeout {timeout}; '
            f'set net:max-retries 2; '
            f'set net:reconnect-interval-base 0; '
            f'set net:reconnect-interval-multiplier 0; '
            f'set ssl:verify-certificate no; '
            f'open {mirror_url}/{distro_dir} && cls'
        ]
        debug_print(f"Running lftp command: {' '.join(lftp_cmd)}")

        result = subprocess.run(
            lftp_cmd,
            capture_output=True,
            text=True,
        )

        debug_print(f"LFTP stdout/stderr for {mirror_url}/{distro_dir}: {result.returncode}")
        debug_print("=" * 60)
        debug_print(result.stdout.strip())
        debug_print("-" * 60)
        debug_print(result.stderr.strip())
        debug_print("=" * 60)

        if result.returncode == 0:
            return result.stdout.strip()
        else:
            return result.stderr.strip()  # Return stderr for error analysis

    except subprocess.TimeoutExpired:
        debug_print(f"lftp timeout for {mirror_url}/{distro_dir} (timeout: {timeout}s)")
        return None
    except Exception as e:
        debug_print(f"lftp error for {mirror_url}/{distro_dir}: {e}")
        return None

def parse_lftp_output(output):
    """Parse lftp output to count directories and files from 'cls' command."""
    if not output:
        return 0, 0, False, False  # dir_count, file_count, is_404_error, is_connection_error

    lines = output.strip().split('\n')
    dir_count = 0
    file_count = 0
    is_404_error = False
    is_connection_error = False

    for line in lines:
        line = line.strip()
        if not line:
            continue

        # Check for connection problems and fatal errors
        if any(keyword in line.lower() for keyword in [
            'fatal error', 'max-retries exceeded', 'connection failed',
            'connection refused', 'timeout', 'network unreachable',
            'host unreachable', 'no route to host'
        ]):
            is_connection_error = True
            continue

        # Check for 404 error - strong indication of non-existent directory
        if '404' in line and ('Not Found' in line or 'Access failed' in line):
            is_404_error = True
            continue

        # Skip lines that start with terminal control sequences or are not file/dir entries
        if line.startswith('\x1b') or line.startswith(']0;') or line.startswith('----'):
            continue

        # Skip error messages and connection info
        if any(keyword in line.lower() for keyword in ['access failed', 'connecting to', 'cd:', 'error']):
            continue

        # Handle 'cls' output format which shows simple file/directory names
        # Directories end with '/', files don't
        if line.endswith('/'):
            dir_count += 1
            debug_print(f"  Found directory: {line}")
        else:
            # Check if it's a file (not a directory and not empty)
            if line and not line.startswith('total '):
                file_count += 1
                debug_print(f"  Found file: {line}")

    print(f"Parse results: {dir_count} directories, {file_count} files, 404_error: {is_404_error}, connection_error: {is_connection_error}")
    return dir_count, file_count, is_404_error, is_connection_error

def probe_mirror_url(mirror_url, timeout=5):
    """Probe a mirror URL to find distribution directories."""
    probe_dirs = []
    site_connection_error = False
    site_reachable = False  # Track if we got any response from the site

    for distro_dir in POSSIBLE_DISTRO_DIRS:
        debug_print(f"  Checking {distro_dir}...")

        output = run_lftp_cls(mirror_url, distro_dir, timeout)
        if output:
            print(f"cls {distro_dir}\n{output}")
            dir_count, file_count, is_404_error, is_connection_error = parse_lftp_output(output)

            # Check if site is reachable (got any response including 404, redirections, etc.)
            if any(keyword in output.lower() for keyword in [
                'access failed:', '404 not found', '403 forbidden',
            ]):
                site_reachable = True
                debug_print(f"    Site is reachable - got response from {distro_dir}")

            if is_connection_error:
                debug_print(f"    *** CONNECTION ERROR for {distro_dir} - site has connection problems ***")
                site_connection_error = True
                break  # Stop probing this site - connection problems are site-wide

            if is_404_error:
                debug_print(f"    *** 404 ERROR for {distro_dir} - directory does not exist ***")
                continue  # Skip this directory as it doesn't exist

            debug_print(f"    Found {dir_count} directories and {file_count} files in {distro_dir}")

            if dir_count >= 1:
                probe_dirs.append(distro_dir)
                print(f"    ✓ Added {distro_dir} (found {dir_count} dirs and {file_count} files)")
            else:
                debug_print(f"    No directories found in {distro_dir}")

        # Small delay to be respectful to servers
        time.sleep(0.1)

    return probe_dirs, (site_connection_error and not site_reachable)

def main():
    """Main function to probe mirrors without 'ls' field."""
    print("Loading ls-mirrors.json...")
    ls_data = load_ls_mirrors_data()
    official_mirrors = load_official_mirrors_data()

    if not ls_data:
        print("Error: Could not load ls-mirrors.json")
        sys.exit(1)

    if not official_mirrors:
        print("Error: Could not load official-mirrors.json")
        sys.exit(1)

    # Find mirrors without 'ls' field
    mirrors_to_probe = []
    for mirror_url, mirror_info in ls_data.items():
        if not mirror_info.get('ls'):
            new_info = official_mirrors.get(mirror_url, {})
            if new_info and (not new_info.get('top_level') and not new_info.get('cache_dirs')):
                mirrors_to_probe.append(mirror_url)
                #  if len(mirrors_to_probe) > 3: # for quick debug run
                #      break

    print(f"Found {len(mirrors_to_probe)} mirrors without 'ls'/'top_level' field to probe")

    probe_results = {}
    blacklist_connection_errors = []
    blacklist_no_dirs = []

    for i, mirror_url in enumerate(mirrors_to_probe, 1):
        print(f"\n[{i}/{len(mirrors_to_probe)}] Processing: {mirror_url}")

        probe_dirs, site_connection_error = probe_mirror_url(mirror_url, timeout=4)  # Increased timeout for better reliability

        if probe_dirs:
            probe_results[mirror_url] = {'probe_dirs': probe_dirs}
            print(f"  ✓ Found {len(probe_dirs)} distribution directories")
        elif site_connection_error:
            blacklist_connection_errors.append(mirror_url)
            print(f"  ✗ Site has connection problems - adding to blacklist")
        else:
            blacklist_no_dirs.append(mirror_url)
            print(f"  ✗ No distribution directories found - adding to blacklist")

    # Save probe results
    print(f"\nSaving probe results to {PROBE_MIRRORS_OUTPUT_PATH}")
    with open(PROBE_MIRRORS_OUTPUT_PATH, 'w') as f:
        json.dump(probe_results, f, indent=2)

    # Save blacklist with classified reasons
    if blacklist_connection_errors:
        print(f"Saving connection errors to {NOREACH_MIRRORS_OUTPUT_PATH}")
        with open(NOREACH_MIRRORS_OUTPUT_PATH, 'w') as f:
            for url in blacklist_connection_errors:
                f.write(f"{url}\n")

    if blacklist_no_dirs:
        print(f"Saving no content errors to {NOCONTENT_MIRRORS_OUTPUT_PATH}")
        with open(NOCONTENT_MIRRORS_OUTPUT_PATH, 'w') as f:
            for url in blacklist_no_dirs:
                f.write(f"{url}\n")

    print(f"\nSummary:")
    print(f"  - Probed {len(mirrors_to_probe)} mirrors")
    print(f"  - Found distribution directories in {len(probe_results)} mirrors")
    print(f"  - Added {len(blacklist_connection_errors)} mirrors to blacklist (connection errors)")
    print(f"  - Added {len(blacklist_no_dirs)} mirrors to blacklist (no directories)")
    print(f"  - Results saved to probe-mirrors.json")
    print(f"  - Blacklist saved to noreach-mirrors.txt")
    print(f"  - Blacklist saved to nocontent-mirrors.txt")

if __name__ == "__main__":
    main()
