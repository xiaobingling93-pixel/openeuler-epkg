#!/bin/sh

# keep clean and minimal -- it's sourced by every user terminal

# XXX: when user run 'epkg ANYCMD', it shall auto run 'epkg init' if necessary

__epkg_add_appbin_path() {
	local PATH_ARRAY
	local NEW_PATH

	# Use IFS (Internal Field Separator) to split the PATH into an array
	IFS=':' read -ra PATH_ARRAY <<< "$PATH"

	# Create a new PATH variable without the unwanted directories
	NEW_PATH=""
	for dir in "${PATH_ARRAY[@]}"; do
		if [[ "${dir#*/app-bin}" = "$dir" ]]; then
			# Append the directory to the new PATH if it doesn't end with /app-bin
			NEW_PATH+="$dir:"
		fi
	done

	# Remove the trailing colon
	NEW_PATH=${NEW_PATH%:}

	# Export the new PATH
	local HOME_EPKG=$HOME/.epkg
	local EPKG_CONFIG_DIR=$HOME_EPKG/config
	source $EPKG_CONFIG_DIR/shell-cmd-path.sh
	export PATH="$EPKG_APPBIN_PATH:$NEW_PATH"
	[ -n "$epkg_active_env_path" ] && export PATH="$epkg_active_env_path:$PATH"

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
				create|enable)
					$project_dir/bin/epkg "$@" || return
					__epkg_add_appbin_path
					return
					;;
				remove|disable)
					$project_dir/bin/epkg "$@" || return
					__epkg_add_appbin_path
					return
					;;
				activate)
					local epkg_active_env_path=$(
						source $project_dir/lib/epkg/env.sh
						__epkg_activate_environment "$env"
					)
					__epkg_add_appbin_path
					export EPKG_ENV_NAME=$env
					return
					;;
				deactivate)
					__epkg_add_appbin_path
					unset EPKG_ENV_NAME
					return
					;;
			esac
			;;
	esac

	$project_dir/bin/epkg "$@"
}
