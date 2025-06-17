import requests
import yaml
import json
import re
from bs4 import BeautifulSoup
import os
import sys
from collections import OrderedDict
import glob
from urllib.parse import urlparse, urlunparse

# debug_print is now imported from common.py

# Define paths for local mirror files
BASE_DIR = os.path.dirname(os.path.abspath(__file__))
FEDORA_MIRRORS_PATH = os.path.join(BASE_DIR, 'mirrors-fedora.html')
UBUNTU_MIRRORS_PATH = os.path.join(BASE_DIR, 'mirrors-ubuntu.html')
NEW_MIRRORS_OUTPUT_PATH = os.path.join(BASE_DIR, 'new-mirrors.json')

# Cache file names (to be located in BASE_DIR, which is 'scripts/')
ALPINE_CACHE_TXT = "mirrors-alpine.txt"
ARCH_CACHE_TXT = "mirrors-archlinux.txt"
DEBIAN_CACHE_HTML = "mirrors-debian.html"
OPENSUSE_CACHE_HTML = "mirrors-opensuse.html"

# URLs for fetching mirror lists
ALPINE_MIRRORS_URL = "https://www.alpinelinux.org/mirrors/"
DEBIAN_MIRRORS_URL = "https://www.debian.org/mirror/list"
UBUNTU_MIRRORS_URL = "https://launchpad.net/ubuntu/+archivemirrors"
ARCH_HTML_MIRRORLIST_URL = "https://archlinux.org/mirrorlist/all/"
OPENSUSE_MIRRORS_URL = "https://mirrors.opensuse.org/"
OPENEULER_MIRRORS_PATH = os.path.join(BASE_DIR, 'mirrors-openeuler.html')

# Import common utilities
from common import load_distro_configs, get_distro_configs, debug_print

def parse_bandwidth(bandwidth_text):
    """Parse bandwidth text (like '10 Gbps') to a numerical value in Mbps.

    For example:
    - '10 Gbps' -> 10000 (10 Gbps = 10,000 Mbps)
    - '10 Mbps' -> 10 (10 Mbps = 10 Mbps)
    - '1 Gbps' -> 1000 (1 Gbps = 1,000 Mbps)
    - '100 Mbps' -> 100 (100 Mbps = 100 Mbps)
    """
    if not bandwidth_text:
        return None

    # Convert to lowercase for easier matching
    bandwidth_text = str(bandwidth_text).lower()

    # Try to extract the number
    number_match = re.search(r'([\d.]+)', bandwidth_text)
    if not number_match:
        return None

    value = float(number_match.group(1))

    # Convert to Mbps based on unit
    if 'gbps' in bandwidth_text or ' gb' in bandwidth_text or 'gbit' in bandwidth_text:
        # 1 Gbps = 1000 Mbps
        value *= 1000
    elif 'tbps' in bandwidth_text or ' tb' in bandwidth_text or 'tbit' in bandwidth_text:
        # 1 Tbps = 1,000,000 Mbps
        value *= 1000000
    elif 'kbps' in bandwidth_text or ' kb' in bandwidth_text or 'kbit' in bandwidth_text:
        # 1 Kbps = 0.001 Mbps
        value *= 0.001

    # Round to integer for cleaner values
    return int(value)

# load_distro_configs is now imported from common.py


# Helper function to add/update a mirror entry
def add_mirror(temp_mirror_groups, original_url, canonical_distro_name_from_parser, metadata_from_parser):
    """
    Processes a mirror URL, attempts to strip known distro prefixes, and aggregates it
    into temp_mirror_groups. Grouping is based on (netloc, common_path_after_stripping).
    """
    debug_print(f"Called for {canonical_distro_name_from_parser}. URL='{original_url}'", "mirror")

    original_url_cleaned = original_url.rstrip('/')
    parsed_url = urlparse(original_url_cleaned)
    netloc = parsed_url.netloc
    original_path = parsed_url.path

    if parsed_url.scheme.lower() == 'rsync':
        pass  # Still process for grouping purposes

    common_path_for_grouping = original_path
    path_was_stripped = False

    # Get prefixes from config only
    distro_configs = get_distro_configs()
    prefixes_from_config = distro_configs.get(canonical_distro_name_from_parser, [])

    if not prefixes_from_config:
        # If we don't have any prefixes, use the distro name itself as a fallback
        prefixes_from_config = [canonical_distro_name_from_parser]

    prefixes_from_config.sort(key=len, reverse=True) # Longest first for specificity

    for prefix_to_try in prefixes_from_config:
        # Case 1: URL path ends with the distro name (e.g. /repo/opensuse)
        if original_path.endswith(f"/{prefix_to_try}"):
            common_path_for_grouping = original_path[:-len(prefix_to_try)-1]
            if not common_path_for_grouping:
                common_path_for_grouping = "/"
            path_was_stripped = True
            debug_print(f"Stripped trailing '{prefix_to_try}' from URL path: '{original_path}' -> '{common_path_for_grouping}'", "strip")
            break

        # Case 2: URL path starts with /<prefix>/ or is exactly /<prefix>
        elif original_path.startswith(f"/{prefix_to_try}/") or original_path == f"/{prefix_to_try}":
            common_path_for_grouping = "/" # Group at the host's root level
            path_was_stripped = True
            debug_print(f"Stripped leading '{prefix_to_try}' from URL path: '{original_path}' -> '/'", "strip")
            break

        # Case 3: URL contains /pub/<prefix> pattern - very common in mirror directories
        elif f"/pub/{prefix_to_try}" in original_path:
            parts = original_path.split(f"/pub/{prefix_to_try}")
            common_path_for_grouping = parts[0] + "/pub"
            path_was_stripped = True
            debug_print(f"Stripped '{prefix_to_try}' from /pub/ path: '{original_path}' -> '{common_path_for_grouping}'", "strip")
            break

    group_key = (netloc, common_path_for_grouping)

    if group_key not in temp_mirror_groups:
        temp_mirror_groups[group_key] = {
            'distro_dirs': set(),
            'protocols': set(),
            'original_urls': set(),
            'metadata_store': {},
            'representative_scheme': parsed_url.scheme.lower(),
            'distros': set()
        }

    entry = temp_mirror_groups[group_key]
    entry['distros'].add(canonical_distro_name_from_parser)

    if path_was_stripped:
        if common_path_for_grouping == '/':
            stripped_dirs = original_path[1:].strip('/')
        else:
            stripped_dirs = original_path[len(common_path_for_grouping):].strip('/')

        if stripped_dirs:
            entry['distro_dirs'].add(stripped_dirs)
            debug_print(f"Added stripped directory '{stripped_dirs}' to distro_dirs", "strip")
        else:
            entry['distro_dirs'].add(canonical_distro_name_from_parser)
            debug_print(f"Fallback: Using canonical name '{canonical_distro_name_from_parser}' for distro_dirs", "strip")

    entry['protocols'].add(parsed_url.scheme.lower())
    entry['original_urls'].add(original_url_cleaned)

    # Update representative_scheme preference (https > http > others)
    current_scheme_lower = parsed_url.scheme.lower()
    if current_scheme_lower == 'https':
        entry['representative_scheme'] = 'https'
    elif current_scheme_lower == 'http' and entry['representative_scheme'] != 'https':
        entry['representative_scheme'] = 'http'

    # Merge metadata
    for meta_key, meta_val in metadata_from_parser.items():
        if meta_key in ['distro_dirs', 'protocols', 'top_level']:
            continue
        if meta_key == 'internet2' and meta_val is False:
            continue
        if meta_key == 'priority':
            continue

        if meta_val is not None:
            entry['metadata_store'][meta_key] = meta_val

    # Set top_level=True when distro_dirs is empty (indicating a top-level mirror)
    if not entry['distro_dirs']:
        entry['metadata_store']['top_level'] = True

