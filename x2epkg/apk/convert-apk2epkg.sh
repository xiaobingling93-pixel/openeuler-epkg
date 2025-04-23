#!/bin/bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2025 Huawei Technologies Co., Ltd. All rights reserved.

apk_file=$1
epkg_repo_path="$OUT_DIR"
if [ "$epkg_repo_path" == "" ]; then
  epkg_repo_path=$(dirname "$apk_file")
fi
apk_origin_url="$ORIGIN_URL"

source lib/common.sh

decompress_apk()
{
	tar -xzf "${apk_file}" -C "${epkg_conversion_dir}/fs/" 2>/dev/null
}

generate_files()
{
	generate_mtree_files
	# 生成package.json
	tmp_dir=$(mktemp -d)
	pkg_file="${epkg_conversion_dir}/fs/.PKGINFO"
	./apk/gen-install-scriptlets.sh "${epkg_conversion_dir}/info/install"
	python3 apk/gen-package.py "$pkg_file" "${epkg_conversion_dir}/info/"
	rm -rf "$tmp_dir" "${epkg_conversion_dir}/fs/.PKGINFO" "${epkg_conversion_dir}/fs/.SIGN.RSA.alpine"*
	python3 lib/compress2epkg.py "$epkg_repo_path" "$apk_origin_url"

}

init_conversion_dirs
decompress_apk
generate_files
