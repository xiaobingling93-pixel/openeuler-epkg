#!/bin/bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2025 Huawei Technologies Co., Ltd. All rights reserved.

rpm_file=$1
epkg_repo_path="$OUT_DIR"
if [ "$epkg_repo_path" == "" ]; then
  epkg_repo_path=$(dirname "$rpm_file")
fi
rpm_origin_url="$ORIGIN_URL"

source lib/common.sh

decompress_rpm()
{
	rpm2cpio "${rpm_file}" | cpio -idm --quiet -D "${epkg_conversion_dir}/fs/" 2>/dev/null
}

generate_files()
{
	generate_mtree_files
	# 生成package.json
	tmp_dir=$(mktemp -d)
	./rpm/gen-install-scriptlets.sh "$rpm_file" "${epkg_conversion_dir}/info/"
	python3 rpm/gen-package.py "$rpm_file" "${epkg_conversion_dir}/info/" "$tmp_dir"
	python3 lib/compress2epkg.py "$epkg_repo_path" "$rpm_origin_url"
	rm -rf "$tmp_dir"
}

init_conversion_dirs
decompress_rpm
generate_files
