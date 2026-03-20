#!/bin/sh
# Shared test functions for cross-platform channel tests
# Sourced by channels/*.sh test scripts.

# Detect the Python command name for the current platform
# On Windows, conda only provides 'python.exe', not 'python3.exe'
_get_python_cmd() {
    if [ "$OS" = "Windows_NT" ]; then
        echo "python"
    else
        echo "python3"
    fi
}

# Test: install and verify a package runs
# Usage: test_install_run <pkg> <cmd> [args...]
# Example: test_install_run python python --version
test_install_run() {
    local pkg="$1"
    shift
    run_install "$pkg" || return 1
    run "$@" || return 1
    return 0
}

# Test: programming language interpreter
# Usage: test_lang_python / test_lang_perl / test_lang_ruby / test_lang_nodejs / test_lang_go
test_lang_python() {
    local py_cmd="$(_get_python_cmd)"
    # Different package names in different channels
    if [ "$CHANNEL_NAME" = "brew" ]; then
        run_install python || return 1
        run $py_cmd --version || return 1
        run $py_cmd -c "print('Hello from Python')" || return 1
    else
        run_install python || return 1
        run $py_cmd --version || return 1
        run $py_cmd -c "print('Hello from Python')" || return 1
    fi
    return 0
}

test_lang_perl() {
    run_install perl || return 1
    run perl -e 'print "Hello from Perl\n"' || return 1
    run perl -v | head -2 || return 1
    return 0
}

test_lang_ruby() {
    run_install ruby || return 1
    run ruby --version || return 1
    run ruby -e 'puts "Hello from Ruby"' || return 1
    return 0
}

test_lang_nodejs() {
    run_install nodejs node || return 1
    run node --version || return 1
    run node -e "console.log('Hello from Node.js')" || return 1
    return 0
}

test_lang_go() {
    run_install go || return 1
    run go version || return 1
    return 0
}

# Test: build tools
test_build_cmake() {
    run_install cmake || return 1
    run cmake --version || return 1
    return 0
}

test_build_make() {
    run_install make || return 1
    # brew's make package installs gmake, not make
    if [ "$CHANNEL_NAME" = "brew" ]; then
        run gmake --version || return 1
    else
        run make --version || return 1
    fi
    return 0
}

test_build_ninja() {
    run_install ninja || return 1
    run ninja --version || return 1
    return 0
}

# Test: scientific computing
test_scipy_numpy() {
    run_install numpy || return 1
    local py_cmd="$(_get_python_cmd)"
    run $py_cmd -c "import numpy; print('numpy version:', numpy.__version__)" || return 1
    return 0
}

test_scipy_scipy() {
    run_install scipy || return 1
    local py_cmd="$(_get_python_cmd)"
    run $py_cmd -c "import scipy; print('scipy version:', scipy.__version__)" || return 1
    return 0
}

test_scipy_pandas() {
    run_install pandas || return 1
    local py_cmd="$(_get_python_cmd)"
    run $py_cmd -c "import pandas; print('pandas version:', pandas.__version__)" || return 1
    return 0
}

# Test: ML/AI
test_ml_scikit() {
    run_install scikit-learn || return 1
    local py_cmd="$(_get_python_cmd)"
    run $py_cmd -c "import sklearn; print('sklearn version:', sklearn.__version__)" || return 1
    return 0
}

# Test: utilities
test_util_jq() {
    run_install jq || return 1
    run jq --version || return 1
    run jq . <<< '{"test":1}' || return 1
    return 0
}

test_util_curl() {
    run_install curl || return 1
    run curl --version || return 1
    return 0
}

test_util_wget() {
    run_install wget || return 1
    run wget --version || return 1
    return 0
}

test_util_sed() {
    run_install sed || return 1
    run sed --version || return 1
    return 0
}

#========================================
# Standard Test Suites
#========================================

# Test Suite 1: Utility packages
# Usage: test_suite_utils [skip_list]
# Example: test_suite_utils "curl sed"  # skip curl and sed
test_suite_utils() {
    local skip_list="$1"

    # jq - JSON processor (common to most channels)
    if ! echo "$skip_list" | grep -qw "jq"; then
        test_util_jq || channel_skip "no jq for channel=$CHANNEL_NAME"
    fi

    # tree - directory listing
    if ! echo "$skip_list" | grep -qw "tree"; then
        run_install tree || channel_skip "no tree for channel=$CHANNEL_NAME"
        run tree --version || channel_skip "tree not found"
    fi
}

