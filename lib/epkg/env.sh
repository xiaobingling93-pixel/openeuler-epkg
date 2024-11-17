#!/usr/bin/env bash

list_environments() {
	# List all environments
	echo "Available environments(sort by time):"
	all_envs=$(ls -t $EPKG_ENVS_ROOT)
	echo "Environment          Status"
	echo "---------------------"
	echo "$all_envs" | awk '{print $1 "          " ($1 == "'$EPKG_ENV_NAME'" ? "Y" : "")}' | column -t
	# echo "You are in [$EPKG_ENV_NAME] now"
}

create_environment() {
	local env=$1
	local curr_env_root=
	__get_curr_env_root $env
	local epkg_helper=
	__get_epkg_helper "env_mode" "$curr_env_root/$env/"

	#_check_env_existed $env
	#if [ $? -eq 0 ]; then
	#	echo "$env already existed!"
	#	return
	#fi

	# XXX: is epkg_helper secure?
	# XXX: merge N mkdir into 1 single cmd
	$epkg_helper mkdir -p "$curr_env_root/$env/profile-1/tmp"

	$epkg_helper ln -sT "$curr_env_root/$env/profile-1" "$curr_env_root/$env/profile-current"

	$epkg_helper mkdir -p "$curr_env_root/$env/profile-1/usr/app-bin"
	$epkg_helper mkdir -p "$curr_env_root/$env/profile-1/usr/bin"
	$epkg_helper mkdir -p "$curr_env_root/$env/profile-1/usr/sbin"
	$epkg_helper mkdir -p "$curr_env_root/$env/profile-1/usr/lib"
	$epkg_helper mkdir -p "$curr_env_root/$env/profile-1/usr/lib64"

	# use relative symlink
	$epkg_helper ln -sT  "$curr_env_root/$env/profile-1/usr/app-bin"  "$curr_env_root/$env/profile-1/app-bin"
	$epkg_helper ln -sT  "$curr_env_root/$env/profile-1/usr/bin"  "$curr_env_root/$env/profile-1/bin"
	$epkg_helper ln -sT  "$curr_env_root/$env/profile-1/usr/sbin"  "$curr_env_root/$env/profile-1/sbin"
	$epkg_helper ln -sT  "$curr_env_root/$env/profile-1/usr/lib"  "$curr_env_root/$env/profile-1/lib"
	$epkg_helper ln -sT  "$curr_env_root/$env/profile-1/usr/lib64"  "$curr_env_root/$env/profile-1/lib64"

	__epkg_activate_environment $env
	echo "Environment '$env' created."
}

activate_environment() {
	local env=$1
	local curr_env_root=
	__get_curr_env_root $env

	# XXX: avoid these extra actions
	mkdir -p "$curr_env_root/$env/profile-1/usr/bin"
	mkdir -p "$curr_env_root/$env/profile-1/usr/sbin"
	mkdir -p "$curr_env_root/$env/profile-1/usr/lib"
	mkdir -p "$curr_env_root/$env/profile-1/usr/lib64"

	__epkg_activate_environment $env
	echo "Environment '$env' activated."
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

	[ -n "$EPKG_ENV_NAME" ] && {
		env=$EPKG_ENV_NAME
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
