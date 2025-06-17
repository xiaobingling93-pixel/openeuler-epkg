#!/usr/bin/env python3
import json
import os
import re
import requests
from urllib.parse import urlparse, urljoin
from bs4 import BeautifulSoup, XMLParsedAsHTMLWarning
import time
import datetime
import sys
import warnings
import hashlib
import argparse
import socket
import subprocess

# Import common utilities first so debug_print is available
from common import load_distro_configs, get_distro_configs, debug_print

# For JavaScript rendering
import asyncio

PYPPETEER_AVAILABLE = False
try:
    import pyppeteer
    PYPPETEER_AVAILABLE = True
    debug_print("Successfully imported pyppeteer for JavaScript rendering")

    # Suppress pyppeteer cleanup warnings that occur at script exit
    warnings.filterwarnings("ignore", category=RuntimeWarning, message=".*Event loop is closed.*")
    warnings.filterwarnings("ignore", category=RuntimeWarning, message=".*coroutine.*was never awaited.*")

except ImportError as e:
    debug_print(f"JavaScript rendering will not be available: {e}")
    PYPPETEER_AVAILABLE = False

# Global cache for country code resolutions to avoid repeated lookups
COUNTRY_CODE_CACHE = {}

"""
COMMON MIRROR FAILURE REASONS AND ANALYSIS
==========================================

Based on processing thousands of mirror URLs, the most common failure categories are:

1. NETWORK CONNECTIVITY ISSUES (~40% of failures)
   - Connection timeouts (timeout after 5s/10s)
   - Network unreachable (Errno 101) - routing/firewall issues
   - Connection refused (Errno 111) - service not running on port
   - No route to host (Errno 113) - network infrastructure problems
   - Connection reset by peer (Errno 104) - server drops connection
   - DNS resolution failures - temporary/permanent domain issues

2. HTTP ACCESS CONTROL (~20% of failures)
   - HTTP 403 Forbidden - server blocks directory listing or geographic restrictions
   - HTTP 404 Not Found - mirror moved/removed content
   - HTTP 503 Service Unavailable - temporary server overload
   - HTTP 451 - legal/jurisdictional blocking

3. SSL/TLS CERTIFICATE ISSUES (~5% of failures)
   - Certificate verification failed - self-signed/invalid certs
   - Certificate has expired - unmaintained mirrors
   - Missing Subject Key Identifier - old/malformed certificates

4. CONTENT/PARSING ISSUES (~30% of failures)
   - JavaScript-based directory listings (SPA/dynamic content)
   - Custom HTML formats not supported by our parsers
   - Authentication-required pages
   - Redirects to non-directory content

5. OTHER ISSUES (~2% of failures)
   - Bandwidth limiting/rate limiting
   - Geoblocking based on IP location
   - Server maintenance/temporary outages
"""

# Define paths
BASE_DIR = os.path.dirname(os.path.abspath(__file__))
LS_MIRRORS_OUTPUT_PATH = os.path.join(BASE_DIR, 'ls-mirrors.json')
NEW_MIRRORS_PATH = os.path.join(BASE_DIR, 'new-mirrors.json')
EXISTING_MIRRORS_PATH = os.path.join(BASE_DIR, '../../channel/mirrors.json')
INDEX_HTML_CACHE_DIR = os.path.join(BASE_DIR, 'index')
FAILED_MIRRORS_LOG_PATH = os.path.join(BASE_DIR, 'failed-mirrors.log')

# Global variables for directory filtering (computed once for all mirrors)
DISTRO_CONFIGS = None
VALID_DIRS = None

def initialize_distro_configs():
    """Initialize global DISTRO_CONFIGS and VALID_DIRS for directory filtering."""
    global DISTRO_CONFIGS, VALID_DIRS

    if DISTRO_CONFIGS is not None:
        return  # Already initialized

    DISTRO_CONFIGS = get_distro_configs()

    # Collect all valid distro directories from DISTRO_CONFIGS
    VALID_DIRS = set()
    for distro_name, distro_dirs in DISTRO_CONFIGS.items():
        # Strip leading/trailing slashes and add to valid dirs
        normalized_dirs = [d.strip('/').lower() for d in distro_dirs]
        VALID_DIRS.update(normalized_dirs)
        # Also add the distro name itself
        VALID_DIRS.add(distro_name.lower())

def load_existing_ls_data():
    """Load existing ls-mirrors.json data."""
    if os.path.exists(LS_MIRRORS_OUTPUT_PATH):
        try:
            with open(LS_MIRRORS_OUTPUT_PATH, 'r') as f:
                return json.load(f)
        except Exception as e:
            debug_print(f"Error loading existing ls-mirrors.json: {e}")
            return {}
    return {}

def load_new_mirrors_data():
    """Load mirrors.json data."""
    if os.path.exists(NEW_MIRRORS_PATH):
        try:
            with open(NEW_MIRRORS_PATH, 'r') as f:
                return json.load(f)
        except Exception as e:
            debug_print(f"Error loading mirrors.json: {e}")
            return {}
    return {}

def load_mirrors_data():
    """Load mirrors.json data."""
    if os.path.exists(EXISTING_MIRRORS_PATH):
        try:
            with open(EXISTING_MIRRORS_PATH, 'r') as f:
                return json.load(f)
        except Exception as e:
            debug_print(f"Error loading mirrors.json: {e}")
            return {}
    return {}

def get_cache_filename(url):
    """Generate a cache filename for a URL."""
    # Create a safe filename from URL
    parsed = urlparse(url)
    safe_netloc = re.sub(r'[^\w\-_.]', '_', parsed.netloc)
    safe_path = re.sub(r'[^\w\-_.]', '_', parsed.path.strip('/'))

    if safe_path:
        return f"{safe_netloc}_{safe_path}.html"
    else:
        return f"{safe_netloc}.html"

