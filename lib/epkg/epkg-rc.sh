#!/bin/sh
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

# keep clean and minimal -- it's sourced by every user terminal
__epkg_add_path() {
    local epkg_pathd_dir="$HOME/.epkg/config/path.d"
    local p=
    local new_path=
    local old_path=":$PATH:"

    # Build PATH components
    local prepend_paths=$(ls -v "$epkg_pathd_dir/prepend"/* 2>/dev/null | xargs -I{} readlink -f {})
    local append_paths=$(ls -v "$epkg_pathd_dir/append"/* 2>/dev/null | xargs -I{} readlink -f {})

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
	if [ -n "${ZSH_VERSION}" ]; then
		rehash
	elif [ -n "${BASH_VERSION}" ]; then
		hash -r
	fi
}

epkg() {
	local cmd="$1"

	if [ -d "/opt/epkg/users/public/envs/common/" ]; then
		local epkg_common_profile="/opt/epkg/users/public/envs/common/profile-current"
	else
		local epkg_common_profile="$HOME/.epkg/envs/common/profile-current"
	fi
	local epkg_rust="$epkg_common_profile/usr/bin/epkg"

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
					$epkg_rust "$@" || return
					export EPKG_ACTIVE_ENV="$env"
					__epkg_update_path
					return
					;;
				remove)
					$epkg_rust "$@" || return
					[ "$env" = "$EPKG_ACTIVE_ENV" ] && unset EPKG_ACTIVE_ENV
					__epkg_update_path
					return
					;;
				activate)
					# Check Parameters $#==3 or ($#==4 and $4==--pure)
					if [ $# -eq 3 ] || [ $# -eq 4 ] && [ "$4" = "--pure" ]; then
						:
					else
						echo "Usage: epkg env activate <env_name> [--pure]"
						return
					fi

					[ -z "$env" ] && { echo "env_name cannot be empty!"; return; }
					[ "$env" = "common" ] && { echo "$env cannot be activated!"; return; }
					[ ! -d "$HOME/.epkg/envs/$env" ] && { echo "$env not exist!"; return; }
					# --pure
					local opt_pure="$4"
					export EPKG_ACTIVE_ENV="$env"
					echo "Environment '$env' activated${4:+ $opt_pure}."
					__epkg_update_path
					return
					;;
				deactivate)
					[ $# -ne 2 ] && { echo "Usage: epkg env deactivate"; return; }
					[ -z "$EPKG_ACTIVE_ENV" ] && { echo "No environment activated."; return; }

					echo "Environment '$EPKG_ACTIVE_ENV' deactivated."
					unset EPKG_ACTIVE_ENV
					__epkg_update_path
					return
					;;
				register|unregister)
					$epkg_rust "$@" || return
					# update PATH
					__epkg_update_path
					return
					;;
			esac
			;;
		install|remove)
			$epkg_rust "$@"
			__rehash_path
			return
			;;
	esac

	$epkg_rust "$@"
}

# vim: sw=4 ts=4 et
