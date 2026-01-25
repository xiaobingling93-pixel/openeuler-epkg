#!/bin/sh
# Test install/remove/upgrade/run --help

set -e

. "$(dirname "$0")/../vars.sh"
. "$(dirname "$0")/../lib.sh"

# Configuration constants
if [ "$LIGHT_TEST" = 1 ]; then
    BATCH_SIZE=5
    MAX_BATCHES=2
else
    # stress test
    BATCH_SIZE=20
    MAX_BATCHES=100
fi

# Helper: check whether the current batch has any error, either via
# EPKG_BATCH_ERROR or a no_exit=true marker in the epkg output.
batch_has_error() {
    if [ "${EPKG_BATCH_ERROR:-0}" -ne 0 ]; then
        return 0
    fi
    echo "${EPKG_BATCH_CMD_OUTPUT:-}" | grep -q "Command '.*' exited with code .* (no_exit=true, continuing)"
}

# Test all epkg --help commands
test_epkg_help_commands() {
    log "Testing epkg --help"
    epkg --help >/dev/null || error "epkg --help failed"
    epkg install --help >/dev/null || error "epkg install --help failed"
    epkg remove --help >/dev/null || error "epkg remove --help failed"
    epkg upgrade --help >/dev/null || error "epkg upgrade --help failed"
    epkg list --help >/dev/null || error "epkg list --help failed"
}

# Create test environment for an OS
create_test_environment() {
    local os="$1"
    local env_name="test-$os"

    log "Creating environment for $os"
    if ! epkg env create "$env_name" -c "$os"; then
        error "Failed to create environment for $os"
        return 1
    fi
    return 0
}

# Get available package list for an OS
get_package_list() {
    local os="$1"
    local env_name="test-$os"

    log "Getting available packages for $os"
    epkg -e "$env_name" list --available 2>/dev/null | awk 'NR>3 && /^[A_]/ {print $2}'
}

# Isolate problematic package set by repeatedly calling process_batch with reduced
# package subsets and checking for any errors (epkg failures, run_cmd_help failures,
# or no_exit messages in the epkg output). This performs a simple delta-reduction:
# try removing one package at a time and keep any smaller subset that still fails,
# so we converge towards a minimal failing set (which may contain 1 or more packages).
isolate_problematic_package() {
    local os="$1"
    local batch_pkgs="$2"

    log "Attempting to isolate problematic packages for $os..."

    # Current failing set we are trying to minimize
    local current_pkgs="$batch_pkgs"

    # Helper to count words in a list
    _count_pkgs() {
        echo "$1" | tr -s ' ' '\n' | sed '/^$/d' | wc -l
    }

    while :; do
        # Normalize whitespace
        current_pkgs=$(echo "$current_pkgs" | tr -s ' ' ' ')
        local count
        count=$(_count_pkgs "$current_pkgs")

        # If 0 or 1 package remains, we cannot reduce further
        if [ "$count" -le 1 ]; then
            break
        fi

        log "Current failing set for $os (size=$count): $current_pkgs"

        local reduced_found=0

        # Try removing each package and see if the remaining subset still fails
        for pkg_to_remove in $current_pkgs; do
            if [ -z "$pkg_to_remove" ]; then
                continue
            fi

            # Build subset without this package
            local subset
            subset=$(echo "$current_pkgs" | tr ' ' '\n' | sed "/^${pkg_to_remove}\$/d" | tr '\n' ' ')
            subset=$(echo "$subset" | tr -s ' ' ' ')

            # Skip if subset is empty
            if [ -z "$subset" ]; then
                continue
            fi

            log "Testing subset without '$pkg_to_remove': $subset"

            # Reset batch state
            EPKG_BATCH_CMD_OUTPUT=""
            EPKG_BATCH_ERROR=0

            # Use a dedicated batch number label for isolation runs
            process_batch "$os" "isolation" "$subset"

            # Check for any error conditions on this subset
            if batch_has_error; then
                # Subset still fails; we can reduce to this smaller set
                log "Subset without '$pkg_to_remove' still fails, reducing search space"
                current_pkgs="$subset"
                reduced_found=1
                break
            fi
        done

        # If no smaller failing subset was found, we cannot reduce further
        if [ "$reduced_found" -eq 0 ]; then
            break
        fi
    done

    log "Final minimal failing package set for $os: $current_pkgs"
    log "To reproduce this failure from the project root, run:"
    log "  tests/e2e/test.sh install-remove-upgrade/test-install-remove-upgrade.sh $os $current_pkgs"
    error "Test failed: minimal failing package set for $os is: $current_pkgs"
}

# Test executables with --help
test_executables() {
    local env_name="$1"
    local batch_pkgs="$2"

    log "Finding installed executables"
    local executables
    executables=$(
        # List executables directly from the environment's /usr/bin directory
        if [ -d "/root/.epkg/envs/$env_name/usr/bin" ]; then
            (cd "/root/.epkg/envs/$env_name" && ls usr/bin | sed 's#^#/usr/bin/#')
        fi
    )

    # Test --help on executables
    for exe in $executables; do
        if [ -z "$exe" ]; then
            continue
        fi

        if ! run_cmd_help /root/.epkg/envs/$env_name"$exe" "$env_name"; then
            log "WARNING: run_cmd_help failed for $exe"
            # Mark batch as having errors; isolation is handled at the batch level.
            EPKG_BATCH_ERROR=1
        fi
    done
}

