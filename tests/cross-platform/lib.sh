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