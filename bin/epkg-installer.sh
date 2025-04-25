#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

# Check if the architecture is either x86_64 or aarch64
ARCH=$(uname -m)
if [ "$ARCH" != "x86_64" ] && [ "$ARCH" != "aarch64" ]; then
    echo "Error: This script only supports x86_64 and aarch64 architectures."
    echo "Your system architecture is: $ARCH"
    exit 1
fi
PAGE_SIZE=$(getconf PAGE_SIZE)
# Download File
EPKG_URL=https://repo.oepkgs.net/openeuler/epkg/rootfs/
# for quick develop-test cycle
EPKG_VERSION=master
EPKG_MANAGER_URL=https://gitee.com/openeuler/epkg/repository/archive/$EPKG_VERSION.tar.gz
EPKG_MANAGER_TAR=$EPKG_VERSION.tar.gz
EPKG_STATIC=epkg
EPKG_ROOTFS=epkg-rootfs
if [ "$PAGE_SIZE" -eq 65536 ]; then
    ELF_LOADER=elf-loader-64k
else
    ELF_LOADER=elf-loader
fi
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

dependency_check() {
    local cmd_names="id tar cat cp chmod chown curl"
    local cmd
    local missing_cmds=

    for cmd in $cmd_names; do
        if ! command -v "$1" >/dev/null $cmd; then
            missing_cmds="$missing_cmds $cmd"
        fi
    done

    if [[ -n "$missing_cmds" ]]; then
        echo "Commands '$missing_cmds' not found, please install first"
        return 1
    fi

    return 0
}

select_installation_mode() {
    # User-based choice
    if [[ "$(id -u)" = "0" ]]; then
        echo "Attention: Execute by $USER, Select the installation mode"
        echo "1: user   mode: epkg will be installed in the $HOME/.epkg/"
        echo "2: global mode: epkg will be installed in the /opt/epkg/"
        read mode_choice
    else
        echo "Attention: Execute by $USER, epkg will be installed in the $HOME/.epkg/, sure to continue? (y: continue, others: exit)"
        read mode_choice
        if [[ "$mode_choice" == "y" ]]; then
            mode_choice=1
        fi
    fi
    # Set epkg var
    if [[ "$mode_choice" == "1" ]]; then
        EPKG_INSTALL_MODE="user"
        EPKG_COMMON_ROOT=$HOME_EPKG/envs/common
        EPKG_STORE_ROOT=$HOME_EPKG/store
        EPKG_CACHE=$HOME/.cache/epkg
    elif [[ "$mode_choice" == "2" && "$(id -u)" = "0" ]]; then
        EPKG_INSTALL_MODE="global"
        EPKG_COMMON_ROOT=$PUB_EPKG/envs/common
        EPKG_STORE_ROOT=$OPT_EPKG/store
        EPKG_CACHE=$OPT_EPKG/cache
        RC_PATH=/etc/profile.d/epkg.sh
    elif [[ "$mode_choice" == "2" && "$(id -u)" != "0" ]]; then
        echo "Attention: Please use the root user to execute the global installation mode"
        return 1
    else
        echo "epkg installer exit!"
        return 1
    fi
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

    mkdir -p $EPKG_COMMON_ROOT/profile-1/usr/{ebin,bin,sbin,lib,lib64}
    mkdir -p $EPKG_COMMON_ROOT/profile-1/etc/epkg

    cd $EPKG_COMMON_ROOT/profile-1
	ln -sT "usr/bin"     "bin"
	ln -sT "usr/sbin"    "sbin"
	ln -sT "usr/lib"     "lib"
	ln -sT "usr/lib64"   "lib64"
    ln -sT "$EPKG_COMMON_ROOT/profile-1" "$EPKG_COMMON_ROOT/profile-current"
}

epkg_verify_checksum() {
    local checksum_file=$1
    pushd "$EPKG_CACHE" > /dev/null
    if ! sha256sum -c "$checksum_file" > /dev/null 2>&1; then
        echo "checksum error: $checksum_file"
        popd > /dev/null
        exit 1
    fi
    popd > /dev/null  # 返回原始目录
}