# Test Suite 2: Programming Languages
# Usage: test_suite_langs [skip_list]
test_suite_langs() {
    local skip_list="$1"

    # Python
    if ! echo "$skip_list" | grep -qw "python"; then
        test_lang_python
    fi

    # Perl
    if ! echo "$skip_list" | grep -qw "perl"; then
        test_lang_perl
    fi

    # Ruby
    if ! echo "$skip_list" | grep -qw "ruby"; then
        # Skip ruby for brew - libyaml dependency conflicts with perl (both have "Changes" file)
        if [ "$CHANNEL_NAME" != "brew" ]; then
            test_lang_ruby
        fi
    fi

    # Node.js
    if ! echo "$skip_list" | grep -qw "nodejs"; then
        # Different package names in different channels
        if [ "$CHANNEL_NAME" = "brew" ]; then
            run_install node
        else
            run_install nodejs node
        fi
        run node -e "console.log('Hello from Node.js')"
    fi

    # Go
    if ! echo "$skip_list" | grep -qw "go"; then
        test_lang_go
    fi
}

# Test Suite 3: Build Systems
# Usage: test_suite_build [skip_list]
test_suite_build() {
    local skip_list="$1"

    # cmake
    if ! echo "$skip_list" | grep -qw "cmake"; then
        test_build_cmake
    fi

    # make
    if ! echo "$skip_list" | grep -qw "make"; then
        test_build_make
    fi

    # ninja
    if ! echo "$skip_list" | grep -qw "ninja"; then
        test_build_ninja
    fi
}

# Test Suite 4: Scientific Computing
# Usage: test_suite_scipy [skip_list]
test_suite_scipy() {
    local skip_list="$1"

    # numpy
    if ! echo "$skip_list" | grep -qw "numpy"; then
        test_scipy_numpy
    fi

    # scipy
    if ! echo "$skip_list" | grep -qw "scipy"; then
        test_scipy_scipy
    fi

    # pandas
    if ! echo "$skip_list" | grep -qw "pandas"; then
        test_scipy_pandas
    fi
}

# Test Suite 5: Machine Learning
# Usage: test_suite_ml [skip_list]
test_suite_ml() {
    local skip_list="$1"

    # scikit-learn
    if ! echo "$skip_list" | grep -qw "scikit"; then
        test_ml_scikit
    fi
}

# Test Suite 6: Package Management
# Usage: test_suite_pkgmgr <package_to_remove>
test_suite_pkgmgr() {
    local pkg_to_remove="${1:-tree}"

    # Remove package
    run_remove "$pkg_to_remove"

    # List installed packages
    epkg list | head -30

    # Search for package
    epkg search jq | head -20

    # Show package info
    epkg info jq | head -20
}

# Test Suite 7: Query Commands
# Tests various epkg query commands (info, search, list)
test_suite_queries() {
    local skip_list="$1"

    # epkg info - show package information
    if ! echo "$skip_list" | grep -qw "info"; then
        epkg info python | head -20 || return 1
    fi

    # epkg search - search for packages
    if ! echo "$skip_list" | grep -qw "search"; then
        epkg search python | head -10 || return 1
    fi

    # epkg list - list installed packages
    if ! echo "$skip_list" | grep -qw "list"; then
        epkg list | head -10 || return 1
    fi

    return 0
}

# Test Suite 8: History and Restore
# Tests epkg history and restore commands
test_suite_history() {
    local skip_list="$1"

    # epkg history - show environment history
    if ! echo "$skip_list" | grep -qw "history"; then
        epkg history
    fi

    # epkg restore - restore to previous generation
    if ! echo "$skip_list" | grep -qw "restore"; then
        epkg restore -1
    fi

    return 0
}

# Test Suite 9: Environment Export/Import
# Tests epkg env export and import commands
test_suite_env_io() {
    local skip_list="$1"
    local export_file="/tmp/epkg-env-export-$CHANNEL_NAME.yaml"

    # epkg env export - export environment configuration
    if ! echo "$skip_list" | grep -qw "export"; then
        epkg env export > "$export_file" || return 1
        # Verify export file has content
        if [ ! -s "$export_file" ]; then
            echo "Export file is empty" >&2
            return 1
        fi
        echo "Exported environment to $export_file"
        head -20 "$export_file"
    fi

    # Note: import is tested during env create, not here
    # as it would conflict with existing env

    return 0
}

# Test Suite 10: Garbage Collection
# Tests epkg gc command
test_suite_gc() {
    local skip_list="$1"

    # epkg gc - clean up unused cache and store files
    if ! echo "$skip_list" | grep -qw "gc"; then
        epkg gc || return 1
    fi

    return 0
}