def get_content_from_url_or_cache(url, cache_filename, base_dir, is_json=False, timeout=15):
    cache_filepath = os.path.join(base_dir, cache_filename)
    debug_print(f"Attempting to use cache for {url} at {cache_filepath}", "cache")
    try:
        if os.path.exists(cache_filepath):
            debug_print(f"Cache hit: Reading from {cache_filepath}", "cache")
            if is_json:
                with open(cache_filepath, 'r', encoding='utf-8') as f:
                    return json.load(f)
            else: # HTML
                with open(cache_filepath, 'rb') as f: # Read as bytes for BeautifulSoup
                    return f.read()
        else:
            debug_print(f"Cache miss: Downloading from {url}", "cache")
            response = requests.get(url, timeout=timeout)
            response.raise_for_status()

            os.makedirs(base_dir, exist_ok=True)

            if is_json:
                try:
                    parsed_json = response.json() # Parse to validate and to pretty debug_print
                    with open(cache_filepath, 'w', encoding='utf-8') as f:
                        json.dump(parsed_json, f, indent=4)
                    debug_print(f"Cached JSON to {cache_filepath}", "cache")
                    return parsed_json # Return parsed JSON
                except json.JSONDecodeError as e:
                    debug_print(f"Downloaded content from {url} is not valid JSON: {e}")
                    return None
            else: # HTML
                with open(cache_filepath, 'wb') as f: # Write as bytes
                    f.write(response.content)
                debug_print(f"Cached HTML to {cache_filepath}", "cache")
                return response.content # Return bytes

    except requests.RequestException as e:
        debug_print(f"Failed to fetch {url}: {e}")
    except IOError as e:
        debug_print(f"File I/O error for {cache_filepath}: {e}")
    return None # General failure or if JSON parsing failed after download

def parse_alpine_mirrors(temp_mirror_groups):
    debug_print("Fetching and parsing Alpine Linux mirrors...")
    html_content = get_content_from_url_or_cache(ALPINE_MIRRORS_URL, ALPINE_CACHE_TXT, BASE_DIR)
    alpine_added_count = 0

    if not html_content:
        print("Failed to get Alpine mirrors content.")
        return

    debug_print(f"Alpine HTML content length: {len(html_content) if html_content else 0}")
    soup = BeautifulSoup(html_content, 'html.parser')

    with open(os.path.join(BASE_DIR, ALPINE_CACHE_TXT), 'r', encoding='utf-8', errors='replace') as f:
        debug_print(f"Reading Alpine mirrors from cache file...")
        try:
            lines = f.readlines()
            debug_print(f"Alpine text file has {len(lines)} lines")
            # Extract URLs from text file - Alpine mirror list is simple
            for line in lines:
                line = line.strip()
                # Look for HTTP URLs in the line
                if line.startswith('http://') or line.startswith('https://'):
                    mirror_url = line.strip()
                    debug_print(f"Alpine: Found mirror URL: {mirror_url}")
                    metadata = {
                        'country': None,  # We don't have country info from the plain text file
                        'country_code': None
                    }
                    add_mirror(temp_mirror_groups, mirror_url, "alpine", metadata)
                    alpine_added_count += 1
        except Exception as e:
            debug_print(f"Error reading Alpine mirror file: {e}")

    # Also try normal HTML parsing in case we have actual HTML content
    try:
        for country_header in soup.select('h4'):
            country_name_text = country_header.get_text(strip=True)
            country_name = country_name_text
            country_code = None

            img_tag = country_header.find('img', src=True)
            if img_tag and 'flags/' in img_tag['src']:
                try:
                    # e.g. /images/flags/us.png -> US
                    country_code = img_tag['src'].split('flags/')[-1].split('.')[0].upper()
                    # Clean up country name if flag was part of its text node
                    # This depends on precise HTML, assuming country name is main text of h4
                except IndexError:
                    pass # Could not parse country code from img src

            ul_tag = country_header.find_next_sibling('ul')
            if ul_tag:
                for li_tag in ul_tag.find_all('li'):
                    a_tag = li_tag.find('a', href=True)
                    if a_tag:
                        mirror_url = a_tag['href']
                        if mirror_url.startswith("http://") or mirror_url.startswith("https://") or mirror_url.startswith("rsync://"):
                            debug_print(f"Alpine: Found mirror URL in HTML: {mirror_url}")
                            metadata = {
                                'country': country_name,
                                'country_code': country_code.upper() if country_code else None
                                # Protocols will be inferred by add_mirror from the URL scheme
                            }
                            add_mirror(temp_mirror_groups, mirror_url, "alpine", metadata)
                            alpine_added_count += 1
    except Exception as e:
        debug_print(f"Error parsing Alpine HTML content: {e}")

    print(f"Alpine: Total mirrors passed to add_mirror: {alpine_added_count}")

