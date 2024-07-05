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

	cat > $EPKG_CONFIG_DIR/shell-add-path.sh <<EOM
## auto managed by 'epkg init|enable|disable'
export PATH="$path\$PATH"
EOM
}

__epkg_add_path() {
	local env=$1
	local env_dir='$HOME/.epkg/envs/'"$env"'/profile-current'
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

	path=$HOME/epkg_manager/bin:$path
	__epkg_create_path_rc "$path"
	__epkg_rehash
}

__epkg_enable_environment() {
	local env=$1

	ln -s "$EPKG_ENVS_ROOT/$env" "$EPKG_CONFIG_DIR/enabled-envs/$env"
	__epkg_update_path

	echo "Environment '$env' added to PATH."
}

__epkg_disable_environment() {
	local env=$1

	rm -f "$EPKG_CONFIG_DIR/enabled-envs/$env"
	__epkg_update_path

	echo "Environment '$env' removed from PATH."
}

__epkg_activate_environment() {
	local env=$1

	export EPKG_ENV_NAME=$env
	__epkg_update_path

	echo "Environment '$env' activated."
}

__epkg_deactivate_environment() {
	local env=$EPKG_ENV_NAME

	unset EPKG_ENV_NAME
	__epkg_update_path

	echo "Environment '$env' deactivated."
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
