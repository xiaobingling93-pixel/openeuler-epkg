#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.


# Source the required scripts
source /root/skel.sh
source /root/params_parser.sh
source /root/"$build_system".sh
source /root/phase.sh


# Check if the build was successful
if [ $? -eq 0 ]; then
  echo "build success"
else
  echo "build failed"
fi
