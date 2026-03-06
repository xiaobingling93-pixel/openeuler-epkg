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
- **common.sh** — Sourced by each lang script: sets `LANG_NAME` from script name and `SCRIPT_DIR` from `$0`; requires `ENV_NAME`, `EPKG_BIN` (clear error if unset). Provides `run()`, `run_install()` (with `--ignore-missing`), `check_cmd()`, `lang_skip()`, `lang_ok()`.
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

## Adding a language

Add `langs/<name>.sh` (executable). It must:

1. One line: `. "$(dirname "$0")/../common.sh"` (LANG_NAME is set from script name).
2. `run_install pkg1 pkg2 pkg3` then `check_cmd <tool> <version-flag> || lang_skip "reason"`.
3. Use `run cmd...` for build/run steps. End with `lang_ok`.
