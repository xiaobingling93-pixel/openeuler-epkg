#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

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

__epkg_register_environment() {
	local env=$1

	if [[ "$env" == "common" ]]; then
		echo "Environment $env cannot be registered."
		return 1
	fi
	_check_env_existed $env || return 1
	_check_env_registered $env && return 1

	ln -sT "$EPKG_ENVS_ROOT/$env" "$EPKG_CONFIG_DIR/registered-envs/$env"
	echo "Environment '$env' has been registered to PATH."
}

__epkg_unregister_environment() {
	local env=$1

	if [[ "$env" == "common" ]]; then
		echo "Environment $env cannot be registered."
		return 1
	fi
	_check_env_existed $env || return 1
	_check_env_registered $env || return 1

	rm -f "$EPKG_CONFIG_DIR/registered-envs/$env"
	echo "Environment '$env' has been unregistered from PATH."
}

__epkg_activate_environment() {
	local env=$1
	export EPKG_ACTIVE_ENV=env
	echo "Environment '$env' activated."
}

__epkg_deactivate_environment() {
	echo "Environment '$EPKG_ACTIVE_ENV' deactivated."
	export EPKG_ACTIVE_ENV=main
}

_check_env_existed() {
	local check_env=$1
	if [ -d "$EPKG_ENVS_ROOT/${check_env}" ];then
		echo "Environment ${check_env} exist."
		return 0
	fi
	echo "Environment ${check_env} not exist."
	return 1
}

_check_env_registered() {
	local check_env=$1
	if [ -L "$EPKG_CONFIG_DIR/registered-envs/${check_env}" ]; then
		echo "Environment ${check_env} had been registered."
		return 0
	fi
	echo "Environment ${check_env} not registered."
	return 1
}

list_environments() {
	# List all environments
	echo "Available environments(sort by time):"
	all_envs=$(ls -t $EPKG_ENVS_ROOT | grep -v 'common')
	echo "Environment          Status"
	echo "---------------------"
	echo "$all_envs" | awk '{print $1 "          " ($1 == "'$EPKG_ACTIVE_ENV'" ? "Y" : "")}' | column -t
	# echo "You are in [$EPKG_ACTIVE_ENV] now"
}

create_environment() {
	local env=$1
	local subcmd=$2
	local repo_path=$3

	if [[ "$env" == "common" ]]; then
		echo "Environment $env cannot be create."
		return 1
	fi
	_check_env_existed $env && return 1
	
	local curr_env_root=
	__get_curr_env_root $env
	local epkg_helper=
	__get_epkg_helper "env_mode" "$curr_env_root/$env/"

	$epkg_helper mkdir -p $curr_env_root/$env/profile-1/usr/{app-bin,bin,sbin,lib,lib64}
	
    cd $curr_env_root/$env/profile-1
	$epkg_helper ln -sfT "usr/bin"     "bin"
	$epkg_helper ln -sfT "usr/sbin"    "sbin"
	$epkg_helper ln -sfT "usr/lib"     "lib"
	$epkg_helper ln -sfT "usr/lib64"   "lib64"
	$epkg_helper ln -sfT "$curr_env_root/$env/profile-1" "$curr_env_root/$env/profile-current"

	if [[  "$subcmd" == "--repo" ]];then
		if [[ "$repo_path" == *"/"* ]];then
			init_channel_repo $env ${1%/*} ${1#*/}
		else
			init_channel_repo $env $repo_path
		fi
	else 
		init_channel_repo $env openEuler-24.09
	fi

	echo "Environment '$env' has been created."
}

remove_environment() {
	local env=$1
	local curr_env_root=
	__get_curr_env_root $env

	if [[ "$env" == "common" || "$env" == "main" ]]; then
		echo "Environment $env cannot be removed."
		return 1
	fi
	_check_env_existed $env || return 1
	_check_env_registered $env && __epkg_unregister_environment $env

	mv "$curr_env_root/$env" "$curr_env_root/.$env"
	echo "Environment $env has been removed."
}

# setup env variable
get_active_env() {
	env="$*"
	env="${env#*--env }"

	[ "$env" != "$*" ] && {
		env=${env%% *}
		return
	}

	[ -n "$EPKG_ACTIVE_ENV" ] && {
		env=$EPKG_ACTIVE_ENV
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
