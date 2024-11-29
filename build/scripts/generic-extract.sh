#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

# Todo: Checksum Check
pkg_decompress() {
    find "$BUILD_SOURCES_DIR" -type f | while read -r file; do
        ext="${file##*.}"
        case "$ext" in
            zip)
                unzip -q "$file" -d "$BUILD_SRC_DIR"
                echo "Decompress success: $file"
                ;;
            tar.gz|tgz|gz)
                tar -xzf "$file" -C "$BUILD_SRC_DIR"
                echo "Decompress success: $file"
                ;;
            tar.bz2)
                tar -xjf "$file" -C "$BUILD_SRC_DIR"
                echo "Decompress success: $file"
                ;;
            *)
                echo "Unknown format or no need to decompress: $file"
                ;;
        esac
    done
}