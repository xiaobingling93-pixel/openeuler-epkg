#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

make_build() {
  if [ -n "${makePath}" ]; then
    pushd ${makePath}
  fi
  make -j8 ${makeFlags}
}

make_install() {
  make install DESTDIR=$BUILD_OUT_DIR PREFIX=$BUILD_OUT_DIR
}
