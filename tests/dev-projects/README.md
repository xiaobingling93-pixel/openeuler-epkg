# dev-projects tests

Tests simple software project development in common languages (Python, Go, Rust, Node, C, ...) across multiple OS envs.

- **Modular**: `run.sh` (main) + `lib.sh` (shared routines) + `langs/*.sh` (one script per language).
- **OSes**: `ALL_OS="openeuler fedora debian ubuntu alpine archlinux conda"`; create one env per OS, run all (or selected) lang tests inside it.
- **Selection**: `-o OS` run a single OS; `-t LANG` run a single test (e.g. `-t python`); `-c` remove env after run (default: leave for debug).
- **Reuse**: Top-level support in `lib.sh` (env create/remove, `run_in_env`, logging, timeout, arg parsing); per-lang scripts are thin and use `ENV_NAME`, `OS`, `EPKG_BIN` only.
- Run from repo root: `timeout 600 tests/dev-projects/run.sh -o ubuntu -t python` or `tests/dev-projects/run.sh` for full run (env removed before create, left for debug at end).

## Layout

- **run.sh** — Main script: removes env if present, creates fresh env, runs lang tests. Each `run()`/`run_install()` writes to a per-command log under `LOG_DIR`; on Error/Warning/WARN we stop and show that log. Never removes env at end (left for debug).
- **lib.sh** — Shared routines: `ALL_OS`, env create/remove (remove is idempotent; used only before create), `run_in_env`, log/error, timeout, `-o`/`-t` parsing.
- **common.sh** — Sourced by each lang script: sets `LANG_NAME` from script name and `SCRIPT_DIR` from `$0`; requires `ENV_NAME`, `EPKG_BIN` (clear error if unset). Provides `run()`, `run_install()` (with `--ignore-missing`), `check_cmd()`, `lang_skip()`, `lang_ok()`, `run_ebin()`, `run_ebin_if()`.
- **langs/*.sh** — One script per language (python, go, rust, node, c). Each: one-line header, epkg install + check_cmd, minimal project build/run, then (if the lang has a pkg manager) install one pkg via it and verify (pip/npm/go get/cargo add). **Runnable standalone**: `ENV_NAME=dev-alpine OS=alpine EPKG_BIN=/path/to/epkg ./langs/c.sh` (env must exist).

## Usage

From repo root (epkg built):

```sh
# All OSes, all lang tests (env removed before create if present; left for debug at end)
tests/dev-projects/run.sh

# Single OS
tests/dev-projects/run.sh -o ubuntu

# Single failed test (e.g. python on alpine)
tests/dev-projects/run.sh -o alpine -t python

# Debug mode
tests/dev-projects/run.sh -d -o alpine -t c
```

With timeout:

```sh
timeout 600 tests/dev-projects/run.sh -o openeuler -t go
```

## Testing Native Windows epkg.exe from WSL2

When developing epkg for Windows, you can run the dev-projects tests from WSL2 against a native Windows epkg.exe binary. This is useful for:

- Testing Windows builds without leaving the Linux development environment
- CI/CD pipelines that run on WSL2 but need to validate Windows binaries
- Cross-platform regression testing

### WSL2 Test Script: run-wsl2-windows.sh

The `run-wsl2-windows.sh` script wraps `run.sh` with WSL2-specific configuration:

- Automatically detects WSL2 environment
- Sets `EPKG_BIN` to point to Windows epkg.exe
- Translates `run_ebin` calls to use `epkg.exe run` for Linux distros (since they need VM execution)

```sh
# Test all languages on alpine from WSL2
tests/dev-projects/run-wsl2-windows.sh -o alpine

# Test only python on ubuntu
tests/dev-projects/run-wsl2-windows.sh -o ubuntu -t python

# Use custom epkg.exe location
tests/dev-projects/run-wsl2-windows.sh -e /mnt/c/ProgramData/epkg/epkg.exe -o alpine

# With debug mode
tests/dev-projects/run-wsl2-windows.sh -dd -o alpine -t python
```

### Prerequisites

1. **Build epkg for Windows**: Cross-compile or build natively on Windows
2. **Place epkg.exe**: Default location is `/mnt/c/Users/$USER/epkg.exe`, or specify with `-e`
3. **WSL2**: Must be running in a WSL2 distribution (not WSL1)

### How It Works

When running from WSL2 against Linux distros (alpine, ubuntu, etc.):

1. `run_ebin()` detects non-Linux host via environment variables
2. Since Linux distros run in VM on Windows, epkg doesn't create ebin/ wrappers
   (ELF binaries need VM execution, creating wrappers is too complex and script interpreters are also ELF)
3. Instead of executing `$ENV_ROOT/ebin/$bin` directly, it runs `$EPKG_BIN -e $ENV_NAME run -- $bin "$@"`
4. This invokes the Windows epkg.exe which runs the command inside the VM

For native Windows/macOS distros (msys2/conda/brew), ebin/ wrappers are still created and used directly.

Note: This same behavior also applies to macOS testing. On macOS, Linux distros need VM execution, so `run_ebin()` uses `epkg run` and ebin/ is empty. Native macOS homebrew packages use ebin/ wrappers directly.

## Adding a language

Add `langs/<name>.sh` (executable). It must:

1. One line: `. "$(dirname "$0")/../common.sh"` (LANG_NAME is set from script name).
2. `run_install pkg1 pkg2 pkg3` then `check_cmd <tool> <version-flag> || lang_skip "reason"`.
3. Use `run cmd...` for build/run steps. End with `lang_ok`.
