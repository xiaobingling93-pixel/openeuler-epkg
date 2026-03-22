#!/bin/sh
# Minimal Crystal project: build and run.

. "$(dirname "$0")/../common.sh"

run_install crystal
check_cmd crystal --version || lang_skip "no crystal for OS=$OS"

run_ebin crystal --version

# msys2 has bash but no /bin/sh
if [ "$OS" = "msys2" ]; then
    SHELL_CMD="bash -c"
else
    SHELL_CMD="/bin/sh -c"
fi

run $SHELL_CMD 'mkdir -p /tmp/crystalproj && cd /tmp/crystalproj && echo "puts \"ok\"" > main.cr'
run $SHELL_CMD 'cd /tmp/crystalproj && crystal run main.cr' | grep -q ok
lang_ok
