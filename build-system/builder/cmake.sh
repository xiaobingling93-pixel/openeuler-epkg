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
}

cmake_install() {
  rm -rf /opt/buildroot
  mkdir /opt/buildroot
  make install DESTDIR=/opt/buildroot
}