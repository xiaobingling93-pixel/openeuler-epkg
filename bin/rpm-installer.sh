#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2025 Huawei Technologies Co., Ltd. All rights reserved.

ARCH=$(uname -m)
# Rpm params
RPM_BUILDROOT_DIR=$(rpmbuild --eval '%{buildroot}')
ROOTFS_DIR=${RPM_BUILDROOT_DIR}/rootfs

# for quick develop-test cycle
EPKG_STATIC=epkg
EPKG_ROOTFS=epkg-rootfs
EPKG_HELPER=epkg-helper
EPKG_HASH=epkg-hash
ELF_LOADER=elf-loader
# Global Epkg Path - Only Global Mode Use
OPT_EPKG=/opt/epkg
PUB_EPKG=$OPT_EPKG/users/public
# User Epkg Path
HOME_EPKG=$HOME/.epkg
# Epkg Mode-based Path
EPKG_INSTALL_MODE=
EPKG_COMMON_ROOT=
EPKG_STORE_ROOT=
ELFLOADER_EXEC=
EPKG_CACHE=
EPKG_PKG_CACHE_DIR=
EPKG_CHANNEL_CACHE_DIR=

# Shell Type
shell=$(basename "$SHELL")
case "$shell" in
	"bash")
		RC_PATH=$HOME/.bashrc
		;;
	"zsh")
		RC_PATH=$HOME/.zshrc
		;;
	*)
		echo "Unsupported shell: $shell"
		exit 1
		;;
esac

select_installation_mode() {
    EPKG_INSTALL_MODE="global"
    EPKG_COMMON_ROOT=$PUB_EPKG/envs/common
    EPKG_STORE_ROOT=$OPT_EPKG/store
    EPKG_CACHE=$OPT_EPKG/cache
    RC_PATH=/etc/profile.d/epkg.sh
    ELFLOADER_EXEC=$EPKG_COMMON_ROOT/profile-1/usr/bin/elf-loader
    EPKG_PKG_CACHE_DIR=$EPKG_CACHE/packages
    EPKG_CHANNEL_CACHE_DIR=$EPKG_CACHE/channel
    # Make init home
    create_init_home
}

create_init_home() {
    mkdir -p $EPKG_STORE_ROOT
    mkdir -p $EPKG_PKG_CACHE_DIR
    mkdir -p $EPKG_CHANNEL_CACHE_DIR

    mkdir -p $EPKG_COMMON_ROOT/profile-1/usr/{app-bin,bin,sbin,lib,lib64}
    mkdir -p $EPKG_COMMON_ROOT/profile-1/etc/epkg

    cd $EPKG_COMMON_ROOT/profile-1
	ln -sT "usr/bin"     "bin"
	ln -sT "usr/sbin"    "sbin"
	ln -sT "usr/lib"     "lib"
	ln -sT "usr/lib64"   "lib64"
    ln -sT "$EPKG_COMMON_ROOT/profile-1" "$EPKG_COMMON_ROOT/profile-current"
}

epkg_unpack() {
    # unpack epkg_manager
    local EPKG_MANAGER_DIR=$EPKG_CACHE/epkg-manager

    cp    $EPKG_MANAGER_DIR/bin/epkg.sh                              $EPKG_COMMON_ROOT/profile-1/usr/bin/
	cp    $EPKG_MANAGER_DIR/bin/epkg-uninstaller.sh                  $EPKG_COMMON_ROOT/profile-1/usr/bin/
    cp -a $EPKG_MANAGER_DIR/lib/epkg                                 $EPKG_COMMON_ROOT/profile-1/usr/lib/
    cp    $EPKG_MANAGER_DIR/channel.json                             $EPKG_COMMON_ROOT/profile-1/etc/epkg/
    cp    $EPKG_MANAGER_DIR/channel/openEuler-24.03-LTS-channel.yaml $EPKG_COMMON_ROOT/profile-1/etc/epkg/channel.yaml
    echo -e "{\n}" >                                                 $EPKG_COMMON_ROOT/profile-1/installed-packages.json

    # unpack epkg static binary
    cp $EPKG_CACHE/$EPKG_STATIC-$ARCH  $EPKG_COMMON_ROOT/profile-1/usr/bin/$EPKG_STATIC

    # unpack epkg build
    if [[ "$EPKG_INSTALL_MODE" == "global" ]]; then
        cp -a $EPKG_MANAGER_DIR/build  $OPT_EPKG
    else
        cp -a $EPKG_MANAGER_DIR/build  $HOME_EPKG
    fi

    # unpack epkg hash
    cp $EPKG_CACHE/$EPKG_HASH-$ARCH $EPKG_COMMON_ROOT/profile-1/usr/bin/$EPKG_HASH

    # unpack epkg_helper
    if [[ "$EPKG_INSTALL_MODE" == "global" ]]; then
        /bin/cp -rf $EPKG_CACHE/$EPKG_HELPER-$ARCH $EPKG_COMMON_ROOT/profile-1/usr/bin/$EPKG_HELPER
        chown -R $USER:$USER $OPT_EPKG
        chmod -R 755 $OPT_EPKG
        chmod 4755 $EPKG_COMMON_ROOT/profile-1/usr/bin/$EPKG_HELPER
    else
        chown -R $USER:$USER $HOME_EPKG
        chmod -R 755 $HOME_EPKG
    fi

    # unpack elf loader
	/bin/cp -f $EPKG_CACHE/$ELF_LOADER-$ARCH $ELFLOADER_EXEC
    chmod a+x $ELFLOADER_EXEC
}

