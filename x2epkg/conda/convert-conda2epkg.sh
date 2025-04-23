#!/bin/bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2025 Huawei Technologies Co., Ltd. All rights reserved.

conda_file=$1
epkg_repo_path="$OUT_DIR"
if [ "$epkg_repo_path" == "" ]; then
  epkg_repo_path=$(dirname "$conda_file")
fi
conda_origin_url="$ORIGIN_URL"

source lib/common.sh

# decompress conda file by unzip and tar commands
decompress_tar()
{
  base_name=$(basename "${conda_file/.conda//}")
  rm -rf *.tar*
  unzip -o "${conda_file}"
  if [ -f "pkg-${base_name}.tar.zst" ]; then
    zstd -d "pkg-${base_name}.tar.zst" && tar -xf "pkg-${base_name}.tar" -C "${epkg_conversion_dir}/fs/"
  elif [ -f "pkg-${base_name}.tar.bz2" ]; then
    tar -xjf "pkg-${base_name}.tar.bz2" -C "${epkg_conversion_dir}/fs/"
  else
    echo "Can't decompress this tarball"
    exit 1
  fi
  rm -rf info
  if [ -f "info-${base_name}.tar.zst" ]; then
    zstd -d "info-${base_name}.tar.zst" && tar -xf "info-${base_name}.tar"
  elif [ -f "info-${base_name}.tar.bz2" ]; then
    tar -xjf "info-${base_name}.tar.bz2"
  else
    echo "Can't decompress this tarball"
    exit 1
  fi
}

generate_files()
{
  generate_mtree_files
  # 生成package.json
  tmp_dir=$(mktemp -d)
  ./conda/gen-install-scriptlets.sh "info/recipe" "${epkg_conversion_dir}/info/install"
  python3 conda/gen-package.py "info" "${epkg_conversion_dir}/info" "$tmp_dir"  # info参数是当前目录下的info目录
  python3 lib/compress2epkg.py "$epkg_repo_path" "$conda_origin_url"  # common method
  rm -rf "$tmp_dir" info
}

init_conversion_dirs
decompress_tar
generate_files
