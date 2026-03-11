#!/bin/sh
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

# keep clean and minimal -- it's sourced by every user terminal

epkg() {
    local env_self_dir="$HOME/.epkg/envs/self"
    [ -d "$env_self_dir" ] || env_self_dir="/opt/epkg/envs/root/self"

    local epkg_rust="$env_self_dir/usr/bin/epkg"
    [ -x "$epkg_rust" ] || {
        # This is possible after 'epkg self remove', and user has not re-opened current shell
        echo "epkg: command not found"
        return 1
    }

    # issue[IB8I93]: A user create new environment, su other user, error reported that the activated environment does not exist
    if [ -n "$EPKG_ACTIVE_ENV" ] && [ ! -d "$HOME/.epkg/envs/$EPKG_ACTIVE_ENV" ]; then
        unset EPKG_ACTIVE_ENV
    fi

    local cmd=""
    local sub_cmd=""
    local i=1
    local skip_next=0
    local has_help=0
    while [ $i -le $# ]; do
        eval "arg=\${$i}"   # works for both zsh and bash
        if [ $skip_next -eq 1 ]; then
            skip_next=0
            i=$((i + 1))
            continue
        fi
        case "$arg" in
            --)
                # End of options, treat remaining as non-options
                break
                ;;
            --*=*)
                # Option with value in same argument, skip
                ;;
            -h|--help|-V|--version|-q|--quiet|-v|--verbose|-y|--assume-yes|--dry-run|--download-only|--assume-no|-m|--ignore-missing)
                # Flag options that don't take a value
                has_help=1
                ;;
            -e|--env|-r|--root|--config|--arch|--metadata-expire|--proxy|--retry|--parallel-download|--parallel-processing)
                # Options that take a value, skip next argument
                skip_next=1
                ;;
            -*)
                # Unknown option, assume it might take a value
                skip_next=1
                ;;
            *)
                # Non-option argument
                if [ -z "$cmd" ]; then
                    cmd="$arg"
                elif [ -z "$sub_cmd" ]; then
                    sub_cmd="$arg"
                    # Continue parsing to detect help flags later
                fi
                ;;
        esac
        i=$((i + 1))
    done

    case "$cmd" in
        env)
            case "$sub_cmd" in
                path|register|unregister|activate|deactivate|remove)
                    local output
                    output=$("$epkg_rust" "$@") || return
                    echo "$output"
                    if [ $has_help -eq 0 ]; then
                        eval "$output" || return
                        __rehash_path
                    fi
                    ;;
                *)
                    "$epkg_rust" "$@"
                    ;;
            esac
            ;;
        install|remove|switch)
            "$epkg_rust" "$@" &&
            __rehash_path
            ;;
        *)
            "$epkg_rust" "$@"
            ;;
    esac
}

__rehash_path() {
    if [ -n "$ZSH_VERSION" ]; then
        rehash
    elif [ -n "$BASH_VERSION" ]; then
        hash -r
    elif [ -n "$KSH_VERSION" ]; then
        hash -r
    elif [ -n "$FISH_VERSION" ]; then
        true  # Fish doesn't need explicit rehashing
    elif [ -n "$YASH_VERSION" ]; then
        rehash
    elif [ -n "$TCSH_VERSION" -o -n "$tcsh" ]; then
        rehash
    else
        # Fallback for unknown shells (try common rehash commands)
        # hash -r: busybox sh; dash
        hash -r 2>/dev/null || rehash 2>/dev/null || true
    fi
}

# change PATH in bashrc
epkg env path >/dev/null

# vim: sw=4 ts=4 et
