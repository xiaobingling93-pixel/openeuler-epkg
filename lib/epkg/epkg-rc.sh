#!/bin/sh
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

# keep clean and minimal -- it's sourced by every user terminal
__epkg_append_path() {
	if [ -d "/opt/epkg/users/public/envs/common/" ]; then
		source /opt/epkg/users/public/envs/common/profile-1/usr/lib/epkg/env.sh
		source /opt/epkg/users/public/envs/common/profile-1/usr/lib/epkg/paths.sh
	else
		source $HOME/.epkg/envs/common/profile-1/usr/lib/epkg/env.sh
		source $HOME/.epkg/envs/common/profile-1/usr/lib/epkg/paths.sh
	fi

	# Get epkg app-bin path
	local curr_envs=()
	local epkg_appbin_path=
	local epkg_enabled_envs_dir=$HOME/.epkg/config/enabled-envs
	# Current shell activate env
	if [[ -n $EPKG_ACTIVE_ENV && "$EPKG_ACTIVE_ENV" != "main" ]]; then
		curr_envs+=($EPKG_ACTIVE_ENV)
	else
		# Enabled envs (init main & common) 
		if [[ -d $epkg_enabled_envs_dir && -n "$(ls -A $epkg_enabled_envs_dir)" ]]; then
			for file in "$epkg_enabled_envs_dir"/*; do
				curr_envs+=(${file##*/})
			done
		fi
	fi
	# Create path
	for env in "${curr_envs[@]}";do
		epkg_appbin_path+=$(__epkg_add_path $env)
	done

    # Get system origin path
	local PATH_ARRAY
	local SYSTEM_ORIGIN_PATH
	# Use IFS (Internal Field Separator) to split the PATH into an array
	IFS=':' read -ra PATH_ARRAY <<< "$PATH"
	# Create a new PATH variable without the unwanted directories
	SYSTEM_ORIGIN_PATH=""
	for dir in "${PATH_ARRAY[@]}"; do
		if [[ -n "$dir" && "${dir#*/app-bin}" = "$dir" ]]; then
			# Append the directory to the new PATH if it doesn't end with /app-bin
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
	
	if [ -n "${ZSH_VERSION}" ]; then
		rehash
	elif [ -n "${BASH_VERSION}" ]; then
		hash -r
	fi
}

epkg() {
	local cmd="$1"

	if [ -d "/opt/epkg/users/public/envs/common/" ]; then
		local project_dir=/opt/epkg/users/public/envs/common/profile-1/usr
	elif [ -d "$COMMON_PROFILE_LINK" ]; then
		local project_dir=$COMMON_PROFILE_LINK/usr
	else
		local project_dir=$HOME/.epkg/envs/common/profile-1/usr
	fi

	if [ -z $EPKG_ACTIVE_ENV ]; then
		export EPKG_ACTIVE_ENV=main
	fi

	case "$cmd" in
		env)
			local sub_cmd=$2
			local env=$3
			case "$sub_cmd" in
				create)
					$project_dir/bin/epkg "$@" || return
					# update PATH
					echo "Environment '$env' activated."
					export EPKG_ACTIVE_ENV=$env
					__epkg_add_appbin_path
					return
					;;
				remove)
					$project_dir/bin/epkg "$@" || return
					# update PATH
					if [[ "$env" == "$EPKG_ACTIVE_ENV" ]]; then
						unset EPKG_ACTIVE_ENV
					fi
					__epkg_add_appbin_path
					return
					;;
				activate)
					if [[ "$env" == "common" ]]; then
						echo "$env cannot be activated!"
						return
					fi
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
			esac
			;;
	esac

	$project_dir/bin/epkg "$@"
}
