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

__epkg_ls_pathd() {
    [ -d "$1" ] || return

    # No need to save/restore directory since function runs in a subshell
    cd "$1" || return

    local link
    for link in $(command ls -rv)
    do
        readlink -f "$link"
    done
}

# TODO: should add path for active env too, and consider de-duplicate paths
__epkg_add_path() {
    # For zsh, enable word splitting just for this function
    if [ -n "$ZSH_VERSION" ]; then
        setopt local_options SH_WORD_SPLIT
    fi

    local epkg_pathd_dir="$HOME/.epkg/config/path.d"
    local p=
    local new_path=
    local old_path=":$PATH:"

    # Build PATH components
    local  append_paths=$(__epkg_ls_pathd "$epkg_pathd_dir/append")
    local prepend_paths=$(__epkg_ls_pathd "$epkg_pathd_dir/prepend")

    # Construct new PATH
    for p in $prepend_paths; do [ "${old_path%%:$p:*}" = "$old_path" ] && new_path="$new_path:$p"; done
    new_path="${new_path}:$PATH"
    for p in $append_paths;  do [ "${old_path%%:$p:*}" = "$old_path" ] && new_path="$new_path:$p"; done

    # Remove any trailing colon and export
    export PATH="${new_path#:}"
}

# change PATH in bashrc
__epkg_add_path

__epkg_update_path() {
	__epkg_add_path
	__rehash_path
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

epkg() {
	local cmd="$1"

	if [ -d "/opt/epkg/users/public/envs/common/" ]; then
		local epkg_common_root="/opt/epkg/users/public/envs/common"
	else
		local epkg_common_root="$HOME/.epkg/envs/common"
	fi
	local epkg_rust="$epkg_common_root/usr/bin/epkg"

	# issue[IB8I93]: A user create new environment, su other user, error reported that the activated environment does not exist
	if [ -n "$EPKG_ACTIVE_ENV" ] && [ ! -d "$HOME/.epkg/envs/$EPKG_ACTIVE_ENV" ]; then
		unset EPKG_ACTIVE_ENV
	fi

	case "$cmd" in
		env)
			local sub_cmd="$2"
			local env="$3"
			case "$sub_cmd" in
				create)
					"$epkg_rust" "$@" || return
					__epkg_update_path
					;;
				remove)
					"$epkg_rust" "$@" || return
					[ "$env" = "$EPKG_ACTIVE_ENV" ] && unset EPKG_ACTIVE_ENV
					__epkg_update_path
					;;
				activate)
                    __epkg_activate_environment "$@" || return
					__epkg_update_path
					;;
				deactivate)
					[ $# -ne 2 ] && { echo "Usage: epkg env deactivate"; return 1; }
					[ -z "$EPKG_ACTIVE_ENV" ] && { echo "No environment activated."; return 0; }

					echo "Environment '$EPKG_ACTIVE_ENV' deactivated."
					unset EPKG_ACTIVE_ENV
					__epkg_update_path
					;;
				register|unregister)
					"$epkg_rust" "$@" || return
					__epkg_update_path
					;;
			esac
			;;
		install|remove)
			"$epkg_rust" "$@" &&
			__rehash_path
			;;
        *)
            "$epkg_rust" "$@"
            ;;
	esac
}

__epkg_activate_environment()
{
    local opt_pure=
    if [ $# -eq 3 ]; then
        env="$3"
    elif [ $# -eq 4 ] && [ "$4" = "--pure" ]; then
        env="$3"
        opt_pure="$4"
    elif [ $# -eq 4 ] && [ "$3" = "--pure" ]; then
        opt_pure="$3"
        env="$4"
    else
        echo "Usage: epkg env activate [--pure] <env_name>"
        echo "       epkg env activate <env_name> [--pure]"
        return 1
    fi

    [ -z "$env" ] && { echo "env_name cannot be empty!"; return 1; }
    [ "$env" = "common" ] && { echo "'$env' cannot be activated!"; return 1; }
    [ ! -d "$HOME/.epkg/envs/$env" ] && { echo "$env not exist!"; return 1; }

    export EPKG_ACTIVE_ENV="$env"
    echo "Environment '$env' activated${opt_pure:+ $opt_pure}"
}

# vim: sw=4 ts=4 et
