#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.


cmake_build() {
  rm -rf build_cmake
  mkdir build_cmake
  # shellcheck disable=SC2164
  cd build_cmake
  cmake .. ${cmakeFlags}
  make -j8 ${makeFlags}
  if [ $? -eq 0 ]; then
    echo "cmake build finished"
  else
    echo "cmake build failed"
    exit 1
  fi
}

cmake_package() {
  make install DESTDIR="$BUILD_FS_DIR"
  if [ $? -eq 0 ]; then
    echo "cmake package finished"
  else
    echo "cmake package failed"
    exit 1
  fi
}