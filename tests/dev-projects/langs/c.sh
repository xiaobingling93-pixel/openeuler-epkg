#!/bin/sh
# Minimal C project: gcc/make, build and run.

. "$(dirname "$0")/../common.sh"

run_install build-base gcc make build-essential

# Find available C compiler
CC=""
if check_cmd gcc --version; then
    CC=gcc
elif check_cmd clang --version; then
    CC=clang
else
    for gcc_ver in gcc-15 gcc-14 gcc-13 gcc-12 gcc-11; do
        if check_cmd $gcc_ver --version; then
            CC=$gcc_ver
            break
        fi
    done
fi
[ -n "$CC" ] || lang_skip "no C compiler found for OS=$OS"
run_ebin $CC --version

# msys2 has bash but no /bin/sh
if [ "$OS" = "msys2" ]; then
    SHELL_CMD="bash -c"
else
    SHELL_CMD="/bin/sh -c"
fi

# For brew, compile and run using ebin wrappers directly (native execution)
# - macOS: Homebrew packages are native Mach-O binaries
# - Linux: elf-loader handles brew mount (env_root -> /home/linuxbrew/.linuxbrew)
if [ "$OS" = "brew" ]; then
    mkdir -p /tmp/cproj
    cd /tmp/cproj
    printf "%s\n" "#include <stdio.h>" "int main(void) { puts(\"ok\"); return 0; }" > main.c
    "$ENV_ROOT/ebin/$CC" -o hello main.c && ./hello | grep -q ok || exit 1
    printf 'all: hello\nhello: main.c\n\t%s -o hello main.c\n' "$CC" > Makefile
    "$ENV_ROOT/ebin/make" && ./hello | grep -q ok || exit 1
else
    run $SHELL_CMD 'mkdir -p /tmp/cproj && cd /tmp/cproj && printf "%s\n" "#include <stdio.h>" "int main(void) { puts(\"ok\"); return 0; }" > main.c'
    run $SHELL_CMD "cd /tmp/cproj && $CC -o hello main.c && ./hello" | grep -q ok
    run $SHELL_CMD "cd /tmp/cproj && printf 'all: hello\nhello: main.c\n\t$CC -o hello main.c\n' > Makefile && make && ./hello" | grep -q ok
fi
lang_ok
