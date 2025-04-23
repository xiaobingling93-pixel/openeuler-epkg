#!/bin/bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

origin_directory="$1"
output_directory="$2"

declare -A SCRIPTLET2FILE=(
	["pre-link.sh"]="pre"
	["pre-unlink.sh"]="preun"
	["post-link.sh"]="post"
	["post-unlink.sh"]="postun"
)

for file_name in "${!SCRIPTLET2FILE[@]}"; do
  script_name="${SCRIPTLET2FILE[$file_name]}"
  if [ -f "$output_directory/$file_name" ]; then
      mv "$origin_directory/$file_name" "$output_directory/$script_name"
  fi
done
