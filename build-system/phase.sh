#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.


prep() {
  pushd /root/workspace
}

build() {
  echo "$build_system build"
  "$build_system"_build
}

install() {
  echo "$build_system install"
  "$build_system"_install
}

prep
build
install