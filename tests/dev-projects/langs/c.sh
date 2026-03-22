#!/bin/sh
# Minimal C project: gcc/make, build and run.

. "$(dirname "$0")/../common.sh"

run_install build-base gcc make build-essential
check_cmd gcc --version || lang_skip "no gcc/make for OS=$OS"

run_ebin gcc --version

# msys2 has bash but no /bin/sh
if [ "$OS" = "msys2" ]; then
    SHELL_CMD="bash -c"
else
    SHELL_CMD="/bin/sh -c"
fi

run $SHELL_CMD 'mkdir -p /tmp/cproj && cd /tmp/cproj && printf "%s\n" "#include <stdio.h>" "int main(void) { puts(\"ok\"); return 0; }" > main.c'
run $SHELL_CMD 'cd /tmp/cproj && gcc -o hello main.c && ./hello' | grep -q ok

run $SHELL_CMD 'cd /tmp/cproj && printf "all: hello\nhello: main.c\n\tgcc -o hello main.c\n" > Makefile && make && ./hello' | grep -q ok
lang_ok