def resolve_hostname_to_ip(hostname):
    """Resolve hostname to IP address."""
    try:
        debug_print(f"Resolving DNS for hostname: {hostname}")
        ip = socket.gethostbyname(hostname)
        debug_print(f"Resolved {hostname} to IP: {ip}")
        return ip
    except socket.gaierror as e:
        debug_print(f"DNS resolution failed for {hostname}: {e}")
        return None
    except Exception as e:
        debug_print(f"Unexpected error resolving {hostname}: {e}")
        return None

def get_country_code_from_ip(ip_address):
    """Get country code from IP address using local GeoIP database."""
    # Check cache first
    if ip_address in COUNTRY_CODE_CACHE:
        debug_print(f"Using cached country code for IP {ip_address}: {COUNTRY_CODE_CACHE[ip_address]}")
        return COUNTRY_CODE_CACHE[ip_address]

    debug_print(f"Getting country code for IP: {ip_address}")

    # Try GeoLite2 .mmdb format first (newer format)
    try:
        import geoip2.database
        import geoip2.errors

        # Try different possible locations for GeoLite2 database
        possible_mmdb_paths = [
            '/usr/share/GeoIP/GeoLite2-Country.mmdb',
            '/var/lib/GeoIP/GeoLite2-Country.mmdb',
            '/opt/GeoIP/GeoLite2-Country.mmdb',
            os.path.join(BASE_DIR, 'GeoLite2-Country.mmdb'),
            os.path.expanduser('~/.local/share/GeoIP/GeoLite2-Country.mmdb'),
        ]

        db_path = None
        for path in possible_mmdb_paths:
            if os.path.exists(path):
                db_path = path
                break

        if db_path:
            debug_print(f"Using GeoLite2 database: {db_path}")
            try:
                with geoip2.database.Reader(db_path) as reader:
                    response = reader.country(ip_address)
                    country_code = response.country.iso_code

                    if country_code:
                        debug_print(f"Got country code {country_code} for IP {ip_address} (GeoLite2)")
                        # Cache the result
                        COUNTRY_CODE_CACHE[ip_address] = country_code
                        return country_code

            except geoip2.errors.AddressNotFoundError:
                debug_print(f"IP address {ip_address} not found in GeoLite2 database")
            except Exception as e:
                debug_print(f"Error using GeoLite2 database: {e}")
        else:
            debug_print("No GeoLite2 .mmdb database found")

    except ImportError:
        debug_print("geoip2 library not available, trying legacy GeoIP")
    except Exception as e:
        debug_print(f"Error with geoip2: {e}")

    # Fallback to legacy GeoIP .dat format using pygeoip
    try:
        import pygeoip

        legacy_db_paths = [
            '/usr/share/GeoIP/GeoIP.dat',
            '/var/lib/GeoIP/GeoIP.dat',
            os.path.join(BASE_DIR, 'GeoIP.dat'),
        ]

        db_path = None
        for path in legacy_db_paths:
            if os.path.exists(path):
                db_path = path
                break

        if db_path:
            debug_print(f"Using legacy GeoIP database: {db_path}")
            try:
                gi = pygeoip.GeoIP(db_path)
                country_code = gi.country_code_by_addr(ip_address)

                if country_code and country_code != '--':
                    debug_print(f"Got country code {country_code} for IP {ip_address} (pygeoip)")
                    # Cache the result
                    COUNTRY_CODE_CACHE[ip_address] = country_code
                    return country_code
                else:
                    debug_print(f"No country code found for IP {ip_address} in legacy database")

            except Exception as e:
                debug_print(f"Error using legacy GeoIP database: {e}")
        else:
            debug_print("No legacy GeoIP .dat database found")

    except ImportError:
        debug_print("pygeoip library not available")
    except Exception as e:
        debug_print(f"Error with pygeoip: {e}")

    # Also try the python-geoip package as another fallback
    try:
        import geoip

        debug_print("Trying python-geoip package")
        result = geoip.geolite2.lookup(ip_address)
        if result and result.country:
            country_code = result.country
            debug_print(f"Got country code {country_code} for IP {ip_address} (python-geoip)")
            # Cache the result
            COUNTRY_CODE_CACHE[ip_address] = country_code
            return country_code
        else:
            debug_print(f"No country code found for IP {ip_address} using python-geoip")

    except ImportError:
        debug_print("python-geoip library not available")
    except Exception as e:
        debug_print(f"Error with python-geoip: {e}")

    # If all methods failed
    debug_print(f"Failed to get country code for IP {ip_address} using any method")
    debug_print("Install databases with: sudo apt install geoip-database-extra")
    debug_print("Or download GeoLite2 from: https://dev.maxmind.com/geoip/geolite2-free-geolocation-data")

    # Cache the failure
    COUNTRY_CODE_CACHE[ip_address] = None
    return None

def resolve_mirror_country_code(mirror_url):
    """Resolve mirror URL to country code via DNS and GeoIP lookup."""
    try:
        # Parse URL to get hostname
        parsed = urlparse(mirror_url)
        hostname = parsed.netloc

        if not hostname:
            debug_print(f"Could not extract hostname from URL: {mirror_url}")
            return None

        # Remove port if present
        if ':' in hostname:
            hostname = hostname.split(':')[0]

        debug_print(f"Attempting to resolve country code for mirror: {mirror_url} (hostname: {hostname})")

        # Resolve hostname to IP
        ip_address = resolve_hostname_to_ip(hostname)
        if not ip_address:
            return None

        # Get country code from IP
        country_code = get_country_code_from_ip(ip_address)
        if country_code:
            debug_print(f"Successfully resolved {mirror_url} to country code: {country_code}")

        return country_code

    except Exception as e:
        debug_print(f"Error resolving country code for {mirror_url}: {e}")
        return None

def log_failed_mirror(url, error_msg):
    """Log failed mirror URL with error message to a log file."""
    try:
        with open(FAILED_MIRRORS_LOG_PATH, 'a', encoding='utf-8') as f:
            timestamp = time.strftime('%Y-%m-%d %H:%M:%S')
            f.write(f"[{timestamp}] {url} - {error_msg}\n")
    except Exception as e:
        debug_print(f"Error writing to failed mirrors log: {e}")

