#!/bin/sh
# Test that bash can be installed and /bin/sh is usable across all OSes

. "$(dirname "$0")/../vars.sh"
. "$(dirname "$0")/../lib.sh"

log "Starting bash installation and /bin/sh usability test"

# Helper to create environment for an OS
create_env() {
    local os="$1"
    env_name="test-bash-$os"

    log "Creating environment for $os: $env_name"
    epkg env create "$env_name" -c "$os" || error "Failed to create environment $env_name for $os"
}

# Helper to test /bin/sh usability
test_sh() {
    local env_name="$1"

    log "Testing /bin/sh usability in $env_name"
    # Check if /bin/sh exists and can execute a simple command
    if ! epkg -e "$env_name" run /bin/sh -c 'exit 0' >/dev/null 2>&1; then
        error "/bin/sh not usable in $env_name"
    fi
    log "/bin/sh is usable in $env_name"
}

# Main test loop
for os in $ALL_OS; do
    log "Testing OS: $os"
    create_env "$os"

    # Install bash
    log "Installing bash in $env_name"
    epkg -e "$env_name" --assume-yes install --no-install-essentials bash || error "Failed to install bash in $env_name"

    # Test /bin/sh
    test_sh "$env_name"

    # Test that epkg can auto-detect environment when run inside via 'epkg run'
    # Requirement: epkg should read /etc/epkg/env.yaml to determine active environment
    # without needing EPKG_ACTIVE_ENV or -e flag when executed inside the environment namespace.
    log "Testing epkg list via bash command in $env_name"
    # Determine epkg command to use inside environment
    if epkg -e "$env_name" run bash -c "command -v epkg" >/dev/null 2>&1; then
        # epkg is in PATH inside environment
        epkg_cmd="epkg"
        log "epkg found in PATH inside environment"
    else
        # epkg not in PATH, use absolute path from EPKG_BINARY
        if [ -z "$EPKG_BINARY" ]; then
            error "EPKG_BINARY not set, cannot test epkg list via bash command"
        fi
        epkg_cmd="$EPKG_BINARY"
        log "Using absolute path to epkg: $epkg_cmd"
    fi

    # Test that epkg list works via bash command
    if ! epkg -e "$env_name" run bash -c 'cmd="$1"; "$cmd" list' -- "$epkg_cmd" >/dev/null 2>&1; then
        error "epkg list via bash command failed in $env_name"
    fi

    # Compare output (skip headers, separator, and total line)
    list1=$(epkg -e "$env_name" list | tail -n +5 | grep -v '^Total' | grep -v '^$' | sort)
    list2=$(epkg -e "$env_name" run bash -c 'cmd="$1"; "$cmd" list' -- "$epkg_cmd" | tail -n +5 | grep -v '^Total' | grep -v '^$' | sort)
    if [ "$list1" != "$list2" ]; then
        log "ERROR: epkg list output differs between direct and bash command in $env_name"
        log "Direct output (first 10 lines):"
        echo "$list1" | head -10 >&2
        log "Bash command output (first 10 lines):"
        echo "$list2" | head -10 >&2
        error "epkg list output differs between direct and bash command in $env_name"
    fi
    log "epkg list via bash command matches direct output"

    # Test package manager queries for bash
    case "$os" in
        openeuler|fedora)
            log "Testing rpm -q -a for bash in $env_name"
            if ! epkg -e "$env_name" run rpm -q -a | grep -q bash; then
                error "rpm -q -a does not show bash in $env_name"
            fi
            ;;
        debian|ubuntu)
            log "Testing dpkg-query -l for bash in $env_name"
            if ! epkg -e "$env_name" run dpkg-query -l | grep -q '^ii.*bash'; then
                error "dpkg-query -l does not show bash in $env_name"
            fi
            ;;
    esac

    # Test epkg info bash
    log "Testing epkg info bash in $env_name"
    if ! epkg -e "$env_name" info bash >/dev/null 2>&1; then
        error "epkg info bash failed in $env_name"
    fi

    # Test epkg search --paths /bin/bash (rpm/deb systems only)
    case "$os" in
        openeuler|debian) # only test one for each format, since the filelist downloads for search are time consuming
            log "Testing epkg search --paths /bin/bash in $env_name"
            if ! epkg -e "$env_name" search --paths /bin/bash >/dev/null 2>&1; then
                error "epkg search --paths /bin/bash failed in $env_name"
            fi
            ;;
    esac

    # Clean up environment
    log "Removing environment $env_name"
    epkg --assume-yes env remove "$env_name" 2>/dev/null || true
done

log "All OSes passed bash installation and /bin/sh usability test"
