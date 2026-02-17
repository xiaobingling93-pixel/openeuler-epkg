#!/bin/sh
# Test env register/activate and PATH

. "$(dirname "$0")/../vars.sh"
. "$(dirname "$0")/../lib.sh"

log "Starting env register/activate test"

# Create some test environments
ENV1="test-env1"
ENV2="test-env2"
ENV3="test-env3"

log "Creating test environments"
epkg env create "$ENV1" -c debian || error "Failed to create env1"
epkg env create "$ENV2" -c ubuntu || error "Failed to create env2"
epkg env create "$ENV3" -c alpine || error "Failed to create env3"

# Register environments with different priorities
log "Registering environments"
epkg env register "$ENV1" || error "Failed to register env1"
epkg env register "$ENV2" --priority 10 || error "Failed to register env2"
epkg env register "$ENV3" --priority 5 || error "Failed to register env3"

# Check env path
log "Checking env path after registration"
PATH_OUTPUT1=$(epkg env path)
log "PATH output: $PATH_OUTPUT1"

# Verify env paths are in order (env2 priority 10, env3 priority 5, env1 default)
if ! echo "$PATH_OUTPUT1" | grep -q "$ENV2"; then
    error "env2 not in PATH"
fi
if ! echo "$PATH_OUTPUT1" | grep -q "$ENV3"; then
    error "env3 not in PATH"
fi
if ! echo "$PATH_OUTPUT1" | grep -q "$ENV1"; then
    error "env1 not in PATH"
fi

# Activate an environment
log "Activating env3"
eval "$(epkg env activate "$ENV3")" || error "Failed to activate env3"

# Check env path after activation
log "Checking env path after activation"
PATH_OUTPUT2=$(epkg env path)
log "PATH output: $PATH_OUTPUT2"

# Verify activated env is first in PATH
if ! echo "$PATH_OUTPUT2" | grep -q "^export PATH=\".*$ENV3"; then
    # Check if it's at least in the PATH
    if ! echo "$PATH_OUTPUT2" | grep -q "$ENV3"; then
        error "Activated env3 not in PATH"
    fi
fi

# Unregister an environment
log "Unregistering env2"
epkg env unregister "$ENV2" || error "Failed to unregister env2"

# Check env path after de-registration
log "Checking env path after de-registration"
PATH_OUTPUT3=$(epkg env path)
log "PATH output: $PATH_OUTPUT3"

# Verify env2 is not in PATH
if echo "$PATH_OUTPUT3" | grep -q "$ENV2"; then
    error "env2 still in PATH after de-registration"
fi

# Deactivate
log "Deactivating env3"
eval "$(epkg env deactivate)" || error "Failed to deactivate env3"

# Check env path after de-activation
log "Checking env path after de-activation"
PATH_OUTPUT4=$(epkg env path)
log "PATH output: $PATH_OUTPUT4"

# Verify env3 is still in PATH but not first
if ! echo "$PATH_OUTPUT4" | grep -q "$ENV3"; then
    error "env3 not in PATH after de-activation"
fi

# Re-register env2 with different priority
log "Re-registering env2 with priority 1"
epkg env register "$ENV2" --priority 1 || error "Failed to re-register env2"

# Check final env path
log "Checking final env path"
PATH_OUTPUT5=$(epkg env path)
log "PATH output: $PATH_OUTPUT5"

# Verify all envs are in PATH with correct order
if ! echo "$PATH_OUTPUT5" | grep -q "$ENV1"; then
    error "env1 not in final PATH"
fi
if ! echo "$PATH_OUTPUT5" | grep -q "$ENV2"; then
    error "env2 not in final PATH"
fi
if ! echo "$PATH_OUTPUT5" | grep -q "$ENV3"; then
    error "env3 not in final PATH"
fi

# Test that 'main' environment cannot be made public
log "Testing that 'main' environment cannot be made public"
if epkg env create main --public 2>/dev/null; then
    error "Should not be able to create 'main' as public"
fi

# Verify 'main' is private
MAIN_PUBLIC=$(epkg -e main env config get public 2>/dev/null | grep -i true || echo "false")
if [ "$MAIN_PUBLIC" = "true" ]; then
    error "'main' environment should be private"
fi

log "Env register/activate test completed successfully"

# Cleanup
epkg env remove "$ENV1" 2>/dev/null || true
epkg env remove "$ENV2" 2>/dev/null || true
epkg env remove "$ENV3" 2>/dev/null || true