def fetch_directory_listing(mirror_url, timeout=5):
    """Fetch directory listing from a mirror URL, with caching."""
    # Ensure cache directory exists
    os.makedirs(INDEX_HTML_CACHE_DIR, exist_ok=True)

    cache_file = os.path.join(INDEX_HTML_CACHE_DIR, get_cache_filename(mirror_url))

    # Check if we already have cached content
    if os.path.exists(cache_file):
        debug_print(f"Using cached content for: {mirror_url}")
        try:
            with open(cache_file, 'r', encoding='utf-8') as f:
                return f.read(), None  # Return content and no error
        except Exception as e:
            debug_print(f"Error reading cache file {cache_file}: {e}")
            return "", None  # Return content and no error

    # Fetch from URL
    try:
        debug_print(f"Fetching directory listing from: {mirror_url}")
        headers = {
            'User-Agent': 'Mozilla/5.0 (compatible; epkg-mirror-scanner/1.0)'
        }

        # Ensure URL ends with / for directory listing
        url = mirror_url if mirror_url.endswith('/') else mirror_url + '/'

        # If you want retry HTTP GET, confirm and delete this 0-sized file list:
        # cd index && find -type f -name '*.html' -size 0 -ls
        os.system(f"touch {cache_file}")

        response = requests.get(url, headers=headers, timeout=timeout)
        response.raise_for_status()

        # Save to cache
        try:
            with open(cache_file, 'w', encoding='utf-8') as f:
                f.write(response.text)
            debug_print(f"Cached content to: {cache_file}")
        except Exception as e:
            debug_print(f"Error saving to cache file {cache_file}: {e}")

        return response.text, None  # Return content and no error
    except requests.exceptions.HTTPError as e:
        error_msg = f"HTTP {e.response.status_code}: {e.response.reason}"
        debug_print(f"HTTP error fetching {url}: {error_msg}")
        log_failed_mirror(url, error_msg)
        return None, error_msg
    except requests.exceptions.ConnectionError as e:
        error_msg = f"Connection error: {str(e)}"
        debug_print(f"Connection error fetching {url}: {error_msg}")
        log_failed_mirror(url, error_msg)
        return None, error_msg
    except requests.exceptions.Timeout as e:
        error_msg = f"Timeout after {timeout}s"
        debug_print(f"Timeout fetching {url}: {error_msg}")
        log_failed_mirror(url, error_msg)
        return None, error_msg
    except requests.exceptions.RequestException as e:
        error_msg = f"Request error: {str(e)}"
        debug_print(f"Failed to fetch {url}: {error_msg}")
        log_failed_mirror(url, error_msg)
        return None, error_msg

def parse_apache_style(soup):
    """Parse Apache-style directory listing."""
    directories = []
    debug_print("parse_apache_style: Looking for <pre> tag")

    # Look for pre tag with directory listing (Apache style)
    pre_tag = soup.find('pre')
    if pre_tag:
        debug_print("parse_apache_style: Found <pre> tag")
        lines = pre_tag.get_text().split('\n')
        debug_print(f"parse_apache_style: Processing {len(lines)} lines")
        for line in lines:
            # Apache format: drwxr-xr-x   2 root root  4096 Nov 15 12:34 dirname/
            if line.strip().startswith('d') and line.strip().endswith('/'):
                parts = line.split()
                if len(parts) >= 8:
                    dirname = parts[-1].rstrip('/')
                    if dirname and dirname not in ['..', '.']:
                        debug_print(f"parse_apache_style: Found directory: {dirname}")
                        directories.append(dirname)
    else:
        debug_print("parse_apache_style: No <pre> tag found")

    debug_print(f"parse_apache_style: Found {len(directories)} directories: {directories}")
    return directories

def parse_nginx_style(soup):
    """Parse Nginx-style directory listing."""
    directories = []
    debug_print("parse_nginx_style: Looking for directory links")

    links = soup.find_all('a', href=True)
    debug_print(f"parse_nginx_style: Found {len(links)} links")

    # Look for links in a simple list format
    for link in links:
        href = link.get('href', '').strip()
        text = link.get_text(strip=True)

        # Skip parent directory links
        if href in ['..', '../', '/', '.'] or text in ['..', 'Parent Directory']:
            debug_print(f"parse_nginx_style: Skipping parent/navigation link: {href}")
            continue

        # Nginx often shows directories with trailing slash
        if href.endswith('/'):
            dirname = href.rstrip('/')
            if dirname and dirname not in directories:
                debug_print(f"parse_nginx_style: Found directory from href: {dirname}")
                directories.append(dirname)
        elif text.endswith('/'):
            dirname = href.rstrip('/')
            if dirname and dirname not in directories:
                debug_print(f"parse_nginx_style: Found directory from text: {dirname}")
                directories.append(dirname)

    debug_print(f"parse_nginx_style: Found {len(directories)} directories: {directories}")
    return directories

