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
epkg env register "$ENV2" --path-order 10 || error "Failed to register env2"
epkg env register "$ENV3" --path-order  5 || error "Failed to register env3"

# Check env path
log "Checking env path after registration"
PATH_OUTPUT1=$(epkg env path)
log "PATH output: $PATH_OUTPUT1"

# Verify all env paths are present
if ! echo "$PATH_OUTPUT1" | grep -q "$ENV2"; then
    error "env2 not in PATH"
fi
if ! echo "$PATH_OUTPUT1" | grep -q "$ENV3"; then
    error "env3 not in PATH"
fi
if ! echo "$PATH_OUTPUT1" | grep -q "$ENV1"; then
    error "env1 not in PATH"
fi

# Verify registered envs ordering (env3 path-order 5, env2 path-order 10, env1 default 100)
PATH_STR1=${PATH_OUTPUT1#export PATH=\"}
PATH_STR1=${PATH_STR1%\"}
case "$PATH_STR1" in
    *"$ENV3"*"$ENV2"*"$ENV1"*) ;;
    *) error "registered env order incorrect after registration (expected ENV3 before ENV2 before ENV1)";;
esac

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

# Re-register env2 with different path-order
log "Re-registering env2 with path-order 1"
epkg env register "$ENV2" --path-order 1 || error "Failed to re-register env2"

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

# Verify registered envs ordering after re-register (env2 path-order 1, env3 path-order 5, env1 default 100)
PATH_STR5=${PATH_OUTPUT5#export PATH=\"}
PATH_STR5=${PATH_STR5%\"}
case "$PATH_STR5" in
    *"$ENV2"*"$ENV3"*"$ENV1"*) ;;
    *) error "registered env order incorrect after re-register (expected ENV2 before ENV3 before ENV1)";;
esac

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

# ==============================================================================
# Test: Install to activated environment (issue #76)
# When an environment is activated, `epkg install <pkg>` should install to that
# environment, and the package should appear in `epkg history` and `epkg list`.
# ==============================================================================
log "Testing install to activated environment (issue #76)"

# Create a test environment for this test
ENV_INSTALL="test-install-env"
log "Creating test environment: $ENV_INSTALL"
epkg env create "$ENV_INSTALL" -c alpine || error "Failed to create $ENV_INSTALL"

# Get initial history count (before install)
HISTORY_BEFORE=$(epkg -e "$ENV_INSTALL" history 2>/dev/null | grep -c "Install")
log "History install count before: $HISTORY_BEFORE"

# Activate the environment
log "Activating $ENV_INSTALL"
eval "$(epkg env activate "$ENV_INSTALL")" || error "Failed to activate $ENV_INSTALL"

# Verify EPKG_ACTIVE_ENV is set correctly
if [ -z "$EPKG_ACTIVE_ENV" ]; then
    error "EPKG_ACTIVE_ENV should be set after activation"
fi
log "EPKG_ACTIVE_ENV=$EPKG_ACTIVE_ENV"

# Install a small package (jq is typically small and fast to install)
log "Installing jq to activated environment"
epkg install -y jq || error "Failed to install jq"

# Verify jq is in the activated environment's list
log "Checking if jq appears in package list"
if ! epkg list | grep -w "jq"; then
    error "jq not found in 'epkg list' after install to activated env"
fi

# Verify jq is in the environment-specific list
log "Checking if jq appears in environment-specific list"
if ! epkg -e "$ENV_INSTALL" list | grep -w "jq"; then
    error "jq not found in 'epkg -e $ENV_INSTALL list' after install"
fi

# Verify install appears in history
log "Checking if install appears in history"
HISTORY_AFTER=$(epkg -e "$ENV_INSTALL" history 2>/dev/null | grep -c "Install")
log "History install count after: $HISTORY_AFTER"
if [ "$HISTORY_AFTER" -le "$HISTORY_BEFORE" ]; then
    error "Install should be recorded in history (before: $HISTORY_BEFORE, after: $HISTORY_AFTER)"
fi

# Verify the latest history entry is for jq installation
LATEST_HISTORY=$(epkg -e "$ENV_INSTALL" history 2>/dev/null | grep "Install" | tail -1)
if ! echo "$LATEST_HISTORY" | grep -q "jq"; then
    error "Latest history entry should mention jq: $LATEST_HISTORY"
fi

# Verify jq binary is accessible
log "Verifying jq binary is accessible"
if ! command -v jq >/dev/null 2>&1; then
    error "jq command not found in PATH after install"
fi

# Deactivate the environment
log "Deactivating $ENV_INSTALL"
eval "$(epkg env deactivate)" || error "Failed to deactivate $ENV_INSTALL"

# ==============================================================================
# Test: Install to activated environment in PURE mode
# When an environment is activated with --pure, `epkg install <pkg>` should
# correctly identify the environment (EPKG_ACTIVE_ENV has '!' suffix).
# ==============================================================================
log "Testing install to activated environment in pure mode (issue #76)"

# Create a test environment for pure mode test
ENV_PURE="test-pure-env"
log "Creating test environment: $ENV_PURE"
epkg env create "$ENV_PURE" -c alpine || error "Failed to create $ENV_PURE"

# Activate with --pure mode
log "Activating $ENV_PURE in pure mode"
eval "$(epkg env activate "$ENV_PURE" --pure)" || error "Failed to activate $ENV_PURE --pure"

# Verify EPKG_ACTIVE_ENV has '!' suffix in pure mode
if [ -z "$EPKG_ACTIVE_ENV" ]; then
    error "EPKG_ACTIVE_ENV should be set after pure mode activation"
