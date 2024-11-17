#!/usr/bin/env bash
if [ -d "/opt/epkg/users/public/envs/common/" ]; then
	# XXX: PROJECT_DIR too generic; no need export here in user shell env
	export PROJECT_DIR=/opt/epkg/users/public/envs/common/profile-1/usr
elif [ -d "$COMMON_PROFILE_LINK" ]; then
	export PROJECT_DIR=$COMMON_PROFILE_LINK/usr
else # TODO: if not yet run 'epkg init', don't source $PROJECT_DIR files
    export PROJECT_DIR=$HOME/.epkg/envs/common/profile-1/usr
fi

# XXX: too many source, will pollute user shell env
# - epkg-installer.sh shall only add 1 function epkg() to user shell
# - when user run 'epkg ANYCMD', it shall auto run 'epkg init' if necessary
source $PROJECT_DIR/lib/epkg/paths.sh
source $PROJECT_DIR/lib/epkg/init.sh
source $PROJECT_DIR/lib/epkg/env.sh
source $PROJECT_DIR/lib/epkg/package.sh
source $PROJECT_DIR/lib/epkg/query.sh
source $PROJECT_DIR/lib/epkg/repo.sh
source $PROJECT_DIR/lib/epkg/cache-repo.sh

__get_epkg_helper() {
	local mode=$1
	local curr_env_path=$2
	local global_comm_path=$PUB_EPKG/envs/common/

	if [[ "$mode" == "env_mode" && "$curr_env_path" =~ "$global_comm_path" ]]; then
		epkg_helper=$EPKG_HELPER_EXEC
	elif [[ "$mode" == "install_mode" && -d "$global_comm_path" ]]; then
		epkg_helper=$EPKG_HELPER_EXEC
	fi
}

__check_epkg_user_init() {
	local epkg_helper=
	__get_epkg_helper "install_mode"

	if [ ! -d "$EPKG_ENVS_ROOT/main/" ]; then
		echo "Warning: epkg has not been initialized"
		echo "please execute: epkg init"
		return 1
	fi
}

__get_help_info() {
	cat <<-EOF
Usage:
epkg init

epkg env list

epkg create [env]
epkg activate [env]
epkg deactivate [env]

epkg install [PACKAGE]
EOF
}

epkg() {
	local cmd="$1"
	local input_env="$2"
	shift

	if [[ "$cmd" != "init" && "$cmd" != '-h' && "$cmd" != '--help' ]]; then
		if ! __check_epkg_user_init; then
			return 1
		fi
		echo "EPKG_ENV_NAME: $EPKG_ENV_NAME"
		local env=
		get_active_env "$@"
		[ "$cmd" = 'init' ] || set_epkg_env_dirs $env
	fi

	case "$cmd" in
		--help|-h)
			__get_help_info
			;;
		init)
			epkg_init "$@"
			;;
		create)
			echo "Attention: env $input_env will be create."
			create_environment $input_env
			shift
			subcmd=$1
			if [ $# -gt 0 ]; then # zsh compatible; when $# < shift
				shift
			fi
			case $subcmd in 
				"--repo")
					if [[ "$1" == *"/"* ]];then
						init_channel_repo $input_env ${1%/*} ${1#*/}
					else
						init_channel_repo $input_env $1
					fi
					;;
				*)
					init_channel_repo $input_env openEuler-24.09
					;;
			esac
			;;
		enable)
			__epkg_enable_environment $input_env
			;;
		disable)
			__epkg_disable_environment $input_env
			;;
		activate)
			__epkg_activate_environment $input_env
			;;
		deactivate)
			__epkg_deactivate_environment
			;;
		env)
			subcmd=$1
			shift
			case $subcmd in
				"list")
					list_environments
					;;
				*)
					echo "Usage: epkg env [list|create|remove|enable|disable|activate|deactivate|history|rollback]"
					;;
			esac
			;;
		install)
			# XXX: 'epkg install' shall be in rust
			installroot=""
			package_arr=()
			while [[ $# -gt 0 ]];do
				case "$1" in
					--installroot=*)
						installroot="${1#*=}"
						shift
						;;
					*)
						package_arr+=("$1")
						shift
						;;
				esac
			done
			if [ ${#package_arr[@]} -eq 0 ]; then
				echo "No Packages specified." >&2
				exit 1
			fi
			install_package
			;;
		*)
			command epkg "$@"
			;;
	esac
}
