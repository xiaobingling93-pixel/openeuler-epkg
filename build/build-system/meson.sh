#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

meson_build() {
  pip install ninja
  if [ -n "${mesonPath}" ]; then
    pushd ${mesonPath}
  fi
  arch=`uname -m`
  meson setup . "$(arch)_compile_gnu"
  meson compile -C "$(arch)_compile_gnu" -j 8 --verbose
  if [ $? -eq 0 ]; then
    echo "meson build finished"
  else
    echo "meson build failed"
    exit 1
  fi
}

meson_package() {
  arch=`uname -m`
  DESTDIR="$BUILD_FS_DIR" meson install -C "$(arch)_compile_gnu" --no-rebuild
  if [ $? -eq 0 ]; then
    echo "meson package finished"
  else
    echo "meson package failed"
    exit 1
  fi
}