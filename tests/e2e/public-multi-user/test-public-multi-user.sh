#!/bin/sh
# Test public mode and multi-user functionality

set -e

. "$(dirname "$0")/../vars.sh"
. "$(dirname "$0")/../lib.sh"

log "Starting public multi-user test"

# For root: create public environment and install ripgrep and busybox-static
log "Root: creating public environment and installing ripgrep and busybox-static"
epkg env create --public alpine -c alpine || error "Failed to create public env for root"
epkg -e alpine install --assume-yes ripgrep busybox-static || error "Failed to install ripgrep and busybox-static for root"

# Find busybox path in alpine environment
if [ -x "/opt/epkg/envs/root/alpine/usr/bin/busybox.static" ]; then
    BUSYBOX="/opt/epkg/envs/root/alpine/usr/bin/busybox.static"
elif [ -x "/opt/epkg/envs/root/alpine/bin/busybox.static" ]; then
    BUSYBOX="/opt/epkg/envs/root/alpine/bin/busybox.static"
elif [ -n "$(command -v busybox)" ]; then
    BUSYBOX="$(command -v busybox)"
else
    error "Could not find busybox in alpine environment"
fi
log "Using busybox at: $BUSYBOX"

# Helper function to run commands as a user using busybox su
run_as_user() {
    local user="$1"
    shift
    "$BUSYBOX" su -s /bin/sh "$user" -c "$*"
}

# Create test users
USER_A="epkgtest_a"
USER_B="epkgtest_b"

log "Creating test users"
if ! id "$USER_A" >/dev/null 2>&1; then
    adduser -D -s /bin/sh "$USER_A" || error "Failed to create user A"
fi

if ! id "$USER_B" >/dev/null 2>&1; then
    adduser -D -s /bin/sh "$USER_B" || error "Failed to create user B"
fi

# Mount tmpfs for user environments
log "Mounting tmpfs for user environments"
USER_A_HOME=$(getent passwd "$USER_A" | cut -d: -f6)
USER_B_HOME=$(getent passwd "$USER_B" | cut -d: -f6)
mkdir -p "$USER_A_HOME/.epkg/envs" "$USER_B_HOME/.epkg/envs"
mount -t tmpfs tmpfs "$USER_A_HOME/.epkg/envs"
mount -t tmpfs tmpfs "$USER_B_HOME/.epkg/envs"

# For user A: install with shared store
log "Installing epkg for user A with shared store"
run_as_user "$USER_A" "epkg self install --store shared" || error "Failed to install for user A"

# For user B: install with shared store
log "Installing epkg for user B with shared store"
run_as_user "$USER_B" "epkg self install --store shared" || error "Failed to install for user B"

# For user A: create public environment and install jq
log "User A: creating public environment and installing jq"
run_as_user "$USER_A" "epkg env create --public puba -c archlinux" || error "Failed to create public env for user A"
run_as_user "$USER_A" "epkg -e puba install --assume-yes jq" || error "Failed to install jq for user A"

# For user B: create private environment and install htop
log "User B: creating private environment and installing htop"
run_as_user "$USER_B" "epkg env create privb -c archlinux" || error "Failed to create private env for user B"
run_as_user "$USER_B" "epkg -e privb install --assume-yes htop" || error "Failed to install htop for user B"

# Verify that each user can see their own envs + others' public envs
log "Verifying env list for user A"
ENV_LIST_A=$(run_as_user "$USER_A" "epkg env list")
if ! echo "$ENV_LIST_A" | grep -q "puba"; then
    error "User A cannot see their own public env"
fi
if ! echo "$ENV_LIST_A" | grep -q "alpine"; then
    error "User A cannot see root's public env"
fi

log "Verifying env list for user B"
ENV_LIST_B=$(run_as_user "$USER_B" "epkg env list")
if ! echo "$ENV_LIST_B" | grep -q "privb"; then
    error "User B cannot see their own private env"
fi
if ! echo "$ENV_LIST_B" | grep -q "puba"; then
    error "User B cannot see user A's public env"
fi
if ! echo "$ENV_LIST_B" | grep -q "alpine"; then
    error "User B cannot see root's public env"
fi

log "Verifying env list for root"
ENV_LIST_ROOT=$(epkg env list 2>/dev/null)
if ! echo "$ENV_LIST_ROOT" | grep -q "alpine"; then
    error "Root cannot see their own public env"
fi
if ! echo "$ENV_LIST_ROOT" | grep -q "puba"; then
    error "Root cannot see user A's public env"
fi

# Verify that each user can run their own installed commands
log "User A: testing jq command"
if ! run_as_user "$USER_A" "epkg -e puba run jq --version"; then
    error "User A cannot run jq"
fi

log "User B: testing htop command"
if ! run_as_user "$USER_B" "epkg -e privb run htop --version"; then
    error "User B cannot run htop"
fi

# Verify that users can run commands from public envs
log "User B: testing jq from user A's public env"
if ! run_as_user "$USER_B" "epkg -e puba run jq --version"; then
    error "User B cannot run jq from user A's public env"
fi

log "User A: testing ripgrep from root's public env"
if ! run_as_user "$USER_A" "epkg -e alpine run rg --version"; then
    error "User A cannot run ripgrep from root's public env"
fi

log "Root: testing jq from user A's public env"
if ! epkg -e puba run jq --version; then
    error "Root cannot run jq from user A's public env"
fi

log "Public multi-user test completed successfully"

# Cleanup
run_as_user "$USER_A" "epkg env remove puba"
run_as_user "$USER_B" "epkg env remove privb"
epkg env remove alpine

