#!/bin/sh
# Minimal C project: gcc/make, build and run.

. "$(dirname "$0")/../common.sh"

run_install build-base gcc make build-essential

# Find available C compiler (gcc, gcc-*, or clang)
CC=""
if check_cmd gcc --version; then
    CC=gcc
elif check_cmd clang --version; then
    CC=clang
else
    # Try versioned gcc (e.g., gcc-15 on Homebrew)
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
# brew is native macOS packages, use host shell directly
if [ "$OS" = "msys2" ]; then
    SHELL_CMD="bash -c"
elif [ "$OS" = "brew" ]; then
    # For brew, use host shell and run_ebin for compilation
    SHELL_CMD="/bin/sh -c"
else
    SHELL_CMD="/bin/sh -c"
fi

# For brew, compile and run using ebin wrappers directly (native execution)
# On macOS, Homebrew packages are native and can run on host directly.
# On Linux (linuxbrew), packages need sandbox due to glibc version mismatch
# between host ld.so and Homebrew libc.so.6.
if [ "$OS" = "brew" ]; then
    # Check if we're on Linux - need sandbox for linuxbrew
    if [ "$(uname -s)" = "Linux" ]; then
        # Linuxbrew: compile inside sandbox, run inside sandbox
        run sh -c 'mkdir -p /tmp/cproj && cd /tmp/cproj && printf "%s\n" "#include <stdio.h>" "int main(void) { puts(\"ok\"); return 0; }" > main.c'
        run sh -c "cd /tmp/cproj && $CC -o hello main.c && ./hello" | grep -q ok
        run sh -c "cd /tmp/cproj && printf 'all: hello\nhello: main.c\n\t$CC -o hello main.c\n' > Makefile && make && ./hello" | grep -q ok
    else
        # macOS Homebrew: native execution on host
        mkdir -p /tmp/cproj
        cd /tmp/cproj
        printf "%s\n" "#include <stdio.h>" "int main(void) { puts(\"ok\"); return 0; }" > main.c
        "$ENV_ROOT/ebin/$CC" -o hello main.c && ./hello | grep -q ok || exit 1
        printf 'all: hello\nhello: main.c\n\t%s -o hello main.c\n' "$CC" > Makefile
        "$ENV_ROOT/ebin/make" && ./hello | grep -q ok || exit 1
    fi
else
    run $SHELL_CMD 'mkdir -p /tmp/cproj && cd /tmp/cproj && printf "%s\n" "#include <stdio.h>" "int main(void) { puts(\"ok\"); return 0; }" > main.c'
    run $SHELL_CMD "cd /tmp/cproj && $CC -o hello main.c && ./hello" | grep -q ok
    run $SHELL_CMD "cd /tmp/cproj && printf 'all: hello\nhello: main.c\n\t$CC -o hello main.c\n' > Makefile && make && ./hello" | grep -q ok
fi
lang_ok
