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
    epkg -e "$env_name" --assume-yes install bash || error "Failed to install bash in $env_name"

    # Test /bin/sh
    test_sh "$env_name"

    # Clean up environment
    log "Removing environment $env_name"
    epkg --assume-yes env remove "$env_name" 2>/dev/null || true
done

log "All OSes passed bash installation and /bin/sh usability test"
