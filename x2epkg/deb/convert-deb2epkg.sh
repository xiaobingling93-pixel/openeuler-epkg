#!/bin/bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

deb_file=$1
deb_name=$(basename "$deb_file")
epkg_repo_path="$OUT_DIR"
if [ "$epkg_repo_path" == "" ]; then
  epkg_repo_path=$(dirname "$deb_file")
fi

source lib/common.sh

decompress_deb()
{
  ar x "${deb_file}"
  tar -xf data.tar.xz -C "${epkg_conversion_dir}/fs/" 2>/dev/null
  tar -xf control.tar.xz -C "${epkg_conversion_dir}/info/install" 2>/dev/null
  rm -f "${epkg_conversion_dir}/info/install/"{conffiles,md5sums}
}

generate_files()
{
  find ${epkg_conversion_dir}/fs/ -mindepth 1 -exec stat --format='%n mode=%a size=%s' {} \; > ${epkg_conversion_dir}/info/files

  sed -i "s|^${epkg_conversion_dir}/fs/||" "${epkg_conversion_dir}/info/files"
  tmp_dir=$(mktemp -d)
  # 生成package.json
  ./deb/gen-install-scriptlets.sh "${epkg_conversion_dir}/info/install"
  python3 deb/gen-package.py "${epkg_conversion_dir}/info/install/control" "${epkg_conversion_dir}/info/" "${deb_name}"
  rm -f "${epkg_conversion_dir}/info/install/control"
  python3 lib/compress2epkg.py "$epkg_repo_path"
  rm -rf "$tmp_dir"
}

init_conversion_dirs
decompress_deb
generate_files
