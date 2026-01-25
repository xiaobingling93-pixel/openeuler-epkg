#!/bin/sh
# Test public mode and multi-user functionality

set -e
set -o pipefail

. "$(dirname "$0")/../vars.sh"
. "$(dirname "$0")/../lib.sh"

log "Starting public multi-user test"
[ -n "$INTERACTIVE" ] && set -x

# For root: create public environment and install ripgrep and busybox-static
log "Root: creating public environment and installing ripgrep and busybox-static"
epkg env create --public alpine -c alpine || error "Failed to create public env for root"
epkg -e alpine --assume-yes install ripgrep busybox-static || error "Failed to install ripgrep and busybox-static for root"

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
chown $USER_A $USER_A_HOME/.epkg
chown $USER_B $USER_B_HOME/.epkg
mount -o uid=$(id -u $USER_A) -t tmpfs tmpfs "$USER_A_HOME/.epkg/envs" || error "mount tmpfs"
mount -o uid=$(id -u $USER_B) -t tmpfs tmpfs "$USER_B_HOME/.epkg/envs" || error "mount tmpfs"

# For user A: light install with shared store
# log "Installing epkg for user A (shared light)"
# run_as_user "$USER_A" "epkg self install --store shared" || error "Failed to install for user A"

# For user B: light install with auto=shared store
log "Installing epkg for user B (auto=private)"
run_as_user "$USER_B" "epkg self install --store auto" || error "Failed to install for user B"

# For user A: create public environment and install jq
# log "User A: creating public environment and installing jq"
# run_as_user "$USER_A" "epkg env create --public puba -c archlinux" || error "Failed to create public env for user A"
# ls -C /opt/epkg/envs/$USER_A/puba || error "No public env dir created"
# run_as_user "$USER_A" "epkg -e puba --assume-yes install jq" || error "Failed to install jq for user A"
# /opt/epkg/envs/ dir in env is empty if separate mounted
# epkg -e alpine run busybox.static ls /opt/epkg/envs/$USER_A/puba || error "Public env dir not visible in env"

# For user B: create private environment and install htop
log "User B: creating private environment and installing htop"
run_as_user "$USER_B" "epkg env create privb -c archlinux" || error "Failed to create private env for user B"
# ls -C /opt/epkg/envs/$USER_B/privb || error "No private env dir created"
run_as_user "$USER_B" "epkg -e privb --assume-yes install htop" || error "Failed to install htop for user B"
# /opt/epkg/envs/ dir in env is empty if separate mounted
# run_as_user "$USER_B" "epkg -e privb run ls -C /opt/epkg/envs/$USER_B/privb" || error "Private env dir not visible in env"

# Verify that each user can see their own envs + others' public envs
log "Verifying env list for user A"
ENV_LIST_A=$(run_as_user "$USER_A" "epkg env list")
# if ! echo "$ENV_LIST_A" | grep -q "puba"; then
#     error "User A cannot see their own public env"
# fi
if ! echo "$ENV_LIST_A" | grep -q "alpine"; then
    error "User A cannot see root's public env"
fi

log "Verifying env list for user B"
ENV_LIST_B=$(run_as_user "$USER_B" "epkg env list")
if ! echo "$ENV_LIST_B" | grep -q "privb"; then
    error "User B cannot see their own private env"
fi
# if ! echo "$ENV_LIST_B" | grep -q "puba"; then
#     error "User B cannot see user A's public env"
# fi
if ! echo "$ENV_LIST_B" | grep -q "alpine"; then
    error "User B cannot see root's public env"
fi

log "Verifying env list for root"
ENV_LIST_ROOT=$(epkg env list 2>/dev/null)
if ! echo "$ENV_LIST_ROOT" | grep -q "alpine"; then
    error "Root cannot see their own public env"
fi
# if ! echo "$ENV_LIST_ROOT" | grep -q "puba"; then
#     error "Root cannot see user A's public env"
# fi

# Verify that each user can run their own installed commands
# log "User A: testing jq command"
# if ! run_as_user "$USER_A" "epkg -e puba run jq --version"; then
#     error "User A cannot run jq"
# fi

log "User B: testing htop command"
if ! run_as_user "$USER_B" "epkg -e privb run htop --version"; then
    error "User B cannot run htop"
fi

