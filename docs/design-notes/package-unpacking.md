# Package Unpacking System

This document describes the new pure Rust package unpacking system implemented for epkg.

## Overview

The package unpacking system has been redesigned to handle multiple package formats using pure Rust code without relying on external tools like `dpkg`, `rpm`, `tar`, or `ar`.

## Architecture

### Core Modules

1. **`store.rs`** - Generic package unpacking and store management
2. **`deb_pkg.rs`** - Debian package (.deb) unpacking
3. **`rpm_pkg.rs`** - RPM package (.rpm) unpacking
4. **`epkg.rs`** - EPKG package (.epkg) format support

### Key Functions

#### `store.rs`

- `unpack_packages(package_files)` - Unpacks multiple packages
- `unpack_mv_package(package_file)` - Unpacks a single package and moves to store
- `general_unpack_package(package_file, store_tmp_dir)` - Generic unpacking dispatcher
- `create_filelist_txt(store_tmp_dir)` - Creates mtree format file listing

#### `deb_pkg.rs`

- `unpack_package(deb_file, store_tmp_dir)` - Main Debian package unpacker
- `create_scriptlets()` - Maps Debian scripts to common format
- `create_package_txt()` - Converts control file to package.txt

## Package Format Detection

The system automatically detects package format based on file extensions:

- `.deb` → Debian packages
- `.rpm` → RPM packages
- `.epkg` → EPKG packages format
- `.apk` → Alpine packages (future)
- `.pkg.tar.xz`, `.pkg.tar.zst` → Pacman packages (future)

## Directory Structure

After unpacking, packages follow this standard structure:

```
store_tmp_dir/
├── fs/                    # Unpacked filesystem files
│   ├── bin/
│   ├── lib/
│   ├── etc/
│   ├── include/
│   └── share/
└── info/                  # Package metadata
    ├── package.txt        # Package description (unified format)
    ├── filelist.txt       # File listing in mtree format
    ├── deb/              # Original Debian control files
    │   ├── control
    │   ├── preinst
    │   ├── postinst
    │   ├── prerm
    │   └── postrm
    └── install/          # Standardized scripts
        ├── pre_install.sh
        ├── post_install.sh
        ├── pre_upgrade.sh
        ├── post_upgrade.sh
        ├── pre_uninstall.sh
        └── post_uninstall.sh
```

## Debian Package Processing

### AR Archive Extraction

Debian packages are AR archives containing:
- `debian-binary` - Version info
- `control.tar.*` - Control files (metadata, scripts)
- `data.tar.*` - Filesystem content

### Compression Support

Supports multiple compression formats:
- `.tar.gz` (gzip)
- `.tar.xz` (xz)
- `.tar.zst` (zstd)
- `.tar` (uncompressed)

### Script Mapping
Debian maintainer scripts are mapped to standardized names:

| Debian Script | Common Script(s)                       |
|:-------------:|:---------------------------------------|
| `preinst`     | `pre_install.sh`, `pre_upgrade.sh`     |
| `postinst`    | `post_install.sh`, `post_upgrade.sh`   |
| `prerm`       | `pre_uninstall.sh`                     |
| `postrm`      | `post_uninstall.sh`                    |

### Field Mapping

Debian control fields are mapped as follows:

| Debian Field    | Common Field  |
|:---------------:|:--------------|
| `Package`       | `pkgname`     |
| `Version`       | `version`     |
| `Architecture`  | `arch`        |
| `Depends`       | `requires`    |
| `Description`   | `summary`     |
| `Maintainer`    | `maintainer`  |
| ...             | ...           |

## Content-Addressable Storage

Packages are stored using content-addressable hashing:

1. Unpack to temporary directory
2. Calculate hash of `fs/` and `info/install/` contents
3. Generate package line: `{hash}__{pkgname}__{version}`
4. Move to final store location /opt/epkg/store/$pkgline

## mtree Format

The `filelist.txt` uses BSD mtree format with attributes:

```
path/to/file type=file mode=755 sha256=abc123... uname=user gname=group
path/to/dir type=dir mode=755
path/to/link type=link link=target
```

## Dependencies

- `ar` - AR archive handling
- `tar` - TAR archive extraction
- `flate2` - Gzip compression
- `xz2` - XZ compression
- `zstd` - Zstandard compression
- `sha2` - SHA256 hashing
- `walkdir` - Directory traversal

## Future Extensions

The modular design allows easy addition of new package formats:

1. Create new module (e.g., `apk_pkg.rs`)
2. Implement `unpack_package()` function
3. Add format detection in `detect_package_format()`
4. Add case in `general_unpack_package()`

## Error Handling

All functions use `color_eyre::Result` for comprehensive error reporting with context.
