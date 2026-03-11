#!/usr/bin/env python3
import json
import os
import re
from collections import OrderedDict
from urllib.parse import urlparse
import sys

# Import pycountry for ISO country code detection
import pycountry

# Import common utilities
from common import debug_print, load_distro_configs, get_valid_dirs

# Define paths
BASE_DIR = os.path.dirname(os.path.abspath(__file__))
INPUT_DIR = os.path.join(BASE_DIR, 'input')
OUTPUT_DIR = os.path.join(BASE_DIR, 'output')
OFFICIAL_MIRRORS_INPUT_PATH = os.path.join(OUTPUT_DIR, 'official-mirrors.json')
LS_MIRRORS_INPUT_PATH = os.path.join(OUTPUT_DIR, 'ls-mirrors.json')
PROBE_MIRRORS_INPUT_PATH = os.path.join(OUTPUT_DIR, 'probe-mirrors.json')
NOREACH_MIRRORS_INPUT_PATH = os.path.join(OUTPUT_DIR, 'noreach-mirrors.txt')
NOCONTENT_MIRRORS_INPUT_PATH = os.path.join(OUTPUT_DIR, 'nocontent-mirrors.txt')
FINAL_JSON_OUTPUT_PATH = os.path.join(BASE_DIR, '../..', 'assets/mirrors', 'mirrors.json')

# Define protocol bit masks
PROTO_HTTP = 1    # 0b001
PROTO_HTTPS = 2   # 0b010
PROTO_RSYNC = 4   # 0b100

# Update key order with new compact names
# Final output field order is determined by the code order in compact_mirror_data()
KEY_ORDER = ['country_code', 'distros', 'distro_dirs', 'probe_dirs', 'ls', 'top_level', 'protocols', 'bandwidth', 'internet2']

# Mapping of old keys to new compact keys (for final output)
# It's unused, real mapping is in code compact_mirror_data()
KEY_MAP = {
    'distros': 'top',
    'distro_dirs': 'ls',
    'probe_dirs': 'ls',    # lftp cls probed directories
    'ls': 'ls',             # lftp ls directories
    'top_level': 'top',
    'country_code': 'cc',
    'protocols': 'p',
    'bandwidth': 'bw',
    'internet2': 'i2',
}


# debug_print is now imported from common.py


def get_country_code(country_name):
    """Convert a country name to its ISO 3166-1 alpha-2 code using pycountry.

    Args:
        country_name (str): The country name to convert

    Returns:
        str: Two-letter country code if found, otherwise None
    """
    if not country_name:
        return None

    try:
        # Try exact match first
        country = pycountry.countries.get(name=country_name)
        if country:
            return country.alpha_2.upper()

        # Try searching by name
        countries = pycountry.countries.search_fuzzy(country_name)
        if countries:
            return countries[0].alpha_2.upper()
    except (LookupError, AttributeError):
        pass

    # Handle common variations not in pycountry
    name_variations = {
        'UK':                       'GB',
        'UNITED KINGDOM':           'GB',
        'ENGLAND':                  'GB',
        'UNITED STATES':            'US',
        'USA':                      'US',
        'UNITED STATES OF AMERICA': 'US',
        'TAIWAN':                   'TW',
        'SOUTH KOREA':              'KR',
        'REPUBLIC OF KOREA':        'KR',
        'VIETNAM':                  'VN',
        'Czechia':                  'CZ',
        'TURKEY':                   'TR',
        'RUSSIA':                   'RU',
        'RUSSIAN FEDERATION':       'RU',
        'IRAN, ISLAMIC REPUBLIC OF': 'IR',
    }

    return name_variations.get(country_name.upper())

