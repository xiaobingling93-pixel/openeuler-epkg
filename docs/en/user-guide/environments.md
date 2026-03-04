# Environments

Environments are isolated roots: each has a channel (e.g. debian, alpine, fedora with optional version), its own set of installed packages, and a directory `usr/ebin` where exposed binaries are linked. You can **register** environments so their `usr/ebin` is added to your PATH, or use **activate** for the current shell only.

## List environments

```bash
epkg env list
```

Example output:

```
 Type      Status         Environment                         Root
=========-==============-===================================-================================
 private                  __c__compass-ci__.eenv              /c/compass-ci/.eenv
 private                  __c__epkg__scripts__mirror__.eenv   /c/epkg/scripts/mirror/.eenv
 private                  aa                                  /home/wfg/.epkg/envs/aa
 private                  alpine                              /home/wfg/.epkg/envs/alpine
 private                  archlinux                           /home/wfg/.epkg/envs/archlinux
 private                  conda                               /home/wfg/.epkg/envs/conda
 private                  debian                              /home/wfg/.epkg/envs/debian
 private                  fedora                              /home/wfg/.epkg/envs/fedora
 private   registered@0   main                                /home/wfg/.epkg/envs/main
 private                  openeuler                           /home/wfg/.epkg/envs/openeuler
 private                  opensuse                            /home/wfg/.epkg/envs/opensuse
 private                  ubuntu                              /home/wfg/.epkg/envs/ubuntu
```

Columns: **Environment** name, **Type** (private/public), **Status** (e.g. registered@ORDER). In shared store mode, other users’ public envs may appear with an owner prefix (e.g. `root/envname`).

## Create an environment

```bash
epkg env create [ENV_NAME] [-c|--channel CHANNEL] [-P|--public] [-i|--import FILE]
```

- **ENV_NAME** — Name of the new environment.
- **-c, --channel** — Channel (e.g. `debian` (default), `ubuntu`, `alpine`, `fedora`, `openeuler`, `archlinux`, `conda`).
- **-P, --public** — Make the environment public (in shared store mode, other users can use it read-only as `owner/envname`).
- **-i, --import** — Import from a config file.

Examples:

```bash
epkg env create mydebian -c debian
# Creating environment 'mydebian' in $HOME/.epkg/envs/mydebian

epkg env create myalpine -c alpine
# Creating environment 'myalpine' in $HOME/.epkg/envs/myalpine
```

### Create by path (--root)

You can create an environment at an arbitrary path; epkg generates a name from the path:

```bash
epkg env create --root /tmp/myproject/.eenv -c alpine
# Creating environment '__tmp__myproject__.eenv' in /tmp/myproject/.eenv
# Note: environment name was auto-generated from path
```

Using **`.eenv`** as the directory name enables **implicit env discovery**: from a script under that tree, `epkg run ./script.sh` (or `epkg run /path/to/project/subdir/script.sh`) can resolve the environment from the containing `.eenv` directory.

## Remove an environment

```bash
epkg env remove [ENV_NAME]
```

If the env is registered, it is unregistered first. Example:

```
# Environment 'myenv' is not registered.
# Environment 'myenv' has been removed.
```

## Register and unregister

**Register** adds the environment’s `usr/ebin` to your default PATH (persisted across shells). **Unregister** removes it.

```bash
epkg env register [ENV_NAME] [--path-order N]
epkg env unregister [ENV_NAME]
```

After register, the command prints the new PATH; you can run `eval "$(epkg env path)"` in the current shell to apply it, or rely on your RC file if it sources epkg’s path helper.

Example:

```bash
epkg env register myalpine
# Registering environment 'myalpine' with PATH order 100
# export PATH="/home/user/.epkg/envs/myalpine/usr/ebin:/home/user/.epkg/envs/main/usr/ebin:..."

epkg env unregister myalpine
# export PATH="/home/user/.epkg/envs/main/usr/ebin:..."
# Environment 'myalpine' has been unregistered.
```

**--path-order** — Lower number means earlier in PATH. Default is 100.

## Activate and deactivate

**Activate** sets the environment for the current shell only (session-specific). **Deactivate** clears it.

```bash
epkg env activate [ENV_NAME]
epkg env deactivate
```

After activate, the shell’s PATH is updated so the activated env is preferred. Useful for temporary focus on one env without changing registered envs.

## Path and config

- **Path** — Print the current PATH that includes all registered (and optionally activated) envs. Order is:

  1. Activated envs (most recently activated first)
  2. Registered envs with lower `--path-order` first (prepend side)
  3. Original/system PATH
  4. Registered envs with negative `--path-order` (append side)

  ```bash
  epkg env path
  # export PATH="/home/user/.epkg/envs/main/usr/ebin:..."
  ```

- **Config** — View or edit per-env config:

  ```bash
  epkg env config edit
  epkg env config get <key>
  epkg env config set <key> <value>
  ```

  Examples: `env_root`, `public` (bool).

## Selecting the environment for commands

For any command that operates on an environment, you can specify it with:

