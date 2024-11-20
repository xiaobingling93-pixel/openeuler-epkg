#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.


ruby_build() {
    if [ -f *.gemspec ]; then
      gem build *.gemspec
    fi
    mkdir -p usr/
    gem install -V --local --build-root usr --force --document=ri,doc *.gem
}

ruby_install() {
    rm -rf /opt/buildroot
    mkdir /opt/buildroot
    cp -r usr/ /opt/buildroot
}