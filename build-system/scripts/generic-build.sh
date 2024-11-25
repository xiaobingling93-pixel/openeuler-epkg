#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

# Source the required scripts
source /root/workspace/scripts/pkgvars.sh
source /root/workspace/scripts/"$build_system".sh
source /root/workspace/scripts/phase.sh

cd $epkg_src_path/$name-$version

phases="prep patch build install"
# XXX: use some_name coding style in shell script, except if the variable comes from pkg.yaml
for curPhase in ${phases[*]}; do
	runPhase "$curPhase"
done