- **-e, --env ENV_NAME** — Name (e.g. `main`, `alpine`) or, in shared store, `owner/envname`.
- **-r, --root DIR** — Root directory of the env (e.g. after `env create --root /path`).

If both are present, `-r` takes precedence.
If neither is given, epkg finds .eenv/ env, uses the **activated** env, or the **registered** envs (for `run`, the first env in PATH that provides the command), or falls back to **main**.

Examples:

```bash
epkg -e alpine install htop
epkg -e alpine list
epkg -e alpine run htop --version
epkg --root /tmp/myproject/.eenv run jq --version
```

## Sandbox modes and VMM selection for `epkg run`

When you run commands inside an environment, epkg can add extra isolation on top of the per‑env root filesystem:

- **env** (default) — User and mount namespaces with bind mounts of the environment over `/usr`, `/etc`, `/var`, `/run`, etc. Provides isolation for compatibility; not a strong security boundary.
- **fs** — The environment becomes the new root via `pivot_root`; proc, tmpfs (/tmp, /dev), and other pseudo-filesystems are mounted under it. Stronger filesystem isolation.
- **vm** — Run the command inside a lightweight VM with the environment root shared via virtiofs. See `docs/design-notes/sandbox-vmm.md` for design and dependencies (VMM, kernel, virtiofsd, optional libkrun).

You select the sandbox mode per command:

```bash
epkg -e mydebian run --sandbox=env bash
epkg -e mydebian run --sandbox=fs  python3 script.py
epkg -e mydebian run --sandbox=vm  bash
```

Or set a **per‑env default** in `env_root/etc/epkg/env.yaml` via:

```bash
# Make fs the default sandbox for this env
epkg -e mydebian env config set sandbox.sandbox_mode fs

# After that, plain `epkg -e mydebian run <cmd>` uses fs unless you override --sandbox
epkg -e mydebian run bash
```

User-level defaults can be set in `~/.epkg/config/options.yaml` (same `sandbox.sandbox_mode` key). CLI `--sandbox` overrides both.

For the host‑side tools that sandboxing relies on (user namespaces and `newuidmap`/`newgidmap`), install a minimal set with:

```bash
cd /c/epkg
./bin/make.sh sandbox-depends
```

This pulls in the appropriate `uidmap`/`shadow`/`shadow-uidmap` package for your distro; see the [Troubleshooting](troubleshooting.md) guide for user‑namespace errors.

### Choosing a VMM backend (`--vmm`)

When you use `--sandbox=vm`, epkg can try multiple VMM backends in order. Use
the `--vmm` option on `epkg run` to specify a comma-separated preference list:

```bash
# Prefer libkrun, fall back to QEMU
epkg -e myenv run --sandbox=vm --vmm=libkrun,qemu bash

# Force QEMU even if libkrun support is compiled in
epkg -e myenv run --sandbox=vm --vmm=qemu bash
```

Backend names:

- **libkrun** — libkrun-based microVM backend (only available when epkg is built
  with the `libkrun` feature and libkrunfw is installed in the env).
- **qemu** — QEMU + virtiofs backend.

If `--vmm` is omitted:

- With `libkrun` enabled at build time, the default order is `libkrun,qemu`.
- Without `libkrun`, the default order is `qemu` only.

If a backend fails (for example missing binaries or misconfiguration), epkg
logs a warning and automatically tries the next backend in the list.

## Public environments (shared store)

When epkg is used with a **shared** store (e.g. root with `/opt/epkg`), environments can be **public**. Other users can then:

- List them (they appear as `owner/envname` in `epkg env list`).
- Use them read-only: `epkg -e owner/envname run <cmd>`, `epkg -e owner/envname search <pkg>`, etc.

Creating with `-P` makes an env public. The `main` env cannot be public.

## Store mode rules (self install)

- **epkg self install** can take `--store private|shared|auto`.
- **auto** (default): private if not root; shared if root.

Public/private only applies in shared store mode.

## Best practices

### Organize environments by purpose

- **main** — Default environment for general use
- **project-name** — Per-project environments with specific dependencies
- **distro-name** — Environments for trying packages from specific distributions
- **tool-name** — Environments for specific tools or toolchains

### Use path-order for PATH ordering

When registering multiple environments, use `--path-order` to control which binaries take precedence:

```bash
epkg env register dev-env  --path-order 5   # Earlier in PATH
epkg env register test-env --path-order 20  # Later in PATH
```

Lower numbers = earlier in PATH.

### Project-specific environments

For projects that need isolated dependencies:

```bash
cd /path/to/project
epkg env create --root ./.eenv -c alpine
# Add to .gitignore: .eenv/
# Document in README: "Run: epkg run ./setup.sh"
```

This keeps project dependencies isolated and makes the project portable.

### Clean up unused environments

Periodically review and remove environments you no longer need:

```bash
epkg env list
epkg env remove old-env
epkg gc  # Clean up unused store files
```

## See also

- [Package operations](package-operations.md) — Installing packages in environments
- [Advanced usage](advanced.md) — Running commands and services
