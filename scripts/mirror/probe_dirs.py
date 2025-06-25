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
PROBE_MIRRORS_OUTPUT_PATH = os.path.join(BASE_DIR, 'probe-mirrors.json')
BLACKLIST_OUTPUT_PATH = os.path.join(BASE_DIR, 'mirror_blacklist.txt')

# List of possible distribution directories to probe, ordered by frequency
POSSIBLE_DISTRO_DIRS = [
    "ubuntu", "debian", "archlinux", "rocky", "fedora", "centos",
    "almalinux", "epel", "debian-security", "opensuse", "raspbian",
    "linuxmint", "centos-stream", "alpine", "manjaro", "fedora-epel",
    "ubuntu-ports", "rpmfusion", "deepin", "centos-vault",
    "debian-multimedia", "mxlinux", "CentOS", "openeuler", "arch",
    "mint", "endeavouros", "ubuntu-archive", "archlinuxarm",
    "fedora-secondary", "rocky-linux", "archlinuxcn", "armbian",
    "deb-multimedia", "raspberrypi", "openSUSE", "anaconda",
    "artixlinux", "alpinelinux"
]

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

def run_lftp_ls(mirror_url, distro_dir, timeout=10):
    """Run lftp command to list directory contents."""
    try:
        lftp_cmd = ['lftp', '-c', f'open {mirror_url}/{distro_dir}; ls']
        debug_print(f"Running lftp command: {' '.join(lftp_cmd)}")

        result = subprocess.run(
            lftp_cmd,
            capture_output=True,
            text=True,
            timeout=timeout
        )

        if result.returncode == 0:
            return result.stdout.strip()
        else:
            debug_print(f"lftp failed for {mirror_url}/{distro_dir}: {result.stderr}")
            return None

    except subprocess.TimeoutExpired:
        debug_print(f"lftp timeout for {mirror_url}/{distro_dir}")
        return None
    except Exception as e:
        debug_print(f"lftp error for {mirror_url}/{distro_dir}: {e}")
        return None

def parse_lftp_output(output):
    """Parse lftp output to count directories."""
    if not output:
        return 0

    lines = output.strip().split('\n')
    dir_count = 0

    for line in lines:
        line = line.strip()
        if line and not line.startswith('total '):
            # Count lines that look like directory entries
            # lftp typically shows permissions, size, date, name
            parts = line.split()
            if len(parts) >= 9:  # Typical lftp output format
                # Check if it's a directory (starts with 'd')
                if parts[0].startswith('d'):
                    dir_count += 1

    return dir_count

def probe_mirror_url(mirror_url):
    """Probe a mirror URL to find distribution directories."""
    probe_dirs = []

    print(f"Probing mirror: {mirror_url}")

    for distro_dir in POSSIBLE_DISTRO_DIRS:
        debug_print(f"  Checking {distro_dir}...")

        output = run_lftp_ls(mirror_url, distro_dir)
        if output:
            dir_count = parse_lftp_output(output)
            debug_print(f"    Found {dir_count} directories in {distro_dir}")

            if dir_count >= 3:
                probe_dirs.append(distro_dir)
                print(f"    ✓ Added {distro_dir} (found {dir_count} dirs)")

        # Small delay to be respectful to servers
        time.sleep(0.1)

    return probe_dirs

def main():
    """Main function to probe mirrors without 'ls' field."""
    print("Loading ls-mirrors.json...")
    ls_data = load_ls_mirrors_data()

    if not ls_data:
        print("Error: Could not load ls-mirrors.json")
        sys.exit(1)

    # Find mirrors without 'ls' field
    mirrors_to_probe = []
    for mirror_url, mirror_info in ls_data.items():
        if not mirror_info.get('ls'):
            mirrors_to_probe.append(mirror_url)

    print(f"Found {len(mirrors_to_probe)} mirrors without 'ls' field to probe")

    probe_results = {}
    blacklist = []

    for i, mirror_url in enumerate(mirrors_to_probe, 1):
        print(f"\n[{i}/{len(mirrors_to_probe)}] Processing: {mirror_url}")

        probe_dirs = probe_mirror_url(mirror_url)

        if probe_dirs:
            probe_results[mirror_url] = {'probe_dirs': probe_dirs}
            print(f"  ✓ Found {len(probe_dirs)} distribution directories")
        else:
            blacklist.append(mirror_url)
            print(f"  ✗ No distribution directories found - adding to blacklist")

    # Save probe results
    print(f"\nSaving probe results to {PROBE_MIRRORS_OUTPUT_PATH}")
    with open(PROBE_MIRRORS_OUTPUT_PATH, 'w') as f:
        json.dump(probe_results, f, indent=2)

    # Save blacklist
    print(f"Saving blacklist to {BLACKLIST_OUTPUT_PATH}")
    with open(BLACKLIST_OUTPUT_PATH, 'w') as f:
        for url in blacklist:
            f.write(f"{url}\n")

    print(f"\nSummary:")
    print(f"  - Probed {len(mirrors_to_probe)} mirrors")
    print(f"  - Found distribution directories in {len(probe_results)} mirrors")
    print(f"  - Added {len(blacklist)} mirrors to blacklist")
    print(f"  - Results saved to probe-mirrors.json")
    print(f"  - Blacklist saved to mirror_blacklist.txt")

if __name__ == "__main__":
    main()
