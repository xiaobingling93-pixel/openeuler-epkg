---
name: epkg-usage
description: epkg package manager concepts, use cases, features, commands, environment management, package install/query/run
---

# epkg Package Manager Skill Document

> This document helps AI Agents quickly understand and master the core concepts, use cases, and operations of the epkg package manager.

---

## 1. Core Concepts

### 1.1 What is epkg?

epkg is a **lightweight, multi-source Linux package manager** with key features:

- **No root required**: User-space installation, works for regular users
- **Multi-distro support**: Supports RPM, DEB, Alpine, Arch, Conda formats simultaneously
- **Environment isolation**: Each environment is independent, can mix packages from different distros
- **Efficient storage**: File-level deduplication, parallel downloads, content-addressed storage
- **Sandbox execution**: Supports namespace isolation and virtual machine isolation

### 1.2 Core Concept Model

```
┌─────────────────────────────────────────────────────────────┐
│                        Host System                           │
│  (Any Linux distro: openEuler, Debian, Fedora, Arch...)     │
├─────────────────────────────────────────────────────────────┤
│  ~/.epkg/envs/                                               │
│  ├── main/          ← Default environment (auto-created)     │
│  │   ├── ebin/      ← Exposed binary entry points (PATH)     │
│  │   ├── usr/       ← Software files (symlink → store)       │
│  │   └── etc/       ← Configuration files                    │
│  ├── alpine/        ← Alpine environment (independent)       │
│  ├── debian/        ← Debian environment (independent)       │
│  └── fedora/        ← Fedora environment (independent)       │
├─────────────────────────────────────────────────────────────┤
│  ~/.epkg/store/     ← Content-addressed storage (shared)     │
│  └── <hash>__<name>__<version>__<arch>/                     │
│      └── fs/        ← Extracted package files                │
└─────────────────────────────────────────────────────────────┘
```

### 1.3 Key Terminology

| Term | Meaning |
|------|---------|
| **Environment** | Independent software installation context with its own channel and installed package list |
| **Channel** | Software source configuration, e.g. `debian`, `alpine`, `fedora`, `conda` |
| **Store** | Content-addressed storage where all packages are extracted; environments reference via symlink |
| **ebin** | Environment's binary entry directory, added to PATH |
| **Register** | Persistently add environment's ebin to PATH |
| **Activate** | Temporarily activate an environment for current shell |
| **Generation** | Historical version of environment, supports rollback |

---

## 2. Use Cases

### 2.1 End User Scenarios

| Scenario | Traditional Pain Point | epkg Solution |
|----------|------------------------|---------------|
| Install extra software | Requires root, old system packages | No root needed, multiple distro sources available |
| Use newer software versions | System repos lag behind | Choose rolling-release sources like Fedora/Arch |
| Mix different distro software | Dependency conflicts, can't coexist | Multi-environment isolation, PATH combination |
| System rollback | Requires system-level snapshots | Each install records generation, one-click rollback |

### 2.2 Developer Scenarios

| Scenario | Traditional Pain Point | epkg Solution |
|----------|------------------------|---------------|
| Project dependency management | Pollutes system environment | Project directory `.eenv` isolated environment |
| Multi-language development | pip/npm global pollution | Per-environment independent toolchain |
| Cross-distro testing | Needs multiple machines or containers | One-click create Debian/Fedora/Alpine environments |
| CI/CD environments | Large images, slow builds | Static binary, deduplicated storage |

### 2.3 Container/Embedded Scenarios

| Scenario | Traditional Pain Point | epkg Solution |
|----------|------------------------|---------------|
| Image size | RPM/DEB metadata bloated | Slim storage, optional busybox applets |
| Base image | Must choose specific distro | Can mix multi-distro packages |
| Security isolation | Container escape risk | Supports VM-level sandbox |

---

## 3. Feature Overview

### 3.1 Command Groups Quick Reference

