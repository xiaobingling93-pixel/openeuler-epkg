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

# initialize PATH to epkg packages for bash/zsh shell
__epkg_create_path_rc() {
	local path="$1"

	cat > $EPKG_CONFIG_DIR/shell-path.sh <<EOM
## auto managed by 'epkg init|enable|disable'
export PATH="$path\$PATH"
EOM
}

__epkg_add_path() {
	local env=$1
	local env_dir='$HOME/.epkg/envs/'"$env"'/env-current'
	local dir
	for dir in usr/bin bin
	do
		[ "${path#*$env_dir/$dir}" = "$path" ] &&
		path="$env_dir/$dir:$path"
	done
}

__epkg_update_path() {
	local file
	local path=

	__epkg_add_path common

	for file in $EPKG_CONFIG_DIR/enabled-envs/*
	do
		__epkg_add_path ${file##*/}
	done

	if [ -n "$EPKG_ENV_NAME" ]; then
		__epkg_add_path $EPKG_ENV_NAME
	fi

	__epkg_create_path_rc "$path"
	__epkg_rehash
}

__epkg_enable_environment() {
	local env=$1

	ln -s "$EPKG_ENVS_ROOT/$env" "$EPKG_CONFIG_DIR/enabled-envs/$env"
	__epkg_update_path

	echo "Environment $env added to PATH."
}

__epkg_disable_environment() {
	local env=$1

	rm -f "$EPKG_CONFIG_DIR/enabled-envs/$env"
	__epkg_update_path

	echo "Environment $env removed from PATH."
}

__epkg_activate_environment() {
	local env=$1

	export EPKG_ENV_NAME=$env
	__epkg_update_path

	echo "Environment $env activated."
}

__epkg_deactivate_environment() {
	local env=$EPKG_ENV_NAME

	unset EPKG_ENV_NAME
	__epkg_update_path

	echo "Environment $env deactivated."
}

epkg() {
	local cmd="$1"
	local env="$2"
	local HOME_EPKG=$HOME/.epkg
	local EPKG_CONFIG_DIR=$HOME_EPKG/config
	case "$cmd" in
		enable)
			__epkg_enable_environment $env
			;;
		disable)
			__epkg_disable_environment $env
			;;
		activate)
			__epkg_activate_environment $env
			;;
		deactivate)
			__epkg_deactivate_environment
			;;
		*)
			command epkg "$@"
			;;
	esac
}
