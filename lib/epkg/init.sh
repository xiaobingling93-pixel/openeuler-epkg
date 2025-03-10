#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

__epkg_init() {
	# rpm install init script: DevStation may no internet
	if rpm -q epkg >/dev/null 2>&1; then
		ARCH=$(uname -m)
        echo "epkg package is rpm installed. exec external script."

		local epkg_helper=
		__get_epkg_helper "install_mode" ""
		# prepare_conf
    	$epkg_helper cp /etc/resolv.conf $EPKG_COMMON_ROOT/profile-current/etc/resolv.conf
		$epkg_helper mkdir -p $EPKG_COMMON_ROOT/profile-current/etc/pki/ca-trust/extracted/pem/
		$epkg_helper cp /etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem  $EPKG_COMMON_ROOT/profile-current/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem
		$epkg_helper chmod 755 $EPKG_COMMON_ROOT/profile-current/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem

		# Create symlinks for installed packages
		$epkg_helper tar -zxf $EPKG_CACHE/epkg-rootfs-$ARCH.tar.gz --strip-components=1 -C $EPKG_STORE_ROOT &> /dev/null
		symlink_dir=$EPKG_COMMON_ROOT/profile-current
		for pkg in $(ls $EPKG_STORE_ROOT); do
			fs_dir="$EPKG_STORE_ROOT/$pkg/fs"
			$EPKG_COMMON_ROOT/profile-1/usr/bin/epkg localinstall "$fs_dir" "$symlink_dir"
		done
    fi

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
	# mkdir $HOME/.epkg/registered-envs
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

# vim: sw=4 ts=4 et
