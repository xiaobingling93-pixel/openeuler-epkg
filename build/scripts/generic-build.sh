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
source $epkg_home_path/build/build-system/"$buildSystem".sh

# download & decompress
source $epkg_home_path/build/scripts/generic-download.sh
source $epkg_home_path/build/scripts/generic-extract.sh
pkg_download
pkg_decompress

# generate phase.sh
generate_phase prep
generate_phase patch
source $epkg_home_path/build/workspace/scripts/phase.sh

# build env create
echo "buildRequires:$buildRequires"
source $epkg_home_path/envs/common/profile-current/usr/lib/epkg/epkg-rc.sh
epkg env create build
epkg install $buildRequires

# run phase
cd $epkg_home_path/build/workspace/src/$name-$version
phases="prep patch build install"
for curPhase in ${phases[*]}; do
	runPhase "$curPhase"
done

# hash
epkg_hash_exec=$epkg_home_path/envs/common/profile-current/usr/bin/epkg-hash
file_hash=$($epkg_hash_exec "$epkg_fs_path" )
echo "pkg_fs_hash: $file_hash"
