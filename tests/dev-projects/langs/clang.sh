#!/bin/sh
# Minimal C project with clang: build and run.

. "$(dirname "$0")/../common.sh"

run_install build-base clang llvm build-essential clang
check_cmd clang --version || lang_skip "no clang for OS=$OS"

run_ebin clang --version

# msys2/conda on Windows have bash but no /bin/sh
if [ "$OS" = "msys2" ] || [ "$OS" = "conda" ]; then
    SHELL_CMD="bash -c"
else
    SHELL_CMD="/bin/sh -c"
fi

run $SHELL_CMD 'mkdir -p /tmp/clangproj && cd /tmp/clangproj && printf "%s\n" "#include <stdio.h>" "int main(void) { puts(\"ok\"); return 0; }" > main.c'
run $SHELL_CMD 'cd /tmp/clangproj && clang -o hello main.c && ./hello' | grep -q ok
lang_ok
