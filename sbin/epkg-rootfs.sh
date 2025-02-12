#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

ARCH=$(uname -m)
repo_url=https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.03-LTS/everything/$ARCH
store_url=$repo_url/store
pkg_info_url=$repo_url/repodata/pkg-info.zst

EPKG_ROOTFS_CACHE=$HOME/.cache/epkg/rootfs
EPKG_ROOTFS_PKG_INFO_DIR=$EPKG_ROOTFS_CACHE/pkg-info
EPKG_ROOTFS_PKG_STORE=$EPKG_ROOTFS_CACHE/pkg-store
EPKG_ROOTFS_PKG_UNPACK=$EPKG_ROOTFS_CACHE/pkg-unpack
EPKG_ROOTFS_OUT=$EPKG_ROOTFS_CACHE/epkg-rootfs-${ARCH}

find_local_pkg_json() {
    local pkg_name="__"$1"__"
    local local_repo_dir=$2

    find "$local_repo_dir" -maxdepth 2 -mindepth 1 -type f -name "*$pkg_name*"| while read -r dir; do
        dir_name=$(basename "$dir")
        IFS='__' read -ra parts <<< "$dir_name"
        if [[ "__${parts[2]}__" == "$pkg_name" ]]; then
            echo "$dir"
            return
        fi
    done
}

download_packages() {
    local local_pkg_json_dir=$1
    local download_url=$2

    local epkg_name=$(basename $local_pkg_json_dir)
    local epkg_name=${epkg_name%.json}.epkg
    local pkg_download_url=$download_url/${epkg_name:0:2}/$epkg_name

    echo "Downloading ${pkg_download_url##*/}"
    curl --silent -o $EPKG_ROOTFS_PKG_STORE/$epkg_name $pkg_download_url
    mkdir -p $EPKG_ROOTFS_PKG_UNPACK/${epkg_name%%.epkg}
    tar --use-compress-program=zstd -xf $EPKG_ROOTFS_PKG_STORE/$epkg_name -C $EPKG_ROOTFS_PKG_UNPACK/${epkg_name%%.epkg}
}

rootfs_prep_home() {
    rm -rf $EPKG_ROOTFS_CACHE
    mkdir -p $EPKG_ROOTFS_PKG_STORE
    mkdir -p $EPKG_ROOTFS_PKG_UNPACK

    echo "Downloading $pkg_info_url"
    curl -# -o $EPKG_ROOTFS_CACHE/pkg-info.zst $pkg_info_url
    tar --use-compress-program=zstd -xf $EPKG_ROOTFS_CACHE/pkg-info.zst -C $EPKG_ROOTFS_CACHE/
}

rootfs_prep_pkg() {
    local rootfs_package=(
        coreutils tar gzip zstd jq curl grep sed gawk setup which file bash libcap file-libs fuse libpng 
        libstdc++ libtasn1 libtirpc libevent libxcrypt fuse-common cracklib ca-certificates 
        chkconfig ncurses-base pcre2 libffi libsepol basesystem newt ncurses-libs publicsuffix-list 
        krb5-libs glibc openEuler-gpg-keys libnghttp2 oniguruma pam gmp libunistring libidn2 readline 
        openEuler-release attr libselinux mpfr tzdata patchelf crypto-policies libverto audit-libs  
        libcurl libmount zlib p11-kit-trust cyrus-sasl-lib libcap-ng openssl-libs popt libpwquality 
        p11-kit ncurses bc libgcc e2fsprogs gdbm libblkid openEuler-repos libnsl2 openldap brotli keyutils-libs 
        libuuid filesystem findutils slang libpsl libacl libssh info libev libsigsegv
    )

    for pkg in "${rootfs_package[@]}"; do
        local local_pkg_dir=$(find_local_pkg_json $pkg $EPKG_ROOTFS_PKG_INFO_DIR) 
        download_packages $local_pkg_dir $store_url
    done
}

rootfs_prep_compress() {
    tar -zcf ${EPKG_ROOTFS_OUT}.tar.gz -C $EPKG_ROOTFS_PKG_UNPACK .
    echo "rootfs compress success: ${EPKG_ROOTFS_OUT}.tar.gz"
}

rootfs_prep_home
rootfs_prep_pkg
rootfs_prep_compress

# vim: sw=4 ts=4 et
