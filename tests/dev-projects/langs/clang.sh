#!/bin/sh
# Minimal C project with clang: build and run.

. "$(dirname "$0")/../common.sh"

run_install build-base clang llvm build-essential clang

# Alpine installs clang as clang21, but provides clang symlink
# Check for clang first, then versioned variants
CLANG_CMD=""
for cmd in clang clang-21 clang21; do
    if check_cmd $cmd --version 2>/dev/null; then
        CLANG_CMD="$cmd"
        break
    fi
done
[ -n "$CLANG_CMD" ] || lang_skip "no clang for OS=$OS"

run_ebin $CLANG_CMD --version

# msys2/conda on Windows have bash but no /bin/sh
if [ "$OS" = "msys2" ] || [ "$OS" = "conda" ]; then
    SHELL_CMD="bash -c"
else
    SHELL_CMD="/bin/sh -c"
fi

run $SHELL_CMD "mkdir -p /tmp/clangproj && cd /tmp/clangproj && printf '%s\n' '#include <stdio.h>' 'int main(void) { puts(\"ok\"); return 0; }' > main.c"
run $SHELL_CMD "cd /tmp/clangproj && $CLANG_CMD -o hello main.c && ./hello" | grep -q ok
lang_ok
