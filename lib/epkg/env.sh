#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

__get_curr_env_root() {
	local curr_env=$1
	if [[ "$curr_env" == "common" ]]; then
		curr_env_root=$(dirname "$EPKG_COMMON_ROOT")
	else
		curr_env_root=$EPKG_ENVS_ROOT
	fi
}

__check_env_existed() {
	local check_env=$1
	if [ -d "$EPKG_ENVS_ROOT/${check_env}" ];then
		echo "Environment ${check_env} exist."
		return 0
	fi
	echo "Environment ${check_env} not exist."
	return 1
}

__check_env_registered() {
	local check_env=$1
	if [ -L "$EPKG_CONFIG_DIR/registered-envs/${check_env}" ]; then
		echo "Environment ${check_env} had been registered."
		return 0
	fi
	echo "Environment ${check_env} not registered."
	return 1
}

__epkg_register_environment() {
	local env=$1

	if [[ "$env" == "common" ]]; then
		echo "Environment $env cannot be registered."
		return 1
	fi
	__check_env_existed $env || return 1

	ln -sfT "$EPKG_ENVS_ROOT/$env" "$EPKG_CONFIG_DIR/registered-envs/$env"
	echo "Environment '$env' has been registered."
}

__epkg_unregister_environment() {
	local env=$1

	if [[ "$env" == "common" ]]; then
		echo "Environment $env cannot be registered."
		return 1
	fi
	__check_env_existed $env || return 1
	__check_env_registered $env || return 1

	rm -f "$EPKG_CONFIG_DIR/registered-envs/$env"
	echo "Environment '$env' has been unregistered from PATH."
}

__epkg_init_channel()
{
	local env=$1
	local channel=$2
	local repo=$3

	# channel.yaml
	local env_channel_yaml=${HOME}/.epkg/envs/${env}/profile-current/etc/epkg/channel.yaml
	mkdir -p $(dirname ${env_channel_yaml})
	cp $EPKG_CACHE/epkg-manager/channel/${channel}-channel.yaml  $env_channel_yaml
	# installed-packages.json
	echo -e "{\n}" > $HOME/.epkg/envs/$env/profile-current/installed-packages.json

	return 0
}

__epkg_create_environment() {
	local env=$1
	local subcmd=$2
	local repo_path=$3

	if [ -n "$repo_path" ] && [ ! -f "$EPKG_CACHE/epkg-manager/channel/${repo_path}-channel.yaml" ]; then
		echo "channel ${repo_path} not found"
		return 1
	fi
	if [[ "$env" == "common" ]]; then
		echo "Environment $env cannot be create."
		return 1
	fi
	__check_env_existed $env && return 1

	local curr_env_root=
	__get_curr_env_root $env

	mkdir -p $curr_env_root/$env/profile-1/usr/{app-bin,bin,sbin,lib,lib64}
	mkdir -p $curr_env_root/$env/profile-1/etc

    cd $curr_env_root/$env/profile-1
	ln -sfT "usr/bin"     "bin"
	ln -sfT "usr/sbin"    "sbin"
	ln -sfT "usr/lib"     "lib"
	ln -sfT "usr/lib64"   "lib64"
	ln -sfT "$curr_env_root/$env/profile-1" "$curr_env_root/$env/profile-current"
	cp /etc/resolv.conf $curr_env_root/$env/profile-current/etc/resolv.conf

	if [[  "$subcmd" == "--repo" ]];then
		if [[ "$repo_path" == *"/"* ]];then
			__epkg_init_channel $env ${1%/*} ${1#*/} || return 1
		else
			__epkg_init_channel $env $repo_path || return 1
		fi
	else
		__epkg_init_channel $env openEuler-24.03-LTS || return 1
	fi

	echo "Environment '$env' has been created."
}

__epkg_remove_environment() {
	local env=$1
	local curr_env_root=
	__get_curr_env_root $env

	if [[ "$env" == "common" || "$env" == "main" ]]; then
		echo "Environment $env cannot be removed."
		return 1
	fi
	__check_env_existed $env || return 1
	__check_env_registered $env && __epkg_unregister_environment $env

	mv "$curr_env_root/$env" "$curr_env_root/.$env"
	echo "Environment $env has been removed."
}

__epkg_list_environments() {
	local all_envs=$(ls -t $EPKG_ENVS_ROOT | grep -v 'common')
	local registered_envs=$(ls -t $EPKG_CONFIG_DIR/registered-envs/)

	printf "%-15s  %20s\n" "Environment" "Status"
	printf "%35s\n" | tr ' ' '-'
	# Use awk to format and add the registered or activated status
	echo "$all_envs" | awk -v active="$EPKG_ACTIVE_ENV"  -v registered="$registered_envs" '
	BEGIN {
        split(registered, reg_array, "\n")
        for (i in reg_array) {
            reg[reg_array[i]] = 1
        }
    }
	{
		status = ""
		if ($1 == active) {
			status = (status ? status "|" : "") "activated"
		}
		if ($1 in reg) {
			status = (status ? status "|" : "") "registered"
		}
		printf "%-15s  %20s\n", $1, status
	}'
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

# vim: sw=4 ts=4 et