epkg_download() {
    # download epkg_manager
    echo "download epkg manager"
    curl -# -o $EPKG_CACHE/$EPKG_MANAGER_TAR --max-redirs 3 --location $EPKG_MANAGER_URL

    # download static epkg binary
    echo "download static epkg binary"
	curl -# -o $EPKG_CACHE/$EPKG_STATIC-$ARCH $EPKG_URL/$EPKG_STATIC-$ARCH --retry 5
    curl -# -o $EPKG_CACHE/$EPKG_STATIC-$ARCH.sha256 $EPKG_URL/$EPKG_STATIC-$ARCH.sha256
    epkg_verify_checksum "$EPKG_STATIC-$ARCH.sha256"

    # download epkg elf loader
    echo "download epkg elf loader"
	curl -# -o $EPKG_CACHE/$ELF_LOADER-$ARCH $EPKG_URL/$ELF_LOADER-$ARCH --retry 5
    curl -# -o $EPKG_CACHE/$ELF_LOADER-$ARCH.sha256 $EPKG_URL/$ELF_LOADER-$ARCH.sha256
    epkg_verify_checksum "$ELF_LOADER-$ARCH.sha256"
}

epkg_unpack() {
    # unpack epkg_manager
    mkdir -p $EPKG_CACHE/epkg-manager
    tar -xvf $EPKG_CACHE/$EPKG_MANAGER_TAR --strip-components 1 -C $EPKG_CACHE/epkg-manager > /dev/null
    local EPKG_MANAGER_DIR=$EPKG_CACHE/epkg-manager

    cp    $EPKG_MANAGER_DIR/bin/epkg.sh                              $EPKG_COMMON_ROOT/profile-1/usr/bin/
    cp -a $EPKG_MANAGER_DIR/lib/epkg                                 $EPKG_COMMON_ROOT/profile-1/usr/lib/
    cp    $EPKG_MANAGER_DIR/channel/openEuler-24.03-LTS-channel.yaml $EPKG_COMMON_ROOT/profile-1/etc/epkg/channel.yaml
    echo -e "{\n}" >                                                 $EPKG_COMMON_ROOT/profile-1/installed-packages.json

    # unpack epkg build
    if [[ "$EPKG_INSTALL_MODE" == "global" ]]; then
        cp -a $EPKG_MANAGER_DIR/build  $OPT_EPKG
    else
        cp -a $EPKG_MANAGER_DIR/build  $HOME_EPKG
    fi

    # chmod
    if [[ "$EPKG_INSTALL_MODE" == "global" ]]; then
        chown -R $USER:$USER $OPT_EPKG
        chmod -R 755 $OPT_EPKG
    else
        chown -R $USER:$USER $HOME_EPKG
        chmod -R 755 $HOME_EPKG
    fi

    # unpack epkg static binary
    cp $EPKG_CACHE/$EPKG_STATIC-$ARCH  $EPKG_COMMON_ROOT/profile-1/usr/bin/$EPKG_STATIC
    if [[ "$EPKG_INSTALL_MODE" == "global" ]]; then
        chmod 4755 $EPKG_COMMON_ROOT/profile-1/usr/bin/$EPKG_STATIC
    else
        chmod 755 $EPKG_COMMON_ROOT/profile-1/usr/bin/$EPKG_STATIC
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
    chmod 755 $EPKG_COMMON_ROOT/profile-current/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem
}

# step 0. dependency check
dependency_check || exit 1

# step 1. select installation mode
select_installation_mode || exit 1
echo "Attention: Directories $EPKG_CACHE and $EPKG_COMMON_ROOT will be created."
echo "Attention: File $RC_PATH will be modified."

# step 2. download - unpack - change bashrc
epkg_download
epkg_unpack
epkg_change_bashrc

# step 3. common env init
prepare_conf

# step 4. automic init
$EPKG_COMMON_ROOT/profile-1/usr/bin/epkg.sh init

# vim: sw=4 ts=4 et
