#!/bin/sh
# Minimal Erlang: run script.

. "$(dirname "$0")/../common.sh"

run_install erlang
check_cmd erl -eval "halt()." -noshell 2>/dev/null || lang_skip "no erlang for OS=$OS"

run_ebin_if erl -eval "halt()." -noshell

run /bin/sh -c 'erl -eval "io:format(\"ok~n\"), halt()." -noshell' | grep -q ok
lang_ok