def guess_country_code_from_domain(domain):
    """
    Guess an ISO-3166-1 alpha-2 country code from a domain.

    Returns
        (code, source)
            code   – two-letter country code or None
            source – 'tld', 'subdomain', or None (reliability flag)
    """
    if not domain:
        return None, None

    domain = domain.lower()
    iso_country_codes = {c.alpha_2.upper() for c in pycountry.countries}

    generic_tlds = {
        'com', 'net', 'org', 'edu', 'gov', 'mil', 'int', 'info', 'biz',
        'name', 'pro', 'aero', 'coop', 'museum', 'mobi', 'jobs', 'travel',
        'app', 'dev', 'cloud', 'online', 'site', 'tech', 'io', 'ai', 'co'
    }

    # 1. Prefer the ccTLD – this is highly reliable
    m = re.search(r'\.([a-z]{2})$', domain)
    if m:
        tld = m.group(1).upper()
        if tld == 'UK':
            tld = 'GB'
        if tld in iso_country_codes and tld not in generic_tlds:
            return tld, 'tld'

    # 2. Fallback: look at sub-domain labels (right-to-left, skipping the TLD)
    #    This is less reliable – many services use 2-letter location tags.
    labels = domain.split('.')[:-1]          # drop the TLD
    for label in reversed(labels):
        if label == 'uk':
            label = 'GB'
        if len(label) == 2 and label.upper() in iso_country_codes:
            return label.upper(), 'subdomain'

    return None, None

def merge_mirror_data(existing_mirrors, new_mirror_url, new_mirror_info):
    """Merges a single new mirror's data into the existing_mirrors structure."""
    new_mirror_url = new_mirror_url.rstrip('/')

    # Convert country to country_code if needed
    if 'country' in new_mirror_info:
        country_code = get_country_code(new_mirror_info['country'])
        if country_code:
            # Check if there was an existing country_code that differs
            if 'country_code' in new_mirror_info and new_mirror_info['country_code'] != country_code:
                print(f"Warning: Converted country code '{country_code}' from '{new_mirror_info['country']}' "
                      f"differs from existing code '{new_mirror_info['country_code']}' for URL: {new_mirror_url}")
            new_mirror_info['country_code'] = country_code
            debug_print(f"Converted country '{new_mirror_info['country']}' to code '{country_code}'", "country")
        del new_mirror_info['country']


    if new_mirror_url not in existing_mirrors:
        existing_mirrors[new_mirror_url] = OrderedDict()
        # Initialize with default order for new entries
        for key in KEY_ORDER:
            if key in new_mirror_info:
                existing_mirrors[new_mirror_url][key] = new_mirror_info[key]
            # Removed default priority assignment
            elif key == 'protocols':
                 existing_mirrors[new_mirror_url][key] = []
            elif key == 'distro_dirs':
                 existing_mirrors[new_mirror_url][key] = [] # Should be filled by new_mirror_info
            elif key == 'distros':
                 existing_mirrors[new_mirror_url][key] = [] # Should be filled by new_mirror_info

    current_entry = existing_mirrors[new_mirror_url]

    # Update fields based on new_mirror_info, maintaining order
    for key in KEY_ORDER:
        if key in new_mirror_info:
            if key == 'distro_dirs':
                current_distro_dirs_val = current_entry.get('distro_dirs', [])
                # Ensure it's a list (though it should be from fetch_new_mirrors)
                if not isinstance(current_distro_dirs_val, list):
                    current_distro_dirs_val = [current_distro_dirs_val]

                new_distro_dirs_val = new_mirror_info['distro_dirs']
                # Ensure new_distro_dirs_val is also a list
                if not isinstance(new_distro_dirs_val, list):
                    new_distro_dirs_val = [new_distro_dirs_val]

                for item in new_distro_dirs_val:
                    if item not in current_distro_dirs_val:
                        current_distro_dirs_val.append(item)
                current_entry['distro_dirs'] = current_distro_dirs_val
            elif key == 'distros':
                current_distros_val = current_entry.get('distros', [])
                # Ensure it's a list
                if not isinstance(current_distros_val, list):
                    current_distros_val = [current_distros_val]

                new_distros_val = new_mirror_info['distros']
                # Ensure new_distros_val is also a list
                if not isinstance(new_distros_val, list):
                    new_distros_val = [new_distros_val]

                for item in new_distros_val:
                    if item not in current_distros_val:
                        current_distros_val.append(item)
                current_entry['distros'] = current_distros_val
            elif key == 'protocols':
                current_protocols = current_entry.get('protocols', [])
                new_protocols = new_mirror_info.get('protocols', [])
                for p in new_protocols:
                    if p not in current_protocols:
                        current_protocols.append(p)
                current_entry['protocols'] = sorted(list(set(current_protocols))) # Keep unique and sorted
            elif key == 'top_level':
                current_entry[key] = new_mirror_info[key]
            elif key not in current_entry: # Add if new and not present
                current_entry[key] = new_mirror_info[key]
        # Removed default priority assignment

    # Remove keys that are not in KEY_ORDER and have no value (or default empty list for protocols/distro_dirs)
    keys_to_remove = []
    for k in current_entry.keys():
        if k not in KEY_ORDER:
            keys_to_remove.append(k)
        elif k in ['protocols', 'distro_dirs'] and not current_entry[k]: # remove empty protocol/distro_dirs lists if they were not filled
            pass # Keep them for now, might be filled by YAML
    for k_rem in keys_to_remove:
        del current_entry[k_rem]

    # Ensure the entry itself is an OrderedDict with the desired key order
    ordered_entry = OrderedDict()
    for key_in_order in KEY_ORDER:
        if key_in_order in current_entry:
            ordered_entry[key_in_order] = current_entry[key_in_order]
    existing_mirrors[new_mirror_url] = ordered_entry

