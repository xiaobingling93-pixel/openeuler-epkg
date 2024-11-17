#!/usr/bin/env bash

__epkg_rehash() {
	if [ -n "${ZSH_VERSION}" ]; then
		rehash
	elif [ -n "${BASH_VERSION}" ]; then
		hash -r
	else
		:  # pass
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

# update EPKG_ENV_NAME to user shell rc file
_update_epkg_env_name() {
	local env=$1
	local shell

	if grep -q "EPKG_ENV_NAME" $RC_PATH; then
		sed -i "s/^export EPKG_ENV_NAME=.*$/export EPKG_ENV_NAME=$env/" $RC_PATH
	else
		echo "export EPKG_ENV_NAME=$env" >> "$RC_PATH"
	fi
}

# initialize PATH to epkg packages for bash/zsh shell
__epkg_create_path_rc() {
	local epkg_path="$1"
	local ORIGIN_PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
	cat > $EPKG_CONFIG_DIR/shell-add-path.sh <<EOM
## auto managed by 'epkg init|enable|disable'
export PATH="$epkg_path:$ORIGIN_PATH"
EOM
}

__epkg_add_path() {
	local env_to_add=$1
	local curr_env_root=
	__get_curr_env_root $env_to_add
	local env_dir=$curr_env_root/$env_to_add/profile-current
	local dir

	for dir in app-bin usr/app-bin
	do
		tmp_path=${epkg_path#*$env_dir/$dir}
		if [ $tmp_path = $epkg_path ]; then
			epkg_path="$env_dir/$dir:$epkg_path"
		fi
	done

	echo "Add $env_to_add to path"
}

__epkg_update_path() {
	local file

	__epkg_add_path common
	for file in $EPKG_CONFIG_DIR/enabled-envs/*
	do
		env_to_add=${file##*/}
		[ "$env_to_add" != $env ] && [ "$env_to_add" != "common" ] &&
		__epkg_add_path $env_to_add
	done
}

__epkg_enable_environment() {
	local env=$1
	local epkg_path=

	_check_env_enabled $env
	if [ $? -eq 0 ]; then
		echo "$env already enabled!"
		return
	fi

	if [ -d "$EPKG_ENVS_ROOT/$env" ]; then
		ln -sT "$EPKG_ENVS_ROOT/$env" "$EPKG_CONFIG_DIR/enabled-envs/$env"
	fi
	__epkg_update_path $env
	__epkg_add_path $env
	__epkg_create_path_rc "$epkg_path"
	__epkg_rehash
	source $RC_PATH
	echo "Environment '$env' added to PATH."
}

__epkg_disable_environment() {
	local env=$1
	local epkg_path=

	_check_env_enabled $env
	if [ $? -eq 1 ]; then
		echo "$env already disabled!"
		return
	fi

	rm -f "$EPKG_CONFIG_DIR/enabled-envs/$env"
	__epkg_update_path $env
	__epkg_create_path_rc "$epkg_path"
	__epkg_rehash
	source $RC_PATH

	echo "Environment '$env' removed from PATH."
}

__epkg_activate_environment() {
	local env=$1
	local epkg_path=

	__epkg_rehash
	if [[ "$env" != "common" ]]; then
		__epkg_add_path common
	fi
	__epkg_add_path $env

	local ORIGIN_PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
	export PATH="$epkg_path:$ORIGIN_PATH"
	export EPKG_ENV_NAME=$env
	set_epkg_env_dirs $env

	echo "Environment '$env' activated."
}

__epkg_deactivate_environment() {
	local epkg_path=

	__epkg_rehash
	__epkg_add_path common
	__epkg_add_path main

	local ORIGIN_PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
	export PATH="$epkg_path:$ORIGIN_PATH"
	export EPKG_ENV_NAME=main
	set_epkg_env_dirs main

	echo "Environment '$env' deactivated."
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