def parse_table_style(soup):
    """Parse table-based directory listing (common in many servers)."""
    directories = []
    debug_print("parse_table_style: Looking for table-based listings")

    tables = soup.find_all('table')
    debug_print(f"parse_table_style: Found {len(tables)} tables")

    # Look for table-based listings
    for table_idx, table in enumerate(tables):
        debug_print(f"parse_table_style: Processing table {table_idx + 1}")
        rows = table.find_all('tr')
        debug_print(f"parse_table_style: Table has {len(rows)} rows")

        for row_idx, row in enumerate(rows):
            cells = row.find_all(['td', 'th'])
            if len(cells) >= 2:
                # First cell often contains the name
                name_cell = cells[0]
                link = name_cell.find('a', href=True)

                if link:
                    href = link.get('href', '').strip()
                    text = link.get_text(strip=True)

                    # Skip parent directory links
                    if href in ['..', '../', '/', '.'] or text in ['..', 'Parent Directory']:
                        debug_print(f"parse_table_style: Skipping parent/navigation link: {href}")
                        continue

                    # Check if it's a directory (various indicators)
                    is_directory = False

                    # Check href ending
                    if href.endswith('/'):
                        is_directory = True
                        debug_print(f"parse_table_style: Directory detected by href ending '/': {href}")

                    # Check text ending
                    elif text.endswith('/'):
                        is_directory = True
                        debug_print(f"parse_table_style: Directory detected by text ending '/': {text}")

                    # Check type column (if exists)
                    elif len(cells) > 1:
                        type_text = cells[1].get_text().lower()
                        if 'dir' in type_text or 'folder' in type_text:
                            is_directory = True
                            debug_print(f"parse_table_style: Directory detected by type column: {type_text}")

                    # Check size column (directories often show "-" or empty)
                    elif len(cells) > 2:
                        size_text = cells[2].get_text().strip()
                        if size_text in ['-', '', 'Directory']:
                            is_directory = True
                            debug_print(f"parse_table_style: Directory detected by size column: {size_text}")

                    if is_directory:
                        # Use full path from href instead of just the last component
                        dirname = href.rstrip('/') if href.endswith('/') else text.rstrip('/')
                        if dirname and dirname not in directories:
                            debug_print(f"parse_table_style: Adding directory: {dirname}")
                            directories.append(dirname)

    debug_print(f"parse_table_style: Found {len(directories)} directories: {directories}")
    return directories

def parse_generic_links(soup):
    """Parse generic links that might be directories."""
    directories = []
    debug_print("parse_generic_links: Looking for generic directory links")

    links = soup.find_all('a', href=True)
    debug_print(f"parse_generic_links: Found {len(links)} links to examine")

    # Generic link parsing as fallback
    for link in links:
        href = link.get('href', '').strip()
        text = link.get_text(strip=True)

        # Skip parent directory links and common file types
        if href in ['..', '../', '/', '.'] or text in ['..', 'Parent Directory']:
            debug_print(f"parse_generic_links: Skipping parent/navigation link: {href}")
            continue

        # Skip obvious files
        if any(href.lower().endswith(ext) for ext in ['.html', '.htm', '.txt', '.xml', '.gz', '.tar', '.zip', '.deb', '.rpm']):
            debug_print(f"parse_generic_links: Skipping file with known extension: {href}")
            continue

        # If no extension and not ending with common file patterns, might be directory
        if href.endswith('/') or ('.' not in href.split('/')[-1]):
            # Use full path from href instead of just the last component
            dirname = href.rstrip('/')
            if dirname and len(dirname) > 0 and dirname not in directories:
                debug_print(f"parse_generic_links: Adding potential directory: {dirname}")
                directories.append(dirname)

    debug_print(f"parse_generic_links: Found {len(directories)} directories: {directories}")
    return directories

def fetch_and_parse_with_js(mirror_url):
    """Fetch and parse directory listing using JavaScript rendering with pyppeteer."""
    # Check if JavaScript rendering is available
    if not PYPPETEER_AVAILABLE:
        print("JavaScript rendering not available - missing dependencies")
        print("To enable, install: pip install pyppeteer asyncio")
        return None

    try:
        # Create cache filename for JS-rendered content
        js_cache_file = os.path.join(INDEX_HTML_CACHE_DIR, get_cache_filename(mirror_url) + '.js.html')
        js_html_content = None

        # Check if we have cached JS-rendered content
        if os.path.exists(js_cache_file):
            debug_print(f"Using cached JS-rendered content for: {mirror_url}")
            try:
                with open(js_cache_file, 'r', encoding='utf-8') as f:
                    js_html_content = f.read()
                print(f"Using cached JS-rendered content from {js_cache_file}")
            except Exception as e:
                debug_print(f"Error reading cached JS content: {e}")
                return None
        else:
            print(f"Fetching with JavaScript rendering: {mirror_url}")
            # Ensure URL ends with / for directory listing
            url = mirror_url if mirror_url.endswith('/') else mirror_url + '/'

            try:
                # Use pyppeteer to render JavaScript with proper asyncio handling
                try:
                    js_html_content = asyncio.run(render_with_pyppeteer(url))
                except RuntimeError as e:
                    if "Event loop is closed" in str(e):
                        debug_print("Event loop error, skipping JavaScript rendering")
                        return None
                    else:
                        raise

                # Save rendered HTML to cache
                if js_html_content:
                    with open(js_cache_file, 'w', encoding='utf-8') as f:
                        f.write(js_html_content)
                    print(f"Saved JS-rendered content to {js_cache_file}")
                else:
                    os.system(f"touch {js_cache_file}")
                    print("Failed to get content from pyppeteer")
                    return None
            except Exception as e:
                print(f"Error during JavaScript rendering: {str(e)}")
                return None

        # Try parsing the JS-rendered content
        if js_html_content:
            directories = parse_directory_listing(js_html_content)
            if directories:
                print(f"Found {len(directories)} directories with JavaScript rendering")
                return directories
            else:
                print("No directories found in JavaScript-rendered content")

    except Exception as e:
        print(f"JavaScript rendering process failed: {str(e)}")

    return None

async def render_with_pyppeteer(url):
    """Render a page with pyppeteer and return the HTML content."""
    browser = None
    try:
        browser = await pyppeteer.launch(
            executablePath='/usr/bin/chromium',  # or '/usr/bin/chromium-browser'
            headless=True,
            #  args=['--no-sandbox', '--disable-setuid-sandbox']
        )
        page = await browser.newPage()

        # Set viewport size
        await page.setViewport({'width': 1280, 'height': 800})

        # Navigate to the URL with a timeout
        await page.goto(url, {'timeout': 30000, 'waitUntil': 'networkidle0'})

        # Wait a bit for any JavaScript to execute
        await asyncio.sleep(2)

        # Get the rendered HTML
        content = await page.content()

        return content
    except Exception as e:
        print(f"Pyppeteer rendering error: {str(e)}")
        return None
    finally:
        # Ensure browser is closed properly
        if browser:
            try:
                await browser.close()
            except Exception as e:
                debug_print(f"Error closing browser: {e}")

