#!/usr/bin/env bash
# SPDX-License-Identifier: MulanPSL-2.0+
# Copyright (c) 2024 Huawei Technologies Co., Ltd. All rights reserved.

# Project Dir
if [ -d "/opt/epkg/users/public/envs/common/" ]; then
	PROJECT_DIR=/opt/epkg
else
	PROJECT_DIR=$HOME/.epkg
fi
# Build Dir
BUILD_WORKSPACE_DIR=$HOME/epkg-build
rm -rf $BUILD_WORKSPACE_DIR
BUILD_SCRIPTS_DIR=$BUILD_WORKSPACE_DIR/scripts
BUILD_SOURCES_DIR=$BUILD_WORKSPACE_DIR/sources
BUILD_PATCHES_DIR=$BUILD_WORKSPACE_DIR/patches
BUILD_SRC_DIR=$BUILD_WORKSPACE_DIR/src
BUILD_OUT_DIR=$BUILD_WORKSPACE_DIR/fs
mkdir -p $BUILD_WORKSPACE_DIR
mkdir -p $BUILD_SCRIPTS_DIR
mkdir -p $BUILD_SOURCES_DIR
mkdir -p $BUILD_PATCHES_DIR
mkdir -p $BUILD_SRC_DIR
mkdir -p $BUILD_OUT_DIR

# Parse yaml
yaml_path=$1
python "$PROJECT_DIR/build/scripts/pkg-yaml2sh.py" $yaml_path $PROJECT_DIR $BUILD_SCRIPTS_DIR
echo "Generate pkgvars.sh"

# Source the required scripts
source $BUILD_SCRIPTS_DIR/pkgvars.sh
source $PROJECT_DIR/build/build-system/"$buildSystem".sh
source $PROJECT_DIR/build/scripts/generic-download.sh
source $PROJECT_DIR/build/scripts/generic-extract.sh
source $PROJECT_DIR/build/scripts/generic-phase.sh
# download & decompress
pkg_download
pkg_decompress
# generate phase.sh
generate_phase prep
generate_phase patch

# build env create
source $PROJECT_DIR/envs/common/profile-current/usr/lib/epkg/epkg-rc.sh
echo "buildRequires:$buildRequires"
epkg env create build
epkg install $buildRequires

# run phase
source $BUILD_SCRIPTS_DIR/phase.sh
cd $BUILD_SRC_DIR/$name-$version
phases="prep patch build install"
for curPhase in ${phases[*]}; do
	runPhase "$curPhase"
done

# hash
epkg_hash_exec=$PROJECT_DIR/envs/common/profile-current/usr/bin/epkg-hash
file_hash=$($epkg_hash_exec "$BUILD_OUT_DIR" )
echo "pkg_fs_hash: $file_hash"
