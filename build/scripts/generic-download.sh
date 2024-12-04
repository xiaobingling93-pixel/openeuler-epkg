#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

# Todo: Checksum Check
src_download() {
    for url in "${sources[@]}"; do
		echo "Downloading ${url##*/}"
        local local_file="$BUILD_SOURCES_DIR/$(basename "$url")"
        curl --silent --insecure -L -o "$local_file" "$url"
        src_unpack $local_file
    done

    for url in "${patches[@]}"; do
        echo "Downloading ${url##*/}"
        local local_file="$BUILD_PATCHES_DIR/$(basename "$url")"
        curl --silent --insecure -L -o "$local_file" "$url"
        patch -p1 -d "$BUILD_SRC_DIR/${name}-${version}" < "$local_file"
    done   
}

src_unpack() {
    local file=$1
    local ext="${file##*.}"
    case "$ext" in
        zip)
            unzip -q "$file" -d "$BUILD_SRC_DIR"
            ;;
        tar.gz|tgz|gz)
            tar --no-same-owner -xzf "$file" -C "$BUILD_SRC_DIR"
            ;;
        tar.bz2)
            tar --no-same-owner -xjf "$file" -C "$BUILD_SRC_DIR"
            ;;
        *)
            ;;
    esac
}