# Test owner/env_name format for accessing other users' public envs
# log "User B: testing owner/env_name format to access user A's public env"
# if ! run_as_user "$USER_B" "epkg -e ${USER_A}/puba run jq --version"; then
#     error "User B cannot access user A's public env using owner/env_name format"
# fi

# Test owner/env_name format for root's public env
log "User A: testing owner/env_name format to access root's public env"
if ! run_as_user "$USER_A" "epkg -e root/alpine run rg --version"; then
    error "User A cannot access root's public env using owner/env_name format"
fi

# log "Root: testing jq from user A's public env"
# if ! epkg -e ${USER_A}/puba run jq --version; then
#     error "Root cannot run jq from user A's public env"
# fi

# Test owner/env_name format for root accessing user A's public env
# log "Root: testing owner/env_name format to access user A's public env"
# if ! epkg -e "${USER_A}/puba" run jq --version; then
#     error "Root cannot access user A's public env using owner/env_name format"
# fi

# Verify that 'main' environment is always private
log "Verifying that 'main' environment is always private"
if run_as_user "$USER_A" "epkg env create main --public" 2>/dev/null; then
    error "Should not be able to create 'main' as public"
fi

# Verify that SELF_ENV.public is always true
log "Verifying SELF_ENV public attribute is always true"
USER_A_SELF_PUBLIC=$(run_as_user "$USER_A" "epkg -e self env config get public" 2>/dev/null | grep -o true || echo "false")
if [ "$USER_A_SELF_PUBLIC" != "true" ]; then
    error "SELF_ENV should always be public (true) for user A"
fi

# Test info command on other user's public env
# log "User B: testing info command on user A's public env"
# if ! run_as_user "$USER_B" "epkg -e ${USER_A}/puba info jq" | grep -q "jq$"; then
#     error "User B cannot get info about jq from user A's public env"
# fi

log "User A: testing info command on root's public env"
if ! run_as_user "$USER_A" "epkg -e root/alpine info ripgrep" | grep -q "ripgrep$"; then
    error "User A cannot get info about ripgrep from root's public env"
fi

# log "Root: testing info command on user A's public env"
# if ! epkg -e "${USER_A}/puba" info jq | grep -q "jq$"; then
#     error "Root cannot get info about jq from user A's public env"
# fi

# Test list command on other user's public env
# log "User B: testing list command on user A's public env"
# LIST_OUTPUT_B=$(run_as_user "$USER_B" "epkg -e ${USER_A}/puba list --installed")
# if ! echo "$LIST_OUTPUT_B" | grep -q "jq"; then
#     error "User B cannot list installed packages from user A's public env"
# fi

log "User A: testing list command on root's public env"
LIST_OUTPUT_A=$(run_as_user "$USER_A" "epkg -e root/alpine list --installed")
if ! echo "$LIST_OUTPUT_A" | grep -q "ripgrep"; then
    error "User A cannot list installed packages from root's public env"
fi
if ! echo "$LIST_OUTPUT_A" | grep -q "busybox-static"; then
    error "User A cannot see busybox-static in root's public env"
fi

# log "Root: testing list command on user A's public env"
# LIST_OUTPUT_ROOT=$(epkg -e "${USER_A}/puba" list --installed)
# if ! echo "$LIST_OUTPUT_ROOT" | grep -q "jq"; then
#     error "Root cannot list installed packages from user A's public env"
# fi

# Test search command on other user's public env
# log "User B: testing search command on user A's public env"
# if ! run_as_user "$USER_B" "epkg -e ${USER_A}/puba search jq" | grep '^jq'; then
#     error "User B cannot search for jq in user A's public env"
# fi

log "User A: testing search command on root's public env"
if ! run_as_user "$USER_A" "epkg -e root/alpine search ripgrep" | grep '^ripgrep'; then
    error "User A cannot search for ripgrep in root's public env"
fi
if ! run_as_user "$USER_A" "epkg -e root/alpine search busybox" | grep '^busybox'; then
    error "User A cannot search for busybox in root's public env"
fi

# log "Root: testing search command on user A's public env"
# if ! epkg -e "${USER_A}/puba" search jq | grep '^jq'; then
#     error "Root cannot search for jq in user A's public env"
# fi

log "Public multi-user test completed successfully"

# Cleanup
run_as_user "$USER_A" "epkg env remove puba"
run_as_user "$USER_B" "epkg env remove privb"
epkg env remove alpine