def compact_mirror_data(url, data):
    """Convert mirror data to compact format."""
    compact = {}

    # Handle country_code → cc
    if 'country_code' in data:
        compact['cc'] = data['country_code']

    # Handle top_level → top (1)
    if data.get('top_level') or data.get('root'):
        distros = data.get('distros', [])
        if len(distros) != 1:
            print(f"WARN: top_level url shall have 1 distros: {url} {data}")
        compact['top'] = distros[0]

        if 'distro_dirs' in data and data['distro_dirs']:
            print(f"WARN: top_level url shall not have distro_dirs entry: {url} {data}")
        if 'ls' in data and data['ls']:
            print(f"WARN: top_level url shall not have ls entry: {url} {data}")
        if 'probe_dirs' in data and data['probe_dirs']:
            print(f"WARN: top_level url shall not have probe_dirs entry: {url} {data}")

    # Combine all directories into ls (remove dir and pdir)
    all_dirs = []

    # Add distro_dirs (filter out fedora 'alt')
    if 'distro_dirs' in data:
        distro_dirs = [d for d in data['distro_dirs'] if 'alt' not in d]
        all_dirs.extend(distro_dirs)

    # Add probe_dirs
    if 'probe_dirs' in data and data['probe_dirs']:
        all_dirs.extend(data['probe_dirs'])

    # Add ls directories
    if 'ls' in data and data['ls']:
        all_dirs.extend(data['ls'])

    # Remove duplicates and sort
    if all_dirs:
        unique_dirs = sorted(set(all_dirs))
        compact['ls'] = unique_dirs

    # Handle protocols → p (bitmask)
    if 'protocols' in data:
        bitmask = 0
        for proto in data['protocols']:
            proto = proto.lower()
            if proto == 'http':
                bitmask |= PROTO_HTTP
            elif proto == 'https':
                bitmask |= PROTO_HTTPS
            elif proto == 'rsync':
                bitmask |= PROTO_RSYNC
        if bitmask:
            compact['p'] = bitmask

    # Handle bandwidth → bw
    if 'bandwidth' in data:
        compact['bw'] = data['bandwidth']

    # Handle internet2 → i2 (1)
    if data.get('internet2'):
        compact['i2'] = 1

    return compact

