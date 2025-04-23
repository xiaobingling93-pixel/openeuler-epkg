#!/bin/bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2025 Huawei Technologies Co., Ltd. All rights reserved.

arch_file=$1
epkg_repo_path="$OUT_DIR"
if [ "$epkg_repo_path" == "" ]; then
  epkg_repo_path=$(dirname "$arch_file")
fi
archlinux_origin_url="$ORIGIN_URL"

source lib/common.sh

decompress_tar()
{
  tar --use-compress-program=unzstd -xf "${arch_file}" -C "${epkg_conversion_dir}/fs/" 2>/dev/null
}

generate_files()
{
	generate_mtree_files
	# 生成package.json
	tmp_dir=$(mktemp -d)
	if [ -f "${epkg_conversion_dir}/fs/.INSTALL" ]; then
	  python3 archlinux/gen-install-scriptlets.py "${epkg_conversion_dir}/fs/.INSTALL" "${epkg_conversion_dir}/info/"
	fi
	python3 archlinux/gen-package.py "${epkg_conversion_dir}/fs/.PKGINFO" "${epkg_conversion_dir}/info/" "$tmp_dir"
	python3 lib/compress2epkg.py "$epkg_repo_path" "$archlinux_origin_url"  # common method
  rm -rf "$tmp_dir"
}

init_conversion_dirs
decompress_tar
generate_files