```
epkg <command>

┌─ Self Management ─────────────────────────────────────────┐
│  self install    Install epkg itself                      │
│  self upgrade    Upgrade epkg                             │
│  self remove     Remove epkg                              │
├─ Package Operations ──────────────────────────────────────┤
│  install         Install packages                         │
│  update          Update repository metadata               │
│  upgrade         Upgrade packages                         │
│  remove          Remove packages                          │
│  list            List packages                            │
│  info            Show package info                        │
│  search          Search packages                          │
├─ Environment Management ──────────────────────────────────┤
│  env list        List all environments                    │
│  env create      Create environment                       │
│  env remove      Remove environment                       │
│  env register    Register environment to PATH (persistent)│
│  env unregister  Unregister environment                   │
│  env activate    Activate environment (current shell)     │
│  env deactivate  Deactivate environment                   │
│  env path        Show PATH                                │
│  env config      Environment configuration                │
│  env export      Export environment config                │
├─ Execution & Sandbox ─────────────────────────────────────┤
│  run             Run command in environment               │
│  service         Service management                       │
│  busybox         Built-in command implementations         │
├─ History & Rollback ──────────────────────────────────────┤
│  history         View environment history                 │
│  restore         Rollback to specified version            │
│  gc              Garbage collection                       │
└───────────────────────────────────────────────────────────┘
```

### 3.2 Global Options

```bash
epkg [OPTIONS] <command>

Environment Selection:
  -e, --env <NAME>      Select environment by name
  -r, --root <DIR>      Select environment by path
  --arch <ARCH>         Specify architecture (x86_64, aarch64, riscv64)

Execution Control:
  --dry-run             Simulate run, don't modify system
  --download-only       Download only, don't install
  -y, --assume-yes      Auto-confirm
  -q, --quiet           Quiet mode
  -v, --verbose         Verbose output

Network:
  --proxy <URL>         HTTP proxy
  --parallel-download N Parallel download threads
```

### 3.3 Supported Distributions (Channel)

| Format | Distributions |
|--------|---------------|
| **RPM** | openEuler, Fedora, CentOS, AlmaLinux, Rocky, EPEL |
| **DEB** | Debian, Ubuntu, Linux Mint, Deepin |
| **APK** | Alpine (main, community) |
| **Pacman** | Arch Linux (core, extra, multilib, AUR) |
| **Conda** | conda-forge, main, free |

View complete list: `epkg repo list`

---

## 4. Typical Workflows

### 4.1 Quick Start Workflow

```bash
# 1. Install epkg
wget https://raw.atomgit.com/openeuler/epkg/raw/master/bin/epkg-installer.sh
bash epkg-installer.sh
bash  # Start a new shell

# 2. Create environment and install software
epkg env create myalpine -c alpine
epkg -e myalpine install bash htop

# 3. Run commands
epkg -e myalpine run htop

# 4. Register environment (persistent PATH)
epkg env register myalpine
htop --version  # Directly available
```

### 4.2 Multi-Environment Mixing

```bash
# Create different distro environments
epkg env create debian-env -c debian
epkg env create fedora-env -c fedora
epkg env create alpine-env -c alpine

# Install software in each environment
epkg -e debian-env install python3
epkg -e fedora-env install rustc
epkg -e alpine-env install nodejs

# Register to PATH (by priority)
epkg env register alpine-env --path-order 10   # Highest priority
epkg env register fedora-env  --path-order 20
epkg env register debian-env  --path-order 30

# Now all environment binaries are available in PATH
python3 --version   # From debian-env
rustc --version     # From fedora-env
node --version      # From alpine-env
```

### 4.3 Project-Specific Environment

```bash
cd /path/to/myproject

# Create .eenv in project directory
epkg env create --root ./.eenv -c alpine
epkg --root ./.eenv install py3-pip py3-requests

# Add to .gitignore
echo ".eenv/" >> .gitignore

# Team members run scripts after cloning
epkg run ./setup.sh    # Auto-discovers .eenv
epkg run ./main.py
```

### 4.4 Rollback and History Management

