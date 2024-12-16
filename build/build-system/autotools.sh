#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

configure() {
  ./configure ${configureFlags}
}


autotools_build() {
  if [ -n "${configurePath}" ]; then
    pushd ${configurePath}
  fi
  if [ ! -f "configure" ]; then
    autoreconf -vif
  fi
  configure
  make -j8 ${makeFlags}
  if [ $? -eq 0 ]; then
    echo "autotools build finished"
  else
    echo "autotools build failed"
    exit 1
  fi
}

autotools_package() {
  make install DESTDIR=$BUILD_FS_DIR
  if [ $? -eq 0 ]; then
    echo "autotools package finished"
  else
    echo "autotools package failed"
    exit 1
  fi
}