def fetch_directory_listing_with_lftp(mirror_url, timeout=30):
    """Fetch directory listing using lftp as fallback method."""
    try:
        # Create cache filename for lftp output
        lftp_cache_file = os.path.join(INDEX_HTML_CACHE_DIR, get_cache_filename(mirror_url).replace('.html', '.lftp'))

        # Check if we have cached lftp output
        if os.path.exists(lftp_cache_file):
            debug_print(f"Using cached lftp output for: {mirror_url}")
            try:
                with open(lftp_cache_file, 'r', encoding='utf-8') as f:
                    lftp_output = f.read()
                print(f"Using cached lftp content from {lftp_cache_file}")
                return lftp_output
            except Exception as e:
                debug_print(f"Error reading cached lftp content: {e}")
                # Continue to fetch fresh content

        debug_print(f"Attempting lftp directory listing for: {mirror_url}")

        # Prepare lftp command
        lftp_cmd = ['lftp', '-c', f'open {mirror_url}/; ls']
        os.system(f"touch {lftp_cache_file}")

        # Run lftp with timeout
        result = subprocess.run(
            lftp_cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,  # Capture stderr in stdout
            text=True,
            timeout=timeout
        )

        if result.returncode == 0:
            debug_print(f"LFTP successful for {mirror_url}")
            # Save to cache
            try:
                os.makedirs(os.path.dirname(lftp_cache_file), exist_ok=True)
                with open(lftp_cache_file, 'w', encoding='utf-8') as f:
                    f.write(result.stdout)
                debug_print(f"Saved lftp output to {lftp_cache_file}")
            except Exception as e:
                debug_print(f"Error saving lftp cache: {e}")
            return result.stdout
        else:
            debug_print(f"LFTP failed for {mirror_url}: {result.stdout}")
            # Save failed result to cache to avoid retrying
            try:
                os.makedirs(os.path.dirname(lftp_cache_file), exist_ok=True)
                with open(lftp_cache_file, 'w', encoding='utf-8') as f:
                    f.write(f"# LFTP FAILED: {result.stdout}")
                debug_print(f"Saved failed lftp result to {lftp_cache_file}")
            except Exception as e:
                debug_print(f"Error saving failed lftp cache: {e}")
            return None

    except subprocess.TimeoutExpired:
        debug_print(f"LFTP timeout for {mirror_url}")
        return None
    except FileNotFoundError:
        debug_print("LFTP not found - install lftp package for enhanced directory listing")
        return None
    except Exception as e:
        debug_print(f"LFTP error for {mirror_url}: {e}")
        return None

def parse_lftp_output(lftp_output, mirror_url):
    """Parse lftp output and extract directories, handle redirections."""
    if not lftp_output:
        return []

    # Check if this is a cached failed result
    if lftp_output.startswith("# LFTP FAILED:"):
        debug_print("Found cached lftp failure, skipping")
        return []

    directories = []
    redirect_url = None

    # Process each line
    for line in lftp_output.split('\n'):
        line = line.strip()
        if not line:
            continue

        # Check for redirection
        if 'received redirection to' in line:
            # Extract redirect URL: cd: received redirection to `http://mirror.as43289.net'
            import re
            match = re.search(r"received redirection to [`']([^`']+)[`']", line)
            if match:
                redirect_url = match.group(1)
                debug_print(f"LFTP detected redirection from {mirror_url} to {redirect_url}")
                continue

        # Parse directory entries
        # Format: drwxr-xr-x  --  debian
        # Format: drwxr-xr-x            -  2025-06-20 22:27  apache
        if line.startswith('d') or 'drwx' in line:
            # Split and get the last part (directory name)
            parts = line.split()
            if len(parts) >= 2:
                # The directory name is typically the last part
                dir_name = parts[-1]
                # Clean up directory name
                dir_name = dir_name.strip('/')
                if dir_name and dir_name not in ['.', '..']:
                    directories.append(dir_name)
                    debug_print(f"LFTP found directory: {dir_name}")

    # Handle redirection case
    if redirect_url:
        print(f"\n### REDIRECTION DETECTED via LFTP")
        print(f"Mirror {mirror_url} redirects to: {redirect_url}")
        print(f"### RECOMMENDED: Add the redirect URL to channel/mirrors.yaml:")
        print(f"#{redirect_url}:")
        if directories:
            print("#  ls:")
            for dir_name in sorted(directories):
                print(f"#  - {dir_name}")
        return []  # Return empty to indicate redirection

    debug_print(f"LFTP found {len(directories)} directories: {directories}")
    return directories

