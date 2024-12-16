#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.


ruby_build() {
  if [ -f *.gemspec ]; then
    gem build *.gemspec
  fi
  mkdir -p usr/
  gem install -V --local --build-root usr --force --document=ri,doc *.gem
  if [ $? -eq 0 ]; then
    echo "ruby package finished"
  else
    echo "ruby package failed"
    exit 1
  fi
}

ruby_package() {
  cp -r usr/ "$BUILD_FS_DIR"
}