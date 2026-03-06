#!/bin/sh
# Minimal Crystal project: build and run.

. "$(dirname "$0")/../common.sh"

run_install crystal
check_cmd crystal --version || lang_skip "no crystal for OS=$OS"

run_ebin crystal --version

run /bin/sh -c 'mkdir -p /tmp/crystalproj && cd /tmp/crystalproj && echo "puts \"ok\"" > main.cr'
run /bin/sh -c 'cd /tmp/crystalproj && crystal run main.cr' | grep -q ok
lang_ok
