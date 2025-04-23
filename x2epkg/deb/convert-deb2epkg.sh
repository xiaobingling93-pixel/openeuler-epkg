#!/bin/bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2025 Huawei Technologies Co., Ltd. All rights reserved.

deb_file=$1
deb_name=$(basename "$deb_file")
epkg_repo_path="$OUT_DIR"
debian_origin_url="$ORIGIN_URL"
if [ "$epkg_repo_path" == "" ]; then
  epkg_repo_path=$(dirname "$deb_file")
fi

source lib/common.sh

decompress_deb()
{
  rm -f control.tar* data.tar*
  ar x "${deb_file}"
  if [ -f data.tar.gz ]; then
    tar -xzf data.tar.gz -C "${epkg_conversion_dir}/fs/"
  elif [ -f data.tar.zst ]; then
    zstd -d data.tar.zst && tar -xf data.tar -C "${epkg_conversion_dir}/fs/"
  else
    find -name "data.tar*" -exec tar xf {} -C "${epkg_conversion_dir}/fs/" \;
  fi
  if [ -f control.tar.gz ]; then
    tar -xzf control.tar.gz -C "${epkg_conversion_dir}/info/install"
  elif [ -f control.tar.zst ]; then
    zstd -d control.tar.zst && tar -xf control.tar -C "${epkg_conversion_dir}/info/install"
  else
    find -name "control.tar*" -exec tar xf {} -C "${epkg_conversion_dir}/info/install" \;
  fi
  rm -f "${epkg_conversion_dir}/info/install/"{md5sums,shlibs,triggers}
}

generate_files()
{
  generate_mtree_files
  tmp_dir=$(mktemp -d)
  # 生成package.json
  ./deb/gen-install-scriptlets.sh "${epkg_conversion_dir}/info/install"
  python3 deb/gen-package.py "${epkg_conversion_dir}/info/install/control" "${epkg_conversion_dir}/info/" "${deb_name}"
  rm -f "${epkg_conversion_dir}/info/install/control"
  python3 lib/compress2epkg.py "$epkg_repo_path" "$debian_origin_url"
  rm -rf "$tmp_dir"
}

init_conversion_dirs
decompress_deb
generate_files
