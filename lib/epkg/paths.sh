#!/usr/bin/env bash

OPT_EPKG=/opt/epkg
HOME_EPKG=$HOME/.epkg

EPKG_META_DIR=$HOME_EPKG/meta
EPKG_ENVS_ROOT=$HOME_EPKG/envs
EPKG_STORE_ROOT=$HOME_EPKG/store

EPKG_PKG_CACHE_DIR=$HOME/.cache/epkg/packages

EPKG_ENV=$EPKG_ENVS_ROOT/epkg/env-current
EPKG_EXEC=$EPKG_ENV/usr/bin/epkg
EPKG_RC=$EPKG_ENV/usr/lib/epkg/epkg-rc.sh
FAKEROOT_EXEC=$EPKG_ENV/usr/bin/fakeroot
ELFLOADER_EXEC=$EPKG_ENV/usr/bin/elf-loader

init_paths() {
	mkdir -p $EPKG_META_DIR/enabled-envs
	mkdir -p $EPKG_STORE_ROOT
	mkdir -p $EPKG_PKG_CACHE_DIR
	init_opt_dir
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

	ENV_LINK=$EPKG_ENVS_ROOT/$env/env-current
	CURRENT_ENV=$(realpath $ENV_LINK)
	RPMDB_DIR=$CURRENT_ENV/var/lib/rpm
	EPKG_VARLIB_DIR=$CURRENT_ENV/var/lib/epkg
}