```bash
# View environment history
epkg history

# Sample output:
# id | timestamp           | action  | packages | command line
# ---+---------------------+---------+----------+---------------------------
# 1  | 2026-03-11 10:00:00 | Create  |          | epkg env create alpine
# 2  | 2026-03-11 10:05:00 | Install | +6       | epkg -e alpine install bash
# 3  | 2026-03-11 11:00:00 | Install | +2       | epkg -e alpine install jq

# Rollback to specified version
epkg restore 2        # Rollback to generation 2
epkg restore -1       # Rollback one version
```

### 4.5 Sandbox Modes

```bash
# Default: namespace isolation (env mode)
epkg -e myenv run bash

# Filesystem isolation (pivot_root)
epkg -e myenv run --sandbox=fs bash

# Virtual machine isolation (most secure)
epkg -e myenv run --sandbox=vm bash

# Choose VMM backend
epkg -e myenv run --sandbox=vm --vmm=libkrun,qemu bash
```

---

## 5. Common Commands Detail

### 5.1 Package Installation

```bash
# Basic install
epkg install package-name

# Specify environment
epkg -e myenv install package-name

# Install multiple packages
epkg install bash jq htop

# Install local/remote package files
epkg install ./package.rpm https://example.com/package.deb

# Options
epkg install --dry-run package          # Preview
epkg install -y package                 # Non-interactive
epkg install --no-install-recommends    # Don't install recommended packages
```

### 5.2 Package Query

```bash
# List installed packages (default)
epkg list
epkg list bash*          # glob filter

# List upgradable packages
epkg list --upgradable

# List available packages from repo
epkg list --available

# Search packages
epkg search htop
epkg search --files ".desktop"   # Search files

# Package details
epkg info bash
epkg info bash --arch aarch64    # Other architecture
```

### 5.3 Environment Management

```bash
# List environments
epkg env list

# Create environment
epkg env create myenv -c alpine
epkg env create myenv -c debian:13       # Specify version
epkg env create --root /tmp/test/.eenv   # Specify path

# Register/unregister
epkg env register myenv --path-order 10
epkg env unregister myenv

# Activate/deactivate (temporary)
epkg env activate myenv
epkg env deactivate

# View PATH
epkg env path
# Output: export PATH="/home/user/.epkg/envs/main/ebin:..."

# Environment config
epkg env config get sandbox.sandbox_mode
epkg env config set sandbox.sandbox_mode fs
```

### 5.4 Running Commands

```bash
# Run in environment
epkg -e myenv run command --args

# Examples
epkg -e alpine run jq --version
epkg -e debian run python3 -c "print('hello')"

# Auto-discover .eenv
cd /project
epkg run ./script.sh

# Built-in busybox commands
epkg busybox ls -la
epkg busybox cat /etc/passwd
epkg busybox sha256sum file.txt
```

---

## 6. Directory Structure and Files

### 6.1 User Private Installation Layout

```
~/.epkg/
├── envs/                          # Environment directory
│   ├── self/                      # epkg self environment
│   │   ├── usr/bin/epkg           # epkg binary
│   │   └── usr/src/epkg/          # Source code and assets
│   ├── main/                      # Default environment
│   │   ├── ebin/                  # Exposed binaries (PATH)
│   │   ├── usr/                   # symlink → store
│   │   ├── etc/                   # Configuration
│   │   └── generations/           # Historical versions
│   └── <env-name>/                # Other environments
├── store/                         # Content-addressed storage
│   └── <hash>__<name>__<ver>__<arch>/fs/
└── config/                        # Configuration files
    ├── options.yaml               # Global options
    └── envs/<env>.yaml            # Environment config

~/.cache/epkg/
├── downloads/                     # Download cache
├── channels/                      # Repo metadata cache
└── aur_builds/                    # AUR build cache
```

### 6.2 Root Shared Installation Layout

```
/opt/epkg/
├── cache/downloads/               # Shared download cache
├── cache/channels/                # Shared metadata cache
├── store/                         # Shared storage
└── envs/
    ├── root/<env>/                # root user environments
    └── <owner>/<env>/             # Other users' public environments
```