def parse_debian_mirrors(temp_mirror_groups):
    debian_added_count = 0
    debug_print("Fetching and parsing Debian mirrors...")
    html_content = get_content_from_url_or_cache(DEBIAN_MIRRORS_URL, DEBIAN_CACHE_HTML, BASE_DIR)
    if not html_content:
        print("Failed to get Debian mirrors content.")
        return
    debug_print(f"Debian HTML content length: {len(html_content) if html_content else 0}")
    soup = BeautifulSoup(html_content, 'html.parser')

    country_table_heading = soup.find('h2', id='per-country')
    if not country_table_heading:
        debug_print("Debian: Could not find 'Debian Mirrors per Country' heading (h2 id='per-country').")
        debug_print(f"Debian: Total mirrors passed to add_mirror: {debian_added_count}")
        return

    mirror_table = country_table_heading.find_next_sibling('table')
    if not mirror_table:
        debug_print("Debian: Could not find table immediately following 'per-country' heading.")
        debug_print(f"Debian: Total mirrors passed to add_mirror: {debian_added_count}")
        return

    for row in mirror_table.find_all('tr'):
        cols = row.find_all('td')
        if len(cols) >= 2: # Need at least Country and Site columns
            country_name_text = cols[0].get_text(strip=True)
            site_anchor = cols[1].find('a', href=True)

            if site_anchor:
                href = site_anchor.get('href')
                if href and (href.startswith('http://') or href.startswith('https://')):
                    mirror_url_base = href.rstrip('/')
                    # Attempt to extract country code if present in the country name text, e.g. "Country (XX)"
                    # This is a guess; the page doesn't consistently provide codes here.
                    # The previous logic for country code extraction from cell_text is less applicable here.
                    country_code = None
                    # Example: match = re.search(r'\(([A-Z]{2})\)', country_name_text)
                    # if match: country_code = match.group(1)

                    metadata = {
                        'country': country_name_text,
                        'country_code': country_code # May be None
                    }
                    debug_print(f"Debian: Attempting to add mirror: {mirror_url_base}, Metadata: {metadata}")
                    add_mirror(temp_mirror_groups, mirror_url_base, "debian", metadata)
                    debian_added_count += 1

    # Secondary parsing for the "Complete List of Mirrors" section.
    # This section has a different structure: table with country headers in <tr><td><big><strong>Country</strong></big></td></tr>
    complete_list_heading = soup.find('h2', id='complete-list')
    if complete_list_heading:
        debug_print("Debian: Found 'Complete List of Mirrors' heading. Attempting to parse...")
        # Find the table that contains the complete list
        complete_list_table = complete_list_heading.find_next('table')
        if complete_list_table:
            current_country_name = None
            debug_print("Debian: Found Complete List table")
            # Process all rows in the table
            for row in complete_list_table.find_all('tr'):
                # Check if this is a country header row (contains <big><strong>Country</strong></big>)
                big_tag = row.find('big')
                if big_tag and big_tag.find('strong'):
                    country_name = big_tag.get_text(strip=True)
                    debug_print(f"Debian: Found country header: '{country_name}'")
                    current_country_name = country_name
                    continue

                # If we have a current country and this row has TD elements with links, it's likely a mirror entry
                if not current_country_name:
                    continue

                # Process a mirror entry row
                tds = row.find_all('td')
                if len(tds) < 2:  # Need at least the host name and URL columns
                    continue

                # Debug output for 163.com
                row_text = " | ".join(td.get_text(strip=True) for td in tds)
                if '163.com' in row_text:
                    debug_print(f"Debian (Complete List Row Debug for 163.com): Country '{current_country_name}', Row content: [{row_text}]")

                # First TD often contains hostname
                hostname = tds[0].get_text(strip=True)

                # Process all TDs looking for links
                for td_idx, td in enumerate(tds):
                    if '163.com' in td.get_text():
                        debug_print(f"Debian (Complete List Cell Debug for 163.com): TD {td_idx}: '{td.get_text(strip=True)}'")

                    # Find links in this TD
                    for anchor in td.find_all('a', href=True):
                        href = anchor.get('href')
                        if href and (href.startswith('http://') or href.startswith('https://')):
                            if '163.com' in href:
                                debug_print(f"Debian (Complete List 163.com Found): URL='{href}', Country='{current_country_name}'")

                            mirror_url_base = href.rstrip('/')
                            metadata = {
                                'country': current_country_name,
                                'country_code': None  # We could try to extract this from context if needed
                            }
                            debug_print(f"Debian (Complete List): Adding mirror: {mirror_url_base}, Metadata: {metadata}")
                            add_mirror(temp_mirror_groups, mirror_url_base, "debian", metadata)
                            debian_added_count += 1

    debug_print(f"Debian: Total mirrors passed to add_mirror: {debian_added_count}")

def parse_arch_mirrors(temp_mirror_groups):
    debug_print("Fetching and parsing Arch Linux mirrors from HTML mirrorlist...")
    content = get_content_from_url_or_cache(ARCH_HTML_MIRRORLIST_URL, ARCH_CACHE_TXT, BASE_DIR, is_json=False)
    if not content:
        print("Failed to get Arch Linux mirrorlist content.")
        return

    try:
        # The content is bytes, decode it to string
        mirror_list_text = content.decode('utf-8')
    except UnicodeDecodeError:
        debug_print("Failed to decode Arch Linux mirrorlist content as UTF-8.")
        return

    count = 0
    current_country = None

    for line in mirror_list_text.splitlines():
        stripped_line = line.strip()

        # Extract country from section headers (## Country)
        if stripped_line.startswith('## '):
            current_country = stripped_line[3:].strip()
            debug_print(f"Arch: Found country header: {current_country}")

        elif stripped_line.startswith('#Server = '):
            # Extract the URL part after '#Server = '
            mirror_url_template = stripped_line.split('=', 1)[1].strip()

            # The URL usually ends with /$repo/os/$arch or similar placeholders
            base_url = mirror_url_template.split('$repo/os/$arch')[0]

            # Further ensure it ends with a slash if it's a directory-like URL
            if not base_url.endswith('/'):
                if mirror_url_template.endswith('$repo/os/$arch'):
                    base_url += '/'

            if base_url:
                # Include country information in metadata if available
                metadata = {
                    'country': current_country,
                    'country_code': None,
                }

                debug_print(f"Arch: Adding mirror {base_url} with country {current_country}")
                add_mirror(temp_mirror_groups, base_url, "archlinux", metadata)
                count += 1

    print(f"Processed {count} Arch Linux mirrors from HTML mirrorlist.")

