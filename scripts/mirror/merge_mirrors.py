#!/usr/bin/env python3
import json
import yaml
import os
import re
from collections import OrderedDict
from urllib.parse import urlparse
import sys

# Import pycountry for ISO country code detection
import pycountry

# Import common utilities
from common import debug_print

# URL blacklist for problematic mirrors
URL_BLACKLIST = [
    'http://mirror.one.com',    # redirects to mirror.group.one
    'http://fedora-alt.c3sl.ufpr.br',
    'https://linux2.yz.yamagata-u.ac.jp/pub/linux', # duplicate DNS
    #  'https://ftp.funet.fi/pub/mirrors/ftp.opensuse.com/pub',
]

# Define paths
BASE_DIR = os.path.dirname(os.path.abspath(__file__))
NEW_MIRRORS_INPUT_PATH = os.path.join(BASE_DIR, 'new-mirrors.json')
LS_MIRRORS_INPUT_PATH = os.path.join(BASE_DIR, 'ls-mirrors.json')
EXISTING_YAML_PATH = os.path.join(BASE_DIR, '../..', 'channel', 'mirrors.yaml') # Original YAML
FINAL_JSON_OUTPUT_PATH = os.path.join(BASE_DIR, '../..', 'channel', 'mirrors.json')

# Define protocol bit masks
PROTO_HTTP = 1    # 0b001
PROTO_HTTPS = 2   # 0b010
PROTO_RSYNC = 4   # 0b100

# Update key order with new compact names
KEY_ORDER = ['distros', 'distro_dirs', 'top_level', 'country_code', 'protocols', 'bandwidth', 'internet2', 'ls']