# Process a batch of packages: install, upgrade, test, remove
process_batch() {
    local os="$1"
    local env_name="test-$os"
    local batch_num="$2"
    local batch_pkgs="$3"

    log "Processing batch $batch_num for $os"

    # Reset batch state for this run
    EPKG_BATCH_CMD_OUTPUT=""
    EPKG_BATCH_ERROR=0

    # Install packages with --prefer-low-version
    log "Installing packages in batch $batch_num"
    local install_output install_exit
    install_output=$(epkg -e "$env_name" --assume-yes install --prefer-low-version $batch_pkgs 2>&1)
    install_exit=$?
    # Accumulate output for later analysis
    EPKG_BATCH_CMD_OUTPUT="${EPKG_BATCH_CMD_OUTPUT}
${install_output}"
    if [ $install_exit -ne 0 ]; then
        log "WARNING: Installation failed for some packages in batch $batch_num"
        EPKG_BATCH_ERROR=1
    fi

    # Upgrade
    log "Running upgrade"
    local upgrade_output upgrade_exit
    upgrade_output=$(epkg -e "$env_name" --assume-yes upgrade 2>&1)
    upgrade_exit=$?
    EPKG_BATCH_CMD_OUTPUT="${EPKG_BATCH_CMD_OUTPUT}
${upgrade_output}"
    if [ $upgrade_exit -ne 0 ]; then
        log "WARNING: Upgrade failed in batch $batch_num"
        EPKG_BATCH_ERROR=1
    fi

    # Test executables
    test_executables "$env_name" "$batch_pkgs"

    # Remove half of the packages
    local pkg_count
    pkg_count=$(echo "$batch_pkgs" | wc -w)
    local remove_count=$((pkg_count / 2))
    local remove_pkgs
    remove_pkgs=$(echo "$batch_pkgs" | tr ' ' '\n' | head -n $remove_count | tr '\n' ' ')
    log "Removing packages: $remove_pkgs"
    local remove_output remove_exit
    remove_output=$(epkg -e "$env_name" --assume-yes remove $remove_pkgs 2>&1)
    remove_exit=$?
    EPKG_BATCH_CMD_OUTPUT="${EPKG_BATCH_CMD_OUTPUT}
${remove_output}"
    if [ $remove_exit -ne 0 ]; then
        log "WARNING: Removal failed for some packages in batch $batch_num"
        EPKG_BATCH_ERROR=1
    fi
}

# Process all packages for a single OS
process_os() {
    local os="$1"
    local env_name="test-$os"

    log "Testing OS: $os"

    # Create environment
    create_test_environment "$os"

    # Get package list
    local pkg_list
    pkg_list=$(get_package_list "$os")
    if [ $? -ne 0 ]; then
        cleanup_environment "$os"
        return 1
    fi

    # Create shuffled list
    local shuffled_list
    shuffled_list=$(echo "$pkg_list" | shuf | head -n $((BATCH_SIZE * MAX_BATCHES)))

    # Process in batches
    local batch_count=0
    local batch_pkgs=""

    for pkg in $shuffled_list; do
        if [ -z "$pkg" ]; then
            continue
        fi

        batch_pkgs="$batch_pkgs $pkg"
        local pkg_count
        pkg_count=$(echo "$batch_pkgs" | wc -w)

        if [ "$pkg_count" -ge "$BATCH_SIZE" ] || [ "$batch_count" -ge "$MAX_BATCHES" ]; then
            batch_count=$((batch_count + 1))
            process_batch "$os" "$batch_count" "$batch_pkgs"

            # If any error (epkg failures, run_cmd_help failures, or no_exit marker)
            # was observed during this batch, attempt to isolate the problematic package(s).
            if batch_has_error; then
                isolate_problematic_package "$os" "$batch_pkgs"
            fi

            # Clear batch
            batch_pkgs=""

            # Stop if we've processed enough batches
            if [ "$batch_count" -ge "$MAX_BATCHES" ]; then
                break
            fi
        fi
    done

    # Clean up
    cleanup_environment "$os"
}

# Clean up test environment
cleanup_environment() {
    local os="$1"
    local env_name="test-$os"

    log "Cleaning up environment for $os"
    epkg env remove "$env_name"
}

# Main execution
main() {
    log "Starting install/remove/upgrade test"

    # Test epkg commands
    test_epkg_help_commands

    # If arguments are provided, treat them as:
    #   $1: OS name
    #   $2...: explicit package list to test (single batch)
    #
    # This allows reproducing a failing batch directly via:
    #   tests/e2e/test.sh install-remove-upgrade/test-install-remove-upgrade.sh <os> <packages...>
    if [ "$#" -ge 1 ]; then
        local os="$1"
        shift

        if [ "$#" -gt 0 ]; then
            local pkg_list="$*"
            log "Reproducing failure for OS '$os' with explicit package list: $pkg_list"

            # Create a dedicated environment and run a single batch with the
            # provided packages. Any isolation is still handled by
            # isolate_problematic_package/process_batch.
            create_test_environment "$os"
            process_batch "$os" "repro" "$pkg_list"
            cleanup_environment "$os"
            batch_has_error && error "batch failed"
        else
            # Only override the OS list (no explicit packages)
            ALL_OS="$os"
            for os in $ALL_OS; do
                process_os "$os"
            done
        fi
    else
        # Default behavior: test all OSes
        for os in $ALL_OS; do
            process_os "$os"
        done
    fi

    log "Install/remove/upgrade test completed successfully"
}

# Run main function
main "$@"
