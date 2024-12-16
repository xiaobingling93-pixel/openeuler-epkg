#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.


autogen_build() {
    chmod 755 ${autogen_file}
    ./"${autogen_file}"
}

autogen_package() {
    make install DESTDIR="$BUILD_FS_DIR"
}