def parse_opensuse_mirrors(temp_mirror_groups):
    debug_print("Fetching and parsing openSUSE mirrors...")
    url = "https://mirrors.opensuse.org/"
    html_content = get_content_from_url_or_cache(url, "mirrors-opensuse.html", BASE_DIR)
    debug_print(f"openSUSE HTML content length: {len(html_content) if html_content else 0}")

    opensuse_added_count = 0  # Track mirrors added for diagnostic purposes

    if not html_content:
        print("Could not get openSUSE mirror list.")
        debug_print(f"openSUSE: Total mirrors passed to add_mirror: {opensuse_added_count}")
        return

    soup = BeautifulSoup(html_content, 'html.parser')

    # Looking for tables with mirror listings
    debug_print("openSUSE: Searching for mirror entries in HTML structure")

    # First pattern: Look for rows with country and URL information
    rows = soup.find_all('tr')
    for row in rows:
        # Check if this row contains the pattern we're looking for
        country_div = row.select_one('td div.country')
        hostname_div = row.select_one('td div.hostname')
        url_divs = row.select('td div.url')

        # If we found a country and some URLs, this is likely a mirror entry
        if country_div and (hostname_div or url_divs):
            country_code = country_div.get_text(strip=True)

            # Add debug output for tracking country codes
            if '.jp' in str(hostname_div) or '.au' in str(hostname_div) or 'netspace.net.au' in str(hostname_div) or 'kddilabs.jp' in str(hostname_div):
                debug_print(f"openSUSE: Found country code '{country_code}' for {hostname_div.get_text(strip=True) if hostname_div else 'unknown'}", "country")

            # Extract hostname if available
            hostname = None
            url_hostname = None
            if hostname_div:
                hostname_link = hostname_div.find('a')
                if hostname_link:
                    hostname = hostname_link.get_text(strip=True)
                    # Also extract the hostname from the href if available
                    href = hostname_link.get('href', '')
                    if href:
                        try:
                            url_hostname = urlparse(href).netloc
                            # Extra debug for 163.com
                            if '163.com' in href:
                                debug_print(f"openSUSE: Found 163.com in hostname href: {href}, extracted hostname: {url_hostname}")
                        except Exception as e:
                            debug_print(f"Error parsing URL: {e}")
                            pass

            # Debug output for 163.com
            if '163.com' in str(row):
                debug_print(f"openSUSE: Found row with 163.com, Country code: {country_code}, Hostname: {hostname}")
                debug_print(f"openSUSE: Row HTML: {row}")

            # Extract all URLs from this mirror entry
            for url_div in url_divs:
                url_link = url_div.find('a', href=True)
                if url_link and url_link.get('href'):
                    mirror_url = url_link['href'].rstrip('/')

                    # Only process URLs that start with http://, https://, or rsync://
                    if mirror_url.startswith('http://') or mirror_url.startswith('https://') or mirror_url.startswith('rsync://'):
                        # Extract hostname from the mirror URL for additional checks
                        try:
                            mirror_netloc = urlparse(mirror_url).netloc
                        except:
                            mirror_netloc = ""

                        # Make sure country_code is not None to prevent issues
                        if country_code is None:
                            country_code = ''

                        # Add extra debugging for Australian and Japanese domains
                        if '.au' in mirror_netloc or 'netspace.net.au' in mirror_netloc:
                            debug_print(f"openSUSE: Processing Australian URL: {mirror_url}, Country Code from HTML: {country_code}", "country")
                        elif '.jp' in mirror_netloc or 'kddilabs.jp' in mirror_netloc:
                            debug_print(f"openSUSE: Processing Japanese URL: {mirror_url}, Country Code from HTML: {country_code}", "country")

                        metadata = {
                            'country': None,  # We only have country code, not full name
                            'country_code': country_code.upper() if country_code else None
                        }

                        debug_print(f"openSUSE: Attempting to add mirror: {mirror_url}, Metadata: {metadata}")
                        add_mirror(temp_mirror_groups, mirror_url, "opensuse", metadata)
                        opensuse_added_count += 1

    # If we found no mirrors with the above pattern, try other patterns
    if opensuse_added_count == 0:
        debug_print("openSUSE: First parsing pattern found no mirrors. Trying alternative patterns...")

        # Look for any links with openSUSE-related URLs
        for link in soup.find_all('a', href=True):
            href = link.get('href')
            if ('opensuse' in href.lower() or 'suse' in href.lower()) and \
               (href.startswith('http://') or href.startswith('https://') or href.startswith('rsync://')):
                # Extract country code from domain TLD if possible
                parsed_url = urlparse(href)
                netloc = parsed_url.netloc.lower()

                # Default country code - don't set to None as this may cause issues later
                country_code = ''

                # Try to extract country code from domain TLD
                tld_match = re.search(r'\.([a-z]{2})$', netloc)
                if tld_match and tld_match.group(1) not in ['com', 'net', 'org', 'edu', 'gov', 'mil', 'int']:
                    # Extract country code from domain TLD (e.g., .jp, .de, .uk)
                    country_code = tld_match.group(1)
                    debug_print(f"openSUSE: Extracted country code '{country_code}' from domain: {netloc}")

                metadata = {'country': None, 'country_code': country_code}  # Set country code if we determined it

                mirror_url = href.rstrip('/')
                debug_print(f"openSUSE: (Fallback) Attempting to add mirror: {mirror_url}")
                add_mirror(temp_mirror_groups, mirror_url, "opensuse", metadata)
                opensuse_added_count += 1

    debug_print(f"openSUSE: Total mirrors passed to add_mirror: {opensuse_added_count}")

