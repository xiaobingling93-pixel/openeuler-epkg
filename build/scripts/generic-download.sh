#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

pkg_download() {
    for url in "${sources[@]}"; do
		echo "Downloading ${url##*/}"
        curl --silent -L -o "$BUILD_SOURCES_DIR/$(basename "$url")" "$url"
    done

    for url in "${patches[@]}"; do
        echo "Downloading ${url##*/}"
        curl --silent -L -o "$BUILD_PATCHES_DIR/$(basename "$url")" "$url"
    done   
}
