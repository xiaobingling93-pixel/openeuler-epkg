#!/bin/sh
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

# keep clean and minimal -- it's sourced by every user terminal

# PATH prepend/append rules:
# - Files in path.d/prepend and path.d/append are symlinks named with numeric prefixes
# - Files are processed in reverse version order (e.g. 10-main before 3-focal)
# - Each symlink points to an ebin directory in an epkg environment
# Example path.d/prepend:
#   10-main -> ~/.epkg/envs/main/usr/ebin
#   3-focal -> ~/.epkg/envs/focal/usr/ebin
# This results in PATH ordering:
#   /home/user/.epkg/envs/main/usr/ebin:
#   /home/user/.epkg/envs/focal/usr/ebin:
#   [original PATH]

epkg() {
    local epkg_common_root="$HOME/.epkg/envs/common"
    [ -d "$epkg_common_root" ] || epkg_common_root="/opt/epkg/envs/root/common"
    [ -d "$epkg_common_root" ] || { echo "Cannot find common env, abort"; exit 1; }

    local epkg_rust="$epkg_common_root/usr/bin/epkg"

    # issue[IB8I93]: A user create new environment, su other user, error reported that the activated environment does not exist
    if [ -n "$EPKG_ACTIVE_ENV" ] && [ ! -d "$HOME/.epkg/envs/$EPKG_ACTIVE_ENV" ]; then
        unset EPKG_ACTIVE_ENV
    fi

    local cmd="$1"
    case "$cmd" in
        env)
            local sub_cmd="$2"
            case "$sub_cmd" in
                path|register|unregister|activate|deactivate|remove)
                    local output
                    output=$("$epkg_rust" "$@") || return
                    eval "$output" || return
                    __rehash_path
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
epkg env path

# vim: sw=4 ts=4 et