def deduplicate_mirrors_by_base_url(mirrors):
    """
    Deduplicate mirrors by keeping the one with the shortest path when the same base site exists.
    Merges data from longer path URLs into the shortest path URL, adding path components to distro_dirs if valid.

    Args:
        mirrors (OrderedDict): Dictionary of URL -> mirror data

    Returns:
        OrderedDict: Deduplicated mirrors
    """
    # Ensure valid directories are computed
    valid_dirs = get_valid_dirs(BASE_DIR)

    def get_delta_suffix(parent_path, child_path):
        """Return the path suffix of child_path relative to parent_path.
        Returns empty string if child_path is not a descendant of parent_path."""
        # Remove leading/trailing slashes
        parent = parent_path.strip('/')
        child = child_path.strip('/')
        if parent == '':
            # root parent
            return child
        # Check if child starts with parent + '/'
        if child.startswith(parent + '/'):
            return child[len(parent) + 1:]  # Remove parent and the slash
        return ''

    def should_merge_long_url(shortest_path, long_path, long_data, valid_dirs):
        """Determine whether a longer URL should be merged into the shortest URL.
        Returns tuple (should_merge, delta_suffix)."""
        if long_path == shortest_path:
            # Same path duplicate (different scheme). Merge all fields.
            return True, ''
        delta = get_delta_suffix(shortest_path, long_path)
        if not delta:
            # Not a descendant
            return False, ''
        # Check delta suffix is in valid_dirs (case-insensitive)
        if delta not in valid_dirs:
            return False, ''
        # Check top_level field is true
        if not long_data.get('top_level'):
            return False, ''
        return True, delta

    # Group mirrors by site (netloc only, ignoring scheme)
    base_url_groups = {}

    for url, data in mirrors.items():
        parsed = urlparse(url)
        base_url = parsed.netloc

        if base_url not in base_url_groups:
            base_url_groups[base_url] = []

        base_url_groups[base_url].append((url, data, parsed.path))

    deduplicated_mirrors = OrderedDict()

    for base_url, url_group in base_url_groups.items():
        if len(url_group) == 1:
            # Only one URL for this base, keep it
            url, data, _ = url_group[0]
            deduplicated_mirrors[url] = data
        else:
            # Multiple URLs for the same base
            # Sort by path length (ascending) and then by path (for consistency)
            url_group.sort(key=lambda x: (len(x[2]), x[2]))

            # Check if the shortest is root (empty path) and has minimal data
            # If so, we might want to drop it in favor of more specific paths
            shortest_url, shortest_data, shortest_path = url_group[0]

            # Count distros in shortest entry
            shortest_distros = len(shortest_data.get('distros', []))
            shortest_has_ls = 'ls' in shortest_data and shortest_data['ls']
            shortest_has_pdir = 'pdir' in shortest_data and shortest_data['pdir']

            # If shortest is root and has minimal data, consider dropping it
            drop_root = False
            if shortest_path == '' and len(url_group) > 1:
                # Root with only 0-1 distros and no ls data is likely not a real mirror
                # when there are more specific paths available
                if shortest_distros <= 1 and not shortest_has_ls:
                    print(f"Dropping root URL with minimal data: {shortest_url}")
                    drop_root = True
                    # Remove the root from consideration
                    url_group = url_group[1:]
                    if not url_group:
                        continue
                    # Get new shortest
                    shortest_url, shortest_data, shortest_path = url_group[0]

            # Create a temporary entry for merging valid paths
            merged_entry = OrderedDict()
            merged_entry[shortest_url] = shortest_data

            # URLs to keep separate (with structural path components)
            separate_urls = []

            # Process longer path URLs (starting from index 1 if we didn't drop root, 0 if we did)
            start_idx = 0 if drop_root else 1
            for url, data, path in url_group[start_idx:]:
                # Skip if this is the shortest (which we already have in merged_entry)
                if url == shortest_url and path == shortest_path:
                    continue

                # Debug for specific site
                if 'mirrors.mit.edu' in url:
                    print(f"DEBUG mirrors.mit.edu: processing {url}, path={path}, data={data}, shortest_url={shortest_url}")

                # Determine whether to merge this longer URL
                should_merge, delta_suffix = should_merge_long_url(shortest_path, path, data, valid_dirs)

                if should_merge:
                    if delta_suffix:
                        # Delta suffix is valid and long URL has top_level true
                        print(f"Merging duplicate base URL with longer path: {url} into {shortest_url} (delta suffix: {delta_suffix})")
                        # Add delta suffix to distro_dirs of the canonical entry
                        current_dirs = merged_entry[shortest_url].get('distro_dirs', [])
                        if not isinstance(current_dirs, list):
                            current_dirs = [current_dirs]
                        if delta_suffix not in current_dirs:
                            current_dirs.append(delta_suffix)
                        merged_entry[shortest_url]['distro_dirs'] = current_dirs
                        # Do NOT merge any other fields from the longer URL
                    else:
                        # Same path duplicate (different scheme) - merge all fields
                        print(f"Merging duplicate base URL with same path: {url} into {shortest_url}")
                        merge_mirror_data(merged_entry, shortest_url, data)
                else:
                    # Keep as separate entry
                    print(f"Keeping separate due to invalid delta suffix or missing top_level: {url}")
                    separate_urls.append((url, data))

            # Add the merged entry (if we merged anything or even if just the shortest)
            if not drop_root or merged_entry[shortest_url].get('distros') or merged_entry[shortest_url].get('ls') or merged_entry[shortest_url].get('pdir'):
                deduplicated_mirrors[shortest_url] = merged_entry[shortest_url]

            # Add separate entries
            for url, data in separate_urls:
                deduplicated_mirrors[url] = data

    return deduplicated_mirrors

