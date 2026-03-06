#!/bin/sh
# Minimal Zig project: build and run.

. "$(dirname "$0")/../common.sh"

run_install zig
check_cmd zig version || lang_skip "no zig for OS=$OS"

run_ebin zig version

# Zig needs writable global cache (avoid AccessDenied on host .cache)
run /bin/sh -c 'mkdir -p /tmp/zigproj && cd /tmp/zigproj && echo "const std = @import(\"std\"); pub fn main() void { std.debug.print(\"ok\", .{}); }" > main.zig'
run /bin/sh -c 'export ZIG_GLOBAL_CACHE_DIR=/tmp/zig-cache && cd /tmp/zigproj && zig run main.zig' | grep -q ok
lang_ok