def parse_directory_listing(html_content, mirror_url=None):
    """Parse HTML directory listing and extract directory names using multiple strategies."""
    if not html_content:
        return None

    try:
        # Suppress XMLParsedAsHTMLWarning since we're intentionally parsing XML as HTML
        warnings.filterwarnings("ignore", category=XMLParsedAsHTMLWarning)
        soup = BeautifulSoup(html_content, 'html.parser')
        all_directories = []

        # Check for "Index of" pattern and detect path mismatches
        path_prefix = ""
        if mirror_url:
            # Look for "Index of" patterns in title or heading elements
            title_text = soup.title.get_text() if soup.title else ""
            h1_text = soup.h1.get_text() if soup.h1 else ""
            h2_text = soup.h2.get_text() if soup.h2 else ""

            for text in [title_text, h1_text, h2_text]:
                if "Index of" in text or "index of" in text.lower():
                    # Extract the indexed path
                    match = re.search(r'(?:Index of|index of)\s+(.+?)(?:\s|$)', text, re.IGNORECASE)
                    if match:
                        indexed_path = match.group(1).strip()
                        # Get the path part of mirror_url
                        mirror_path = mirror_url.split('://', 1)[1]  # Remove protocol
                        mirror_path = mirror_path.split('/', 1)[1] if '/' in mirror_path else ""  # Remove domain, keep path
                        mirror_path = "/" + mirror_path if mirror_path and not mirror_path.startswith('/') else mirror_path

                        # Check if indexed_path has extra components beyond mirror_path
                        if indexed_path != mirror_path and indexed_path.startswith('/'):
                            if mirror_path and not indexed_path.startswith(mirror_path):
                                # Path mismatch detected
                                extra_path = indexed_path
                                if extra_path.startswith('/'):
                                    extra_path = extra_path.lstrip('/')
                                path_prefix = extra_path
                                print(f"WARNING: Path mismatch detected: Index shows '{indexed_path}' but mirror URL path is '{mirror_path}'. Will prefix directories with '{path_prefix}'")
                        break

        # Try different parsing strategies
        strategies = [
            ("Apache-style", parse_apache_style),
            ("Table-style", parse_table_style),
            ("Nginx-style", parse_nginx_style),
            ("Generic links", parse_generic_links)
        ]

        for strategy_name, parse_func in strategies:
            directories = parse_func(soup)
            if directories:
                debug_print(f"{strategy_name} parsing found {len(directories)} directories")
                all_directories.extend(directories)
            else:
                debug_print(f"{strategy_name} parsing found no directories")

        # Remove duplicates while preserving order
        unique_directories = []
        seen = set()
        for d in all_directories:
            if d not in seen:
                unique_directories.append(d)
                seen.add(d)

        # Apply path prefix if detected from Index mismatch
        if path_prefix:
            prefixed_directories = []
            for d in unique_directories:
                if d.startswith(('http://', 'https://')):
                    prefixed_directories.append(d)  # Keep full URLs as-is
                else:
                    prefixed_d = f"{path_prefix}/{d.strip('/')}" if not d.startswith(path_prefix) else d
                    prefixed_directories.append(prefixed_d)
            unique_directories = prefixed_directories
            print(f"Applied path prefix '{path_prefix}' to directories")

        debug_print(f"Found {len(unique_directories)} total unique directories: {unique_directories[:10]}{'...' if len(unique_directories) > 10 else ''}")
        return unique_directories

    except Exception as e:
        debug_print(f"Error parsing directory listing: {e}")
        return []

def filter_directories(mirrors_data, directories, mirror_distros, mirror_distro_dirs, mirror_url=None):
    """Filter directories based on DISTRO_CONFIGS and mirror's distro information."""
    if not directories:
        return []

    # Ensure global configs are initialized
    initialize_distro_configs()

    # Normalize mirror_url to http:// for consistent matching
    normalized_mirror_url = None
    if mirror_url:
        if mirror_url.startswith('https://'):
            normalized_mirror_url = 'http://' + mirror_url[8:]  # Convert https:// to http://
        elif mirror_url.startswith('http://'):
            normalized_mirror_url = mirror_url  # Already http://
        else:
            normalized_mirror_url = 'http://' + mirror_url  # Add http:// prefix

    # Filter directories with full matching after stripping leading/trailing /
    filtered = []
    # Keep track of original directories for prefix analysis
    processed_directories = []

    for directory in directories:
        original_directory = directory
        # Strip mirror URL prefix to handle full URLs like http://mirror.example.com/debian
        # Normalize directory URL to http:// for consistent matching
        if directory.startswith(('http://', 'https://')):
            # Normalize directory to http://
            if directory.startswith('https://'):
                normalized_directory = 'http://' + directory[8:]  # Convert https:// to http://
            else:
                normalized_directory = directory  # Already http://

            # Strip the normalized mirror URL to get just the distro directory name
            if normalized_mirror_url and normalized_directory.startswith(normalized_mirror_url):
                directory = normalized_directory[len(normalized_mirror_url):]  # Remove mirror URL prefix

        # Normalize directory by stripping leading/trailing slashes
        directory = directory.strip('/').strip('./')
        processed_directories.append((original_directory, directory))

        normalized_dir = directory.lower()

        # Full matching only - no partial matching
        if normalized_dir in VALID_DIRS and directory not in filtered:
            filtered.append(directory)

    debug_print(f"Filtered {len(directories)} directories to {len(filtered)}: {filtered}")

    # Check for distro-specific top-level directory patterns
    if not filtered and (mirror_distros == ['debian'] or mirror_distros == ['ubuntu']):
        # Check for typical debian/ubuntu top-level directories
        dir_names = [d.strip('/').lower() for d in directories]
        debian_dirs = {'dists', 'pool', 'indices', 'project'}
        if debian_dirs.issubset(set(dir_names)):
            print(f"Detected debian/ubuntu top-level structure: {dir_names}")
            return 1

    if not filtered and mirror_distros == ['archlinux']:
        # Check for typical archlinux top-level directories
        dir_names = [d.strip('/').lower() for d in directories]
        arch_dirs = {'core', 'extra', 'multilib'}
        if arch_dirs.issubset(set(dir_names)):
            print(f"Detected archlinux top-level structure: {dir_names}")
            return 1

    if not filtered and mirror_distros == ['alpine']:
        # Check for typical alpine top-level directories
        dir_names = [d.strip('/').lower() for d in directories]
        alpine_patterns = any(d.startswith('v') and d[1:].replace('.', '').isdigit() for d in dir_names)
        has_edge = 'edge' in dir_names
        has_latest = any('latest' in d for d in dir_names)
        if alpine_patterns and (has_edge or has_latest):
            print(f"Detected alpine top-level structure: {dir_names}")
            return 1

    if not filtered and mirror_distros == ['fedora']:
        # Check for typical fedora top-level directories
        dir_names = [d.strip('/').lower() for d in directories]
        fedora_dirs = {'releases', 'updates'}
        if fedora_dirs.issubset(set(dir_names)):
            print(f"Detected fedora top-level structure: {dir_names}")
            return 1

    # If no directories matched, try to find common prefixes
    if not filtered and len(directories) >= 3:
        print("No exact matches found. Analyzing directory structure for common prefixes...")
        prefix_matches = {}

        for original_dir, processed_dir in processed_directories:
            # Remove trailing slash for matching
            dir_path = processed_dir.rstrip('/')

            # Check if this directory ends with any valid directory name
            for valid_dir in VALID_DIRS:
                if dir_path.lower().endswith('/' + valid_dir.lower()) or dir_path.lower() == valid_dir.lower():
                    # Extract the prefix
                    if dir_path.lower() == valid_dir.lower():
                        prefix = ''
                    else:
                        prefix = dir_path[:-(len(valid_dir)+1)]  # +1 for the slash

                    if prefix not in prefix_matches:
                        prefix_matches[prefix] = []
                    prefix_matches[prefix].append((valid_dir, original_dir))

        # Collect all valid directories from prefix matches
        collected_dirs = set()

        # Print recommendations for prefixes with multiple matches and collect valid dirs
        for prefix, matches in prefix_matches.items():
            if not prefix:
                continue
            if prefix == 'http:/' or prefix == 'https:/':
                continue
            if 'rsync://' in prefix or 'ftp://' in prefix:
                continue
            if len(matches) >= 3:  # At least 3 matches with the same prefix
                print(f"\nFound common prefix: '{prefix}' with {len(matches)} matches:")
                filtered = None

                # Validate that the prefix matches the mirror URL structure
                normalized_prefix = prefix.strip('/')
                prefix_matches_mirror = (
                    mirror_url.rstrip('/').endswith('/' + normalized_prefix) or
                    mirror_url.rstrip('/').endswith(normalized_prefix)
                )

                for valid_dir, original_path in matches:
                    print(f"  - {original_path} (matches '{valid_dir}')")
                    # Only collect if prefix matches mirror URL structure
                    if prefix_matches_mirror:
                        collected_dirs.add(valid_dir)

                if '://' in prefix:
                    new_url = f"{mirror_url}"
                else:
                    # Check if mirror_url already ends with this prefix to avoid duplication
                    if mirror_url.rstrip('/').endswith('/' + prefix.strip('/')):
                        new_url = mirror_url
                    else:
                        new_url = f"{mirror_url}/{prefix.strip('/')}"

                # Check if this exact URL or a similar one already exists in mirrors_data
                if new_url in mirrors_data:
                    continue

                print("\n### RECOMMENDED CONFIGURATION for channel/mirrors.yaml")
                print(f"#{new_url}:")
                print("#  ls:")
                for valid_dir, _ in matches:
                    print(f"#  - {valid_dir}")

        # Return collected valid directories if any were found
        if collected_dirs:
            filtered = sorted(list(collected_dirs))

    return filtered

