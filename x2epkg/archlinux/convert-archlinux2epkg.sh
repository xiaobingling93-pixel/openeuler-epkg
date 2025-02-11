#!/bin/bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

arch_file=$1
epkg_repo_path="$OUT_DIR"
if [ "$epkg_repo_path" == "" ]; then
  epkg_repo_path=$(dirname "$arch_file")
fi
epkg_conversion_dir="${HOME}/epkg_conversion"

init_conversion_dirs()
{
	rm -rf ${epkg_conversion_dir}/*

	mkdir -p ${epkg_conversion_dir}/{fs,info}
	mkdir -p ${epkg_conversion_dir}/info/pgp
	mkdir -p ${epkg_conversion_dir}/info/install
	touch ${epkg_conversion_dir}/info/{package.json,files}
}

decompress_tar()
{
  tar --use-compress-program=unzstd -xvf "${arch_file}" -C "${epkg_conversion_dir}/fs/" 2>/dev/null
}

generate_files()
{
	find ${epkg_conversion_dir}/fs/ -mindepth 1 -exec stat --format='%n mode=%a size=%s' {} \; > ${epkg_conversion_dir}/info/files

	sed -i "s|^${epkg_conversion_dir}/fs/||" "${epkg_conversion_dir}/info/files"
	# 生成package.json
	tmp_dir=$(mktemp -d)
	if [ -f "${epkg_conversion_dir}/fs/.INSTALL" ]; then
	  python3 archlinux/gen-install-scriptlets.py "${epkg_conversion_dir}/fs/.INSTALL" "${epkg_conversion_dir}/info/"
	fi
	python3 archlinux/gen-package.py "${epkg_conversion_dir}/fs/.PKGINFO" "${epkg_conversion_dir}/info/" "$tmp_dir"
	python3 lib/compress2epkg.py "$epkg_repo_path/info/"  # common method
  # python3 archlinux/gen-install-scriptlets.py test/archlinux/lib32-fakeroot/.INSTALL test/info/
	rm -rf "$tmp_dir"
}

init_conversion_dirs
decompress_tar
generate_files