epkg_change_bashrc() {
    # User-based cat
    if [[ "$(id -u)" = "0" && "$EPKG_INSTALL_MODE" == "global" ]]; then
        cat << EOF >> $RC_PATH

# epkg begin
source /opt/epkg/users/public/envs/common/profile-current/usr/lib/epkg/epkg-rc.sh
# epkg end
EOF
    else
        cat << EOF >> $RC_PATH

# epkg begin
source $HOME/.epkg/envs/common/profile-current/usr/lib/epkg/epkg-rc.sh
# epkg end
EOF
    fi
}

prepare_conf() {
    # curl resolv.conf
    cp /etc/resolv.conf $EPKG_COMMON_ROOT/profile-current/etc/resolv.conf
    mkdir -p $EPKG_COMMON_ROOT/profile-current/etc/pki/ca-trust/extracted/pem/
    cp /etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem  $EPKG_COMMON_ROOT/profile-current/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem
}

prepare_epkg_rootfs() {
	# uncompress epkg_rootfs
	echo "install epkg rootfs, it will take 3min, please wait patiently.."
	/bin/tar -zxf $EPKG_CACHE/$EPKG_ROOTFS-$ARCH.tar.gz --strip-components=1 -C $EPKG_STORE_ROOT &> /dev/null
    /bin/chmod -R 755 $EPKG_STORE_ROOT

	# create comm profile-1 symlink to store
	create_rootfs_symlinks
    echo "Environment common created."
}

create_rootfs_symlinks() {
	uncompress_dir="$EPKG_STORE_ROOT"
	symlink_dir="$EPKG_COMMON_ROOT/profile-1"
	for pkg in $(ls $EPKG_STORE_ROOT);
	do
		local fs_dir="$EPKG_STORE_ROOT/$pkg/fs"
		local fs_files=$(/bin/find $fs_dir \( -type f -o -type l \))
		create_symlink_by_fs
	done
}

create_symlink_by_fs() {

	while IFS= read -r fs_file; do
		rfs_file=${fs_file#$fs_dir}

		$ROOTFS_LINK/bin/ls $fs_file &> /dev/null || continue

		$ROOTFS_LINK/bin/mkdir -p "$symlink_dir/$($ROOTFS_LINK/bin/dirname "$rfs_file")"

		if [ "${fs_file#*/bin/}" != "$fs_file" ]; then
			handle_exec "$fs_file" && continue
		fi

		if [ "${fs_file#*/sbin/}" != "$fs_file" ]; then
			handle_exec "$fs_file" && continue
		fi

		if [[ "${fs_file}" == *"/etc/"* ]]; then
		    $ROOTFS_LINK/bin/cp -r $fs_file $symlink_dir/$rfs_file &> /dev/null
			continue
		fi

		[ -e "$symlink_dir/$rfs_file" ] && continue

		[[ "$rfs_file" =~  "/etc/yum.repos.d" ]] && continue

        $ROOTFS_LINK/bin/ln -s "$fs_file" "$symlink_dir/$rfs_file"
	done <<< "$fs_files"
}

handle_exec() {
	local file_type=$($ROOTFS_LINK/bin/file $1)
	if [[ "$file_type" =~ 'ELF 64-bit LSB shared object' ]]; then
		handle_elf
	elif [[ "$file_type" =~ 'ELF 64-bit LSB pie executable' ]]; then
		handle_elf
	elif [[ "$file_type" =~ 'ELF 64-bit LSB executable' ]]; then
		handle_elf
	elif [[ "$file_type" =~ 'ASCII text executable' ]]; then
		$ROOTFS_LINK/bin/cp $fs_file $symlink_dir/$rfs_file
    elif [[ "$file_type" =~ 'symbolic link' ]]; then
		handle_symlink
	fi
}

handle_symlink() {
	local ln_fs_file=$($ROOTFS_LINK/bin/readlink -f  $fs_file)
    if [ ! -e "$ln_fs_file" ]; then
        return 1
    fi

	local ln_rfs=${ln_fs_file#$fs_dir}
	ln -sf $symlink_dir/$ln_rfs $symlink_dir/$rfs_file
}

handle_elf() {
	local id1="{{SOURCE_ENV_DIR LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9}}"
	local id2="{{TARGET_ELF_PATH LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9 LONG0 LONG1 LONG2 LONG3 LONG4 LONG5 LONG6 LONG7 LONG8 LONG9}}"

	$ROOTFS_LINK/bin/cp $ELFLOADER_EXEC $symlink_dir/$rfs_file
    replace_string "$symlink_dir/$rfs_file" "$id1" "$symlink_dir"
    replace_string "$symlink_dir/$rfs_file" "$id2" "$fs_file"
}

replace_string() {
	local binary_file="$1"
	local long_id="$2"
	local str="$3"

	local position=$($ROOTFS_LINK/bin/grep -m1 -oba "$long_id" $binary_file | $ROOTFS_LINK/bin/cut -d ":" -f 1)
	[ -n "$position" ] && {
		$ROOTFS_LINK/bin/echo -en "$str\0" | $ROOTFS_LINK/bin/dd of=$binary_file bs=1 seek="$position" conv=notrunc status=none
	}
}

select_installation_mode

epkg_unpack
epkg_change_bashrc

prepare_conf
prepare_epkg_rootfs

$EPKG_COMMON_ROOT/profile-1/usr/bin/epkg.sh init

# vim: sw=4 ts=4 et
