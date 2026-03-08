#!/bin/bash
# Run simple software-project dev tests across OSes and languages.
# Supports -d/-dd/-ddd, -o OS (single OS), -t TEST (single lang test).
# Removes env before create if it exists; never removes at end (leaves env for human/agent debug).
# Each run()/run_install() writes to an individual log file; on Error:/Warning:/WARN we stop and show log.

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
. "$PROJECT_ROOT/tests/common.sh"
. "$SCRIPT_DIR/lib.sh"

# Remember the original test invocation for error messages.
ORIG_CMD="$0 $*"

# Parse -d/-dd/-ddd/-h first (common.sh parse_debug_flags rejects -o), then -o/-t/-c
DEBUG_FLAG=""
REMAINING=""
while [ $# -gt 0 ]; do
    case "$1" in
        -h|--help)
            echo "Usage: $0 [-d|--debug|-dd|-ddd] [-o|--os OS] [-t|--test LANG]"
            echo "  -o OS   Run only this OS (e.g. ubuntu, alpine)"
            echo "  -t LANG Run only this lang test (e.g. python, go)"
            echo "  Env is removed before create if present; never removed at end (left for debug)."
            echo "OSes: $ALL_OS"
            exit 0
            ;;
        -ddd) DEBUG_FLAG="-ddd"; shift ;;
        -dd)  DEBUG_FLAG="-dd"; shift ;;
        -d|--debug) DEBUG_FLAG="-d"; shift ;;
        *)    REMAINING="$REMAINING $1"; shift ;;
    esac
done
eval set -- $REMAINING

set_epkg_bin
set_color_names
export EPKG_BIN GREEN YELLOW NC

case "${DEBUG_FLAG:-}" in
    -ddd) export RUST_LOG=trace RUST_BACKTRACE=1; set -x ;;
    -dd)  export RUST_LOG=debug RUST_BACKTRACE=1; set -x ;;
    -d)   set -x ;;
esac

parse_run_args "$@" || exit 1

LOG_DIR="${LOG_DIR:-/tmp}"
export LOG_DIR

log "Starting dev-projects test (OS: ${SELECT_OS:-all}, test: ${SELECT_TEST:-all})"

TIMEOUT_LANG=300
FAILED=0
FAILED_OS=""
FAILED_LANG=""

for os in $ALL_OS; do
    should_run_os "$os" || continue
    ENV_NAME=$(env_name_for "$os")
    ENV_ROOT="${EPKG_ENVS_DIR:-$HOME/.epkg/envs}/$ENV_NAME"
    export ENV_NAME ENV_ROOT OS="$os"

    remove_env "$os"
    create_env "$os"

    for lang in $(list_lang_tests); do
        should_run_test "$lang" || continue
        script="$SCRIPT_DIR/langs/${lang}.sh"
        [ -x "$script" ] || continue
        log "Running test $lang on $os (env=$ENV_NAME)"
        if ! run_with_timeout "$TIMEOUT_LANG" "$script"; then
            log "Test $lang failed for $os (env left for debug: $ENV_NAME)"
            if [ "$FAILED" -eq 0 ]; then
                FAILED_OS="$os"
                FAILED_LANG="$lang"
            fi
            FAILED=1
        fi
    done

    log "Leaving env $ENV_NAME for debug"
done

if [ $FAILED -eq 1 ]; then
    if [ -n "$FAILED_OS" ] && [ -n "$FAILED_LANG" ]; then
        error "Lang test failed; reproduce with: $SCRIPT_DIR/run.sh -o $FAILED_OS -t $FAILED_LANG"
    else
        error "Lang test failed; reproduce with: $SCRIPT_DIR/run.sh $*"
    fi
fi
log "All dev-projects tests passed"
