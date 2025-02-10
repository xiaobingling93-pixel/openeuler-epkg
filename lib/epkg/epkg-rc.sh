#!/bin/sh
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

# keep clean and minimal -- it's sourced by every user terminal
__epkg_append_path() {
	if [ -d "/opt/epkg/users/public/envs/common/" ]; then
		source /opt/epkg/users/public/envs/common/profile-current/usr/lib/epkg/env.sh
		source /opt/epkg/users/public/envs/common/profile-current/usr/lib/epkg/paths.sh
	else
		source $HOME/.epkg/envs/common/profile-current/usr/lib/epkg/env.sh
		source $HOME/.epkg/envs/common/profile-current/usr/lib/epkg/paths.sh
	fi

	# Get epkg app-bin path
	local curr_envs=()
	local epkg_appbin_path=
	local epkg_registered_envs_dir=$HOME/.epkg/config/registered-envs
	# Add epkg path check
	if [ -n "$opt_pure" ]; then
		# Activate env --pure
		curr_envs+=($EPKG_ACTIVE_ENV)
	else
		declare -A seen_envs
		# Activate env
		if [ -n "$EPKG_ACTIVE_ENV" ]; then
			curr_envs+=($EPKG_ACTIVE_ENV)
        	seen_envs[$EPKG_ACTIVE_ENV]=1
		fi
		# Registered envs
		if [[ -d $epkg_registered_envs_dir && -n "$(ls -A $epkg_registered_envs_dir)" ]]; then
			while IFS= read -r file; do
				env_name=${file##*/}
				if [[ ! ${seen_envs[$env_name]} ]]; then
					curr_envs+=("$env_name")
					seen_envs[$env_name]=1  
				fi
			done < <(ls -lt --time-style=long-iso "$epkg_registered_envs_dir" | grep '^l' |  awk '{print $(NF-2)}')
		fi
	fi
	# Create path
	for env in "${curr_envs[@]}";do
		epkg_appbin_path+=$(__epkg_add_path $env)
	done

    # Get system origin path
	local PATH_DIRS
	local SYSTEM_ORIGIN_PATH
	# Use IFS (Internal Field Separator) to split the PATH into an array
	if [ -n "$BASH_VERSION" ]; then
		PATH_DIRS="${PATH//:/ }"
	elif [ -n "$ZSH_VERSION" ]; then
		PATH_DIRS=(${(@s/:/)PATH})
	else
		PATH_DIRS=$(echo "$PATH" | tr : ' ')
	fi
	# Create a new PATH variable without the unwanted directories
	SYSTEM_ORIGIN_PATH=""
	for dir in $PATH_DIRS; do
		if [[ -n "$dir" && "$dir" != *epkg* ]]; then
			SYSTEM_ORIGIN_PATH+="$dir:"
		fi
	done
	# Remove the trailing colon
	SYSTEM_ORIGIN_PATH=${SYSTEM_ORIGIN_PATH%:}

	echo $epkg_appbin_path$SYSTEM_ORIGIN_PATH
}

# change PATH in bashrc
export PATH=$(__epkg_append_path)

__epkg_add_appbin_path() {
	export PATH=$(__epkg_append_path)
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
		local epkg_common_profile=/opt/epkg/users/public/envs/common/profile-current
	else
		local epkg_common_profile=$HOME/.epkg/envs/common/profile-current
	fi
	local epkg_sh=$epkg_common_profile/usr/bin/epkg.sh
	local epkg_rust=$epkg_common_profile/usr/bin/epkg

	# issue[IB8I93]: A user create new environment, su other user, error reported that the activated environment does not exist
	if [[ -n "$EPKG_ACTIVE_ENV" && ! -d "$HOME/.epkg/envs/$EPKG_ACTIVE_ENV" ]]; then
		unset EPKG_ACTIVE_ENV
	fi

	case "$cmd" in
		env)
			local sub_cmd=$2
			local env=$3
			case "$sub_cmd" in
				create)
					$epkg_sh "$@" || return
					# update PATH
					echo "Environment '$env' activated."
					export EPKG_ACTIVE_ENV=$env
					__epkg_add_appbin_path
					return
					;;
				remove)
					$epkg_sh "$@" || return
					# update PATH
					if [[ "$env" == "$EPKG_ACTIVE_ENV" ]]; then
						unset EPKG_ACTIVE_ENV
					fi
					__epkg_add_appbin_path
					return
					;;
				activate)
					if [ -z "$env" ]; then
						echo "env_name cannot be empty!"
						return
					elif [[ "$env" == "common" ]]; then
						echo "$env cannot be activated!"
						return
					elif [ ! -d "$HOME/.epkg/envs/$env" ]; then
						echo "$env not exist!"
						return
					fi
					# --pure
					local opt_pure=$4
					# update PATH
					echo "Environment '$env' activated."
					export EPKG_ACTIVE_ENV=$env
					__epkg_add_appbin_path
					return
					;;
				deactivate)
					# update PATH
					echo "Environment '$EPKG_ACTIVE_ENV' deactivated."
					unset EPKG_ACTIVE_ENV
					__epkg_add_appbin_path
					return
					;;
				register)
					$epkg_sh "$@" || return
					# update PATH
					__epkg_add_appbin_path
					return
					;;
				unregister)
					$epkg_sh "$@" || return
					# update PATH
					__epkg_add_appbin_path
					return					
					;;
			esac
			;;
		install)
			echo "rust epkg install..."
			$epkg_sh update
			$epkg_rust "$@"
			__rehash_path
			return
			;;
	esac

	$epkg_sh "$@" || return

	case "$cmd" in
		install)
			__rehash_path
			;;
	esac
}

# vim: sw=4 ts=4 et
