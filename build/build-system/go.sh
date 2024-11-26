#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

go_build() {
  if [ -n "${goPath}" ]; then
    pushd ${goPath}
  fi
  go build
}

go_install() {
  export GOPATH="/opt/buildroot"
  export PATH=$PATH:$GOPATH/bin
  go install
}