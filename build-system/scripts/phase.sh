#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

runPhase() {
  phase=$1

  function="${build_system}_${phase}"
  if type $function &> /dev/null; then
    echo "exec $build_system ${phase} ..."
    $function
  else 
    echo "exec phase.sh ${phase} ..."
    $phase
  fi
}

prep() {
  echo "exec phase.sh prep"
}

patch() {
  echo "exec phase.sh patch"
}

build() {
  echo "exec phase.sh build"
}

install() {
  echo "exec phase.sh install"
}
