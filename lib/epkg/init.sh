#!/usr/bin/env bash

EPKG_TMP=/tmp/$USER
EPKG_ROOTFS_TAR_NAME="epkg_rootfs"

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
	__epkg_enable_environment common
	create_environment main     # main user environment
	__epkg_enable_environment main
	init_rc
	prepare_rootfs
}

init_rc() {
	cp -rf $PROJECT_DIR/lib/* $EPKG_ENVS_ROOT/common/profile-current/usr/lib/
	cp $PROJECT_DIR/bin/epkg $EPKG_ENVS_ROOT/common/profile-current/usr/bin/
	cp $PROJECT_DIR/channel.json $HOME_EPKG/
	append_user_rc
}

# append content to user shell rc file
append_user_rc() {
	if grep -qF "shell-add-path.sh" "$RC_PATH"; then
		echo "epkg is already initialized in '$RC_PATH'"
	else
		echo "source $HOME/.epkg/config/shell-add-path.sh" >> "$RC_PATH"
		echo "source $EPKG_RC" >> "$RC_PATH"
		echo "For changes to take effect, close and re-open your current shell."
	fi
}

prepare_rootfs() {
	if [ ! -d $EPKG_TMP/$EPKG_ROOTFS_TAR_NAME ]; then
		echo "No $EPKG_ROOTFS_TAR_NAME exist!"
		retrun 1
	fi

	cp -ar $EPKG_TMP/epkg_rootfs/* "$EPKG_ENVS_ROOT/common/profile-1"
	__fix_rootfs_needed $EPKG_ENVS_ROOT/common/profile-1/
	echo "export EPKG_INITIALIZED=yes" >> $RC_PATH

	return 0
}
