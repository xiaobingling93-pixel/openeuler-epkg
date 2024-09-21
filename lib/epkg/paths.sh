#!/usr/bin/env bash

OPT_EPKG=/opt/epkg
HOME_EPKG=$HOME/.epkg

EPKG_TEMP=$HOME_EPKG/.temp
EPKG_CONFIG_DIR=$HOME_EPKG/config
EPKG_ENVS_ROOT=$HOME_EPKG/envs
EPKG_STORE_ROOT=$HOME_EPKG/store
EPKG_PKG_CACHE_DIR=$HOME/.cache/epkg/packages

COMMON_PROFILE_LINK=$EPKG_ENVS_ROOT/common/profile-current

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
	mkdir -p $EPKG_TEMP
	mkdir -p $EPKG_CONFIG_DIR/enabled-envs
	mkdir -p $EPKG_STORE_ROOT
	mkdir -p $EPKG_PKG_CACHE_DIR
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
	CURRENT_PROFILE_LINK=$EPKG_ENVS_ROOT/$env/profile-current
	CURRENT_PROFILE_DIR=$(realpath $CURRENT_PROFILE_LINK)
	RPMDB_DIR=$CURRENT_PROFILE_DIR/var/lib/rpm
	EPKG_VARLIB_DIR=$CURRENT_PROFILE_DIR/var/lib/epkg
}
