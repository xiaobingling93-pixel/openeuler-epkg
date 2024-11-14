#!/usr/bin/env bash

OPT_EPKG=/opt/epkg
HOME_EPKG=$HOME/.epkg
PUB_EPKG=$OPT_EPKG/users/public
EPKG_ENVS_ROOT=$HOME_EPKG/envs
EPKG_CONFIG_DIR=$HOME_EPKG/config
# These PATHs are based on the installation mode
EPKG_ENV_COMM_ROOT=$EPKG_ENVS_ROOT
EPKG_TEMP=$HOME_EPKG/tmp
EPKG_CACHE=$HOME/.cache/epkg
EPKG_STORE_ROOT=$HOME_EPKG/store
if [ -d "$PUB_EPKG" ]; then
	EPKG_ENV_COMM_ROOT=$PUB_EPKG/envs
	EPKG_TEMP=$PUB_EPKG/tmp
	EPKG_CACHE=$OPT_EPKG/cache
	EPKG_STORE_ROOT=$OPT_EPKG/store
fi
EPKG_PKG_CACHE_DIR=$EPKG_CACHE/packages
EPKG_CHANNEL_CACHE_DIR=$EPKG_CACHE/channel
# These PATHs are related to the common env
COMMON_PROFILE_LINK=$EPKG_ENV_COMM_ROOT/common/profile-current
if [ -d "$COMMON_PROFILE_LINK" ]; then
	export PROJECT_DIR=$COMMON_PROFILE_LINK/usr
fi
EPKG_EXEC=$COMMON_PROFILE_LINK/usr/bin/epkg
EPKG_RC=$COMMON_PROFILE_LINK/usr/lib/epkg/epkg-rc.sh
FAKEROOT_EXEC=$COMMON_PROFILE_LINK/usr/bin/fakeroot
ELFLOADER_EXEC=$COMMON_PROFILE_LINK/usr/bin/elf-loader

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

init_paths() {
	# global PATH
	$epkg_helper mkdir -p $EPKG_TEMP
	$epkg_helper mkdir -p $EPKG_STORE_ROOT
	$epkg_helper mkdir -p $EPKG_PKG_CACHE_DIR
	# user PATH
	mkdir -p $EPKG_CONFIG_DIR/enabled-envs
	#init_opt_dir
}

# In normal user installation, cannot write to /opt dir.
# So need redirect /opt accesses to $HOME/.epkg/opt
init_opt_dir() {
	mkdir -p /opt/epkg/store 2>/dev/null
	if [  -d /opt/epkg/store ]; then
		# prepare for mount --bind $HOME/.epkg/store /opt/epkg/store
		:
	else
		# prepare for mount --bind $HOME/.epkg/opt /opt
		mkdir -p $HOME_EPKG/opt/epkg
		ln -s $HOME_EPKG/store $HOME_EPKG/opt/epkg/store

		# Pitfall: the other /opt/* will be hidden by the mount.
		# Fortunately they'll normally only be accessed by the software
		# installed in /opt.
	fi
}

set_epkg_env_dirs() {
	local env=$1
	local curr_env_root=
	__get_curr_env_root $env
	CURRENT_PROFILE_LINK=$curr_env_root/$env/profile-current
	CURRENT_PROFILE_DIR=$(realpath $CURRENT_PROFILE_LINK)
	RPMDB_DIR=$CURRENT_PROFILE_DIR/var/lib/rpm
	EPKG_VARLIB_DIR=$CURRENT_PROFILE_DIR/var/lib/epkg
}
