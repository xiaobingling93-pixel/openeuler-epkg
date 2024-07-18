#!/usr/bin/env bash
export PROJECT_DIR=$HOME/epkg_manager
source $PROJECT_DIR/lib/epkg/paths.sh
source $PROJECT_DIR/lib/epkg/env.sh

__epkg_rehash() {
	if [ -n "${ZSH_VERSION}" ]; then
		rehash
	elif [ -n "${BASH_VERSION}" ]; then
		hash -r
	else
		:  # pass
	fi
}

_init_current_rc() {
	local path=
	local ORIGIN_PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"

	path=$path:$ORIGIN_PATH
	__epkg_add_path common

	if ! echo "$path" | grep -q -F "epkg_manager"; then
		path=$path:$HOME/epkg_manager/bin
	fi

	export PATH=$path
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

	echo "update_epkg_env_name"
}

# initialize PATH to epkg packages for bash/zsh shell
__epkg_create_path_rc() {
	local path="$1"
	local ORIGIN_PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
	cat > $EPKG_CONFIG_DIR/shell-add-path.sh <<EOM
## auto managed by 'epkg init|enable|disable'
export PATH="$path:$ORIGIN_PATH"
EOM
}

__epkg_add_path() {
	local env_to_add=$1
	local env_dir=$HOME/.epkg/envs/$env_to_add/profile-current
	local dir

	for dir in usr/bin bin
	do
		tmp_path=${path#*$env_dir/$dir}
		if [ $tmp_path = $path ]; then
			path="$env_dir/$dir:$path"
		fi
	done

	echo "Add $env_to_add to path"
	echo "path: $path"
}

__epkg_update_path() {
	local file

	__epkg_add_path common
	for file in $EPKG_CONFIG_DIR/enabled-envs/*
	do
		env_to_add=${file##*/}
		[ $env_to_add != $env ] && [ $env_to_add != "common" ] &&
		__epkg_add_path $env_to_add
	done

	if ! echo "$path" | grep -q -F "epkg_manager"; then
		path=$path:$HOME/epkg_manager/bin
	fi
}

__epkg_enable_environment() {
	local env=$1
	local path=
	local env_enabled=

	_check_env_enabled $env
	if [ $env_enabled = 1 ]; then
		echo "$env already enabled"
		return
	fi

	ln -sT "$EPKG_ENVS_ROOT/$env" "$EPKG_CONFIG_DIR/enabled-envs/$env"
	__epkg_update_path $env
	__epkg_add_path $env
	__epkg_create_path_rc "$path"
	__epkg_rehash
	source $RC_PATH

	echo "Environment '$env' added to PATH."
}

__epkg_disable_environment() {
	local env=$1
	local path=
	local env_enabled=

	_check_env_enabled $env
	if [ $env_enabled = 0 ]; then
		echo "$env already disabled"
		return
	fi

	if [ $env = $EPKG_ENV_NAME ]; then
		echo "Warning: you are trying to disable current env!"
		echo "sure to continue? (y: continue, others: exit)"
		read choice
		if [ "$choice" != "y" ]; then
			return
		fi
	fi

	rm -f "$EPKG_CONFIG_DIR/enabled-envs/$env"
	__epkg_update_path $env
	__epkg_create_path_rc "$path"
	__epkg_rehash
	source $RC_PATH

	echo "Environment '$env' removed from PATH."
}

__epkg_activate_environment() {
	local env=$1
	local path=

	__epkg_rehash
	__epkg_add_path common
	__epkg_add_path $env
	path=$path:$HOME/epkg_manager/bin

	local ORIGIN_PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
	export PATH="$path:$ORIGIN_PATH"
	export EPKG_ENV_NAME=$env
	set_epkg_env_dirs $env

	echo "Environment '$env' activated."
}

__epkg_deactivate_environment() {
	local path=

	__epkg_rehash
	__epkg_add_path common
	__epkg_add_path main
	path=$path:$HOME/epkg_manager/bin

	local ORIGIN_PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
	export PATH="$path:$ORIGIN_PATH"
	export EPKG_ENV_NAME=main
	set_epkg_env_dirs main

	echo "Environment '$env' deactivated."
}

_check_env_existed() {
	local env=$1
	all_envs=$(ls -lt $EPKG_ENVS_ROOT | grep '^d' | awk '{print $9}')
	if echo "$all_envs" | grep -q -F "$env"; then
		env_existed=1
		return
	fi
	env_existed=0
}

_check_env_enabled() {
	local env=$1
	if [ -L "$EPKG_CONFIG_DIR/enabled-envs/$env" ]; then
		env_enabled=1
		return
	fi
	env_enabled=0
}

__fix_rootfs_needed() {
	local envrootfs="$1"
	mkdir -p "$envrootfs/tmp"
	find "$envrootfs" -type f -executable -exec file {} + | grep ELF | cut -d: -f1 > "$envrootfs/tmp/elf_files"
	local whitelist="linux-vdso.so.1 statically"
	while read elf_file; do
		# Add your code here to process each ELF file in envrootfs
		# echo "Processing ELF file: $elf_file"
		# 查看elf_file文件的so，并解析出实际的名称
		dependencies=$(ldd "$elf_file" | awk '{print $1}')
		for dependency in $dependencies
		do
			if [[ " ${whitelist[@]} " =~ " ${dependency} " ]]; then
				continue
			fi
			# Find the actual path of the dependency in envrootfs
			actual_path=$(grep "$(basename $dependency)" "$envrootfs/tmp/elf_files" | grep "usr/lib" | head -n1)
			if [ -n "$actual_path" ]; then
				patchelf --replace-needed "$dependency" "$actual_path" "$elf_file"
				continue
			fi

			# 如果不存在就直接查找
			actual_path=$(find "$envrootfs" -name "$(basename $dependency)" | grep "usr/lib" | head -n1)
			if [ -n "$actual_path" ]; then
				patchelf --replace-needed "$dependency" "$actual_path" "$elf_file"
			else
				echo "Dependency $dependency not found in envrootfs."
			fi
		
		done
	done  < "$envrootfs/tmp/elf_files"
	rm -rf "$envrootfs/tmp/elf_files"
}

# 重定向指定文件的依赖到rootfs中
__fix_file_needed() {
	local whitelist="linux-vdso.so.1 statically"
	local rootfs="$1"
	local elf_file="$2"
	dependencies=$(ldd "$elf_file" | awk '{print $1}')

	for dependency in $dependencies
	do
		if [[ " ${whitelist[@]} " =~ " ${dependency} " ]]; then
				continue
		fi
		# Find the actual path of the dependency in rootfs
		actual_path=$(find "$rootfs" -name "$(basename $dependency)" | grep "usr/lib" | head -n1)
		if [ -n "$actual_path" ]; then
			patchelf --replace-needed "$dependency" "$actual_path" "$elf_file"
		else
			echo "Dependency $dependency not found in envrootfs."
		fi
	
	done

}

epkg() {
	local cmd="$1"
	local env="$2"
	local HOME_EPKG=$HOME/.epkg
	local EPKG_CONFIG_DIR=$HOME_EPKG/config
	case "$cmd" in
		create)
			create_environment $env
			;;
		enable)
			__epkg_enable_environment $env
			;;
		disable)
			__epkg_disable_environment $env
			;;
		activate)
			activate_environment $env
			;;
		deactivate)
			__epkg_deactivate_environment
			;;
		*)
			command epkg "$@"
			;;
	esac
}
