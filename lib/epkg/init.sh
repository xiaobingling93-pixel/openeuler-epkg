#!/usr/bin/env bash

EPKG_TMP=/tmp/
EPKG_ROOTFS_TAR_NAME="epkg_rootfs.tar.gz"

epkg_init() {
	local reverse=false

	while [ $# -gt 0 ]; do
		case "$1" in
			--reverse)
				reverse=true
				;;
			*)
				echo "Invalid option: $1"
				return 1
				;;
		esac
		shift
	done

	init_paths
	create_environment common   # package manage tools etc.
	create_environment main     # main user environment
	__epkg_update_path
	init_rc
	prepare_rootfs
}

init_rc() {
	local shell

	shell=$(basename "$SHELL")

	local rc_path
	case "$shell" in
		"bash")
			rc_path="$HOME/.bashrc"
			;;
		"zsh")
			rc_path="$HOME/.zshrc"
			;;
		*)
			echo "Unsupported shell: $shell"
			return 1
			;;
	esac
	append_user_rc "$rc_path"
}

# append content to user shell rc file
append_user_rc() {
	local rc_path="$1"

	if grep -qF "shell-add-path.sh" "$rc_path"; then
		echo "epkg is already initialized in '$rc_path'"
	else
		echo 'source $HOME/.epkg/config/shell-add-path.sh' >> "$rc_path"
		echo "source $EPKG_RC" >> "$rc_path"
		echo "For changes to take effect, close and re-open your current shell."
	fi
}

prepare_rootfs() {
	if [ ! -f $EPKG_TMP/$EPKG_ROOTFS_TAR_NAME ]; then
		echo "No $EPKG_ROOTFS_TAR_NAME exist!"
		retrun
	fi
	tar -zxvf $EPKG_TMP/$EPKG_ROOTFS_TAR_NAME -C $EPKG_TMP
	cp -ar $EPKG_TMP/epkg_rootfs/* "$EPKG_ENVS_ROOT/common/profile-1"
	__fix_rootfs_needed $EPKG_ENVS_ROOT/common/profile-1/
}