# Test Suite 11: Package Upgrade
# Tests epkg upgrade command
test_suite_upgrade() {
    local skip_list="$1"

    # epkg upgrade - upgrade packages
    if ! echo "$skip_list" | grep -qw "upgrade"; then
        epkg list --upgradable | head -10
        epkg upgrade
    fi

    return 0
}

# Test Suite 12: List Variants
# Tests epkg list with different options
test_suite_list_variants() {
    local skip_list="$1"

    # epkg list --installed (default)
    if ! echo "$skip_list" | grep -qw "list_installed"; then
        epkg list --installed | head -10 || return 1
    fi

    # epkg list --upgradable
    if ! echo "$skip_list" | grep -qw "list_upgradable"; then
        epkg list --upgradable | head -10 || return 1
    fi

    # epkg list with pattern (faster than --all/--available)
    if ! echo "$skip_list" | grep -qw "list_pattern"; then
        epkg list "python*" | head -10 || return 1
    fi

    # Note: --all and --available are very slow (31000+ packages)
    # They are tested separately or skipped for quick tests

    return 0
}

# Test Suite 13: Environment Management
# Tests epkg env commands
test_suite_env() {
    local skip_list="$1"
    local test_env="${ENV_NAME}-test"

    # epkg env create (already tested in setup, test with --root)
    if ! echo "$skip_list" | grep -qw "env_create"; then
        "$EPKG_BIN" env create "$test_env" -c "$CHANNEL_NAME"
    fi

    # epkg env list (via epkg env without args)
    if ! echo "$skip_list" | grep -qw "env_list"; then
        "$EPKG_BIN" env list | head -10
    fi

    # epkg env remove
    if ! echo "$skip_list" | grep -qw "env_remove"; then
        "$EPKG_BIN" env remove "$test_env"
    fi

    # epkg env path
    if ! echo "$skip_list" | grep -qw "env_path"; then
        epkg env path || return 1
    fi

    # epkg env config get
    if ! echo "$skip_list" | grep -qw "env_config"; then
        epkg env config get name
    fi

    return 0
}

# Test Suite 14: Repo Commands
# Tests epkg repo commands
test_suite_repo() {
    local skip_list="$1"

    # epkg repo list
    if ! echo "$skip_list" | grep -qw "repo_list"; then
        epkg repo list || return 1
    fi

    return 0
}

# Test Suite 15: Run Variants
# Tests epkg run with different scenarios
test_suite_run() {
    local skip_list="$1"
    local py_cmd="$(_get_python_cmd)"

    # epkg run with python (reliable cross-platform)
    if ! echo "$skip_list" | grep -qw "run_python"; then
        epkg install python
        epkg run $py_cmd --version || return 1
    fi

    # epkg run with python -c
    if ! echo "$skip_list" | grep -qw "run_python_c"; then
        epkg run $py_cmd -c "import os; print('Run test OK')" || return 1
    fi

    return 0
}

# Test Suite 16: Search Variants
# Tests epkg search with different patterns
test_suite_search() {
    local skip_list="$1"

    # epkg search with pattern
    if ! echo "$skip_list" | grep -qw "search_pattern"; then
        epkg search "python" | head -10 || return 1
    fi

    # epkg search for file (if supported)
    if ! echo "$skip_list" | grep -qw "search_file"; then
        epkg search "*/bin/python" | head -10
    fi

    return 0
}

# Test Suite 17: Info Variants
# Tests epkg info with different inputs
test_suite_info() {
    local skip_list="$1"

    # epkg info for installed package
    if ! echo "$skip_list" | grep -qw "info_installed"; then
        epkg info python | head -20
    fi

    # epkg info for available package
    if ! echo "$skip_list" | grep -qw "info_available"; then
        epkg info jq | head -20 || return 1
    fi

    return 0
}

# Test Suite 18: Dry Run
# Tests --dry-run flag
test_suite_dry_run() {
    local skip_list="$1"

    # epkg install --dry-run (use existing package for realistic test)
    if ! echo "$skip_list" | grep -qw "dry_install"; then
        echo "Testing: epkg --dry-run install curl"
        epkg --dry-run install curl
    fi

    # epkg remove --dry-run
    if ! echo "$skip_list" | grep -qw "dry_remove"; then
        echo "Testing: epkg --dry-run remove curl"
        epkg --dry-run remove curl
    fi

    return 0
}