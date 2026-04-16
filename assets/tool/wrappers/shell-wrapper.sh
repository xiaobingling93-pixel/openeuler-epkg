#!/bin/sh
# Generic shell wrapper for tools that need mirror env vars.
# This file is copied/symlinked as the tool name (go, cargo, mvn, etc).
# The tool name is derived from basename($0).
#
# IMPORTANT: This wrapper uses PATH resolution instead of hardcoded paths.
#
# Why NOT hardcoded paths like "/usr/bin/$tool":
#
# 1. Brew packages use different paths than system packages:
#    - System go: /usr/bin/go -> go 1.24 (host system)
#    - Brew go:   $env_root/bin/go -> go 1.26 (Cellar/go/VERSION/libexec/bin/go)
#    Hardcoding "/usr/bin/go" would call the WRONG version!
#
# 2. Different platforms have different homebrew prefixes:
#    - Linux:     /home/linuxbrew/.linuxbrew/bin
#    - macOS ARM: /opt/homebrew/bin
#    - macOS Intel: /usr/local/bin
#    Hardcoding any one path fails on other platforms.
#
# 3. PATH is already correctly set by epkg:
#    - PATH order: usr/local/bin (wrappers) -> bin (tools) -> ebin -> system
#    - After removing wrapper's directory, PATH finds the correct tool
#    - Works universally across all platforms and package formats
#
# Example: When wrapper is at $env_root/usr/local/bin/go:
#    - PATH=/home/linuxbrew/.linuxbrew/usr/local/bin:/home/linuxbrew/.linuxbrew/bin:...
#    - Remove usr/local/bin from PATH
#    - PATH resolution finds /home/linuxbrew/.linuxbrew/bin/go (the brew version)
#    - Correct version is called with GOPROXY env var already set

set -e

_load_mirror_env_vars() {
    _tool="$1"
    _config_file="${HOME}/.config/epkg/tool/my_region/${_tool}.yaml"

    [ -f "$_config_file" ] || return 0

    while IFS= read -r _line; do
        case "$_line" in \#*|"") continue ;; esac
        case "$_line" in
            *": "*)
                _key="${_line%%: *}"
                _val="${_line#*: }"
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
    _load_mirror_env_vars "$_tool"

    # Find the actual tool binary (skip this wrapper script)
    # PATH resolution will find the real binary in env/bin or ebin
    # Remove this wrapper's directory from PATH to avoid recursion
    _wrapper_dir=$(dirname "$0")
    _new_path=""
    for _p in $(echo "$PATH" | tr ':' ' '); do
        if [ "$_p" != "$_wrapper_dir" ]; then
            if [ -z "$_new_path" ]; then
                _new_path="$_p"
            else
                _new_path="${_new_path}:${_p}"
            fi
        fi
    done

    # Execute the actual tool via modified PATH
    exec env PATH="$_new_path" "$_tool" "$@"
}

_main "$@"