### 6.3 Key Configuration Files

| File | Purpose |
|------|---------|
| `~/.epkg/config/options.yaml` | Global options (default sandbox, proxy, etc.) |
| `~/.epkg/config/envs/<env>.yaml` | Environment config (channel, public, etc.) |
| `<env_root>/etc/epkg/env.yaml` | In-environment config |
| `~/.bashrc` | Load epkg shell integration |

---

## 7. Advanced Features

### 7.1 Sandbox Mode Comparison

| Mode | Isolation Level | Performance | Use Case |
|------|----------------|-------------|----------|
| `env` | namespace | Highest | Daily development |
| `fs` | pivot_root | High | Need filesystem isolation |
| `vm` | Virtual machine | Lower | Running untrusted code |

### 7.2 VMM Backends

| Backend | Features |
|---------|----------|
| `libkrun` | Lightweight microVM, fast startup |
| `qemu` | Full VM, broad compatibility |

### 7.3 Built-in Busybox Commands

```bash
epkg busybox --list    # View available commands

# Common commands
epkg busybox ls        # List directory
epkg busybox cat       # View file
epkg busybox grep      # Search text
epkg busybox sed       # Stream editor
epkg busybox wget      # Download
epkg busybox tar       # Archive
epkg busybox sha256sum # Hash calculation
```

---

## 8. Troubleshooting

### 8.1 Common Issues

| Issue | Solution |
|-------|----------|
| `command not found` | Confirm environment is registered: `epkg env list` |
| Download failed | Check network/proxy: `--proxy` |
| Permission error | Confirm running in user namespace |
| Dependency conflict | Check `--dry-run` output |

### 8.2 Debug Options

```bash
# Verbose output
epkg -v install package

# Debug log
RUST_LOG=debug epkg install package

# Simulate run
epkg --dry-run install package
```

### 8.3 Cleanup and Recovery

```bash
# Garbage collection
epkg gc

# Clean old downloads
epkg gc --old-downloads 7  # Older than 7 days

# History rollback
epkg history
epkg restore <gen_id>
```

---

## 9. Best Practices

### 9.1 Environment Naming Convention

```
main           # Default environment
dev-<project>  # Project development environment
<distro>       # Distro test environment (debian, fedora)
<tool>         # Tool environment (rust, python, node)
```

### 9.2 PATH Priority Strategy

```bash
# Dev tools first
epkg env register dev-tools --path-order 5

# System compatibility environment
epkg env register compat --path-order 50

# Append to end of PATH
epkg env register fallback --path-order -10
```

### 9.3 Project Workflow

```
project/
├── .eenv/           # epkg environment (add to .gitignore)
├── .gitignore
├── README.md        # Doc: "Run epkg run ./setup.sh"
└── src/
```

---

## 10. Comparison with Similar Tools

| Feature | epkg | Nix | Conda | Docker |
|---------|------|-----|-------|--------|
| No root | ✅ | ✅ | ✅ | ❌ |
| Multi-distro | ✅ | Partial | ❌ | ✅ |
| Mix packages | ✅ | ✅ | ❌ | ❌ |
| Native performance | ✅ | ✅ | ✅ | Overhead |
| Learning curve | Low | High | Low | Medium |

---

## Appendix: Command Cheat Sheet

```bash
# Install
wget ... && bash epkg-installer.sh && bash

# Environment
epkg env list
epkg env create <name> -c <channel>
epkg env remove <name>
epkg env register <name>
epkg env activate <name>

# Package
epkg install <pkg>
epkg remove <pkg>
epkg update
epkg upgrade
epkg list
epkg search <pattern>
epkg info <pkg>

# Run
epkg -e <env> run <cmd>
epkg run ./script.sh

# History
epkg history
epkg restore <gen>

# Other
epkg gc
epkg repo list
epkg --version
```

---

*Document Version: v1.0*
*Applicable to: epkg 0.2.4+*