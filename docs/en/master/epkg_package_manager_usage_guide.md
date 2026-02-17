# epkg User Guide

## Introduction

This document explains how to initialize the working environment for the epkg package manager and how to use its basic features. All operation results in this document are demonstrated using a non-root user as an example.
Note: Currently, epkg packages are only compatible with the AArch64 architecture, and support for other architectures will be expanded in the future.

## Quick Start

The following examples demonstrate how to install different versions of software packages.

```bash
# Install epkg using curl.
# During installation, you can choose between user/global installation modes to install epkg for the current user or all users.
# Only the root user can use the global installation mode.
wget https://repo.oepkgs.net/openeuler/epkg/rootfs/epkg-installer.sh
sh epkg-installer.sh

# Uninstall epkg.
wget https://repo.oepkgs.net/openeuler/epkg/rootfs/epkg-uninstaller.sh
sh epkg-uninstaller.sh

# Install epkg.
epkg self install
bash // Re-execute .bashrc to update the PATH

# Create environment 1.
epkg env create t1
epkg install tree
tree --version
which tree

# View repositories.
[root@vm-4p64g ~]# epkg repo list
------------------------------------------------------------------------------------------------------------------------------------------------------
channel                        | repo            | url
------------------------------------------------------------------------------------------------------------------------------------------------------
openEuler-22.03-LTS-SP3        | OS              | https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-22.03-LTS-SP3/OS/aarch64/
openEuler-24.09                | everything      | https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64/
openEuler-24.09                | OS              | https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/OS/aarch64/
------------------------------------------------------------------------------------------------------------------------------------------------------

# Create environment 2, specify a repository.
epkg env create t2 --repo openEuler-22.03-LTS-SP3
epkg install tree 
tree --version
which tree

# Switch back to environment 1.
epkg env activate t1
```

## epkg Usage

```bash
Usage:
    epkg install PACKAGE 
    epkg install [--env ENV] PACKAGE (under development)
    epkg remove [--env ENV] PACKAGE (under development)
    epkg upgrade [PACKAGE] (under development)

    epkg search PACKAGE (under development)
    epkg list (under development)
    
    epkg env list
    epkg env create|remove ENV
    epkg env activate ENV
    epkg env deactivate ENV
    epkg env register|unregister ENV
    epkg env history ENV (under development)
    epkg env rollback ENV (under development)
```

Package installation:

```bash
epkg env create $env # Create an environment.
epkg install $package # Install a package in the environment.
epkg env create $env2 --repo $repo # Create environment 2, specify a repository.
epkg install $package # Install a package in environment 2.
```

Package building:

```bash
epkg build ${yaml_path}/$pkg_name.yaml
```

### Installing Software

Function description:

Install software in the current environment (confirm the current environment before operation).

Command:

```shell
epkg install ${package_name}
```

Example output:

