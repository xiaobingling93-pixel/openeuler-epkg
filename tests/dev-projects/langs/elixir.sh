#!/bin/sh
# Minimal Elixir project: run script (needs Erlang).

. "$(dirname "$0")/../common.sh"

# Brew: elixir depends on erlang which needs gcc for libstdc++.so dependency
if [ "$OS" = "brew" ]; then
    $EPKG_BIN -e "$ENV_NAME" --assume-yes install --ignore-missing gcc || true
fi
run_install elixir erlang
check_cmd elixir --version || lang_skip "no elixir for OS=$OS"

# Brew: run_ebin fails because elf-loader PATH doesn't include erl.
# Use 'run' with bash instead for brew.
if [ "$OS" = "brew" ]; then
    run bash -c "elixir --version"
else
    run_ebin elixir --version
fi

run elixir -e "IO.puts(1+1)"
run elixir -e "IO.puts(\"ok\")"
lang_ok
