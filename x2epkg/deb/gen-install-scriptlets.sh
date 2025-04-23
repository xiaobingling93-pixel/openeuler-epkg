#!/bin/bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2025 Huawei Technologies Co., Ltd. All rights reserved.

output_directory="$1"

declare -A SCRIPTLET2FILE=(
	["preinst"]="pre"
	["prerm"]="preun"
	["postinst"]="post"
	["postrm"]="postun"
)

for file_name in "${!SCRIPTLET2FILE[@]}"; do
  script_name="${SCRIPTLET2FILE[$file_name]}"
  if [ -f "$output_directory/$file_name" ]; then
      mv "$output_directory/$file_name" "$output_directory/$script_name"
  fi
done