```shell
[root@2d785c36ee2e /]# epkg env activate t1
Add common to path
Add t1 to path
Environment 't1' activated.
Environment 't1' activated.
[root@2d785c36ee2e /]# epkg install tree
EPKG_ENV_NAME: t1
Caching repodata for: "OS"
Cache for "OS" already exists. Skipping...
Caching repodata for: "OS"
Cache for "OS" already exists. Skipping...
Caching repodata for: "everything"
Cache for "everything" already exists. Skipping...
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/FF/FFCRTKRFGFQ6S2YVLOSUF6PHSMRP7A2N__ncurses-libs__6.4__8.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/D5/D5BOEFTRBNV3E4EXBVXDSRNTIGLGWVB7__glibc-all-langpacks__2.38__34.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/VX/VX6SUOPGEVDWF6E5M2XBV53VS7IXSFM5__openEuler-repos__1.0__3.3.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/LO/LO6RYZTBB2Q7ZLG6SWSICKGTEHUTBWUA__libselinux__3.5__3.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/EP/EPIEEK2P5IUPO4PIOJ2BXM3QPEFTZUCT__basesystem__12__3.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/2G/2GYDDYVWYYIDGOLGTVUACSBHYVRCRJH3__setup__2.14.5__2.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/HC/HCOKXTWQQUPCFPNI7DMDC6FGSDOWNACC__glibc__2.38__34.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/OJ/OJQAHJTY3Y7MZAXETYMTYRYSFRVVLPDC__glibc-common__2.38__34.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/FJ/FJXG3K2TSUYXNU4SES2K3YSTA3AHHUMB__tree__2.1.1__1.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/KD/KDYRBN74LHKSZISTLMYOMTTFVLV4GPYX__readline__8.2__2.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/MN/MNJPSSBS4OZJL5EB6YKVFLMV4TGVBUBA__tzdata__2024a__2.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/S4/S4FBO2SOMG3GKP5OMDWP4XN5V4FY7OY5__bash__5.2.21__1.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/EJ/EJGRNRY5I6XIDBWL7H5BNYJKJLKANVF6__libsepol__3.5__3.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/TZ/TZRQZRU2PNXQXHRE32VCADWGLQG6UL36__bc__1.07.1__12.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/WY/WYMBYMCARHXD62ZNUMN3GQ34DIWMIQ4P__filesystem__3.16__6.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/KQ/KQ2UE3U5VFVAQORZS4ZTYCUM4QNHBYZ7__openEuler-release__24.09__55.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/HD/HDTOK5OTTFFKSTZBBH6AIAGV4BTLC7VT__openEuler-gpg-keys__1.0__3.3.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/EB/EBLBURHOKKIUEEFHZHMS2WYF5OOKB4L3__pcre2__10.42__8.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/YW/YW5WTOMKY2E5DLYYMTIDIWY3XIGHNILT__info__7.0.3__3.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start download https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.09/everything/aarch64//store/E4/E4KCO6VAAQV5AJGNPW4HIXDHFXMR4EJV__ncurses-base__6.4__8.oe2409.epkg
############################################################################################################################################################################################################### 100.0%
start install FFCRTKRFGFQ6S2YVLOSUF6PHSMRP7A2N__ncurses-libs__6.4__8.oe2409
start install D5BOEFTRBNV3E4EXBVXDSRNTIGLGWVB7__glibc-all-langpacks__2.38__34.oe2409
start install VX6SUOPGEVDWF6E5M2XBV53VS7IXSFM5__openEuler-repos__1.0__3.3.oe2409
start install LO6RYZTBB2Q7ZLG6SWSICKGTEHUTBWUA__libselinux__3.5__3.oe2409
start install EPIEEK2P5IUPO4PIOJ2BXM3QPEFTZUCT__basesystem__12__3.oe2409
start install 2GYDDYVWYYIDGOLGTVUACSBHYVRCRJH3__setup__2.14.5__2.oe2409
start install HCOKXTWQQUPCFPNI7DMDC6FGSDOWNACC__glibc__2.38__34.oe2409
start install OJQAHJTY3Y7MZAXETYMTYRYSFRVVLPDC__glibc-common__2.38__34.oe2409
start install FJXG3K2TSUYXNU4SES2K3YSTA3AHHUMB__tree__2.1.1__1.oe2409
start install KDYRBN74LHKSZISTLMYOMTTFVLV4GPYX__readline__8.2__2.oe2409
start install MNJPSSBS4OZJL5EB6YKVFLMV4TGVBUBA__tzdata__2024a__2.oe2409
start install S4FBO2SOMG3GKP5OMDWP4XN5V4FY7OY5__bash__5.2.21__1.oe2409
start install EJGRNRY5I6XIDBWL7H5BNYJKJLKANVF6__libsepol__3.5__3.oe2409
start install TZRQZRU2PNXQXHRE32VCADWGLQG6UL36__bc__1.07.1__12.oe2409
start install WYMBYMCARHXD62ZNUMN3GQ34DIWMIQ4P__filesystem__3.16__6.oe2409
start install KQ2UE3U5VFVAQORZS4ZTYCUM4QNHBYZ7__openEuler-release__24.09__55.oe2409
start install HDTOK5OTTFFKSTZBBH6AIAGV4BTLC7VT__openEuler-gpg-keys__1.0__3.3.oe2409
start install EBLBURHOKKIUEEFHZHMS2WYF5OOKB4L3__pcre2__10.42__8.oe2409
start install YW5WTOMKY2E5DLYYMTIDIWY3XIGHNILT__info__7.0.3__3.oe2409
start install E4KCO6VAAQV5AJGNPW4HIXDHFXMR4EJV__ncurses-base__6.4__8.oe2409
```

### Listing Environments

Function description:

List all environments in epkg (under the `$EPKG_ENVS_ROOT` directory) and indicate the current environment.

Command:

```shell
epkg env list
```

Example output:

