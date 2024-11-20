#!/usr/bin/env bash

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

__get_curr_env_root() {
	local curr_env=$1
	if [[ "$curr_env" == "common" ]]; then
		curr_env_root=$(dirname "$EPKG_COMMON_ROOT")
	else
		curr_env_root=$EPKG_ENVS_ROOT
	fi
}

__epkg_enable_environment() {
	local env=$1

	_check_env_enabled $env
	if [ $? -eq 0 ]; then
		echo "$env already enabled!"
		return
	fi

	if [ -d "$EPKG_ENVS_ROOT/$env" ]; then
		ln -sT "$EPKG_ENVS_ROOT/$env" "$EPKG_CONFIG_DIR/enabled-envs/$env"
	fi

	echo "Environment '$env' added to PATH."
}

__epkg_disable_environment() {
	local env=$1

	_check_env_enabled $env
	if [ $? -eq 1 ]; then
		echo "$env already disabled!"
		return
	fi

	rm -f "$EPKG_CONFIG_DIR/enabled-envs/$env"

	echo "Environment '$env' removed from PATH."
}

__epkg_activate_environment() {
	local env=$1
	export EPKG_CURR_ENV=env
	echo "Environment '$env' activated."
}

__epkg_deactivate_environment() {
	echo "Environment '$EPKG_CURR_ENV' deactivated."
	export EPKG_CURR_ENV=main
}

_check_env_existed() {
	local env=$1
	all_envs=$(ls -lt $EPKG_ENVS_ROOT | grep '^d' | awk '{print $9}')
	if echo "$all_envs" | grep -q -F -- "$env"; then
		return 0
	fi
	return 1
}

_check_env_enabled() {
	local env=$1
	if [ -L "$EPKG_CONFIG_DIR/enabled-envs/$env" ]; then
		return 0
	fi
	return 1
}

list_environments() {
	# List all environments
	echo "Available environments(sort by time):"
	all_envs=$(ls -t $EPKG_ENVS_ROOT)
	echo "Environment          Status"
	echo "---------------------"
	echo "$all_envs" | awk '{print $1 "          " ($1 == "'$EPKG_CURR_ENV'" ? "Y" : "")}' | column -t
	# echo "You are in [$EPKG_CURR_ENV] now"
}

create_environment() {
	local env=$1
	local subcmd=$2
	local repo_path=$3

	local curr_env_root=
	__get_curr_env_root $env
	local epkg_helper=
	__get_epkg_helper "env_mode" "$curr_env_root/$env/"

	#_check_env_existed $env
	#if [ $? -eq 0 ]; then
	#	echo "$env already existed!"
	#	return
	#fi

	$epkg_helper mkdir -p $curr_env_root/$env/profile-1/usr/{app-bin,bin,sbin,lib,lib64}
	
    cd $curr_env_root/$env/profile-1
    $epkg_helper ln -sT "usr/app-bin" "app-bin"
	$epkg_helper ln -sT "usr/bin"     "bin"
	$epkg_helper ln -sT "usr/sbin"    "sbin"
	$epkg_helper ln -sT "usr/lib"     "lib"
	$epkg_helper ln -sT "usr/lib64"   "lib64"
	$epkg_helper ln -sT "$curr_env_root/$env/profile-1" "$curr_env_root/$env/profile-current"

	if [[  "$subcmd" == "--repo" ]];then
		if [[ "$repo_path" == *"/"* ]];then
			init_channel_repo $env ${1%/*} ${1#*/}
		else
			init_channel_repo $env $repo_path
		fi
	else 
		init_channel_repo $env openEuler-24.09
	fi

	echo "Environment '$env' created."
}

remove_environment() {
	local env=$1
	local curr_env_root=
	__get_curr_env_root $env
	_check_env_existed $env
	if [ $? -eq 1 ]; then
		echo "$env no existed!"
		return
	fi
	
	mv "$curr_env_root/$env" "$curr_env_root/.$env"
	echo "$env remove success!"
}

# setup env variable
get_active_env() {
	env="$*"
	env="${env#*--env }"

	[ "$env" != "$*" ] && {
		env=${env%% *}
		return
	}

	[ -n "$EPKG_CURR_ENV" ] && {
		env=$EPKG_CURR_ENV
		return
	}

	env=main
}

env_history() {
	local env=$1

	ls -l $EPKG_ENVS_ROOT/$env
}

# Rollback environment to previous state
env_rollback() {
	local env=$1

	echo "Environment '$env' rolled back."
	# Add implementation for rollback (if available)
}
