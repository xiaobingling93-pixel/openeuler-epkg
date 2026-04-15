#!/bin/sh
# Minimal Erlang: run script.

. "$(dirname "$0")/../common.sh"

# Brew: erlang needs gcc for libstdc++.so dependency
if [ "$OS" = "brew" ]; then
    $EPKG_BIN -e "$ENV_NAME" --assume-yes install --ignore-missing gcc || true
fi
run_install erlang
check_cmd erl -eval "halt()." -noshell 2>/dev/null || lang_skip "no erlang for OS=$OS"

run_ebin_if erl -eval "halt()." -noshell

# msys2 has bash but no /bin/sh
if [ "$OS" = "msys2" ]; then
    SHELL_CMD="bash -c"
else
    SHELL_CMD="/bin/sh -c"
fi

run $SHELL_CMD 'erl -eval "io:format(\"ok~n\"), halt()." -noshell' | grep -q ok
lang_ok
