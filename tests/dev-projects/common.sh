#!/bin/sh
# Sourced by langs/*.sh. Provides run(), run_install(), check_cmd(), lang_skip(), lang_ok().
# When sourced from langs/foo.sh, $0 is the lang script: LANG_NAME and SCRIPT_DIR are set from it.
# Requires ENV_NAME, EPKG_BIN (and optionally OS, ENV_ROOT). From run.sh we export ENV_NAME, ENV_ROOT, OS.
# ENV_ROOT is used by run_ebin/run_ebin_if to exercise usr/ebin/<tool> (exposed binaries). Standalone:
#   ENV_NAME=dev-alpine ENV_ROOT=$HOME/.epkg/envs/dev-alpine OS=alpine EPKG_BIN=/path/to/epkg ./langs/c.sh

SCRIPT_DIR="${SCRIPT_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"
LANG_NAME="${LANG_NAME:-$(basename "$0" .sh)}"

[ -n "$ENV_NAME" ] && [ -n "$EPKG_BIN" ] || {
    echo "[dev-projects] Set ENV_NAME and EPKG_BIN (e.g. run from run.sh or: ENV_NAME=... OS=... EPKG_BIN=... $0)" >&2
    exit 1
}

# Log epkg command to stderr. Use color if GREEN/NC exported from run.sh.
_log_cmd() {
    printf '%b\n' "${GREEN:-}[${OS:-?}/${LANG_NAME}]${NC:-} \$ $*" >&2
}

# Fail with clear debug info if log contains Error:, Warning:, or WARN
_check_log_and_fail() {
    local log_file="$1"
    local cmd_display="$2"
    if [ ! -f "$log_file" ]; then
        return 0
    fi
    local error_pattern='Error:|Warning:|WARN|exited with code'
    if grep -qE "$error_pattern" "$log_file"; then
        echo "" >&2
        echo "Reproduce command: $cmd_display" >&2
        echo "Log file: $log_file" >&2
        echo "<<<<<<<<<<<<<<<<<<<" >&2
        grep -n -B2 -A3 -E "$error_pattern" "$log_file" >&2
        echo ">>>>>>>>>>>>>>>>>>>" >&2
        exit 1
    fi
}

# Per-run log file counter (fresh per lang script process)
RUN_COUNT=${RUN_COUNT:-0}

# Shared: log to file, check for Error/Warning/WARN, cat log, return exit code. Used by run() and run_install().
_run_logged() {
    tag=$1
    cmd_display=$2
    shift 2
    RUN_COUNT=$((RUN_COUNT + 1))
    log_dir="${LOG_DIR:-/tmp}"
    log_file="$log_dir/epkg-dev-projects-${OS:-?}-${LANG_NAME:-?}-${tag}-${RUN_COUNT}.log"
    if ! mkdir -p "$log_dir" 2>/dev/null; then
        log_dir="/tmp"
        log_file="$log_dir/epkg-dev-projects-${OS:-?}-${LANG_NAME:-?}-${tag}-${RUN_COUNT}.log"
    fi
    _log_cmd "$cmd_display"
    "$EPKG_BIN" -e "$ENV_NAME" "$@" > "$log_file" 2>&1
    r=$?
    _check_log_and_fail "$log_file" "$cmd_display"
    cat "$log_file"
    return $r
}

# "run --" so options like ruby -e / node -e are not parsed as epkg run options.
run() {
    _run_logged "run" "epkg -e $ENV_NAME run -- $*" run -- "$@"
}

run_install() {
    _run_logged "install" "epkg -e $ENV_NAME --assume-yes install --ignore-missing $*" --assume-yes install --ignore-missing "$@"
}

# Quiet: no log file, no grep, no stop. Use with || lang_skip or || run_install ... for graceful fallback.
check_cmd() {
    "$EPKG_BIN" -e "$ENV_NAME" run -- "$@"
}

# Run the exposed binary at env usr/ebin/<name> (exercises ebin wrappers). No-op if ENV_ROOT unset.
run_ebin() {
    [ -z "${ENV_ROOT:-}" ] && return 0
    bin=$1
    shift
    run "$ENV_ROOT/usr/ebin/$bin" "$@"
}

# Run ebin binary only if it exists (for optional names, e.g. pip3 vs pip).
run_ebin_if() {
    [ -z "${ENV_ROOT:-}" ] && return 0
    bin=$1
    [ ! -x "$ENV_ROOT/usr/ebin/$bin" ] && return 0
    shift
    run "$ENV_ROOT/usr/ebin/$bin" "$@"
}

# Call when this language is not available on this OS (exit 0)
lang_skip() {
    printf '%b\n' "${YELLOW:-}[${OS:-?}/${LANG_NAME}]${NC:-} $*, skip" >&2
    exit 0
}

# Call on success
lang_ok() {
    printf '%b\n' "${GREEN:-}[${OS:-?}/${LANG_NAME}]${NC:-} OK"
}
