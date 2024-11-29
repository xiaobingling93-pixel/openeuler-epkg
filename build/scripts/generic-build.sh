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
BUILD_SCRIPTS_DIR=$BUILD_WORKSPACE_DIR/scripts
BUILD_SOURCES_DIR=$BUILD_WORKSPACE_DIR/sources
BUILD_PATCHES_DIR=$BUILD_WORKSPACE_DIR/patches
BUILD_SRC_DIR=$BUILD_WORKSPACE_DIR/src
BUILD_OUT_DIR=$BUILD_WORKSPACE_DIR/fs
EPKG_BUILD_ENV_NAME="build-$(mktemp -u XXXX)"

dependency_check() {
	# Check Python 
	if ! command -v python3 &> /dev/null; then
		echo "Python is not installed. Please install."
		return 1
	fi

	# Check PyYAML 
	if ! pip show pyyaml &> /dev/null; then
		echo "pyyaml is not installed. Please install."
		return 1
	fi

    return 0
}

create_build_home() {
	rm -rf $BUILD_WORKSPACE_DIR
	mkdir -p $BUILD_WORKSPACE_DIR
	mkdir -p $BUILD_SCRIPTS_DIR
	mkdir -p $BUILD_SOURCES_DIR
	mkdir -p $BUILD_PATCHES_DIR
	mkdir -p $BUILD_SRC_DIR
	mkdir -p $BUILD_OUT_DIR
	return 0
}

parse_yaml() {
	yaml_path=$1
	python "$PROJECT_DIR/build/scripts/pkg-yaml2sh.py" $yaml_path $PROJECT_DIR $BUILD_SCRIPTS_DIR
	return 0
}

# step 0. Dependency check
dependency_check || exit 1
create_build_home

# step 1. Parse yaml
yaml_path=$1
parse_yaml $yaml_path
echo "Generate pkgvars.sh"

# Source the required scripts
source $BUILD_SCRIPTS_DIR/pkgvars.sh
source $PROJECT_DIR/build/build-system/"$buildSystem".sh
source $PROJECT_DIR/build/scripts/generic-download.sh
source $PROJECT_DIR/build/scripts/generic-extract.sh
source $PROJECT_DIR/build/scripts/generic-phase.sh

# step 2. Download & Decompress
pkg_download
pkg_decompress

# step 3. Generate phase.sh
generate_phase prep
generate_phase patch

# step 4. Build env create
source $PROJECT_DIR/envs/common/profile-current/usr/lib/epkg/epkg-rc.sh
echo "buildRequires:$buildRequires"
epkg env create $EPKG_BUILD_ENV_NAME
epkg install $buildRequires

# step5. Run phase
source $BUILD_SCRIPTS_DIR/phase.sh
cd $BUILD_SRC_DIR/$name-$version
phases="prep patch build install"
for curPhase in ${phases[*]}; do
	runPhase "$curPhase"
done

# step6. Calculate hash (Todo Demo, just print)
epkg_hash_exec=$PROJECT_DIR/envs/common/profile-current/usr/bin/epkg-hash
file_hash=$($epkg_hash_exec "$BUILD_OUT_DIR" )
echo "pkg_fs_hash: $file_hash"
