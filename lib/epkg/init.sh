#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

__epkg_init() {
	# check epkg init ready
	if [ -d "$EPKG_ENVS_ROOT/main/" ]; then
		echo "epkg had been initialized, $USER user had been initialized"
		return 0
	fi

	if [[ -d "$PUB_EPKG" && -d "$COMMON_PROFILE_LINK" ]]; then
		echo "epkg had been initialized, $USER user initialization is in progress ..."
	else
		echo "epkg has not been initialized, epkg initialization is in progress ..."
	fi
	mkdir -p $EPKG_STORE_ROOT
	mkdir -p $EPKG_PKG_CACHE_DIR
	mkdir -p $EPKG_CHANNEL_CACHE_DIR
	mkdir -p $EPKG_CONFIG_DIR/registered-envs

	__epkg_create_environment main     # main user environment
	__epkg_register_environment main
	echo "Warning: For changes to take effect, close and re-open your current shell."
}

__check_epkg_user_init() {
	if [ ! -d "$EPKG_ENVS_ROOT/main/" ]; then
		return 1
	fi
}