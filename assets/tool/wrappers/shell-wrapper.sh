#!/bin/sh
# Generic shell wrapper for tools that need mirror env vars.
# This file is copied/symlinked as the tool name (go, cargo, mvn, etc).
# The tool name is derived from basename($0).

set -e

_load_mirror_env_vars() {
    _tool="$1"
    _config_file="${HOME}/.config/epkg/tool/my_region/${_tool}.yaml"

    [ -f "$_config_file" ] || return 0

    while IFS= read -r _line; do
        case "$_line" in \#*|"") continue ;; esac
        case "$_line" in
            *:*)
                _key="${_line%%:*}"
                _val="${_line#*:}"
                _key=$(echo "$_key" | tr -d ' \t')
                _val=$(echo "$_val" | tr -d ' \t')
                if [ -n "$_key" ]; then
                    eval "_cur=\${$_key:-}"
                    [ -z "$_cur" ] && export "$_key=$_val"
                fi
                ;;
        esac
    done < "$_config_file"
}

_main() {
    _tool=$(basename "$0")
    case "$_tool" in pip3) _tool=pip ;; esac
    _load_mirror_env_vars "$_tool"
    exec "/usr/bin/$(basename "$0")" "$@"
}

_main "$@"