def parse_fedora_mirrors(temp_mirror_groups):
    debug_print(f"Parsing Fedora mirrors from {FEDORA_MIRRORS_PATH}")
    fedora_added_count = 0
    centos_added_count = 0
    rocky_added_count = 0

    # Try to read the file content directly first to check its size
    try:
        with open(FEDORA_MIRRORS_PATH, 'r', encoding='utf-8', errors='replace') as f:
            content = f.read()
            debug_print(f"Fedora content loaded, size: {len(content)} bytes")

            # If file is empty or very small, try fallback
            if len(content) < 100:  # Consider it empty or corrupted
                debug_print("Fedora file appears to be empty or corrupted.")
                return

            # If file has content, try to parse it with BeautifulSoup
            soup = BeautifulSoup(content, 'html.parser')
    except FileNotFoundError:
        debug_print(f"Fedora mirror file not found at {FEDORA_MIRRORS_PATH}. Skipping.")
        return
    except Exception as e:
        debug_print(f"Error loading Fedora HTML: {e}")
        return

    mirror_rows = soup.select('tr.mirror-row')
    debug_print(f"Found {len(mirror_rows)} mirror rows in Fedora HTML")

    for row in mirror_rows:
        actual_country_code = row.select_one('td:nth-of-type(1)')
        actual_country_code = actual_country_code.get_text(strip=True) if actual_country_code else None

        # Site name is in td:nth-of-type(2)
        site_name = row.select_one('td:nth-of-type(2)')
        site_name = site_name.get_text(strip=True) if site_name else "Unknown Site"

        # Location (City, State/Region) is in td:nth-of-type(3)
        country_name_detail = row.select_one('td:nth-of-type(3)')
        country_name_detail = country_name_detail.get_text(strip=True) if country_name_detail else actual_country_code

        bandwidth_text = row.select_one('td:nth-of-type(5)')
        bandwidth_text = bandwidth_text.get_text(strip=True) if bandwidth_text else None

        internet2_text = row.select_one('td:nth-of-type(6)')
        internet2_text = internet2_text.get_text(strip=True).lower() if internet2_text else 'no'
        internet2 = internet2_text == 'yes'

        categories_cell = row.select_one('td:nth-of-type(4)')
        if not categories_cell:
            continue

        # Process each list item that represents a category (like "Fedora Linux", "Fedora EPEL", etc.)
        for li in categories_cell.select('ul.list-unstyled > li'):
            # Extract the category name by getting text content before the first link
            full_text = li.get_text(strip=True)

            # Extract the category name - it's the text before any links
            category_name = ""
            for node in li.contents:
                if isinstance(node, str):
                    category_name += node.strip()
                elif node.name == "a":  # Stop when we hit the first link
                    break

            category_name = category_name.strip()
            if not category_name:
                # Try getting it by removing all link text from full text
                link_texts = [a.get_text(strip=True) for a in li.find_all('a')]
                for link_text in link_texts:
                    full_text = full_text.replace(link_text, '')
                category_name = full_text.strip()

            debug_print(f"Fedora: Found category: '{category_name}' from {site_name}")

            canonical_distro_name_for_config = None
            # Determine canonical_distro_name based on category text for config lookup
            if 'EPEL' in category_name.upper(): # Check EPEL first (case-insensitive)
                canonical_distro_name_for_config = "rocky"
            elif 'CentOS' in category_name: # Covers "CentOS Stream", "CentOS Linux"
                canonical_distro_name_for_config = "centos"
            elif 'Fedora' in category_name: # Covers "Fedora Linux", "Fedora Secondary"
                canonical_distro_name_for_config = "fedora"

            if canonical_distro_name_for_config: # If we identified a relevant distro type
                debug_print(f"Fedora: Processing {canonical_distro_name_for_config} links from {site_name}")
                # Get all links in this category
                for link_tag in li.find_all('a', href=True):
                    href = link_tag.get('href')
                    if not href:
                        continue

                    # Get the protocol from the link text or the URL itself
                    protocol_text = link_tag.get_text(strip=True).lower()
                    # If the link text is empty, try to get the protocol from the URL
                    if not protocol_text:
                        if href.startswith('https://'):
                            protocol_text = 'https'
                        elif href.startswith('http://'):
                            protocol_text = 'http'
                        elif href.startswith('rsync://'):
                            protocol_text = 'rsync'
                        elif href.startswith('ftp://'):
                            protocol_text = 'ftp'

                    debug_print(f"Fedora: Found {canonical_distro_name_for_config} link: {protocol_text} -> {href}")

                    if protocol_text in ['http', 'https', 'rsync', 'ftp']:
                        # Override country code for known Chinese domains regardless of what's in the HTML
                        corrected_country_code = actual_country_code.upper() if actual_country_code else None

                        # Convert bandwidth to numerical value in Mbps
                        bandwidth_mbps = parse_bandwidth(bandwidth_text)

                        metadata = {
                            # 'country': country_name_detail,  # Not setting country for Fedora mirrors per requirement
                            'country_code': corrected_country_code,
                            'internet2': internet2,
                            'bandwidth': bandwidth_mbps
                            # The protocol of the specific href will be processed by add_mirror
                        }

                        # Keep original bandwidth text in debug output
                        if bandwidth_mbps and bandwidth_text:
                            debug_print(f"Fedora: Converted bandwidth '{bandwidth_text}' to {bandwidth_mbps} Mbps")

                        # Log which mirror we're adding
                        debug_print(f"Fedora: Adding {canonical_distro_name_for_config} mirror: {href}")
                        # Pass the original href and the determined canonical_distro_name_for_config
                        # add_mirror will use canonical_distro_name_for_config to find the correct
                        # list of suffixes (e.g., DISTRO_CONFIGS['rocky']) for stripping.
                        add_mirror(temp_mirror_groups, href, canonical_distro_name_for_config, metadata)

                        # Track counts by distro type
                        if canonical_distro_name_for_config == "fedora":
                            fedora_added_count += 1
                        elif canonical_distro_name_for_config == "centos":
                            centos_added_count += 1
                        elif canonical_distro_name_for_config == "rocky":
                            rocky_added_count += 1

    print(f"Fedora: Total mirrors passed to add_mirror: Fedora={fedora_added_count}, CentOS={centos_added_count}, EPEL={rocky_added_count}")

