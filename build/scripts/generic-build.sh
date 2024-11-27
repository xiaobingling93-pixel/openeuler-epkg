#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

# Source the required scripts
if [ -d "/opt/epkg/users/public/envs/common/" ]; then
	epkg_home_path=/opt/epkg
else
	epkg_home_path=$HOME/.epkg
fi
source $epkg_home_path/build/scripts/generic-phase.sh
source $epkg_home_path/build/workspace/scripts/pkgvars.sh
source $epkg_home_path/build/workspace/scripts/phase.sh
source $epkg_home_path/build/build-system/"$build_system".sh

# build env create
echo "build_requires:$build_requires"
source $epkg_home_path/envs/common/profile-current/usr/lib/epkg/epkg-rc.sh
epkg env create build
epkg install $build_requires
ln -sf $epkg_home_path/envs/build/profile-current/usr/bin/bash $epkg_home_path/envs/build/profile-current/usr/bin/sh

# run phase
cd $epkg_home_path/build/workspace/src/$name-$version
phases="prep patch build install"
for curPhase in ${phases[*]}; do
	runPhase "$curPhase"
done
