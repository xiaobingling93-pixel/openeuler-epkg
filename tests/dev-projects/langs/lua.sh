#!/bin/sh
# Minimal Lua project: run script.

. "$(dirname "$0")/../common.sh"

run_install lua lua5.4 lua5.3 lua50
check_cmd lua -e "print(1)" || check_cmd lua5.4 -e "print(1)" || lang_skip "no lua for OS=$OS"

run_ebin_if lua -e "print(1)"
run_ebin_if lua5.4 -e "print(1)"

# Find the lua command
LUA_CMD=""
if check_cmd lua -e "print(1)" 2>/dev/null; then
    LUA_CMD="lua"
elif check_cmd lua5.4 -e "print(1)" 2>/dev/null; then
    LUA_CMD="lua5.4"
fi

run $LUA_CMD -e "print(1+1)"

# Create test file - use lua for conda/Windows (no /bin/sh)
if [ "$OS" = "conda" ]; then
    run $LUA_CMD -e "os.execute('mkdir -p /tmp/luaproj'); f = io.open('/tmp/luaproj/main.lua', 'w'); f:write('print(\"ok\")'); f:close()"
    run $LUA_CMD /tmp/luaproj/main.lua | grep -q ok
else
    run /bin/sh -c 'mkdir -p /tmp/luaproj && cd /tmp/luaproj && echo "print(\"ok\")" > main.lua'
    run /bin/sh -c 'cd /tmp/luaproj && (lua main.lua || lua5.4 main.lua)' | grep -q ok
fi
lang_ok