def parse_ubuntu_mirrors(temp_mirror_groups):
    debug_print(f"Fetching and parsing Ubuntu mirrors from {UBUNTU_MIRRORS_URL}")
    ubuntu_added_count = 0

    # Try fetching from URL first, then fall back to local file if that fails
    content_bytes = get_content_from_url_or_cache(UBUNTU_MIRRORS_URL, "mirrors-ubuntu.html", BASE_DIR)
    if not content_bytes:
        debug_print(f"Failed to fetch Ubuntu mirrors from URL. Trying local file {UBUNTU_MIRRORS_PATH}")
        try:
            with open(UBUNTU_MIRRORS_PATH, 'r', encoding='utf-8', errors='replace') as f:
                content = f.read()
                debug_print(f"Ubuntu content loaded from local file, size: {len(content)} bytes")
        except FileNotFoundError:
            debug_print(f"Ubuntu mirror file not found at {UBUNTU_MIRRORS_PATH}.")
            content = None
        except Exception as e:
            debug_print(f"Error reading Ubuntu mirror file: {e}")
            content = None
    else:
        # Convert bytes to string if needed
        if isinstance(content_bytes, bytes):
            try:
                content = content_bytes.decode('utf-8')
            except UnicodeDecodeError:
                # Try another encoding if utf-8 fails
                try:
                    content = content_bytes.decode('latin-1')
                except Exception as e:
                    debug_print(f"Error decoding content: {e}")
                    content = None
        else:
            content = content_bytes
        debug_print(f"Successfully fetched Ubuntu mirrors from URL, size: {len(content) if content else 0} bytes")

    # If we still don't have content, use fallback mirrors
    if not content:
        debug_print("Ubuntu: No mirror content available.")
        return

    # First try parsing as JSON (original format from Ubuntu mirrors API)
    try:
        mirror_data = json.loads(content)
        debug_print(f"Successfully parsed Ubuntu data as JSON: {len(mirror_data)} entries")
        # Process JSON data below outside this try block
    except json.JSONDecodeError as e:
        debug_print(f"Error parsing Ubuntu mirror JSON: {e}")

        # If content is HTML, try parsing with BeautifulSoup
        # Safely check if the content contains HTML tags
        html_indicators = ["<html", "<body", "<!DOCTYPE", "<head"]
        is_likely_html = any(indicator in content for indicator in html_indicators) if isinstance(content, str) else False
        if is_likely_html:
            debug_print("Trying to parse Ubuntu data as HTML...")
            soup = BeautifulSoup(content, 'html.parser')

            # Attempt to extract country headings and organize mirrors by country
            links_found = False
            current_country = None
            country_code_map = {}

            current_country = None
            rows = soup.find_all('tr')
            debug_print(f"Ubuntu: Sequentially processing {len(rows)} table rows in HTML content")

            for row in rows:
                # If this row has a country header, update current_country
                th_cell = row.find('th', attrs={'colspan': '2'})
                if th_cell is not None:
                    country_name = th_cell.get_text(strip=True)
                    if country_name:
                        current_country = country_name
                        country_code_map[country_name] = {'name': country_name, 'code': None}
                        debug_print(f"Ubuntu: Switched current country to: {country_name}")
                    continue  # Move to next row after updating country

                # Process data rows only if we have a current country context
                if current_country is None:
                    continue  # Skip rows before the first country header

                cells = row.find_all('td')
                if len(cells) < 3:
                    continue  # Not enough cells to process mirror info

                links_cell = cells[1]
                mirror_links = links_cell.find_all('a', href=True)
                bandwidth_cell = cells[2] if len(cells) > 2 else None
                bandwidth_text = bandwidth_cell.get_text(strip=True) if bandwidth_cell else None

                if bandwidth_text and any(x in bandwidth_text.lower() for x in ['gbps', 'mbps', 'tb', 'gb', 'mb']):
                    debug_print(f"Ubuntu: Found row with bandwidth: {bandwidth_text}")

                for a_tag in mirror_links:
                    href = a_tag['href']
                    if href.startswith(('http://', 'https://', 'rsync://')) and ("/ubuntu" in href or ".ubuntu.com" in href or "archive.ubuntu.com" in href):
                        mirror_url = href.rstrip('/')
                        country_info = country_code_map.get(current_country, {'name': current_country, 'code': None})

                        debug_print(f"Ubuntu: Found mirror URL from HTML table: {mirror_url}, Bandwidth: {bandwidth_text}, Country: {country_info['name']}")
                        bandwidth_mbps = parse_bandwidth(bandwidth_text)
                        metadata = {
                            'country': country_info['name'],
                            'country_code': country_info['code'],
                            'bandwidth': bandwidth_mbps
                        }
                        if bandwidth_mbps and bandwidth_text:
                            debug_print(f"Ubuntu: Converted bandwidth '{bandwidth_text}' to {bandwidth_mbps} Mbps")
                        add_mirror(temp_mirror_groups, mirror_url, "ubuntu", metadata)
                        ubuntu_added_count += 1
                        links_found = True


            # If we found no mirrors in tables, fall back to finding any links in the HTML content
            if not links_found:
                print("Ubuntu: No mirrors found in table rows, trying to find individual links...")
                for a_tag in soup.find_all('a', href=True):
                    href = a_tag['href']
                    if href.startswith("http://") or href.startswith("https://"):
                        # Check if this looks like a Ubuntu mirror URL
                        if "/ubuntu" in href or ".ubuntu.com" in href or "archive.ubuntu.com" in href:
                            mirror_url = href.rstrip('/')
                            debug_print(f"Ubuntu: Found mirror URL from HTML: {mirror_url}")
                            metadata = {'country': None, 'country_code': None}
                            add_mirror(temp_mirror_groups, mirror_url, "ubuntu", metadata)
                            ubuntu_added_count += 1
                            links_found = True

            if links_found:
                debug_print(f"Ubuntu: Added {ubuntu_added_count} mirrors from HTML content")
                return

        # Try extracting just URLs from the text content
        if isinstance(content, str):  # Make sure content is a string before splitting
            lines = content.splitlines()
            debug_print(f"Trying to parse Ubuntu mirror file as text: {len(lines)} lines")
            for line in lines:
                line = line.strip()
                # Extract URLs that start with http:// or https://
                if line.startswith('http://') or line.startswith('https://'):
                    parts = line.split()
                    mirror_url = parts[0].strip() # Take just the first word in case there's text after URL
                    debug_print(f"Ubuntu: Found mirror URL from text: {mirror_url}")
                    metadata = {'country': None, 'country_code': None}
                    add_mirror(temp_mirror_groups, mirror_url, "ubuntu", metadata)
                    ubuntu_added_count += 1

        if ubuntu_added_count > 0:
            print(f"Ubuntu: Added {ubuntu_added_count} mirrors from text file")
            return

        # If we still haven't found any mirrors, use hardcoded fallback
        print("Ubuntu: No mirrors found in file.")
        return
    except FileNotFoundError:
        debug_print(f"Ubuntu mirror file not found at {UBUNTU_MIRRORS_PATH}. Skipping.")
        return
    except Exception as e:
        debug_print(f"Error reading Ubuntu mirror file: {e}")
        return

    # Process JSON data
    debug_print("Processing Ubuntu mirror JSON data:")
    for idx, mirror_entry in enumerate(mirror_data):
        # Check if mirror_entry has the required fields
        if 'url' not in mirror_entry:
            debug_print(f"Ubuntu: Entry {idx} missing URL, skipping")
            continue # Skip entries without required fields

        # Mirror URL may have trailing slash but should be normalized in add_mirror
        mirror_url = mirror_entry['url']

        # Get country code if available, otherwise use None
        country_code = mirror_entry.get('country_code', None)
        country_name = mirror_entry.get('country', None)  # May not be present

        # Display the mirror URL we're processing
        debug_print(f"Ubuntu: Processing mirror: {mirror_url}, Country: {country_code or 'Unknown'}")

        # Protocol field might be used elsewhere but here we infer it from the URL scheme
        # We'll extract any bandwidth data if available
        metadata = {
            'country': country_name,
            'country_code': country_code.upper() if country_code else None,
            # Other metadata fields from the JSON if available
            'speed': mirror_entry.get('speed', None),
            'official': mirror_entry.get('official', False)
        }

        if not mirror_url.startswith(('http://', 'https://', 'rsync://')):
            debug_print(f"Ubuntu: Skipping non-http/https/rsync URL: {mirror_url}")
            continue

        # Add to temporary grouping dictionary with 'ubuntu' as the distro prefix
        add_mirror(temp_mirror_groups, mirror_url, "ubuntu", metadata)
        ubuntu_added_count += 1

    print(f"Ubuntu: Total mirrors passed to add_mirror: {ubuntu_added_count}")