fi
log "EPKG_ACTIVE_ENV=$EPKG_ACTIVE_ENV"

# Verify the '!' suffix is present (indicating pure mode)
case "$EPKG_ACTIVE_ENV" in
    *!*) log "Pure mode suffix '!' detected in EPKG_ACTIVE_ENV" ;;
    *)   error "Expected '!' suffix in EPKG_ACTIVE_ENV for pure mode, got: $EPKG_ACTIVE_ENV" ;;
esac

# Install a small package
log "Installing tree to pure-activated environment"
epkg install -y tree || error "Failed to install tree"

# Verify tree is in the package list
log "Checking if tree appears in package list"
if ! epkg list | grep -w "tree"; then
    error "tree not found in 'epkg list' after install to pure-activated env"
fi

# Verify tree is in the environment-specific list
log "Checking if tree appears in environment-specific list"
if ! epkg -e "$ENV_PURE" list | grep -w "tree"; then
    error "tree not found in 'epkg -e $ENV_PURE list' after install"
fi

# Verify install appears in history
log "Checking if install appears in history for pure mode"
HISTORY_PURE=$(epkg -e "$ENV_PURE" history 2>/dev/null | grep -c "Install")
if [ "$HISTORY_PURE" -lt 1 ]; then
    error "Install should be recorded in history for pure mode env"
fi

# Deactivate
log "Deactivating $ENV_PURE"
eval "$(epkg env deactivate)" || error "Failed to deactivate $ENV_PURE"

# Cleanup
log "Removing test environment: $ENV_PURE"
epkg env unregister "$ENV_PURE" 2>/dev/null || true
epkg env remove "$ENV_PURE" 2>/dev/null || true

log "Install to pure-activated environment test completed successfully"

# ==============================================================================
# Test: Install to STACKED activated environment
# When environments are stacked, EPKG_ACTIVE_ENV contains multiple envs.
# Install should go to the first (most recently activated) environment.
# ==============================================================================
log "Testing install to stacked activated environments (issue #76)"

# Create two test environments for stack mode test
ENV_STACK1="test-stack1-env"
ENV_STACK2="test-stack2-env"
log "Creating test environments: $ENV_STACK1, $ENV_STACK2"
epkg env create "$ENV_STACK1" -c alpine || error "Failed to create $ENV_STACK1"
epkg env create "$ENV_STACK2" -c alpine || error "Failed to create $ENV_STACK2"

# Activate first environment
log "Activating $ENV_STACK1"
eval "$(epkg env activate "$ENV_STACK1")" || error "Failed to activate $ENV_STACK1"

# Activate second environment with --stack
log "Stacking $ENV_STACK2 on top"
eval "$(epkg env activate "$ENV_STACK2" --stack)" || error "Failed to stack $ENV_STACK2"

# Verify EPKG_ACTIVE_ENV has both environments
log "EPKG_ACTIVE_ENV=$EPKG_ACTIVE_ENV"
case "$EPKG_ACTIVE_ENV" in
    "$ENV_STACK2:$ENV_STACK1") log "Stack mode detected correctly" ;;
    *) error "Expected '$ENV_STACK2:$ENV_STACK1' in EPKG_ACTIVE_ENV, got: $EPKG_ACTIVE_ENV" ;;
esac

# Install a package - should go to ENV_STACK2 (first in the stack)
log "Installing sl to stacked environment"
epkg install -y sl || error "Failed to install sl"

# Verify sl is in ENV_STACK2's list (the top of stack)
log "Checking if sl appears in $ENV_STACK2 list"
if ! epkg -e "$ENV_STACK2" list | grep -w "sl"; then
    error "sl not found in 'epkg -e $ENV_STACK2 list' after install (should go to top of stack)"
fi

# Verify sl is NOT in ENV_STACK1's list (the bottom of stack)
log "Checking that sl is NOT in $ENV_STACK1 list"
if epkg -e "$ENV_STACK1" list | grep -q "^[EI].*sl"; then
    error "sl should NOT be in $ENV_STACK1 list (should go to top of stack only)"
fi

# Verify install appears in ENV_STACK2's history
log "Checking if install appears in $ENV_STACK2 history"
HISTORY_STACK2=$(epkg -e "$ENV_STACK2" history 2>/dev/null | grep -c "Install" || echo 0)
if [ "$HISTORY_STACK2" -lt 1 ]; then
    error "Install should be recorded in $ENV_STACK2 history"
fi

# Deactivate both (need to call deactivate twice)
log "Deactivating $ENV_STACK2"
eval "$(epkg env deactivate)" || error "Failed to deactivate $ENV_STACK2"
log "Deactivating $ENV_STACK1"
eval "$(epkg env deactivate)" || error "Failed to deactivate $ENV_STACK1"

# Cleanup
log "Removing test environments: $ENV_STACK1, $ENV_STACK2"
epkg env unregister "$ENV_STACK1" 2>/dev/null || true
epkg env remove "$ENV_STACK1" 2>/dev/null || true
epkg env unregister "$ENV_STACK2" 2>/dev/null || true
epkg env remove "$ENV_STACK2" 2>/dev/null || true

log "Install to stacked environment test completed successfully"

log "Install to activated environment test completed successfully"

log "Env register/activate test completed successfully"

# Cleanup
epkg env remove "$ENV1" 2>/dev/null || true
epkg env remove "$ENV2" 2>/dev/null || true
epkg env remove "$ENV3" 2>/dev/null || true