# Mapping of old keys to new compact keys (for final output)
KEY_MAP = {
    'distros': 'os',
    'distro_dirs': 'dir',
    'top_level': 'root',
    'country_code': 'cc',
    'protocols': 'p',
    'bandwidth': 'bw',
    'internet2': 'i2',
    'ls': 'ls'  # Keep ls as is
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

def compact_mirror_data(data):
    """Convert mirror data to compact format."""
    compact = {}

    # Handle distros → os
    if 'distros' in data:
        compact['os'] = data['distros']

    # Handle distro_dirs → dir, removing duplicates with distros
    if 'distro_dirs' in data:
        # Remove fedora 'alt'
        distro_dirs = [d for d in data['distro_dirs'] if 'alt' not in d]
        # Remove duplicates in distros
        unique_dirs = set(distro_dirs) - set(data.get('distros', []))
        if unique_dirs:
            compact['dir'] = sorted(list(unique_dirs))

    # Handle top_level → root (1)
    if data.get('top_level'):
        compact['root'] = 1

    # Handle country_code → cc
    if 'country_code' in data:
        compact['cc'] = data['country_code']

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

    # Handle ls → ls (remove duplicates with distros and distro_dirs)
    if 'ls' in data and data['ls']:
        ls_dirs = data['ls']
        # Remove duplicates that exist in distros and distro_dirs
        distros_set = set(data.get('distros', []))
        distro_dirs_set = set(data.get('distro_dirs', []))
        unique_ls = [d for d in ls_dirs if d not in distros_set and d not in distro_dirs_set]
        if unique_ls:
            compact['ls'] = unique_ls

    return compact

def deduplicate_mirrors_by_base_url(mirrors):
    """
    Deduplicate mirrors by keeping the one with the longer path when the same base site exists.

    Args:
        mirrors (OrderedDict): Dictionary of URL -> mirror data

    Returns:
        OrderedDict: Deduplicated mirrors
    """
    from urllib.parse import urlparse

    # Group mirrors by base URL (scheme + netloc)
    base_url_groups = {}

    for url, data in mirrors.items():
        parsed = urlparse(url)
        base_url = f"{parsed.scheme}://{parsed.netloc}"

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
            # Multiple URLs for the same base, find the one with the longest path
            # Sort by path length (descending) and then by path (for consistency)
            url_group.sort(key=lambda x: (-len(x[2]), x[2]))

            longest_url, longest_data, longest_path = url_group[0]

            # Remove shorter path URLs and keep only the longest one
            for url, data, path in url_group[1:]:
                print(f"Removing duplicate base URL with shorter path: {url} (keeping {longest_url})")

            # Keep only the URL with the longest path
            deduplicated_mirrors[longest_url] = longest_data

    return deduplicated_mirrors

def main():
    print("Starting mirror merging process...")
    final_mirrors = OrderedDict()

    # 1. Load existing mirrors.yaml (if it exists) as a base
    if os.path.exists(EXISTING_YAML_PATH):
        print(f"Loading base mirrors from {EXISTING_YAML_PATH}...")
        try:
            with open(EXISTING_YAML_PATH, 'r') as f:
                yaml_mirrors = yaml.safe_load(f) or {}
                for url, data in yaml_mirrors.items():
                    # Convert to OrderedDict to maintain structure when merging
                    ordered_data = OrderedDict()
                    for key in KEY_ORDER:
                        if key in data:
                            ordered_data[key] = data[key]
                    final_mirrors[url.rstrip('/')] = ordered_data
            print(f"Loaded {len(final_mirrors)} mirrors from YAML.")
        except Exception as e:
            print(f"Error loading or parsing {EXISTING_YAML_PATH}: {e}. Starting with an empty base.")
            final_mirrors = OrderedDict()
    else:
        print(f"{EXISTING_YAML_PATH} not found. Starting with an empty base.")
        final_mirrors = OrderedDict()

    # 2. Load new mirrors from new-mirrors.json
    if os.path.exists(NEW_MIRRORS_INPUT_PATH):
        print(f"Loading new mirrors from {NEW_MIRRORS_INPUT_PATH}...")
        try:
            with open(NEW_MIRRORS_INPUT_PATH, 'r') as f:
                new_mirrors_data = json.load(f)
            print(f"Loaded {len(new_mirrors_data)} mirrors from {NEW_MIRRORS_INPUT_PATH}.")
            for url, data in new_mirrors_data.items():
                merge_mirror_data(final_mirrors, url, data)
        except Exception as e:
            print(f"Error loading or parsing {NEW_MIRRORS_INPUT_PATH}: {e}")
    else:
        print(f"{NEW_MIRRORS_INPUT_PATH} not found. No new mirrors to merge.")

    # 3. Load and merge ls data from ls-mirrors.json
    if os.path.exists(LS_MIRRORS_INPUT_PATH):
        print(f"Loading ls data from {LS_MIRRORS_INPUT_PATH}...")
        try:
            with open(LS_MIRRORS_INPUT_PATH, 'r') as f:
                ls_mirrors_data = json.load(f)
            print(f"Loaded ls data for {len(ls_mirrors_data)} mirrors from {LS_MIRRORS_INPUT_PATH}.")
            for url, ls_data in ls_mirrors_data.items():
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
                if 'root' in ls_data and ls_data['root']:
                    final_mirrors[url]['top_level'] = ls_data['root']
        except Exception as e:
            print(f"Error loading or parsing {LS_MIRRORS_INPUT_PATH}: {e}")
    else:
        print(f"{LS_MIRRORS_INPUT_PATH} not found. No ls data to merge.")

    print(f"Mirrors after loading and merging: {len(final_mirrors)}")

    # 4. Deduplicate mirrors by base URL, keeping the one with longer path
    print("Deduplicating mirrors by base URL...")
    final_mirrors = deduplicate_mirrors_by_base_url(final_mirrors)
    print(f"Mirrors after deduplication: {len(final_mirrors)}")

    # 5. Post-processing: ensure all entries follow the KEY_ORDER and clean up empty optional fields
    processed_final_mirrors = OrderedDict()

    for url, data in final_mirrors.items():

        if url in URL_BLACKLIST:
            print(f"Skipping blacklisted URL: {url}")
            continue

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
        ordered_data = compact_mirror_data(data)

        if ordered_data.get('root') == 1 and ordered_data.get('os') == ['debian']:
            print(f"Skipping root Debian") # a bit too complex for mirror.rs to handle this case
            continue

        # Add mirror if it has required fields and valid ls data
        if ordered_data.get('os') or ordered_data.get('dir') or ordered_data.get('ls'):
            processed_final_mirrors[url] = ordered_data
        else:
            print(f"Skipping mirror {url}: missing os/dir/ls data")

    print(f"Writing {len(processed_final_mirrors)} merged mirrors to {FINAL_JSON_OUTPUT_PATH}...")
    os.makedirs(os.path.dirname(FINAL_JSON_OUTPUT_PATH), exist_ok=True)
    with open(FINAL_JSON_OUTPUT_PATH, 'w') as f:
        f.write('{')

        # Get sorted list of URLs for consistent output
        urls = list(processed_final_mirrors.keys())

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
        f.write('\n}')

    print("Mirror merging complete.")

if __name__ == "__main__":
    main()
