#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

make_build() {
  if [ -n "${makePath}" ]; then
    pushd ${makePath}
  fi
  make -j8 ${makeFlags}
  if [ $? -eq 0 ]; then
    echo "make build finished"
  else
    echo "make build failed"
    exit 1
  fi
}

make_package() {
  make install PREFIX=$BUILD_FS_DIR/usr
  if [ $? -eq 0 ]; then
    echo "make package finished"
  else
    echo "make package failed"
    exit 1
  fi
}
