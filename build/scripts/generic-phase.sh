#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

runPhase() {
  phase=$1

  buildsystem_function="${buildSystem}_${phase}"
  pkg_function="${name}_${phase}"
  if type $pkg_function &> /dev/null; then
    echo "exec phase.sh ${name}_${phase} ..."
    $pkg_function
  elif type $buildsystem_function &> /dev/null; then
    echo "exec $buildSystem ${phase} ..."
    $buildsystem_function
  else
    echo "no define phase ${phase}"
  fi
}

generate_phase() {
  local phase_name="$1"
  local phase_content_var="${phase_name}_content"

  if [[ -z "${!phase_content_var}" ]]; then
    echo "not found $phase_name content"
    return 1
  fi

  local phase_content="${!phase_content_var}"

  cat <<EOF
$phase_name () {
${phase_content}
}
EOF
}
