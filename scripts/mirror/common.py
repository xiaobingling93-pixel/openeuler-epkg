#!/usr/bin/env python3
import os
import yaml
import glob

# Global configurations loaded from YAML files
DISTRO_CONFIGS = {}

# Debug output function
def debug_print(message, category=None):
    """Print debug messages only if DEBUG environment variable is set"""
    if os.environ.get('DEBUG', '').lower() in ('true', '1', 'yes'):
        if category:
            print(f"DEBUG [{category}]: {message}")
        else:
            print(f"DEBUG: {message}")

def load_distro_configs(base_dir):
    """Loads distro-specific configurations from YAML files in the 'sources' directory."""
    global DISTRO_CONFIGS
    # sources_dir is where files like 'alpine.yaml', 'debian.yaml' are stored.
    # BASE_DIR is 'scripts/mirror/', so sources_dir is '../../sources/'.
    sources_dir = os.path.join(base_dir, '../..', 'sources')
    yaml_files = glob.glob(os.path.join(sources_dir, '*.yaml'))

    debug_print(f"Searching for distro configs in: {os.path.abspath(sources_dir)}")

    for yaml_file in yaml_files:
        filename = os.path.basename(yaml_file)
        # Derive distro_name from filename, e.g., 'alpine.yaml' -> 'alpine'
        distro_name = filename.replace('.yaml', '')

        # Avoid loading 'mirrors.yaml' or 'new-mirrors.yaml' (if it were yaml) as a distro config
        if distro_name in ['mirrors', 'new-mirrors', 'all-mirrors', 'example-config']:
            debug_print(f"Skipping non-distro config file: {filename}")
            continue

        try:
            with open(yaml_file, 'r', encoding='utf-8') as f:
                config = yaml.safe_load(f)
                # We expect 'distro_dirs' to be a list of strings.
                if isinstance(config, dict) and 'distro_dirs' in config and isinstance(config['distro_dirs'], list):
                    DISTRO_CONFIGS[distro_name] = config['distro_dirs']
                    debug_print(f"Loaded config for '{distro_name}': {DISTRO_CONFIGS[distro_name]}")
                else:
                    debug_print(f"Warning: '{filename}' is missing 'distro_dirs', has incorrect format, or is not a dictionary. Skipping.")
        except yaml.YAMLError as e:
            print(f"Error parsing YAML file '{filename}': {e}")
        except IOError as e:
            debug_print(f"Error reading file '{filename}': {e}")

    if not DISTRO_CONFIGS:
        print("Warning: No distro configurations were loaded. Directory filtering may not function as expected.")
    else:
        print(f"Successfully loaded {len(DISTRO_CONFIGS)} distro configurations.")

def get_distro_configs():
    """Get the loaded DISTRO_CONFIGS dictionary."""
    return DISTRO_CONFIGS
