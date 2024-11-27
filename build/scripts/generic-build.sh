#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

# Source the required scripts
if [ -d "/opt/epkg/users/public/envs/common/" ]; then
	epkg_manager_path=/opt/epkg
else
	epkg_manager_path=$HOME/.epkg
fi
source $epkg_manager_path/build/scripts/generic-phase.sh
source $epkg_manager_path/build/workspace/scripts/pkgvars.sh
source $epkg_manager_path/build/workspace/scripts/phase.sh
source $epkg_manager_path/build/build-system/"$build_system".sh

cd $epkg_manager_path/build/workspace/src/$name-$version
phases="prep patch build install"
# XXX: use some_name coding style in shell script, except if the variable comes from pkg.yaml
for curPhase in ${phases[*]}; do
	runPhase "$curPhase"
done