def load_blacklist():
    """Load mirror blacklist from noreach-mirrors.txt and nocontent-mirrors.txt"""
    blacklist = set()
    noreach_count = 0
    nocontent_count = 0

    if os.path.exists(NOREACH_MIRRORS_INPUT_PATH):
        try:
            with open(NOREACH_MIRRORS_INPUT_PATH, 'r') as f:
                for line in f:
                    url = line.strip()
                    if url and not url.startswith('#'):
                        blacklist.add(url.rstrip('/'))
                        noreach_count += 1
            print(f"Loaded {noreach_count} connection error mirrors from {NOREACH_MIRRORS_INPUT_PATH}")
        except Exception as e:
            print(f"Error loading noreach blacklist from {NOREACH_MIRRORS_INPUT_PATH}: {e}")

    if os.path.exists(NOCONTENT_MIRRORS_INPUT_PATH):
        try:
            with open(NOCONTENT_MIRRORS_INPUT_PATH, 'r') as f:
                for line in f:
                    url = line.strip()
                    if url and not url.startswith('#'):
                        blacklist.add(url.rstrip('/'))
                        nocontent_count += 1
            print(f"Loaded {nocontent_count} no content mirrors from {NOCONTENT_MIRRORS_INPUT_PATH}")
        except Exception as e:
            print(f"Error loading nocontent blacklist from {NOCONTENT_MIRRORS_INPUT_PATH}: {e}")

    print(f"Total blacklisted mirrors: {len(blacklist)}")
    return blacklist

