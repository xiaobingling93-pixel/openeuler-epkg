#!/bin/sh
# Test that bash can be installed and /bin/sh is usable across all OSes

. "$(dirname "$0")/../vars.sh"
. "$(dirname "$0")/../lib.sh"

# Narrow OS list: 1) first script arg, 2) E2E_OS, 3) default ALL_OS from vars.sh
if [ -n "${1:-}" ]; then
	ALL_OS="$1"
elif [ -n "${E2E_OS:-}" ]; then
	ALL_OS="$E2E_OS"
fi

log "Starting bash installation and /bin/sh usability test (OS list: $ALL_OS)"

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
    if ! epkg -e "$env_name" run /bin/sh -c 'exit 0'; then
        error "/bin/sh not usable in $env_name"
    fi
    log "/bin/sh is usable in $env_name"
}

# Functions for the main loop

install_bash() {
    local env_name="$1"
    log "Installing bash in $env_name"
    epkg -e "$env_name" --assume-yes install --no-install-essentials bash || error "Failed to install bash in $env_name"
}

install_curl() {
    local env_name="$1"
    log "Installing curl in $env_name"
    epkg -e "$env_name" --assume-yes install --no-install-essentials curl || error "Failed to install curl in $env_name"
}

test_curl_bing() {
    local env_name="$1"
    if [ "${E2E_BACKEND:-}" = vm ]; then
        log "Skipping curl HTTPS test in E2E_BACKEND=vm (guest DNS is not guaranteed)"
        return
    fi
    log "Testing curl -I https://bing.com/ in $env_name"
    if ! epkg -e "$env_name" run curl -I https://bing.com/; then
        error "curl -I https://bing.com/ failed in $env_name"
    fi
    log "curl -I https://bing.com/ succeeded in $env_name"
}

test_epkg_list_via_bash() {
    local env_name="$1"
    local os="$2"
    local epkg_cmd list1 list2 diff_tmp1 diff_tmp2

    # Compare output (skip headers, separator, and total line) - skip for conda
    if [ "$os" = "conda" ]; then
        log "Skipping epkg list comparison for conda (epkg run runs in host OS not environment)"
        return
    fi

    log "Testing epkg list via bash command in $env_name"
    # Determine epkg command to use inside environment
    # In the e2e VM, PATH may resolve epkg to the guest stub (/usr/bin/epkg), not the same binary as
    # the harness; always use EPKG_BINARY so nested list matches the direct epkg wrapper.
    if [ "${E2E_BACKEND:-}" = vm ] && [ -n "${EPKG_BINARY:-}" ]; then
        epkg_cmd="$EPKG_BINARY"
        log "E2E_BACKEND=vm: using EPKG_BINARY for nested epkg: $epkg_cmd"
    elif epkg -e "$env_name" run bash -c "command -v epkg"; then
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

    # Test that epkg list works via bash command (nested epkg must use same -e as the wrapper)
    if ! epkg -e "$env_name" run bash -c "\"$epkg_cmd\" -e \"$env_name\" list"; then
        epkg -e "$env_name" run bash -c 'echo $PATH'
        error "epkg list via bash command failed in $env_name"
    fi

    # Compare output (skip headers, separator, and total line)
    list1=$(epkg -e "$env_name" list | tail -n +5 | grep -v '^Total' | grep -v '^$' | sort)
    list2=$(epkg -e "$env_name" run bash -c "\"$epkg_cmd\" -e \"$env_name\" list" | tail -n +5 | grep -v '^Total' | grep -v '^$' | sort)
    if [ "$list1" != "$list2" ]; then
        log "ERROR: epkg list output differs between direct and bash command in $env_name"
        # Show diff for easier debugging
        diff_tmp1=$(mktemp)
        diff_tmp2=$(mktemp)
        echo "$list1" > "$diff_tmp1"
        echo "$list2" > "$diff_tmp2"
        log "Diff (direct vs bash command):"
        diff -u "$diff_tmp1" "$diff_tmp2" >&2
        rm -f "$diff_tmp1" "$diff_tmp2"
        error "epkg list output differs between direct and bash command in $env_name"
    fi
    log "epkg list via bash command matches direct output"
}

test_package_manager_queries() {
    local env_name="$1"
    local os="$2"
    if [ "${E2E_BACKEND:-}" = vm ]; then
        log "Skipping rpm/dpkg queries in E2E_BACKEND=vm (epkg run uses direct exec; DB-backed tools need namespaces)"
        return
    fi
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
}

test_epkg_info_bash() {
    local env_name="$1"
    log "Testing epkg info bash in $env_name"
    if ! epkg -e "$env_name" info bash; then
        error "epkg info bash failed in $env_name"
    fi
}

test_epkg_search_paths() {
    local env_name="$1"
    local os="$2"
    case "$os" in
        openeuler|debian) # only test one for each format, since the filelist downloads for search are time consuming
            log "Testing epkg search --paths /bin/bash in $env_name"
            if ! epkg -e "$env_name" search --paths /bin/bash; then
                error "epkg search --paths /bin/bash failed in $env_name"
            fi
            ;;
    esac
}

cleanup_env() {
    local env_name="$1"
    log "Removing environment $env_name"
    epkg --assume-yes env remove "$env_name"
}

# Main test loop
for os in $ALL_OS; do
    log "Testing OS: $os"
    create_env "$os"

    # Install bash
    install_bash "$env_name"

    # Test /bin/sh
    test_sh "$env_name"

    # Test epkg list via bash command
    test_epkg_list_via_bash "$env_name" "$os"

    # Test package manager queries for bash
    test_package_manager_queries "$env_name" "$os"

    # Test epkg info bash
    test_epkg_info_bash "$env_name"

    # Test epkg search --paths /bin/bash (rpm/deb systems only)
    test_epkg_search_paths "$env_name" "$os"

    # Install curl and test curl https://bing.com/
    # This verifies ssl certs are properly installed.
    install_curl "$env_name"
    test_curl_bing "$env_name"

    # Clean up environment
    cleanup_env "$env_name"
done

log "All OSes passed bash installation and /bin/sh usability test"
