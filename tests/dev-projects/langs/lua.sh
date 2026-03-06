#!/bin/sh
# Minimal Lua project: run script.

. "$(dirname "$0")/../common.sh"

run_install lua lua5.4 lua5.3 lua50
check_cmd lua -e "print(1)" || check_cmd lua5.4 -e "print(1)" || lang_skip "no lua for OS=$OS"

run_ebin_if lua -e "print(1)"
run_ebin_if lua5.4 -e "print(1)"

run /bin/sh -c 'lua -e "print(1+1)" || lua5.4 -e "print(1+1)"'
run /bin/sh -c 'mkdir -p /tmp/luaproj && cd /tmp/luaproj && echo "print(\"ok\")" > main.lua'
run /bin/sh -c 'cd /tmp/luaproj && (lua main.lua || lua5.4 main.lua)' | grep -q ok
lang_ok
