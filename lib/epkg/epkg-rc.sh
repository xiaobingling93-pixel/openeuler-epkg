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

	cat > $EPKG_META_DIR/shell-path.sh <<EOM
## auto managed by 'epkg init|enable|disable'
export PATH="$path\$PATH"
EOM
}

__epkg_update_path() {
	local file
	local path=
	for file in $EPKG_META_DIR/enabled-envs/*
	do
		local env=${file##*/}
		local env_dir='$HOME/.epkg/envs/'"$env"'/env-current'
		local dir
		for dir in usr/sbin usr/bin sbin bin
		do
			path="$env_dir/$dir:$path"
		done
	done

	__epkg_create_path_rc "$path"
	__epkg_rehash
}

__epkg_enable_environment() {
	local env=$1

	ln -s "$EPKG_ENVS_ROOT/$env" "$EPKG_META_DIR/enabled-envs/$env"
	__epkg_update_path

	echo "Environment $env added to PATH."
}

__epkg_disable_environment() {
	local env=$1

	rm -f "$EPKG_META_DIR/enabled-envs/$env"
	__epkg_update_path

	echo "Environment $env removed from PATH."
}

epkg() {
	local cmd="$1"
	local env="$2"
	local HOME_EPKG=$HOME/.epkg
	local EPKG_META_DIR=$HOME_EPKG/meta
	case "$cmd" in
		enable)
			__epkg_enable_environment $env
			;;
		disable)
			__epkg_disable_environment $env
			;;
		*)
			command epkg "$@"
			;;
	esac
}