def parse_openeuler_mirrors(temp_mirror_groups):
    print(f"Parsing openEuler mirrors from local file: {OPENEULER_MIRRORS_PATH}")
    if not os.path.exists(OPENEULER_MIRRORS_PATH):
        print(f"openEuler mirror file not found: {OPENEULER_MIRRORS_PATH}")
        return

    try:
        with open(OPENEULER_MIRRORS_PATH, 'rb') as f:
            soup = BeautifulSoup(f, 'html.parser')
    except IOError as e:
        debug_print(f"Failed to read or parse {OPENEULER_MIRRORS_PATH}: {e}")
        return

    count = 0
    # The mirrors are in a table within a div with class 'o-table mirror-pc'
    # More specifically, within tbody of that table.
    mirror_table_div = soup.find('div', class_='o-table mirror-pc')
    if not mirror_table_div:
        debug_print(f"Could not find the main mirror table div ('o-table mirror-pc') in {OPENEULER_MIRRORS_PATH}.")
        return

    table = mirror_table_div.find('table')
    if not table:
        debug_print(f"Could not find table within 'o-table mirror-pc' div in {OPENEULER_MIRRORS_PATH}.")
        return

    tbody = table.find('tbody')
    if not tbody:
        debug_print(f"Could not find tbody in the mirror table in {OPENEULER_MIRRORS_PATH}.")
        return

    for row in tbody.find_all('tr'):
        cells = row.find_all('td')
        if len(cells) < 2: # Expecting at least Site and Location columns
            continue

        # First cell (index 0) for Site URL
        site_cell = cells[0]
        link_tag = site_cell.find('a', href=True)
        if not link_tag:
            continue

        mirror_url = link_tag['href'].rstrip('/')
        if not (mirror_url.startswith('http://') or mirror_url.startswith('https://') or mirror_url.startswith('rsync://')):
            continue # Skip if not a valid protocol

        # Second cell (index 1) for Location (Country)
        location_cell = cells[1]
        country_name = location_cell.get_text(strip=True)
        if not country_name:
            country_name = None # Ensure it's None if empty, not an empty string

        metadata = {
            'country': country_name,
            'country_code': None  # Country code is not available in this HTML table
        }
        add_mirror(temp_mirror_groups, mirror_url, "openeuler", metadata)
        count += 1

    if count > 0:
        print(f"Processed {count} openEuler mirrors from {OPENEULER_MIRRORS_PATH}.")
    else:
        print("No openEuler mirrors found or parsed from {OPENEULER_MIRRORS_PATH}. Check HTML structure and parsing logic.")

