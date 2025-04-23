#!/bin/bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2025 Huawei Technologies Co., Ltd. All rights reserved.

output_directory="$1"

declare -A SCRIPTLET2FILE=(
	[".pre-install"]="pre"
	[".pre-deinstall"]="preun"
	[".pre-upgrade"]="preup"
	[".post-install"]="post"
	[".post-deinstall"]="postun"
	[".post-upgrade"]="postup"
)

for file_name in "${!SCRIPTLET2FILE[@]}"; do
  script_name="${SCRIPTLET2FILE[$file_name]}"
  if [ -f "$output_directory/../../fs/$file_name" ]; then
      mv "$output_directory/../../fs/$file_name" "$output_directory/$script_name"
  fi
done
