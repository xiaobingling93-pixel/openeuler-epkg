# Tool Mirror Acceleration Feature

## Problem Statement

When users run `epkg install npm/pip/go/cargo` etc., the package download speed can be slow due to default mirrors being far from the user's location. This feature automatically injects mirror environment variables to accelerate downloads.

## Solution Overview

Automatically create wrapper scripts in `$env_root/usr/local/bin/` for common package managers that:
1. Read region-specific mirror config from `~/.config/epkg/tool/my_region/$tool.yaml`
2. Export mirror env vars if not already set
3. Execute the original tool binary

---

## Directory Structure

### Development (in repo)

```
/c/epkg/assets/tool/
├── env_vars/
│   ├── cn/                    # China region
│   │   ├── pip.yaml
│   │   ├── npm.yaml
│   │   ├── gem.yaml
│   │   ├── go.yaml
│   │   └── cargo.yaml
│   ├── eu/                    # Europe region
│   │   └── ...
│   └── us/                    # US region
│       └── ...
└── wrappers/
    ├── pip                    # Python3 script
    ├── npm                    # Node.js script
    ├── gem                    # Ruby script
    ├── shell-wrapper.sh       # Generic shell script
    ├── go -> shell-wrapper.sh # Symlink
    └── cargo -> shell-wrapper.sh
```

### Deployment (on user system)

```
# Created on 'epkg self install':
~/.config/epkg/tool/
├── env_vars  -> $EPKG_SRC/assets/tool/env_vars  (symlink)
└── my_region -> cn                              (symlink to region dir)

# Created in each environment:
$env_root/usr/local/bin/
├── pip           (wrapper, if pip installed & conditions met)
├── pip3          (wrapper, uses pip config)
├── npm           (wrapper)
├── gem           (wrapper)
├── go            (wrapper)
└── cargo         (wrapper)
```

---

## Config File Format

### YAML Format (simple key: value with comments)

Example `assets/tool/env_vars/cn/pip.yaml`:

```yaml
# pip mirror env vars for China
PIP_INDEX_URL: https://pypi.tuna.tsinghua.edu.cn/simple
PIP_TRUSTED_HOST: pypi.tuna.tsinghua.edu.cn
# config_file: ~/.pip/pip.conf
```

Example `assets/tool/env_vars/cn/go.yaml`:

```yaml
# Go module proxy env vars for China
GOPROXY: https://goproxy.cn,https://goproxy.io,direct
# config_file: (check $GOPROXY env var)
```

---

## Country to Region Mapping

The `country_to_region()` function maps country codes to regions:

| Country Code | Region |
|--------------|--------|
| CN | cn |
| US | us |
| AT, BE, BG, HR, CY, CZ, DE, DK, EE, ES, FI, FR, GB, GR, HU, IE, IT, LT, LU, LV, MT, NL, PL, PT, RO, SE, SI, SK | eu |
| JP, KR, AU, CA, NZ | us |

---

## Wrapper Scripts

### pip (Python3)

Uses Python's import mechanism to chain into the original pip:

```python
#!/usr/bin/env python3
import os
import sys

def load_mirror_env_vars(tool):
    config_path = os.path.expanduser(f"~/.config/epkg/tool/my_region/{tool}.yaml")
    if not os.path.exists(config_path):
        return
    # ... parse YAML and set env vars

def main():
    load_mirror_env_vars('pip')
    from pip._internal.cli.main import main as pip_main
    sys.exit(pip_main())

if __name__ == "__main__":
    main()
```

### Shell-based tools (go, cargo)

Use a shared `shell-wrapper.sh` script with symlinks:

```sh
#!/bin/sh
# The tool name is derived from basename($0)
_load_mirror_env_vars() {
    _config_file="${HOME}/.config/epkg/tool/my_region/${1}.yaml"
    # ... parse YAML and export env vars
}

_main() {
    _tool=$(basename "$0")
    case "$_tool" in pip3) _tool=pip ;; esac
    _load_mirror_env_vars "$_tool"
    exec "/usr/bin/$(basename "$0")" "$@"
}
```

---

## Skip Conditions

Wrapper is NOT created if:

| Condition | Action |
|-----------|--------|
| Env var already set | Skip wrapper (user has configured) |
| User config file exists | Skip wrapper (user has own config) |
| Wrapper already exists | Skip (already created) |
| Tool not in supported list | Skip |
| Region config file missing | Skip |

### User Config File Detection

| Tool | Config File Path |
|------|-----------------|
| pip | `~/.pip/pip.conf`, `~/.config/pip/pip.conf` |
| npm | `~/.npmrc` |
| gem | `~/.gemrc` |
| go | (check `$GOPROXY` env var) |
| cargo | `~/.cargo/config.toml`, `~/.cargo/config` |

---

## Implementation

### Rust Module: `src/tool_wrapper.rs`

Key functions:

- `setup_tool_config_symlinks()` - Called on `epkg self install`
- `setup_tool_wrappers(plan)` - Called after `link_packages()` in install
- `country_to_region(cc)` - Maps country code to region
- `should_create_wrapper(tool, env_root)` - Check skip conditions
- `create_tool_wrapper(tool, env_root)` - Create wrapper script

### Integration Points

1. **`src/init.rs`**: Call `setup_tool_config_symlinks()` after `create_environment(SELF_ENV)`
2. **`src/install.rs`**: Call `setup_tool_wrappers(plan)` after `link_packages(plan)`

---

## Testing

1. Fresh install: Install pip, verify wrapper created
2. Existing config: Create `~/.pip/pip.conf`, install pip, verify wrapper NOT created
3. Existing env var: Set `PIP_INDEX_URL`, install pip, verify wrapper NOT created
4. Region detection: Check `~/.config/epkg/tool/my_region` symlink points to correct region
