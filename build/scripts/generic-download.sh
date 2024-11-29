#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

pkg_download() {
    for url in "${sources[@]}"; do
		echo "Downloading ${url##*/}"
        curl --silent -L -o "$epkg_sources_path/$(basename "$url")" "$url"
    done

    for url in "${patches[@]}"; do
        echo "Downloading ${url##*/}"
        curl --silent -L -o "$epkg_patches_path/$(basename "$url")" "$url"
    done   
}
