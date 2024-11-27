#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

runPhase() {
  phase=$1

  buildsystem_function="${build_system}_${phase}"
  pkg_function="${name}_${phase}"
  if type $pkg_function &> /dev/null; then
    echo "exec phase.sh ${name}_${phase} ..."
    $pkg_function
  else 
    echo "exec $build_system ${phase} ..."
    $buildsystem_function
  fi
}

# parse_yaml.py generate ${name}_${phase} function