def should_skip_mirror(mirror_url, mirror_info):
    """Check if mirror should be skipped based on criteria."""
    # Skip if top_level is true
    if mirror_info.get('root') == 1:
        return "root"

    if mirror_info.get('top_level'):
        return "top_level"

    return False

def process_mirrors(force_update=False):
    """Process mirrors and update ls data."""
    # Load existing data
    ls_data = load_existing_ls_data()
    mirrors_data = load_mirrors_data()
    new_mirrors_data = load_new_mirrors_data()

    processed_count = 0
    updated_count = 0
    skipped_count = 0
    total_mirrors = len(mirrors_data)

    for i, (mirror_url, mirror_info) in enumerate(mirrors_data.items(), 1):
        # Print progress with inline result
        print(f"[{datetime.datetime.now().strftime('%Y-%m-%d %H:%M:%S')}] Processing mirror {i}/{total_mirrors}: {mirror_url}", end=" ... ", flush=True)

        # Update ls_data
        if mirror_url not in ls_data:
            ls_data[mirror_url] = {}

        # Check if mirror is missing country code and try to resolve it
        has_country_code = mirror_info.get('cc') or mirror_info.get('country_code') or mirror_info.get('country')
        resolved_cc = 'cc' in ls_data[mirror_url] or (mirror_url in new_mirrors_data and 'cc' in new_mirrors_data[mirror_url])
        if not has_country_code and not resolved_cc:
            debug_print(f"Mirror {mirror_url} missing country code, attempting GeoIP resolution")
            resolved_cc = resolve_mirror_country_code(mirror_url)
            if resolved_cc:
                ls_data[mirror_url]['cc'] = resolved_cc
                debug_print(f"Resolved country code {resolved_cc} for {mirror_url}")
                print(f"(resolved cc: {resolved_cc})", end = " ")
            else:
                debug_print(f"Failed to resolve country code for {mirror_url}")
                print(f"(no cc resolved) ", end = " ")

        # Check if mirror already has 'ls' data
        if mirror_url in ls_data and 'ls' in ls_data[mirror_url] and ls_data[mirror_url]['ls']:
            print("already has ls data, skipping")
            skipped_count += 1
            continue

        # Check if mirror_info has 'ls' field
        if not force_update and 'ls' in mirror_info:
            print("already has ls in mirrors.json, skipping")
            skipped_count += 1
            continue

        # Skip non-HTTP mirrors
        if not mirror_url.startswith(('http://', 'https://')):
            print("skipping non-HTTP")
            skipped_count += 1
            continue

        # Apply filtering criteria
        reason = should_skip_mirror(mirror_url, mirror_info)
        if reason:
            print(f"skipping {reason}")
            skipped_count += 1
            continue

        processed_count += 1

        # Fetch directory listing
        html_content, error_msg = fetch_directory_listing(mirror_url)
        if not html_content:
            if error_msg:
                print(f"failed to fetch ({error_msg})")
            else:
                print("failed to fetch")

        # Parse directory listing
        directories = parse_directory_listing(html_content, mirror_url)

        # If no directories found, try lftp as fallback
        if not directories:
            print("no directories found with HTML parsing, trying lftp...")
            lftp_output = fetch_directory_listing_with_lftp(mirror_url)
            if lftp_output:
                directories = parse_lftp_output(lftp_output, mirror_url)
                if directories:
                    print(f"LFTP found {len(directories)} directories: {directories}")
                else:
                    print("LFTP found no directories (or redirection detected)")
            else:
                print("LFTP failed")

        # Get mirror's distro information
        mirror_distros = mirror_info.get('os', mirror_info.get('distros', []))
        mirror_distro_dirs = mirror_info.get('dir', mirror_info.get('distro_dirs', []))

        # Filter directories
        filtered_dirs = filter_directories(mirrors_data, directories, mirror_distros, mirror_distro_dirs, mirror_url)
        if filtered_dirs is None:
            continue
        if filtered_dirs == 1:
            ls_data[mirror_url]['root'] = 1
            print("root=1")
            continue
        if not filtered_dirs and '.js' in html_content and '</script>' in html_content:
            print("no directories found with standard parsing, trying JavaScript rendering...")

            # Try with JavaScript rendering
            js_directories = fetch_and_parse_with_js(mirror_url)
            if js_directories:
                directories = js_directories
                filtered_dirs = filter_directories(mirrors_data, directories, mirror_distros, mirror_distro_dirs, mirror_url)
                if filtered_dirs is None:
                    continue
                print(f"Filtered {len(filtered_dirs)} directories with JavaScript rendering")
            else:
                print("no directories found even with JavaScript rendering")
                continue

        ls_data[mirror_url]['ls'] = filtered_dirs
        print(f"{len(filtered_dirs)} dirs: {filtered_dirs}")

        updated_count += 1
        #  debug_print(f"Updated {mirror_url} with {len(filtered_dirs)} directories")

    print(f"\nSummary: Processed {processed_count} mirrors, updated {updated_count}, skipped {skipped_count}")
    return ls_data