```shell
[small_leek@19e784a5bc38 bin]# epkg env list
Available environments(sort by time):
w1
main
common
You are in [main] now
```

### Creating an Environment

Function description:

Create a new environment. After successful creation, the new environment is activated by default, but is not globally registered.

Command:

```shell
epkg env create ${env_name} [--public]
```

Options:
- `--public`: Make the environment public (usable by all users). Only meaningful in shared store mode.

**Shared Store Mode Rules: (`epkg self install --store=shared|private|auto`)**

The shared store mode is determined automatically based on:
1. private if not running as root
2. private if current executable starts with `/home/`
3. public  if current executable starts with `/opt/epkg/`
4. public  if running as root and `/opt/epkg/store/` exists
5. private if `$HOME/.epkg/store/` exists
6. public  if `/opt/epkg/store/` exists
7. error and abort otherwise (run `epkg self install` first)

**Environment Public Attribute Rules:**

- `self` environment: always created as public
- `main` environment: always created as private
- Other environments: public/private is determined by the `--public` option on `epkg env create`

**Environment Access Rules:**

- In SHARED store mode:
  - Users can access their own public/private environments
  - Users can access all other users' public environments using `-e owner/env_name` format
  - PATH includes: user's own public/private registered envs + all other users' public registered envs
- In PRIVATE store mode:
  - Users can only access their own (private) environments
  - PATH includes: user's own (private) registered envs

Example output:

```shell
[small_leek@b0e608264355 bin]# epkg env create work1
YUM --installroot directory structure created successfully in: /root/.epkg/envs/work1/profile-1
Environment 'work1' added to PATH.
Environment 'work1' activated.
Environment 'work1' created.
```

**Visiting Other Users' Public Environments:**

In shared store mode, you can read-only visit another user's public environment using the `owner/env_name` format:

```shell
# List environments (shows own envs + other users' public envs)
epkg env list

# Run command in another user's public environment
epkg -e alice/pubenv run jq --version

# Search packages in another user's public environment
epkg -e alice/pubenv search package_name

# Get package info in another user's public environment
epkg -e alice/pubenv info package_name
```

### Activating an Environment

Function description:

Activate the specified environment, refresh `EPKG_ENV_NAME` and `RPMDB_DIR` (used to point to `--dbpath` when software is installed into the specified environment), refresh `PATH` to include the specified environment and the common environment, and set the specified environment as the first priority.

Command:

```shell
epkg env activate ${env_name}
```

Example output:

```shell
[small_leek@9d991d463f89 bin]# epkg env activate main
Environment 'main' activated
```

### Deactivating an Environment

Function description:

Deactivate the specified environment, refresh `EPKG_ENV_NAME` and `RPMDB_DIR`, refresh `PATH`, and default to the main environment.

Command:

```shell
epkg env deactivate ${env_name}
```

Example output:

```shell
[small_leek@398ec57ce780 bin]# epkg env deactivate w1
Environment 'w1' deactivated.
```

### Registering an Environment

Function description:

Register the specified environment, persistently refresh `PATH` to include all registered environments in epkg, and set the specified environment as the first priority.

Command:

```shell
epkg env register ${env_name}
```

Example output:

```shell
[small_leek@5042ae77dd75 bin]# epkg env register lkp
EPKG_ACTIVE_ENV: 
Environment 'lkp' has been registered to PATH.
```

### Unregistering an Environment

Function description:

Unregister the specified environment, persistently refresh `PATH` to include all registered environments in epkg except the specified one.

Command:

```shell
epkg env unregister ${env_name}
```

Example output:

```shell
[small_leek@69393675945d /]# epkg env unregister w4
EPKG_ACTIVE_ENV: 
Environment 'w4' has been unregistered from PATH.
```

### Building an epkg Package

Function description:

Build an epkg package using the YAML file provided by autopkg.

Command:

```shell
epkg build ${yaml_path}/$pkg_name.yaml
```

Example output:

```shell
[small_leek@69393675945d /]#  epkg build /root/epkg/build/test/tree/package.yaml
pkg_hash: fbfqtsnza9ez1zk0cy23vyh07xfzsydh, dir: /root/.cache/epkg/build-workspace/result
Compress success: /root/.cache/epkg/build-workspace/epkg/fbfqtsnza9ez1zk0cy23vyh07xfzsydh__tree__2.1.1__0.oe2409.epkg
```