def main():
    print("Starting new mirror generation...")
    load_distro_configs(BASE_DIR)

    temp_mirror_groups = {} # Temporary dict for grouping by (netloc, common_path)

    # Parsers will now populate temp_mirror_groups
    parse_alpine_mirrors(temp_mirror_groups)
    parse_debian_mirrors(temp_mirror_groups)
    parse_arch_mirrors(temp_mirror_groups)
    parse_opensuse_mirrors(temp_mirror_groups)
    parse_fedora_mirrors(temp_mirror_groups)
    parse_ubuntu_mirrors(temp_mirror_groups)
    parse_openeuler_mirrors(temp_mirror_groups)

    debug_print("\n--- DEBUG: Checking temp_mirror_groups for all distros ---")
    distro_counts = {}
    for key_tuple, group_data_val in temp_mirror_groups.items():
        for distro in group_data_val.get('distro_dirs', set()):
            distro_counts[distro] = distro_counts.get(distro, 0) + 1

            # Print first 2 mirrors of each distro type as examples
            if distro_counts[distro] <= 2:
                debug_print(f"DEBUG_TEMP: Found {distro} in temp_mirror_groups: Key={key_tuple}")
                if '163.com' in key_tuple[0] or 'ustc.edu' in key_tuple[0]:
                    debug_print(f"DEBUG_TEMP: Notable mirror found: {key_tuple}, Data={group_data_val}")

    print("\nMirror counts by distro:")
    for distro, count in distro_counts.items():
        print(f"  {distro}: {count} mirrors")
    debug_print("")

    # Find rsync-only mirrors for debugging
    rsync_only_count = 0
    for key_tuple, group_data in temp_mirror_groups.items():
        protocols = group_data['protocols']
        if len(protocols) == 1 and 'rsync' in protocols:
            rsync_only_count += 1
            if rsync_only_count <= 3:  # Show a few examples
                debug_print(f"DEBUG_RSYNC_ONLY: {key_tuple} - Distros: {group_data['distros']}")
    print(f"Total: {rsync_only_count} rsync-only mirrors will be skipped\n")

    new_mirrors = {} # This will be the final dict for JSON output

    # Process aggregated groups to create the final new_mirrors structure
    for (netloc, common_path), group_data in temp_mirror_groups.items():
        # Skip mirrors that only support rsync protocol
        protocols = group_data['protocols']
        if len(protocols) == 1 and 'rsync' in protocols:
            debug_print(f"Skipping rsync-only mirror: {netloc}{common_path}")
            continue

        # Construct the final URL key. Ensure no trailing slash for root paths.
        # common_path will be '/' for root, or a path like '/sub/path/'
        # urlunparse handles joining correctly.
        final_url_key = urlunparse((group_data['representative_scheme'], netloc, common_path.rstrip('/'), '', '', ''))

        # If common_path was just '/', urlunparse might produce 'scheme://netloc/'
        # We want 'scheme://netloc' if path is effectively root.
        if common_path == '/':
            final_url_key = urlunparse((group_data['representative_scheme'], netloc, '', '', '', '')).rstrip('/')
        else:
            # For non-root paths, ensure they are correctly formed. urlunparse might add a trailing / if path is not empty.
            # We want to control this: rstrip('/') if it's not meant to be a directory root for the grouping.
            # However, common_path_for_grouping from add_mirror is usually '/' or an original path.
            # Let's ensure the final key is clean.
            final_url_key = final_url_key.rstrip('/')


        final_entry = {
            'distro_dirs': sorted(list(group_data['distro_dirs'])),
            'protocols': sorted(list(group_data['protocols'])),
            'original_urls': sorted(list(group_data['original_urls'])),
            'distros': sorted(list(group_data['distros']))  # Add distros list to output
            # Removed 'priority' field per requirement - will not auto-generate it
        }
        # Add merged metadata from metadata_store
        final_entry.update(group_data['metadata_store'])

        new_mirrors[final_url_key] = final_entry

    # The old post-processing loop for 'top_level' based distro_dirs formatting is removed,
    # as 'top_level' is no longer explicitly managed in this way. The 'distro_dirs' is always a list.
    # Sets were already converted to sorted lists above.

    debug_print("\n--- DEBUG: Checking final new_mirrors for all distros ---")
    final_distro_counts = {}
    for url_key_final, entry_data_final in new_mirrors.items():
        for distro in entry_data_final.get('distro_dirs', []):
            final_distro_counts[distro] = final_distro_counts.get(distro, 0) + 1

            # Print first mirror of each distro type as an example
            if final_distro_counts[distro] <= 1:
                debug_print(f"DEBUG_FINAL: Found {distro} in new_mirrors: Key={url_key_final}")
                if distro in ['fedora', 'ubuntu', 'alpine'] or '163.com' in url_key_final or 'ustc.edu' in url_key_final:
                    debug_print(f"DEBUG_FINAL: Notable mirror found: {url_key_final}, Data={entry_data_final}")

    print("\nFinal mirror counts by distro:")
    for distro, count in final_distro_counts.items():
        print(f"  {distro}: {count} entries")
    debug_print("")

    # Check for specific problematic mirrors in the final output
    for target_mirror in ['mirrors.163.com', 'mirrors.ustc.edu.cn']:
        found = False
        for url_key_final in new_mirrors.keys():
            if target_mirror in url_key_final:
                found = True
                debug_print(f"DEBUG_FINAL: {target_mirror} IS present in the final output: {url_key_final}")
                break
        if not found:
            debug_print(f"DEBUG_FINAL: WARNING! {target_mirror} is NOT present in the final output!")
    debug_print("")

    # Verify that the expected distros have at least some entries
    for expected_distro in ['debian', 'opensuse', 'fedora', 'ubuntu', 'alpine', 'archlinux']:
        if final_distro_counts.get(expected_distro, 0) == 0:
            debug_print(f"DEBUG_FINAL: WARNING! No {expected_distro} mirrors in final output!")

    print(f"Writing {len(new_mirrors)} new/updated mirrors to {NEW_MIRRORS_OUTPUT_PATH}")
    os.makedirs(os.path.dirname(NEW_MIRRORS_OUTPUT_PATH), exist_ok=True)
    with open(NEW_MIRRORS_OUTPUT_PATH, 'w') as f:
        json.dump(new_mirrors, f, indent=4, sort_keys=True) # sort_keys for consistent output

    print("New mirror generation complete.")

if __name__ == "__main__":
    main()
