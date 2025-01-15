#!/bin/bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

#[[ $# -eq 3 ]] || {
#	echo 'Rpm file and source rpm file path is required'

#	exit 99
#}
SCRIPT_DIR=$(dirname "$(readlink -f "$0")")
source "$SCRIPT_DIR/../lib/epkg/hash.sh"

rpm_file=$1
epkg_repo_path=$2

epkg_conversion_dir="${HOME}/epkg_conversion"

init_conversion_dirs()
{
	rm -rf ${epkg_conversion_dir}/*

	mkdir -p ${epkg_conversion_dir}/{fs,info}
	mkdir -p ${epkg_conversion_dir}/info/pgp
	mkdir -p ${epkg_conversion_dir}/info/install
	touch ${epkg_conversion_dir}/info/{package.json,buildinfo.json,files}
}

decompress_rpm()
{
	rpm2cpio "${rpm_file}" | cpio -idm --quiet -D "${epkg_conversion_dir}/fs/" 2>/dev/null
}

generate_files()
{
	find ${epkg_conversion_dir}/fs/ -mindepth 1 -exec stat --format='%n mode=%a size=%s mtime=%Y' {} \; > ${epkg_conversion_dir}/info/files

	sed -i "s|^${epkg_conversion_dir}/fs/||" "${epkg_conversion_dir}/info/files"
	# 生成package.json
	tmp_dir=$(mktemp -d)
	./gen-install-scriptlets.sh "$rpm_file" "${epkg_conversion_dir}/info/"
	python3 gen-package.py "$rpm_file" "${epkg_conversion_dir}/info/" "$tmp_dir"
	python3 ../lib/compress2epkg.py "$epkg_repo_path"
	rm -rf "$tmp_dir"
}

init_conversion_dirs
decompress_rpm
generate_files
