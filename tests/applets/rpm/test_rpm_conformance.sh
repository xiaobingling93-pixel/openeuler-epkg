#!/bin/bash
# Compare epkg busybox rpm with system rpm for querying package files
# This script tests compatibility between implementations

# Get script directory and project root
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"

. $PROJECT_ROOT/tests/common.sh
set_epkg_bin

RPM_URL='https://mirrors.tuna.tsinghua.edu.cn/openeuler/openEuler-24.03-LTS-SP3/OS/x86_64/Packages/systemd-255-50.oe2403sp3.x86_64.rpm'
RPM_FILE="/tmp/$(basename "$RPM_URL")"

# RPM_FILE=$HOME/.cache/epkg/downloads/fedora/releases/42/Everything/x86_64/os/Packages/f/freetype-2.13.3-2.fc42.x86_64.rpm
# RPM_FILE=$HOME/.cache/epkg/downloads/fedora/releases/42/Everything/x86_64/os/Packages/g/gtk3-3.24.49-2.fc42.x86_64.rpm
# RPM_FILE=$HOME/.cache/epkg/downloads/openeuler/openEuler-25.09/everything/x86_64/Packages/selinux-policy-40.7-9.oe2509.noarch.rpm
# RPM_FILE=$HOME/.cache/epkg/downloads/openeuler/openEuler-25.09/everything/x86_64/Packages/systemd-255-54.oe2509.x86_64.rpm

EPKG_RPM="$EPKG_BIN busybox rpm"
HOST_RPM="${HOST_RPM:-rpm}"

# Check if system rpm is available
if ! command -v rpm &> /dev/null; then
    echo "Warning: system rpm not found, skipping comparison tests"
    exit 0
fi

if [ ! -f "$RPM_FILE" ]; then
    wget $RPM_URL -O $RPM_FILE
fi
if [ ! -f "$RPM_FILE" ]; then
    echo "Error: RPM file not found: $RPM_FILE" >&2
    exit 1
fi

# Verify epkg busybox rpm works
if ! $EPKG_BIN busybox rpm --help &> /dev/null; then
    echo "Error: epkg busybox rpm command not working"
    exit 1
fi

# Helper function to run a command and capture stdout/stderr
# Usage: run_cmd "command" output_file
run_cmd() {
    local cmd="$1"
    local output_file="$2"
    # Run command, redirect stderr to stdout, capture both
    $cmd 2>&1 | grep -v \
	-e '^error: Unable to open sqlite database' \
	-e '^error: cannot open Packages' \
	-e '^warning: .* Header V4 RSA/SHA256 Signature, key ID' \
	> "$output_file"
}

# Helper function to normalize rpm output for comparison
# Removes signature warnings, timestamps, and normalizes whitespace
normalize_output() {
    local input_file="$1"

    # Use array for grep patterns to avoid backslashes
    local grep_patterns=(
        -e '^Install Date'
        -e '^Signature'
        -e '^License'
        -e '^Vendor'
        -e '^Build Host'
        -e '^Bug URL'
    )

    # Build sed commands array
    local sed_cmds=(
        -e 's/\x1b\[[0-9;]*[a-zA-Z]//g'
        -e 's/[[:space:]]\+/ /g'
        -e 's/^[[:space:]]*//'
        -e 's/[[:space:]]*$//'
        -e 's/Build Date.*: .*, \(.*\) \(.*\) \(.*\) \(.*\) \(.*\)/Build Date: \1 \2 \3 \4 \5/'
        -e 's/Build Date.*: \(.*\), \(.*\) \(.*\) \(.*\)/Build Date: \1 \2 \3 \4/'
    )

    # Run pipeline with combined grep and sed
    grep -v -i "${grep_patterns[@]}" "$input_file" | \
    sed "${sed_cmds[@]}"
}

