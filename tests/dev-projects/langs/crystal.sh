#!/bin/sh
# Minimal Crystal project: build and run.

. "$(dirname "$0")/../common.sh"

run_install crystal gcc
check_cmd crystal --version || lang_skip "no crystal for OS=$OS"

run_ebin crystal --version

# msys2/conda on Windows have bash but no /bin/sh
# brew: host /bin/sh fails in namespace (vdso_time SIGSEGV), use brew's bash
if [ "$OS" = "msys2" ] || [ "$OS" = "conda" ]; then
    SHELL_CMD="bash -c"
elif [ "$OS" = "brew" ]; then
    SHELL_CMD="bash -c"
else
    SHELL_CMD="/bin/sh -c"
fi

run $SHELL_CMD 'mkdir -p /tmp/crystalproj && cd /tmp/crystalproj && echo "puts \"ok\"" > main.cr'
run $SHELL_CMD 'cd /tmp/crystalproj && crystal run main.cr'
lang_ok