def merge_probe_data(existing_mirrors, probe_data):
    """Merge probe data into existing mirrors, removing duplicates with ls/dir/os fields."""
    for url, probe_info in probe_data.items():
        if url not in existing_mirrors:
            existing_mirrors[url] = OrderedDict()
            # Initialize with default order for new entries
            for key in KEY_ORDER:
                if key == 'protocols':
                    existing_mirrors[url][key] = []
                elif key == 'distro_dirs':
                    existing_mirrors[url][key] = []
                elif key == 'distros':
                    existing_mirrors[url][key] = []

        if 'probe_dirs' in probe_info and probe_info['probe_dirs']:
            # Get existing directories from ls, dir, and os fields
            existing_dirs = set()

            # Add ls directories
            if 'ls' in existing_mirrors[url]:
                existing_dirs.update(existing_mirrors[url]['ls'])

            # Add distro_dirs
            if 'distro_dirs' in existing_mirrors[url]:
                existing_dirs.update(existing_mirrors[url]['distro_dirs'])

            # Add distros (os field)
            if 'distros' in existing_mirrors[url]:
                existing_dirs.update(existing_mirrors[url]['distros'])

            # Filter out duplicates
            filtered_probe_dirs = [d for d in probe_info['probe_dirs'] if d not in existing_dirs]

            if filtered_probe_dirs:
                existing_mirrors[url]['probe_dirs'] = filtered_probe_dirs
                debug_print(f"Merged probe data for {url}: {len(filtered_probe_dirs)} unique directories")
            else:
                debug_print(f"No unique probe directories for {url} (all duplicates)")