# Compare epkg and system rpm output for a given set of options
# Usage: compare_rpm_query "options" ["description"]
compare_rpm_query() {
    local options="$1"
    local description="${2:-$options}"

    echo "=== Testing $description ==="

    seqno=$((seqno+1))
    local host_cli="$HOST_RPM -qp $RPM_FILE $options"
    local epkg_cli="$EPKG_RPM -qp $PKGLINE  $options"
    run_cmd "$host_cli" /tmp/host_rpm.out-$seqno
    run_cmd "$epkg_cli" /tmp/epkg_rpm.out-$seqno

    # Normalize outputs
    normalize_output /tmp/host_rpm.out-$seqno > /tmp/host_rpm.norm
    normalize_output /tmp/epkg_rpm.out-$seqno > /tmp/epkg_rpm.norm

    # Compare normalized outputs
    if diff -u /tmp/host_rpm.norm /tmp/epkg_rpm.norm > /tmp/rpm.diff; then
        echo "  OK: Outputs match"
    else
        echo "  FAIL: Outputs differ"
        echo "  Diff (first 30 lines):"
        head -30 /tmp/rpm.diff | sed 's/^/    /'
        # For debugging, show raw outputs
	echo "  Host rpm raw output (first 20 lines) ($host_cli):"
        head -20 -v /tmp/host_rpm.out-$seqno | sed 's/^/    /'
	echo "  Epkg rpm raw output (first 20 lines) ($epkg_cli):"
        head -20 -v /tmp/epkg_rpm.out-$seqno | sed 's/^/    /'
    fi
    echo
}
# Compare error conditions
compare_error_condition() {
    local options="$1"
    local description="${2:-$options}"
    local allow_exit_mismatch="${3:-false}"
    echo "=== Testing $description ==="
    # Run host rpm
    run_cmd "$HOST_RPM $options" /tmp/host_rpm.out
    local host_exit=$?
    # Run epkg rpm
    run_cmd "$EPKG_RPM $options" /tmp/epkg_rpm.out
    local epkg_exit=$?
    if [ "$allow_exit_mismatch" != "true" ] && [ $host_exit -ne $epkg_exit ]; then
        echo "  FAIL: Exit codes differ (host=$host_exit, epkg=$epkg_exit)"
        echo "  Host rpm stderr (first 20 lines):"
        head -20 /tmp/host_rpm.out | sed 's/^/    /'
        echo "  Epkg rpm stderr (first 20 lines):"
        head -20 /tmp/epkg_rpm.out | sed 's/^/    /'
        return
    fi
    # Normalize outputs (strip colors, extra whitespace)
    normalize_output /tmp/host_rpm.out > /tmp/host_rpm.norm
    normalize_output /tmp/epkg_rpm.out > /tmp/epkg_rpm.norm
    if diff -u /tmp/host_rpm.norm /tmp/epkg_rpm.norm > /tmp/rpm.diff; then
        echo "  OK: Outputs match"
    else
        echo "  FAIL: Outputs differ"
        echo "  Diff (first 30 lines):"
        head -30 /tmp/rpm.diff | sed 's/^/    /'
	echo "  Host rpm raw output (first 20 lines) ($HOST_RPM $options):"
        head -20 /tmp/host_rpm.out | sed 's/^/    /'
	echo "  Epkg rpm raw output (first 20 lines) ($EPKG_RPM $options):"
        head -20 /tmp/epkg_rpm.out | sed 's/^/    /'
    fi
    echo
}

# Test abnormal conditions
test_abnormal_conditions() {
    echo "=== Testing abnormal conditions ==="

    # Non-existent file
    compare_error_condition "-qp /nonexistent/file.rpm" "non-existent file"

    # Invalid option - clap error, exit code may differ
    compare_error_condition "--invalid-option" "invalid option" "--invalid-option" "true"

    # Missing required argument for -p - clap error, exit code may differ
    compare_error_condition "-qp" "missing argument for -p" "true"

    # Missing required argument for -f - clap error, exit code may differ
    compare_error_condition "-qf" "missing argument for -f" "true"

    # Query for non-installed package
    compare_error_condition "-q nonexistentpackage" "query for non-installed package"

    # File ownership query for non-existent file
    compare_error_condition "-qf /nonexistent/file" "file ownership query for non-existent file"

    echo
}

# Start tests
echo "Running $(realpath ${BASH_SOURCE[0]})" "$@"
echo "Starting RPM conformance tests on RPM_FILE: $RPM_FILE"
echo "Using epkg binary: $EPKG_BIN"
echo

# Unpack once for reuse in compare_rpm_query cases
package_store_path=$($EPKG_BIN unpack "$RPM_FILE")
export PKGLINE=$(basename "$package_store_path")
seqno=0

# Basic query: package name (different pattern)
compare_rpm_query "" "basic package name query"

# Package file queries using helper
compare_rpm_query "-i" "package info"
# compare_rpm_query "-l" "package file list"
compare_rpm_query "--scripts" "package scripts"
compare_rpm_query "--triggers" "package triggers"
compare_rpm_query "--filetriggers" "package file triggers"
compare_rpm_query "--provides" "package provides"
compare_rpm_query "--requires" "package requires"
compare_rpm_query "--conflicts" "package conflicts"
compare_rpm_query "--obsoletes" "package obsoletes"
# dumb support for now
# compare_rpm_query "-V" "package verify"
# compare_rpm_query "-s" "package file states"

# Test abnormal conditions
# test_abnormal_conditions

exit 0

echo "=== All tests completed ==="
