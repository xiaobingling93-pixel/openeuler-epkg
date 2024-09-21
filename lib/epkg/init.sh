#!/usr/bin/env bash
SCRIPT_DIR=$(dirname "$(readlink -f "$0")")
source "$SCRIPT_DIR/../lib/epkg/package.sh"
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
	prepare_epkg_rootfs

	__epkg_enable_environment common
	create_environment main     # main user environment
	__epkg_enable_environment main
	init_rc
}

init_rc() {
	cp -rf $PROJECT_DIR/lib/* $EPKG_ENVS_ROOT/common/profile-current/usr/lib/ &> /dev/null
	cp $PROJECT_DIR/bin/epkg $EPKG_ENVS_ROOT/common/profile-current/usr/bin/ &> /dev/null
	cp $PROJECT_DIR/../etc/epkg/channel.json $HOME_EPKG/
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

create_rootfs_symlinks() {
	ROOTFS_LINK=""
	for pkg in $(ls $HOME_EPKG/store/);
	do
		local fs_dir="$HOME_EPKG/store/$pkg/fs"
		local fs_files=$(/bin/find $fs_dir \( -type f -o -type l \))
		create_symlink_by_fs
	done
	ROOTFS_LINK=$COMMON_PROFILE_LINK
}


prepare_epkg_rootfs() {
	# download epkg_rootfs
	curl -# -o $EPKG_TEMP/elf-loader https://repo.oepkgs.net/openeuler/epkg/rootfs/elf-loader --retry 5
	chmod a+x $EPKG_TEMP/elf-loader
	/bin/cp $EPKG_TEMP/elf-loader $EPKG_ENVS_ROOT/common/profile-1/usr/bin/

	echo "download epkg rootfs"
	curl -# -o $EPKG_TEMP/store.zst https://repo.oepkgs.net/openeuler/epkg/rootfs/store.zst --retry 5
	# uncompress epkg_rootfs
	echo "install epkg rootfs, it will take 3min, please wait patiently.."
	/bin/tar --zstd -xf $EPKG_TEMP/store.zst -C $HOME_EPKG &> /dev/null
	# create comm profile-1 symlink to store
	create_rootfs_symlinks
	echo "export EPKG_INITIALIZED=yes" >> $RC_PATH
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
