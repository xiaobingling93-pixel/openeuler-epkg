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
BUILD_WORKSPACE_DIR=$HOME/.cache/epkg/build-workspace/${name}
BUILD_SCRIPTS_DIR=$BUILD_WORKSPACE_DIR/scripts
BUILD_SOURCES_DIR=$BUILD_WORKSPACE_DIR/sources
BUILD_PATCHES_DIR=$BUILD_WORKSPACE_DIR/patches
BUILD_SRC_DIR=$BUILD_WORKSPACE_DIR/src
BUILD_RESULT_DIR=$BUILD_WORKSPACE_DIR/result
BUILD_FS_DIR=$BUILD_RESULT_DIR/fs
BUILD_INFO_DIR=$BUILD_RESULT_DIR/info
BUILD_PGP_DIR=$BUILD_RESULT_DIR/info/pgp

prep_pipeline() {
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

	# Mkdir build home
	rm -rf $BUILD_WORKSPACE_DIR
	mkdir -p $BUILD_WORKSPACE_DIR
	mkdir -p $BUILD_SCRIPTS_DIR
	mkdir -p $BUILD_SOURCES_DIR
	mkdir -p $BUILD_PATCHES_DIR
	mkdir -p $BUILD_SRC_DIR
	mkdir -p $BUILD_RESULT_DIR
	mkdir -p $BUILD_FS_DIR
	mkdir -p $BUILD_INFO_DIR
	mkdir -p $BUILD_PGP_DIR
	return 0
}

build_pipeline() {
	# step 1. Parse yaml
	yaml_path=$1
	python "$PROJECT_DIR/build/scripts/pkg-yaml2sh.py" $yaml_path $PROJECT_DIR $BUILD_SCRIPTS_DIR
	echo "Generate pkgvars.sh"

	# step 2. Source file
	source $BUILD_SCRIPTS_DIR/pkgvars.sh
	if [ -e "$BUILD_SCRIPTS_DIR/phase.sh" ]; then
		source $BUILD_SCRIPTS_DIR/phase.sh
	fi
	source $PROJECT_DIR/build/build-system/"$buildSystem".sh
	source $PROJECT_DIR/build/scripts/generic-download.sh
	source $PROJECT_DIR/build/scripts/generic-extract.sh
	source $PROJECT_DIR/build/scripts/generic-phase.sh
	source $PROJECT_DIR/build/scripts/generic-patch.sh

	# step 3. Download & Decompress
	pkg_download
	pkg_decompress

	# step 4. Patch file
	cd $BUILD_SRC_DIR/$name-$version
	pkg_patch

	# step 5. Build env create
	source $PROJECT_DIR/envs/common/profile-current/usr/lib/epkg/epkg-rc.sh
	echo "buildRequires:${buildRequires[@]}"
	epkg env create build
	epkg install ${buildRequires[@]}

	# step6. Run phase
	phases="prepare build package"
	for curPhase in ${phases[*]}; do
		runPhase "$curPhase"
	done
}

post_pipeline() {
	# Generate epkg info (demo, empty file)
	touch $BUILD_INFO_DIR/runtimePhase.sh
	touch $BUILD_INFO_DIR/buildinfo.json
	touch $BUILD_INFO_DIR/package.json
	touch $BUILD_INFO_DIR/files

	echo "hash calculate dir: $BUILD_RESULT_DIR"
	epkg_hash_exec=$PROJECT_DIR/envs/common/profile-current/usr/bin/epkg-hash
	file_hash=$($epkg_hash_exec "$BUILD_RESULT_DIR" )
	echo "pkg_hash: $file_hash"
}

# Prep step. Dependency check
prep_pipeline || exit 1

# Main step
build_pipeline $@

# Post-Process Step
post_pipeline