#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

runPhase() {
  phase=$1

  buildsystem_function="${buildSystem}_${phase}"
  if type $phase &> /dev/null; then
    echo "exec phase.sh ${name}_${phase} ..."
    $phase
  elif type $buildsystem_function &> /dev/null; then
    echo "exec $buildSystem ${phase} ..."
    $buildsystem_function
  else
    echo "no define phase ${phase}"
  fi
}
