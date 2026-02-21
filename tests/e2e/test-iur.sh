#!/bin/sh
# Run install-remove-upgrade tests with predefined matrix
# This script runs heavy-weight install/remove/upgrade tests that are
# skipped from test-all.sh to avoid accumulating cache on developer machines
# Supports debug mode with -d/-dd flags.

. "$(dirname "$0")/host-vars.sh"

# Parse command line flags
DEBUG_FLAG=""
while [ $# -gt 0 ] && [ "${1#-}" != "$1" ]; do
    case "$1" in
        -h|--help)
            echo "Usage: $0 [-d|--debug|-dd]"
            echo "Run install-remove-upgrade tests with predefined matrix."
            exit 0
            ;;
        -dd)
            DEBUG_FLAG="-dd"
            ;;
        -d|--debug)
            DEBUG_FLAG="-d"
            ;;
        *)
            echo "Unknown option: $1" >&2
            exit 1
            ;;
    esac
    shift
done

SCRIPT_DIR="$(dirname "$0")"
FAILED_TESTS=""
PASSED_TESTS=""

# Predefined test matrix
# Format: OS:PACKAGES (space-separated packages)
# We test a small but representative set of packages for each OS
TEST_MATRIX="
openeuler:curl wget vim
fedora:curl wget vim nano
debian:curl wget vim nano tree
ubuntu:curl wget vim nano tree htop
alpine:curl wget vim busybox rpm2cpio
archlinux:curl wget vim nano base-devel
conda:python numpy pandas
"

# Run tests for each entry in the matrix
while IFS=: read os packages; do
    # Skip empty lines
    if [ -z "$os" ] || [ -z "$packages" ]; then
        continue
    fi

    echo "========================================="
    echo "Testing OS: $os with packages: $packages"
    echo "========================================="

    # Run the install-remove-upgrade test with specific OS and packages
    if "$SCRIPT_DIR/test-one.sh" $DEBUG_FLAG "install-remove-upgrade/test-install-remove-upgrade.sh" "$os" $packages; then
        echo "PASSED: $os with packages: $packages"
        PASSED_TESTS="$PASSED_TESTS ${os}:$(echo $packages | tr ' ' ',')"
    else
        echo "FAILED: $os with packages: $packages"
        FAILED_TESTS="$FAILED_TESTS ${os}:$(echo $packages | tr ' ' ',')"
    fi
    echo ""
done <<EOF
$TEST_MATRIX
EOF

# Also run a comprehensive test with "all-os" but limited packages
echo "========================================="
echo "Testing all OSes with limited package set"
echo "========================================="

if "$SCRIPT_DIR/test-one.sh" $DEBUG_FLAG "install-remove-upgrade/test-install-remove-upgrade.sh" "all-os" curl wget vim; then
    echo "PASSED: all-os with curl,wget,vim"
    PASSED_TESTS="$PASSED_TESTS all-os:curl,wget,vim"
else
    echo "FAILED: all-os with curl,wget,vim"
    FAILED_TESTS="$FAILED_TESTS all-os:curl,wget,vim"
fi

# Summary
echo "========================================="
echo "Test Summary"
echo "========================================="
echo "Passed tests:"
if [ -n "$PASSED_TESTS" ]; then
    echo "$PASSED_TESTS" | tr ' ' '\n' | sed 's/^/- /'
else
    echo "None"
fi

echo ""
echo "Failed tests:"
if [ -n "$FAILED_TESTS" ]; then
    echo "$FAILED_TESTS" | tr ' ' '\n' | sed 's/^/- /'
    exit 1
else
    echo "None"
fi

exit 0
