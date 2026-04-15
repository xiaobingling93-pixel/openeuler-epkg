#!/bin/sh
# Minimal Zig project: build and run.

. "$(dirname "$0")/../common.sh"

# Brew: zig needs gcc for libstdc++.so dependency
if [ "$OS" = "brew" ]; then
    $EPKG_BIN -e "$ENV_NAME" --assume-yes install --ignore-missing gcc || true
fi
run_install zig
check_cmd zig version || lang_skip "no zig for OS=$OS"

run_ebin zig version

# msys2 has bash but no /bin/sh
if [ "$OS" = "msys2" ]; then
    SHELL_CMD="bash -c"
else
    SHELL_CMD="/bin/sh -c"
fi

# Zig needs writable global cache (avoid AccessDenied on host .cache)
run $SHELL_CMD 'mkdir -p /tmp/zigproj && cd /tmp/zigproj && echo "const std = @import(\"std\"); pub fn main() void { std.debug.print(\"ok\", .{}); }" > main.zig'
run $SHELL_CMD 'export ZIG_GLOBAL_CACHE_DIR=/tmp/zig-cache && cd /tmp/zigproj && zig run main.zig' | grep -q ok
lang_ok
