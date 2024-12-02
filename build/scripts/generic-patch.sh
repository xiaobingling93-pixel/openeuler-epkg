#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

pkg_patch() {
    for url in "${patches[@]}";do
        patch -p1 -d "$BUILD_SRC_DIR/${name}-${version}" < "$BUILD_PATCHES_DIR/$(basename "$url")"
    done
}