def main():
    print("Starting mirror merging process...")
    final_mirrors = OrderedDict()

    # Load blacklist first
    blacklist = load_blacklist()

    # 1. Load new mirrors from official-mirrors.json
    if os.path.exists(OFFICIAL_MIRRORS_INPUT_PATH):
        print(f"Loading new mirrors from {OFFICIAL_MIRRORS_INPUT_PATH}...")
        try:
            with open(OFFICIAL_MIRRORS_INPUT_PATH, 'r') as f:
                official_mirrors = json.load(f)
            print(f"Loaded {len(official_mirrors)} mirrors from {OFFICIAL_MIRRORS_INPUT_PATH}.")
            for url, data in official_mirrors.items():
                # Skip blacklisted URLs
                if url.rstrip('/') in blacklist:
                    print(f"Skipping blacklisted URL from official-mirrors.json: {url}")
                    continue
                merge_mirror_data(final_mirrors, url, data)
        except Exception as e:
            print(f"Error loading or parsing {OFFICIAL_MIRRORS_INPUT_PATH}: {e}")
    else:
        print(f"{OFFICIAL_MIRRORS_INPUT_PATH} not found. No new mirrors to merge.")

    # 2. Load and merge ls data from ls-mirrors.json
    if os.path.exists(LS_MIRRORS_INPUT_PATH):
        print(f"Loading ls data from {LS_MIRRORS_INPUT_PATH}...")
        try:
            with open(LS_MIRRORS_INPUT_PATH, 'r') as f:
                ls_mirrors_data = json.load(f)
            print(f"Loaded ls data for {len(ls_mirrors_data)} mirrors from {LS_MIRRORS_INPUT_PATH}.")
            for url, ls_data in ls_mirrors_data.items():
                # Skip blacklisted URLs
                if url.rstrip('/') in blacklist:
                    print(f"Skipping blacklisted URL from ls-mirrors.json: {url}")
                    continue

                # Ensure the mirror exists in final_mirrors
                if url not in final_mirrors:
                    final_mirrors[url] = OrderedDict()
                    # Initialize with default order for new entries
                    for key in KEY_ORDER:
                        if key == 'protocols':
                            final_mirrors[url][key] = []
                        elif key == 'distro_dirs':
                            final_mirrors[url][key] = []
                        elif key == 'distros':
                            final_mirrors[url][key] = []

                # Merge ls data
                if 'ls' in ls_data and ls_data['ls']:
                    final_mirrors[url]['ls'] = ls_data['ls']
                    debug_print(f"Merged ls data for {url}: {len(ls_data['ls'])} directories")
                if 'cc' in ls_data and ls_data['cc']:
                    final_mirrors[url]['country_code'] = ls_data['cc']
        except Exception as e:
            print(f"Error loading or parsing {LS_MIRRORS_INPUT_PATH}: {e}")
    else:
        print(f"{LS_MIRRORS_INPUT_PATH} not found. No ls data to merge.")

    # 3. Load and merge probe data from probe-mirrors.json
    if os.path.exists(PROBE_MIRRORS_INPUT_PATH):
        print(f"Loading probe data from {PROBE_MIRRORS_INPUT_PATH}...")
        try:
            with open(PROBE_MIRRORS_INPUT_PATH, 'r') as f:
                probe_mirrors_data = json.load(f)
            print(f"Loaded probe data for {len(probe_mirrors_data)} mirrors from {PROBE_MIRRORS_INPUT_PATH}.")
            merge_probe_data(final_mirrors, probe_mirrors_data)
        except Exception as e:
            print(f"Error loading or parsing {PROBE_MIRRORS_INPUT_PATH}: {e}")
    else:
        print(f"{PROBE_MIRRORS_INPUT_PATH} not found. No probe data to merge.")

    print(f"Mirrors after loading and merging: {len(final_mirrors)}")

    # 4. Deduplicate mirrors by base URL, keeping the one with longer path
    print("Deduplicating mirrors by base URL...")
    final_mirrors = deduplicate_mirrors_by_base_url(final_mirrors)
    print(f"Mirrors after deduplication: {len(final_mirrors)}")

    # 5. Post-processing: ensure all entries follow the KEY_ORDER and clean up empty optional fields
    processed_final_mirrors = OrderedDict()

    for url, data in final_mirrors.items():

        # Try to guess country code from domain
        domain = urlparse(url).netloc
        if domain:
            guessed_cc, cc_source = guess_country_code_from_domain(domain)

            # Ignore common country domains abused
            if guessed_cc in ['MM', 'ME', 'IO', 'CO', 'CC']:
                guessed_cc = None

            # If we managed to guess a code, drop the now-redundant country_domain key
            if 'country_domain' in data:
                del data['country_domain']

            # Warn ONLY when the reliable ccTLD disagrees with stored code
            if guessed_cc:
                if data.get('country_code') and data['country_code'] != guessed_cc:
                    print(f"Warning: country_code '{data['country_code']}' does not match "
                          f"country_domain '{guessed_cc}' for URL: {url}")
                else:
                    data['country_code'] = guessed_cc

        # Compact the data
        ordered_data = compact_mirror_data(url, data)

        if 'cc' not in ordered_data:
            print(f"Skipping no-cc {url} {ordered_data}")
            continue

        if ordered_data.get('top') == 'debian':
            print(f"Skipping root Debian {url} {ordered_data}") # a bit too complex for mirror.rs to handle this case
            continue

        # Add mirror if it has required fields and valid ls data
        if ordered_data.get('top') or ordered_data.get('ls'):
            processed_final_mirrors[url] = ordered_data
        else:
            print(f"Skipping mirror {url}: missing top/ls data")

    print(f"Writing {len(processed_final_mirrors)} merged mirrors to {FINAL_JSON_OUTPUT_PATH}...")
    os.makedirs(os.path.dirname(FINAL_JSON_OUTPUT_PATH), exist_ok=True)
    with open(FINAL_JSON_OUTPUT_PATH, 'w') as f:
        f.write('{')

        # Get sorted list of URLs for consistent output
        urls = sorted(list(processed_final_mirrors.keys()))

        for i, url in enumerate(urls):
            url_json = json.dumps(url)
            metadata = processed_final_mirrors[url]

            # Create compact JSON for this URL entry on one line
            # Format metadata as a single compact JSON object
            metadata_str = json.dumps(metadata, separators=(',', ':'))

            # Write the URL and its metadata on one line
            f.write('\n' + url_json + ':' + metadata_str)

            # Add comma if not the last URL
            if i < len(urls) - 1:
                f.write(',')

        # Close the entire JSON object
        f.write('\n}\n')

    print("Mirror merging complete.")

if __name__ == "__main__":
    main()
