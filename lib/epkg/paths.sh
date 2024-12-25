#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

# Global Epkg Path - Only Global Mode Use
OPT_EPKG=/opt/epkg
PUB_EPKG=$OPT_EPKG/users/public
# User Epkg Path
HOME_EPKG=$HOME/.epkg
EPKG_ENVS_ROOT=$HOME_EPKG/envs
EPKG_CONFIG_DIR=$HOME_EPKG/config
# These PATHs are based on the installation mode
if [ -d "$PUB_EPKG" ]; then
	EPKG_COMMON_ROOT=$PUB_EPKG/envs/common
	EPKG_CACHE=$OPT_EPKG/cache
	EPKG_STORE_ROOT=$OPT_EPKG/store
else 
	EPKG_COMMON_ROOT=$EPKG_ENVS_ROOT/common
	EPKG_CACHE=$HOME/.cache/epkg
	EPKG_STORE_ROOT=$HOME_EPKG/store
fi
EPKG_PKG_CACHE_DIR=$EPKG_CACHE/packages
EPKG_CHANNEL_CACHE_DIR=$EPKG_CACHE/channel
# These PATHs are related to the common env
COMMON_PROFILE_LINK=$EPKG_COMMON_ROOT/profile-current
ELFLOADER_EXEC=$COMMON_PROFILE_LINK/usr/bin/elf-loader
EPKG_HELPER_EXEC=$EPKG_COMMON_ROOT/profile-1/usr/bin/epkg-helper

__epkg_add_path() {
	local env_to_add=$1
	local curr_env_root=
	__get_curr_env_root $env_to_add
	local appbin_path=$curr_env_root/$env_to_add/profile-current/usr/app-bin
	echo "$appbin_path:"
}

__set_epkg_curr_dir() {
	local env=$1
	local curr_env_root=
	__get_curr_env_root $env
	CURRENT_PROFILE_LINK=$curr_env_root/$env/profile-current
	CURRENT_PROFILE_DIR=$(realpath $CURRENT_PROFILE_LINK)
}
