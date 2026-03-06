#!/bin/sh
# Minimal Elixir project: run script (needs Erlang).

. "$(dirname "$0")/../common.sh"

run_install elixir erlang
check_cmd elixir --version || lang_skip "no elixir for OS=$OS"

run_ebin elixir --version

run elixir -e "IO.puts(1+1)"
run elixir -e "IO.puts(\"ok\")"
lang_ok
