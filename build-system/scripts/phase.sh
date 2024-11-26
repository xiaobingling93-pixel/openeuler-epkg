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
    echo "exec phase.sh basic_${phase} ..."
    basic_$phase
  fi
}

basic_prep() {
  echo "exec phase.sh basic_prep"
}

basic_patch() {
  patch_files=$(find "$epkg_patches_path" -type f -name "*.patch")
  while IFS= read -r patch_file; do
    echo "Patching file: $patch_file"
    patch -p1 -N < "$patch_file"
  done <<< "$patch_files"
}

basic_build() {
  echo "exec phase.sh basic_build"
}

basic_install() {
  echo "exec phase.sh basic_install"
}
