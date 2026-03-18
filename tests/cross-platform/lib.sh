#!/bin/sh
# Shared test functions for cross-platform channel tests
# Sourced by channels/*.sh test scripts.

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
    run_install python python3 || return 1
    run python --version || return 1
    run python -c "print('Hello from Python')" || return 1
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
    run make --version || return 1
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
    run python -c "import numpy; print('numpy version:', numpy.__version__)" || return 1
    return 0
}

test_scipy_scipy() {
    run_install scipy || return 1
    run python -c "import scipy; print('scipy version:', scipy.__version__)" || return 1
    return 0
}

test_scipy_pandas() {
    run_install pandas || return 1
    run python -c "import pandas; print('pandas version:', pandas.__version__)" || return 1
    return 0
}

# Test: ML/AI
test_ml_scikit() {
    run_install scikit-learn || return 1
    run python -c "import sklearn; print('sklearn version:', sklearn.__version__)" || return 1
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