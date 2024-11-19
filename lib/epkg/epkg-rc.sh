#!/bin/sh

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
	if [ -n "$EPKG_ENV_NAME" ]; then
		curr_envs+=($EPKG_ENV_NAME)
		curr_envs+=(common)
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
	else # TODO: if not yet run 'epkg init', don't source $PROJECT_DIR files
		local project_dir=$HOME/.epkg/envs/common/profile-1/usr
	fi

	case "$cmd" in
		env)
			local sub_cmd=$2
			local env=$3
			case "$sub_cmd" in
				create)
					$project_dir/bin/epkg "$@" || return
					export EPKG_ENV_NAME=$env
					__epkg_add_appbin_path
					return
					;;
				remove)
					$project_dir/bin/epkg "$@" || return
					unset EPKG_ENV_NAME
					__epkg_add_appbin_path
					return
					;;
				enable)
					$project_dir/bin/epkg "$@" || return
					return
					;;
				disable)
					$project_dir/bin/epkg "$@" || return
					return
					;;
				activate)
					export EPKG_ENV_NAME=$env
					__epkg_add_appbin_path
					return
					;;
				deactivate)
					unset EPKG_ENV_NAME
					__epkg_add_appbin_path
					return
					;;
			esac
			;;
	esac

	$project_dir/bin/epkg "$@"
}
