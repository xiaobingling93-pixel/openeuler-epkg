#!/usr/bin/env bash

# XXX: 'epkg init' should
# - not require root privilege
# - only modify $HOME, setup env for current normal user

shell=$(basename "$SHELL")
case "$shell" in
	"bash")
		RC_PATH=$HOME/.bashrc
		PROFILE_PATH=$HOME/.bash_profile
		;;
	"zsh")
		RC_PATH=$HOME/.zshrc
		PROFILE_PATH=$HOME/.zprofile
		;;
	*)
		echo "Unsupported shell: $shell"
		exit 1
		;;
esac

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

	# check epkg init ready
	if [ -d "$EPKG_ENVS_ROOT/main/" ]; then
		echo "epkg had been initialized, $USER user had been initialized"
		return 0
	fi

	local epkg_helper=
	__get_epkg_helper "install_mode"
	init_paths
	if [[ -d "$PUB_EPKG" && -d "$COMMON_PROFILE_LINK" ]]; then
		echo "epkg had been initialized, $USER user initialization is in progress ..."
		__epkg_activate_environment common
	else
		echo "epkg has not been initialized, epkg initialization is in progress ..."
		create_environment common  
		prepare_epkg_rootfs
	fi
	__epkg_enable_environment common

	create_environment main     # main user environment
	__epkg_enable_environment main
	append_user_rc
}

# append content to user shell rc file
append_user_rc() {
	if grep -qF "shell-cmd-path.sh" "$RC_PATH"; then
		echo "epkg is already initialized in '$RC_PATH'"
	else
		echo "source $EPKG_CONFIG_DIR/shell-cmd-path.sh" >> "$RC_PATH"
		echo 'export PATH="$EPKG_APPBIN_PATH:$PATH"'      >> "$RC_PATH"
		echo "For changes to take effect, close and re-open your current shell."
	fi
}

create_rootfs_symlinks() {
	ROOTFS_LINK=""
	uncompress_dir="$EPKG_STORE_ROOT"
	symlink_dir="$COMMON_PROFILE_LINK"
	for pkg in $(ls $EPKG_STORE_ROOT);
	do
		local fs_dir="$EPKG_STORE_ROOT/$pkg/fs"
		local fs_files=$($epkg_helper /bin/find $fs_dir \( -type f -o -type l \))
		create_symlink_by_fs
	done
	ROOTFS_LINK=$COMMON_PROFILE_LINK
}

prepare_epkg_rootfs() {
	# download epkg_rootfs
	$epkg_helper curl -# -o $EPKG_CACHE/elf-loader https://repo.oepkgs.net/openeuler/epkg/rootfs/elf-loader --retry 5
	$epkg_helper chmod a+x $EPKG_CACHE/elf-loader
	$epkg_helper /bin/cp $EPKG_CACHE/elf-loader $COMMON_PROFILE_LINK/usr/bin/

	echo "download epkg rootfs"
	$epkg_helper curl -# -o $EPKG_CACHE/store.tar.gz https://repo.oepkgs.net/openeuler/epkg/rootfs/store.tar.gz --retry 5
	# uncompress epkg_rootfs
	echo "install epkg rootfs, it will take 3min, please wait patiently.."
	$epkg_helper /bin/tar -xf $EPKG_CACHE/store.tar.gz --strip-components=1 -C $EPKG_STORE_ROOT &> /dev/null
	# create comm profile-1 symlink to store
	create_rootfs_symlinks
}