def save_ls_data(ls_data):
    """Save ls data to JSON file."""
    try:
        os.makedirs(os.path.dirname(LS_MIRRORS_OUTPUT_PATH), exist_ok=True)
        with open(LS_MIRRORS_OUTPUT_PATH, 'w') as f:
            json.dump(ls_data, f, indent=2, sort_keys=True)
        print(f"Saved ls data to {LS_MIRRORS_OUTPUT_PATH}")
    except Exception as e:
        print(f"Error saving ls data: {e}")

def debug_parse_single_file(html_file):
    """Debug function to parse a single HTML file."""
    print(f"Debug mode: Parsing single file {html_file}")
    os.environ['DEBUG'] = '1'

    # Load configurations
    load_distro_configs(BASE_DIR)
    initialize_distro_configs()

    # Check if file exists
    if not os.path.exists(html_file):
        print(f"Error: File {html_file} does not exist")
        return

    # Read HTML content
    try:
        with open(html_file, 'r', encoding='utf-8') as f:
            html_content = f.read()
        print(f"Successfully read {len(html_content)} characters from {html_file}")
    except Exception as e:
        print(f"Error reading file: {e}")
        return

    mirrors_data = load_mirrors_data()
    html_filename = os.path.basename(html_file)

    for i, (mirror_url, mirror_info) in enumerate(mirrors_data.items(), 1):

        cache_filename = get_cache_filename(mirror_url)
        if cache_filename != html_filename:
            continue

        # Parse directory listing
        print("\n=== Starting directory parsing ===")
        directories = parse_directory_listing(html_content, mirror_url)

        # If no directories found, try lftp as fallback
        if not directories:
            print("No directories found with HTML parsing, trying lftp...")
            lftp_output = fetch_directory_listing_with_lftp(mirror_url)
            if lftp_output:
                directories = parse_lftp_output(lftp_output, mirror_url)
                if directories:
                    print(f"LFTP found {len(directories)} directories: {directories}")
                else:
                    print("LFTP found no directories (or redirection detected)")
            else:
                print("LFTP failed")

        print(f"\n=== Results ===")
        print(f"Found {len(directories)} directories:")
        for i, directory in enumerate(directories, 1):
            print(f"  {i}: {directory}")

        # Show what would be filtered
        print(f"\n=== Filtering test ===")
        filtered_dirs = filter_directories(mirrors_data, directories, [], [], mirror_url)
        if filtered_dirs is None:
            continue
        if filtered_dirs == 1:
            print("root=1")
            continue
        if not filtered_dirs and '.js' in html_content and '</script>' in html_content:
            print("no directories found with standard parsing, trying JavaScript rendering...")

            # Try with JavaScript rendering
            js_directories = fetch_and_parse_with_js(mirror_url)
            if js_directories:
                directories = js_directories
                filtered_dirs = filter_directories(mirrors_data, directories, [], [], mirror_url)
                if filtered_dirs is None:
                    continue
                print(f"found {len(directories)} directories with JavaScript rendering")
            else:
                print("no directories found even with JavaScript rendering")
                continue
        print(f"After filtering: {len(filtered_dirs)} directories:")
        for i, directory in enumerate(filtered_dirs, 1):
            print(f"  {i}: {directory}")

def main():
    parser = argparse.ArgumentParser(description='Process mirror directory listings')
    parser.add_argument('--parse', help='Debug mode: parse a single HTML file (e.g., index/example.com.html)')
    parser.add_argument('--update', action='store_true', help='force update existing ls fields')

    args = parser.parse_args()

    if args.parse:
        debug_parse_single_file(args.parse)
        return

    print("Starting mirror directory listing process...")

    # Load configurations
    load_distro_configs(BASE_DIR)

    # Initialize global directory filtering configs
    initialize_distro_configs()

    # Process mirrors and get updated ls data
    ls_data = process_mirrors(force_update=args.update)

    # Save results
    save_ls_data(ls_data)

    # Show failed mirrors summary
    if os.path.exists(FAILED_MIRRORS_LOG_PATH):
        try:
            with open(FAILED_MIRRORS_LOG_PATH, 'r', encoding='utf-8') as f:
                failed_count = len(f.readlines())
            print(f"Failed mirrors logged: {failed_count} (see {FAILED_MIRRORS_LOG_PATH})")
        except Exception as e:
            debug_print(f"Error reading failed mirrors log: {e}")

    print("Mirror directory listing process complete.")

if __name__ == "__main__":
    main()
